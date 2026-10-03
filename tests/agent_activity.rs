use openraid::{metrics::Metrics, storage::Store, tools::ToolBus, workspace::WorkspaceTools};
use serde_json::json;
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

// The test executable is also the controlled PTY child. Admission and release
// are local files so this checks real output while the child is still running.
#[test]
fn activity_child_fixture() {
    if std::env::var_os("OPENRAID_ACTIVITY_FIXTURE").is_none() {
        return;
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(b"PTY_LIVE_ACTIVITY\n").unwrap();
    #[cfg(not(windows))]
    {
        stdout.write_all(&[0xe7]).unwrap();
        stdout.flush().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        stdout.write_all(&[0x95, 0x8c]).unwrap();
    }
    // Rust's Windows console writer requires valid UTF-8 for each write.
    // Incremental decoder unit tests cover split reads on Windows as well.
    #[cfg(windows)]
    stdout.write_all("界".as_bytes()).unwrap();
    stdout.write_all(b" UTF8_COMPLETE\n").unwrap();
    stdout.flush().unwrap();
    std::fs::write("activity.ready", b"ready").unwrap();
    while !Path::new("activity.release").exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
    stdout.write_all(b"PTY_ACTIVITY_COMPLETE\n").unwrap();
    stdout.flush().unwrap();
}

fn child_command() -> String {
    let exe = std::env::current_exe().unwrap();
    #[cfg(windows)]
    return format!(
        "$env:OPENRAID_ACTIVITY_FIXTURE='1'; & '{}' --exact activity_child_fixture --nocapture",
        exe.display().to_string().replace('\'', "''")
    );
    #[cfg(not(windows))]
    return format!(
        "OPENRAID_ACTIVITY_FIXTURE=1 '{}' --exact activity_child_fixture --nocapture",
        exe.display().to_string().replace('\'', "'\\''")
    );
}

struct ReleaseOnDrop(PathBuf);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.0, b"release");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_pty_streams_activity_before_exit_and_preserves_split_utf8() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceTools::new(root.path(), 1).unwrap();
    let store = Store::open(root.path().join("activity.sqlite"))
        .await
        .unwrap();
    let metrics = Arc::new(Metrics::new(2));
    let bus = ToolBus::new(store, workspace.clone()).with_metrics(metrics.clone());
    let release = ReleaseOnDrop(root.path().join("activity.release"));
    let child = bus
        .execute(
            "agent-002",
            "pty_spawn",
            &json!({"command":child_command(),"rows":30,"cols":120}),
        )
        .await
        .unwrap();
    let id = child["id"].as_str().unwrap();

    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if root.path().join("activity.ready").exists()
                && metrics
                    .agent_activity("agent-002")
                    .contains("UTF8_COMPLETE")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("active PTY output was not streamed to inspector metrics");
    let active = metrics.agent_activity("agent-002");
    assert!(active.contains("PTY_LIVE_ACTIVITY"));
    assert!(active.contains("界 UTF8_COMPLETE"), "{active}");
    assert!(!active.contains('\u{fffd}'), "{active}");
    assert!(!active.contains("PTY_ACTIVITY_COMPLETE"));
    assert!(metrics.agent_activity("agent-001").is_empty());
    assert!(metrics.agent_output("agent-002").is_empty());
    let status = workspace
        .execute("pty_read", &json!({"id":id}))
        .await
        .unwrap();
    assert_eq!(status["output_complete"], false);

    drop(release);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let status = workspace
                .execute("pty_read", &json!({"id":id}))
                .await
                .unwrap();
            if status["output_complete"] == true {
                assert_eq!(status["exit_code"], 0);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("released PTY child did not exit");
    assert!(metrics
        .agent_activity("agent-002")
        .contains("PTY_ACTIVITY_COMPLETE"));
    workspace
        .execute("pty_kill", &json!({"id":id,"cleanup":true}))
        .await
        .unwrap();
}
