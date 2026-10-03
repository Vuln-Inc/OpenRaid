use openraid::{storage::Store, tools::ToolBus, workspace::WorkspaceTools};
use serde_json::{json, Value};
use std::{
    io::{BufRead, IsTerminal, Write},
    time::Duration,
};

// The test executable doubles as an account/network-independent interactive
// child, exercising the actual OS terminal rather than a mocked PTY backend.
#[test]
fn pty_child_fixture() {
    if std::env::var_os("OPENRAID_PTY_FIXTURE").is_none() {
        return;
    }
    if std::env::var("OPENRAID_PTY_FIXTURE").unwrap() == "queue" {
        std::fs::write("ordinary.ready", "ready").unwrap();
        while !std::path::Path::new("ordinary.release").exists() {
            std::thread::sleep(Duration::from_millis(10));
        }
        println!("ORDINARY_COMMAND_DRAINED");
        return;
    }
    println!(
        "PTY_READY stdin={} stdout={} pid={} PTY_READY_END",
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        std::process::id()
    );
    std::io::stdout().flush().unwrap();
    if std::env::var("OPENRAID_PTY_FIXTURE").unwrap() == "never-read" {
        loop {
            std::thread::park();
        }
    }
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.trim() == "quit" {
            println!("PTY_GOODBYE");
            break;
        }
        if line.trim() == "large" {
            println!("LARGE_BEGIN{}LARGE_END", "z".repeat(96 * 1024));
        } else {
            println!("PTY_REPLY:{line}");
        }
        std::io::stdout().flush().unwrap();
    }
}

fn child_args() -> Value {
    // Set the fixture selector in the shell so unrelated concurrent tests never
    // inherit a process-global environment mutation.
    let exe = std::env::current_exe().unwrap();
    #[cfg(windows)]
    let command = format!(
        "$env:OPENRAID_PTY_FIXTURE='1'; & '{}' --exact pty_child_fixture --nocapture",
        exe.display().to_string().replace('\'', "''")
    );
    #[cfg(not(windows))]
    let command = format!(
        "OPENRAID_PTY_FIXTURE=1 '{}' --exact pty_child_fixture --nocapture",
        exe.display().to_string().replace('\'', "'\\''")
    );
    json!({"command":command,"rows":30,"cols":120})
}

async fn read_until(workspace: &WorkspaceTools, id: &str, needle: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let page = workspace
                .execute("pty_read", &json!({"id":id}))
                .await
                .unwrap();
            if page["content"].as_str().unwrap().contains(needle) {
                return page;
            }
            assert_ne!(
                page["output_complete"], true,
                "process exited without {needle}: {page}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native PTY fixture did not respond")
}

async fn wait_exit(workspace: &WorkspaceTools, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let page = workspace
                .execute("pty_read", &json!({"id":id}))
                .await
                .unwrap();
            if page["output_complete"] == true {
                return page;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("native PTY process did not reap/drain")
}

fn fixture_pid(ready: &Value) -> u32 {
    ready["content"]
        .as_str()
        .unwrap()
        .split("pid=")
        .nth(1)
        .unwrap()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap()
}

async fn assert_reaped(pid: u32) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let running = tokio::task::spawn_blocking(move || {
                #[cfg(windows)]
                let status = std::process::Command::new("powershell.exe").args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}")]).status().unwrap();
                #[cfg(not(windows))]
                let status = std::process::Command::new("kill").args(["-0", &pid.to_string()]).output().unwrap().status;
                status.success()
            }).await.unwrap();
            if !running { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("PTY cleanup left its nested interactive child running");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_terminal_interaction_resize_full_disk_output_and_natural_exit() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceTools::new(root.path(), 2).unwrap();
    let child = workspace.execute("pty_spawn", &child_args()).await.unwrap();
    let id = child["id"].as_str().unwrap();
    let ready = read_until(&workspace, id, "PTY_READY_END").await;
    assert!(
        ready["content"]
            .as_str()
            .unwrap()
            .contains("stdin=true stdout=true"),
        "fixture must receive real terminal handles: {ready}"
    );
    let size = workspace
        .execute("pty_resize", &json!({"id":id,"rows":42,"cols":132}))
        .await
        .unwrap();
    assert_eq!(size["rows"], 42);
    assert_eq!(size["cols"], 132);
    workspace
        .execute("pty_write", &json!({"id":id,"data":"hello\r\n"}))
        .await
        .unwrap();
    read_until(&workspace, id, "PTY_REPLY:hello").await;
    workspace
        .execute("pty_write", &json!({"id":id,"data":"large\r\nquit\r\n"}))
        .await
        .unwrap();
    let exited = wait_exit(&workspace, id).await;
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited["status"], "exited");
    assert_eq!(exited["output_error"], Value::Null);
    assert_eq!(exited["has_more"], true);
    assert!(exited["content"].as_str().unwrap().len() <= 64 * 1024);
    let output_path = exited["output_path"].as_str().unwrap();
    let bytes = std::fs::read(output_path).unwrap();
    assert!(bytes.len() > 96 * 1024);
    assert!(String::from_utf8_lossy(&bytes).contains("LARGE_END"));
    assert!(String::from_utf8_lossy(&bytes).contains("PTY_GOODBYE"));
    let mut cursor = 0;
    let mut paged = Vec::new();
    loop {
        let page = workspace
            .execute(
                "pty_read",
                &json!({"id":id,"byte_offset":cursor,"limit":4096}),
            )
            .await
            .unwrap();
        // Fixture data and terminal sequences are ASCII, so byte pages round-trip.
        paged.extend_from_slice(page["content"].as_str().unwrap().as_bytes());
        cursor = page["next_byte_offset"].as_u64().unwrap();
        if page["has_more"] == false {
            break;
        }
    }
    assert_eq!(paged, bytes);
    assert!(workspace
        .execute("pty_write", &json!({"id":id,"data":"late"}))
        .await
        .is_err());
    workspace
        .execute("pty_kill", &json!({"id":id,"cleanup":true}))
        .await
        .unwrap();
    assert_eq!(
        workspace.execute("pty_list", &json!({})).await.unwrap()["sessions"],
        json!([])
    );
    assert!(
        std::path::Path::new(output_path).exists(),
        "cleanup must preserve durable output"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_registry_capacity_explicit_kill_and_fresh_board_enforcement() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("swarm.sqlite")).await.unwrap();
    let workspace = WorkspaceTools::new(root.path(), 1).unwrap();
    let bus = ToolBus::new(store.clone(), workspace.clone());
    let child = bus
        .execute("agent-001", "pty_spawn", &child_args())
        .await
        .unwrap();
    let id = child["id"].as_str().unwrap();
    let pid = fixture_pid(&read_until(&workspace, id, "PTY_READY_END").await);
    let command_error = tokio::time::timeout(
        Duration::from_secs(2),
        workspace.execute("run_command", &json!({"command":"echo capacity"})),
    )
    .await
    .expect("PTY-held capacity must not strand a command caller")
    .unwrap_err();
    assert!(command_error.to_string().contains("pty_kill"));
    assert!(
        workspace.execute("pty_spawn", &child_args()).await.is_err(),
        "active PTYs must honor the command process bound"
    );
    assert_eq!(
        bus.clone()
            .execute("agent-002", "pty_list", &json!({}))
            .await
            .unwrap()["sessions"][0]["id"],
        id
    );
    store
        .append("owner", "coordinate before acting", true)
        .await
        .unwrap();
    assert!(bus
        .execute(
            "agent-002",
            "pty_write",
            &json!({"id":id,"data":"stale\r\n"})
        )
        .await
        .is_err());
    bus.execute("agent-002", "board_read", &json!({}))
        .await
        .unwrap();
    let killed = bus
        .execute("agent-002", "pty_kill", &json!({"id":id}))
        .await
        .unwrap();
    assert_eq!(killed["status"], "killed");
    assert_eq!(killed["output_complete"], true);
    assert_reaped(pid).await;
    let replacement = workspace.execute("pty_spawn", &child_args()).await.unwrap();
    assert_ne!(replacement["id"], id);
    workspace
        .execute("pty_kill", &json!({"id":replacement["id"],"cleanup":true}))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn last_workspace_handle_drop_reaps_running_pty_and_preserves_output() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceTools::new(root.path(), 1).unwrap();
    let child = workspace.execute("pty_spawn", &child_args()).await.unwrap();
    let id = child["id"].as_str().unwrap();
    let pid = fixture_pid(&read_until(&workspace, id, "PTY_READY_END").await);
    let path = child["output_path"].as_str().unwrap().to_owned();
    tokio::time::timeout(
        Duration::from_secs(20),
        tokio::task::spawn_blocking(move || drop(workspace)),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(std::fs::read_to_string(path).unwrap().contains("PTY_READY"));
    assert_reaped(pid).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unicode_byte_pages_are_exactly_reversible_even_inside_codepoints() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceTools::new(root.path(), 1).unwrap();
    let child = workspace.execute("pty_spawn", &child_args()).await.unwrap();
    let id = child["id"].as_str().unwrap();
    read_until(&workspace, id, "PTY_READY_END").await;
    workspace
        .execute("pty_write", &json!({"id":id,"data":"İ世界🙂\r\nquit\r\n"}))
        .await
        .unwrap();
    let exited = wait_exit(&workspace, id).await;
    let bytes = std::fs::read(exited["output_path"].as_str().unwrap()).unwrap();
    let unicode = "İ世界🙂".as_bytes();
    let start = bytes
        .windows(unicode.len())
        .position(|window| window == unicode)
        .expect("fixture Unicode survives native terminal output");
    let mut reconstructed = Vec::new();
    for offset in start..start + unicode.len() {
        let page = workspace
            .execute("pty_read", &json!({"id":id,"byte_offset":offset,"limit":1}))
            .await
            .unwrap();
        let exact = page["content_hex"].as_str().unwrap();
        assert_eq!(exact.len(), 2);
        reconstructed.push(u8::from_str_radix(exact, 16).unwrap());
        assert_eq!(page["next_byte_offset"], offset + 1);
    }
    assert_eq!(reconstructed, unicode);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_kill_reaps_background_group_after_shell_leader_already_exited() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceTools::new(root.path(), 1).unwrap();
    let child = workspace.execute("pty_spawn", &json!({"program":"sh","args":["-c","trap '' HUP; sleep 600 & printf 'BACKGROUND_READY:%s\\n' \"$!\"; exit 0"]})).await.unwrap();
    let id = child["id"].as_str().unwrap();
    read_until(&workspace, id, "BACKGROUND_READY:").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let page = workspace
                .execute("pty_read", &json!({"id":id}))
                .await
                .unwrap();
            if page["exit_code"] == 0 {
                assert_eq!(
                    page["output_complete"], false,
                    "background child must retain the terminal to exercise leader-exited cleanup"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let killed = tokio::time::timeout(
        Duration::from_secs(10),
        workspace.execute("pty_kill", &json!({"id":id,"cleanup":true})),
    )
    .await
    .expect("kill must terminate background PTY holders after leader exit")
    .unwrap();
    assert_eq!(killed["output_complete"], true);
    assert_eq!(
        workspace.execute("pty_list", &json!({})).await.unwrap()["sessions"],
        json!([])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn immediate_large_input_does_not_deadlock_native_startup_handshake() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceTools::new(root.path(), 1).unwrap();
    let child = workspace.execute("pty_spawn", &child_args()).await.unwrap();
    let id = child["id"].as_str().unwrap();
    // Deliberately do not wait for readiness/output. More than the pipe input
    // buffer exercises input-before-ConPTY-bootstrap writer contention.
    let input = format!("early:{}\r\nquit\r\n", "x".repeat(32 * 1024));
    tokio::time::timeout(
        Duration::from_secs(20),
        workspace.execute("pty_write", &json!({"id":id,"data":input})),
    )
    .await
    .expect("early input must not deadlock terminal bootstrap")
    .unwrap();
    let exited = wait_exit(&workspace, id).await;
    assert_eq!(exited["exit_code"], 0);
    let bytes = std::fs::read(exited["output_path"].as_str().unwrap()).unwrap();
    let output = String::from_utf8_lossy(&bytes);
    assert!(output.contains("PTY_READY_END"));
    assert!(output.contains("PTY_REPLY:early:"));
    assert!(output.contains("PTY_GOODBYE"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_pty_occupancy_preserves_ordinary_command_queueing() {
    struct ReleaseOnDrop(std::path::PathBuf);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, "release");
        }
    }
    let root = tempfile::tempdir().unwrap();
    let _release = ReleaseOnDrop(root.path().join("ordinary.release"));
    let workspace = WorkspaceTools::new(root.path(), 2).unwrap();
    let terminal = workspace.execute("pty_spawn", &child_args()).await.unwrap();
    let id = terminal["id"].as_str().unwrap();
    read_until(&workspace, id, "PTY_READY_END").await;
    let mut args = child_args();
    args.as_object_mut().unwrap().remove("rows");
    args.as_object_mut().unwrap().remove("cols");
    #[cfg(windows)]
    {
        args["command"] = json!(args["command"]
            .as_str()
            .unwrap()
            .replace("='1'", "='queue'"));
    }
    #[cfg(unix)]
    {
        args["command"] = json!(args["command"]
            .as_str()
            .unwrap()
            .replace("FIXTURE=1", "FIXTURE=queue"));
    }
    let first_workspace = workspace.clone();
    let first = tokio::spawn(async move { first_workspace.execute("run_command", &args).await });
    tokio::time::timeout(Duration::from_secs(20), async {
        while !root.path().join("ordinary.ready").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ordinary process must start beside one PTY");
    let second_workspace = workspace.clone();
    let mut second = tokio::spawn(async move {
        second_workspace
            .execute("run_command", &json!({"command":"echo ORDINARY_QUEUED"}))
            .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second)
            .await
            .is_err(),
        "an ordinary-command-held slot should queue, not return a persistent-capacity error"
    );
    std::fs::write(root.path().join("ordinary.release"), "release").unwrap();
    let first = tokio::time::timeout(Duration::from_secs(20), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first["success"], true);
    let second = tokio::time::timeout(Duration::from_secs(20), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(second["stdout"]
        .as_str()
        .unwrap()
        .contains("ORDINARY_QUEUED"));
    workspace
        .execute("pty_kill", &json!({"id":id,"cleanup":true}))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_kill_releases_concurrent_input_to_a_child_that_never_reads() {
    let root = tempfile::tempdir().unwrap();
    let workspace = WorkspaceTools::new(root.path(), 1).unwrap();
    let mut args = child_args();
    #[cfg(windows)]
    {
        args["command"] = json!(args["command"]
            .as_str()
            .unwrap()
            .replace("='1'", "='never-read'"));
    }
    #[cfg(unix)]
    {
        args["command"] = json!(args["command"]
            .as_str()
            .unwrap()
            .replace("FIXTURE=1", "FIXTURE=never-read"));
    }
    let child = workspace.execute("pty_spawn", &args).await.unwrap();
    let id = child["id"].as_str().unwrap();
    read_until(&workspace, id, "PTY_READY_END").await;
    let pid = fixture_pid(&read_until(&workspace, id, "PTY_READY_END").await);
    let writer_workspace = workspace.clone();
    let writer_id = id.to_owned();
    let writer = tokio::spawn(async move {
        writer_workspace
            .execute(
                "pty_write",
                &json!({"id":writer_id,"data":"x".repeat(64 * 1024)}),
            )
            .await
    });
    // Let the OS apply backpressure. The test deadline diagnoses deadlock; no
    // production process/request receives an automatic duration abort.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let killed = tokio::time::timeout(
        Duration::from_secs(10),
        workspace.execute("pty_kill", &json!({"id":id,"cleanup":true})),
    )
    .await
    .expect("explicit kill must release concurrent blocked PTY input")
    .unwrap();
    assert_eq!(killed["output_complete"], true);
    let _ = tokio::time::timeout(Duration::from_secs(10), writer)
        .await
        .expect("blocked input must finish after explicit process cleanup")
        .unwrap();
    assert_reaped(pid).await;
}
