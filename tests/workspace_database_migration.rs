use anyhow::{ensure, Context, Result};
use openraid::{auth::AuthStore, config::Config, workspace::WorkspaceTools};
use serde_json::json;
use std::{path::Path, process::Command, time::Duration};

fn isolated_command(workspace: &Path, state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_openraid"));
    command
        .current_dir(workspace)
        .env("OPENRAID_AUTH_FILE", state.join("auth.json"))
        .env("XDG_CONFIG_HOME", state)
        .env("XDG_DATA_HOME", state);
    command
}

fn remember_database(workspace: &Path, state: &Path, database: &Path) -> Result<()> {
    let mut auth = AuthStore::load_with_opencode(state.join("auth.json"), None)?;
    auth.set_api_key("openai", "workspace-migration-fixture")?;
    auth.remember_launch(&Config {
        agents: 1,
        workspace: workspace.to_owned(),
        database: database.to_owned(),
        base_url: "http://127.0.0.1:1/v1".into(),
        grace_period: Duration::ZERO,
        ..Config::default()
    });
    auth.save()
}

fn setup_command(state: &Path, explicit_database: Option<&Path>) -> String {
    let executable = env!("CARGO_BIN_EXE_openraid");
    #[cfg(windows)]
    {
        let quote = |text: &str| text.replace('\'', "''");
        let mut command = format!(
            "Remove-Item Env:OPENRAID_PROVIDER,Env:OPENRAID_MODEL,Env:OPENRAID_VARIANT,Env:OPENRAID_BASE_URL -ErrorAction SilentlyContinue; \
             $env:OPENRAID_AUTH_FILE='{}'; $env:XDG_CONFIG_HOME='{}'; $env:XDG_DATA_HOME='{}'; \
             $env:OPENRAID_API_KEY='workspace-migration-fixture'; $env:OPENAI_API_KEY='workspace-migration-fixture'; & '{}' setup",
            quote(&state.join("auth.json").to_string_lossy()),
            quote(&state.to_string_lossy()),
            quote(&state.to_string_lossy()),
            quote(executable)
        );
        if let Some(database) = explicit_database {
            command.push_str(&format!(
                " --database '{}'",
                quote(&database.to_string_lossy())
            ));
        }
        command
    }
    #[cfg(not(windows))]
    {
        let quote = |text: &str| text.replace('\'', "'\\''");
        let mut command = format!(
            "env -u OPENRAID_PROVIDER -u OPENRAID_MODEL -u OPENRAID_VARIANT -u OPENRAID_BASE_URL \
             OPENRAID_AUTH_FILE='{}' XDG_CONFIG_HOME='{}' XDG_DATA_HOME='{}' \
             OPENRAID_API_KEY=workspace-migration-fixture OPENAI_API_KEY=workspace-migration-fixture '{}' setup",
            quote(&state.join("auth.json").to_string_lossy()),
            quote(&state.to_string_lossy()),
            quote(&state.to_string_lossy()),
            quote(executable)
        );
        if let Some(database) = explicit_database {
            command.push_str(&format!(
                " --database '{}'",
                quote(&database.to_string_lossy())
            ));
        }
        command
    }
}

async fn open_and_close_home(
    workspace: &Path,
    state: &Path,
    explicit_database: Option<&Path>,
    explicit_session: Option<&str>,
) -> Result<()> {
    let tools = WorkspaceTools::new(workspace, 1)?;
    let mut command = setup_command(state, explicit_database);
    if let Some(session) = explicit_session {
        // Catalog IDs contain only the generated session identifier.
        command.push_str(&format!(" --session {session}"));
    }
    let child = tools
        .execute(
            "pty_spawn",
            &json!({"command":command,"rows":30,"cols":120}),
        )
        .await?;
    let id = child["id"].as_str().context("missing console id")?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let page = tools.execute("pty_read", &json!({"id":id})).await?;
            if page["content"]
                .as_str()
                .unwrap_or_default()
                .contains("openraid by vuln.industries")
            {
                break;
            }
            ensure!(page["output_complete"] != true, "setup failed: {page}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tools
            .execute("pty_write", &json!({"id":id,"data":"\u{001b}"}))
            .await?;
        tokio::time::sleep(Duration::from_millis(80)).await;
        tools
            .execute("pty_write", &json!({"id":id,"data":"q"}))
            .await?;
        loop {
            let page = tools.execute("pty_read", &json!({"id":id})).await?;
            if page["output_complete"] == true {
                ensure!(page["exit_code"] == 0, "console failed: {page}");
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    // Always clean up the console, including assertion failures and timeouts.
    tools
        .execute("pty_kill", &json!({"id":id,"cleanup":true}))
        .await?;
    result??;
    Ok(())
}

fn assert_no_root_database(workspace: &Path) {
    for name in [
        "openraid.sqlite3",
        "openraid.sqlite3-wal",
        "openraid.sqlite3-shm",
    ] {
        assert!(
            !workspace.join(name).exists(),
            "unexpected root file {name}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_setup_does_not_recreate_deleted_legacy_database_from_auth() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let root = std::fs::canonicalize(workspace.path())?;
    remember_database(&root, state.path(), &root.join("openraid.sqlite3"))?;
    open_and_close_home(&root, state.path(), None, None).await?;
    assert!(root.join(".openraid/openraid.sqlite3").is_file());
    assert_no_root_database(&root);
    let saved = AuthStore::load_with_opencode(state.path().join("auth.json"), None)?;
    assert_eq!(
        saved.launch_profile_for(&root).unwrap().database,
        std::fs::canonicalize(root.join(".openraid/openraid.sqlite3"))?
    );
    // A second process verifies that the corrected persisted profile stays fixed.
    open_and_close_home(&root, state.path(), None, None).await?;
    assert_no_root_database(&root);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_setup_ignores_registered_legacy_root_session_without_modifying_it() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let root = std::fs::canonicalize(workspace.path())?;
    let legacy = root.join("openraid.sqlite3");
    let output = isolated_command(&root, state.path())
        .args([
            "demo",
            "legacy session sentinel",
            "--agents",
            "1",
            "--no-tui",
            "--grace-secs",
            "0",
            "--database",
        ])
        .arg(&legacy)
        .output()?;
    ensure!(
        output.status.success(),
        "demo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    remember_database(&root, state.path(), &legacy)?;
    let before = [
        "openraid.sqlite3",
        "openraid.sqlite3-wal",
        "openraid.sqlite3-shm",
    ]
    .map(|name| (name, std::fs::read(root.join(name)).ok()));
    open_and_close_home(&root, state.path(), None, None).await?;
    assert!(root.join(".openraid/openraid.sqlite3").is_file());
    for (name, contents) in before {
        assert_eq!(
            std::fs::read(root.join(name)).ok(),
            contents,
            "implicit setup modified legacy file {name}"
        );
    }
    let output = isolated_command(&root, state.path())
        .args(["sessions", "--json"])
        .output()?;
    ensure!(output.status.success(), "session listing failed");
    let sessions: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let legacy_session = sessions
        .as_array()
        .context("session array missing")?
        .iter()
        .find(|entry| {
            entry["database"]
                .as_str()
                .is_some_and(|database| Path::new(database) == legacy)
        })
        .context("legacy catalog entry must remain available")?;
    let id = legacy_session["id"]
        .as_str()
        .context("session id missing")?;
    open_and_close_home(&root, state.path(), None, Some(id)).await?;
    let saved = AuthStore::load_with_opencode(state.path().join("auth.json"), None)?;
    assert_eq!(
        saved.launch_profile_for(&root).unwrap().database,
        std::fs::canonicalize(&legacy)?,
        "explicit session selection must preserve the original history location"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_setup_preserves_remembered_custom_database() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let root = std::fs::canonicalize(workspace.path())?;
    let custom = root.join("custom/history.sqlite3");
    remember_database(&root, state.path(), &custom)?;
    open_and_close_home(&root, state.path(), None, None).await?;
    assert!(custom.is_file());
    assert!(!root.join(".openraid/openraid.sqlite3").exists());
    assert_no_root_database(&root);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_setup_honors_explicit_legacy_root_database() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let root = std::fs::canonicalize(workspace.path())?;
    let legacy = root.join("openraid.sqlite3");
    remember_database(&root, state.path(), &legacy)?;
    open_and_close_home(&root, state.path(), Some(&legacy), None).await?;
    assert!(legacy.is_file());
    assert!(!root.join(".openraid/openraid.sqlite3").exists());
    Ok(())
}
