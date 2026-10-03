use serde_json::Value;
use std::process::{Command, Output};

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_openraid"))
}

fn successful_json(output: Output) -> Value {
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI emits JSON")
}

#[test]
fn actual_binary_demo_owner_injection_and_cursor_pagination() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("swarm.sqlite3");
    let output = command()
        .args(["demo", "--agents", "4", "--no-tui", "--grace-secs", "0"])
        .arg("--workspace")
        .arg(directory.path())
        .arg("--database")
        .arg(&database)
        .output()
        .unwrap();
    assert!(successful_json(output).is_object());

    let injected = successful_json(
        command()
            .args(["post", "owner integration correction", "--database"])
            .arg(&database)
            .output()
            .unwrap(),
    );
    assert_eq!(injected["owner"], true);
    assert_eq!(injected["sender"], "owner");
    let sequence = injected["seq"].as_u64().unwrap();
    let page = successful_json(
        command()
            .args(["board", "--database"])
            .arg(&database)
            .arg("--after")
            .arg((sequence - 1).to_string())
            .args(["--limit", "1"])
            .output()
            .unwrap(),
    );
    assert_eq!(page.as_array().unwrap().len(), 1);
    assert_eq!(page[0], injected);
}

#[test]
fn invalid_agent_count_is_rejected_before_database_creation() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("should-not-exist.sqlite3");
    let output = command()
        .args(["demo", "--agents", "501", "--no-tui"])
        .arg("--workspace")
        .arg(directory.path())
        .arg("--database")
        .arg(&database)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("between 1 and 500"));
    assert!(!database.exists());
}

#[test]
fn help_does_not_expose_environment_credentials() {
    let secret = "openraid-help-must-not-leak-this-value";
    let output = command()
        .args(["run", "--help"])
        .env("OPENRAID_API_KEY", secret)
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("OPENRAID_API_KEY"));
    assert!(!help.contains(secret));
}

#[test]
fn auth_list_includes_saved_custom_and_configured_providers_without_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let auth = directory.path().join("auth.json");
    let output = command()
        .args([
            "auth",
            "connect",
            "saved-fixture",
            "--api-key",
            "private-saved-key",
        ])
        .env("OPENRAID_AUTH_FILE", &auth)
        .env("XDG_DATA_HOME", directory.path())
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    std::fs::write(
        directory.path().join("openraid.json"),
        r#"{
        "provider":{"configured-fixture":{"options":{"apiKey":"private-configured-key"}}}
    }"#,
    )
    .unwrap();
    let output = command()
        .args(["auth", "list"])
        .env("OPENRAID_AUTH_FILE", &auth)
        .env("XDG_DATA_HOME", directory.path())
        .env("XDG_CONFIG_HOME", directory.path())
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("saved-fixture  connected"));
    assert!(text.contains("configured-fixture  connected"));
    assert!(!text.contains("private-saved-key"));
    assert!(!text.contains("private-configured-key"));
}

#[test]
fn session_history_is_scoped_to_cwd_and_explicit_ids_can_cross_workspaces() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let auth = directory.path().join("auth.json");
    let isolated = || {
        let mut cmd = command();
        cmd.env("OPENRAID_AUTH_FILE", &auth)
            .env("XDG_CONFIG_HOME", directory.path());
        cmd
    };
    for (workspace, objective) in [
        (&first, "first project task"),
        (&second, "second project task"),
    ] {
        successful_json(
            isolated()
                .args([
                    "demo",
                    objective,
                    "--agents",
                    "2",
                    "--no-tui",
                    "--grace-secs",
                    "0",
                ])
                .current_dir(workspace)
                .output()
                .unwrap(),
        );
    }
    let local = successful_json(
        isolated()
            .args(["sessions", "--json"])
            .current_dir(&first)
            .output()
            .unwrap(),
    );
    assert_eq!(local.as_array().unwrap().len(), 1);
    assert_eq!(local[0]["title"], "first project task");
    let id = local[0]["id"].as_str().unwrap();
    let database = std::path::PathBuf::from(local[0]["database"].as_str().unwrap());
    let all = successful_json(
        isolated()
            .args(["sessions", "--all", "--json"])
            .current_dir(&second)
            .output()
            .unwrap(),
    );
    assert_eq!(all.as_array().unwrap().len(), 2);
    // An explicit ID selects the original workspace even when invoked elsewhere.
    successful_json(
        isolated()
            .args([
                "demo",
                "explicit reopen",
                "--session",
                id,
                "--no-tui",
                "--grace-secs",
                "0",
            ])
            .current_dir(&second)
            .output()
            .unwrap(),
    );
    let board = successful_json(
        isolated()
            .arg("board")
            .arg("--database")
            .arg(&database)
            .output()
            .unwrap(),
    );
    assert!(board.as_array().unwrap().iter().any(|entry| entry["body"]
        .as_str()
        .is_some_and(|body| body.contains("explicit reopen"))));
    successful_json(
        isolated()
            .args([
                "demo",
                "separate task",
                "--new",
                "--agents",
                "1",
                "--no-tui",
                "--grace-secs",
                "0",
            ])
            .current_dir(&first)
            .output()
            .unwrap(),
    );
    let local = successful_json(
        isolated()
            .args(["sessions", "--json"])
            .current_dir(&first)
            .output()
            .unwrap(),
    );
    assert_eq!(local.as_array().unwrap().len(), 2);
    let mismatch = isolated()
        .args(["demo", "--session", id, "--workspace"])
        .arg(&second)
        .args(["--no-tui"])
        .output()
        .unwrap();
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("belongs to"));
    let durable_state = || {
        rusqlite::Connection::open(&database)
            .unwrap()
            .query_row(
                "SELECT revision, next_id,
                (SELECT group_concat(agent_id || ':' || active, '|') FROM members),
                (SELECT draining FROM membership_phase),
                (SELECT COUNT(*) FROM votes), (SELECT COUNT(*) FROM board)
             FROM membership_state",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .unwrap()
    };
    let before = durable_state();
    let wrong_database = isolated()
        .args([
            "demo",
            "must not mutate another project",
            "--agents",
            "3",
            "--database",
        ])
        .arg(&database)
        .args(["--no-tui", "--grace-secs", "0"])
        .current_dir(&second)
        .output()
        .unwrap();
    assert!(!wrong_database.status.success());
    assert!(String::from_utf8_lossy(&wrong_database.stderr).contains("belongs to"));
    assert_eq!(
        durable_state(),
        before,
        "rejected open must not mutate durable session state"
    );
    let missing = isolated()
        .args(["demo", "--session", "does-not-exist", "--no-tui"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("unknown session"));
}
