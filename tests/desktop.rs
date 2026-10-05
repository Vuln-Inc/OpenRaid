//! The native desktop facade must remain a credential-free view of the same runtime.
use anyhow::Result;
use openraid::{config::Config, desktop::DesktopSession, storage::Store};
use serde_json::Value;
use std::time::Duration;

fn config(root: &std::path::Path, agents: usize) -> Config {
    Config {
        agents,
        workspace: root.to_owned(),
        database: root.join(".openraid/session.sqlite3"),
        mock: true,
        interactive_session: true,
        grace_period: Duration::ZERO,
        ..Config::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_snapshot_never_serializes_provider_credentials() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut settings = config(root.path(), 2);
    settings.api_key = Some("DESKTOP_SECRET_API_KEY".into());
    settings.provider_headers.insert(
        "Authorization".into(),
        "Bearer DESKTOP_SECRET_HEADER".into(),
    );
    settings.provider_options = serde_json::json!({"apiKey":"DESKTOP_SECRET_OPTION"});
    let desktop = DesktopSession::new(settings).await?;
    let snapshot = serde_json::to_value(desktop.snapshot().await?)?;
    let serialized = snapshot.to_string();
    assert_eq!(snapshot["state"], "IDLE");
    assert_eq!(snapshot["mock"], true);
    assert_eq!(snapshot["agents"].as_array().unwrap().len(), 2);
    for forbidden in [
        "DESKTOP_SECRET_API_KEY",
        "DESKTOP_SECRET_HEADER",
        "DESKTOP_SECRET_OPTION",
        "api_key",
        "oauth",
        "provider_headers",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "snapshot leaked {forbidden}"
        );
    }
    desktop.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_reads_existing_cli_database_without_replaying_prompts() -> Result<()> {
    let root = tempfile::tempdir()?;
    let settings = config(root.path(), 2);
    let store = Store::open(&settings.database).await?;
    store.append("owner", "historical CLI prompt", true).await?;
    store
        .append("agent-001", "historical CLI board entry", false)
        .await?;
    let desktop = DesktopSession::new(settings.clone()).await?;
    let snapshot = serde_json::to_value(desktop.snapshot().await?)?;
    assert_eq!(snapshot["state"], "IDLE");
    assert!(snapshot["board"].as_array().unwrap().iter().any(|entry| {
        entry["sender"] == "agent-001" && entry["body"] == "historical CLI board entry"
    }));
    assert_eq!(snapshot["prompts"][0]["body"], "historical CLI prompt");
    assert_eq!(desktop.metrics().snapshot().tools, 0);
    desktop.shutdown().await?;
    let reopened = Store::open(&settings.database).await?;
    assert_eq!(reopened.prompts().await?.len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_pushes_committed_board_and_metrics_changes() -> Result<()> {
    let root = tempfile::tempdir()?;
    let desktop = DesktopSession::new(config(root.path(), 2)).await?;
    let mut changes = desktop.subscribe_changes();
    changes.borrow_and_update();
    desktop
        .store()
        .append("agent-002", "live desktop board", false)
        .await?;
    tokio::time::timeout(Duration::from_secs(5), changes.changed()).await??;
    let snapshot = serde_json::to_value(desktop.snapshot().await?)?;
    assert!(snapshot["board"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| { entry["body"] == "live desktop board" }));
    changes.borrow_and_update();
    desktop
        .metrics()
        .append_output("agent-002", "live generation 界");
    tokio::time::timeout(Duration::from_secs(5), changes.changed()).await??;
    let snapshot = serde_json::to_value(desktop.snapshot().await?)?;
    let agent = snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["id"] == "agent-002")
        .unwrap();
    assert!(agent["stream"]
        .as_str()
        .unwrap()
        .contains("live generation 界"));
    desktop.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_rejects_blank_prompts_and_invalid_membership_without_mutation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let desktop = DesktopSession::new(config(root.path(), 2)).await?;
    assert!(desktop
        .control()
        .post_prompt(" \n\t ".into())
        .await
        .is_err());
    assert!(desktop.control().add_agents(499).await.is_err());
    assert!(desktop
        .control()
        .remove_agents(vec!["agent-does-not-exist".into()])
        .await
        .is_err());
    assert_eq!(desktop.control().members().len(), 2);
    assert!(desktop.store().prompts().await?.is_empty());
    assert!(!desktop.control().is_busy());
    desktop.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_500_agent_snapshot_uses_bounded_utf8_previews() -> Result<()> {
    let root = tempfile::tempdir()?;
    let desktop = DesktopSession::new(config(root.path(), 500)).await?;
    desktop
        .metrics()
        .append_output("agent-500", &"界".repeat(20_000));
    let snapshot: Value = serde_json::to_value(desktop.snapshot().await?)?;
    let agents = snapshot["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 500);
    let agent = agents
        .iter()
        .find(|agent| agent["id"] == "agent-500")
        .unwrap();
    let stream = agent["stream"].as_str().unwrap();
    assert!(!stream.is_empty());
    assert!(
        stream.len() <= 16 * 1024,
        "stream previews must stay bounded"
    );
    assert!(!stream.contains('\u{fffd}'));
    desktop.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_state_events_cover_pause_and_resume_without_creating_prompts() -> Result<()> {
    let root = tempfile::tempdir()?;
    let desktop = DesktopSession::new(config(root.path(), 2)).await?;
    let mut changes = desktop.subscribe_changes();
    changes.borrow_and_update();
    desktop.control().pause().await?;
    tokio::time::timeout(Duration::from_secs(5), changes.changed()).await??;
    assert_eq!(
        serde_json::to_value(desktop.snapshot().await?)?["state"],
        "PAUSED"
    );
    changes.borrow_and_update();
    desktop.control().resume().await?;
    tokio::time::timeout(Duration::from_secs(5), changes.changed()).await??;
    assert_eq!(
        serde_json::to_value(desktop.snapshot().await?)?["state"],
        "IDLE"
    );
    assert!(desktop.store().prompts().await?.is_empty());
    assert!(desktop.activity("../unknown-agent").await.is_err());
    desktop.shutdown().await?;
    // Host close and native-window close can arrive concurrently; repeated close
    // must remain harmless after the runtime handle has already been joined.
    desktop.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_mock_round_completes_and_persists_for_terminal_reopen() -> Result<()> {
    let root = tempfile::tempdir()?;
    let settings = config(root.path(), 2);
    let desktop = DesktopSession::new(settings.clone()).await?;
    let mut changes = desktop.subscribe_changes();
    desktop
        .control()
        .post_prompt("verify desktop runtime handoff".into())
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = desktop.snapshot().await?;
            if !desktop.control().is_busy()
                && snapshot
                    .board
                    .iter()
                    .any(|entry| entry.body == "all workers drained; swarm complete")
            {
                assert_eq!(snapshot.votes.iter().filter(|vote| vote.done).count(), 2);
                break;
            }
            changes.changed().await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    desktop.shutdown().await?;
    let store = Store::open(&settings.database).await?;
    assert_eq!(
        store.prompts().await?[0].body,
        "verify desktop runtime handoff"
    );
    assert!(store.unfinished_prompt().await?.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_membership_events_follow_committed_runtime_roster() -> Result<()> {
    let root = tempfile::tempdir()?;
    let desktop = DesktopSession::new(config(root.path(), 2)).await?;
    let mut changes = desktop.subscribe_changes();
    changes.borrow_and_update();
    let added = desktop.control().add_agents(1).await?;
    tokio::time::timeout(Duration::from_secs(5), changes.changed()).await??;
    let snapshot = desktop.snapshot().await?;
    assert_eq!(snapshot.agents.len(), 3);
    assert!(snapshot.agents.iter().any(|agent| agent.id == added[0]));
    changes.borrow_and_update();
    desktop.control().remove_agents(added.clone()).await?;
    tokio::time::timeout(Duration::from_secs(5), changes.changed()).await??;
    let snapshot = desktop.snapshot().await?;
    assert_eq!(snapshot.agents.len(), 2);
    assert!(!snapshot.agents.iter().any(|agent| agent.id == added[0]));
    desktop.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_live_activity_tail_contains_tools_and_pty_without_unbounded_reads() -> Result<()> {
    let root = tempfile::tempdir()?;
    let desktop = DesktopSession::new(config(root.path(), 2)).await?;
    let metrics = desktop.metrics();
    metrics.append_output("agent-001", &"older generation 界".repeat(5000));
    metrics.record_tool_start("agent-001", "pty_spawn", "{\"command\":\"fixture\"}");
    metrics.append_activity("agent-001", "PTY_LIVE_OUTPUT 界\n");
    metrics.record_tool_result("agent-001", "pty_spawn", "{\"exit_code\":0}");
    let tail = desktop.activity_tail("agent-001", 1024).await?;
    assert!(tail.len() <= 1024);
    assert!(tail.contains("[tool] pty_spawn"));
    assert!(tail.contains("PTY_LIVE_OUTPUT 界"));
    assert!(tail.contains("[result] pty_spawn"));
    assert!(!tail.contains('\u{fffd}'));
    let clamped = desktop.activity_tail("agent-001", usize::MAX).await?;
    assert!(clamped.len() <= 64 * 1024);
    assert!(desktop.activity_tail("unknown-agent", 1024).await.is_err());
    desktop.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_model_selection_snapshot_tracks_variant_changes_and_default() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut settings = config(root.path(), 2);
    settings.variant = Some("high".into());
    let desktop = DesktopSession::new(settings).await?;
    let initial = desktop.snapshot().await?;
    assert_eq!(initial.variant.as_deref(), Some("high"));

    let mut changes = desktop.subscribe_changes();
    changes.borrow_and_update();
    let mut selection = (*desktop.control().current().config).clone();
    selection.model = "desktop-model-selection".into();
    selection.variant = Some("low".into());
    desktop.control().switch(selection.clone()).await?;
    tokio::time::timeout(Duration::from_secs(5), changes.changed()).await??;
    let selected = desktop.snapshot().await?;
    assert!(selected.mock);
    assert_eq!(selected.provider, selection.provider);
    assert_eq!(selected.model, selection.model);
    assert_eq!(selected.variant.as_deref(), Some("low"));

    changes.borrow_and_update();
    selection.variant = None;
    desktop.control().switch(selection).await?;
    tokio::time::timeout(Duration::from_secs(5), changes.changed()).await??;
    let default = serde_json::to_value(desktop.snapshot().await?)?;
    assert!(default["variant"].is_null());
    assert_eq!(default["mock"], true);
    assert!(desktop.store().prompts().await?.is_empty());
    desktop.shutdown().await?;
    Ok(())
}
