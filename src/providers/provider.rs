//! One pooled HTTP client and one admission semaphore for the entire swarm.
//! Requests have no duration deadlines. Transient failures retry indefinitely.

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::{mpsc, Semaphore};

#[path = "responses.rs"]
mod responses;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    #[default]
    ChatCompletions,
    Responses,
    Anthropic,
    Gemini,
    Sdk,
}

impl std::str::FromStr for Protocol {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "chat" | "chat-completions" | "openai-compatible" => Ok(Self::ChatCompletions),
            "responses" | "openai-responses" => Ok(Self::Responses),
            "anthropic" | "messages" => Ok(Self::Anthropic),
            "gemini" | "google" => Ok(Self::Gemini),
            "sdk" => Ok(Self::Sdk),
            _ => bail!(
                "unknown provider protocol {value}; choose chat, responses, anthropic, gemini, or sdk"
            ),
        }
    }
}

#[derive(Debug)]
pub(super) struct PermanentProviderError;

impl std::fmt::Display for PermanentProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "provider returned an error event; check model, request options, and credentials",
        )
    }
}

impl std::error::Error for PermanentProviderError {}

const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_COMPLETION_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub struct ContextOverflow;

impl std::fmt::Display for ContextOverflow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("provider context window exceeded; compact history before retrying")
    }
}

impl std::error::Error for ContextOverflow {}

pub fn is_context_overflow(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ContextOverflow>().is_some()
}

fn overflow_body(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    let error = &value["error"];
    let code = error["code"]
        .as_str()
        .or_else(|| error["type"].as_str())
        .unwrap_or_default();
    if matches!(
        code,
        "context_length_exceeded"
            | "context_window_exceeded"
            | "prompt_too_long"
            | "too_many_tokens"
    ) {
        return true;
    }
    let message = error["message"]
        .as_str()
        .unwrap_or_default()
        .to_ascii_lowercase();
    message.contains("maximum context length")
        || message.contains("context window")
        || message.contains("prompt is too long")
        || message.contains("exceeds the context")
}

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub max_in_flight: usize,
    pub max_output_tokens: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Keep the wire representation: malformed arguments can be returned as a
    /// tool error to the model instead of silently modifying its request.
    pub arguments: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub finish_reason: String,
    /// Original Responses output includes opaque reasoning state for replay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_items: Vec<Value>,
    /// Signed native thinking blocks / thought signatures survive tool turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_content: Option<Value>,
}

impl Completion {
    pub fn assistant_message(&self) -> Value {
        let mut message = json!({"role": "assistant", "content": self.content});
        if !self.tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(
                self.tool_calls
                    .iter()
                    .map(|call| {
                        json!({"id": call.id, "type": "function", "function": {
                            "name": call.name, "arguments": call.arguments
                        }})
                    })
                    .collect(),
            );
        }
        if !self.response_items.is_empty() {
            message["_openraid_response_items"] = Value::Array(self.response_items.clone());
        }
        if let Some(content) = &self.native_content {
            message["_openraid_native"] = content.clone();
        }
        message
    }
}

#[derive(Clone, Debug)]
pub enum ProviderEvent {
    TextDelta(String),
    Retry {
        attempt: u64,
        reason: String,
        delay_ms: u64,
    },
    Usage(Usage),
    /// A replayed request replaces its preceding partial response.
    Reset {
        attempt: u64,
    },
}

#[derive(Clone)]
pub struct Provider {
    client: reqwest::Client,
    config: Arc<ProviderConfig>,
    endpoint: Arc<str>,
    permits: Arc<Semaphore>,
    protocol: Protocol,
    options: Arc<Value>,
    headers: reqwest::header::HeaderMap,
    oauth: Option<crate::oauth::OAuthSession>,
    provider_id: Arc<str>,
}

#[derive(Serialize)]
struct ChatRequest<'a, M: ?Sized> {
    model: &'a str,
    messages: &'a M,
    max_tokens: usize,
    stream: bool,
    stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

impl Provider {
    /// Model switches retain the one HTTP pool and global admission budget.
    pub fn share_transport(mut self, existing: &Self) -> Self {
        self.client = existing.client.clone();
        self.permits = existing.permits.clone();
        self
    }

    pub fn new(config: ProviderConfig) -> Result<Self> {
        Self::new_with_options(
            config,
            Protocol::ChatCompletions,
            json!({}),
            BTreeMap::new(),
        )
    }

    pub fn new_with_options(
        config: ProviderConfig,
        protocol: Protocol,
        options: Value,
        headers: BTreeMap<String, String>,
    ) -> Result<Self> {
        if !options.is_object() {
            bail!("provider options must be a JSON object");
        }
        if protocol == Protocol::Sdk
            && options
                .get("sdkSettings")
                .is_some_and(|settings| !settings.is_object())
        {
            bail!("SDK factory settings must be a JSON object");
        }
        if config.max_in_flight == 0 || config.max_output_tokens == 0 {
            bail!("provider concurrency and output token budget must be positive");
        }
        if config.model.trim().is_empty() {
            bail!("provider model must not be empty");
        }
        let base = config.base_url.trim();
        let endpoint = match protocol {
            Protocol::ChatCompletions => endpoint_suffix(base, "chat/completions")?,
            Protocol::Responses => endpoint_suffix(base, "responses")?,
            Protocol::Anthropic | Protocol::Gemini => {
                crate::provider_wire::native_endpoint(protocol, base, &config.model)?
            }
            Protocol::Sdk => "http://localhost/sdk-bridge".into(),
        };
        let url = reqwest::Url::parse(&endpoint).context("invalid provider base URL")?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("provider URL must use http or https");
        }
        // reqwest 0.12 has no request/read/connect deadline by default. Keep
        // those defaults; an idle-pool lifetime is not an operation timeout.
        let client = reqwest::Client::builder()
            .user_agent(concat!("openraid/", env!("CARGO_PKG_VERSION")))
            .pool_max_idle_per_host(config.max_in_flight)
            .build()?;
        let permits = Arc::new(Semaphore::new(config.max_in_flight));
        let mut header_map = reqwest::header::HeaderMap::new();
        for (name, value) in headers {
            header_map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .context("invalid custom header name")?,
                reqwest::header::HeaderValue::from_str(&value)
                    .context("invalid custom header value")?,
            );
        }
        Ok(Self {
            client,
            config: Arc::new(config),
            endpoint: endpoint.into(),
            permits,
            protocol,
            options: Arc::new(options),
            headers: header_map,
            oauth: None,
            provider_id: "".into(),
        })
    }

    pub fn with_sdk(mut self, npm: &str, provider_id: &str) -> Self {
        self.provider_id = provider_id.into();
        if self.protocol == Protocol::Sdk {
            let settings = self
                .options
                .get("sdkSettings")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let options = Arc::make_mut(&mut self.options);
            options
                .as_object_mut()
                .expect("validated provider options")
                .remove("sdkSettings");
            options["_openraid_sdk"] =
                json!({"npm":npm,"provider":provider_id,"settings":settings});
        }
        self
    }

    pub fn with_oauth(mut self, session: crate::oauth::OAuthSession) -> Self {
        self.provider_id = session.provider_id().into();
        self.oauth = Some(session);
        self
    }

    pub async fn complete(
        &self,
        messages: &[Value],
        tools: &[Value],
        events: Option<mpsc::Sender<ProviderEvent>>,
    ) -> Result<Completion> {
        self.complete_serializable(messages, tools, events).await
    }

    /// Serialize a borrowed context view without duplicating each queued
    /// agent's model history. `messages` must serialize as a chat-message array.
    pub async fn complete_serializable<M: Serialize + Sync + ?Sized>(
        &self,
        messages: &M,
        tools: &[Value],
        events: Option<mpsc::Sender<ProviderEvent>>,
    ) -> Result<Completion> {
        // Borrow histories while queued. Request materialization happens only
        // after admission, rather than duplicating history for all 500 workers.
        let payload = ChatRequest {
            model: &self.config.model,
            messages,
            max_tokens: self.config.max_output_tokens,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
            tools: (!tools.is_empty()).then_some(tools),
            tool_choice: (!tools.is_empty()).then_some("auto"),
        };
        let mut attempt = 0_u64;
        let mut oauth_refresh_attempted = false;
        loop {
            let permit = self
                .permits
                .acquire()
                .await
                .context("provider admission closed")?;
            let authorization = match &self.oauth {
                Some(session) => Some(session.authorization().await?),
                None => None,
            };
            if self.protocol == Protocol::Sdk {
                let history = serde_json::to_value(messages)?;
                history
                    .as_array()
                    .context("provider history must be a message array")?;
                let mut headers = self
                    .headers
                    .iter()
                    .map(|(name, value)| Ok((name.to_string(), value.to_str()?.to_owned())))
                    .collect::<Result<BTreeMap<_, _>>>()?;
                let mut bridge_config = self.config.as_ref().clone();
                let mut bridge_options = self.options.as_ref().clone();
                if let Some(authorization) = &authorization {
                    bridge_config.api_key = Some(authorization.api_key.clone());
                    headers.retain(|name, _| !is_api_key_header(name));
                    for (name, value) in &authorization.headers {
                        headers.retain(|existing, _| !existing.eq_ignore_ascii_case(name));
                        headers.insert(name.to_ascii_lowercase(), value.clone());
                    }
                    headers.insert(
                        "authorization".into(),
                        format!("Bearer {}", authorization.api_key),
                    );
                    if let Some(settings) = authorization.sdk_settings.as_object() {
                        for (key, value) in settings {
                            bridge_options["_openraid_sdk"]["settings"][key] = value.clone();
                        }
                    }
                }
                let completion = crate::sdk_bridge::complete(
                    &bridge_config,
                    &bridge_options,
                    &headers,
                    &history,
                    tools,
                )
                .await;
                let completion = match completion {
                    Ok(completion) => completion,
                    Err(error) if crate::sdk_bridge::is_retryable(&error) => {
                        drop(history);
                        drop(permit);
                        wait_backoff(
                            &events,
                            &mut attempt,
                            "SDK provider temporarily unavailable".into(),
                            None,
                        )
                        .await;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if !completion.content.is_empty() {
                    emit(
                        &events,
                        ProviderEvent::TextDelta(completion.content.clone()),
                    )
                    .await;
                }
                emit(&events, ProviderEvent::Usage(completion.usage.clone())).await;
                return Ok(completion);
            }
            let mut body = match self.protocol {
                Protocol::ChatCompletions => serde_json::to_value(&payload)?,
                Protocol::Responses | Protocol::Anthropic | Protocol::Gemini => {
                    let history = serde_json::to_value(messages)?;
                    let history = history
                        .as_array()
                        .context("provider history must be a message array")?;
                    if self.protocol == Protocol::Responses {
                        responses::request(
                            &self.config.model,
                            history,
                            tools,
                            self.config.max_output_tokens,
                            &self.options,
                        )?
                    } else {
                        crate::provider_wire::build_request(
                            self.protocol,
                            &self.config.model,
                            history,
                            tools,
                            self.config.max_output_tokens,
                            &self.options,
                        )?
                    }
                }
                Protocol::Sdk => unreachable!("SDK bridge request handled above"),
            };
            if self.protocol == Protocol::ChatCompletions {
                if let Some(messages) = body["messages"].as_array_mut() {
                    for message in messages {
                        let reasoning = message["_openraid_native"]["chat"]["reasoning_content"]
                            .as_str()
                            .map(str::to_owned);
                        if let Some(reasoning) = reasoning {
                            message["reasoning_content"] = Value::String(reasoning);
                        } else if message["role"] == "assistant"
                            && self.config.model.to_ascii_lowercase().contains("deepseek")
                        {
                            message["reasoning_content"] = json!("");
                        }
                        if let Some(details) = message["_openraid_native"]["chat"]
                            ["reasoning_details"]
                            .as_array()
                            .cloned()
                        {
                            message["reasoning_details"] = Value::Array(details);
                        }
                        if let Some(message) = message.as_object_mut() {
                            message.retain(|key, _| !key.starts_with("_openraid_"));
                        }
                    }
                }
                if let Some(options) = self.options.as_object() {
                    for (key, value) in options {
                        if !matches!(key.as_str(), "model" | "messages" | "tools" | "stream") {
                            body[key] = value.clone();
                        }
                    }
                }
                if body.get("max_completion_tokens").is_some() {
                    body.as_object_mut()
                        .expect("provider request object")
                        .remove("max_tokens");
                }
            }
            if let Some(session) = &self.oauth {
                session.prepare_request(&mut body)?;
            }
            if self.provider_id.as_ref() == "snowflake-cortex"
                && self.protocol == Protocol::ChatCompletions
            {
                if let Some(cap) = body
                    .as_object_mut()
                    .expect("provider request object")
                    .remove("max_tokens")
                {
                    body["max_completion_tokens"] = cap;
                }
            }
            let mut final_headers = reqwest::header::HeaderMap::new();
            if self.protocol == Protocol::Anthropic {
                final_headers.insert(
                    "anthropic-version",
                    reqwest::header::HeaderValue::from_static("2023-06-01"),
                );
            }
            if let Some(key) = self
                .config
                .api_key
                .as_ref()
                .filter(|_| authorization.is_none())
            {
                let (name, text) = match self.protocol {
                    Protocol::Anthropic
                        if matches!(
                            self.provider_id.as_ref(),
                            "github-copilot" | "github-copilot-enterprise"
                        ) =>
                    {
                        ("authorization", format!("Bearer {key}"))
                    }
                    Protocol::Anthropic => ("x-api-key", key.clone()),
                    Protocol::Gemini => ("x-goog-api-key", key.clone()),
                    _ => ("authorization", format!("Bearer {key}")),
                };
                let mut value = reqwest::header::HeaderValue::from_str(&text)
                    .context("invalid provider credential header")?;
                value.set_sensitive(true);
                final_headers.insert(name, value);
            }
            final_headers.extend(self.headers.clone());
            if let Some(authorization) = &authorization {
                for (name, value) in &authorization.headers {
                    final_headers.insert(
                        reqwest::header::HeaderName::from_bytes(name.as_bytes())
                            .context("invalid OAuth header name")?,
                        reqwest::header::HeaderValue::from_str(value)
                            .context("invalid OAuth header value")?,
                    );
                }
                let mut bearer = reqwest::header::HeaderValue::from_str(&format!(
                    "Bearer {}",
                    authorization.api_key
                ))
                .context("invalid OAuth bearer token")?;
                bearer.set_sensitive(true);
                final_headers.insert(reqwest::header::AUTHORIZATION, bearer);
                for name in ["api-key", "x-api-key", "x-goog-api-key"] {
                    final_headers.remove(name);
                }
            }
            let request = self
                .client
                .post(self.endpoint.as_ref())
                .headers(final_headers)
                .json(&body);
            // .json() owns serialized bytes; release the intermediate history
            // before HTTP/retry waits so queued agents retain only borrowed views.
            drop(body);
            let response = request.send().await;
            let retry = match response {
                Err(error) if error.is_builder() => return Err(error.into()),
                Err(error) => (
                    format!("transport: {}", safe_transport_reason(&error)),
                    None,
                ),
                Ok(response) if !response.status().is_success() => {
                    let status = response.status();
                    if let Some(session) = self.oauth.as_ref().filter(|session| {
                        status.as_u16() == 401
                            && !oauth_refresh_attempted
                            && session.provider_id() == "snowflake-cortex"
                    }) {
                        oauth_refresh_attempted = true;
                        let rejected_access = authorization
                            .as_ref()
                            .map(|authorization| authorization.api_key.as_str())
                            .unwrap_or_default();
                        session.refresh_after_unauthorized(rejected_access).await?;
                        drop(response);
                        drop(permit);
                        wait_backoff(
                            &events,
                            &mut attempt,
                            "Refreshing Snowflake authorization".into(),
                            Some(Duration::ZERO),
                        )
                        .await;
                        continue;
                    }
                    let delay = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    if status.as_u16() == 429 || status.as_u16() == 408 || status.is_server_error()
                    {
                        (format!("HTTP {status}"), delay)
                    } else {
                        if status.as_u16() == 400 || status.as_u16() == 413 {
                            let mut stream = response.bytes_stream();
                            let mut body = Vec::new();
                            while let Some(chunk) = stream.next().await {
                                let chunk = chunk?;
                                let remaining = 64 * 1024 - body.len();
                                body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                                if body.len() == 64 * 1024 {
                                    break;
                                }
                            }
                            if overflow_body(&body) {
                                return Err(ContextOverflow.into());
                            }
                            if status.as_u16() == 400
                                && self.provider_id.as_ref() == "snowflake-cortex"
                                && snowflake_conversation_complete(&body)
                            {
                                let completion = Completion {
                                    finish_reason: "stop".into(),
                                    ..Completion::default()
                                };
                                emit(&events, ProviderEvent::Usage(completion.usage.clone())).await;
                                return Ok(completion);
                            }
                        }
                        // Configuration/authentication failures require an actionable
                        // error, not a hot retry loop. Runtime retains the agent.
                        bail!("provider returned HTTP {status}; check model, URL, and credentials");
                    }
                }
                Ok(response) => match read_response(response, &events, self.protocol).await {
                    Ok(completion) => {
                        emit(&events, ProviderEvent::Usage(completion.usage.clone())).await;
                        return Ok(completion);
                    }
                    Err(error)
                        if is_context_overflow(&error)
                            || error.downcast_ref::<PermanentProviderError>().is_some() =>
                    {
                        return Err(error)
                    }
                    Err(error) => (format!("response interrupted or invalid: {error}"), None),
                },
            };
            drop(permit);
            wait_backoff(&events, &mut attempt, retry.0, retry.1).await;
        }
    }
}

fn endpoint_suffix(base: &str, suffix: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(base).context("invalid provider base URL")?;
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with(&format!("/{suffix}")) {
        path.to_owned()
    } else {
        format!("{path}/{suffix}")
    };
    url.set_path(&path);
    Ok(url.to_string())
}

fn is_api_key_header(name: &str) -> bool {
    ["api-key", "x-api-key", "x-goog-api-key"]
        .iter()
        .any(|key| name.eq_ignore_ascii_case(key))
}

fn snowflake_conversation_complete(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value["message"]
                .as_str()
                .or_else(|| value["error"].as_str())
                .or_else(|| value["error"]["message"].as_str())
                .map(|message| {
                    message
                        .to_ascii_lowercase()
                        .contains("conversation complete")
                })
        })
        .unwrap_or(false)
}

fn safe_transport_reason(error: &reqwest::Error) -> &'static str {
    // Never put a URL (which may contain credentials) into shared telemetry.
    if error.is_connect() {
        "connection failed"
    } else if error.is_body() {
        "body transport failed"
    } else {
        "request failed"
    }
}

fn retry_delay(attempt: u64) -> Duration {
    let base = 250_u64.saturating_mul(1_u64 << attempt.saturating_sub(1).min(7));
    // Small jitter prevents the shared 500-task client from retrying in lockstep.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    Duration::from_millis(base.min(30_000) + nanos % 251)
}

async fn wait_backoff(
    events: &Option<mpsc::Sender<ProviderEvent>>,
    attempt: &mut u64,
    reason: String,
    delay: Option<Duration>,
) {
    *attempt = attempt.saturating_add(1);
    let delay = delay.unwrap_or_else(|| retry_delay(*attempt));
    emit(
        events,
        ProviderEvent::Retry {
            attempt: *attempt,
            reason,
            delay_ms: delay.as_millis().min(u64::MAX as u128) as u64,
        },
    )
    .await;
    tokio::time::sleep(delay).await;
    emit(events, ProviderEvent::Reset { attempt: *attempt }).await;
}

async fn emit(events: &Option<mpsc::Sender<ProviderEvent>>, event: ProviderEvent) {
    if let Some(sender) = events {
        // Await bounded backpressure. Consumer shutdown never affects the board.
        let _ = sender.send(event).await;
    }
}

async fn read_response(
    response: reqwest::Response,
    events: &Option<mpsc::Sender<ProviderEvent>>,
    protocol: Protocol,
) -> Result<Completion> {
    let is_json = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("application/json"));
    let mut stream = response.bytes_stream();
    if is_json {
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len().saturating_add(chunk.len()) > MAX_COMPLETION_BYTES {
                bail!("provider JSON response exceeds memory limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        let mut acc = ProtocolAccumulator::new(protocol);
        acc.apply(&value, false)?;
        let completion = acc.finish()?;
        if !completion.content.is_empty() {
            emit(events, ProviderEvent::TextDelta(completion.content.clone())).await;
        }
        return Ok(completion);
    }
    let mut decoder = SseDecoder::default();
    let mut accumulator = ProtocolAccumulator::new(protocol);
    while let Some(chunk) = stream.next().await {
        for data in decoder.push(&chunk?)? {
            if data.trim() == "[DONE]" {
                return accumulator.finish();
            }
            let value: Value = serde_json::from_str(&data).context("invalid SSE JSON")?;
            let delta = accumulator.apply(&value, true)?;
            if !delta.is_empty() {
                emit(events, ProviderEvent::TextDelta(delta)).await;
            }
            if accumulator.is_finished() {
                return accumulator.finish();
            }
        }
    }
    for data in decoder.finish()? {
        if data.trim() == "[DONE]" {
            return accumulator.finish();
        }
        let value: Value = serde_json::from_str(&data)?;
        let delta = accumulator.apply(&value, true)?;
        if !delta.is_empty() {
            emit(events, ProviderEvent::TextDelta(delta)).await;
        }
        if accumulator.is_finished() {
            return accumulator.finish();
        }
    }
    if !accumulator.has_completion_marker() {
        bail!("stream ended before a completion marker");
    }
    accumulator.finish()
}

enum ProtocolAccumulator {
    Chat(Accumulator),
    Responses(responses::ResponsesAccumulator),
    Native(crate::provider_wire::NativeAccumulator),
}

impl ProtocolAccumulator {
    fn new(protocol: Protocol) -> Self {
        match protocol {
            Protocol::ChatCompletions => Self::Chat(Accumulator::default()),
            Protocol::Responses => Self::Responses(responses::ResponsesAccumulator::default()),
            Protocol::Anthropic => Self::Native(crate::provider_wire::NativeAccumulator::new(
                crate::provider_wire::NativeProtocol::Anthropic,
            )),
            Protocol::Gemini => Self::Native(crate::provider_wire::NativeAccumulator::new(
                crate::provider_wire::NativeProtocol::Gemini,
            )),
            Protocol::Sdk => unreachable!("SDK bridge responses are normalized directly"),
        }
    }

    fn apply(&mut self, value: &Value, streaming: bool) -> Result<String> {
        match self {
            Self::Chat(acc) => acc.apply(value, streaming),
            Self::Responses(acc) => acc.apply(value, streaming),
            Self::Native(acc) => acc.apply(value, streaming),
        }
    }

    fn is_finished(&self) -> bool {
        match self {
            // Chat usage follows finish_reason, so keep reading until [DONE] / EOF.
            Self::Chat(_) => false,
            Self::Responses(acc) => acc.is_finished(),
            Self::Native(acc) => acc.is_finished(),
        }
    }

    fn has_completion_marker(&self) -> bool {
        match self {
            Self::Chat(acc) => !acc.completion.finish_reason.is_empty(),
            Self::Native(acc) => acc.is_complete(),
            _ => self.is_finished(),
        }
    }

    fn finish(self) -> Result<Completion> {
        match self {
            Self::Chat(acc) => acc.finish(),
            Self::Responses(acc) => acc.finish(),
            Self::Native(acc) => acc.finish(),
        }
    }
}

#[derive(Default)]
struct Accumulator {
    completion: Completion,
    calls: BTreeMap<u64, ToolCall>,
    bytes: usize,
    reasoning: String,
    saw_reasoning: bool,
    reasoning_details: BTreeMap<u64, Value>,
}

impl Accumulator {
    fn apply(&mut self, value: &Value, streaming: bool) -> Result<String> {
        if value.get("error").is_some_and(|error| !error.is_null()) {
            if overflow_body(&serde_json::to_vec(value)?) {
                return Err(ContextOverflow.into());
            }
            return Err(PermanentProviderError.into());
        }
        if !streaming && value["output"].is_array() && !value["choices"].is_array() {
            return Err(PermanentProviderError.into());
        }
        if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
            self.completion.usage = Usage {
                input_tokens: usage["prompt_tokens"]
                    .as_u64()
                    .or_else(|| usage["input_tokens"].as_u64())
                    .unwrap_or(0),
                output_tokens: usage["completion_tokens"]
                    .as_u64()
                    .or_else(|| usage["output_tokens"].as_u64())
                    .unwrap_or(0),
                cached_tokens: usage["prompt_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .or_else(|| usage["input_tokens_details"]["cached_tokens"].as_u64())
                    .unwrap_or(0),
            };
        }
        let Some(choices) = value["choices"].as_array() else {
            return Ok(String::new());
        };
        let Some(choice) = choices
            .iter()
            .find(|choice| choice["index"].as_u64().unwrap_or(0) == 0)
        else {
            return Ok(String::new());
        };
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.completion.finish_reason = reason.to_owned();
        }
        let message = &choice[if streaming { "delta" } else { "message" }];
        if let Some(details) = message["reasoning_details"].as_array() {
            for (position, detail) in details.iter().enumerate() {
                self.bytes = self.bytes.saturating_add(detail.to_string().len());
                let index = detail["index"].as_u64().unwrap_or(position as u64);
                let target = self
                    .reasoning_details
                    .entry(index)
                    .or_insert_with(|| json!({}));
                if let Some(fields) = detail.as_object() {
                    for (key, value) in fields {
                        if streaming
                            && matches!(key.as_str(), "text" | "summary" | "data" | "signature")
                        {
                            if let (Some(old), Some(delta)) = (target[key].as_str(), value.as_str())
                            {
                                target[key] = Value::String(format!("{old}{delta}"));
                                continue;
                            }
                        }
                        if !value.is_null() || target.get(key).is_none() {
                            target[key] = value.clone();
                        }
                    }
                }
            }
        }
        if let Some(reasoning) = message["reasoning_content"]
            .as_str()
            .or_else(|| message["reasoning"].as_str())
        {
            self.reasoning.push_str(reasoning);
            self.bytes = self.bytes.saturating_add(reasoning.len());
            self.saw_reasoning = true;
        }
        let delta = message["content"]
            .as_str()
            .or_else(|| message["refusal"].as_str())
            .unwrap_or_default()
            .to_owned();
        self.bytes = self.bytes.saturating_add(delta.len());
        self.completion.content.push_str(&delta);
        if let Some(calls) = message["tool_calls"].as_array() {
            for (position, fragment) in calls.iter().enumerate() {
                let index = fragment["index"].as_u64().unwrap_or(position as u64);
                let call = self.calls.entry(index).or_default();
                for (target, source) in [
                    (&mut call.id, fragment["id"].as_str()),
                    (&mut call.name, fragment["function"]["name"].as_str()),
                    (
                        &mut call.arguments,
                        fragment["function"]["arguments"].as_str(),
                    ),
                ] {
                    if let Some(text) = source {
                        self.bytes = self.bytes.saturating_add(text.len());
                        target.push_str(text);
                    }
                }
            }
        }
        if self.bytes > MAX_COMPLETION_BYTES {
            bail!("provider completion exceeds memory limit");
        }
        Ok(delta)
    }

    fn finish(mut self) -> Result<Completion> {
        if self.saw_reasoning || !self.reasoning_details.is_empty() {
            let mut chat = json!({});
            if self.saw_reasoning {
                chat["reasoning_content"] = Value::String(self.reasoning);
            }
            if !self.reasoning_details.is_empty() {
                chat["reasoning_details"] =
                    Value::Array(self.reasoning_details.into_values().collect());
            }
            self.completion.native_content = Some(json!({"chat":chat}));
        }
        for call in self.calls.into_values() {
            if call.id.is_empty() || call.name.is_empty() {
                bail!("provider supplied an incomplete tool call");
            }
            self.completion.tool_calls.push(call);
        }
        if self.completion.finish_reason.is_empty()
            && self.completion.content.is_empty()
            && self.completion.tool_calls.is_empty()
        {
            bail!("provider supplied an empty response");
        }
        Ok(self.completion)
    }
}

/// Incremental byte framing handles split UTF-8, CRLF and multiline SSE data.
/// Memory is bounded per event; parsing never clones the whole response history.
#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<String>,
    event_bytes: usize,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut start = 0;
        while let Some(relative) = self.pending[start..].iter().position(|byte| *byte == b'\n') {
            let end = start + relative;
            let line = std::str::from_utf8(&self.pending[start..end])?
                .trim_end_matches('\r')
                .to_owned();
            start = end + 1;
            self.line(&line, &mut events)?;
        }
        self.pending.drain(..start);
        if self.pending.len().saturating_add(self.event_bytes) > MAX_EVENT_BYTES {
            bail!("SSE event exceeds memory limit");
        }
        Ok(events)
    }

    fn line(&mut self, line: &str, events: &mut Vec<String>) -> Result<()> {
        if line.is_empty() {
            if !self.data.is_empty() {
                events.push(self.data.join("\n"));
                self.data.clear();
            }
            self.event_bytes = 0;
        } else if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            self.event_bytes = self.event_bytes.saturating_add(value.len());
            if self.event_bytes > MAX_EVENT_BYTES {
                bail!("SSE event exceeds memory limit");
            }
            self.data.push(value.to_owned());
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<String>> {
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            let line = std::str::from_utf8(&self.pending)?
                .trim_end_matches('\r')
                .to_owned();
            self.line(&line, &mut events)?;
        }
        self.line("", &mut events)?;
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Result<Value> {
        Ok(read_http_request_with_headers(stream).await?.0)
    }

    async fn read_http_request_with_headers(
        stream: &mut tokio::net::TcpStream,
    ) -> Result<(Value, String)> {
        let mut buffer = Vec::new();
        let header_end = loop {
            let mut chunk = [0; 4096];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                bail!("request closed before headers");
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = std::str::from_utf8(&buffer[..header_end])?.to_owned();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .context("missing content length")?;
        while buffer.len() < header_end + length {
            let mut chunk = [0; 4096];
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                bail!("request closed before body");
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        Ok((
            serde_json::from_slice(&buffer[header_end..header_end + length])?,
            headers,
        ))
    }

    async fn http_reply(
        stream: &mut tokio::net::TcpStream,
        status: &str,
        body: &str,
        extra: &str,
    ) -> Result<()> {
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}", body.len());
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn http_retries_rate_limit_server_failure_and_partial_stream_without_duplicate_tools(
    ) -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            for attempt in 0..4 {
                let (mut socket, _) = listener.accept().await?;
                let body = read_http_request(&mut socket).await?;
                assert_eq!(body["model"], "test");
                assert_eq!(body["tools"][0]["function"]["name"], "board_read");
                assert_eq!(body["stream_options"]["include_usage"], true);
                match attempt {
                    0 => http_reply(&mut socket, "429 Too Many Requests", "", "Retry-After: 0\r\n").await?,
                    1 => http_reply(&mut socket, "503 Service Unavailable", "", "Retry-After: 0\r\n").await?,
                    2 => http_reply(&mut socket, "200 OK", "data: {\"choices\":[{\"delta\":{\"content\":\"discard partial\"}}]}\n\n", "").await?,
                    _ => http_reply(&mut socket, "200 OK", concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"final\",\"tool_calls\":[{\"index\":0,\"id\":\"call_one\",\"function\":{\"name\":\"board_read\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":4}}\n\n",
                        "data: [DONE]\n\n"
                    ), "").await?,
                }
            }
            Ok::<_, anyhow::Error>(())
        });
        let provider = Provider::new(ProviderConfig {
            base_url: format!("http://{address}/v1"),
            api_key: None,
            model: "test".into(),
            max_in_flight: 1,
            max_output_tokens: 16,
        })?;
        let (sender, mut events) = mpsc::channel(2);
        let drain = tokio::spawn(async move {
            let mut retries = 0;
            let mut resets = 0;
            while let Some(event) = events.recv().await {
                match event {
                    ProviderEvent::Retry { .. } => retries += 1,
                    ProviderEvent::Reset { .. } => resets += 1,
                    _ => {}
                }
            }
            (retries, resets)
        });
        let completion = provider.complete(&[json!({"role":"user","content":"work"})],
            &[json!({"type":"function","function":{"name":"board_read","parameters":{"type":"object"}}})],
            Some(sender)).await?;
        assert_eq!(completion.content, "final");
        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].id, "call_one");
        assert_eq!(completion.usage.output_tokens, 4);
        assert_eq!(drain.await?, (3, 3));
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn cloned_providers_share_admission_and_surface_permanent_errors() -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let provider = Provider::new(ProviderConfig {
            base_url: format!("http://{address}/v1/chat/completions"),
            api_key: None,
            model: "test".into(),
            max_in_flight: 2,
            max_output_tokens: 16,
        })?;
        let clone = provider.clone();
        assert!(Arc::ptr_eq(&provider.permits, &clone.permits));
        let held = provider.permits.acquire().await?;
        assert_eq!(clone.permits.available_permits(), 1);
        drop(held);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            read_http_request(&mut socket).await?;
            http_reply(&mut socket, "401 Unauthorized", "", "").await
        });
        let error = clone.complete(&[], &[], None).await.unwrap_err();
        assert!(error.to_string().contains("401"));
        assert_eq!(provider.permits.available_permits(), 2);
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn configured_headers_override_defaults_and_oauth_remains_authoritative() -> Result<()> {
        for (protocol, use_oauth) in [
            (Protocol::ChatCompletions, false),
            (Protocol::Anthropic, false),
            (Protocol::Gemini, false),
            (Protocol::ChatCompletions, true),
            (Protocol::Anthropic, true),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let address = listener.local_addr()?;
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await?;
                let (_, raw_headers) = read_http_request_with_headers(&mut socket).await?;
                for name in [
                    "authorization",
                    "x-api-key",
                    "anthropic-version",
                    "x-goog-api-key",
                    "x-github-api-version",
                ] {
                    let count = raw_headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                        .count();
                    assert!(
                        count <= 1,
                        "duplicate {name} header on {protocol:?}: {count}"
                    );
                }
                let headers = raw_headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                    .collect::<BTreeMap<_, _>>();
                match protocol {
                    Protocol::ChatCompletions => assert_eq!(
                        headers["authorization"],
                        if use_oauth {
                            "Bearer dynamic-oauth"
                        } else {
                            "Bearer configured-token"
                        }
                    ),
                    Protocol::Anthropic => {
                        if use_oauth {
                            assert!(!headers.contains_key("x-api-key"));
                        } else {
                            assert_eq!(headers["x-api-key"], "configured-token");
                        }
                        assert_eq!(headers["anthropic-version"], "configured-version");
                    }
                    Protocol::Gemini => assert_eq!(headers["x-goog-api-key"], "configured-token"),
                    _ => unreachable!(),
                }
                if use_oauth {
                    assert_eq!(headers["authorization"], "Bearer dynamic-oauth");
                    assert_eq!(headers["x-github-api-version"], "2026-06-01");
                    for name in ["api-key", "x-api-key", "x-goog-api-key"] {
                        assert!(
                            !headers.contains_key(name),
                            "stale {name} must not accompany OAuth"
                        );
                    }
                }
                let body = match protocol {
                    Protocol::ChatCompletions => json!({"choices":[{"message":{"content":"done"},"finish_reason":"stop"}]}),
                    Protocol::Anthropic => json!({"content":[{"type":"text","text":"done"}],"stop_reason":"end_turn"}),
                    Protocol::Gemini => json!({"candidates":[{"content":{"parts":[{"text":"done"}]},"finishReason":"STOP"}]}),
                    _ => unreachable!(),
                }.to_string();
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).await?;
                socket.shutdown().await?;
                Ok::<_, anyhow::Error>(())
            });
            let headers = if use_oauth {
                let mut headers = BTreeMap::from([
                    ("Authorization".into(), "Bearer configured-token".into()),
                    ("x-github-api-version".into(), "stale-version".into()),
                    ("Api-Key".into(), "stale-key".into()),
                    ("X-Api-Key".into(), "stale-key".into()),
                    ("X-Goog-Api-Key".into(), "stale-key".into()),
                ]);
                if protocol == Protocol::Anthropic {
                    headers.insert("Anthropic-Version".into(), "configured-version".into());
                }
                headers
            } else {
                match protocol {
                    Protocol::Anthropic => BTreeMap::from([
                        ("X-Api-Key".into(), "configured-token".into()),
                        ("Anthropic-Version".into(), "configured-version".into()),
                    ]),
                    Protocol::Gemini => {
                        BTreeMap::from([("X-Goog-Api-Key".into(), "configured-token".into())])
                    }
                    _ => {
                        BTreeMap::from([("Authorization".into(), "Bearer configured-token".into())])
                    }
                }
            };
            let mut provider = Provider::new_with_options(
                ProviderConfig {
                    base_url: format!("http://{address}/v1"),
                    api_key: Some("generated-default".into()),
                    model: "test".into(),
                    max_in_flight: 1,
                    max_output_tokens: 16,
                },
                protocol,
                json!({}),
                headers,
            )?;
            if use_oauth {
                provider = provider.with_oauth(crate::oauth::OAuthSession::from_credential(
                    "github-copilot",
                    json!({"type":"oauth","refresh":"dynamic-oauth"}),
                )?);
            }
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                provider.complete(&[json!({"role":"user","content":"work"})], &[], None),
            )
            .await??;
            assert_eq!(result.content, "done");
            server.await??;
        }
        Ok(())
    }

    #[tokio::test]
    async fn real_concurrent_requests_obey_shared_admission_and_complete_all_queued_work(
    ) -> Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let server_active = active.clone();
        let server_peak = peak.clone();
        let server = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            for _ in 0..16 {
                let (mut socket, _) = listener.accept().await?;
                let active = server_active.clone();
                let peak = server_peak.clone();
                handlers.spawn(async move {
                    read_http_request(&mut socket).await?;
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    // Finish server work before making the completion observable.
                    active.fetch_sub(1, Ordering::SeqCst);
                    http_reply(&mut socket, "200 OK", concat!(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: [DONE]\n\n"
                    ), "").await
                });
            }
            while let Some(result) = handlers.join_next().await {
                result??;
            }
            Ok::<_, anyhow::Error>(())
        });
        let provider = Provider::new(ProviderConfig {
            base_url: format!("http://{address}/v1"),
            api_key: None,
            model: "test".into(),
            max_in_flight: 2,
            max_output_tokens: 16,
        })?;
        let mut requests = tokio::task::JoinSet::new();
        for index in 0..16 {
            let provider = provider.clone();
            requests.spawn(async move {
                provider
                    .complete(
                        &[json!({"role":"user","content":index.to_string()})],
                        &[],
                        None,
                    )
                    .await
            });
        }
        let mut completed = 0;
        while let Some(result) = requests.join_next().await {
            assert_eq!(result??.content, "ok");
            completed += 1;
        }
        server.await??;
        assert_eq!(completed, 16);
        assert!((1..=2).contains(&peak.load(Ordering::SeqCst)));
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(provider.permits.available_permits(), 2);
        Ok(())
    }

    #[test]
    fn sse_handles_arbitrary_utf8_boundaries_crlf_comments_and_multiline() -> Result<()> {
        let wire =
            ": heartbeat\r\ndata: {\"text\":\"ö\",\r\ndata: \"ok\":true}\r\n\r\ndata: [DONE]\n\n";
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for byte in wire.as_bytes() {
            events.extend(decoder.push(&[*byte])?);
        }
        events.extend(decoder.finish()?);
        assert_eq!(events.len(), 2);
        assert_eq!(serde_json::from_str::<Value>(&events[0])?["text"], "ö");
        assert_eq!(events[1], "[DONE]");
        Ok(())
    }

    #[test]
    fn fragmented_interleaved_tool_calls_and_usage_are_reassembled() -> Result<()> {
        let mut acc = Accumulator::default();
        acc.apply(
            &json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":1,"id":"call_b","function":{"name":"board_","arguments":"{\""}},
                {"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{}"}}
            ]}}]}),
            true,
        )?;
        acc.apply(
            &json!({"choices":[{"index":0,"delta":{"tool_calls":[
            {"index":1,"function":{"name":"post","arguments":"body\":\"done\"}"}}
        ]},"finish_reason":"stop"}]}),
            true,
        )?;
        acc.apply(&json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":9}}}), true)?;
        let result = acc.finish()?;
        assert_eq!(result.tool_calls[0].id, "call_a");
        assert_eq!(result.tool_calls[1].name, "board_post");
        assert_eq!(
            serde_json::from_str::<Value>(&result.tool_calls[1].arguments)?["body"],
            "done"
        );
        assert_eq!(
            result.usage,
            Usage {
                input_tokens: 12,
                output_tokens: 5,
                cached_tokens: 9
            }
        );
        assert_eq!(result.assistant_message()["tool_calls"][1]["id"], "call_b");
        Ok(())
    }

    #[test]
    fn decoder_rejects_oversized_events_and_tool_protocol_errors() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(&vec![b'x'; MAX_EVENT_BYTES + 1]).is_err());
        let mut acc = Accumulator::default();
        acc.apply(
            &json!({"choices":[{"delta":{"tool_calls":[{"function":{"name":"exec"}}]}}]}),
            true,
        )
        .unwrap();
        assert!(acc.finish().is_err());
    }

    #[test]
    fn compatible_reasoning_deltas_survive_tool_replay() -> Result<()> {
        let mut acc = Accumulator::default();
        acc.apply(
            &json!({"choices":[{"delta":{"reasoning_content":"Think "}}]}),
            true,
        )?;
        acc.apply(&json!({"choices":[{"delta":{"reasoning_content":"carefully", "tool_calls":[{"index":0,"id":"call_1","function":{"name":"board_read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}), true)?;
        let completion = acc.finish()?;
        assert_eq!(completion.content, "");
        assert_eq!(
            completion.assistant_message()["_openraid_native"]["chat"]["reasoning_content"],
            "Think carefully"
        );
        assert_eq!(completion.tool_calls[0].name, "board_read");
        Ok(())
    }

    #[test]
    fn endpoint_routing_preserves_query_values_and_full_paths() -> Result<()> {
        let endpoint = endpoint_suffix("https://example.test/v1/?route=/", "responses")?;
        let url = reqwest::Url::parse(&endpoint)?;
        assert_eq!(url.path(), "/v1/responses");
        assert_eq!(url.query(), Some("route=/"));
        let endpoint = endpoint_suffix(
            "https://example.test/backend-api/codex/responses/?route=/",
            "responses",
        )?;
        assert_eq!(
            reqwest::Url::parse(&endpoint)?.path(),
            "/backend-api/codex/responses"
        );
        Ok(())
    }

    #[test]
    fn openrouter_signed_and_encrypted_reasoning_details_survive_streaming() -> Result<()> {
        let mut acc = Accumulator::default();
        acc.apply(&json!({"choices":[{"delta":{"reasoning_details":[
            {"index":1,"type":"reasoning.encrypted","id":"enc_1","data":"opaque"},
            {"index":0,"type":"reasoning.text","id":"text_1","format":"anthropic-claude-v1","text":"Think ","signature":null}
        ]}}]}), true)?;
        acc.apply(
            &json!({"choices":[{"delta":{"reasoning_details":[
            {"index":0,"text":"carefully","signature":"signed"}
        ],"content":"done"},"finish_reason":"stop"}]}),
            true,
        )?;
        let message = acc.finish()?.assistant_message();
        let details = &message["_openraid_native"]["chat"]["reasoning_details"];
        assert_eq!(details[0]["text"], "Think carefully");
        assert_eq!(details[0]["signature"], "signed");
        assert_eq!(details[0]["id"], "text_1");
        assert_eq!(details[1]["data"], "opaque");
        assert_eq!(message["content"], "done");
        Ok(())
    }

    #[test]
    fn non_streaming_response_and_provider_validation() -> Result<()> {
        let mut acc = Accumulator::default();
        acc.apply(
            &json!({"choices":[{"message":{"content":"complete","tool_calls":[
            {"id":"one","function":{"name":"board_read","arguments":"{}"}}
        ]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}),
            false,
        )?;
        assert_eq!(acc.finish()?.tool_calls[0].name, "board_read");
        assert!(Provider::new(ProviderConfig {
            base_url: "file:///tmp".into(),
            api_key: None,
            model: "test".into(),
            max_in_flight: 1,
            max_output_tokens: 16
        })
        .is_err());
        Ok(())
    }

    #[test]
    fn overflow_is_distinct_from_configuration_errors_and_survives_context() {
        assert!(overflow_body(
            br#"{"error":{"code":"context_length_exceeded","message":"hidden"}}"#
        ));
        assert!(overflow_body(
            br#"{"error":{"message":"This model maximum context length is 8192 tokens"}}"#
        ));
        assert!(!overflow_body(br#"{"error":{"code":"invalid_api_key"}}"#));
        let error = anyhow::Error::new(ContextOverflow).context("model request");
        assert!(is_context_overflow(&error));
        assert!(!is_context_overflow(&anyhow::anyhow!("invalid model")));
    }
}
