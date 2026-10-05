//! Packaging contracts that can be checked without Node, Tauri, or a WebView.
use serde_json::Value;
use std::{fs, path::Path};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn json(relative: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(root().join(relative)).unwrap()).unwrap()
}

#[test]
fn default_terminal_manifest_and_lock_do_not_require_desktop_dependencies() {
    let manifest = fs::read_to_string(root().join("Cargo.toml")).unwrap();
    let lock = fs::read_to_string(root().join("Cargo.lock")).unwrap();
    for name in [
        "tauri",
        "tauri-build",
        "tauri-runtime",
        "tauri-runtime-wry",
        "wry",
        "webkit2gtk",
        "webview2-com",
    ] {
        assert!(
            !manifest.lines().any(|line| {
                line.trim_start().starts_with(&format!("{name} ="))
                    || line.trim_start().starts_with(&format!("{name}="))
            }),
            "default manifest must not require {name}"
        );
        assert!(
            !lock
                .lines()
                .any(|line| line == format!("name = \"{name}\"")),
            "default lockfile must not resolve {name}"
        );
    }
}

#[test]
fn desktop_is_a_separate_package_using_the_in_process_library() {
    let manifest = fs::read_to_string(root().join("desktop/src-tauri/Cargo.toml")).unwrap();
    assert!(manifest.contains("name = \"openraid-desktop\""));
    assert!(manifest.lines().any(|line| line.trim() == "[workspace]"));
    assert!(manifest.contains("openraid = { path = \"../..\" }"));
    let root_manifest = fs::read_to_string(root().join("Cargo.toml")).unwrap();
    assert!(!root_manifest.contains("desktop/src-tauri"));
}

#[test]
fn desktop_assets_are_local_and_capabilities_do_not_grant_shell_or_filesystem() {
    let config = json("desktop/src-tauri/tauri.conf.json");
    assert_eq!(config["build"]["frontendDist"], "../dist");
    let csp = config["app"]["security"]["csp"].as_str().unwrap();
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("script-src 'self'"));
    assert!(!csp.contains("unsafe-eval"));
    assert!(!csp.contains("https:"));
    let capability = json("desktop/src-tauri/capabilities/default.json");
    assert_eq!(capability["windows"], serde_json::json!(["main"]));
    assert!(capability.get("remote").is_none());
    let permissions = capability["permissions"].as_array().unwrap();
    for permission in permissions {
        let permission = permission.as_str().unwrap();
        assert!(
            matches!(
                permission,
                "core:event:allow-listen" | "core:event:allow-unlisten"
            ),
            "unexpected broad desktop capability: {permission}"
        );
    }
}

#[test]
fn desktop_host_pushes_shared_runtime_events_without_cli_processes() {
    let host = fs::read_to_string(root().join("desktop/src-tauri/src/lib.rs")).unwrap();
    assert!(host.contains("DesktopSession::new("));
    assert!(host.contains("subscribe_changes()"));
    assert!(host.contains("changes.changed()"));
    assert!(host.contains("app.emit(SNAPSHOT_EVENT"));
    for forbidden in [
        "std::process::Command",
        "tokio::process::Command",
        "tauri_plugin_shell",
        "tokio::time::interval",
    ] {
        assert!(
            !host.contains(forbidden),
            "desktop host must use pushed in-process runtime state, not {forbidden}"
        );
    }
}

#[test]
fn desktop_security_does_not_load_a_remote_webview_or_shell_plugin() {
    let config = json("desktop/src-tauri/tauri.conf.json");
    assert!(config["app"]["windows"][0].get("url").is_none());
    let manifest = fs::read_to_string(root().join("desktop/src-tauri/Cargo.toml")).unwrap();
    assert!(!manifest.contains("tauri-plugin-shell"));
    assert!(!manifest.contains("tauri-plugin-fs"));
    let package = json("desktop/package.json");
    assert!(package["dependencies"]
        .get("@tauri-apps/plugin-shell")
        .is_none());
    assert!(package["dependencies"]
        .get("@tauri-apps/plugin-fs")
        .is_none());
}

#[test]
fn local_desktop_artifacts_are_excluded_from_public_source() {
    let root_ignore = fs::read_to_string(root().join(".gitignore")).unwrap();
    for pattern in [".env.*", ".openraid/", "/.playwright-mcp/"] {
        assert!(root_ignore.lines().any(|line| line == pattern));
    }
    let desktop_ignore = fs::read_to_string(root().join("desktop/.gitignore")).unwrap();
    for pattern in [
        "/dist/",
        "/src-tauri/target/",
        "/src-tauri/gen/",
        "/coverage/",
    ] {
        assert!(desktop_ignore.lines().any(|line| line == pattern));
    }
}

#[test]
fn configured_bundle_icons_exist_with_the_expected_file_headers() {
    let config = json("desktop/src-tauri/tauri.conf.json");
    for icon in config["bundle"]["icon"].as_array().unwrap() {
        let icon = icon.as_str().unwrap();
        let bytes = fs::read(root().join("desktop/src-tauri").join(icon)).unwrap();
        let header: &[u8] = match Path::new(icon).extension().unwrap().to_str().unwrap() {
            "png" => b"\x89PNG\r\n\x1a\n",
            "ico" => b"\x00\x00\x01\x00",
            "icns" => b"icns",
            extension => panic!("unexpected icon format: {extension}"),
        };
        assert!(bytes.starts_with(header), "invalid icon header: {icon}");
    }
}

#[test]
fn desktop_and_terminal_ship_from_the_same_release_workflow() {
    assert!(!root().join(".github/workflows/desktop.yml").exists());
    let workflow = fs::read_to_string(root().join(".github/workflows/release.yml")).unwrap();
    for required in [
        "--verify-tag",
        "Verify default TUI dependency isolation",
        "npm ci --prefix desktop",
        "npm test --prefix desktop",
        "--manifest-path desktop/src-tauri/Cargo.toml",
        "npm run tauri -- build --no-bundle --ci -- --locked",
        "--desktop-binary",
        "python scripts/test-package-release.py",
        "needs: build",
    ] {
        assert!(
            workflow.contains(required),
            "missing release gate: {required}"
        );
    }
    assert_eq!(workflow.matches("desktop_binary:").count(), 4);
}
