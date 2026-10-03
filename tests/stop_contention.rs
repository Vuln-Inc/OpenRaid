use anyhow::{bail, ensure, Context, Result};
use openraid::{config::Config, runtime::Harness};
use std::time::Duration;
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn read_request(socket: &mut TcpStream) -> Result<()> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "provider request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(offset) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let body_length = std::str::from_utf8(&bytes[..header_end])?
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .context("provider request is missing content length")?;
    while bytes.len() < header_end + body_length {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "provider request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_cancels_provider_before_blocked_control_transaction_can_finish() -> Result<()> {
    let root = tempfile::tempdir()?;
    let database = root.path().join("stop-contention.sqlite3");
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let harness = Harness::new(Config {
        agents: 1,
        max_in_flight: 1,
        objective: "stop must interrupt requests even while controls await SQLite".into(),
        workspace: root.path().to_owned(),
        database: database.clone(),
        base_url: format!("http://{}/v1", listener.local_addr()?),
        ..Config::default()
    })
    .await?;
    let control = harness.control.clone();
    let mut run = AbortOnDrop(tokio::spawn(harness.run()));
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
        .await
        .context("provider request was not admitted")??;
    tokio::time::timeout(Duration::from_secs(5), read_request(&mut socket)).await??;

    let writer = rusqlite::Connection::open(&database)?;
    writer.busy_timeout(Duration::from_secs(2))?;
    writer.execute_batch("BEGIN IMMEDIATE")?;

    let pausing_control = control.clone();
    let mut pause = Box::pin(async move { pausing_control.pause().await });
    // Poll pause first: it acquires the private changes lock before awaiting
    // its Store query. Keeping this future alive preserves that lock, without
    // relying on a sleep or scheduler timing to establish contention.
    tokio::select! {
        biased;
        result = &mut pause => bail!("pause unexpectedly completed under SQLite contention: {result:?}"),
        _ = std::future::ready(()) => {},
    }
    let mut pausing = AbortOnDrop(tokio::spawn(pause));
    let mut stopping = AbortOnDrop(tokio::spawn(async move { control.stop_work().await }));

    let mut byte = [0u8; 1];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), socket.read(&mut byte))
            .await
            .context("stop waited for the blocked changes transaction before cancelling HTTP")??,
        0,
        "the active request must be cancelled while the external writer still holds its lock"
    );
    writer.execute_batch("ROLLBACK")?;
    // Pause may be rejected when the stop wins admission; either outcome is
    // valid as long as it cannot delay cancellation or strand the round.
    let _ = tokio::time::timeout(Duration::from_secs(5), &mut pausing.0).await??;
    tokio::time::timeout(Duration::from_secs(5), &mut stopping.0).await???;
    let summary = tokio::time::timeout(Duration::from_secs(5), &mut run.0).await???;
    assert_eq!(summary.finished_agents, 1);
    assert_eq!(summary.votes, 0);
    Ok(())
}
