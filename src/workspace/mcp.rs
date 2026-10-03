//! One shared MCP connection per configured server, never one process per agent.
use anyhow::{ensure, Context, Result};
use reqwest_mcp as http_client;
use rmcp::{
    model::{CallToolRequestParams, ClientConfig},
    service::{RoleClient, RunningService},
    transport::{
        streamable_http_client::{
            SseError, StreamableHttpClient, StreamableHttpClientTransportConfig,
            StreamableHttpError, StreamableHttpPostResponse,
        },
        StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::{
    process::{Child, Command},
    sync::{watch, Mutex as AsyncMutex, Semaphore},
    task::JoinHandle,
};

#[derive(Clone, Default)]
pub struct ServerConfig {
    pub kind: String,
    pub command: Vec<String>,
    pub url: String,
    pub environment: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub enabled: bool,
}
impl ServerConfig {
    fn parse(value: &Value) -> Result<Self> {
        let mut command = if let Some(command) = value["command"].as_str() {
            vec![command.to_owned()]
        } else {
            value["command"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        if let Some(args) = value["args"].as_array() {
            command.extend(args.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        let string_map = |value: &Value| -> BTreeMap<String, String> {
            value
                .as_object()
                .map(|map| {
                    map.iter()
                        .filter_map(|(name, value)| {
                            value.as_str().map(|value| (name.clone(), value.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let url = value["url"].as_str().unwrap_or_default().to_owned();
        let kind = value["type"]
            .as_str()
            .unwrap_or(if command.is_empty() {
                "remote"
            } else {
                "local"
            })
            .to_owned();
        let enabled =
            value["enabled"].as_bool().unwrap_or(true) && value["disabled"].as_bool() != Some(true);
        ensure!(
            matches!(kind.as_str(), "local" | "stdio" | "remote" | "http"),
            "unsupported MCP transport type"
        );
        ensure!(
            !enabled
                || if matches!(kind.as_str(), "local" | "stdio") {
                    !command.is_empty()
                } else {
                    !url.is_empty()
                },
            "MCP server needs a command or URL for its transport"
        );
        Ok(Self {
            kind,
            command,
            url,
            environment: string_map(
                value
                    .get("environment")
                    .or_else(|| value.get("env"))
                    .unwrap_or(&Value::Null),
            ),
            headers: string_map(&value["headers"]),
            enabled,
        })
    }
}

pub fn load(workspace: &Path, explicit: Option<&Path>) -> Result<BTreeMap<String, ServerConfig>> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".config"))
        });
    let mut paths = Vec::new();
    if let Some(root) = root {
        for file in ["opencode.json", "opencode.jsonc"] {
            let path = root.join("opencode").join(file);
            if path.is_file() {
                paths.push(path);
            }
        }
    }
    let local = explicit.map(PathBuf::from).or_else(|| {
        [
            "openraid.json",
            "openraid.jsonc",
            "opencode.json",
            "opencode.jsonc",
        ]
        .iter()
        .map(|file| workspace.join(file))
        .find(|file| file.is_file())
    });
    if let Some(path) = local {
        paths.retain(|old| old != &path);
        paths.push(path);
    }
    load_paths(workspace, &paths)
}

fn merge_config(base: &mut Value, overlay: &Value) {
    if let (Some(base), Some(overlay)) = (base.as_object_mut(), overlay.as_object()) {
        for (name, value) in overlay {
            merge_config(base.entry(name.clone()).or_insert(Value::Null), value);
        }
    } else {
        *base = overlay.clone();
    }
}

fn load_paths(workspace: &Path, paths: &[PathBuf]) -> Result<BTreeMap<String, ServerConfig>> {
    let mut servers = BTreeMap::<String, Value>::new();
    for path in paths {
        let value: Value = json5::from_str(&std::fs::read_to_string(path)?)?;
        let mcp = value
            .get("mcp")
            .or_else(|| value.get("mcpServers"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mcp =
            crate::provider_settings::expand_config(&mcp, path.parent().unwrap_or(workspace))?;
        if let Some(entries) = mcp.as_object() {
            for (name, value) in entries {
                if value == &Value::Bool(false) {
                    servers.remove(name);
                    continue;
                }
                merge_config(servers.entry(name.clone()).or_insert(Value::Null), value);
            }
        }
    }
    servers
        .into_iter()
        .map(|(name, value)| {
            let config = ServerConfig::parse(&value)
                .with_context(|| format!("invalid MCP server {name}"))?;
            Ok((name, config))
        })
        .collect()
}

struct RemoteLifecycle {
    stop: watch::Sender<bool>,
    requests: Arc<Semaphore>,
}
impl RemoteLifecycle {
    async fn close(&self) {
        self.stop.send_replace(true);
        let _ = self.requests.acquire_many(u32::MAX).await;
    }
}

// rmcp's bootstrap HTTP POST awaits outside its own cancellation select. Keep
// an outer owned lifecycle so explicit shutdown also releases held handshakes.
#[derive(Clone)]
struct LifecycleHttpClient {
    client: http_client::Client,
    lifecycle: Arc<RemoteLifecycle>,
}
impl LifecycleHttpClient {
    async fn stream(
        &self,
        stream: futures_util::stream::BoxStream<
            'static,
            std::result::Result<sse_stream::Sse, SseError>,
        >,
    ) -> std::result::Result<
        futures_util::stream::BoxStream<'static, std::result::Result<sse_stream::Sse, SseError>>,
        StreamableHttpError<http_client::Error>,
    > {
        use futures_util::StreamExt;
        let stop = self.lifecycle.stop.subscribe();
        let permit = self
            .lifecycle
            .requests
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| StreamableHttpError::TransportChannelClosed)?;
        Ok(futures_util::stream::unfold(
            (stream, stop, permit),
            |(mut stream, mut stop, permit)| async move {
                if *stop.borrow() {
                    return None;
                }
                tokio::select! {
                    item = stream.next() => item.map(|item| (item, (stream, stop, permit))),
                    _ = stop.changed() => None,
                }
            },
        )
        .boxed())
    }
    async fn response(
        &self,
        response: StreamableHttpPostResponse,
    ) -> std::result::Result<StreamableHttpPostResponse, StreamableHttpError<http_client::Error>>
    {
        match response {
            StreamableHttpPostResponse::Sse(stream, session) => Ok(
                StreamableHttpPostResponse::Sse(self.stream(stream).await?, session),
            ),
            response => Ok(response),
        }
    }
    async fn guarded<T>(
        &self,
        future: impl std::future::Future<
            Output = std::result::Result<T, StreamableHttpError<http_client::Error>>,
        >,
    ) -> std::result::Result<T, StreamableHttpError<http_client::Error>> {
        let mut stop = self.lifecycle.stop.subscribe();
        if *stop.borrow() {
            return Err(StreamableHttpError::TransportChannelClosed);
        }
        let _permit = self
            .lifecycle
            .requests
            .acquire()
            .await
            .map_err(|_| StreamableHttpError::TransportChannelClosed)?;
        tokio::select! {
            result = future => result,
            _ = stop.changed() => Err(StreamableHttpError::TransportChannelClosed),
        }
    }
}
impl StreamableHttpClient for LifecycleHttpClient {
    type Error = http_client::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: rmcp::model::ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: std::collections::HashMap<
            reqwest::header::HeaderName,
            reqwest::header::HeaderValue,
        >,
    ) -> std::result::Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let response = self
            .guarded(self.client.post_message(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
            ))
            .await?;
        self.response(response).await
    }
    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: rmcp::model::ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: std::collections::HashMap<
            reqwest::header::HeaderName,
            reqwest::header::HeaderValue,
        >,
        max: usize,
    ) -> std::result::Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let response = self
            .guarded(self.client.post_message_with_max_sse_event_size(
                uri,
                message,
                session_id,
                auth_header,
                custom_headers,
                max,
            ))
            .await?;
        self.response(response).await
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: std::collections::HashMap<
            reqwest::header::HeaderName,
            reqwest::header::HeaderValue,
        >,
    ) -> std::result::Result<(), StreamableHttpError<Self::Error>> {
        self.guarded(
            self.client
                .delete_session(uri, session_id, auth_header, custom_headers),
        )
        .await
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: std::collections::HashMap<
            reqwest::header::HeaderName,
            reqwest::header::HeaderValue,
        >,
    ) -> std::result::Result<
        futures_util::stream::BoxStream<'static, std::result::Result<sse_stream::Sse, SseError>>,
        StreamableHttpError<Self::Error>,
    > {
        let stream = self
            .guarded(self.client.get_stream(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
            ))
            .await?;
        self.stream(stream).await
    }
    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: std::collections::HashMap<
            reqwest::header::HeaderName,
            reqwest::header::HeaderValue,
        >,
        max: usize,
    ) -> std::result::Result<
        futures_util::stream::BoxStream<'static, std::result::Result<sse_stream::Sse, SseError>>,
        StreamableHttpError<Self::Error>,
    > {
        let stream = self
            .guarded(self.client.get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max,
            ))
            .await?;
        self.stream(stream).await
    }
}

struct Connection {
    client: RunningService<RoleClient, ClientConfig>,
    child: AsyncMutex<Option<Child>>,
    remote: Option<Arc<RemoteLifecycle>>,
}
impl Connection {
    fn is_closed(&self) -> bool {
        self.client.is_closed() || self.client.is_transport_closed()
    }
    async fn close(&self) {
        self.client.cancellation_token().cancel();
        if let Some(remote) = &self.remote {
            remote.close().await;
        }
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(remote) = &self.remote {
            remote.stop.send_replace(true);
        }
        if let Some(mut child) = self.child.get_mut().take() {
            let _ = child.start_kill();
        }
    }
}
struct Server {
    config: ServerConfig,
    enabled: AtomicBool,
    connection: AsyncMutex<Option<Arc<Connection>>>,
    pending_child: AsyncMutex<Option<Child>>,
    pending_remote: Mutex<Option<Arc<RemoteLifecycle>>>,
    status: Mutex<String>,
    permits: Semaphore,
}
#[derive(Clone)]
struct Binding {
    server: String,
    name: String,
    kind: u8,
    definition: Value,
}
#[derive(Clone, Default)]
pub struct Hub {
    servers: Arc<BTreeMap<String, Arc<Server>>>,
    bindings: Arc<Mutex<BTreeMap<String, Binding>>>,
    workspace: Arc<PathBuf>,
    preparations: Arc<AsyncMutex<BTreeMap<String, JoinHandle<()>>>>,
}

fn alias(server: &str, tool: &str, kind: u8) -> String {
    let clean = |text: &str| {
        text.chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '_'
                }
            })
            .take(19)
            .collect::<String>()
    };
    let mut hash = 14695981039346656037u64;
    for byte in server.bytes().chain([0, kind, 0]).chain(tool.bytes()) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(1099511628211);
    }
    format!("mcp_{}_{}_{hash:016x}", clean(server), clean(tool))
}
impl Hub {
    pub fn new(config: BTreeMap<String, ServerConfig>, workspace: PathBuf) -> Self {
        Self {
            servers: Arc::new(
                config
                    .into_iter()
                    .map(|(name, config)| {
                        let enabled = config.enabled;
                        (
                            name,
                            Arc::new(Server {
                                config,
                                enabled: AtomicBool::new(enabled),
                                connection: AsyncMutex::new(None),
                                pending_child: AsyncMutex::new(None),
                                pending_remote: Mutex::new(None),
                                status: Mutex::new(
                                    if enabled { "configured" } else { "disabled" }.into(),
                                ),
                                permits: Semaphore::new(8),
                            }),
                        )
                    })
                    .collect(),
            ),
            bindings: Arc::default(),
            workspace: Arc::new(workspace),
            preparations: Arc::default(),
        }
    }
    pub fn statuses(&self) -> Vec<(String, String)> {
        self.servers
            .iter()
            .map(|(name, server)| {
                (
                    name.clone(),
                    server
                        .status
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone(),
                )
            })
            .collect()
    }
    async fn connection(&self, name: &str) -> Result<Arc<Connection>> {
        let server = self.servers.get(name).context("MCP server missing")?;
        ensure!(
            server.enabled.load(Ordering::Relaxed),
            "MCP server is disabled"
        );
        let mut cached = server.connection.lock().await;
        ensure!(
            server.enabled.load(Ordering::Relaxed),
            "MCP server is disabled"
        );
        self.connect_locked(name, server, &mut cached).await
    }
    async fn connect_locked(
        &self,
        name: &str,
        server: &Server,
        cached: &mut Option<Arc<Connection>>,
    ) -> Result<Arc<Connection>> {
        if let Some(connection) = cached.as_ref().filter(|connection| !connection.is_closed()) {
            return Ok(connection.clone());
        }
        if let Some(connection) = cached.take() {
            connection.close().await;
        }
        *server.status.lock().unwrap_or_else(|e| e.into_inner()) = "connecting".into();
        let mut info = ClientConfig::default();
        info.client_info.name = "openraid".into();
        info.client_info.version = env!("CARGO_PKG_VERSION").into();
        info.client_info.title = Some("openraid by vuln.industries".into());
        let result = self.open_connection(name, server, info).await;
        let connection = match result {
            Ok(connection) => connection,
            Err(_) => {
                Self::reap_pending_child(server).await;
                *server.status.lock().unwrap_or_else(|e| e.into_inner()) =
                    "connection failed · check command/URL/auth".into();
                return Err(anyhow::anyhow!("MCP {name} initialization or discovery failed; check command, URL and authentication"));
            }
        };
        *cached = Some(connection.clone());
        Ok(connection)
    }
    async fn open_connection(
        &self,
        name: &str,
        server: &Server,
        info: ClientConfig,
    ) -> Result<Arc<Connection>> {
        let client = if matches!(server.config.kind.as_str(), "local" | "stdio") {
            let program = server
                .config
                .command
                .first()
                .context("MCP command missing")?;
            let mut command = if cfg!(windows)
                && (program.ends_with(".cmd")
                    || program.ends_with(".bat")
                    || matches!(program.as_str(), "npx" | "npm" | "bunx"))
            {
                let mut command = Command::new("cmd.exe");
                command.args(["/d", "/c", program]);
                command
            } else {
                Command::new(program)
            };
            command
                .args(&server.config.command[1..])
                .envs(&server.config.environment)
                .current_dir(self.workspace.as_ref())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let mut process = command
                .spawn()
                .map_err(|_| anyhow::anyhow!("MCP {name} could not start; check its command"))?;
            let stdout = process.stdout.take().context("MCP stdout missing")?;
            let stdin = process.stdin.take().context("MCP stdin missing")?;
            *server.pending_child.lock().await = Some(process);
            info.serve((stdout, stdin)).await
        } else {
            let mut config =
                StreamableHttpClientTransportConfig::with_uri(server.config.url.clone());
            let mut headers = std::collections::HashMap::new();
            for (key, value) in &server.config.headers {
                headers.insert(
                    reqwest::header::HeaderName::from_bytes(key.as_bytes())?,
                    reqwest::header::HeaderValue::from_str(value)?,
                );
            }
            config = config.custom_headers(headers);
            let (stop, _) = watch::channel(false);
            let lifecycle = Arc::new(RemoteLifecycle {
                stop,
                requests: Arc::new(Semaphore::new(u32::MAX as usize)),
            });
            *server
                .pending_remote
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(lifecycle.clone());
            let transport = StreamableHttpClientTransport::with_client(
                LifecycleHttpClient {
                    client: http_client::Client::new(),
                    lifecycle,
                },
                config,
            );
            info.serve(transport).await
        };
        let client = client.map_err(|_| anyhow::anyhow!("MCP initialization failed"))?;
        let capabilities = client
            .peer_info()
            .context("MCP capabilities missing")?
            .capabilities
            .clone();
        let tools = if capabilities.tools.is_some() {
            client
                .list_all_tools()
                .await
                .map_err(|_| anyhow::anyhow!("MCP {name} tool discovery failed"))?
        } else {
            Vec::new()
        };
        let connection = Arc::new(Connection {
            client,
            child: AsyncMutex::new(server.pending_child.lock().await.take()),
            remote: server
                .pending_remote
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take(),
        });
        let mut bindings = self.bindings.lock().unwrap_or_else(|e| e.into_inner());
        bindings.retain(|_, binding| binding.server != name);
        for tool in tools {
            let original = tool.name.to_string();
            let id = alias(name, &original, 0);
            let definition = json!({"type":"function","function":{"name":id,"description":format!("MCP {name}: {}",tool.description.as_deref().unwrap_or(&original)),"parameters":&tool.input_schema}});
            bindings.insert(
                id,
                Binding {
                    server: name.into(),
                    name: original,
                    kind: 0,
                    definition,
                },
            );
        }
        for (kind, original, description, parameters) in [
            (
                1,
                "resources_list",
                "List resources exposed by this MCP server",
                json!({"type":"object","properties":{}}),
            ),
            (
                2,
                "resource_read",
                "Read an MCP resource by URI",
                json!({"type":"object","properties":{"uri":{"type":"string"}},"required":["uri"]}),
            ),
            (
                3,
                "prompts_list",
                "List reusable MCP prompts",
                json!({"type":"object","properties":{}}),
            ),
            (
                4,
                "prompt_get",
                "Get an MCP prompt",
                json!({"type":"object","properties":{"name":{"type":"string"},"arguments":{"type":"object"}},"required":["name"]}),
            ),
        ] {
            if (kind <= 2 && capabilities.resources.is_none())
                || (kind >= 3 && capabilities.prompts.is_none())
            {
                continue;
            }
            let id = alias(name, original, kind);
            let definition = json!({"type":"function","function":{"name":id,"description":format!("MCP {name}: {description}"),"parameters":parameters}});
            bindings.insert(
                id,
                Binding {
                    server: name.into(),
                    name: original.into(),
                    kind,
                    definition,
                },
            );
        }
        let count = bindings
            .values()
            .filter(|binding| binding.server == name)
            .count();
        drop(bindings);
        *server.status.lock().unwrap_or_else(|e| e.into_inner()) = format!("ready · {count} tools");
        Ok(connection)
    }
    async fn reap_pending_child(server: &Server) {
        let remote = server
            .pending_remote
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(remote) = remote {
            remote.close().await;
        }
        if let Some(mut child) = server.pending_child.lock().await.take() {
            let _ = child.kill().await;
        }
    }

    /// Schedule read-only initialization independently of provider workers.
    /// Handles stay owned until explicit disable or final shutdown joins them.
    pub async fn prepare_enabled(&self) {
        let mut preparations = self.preparations.lock().await;
        let finished: Vec<_> = preparations
            .iter()
            .filter(|(_, task)| task.is_finished())
            .map(|(name, _)| name.clone())
            .collect();
        for name in finished {
            if let Some(task) = preparations.remove(&name) {
                let _ = task.await;
            }
        }
        for (name, server) in self.servers.iter() {
            if !server.enabled.load(Ordering::Relaxed) || preparations.contains_key(name) {
                continue;
            }
            let hub = self.clone();
            let target = name.clone();
            preparations.insert(
                name.clone(),
                tokio::spawn(async move {
                    let _ = hub.connection(&target).await;
                }),
            );
        }
    }

    async fn cancel_preparation(
        preparations: &mut BTreeMap<String, JoinHandle<()>>,
        name: &str,
        server: &Server,
    ) -> bool {
        let mut pending = false;
        if let Some(task) = preparations.remove(name) {
            pending = !task.is_finished();
            task.abort();
            let _ = task.await;
        }
        Self::reap_pending_child(server).await;
        pending
    }
    pub async fn connect_enabled(&self) {
        let mut jobs = tokio::task::JoinSet::new();
        for (name, server) in self
            .servers
            .iter()
            .filter(|(_, server)| server.enabled.load(Ordering::Relaxed))
        {
            let _ = server;
            let hub = self.clone();
            let name = name.clone();
            jobs.spawn(async move {
                let _ = hub.connection(&name).await;
            });
        }
        while jobs.join_next().await.is_some() {}
    }
    pub fn definitions(&self) -> Vec<Value> {
        self.bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|binding| {
                self.servers[&binding.server]
                    .enabled
                    .load(Ordering::Relaxed)
            })
            .map(|binding| binding.definition.clone())
            .collect()
    }
    pub fn contains(&self, name: &str) -> bool {
        self.bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(name)
    }
    pub async fn call(&self, name: &str, args: &Value) -> Result<Value> {
        let binding = self
            .bindings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
            .context("MCP tool missing")?;
        let server = &self.servers[&binding.server];
        let _permit = server.permits.acquire().await?;
        let connection = self.connection(&binding.server).await?;
        let result = match binding.kind {
            0 => serde_json::to_value(
                connection
                    .client
                    .call_tool(serde_json::from_value::<CallToolRequestParams>(
                        json!({"name":binding.name,"arguments":args}),
                    )?)
                    .await
                    .map_err(|_| anyhow::anyhow!("MCP {} tool call failed", binding.server))?,
            )?,
            1 => serde_json::to_value(
                connection
                    .client
                    .list_all_resources()
                    .await
                    .map_err(|_| anyhow::anyhow!("MCP resource listing failed"))?,
            )?,
            2 => serde_json::to_value(
                connection
                    .client
                    .read_resource(serde_json::from_value(args.clone())?)
                    .await
                    .map_err(|_| anyhow::anyhow!("MCP resource read failed"))?,
            )?,
            3 => serde_json::to_value(
                connection
                    .client
                    .list_all_prompts()
                    .await
                    .map_err(|_| anyhow::anyhow!("MCP prompt listing failed"))?,
            )?,
            _ => serde_json::to_value(
                connection
                    .client
                    .get_prompt(serde_json::from_value(args.clone())?)
                    .await
                    .map_err(|_| anyhow::anyhow!("MCP prompt read failed"))?,
            )?,
        };
        Ok(result)
    }
    pub async fn toggle(&self, name: &str) -> Result<()> {
        let server = self.servers.get(name).context("MCP server missing")?;
        let mut preparations = self.preparations.lock().await;
        let preparing = Self::cancel_preparation(&mut preparations, name, server).await;
        let mut cached = server.connection.lock().await;
        let connected = cached
            .as_ref()
            .is_some_and(|connection| !connection.is_closed());
        if server.enabled.load(Ordering::Relaxed) && (connected || preparing) {
            server.enabled.store(false, Ordering::Relaxed);
            if let Some(connection) = cached.take() {
                connection.close().await;
            }
            *server.status.lock().unwrap_or_else(|e| e.into_inner()) = "disabled".into();
        } else {
            server.enabled.store(true, Ordering::Relaxed);
            self.connect_locked(name, server, &mut cached).await?;
        }
        Ok(())
    }
    async fn close_server(&self, server: &Server) {
        let mut cached = server.connection.lock().await;
        server.enabled.store(false, Ordering::Relaxed);
        if let Some(connection) = cached.take() {
            connection.close().await;
        }
        *server.status.lock().unwrap_or_else(|e| e.into_inner()) = "disabled".into();
    }
    pub async fn shutdown(&self) {
        let mut preparations = self.preparations.lock().await;
        for (name, server) in self.servers.iter() {
            Self::cancel_preparation(&mut preparations, name, server).await;
            self.close_server(server).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    #[test]
    fn project_overlays_disable_inherited_servers_and_preserve_connection_settings() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let global = directory.path().join("global.json");
        let project = directory.path().join("project.jsonc");
        std::fs::write(&global,json!({"mcp":{
            "local":{"type":"local","command":["node","server.mjs"],"environment":{"KEEP":"yes","CHANGE":"old"}},
            "remote":{"type":"remote","url":"http://localhost/mcp","headers":{"Authorization":"Bearer fixture"}},
            "removed":{"command":["unused"]}
        }}).to_string())?;
        std::fs::write(
            &project,
            r#"{mcp:{local:{environment:{CHANGE:"new"}},remote:{enabled:false},removed:false,placeholder:{enabled:false}}}"#,
        )?;
        let configs = load_paths(directory.path(), &[global, project])?;
        assert_eq!(configs["local"].command, ["node", "server.mjs"]);
        assert_eq!(configs["local"].environment["KEEP"], "yes");
        assert_eq!(configs["local"].environment["CHANGE"], "new");
        assert!(!configs["remote"].enabled);
        assert_eq!(configs["remote"].url, "http://localhost/mcp");
        assert_eq!(configs["remote"].headers["Authorization"], "Bearer fixture");
        assert!(!configs.contains_key("removed"));
        assert!(!configs["placeholder"].enabled);
        Ok(())
    }

    #[tokio::test]
    async fn failed_start_and_invalid_headers_leave_retryable_failed_status() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let hub = Hub::new(
            BTreeMap::from([
                (
                    "missing".into(),
                    ServerConfig::parse(
                        &json!({"command":[directory.path().join("missing-program")]}),
                    )?,
                ),
                (
                    "headers".into(),
                    ServerConfig::parse(
                        &json!({"url":"http://127.0.0.1:1/mcp","headers":{"Authorization":"secret\ninvalid"}}),
                    )?,
                ),
            ]),
            directory.path().into(),
        );
        hub.connect_enabled().await;
        for (_, status) in hub.statuses() {
            assert!(status.starts_with("connection failed"), "{status}");
        }
        assert!(hub.definitions().is_empty());
        let error = hub.toggle("headers").await.unwrap_err().to_string();
        assert!(!error.contains("secret"));
        assert!(!hub
            .statuses()
            .iter()
            .any(|(_, status)| status == "connecting"));
        hub.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn queued_connection_cannot_reenable_a_disabled_server() -> Result<()> {
        let hub = Hub::new(
            BTreeMap::from([(
                "fixture".into(),
                ServerConfig::parse(&json!({"command":["missing-program"]}))?,
            )]),
            std::env::current_dir()?,
        );
        let server = &hub.servers["fixture"];
        let guard = server.connection.lock().await;
        let mut connect = std::pin::pin!(hub.connection("fixture"));
        // Poll once while the mutex is held, guaranteeing that the initial
        // enabled check passed and the connection is now queued behind toggle.
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(connect.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        server.enabled.store(false, Ordering::Relaxed);
        drop(guard);
        let error = connect
            .await
            .err()
            .context("queued connection unexpectedly succeeded")?;
        assert_eq!(error.to_string(), "MCP server is disabled");
        assert_eq!(
            hub.statuses()[0].1,
            "configured",
            "no connection attempt occurred"
        );
        assert!(server.connection.lock().await.is_none());
        Ok(())
    }

    async fn serve_fixture(mut socket: TcpStream, initializations: Arc<AtomicUsize>) -> Result<()> {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        let end = loop {
            let count = socket.read(&mut chunk).await?;
            ensure!(count > 0, "fixture request ended early");
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = std::str::from_utf8(&bytes[..end])?;
        ensure!(
            headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("authorization: Bearer fixture")),
            "fixture authentication missing"
        );
        let length = headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        while bytes.len() < end + length {
            let count = socket.read(&mut chunk).await?;
            ensure!(count > 0, "fixture body ended early");
            bytes.extend_from_slice(&chunk[..count]);
        }
        let message: Value = serde_json::from_slice(&bytes[end..end + length])?;
        if message.get("id").is_none() {
            socket
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await?;
        } else {
            let result = match message["method"].as_str().unwrap_or_default() {
                "initialize" => {
                    initializations.fetch_add(1, Ordering::Relaxed);
                    json!({"protocolVersion":message["params"]["protocolVersion"],"capabilities":{"tools":{},"resources":{},"prompts":{}},"serverInfo":{"name":"fixture","version":"1"}})
                }
                "tools/list" => {
                    json!({"tools":[{"name":"resources_list","description":"real resource-named tool","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]})
                }
                "tools/call" => {
                    assert_eq!(message["params"]["name"], "resources_list");
                    json!({"content":[{"type":"text","text":message["params"]["arguments"]["text"]}]})
                }
                "resources/list" => {
                    json!({"resources":[{"uri":"fixture://resource","name":"fixture-resource"}]})
                }
                "resources/read" => {
                    assert_eq!(message["params"]["uri"], "fixture://resource");
                    json!({"contents":[{"uri":"fixture://resource","text":"resource contents"}]})
                }
                "prompts/list" => {
                    json!({"prompts":[{"name":"fixture-prompt","description":"Fixture prompt","arguments":[{"name":"topic","required":true}]}]})
                }
                "prompts/get" => {
                    assert_eq!(message["params"]["name"], "fixture-prompt");
                    assert_eq!(message["params"]["arguments"]["topic"], "integration");
                    json!({"messages":[{"role":"user","content":{"type":"text","text":"Prompt integration"}}]})
                }
                method => anyhow::bail!("unexpected fixture method {method}"),
            };
            let body = json!({"jsonrpc":"2.0","id":message["id"],"result":result}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;
        }
        socket.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn shared_http_connection_routes_real_and_synthetic_tools_and_reconnects() -> Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let url = format!("http://{}/mcp", listener.local_addr()?);
            let initializations = Arc::new(AtomicUsize::new(0));
            let count = initializations.clone();
            let fixture = tokio::spawn(async move {
                let mut requests = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        accepted=listener.accept()=>{
                            let (socket,_)=accepted?;
                            let count=count.clone();
                            requests.spawn(serve_fixture(socket,count));
                        },
                        result=requests.join_next(),if !requests.is_empty()=>{result.unwrap()??;}
                    }
                }
                #[allow(unreachable_code)]
                Ok::<_, anyhow::Error>(())
            });
            let hub = Hub::new(
                BTreeMap::from([(
                    "fixture".into(),
                    ServerConfig::parse(
                        &json!({"url":url,"headers":{"Authorization":"Bearer fixture"}}),
                    )?,
                )]),
                std::env::current_dir()?,
            );
            let result = async {
                tokio::join!(hub.connect_enabled(), hub.connect_enabled());
                assert_eq!(initializations.load(Ordering::Relaxed), 1);
                assert_eq!(
                    hub.definitions().len(),
                    5,
                    "real resources_list must not be replaced by the synthetic list"
                );
                let real = alias("fixture", "resources_list", 0);
                let synthetic = alias("fixture", "resources_list", 1);
                let first_args = json!({"text":"one"});
                let second_args = json!({"text":"two"});
                let (first, second) =
                    tokio::join!(hub.call(&real, &first_args), hub.call(&real, &second_args));
                assert_eq!(first?["content"][0]["text"], "one");
                assert_eq!(second?["content"][0]["text"], "two");
                assert_eq!(
                    hub.call(&synthetic, &json!({})).await?[0]["uri"],
                    "fixture://resource"
                );
                assert_eq!(
                    hub.call(
                        &alias("fixture", "resource_read", 2),
                        &json!({"uri":"fixture://resource"})
                    )
                    .await?["contents"][0]["text"],
                    "resource contents"
                );
                assert_eq!(
                    hub.call(&alias("fixture", "prompts_list", 3), &json!({}))
                        .await?[0]["name"],
                    "fixture-prompt"
                );
                assert_eq!(
                    hub.call(
                        &alias("fixture", "prompt_get", 4),
                        &json!({"name":"fixture-prompt","arguments":{"topic":"integration"}})
                    )
                    .await?["messages"][0]["content"]["text"],
                    "Prompt integration"
                );
                hub.toggle("fixture").await?;
                assert!(hub.definitions().is_empty());
                assert!(hub.call(&real, &json!({"text":"disabled"})).await.is_err());
                hub.toggle("fixture").await?;
                assert_eq!(initializations.load(Ordering::Relaxed), 2);
                assert_eq!(hub.definitions().len(), 5);
                Ok::<_, anyhow::Error>(())
            }
            .await;
            hub.shutdown().await;
            fixture.abort();
            let _ = fixture.await;
            result
        })
        .await
        .context("MCP HTTP fixture timed out")?
    }

    #[tokio::test]
    #[ignore = "requires Node.js for the local MCP stdio fixture"]
    async fn held_initialize_preparation_is_owned_and_reaped_on_explicit_lifecycle() -> Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let script = r#"
const fs = require('node:fs');
require('node:readline').createInterface({input:process.stdin}).on('line', line => {
  if (JSON.parse(line).method === 'initialize') fs.appendFileSync(process.env.INIT_LOG, `${process.pid}\n`);
});
"#;
            let directory = tempfile::tempdir()?;
            for disable in [true, false] {
                let log = directory.path().join(if disable { "disable.txt" } else { "shutdown.txt" });
                let hub = Hub::new(BTreeMap::from([("held".into(), ServerConfig::parse(&json!({
                    "command":["node","--eval",script],"environment":{"INIT_LOG":log}
                }))?)]), directory.path().to_owned());
                tokio::join!(hub.prepare_enabled(), hub.prepare_enabled());
                while !log.is_file() {
                    tokio::task::yield_now().await;
                }
                assert_eq!(std::fs::read_to_string(&log)?.lines().count(), 1);
                assert_eq!(hub.statuses()[0].1, "connecting");
                assert!(hub.servers["held"].pending_child.lock().await.as_ref().and_then(Child::id).is_some());
                assert_eq!(hub.preparations.lock().await.len(), 1);
                assert!(hub.definitions().is_empty());
                if disable {
                    hub.toggle("held").await?;
                } else {
                    hub.shutdown().await;
                }
                assert_eq!(hub.statuses()[0].1, "disabled");
                assert!(hub.preparations.lock().await.is_empty(), "preparation task was joined");
                assert!(hub.servers["held"].pending_child.lock().await.is_none(), "held child was killed and reaped");
                assert!(hub.servers["held"].connection.lock().await.is_none());
                hub.prepare_enabled().await;
                assert!(hub.preparations.lock().await.is_empty(), "disabled servers do not restart");
            }
            Ok::<_, anyhow::Error>(())
        }).await.context("held MCP initialization fixture timed out")?
    }

    #[tokio::test]
    #[ignore = "requires Node.js for the local MCP stdio fixture"]
    async fn stdio_shares_one_process_and_reaps_it_on_disable_and_shutdown() -> Result<()> {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let script = r#"
const readline = require('node:readline');
const fs = require('node:fs');
readline.createInterface({input:process.stdin}).on('line', line => {
  const message = JSON.parse(line);
  if (message.id === undefined) return;
  let result;
  if (message.method === 'initialize') {
    fs.appendFileSync(process.env.INIT_LOG, `${process.pid}\n`);
    result = {protocolVersion:message.params.protocolVersion,capabilities:{tools:{}},serverInfo:{name:'stdio-fixture',version:'1'}};
  } else if (message.method === 'tools/list') {
    result = {tools:[{name:'echo',description:'Echo fixture',inputSchema:{type:'object'}}]};
  } else if (message.method === 'tools/call') {
    result = {content:[{type:'text',text:message.params.arguments.text}]};
  } else throw Error(`unexpected ${message.method}`);
  process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:message.id,result})+'\n', () => {
    if (message.params?.arguments?.text === 'close transport') process.exit(0);
  });
});
"#;
            let directory = tempfile::tempdir()?;
            let log = directory.path().join("initializations.txt");
            let hub = Hub::new(
                BTreeMap::from([("local".into(),ServerConfig::parse(&json!({"command":["node","--eval",script],"environment":{"INIT_LOG":log}}))?)]),
                directory.path().into(),
            );
            let result = async {
                tokio::join!(hub.connect_enabled(),hub.connect_enabled());
                assert_eq!(std::fs::read_to_string(&log)?.lines().count(),1);
                let connection = hub.connection("local").await?;
                assert!(connection.child.lock().await.as_ref().and_then(Child::id).is_some());
                assert_eq!(hub.call(&alias("local","echo",0),&json!({"text":"stdio works"})).await?["content"][0]["text"],"stdio works");
                hub.toggle("local").await?;
                assert!(connection.is_closed());
                assert!(connection.child.lock().await.is_none(),"disabled subprocess is reaped");
                hub.toggle("local").await?;
                assert_eq!(std::fs::read_to_string(&log)?.lines().count(),2);
                let connection = hub.connection("local").await?;
                assert_eq!(hub.call(&alias("local","echo",0),&json!({"text":"close transport"})).await?["content"][0]["text"],"close transport");
                while !connection.is_closed() {
                    tokio::task::yield_now().await;
                }
                hub.toggle("local").await?;
                assert!(connection.child.lock().await.is_none(),"closed subprocess is reaped before retry");
                assert_eq!(std::fs::read_to_string(&log)?.lines().count(),3);
                assert_eq!(hub.call(&alias("local","echo",0),&json!({"text":"retry works"})).await?["content"][0]["text"],"retry works");
                let connection = hub.connection("local").await?;
                hub.shutdown().await;
                assert!(connection.child.lock().await.is_none(),"shutdown subprocess is reaped");
                Ok::<_,anyhow::Error>(())
            }.await;
            hub.shutdown().await;
            result
        }).await.context("MCP stdio fixture timed out")?
    }
}
