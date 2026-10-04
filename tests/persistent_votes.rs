//! Completion votes park workers and survive peer traffic, while authenticated
//! owner steering still clears evidence and returns the worker to its provider.
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use openraid::{config::Config, runtime::Harness, storage::Store};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};

async fn request_json(socket: &mut TcpStream) -> Result<Value> {
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
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .context("missing content length")?;
    ensure!(length <= 1024 * 1024, "unexpected request size");
    while bytes.len() < header_end + length {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request ended before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(
        &bytes[header_end..header_end + length],
    )?)
}

async fn serve_votes(
    listener: TcpListener,
    store: Store,
    mut stop: watch::Receiver<bool>,
) -> Result<Vec<(Value, bool)>> {
    let mut requests = Vec::new();
    loop {
        let (mut socket, _) = tokio::select! {
            biased;
            _ = stop.changed() => break,
            socket = listener.accept() => socket?,
        };
        let request = request_json(&mut socket).await?;
        let old_vote_present = store.vote("agent-001").await?.is_some_and(|vote| vote.done);
        let index = requests.len();
        requests.push((request, old_vote_present));
        let evidence = if index == 0 {
            "verified original objective"
        } else {
            "verified corrected owner objective"
        };
        let body = json!({
            "choices":[{"finish_reason":"stop","message":{
                "role":"assistant", "content":"verified completion evidence",
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
    Ok(requests)
}

async fn wait_for_vote(store: &Store, expected_seq: u64) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if store
                .vote("agent-001")
                .await?
                .is_some_and(|vote| vote.done && vote.board_seq == expected_seq)
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context("worker did not cast its expected completion vote")?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continuous_external_peer_traffic_preserves_vote_and_does_not_restart_grace() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("persistent.sqlite");
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let harness = Harness::new(Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: database.clone(),
        objective: "verify the objective and park after voting until owner steering".into(),
        base_url: format!("http://{address}/v1"),
        grace_period: Duration::from_millis(500),
        no_tui: true,
        ..Config::default()
    })
    .await?;
    let store = harness.store.clone();
    let external = Store::open(database).await?;
    let (stop, stop_rx) = watch::channel(false);
    let server = tokio::spawn(serve_votes(listener, store.clone(), stop_rx.clone()));
    let traffic_store = store.clone();
    let traffic = tokio::spawn(async move {
        wait_for_vote(&traffic_store, 1).await?;
        let mut stop_rx = stop_rx;
        let mut count = 0;
        let mut vote_preserved = true;
        loop {
            tokio::select! {
                biased;
                _ = stop_rx.changed() => break,
                _ = tokio::time::sleep(Duration::from_millis(10)) => {},
            }
            external
                .append("agent-777", format!("ordinary peer status {count}"), false)
                .await?;
            count += 1;
            vote_preserved &= traffic_store
                .vote("agent-001")
                .await?
                .is_some_and(|vote| vote.done && vote.board_seq == 1);
        }
        Ok::<_, anyhow::Error>((count, vote_preserved))
    });

    // A test deadline prevents the original endless revote/grace loop hanging CI.
    let run = tokio::time::timeout(Duration::from_secs(5), harness.run()).await;
    stop.send_replace(true);
    let traffic = traffic.await??;
    let requests = server.await??;
    let summary = run.context("peer traffic prevented completion consensus")??;

    assert_eq!((summary.finished_agents, summary.votes), (1, 1));
    assert!(
        traffic.0 >= 20,
        "test must sustain traffic across several supervisor ticks"
    );
    assert!(
        traffic.1,
        "peer posts must preserve the original done vote and evidence cursor"
    );
    assert_eq!(
        requests.len(),
        1,
        "parked voters must not spend tokens on peer chatter"
    );
    let vote = store.vote("agent-001").await?.context("missing vote")?;
    assert!(vote.done);
    assert_eq!(vote.board_seq, 1);
    assert_eq!(vote.reason, "verified original objective");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_owner_correction_wakes_parked_voter_and_requires_new_evidence() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("owner-steering.sqlite");
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let harness = Harness::new(Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: database.clone(),
        objective: "park on completion, then reconsider authenticated owner corrections".into(),
        base_url: format!("http://{address}/v1"),
        grace_period: Duration::from_millis(750),
        no_tui: true,
        ..Config::default()
    })
    .await?;
    let store = harness.store.clone();
    let external = Store::open(database).await?;
    let (stop, stop_rx) = watch::channel(false);
    let server = tokio::spawn(serve_votes(listener, store.clone(), stop_rx));
    let inject = async {
        wait_for_vote(&store, 1).await?;
        for index in 0..10 {
            // Even a peer claiming to be the owner must not revoke a vote.
            external
                .append("owner", format!("unprivileged peer status {index}"), false)
                .await?;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        ensure!(
            store
                .vote("agent-001")
                .await?
                .is_some_and(|vote| vote.done && vote.board_seq == 1),
            "unprivileged peer traffic changed the parked completion vote"
        );
        let owner = external
            .append(
                "owner",
                "authenticated correction: verify the revised deliverable",
                true,
            )
            .await?;
        ensure!(
            store
                .vote("agent-001")
                .await?
                .is_none_or(|vote| vote.board_seq >= owner.seq),
            "owner append must atomically clear old evidence"
        );
        wait_for_vote(&store, owner.seq).await?;
        Ok::<_, anyhow::Error>(owner.seq)
    };
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(harness.run(), inject)
    })
    .await;
    stop.send_replace(true);
    let requests = server.await??;
    let (run, injection) = result.context("parked voter did not reconsider owner correction")?;
    let summary = run?;
    let owner_seq = injection?;

    assert_eq!((summary.finished_agents, summary.votes), (1, 1));
    assert_eq!(
        requests.len(),
        2,
        "only the owner correction should require another provider turn"
    );
    assert!(
        !requests[1].1,
        "owner evidence must be invalidated before provider admission"
    );
    assert!(requests[1].0["messages"]
        .to_string()
        .contains("authenticated correction: verify the revised deliverable"));
    let vote = store
        .vote("agent-001")
        .await?
        .context("missing corrected vote")?;
    assert!(vote.done);
    assert_eq!(vote.board_seq, owner_seq);
    assert_eq!(vote.reason, "verified corrected owner objective");
    Ok(())
}
