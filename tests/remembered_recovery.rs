use anyhow::{ensure, Context, Result};
use openraid::{auth::AuthStore, config::Config, storage::Store, workspace::WorkspaceTools};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn read_request(socket: &mut TcpStream) -> Result<Value> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let end = loop {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request ended before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
        ensure!(bytes.len() < 64 * 1024, "unexpected fixture headers");
    };
    let length = std::str::from_utf8(&bytes[..end])?
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .context("missing content length")?;
    ensure!(length < 1024 * 1024, "unexpected fixture body size");
    while bytes.len() < end + length {
        let count = socket.read(&mut chunk).await?;
        ensure!(count > 0, "request body ended early");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(&bytes[end..end + length])?)
}

fn setup_command(root: &Path, auth: &Path) -> String {
    let executable = env!("CARGO_BIN_EXE_openraid");
    #[cfg(windows)]
    {
        let quote = |text: &str| text.replace('\'', "''");
        format!(
            "Remove-Item Env:OPENRAID_PROVIDER,Env:OPENRAID_MODEL,Env:OPENRAID_VARIANT,Env:OPENRAID_BASE_URL -ErrorAction SilentlyContinue; \
             $env:OPENRAID_AUTH_FILE='{}'; $env:XDG_CONFIG_HOME='{}'; $env:XDG_DATA_HOME='{}'; \
             $env:OPENRAID_API_KEY='remembered-fixture'; $env:OPENAI_API_KEY='remembered-fixture'; & '{}' setup",
            quote(&auth.to_string_lossy()), quote(&root.to_string_lossy()),
            quote(&root.to_string_lossy()), quote(executable)
        )
    }
    #[cfg(not(windows))]
    {
        let quote = |text: &str| text.replace('\'', "'\\''");
        format!(
            "env -u OPENRAID_PROVIDER -u OPENRAID_MODEL -u OPENRAID_VARIANT -u OPENRAID_BASE_URL \
             OPENRAID_AUTH_FILE='{}' XDG_CONFIG_HOME='{}' XDG_DATA_HOME='{}' \
             OPENRAID_API_KEY=remembered-fixture OPENAI_API_KEY=remembered-fixture '{}' setup",
            quote(&auth.to_string_lossy()),
            quote(&root.to_string_lossy()),
            quote(&root.to_string_lossy()),
            quote(executable)
        )
    }
}

async fn exit_console(workspace: &WorkspaceTools, id: &str) -> Result<()> {
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
    workspace
        .execute("pty_kill", &json!({"id":id,"cleanup":true}))
        .await?;
    Ok(())
}

async fn wait_for_console(workspace: &WorkspaceTools, id: &str) -> Result<()> {
    loop {
        let page = workspace.execute("pty_read", &json!({"id":id})).await?;
        if page["content"]
            .as_str()
            .unwrap_or_default()
            .contains("openraid by vuln.industries")
        {
            return Ok(());
        }
        ensure!(
            page["output_complete"] != true,
            "remembered home exited: {page}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_setup_keeps_unfinished_home_idle_until_explicit_resume_then_completed_home_idle(
) -> Result<()> {
    // This timeout belongs solely to the regression harness, never to a model,
    // native operation, or production recovery policy.
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir()?;
        let root = std::fs::canonicalize(directory.path())?;
        let database = root.join("remembered.sqlite3");
        let auth_path = root.join("auth.json");
        let store = Store::open(&database).await?;
        let roster = store.initialize_membership(2, false).await?;
        let prompt = store.append("owner", "remembered unfinished task sentinel", true).await?;
        store.save_checkpoint(&roster.agent_ids[0], &json!({"cursor":prompt.seq,"messages":[
            {"role":"user","content":"remembered checkpoint history sentinel"}
        ]})).await?;
        store.flush().await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let mut auth = AuthStore::load_with_opencode(&auth_path, None)?;
        auth.set_api_key("openai", "remembered-fixture")?;
        auth.set_selection("openai", "gpt-4.1-mini", None);
        auth.remember_launch(&Config {
            agents: 1, // Recovery must preserve the actual durable two-member roster.
            workspace: root.clone(), database: database.clone(),
            base_url: format!("http://{address}/v1"), grace_period: Duration::ZERO,
            ..Config::default()
        });
        auth.save()?;
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_request_count = request_count.clone();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for index in 0..2 {
                let (mut socket, _) = listener.accept().await?;
                server_request_count.fetch_add(1, Ordering::SeqCst);
                requests.push(read_request(&mut socket).await?);
                let body = json!({"choices":[{"message":{"role":"assistant","tool_calls":[{
                    "id":format!("recovered-vote-{index}"),"type":"function","function":{
                        "name":"vote_done","arguments":"{\"done\":true,\"evidence\":\"public remembered recovery verified\"}"
                    }
                }]},"finish_reason":"tool_calls"}]}).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
                socket.shutdown().await?;
            }
            Ok::<_, anyhow::Error>(requests)
        });
        let workspace = WorkspaceTools::new(&root, 2)?;
        let command = setup_command(&root, &auth_path);
        let unfinished_cursor = store.latest_seq().await?;
        let checkpoint = store.load_checkpoint(&roster.agent_ids[0]).await?;
        let idle = workspace.execute("pty_spawn", &json!({"command":command,"rows":30,"cols":120})).await?;
        let idle_id = idle["id"].as_str().context("idle console id missing")?;
        wait_for_console(&workspace, idle_id).await?;
        // Give startup and the runtime's board-refresh loop time to act, so a
        // rendered console alone cannot mask an automatically resumed round.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(request_count.load(Ordering::SeqCst), 0, "reopening must not send provider requests");
        assert_eq!(store.latest_seq().await?, unfinished_cursor, "reopening must not start a round or append board messages");
        assert_eq!(store.membership().await?.agent_ids, roster.agent_ids, "idle reopening preserves the durable roster");
        assert_eq!(store.load_checkpoint(&roster.agent_ids[0]).await?, checkpoint);
        assert_eq!(store.unfinished_prompt().await?, Some(prompt.clone()));
        exit_console(&workspace, idle_id).await?;
        assert_eq!(store.latest_seq().await?, unfinished_cursor, "exiting the idle console must not drain unfinished work");
        assert_eq!(store.unfinished_prompt().await?, Some(prompt));

        let resume_command = format!("{command} --resume");
        let child = workspace.execute("pty_spawn", &json!({"command":resume_command,"rows":30,"cols":120})).await?;
        let id = child["id"].as_str().context("native console id missing")?;
        loop {
            if store.read_board(0, 100).await?.iter().any(|entry| entry.body == "all workers drained; swarm complete") {
                break;
            }
            let page = workspace.execute("pty_read", &json!({"id":id})).await?;
            ensure!(page["output_complete"] != true, "setup exited without recovery: {page}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let requests = server.await??;
        assert!(requests.iter().all(|request| request["messages"].to_string().contains("remembered unfinished task sentinel")));
        assert!(requests.iter().any(|request| request["messages"].to_string().contains("remembered checkpoint history sentinel")));
        assert_eq!(store.membership().await?.agent_ids, roster.agent_ids);
        assert_eq!(store.prompts().await?.len(), 1, "explicit recovery does not repost the task");
        exit_console(&workspace, id).await?;
        assert!(store.unfinished_prompt().await?.is_none());
        let completed_cursor = store.latest_seq().await?;

        let idle = workspace.execute("pty_spawn", &json!({"command":command,"rows":30,"cols":120})).await?;
        let idle_id = idle["id"].as_str().context("idle console id missing")?;
        wait_for_console(&workspace, idle_id).await?;
        exit_console(&workspace, idle_id).await?;
        assert_eq!(store.latest_seq().await?, completed_cursor, "completed history is audit-only at the remembered home");
        assert_eq!(store.prompts().await?.len(), 1);
        Ok::<_, anyhow::Error>(())
    }).await?
}
