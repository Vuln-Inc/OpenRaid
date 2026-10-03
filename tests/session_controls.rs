use anyhow::{ensure, Context, Result};
use openraid::{config::Config, provider::Protocol, runtime::Harness};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

async fn request(socket: &mut TcpStream) -> Result<Value> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header = std::str::from_utf8(&bytes[..end])?;
    let length = header
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        })
        .context("missing body length")?;
    while bytes.len() < end + length {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "body closed early");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(&bytes[end..end + length])?)
}
async fn response(socket: &mut TcpStream, name: &str, args: Value) -> Result<()> {
    let body = json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"verified","tool_calls":[{"id":format!("call-{name}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]}).to_string();
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;
    socket.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_switch_finishes_inflight_call_and_next_turn_uses_new_model_and_variant() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let first = TcpListener::bind("127.0.0.1:0").await?;
    let second = TcpListener::bind("127.0.0.1:0").await?;
    let base_a = format!("http://{}/v1", first.local_addr()?);
    let base_b = format!("http://{}/v1", second.local_addr()?);
    let (started_tx, started_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let server_a = tokio::spawn(async move {
        let (mut socket, _) = first.accept().await?;
        let body = request(&mut socket).await?;
        assert_eq!(body["model"], "alpha");
        started_tx.send(()).unwrap();
        finish_rx.await?;
        response(
            &mut socket,
            "board_post",
            json!({"body":"first request completed before live switch"}),
        )
        .await
    });
    let server_b = tokio::spawn(async move {
        let (mut socket, _) = second.accept().await?;
        let body = request(&mut socket).await?;
        assert_eq!(body["model"], "beta");
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |message| message["role"] == "tool" && message["tool_call_id"] == "call-board_post"
            ));
        response(
            &mut socket,
            "vote_done",
            json!({"done":true,"evidence":"verified live model switch and preserved tool history"}),
        )
        .await
    });
    let config = Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: directory.path().join("switch.sqlite3"),
        objective: "verify safe live selection".into(),
        model: "alpha".into(),
        base_url: base_a,
        protocol: Protocol::ChatCompletions,
        max_in_flight: 1,
        grace_period: Duration::ZERO,
        ..Config::default()
    };
    let harness = Harness::new(config.clone()).await?;
    let control = harness.control.clone();
    let store = harness.store.clone();
    let run = tokio::spawn(harness.run());
    started_rx.await?;
    let mut next = config;
    next.model = "beta".into();
    next.base_url = base_b;
    next.variant = Some("high".into());
    next.provider_options = json!({"reasoningEffort":"high"});
    control.switch(next).await?;
    assert_eq!(control.current().config.model, "beta");
    finish_tx.send(()).unwrap();
    server_a.await??;
    server_b.await??;
    let summary = run.await??;
    assert_eq!(summary.finished_agents, 1);
    assert_eq!(
        store.prompts().await?.len(),
        1,
        "model-control notices are not user prompts"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_session_is_idle_until_prompt_and_accepts_two_completed_rounds() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let harness = Harness::new(Config {
        agents: 2,
        workspace: directory.path().to_owned(),
        database: directory.path().join("idle.sqlite3"),
        mock: true,
        interactive_session: true,
        objective: String::new(),
        grace_period: Duration::ZERO,
        ..Config::default()
    })
    .await?;
    let control = harness.control.clone();
    let metrics = harness.metrics.clone();
    let store = harness.store.clone();
    let run = tokio::spawn(harness.run());
    tokio::task::yield_now().await;
    assert!(!control.is_busy());
    assert_eq!(metrics.snapshot().tools, 0);
    assert_eq!(store.latest_seq().await?, 0);
    for (index, text) in ["first task", "second task"].iter().enumerate() {
        control.post_prompt((*text).into()).await?;
        loop {
            let messages = store.read_board(0, 100).await?;
            let finished = messages
                .iter()
                .filter(|message| message.body == "all workers drained; swarm complete")
                .count();
            if finished == index + 1 && !control.is_busy() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            !run.is_finished(),
            "the console remains open after consensus"
        );
    }
    assert_eq!(
        store.prompts().await?.len(),
        2,
        "each submitted prompt is recorded once"
    );
    control.detach();
    assert_eq!(run.await??.finished_agents, 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switch_during_compaction_delivers_owner_notice_before_new_model_turn() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let first = TcpListener::bind("127.0.0.1:0").await?;
    let second = TcpListener::bind("127.0.0.1:0").await?;
    let base_a = format!("http://{}/v1", first.local_addr()?);
    let base_b = format!("http://{}/v1", second.local_addr()?);
    let (started_tx, started_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let server_a = tokio::spawn(async move {
        let (mut socket, _) = first.accept().await?;
        let body = request(&mut socket).await?;
        assert_eq!(body["model"], "alpha");
        assert!(body["tools"]
            .as_array()
            .is_none_or(|tools| tools.is_empty()));
        started_tx.send(()).unwrap();
        finish_rx.await?;
        let body = json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"Older completed work summarized; continue the current objective."}}]}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
        socket.shutdown().await?;
        Ok::<_, anyhow::Error>(())
    });
    let server_b = tokio::spawn(async move {
        let (mut socket, _) = second.accept().await?;
        let body = request(&mut socket).await?;
        assert_eq!(body["model"], "beta");
        assert!(
            body["messages"].as_array().unwrap().iter().any(|message| {
                message["content"].as_str().is_some_and(|text| {
                    text.contains("Owner selected openai/beta with thinking high")
                })
            }),
            "the selected model must receive the owner control notice before its next turn"
        );
        response(
            &mut socket,
            "vote_done",
            json!({"done":true,"evidence":"verified model switch during compaction"}),
        )
        .await
    });
    let config = Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: directory.path().join("compact-switch.sqlite3"),
        objective: "verify model switching during compaction".into(),
        model: "alpha".into(),
        base_url: base_a,
        resume: true,
        context_budget: 8192,
        max_output_tokens: 1024,
        grace_period: Duration::ZERO,
        ..Config::default()
    };
    let harness = Harness::new(config.clone()).await?;
    harness.store.save_checkpoint("agent-001", &json!({
        "cursor":0,
        "messages":(0..6).map(|index| json!({"role":"user","content":format!("older completed turn {index}: {}", "x".repeat(3000))})).collect::<Vec<_>>()
    })).await?;
    let control = harness.control.clone();
    let run = tokio::spawn(harness.run());
    started_rx.await?;
    let mut next = config;
    next.model = "beta".into();
    next.base_url = base_b;
    next.variant = Some("high".into());
    control.switch(next).await?;
    finish_tx.send(()).unwrap();
    server_a.await??;
    server_b.await??;
    assert_eq!(run.await??.finished_agents, 1);
    Ok(())
}

#[tokio::test]
async fn invalid_switch_does_not_change_model_or_revoke_votes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let config = Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: directory.path().join("validation.sqlite3"),
        mock: true,
        interactive_session: true,
        objective: String::new(),
        ..Config::default()
    };
    let harness = Harness::new(config.clone()).await?;
    harness
        .store
        .set_vote("agent-001", true, "verified existing selection")
        .await?;
    let mut invalid = config;
    invalid.agents = 2;
    assert!(harness.control.switch(invalid).await.is_err());
    assert_eq!(harness.control.current().revision, 0);
    assert!(harness.store.vote("agent-001").await?.unwrap().done);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_posted_before_run_starts_is_processed_without_replaying_history() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("startup.sqlite3");
    let store = openraid::storage::Store::open(&database).await?;
    store.append("owner", "historical task", true).await?;
    let harness = Harness::new(Config {
        agents: 2,
        workspace: directory.path().to_owned(),
        database,
        mock: true,
        interactive_session: true,
        objective: String::new(),
        grace_period: Duration::ZERO,
        ..Config::default()
    })
    .await?;
    let control = harness.control.clone();
    control
        .post_prompt("prompt sent before runtime startup".into())
        .await?;
    let run = tokio::spawn(harness.run());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let completed = store
                .read_board(0, 100)
                .await?
                .iter()
                .any(|message| message.body == "all workers drained; swarm complete");
            if completed && !control.is_busy() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    control.detach();
    assert_eq!(run.await??.finished_agents, 2);
    assert_eq!(store.prompts().await?.len(), 2);
    assert_eq!(
        store
            .read_board(0, 100)
            .await?
            .iter()
            .filter(|message| { message.body == "all workers drained; swarm complete" })
            .count(),
        1,
        "only the prompt posted after session construction starts work"
    );
    Ok(())
}
