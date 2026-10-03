//! Exercise the actual network -> model loop -> native tools -> consensus path,
//! including providers that incorrectly label tool-call responses as `stop`.
use std::time::Duration;

use anyhow::{Context, Result};
use openraid::{config::Config, runtime::Harness};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn request_json(socket: &mut TcpStream) -> Result<Value> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let count = socket.read(&mut chunk).await?;
        anyhow::ensure!(count != 0, "request ended before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
        anyhow::ensure!(bytes.len() <= 64 * 1024, "headers unexpectedly large");
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .context("missing content length")?;
    anyhow::ensure!(length <= 1024 * 1024, "scripted request unexpectedly large");
    while bytes.len() < header_end + length {
        let count = socket.read(&mut chunk).await?;
        anyhow::ensure!(count != 0, "request body ended early");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(
        &bytes[header_end..header_end + length],
    )?)
}

fn tool_response(index: usize) -> Value {
    let (name, args) = match index {
        0 => (
            "board_post",
            json!({"body":"i inspected the objective and propose creating artifact.txt, then verifying it with a command; peers can review this global proposal."}),
        ),
        1 => (
            "apply_patch",
            json!({"patch":"*** Begin Patch\n*** Add File: artifact.txt\n+verified-native-work\n*** End Patch"}),
        ),
        2 => {
            let args = if cfg!(windows) {
                json!({"program":"cmd.exe", "args":["/C", "type artifact.txt"]})
            } else {
                json!({"program":"sh", "args":["-c", "cat artifact.txt"]})
            };
            ("run_command", args)
        }
        _ => (
            "vote_done",
            json!({"done":true,"evidence":"artifact.txt was patched and a real subprocess returned its verified-native-work contents"}),
        ),
    };
    json!({
        "choices":[{"index":0,"finish_reason":"stop","message":{
            "role":"assistant","content":"executing coordinated native work",
            "tool_calls":[{"id":format!("call-{index}"),"type":"function","function":{
                "name":name,"arguments":args.to_string()
            }}]
        }}],
        "usage":{"prompt_tokens":11,"completion_tokens":7,"prompt_tokens_details":{"cached_tokens":3}}
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_provider_drives_real_tools_and_cannot_bypass_consensus_with_stop() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for index in 0..4 {
            let (mut socket, _) = listener.accept().await?;
            requests.push(request_json(&mut socket).await?);
            let body = tool_response(index).to_string();
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(headers.as_bytes()).await?;
            socket.write_all(body.as_bytes()).await?;
            socket.shutdown().await?;
        }
        Ok::<_, anyhow::Error>(requests)
    });
    let harness = Harness::new(Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: directory.path().join("live.sqlite"),
        objective: "cooperatively create artifact.txt and verify it using native tools".into(),
        base_url: format!("http://{address}/v1"),
        api_key: None,
        grace_period: Duration::from_millis(20),
        no_tui: true,
        ..Config::default()
    })
    .await?;
    let store = harness.store.clone();
    let metrics = harness.metrics.clone();
    let summary = harness.run().await?;
    let requests = server.await??;

    assert_eq!(
        (summary.agents, summary.finished_agents, summary.votes),
        (1, 1, 1)
    );
    assert_eq!(
        tokio::fs::read_to_string(directory.path().join("artifact.txt")).await?,
        "verified-native-work\n"
    );
    assert_eq!(requests.len(), 4);
    for request in &requests {
        assert_eq!(
            request["messages"][0], requests[0]["messages"][0],
            "system prefix must remain cache-stable"
        );
        assert!(request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "board_post"));
    }
    let command_result = requests[3]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool" && message["tool_call_id"] == "call-2")
        .context("real command result was not returned to the model")?;
    let output: Value = serde_json::from_str(command_result["content"].as_str().unwrap())?;
    assert_eq!(output["exit_code"], 0);
    assert!(output["stdout"]
        .as_str()
        .unwrap()
        .contains("verified-native-work"));
    let checkpoint = store
        .load_checkpoint("agent-001")
        .await?
        .context("missing completed tool checkpoint")?;
    assert_eq!(
        checkpoint["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        4
    );
    let snapshot = metrics.snapshot();
    assert_eq!(
        (
            snapshot.output_tokens,
            snapshot.cached_tokens,
            snapshot.finished
        ),
        (28, 12, 1)
    );
    let board = store.read_board(0, 100).await?;
    assert!(board
        .iter()
        .any(|entry| entry.sender == "agent-001"
            && entry.body.contains("propose creating artifact.txt")));
    assert!(board
        .iter()
        .any(|entry| entry.sender == "agent-001" && entry.body.contains("quitting")));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_context_overflow_compacts_history_then_resumes_native_consensus() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    // A meaningful completed prefix to summarize, still below the proactive
    // local budget. The server models a provider with a smaller actual window.
    let objective = format!(
        "coordinate through the global board and verify completion. retained objective evidence: {}",
        "native-swarm-context ".repeat(100)
    );
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for index in 0..4 {
            let (mut socket, _) = listener.accept().await?;
            requests.push(request_json(&mut socket).await?);
            let (status, response) = match index {
                0 => ("200 OK", tool_response(0)),
                1 => (
                    "400 Bad Request",
                    json!({"error": {
                        "code":"context_length_exceeded", "message":"maximum context length exceeded"
                    }}),
                ),
                2 => (
                    "200 OK",
                    json!({
                        "choices":[{"finish_reason":"stop","message":{
                            "role":"assistant", "content":"owner objective retained: coordinate on the global board and verify native work; keep every board entry available by cursor."
                        }}],
                        "usage":{"prompt_tokens":11,"completion_tokens":7}
                    }),
                ),
                _ => ("200 OK", tool_response(3)),
            };
            let body = response.to_string();
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(headers.as_bytes()).await?;
            socket.write_all(body.as_bytes()).await?;
            socket.shutdown().await?;
        }
        Ok::<_, anyhow::Error>(requests)
    });
    let harness = Harness::new(Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: directory.path().join("overflow.sqlite"),
        objective: objective.clone(),
        base_url: format!("http://{address}/v1"),
        api_key: None,
        grace_period: Duration::from_millis(20),
        no_tui: true,
        ..Config::default()
    })
    .await?;
    let store = harness.store.clone();
    let metrics = harness.metrics.clone();
    let summary = harness.run().await?;
    let requests = server.await??;

    assert_eq!((summary.finished_agents, summary.votes), (1, 1));
    assert_eq!(requests.len(), 4);
    assert!(
        requests[2].get("tools").is_none(),
        "summary request must disable tools"
    );
    assert!(requests[2]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("Completed conversation prefix"));
    assert_eq!(
        requests[0]["messages"][0], requests[3]["messages"][0],
        "normal system prefix must stay stable across compaction"
    );
    let final_messages = requests[3]["messages"].as_array().unwrap();
    assert!(final_messages.iter().any(|message| message["content"]
        .as_str()
        .is_some_and(|content| content.contains("Continuation summary"))));
    assert!(
        final_messages
            .iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == "call-0"),
        "recent tool result must remain paired with its assistant call"
    );
    let original_board = store.read_board(0, 100).await?;
    assert_eq!(
        original_board[0].body, objective,
        "compaction must never replace durable owner history"
    );
    assert!(original_board
        .iter()
        .any(|entry| entry.sender == "agent-001"
            && entry.body.contains("propose creating artifact.txt")));
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.output_tokens, 21);
    assert!(
        snapshot.retries >= 1,
        "provider overflow must be visible in telemetry"
    );
    Ok(())
}
