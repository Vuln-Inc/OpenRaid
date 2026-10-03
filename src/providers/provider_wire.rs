//! Native Messages and GenerateContent adapters. Internal history remains in
//! chat-message form; signed native blocks are retained for subsequent turns.

use crate::provider::{
    Completion, ContextOverflow, PermanentProviderError, Protocol, ToolCall, Usage,
};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_COMPLETION_BYTES: usize = 32 * 1024 * 1024;
static NEXT_TOOL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeProtocol {
    Anthropic,
    Gemini,
}

impl From<Protocol> for NativeProtocol {
    fn from(protocol: Protocol) -> Self {
        native_protocol(protocol).expect("native adapter requires Anthropic or Gemini protocol")
    }
}

fn native_protocol(protocol: crate::provider::Protocol) -> Result<NativeProtocol> {
    match protocol {
        crate::provider::Protocol::Anthropic => Ok(NativeProtocol::Anthropic),
        crate::provider::Protocol::Gemini => Ok(NativeProtocol::Gemini),
        _ => bail!("protocol is not a native Messages or GenerateContent adapter"),
    }
}

pub fn build_request(
    protocol: crate::provider::Protocol,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    max_output_tokens: usize,
    options: &Value,
) -> Result<Value> {
    request_payload(
        native_protocol(protocol)?,
        &Value::Array(messages.to_vec()),
        tools,
        model,
        max_output_tokens,
        options,
    )
}

pub fn native_endpoint(
    protocol: crate::provider::Protocol,
    base_url: &str,
    model: &str,
) -> Result<String> {
    let mut url = reqwest::Url::parse(base_url).context("invalid native provider base URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("provider URL must use http or https");
    }
    match native_protocol(protocol)? {
        NativeProtocol::Anthropic => {
            if !url.path().trim_end_matches('/').ends_with("/messages") {
                let path = url.path().trim_end_matches('/');
                url.set_path(&format!("{path}/messages"));
            }
        }
        NativeProtocol::Gemini => {
            if !url.path().contains(":streamGenerateContent") {
                let path = url.path().trim_end_matches('/').to_owned();
                let model = model.strip_prefix("models/").unwrap_or(model);
                let prefix = if path.ends_with("/models") {
                    path
                } else {
                    format!("{path}/models")
                };
                url.set_path(&format!("{prefix}/{model}:streamGenerateContent"));
            }
            url.query_pairs_mut().append_pair("alt", "sse");
        }
    }
    Ok(url.to_string())
}

pub fn request_payload(
    protocol: NativeProtocol,
    messages: &Value,
    tools: &[Value],
    model: &str,
    max_tokens: usize,
    options: &Value,
) -> Result<Value> {
    let messages = messages
        .as_array()
        .context("provider history must be an array")?;
    let mut payload = match protocol {
        NativeProtocol::Anthropic => anthropic_request(messages, tools, model, max_tokens)?,
        NativeProtocol::Gemini => gemini_request(messages, tools, max_tokens)?,
    };
    // Variant options are already translated from SDK options by variants.rs.
    if let Some(options) = options.as_object() {
        for (key, value) in options {
            if protocol == NativeProtocol::Gemini && key != "generationConfig" {
                payload["generationConfig"][key] = value.clone();
            } else if key == "generationConfig" && value.is_object() {
                for (key, value) in value.as_object().unwrap() {
                    payload["generationConfig"][key] = value.clone();
                }
            } else {
                payload[key] = value.clone();
            }
        }
    }
    if protocol == NativeProtocol::Anthropic {
        if let Some(budget) = payload["thinking"]["budget_tokens"].as_u64() {
            if budget >= max_tokens as u64 {
                // The configured output limit includes thinking tokens.
                // Reject impossible budgets rather than silently changing effort.
                bail!("thinking budget ({budget}) must be smaller than maximum output tokens ({max_tokens})");
            }
        }
    }
    Ok(payload)
}

fn text_content(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    value
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn append_turn(turns: &mut Vec<Value>, role: &str, key: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = turns.last_mut().filter(|last| last["role"] == role) {
        last[key].as_array_mut().unwrap().extend(parts);
    } else {
        let mut turn = json!({"role": role});
        turn[key] = Value::Array(parts);
        turns.push(turn);
    }
}

fn arguments(call: &Value) -> Result<Value> {
    match call["function"]["arguments"].as_str() {
        Some(raw) => {
            serde_json::from_str(raw).context("invalid tool arguments in provider history")
        }
        None => Ok(json!({})),
    }
}

fn anthropic_request(
    messages: &[Value],
    tools: &[Value],
    model: &str,
    max_tokens: usize,
) -> Result<Value> {
    let mut system = Vec::new();
    let mut turns = Vec::new();
    for message in messages {
        let role = message["role"].as_str().unwrap_or("user");
        let text = text_content(&message["content"]);
        if matches!(role, "system" | "developer") {
            if !text.is_empty() {
                system.push(json!({"type":"text", "text":text}));
            }
            continue;
        }
        if role == "tool" {
            append_turn(
                &mut turns,
                "user",
                "content",
                vec![
                    json!({"type":"tool_result", "tool_use_id":message["tool_call_id"], "content":text}),
                ],
            );
            continue;
        }
        let mut blocks = Vec::new();
        if role == "assistant" {
            if let Some(native) = message["_openraid_native"]["anthropic"].as_array() {
                append_turn(&mut turns, role, "content", native.clone());
                continue;
            }
        }
        if !text.is_empty() {
            blocks.push(json!({"type":"text", "text":text}));
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                blocks.push(json!({"type":"tool_use", "id":call["id"],
                    "name":call["function"]["name"], "input":arguments(call)?}));
            }
        }
        append_turn(
            &mut turns,
            if role == "assistant" {
                "assistant"
            } else {
                "user"
            },
            "content",
            blocks,
        );
    }
    let mut payload =
        json!({"model":model, "max_tokens":max_tokens, "stream":true, "messages":turns});
    if !system.is_empty() {
        payload["system"] = Value::Array(system);
    }
    if !tools.is_empty() {
        payload["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    let function = &tool["function"];
                    let mut value =
                        json!({"name":function["name"], "input_schema":function["parameters"]});
                    if let Some(description) = function.get("description") {
                        value["description"] = description.clone();
                    }
                    value
                })
                .collect(),
        );
    }
    Ok(payload)
}

fn gemini_request(messages: &[Value], tools: &[Value], max_tokens: usize) -> Result<Value> {
    let mut system = Vec::new();
    let mut turns = Vec::new();
    let mut call_names = BTreeMap::new();
    let mut native_call_ids: BTreeMap<String, String> = BTreeMap::new();
    for message in messages {
        let role = message["role"].as_str().unwrap_or("user");
        let text = text_content(&message["content"]);
        if matches!(role, "system" | "developer") {
            if !text.is_empty() {
                system.push(json!({"text":text}));
            }
            continue;
        }
        if role == "tool" {
            let id = message["tool_call_id"].as_str().unwrap_or_default();
            let name = call_names
                .get(id)
                .context("tool result has no matching Gemini function call")?;
            let response =
                serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({"result":text}));
            let response = if response.is_object() {
                response
            } else {
                json!({"result":response})
            };
            let mut function = json!({"name":name, "response":response});
            if let Some(native_id) = native_call_ids.get(id) {
                function["id"] = Value::String(native_id.clone());
            }
            append_turn(
                &mut turns,
                "user",
                "parts",
                vec![json!({"functionResponse":function})],
            );
            continue;
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                if let (Some(id), Some(name)) =
                    (call["id"].as_str(), call["function"]["name"].as_str())
                {
                    call_names.insert(id.to_owned(), name.to_owned());
                }
            }
        }
        if role == "assistant" {
            if let Some(native) = message["_openraid_native"]["gemini"].as_array() {
                // Correlate internally generated call ids with actual wire ids.
                let wire_calls = native
                    .iter()
                    .filter_map(|part| part.get("functionCall"))
                    .collect::<Vec<_>>();
                if let Some(calls) = message["tool_calls"].as_array() {
                    for (call, wire) in calls.iter().zip(wire_calls) {
                        if let (Some(id), Some(native_id)) =
                            (call["id"].as_str(), wire["id"].as_str())
                        {
                            native_call_ids.insert(id.to_owned(), native_id.to_owned());
                        }
                    }
                }
                append_turn(&mut turns, "model", "parts", native.clone());
                continue;
            }
        }
        let mut parts = Vec::new();
        if !text.is_empty() {
            parts.push(json!({"text":text}));
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                parts.push(json!({"functionCall":{"name":call["function"]["name"], "args":arguments(call)?}}));
            }
        }
        append_turn(
            &mut turns,
            if role == "assistant" { "model" } else { "user" },
            "parts",
            parts,
        );
    }
    let mut payload = json!({"contents":turns, "generationConfig":{"maxOutputTokens":max_tokens}});
    if !system.is_empty() {
        payload["systemInstruction"] = json!({"parts":system});
    }
    if !tools.is_empty() {
        payload["tools"] = json!([{"functionDeclarations":tools.iter().map(|tool| {
            let function = &tool["function"];
            let mut value = json!({"name":function["name"], "parametersJsonSchema":function["parameters"]});
            if let Some(description) = function.get("description") {
                value["description"] = description.clone();
            }
            value
        }).collect::<Vec<_>>()}]);
    }
    Ok(payload)
}

pub struct NativeAccumulator {
    protocol: NativeProtocol,
    completion: Completion,
    blocks: BTreeMap<u64, Value>,
    arguments: BTreeMap<u64, String>,
    uncached_input_tokens: u64,
    cache_creation_tokens: u64,
    complete: bool,
    bytes: usize,
}

impl NativeAccumulator {
    pub fn new(protocol: impl Into<NativeProtocol>) -> Self {
        let protocol = protocol.into();
        Self {
            protocol,
            completion: Completion::default(),
            blocks: BTreeMap::new(),
            arguments: BTreeMap::new(),
            uncached_input_tokens: 0,
            cache_creation_tokens: 0,
            complete: false,
            bytes: 0,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn is_finished(&self) -> bool {
        // GenerateContent can report usage in a trailing, candidate-free
        // frame after finishReason. Messages has an explicit message_stop.
        self.protocol == NativeProtocol::Anthropic && self.complete
    }

    pub fn apply(&mut self, value: &Value, streaming: bool) -> Result<String> {
        if value.get("error").is_some() || value["type"] == "error" {
            let message = value["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if message.contains("context window")
                || message.contains("prompt is too long")
                || message.contains("maximum context")
                || message.contains("too many tokens")
            {
                return Err(ContextOverflow.into());
            }
            if matches!(
                value["error"]["type"].as_str(),
                Some("overloaded_error" | "api_error" | "rate_limit_error")
            ) || matches!(value["error"]["code"].as_u64(), Some(408 | 429 | 500..=599))
            {
                bail!("native provider temporarily unavailable; retry request");
            }
            return Err(PermanentProviderError.into());
        }
        let incompatible = match self.protocol {
            NativeProtocol::Anthropic => {
                value.get("choices").is_some()
                    || value.get("candidates").is_some()
                    || value["type"]
                        .as_str()
                        .is_some_and(|kind| kind.starts_with("response."))
                    || (!streaming
                        && (!value["content"].is_array() || !value["stop_reason"].is_string()))
            }
            NativeProtocol::Gemini => {
                value.get("choices").is_some()
                    || value["type"].as_str().is_some_and(|kind| {
                        kind.starts_with("response.")
                            || kind.starts_with("message_")
                            || kind.starts_with("content_block_")
                    })
                    || (!streaming
                        && !value["candidates"].is_array()
                        && !value["promptFeedback"].is_object())
            }
        };
        if incompatible {
            return Err(PermanentProviderError.into());
        }
        self.bytes = self.bytes.saturating_add(serde_json::to_vec(value)?.len());
        if self.bytes > MAX_COMPLETION_BYTES {
            bail!("provider completion exceeds memory limit");
        }
        match self.protocol {
            NativeProtocol::Anthropic => self.anthropic(value, streaming),
            NativeProtocol::Gemini => self.gemini(value),
        }
    }

    fn anthropic_usage(&mut self, usage: &Value) {
        if let Some(tokens) = usage["input_tokens"].as_u64() {
            self.uncached_input_tokens = tokens;
        }
        if let Some(tokens) = usage["output_tokens"].as_u64() {
            self.completion.usage.output_tokens = tokens;
        }
        if let Some(tokens) = usage["cache_read_input_tokens"].as_u64() {
            self.completion.usage.cached_tokens = tokens;
        }
        if let Some(tokens) = usage["cache_creation_input_tokens"].as_u64() {
            self.cache_creation_tokens = tokens;
        }
        self.completion.usage.input_tokens = self
            .uncached_input_tokens
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.completion.usage.cached_tokens);
    }

    fn anthropic(&mut self, value: &Value, streaming: bool) -> Result<String> {
        if !streaming {
            self.anthropic_usage(&value["usage"]);
            self.completion.finish_reason =
                value["stop_reason"].as_str().unwrap_or_default().into();
            let mut text = String::new();
            if let Some(blocks) = value["content"].as_array() {
                for (index, block) in blocks.iter().enumerate() {
                    self.blocks.insert(index as u64, block.clone());
                    if block["type"] == "text" {
                        text.push_str(block["text"].as_str().unwrap_or_default());
                    }
                }
            }
            self.completion.content.push_str(&text);
            self.complete = !self.completion.finish_reason.is_empty();
            return Ok(text);
        }
        let index = value["index"].as_u64().unwrap_or(0);
        match value["type"].as_str().unwrap_or_default() {
            "message_start" => self.anthropic_usage(&value["message"]["usage"]),
            "content_block_start" => {
                self.blocks.insert(index, value["content_block"].clone());
                if value["content_block"]["type"] == "text" {
                    let text = value["content_block"]["text"].as_str().unwrap_or_default();
                    self.completion.content.push_str(text);
                    return Ok(text.to_owned());
                }
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        let block = self
                            .blocks
                            .entry(index)
                            .or_insert_with(|| json!({"type":"text", "text":""}));
                        append_string(&mut block["text"], text);
                        self.completion.content.push_str(text);
                        return Ok(text.to_owned());
                    }
                    "input_json_delta" => {
                        self.arguments
                            .entry(index)
                            .or_default()
                            .push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    "thinking_delta" | "signature_delta" => {
                        let key = if delta["type"] == "thinking_delta" {
                            "thinking"
                        } else {
                            "signature"
                        };
                        let block = self
                            .blocks
                            .entry(index)
                            .or_insert_with(|| json!({"type":"thinking", "thinking":""}));
                        append_string(&mut block[key], delta[key].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                self.anthropic_usage(&value["usage"]);
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    self.completion.finish_reason = reason.to_owned();
                }
            }
            "message_stop" => self.complete = true,
            _ => {}
        }
        Ok(String::new())
    }

    fn gemini(&mut self, value: &Value) -> Result<String> {
        if let Some(usage) = value.get("usageMetadata") {
            self.completion.usage = Usage {
                input_tokens: usage["promptTokenCount"].as_u64().unwrap_or(0),
                output_tokens: usage["candidatesTokenCount"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_add(usage["thoughtsTokenCount"].as_u64().unwrap_or(0)),
                cached_tokens: usage["cachedContentTokenCount"].as_u64().unwrap_or(0),
            };
        }
        if value["promptFeedback"]["blockReason"].is_string() {
            return Err(PermanentProviderError.into());
        }
        let Some(candidate) = value["candidates"].as_array().and_then(|candidates| {
            candidates
                .iter()
                .find(|candidate| candidate["index"].as_u64().unwrap_or(0) == 0)
        }) else {
            return Ok(String::new());
        };
        if let Some(reason) = candidate["finishReason"].as_str() {
            if matches!(
                reason,
                "SAFETY"
                    | "RECITATION"
                    | "BLOCKLIST"
                    | "PROHIBITED_CONTENT"
                    | "SPII"
                    | "MALFORMED_FUNCTION_CALL"
                    | "UNEXPECTED_TOOL_CALL"
            ) {
                return Err(PermanentProviderError.into());
            }
            self.completion.finish_reason = reason.to_owned();
            self.complete = true;
        }
        let mut text = String::new();
        if let Some(parts) = candidate["content"]["parts"].as_array() {
            for part in parts {
                if part["thought"] != true {
                    text.push_str(part["text"].as_str().unwrap_or_default());
                }
                self.blocks.insert(self.blocks.len() as u64, part.clone());
            }
        }
        self.completion.content.push_str(&text);
        Ok(text)
    }

    pub fn finish(mut self) -> Result<Completion> {
        if !self.complete {
            bail!("native stream ended before a completion marker");
        }
        let mut native = Vec::new();
        for (index, mut block) in self.blocks {
            match self.protocol {
                NativeProtocol::Anthropic if block["type"] == "tool_use" => {
                    if let Some(raw) = self.arguments.get(&index) {
                        block["input"] =
                            serde_json::from_str(raw).context("invalid streamed tool arguments")?;
                    }
                    self.completion.tool_calls.push(ToolCall {
                        id: block["id"].as_str().unwrap_or_default().into(),
                        name: block["name"].as_str().unwrap_or_default().into(),
                        arguments: serde_json::to_string(&block["input"])?,
                    });
                }
                NativeProtocol::Gemini if block["functionCall"].is_object() => {
                    let call = &block["functionCall"];
                    self.completion.tool_calls.push(ToolCall {
                        id: call["id"].as_str().map(str::to_owned).unwrap_or_else(|| {
                            format!(
                                "gemini_call_{}",
                                NEXT_TOOL_ID.fetch_add(1, Ordering::Relaxed)
                            )
                        }),
                        name: call["name"].as_str().unwrap_or_default().into(),
                        arguments: serde_json::to_string(call.get("args").unwrap_or(&json!({})))?,
                    });
                }
                _ => {}
            }
            native.push(block);
        }
        if self
            .completion
            .tool_calls
            .iter()
            .any(|call| call.id.is_empty() || call.name.is_empty())
        {
            bail!("native provider supplied an incomplete tool call");
        }
        let key = match self.protocol {
            NativeProtocol::Anthropic => "anthropic",
            NativeProtocol::Gemini => "gemini",
        };
        let mut metadata = json!({});
        metadata[key] = Value::Array(native);
        self.completion.native_content = Some(metadata);
        Ok(self.completion)
    }
}

fn append_string(value: &mut Value, delta: &str) {
    if let Value::String(text) = value {
        text.push_str(delta);
    } else {
        *value = Value::String(delta.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_parallel_results_share_a_user_turn() -> Result<()> {
        let history = json!([
            {"role":"system", "content":"Be useful"},
            {"role":"user", "content":"Inspect both"},
            {"role":"assistant", "content":"", "tool_calls":[
                {"id":"a", "function":{"name":"read", "arguments":"{\"path\":\"a\"}"}},
                {"id":"b", "function":{"name":"read", "arguments":"{\"path\":\"b\"}"}}
            ]},
            {"role":"tool", "tool_call_id":"a", "content":"one"},
            {"role":"tool", "tool_call_id":"b", "content":"two"}
        ]);
        let payload = request_payload(
            NativeProtocol::Anthropic,
            &history,
            &[],
            "claude",
            4096,
            &json!({}),
        )?;
        assert_eq!(payload["messages"].as_array().unwrap().len(), 3);
        assert_eq!(
            payload["messages"][2]["content"].as_array().unwrap().len(),
            2
        );
        assert_eq!(payload["messages"][2]["content"][1]["tool_use_id"], "b");
        assert_eq!(payload["system"][0]["text"], "Be useful");
        Ok(())
    }

    #[test]
    fn thinking_budget_cannot_exhaust_output_limit() {
        let result = request_payload(
            NativeProtocol::Anthropic,
            &json!([]),
            &[],
            "claude",
            4096,
            &json!({"thinking":{"type":"enabled", "budget_tokens":4096}}),
        );
        assert!(result.unwrap_err().to_string().contains("must be smaller"));
    }

    #[test]
    fn incremental_thinking_and_tool_arguments_survive_history_replay() -> Result<()> {
        let mut accumulator = NativeAccumulator::new(Protocol::Anthropic);
        for event in [
            json!({"type":"message_start", "message":{"usage":{"input_tokens":20, "cache_read_input_tokens":4, "cache_creation_input_tokens":3}}}),
            json!({"type":"content_block_start", "index":0, "content_block":{"type":"thinking", "thinking":""}}),
            json!({"type":"content_block_delta", "index":0, "delta":{"type":"thinking_delta", "thinking":"Check "}}),
            json!({"type":"content_block_delta", "index":0, "delta":{"type":"thinking_delta", "thinking":"files"}}),
            json!({"type":"content_block_delta", "index":0, "delta":{"type":"signature_delta", "signature":"opaque"}}),
            json!({"type":"content_block_start", "index":1, "content_block":{"type":"tool_use", "id":"call", "name":"read", "input":{}}}),
            json!({"type":"content_block_delta", "index":1, "delta":{"type":"input_json_delta", "partial_json":"{\"path\":"}}),
            json!({"type":"content_block_delta", "index":1, "delta":{"type":"input_json_delta", "partial_json":"\"α.rs\"}"}}),
            json!({"type":"message_delta", "delta":{"stop_reason":"tool_use"}, "usage":{"output_tokens":8}}),
            json!({"type":"message_stop"}),
        ] {
            assert!(accumulator.apply(&event, true)?.is_empty());
        }
        let completion = accumulator.finish()?;
        assert_eq!(completion.usage.input_tokens, 27);
        assert_eq!(completion.usage.cached_tokens, 4);
        assert_eq!(completion.usage.output_tokens, 8);
        assert_eq!(completion.tool_calls[0].arguments, "{\"path\":\"α.rs\"}");
        let message = completion.assistant_message();
        let payload = build_request(
            Protocol::Anthropic,
            "claude",
            &[message],
            &[],
            4096,
            &json!({}),
        )?;
        assert_eq!(
            payload["messages"][0]["content"][0]["thinking"],
            "Check files"
        );
        assert_eq!(payload["messages"][0]["content"][0]["signature"], "opaque");
        assert_eq!(
            payload["messages"][0]["content"][1]["input"]["path"],
            "α.rs"
        );
        Ok(())
    }

    #[test]
    fn error_events_are_permanent_and_truncated_streams_are_retryable() -> Result<()> {
        let mut accumulator = NativeAccumulator::new(Protocol::Gemini);
        let error = accumulator
            .apply(&json!({"error":{"code":403, "message":"denied"}}), true)
            .unwrap_err();
        assert!(error.downcast_ref::<PermanentProviderError>().is_some());
        let transient = accumulator
            .apply(&json!({"error":{"code":503, "message":"busy"}}), true)
            .unwrap_err();
        assert!(transient.downcast_ref::<PermanentProviderError>().is_none());
        accumulator.apply(
            &json!({"candidates":[{"content":{"parts":[{"text":"partial"}]}}]}),
            true,
        )?;
        assert!(accumulator
            .finish()
            .unwrap_err()
            .downcast_ref::<PermanentProviderError>()
            .is_none());
        Ok(())
    }

    #[test]
    fn native_endpoints_preserve_custom_query_and_model_prefix() -> Result<()> {
        let endpoint = native_endpoint(
            Protocol::Gemini,
            "http://localhost:8000/v1beta?custom=1",
            "models/gemini-test",
        )?;
        assert_eq!(endpoint, "http://localhost:8000/v1beta/models/gemini-test:streamGenerateContent?custom=1&alt=sse");
        assert_eq!(
            native_endpoint(
                Protocol::Anthropic,
                "https://example.com/v1/messages",
                "unused"
            )?,
            "https://example.com/v1/messages"
        );
        Ok(())
    }

    #[test]
    fn gemini_parallel_calls_preserve_wire_ids_and_hidden_thinking() -> Result<()> {
        let mut accumulator = NativeAccumulator::new(Protocol::Gemini);
        let text = accumulator.apply(&json!({"candidates":[{"content":{"parts":[
            {"thought":true,"text":"Private reasoning", "thoughtSignature":"thought"},
            {"functionCall":{"id":"wire-a", "name":"read", "args":{"path":"a"}}, "thoughtSignature":"signed-a"},
            {"functionCall":{"id":"wire-b", "name":"read", "args":{"path":"b"}}}
        ]}, "finishReason":"STOP"}], "usageMetadata":{"promptTokenCount":20,"candidatesTokenCount":3,"thoughtsTokenCount":5}}), true)?;
        assert!(text.is_empty());
        let completion = accumulator.finish()?;
        assert_eq!(completion.usage.output_tokens, 8);
        let history = vec![
            completion.assistant_message(),
            json!({"role":"tool", "tool_call_id":"wire-a", "content":"first"}),
            json!({"role":"tool", "tool_call_id":"wire-b", "content":"second"}),
        ];
        let request = build_request(Protocol::Gemini, "gemini", &history, &[], 4096, &json!({}))?;
        assert_eq!(
            request["contents"][0]["parts"][0]["thoughtSignature"],
            "thought"
        );
        assert_eq!(
            request["contents"][1]["parts"][0]["functionResponse"]["id"],
            "wire-a"
        );
        assert_eq!(
            request["contents"][1]["parts"][1]["functionResponse"]["id"],
            "wire-b"
        );
        Ok(())
    }

    #[test]
    fn gemini_finish_marker_allows_trailing_usage() -> Result<()> {
        let mut accumulator = NativeAccumulator::new(Protocol::Gemini);
        accumulator.apply(
            &json!({"candidates":[{"content":{"parts":[{"text":"done"}]},"finishReason":"STOP"}]}),
            true,
        )?;
        assert!(accumulator.is_complete());
        assert!(!accumulator.is_finished());
        accumulator.apply(
            &json!({"usageMetadata":{"promptTokenCount":12,"candidatesTokenCount":4}}),
            true,
        )?;
        let completion = accumulator.finish()?;
        assert_eq!(completion.usage.input_tokens, 12);
        assert_eq!(completion.usage.output_tokens, 4);
        Ok(())
    }

    #[test]
    fn wrong_protocol_json_and_stream_envelopes_do_not_retry_forever() {
        for protocol in [Protocol::Anthropic, Protocol::Gemini] {
            for streaming in [false, true] {
                let error = NativeAccumulator::new(protocol).apply(&json!({"choices":[{"message":{"content":"wrong API"},"finish_reason":"stop"}]}), streaming).unwrap_err();
                assert!(error.downcast_ref::<PermanentProviderError>().is_some());
            }
        }
        let error = NativeAccumulator::new(Protocol::Gemini)
            .apply(
                &json!({"type":"message_start","message":{"content":[]}}),
                true,
            )
            .unwrap_err();
        assert!(error.downcast_ref::<PermanentProviderError>().is_some());
    }
}
