//! Shared live selection; changes take effect at safe worker/tool boundaries.
use crate::{
    config::Config,
    provider::{Protocol, Provider, ProviderConfig},
    storage::{Membership, Store},
};
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex as StdMutex,
    },
};
use tokio::sync::{watch, Mutex};

#[derive(Clone)]
pub struct ActiveModel {
    pub config: Arc<Config>,
    pub provider: Option<Provider>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionNavigation {
    New,
    Open(String),
}

struct PendingPrompt(Arc<AtomicUsize>, watch::Sender<u64>);

impl Drop for PendingPrompt {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
        self.1
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

#[derive(Clone)]
pub struct SessionControl {
    pub mcp: crate::mcp::Hub,
    models: watch::Sender<Arc<ActiveModel>>,
    stop: watch::Sender<bool>,
    round: watch::Sender<bool>,
    busy: watch::Sender<bool>,
    state_changes: watch::Sender<u64>,
    paused: watch::Sender<bool>,
    work_stopped: watch::Sender<bool>,
    drain_cursor: watch::Sender<u64>,
    navigation: Arc<StdMutex<Option<SessionNavigation>>>,
    changes: Arc<Mutex<()>>,
    snapshots: Arc<Mutex<()>>,
    pending_prompts: Arc<AtomicUsize>,
    interruptions: Arc<AtomicU64>,
    stop_pending: Arc<AtomicBool>,
    recovery_requested: Arc<AtomicBool>,
    membership: watch::Sender<Arc<Membership>>,
    worker_stops: Arc<StdMutex<HashMap<String, watch::Sender<bool>>>>,
    workspace: Option<crate::workspace::WorkspaceTools>,
    store: Store,
}

pub fn make_provider(config: &Config) -> Result<Option<Provider>> {
    if config.mock {
        return Ok(None);
    }
    let options = if config.protocol == Protocol::Sdk {
        config.provider_options.clone()
    } else {
        crate::variants::wire_options(
            &config.provider_npm,
            &config.provider_options,
            config.protocol == Protocol::Responses,
        )
    };
    let provider = Provider::new_with_options(
        ProviderConfig {
            base_url: config.base_url.clone(),
            api_key: config.api_key.clone(),
            model: config.api_model.as_ref().unwrap_or(&config.model).clone(),
            max_in_flight: config.max_in_flight,
            max_output_tokens: config.max_output_tokens as usize,
        },
        config.protocol,
        options,
        config.provider_headers.clone(),
    )?
    .with_sdk(&config.provider_npm, &config.provider);
    Ok(Some(match &config.oauth {
        Some(oauth) => provider.with_oauth(oauth.clone()),
        None => provider,
    }))
}

impl SessionControl {
    pub fn new(
        config: Config,
        provider: Option<Provider>,
        store: Store,
        round: watch::Sender<bool>,
    ) -> Self {
        let (membership, _) = watch::channel(Arc::new(Membership {
            revision: 0,
            agent_ids: (1..=config.agents)
                .map(|number| format!("agent-{number:03}"))
                .collect(),
        }));
        let (models, _) = watch::channel(Arc::new(ActiveModel {
            config: Arc::new(config),
            provider,
            revision: 0,
        }));
        let (stop, _) = watch::channel(false);
        let (busy, _) = watch::channel(false);
        let (state_changes, _) = watch::channel(0);
        let (paused, _) = watch::channel(false);
        let (work_stopped, _) = watch::channel(false);
        let (drain_cursor, _) = watch::channel(0);
        Self {
            mcp: crate::mcp::Hub::default(),
            models,
            stop,
            busy,
            state_changes,
            paused,
            work_stopped,
            drain_cursor,
            navigation: Arc::new(StdMutex::new(None)),
            round,
            store,
            changes: Arc::new(Mutex::new(())),
            snapshots: Arc::new(Mutex::new(())),
            pending_prompts: Arc::new(AtomicUsize::new(0)),
            interruptions: Arc::new(AtomicU64::new(0)),
            stop_pending: Arc::new(AtomicBool::new(false)),
            recovery_requested: Arc::new(AtomicBool::new(false)),
            membership,
            worker_stops: Arc::new(StdMutex::new(HashMap::new())),
            workspace: None,
        }
    }
    pub fn with_membership(self, membership: Membership) -> Self {
        self.publish_membership(membership);
        self
    }
    pub fn with_paused(self, paused: bool) -> Self {
        self.paused.send_replace(paused);
        self
    }
    pub(crate) fn take_recovery_request(&self) -> bool {
        self.recovery_requested.swap(false, Ordering::AcqRel)
    }
    pub fn members(&self) -> Vec<String> {
        self.membership.borrow().agent_ids.clone()
    }
    pub fn membership_receiver(&self) -> watch::Receiver<Arc<Membership>> {
        self.membership.subscribe()
    }
    pub fn draining_members(&self) -> Vec<String> {
        let stops = self
            .worker_stops
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut ids = stops
            .iter()
            .filter(|(_, stop)| *stop.borrow())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }
    pub(crate) fn worker_stop(&self, id: &str) -> watch::Receiver<bool> {
        let mut stops = self
            .worker_stops
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        stops
            .entry(id.to_owned())
            .or_insert_with(|| watch::channel(!self.members().iter().any(|member| member == id)).0)
            .subscribe()
    }
    pub(crate) fn finish_draining(&self, id: &str) {
        self.worker_stops
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(id);
    }
    pub(crate) fn publish_membership(&self, membership: Membership) {
        let stops = self
            .worker_stops
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let current = self.membership.borrow().clone();
        if membership.revision < current.revision
            || (membership.revision == current.revision
                && membership.agent_ids == current.agent_ids)
        {
            return;
        }
        for (id, stop) in stops.iter() {
            if !membership.agent_ids.contains(id) {
                stop.send_replace(true);
            }
        }
        self.membership.send_replace(Arc::new(membership));
    }
    pub async fn add_agents(&self, count: usize) -> Result<Vec<String>> {
        let _change = self.changes.lock().await;
        ensure!(!*self.stop.borrow(), "session is closing");
        let control = self.clone();
        let membership = self
            .store
            .add_members(count, self.round.subscribe(), move |membership| {
                control.publish_membership(membership)
            })
            .await?;
        Ok(membership
            .agent_ids
            .into_iter()
            .rev()
            .take(count)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect())
    }
    pub async fn remove_agents(&self, ids: Vec<String>) -> Result<()> {
        let _change = self.changes.lock().await;
        ensure!(!*self.stop.borrow(), "session is closing");
        let control = self.clone();
        self.store
            .remove_members(&ids, self.round.subscribe(), move |membership| {
                control.publish_membership(membership)
            })
            .await?;
        Ok(())
    }
    pub fn with_mcp(mut self, mcp: crate::mcp::Hub) -> Self {
        self.mcp = mcp;
        self
    }
    pub fn with_workspace(mut self, workspace: crate::workspace::WorkspaceTools) -> Self {
        self.workspace = Some(workspace);
        self
    }
    pub fn current(&self) -> Arc<ActiveModel> {
        self.models.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<ActiveModel>> {
        self.models.subscribe()
    }
    pub fn stop_receiver(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }
    /// Coalesced notifications for busy/pause/stop transitions, including
    /// prompt preparation that has not yet reached the durable board.
    pub fn state_receiver(&self) -> watch::Receiver<u64> {
        self.state_changes.subscribe()
    }
    fn notify_state(&self) {
        self.state_changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    pub fn detach(&self) {
        self.stop.send_replace(true);
        self.notify_state();
    }
    pub fn is_closing(&self) -> bool {
        *self.stop.borrow()
    }
    pub fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }
    pub fn is_stopping(&self) -> bool {
        self.stop_pending.load(Ordering::Acquire)
            || *self.round.borrow()
            || (self.is_busy() && self.is_closing())
    }
    pub(crate) fn work_was_stopped(&self) -> bool {
        *self.work_stopped.borrow()
    }
    pub(crate) fn work_stop_receiver(&self) -> watch::Receiver<bool> {
        self.work_stopped.subscribe()
    }
    pub(crate) async fn wait_for_work_stop(&self) {
        let mut stop = self.work_stop_receiver();
        while !*stop.borrow() {
            if stop.changed().await.is_err() {
                break;
            }
        }
    }
    async fn wait_for_interruption(&self, generation: u64) {
        let mut stop = self.work_stop_receiver();
        while self.interruptions.load(Ordering::Acquire) == generation {
            if stop.changed().await.is_err() {
                break;
            }
        }
    }
    pub(crate) fn drain_cursor(&self) -> u64 {
        *self.drain_cursor.borrow()
    }
    pub(crate) fn pause_receiver(&self) -> watch::Receiver<bool> {
        self.paused.subscribe()
    }
    pub async fn pause(&self) -> Result<()> {
        self.set_paused(true).await
    }
    pub async fn resume(&self) -> Result<()> {
        self.set_paused(false).await?;
        if !self.is_busy() && self.store.unfinished_prompt().await?.is_some() {
            self.recovery_requested.store(true, Ordering::Release);
            self.notify_state();
        }
        Ok(())
    }
    async fn set_paused(&self, paused: bool) -> Result<()> {
        let _change = self.changes.lock().await;
        ensure!(!self.is_closing(), "session is closing");
        ensure!(
            !*self.round.borrow(),
            "the round is draining; retry after it finishes"
        );
        if self.is_paused() == paused {
            return Ok(());
        }
        let signal = self.paused.clone();
        let state = self.state_changes.clone();
        if !self.store.has_started().await? {
            self.store.set_paused(paused).await?;
            signal.send_replace(paused);
            self.notify_state();
            return Ok(());
        }
        self.store
            .owner_action_with_pause(
                "owner-control",
                if paused {
                    "Owner paused work. Finish admitted request/tool groups, then wait for resume."
                } else {
                    "Owner resumed work. Continue from preserved history and checkpoints."
                }
                .into(),
                self.round.subscribe(),
                Some(paused),
                move || {
                    signal.send_replace(paused);
                    state.send_modify(|revision| *revision = revision.wrapping_add(1));
                },
            )
            .await?;
        Ok(())
    }
    /// Immediately cancel the current task without claiming successful completion.
    /// The interactive console remains open for another prompt.
    pub async fn stop_work(&self) -> Result<()> {
        ensure!(!self.is_closing(), "session is closing");
        if !self.is_busy() || (self.work_was_stopped() && *self.round.borrow()) {
            if let Some(workspace) = &self.workspace {
                workspace.stop_processes();
            }
            return Ok(());
        }
        if self.stop_pending.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        // Wake every worker before waiting for the durable drain transaction.
        // Neither a busy SQLite actor nor another control holding `changes`
        // may delay cancellation. Admission observes stop_pending meanwhile.
        self.interruptions.fetch_add(1, Ordering::AcqRel);
        self.work_stopped.send_replace(true);
        if let Some(workspace) = &self.workspace {
            workspace.stop_processes();
        }
        self.paused.send_replace(false);
        self.notify_state();
        // Once cancellation is published, its durable bookkeeping must survive
        // a dropped UI/control caller even while another change owns the lock.
        let control = self.clone();
        tokio::spawn(async move {
            let _change = control.changes.lock().await;
            control.drain_work(true).await
        })
        .await
        .context("stop bookkeeping task failed")?
    }
    pub(crate) async fn drain_work(&self, stopped: bool) -> Result<()> {
        let round = self.round.clone();
        let paused = self.paused.clone();
        let work_stopped = self.work_stopped.clone();
        let cursor = self.drain_cursor.clone();
        let interruptions = self.interruptions.clone();
        let stop_pending = self.stop_pending.clone();
        let state = self.state_changes.clone();
        self.store
            .drain_round(
                if stopped {
                    "Owner stopped current work; cancelling in-flight operations immediately."
                } else {
                    "Owner closed session; draining in-flight operations."
                }
                .into(),
                move |seq| {
                    interruptions.fetch_add(1, Ordering::AcqRel);
                    cursor.send_replace(seq);
                    work_stopped.send_replace(stopped);
                    round.send_replace(true);
                    paused.send_replace(false);
                    stop_pending.store(false, Ordering::Release);
                    state.send_modify(|revision| *revision = revision.wrapping_add(1));
                },
            )
            .await?;
        Ok(())
    }
    pub async fn request_navigation(&self, target: SessionNavigation) -> Result<()> {
        let _change = self.changes.lock().await;
        ensure!(!self.is_closing(), "session is closing");
        ensure!(
            !self.is_busy(),
            "stop the current work before switching sessions"
        );
        *self
            .navigation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(target);
        self.detach();
        Ok(())
    }
    pub fn take_navigation(&self) -> Option<SessionNavigation> {
        self.navigation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
    pub fn is_busy(&self) -> bool {
        *self.busy.borrow() || self.pending_prompts.load(Ordering::Acquire) > 0
    }
    pub fn set_busy(&self, busy: bool) {
        self.busy.send_replace(busy);
        self.notify_state();
    }

    pub async fn clear_board(&self) -> Result<()> {
        let _snapshot = self.snapshots.lock().await;
        let _change = self.changes.lock().await;
        ensure!(!self.is_closing(), "session is closing");
        ensure!(
            !self.is_busy(),
            "stop work and wait for all workers to drain before clearing the board"
        );
        self.store.clear_board().await
    }
    pub async fn begin_work(&self) {
        let _change = self.changes.lock().await;
        self.work_stopped.send_replace(false);
        self.drain_cursor.send_replace(0);
        self.busy.send_replace(true);
        self.notify_state();
    }

    /// Admission is serialized with operator stop. A stop acknowledged after
    /// the runtime selected a queued prompt must not be cleared by startup.
    pub(crate) async fn try_begin_work(&self) -> bool {
        let _change = self.changes.lock().await;
        let interruption = self.interruptions.load(Ordering::Acquire);
        if *self.round.borrow() || self.is_closing() || self.stop_pending.load(Ordering::Acquire) {
            return false;
        }
        self.work_stopped.send_replace(false);
        self.drain_cursor.send_replace(0);
        self.busy.send_replace(true);
        if self.stop_pending.load(Ordering::Acquire)
            || self.interruptions.load(Ordering::Acquire) != interruption
        {
            self.work_stopped.send_replace(true);
            self.notify_state();
            return false;
        }
        self.notify_state();
        true
    }

    pub async fn post_prompt(&self, text: String) -> Result<u64> {
        ensure!(!self.is_closing(), "session is closing");
        ensure!(!text.trim().is_empty(), "prompt must not be empty");
        let interruption = self.interruptions.load(Ordering::Acquire);
        self.pending_prompts.fetch_add(1, Ordering::AcqRel);
        self.notify_state();
        let _pending = PendingPrompt(self.pending_prompts.clone(), self.state_changes.clone());
        // Git capture may traverse a large workspace. Keep it serialized with
        // restore, while leaving pause/stop/model controls responsive.
        let _snapshot = tokio::select! {
            biased;
            _ = self.wait_for_interruption(interruption) => {
                bail!("prompt cancelled because the owner stopped work while preparing its snapshot");
            }
            snapshot = self.snapshots.lock() => snapshot,
        };
        let config = self.current().config.clone();
        let snapshot = tokio::select! {
            biased;
            _ = self.wait_for_interruption(interruption) => {
                bail!("prompt cancelled because the owner stopped work while preparing its snapshot");
            }
            snapshot = crate::snapshots::capture(&config.workspace, &config.database) => snapshot.ok(),
        };
        let _change = self.changes.lock().await;
        ensure!(!self.is_closing(), "session is closing");
        ensure!(
            self.interruptions.load(Ordering::Acquire) == interruption,
            "prompt cancelled because the owner stopped work while preparing its snapshot"
        );
        let busy = self.busy.clone();
        let state = self.state_changes.clone();
        let message = self
            .store
            .owner_action("owner", text, self.round.subscribe(), move || {
                busy.send_replace(true);
                state.send_modify(|revision| *revision = revision.wrapping_add(1));
            })
            .await?;
        if let Some(tree) = snapshot {
            self.store.save_snapshot(message.seq, tree).await?;
        }
        // Session metadata is a convenience index, so a catalog write failure
        // must never turn a successfully committed prompt into an apparent retry.
        let _ =
            crate::session_catalog::register(&config.workspace, &config.database, &message.body);
        Ok(message.seq)
    }

    pub async fn restore_prompt(&self, seq: u64) -> Result<(String, bool)> {
        let _snapshot = self.snapshots.lock().await;
        let _change = self.changes.lock().await;
        ensure!(
            !self.is_busy(),
            "restore is available after the current work finishes"
        );
        let message = self
            .store
            .read_board(seq.saturating_sub(1), 1)
            .await?
            .into_iter()
            .next()
            .context("prompt missing")?;
        ensure!(
            message.seq == seq && message.owner && message.sender == "owner",
            "this entry is not a user prompt"
        );
        let config = self.current().config.clone();
        let snapshot = self.store.prompt_snapshot(seq).await?;
        if let Some(tree) = &snapshot {
            crate::snapshots::restore(&config.workspace, &config.database, tree).await?;
        }
        self.store.owner_action("owner-control", format!("Owner restored prompt #{seq} for editing{}. Previous board history remains as an audit record.", if snapshot.is_some() {" and reverted the workspace to its pre-prompt snapshot"} else {""}), self.round.subscribe(), || {}).await?;
        Ok((message.body, snapshot.is_some()))
    }

    pub async fn switch(&self, mut config: Config) -> Result<()> {
        let _change = self.changes.lock().await;
        let current = self.current();
        ensure!(!*self.stop.borrow(), "session is closing");
        ensure!(
            config.agents == current.config.agents && config.workspace == current.config.workspace,
            "model changes cannot change the workspace or swarm size"
        );
        config.objective = current.config.objective.clone();
        config.validate()?;
        let provider = make_provider(&config)?.map(|provider| match &current.provider {
            Some(existing) => provider.share_transport(existing),
            None => provider,
        });
        let body = format!("Owner selected {}/{} with thinking {}. Preserve workspace/history and continue coordinated work with the new model after current operations finish.", config.provider, config.model, config.variant.as_deref().unwrap_or("default"));
        let next = Arc::new(ActiveModel {
            config: Arc::new(config),
            provider,
            revision: current.revision + 1,
        });
        let models = self.models.clone();
        if !self.store.has_started().await? {
            models.send_replace(next);
            return Ok(());
        }
        self.store
            .owner_action("owner-control", body, self.round.subscribe(), move || {
                models.send_replace(next);
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn state_notifications_cover_idle_controls_and_busy_transitions() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let harness = crate::runtime::Harness::new(Config {
            agents: 1,
            workspace: directory.path().to_owned(),
            database: directory.path().join("state.sqlite3"),
            interactive_session: true,
            mock: true,
            ..Config::default()
        })
        .await?;
        let control = &harness.control;
        let mut changes = control.state_receiver();
        control.pause().await?;
        changes.changed().await?;
        assert!(control.is_paused());
        control.resume().await?;
        changes.changed().await?;
        assert!(!control.is_paused());
        control.set_busy(true);
        changes.changed().await?;
        assert!(control.is_busy());
        control.set_busy(false);
        changes.changed().await?;
        assert!(!control.is_busy());
        control.detach();
        changes.changed().await?;
        assert!(control.is_closing());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn snapshot_preparation_does_not_block_controls_or_escape_an_acknowledged_stop(
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let harness = crate::runtime::Harness::new(Config {
            agents: 1,
            workspace: directory.path().to_owned(),
            database: directory.path().join("snapshot-controls.sqlite3"),
            objective: String::new(),
            interactive_session: true,
            mock: true,
            ..Config::default()
        })
        .await?;
        let control = harness.control.clone();
        let mut state_changes = control.state_receiver();
        let capture = control.snapshots.lock().await;
        let posting = {
            let control = control.clone();
            tokio::spawn(async move { control.post_prompt("task pending snapshot".into()).await })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while !control.is_busy() {
                state_changes.changed().await.unwrap();
            }
        })
        .await?;
        tokio::time::timeout(Duration::from_secs(1), control.pause()).await??;
        assert!(control.is_paused());
        tokio::time::timeout(Duration::from_secs(1), control.stop_work()).await??;
        assert!(control.is_stopping());
        assert!(
            !control.try_begin_work().await,
            "late round admission cannot erase an acknowledged stop"
        );
        assert!(control.work_was_stopped());
        let error = tokio::time::timeout(Duration::from_secs(2), posting)
            .await??
            .unwrap_err();
        assert!(error.to_string().contains("prompt cancelled"));
        drop(capture);
        assert!(
            harness.store.prompts().await?.is_empty(),
            "stopped in-preparation prompt is never committed later"
        );
        Ok(())
    }
}
