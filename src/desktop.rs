//! Transport-independent desktop facade over the same harness used by the TUI.
//!
//! No WebView or frontend dependency belongs here. Notifications are watch-based:
//! consumers may coalesce updates, then read a fresh bounded snapshot.
use std::{path::PathBuf, sync::Arc};

use anyhow::{ensure, Context, Result};
use serde::Serialize;
use tokio::{
    sync::{watch, Mutex},
    task::JoinHandle,
};

use crate::{
    config::Config,
    metrics::Metrics,
    runtime::{Harness, RunSummary},
    session::SessionControl,
    storage::{BoardMessage, Store, Vote},
};

const BOARD_PREVIEW_MESSAGES: usize = 256;
const STREAM_PREVIEW_BYTES: usize = 512;

fn runtime_error_message(error: &anyhow::Error) -> String {
    // Nested provider diagnostics may contain credential-bearing URLs or headers.
    // Match the native IPC top-level-only policy.
    error.to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DesktopState {
    Idle,
    Running,
    Paused,
    Stopping,
}

#[derive(Clone, Debug, Serialize)]
pub struct DesktopAgent {
    pub id: String,
    pub status: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub tools: u64,
    pub retries: u64,
    pub detail: String,
    pub stream: String,
}

/// Deliberately excludes Config: it contains provider secrets and OAuth state.
#[derive(Clone, Debug, Serialize)]
pub struct DesktopSnapshot {
    pub session_id: Option<String>,
    pub state: DesktopState,
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub model: String,
    pub provider: String,
    pub variant: Option<String>,
    pub mock: bool,
    pub agents: Vec<DesktopAgent>,
    pub votes: Vec<Vote>,
    pub board: Vec<BoardMessage>,
    pub prompts: Vec<BoardMessage>,
    pub latest_seq: u64,
    pub runtime_error: Option<String>,
}

pub struct DesktopSession {
    control: SessionControl,
    store: Store,
    metrics: Arc<Metrics>,
    changes: watch::Sender<u64>,
    runtime: Mutex<Option<JoinHandle<Result<RunSummary>>>>,
    runtime_error: Arc<Mutex<Option<String>>>,
    session_id: Mutex<Option<String>>,
}

impl DesktopSession {
    /// Interactive construction never starts an empty objective automatically.
    pub async fn new(mut config: Config) -> Result<Arc<Self>> {
        config.interactive_session = true;
        let harness = Harness::new(config).await?;
        let control = harness.control.clone();
        let store = harness.store.clone();
        let metrics = harness.metrics.clone();
        let (changes, _) = watch::channel(0_u64);
        let runtime_error = Arc::new(Mutex::new(None));
        // Subscribe before spawning workers, including nonempty initial objectives.
        let mut board = store.subscribe_board();
        let mut telemetry = metrics.subscribe_changes();
        let mut state = control.state_receiver();
        let mut model = control.subscribe();
        let mut membership = control.membership_receiver();
        let mut stop = control.stop_receiver();
        let runtime = tokio::spawn({
            let changes = changes.clone();
            let runtime_error = runtime_error.clone();
            async move {
                let result = harness.run().await;
                if let Err(error) = &result {
                    *runtime_error.lock().await = Some(runtime_error_message(error));
                }
                changes.send_modify(|revision| *revision = revision.wrapping_add(1));
                result
            }
        });
        tokio::spawn({
            let changes = changes.clone();
            async move {
                loop {
                    if *stop.borrow() {
                        break;
                    }
                    let changed = tokio::select! {
                        result = board.changed() => result,
                        result = telemetry.changed() => result,
                        result = state.changed() => result,
                        result = model.changed() => result,
                        result = membership.changed() => result,
                        result = stop.changed() => result,
                    };
                    if changed.is_err() {
                        break;
                    }
                    changes.send_modify(|revision| *revision = revision.wrapping_add(1));
                }
            }
        });
        Ok(Arc::new(Self {
            control,
            store,
            metrics,
            changes,
            runtime: Mutex::new(Some(runtime)),
            runtime_error,
            session_id: Mutex::new(None),
        }))
    }

    pub fn control(&self) -> &SessionControl {
        &self.control
    }
    pub fn store(&self) -> &Store {
        &self.store
    }
    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.metrics
    }
    pub fn subscribe_changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// The host supplies the ID returned by the shared session catalog.
    pub async fn set_session_id(&self, id: String) {
        *self.session_id.lock().await = Some(id);
        self.changes
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    pub async fn snapshot(&self) -> Result<DesktopSnapshot> {
        let current = self.control.current();
        let members = self.control.members();
        let telemetry = self
            .metrics
            .snapshot_for_members(&members)
            .agents
            .into_iter()
            .map(|agent| (agent.id.clone(), agent))
            .collect::<std::collections::HashMap<_, _>>();
        // Membership commits before a newly admitted worker allocates telemetry.
        // Never omit that durable member merely because its first metric is pending.
        let agents = members
            .into_iter()
            .map(|id| {
                let agent = telemetry.get(&id);
                DesktopAgent {
                    detail: self.metrics.agent_detail(&id),
                    stream: self.metrics.agent_tail(&id, STREAM_PREVIEW_BYTES),
                    status: agent
                        .map_or("starting", |agent| agent.status.label())
                        .to_owned(),
                    input_tokens: agent.map_or(0, |agent| agent.input_tokens),
                    output_tokens: agent.map_or(0, |agent| agent.output_tokens),
                    cached_tokens: agent.map_or(0, |agent| agent.cached_tokens),
                    tools: agent.map_or(0, |agent| agent.tools),
                    retries: agent.map_or(0, |agent| agent.retries),
                    id,
                }
            })
            .collect();
        let latest_seq = self.store.latest_seq().await?;
        let board = self
            .store
            .read_board(
                latest_seq.saturating_sub(BOARD_PREVIEW_MESSAGES as u64),
                BOARD_PREVIEW_MESSAGES,
            )
            .await?;
        let state = if self.control.is_stopping() {
            DesktopState::Stopping
        } else if self.control.is_paused() {
            DesktopState::Paused
        } else if self.control.is_busy() {
            DesktopState::Running
        } else {
            DesktopState::Idle
        };
        Ok(DesktopSnapshot {
            session_id: self.session_id.lock().await.clone(),
            state,
            workspace: current.config.workspace.clone(),
            database: current.config.database.clone(),
            model: current.config.model.clone(),
            provider: current.config.provider.clone(),
            variant: current.config.variant.clone(),
            mock: current.config.mock,
            agents,
            votes: self.store.votes().await?,
            board,
            prompts: self.store.prompts().await?,
            latest_seq,
            runtime_error: self.runtime_error.lock().await.clone(),
        })
    }

    /// Read only an explicitly inspected agent's full disk-spooled activity.
    /// Disk I/O is kept off the asynchronous runtime's worker threads.
    pub async fn activity(&self, id: &str) -> Result<String> {
        ensure!(
            self.control.members().iter().any(|member| member == id)
                || self
                    .control
                    .draining_members()
                    .iter()
                    .any(|member| member == id),
            "unknown agent {id}"
        );
        let metrics = self.metrics.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || metrics.agent_activity(&id))
            .await
            .context("reading agent activity")
    }

    /// A bounded live transcript includes generation, tools and command/PTY output.
    pub async fn activity_tail(&self, id: &str, max_bytes: usize) -> Result<String> {
        ensure!(
            self.control.members().iter().any(|member| member == id)
                || self
                    .control
                    .draining_members()
                    .iter()
                    .any(|member| member == id),
            "unknown agent {id}"
        );
        let metrics = self.metrics.clone();
        let id = id.to_owned();
        let max_bytes = max_bytes.clamp(1, 64 * 1024);
        tokio::task::spawn_blocking(move || metrics.agent_activity_tail(&id, max_bytes))
            .await
            .context("reading live agent activity")
    }

    /// Gracefully detach workers, then flush the shared session database.
    pub async fn shutdown(&self) -> Result<()> {
        self.control.detach();
        let mut runtime = self.runtime.lock().await;
        let result = if let Some(task) = runtime.take() {
            task.await
                .context("joining desktop runtime")
                .and_then(|result| result.map(|_| ()))
        } else {
            Ok(())
        };
        self.store.flush().await?;
        result
    }
}

impl Drop for DesktopSession {
    fn drop(&mut self) {
        // A failed window/host must not leave an unreachable swarm running.
        // Explicit shutdown additionally joins workers and flushes persistence.
        self.control.detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_failure_does_not_expose_nested_provider_diagnostics() {
        let error =
            anyhow::anyhow!("request failed: https://provider.example/?api_key=DESKTOP_SECRET")
                .context("Provider request failed");
        assert_eq!(runtime_error_message(&error), "Provider request failed");
    }
}
