//! Verify the public provider transport against protocol-shaped loopback servers.
//! These tests exercise HTTP routing, authentication, streaming, and tool replay.
use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, Result};
use openraid::provider::{Completion, Protocol, Provider, ProviderConfig};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

struct Request {
    path: String,
    headers: BTreeMap<String, String>,
    header_counts: BTreeMap<String, usize>,
    body: Value,
}

async fn read_request(socket: &mut TcpStream) -> Result<Request> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let count = socket.read(&mut chunk).await?;
        anyhow::ensure!(count != 0, "request ended before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
        anyhow::ensure!(bytes.len() < 64 * 1024, "oversized mock request headers");
    };
    let head = std::str::from_utf8(&bytes[..header_end])?;
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .context("missing request path")?
        .to_owned();
    let headers: BTreeMap<String, String> = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let mut header_counts = BTreeMap::new();
    for (name, _) in head.lines().filter_map(|line| line.split_once(':')) {
        *header_counts.entry(name.to_ascii_lowercase()).or_insert(0) += 1;
    }
    let length: usize = headers
        .get("content-length")
        .context("missing content length")?
        .parse()?;
    anyhow::ensure!(length < 1024 * 1024, "oversized mock request body");
    while bytes.len() < header_end + length {
        let count = socket.read(&mut chunk).await?;
        anyhow::ensure!(count != 0, "request body ended early");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(Request {
        path,
        headers,
        header_counts,
        body: serde_json::from_slice(&bytes[header_end..header_end + length])?,
    })
}

async fn mock_server(
    replies: Vec<(&'static str, String)>,
) -> Result<(String, JoinHandle<Result<Vec<Request>>>)> {
    mock_server_with_status(
        replies
            .into_iter()
            .map(|(kind, body)| ("200 OK", kind, body))
            .collect(),
    )
    .await
}

async fn mock_server_with_status(
    replies: Vec<(&'static str, &'static str, String)>,
) -> Result<(String, JoinHandle<Result<Vec<Request>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, content_type, body) in replies {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .context("provider never contacted mock server")??;
            requests.push(read_request(&mut socket).await?);
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if status == "401 Unauthorized" {
                // A permanent auth failure need not consume the HTTP body;
                // avoid fragmentation after the client has rejected headers.
                socket
                    .write_all(format!("{headers}{body}").as_bytes())
                    .await?;
                let _ = socket.shutdown().await;
                continue;
            }
            socket.write_all(headers.as_bytes()).await?;
            // Deliberately fragment UTF-8 and SSE framing across writes.
            for chunk in body.as_bytes().chunks(7) {
                socket.write_all(chunk).await?;
                tokio::task::yield_now().await;
            }
            socket.shutdown().await?;
        }
        Ok(requests)
    });
    Ok((base, task))
}

fn provider(base: String, protocol: Protocol, options: Value) -> Result<Provider> {
    provider_with_concurrency(base, protocol, options, 1)
}

fn provider_with_concurrency(
    base: String,
    protocol: Protocol,
    options: Value,
    max_in_flight: usize,
) -> Result<Provider> {
    Provider::new_with_options(
        ProviderConfig {
            base_url: base,
            api_key: Some("loopback-secret".into()),
            model: "test-model".into(),
            max_in_flight,
            max_output_tokens: 4096,
        },
        protocol,
        options,
        BTreeMap::from([("x-openraid-test".into(), "protocol-test".into())]),
    )
}

fn tools() -> Vec<Value> {
    vec![json!({"type":"function", "function":{
        "name":"board_post", "description":"Post to the global board",
        "parameters":{"type":"object","properties":{"body":{"type":"string"}},"required":["body"]}
    }})]
}

fn history() -> Vec<Value> {
    vec![
        json!({"role":"system","content":"Coordinate through the board."}),
        json!({"role":"user","content":"Inspect the project."}),
    ]
}

fn sse(values: &[Value]) -> String {
    values
        .iter()
        .map(|value| format!("data: {value}\r\n\r\n"))
        .collect()
}

async fn complete(provider: &Provider, messages: &[Value]) -> Result<Completion> {
    tokio::time::timeout(
        Duration::from_secs(10),
        provider.complete(messages, &tools(), None),
    )
    .await
    .context("provider completion stalled")?
}

#[tokio::test]
async fn codex_lb_responses_stream_preserves_tools_reasoning_and_custom_headers() -> Result<()> {
    let output = json!([
        {"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque-reasoning"},
        {"type":"message","id":"msg_1","role":"assistant","content":[{"type":"output_text","text":"Ready ✓"}]},
        {"type":"function_call","id":"fc_1","call_id":"call_1","name":"board_post","arguments":"{\"body\":\"reviewed\"}"}
    ]);
    let response = json!({"id":"resp_1","status":"completed","output":output,
        "usage":{"input_tokens":12,"output_tokens":8,"input_tokens_details":{"cached_tokens":4}}});
    let stream = sse(&[
        json!({"type":"response.created","response":{"id":"resp_1","status":"in_progress","output":[]}}),
        json!({"type":"response.output_text.delta","item_id":"msg_1","output_index":1,"content_index":0,"delta":"Ready ✓"}),
        json!({"type":"response.completed","response":response}),
    ]);
    let (base, server) = mock_server(vec![
        ("text/event-stream", stream),
        ("application/json", response.to_string()),
    ])
    .await?;
    let client = provider(
        format!("{base}/v1"),
        Protocol::Responses,
        json!({"reasoning":{"effort":"xhigh","summary":"auto"},"store":false}),
    )?;
    let mut messages = history();
    let first = complete(&client, &messages).await?;
    assert_eq!(first.content, "Ready ✓");
    assert_eq!(first.tool_calls.len(), 1);
    assert_eq!(first.tool_calls[0].id, "call_1");
    assert_eq!(first.tool_calls[0].name, "board_post");
    assert_eq!(first.usage.cached_tokens, 4);
    messages.push(first.assistant_message());
    messages.push(json!({"role":"tool","tool_call_id":"call_1","content":"posted"}));
    complete(&client, &messages).await?;
    let requests = server.await??;
    for request in &requests {
        assert_eq!(request.path, "/v1/responses");
        assert_eq!(request.headers["authorization"], "Bearer loopback-secret");
        assert_eq!(request.headers["x-openraid-test"], "protocol-test");
        assert_eq!(request.body["reasoning"]["effort"], "xhigh");
        assert_eq!(request.body["store"], false);
        assert_eq!(request.body["tools"][0]["name"], "board_post");
        assert!(request.body.get("messages").is_none());
    }
    let replay = requests[1].body["input"]
        .as_array()
        .context("missing Responses input")?;
    assert!(
        replay
            .iter()
            .any(|item| item["type"] == "reasoning"
                && item["encrypted_content"] == "opaque-reasoning")
    );
    assert!(replay
        .iter()
        .any(|item| item["type"] == "function_call_output"
            && item["call_id"] == "call_1"
            && item["output"] == "posted"));
    Ok(())
}

#[tokio::test]
async fn codex_backend_full_responses_endpoint_is_not_double_appended() -> Result<()> {
    let response = json!({"status":"completed","output":[{"type":"message","role":"assistant",
        "content":[{"type":"output_text","text":"done"}]}]});
    let (base, server) = mock_server(vec![("application/json", response.to_string())]).await?;
    let client = provider(
        format!("{base}/backend-api/codex/responses"),
        Protocol::Responses,
        json!({}),
    )?;
    assert_eq!(complete(&client, &history()).await?.content, "done");
    assert_eq!(server.await??[0].path, "/backend-api/codex/responses");
    Ok(())
}

#[tokio::test]
async fn anthropic_stream_preserves_signed_thinking_and_native_tool_history() -> Result<()> {
    let stream = sse(&[
        json!({"type":"message_start","message":{"id":"msg_1","role":"assistant","content":[],"usage":{"input_tokens":12,"output_tokens":0,"cache_read_input_tokens":4}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Inspect the project"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed-thinking"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Ready ✓"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"call_1","name":"board_post","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"body\":\"reviewed\"}"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":8}}),
        json!({"type":"message_stop"}),
    ]);
    let response = json!({"role":"assistant","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{"input_tokens":13,"output_tokens":2}});
    let (base, server) = mock_server(vec![
        ("text/event-stream", stream),
        ("application/json", response.to_string()),
    ])
    .await?;
    let client = provider(
        format!("{base}/v1"),
        Protocol::Anthropic,
        json!({"thinking":{"type":"enabled","budget_tokens":1024}}),
    )?;
    let mut messages = history();
    let first = complete(&client, &messages).await?;
    assert_eq!(first.content, "Ready ✓");
    assert_eq!(first.tool_calls[0].arguments, "{\"body\":\"reviewed\"}");
    assert_eq!(first.usage.cached_tokens, 4);
    messages.push(first.assistant_message());
    messages.push(json!({"role":"tool","tool_call_id":"call_1","content":"posted"}));
    complete(&client, &messages).await?;
    let requests = server.await??;
    assert_eq!(requests[0].path, "/v1/messages");
    assert_eq!(requests[0].headers["x-api-key"], "loopback-secret");
    assert!(requests[0].headers.contains_key("anthropic-version"));
    assert_eq!(requests[0].body["tools"][0]["name"], "board_post");
    assert!(requests[0].body["max_tokens"].as_u64().unwrap() > 1024);
    let replay = requests[1].body["messages"]
        .as_array()
        .context("missing Anthropic messages")?;
    assert!(replay.iter().any(|message| message["role"] == "assistant"
        && message["content"].as_array().is_some_and(|parts| parts
            .iter()
            .any(|part| part["type"] == "thinking" && part["signature"] == "signed-thinking"))));
    assert!(replay.iter().any(
        |message| message["content"].as_array().is_some_and(|parts| parts
            .iter()
            .any(|part| part["type"] == "tool_result" && part["tool_use_id"] == "call_1"))
    ));
    Ok(())
}

#[tokio::test]
async fn gemini_stream_preserves_tool_thought_signature_and_result_pairing() -> Result<()> {
    let stream = sse(&[
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"Ready ✓"}]}}]}),
        json!({"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"board_post","args":{"body":"reviewed"}},"thoughtSignature":"signed-gemini"}]},"finishReason":"STOP"}]}),
        json!({"usageMetadata":{"promptTokenCount":12,"candidatesTokenCount":8,"cachedContentTokenCount":4}}),
    ]);
    let response = json!({"candidates":[{"content":{"role":"model","parts":[{"text":"done"}]},"finishReason":"STOP"}]});
    let (base, server) = mock_server(vec![
        ("text/event-stream", stream),
        ("application/json", response.to_string()),
    ])
    .await?;
    let client = provider(
        format!("{base}/v1beta"),
        Protocol::Gemini,
        json!({"generationConfig":{"thinkingConfig":{"thinkingLevel":"high","includeThoughts":true}}}),
    )?;
    let mut messages = history();
    let first = complete(&client, &messages).await?;
    assert_eq!(first.content, "Ready ✓");
    assert_eq!(first.tool_calls[0].name, "board_post");
    assert_eq!(
        (first.usage.input_tokens, first.usage.output_tokens),
        (12, 8)
    );
    assert_eq!(first.usage.cached_tokens, 4);
    let call_id = first.tool_calls[0].id.clone();
    messages.push(first.assistant_message());
    messages.push(json!({"role":"tool","tool_call_id":call_id,"content":"posted"}));
    complete(&client, &messages).await?;
    let requests = server.await??;
    assert!(requests[0]
        .path
        .starts_with("/v1beta/models/test-model:streamGenerateContent"));
    assert_eq!(requests[0].headers["x-goog-api-key"], "loopback-secret");
    assert_eq!(
        requests[0].body["tools"][0]["functionDeclarations"][0]["name"],
        "board_post"
    );
    let replay = requests[1].body["contents"]
        .as_array()
        .context("missing Gemini contents")?;
    assert!(replay.iter().any(
        |message| message["parts"].as_array().is_some_and(|parts| parts
            .iter()
            .any(|part| part["thoughtSignature"] == "signed-gemini"))
    ));
    assert!(replay.iter().any(
        |message| message["parts"].as_array().is_some_and(|parts| parts
            .iter()
            .any(|part| part["functionResponse"]["name"] == "board_post"))
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harness_honors_selected_protocol_variant_and_provider_headers() -> Result<()> {
    use openraid::{config::Config, runtime::Harness};

    let directory = tempfile::tempdir()?;
    let response = json!({"status":"completed","output":[{
        "type":"function_call","call_id":"vote_1","name":"vote_done",
        "arguments":json!({"done":true,"evidence":"Loopback verification reached the native completion gate."}).to_string()
    }],"usage":{"input_tokens":12,"output_tokens":8}});
    let (base, server) = mock_server(vec![("application/json", response.to_string())]).await?;
    let harness = Harness::new(Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: directory.path().join("protocol.sqlite"),
        objective: "Verify selected protocol and thinking variant reach the provider.".into(),
        base_url: format!("{base}/v1"),
        provider: "codex-lb".into(),
        model: "catalog-display-model".into(),
        api_model: Some("test-model".into()),
        provider_npm: "@ai-sdk/openai".into(),
        protocol: Protocol::Responses,
        variant: Some("xhigh".into()),
        provider_options: json!({"reasoningEffort":"xhigh","reasoningSummary":"auto"}),
        provider_headers: BTreeMap::from([("x-selected-provider".into(), "codex-lb".into())]),
        grace_period: Duration::ZERO,
        no_tui: true,
        ..Config::default()
    })
    .await?;
    let summary = tokio::time::timeout(Duration::from_secs(10), harness.run())
        .await
        .context("selected protocol harness stalled")??;
    assert_eq!((summary.finished_agents, summary.votes), (1, 1));
    let requests = server.await??;
    assert_eq!(requests[0].path, "/v1/responses");
    assert_eq!(requests[0].body["model"], "test-model");
    assert_eq!(requests[0].body["reasoning"]["effort"], "xhigh");
    assert_eq!(requests[0].body["reasoning"]["summary"], "auto");
    assert!(requests[0].body.get("reasoningEffort").is_none());
    assert_eq!(requests[0].headers["x-selected-provider"], "codex-lb");
    Ok(())
}

#[tokio::test]
async fn chat_resume_strips_private_native_metadata_without_mutating_history() -> Result<()> {
    let response = json!({"choices":[{"finish_reason":"stop","message":{
        "role":"assistant","content":"continued"
    }}]});
    let (base, server) = mock_server(vec![("application/json", response.to_string())]).await?;
    let client = provider(format!("{base}/v1"), Protocol::ChatCompletions, json!({}))?;
    let mut messages = history();
    messages.push(json!({
        "role":"assistant","content":"prior turn",
        "_openraid_native":{"anthropic":[{"type":"thinking","signature":"signed","thinking":"private"}]},
        "_openraid_response_items":[{"type":"reasoning","encrypted_content":"opaque"}]
    }));
    let original = messages.clone();
    assert_eq!(complete(&client, &messages).await?.content, "continued");
    assert_eq!(
        messages, original,
        "wire normalization must not modify persisted history"
    );
    let requests = server.await??;
    let assistant = &requests[0].body["messages"][2];
    assert_eq!(assistant["content"], "prior turn");
    assert!(assistant.get("_openraid_native").is_none());
    assert!(assistant.get("_openraid_response_items").is_none());
    Ok(())
}

#[tokio::test]
async fn chat_reasoning_content_survives_streamed_tool_followup() -> Result<()> {
    let stream = format!(
        "{}data: [DONE]\r\n\r\n",
        sse(&[
            json!({"choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"Review first. "}}]}),
            json!({"choices":[{"index":0,"delta":{"reasoning_content":"Then coordinate.","content":"Ready"}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"board_post","arguments":"{\"body\":\"reviewed\"}"}}]},"finish_reason":"tool_calls"}]}),
        ])
    );
    let response = json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"done"}}]});
    let (base, server) = mock_server(vec![
        ("text/event-stream", stream),
        ("application/json", response.to_string()),
    ])
    .await?;
    let client = provider(
        format!("{base}/v1"),
        Protocol::ChatCompletions,
        json!({"max_completion_tokens":2048}),
    )?;
    let mut messages = history();
    let first = complete(&client, &messages).await?;
    assert_eq!(
        first.content, "Ready",
        "private reasoning must not become user-facing output"
    );
    messages.push(first.assistant_message());
    messages.push(json!({"role":"tool","tool_call_id":"call_1","content":"posted"}));
    complete(&client, &messages).await?;
    let requests = server.await??;
    for request in &requests {
        assert_eq!(request.body["max_completion_tokens"], 2048);
        assert!(request.body.get("max_tokens").is_none());
    }
    let assistant = &requests[1].body["messages"][2];
    assert_eq!(
        assistant["reasoning_content"],
        "Review first. Then coordinate."
    );
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
    assert!(assistant.get("_openraid_native").is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harness_codex_oauth_applies_account_auth_and_omits_unsupported_output_cap() -> Result<()> {
    use openraid::{auth::AuthStore, config::Config, oauth::OAuthSession, runtime::Harness};

    let directory = tempfile::tempdir()?;
    let mut store = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64
        + 3_600_000;
    store.set_oauth(
        "openai",
        &json!({"type":"oauth","access":"oauth-loopback-secret",
        "refresh":"unused-refresh-token","expires":expires,"accountId":"loopback-account"}),
    )?;
    let session = OAuthSession::from_store("openai", &store)?.context("missing OAuth session")?;
    let response = json!({"status":"completed","output":[{
        "type":"function_call","call_id":"vote_oauth","name":"vote_done",
        "arguments":json!({"done":true,"evidence":"Verified Codex account authorization on a local protocol fixture."}).to_string()
    }]});
    let (base, server) = mock_server(vec![("application/json", response.to_string())]).await?;
    let harness = Harness::new(Config {
        agents: 1,
        workspace: directory.path().to_owned(),
        database: directory.path().join("oauth.sqlite"),
        objective: "Verify locally that imported Codex OAuth reaches the transport.".into(),
        base_url: format!("{base}/backend-api/codex"),
        protocol: Protocol::Responses,
        oauth: Some(session),
        grace_period: Duration::ZERO,
        no_tui: true,
        ..Config::default()
    })
    .await?;
    let summary = tokio::time::timeout(Duration::from_secs(10), harness.run())
        .await
        .context("OAuth loopback harness stalled")??;
    assert_eq!((summary.finished_agents, summary.votes), (1, 1));
    let requests = server.await??;
    assert_eq!(requests[0].path, "/backend-api/codex/responses");
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer oauth-loopback-secret"
    );
    assert_eq!(
        requests[0].headers["chatgpt-account-id"],
        "loopback-account"
    );
    assert!(requests[0].body.get("max_output_tokens").is_none());
    assert_eq!(requests[0].body["store"], false);
    Ok(())
}

#[tokio::test]
async fn copilot_oauth_uses_bearer_for_anthropic_instead_of_api_key_auth() -> Result<()> {
    use openraid::{auth::AuthStore, oauth::OAuthSession};

    let directory = tempfile::tempdir()?;
    let mut store = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    store.set_oauth(
        "github-copilot",
        &json!({"type":"oauth","access":"unused-access-token",
        "refresh":"github-loopback-token","expires":0}),
    )?;
    let session = OAuthSession::from_store("github-copilot", &store)?
        .context("missing Copilot OAuth session")?;
    let response = json!({"role":"assistant","content":[{"type":"text","text":"done"}],
        "stop_reason":"end_turn","usage":{"input_tokens":12,"output_tokens":8}});
    let (base, server) = mock_server(vec![("application/json", response.to_string())]).await?;
    let client =
        provider(format!("{base}/v1"), Protocol::Anthropic, json!({}))?.with_oauth(session);
    assert_eq!(complete(&client, &history()).await?.content, "done");
    let requests = server.await??;
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer github-loopback-token"
    );
    assert_eq!(requests[0].header_counts["authorization"], 1);
    assert!(!requests[0].headers.contains_key("x-api-key"));
    assert_eq!(requests[0].body["max_tokens"], 4096);
    Ok(())
}

#[tokio::test]
async fn imported_snowflake_oauth_uses_bearer_and_renames_native_output_limit() -> Result<()> {
    use openraid::{auth::AuthStore, oauth::OAuthSession};

    let directory = tempfile::tempdir()?;
    let mut store = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64
        + 3_600_000;
    store.set_oauth("snowflake-cortex", &json!({"type":"oauth","accountId":"org-account",
        "access":"snowflake-loopback-token","refresh":"unused-snowflake-refresh","expires":expires}))?;
    let session = OAuthSession::from_store("snowflake-cortex", &store)?
        .context("missing imported Snowflake OAuth session")?;
    let response = json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"done"}}]});
    let (base, server) = mock_server(vec![("application/json", response.to_string()); 3]).await?;
    let client = provider(
        format!("{base}/api/v2/cortex/v1"),
        Protocol::ChatCompletions,
        json!({}),
    )?
    .with_oauth(session);
    assert_eq!(complete(&client, &history()).await?.content, "done");
    let pat = provider(
        format!("{base}/api/v2/cortex/v1"),
        Protocol::ChatCompletions,
        json!({}),
    )?
    .with_sdk("@ai-sdk/openai-compatible", "snowflake-cortex");
    assert_eq!(complete(&pat, &history()).await?.content, "done");
    let generic = provider(format!("{base}/v1"), Protocol::ChatCompletions, json!({}))?;
    assert_eq!(complete(&generic, &history()).await?.content, "done");
    let requests = server.await??;
    assert_eq!(requests[0].path, "/api/v2/cortex/v1/chat/completions");
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer snowflake-loopback-token"
    );
    assert_eq!(requests[0].body["max_completion_tokens"], 4096);
    assert!(requests[0].body.get("max_tokens").is_none());
    assert_eq!(
        requests[1].headers["authorization"],
        "Bearer loopback-secret"
    );
    assert_eq!(requests[1].body["max_completion_tokens"], 4096);
    assert!(requests[1].body.get("max_tokens").is_none());
    assert_eq!(requests[2].body["max_tokens"], 4096);
    assert!(requests[2].body.get("max_completion_tokens").is_none());
    Ok(())
}

#[tokio::test]
async fn snowflake_conversation_complete_error_is_normalized_only_for_snowflake_account(
) -> Result<()> {
    use openraid::{auth::AuthStore, oauth::OAuthSession};

    let directory = tempfile::tempdir()?;
    let mut store = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64
        + 3_600_000;
    store.set_oauth(
        "snowflake-cortex",
        &json!({"type":"oauth","accountId":"org-account",
        "access":"snowflake-loopback-token","refresh":"unused-refresh","expires":expires}),
    )?;
    let session = OAuthSession::from_store("snowflake-cortex", &store)?
        .context("missing Snowflake session")?;
    let error = json!({"error":{"message":"Conversation complete."}}).to_string();
    let (base, server) = mock_server_with_status(vec![
        ("400 Bad Request", "application/json", error.clone()),
        ("400 Bad Request", "application/json", error.clone()),
        ("400 Bad Request", "application/json", error),
        (
            "401 Unauthorized",
            "application/json",
            json!({"error":{"message":"PAT rejected"}}).to_string(),
        ),
    ])
    .await?;
    let snowflake =
        provider(format!("{base}/v1"), Protocol::ChatCompletions, json!({}))?.with_oauth(session);
    let normalized = complete(&snowflake, &history()).await?;
    assert!(normalized.content.is_empty());
    assert!(normalized.tool_calls.is_empty());
    assert_eq!(normalized.finish_reason, "stop");
    let pat = provider(format!("{base}/v1"), Protocol::ChatCompletions, json!({}))?
        .with_sdk("@ai-sdk/openai-compatible", "snowflake-cortex");
    let normalized = complete(&pat, &history()).await?;
    assert_eq!(normalized.finish_reason, "stop");
    assert!(normalized.content.is_empty());
    let generic = provider(format!("{base}/v1"), Protocol::ChatCompletions, json!({}))?;
    let error = complete(&generic, &history()).await.unwrap_err();
    assert!(
        error.to_string().contains("400"),
        "other accounts retain their HTTP error"
    );
    assert!(complete(&pat, &history())
        .await
        .unwrap_err()
        .to_string()
        .contains("401"));
    assert_eq!(server.await??.len(), 4);
    Ok(())
}

#[tokio::test]
async fn snowflake_unauthorized_refreshes_once_rotates_and_replays_the_request() -> Result<()> {
    use openraid::{auth::AuthStore, oauth::OAuthSession};

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("auth.json");
    let mut store = AuthStore::load_with_opencode(&path, None)?;
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64
        + 3_600_000;
    store.set_oauth("snowflake-cortex", &json!({"type":"oauth","accountId":"org-account",
        "access":"rejected-token","refresh":"old-refresh","expires":expires,"metadata":{"retained":"yes"}}))?;
    store.save()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let refresh_url = format!("http://{}/oauth/token-request", listener.local_addr()?);
    let refresh_server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let count = socket.read(&mut chunk).await?;
            anyhow::ensure!(count != 0, "refresh request ended before headers");
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = std::str::from_utf8(&bytes[..header_end])?.to_owned();
        let length: usize = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .context("refresh content length missing")?;
        while bytes.len() < header_end + length {
            let count = socket.read(&mut chunk).await?;
            anyhow::ensure!(count != 0, "refresh body ended early");
            bytes.extend_from_slice(&chunk[..count]);
        }
        let form = std::str::from_utf8(&bytes[header_end..header_end + length])?.to_owned();
        let body = json!({"access_token":"rotated-token","refresh_token":"rotated-refresh","expires_in":600}).to_string();
        let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        socket.write_all(headers.as_bytes()).await?;
        socket.write_all(body.as_bytes()).await?;
        socket.shutdown().await?;
        Ok::<_, anyhow::Error>((head, form))
    });
    let session = OAuthSession::from_store("snowflake-cortex", &store)?
        .context("missing Snowflake session")?
        .with_refresh_url(&refresh_url)?;
    let reply = json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"authorized"}}]});
    let (base, server) = mock_server_with_status(vec![
        (
            "401 Unauthorized",
            "application/json",
            json!({"error":{"message":"rejected"}}).to_string(),
        ),
        ("200 OK", "application/json", reply.to_string()),
    ])
    .await?;
    let client =
        provider(format!("{base}/v1"), Protocol::ChatCompletions, json!({}))?.with_oauth(session);
    assert_eq!(complete(&client, &history()).await?.content, "authorized");
    let requests = server.await??;
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer rejected-token"
    );
    assert_eq!(requests[1].headers["authorization"], "Bearer rotated-token");
    for request in &requests {
        assert_eq!(request.header_counts["authorization"], 1);
    }
    assert_eq!(
        requests[0].body, requests[1].body,
        "refresh replays identical model work"
    );
    let (head, form) = refresh_server.await??;
    assert!(head.to_ascii_lowercase().contains("authorization: basic "));
    assert!(form.contains("grant_type=refresh_token"));
    assert!(form.contains("refresh_token=old-refresh"));
    let persisted = AuthStore::load_with_opencode(path, None)?
        .oauth("snowflake-cortex")?
        .context("rotated credential missing")?;
    assert_eq!(persisted["access"], "rotated-token");
    assert_eq!(persisted["refresh"], "rotated-refresh");
    assert_eq!(persisted["metadata"]["retained"], "yes");
    Ok(())
}

#[tokio::test]
async fn snowflake_source_error_shapes_complete_without_retrying_work() -> Result<()> {
    for body in [
        json!({"message":"Conversation complete"}),
        json!({"error":"CONVERSATION COMPLETE"}),
        json!({"error":{"message":"conversation complete"}}),
    ] {
        let (base, server) = mock_server_with_status(vec![(
            "400 Bad Request",
            "application/json",
            body.to_string(),
        )])
        .await?;
        let client = provider(format!("{base}/v1"), Protocol::ChatCompletions, json!({}))?
            .with_sdk("@ai-sdk/openai-compatible", "snowflake-cortex");
        let completion = complete(&client, &history()).await?;
        assert!(completion.content.is_empty());
        assert_eq!(completion.finish_reason, "stop");
        let requests = server.await??;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].body.get("max_tokens").is_none());
        assert_eq!(requests[0].body["max_completion_tokens"], 4096);
    }
    Ok(())
}

#[tokio::test]
async fn copilot_native_claude_api_key_path_uses_bearer_authorization() -> Result<()> {
    let response = json!({"role":"assistant","content":[{"type":"text","text":"done"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}});
    let (base, server) = mock_server(vec![("application/json", response.to_string())]).await?;
    let client = provider(format!("{base}/v1"), Protocol::Anthropic, json!({}))?
        .with_sdk("@ai-sdk/openai-compatible", "github-copilot");
    assert_eq!(complete(&client, &history()).await?.content, "done");
    let requests = server.await??;
    assert_eq!(requests[0].path, "/v1/messages");
    assert_eq!(
        requests[0].headers["authorization"],
        "Bearer loopback-secret"
    );
    assert!(!requests[0].headers.contains_key("x-api-key"));
    Ok(())
}

#[tokio::test]
#[ignore = "requires Node.js 22.12+ and npm ci --prefix scripts for the optional SDK bridge"]
async fn optional_sdk_subprocess_runs_real_adapter_and_replays_native_tool_parts() -> Result<()> {
    use openraid::{auth::AuthStore, oauth::OAuthSession};

    let response = json!({"id":"chat_1","object":"chat.completion","created":1,"model":"test-model",
        "choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":"Ready",
            "tool_calls":[{"id":"call_sdk","type":"function","function":{"name":"board_post","arguments":"{\"body\":\"reviewed\"}"}}]}}],
        "usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}});
    let second = json!({"id":"chat_2","object":"chat.completion","created":1,"model":"test-model",
        "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"done"}}],
        "usage":{"prompt_tokens":13,"completion_tokens":2,"total_tokens":15}});
    let (base, server) = mock_server(vec![
        ("application/json", response.to_string()),
        ("application/json", second.to_string()),
    ])
    .await?;
    let directory = tempfile::tempdir()?;
    let mut store = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64
        + 3_600_000;
    store.set_oauth(
        "gitlab",
        &json!({"type":"oauth","access":"sdk-oauth-secret",
        "refresh":"unused-refresh","expires":expires,"enterpriseUrl":"https://gitlab.example"}),
    )?;
    let session =
        OAuthSession::from_store("gitlab", &store)?.context("missing mock GitLab session")?;
    let client = Provider::new_with_options(
        ProviderConfig {
            base_url: format!("{base}/v1"),
            api_key: Some("loopback-secret".into()),
            model: "test-model".into(),
            max_in_flight: 1,
            max_output_tokens: 4096,
        },
        Protocol::Sdk,
        json!({}),
        BTreeMap::from([
            ("x-openraid-test".into(), "protocol-test".into()),
            (
                "Authorization".into(),
                "Bearer stale-configured-secret".into(),
            ),
            ("Api-Key".into(), "stale-cloud-key".into()),
            ("X-Api-Key".into(), "stale-message-key".into()),
        ]),
    )?
    .with_sdk("@ai-sdk/openai-compatible", "loopback-sdk")
    .with_oauth(session);
    let mut messages = history();
    let first = complete(&client, &messages).await?;
    assert_eq!(first.content, "Ready");
    assert_eq!(first.tool_calls[0].id, "call_sdk");
    assert_eq!(
        (first.usage.input_tokens, first.usage.output_tokens),
        (12, 8)
    );
    let assistant = first.assistant_message();
    assert!(assistant["_openraid_native"]["sdk"].is_array());
    messages.push(assistant);
    messages.push(json!({"role":"tool","tool_call_id":"call_sdk","content":"posted"}));
    assert_eq!(complete(&client, &messages).await?.content, "done");
    let requests = server.await??;
    for request in &requests {
        assert_eq!(request.path, "/v1/chat/completions");
        assert_eq!(request.headers["authorization"], "Bearer sdk-oauth-secret");
        assert_eq!(request.header_counts["authorization"], 1);
        assert!(!request.headers.contains_key("api-key"));
        assert!(!request.headers.contains_key("x-api-key"));
        assert_eq!(request.headers["x-openraid-test"], "protocol-test");
        assert_eq!(request.body["model"], "test-model");
    }
    let replay = requests[1].body["messages"]
        .as_array()
        .context("missing SDK wire messages")?;
    assert!(replay
        .iter()
        .any(|message| message["role"] == "assistant"
            && message["tool_calls"][0]["id"] == "call_sdk"));
    assert!(replay.iter().any(|message| message["role"] == "tool"
        && message["tool_call_id"] == "call_sdk"
        && message["content"] == "posted"));

    // One shared sidecar must let a second admitted request finish before the
    // first. A FIFO response reader would swap these two completions.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await?;
        let first_request = read_request(&mut first).await?;
        let (mut second, _) = listener.accept().await?;
        let second_request = read_request(&mut second).await?;
        for (socket, text) in [(&mut second, "second-reply"), (&mut first, "first-reply")] {
            if text == "first-reply" {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let body = json!({"id":"parallel","object":"chat.completion","created":1,"model":"test-model",
                "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}).to_string();
            let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            socket.write_all(headers.as_bytes()).await?;
            socket.write_all(body.as_bytes()).await?;
            socket.shutdown().await?;
            tokio::task::yield_now().await;
        }
        Ok::<_, anyhow::Error>([first_request, second_request])
    });
    let concurrent = provider_with_concurrency(base, Protocol::Sdk, json!({}), 2)?
        .with_sdk("@ai-sdk/openai-compatible", "loopback-sdk");
    let mut one = history();
    one.push(json!({"role":"user","content":"parallel-one"}));
    let mut two = history();
    two.push(json!({"role":"user","content":"parallel-two"}));
    let (one_result, two_result) =
        tokio::join!(complete(&concurrent, &one), complete(&concurrent, &two));
    let (one_result, two_result) = (one_result?, two_result?);
    let requests = server.await??;
    let first_was_one = requests[0].body["messages"]
        .as_array()
        .context("missing parallel messages")?
        .iter()
        .any(|message| message["content"] == "parallel-one");
    let expected = if first_was_one {
        ("first-reply", "second-reply")
    } else {
        ("second-reply", "first-reply")
    };
    assert_eq!(
        (one_result.content.as_str(), two_result.content.as_str()),
        expected
    );
    Ok(())
}
