//! Thin native IPC adapter. All swarm work remains in the shared Rust runtime.
use anyhow::{ensure, Context, Result};
use openraid::{
    auth::AuthStore, config::Config, desktop::DesktopSession, session::SessionNavigation,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tauri::{Emitter, Manager, State};
use tokio::sync::Mutex;

const SNAPSHOT_EVENT: &str = "openraid://snapshot";

#[derive(Default)]
struct DesktopState {
    session: Mutex<Option<Arc<DesktopSession>>>,
    /// Serialize navigation and commands so a command never targets an old session.
    commands: Mutex<()>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenRequest {
    workspace: PathBuf,
    session: Option<String>,
    database: Option<PathBuf>,
    provider: Option<String>,
    model: Option<String>,
    variant: Option<String>,
    agents: Option<usize>,
    #[serde(default)]
    mock: bool,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum ControlRequest {
    Start {
        text: String,
    },
    Pause {},
    Resume {},
    Stop {},
    Add {
        count: usize,
    },
    Remove {
        #[serde(rename = "agentId")]
        agent_id: String,
    },
    Restore {
        sequence: u64,
    },
    Model {
        provider: String,
        model: String,
        variant: Option<String>,
    },
    Variant {
        variant: Option<String>,
    },
    Theme {
        name: String,
    },
    New {},
    Session {
        id: String,
    },
    Mcp {
        name: String,
    },
    BoardExport {
        path: PathBuf,
    },
}

fn error(error: anyhow::Error) -> String {
    // Context is enough for the operator; nested HTTP/config diagnostics may
    // include provider headers or credential-bearing request URLs.
    error.to_string()
}

async fn current(state: &DesktopState) -> Result<Arc<DesktopSession>> {
    state
        .session
        .lock()
        .await
        .clone()
        .context("Open a workspace first")
}

async fn prepare(request: OpenRequest) -> Result<Config> {
    let auth = AuthStore::load_default()?;
    let entry = request
        .session
        .as_deref()
        .map(openraid::session_catalog::find)
        .transpose()?;
    let workspace = std::fs::canonicalize(
        entry
            .as_ref()
            .map(|entry| &entry.workspace)
            .unwrap_or(&request.workspace),
    )?;
    ensure!(workspace.is_dir(), "Workspace must be a directory");
    let mut config = Config {
        workspace: workspace.clone(),
        base_url: String::new(),
        interactive_session: true,
        no_tui: true,
        ..Config::default()
    };
    let profile = entry
        .as_ref()
        .and_then(|entry| auth.launch_profile_for_session(&entry.database))
        .or_else(|| auth.launch_profile_for(&workspace));
    if let Some(profile) = profile {
        profile.apply(&mut config);
    }
    let remembered_database = profile
        .map(|profile| openraid::config::remembered_database_path(&workspace, &profile.database));
    if let Some(selection) = entry
        .as_ref()
        .and_then(|entry| auth.selection_for_session(&entry.database))
        .or_else(|| auth.selection_for(&workspace))
        .or_else(|| auth.selection())
    {
        config.provider = selection.provider.clone();
        config.model = selection.model.clone();
        config.variant = selection.variant.clone();
    }
    config.workspace = workspace.clone();
    config.database = entry
        .map(|entry| entry.database)
        .or(request.database)
        .or(remembered_database)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            }
        })
        .unwrap_or_else(|| openraid::config::default_database_path(&workspace));
    if config.database.is_file() {
        config.database = std::fs::canonicalize(&config.database)?;
        if let Some(known) = openraid::session_catalog::list(None)?
            .into_iter()
            .find(|known| known.database == config.database)
        {
            ensure!(
                known.workspace == workspace,
                "Session belongs to {}; open it by session ID",
                known.workspace.display()
            );
        }
    }
    config.mock = request.mock;
    config.objective.clear();
    config.resume = false;
    if let Some(agents) = request.agents {
        config.agents = agents;
    }
    if let Some(provider) = request.provider {
        config.provider = provider;
        config.base_url.clear();
    }
    if let Some(model) = request.model {
        config.model = model;
    }
    if let Some(variant) = request.variant {
        config.variant = Some(variant);
    }
    config.resolve_concurrency();
    config.mcp = openraid::mcp::load(&workspace, config.config_path.as_deref())?;
    if !config.mock {
        let mut catalog = openraid::cli::load_catalog(&workspace, config.config_path.as_deref())?;
        refresh_catalog(&mut catalog, &config, &auth).await?;
        openraid::cli::configure_provider_with_auth(&catalog, &mut config, None, &auth)?;
        config.validate()?;
    }
    config.validate()?;
    Ok(config)
}

fn push_events(app: tauri::AppHandle, session: Arc<DesktopSession>) {
    let mut changes = session.subscribe_changes();
    tauri::async_runtime::spawn(async move {
        loop {
            if changes.changed().await.is_err() {
                break;
            }
            // Coalesce token updates while retaining event-driven delivery.
            tokio::time::sleep(Duration::from_millis(80)).await;
            changes.borrow_and_update();
            let is_current = app
                .state::<DesktopState>()
                .session
                .lock()
                .await
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &session));
            if !is_current {
                break;
            }
            match session.snapshot().await {
                Ok(snapshot) => {
                    let _ =
                        emit_current(&app, &app.state::<DesktopState>(), &session, &snapshot).await;
                }
                Err(error) => {
                    let _ = app.emit("openraid://error", error.to_string());
                }
            }
            if session.control().is_closing() {
                break;
            }
        }
    });
}

async fn emit_current(
    app: &tauri::AppHandle,
    state: &DesktopState,
    session: &Arc<DesktopSession>,
    snapshot: &openraid::desktop::DesktopSnapshot,
) -> Result<()> {
    let current = state.session.lock().await;
    if current
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, session))
    {
        app.emit(SNAPSHOT_EVENT, snapshot)?;
    }
    Ok(())
}

async fn replace(app: &tauri::AppHandle, state: &DesktopState, config: Config) -> Result<Value> {
    if let Some(old) = state.session.lock().await.as_ref() {
        ensure!(
            !old.control().is_busy() && !old.control().is_stopping(),
            "Stop the current work before opening a session"
        );
        ensure!(
            old.control().current().config.database != config.database,
            "This session is already open"
        );
    }
    // Construct/validate the destination before abandoning the current session.
    let session = DesktopSession::new(config).await?;
    let active = session.control().current();
    let entry =
        openraid::session_catalog::register(&active.config.workspace, &active.config.database, "")?;
    session.set_session_id(entry.id).await;
    if !active.config.mock {
        let mut auth = AuthStore::load_default()?;
        auth.remember_launch(&active.config);
        auth.save()?;
    }
    let snapshot = session.snapshot().await?;
    if let Some(old) = state.session.lock().await.as_ref() {
        old.control()
            .request_navigation(SessionNavigation::New)
            .await?;
        old.control().take_navigation();
    }
    if let Some(old) = state.session.lock().await.replace(session.clone()) {
        old.shutdown().await?;
    }
    push_events(app.clone(), session.clone());
    emit_current(app, state, &session, &snapshot).await?;
    Ok(serde_json::to_value(snapshot)?)
}

#[tauri::command]
async fn desktop_open(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    request: OpenRequest,
) -> std::result::Result<Value, String> {
    let _guard = state.commands.lock().await;
    // A frontend reload must reattach, not construct a second runtime or reset
    // the active session to whatever launch preferences happen to remember.
    let existing = state.session.lock().await.clone();
    if let Some(session) = existing {
        let config = session.control().current().config.clone();
        if reconnects_to_current(&request, &config).map_err(error)? {
            return serde_json::to_value(session.snapshot().await.map_err(error)?)
                .map_err(|e| e.to_string());
        }
    }
    let config = prepare(request).await.map_err(error)?;
    replace(&app, &state, config).await.map_err(error)
}

fn reconnects_to_current(request: &OpenRequest, config: &Config) -> Result<bool> {
    if request.session.is_some()
        || request.database.is_some()
        || request.provider.is_some()
        || request.model.is_some()
        || request.variant.is_some()
        || request.agents.is_some()
        || request.mock != config.mock
    {
        return Ok(false);
    }
    Ok(std::fs::canonicalize(&request.workspace)? == config.workspace)
}

#[tauri::command]
async fn desktop_snapshot(state: State<'_, DesktopState>) -> std::result::Result<Value, String> {
    let _guard = state.commands.lock().await;
    let Some(session) = state.session.lock().await.clone() else {
        return Ok(Value::Null);
    };
    serde_json::to_value(session.snapshot().await.map_err(error)?)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn desktop_control(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
    request: ControlRequest,
) -> std::result::Result<Value, String> {
    if matches!(
        &request,
        ControlRequest::Stop {} | ControlRequest::Pause {} | ControlRequest::Resume {}
    ) {
        return control(&app, &state, request).await.map_err(error);
    }
    let _guard = state.commands.lock().await;
    control(&app, &state, request).await.map_err(error)
}

async fn control(
    app: &tauri::AppHandle,
    state: &DesktopState,
    request: ControlRequest,
) -> Result<Value> {
    // Appearance is an application preference, not a session operation. It must
    // also work on the welcome screen before a workspace has been opened.
    if let ControlRequest::Theme { name } = &request {
        save_theme(name)?;
        if state.session.lock().await.is_none() {
            return Ok(desktop_themes());
        }
    }
    let session = current(state).await?;
    let controls = session.control();
    match request {
        ControlRequest::Start { text } => {
            controls.post_prompt(text).await?;
        }
        ControlRequest::Pause {} => controls.pause().await?,
        ControlRequest::Resume {} => controls.resume().await?,
        ControlRequest::Stop {} => controls.stop_work().await?,
        ControlRequest::Add { count } => {
            controls.add_agents(count).await?;
        }
        ControlRequest::Remove { agent_id } => controls.remove_agents(vec![agent_id]).await?,
        ControlRequest::Restore { sequence } => {
            let (text, restored) = controls.restore_prompt(sequence).await?;
            return Ok(json!({"text":text,"restored":restored}));
        }
        ControlRequest::Theme { .. } => {}
        ControlRequest::Mcp { name } => controls.mcp.toggle(&name).await?,
        ControlRequest::Model {
            provider,
            model,
            variant,
        } => switch_model(controls, &provider, &model, variant.as_deref()).await?,
        ControlRequest::Variant { variant } => {
            let config = controls.current().config.clone();
            switch_model(
                controls,
                &config.provider,
                &config.model,
                variant.as_deref(),
            )
            .await?;
        }
        ControlRequest::New {} => {
            let mut config = (*controls.current().config).clone();
            config.database = config
                .workspace
                .join(".openraid/sessions")
                .join(format!("{}.sqlite3", openraid::session_catalog::new_id()));
            config.objective.clear();
            config.resume = false;
            return replace(app, state, config).await;
        }
        ControlRequest::Session { id } => {
            let config = prepare(OpenRequest {
                workspace: controls.current().config.workspace.clone(),
                session: Some(id.clone()),
                database: None,
                provider: None,
                model: None,
                variant: None,
                agents: None,
                mock: controls.current().config.mock,
            })
            .await?;
            return replace(app, state, config).await;
        }
        ControlRequest::BoardExport { path } => {
            let path = if path.is_absolute() {
                path
            } else {
                controls.current().config.workspace.join(path)
            };
            let contents = serde_json::to_vec_pretty(&session.store().export_board().await?)?;
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await
                .context("Creating board export (existing files are not overwritten)")?;
            use tokio::io::AsyncWriteExt;
            file.write_all(&contents).await?;
            file.sync_all().await?;
            return Ok(json!({"path":path}));
        }
    }
    let snapshot = session.snapshot().await?;
    emit_current(app, state, &session, &snapshot).await?;
    Ok(serde_json::to_value(snapshot)?)
}

fn save_theme(name: &str) -> Result<()> {
    ensure!(
        openraid::theme::THEMES.iter().any(|theme| theme.id == name),
        "Choose a theme from the appearance settings"
    );
    openraid::theme_preferences::save(name)?;
    openraid::theme::set_theme(name);
    Ok(())
}

async fn switch_model(
    controls: &openraid::session::SessionControl,
    provider: &str,
    model: &str,
    variant: Option<&str>,
) -> Result<()> {
    let active = controls.current();
    let mut catalog = openraid::cli::load_catalog(
        &active.config.workspace,
        active.config.config_path.as_deref(),
    )?;
    let mut auth = AuthStore::load_default()?;
    refresh_catalog(&mut catalog, &active.config, &auth).await?;
    let config = openraid::cli::resolve_live_selection(
        &catalog,
        &active.config,
        provider,
        model,
        variant,
        &auth,
    )?;
    controls.switch(config.clone()).await?;
    auth.remember_launch(&config);
    auth.save()?;
    Ok(())
}

#[tauri::command]
async fn desktop_activity(
    state: State<'_, DesktopState>,
    agent_id: String,
) -> std::result::Result<String, String> {
    current(&state)
        .await
        .map_err(error)?
        .activity(&agent_id)
        .await
        .map_err(error)
}

#[tauri::command]
async fn desktop_board(
    state: State<'_, DesktopState>,
    after: u64,
    limit: usize,
) -> std::result::Result<Value, String> {
    let session = current(&state).await.map_err(error)?;
    let board = session
        .store()
        .read_board(after, limit.clamp(1, 1000))
        .await
        .map_err(error)?;
    serde_json::to_value(board).map_err(|error| error.to_string())
}

#[tauri::command]
async fn desktop_activity_tail(
    state: State<'_, DesktopState>,
    agent_id: String,
    max_bytes: usize,
) -> std::result::Result<String, String> {
    current(&state)
        .await
        .map_err(error)?
        .activity_tail(&agent_id, max_bytes)
        .await
        .map_err(error)
}

async fn refresh_catalog(
    catalog: &mut openraid::catalog::Catalog,
    config: &Config,
    auth: &AuthStore,
) -> Result<()> {
    if openraid::catalog::is_codex_provider(&config.provider) {
        openraid::cli::refresh_codex_models(catalog, config, auth).await?;
    } else if config.provider == "gitlab" {
        let _ = openraid::cli::refresh_gitlab_catalog(catalog, &config.workspace, None, None, auth)
            .await;
    }
    Ok(())
}

#[tauri::command]
async fn desktop_catalog(state: State<'_, DesktopState>) -> std::result::Result<Value, String> {
    let session = current(&state).await.map_err(error)?;
    let active = session.control().current();
    let mut catalog = openraid::cli::load_catalog(
        &active.config.workspace,
        active.config.config_path.as_deref(),
    )
    .map_err(error)?;
    let auth = AuthStore::load_default().map_err(error)?;
    refresh_catalog(&mut catalog, &active.config, &auth)
        .await
        .map_err(error)?;
    // Explicit allowlist: catalog provider options can contain configured secrets.
    let mut providers = Vec::new();
    for provider in catalog.search_providers("") {
        let connected = provider_connected(provider, &auth)
            || (provider.id == active.config.provider
                && (active.config.oauth.is_some()
                    || active
                        .config
                        .api_key
                        .as_deref()
                        .is_some_and(|key| key != "public")));
        let public = provider.has_public_models() || provider.env.is_empty();
        let authentication = if connected {
            "connected"
        } else if public {
            "not_required"
        } else {
            "missing"
        };
        let auth_hint = match authentication {
            "connected" => "Credentials are available from your shared configuration.".to_owned(),
            "not_required" => "Public models or provider-managed authentication are available.".to_owned(),
            _ => "Connect this provider in OpenRaid authentication settings or set its credential environment variable. Shared OpenRaid and OpenCode credentials are recognized.".to_owned(),
        };
        let models = catalog.search_models(&provider.id, "").iter().map(|model| json!({
            "id": model.id,
            "name": model.name,
            "available": provider.model_available(model, connected),
            "variants": openraid::variants::ordered_names(&openraid::cli::effective_variants(model, active.config.max_output_tokens as usize, &provider.npm))
        })).collect::<Vec<_>>();
        providers.push(json!({
            "id": provider.id,
            "name": provider.name,
            "configured": connected || public,
            "authentication": authentication,
            "authHint": auth_hint,
            "credentialEnv": provider.env,
            "models": models
        }));
    }
    Ok(json!(providers))
}

fn provider_connected(provider: &openraid::catalog::ProviderInfo, auth: &AuthStore) -> bool {
    auth.api_key(&provider.id, &provider.env)
        .ok()
        .flatten()
        .is_some_and(|key| !key.trim().is_empty() && key != "public")
        || auth.oauth(&provider.id).ok().flatten().is_some()
        || provider.options["apiKey"]
            .as_str()
            .is_some_and(|key| !key.trim().is_empty())
        || provider.options["token"]
            .as_str()
            .is_some_and(|key| !key.trim().is_empty())
}

#[tauri::command]
fn desktop_sessions() -> std::result::Result<Value, String> {
    serde_json::to_value(openraid::session_catalog::list(None).map_err(error)?)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn desktop_themes() -> Value {
    json!({"active":openraid::theme::current_theme().id,"themes":openraid::theme::THEMES.iter().map(|theme| json!({"id":theme.id,"name":theme.name,"dark":theme.dark,"palette":{"background":theme.palette.background.to_string(),"surface":theme.palette.surface.to_string(),"text":theme.palette.text.to_string(),"muted":theme.palette.muted.to_string(),"accent":theme.palette.accent.to_string(),"border":theme.palette.border.to_string()}})).collect::<Vec<_>>()})
}

#[tauri::command]
async fn desktop_mcp(state: State<'_, DesktopState>) -> std::result::Result<Value, String> {
    Ok(json!(current(&state)
        .await
        .map_err(error)?
        .control()
        .mcp
        .statuses()
        .iter()
        .map(|(name, status)| json!({"name":name,"status":status}))
        .collect::<Vec<_>>()))
}

async fn shutdown(state: &DesktopState) -> Result<()> {
    let active = state.session.lock().await.clone();
    if let Some(session) = active {
        session.control().stop_work().await?;
    }
    let _guard = state.commands.lock().await;
    let session = state.session.lock().await.take();
    if let Some(session) = session {
        session.control().stop_work().await?;
        session.shutdown().await?;
    }
    Ok(())
}

#[tauri::command]
async fn desktop_close(
    app: tauri::AppHandle,
    state: State<'_, DesktopState>,
) -> std::result::Result<(), String> {
    shutdown(&state).await.map_err(error)?;
    app.exit(0);
    Ok(())
}

pub fn run() {
    let _ = openraid::theme_preferences::initialize();
    tauri::Builder::default()
        .manage(DesktopState::default())
        .invoke_handler(tauri::generate_handler![
            desktop_open,
            desktop_snapshot,
            desktop_control,
            desktop_activity,
            desktop_activity_tail,
            desktop_board,
            desktop_catalog,
            desktop_sessions,
            desktop_themes,
            desktop_mcp,
            desktop_close
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let app = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    match shutdown(&app.state::<DesktopState>()).await {
                        Ok(()) => app.exit(0),
                        Err(error) => {
                            let _ = app.emit("openraid://error", error.to_string());
                        }
                    }
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("Unable to launch Openraid desktop");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopening_the_current_workspace_reattaches_without_replacing_runtime() {
        let workspace = std::fs::canonicalize(".").unwrap();
        let config = Config {
            workspace: workspace.clone(),
            mock: true,
            ..Config::default()
        };
        let request: OpenRequest =
            serde_json::from_value(json!({"workspace":workspace.join("."),"mock":true})).unwrap();
        assert!(reconnects_to_current(&request, &config).unwrap());
        for overrides in [
            json!({"mock":false}),
            json!({"session":"other-session"}),
            json!({"database":"other.sqlite3"}),
            json!({"provider":"other-provider"}),
            json!({"model":"other-model"}),
            json!({"variant":"high"}),
            json!({"agents":10}),
            json!({"workspace":workspace.parent().unwrap()}),
        ] {
            let mut value = json!({"workspace":workspace,"mock":true});
            for (key, value_override) in overrides.as_object().unwrap() {
                value[key] = value_override.clone();
            }
            let request = serde_json::from_value(value).unwrap();
            assert!(!reconnects_to_current(&request, &config).unwrap());
        }
    }

    #[test]
    fn ipc_controls_are_typed_and_reject_unknown_fields() {
        assert!(serde_json::from_value::<ControlRequest>(json!({"action":"pause"})).is_ok());
        assert!(serde_json::from_value::<ControlRequest>(
            json!({"action":"pause", "command":"rm"})
        )
        .is_err());
        assert!(serde_json::from_value::<ControlRequest>(
            json!({"action":"shell", "command":"echo"})
        )
        .is_err());
        assert!(
            serde_json::from_value::<ControlRequest>(json!({"action":"add", "count":-1})).is_err()
        );
    }

    #[test]
    fn launch_does_not_accept_frontend_credentials() {
        assert!(
            serde_json::from_value::<OpenRequest>(json!({"workspace":".","apiKey":"secret"}))
                .is_err()
        );
        let request: OpenRequest = serde_json::from_value(json!({"workspace":"."})).unwrap();
        assert!(!request.mock);
        assert!(request.session.is_none());
    }

    #[test]
    fn controls_accept_documented_camel_case_agent_id() {
        let request: ControlRequest =
            serde_json::from_value(json!({"action":"remove","agentId":"agent-001"})).unwrap();
        assert!(matches!(request, ControlRequest::Remove { agent_id } if agent_id == "agent-001"));
    }

    #[test]
    fn theme_dto_uses_shared_css_rgb_palettes() {
        let value = desktop_themes();
        for theme in value["themes"].as_array().unwrap() {
            for color in theme["palette"].as_object().unwrap().values() {
                let color = color.as_str().unwrap();
                assert!(color.starts_with('#') && color.len() == 7);
            }
        }
    }

    #[test]
    fn theme_dto_includes_dark_and_light_desktop_presets() {
        let value = desktop_themes();
        let themes = value["themes"].as_array().unwrap();
        for (id, name, dark, background) in [
            ("dark", "Dark", true, "#101116"),
            ("light", "Light", false, "#F1F3F8"),
        ] {
            let theme = themes.iter().find(|theme| theme["id"] == id).unwrap();
            assert_eq!(theme["name"], name);
            assert_eq!(theme["dark"], dark);
            assert_eq!(
                theme["palette"]["background"]
                    .as_str()
                    .unwrap()
                    .to_lowercase(),
                background.to_lowercase()
            );
        }
    }
}
