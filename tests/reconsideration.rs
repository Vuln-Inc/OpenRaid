//! Cross-process owner corrections return parked voters to coordination, while
//! ordinary peer traffic preserves their already-verified completion evidence.
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use openraid::{config::Config, runtime::Harness, storage::Store};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn read_request(socket: &mut TcpStream) -> Result<Value> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request ended before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
        ensure!(bytes.len() <= 64 * 1024, "unexpected header size");
    };
    let headers = std::str::from_utf8(&bytes[..header_end])?;
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .context("missing content length")?;
    ensure!(length <= 1024 * 1024, "unexpected request size");
    while bytes.len() < header_end + length {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request body ended early");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(
        &bytes[header_end..header_end + length],
    )?)
}

async fn wait_for_fresh_vote(store: &Store, expected_seq: u64) -> Result<()> {
    loop {
        if store
            .vote("agent-001")
            .await?
            .is_some_and(|vote| vote.done && vote.board_seq == expected_seq)
        {
            return Ok(());
        }
        // Observation cadence only, without a completion or request deadline.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn voted_live_agent_preserves_peer_evidence_and_reconsiders_external_owner_messages(
) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("reconsideration.sqlite");
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let harness = Harness::new(Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: database.clone(),
        objective: "preserve completion votes until authenticated owner corrections".into(),
        base_url: format!("http://{address}/v1"),
        grace_period: Duration::from_secs(1),
        no_tui: true,
        ..Config::default()
    })
    .await?;
    let store = harness.store.clone();
    let external = Store::open(database).await?;
    let server_store = store.clone();
    let server = async move {
        let mut observations = Vec::new();
        for index in 0..2 {
            let (mut socket, _) = listener.accept().await?;
            let request = read_request(&mut socket).await?;
            observations.push((request, server_store.vote("agent-001").await?));
            let evidence = match index {
                0 => "verified original objective",
                _ => "reconsidered external owner correction and verified final completion",
            };
            let body = json!({
                "choices":[{"finish_reason":"stop","message":{
                    "role":"assistant", "content":"revalidating the whole global board",
                    "tool_calls":[{"id":format!("vote-{index}"),"type":"function","function":{
                        "name":"vote_done","arguments":json!({"done":true,"evidence":evidence}).to_string()
                    }}]
                }}],
                "usage":{"prompt_tokens":10,"completion_tokens":2}
            })
            .to_string();
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(headers.as_bytes()).await?;
            socket.write_all(body.as_bytes()).await?;
            socket.shutdown().await?;
        }
        Ok::<_, anyhow::Error>(observations)
    };
    let inject = async {
        wait_for_fresh_vote(&store, 1).await?;
        let peer = external
            .append(
                "agent-777",
                "external peer blocker requires reconsideration",
                false,
            )
            .await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        ensure!(
            store
                .vote("agent-001")
                .await?
                .is_some_and(|vote| vote.done && vote.board_seq == 1),
            "peer chatter must not replace or revoke completed evidence"
        );
        let owner = external
            .append(
                "owner",
                "external owner correction requires fresh verification",
                true,
            )
            .await?;
        wait_for_fresh_vote(&store, owner.seq).await?;
        Ok::<_, anyhow::Error>((peer.seq, owner.seq))
    };
    let (run, injections, observations) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(harness.run(), inject, server)
    })
    .await
    .context("owner reconsideration did not complete")?;
    let summary = run?;
    let (peer_seq, owner_seq) = injections?;
    let observations = observations?;

    assert_eq!((summary.finished_agents, summary.votes), (1, 1));
    assert_eq!(observations.len(), 2);
    let peer_history = observations[1].0["messages"].to_string();
    assert!(peer_history.contains("external peer blocker requires reconsideration"));
    let owner_history = observations[1].0["messages"].to_string();
    assert!(owner_history.contains("external owner correction requires fresh verification"));
    assert!(
        observations[1].1.is_none(),
        "external owner insertion must atomically clear old votes"
    );
    let final_vote = store
        .vote("agent-001")
        .await?
        .context("missing final vote")?;
    assert_eq!(final_vote.board_seq, owner_seq);
    assert!(final_vote.reason.contains("owner correction"));
    let board = store.read_board(0, 100).await?;
    assert_eq!(
        board
            .iter()
            .find(|message| message.seq == peer_seq)
            .unwrap()
            .sender,
        "agent-777"
    );
    assert!(
        board
            .iter()
            .find(|message| message.seq == owner_seq)
            .unwrap()
            .owner
    );
    assert!(board
        .iter()
        .any(|message| message.body.contains("swarm complete")));
    Ok(())
}
