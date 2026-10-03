//! One lightweight task per agent, with durable global-board delivery and gated exit.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};

use crate::{
    config::Config,
    context::{swarm_system_prompt, ContextConfig, ContextMessages, ContextWindow},
    metrics::{AgentStatus, Metrics},
    provider::{is_context_overflow, Completion, Provider, ProviderEvent},
    session::{make_provider, SessionControl},
    storage::Store,
    tools::ToolBus,
    workspace::WorkspaceTools,
};

const BOARD_PAGE: usize = 128;

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub agents: usize,
    pub finished_agents: usize,
    pub votes: usize,
    pub board_messages: u64,
    pub elapsed_ms: u128,
}

/// Grace is a revocable consensus debounce, never an operation deadline.
#[derive(Debug)]
pub struct Consensus {
    threshold: usize,
    grace: Duration,
    since: Option<Instant>,
    owner_revision: u64,
}

impl Consensus {
    pub fn new(agents: usize, grace: Duration) -> Self {
        Self {
            threshold: agents.saturating_mul(3).div_ceil(4),
            grace,
            since: None,
            owner_revision: 0,
        }
    }

    pub fn threshold(&self) -> usize {
        self.threshold
    }

    pub fn update(&mut self, votes: usize, owner_revision: u64, now: Instant) -> bool {
        if self.owner_revision != owner_revision {
            self.since = None;
            self.owner_revision = owner_revision;
        }
        if votes < self.threshold || self.threshold == 0 {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since) >= self.grace
    }
}

pub struct Harness {
    pub store: Store,
    pub metrics: Arc<Metrics>,
    pub control: SessionControl,
    config: Arc<Config>,
    tools: Arc<ToolBus>,
    shutdown: watch::Sender<bool>,
    system: Arc<str>,
    initial_board_seq: u64,
}

impl Harness {
    pub async fn new(config: Config) -> Result<Self> {
        config.validate()?;
        let system = swarm_system_prompt(
            &config.objective,
            &config.workspace.to_string_lossy(),
            config.agents,
        );
        let system_window = ContextWindow::new(system.clone(), context_config(&config));
        let available = config
            .context_budget
            .saturating_sub(config.max_output_tokens as usize);
        ensure!(system_window.estimated_tokens().saturating_add(256) < available,
            "immutable system prompt/objective exceeds available model context; increase --context-budget or shorten the objective");
        let store = Store::open(&config.database).await?;
        let membership = store
            .initialize_membership(config.agents, config.resume)
            .await?;
        let initial_board_seq = store.latest_seq().await?;
        let metrics = Arc::new(Metrics::new(0));
        for id in &membership.agent_ids {
            metrics.add_agent(id);
        }
        if config.interactive_session && config.objective.trim().is_empty() {
            for id in &membership.agent_ids {
                metrics.set_status(id, AgentStatus::Waiting);
                metrics.set_detail(id, "idle · send a prompt to begin");
            }
        }
        let workspace = WorkspaceTools::new(&config.workspace, config.max_processes)?;
        let mcp = crate::mcp::Hub::new(config.mcp.clone(), config.workspace.clone());
        let tools = Arc::new(
            ToolBus::new(store.clone(), workspace.clone())
                .with_mcp(mcp.clone())
                .with_metrics(metrics.clone()),
        );
        let provider = make_provider(&config)?;
        let (shutdown, _) = watch::channel(false);
        let control = SessionControl::new(
            config.clone(),
            provider.clone(),
            store.clone(),
            shutdown.clone(),
        )
        .with_mcp(mcp)
        .with_workspace(workspace)
        .with_membership(membership);
        Ok(Self {
            store,
            metrics,
            control,
            config: Arc::new(config),
            tools,
            shutdown,
            system,
            initial_board_seq,
        })
    }

    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    pub async fn run(self) -> Result<RunSummary> {
        if !self.config.interactive_session {
            self.control.begin_work().await;
            let result = self
                .run_round(self.config.clone(), true, 0)
                .await
                .map(|(summary, _)| summary);
            self.control.set_busy(false);
            self.control.mcp.shutdown().await;
            return result;
        }
        let mut stop = self.control.stop_receiver();
        let mut changes = self.store.subscribe_board();
        // Controls become available when construction finishes. Prompts posted
        // before this future is first polled must still start a round, while
        // historical prompts from before this session stay audit-only.
        let mut cursor = self.initial_board_seq;
        let mut pending = if self.config.resume && self.config.objective.trim().is_empty() {
            self.store
                .unfinished_prompt()
                .await?
                .map(|prompt| (prompt.body, prompt.seq, false))
        } else {
            (!self.config.objective.trim().is_empty())
                .then(|| (self.config.objective.clone(), 0, true))
        };
        let mut rounds = 0;
        let mut last = RunSummary {
            agents: self.control.members().len(),
            finished_agents: 0,
            votes: 0,
            board_messages: cursor,
            elapsed_ms: 0,
        };
        loop {
            // A prompt can be posted and stopped before the runtime future is
            // first polled. Preserve that acknowledged stop instead of clearing
            // its signal while starting the now-abandoned queued task.
            if *self.shutdown.borrow() && self.control.work_was_stopped() {
                cursor = cursor.max(self.control.drain_cursor());
                if pending.as_ref().is_some_and(|(_, seq, _)| *seq <= cursor) {
                    pending = None;
                }
                self.store
                    .append("harness", "all workers drained; work stopped", false)
                    .await?;
                self.store.finish_round().await?;
                self.shutdown.send_replace(false);
                self.control.set_busy(false);
            }
            self.control
                .publish_membership(self.store.membership().await?);
            for id in self.control.members() {
                if self.metrics.add_agent(&id) {
                    self.metrics.set_status(&id, AgentStatus::Waiting);
                    self.metrics
                        .set_detail(&id, "idle · send a prompt to begin");
                }
            }
            let members = self.control.members();
            for agent in self.metrics.snapshot().agents {
                if !members.contains(&agent.id) && agent.status != AgentStatus::Finished {
                    self.metrics.set_status(&agent.id, AgentStatus::Finished);
                    self.metrics
                        .set_detail(&agent.id, "removed while idle · no pending operations");
                }
            }
            if pending.is_none() {
                for message in self.store.read_board(cursor, BOARD_PAGE).await? {
                    cursor = message.seq;
                    if message.owner && message.sender == "owner" {
                        pending = Some((message.body, message.seq, false));
                        break;
                    }
                }
            }
            if *stop.borrow() {
                break;
            }
            if let Some((objective, seq, post)) = pending.take() {
                if !self.control.try_begin_work().await {
                    pending = Some((objective, seq, post));
                    continue;
                }
                let mut config = (*self.control.current().config).clone();
                config.objective = objective;
                config.resume = rounds == 0 && self.config.resume;
                let result = self.run_round(Arc::new(config), post, seq).await;
                self.shutdown.send_replace(false);
                self.control.set_busy(false);
                for id in self.control.members() {
                    self.metrics.set_status(&id, AgentStatus::Waiting);
                    self.metrics.set_detail(
                        &id,
                        if self.control.work_was_stopped() {
                            "idle · previous work stopped"
                        } else {
                            "idle · previous work completed"
                        },
                    );
                }
                match result {
                    Ok((summary, consumed)) => {
                        last = summary;
                        cursor = consumed.max(cursor);
                    }
                    Err(error) => {
                        self.store
                            .append("harness", format!("work could not start: {error:#}"), false)
                            .await?;
                    }
                }
                rounds += 1;
                continue;
            }
            tokio::select! {
                _ = stop.changed() => {},
                _ = changes.changed() => {},
                _ = tokio::time::sleep(Duration::from_millis(250)) => { self.store.refresh_board().await?; },
            }
        }
        self.store.close_session().await?;
        self.control
            .publish_membership(self.store.membership().await?);
        last.agents = self.control.members().len();
        self.control.mcp.shutdown().await;
        Ok(last)
    }

    async fn run_round(
        &self,
        config: Arc<Config>,
        post_objective: bool,
        prompt_seq: u64,
    ) -> Result<(RunSummary, u64)> {
        config.validate()?;
        self.control.mcp.prepare_enabled().await;
        let system = if config.objective == self.config.objective {
            self.system.clone()
        } else {
            swarm_system_prompt(
                &config.objective,
                &config.workspace.to_string_lossy(),
                config.agents,
            )
        };
        let window = ContextWindow::new(system.clone(), context_config(&config));
        ensure!(
            window.estimated_tokens().saturating_add(256)
                < config
                    .context_budget
                    .saturating_sub(config.max_output_tokens as usize),
            "objective exceeds model context; shorten the prompt or select a larger context model"
        );
        let started = Instant::now();
        if !config.resume {
            self.store.reset_votes().await?;
        }
        let run_start_seq = if post_objective {
            let snapshot = if config.interactive_session {
                tokio::select! {
                    biased;
                    _ = self.control.wait_for_work_stop() => None,
                    snapshot = crate::snapshots::capture(&config.workspace, &config.database) => snapshot.ok(),
                }
            } else {
                None
            };
            if self.control.work_was_stopped() {
                self.control.drain_cursor()
            } else {
                match self
                    .store
                    .owner_action(
                        "owner",
                        config.objective.clone(),
                        self.shutdown.subscribe(),
                        || {},
                    )
                    .await
                {
                    Ok(message) => {
                        if let Some(tree) = snapshot {
                            self.store.save_snapshot(message.seq, tree).await?;
                        }
                        message.seq
                    }
                    // Operator stop won the actor transaction before the initial
                    // objective was admitted. There is no new task to dispatch.
                    Err(_) if *self.shutdown.borrow() => self.control.drain_cursor(),
                    Err(error) => return Err(error),
                }
            }
        } else {
            prompt_seq
        };
        let mut membership = self.control.membership_receiver();
        let mut roster = membership.borrow().clone();
        let mut ordered_ids = roster.agent_ids.clone();
        let mut ids: HashSet<String> = ordered_ids.iter().cloned().collect();
        let shared = Arc::new(WorkerShared {
            config: config.clone(),
            store: self.store.clone(),
            tools: self.tools.clone(),
            metrics: self.metrics.clone(),
            control: self.control.clone(),
            shutdown: self.shutdown.subscribe(),
            run_start_seq,
            system,
        });
        let mut workers = JoinSet::new();
        let mut task_ids = HashMap::new();
        for id in &ordered_ids {
            if !self.store.mark_worker_started(id).await? {
                continue;
            }
            let shared = shared.clone();
            let shutdown = WorkerStop::new(self.shutdown.subscribe(), self.control.worker_stop(id));
            let id = id.clone();
            self.metrics.set_status(&id, AgentStatus::Starting);
            let worker_id = id.clone();
            let task = workers
                .spawn(async move { agent_worker(worker_id, shared, shutdown, false).await });
            task_ids.insert(task.id(), id);
        }
        let mut consensus = Consensus::new(ordered_ids.len(), self.config.grace_period);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut owner_revision = 0;
        let mut supervisor_cursor = 0;
        let mut finished = 0;
        let final_votes;
        let completed;
        let mut closing = self.control.stop_receiver();
        let mut round_stop = self.shutdown.subscribe();
        loop {
            if self.control.work_was_stopped() && !*self.shutdown.borrow() {
                // Workers have already cancelled. Wait only for the durable
                // stop gate before publishing completion/clearing round slots.
                round_stop
                    .changed()
                    .await
                    .context("round control disconnected")?;
                continue;
            }
            if self.control.is_closing() && !*self.shutdown.borrow() {
                self.control.drain_work(false).await?;
            }
            if *self.shutdown.borrow() {
                final_votes = 0;
                completed = false;
                owner_revision = self.control.drain_cursor();
                break;
            }
            tokio::select! {
                _ = tick.tick() => {},
                _ = membership.changed() => {},
                _ = closing.changed() => { continue; },
                _ = round_stop.changed() => { continue; },
                exit = workers.join_next_with_id() => {
                    if let Some(exit) = exit {
                        let (task_id, detail) = match exit {
                            Ok((task_id, result)) => (task_id, format!("unexpected worker exit: {result:?}")),
                            Err(error) => (error.id(), format!("worker panic: {error}")),
                        };
                        let id = task_ids.remove(&task_id).context("unknown agent task")?;
                        if !self.recover_worker(id, detail, shared.clone(), &mut workers, &mut task_ids).await? {
                            finished += 1;
                        }
                    }
                    continue;
                },
            }
            self.control
                .publish_membership(self.store.membership().await?);
            let current_roster = membership.borrow().clone();
            if current_roster.revision != roster.revision {
                roster = current_roster;
                ordered_ids = roster.agent_ids.clone();
                ids = ordered_ids.iter().cloned().collect();
                consensus = Consensus::new(ordered_ids.len(), self.config.grace_period);
            }
            let running: HashSet<_> = task_ids.values().cloned().collect();
            for id in &ordered_ids {
                if !running.contains(id) {
                    if !self.store.mark_worker_started(id).await? {
                        continue;
                    }
                    self.metrics.add_agent(id);
                    self.metrics.set_status(id, AgentStatus::Starting);
                    let worker_shared = shared.clone();
                    let worker_id = id.clone();
                    let shutdown =
                        WorkerStop::new(self.shutdown.subscribe(), self.control.worker_stop(id));
                    let task = workers.spawn(async move {
                        agent_worker(worker_id, worker_shared, shutdown, false).await
                    });
                    task_ids.insert(task.id(), id.clone());
                }
            }
            self.store.refresh_board().await?;
            loop {
                let page = self.store.read_board(supervisor_cursor, BOARD_PAGE).await?;
                if page.is_empty() {
                    break;
                }
                for message in page {
                    supervisor_cursor = message.seq;
                    if message.owner {
                        owner_revision = message.seq;
                    }
                }
            }
            let votes = self.store.votes().await?;
            let global_seq = self.store.latest_seq().await?;
            let done = votes
                .iter()
                .filter(|v| v.done && v.board_seq == global_seq && ids.contains(&v.agent_id))
                .count();
            if self.control.is_paused() {
                consensus.update(0, global_seq, Instant::now());
                continue;
            }
            if consensus.update(done, global_seq, Instant::now()) {
                let Some(confirmed_done) = self
                    .store
                    .try_commit_consensus_at_with_shutdown(
                        &ordered_ids,
                        owner_revision,
                        global_seq,
                        consensus.threshold(),
                        self.shutdown.clone(),
                    )
                    .await?
                else {
                    continue;
                };
                final_votes = confirmed_done;
                completed = true;
                self.shutdown.send_replace(true);
                break;
            }
        }
        let mut drain_error = None;
        while let Some(result) = workers.join_next_with_id().await {
            let (task_id, result) = match result {
                Ok((task_id, result)) => (task_id, result),
                Err(error) => (
                    error.id(),
                    Err(anyhow::anyhow!("agent task panicked: {error}")),
                ),
            };
            let id = task_ids
                .remove(&task_id)
                .context("unknown draining agent task")?;
            self.store.mark_worker_finished(&id).await?;
            self.control.finish_draining(&id);
            if let Err(error) = result {
                self.metrics.set_status(&id, AgentStatus::Error);
                self.store
                    .append(
                        &id,
                        format!("worker exited during consensus drain: {error:#}"),
                        false,
                    )
                    .await?;
                drain_error.get_or_insert(error);
            }
            finished += 1;
        }
        self.store
            .append(
                "harness",
                if self.control.work_was_stopped() {
                    "all workers drained; work stopped"
                } else if completed {
                    "all workers drained; swarm complete"
                } else {
                    "all workers drained; session detached"
                },
                false,
            )
            .await?;
        if self.config.interactive_session && !*self.control.stop_receiver().borrow() {
            self.store.finish_round().await?;
        }
        if let Some(error) = drain_error {
            return Err(error);
        }
        Ok((
            RunSummary {
                agents: ordered_ids.len(),
                finished_agents: finished,
                votes: if self.control.work_was_stopped() {
                    0
                } else {
                    final_votes
                },
                board_messages: self.store.latest_seq().await?,
                elapsed_ms: started.elapsed().as_millis(),
            },
            if self.control.work_was_stopped() {
                self.control.drain_cursor()
            } else {
                owner_revision
            },
        ))
    }

    /// An unexpected exit belongs to the same logical agent, not a new roster
    /// member. Resume its durable history unless an explicit withdrawal won.
    async fn recover_worker(
        &self,
        id: String,
        detail: String,
        shared: Arc<WorkerShared>,
        workers: &mut JoinSet<Result<()>>,
        task_ids: &mut HashMap<tokio::task::Id, String>,
    ) -> Result<bool> {
        self.store.mark_worker_finished(&id).await?;
        self.control
            .publish_membership(self.store.membership().await?);
        if *self.shutdown.borrow() || self.control.is_closing() || self.control.work_was_stopped() {
            self.metrics.set_status(&id, AgentStatus::Finished);
            self.control.finish_draining(&id);
            return Ok(false);
        }
        if !self.control.members().contains(&id) {
            self.metrics.set_status(&id, AgentStatus::Finished);
            self.control.finish_draining(&id);
            if detail.starts_with("worker panic") {
                self.store
                    .append(
                        &id,
                        format!("removed worker ended after draining with {detail}"),
                        false,
                    )
                    .await?;
            }
            return Ok(false);
        }
        self.metrics.set_status(&id, AgentStatus::Error);
        self.metrics.set_detail(&id, &detail);
        self.store
            .append(
                "harness",
                format!("{id}: {detail}; restarting from durable checkpoint"),
                false,
            )
            .await?;
        if !self.store.mark_worker_started(&id).await? {
            self.control
                .publish_membership(self.store.membership().await?);
            self.control.finish_draining(&id);
            self.metrics.set_status(&id, AgentStatus::Finished);
            self.store.append(&id, "removed worker exited before recovery restart; quitting after releasing its operation slot", false).await?;
            return Ok(false);
        }
        let worker_id = id.clone();
        let shutdown = WorkerStop::new(self.shutdown.subscribe(), self.control.worker_stop(&id));
        let task =
            workers.spawn(async move { agent_worker(worker_id, shared, shutdown, true).await });
        task_ids.insert(task.id(), id);
        Ok(true)
    }
}

struct WorkerShared {
    config: Arc<Config>,
    store: Store,
    tools: Arc<ToolBus>,
    metrics: Arc<Metrics>,
    control: SessionControl,
    system: Arc<str>,
    run_start_seq: u64,
    shutdown: watch::Receiver<bool>,
}

/// Stop dispatch at safe boundaries without cancelling requests or tool groups.
struct WorkerStop {
    round: watch::Receiver<bool>,
    removed: watch::Receiver<bool>,
}

impl WorkerStop {
    fn new(round: watch::Receiver<bool>, removed: watch::Receiver<bool>) -> Self {
        Self { round, removed }
    }
    fn requested(&self) -> bool {
        *self.round.borrow() || *self.removed.borrow()
    }
    async fn changed(&mut self) -> Result<()> {
        tokio::select! {
            changed = self.round.changed() => changed.context("supervisor disconnected"),
            changed = self.removed.changed() => changed.context("membership control disconnected"),
        }
    }
}

async fn agent_worker(
    id: String,
    shared: Arc<WorkerShared>,
    mut shutdown: WorkerStop,
    recovering: bool,
) -> Result<()> {
    let mut interrupted = shared.control.work_stop_receiver();
    let result = if shutdown.requested() || *interrupted.borrow() {
        Ok(())
    } else {
        // Drop the entire operation future on explicit /stop, including provider
        // streaming, compaction, tool execution, and queued capacity waits.
        // Consensus and member retirement still use safe dispatch boundaries.
        tokio::select! {
            biased;
            _ = async {
                while !*interrupted.borrow() {
                    if interrupted.changed().await.is_err() {
                        break;
                    }
                }
            } => Ok(()),
            result = async {
                if shared.config.mock {
                    mock_worker(&id, &shared, &mut shutdown).await
                } else {
                    live_worker(&id, &shared, &mut shutdown, recovering).await
                }
            } => result,
        }
    };
    if !*shutdown.removed.borrow() {
        result?;
    } else if let Err(error) = result {
        shared.metrics.set_detail(
            &id,
            &format!("removed worker finished pending operation with: {error:#}"),
        );
    }
    shared.metrics.set_status(&id, AgentStatus::Finished);
    shared
        .store
        .append(
            &id,
            if *shutdown.removed.borrow() {
                "removed worker drained all in-flight operations; quitting gracefully"
            } else if shared.control.work_was_stopped() {
                "worker cancelled immediately after owner stopped work; inspect interrupted operations before resuming"
            } else if shared.control.is_closing() {
                "worker drained all in-flight operations before session detach"
            } else {
                "quitting after harness completion consensus"
            },
            false,
        )
        .await?;
    Ok(())
}

async fn mock_worker(id: &str, shared: &WorkerShared, shutdown: &mut WorkerStop) -> Result<()> {
    let mut cursor = 0;
    let mut seen = HashSet::new();
    let mut board_revision = shared.store.subscribe_board();
    if !wait_while_paused(id, shared, shutdown).await? {
        return Ok(());
    }
    // Native board tools enforce the same cursor freshness used by real workers.
    read_mock_board(id, shared, &mut cursor, &mut seen).await?;
    if !seen.contains(id) {
        shared
            .tools
            .execute(
                id,
                "board_post",
                &json!({"body": "offline smoke: present and cooperating"}),
            )
            .await?;
        shared.metrics.record_tool(id);
    }
    // Every arrival/withdrawal notice wakes this loop. Fixed initial-count
    // barriers cannot represent late joins or gracefully retiring workers.
    while !shutdown.requested() {
        if !wait_while_paused(id, shared, shutdown).await? {
            break;
        }
        read_mock_board(id, shared, &mut cursor, &mut seen).await?;
        if shutdown.requested() {
            break;
        }
        let members = shared.control.members();
        if members.iter().all(|member| seen.contains(member)) {
            let evidence = format!("offline smoke verified all {} peer messages from the active roster by ordered global cursor", members.len());
            if !shared
                .store
                .vote(id)
                .await?
                .is_some_and(|vote| vote.done && vote.board_seq >= cursor)
            {
                match shared
                    .tools
                    .execute(id, "vote_done", &json!({"done":true,"evidence":evidence}))
                    .await
                {
                    Ok(_) => {
                        shared.metrics.record_tool(id);
                        shared.metrics.set_status(id, AgentStatus::Voted);
                    }
                    Err(error) => {
                        shared.metrics.set_detail(
                            id,
                            &format!("completion vote awaits fresh board: {error:#}"),
                        );
                        tokio::task::yield_now().await;
                        continue;
                    }
                }
            }
        }
        tokio::select! {
            changed = shutdown.changed() => { changed?; },
            _ = wait_for_pause(&shared.control) => {},
            changed = board_revision.changed() => {
                changed.context("board notifier disconnected")?;
            }
        }
    }
    Ok(())
}

async fn read_mock_board(
    id: &str,
    shared: &WorkerShared,
    cursor: &mut u64,
    seen: &mut HashSet<String>,
) -> Result<()> {
    loop {
        // Exercise tool dispatch and independently inspect its durable source of truth.
        shared
            .tools
            .execute(
                id,
                "board_read",
                &json!({"after":*cursor,"limit":BOARD_PAGE}),
            )
            .await?;
        shared.metrics.record_tool(id);
        let page = shared.store.read_board(*cursor, BOARD_PAGE).await?;
        if page.is_empty() {
            break;
        }
        for entry in page {
            *cursor = entry.seq;
            if entry.seq > shared.run_start_seq
                && entry.body == "offline smoke: present and cooperating"
            {
                seen.insert(entry.sender);
            }
        }
        shared.tools.observe_board(id, *cursor);
    }
    Ok(())
}

async fn live_worker(
    id: &str,
    shared: &WorkerShared,
    shutdown: &mut WorkerStop,
    recovering: bool,
) -> Result<()> {
    let initial = shared.control.current();
    let mut context = ContextWindow::new(shared.system.clone(), context_config(&initial.config));
    context.select_model(initial.revision, context_config(&initial.config));
    let mut cursor = 0;
    if shared.config.resume || recovering {
        if let Some(checkpoint) = shared.store.load_checkpoint(id).await? {
            cursor = checkpoint
                .get("cursor")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if let Some(messages) = checkpoint.get("messages").and_then(Value::as_array) {
                for message in recover_interrupted_tools(messages) {
                    context.push(message);
                }
                // Checkpoints do not authenticate the original model/account.
                // Restore text and tool groups, but never replay their opaque
                // signatures or encrypted reasoning into a new connection.
                context.reconfigure(context_config(&initial.config));
                // The checkpoint cursor records already-delivered contiguous
                // history; restore completion-vote freshness even if no new page exists.
                shared.tools.observe_board(id, cursor);
            }
        }
    }
    context.push(json!({"role":"user", "content":format!("you are {id}. discover the workspace, converse on the global board, negotiate work organically, implement and verify the objective. use vote_done only with evidence.")}));
    let mut board_revision = shared.store.subscribe_board();
    let mut failures: u32 = 0;
    let mut force_compaction = false;
    while !shutdown.requested() {
        if !wait_while_paused(id, shared, shutdown).await? {
            break;
        }
        let active = shared.control.current();
        context.select_model(active.revision, context_config(&active.config));
        if shutdown.requested() {
            break;
        }
        if !drain_board(id, shared, &mut context, &mut cursor).await? {
            save_context(id, shared, &context, cursor).await?;
            park_oversized(id, shared, shutdown, active.revision).await?;
            continue;
        }
        if shutdown.requested() {
            break;
        }
        if let Some(vote) = shared.store.vote(id).await?.filter(|v| v.done) {
            if cursor > vote.board_seq {
                shared
                    .store
                    .set_vote(
                        id,
                        false,
                        "new global board entries require reconsidering completion evidence",
                    )
                    .await?;
                context.push(json!({"role":"user","content":"new global board messages arrived after your completion evidence. your done vote was withdrawn. reconsider every new message, coordinate any remaining work and vote again only with current verification."}));
            } else {
                shared.metrics.set_status(id, AgentStatus::Voted);
                tokio::select! {
                    changed = shutdown.changed() => { changed.context("supervisor disconnected")?; },
                    changed = board_revision.changed() => { changed.context("board notifier disconnected")?; },
                }
                continue;
            }
        }
        if shutdown.requested() {
            break;
        }
        if !compact_context(id, shared, &mut context, force_compaction).await? {
            save_context(id, shared, &context, cursor).await?;
            park_oversized(id, shared, shutdown, active.revision).await?;
            continue;
        }
        if shutdown.requested() {
            break;
        }
        force_compaction = false;
        let selected = shared.control.current();
        if selected.revision != active.revision {
            // Compaction awaits a provider and may overlap an owner selection.
            // Re-read its board notice and apply its context budget before
            // dispatching a regular completion with the newly selected model.
            continue;
        }
        let active = selected;
        // External Store handles commit without this process's publication
        // callback. Reconcile at operation admission as well as supervisor ticks.
        shared
            .control
            .publish_membership(shared.store.membership().await?);
        if shutdown.requested() {
            break;
        }
        // A board/membership transaction can commit while the vote lookup or
        // compaction is awaited. In particular, clearing votes must not turn a
        // parked voter into a request dispatched without the new join notice.
        // Catch up again before admitting that next provider operation.
        if shared.store.latest_seq().await? > cursor {
            continue;
        }
        if !wait_while_paused(id, shared, shutdown).await? {
            break;
        }
        // Resume can append a control notice while this worker is parked.
        if shared.store.latest_seq().await? > cursor {
            continue;
        }
        let provider = active.provider.as_ref().context("missing provider")?;
        shared.metrics.set_status(id, AgentStatus::Thinking);
        let definitions = shared.tools.definitions();
        let completion = match complete_with_metrics(
            id,
            shared,
            provider,
            &context.borrowed_messages(),
            &definitions,
        )
        .await
        {
            Ok(completion) => {
                failures = 0;
                completion
            }
            Err(error) => {
                // Even permanent provider errors remain visible/retryable at the harness boundary.
                failures = failures.saturating_add(1);
                shared.metrics.set_status(id, AgentStatus::Error);
                shared
                    .metrics
                    .set_detail(id, &format!("provider error: {error:#}"));
                shared.metrics.record_retry(id);
                force_compaction = is_context_overflow(&error);
                let mut selection = shared.control.subscribe();
                if selection.borrow().revision != active.revision {
                    failures = 0;
                    force_compaction = false;
                    continue;
                }
                if shutdown.requested() {
                    continue;
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1u64 << failures.min(5))) => {},
                    _ = selection.changed() => {},
                    _ = shutdown.changed() => {},
                }
                if selection.borrow().revision != active.revision {
                    failures = 0;
                    force_compaction = false;
                }
                continue;
            }
        };
        context.push(completion.assistant_message());
        // Persist the pending protocol group before any workspace side effect.
        save_context(id, shared, &context, cursor).await?;
        if completion.tool_calls.is_empty() {
            // A provider finish reason cannot bypass native consensus gating.
            context.push(json!({"role":"user","content":"continue coordinated work through native tools. a text completion does not stop this agent; vote_done requires verified evidence."}));
            tokio::task::yield_now().await;
        }
        for call in completion.tool_calls {
            shared
                .control
                .publish_membership(shared.store.membership().await?);
            shared.metrics.set_status(id, AgentStatus::Tool);
            shared.metrics.set_detail(id, &call.name);
            shared.metrics.record_tool(id);
            shared
                .metrics
                .record_tool_start(id, &call.name, &call.arguments);
            // Freshness may change during streaming. ToolBus rejects stale mutation/vote calls;
            // defer new board history until all assistant/tool results are adjacent and complete.
            let args = serde_json::from_str::<Value>(&call.arguments);
            let output = if *shutdown.removed.borrow() {
                json!({"skipped":true,"reason":"worker removed; this tool had not started before retirement. In-flight operations were drained; inspect shared history before resuming elsewhere."})
            } else if shared.control.work_was_stopped() || shared.control.is_closing() {
                json!({"skipped":true,"reason":"owner stopped or closed the session before this tool started; admitted operations were drained and this unstarted operation was not executed."})
            } else {
                match args {
                    Ok(args) => match shared.tools.execute(id, &call.name, &args).await {
                        Ok(output) => output,
                        Err(error) => json!({"error":format!("{error:#}")}),
                    },
                    Err(error) => json!({"error":format!("invalid tool JSON: {error}")}),
                }
            };
            shared
                .metrics
                .record_tool_result(id, &call.name, &output.to_string());
            context
                .push(json!({"role":"tool","tool_call_id":call.id,"content":output.to_string()}));
            save_context(id, shared, &context, cursor).await?;
        }
        save_context(id, shared, &context, cursor).await?;
    }
    Ok(())
}

async fn wait_for_pause(control: &SessionControl) {
    let mut paused = control.pause_receiver();
    if !*paused.borrow() {
        let _ = paused.changed().await;
    }
}

/// Pauses never cancel a provider request or split a protocol tool group.
/// Workers park at dispatch boundaries with all durable history intact.
async fn wait_while_paused(
    id: &str,
    shared: &WorkerShared,
    shutdown: &mut WorkerStop,
) -> Result<bool> {
    let mut paused = shared.control.pause_receiver();
    let mut closing = shared.control.stop_receiver();
    while *paused.borrow() && !shutdown.requested() && !*closing.borrow() {
        shared.metrics.set_status(id, AgentStatus::Waiting);
        shared.metrics.set_detail(id, "paused · resume to continue");
        tokio::select! {
            changed = paused.changed() => { changed.context("pause control disconnected")?; },
            changed = shutdown.changed() => { changed?; },
            changed = closing.changed() => { changed.context("session close control disconnected")?; },
        }
    }
    // Close is published before the supervisor commits durable drain. Observe
    // it at admission directly, rather than letting a fast completed response
    // dispatch another provider turn in that intervening scheduler window.
    Ok(!shutdown.requested() && !*closing.borrow())
}

async fn drain_board(
    id: &str,
    shared: &WorkerShared,
    context: &mut ContextWindow,
    cursor: &mut u64,
) -> Result<bool> {
    // Snapshot the catch-up boundary so a continuously active swarm cannot starve
    // a worker inside one unbounded read loop. Future entries remain unread and
    // prevent positive completion votes, but do not block workspace/MCP tools.
    let stop_at = shared.store.latest_seq().await?;
    loop {
        let page = shared.store.read_board(*cursor, 8).await?;
        if page.is_empty() {
            break;
        }
        for entry in page {
            if !compact_context(id, shared, context, false).await? {
                return Ok(false);
            }
            context.push(json!({"role":"user","content":format!("global messageboard entry (owner=true marks owner instructions):\n{}", serde_json::to_string(&entry)?)}));
            *cursor = entry.seq;
            shared.tools.observe_board(id, *cursor);
            if *cursor >= stop_at {
                return Ok(true);
            }
        }
    }
    Ok(true)
}

async fn compact_context(
    id: &str,
    shared: &WorkerShared,
    context: &mut ContextWindow,
    mut forced: bool,
) -> Result<bool> {
    let mut failures = 0u32;
    loop {
        let mut shutdown = WorkerStop::new(shared.shutdown.clone(), shared.control.worker_stop(id));
        if !wait_while_paused(id, shared, &mut shutdown).await? {
            return Ok(true);
        }
        shared
            .control
            .publish_membership(shared.store.membership().await?);
        let active = shared.control.current();
        context.select_model(active.revision, context_config(&active.config));
        let provider = active.provider.as_ref().context("missing provider")?;
        if *shared.shutdown.borrow() || !shared.control.members().iter().any(|member| member == id)
        {
            return Ok(true);
        }
        let plan = if forced {
            context.forced_compaction_plan()
        } else {
            context.compaction_plan()
        };
        let Some(plan) = plan else {
            let available = context.available_tokens();
            if forced || context.estimated_tokens() > available {
                shared.metrics.set_status(id, AgentStatus::Error);
                shared.metrics.set_detail(id, "context blocked: no safely shrinkable completed prefix fits the model budget. full history/checkpoint and global board preserved. restart with a larger --context-budget and --resume; no oversized HTTP replay is sent.");
                return Ok(false);
            }
            return Ok(true);
        };
        let prompt = vec![
            json!({"role":"system","content":"summarize execution history accurately. preserve decisions, unfinished work, evidence, and global-board cursor references. return a concise continuation summary."}),
            json!({"role":"user","content":&plan.prompt}),
        ];
        let compacted = match complete_with_metrics(id, shared, provider, &prompt, &[]).await {
            Ok(summary) => context.apply_summary(&plan, &summary.content),
            Err(error) => Err(error),
        };
        match compacted {
            Ok(()) => {
                forced = false;
                failures = 0;
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                shared.metrics.set_status(id, AgentStatus::Retry);
                shared
                    .metrics
                    .set_detail(id, &format!("compaction retained history: {error:#}"));
                shared.metrics.record_retry(id);
                let mut removed = shared.control.worker_stop(id);
                let mut round = shared.shutdown.clone();
                let mut selection = shared.control.subscribe();
                if *removed.borrow() || *round.borrow() {
                    return Ok(true);
                }
                if selection.borrow().revision != active.revision {
                    failures = 0;
                    forced = false;
                    continue;
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1u64 << failures.min(5))) => {},
                    _ = removed.changed() => {},
                    _ = round.changed() => {},
                    _ = selection.changed() => {},
                }
                if selection.borrow().revision != active.revision {
                    failures = 0;
                    forced = false;
                }
            }
        }
    }
}

async fn save_context(
    id: &str,
    shared: &WorkerShared,
    context: &ContextWindow,
    cursor: u64,
) -> Result<()> {
    #[derive(Serialize)]
    struct Checkpoint<'a> {
        cursor: u64,
        messages: ContextMessages<'a>,
    }
    shared
        .store
        .save_checkpoint_serializable(
            id,
            &Checkpoint {
                cursor,
                messages: context.borrowed_messages(),
            },
        )
        .await
}

fn context_config(config: &Config) -> ContextConfig {
    let available = config
        .context_budget
        .saturating_sub(config.max_output_tokens as usize);
    ContextConfig {
        max_tokens: config.context_budget,
        reserve_output_tokens: config.max_output_tokens as usize,
        summary_max_tokens: (available / 8).clamp(64, 2048),
        retain_recent_tokens: available / 4,
    }
}

async fn park_oversized(
    id: &str,
    shared: &WorkerShared,
    shutdown: &mut WorkerStop,
    blocked_revision: u64,
) -> Result<()> {
    // Capacity/configuration blockage is visible, checkpointed, and non-destructive.
    // Workers stay alive until consensus or an operator restart with a larger budget.
    let mut selection = shared.control.subscribe();
    if selection.borrow().revision != blocked_revision || shutdown.requested() {
        return Ok(());
    }
    shared.store.set_vote(id, false, "context exceeds capacity without a safely shrinkable prefix; larger budget/resume required").await?;
    while !shutdown.requested() {
        if selection.borrow().revision != blocked_revision {
            return Ok(());
        }
        tokio::select! {
            changed = shutdown.changed() => { changed.context("supervisor disconnected")?; },
            changed = selection.changed() => { changed.context("selection disconnected")?; return Ok(()); },
        }
    }
    Ok(())
}

/// A crash can leave an unknown side effect. Repair the protocol without replaying
/// the operation; the model must inspect the workspace and establish its outcome.
fn recover_interrupted_tools(messages: &[Value]) -> Vec<Value> {
    let mut recovered = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for message in messages.iter().filter(|m| m["role"] != "system") {
        if message["role"] != "tool" {
            finish_interrupted(&mut recovered, &mut pending);
        }
        recovered.push(message.clone());
        if message["role"] == "assistant" {
            if let Some(calls) = message["tool_calls"].as_array() {
                pending.extend(
                    calls
                        .iter()
                        .filter_map(|call| call["id"].as_str().map(str::to_owned)),
                );
            }
        } else if message["role"] == "tool" {
            if let Some(id) = message["tool_call_id"].as_str() {
                pending.retain(|call| call != id);
            }
        }
    }
    finish_interrupted(&mut recovered, &mut pending);
    recovered
}

fn finish_interrupted(messages: &mut Vec<Value>, pending: &mut Vec<String>) {
    for id in pending.drain(..) {
        messages.push(json!({"role":"tool","tool_call_id":id,"content":"interrupted before a durable result was recorded. execution outcome is unknown; inspect the workspace before attempting any operation again."}));
    }
}

async fn complete_with_metrics<M: Serialize + Sync + ?Sized>(
    id: &str,
    shared: &WorkerShared,
    provider: &Provider,
    messages: &M,
    tools: &[Value],
) -> Result<Completion> {
    let (tx, mut rx) = mpsc::channel(32);
    let request = provider.complete_serializable(messages, tools, Some(tx));
    tokio::pin!(request);
    let mut channel_open = true;
    let completion = loop {
        tokio::select! {
            result = &mut request => break result?,
            event = rx.recv(), if channel_open => match event {
                Some(event) => record_event(id, &shared.metrics, event),
                None => channel_open = false,
            }
        }
    };
    while let Ok(event) = rx.try_recv() {
        record_event(id, &shared.metrics, event);
    }
    shared.metrics.record_usage(
        id,
        completion.usage.input_tokens,
        completion.usage.output_tokens,
        completion.usage.cached_tokens,
    );
    Ok(completion)
}

fn record_event(id: &str, metrics: &Metrics, event: ProviderEvent) {
    match event {
        ProviderEvent::TextDelta(text) => {
            metrics.set_status(id, AgentStatus::Thinking);
            metrics.append_output(id, &text);
        }
        ProviderEvent::Retry { reason, .. } => {
            metrics.record_retry(id);
            metrics.set_status(id, AgentStatus::Retry);
            metrics.set_detail(id, &reason);
        }
        ProviderEvent::Usage(_) => {} // Account once from the final completion.
        ProviderEvent::Reset { .. } => {
            metrics.set_status(id, AgentStatus::Thinking);
            metrics.set_detail(id, "stream restarted; provisional output may repeat");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[test]
    fn consensus_rounds_up_and_owner_or_revocation_restarts_grace() {
        let now = Instant::now();
        let mut gate = Consensus::new(5, Duration::from_secs(2));
        assert_eq!(gate.threshold(), 4);
        assert!(!gate.update(3, 1, now));
        assert!(!gate.update(4, 1, now));
        assert!(!gate.update(4, 2, now + Duration::from_secs(3)));
        assert!(!gate.update(3, 2, now + Duration::from_secs(5)));
        assert!(!gate.update(4, 2, now + Duration::from_secs(6)));
        assert!(gate.update(4, 2, now + Duration::from_secs(8)));
    }

    #[tokio::test]
    async fn blocked_worker_observes_selection_or_removal_before_parking() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let config = Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database: directory.path().join("parking.sqlite3"),
            mock: true,
            interactive_session: true,
            ..Config::default()
        };
        let harness = Harness::new(config.clone()).await?;
        let shared = WorkerShared {
            config: harness.config.clone(),
            store: harness.store.clone(),
            tools: harness.tools.clone(),
            metrics: harness.metrics.clone(),
            control: harness.control.clone(),
            system: harness.system.clone(),
            run_start_seq: 0,
            shutdown: harness.shutdown.subscribe(),
        };
        let mut shutdown = WorkerStop::new(
            harness.shutdown.subscribe(),
            harness.control.worker_stop("agent-001"),
        );
        let blocked_revision = harness.control.current().revision;
        let mut enlarged = config;
        enlarged.context_budget *= 2;
        // The selection changes while the blocked worker would be checkpointing,
        // before it can create the parking receiver. No second change will arrive.
        harness.control.switch(enlarged).await?;
        tokio::time::timeout(
            Duration::from_secs(2),
            park_oversized("agent-001", &shared, &mut shutdown, blocked_revision),
        )
        .await??;
        assert!(harness.store.vote("agent-001").await?.is_none());

        harness
            .control
            .remove_agents(vec!["agent-001".into()])
            .await?;
        tokio::time::timeout(
            Duration::from_secs(2),
            park_oversized(
                "agent-001",
                &shared,
                &mut shutdown,
                harness.control.current().revision,
            ),
        )
        .await??;
        assert!(shutdown.requested());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn selecting_working_model_wakes_failed_compaction_retry_immediately() -> Result<()> {
        async fn read_request(socket: &mut tokio::net::TcpStream) -> Result<Value> {
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            let end = loop {
                let count = socket.read(&mut chunk).await?;
                ensure!(count > 0, "request closed before headers");
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let length = std::str::from_utf8(&bytes[..end])?
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                })
                .context("request missing length")?;
            while bytes.len() < end + length {
                let count = socket.read(&mut chunk).await?;
                ensure!(count > 0, "request body closed early");
                bytes.extend_from_slice(&chunk[..count]);
            }
            Ok(serde_json::from_slice(&bytes[end..end + length])?)
        }
        let directory = tempfile::tempdir()?;
        let first = TcpListener::bind("127.0.0.1:0").await?;
        let second = TcpListener::bind("127.0.0.1:0").await?;
        let config = Config {
            agents: 1,
            workspace: directory.path().to_owned(),
            database: directory.path().join("retry-selection.sqlite3"),
            objective: "verify event-driven compaction retry selection".into(),
            model: "alpha".into(),
            base_url: format!("http://{}/v1", first.local_addr()?),
            context_budget: 8192,
            max_output_tokens: 1024,
            ..Config::default()
        };
        let mut next = config.clone();
        next.model = "beta".into();
        next.base_url = format!("http://{}/v1", second.local_addr()?);
        let harness = Harness::new(config).await?;
        let shared = Arc::new(WorkerShared {
            config: harness.config.clone(),
            store: harness.store.clone(),
            tools: harness.tools.clone(),
            metrics: harness.metrics.clone(),
            control: harness.control.clone(),
            system: harness.system.clone(),
            run_start_seq: 0,
            shutdown: harness.shutdown.subscribe(),
        });
        let old_server = tokio::spawn(async move {
            let (mut socket, _) = first.accept().await?;
            assert_eq!(read_request(&mut socket).await?["model"], "alpha");
            let body = r#"{"error":{"message":"old model cannot summarize"}}"#;
            socket.write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
            socket.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        });
        let new_server = tokio::spawn(async move {
            let (mut socket, _) = second.accept().await?;
            assert_eq!(read_request(&mut socket).await?["model"], "beta");
            let body = json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"Older completed work was summarized. Preserve the global board and continue remaining work using the selected model."}}]}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;
            socket.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        });
        let mut context =
            ContextWindow::new(harness.system.clone(), context_config(&harness.config));
        for number in 0..6 {
            context.push(json!({"role":"user","content":format!("older completed turn {number}: {}", "x".repeat(3000))}));
        }
        let compactor =
            tokio::spawn(
                async move { compact_context("agent-001", &shared, &mut context, true).await },
            );
        old_server.await??;
        tokio::time::timeout(Duration::from_secs(2), async {
            while harness.metrics.snapshot().agents[0].status != AgentStatus::Retry {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        harness.control.switch(next).await?;
        // Initial failed-summary backoff is two seconds. A selection notification
        // must wake it without waiting for that sleep or cancelling any request.
        tokio::time::timeout(Duration::from_secs(1), new_server).await???;
        assert!(compactor.await??);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_mcp_initialization_does_not_park_workers_or_final_shutdown() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/mcp", listener.local_addr()?);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = [0u8; 4096];
            let count = socket.read(&mut bytes).await?;
            ensure!(count > 0, "initialize request missing");
            started_tx
                .send(())
                .map_err(|_| anyhow::anyhow!("runtime fixture exited"))?;
            // Deliberately never respond. Explicit final preparation shutdown
            // closes the pending transport; elapsed time is not a server policy.
            while socket.read(&mut bytes).await? != 0 {}
            Ok::<_, anyhow::Error>(())
        });
        let harness = Harness::new(Config {
            agents: 2,
            workspace: directory.path().to_owned(),
            database: directory.path().join("stalled-mcp.sqlite3"),
            objective: "workers must collaborate despite optional MCP initialization blockage"
                .into(),
            mock: true,
            grace_period: Duration::from_millis(200),
            mcp: [(
                "stalled".into(),
                crate::mcp::ServerConfig {
                    kind: "remote".into(),
                    url,
                    enabled: true,
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Config::default()
        })
        .await?;
        let store = harness.store.clone();
        // Establish the fixture's held operation before launching the very
        // short mock round. Under parallel load, consensus can otherwise win
        // before the preparation task ever dispatches its initialize request.
        harness.control.mcp.prepare_enabled().await;
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .context("MCP initialize not started")??;
        let run = tokio::spawn(harness.run());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let board = store.read_board(0, 100).await?;
                if ["agent-001", "agent-002"].iter().all(|id| {
                    board.iter().any(|entry| {
                        entry.sender == *id
                            && entry.body == "offline smoke: present and cooperating"
                    })
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("workers did not collaborate while MCP initialize held")??;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), run)
                .await
                .context("Harness blocked at final MCP shutdown")???
                .finished_agents,
            2
        );
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .context("pending HTTP initialization socket remained open after shutdown")???;
        Ok(())
    }

    #[test]
    fn resume_repairs_only_missing_tool_results_without_replaying_effects() {
        let messages = vec![
            json!({"role":"system","content":"prefix"}),
            json!({"role":"assistant","tool_calls":[{"id":"a"},{"id":"b"}]}),
            json!({"role":"tool","tool_call_id":"a","content":"success"}),
        ];
        let repaired = recover_interrupted_tools(&messages);
        assert_eq!(repaired.len(), 3);
        assert_eq!(repaired[1]["content"], "success");
        assert_eq!(repaired[2]["tool_call_id"], "b");
        assert!(repaired[2]["content"]
            .as_str()
            .unwrap()
            .contains("outcome is unknown"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn in_run_recovery_restores_pending_tools_but_fresh_launch_ignores_old_checkpoint(
    ) -> Result<()> {
        for recovering in [false, true] {
            let directory = tempfile::tempdir()?;
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await?;
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 4096];
                let header_end = loop {
                    let count = socket.read(&mut chunk).await?;
                    ensure!(count > 0, "request ended before headers");
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        break index + 4;
                    }
                    ensure!(bytes.len() <= 64 * 1024, "unexpected request header size");
                };
                let length = std::str::from_utf8(&bytes[..header_end])?
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .context("missing request content length")?;
                ensure!(length <= 1024 * 1024, "unexpected request body size");
                while bytes.len() < header_end + length {
                    let count = socket.read(&mut chunk).await?;
                    ensure!(count > 0, "request body ended early");
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let request: Value =
                    serde_json::from_slice(&bytes[header_end..header_end + length])?;
                let body = json!({"choices":[{"finish_reason":"stop","message":{
                    "role":"assistant","content":"verified current checkpoint policy",
                    "tool_calls":[{"id":"current-vote","type":"function","function":{
                        "name":"vote_done","arguments":json!({"done":true,"evidence":"verified checkpoint recovery policy"}).to_string()
                    }}]
                }}]}).to_string();
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket.write_all(headers.as_bytes()).await?;
                socket.write_all(body.as_bytes()).await?;
                socket.shutdown().await?;
                Ok::<_, anyhow::Error>(request)
            });
            let harness = Harness::new(Config {
                agents: 1,
                workspace: directory.path().to_owned(),
                database: directory.path().join("recovery.sqlite"),
                objective: "verify checkpoint recovery without replaying unknown side effects"
                    .into(),
                base_url: format!("http://{address}/v1"),
                resume: false,
                no_tui: true,
                ..Config::default()
            })
            .await?;
            let owner = harness
                .store
                .append("owner", "current run objective", true)
                .await?;
            harness.store.save_checkpoint("agent-001", &json!({
                "cursor":owner.seq,"messages":[
                    {"role":"system","content":"old system prefix"},
                    {"role":"user","content":"historical checkpoint sentinel"},
                    {"role":"assistant","content":"pending unknown side effect",
                    "_openraid_response_items":[{"type":"reasoning","encrypted_content":"old-account-encrypted-state"}],
                    "_openraid_native":{"chat":{"reasoning_content":"old-model-reasoning","reasoning_details":[{"type":"reasoning.encrypted","data":"old-account-encrypted-state"}]}},
                    "tool_calls":[{
                        "id":"pending-command","type":"function","function":{
                            "name":"run_command","arguments":"{\"program\":\"never-replay-this\"}"
                        }
                    }]}
                ]
            })).await?;
            let shared = Arc::new(WorkerShared {
                config: harness.config.clone(),
                store: harness.store.clone(),
                tools: harness.tools.clone(),
                metrics: harness.metrics.clone(),
                control: harness.control.clone(),
                system: harness.system.clone(),
                run_start_seq: owner.seq,
                shutdown: harness.shutdown.subscribe(),
            });
            let mut workers = JoinSet::new();
            let mut task_ids = HashMap::new();
            assert!(harness.store.mark_worker_started("agent-001").await?);
            if recovering {
                // Exercise the same supervisor recovery helper used by run_round,
                // after an actual JoinSet panic rather than manually enabling resume.
                let crashed = workers.spawn(async {
                    panic!("deterministic unexpected agent-session interruption");
                    #[allow(unreachable_code)]
                    Ok(())
                });
                task_ids.insert(crashed.id(), "agent-001".to_owned());
                let error = workers
                    .join_next_with_id()
                    .await
                    .context("missing crashed worker")?
                    .unwrap_err();
                let id = task_ids
                    .remove(&error.id())
                    .context("missing crashed identity")?;
                assert!(
                    harness
                        .recover_worker(
                            id,
                            format!("worker panic: {error}"),
                            shared,
                            &mut workers,
                            &mut task_ids
                        )
                        .await?
                );
            } else {
                let shutdown = WorkerStop::new(
                    harness.shutdown.subscribe(),
                    harness.control.worker_stop("agent-001"),
                );
                let worker = workers.spawn(async move {
                    agent_worker("agent-001".into(), shared, shutdown, false).await
                });
                task_ids.insert(worker.id(), "agent-001".to_owned());
            }
            let request = server.await??;
            loop {
                if harness
                    .store
                    .vote("agent-001")
                    .await?
                    .is_some_and(|vote| vote.done)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            harness.shutdown.send_replace(true);
            let (task_id, result) = workers
                .join_next_with_id()
                .await
                .context("missing drained worker")??;
            result?;
            let id = task_ids
                .remove(&task_id)
                .context("missing recovered identity")?;
            assert_eq!(
                id, "agent-001",
                "automatic recovery preserves logical identity"
            );
            harness.store.mark_worker_finished(&id).await?;
            if recovering {
                assert!(harness
                    .store
                    .read_board(0, 100)
                    .await?
                    .iter()
                    .any(|entry| entry.sender == "harness"
                        && entry.body.contains("agent-001: worker panic")
                        && entry.body.contains("restarting from durable checkpoint")));
            }
            let messages = request["messages"]
                .as_array()
                .context("missing model messages")?;
            let historic = messages
                .iter()
                .any(|message| message["content"] == "historical checkpoint sentinel");
            assert_eq!(
                historic, recovering,
                "resume=false should restore only for in-run recovery"
            );
            assert_ne!(messages[0]["content"], "old system prefix");
            assert!(
                messages
                    .iter()
                    .all(|message| message.as_object().is_none_or(|object| {
                        object.keys().all(|key| !key.starts_with("_openraid_"))
                    })),
                "restored signed/encrypted state must not cross model/account boundaries"
            );
            assert!(
                messages.iter().all(|message| {
                    message.get("reasoning_content").is_none()
                        && message.get("reasoning_details").is_none()
                }),
                "checkpoint reasoning must not be converted to wire fields for a new connection"
            );
            let repaired = messages.iter().find(|message| {
                message["role"] == "tool" && message["tool_call_id"] == "pending-command"
            });
            if recovering {
                let repaired = repaired.context("missing unknown-effect protocol repair")?;
                assert!(repaired["content"]
                    .as_str()
                    .unwrap()
                    .contains("outcome is unknown"));
                let checkpoint = harness
                    .store
                    .load_checkpoint("agent-001")
                    .await?
                    .context("missing durable recovered checkpoint")?;
                assert!(checkpoint["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|message| message["tool_call_id"] == "pending-command"));
            } else {
                assert!(repaired.is_none());
            }
            assert_eq!(
                harness.metrics.snapshot().tools,
                1,
                "only the new completion vote executes; interrupted command is not replayed"
            );
            assert_eq!(harness.metrics.snapshot().finished, 1);
        }
        Ok(())
    }
}
