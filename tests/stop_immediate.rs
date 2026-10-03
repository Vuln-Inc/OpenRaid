use anyhow::{ensure, Context, Result};
use openraid::{config::Config, metrics::Metrics, runtime::Harness, workspace::WorkspaceTools};
use serde_json::{json, Value};
use std::{path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

// The test executable supplies real parent/descendant processes independently
// of external programs, network access, and platform shell syntax.
#[test]
fn command_parent_fixture() {
    if !Path::new("fixture.enabled").exists() {
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "command_descendant_fixture", "--nocapture"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    std::fs::write("parent.pid", std::process::id().to_string()).unwrap();
    println!("PARENT_RUNNING");
    let _ = child.wait();
}

#[test]
fn command_descendant_fixture() {
    if !Path::new("fixture.enabled").exists() {
        return;
    }
    std::fs::write("descendant.pid", std::process::id().to_string()).unwrap();
    loop {
        println!("DESCENDANT_RUNNING");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn process_args() -> Value {
    json!({
        "program": std::env::current_exe().unwrap(),
        "args": ["--exact", "command_parent_fixture", "--nocapture"]
    })
}

struct Cleanup(Vec<u32>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for pid in &self.0 {
            #[cfg(windows)]
            let _ = std::process::Command::new("taskkill.exe")
                .args(["/F", "/T", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            #[cfg(unix)]
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(*pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

async fn process_ids(root: &Path) -> Result<Cleanup> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let (Ok(parent), Ok(descendant)) = (
                std::fs::read_to_string(root.join("parent.pid")),
                std::fs::read_to_string(root.join("descendant.pid")),
            ) {
                return Ok::<_, anyhow::Error>(Cleanup(vec![parent.parse()?, descendant.parse()?]));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("parent and descendant did not start")?
}

async fn assert_terminated(processes: &Cleanup) -> Result<()> {
    for pid in &processes.0 {
        let pid = *pid;
        tokio::time::timeout(Duration::from_secs(10), async move {
            loop {
                let running = tokio::task::spawn_blocking(move || {
                    #[cfg(windows)]
                    {
                        std::process::Command::new("powershell.exe")
                            .args([
                                "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                                &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"),
                            ])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status()
                            .unwrap()
                            .success()
                    }
                    #[cfg(unix)]
                    {
                        // An orphan zombie has already stopped executing and
                        // cannot retain pipes; container PID 1 may reap it late.
                        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                            if stat.rsplit_once(") ").is_some_and(|(_, tail)| tail.starts_with('Z')) {
                                return false;
                            }
                        }
                        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
                    }
                }).await?;
                if !running {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .with_context(|| format!("owner stop left process {pid} running"))??;
    }
    Ok(())
}

async fn read_request(socket: &mut TcpStream) -> Result<()> {
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
    let length = std::str::from_utf8(&bytes[..end])?
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        })
        .context("missing body length")?;
    while bytes.len() < end + length {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request body closed early");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(())
}

async fn return_tool(socket: &mut TcpStream, name: &str) -> Result<()> {
    return_tool_args(socket, name, process_args()).await
}

async fn return_tool_args(socket: &mut TcpStream, name: &str, args: Value) -> Result<()> {
    let body = json!({"choices":[{"finish_reason":"tool_calls","message":{
        "role":"assistant","content":null,"tool_calls":[{
            "id":"blocking-process","type":"function","function":{
                "name":name,"arguments":args.to_string()
            }
        }]
    }}]})
    .to_string();
    socket.write_all(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
    ).as_bytes()).await?;
    socket.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_cancels_admitted_provider_and_all_workers_waiting_for_capacity() -> Result<()> {
    let root = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let harness = Harness::new(Config {
        agents: 8,
        max_in_flight: 1,
        objective: "halt every worker immediately".into(),
        workspace: root.path().to_owned(),
        database: root.path().join("stop.sqlite3"),
        base_url: format!("http://{}/v1", listener.local_addr()?),
        ..Config::default()
    })
    .await?;
    let control = harness.control.clone();
    let metrics = harness.metrics.clone();
    let run = tokio::spawn(harness.run());
    let (mut socket, _) =
        tokio::time::timeout(Duration::from_secs(10), listener.accept()).await??;
    read_request(&mut socket).await?;
    // Never answer: seven peers may be waiting for the single provider permit.
    control.stop_work().await?;
    let summary = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .context("stop waited for the provider to complete")???;
    assert_eq!(summary.finished_agents, 8);
    assert_eq!(metrics.snapshot().finished, 8);
    assert_eq!(summary.votes, 0);
    let mut byte = [0u8; 1];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), socket.read(&mut byte)).await??,
        0
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "workers queued for capacity cannot dispatch after stop"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_overrides_consensus_drain_and_cancels_remaining_provider() -> Result<()> {
    let root = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let harness = Harness::new(Config {
        agents: 4,
        max_in_flight: 4,
        objective: "stop must override graceful consensus drain".into(),
        workspace: root.path().to_owned(),
        database: root.path().join("consensus-stop.sqlite3"),
        base_url: format!("http://{}/v1", listener.local_addr()?),
        grace_period: Duration::ZERO,
        ..Config::default()
    })
    .await?;
    let control = harness.control.clone();
    let store = harness.store.clone();
    let run = tokio::spawn(harness.run());
    let mut sockets = Vec::new();
    for _ in 0..4 {
        let (mut socket, _) =
            tokio::time::timeout(Duration::from_secs(10), listener.accept()).await??;
        read_request(&mut socket).await?;
        sockets.push(socket);
    }
    for socket in &mut sockets[..3] {
        return_tool_args(
            socket,
            "vote_done",
            json!({
                "done":true,"evidence":"verified objective; consensus peer remains in flight"
            }),
        )
        .await?;
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if store
                .read_board(0, 100)
                .await?
                .iter()
                .any(|entry| entry.body.contains("completion consensus reached"))
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("three peers did not reach consensus")??;
    assert!(
        !run.is_finished(),
        "the fourth provider still blocks graceful consensus drain"
    );
    control.stop_work().await?;
    let summary = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .context("stop was ignored during consensus drain")???;
    assert_eq!(summary.finished_agents, 4);
    assert_eq!(
        summary.votes, 0,
        "owner stop overrides successful consensus"
    );
    let mut byte = [0u8; 1];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), sockets[3].read(&mut byte)).await??,
        0
    );
    assert!(store
        .read_board(0, 100)
        .await?
        .iter()
        .any(|entry| entry.body == "all workers drained; work stopped"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_terminates_active_command_tree_and_streams_output_before_completion() -> Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("fixture.enabled"), "enabled")?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let harness = Harness::new(Config {
        agents: 1,
        objective: "stop active native processes".into(),
        workspace: root.path().to_owned(),
        database: root.path().join("stop.sqlite3"),
        base_url: format!("http://{}/v1", listener.local_addr()?),
        ..Config::default()
    })
    .await?;
    let control = harness.control.clone();
    let metrics = harness.metrics.clone();
    let run = tokio::spawn(harness.run());
    let (mut socket, _) =
        tokio::time::timeout(Duration::from_secs(10), listener.accept()).await??;
    read_request(&mut socket).await?;
    return_tool(&mut socket, "run_command").await?;
    let processes = process_ids(root.path()).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !metrics
            .agent_activity("agent-001")
            .contains("DESCENDANT_RUNNING")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("active command output never reached agent inspection")?;
    control.stop_work().await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), run)
            .await???
            .finished_agents,
        1
    );
    assert_terminated(&processes).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_terminates_persistent_pty_tree_and_releases_native_capacity() -> Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("fixture.enabled"), "enabled")?;
    let workspace = WorkspaceTools::new(root.path(), 1)?;
    let metrics = Arc::new(Metrics::new(1));
    let spawned = workspace
        .execute_with_activity("agent-001", "pty_spawn", &process_args(), metrics.clone())
        .await?;
    let processes = process_ids(root.path()).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !metrics
            .agent_activity("agent-001")
            .contains("DESCENDANT_RUNNING")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("PTY output never reached agent inspection")?;
    workspace.stop_processes();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let page = workspace
                .execute("pty_read", &json!({"id":spawned["id"]}))
                .await?;
            if page["output_complete"] == true {
                assert_eq!(page["status"], "killed");
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("stopped PTY did not reap and drain")??;
    assert_terminated(&processes).await?;
    let result = workspace
        .execute(
            "run_command",
            &json!({
                "program": std::env::current_exe()?,
                "args": ["--list"]
            }),
        )
        .await?;
    assert_eq!(
        result["success"], true,
        "stopped PTY releases process capacity for later work"
    );
    Ok(())
}

#[test]
fn stop_invalidates_queued_pty_spawn_generation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let hub = openraid::pty::Hub::new(Arc::new(tokio::sync::Semaphore::new(1)));
    let generation = hub.generation();
    hub.stop_all();
    let error = hub
        .execute_at_generation(
            "pty_spawn",
            &process_args(),
            root.path(),
            root.path(),
            generation,
        )
        .unwrap_err();
    assert!(error.to_string().contains("cancelled by owner stop"));
    assert_eq!(
        hub.execute("pty_list", &json!({}), root.path(), root.path())?["sessions"],
        json!([])
    );
    Ok(())
}
