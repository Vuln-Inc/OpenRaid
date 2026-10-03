//! Optional adapter bridge for providers with specialized cloud authentication
//! and protocols. The AI SDK only generates; native tools still run in Rust.

use crate::provider::{Completion, ContextOverflow, ProviderConfig};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio, sync::OnceLock};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{mpsc, oneshot, Mutex},
};

type Reply = std::result::Result<Completion, BridgeFailure>;
type Job = (Value, oneshot::Sender<Reply>);
static BRIDGE: OnceLock<Mutex<Option<mpsc::Sender<Job>>>> = OnceLock::new();

#[derive(Clone, Debug, Deserialize)]
struct BridgeFailure {
    message: String,
    #[serde(default)]
    retryable: bool,
    #[serde(default)]
    context_overflow: bool,
}

#[derive(Debug)]
pub struct RetryableBridgeError(pub String);

impl std::fmt::Display for RetryableBridgeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RetryableBridgeError {}

pub fn is_retryable(error: &anyhow::Error) -> bool {
    error.downcast_ref::<RetryableBridgeError>().is_some()
}

#[derive(Deserialize)]
struct Envelope {
    id: u64,
    completion: Option<Completion>,
    error: Option<BridgeFailure>,
}

pub async fn complete(
    config: &ProviderConfig,
    options: &Value,
    headers: &BTreeMap<String, String>,
    messages: &Value,
    tools: &[Value],
) -> Result<Completion> {
    let payload = json!({
        "model": config.model,
        "baseURL": config.base_url,
        "apiKey": config.api_key,
        "maxOutputTokens": config.max_output_tokens,
        "options": options,
        "headers": headers,
        "messages": messages,
        "tools": tools,
    });
    let mut shared = BRIDGE.get_or_init(|| Mutex::new(None)).lock().await;
    if shared.as_ref().is_none_or(|sender| sender.is_closed()) {
        *shared = Some(spawn_bridge()?);
    }
    let sender = shared.as_ref().context("SDK bridge unavailable")?.clone();
    drop(shared);
    let (reply_tx, reply_rx) = oneshot::channel();
    sender
        .send((payload, reply_tx))
        .await
        .map_err(|_| RetryableBridgeError("SDK bridge restarted; retry request".into()))?;
    match reply_rx
        .await
        .map_err(|_| RetryableBridgeError("SDK bridge task stopped; retry request".into()))?
    {
        Ok(completion) => Ok(completion),
        Err(error) if error.context_overflow => Err(ContextOverflow.into()),
        Err(error) if error.retryable => Err(RetryableBridgeError(error.message).into()),
        Err(error) => bail!("SDK bridge: {}", error.message),
    }
}

/// Discovery shares the same multiplexed sidecar as generation, but never
/// produces tool calls or consumes a model request/output budget.
pub async fn discover_gitlab_models(
    workspace: &std::path::Path,
    base_url: &str,
    api_key: Option<&str>,
    settings: &Value,
) -> Result<Vec<Value>> {
    let config = ProviderConfig {
        base_url: base_url.to_owned(),
        api_key: api_key.map(str::to_owned),
        model: String::new(),
        max_in_flight: 1,
        max_output_tokens: 1,
    };
    let options = json!({"_openraid_sdk": {
        "npm":"gitlab-ai-provider", "provider":"gitlab", "action":"discover-models",
        "workspace":workspace, "settings":settings,
    }});
    Ok(
        complete(&config, &options, &BTreeMap::new(), &json!([]), &[])
            .await?
            .response_items,
    )
}

/// A single optional sidecar is shared by every provider/agent. Request IDs
/// multiplex concurrent generations without spawning hundreds of Node runtimes.
fn spawn_bridge() -> Result<mpsc::Sender<Job>> {
    spawn_bridge_at(script_path()?, 32 * 1024 * 1024)
}

fn spawn_bridge_at(script: PathBuf, max_response_bytes: usize) -> Result<mpsc::Sender<Job>> {
    let node = std::env::var_os("OPENRAID_NODE").unwrap_or_else(|| "node".into());
    let mut command = Command::new(node);
    command
        .arg(script)
        .arg("--server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .context("starting SDK bridge; install Node.js 22.12+ and run npm ci --prefix scripts")?;
    let mut input = child.stdin.take().context("opening SDK bridge input")?;
    let stdout = child.stdout.take().context("opening SDK bridge output")?;
    let mut output = BufReader::new(stdout);
    let (sender, mut jobs) = mpsc::channel::<Job>(128);
    tokio::spawn(async move {
        let mut pending = BTreeMap::<u64, oneshot::Sender<Reply>>::new();
        let mut id = 0_u64;
        let mut responded = false;
        let mut output_buffer = Vec::new();
        let failure = loop {
            tokio::select! {
                job = jobs.recv() => {
                    let Some((request, reply)) = job else { break ("SDK bridge closed", false); };
                    id = id.wrapping_add(1);
                    let mut payload = match serde_json::to_vec(&json!({"id":id,"request":request})) {
                        Ok(payload) => payload,
                        Err(_) => { let _ = reply.send(Err(BridgeFailure { message:"invalid SDK request".into(),retryable:false,context_overflow:false })); continue; }
                    };
                    if payload.len() > 32 * 1024 * 1024 {
                        let _ = reply.send(Err(BridgeFailure { message:"SDK bridge request exceeded 32 MiB".into(),retryable:false,context_overflow:false }));
                        continue;
                    }
                    payload.push(b'\n');
                    pending.insert(id, reply);
                    if input.write_all(&payload).await.is_err() {
                        break ("SDK bridge input closed; verify Node.js and bridge dependencies", responded);
                    }
                }
                chunk = output.fill_buf() => {
                    let chunk = match chunk {
                        Ok(chunk) if !chunk.is_empty() => chunk,
                        _ => break ("SDK bridge stopped; verify Node.js and run npm ci --prefix scripts", responded),
                    };
                    let newline = chunk.iter().position(|byte| *byte == b'\n');
                    let take = newline.map_or(chunk.len(), |offset| offset + 1);
                    if output_buffer.len() + take > max_response_bytes { break ("SDK bridge response exceeded frame limit", false); }
                    output_buffer.extend_from_slice(&chunk[..take]);
                    output.consume(take);
                    if newline.is_none() { continue; }
                    let envelope = match serde_json::from_slice::<Envelope>(&output_buffer) {
                        Ok(envelope) => envelope,
                        Err(_) => break ("invalid SDK bridge response", false),
                    };
                    output_buffer.clear();
                    responded = true;
                    if let Some(reply) = pending.remove(&envelope.id) {
                        let result = match (envelope.completion, envelope.error) {
                            (Some(completion), _) => Ok(completion),
                            (_, Some(error)) => Err(error),
                            _ => Err(BridgeFailure { message:"missing SDK completion".into(),retryable:false,context_overflow:false }),
                        };
                        let _ = reply.send(result);
                    }
                }
                _ = child.wait() => break ("SDK bridge process exited; verify Node.js and bridge dependencies", responded),
            }
        };
        jobs.close();
        let failure = BridgeFailure {
            message: failure.0.into(),
            retryable: failure.1,
            context_overflow: false,
        };
        for (_, reply) in pending {
            let _ = reply.send(Err(failure.clone()));
        }
        while let Ok((_, reply)) = jobs.try_recv() {
            let _ = reply.send(Err(failure.clone()));
        }
        let _ = child.kill().await;
    });
    Ok(sender)
}

pub fn script_path() -> Result<PathBuf> {
    if let Some(directory) = std::env::var_os("OPENRAID_SDK_BRIDGE_DIR") {
        let path = PathBuf::from(directory).join("sdk-bridge.mjs");
        if path.is_file() {
            return Ok(path);
        }
        bail!("OPENRAID_SDK_BRIDGE_DIR must contain sdk-bridge.mjs and installed npm dependencies");
    }
    let mut candidates = Vec::new();
    if let Ok(directory) = std::env::current_dir() {
        candidates.push(directory.join("scripts/sdk-bridge.mjs"));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            candidates.push(directory.join("scripts/sdk-bridge.mjs"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/sdk-bridge.mjs"));
    candidates.into_iter().find(|path| path.is_file()).context(
        "SDK bridge script missing; set OPENRAID_SDK_BRIDGE_DIR to the installed scripts directory",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires Node.js 22.12+; exercises actual sidecar process lifecycle"]
    async fn sidecar_crash_fails_pending_and_restarts_and_oversize_is_bounded() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let script = directory.path().join("fixture.mjs");
        std::fs::write(
            &script,
            r#"
import {createInterface} from 'node:readline';
for await (const line of createInterface({input:process.stdin})) {
  const {id,request}=JSON.parse(line);
  if (request.crash) process.exit(7);
  if (request.huge) { process.stdout.write('x'.repeat(8192)); continue; }
  if (request.pending) continue;
  process.stdout.write(JSON.stringify({id,completion:{content:'ready',tool_calls:[],usage:{input_tokens:1,output_tokens:1,cached_tokens:0},finish_reason:'stop'}})+'\n');
}
"#,
        )?;
        let sender = spawn_bridge_at(script.clone(), 4096)?;
        let (reply, receive) = oneshot::channel();
        sender.send((json!({}), reply)).await?;
        assert_eq!(
            receive
                .await?
                .map_err(|failure| anyhow::anyhow!(failure.message))?
                .content,
            "ready"
        );
        let (pending, pending_receive) = oneshot::channel();
        sender.send((json!({"pending":true}), pending)).await?;
        let (crash, crash_receive) = oneshot::channel();
        sender.send((json!({"crash":true}), crash)).await?;
        assert!(pending_receive.await?.unwrap_err().retryable);
        assert!(crash_receive.await?.unwrap_err().retryable);
        assert!(sender.is_closed());
        let restarted = spawn_bridge_at(script, 4096)?;
        let (reply, receive) = oneshot::channel();
        restarted.send((json!({}), reply)).await?;
        assert_eq!(
            receive
                .await?
                .map_err(|failure| anyhow::anyhow!(failure.message))?
                .content,
            "ready"
        );
        let (reply, receive) = oneshot::channel();
        restarted.send((json!({"huge":true}), reply)).await?;
        let failure = receive.await?.unwrap_err();
        assert!(!failure.retryable);
        assert!(failure.message.contains("frame limit"));
        Ok(())
    }
}
