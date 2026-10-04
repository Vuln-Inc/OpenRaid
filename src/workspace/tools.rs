//! Native tools share one durable, unpartitioned board. The caller supplies the
//! authenticated identity; model arguments cannot impersonate the owner.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::{metrics::Metrics, storage::Store, workspace::WorkspaceTools};

const MAX_BOARD_PAGE: usize = 128;
const MAX_POST_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct ToolBus {
    store: Store,
    workspace: WorkspaceTools,
    // Only scalar delivery cursors live here, never a replicated board.
    delivered: Arc<Mutex<HashMap<String, u64>>>,
    mcp: crate::mcp::Hub,
    metrics: Option<Arc<Metrics>>,
}

impl ToolBus {
    pub fn new(store: Store, workspace: WorkspaceTools) -> Self {
        Self {
            store,
            workspace,
            delivered: Arc::new(Mutex::new(HashMap::new())),
            mcp: crate::mcp::Hub::default(),
            metrics: None,
        }
    }

    pub fn with_mcp(mut self, mcp: crate::mcp::Hub) -> Self {
        self.mcp = mcp;
        self
    }

    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    pub fn definitions(&self) -> Vec<Value> {
        let mut tools = vec![
            definition(
                "board_read",
                "Read the shared global board in sequence order. All agents see the same messages. Only cursor and page size pagination exist. Coordinate contributions through the board; new messages do not block workspace or MCP tools. Read subsequent pages while has_more is true before a positive completion vote.",
                json!({
                    "after": {"type":"integer", "minimum":0, "description":"Exclusive sequence cursor, initially zero."},
                    "limit": {"type":"integer", "minimum":1, "maximum":MAX_BOARD_PAGE, "description":"Page size, default 64."}
                }),
                &[],
            ),
            definition(
                "board_post",
                "Append a message to the global board under your authenticated identity. Discuss proposed contributions and boundaries organically with peers before changes. All messages remain durably available to every agent.",
                json!({"body":{"type":"string", "minLength":1, "description":"Complete message text. No channel or topic routing."}}),
                &["body"],
            ),
            definition(
                "vote_done",
                "Cast or withdraw your completion vote. A positive vote requires evidence and a fully current board. Once accepted, the worker parks until consensus or new owner instructions; ordinary peer messages preserve the vote. Voting never exits a worker: the harness alone enforces consensus and its grace period. Withdraw a vote when further work is needed.",
                json!({
                    "done":{"type":"boolean"},
                    "evidence":{"type":"string", "description":"Verification evidence for completion, or reason for withdrawing."}
                }),
                &["done", "evidence"],
            ),
        ];
        tools.extend(WorkspaceTools::schemas());
        tools.extend(self.mcp.definitions());
        tools
    }

    /// Runtime calls this only after delivering every intervening board message.
    /// Native board_read also advances the cursor, but skipping pages does not.
    pub fn observe_board(&self, agent_id: &str, seq: u64) {
        let mut cursors = self.delivered.lock().unwrap_or_else(|e| e.into_inner());
        let cursor = cursors.entry(agent_id.to_owned()).or_default();
        *cursor = (*cursor).max(seq);
    }

    pub async fn execute(&self, agent_id: &str, name: &str, args: &Value) -> Result<Value> {
        validate_identity(agent_id)?;
        if self.mcp.contains(name) {
            return self.mcp.call(name, args).await;
        }
        match name {
            "board_read" => {
                let object = arguments(args, &["after", "limit"])?;
                let after = optional_u64(object, "after", 0)?;
                let limit = optional_u64(object, "limit", 64)?;
                if !(1..=MAX_BOARD_PAGE as u64).contains(&limit) {
                    bail!("limit must be between 1 and {MAX_BOARD_PAGE}");
                }
                let latest_before = self.store.latest_seq().await?;
                if after > latest_before {
                    bail!("cursor {after} is beyond the latest board sequence {latest_before}");
                }
                let entries = self.store.read_board(after, limit as usize).await?;
                let next_cursor = entries.last().map_or(after, |entry| entry.seq);
                let latest_seq = self.store.latest_seq().await?;
                self.advance_contiguous(agent_id, after, next_cursor);
                Ok(json!({
                    "messages": entries,
                    "next_cursor": next_cursor,
                    "latest_seq": latest_seq,
                    "has_more": next_cursor < latest_seq,
                }))
            }
            "board_post" => {
                let object = arguments(args, &["body"])?;
                let body = required_string(object, "body")?;
                if body.trim().is_empty() {
                    bail!("board message cannot be empty");
                }
                if body.len() > MAX_POST_BYTES {
                    bail!("board message exceeds {MAX_POST_BYTES} bytes; post complete consecutive parts");
                }
                let entry = self.store.append(agent_id, body, false).await?;
                // Seeing one's append must not silently skip unread peer posts.
                self.advance_contiguous(agent_id, entry.seq.saturating_sub(1), entry.seq);
                Ok(json!({"message":entry}))
            }
            "vote_done" => {
                let object = arguments(args, &["done", "evidence"])?;
                let done = object
                    .get("done")
                    .and_then(Value::as_bool)
                    .context("done must be a boolean")?;
                let evidence = required_string(object, "evidence")?;
                if done && evidence.trim().is_empty() {
                    bail!("a completion vote requires verification evidence");
                }
                if done {
                    let seen = self.delivered_cursor(agent_id);
                    // SQLite compares and records in one transaction, so an
                    // intervening owner update cannot revive stale evidence.
                    self.store
                        .set_vote_at(agent_id, true, evidence, seen)
                        .await?;
                } else {
                    self.store.set_vote(agent_id, false, evidence).await?;
                }
                Ok(
                    json!({"accepted":true, "done":done, "worker_must_remain_active":true, "worker_must_park":done}),
                )
            }
            "write_file" | "apply_patch" | "run_command" | "pty_spawn" | "pty_write"
            | "pty_resize" | "pty_kill" | "read_file" | "list_files" | "search_files"
            | "pty_read" | "pty_list" => {
                // Coordination is advisory during work: peer traffic must not
                // prevent progress. Only positive votes require a current board.
                match &self.metrics {
                    Some(metrics) => {
                        self.workspace
                            .execute_with_activity(agent_id, name, args, metrics.clone())
                            .await
                    }
                    None => self.workspace.execute(name, args).await,
                }
            }
            _ => bail!("unknown native tool: {name}"),
        }
    }

    fn advance_contiguous(&self, agent_id: &str, after: u64, next: u64) {
        let mut cursors = self.delivered.lock().unwrap_or_else(|e| e.into_inner());
        let cursor = cursors.entry(agent_id.to_owned()).or_default();
        if after <= *cursor {
            *cursor = (*cursor).max(next);
        }
    }

    fn delivered_cursor(&self, agent_id: &str) -> u64 {
        self.delivered
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent_id)
            .copied()
            .unwrap_or_default()
    }
}

fn definition(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type":"function",
        "function":{
            "name":name,
            "description":description,
            "parameters":{
                "type":"object", "properties":properties,
                "required":required, "additionalProperties":false,
            }
        }
    })
}

fn validate_identity(agent_id: &str) -> Result<()> {
    if agent_id.trim().is_empty()
        || ["owner", "system", "user"].contains(&agent_id.to_ascii_lowercase().as_str())
    {
        bail!("native agent tools require an authenticated agent identity");
    }
    Ok(())
}

fn arguments<'a>(args: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>> {
    let object = args
        .as_object()
        .context("tool arguments must be a JSON object")?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            bail!("unsupported argument: {key}");
        }
    }
    Ok(object)
}

fn optional_u64(args: &Map<String, Value>, key: &str, default: u64) -> Result<u64> {
    args.get(key).map_or(Ok(default), |value| {
        value
            .as_u64()
            .with_context(|| format!("{key} must be a nonnegative integer"))
    })
}

fn required_string<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("{key} must be a string"))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> (tempfile::TempDir, ToolBus) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("swarm.sqlite")).await.unwrap();
        let workspace = WorkspaceTools::new(root.path(), 2).unwrap();
        (root, ToolBus::new(store, workspace))
    }

    #[tokio::test]
    async fn process_activity_is_live_and_authenticated_before_tool_finishes() {
        let (root, bus) = fixture().await;
        let metrics = Arc::new(Metrics::new(2));
        let bus = bus.with_metrics(metrics.clone());
        #[cfg(windows)]
        let args = json!({
            "program":"powershell.exe",
            "args":["-NoProfile", "-Command", "[Console]::Out.WriteLine('live-before-completion'); while (-not (Test-Path -LiteralPath 'release.txt')) { Start-Sleep -Milliseconds 10 }; [Console]::Out.WriteLine('after-release')"]
        });
        #[cfg(not(windows))]
        let args = json!({
            "program":"sh",
            "args":["-c", "printf 'live-before-completion\\n'; while [ ! -f release.txt ]; do sleep 0.01; done; printf 'after-release\\n'"]
        });
        let mut task =
            tokio::spawn(async move { bus.execute("agent-001", "run_command", &args).await });
        let observed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if metrics
                    .agent_activity("agent-001")
                    .contains("live-before-completion")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        // Always release the process, including a failure, so this test leaves no child behind.
        let was_running = !task.is_finished();
        tokio::fs::write(root.path().join("release.txt"), "release")
            .await
            .unwrap();
        let result = match tokio::time::timeout(std::time::Duration::from_secs(10), &mut task).await
        {
            Ok(result) => result.unwrap().unwrap(),
            Err(_) => {
                task.abort();
                let _ = task.await;
                panic!("released command did not finish");
            }
        };
        assert!(
            observed.is_ok(),
            "process output was not observable before completion"
        );
        assert!(
            was_running,
            "stream was inspected only after tool completion"
        );
        assert_eq!(result["success"], true);
        assert!(metrics
            .agent_activity("agent-001")
            .contains("after-release"));
        assert!(metrics.agent_activity("agent-002").is_empty());
    }

    #[tokio::test]
    async fn skipped_pages_and_own_posts_do_not_hide_unread_messages() {
        let (_root, bus) = fixture().await;
        bus.store
            .append("peer", "first peer message", false)
            .await
            .unwrap();
        bus.store
            .append("peer", "second peer message", false)
            .await
            .unwrap();
        bus.execute("agent-001", "board_read", &json!({"after":1,"limit":1}))
            .await
            .unwrap();
        bus.execute("agent-001", "board_post", &json!({"body":"my proposal"}))
            .await
            .unwrap();
        assert!(bus
            .execute(
                "agent-001",
                "vote_done",
                &json!({"done":true,"evidence":"tests passed"})
            )
            .await
            .is_err());
        let first = bus
            .execute("agent-001", "board_read", &json!({"after":0,"limit":1}))
            .await
            .unwrap();
        assert_eq!(first["messages"][0]["body"], "first peer message");
        assert_eq!(first["has_more"], true);
        bus.execute("agent-001", "board_read", &json!({"after":1}))
            .await
            .unwrap();
        assert!(bus
            .execute(
                "agent-001",
                "vote_done",
                &json!({"done":true,"evidence":"tests passed"})
            )
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn board_is_global_and_identity_and_routing_arguments_are_rejected() {
        let (_root, bus) = fixture().await;
        bus.execute(
            "agent-001",
            "board_post",
            &json!({"body":"hello all peers"}),
        )
        .await
        .unwrap();
        for agent in ["agent-001", "agent-002", "agent-003"] {
            let page = bus.execute(agent, "board_read", &json!({})).await.unwrap();
            assert_eq!(page["messages"].as_array().unwrap().len(), 1);
            assert_eq!(page["messages"][0]["sender"], "agent-001");
            assert_eq!(page["messages"][0]["owner"], false);
        }
        assert!(bus
            .execute(
                "agent-001",
                "board_post",
                &json!({"body":"spoof","owner":true})
            )
            .await
            .is_err());
        assert!(bus
            .execute("agent-001", "board_read", &json!({"topic":"hidden"}))
            .await
            .is_err());
        assert!(bus
            .execute("owner", "board_post", &json!({"body":"spoof"}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn withdrawal_is_allowed_when_new_work_arrives_but_positive_vote_requires_evidence() {
        let (_root, bus) = fixture().await;
        assert!(bus
            .execute(
                "agent-001",
                "vote_done",
                &json!({"done":true,"evidence":" "})
            )
            .await
            .is_err());
        bus.store.append("peer", "more work", false).await.unwrap();
        assert!(bus
            .execute(
                "agent-001",
                "vote_done",
                &json!({"done":false,"evidence":"investigating"})
            )
            .await
            .is_ok());
        assert!(bus
            .execute("agent-001", "run_command", &json!({"command":"echo stale"}))
            .await
            .is_ok());
        assert!(bus
            .execute(
                "agent-001",
                "vote_done",
                &json!({"done":true,"evidence":"tests passed"})
            )
            .await
            .is_err());
        bus.observe_board("agent-001", 1);
        assert!(bus
            .execute(
                "agent-001",
                "vote_done",
                &json!({"done":true,"evidence":"tests passed"})
            )
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn intervening_owner_revision_cannot_reuse_delivery_evidence() {
        let (_root, bus) = fixture().await;
        bus.store
            .append("owner", "original objective", true)
            .await
            .unwrap();
        bus.execute("agent-001", "board_read", &json!({}))
            .await
            .unwrap();
        bus.execute(
            "agent-001",
            "vote_done",
            &json!({"done":true,"evidence":"verified original objective"}),
        )
        .await
        .unwrap();
        bus.store
            .append("owner", "additional requirement", true)
            .await
            .unwrap();
        assert!(bus
            .execute(
                "agent-001",
                "vote_done",
                &json!({"done":true,"evidence":"old evidence"})
            )
            .await
            .is_err());
        assert!(bus.store.votes().await.unwrap().is_empty());
        bus.execute("agent-001", "board_read", &json!({"after":1}))
            .await
            .unwrap();
        bus.execute(
            "agent-001",
            "vote_done",
            &json!({"done":true,"evidence":"verified new requirement"}),
        )
        .await
        .unwrap();
        assert_eq!(bus.store.votes().await.unwrap()[0].board_seq, 2);
    }
}
