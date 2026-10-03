use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    thread::{self, JoinHandle},
};

fn command(directory: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_openraid"));
    command
        .current_dir(directory)
        .env("OPENRAID_AUTH_FILE", directory.join("auth.json"))
        .env("XDG_DATA_HOME", directory)
        .env("XDG_CONFIG_HOME", directory)
        .env_remove("OPENRAID_PROVIDER")
        .env_remove("OPENRAID_MODEL")
        .env_remove("OPENRAID_VARIANT")
        .env_remove("OPENRAID_BASE_URL")
        .env_remove("OPENRAID_API_KEY");
    command
}

fn serve_once(status: &str, body: Value) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let status = status.to_owned();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = socket.read(&mut chunk).unwrap();
            assert!(count > 0, "discovery request ended before headers");
            request.extend_from_slice(&chunk[..count]);
            assert!(request.len() < 16 * 1024);
        }
        let body = body.to_string();
        write!(
            socket,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        String::from_utf8(request).unwrap()
    });
    (base_url, server)
}

#[test]
fn codex_listing_fetches_live_models_without_refresh_flag() {
    let directory = tempfile::tempdir().unwrap();
    let (base_url, server) = serve_once(
        "200 OK",
        json!({"data":[
            {"id":"gpt-6.1-sol","metadata":{
                "context_window":64000,"max_output_tokens":8192,
                "supported_reasoning_levels":[{"effort":"high"},{"effort":"xhigh"}]
            }},
            {"id":"gpt-6-astra"},
            {"id":"deployment-only-model"}
        ]}),
    );
    let output = command(directory.path())
        .args(["models", "codex-lb", "--json", "--base-url", &base_url])
        .args(["--api-key", "test-discovery-key"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let models: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(models.len(), 3);
    assert!(models
        .iter()
        .any(|model| model["id"] == "deployment-only-model"));
    assert!(!models.iter().any(|model| model["id"] == "gpt-4.1-mini"));
    let sol = models
        .iter()
        .find(|model| model["id"] == "gpt-6.1-sol")
        .unwrap();
    assert_eq!(sol["limit"]["context"], 64000);
    assert_eq!(sol["variants"].as_object().unwrap().len(), 2);
    assert_eq!(sol["variants"]["xhigh"]["reasoningEffort"], "xhigh");
    let astra = models
        .iter()
        .find(|model| model["id"] == "gpt-6-astra")
        .unwrap();
    assert!(astra["variants"].as_object().unwrap().is_empty());
    assert_eq!(astra["limit"]["context"], 0);
    let request = server.join().unwrap();
    assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer test-discovery-key\r\n"));
}

#[test]
fn codex_pool_imports_global_context_limits_and_project_overrides() {
    let directory = tempfile::tempdir().unwrap();
    let config_directory = directory.path().join("opencode");
    std::fs::create_dir(&config_directory).unwrap();
    let (base_url, server) = serve_once(
        "200 OK",
        json!({"data":[
            {"id":"gpt-6.1-sol","metadata":{"context_window":260000,
                "supported_reasoning_levels":[{"effort":"high"}]}},
            {"id":"gpt-6-luna"},
            {"id":"deployment-only-model"}
        ]}),
    );
    std::fs::write(
        config_directory.join("opencode.json"),
        json!({"provider":{"codex-pool":{
            "npm":"@ai-sdk/openai","options":{"baseURL":base_url,"apiKey":"test-discovery-key"},
            "models":{
                "gpt-6.1-sol":{"limit":{"context":372000,"output":65536}},
                "gpt-6-luna":{"limit":{"context":372000,"output":65536}},
                "unserved-model":{"limit":{"context":372000,"output":65536}}
            }
        }}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("openraid.json"),
        json!({"provider":{"codex-pool":{
            "models":{"gpt-6-luna":{"limit":{"context":320000}}}
        }}})
        .to_string(),
    )
    .unwrap();
    let output = command(directory.path())
        .args(["models", "codex-pool", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let models: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(models.len(), 3);
    let sol = models
        .iter()
        .find(|model| model["id"] == "gpt-6.1-sol")
        .unwrap();
    assert_eq!(sol["limit"]["context"], 372000);
    assert_eq!(sol["limit"]["output"], 65536);
    assert_eq!(sol["variants"]["high"]["reasoningEffort"], "high");
    let luna = models
        .iter()
        .find(|model| model["id"] == "gpt-6-luna")
        .unwrap();
    assert_eq!(luna["limit"]["context"], 320000);
    assert_eq!(luna["limit"]["output"], 65536);
    assert!(!models.iter().any(|model| model["id"] == "unserved-model"));
    let request = server.join().unwrap();
    assert!(request.starts_with("GET /v1/models HTTP/1.1\r\n"));
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer test-discovery-key\r\n"));
}

#[test]
fn codex_headless_run_rejects_unadvertised_model_before_database_creation() {
    let directory = tempfile::tempdir().unwrap();
    let (base_url, server) = serve_once("200 OK", json!({"data":[{"id":"gpt-6.1-sol"}]}));
    let database = directory.path().join("must-not-exist.sqlite3");
    let output = command(directory.path())
        .args([
            "run",
            "Verify server model selection",
            "--provider",
            "codex-lb",
        ])
        .args(["--model", "stale-model", "--base-url", &base_url])
        .args(["--api-key", "test-discovery-key", "--no-tui", "--database"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not advertised by your Codex LB server")
    );
    assert!(!database.exists());
    assert!(server
        .join()
        .unwrap()
        .starts_with("GET /v1/models HTTP/1.1\r\n"));
}

#[test]
fn failed_codex_discovery_does_not_print_guessed_models_or_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let secret = "private-test-discovery-key";
    let (base_url, server) = serve_once("401 Unauthorized", json!({"error":secret}));
    let output = command(directory.path())
        .args(["models", "codex-lb", "--json", "--base-url", &base_url])
        .args(["--api-key", secret])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("401"));
    assert!(!error.contains(secret));
    server.join().unwrap();
}
