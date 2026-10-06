use anyhow::{ensure, Context, Result};
use openraid::{auth::AuthStore, config::Config, workspace::WorkspaceTools};
use serde_json::json;
use std::{path::Path, time::Duration};

fn setup_command(root: &Path) -> String {
    let executable = env!("CARGO_BIN_EXE_openraid");
    #[cfg(windows)]
    {
        let quote = |value: &str| value.replace('\'', "''");
        format!(
            "Remove-Item Env:OPENRAID_PROVIDER,Env:OPENRAID_MODEL,Env:OPENRAID_VARIANT,Env:OPENRAID_BASE_URL,Env:OPENRAID_API_KEY -ErrorAction SilentlyContinue; \
             $env:OPENRAID_AUTH_FILE='{}'; $env:XDG_CONFIG_HOME='{}'; $env:XDG_DATA_HOME='{}'; & '{}' setup",
            quote(&root.join("auth.json").to_string_lossy()),
            quote(&root.to_string_lossy()),
            quote(&root.to_string_lossy()),
            quote(executable),
        )
    }
    #[cfg(not(windows))]
    {
        let quote = |value: &str| value.replace('\'', "'\\''");
        format!(
            "env -u OPENRAID_PROVIDER -u OPENRAID_MODEL -u OPENRAID_VARIANT -u OPENRAID_BASE_URL -u OPENRAID_API_KEY \
             OPENRAID_AUTH_FILE='{}' XDG_CONFIG_HOME='{}' XDG_DATA_HOME='{}' '{}' setup",
            quote(&root.join("auth.json").to_string_lossy()),
            quote(&root.to_string_lossy()),
            quote(&root.to_string_lossy()),
            quote(executable),
        )
    }
}

async fn wait_for_text(workspace: &WorkspaceTools, id: &str, text: &str) -> Result<String> {
    loop {
        let page = workspace.execute("pty_read", &json!({"id":id})).await?;
        let content = page["content"].as_str().unwrap_or_default();
        if content.contains(text) {
            return Ok(content.to_owned());
        }
        ensure!(
            page["output_complete"] != true,
            "console exited before {text}: {page}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn open_and_close(
    workspace: &WorkspaceTools,
    root: &Path,
    consent: Option<bool>,
) -> Result<()> {
    let spawned = workspace
        .execute(
            "pty_spawn",
            &json!({"command":setup_command(root),"rows":30,"cols":120}),
        )
        .await?;
    let id = spawned["id"].as_str().context("console id missing")?;
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        if let Some(enabled) = consent {
            let prompt =
                wait_for_text(workspace, id, "Existing OpenCode information found").await?;
            ensure!(
                !prompt.contains("import-account-secret"),
                "prompt exposed a credential"
            );
            assert_eq!(
                AuthStore::load_with_opencode(root.join("auth.json"), None)?
                    .opencode_import_consent(),
                None
            );
            // Decline is the initial selection; importing requires moving to it.
            let data = if enabled { "\u{001b}[B\r" } else { "\r" };
            workspace
                .execute("pty_write", &json!({"id":id,"data":data}))
                .await?;
        }
        // The consent picker also renders the application branding. Wait for
        // the actual dashboard before sending exit keys; otherwise Unix can
        // receive them while setup is still transitioning out of the picker.
        let console = wait_for_text(workspace, id, "IDLE").await?;
        if consent.is_none() {
            ensure!(
                !console.contains("Existing OpenCode information found"),
                "remembered choice prompted again"
            );
        }
        workspace
            .execute("pty_write", &json!({"id":id,"data":"\u{001b}"}))
            .await?;
        tokio::time::sleep(Duration::from_millis(80)).await;
        workspace
            .execute("pty_write", &json!({"id":id,"data":"q"}))
            .await?;
        loop {
            let page = workspace.execute("pty_read", &json!({"id":id})).await?;
            if page["output_complete"] == true {
                ensure!(page["exit_code"] == 0, "console failed: {page}");
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    let diagnostic = if result.is_err() {
        Some(workspace.execute("pty_read", &json!({"id":id})).await?)
    } else {
        None
    };
    workspace
        .execute("pty_kill", &json!({"id":id,"cleanup":true}))
        .await?;
    result.with_context(|| format!("consent console timed out: {diagnostic:?}"))??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_startup_requires_consent_and_remembers_both_choices() -> Result<()> {
    for enabled in [false, true] {
        let directory = tempfile::tempdir()?;
        let root = std::fs::canonicalize(directory.path())?;
        std::fs::create_dir(root.join("opencode"))?;
        std::fs::write(
            root.join("opencode/auth.json"),
            json!({
                "openrouter":{"type":"api","key":"import-account-secret"}
            })
            .to_string(),
        )?;
        let mut auth = AuthStore::load_with_opencode(root.join("auth.json"), None)?;
        auth.set_api_key("openai", "local-fixture-key")?;
        auth.remember_launch(&Config {
            workspace: root.clone(),
            database: root.join("consent.sqlite3"),
            agents: 1,
            ..Config::default()
        });
        auth.save()?;
        let workspace = WorkspaceTools::new(&root, 1)?;
        open_and_close(&workspace, &root, Some(enabled)).await?;
        assert_eq!(
            AuthStore::load_with_opencode(root.join("auth.json"), None)?.opencode_import_consent(),
            Some(enabled)
        );
        open_and_close(&workspace, &root, None).await?;
    }
    Ok(())
}
