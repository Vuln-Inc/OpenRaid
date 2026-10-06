//! Reopening a session restores its inspector history without dispatching work.
use anyhow::{ensure, Context, Result};
use openraid::{
    config::Config,
    desktop::DesktopSession,
    metrics::{AgentStatus, Metrics},
    runtime::Harness,
};
use serde_json::json;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[test]
fn durable_activity_retains_full_transcript_and_cumulative_telemetry_across_reopen() -> Result<()> {
    let root = tempfile::tempdir()?;
    let database = root.path().join("session.sqlite3");
    let metrics = Metrics::persistent(&database)?;
    metrics.add_agent("agent-001");
    metrics.append_output("agent-001", "early generation sentinel\n");
    metrics.record_tool_start("agent-001", "pty_spawn", "{\"command\":\"fixture\"}");
    metrics.append_activity("agent-001", "[stdout] complete PTY history 界\n");
    metrics.record_tool_result("agent-001", "pty_spawn", "{\"exit_code\":0}");
    metrics.append_output("agent-001", &"界".repeat(20_000));
    metrics.record_usage("agent-001", 100, 40, 25);
    metrics.record_tool("agent-001");
    metrics.record_retry("agent-001");
    metrics.set_status("agent-001", AgentStatus::Waiting);
    metrics.set_detail("agent-001", "paused at a safe boundary");
    metrics.persist_agent("agent-001")?;
    assert!(
        !openraid::metrics::activity_directory(&database)
            .join("metrics.json")
            .exists(),
        "a worker checkpoint does not rewrite whole-swarm metadata"
    );
    let checkpoint_view = Metrics::persistent(&database)?;
    assert_eq!(
        checkpoint_view.snapshot().input_tokens,
        100,
        "per-agent checkpoint must restore before the writer drops"
    );
    assert_eq!(
        checkpoint_view.agent_output("agent-001"),
        metrics.agent_output("agent-001")
    );
    drop(checkpoint_view);
    let transcript = metrics.agent_activity("agent-001");
    let preview = metrics.agent_output("agent-001");
    drop(metrics);

    let restored = Metrics::persistent(&database)?;
    assert_eq!(restored.agent_activity("agent-001"), transcript);
    assert_eq!(restored.agent_output("agent-001"), preview);
    assert_eq!(
        restored.agent_detail("agent-001"),
        "paused at a safe boundary"
    );
    let snapshot = restored.snapshot();
    assert_eq!(
        (
            snapshot.input_tokens,
            snapshot.output_tokens,
            snapshot.cached_tokens
        ),
        (100, 40, 25)
    );
    assert_eq!((snapshot.tools, snapshot.retries), (1, 1));
    assert_eq!(snapshot.agents[0].status, AgentStatus::Waiting);
    let tail = restored.agent_activity_tail("agent-001", 113);
    assert!(tail.len() <= 113);
    assert!(!tail.contains('\u{fffd}'));
    restored.append_output("agent-001", "continued after restore");
    restored.record_usage("agent-001", 10, 5, 0);
    restored.persist()?;
    drop(restored);
    let reopened_again = Metrics::persistent(&database)?;
    assert!(reopened_again
        .agent_activity("agent-001")
        .starts_with(&transcript));
    assert!(reopened_again
        .agent_activity("agent-001")
        .ends_with("continued after restore"));
    assert_eq!(reopened_again.snapshot().input_tokens, 110);
    Ok(())
}

#[test]
fn separate_databases_do_not_share_activity_and_log_only_recovery_is_lazy() -> Result<()> {
    let root = tempfile::tempdir()?;
    let first = root.path().join("first.sqlite3");
    let second = root.path().join("second.sqlite3");
    let directory = openraid::metrics::activity_directory(&first);
    std::fs::create_dir_all(&directory)?;
    // Simulate exit after a streamed write, before dashboard metadata is saved.
    std::fs::write(
        directory.join("agent-001.log"),
        "uncheckpointed streamed history 界",
    )?;
    let recovered = Metrics::persistent(&first)?;
    assert_eq!(
        recovered.agent_activity("agent-001"),
        "uncheckpointed streamed history 界"
    );
    assert!(!recovered.add_agent("../outside"));
    assert!(!recovered.add_agent("nested/agent"));
    assert!(!recovered.add_agent("metrics"));
    recovered.append_activity("agent-001", "\ncontinued activity");
    let isolated = Metrics::persistent(&second)?;
    isolated.add_agent("agent-001");
    assert!(isolated.agent_activity("agent-001").is_empty());
    assert!(isolated.agent_output("agent-001").is_empty());
    Ok(())
}

#[test]
fn smaller_fresh_roster_excludes_archived_slots_but_retains_durable_activity() -> Result<()> {
    let root = tempfile::tempdir()?;
    let database = root.path().join("session.sqlite3");
    let metrics = Metrics::persistent(&database)?;
    for id in ["agent-001", "agent-002"] {
        metrics.add_agent(id);
        metrics.append_output(id, &format!("history from {id}"));
        metrics.record_tool(id);
        metrics.set_status(id, AgentStatus::Finished);
    }
    metrics.persist()?;
    drop(metrics);
    let fresh = Metrics::persistent(&database)?;
    fresh.retain_agents(&["agent-001".into()]);
    assert_eq!(fresh.snapshot().agents.len(), 1);
    assert_eq!(fresh.snapshot().finished, 1);
    assert_eq!(fresh.snapshot().tools, 1);
    fresh.persist()?;
    drop(fresh);
    let restored = Metrics::persistent(&database)?;
    assert_eq!(
        restored.agent_activity("agent-002"),
        "history from agent-002"
    );
    assert_eq!(restored.snapshot().agents.len(), 2);
    Ok(())
}

fn config(root: &std::path::Path) -> Config {
    Config {
        agents: 1,
        workspace: root.to_owned(),
        database: root.join("session.sqlite3"),
        mock: true,
        interactive_session: true,
        grace_period: Duration::ZERO,
        ..Config::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_reopen_restores_history_tokens_and_pause_without_replaying_work() -> Result<()> {
    let root = tempfile::tempdir()?;
    let settings = config(root.path());
    let store = openraid::storage::Store::open(&settings.database).await?;
    store
        .append("owner", "unfinished saved objective sentinel", true)
        .await?;
    let desktop = DesktopSession::new(settings.clone()).await?;
    desktop
        .metrics()
        .append_output("agent-001", "saved response sentinel 界");
    desktop
        .metrics()
        .record_tool_start("agent-001", "run_command", "{\"command\":\"build\"}");
    desktop
        .metrics()
        .append_activity("agent-001", "saved command stdout sentinel\n");
    desktop
        .metrics()
        .record_tool_result("agent-001", "run_command", "{\"exit_code\":0}");
    desktop.metrics().record_usage("agent-001", 125, 50, 30);
    desktop.control().pause().await?;
    desktop.shutdown().await?;
    drop(desktop);

    let reopened = DesktopSession::new(settings).await?;
    let snapshot = reopened.snapshot().await?;
    assert_eq!(serde_json::to_value(&snapshot)?["state"], "PAUSED");
    assert!(!reopened.control().is_busy());
    assert_eq!(snapshot.prompts.len(), 1);
    assert_eq!(
        snapshot.prompts[0].body,
        "unfinished saved objective sentinel"
    );
    assert_eq!(snapshot.agents[0].input_tokens, 125);
    assert_eq!(snapshot.agents[0].output_tokens, 50);
    assert_eq!(snapshot.agents[0].cached_tokens, 30);
    assert!(snapshot.agents[0]
        .stream
        .contains("saved response sentinel 界"));
    let transcript = reopened.activity("agent-001").await?;
    for sentinel in [
        "saved response sentinel 界",
        "[tool] run_command",
        "saved command stdout sentinel",
        "[result] run_command",
    ] {
        assert!(
            transcript.contains(sentinel),
            "missing {sentinel}: {transcript}"
        );
    }
    assert_eq!(
        reopened.metrics().snapshot().tools,
        0,
        "opening saved history must not dispatch mock tools"
    );
    reopened.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_idle_session_recovers_existing_prompt_and_legacy_checkpoint() -> Result<()> {
    let root = tempfile::tempdir()?;
    let settings = config(root.path());
    let store = openraid::storage::Store::open(&settings.database).await?;
    store.initialize_membership(1, false).await?;
    let prompt = store.append("owner", "paused original task", true).await?;
    store.save_checkpoint("agent-001", &json!({"cursor":prompt.seq,"messages":[
        {"role":"assistant","content":"legacy checkpoint response sentinel"},
        {"role":"tool","name":"run_command","tool_call_id":"previous-command","content":"legacy tool result sentinel"}
    ]})).await?;
    store.set_paused(true).await?;
    let harness = Harness::new(settings).await?;
    let control = harness.control.clone();
    let metrics = harness.metrics.clone();
    assert!(control.is_paused());
    assert!(metrics
        .agent_activity("agent-001")
        .contains("legacy checkpoint response sentinel"));
    assert!(metrics
        .agent_activity("agent-001")
        .contains("legacy tool result sentinel"));
    let run = tokio::spawn(harness.run());
    control.resume().await?;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let completed = store
                .read_board(0, 100)
                .await?
                .iter()
                .any(|entry| entry.body == "all workers drained; swarm complete");
            if completed && !control.is_busy() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    control.detach();
    tokio::time::timeout(Duration::from_secs(5), run).await???;
    result??;
    assert_eq!(
        store.prompts().await?.len(),
        1,
        "resume uses existing prompt rather than reposting it"
    );
    assert!(store.unfinished_prompt().await?.is_none());
    assert!(!store.paused().await?);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_startup_resume_clears_durable_pause_and_continues_saved_task() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut settings = config(root.path());
    settings.resume = true;
    let store = openraid::storage::Store::open(&settings.database).await?;
    store.initialize_membership(1, false).await?;
    store.append("owner", "paused startup task", true).await?;
    store.set_paused(true).await?;
    let harness = Harness::new(settings).await?;
    let control = harness.control.clone();
    assert!(
        !control.is_paused(),
        "--resume explicitly continues paused work"
    );
    assert!(!store.paused().await?);
    let run = tokio::spawn(harness.run());
    let completion = tokio::time::timeout(Duration::from_secs(5), async {
        while store.unfinished_prompt().await?.is_some() || control.is_busy() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    control.detach();
    tokio::time::timeout(Duration::from_secs(5), run).await???;
    completion??;
    assert_eq!(store.prompts().await?.len(), 1);
    Ok(())
}

async fn read_request(socket: &mut TcpStream) -> Result<serde_json::Value> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request ended before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let length = std::str::from_utf8(&bytes[..end])?
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .context("missing content length")?;
    while bytes.len() < end + length {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request body ended early");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(&bytes[end..end + length])?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_resume_after_idle_reopen_sends_saved_tool_history_to_provider() -> Result<()> {
    let root = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut settings = config(root.path());
    settings.mock = false;
    settings.base_url = format!("http://{}/v1", listener.local_addr()?);
    let store = openraid::storage::Store::open(&settings.database).await?;
    store.initialize_membership(1, false).await?;
    let prompt = store
        .append(
            "owner",
            "original objective retained for live recovery",
            true,
        )
        .await?;
    store.save_checkpoint("agent-001", &json!({"cursor":prompt.seq,"messages":[
        {"role":"assistant","content":"saved assistant response","tool_calls":[
            {"id":"saved-command","type":"function","function":{"name":"run_command","arguments":"{\"command\":\"build\"}"}}
        ]},
        {"role":"tool","name":"run_command","tool_call_id":"saved-command","content":"saved successful command result"}
    ]})).await?;
    store.set_paused(true).await?;
    let harness = Harness::new(settings).await?;
    let control = harness.control.clone();
    let run = tokio::spawn(harness.run());
    let requests_idle = tokio::time::timeout(Duration::from_millis(100), listener.accept())
        .await
        .is_err();
    control.resume().await?;
    let request = tokio::time::timeout(Duration::from_secs(5), async {
        let (mut socket, _) = listener.accept().await?;
        let request = read_request(&mut socket).await?;
        let body = json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[
            {"id":"recovered-vote","type":"function","function":{"name":"vote_done","arguments":"{\"done\":true,\"evidence\":\"verified recovered wire history\"}"}}
        ]}}]}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
        socket.shutdown().await?;
        loop {
            if store.unfinished_prompt().await?.is_none() && !control.is_busy() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<_, anyhow::Error>(request)
    }).await;
    control.detach();
    tokio::time::timeout(Duration::from_secs(5), run).await???;
    let request = request??;
    assert!(
        requests_idle,
        "reopening a paused session must not dispatch provider requests"
    );
    let messages = request["messages"]
        .as_array()
        .context("provider messages missing")?;
    assert!(messages.iter().any(|message| message["content"]
        .as_str()
        .is_some_and(|content| content.contains("original objective retained for live recovery"))));
    assert!(messages.iter().any(|message| message["role"] == "assistant"
        && message["tool_calls"][0]["id"] == "saved-command"));
    assert!(messages.iter().any(|message| message["role"] == "tool"
        && message["tool_call_id"] == "saved-command"
        && message["content"] == "saved successful command result"));
    assert_eq!(store.prompts().await?.len(), 1);
    Ok(())
}
