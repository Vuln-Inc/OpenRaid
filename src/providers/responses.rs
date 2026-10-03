//! Responses API preserves opaque reasoning items across stateless turns.
use super::{
    Completion, ContextOverflow, PermanentProviderError, ToolCall, Usage, MAX_COMPLETION_BYTES,
};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) fn request(
    model: &str,
    messages: &[Value],
    tools: &[Value],
    max_output_tokens: usize,
    options: &Value,
) -> Result<Value> {
    let mut input = Vec::new();
    let mut instructions = Vec::new();
    for message in messages {
        let role = message["role"].as_str().unwrap_or("user");
        if matches!(role, "system" | "developer") {
            if let Some(text) = message["content"].as_str() {
                instructions.push(text.to_owned());
            }
            continue;
        }
        if role == "tool" {
            input.push(
                json!({"type":"function_call_output", "call_id":message["tool_call_id"],
                "output":message["content"]}),
            );
            continue;
        }
        if let Some(items) = message["_openraid_response_items"].as_array() {
            // These are complete original wire items, including encrypted reasoning.
            input.extend(items.iter().cloned());
            continue;
        }
        if message["content"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
        {
            input.push(json!({"role":role,"content":message["content"]}));
        }
        if let Some(calls) = message["tool_calls"].as_array() {
            for call in calls {
                input.push(json!({"type":"function_call","call_id":call["id"],
                    "name":call["function"]["name"],"arguments":call["function"]["arguments"]}));
            }
        }
    }
    let mut payload = json!({"model":model,"input":input,"stream":true,"store":false,
        "max_output_tokens":max_output_tokens,"include":["reasoning.encrypted_content"]});
    if !instructions.is_empty() {
        payload["instructions"] = Value::String(instructions.join("\n\n"));
    }
    if !tools.is_empty() {
        payload["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    let mut function = tool["function"].clone();
                    function["type"] = json!("function");
                    // Native tools use optional JSON properties, so do not force strict schemas.
                    function["strict"] = json!(false);
                    function
                })
                .collect(),
        );
        payload["tool_choice"] = json!("auto");
    }
    if let Some(options) = options.as_object() {
        for (key, value) in options {
            if !matches!(
                key.as_str(),
                "model" | "input" | "instructions" | "tools" | "stream" | "store"
            ) {
                payload[key] = value.clone();
            }
        }
    }
    Ok(payload)
}

#[derive(Default)]
pub(super) struct ResponsesAccumulator {
    completion: Completion,
    calls: BTreeMap<u64, ToolCall>,
    items: BTreeMap<u64, Value>,
    bytes: usize,
    finished: bool,
}

impl ResponsesAccumulator {
    pub(super) fn apply(&mut self, value: &Value, streaming: bool) -> Result<String> {
        let kind = value["type"].as_str().unwrap_or_default();
        if matches!(kind, "error" | "response.failed") || (!streaming && !value["error"].is_null())
        {
            let error = if kind == "response.failed" {
                &value["response"]["error"]
            } else {
                value.get("error").unwrap_or(value)
            };
            if matches!(
                error["code"].as_str(),
                Some("context_length_exceeded" | "context_window_exceeded")
            ) {
                return Err(ContextOverflow.into());
            }
            if matches!(
                error["code"].as_str().or_else(|| error["type"].as_str()),
                Some(
                    "server_error"
                        | "internal_error"
                        | "rate_limit_exceeded"
                        | "overloaded_error"
                        | "api_error"
                )
            ) {
                bail!("provider temporarily unavailable");
            }
            return Err(PermanentProviderError.into());
        }
        let mut delta = String::new();
        let index = value["output_index"].as_u64().unwrap_or(0);
        match kind {
            "response.output_text.delta" | "response.refusal.delta" => {
                delta = value["delta"].as_str().unwrap_or_default().to_owned();
                self.completion.content.push_str(&delta);
                self.bytes = self.bytes.saturating_add(delta.len());
            }
            "response.output_item.added" | "response.output_item.done" => {
                let item = &value["item"];
                if item["type"] == "function_call" {
                    let call = self.calls.entry(index).or_default();
                    call.id = item["call_id"].as_str().unwrap_or_default().into();
                    call.name = item["name"].as_str().unwrap_or_default().into();
                    if let Some(arguments) = item["arguments"].as_str() {
                        call.arguments = arguments.into();
                    }
                }
                if kind == "response.output_item.done" {
                    self.bytes = self.bytes.saturating_add(item.to_string().len());
                    self.items.insert(index, item.clone());
                }
            }
            "response.function_call_arguments.delta" => {
                let fragment = value["delta"].as_str().unwrap_or_default();
                self.calls
                    .entry(index)
                    .or_default()
                    .arguments
                    .push_str(fragment);
                self.bytes = self.bytes.saturating_add(fragment.len());
            }
            "response.function_call_arguments.done" => {
                if let Some(arguments) = value["arguments"].as_str() {
                    self.calls.entry(index).or_default().arguments = arguments.into();
                }
            }
            "response.completed" | "response.incomplete" => {
                self.apply_complete(&value["response"])?;
                self.finished = true;
            }
            _ if !streaming => {
                self.apply_complete(value)?;
                self.finished = true;
            }
            _ => {}
        }
        if self.bytes > MAX_COMPLETION_BYTES {
            bail!("provider completion exceeds memory limit");
        }
        Ok(delta)
    }

    fn apply_complete(&mut self, response: &Value) -> Result<()> {
        if !response["output"].is_array()
            && !matches!(
                response["status"].as_str(),
                Some("completed" | "incomplete" | "failed")
            )
        {
            return Err(PermanentProviderError.into());
        }
        if response["status"] == "failed" {
            return Err(PermanentProviderError.into());
        }
        if let Some(output) = response["output"]
            .as_array()
            .filter(|output| !output.is_empty())
        {
            let mut text = String::new();
            self.calls.clear();
            self.items.clear();
            for (index, item) in output.iter().enumerate() {
                self.items.insert(index as u64, item.clone());
                if item["type"] == "function_call" {
                    self.calls.insert(
                        index as u64,
                        ToolCall {
                            id: item["call_id"].as_str().unwrap_or_default().into(),
                            name: item["name"].as_str().unwrap_or_default().into(),
                            arguments: item["arguments"].as_str().unwrap_or_default().into(),
                        },
                    );
                }
                if let Some(content) = item["content"].as_array() {
                    for part in content {
                        if part["type"] == "output_text" {
                            text.push_str(part["text"].as_str().unwrap_or_default());
                        } else if part["type"] == "refusal" {
                            text.push_str(part["refusal"].as_str().unwrap_or_default());
                        }
                    }
                }
            }
            self.completion.content = text;
            self.bytes = output.iter().map(|item| item.to_string().len()).sum();
        }
        let usage = &response["usage"];
        self.completion.usage = Usage {
            input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
            cached_tokens: usage["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
        };
        self.completion.finish_reason = if response["status"] == "incomplete" {
            "length"
        } else if self.calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        }
        .into();
        Ok(())
    }

    pub(super) fn is_finished(&self) -> bool {
        self.finished
    }

    pub(super) fn finish(mut self) -> Result<Completion> {
        if !self.finished {
            bail!("Responses stream ended before completion");
        }
        for call in self.calls.into_values() {
            if call.id.is_empty() || call.name.is_empty() {
                bail!("provider supplied an incomplete tool call");
            }
            self.completion.tool_calls.push(call);
        }
        self.completion.response_items = self.items.into_values().collect();
        Ok(self.completion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_snapshot_replaces_deltas_and_replays_encrypted_reasoning() -> Result<()> {
        let mut acc = ResponsesAccumulator::default();
        acc.apply(
            &json!({"type":"response.output_text.delta","delta":"ok"}),
            true,
        )?;
        acc.apply(&json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"call_1","name":"board_read","arguments":""}}), true)?;
        acc.apply(
            &json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{}"}),
            true,
        )?;
        let output = json!([
            {"type":"reasoning","id":"rs_1","encrypted_content":"opaque","summary":[]},
            {"type":"function_call","call_id":"call_1","name":"board_read","arguments":"{}"},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}
        ]);
        acc.apply(&json!({"type":"response.completed","response":{"status":"completed","output":output,"usage":{"input_tokens":9,"output_tokens":2,"input_tokens_details":{"cached_tokens":3}}}}), true)?;
        let result = acc.finish()?;
        assert_eq!(result.content, "ok");
        assert_eq!(result.tool_calls[0].arguments, "{}");
        assert_eq!(result.usage.cached_tokens, 3);
        let payload = request(
            "test",
            &[
                json!({"role":"system","content":"work"}),
                result.assistant_message(),
                json!({"role":"tool","tool_call_id":"call_1","content":"done"}),
            ],
            &[],
            64,
            &json!({"reasoning":{"effort":"high"}}),
        )?;
        assert_eq!(payload["input"][0]["encrypted_content"], "opaque");
        assert_eq!(payload["input"][3]["type"], "function_call_output");
        assert_eq!(payload["instructions"], "work");
        assert_eq!(payload["reasoning"]["effort"], "high");
        Ok(())
    }

    #[test]
    fn interrupted_and_context_overflow_are_not_silent_success() -> Result<()> {
        let mut acc = ResponsesAccumulator::default();
        acc.apply(
            &json!({"type":"response.output_text.delta","delta":"partial"}),
            true,
        )?;
        assert!(acc.finish().is_err());
        let error = ResponsesAccumulator::default().apply(&json!({"type":"response.failed","response":{"error":{"code":"context_length_exceeded"}}}), true).unwrap_err();
        assert!(super::super::is_context_overflow(&error));
        Ok(())
    }

    #[test]
    fn refusal_is_visible_and_wrong_protocol_is_actionable() -> Result<()> {
        let mut acc = ResponsesAccumulator::default();
        acc.apply(&json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"Cannot complete that request."}]}]}), false)?;
        assert_eq!(acc.finish()?.content, "Cannot complete that request.");
        let error = ResponsesAccumulator::default()
            .apply(
                &json!({"choices":[{"message":{"content":"wrong protocol"}}]}),
                false,
            )
            .unwrap_err();
        assert!(error.downcast_ref::<PermanentProviderError>().is_some());
        Ok(())
    }
}
