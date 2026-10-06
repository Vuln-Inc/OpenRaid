//! CLI entry point and shared provider launch resolution.
//!
//! The optional desktop host calls the same catalog, credential, model, and
//! thinking-variant helpers directly; it never invokes the terminal executable.
use crate::{
    auth::AuthStore,
    catalog::{is_codex_provider, Catalog},
    config::{default_database_path, Config},
    provider::Protocol,
    runtime::Harness,
    setup::{Choice, SetupUi},
    storage::Store,
    tui,
};
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::IsTerminal, path::PathBuf, time::Duration};
use tokio::sync::watch;

#[derive(Parser)]
#[command(version, about = "Native single-process collaborative agent swarm")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run a swarm with a configured provider, model and thinking variant.
    Run(RunArgs),
    /// Search providers, models and thinking variants, then launch a swarm.
    Setup(RunArgs),
    /// Browse the complete OpenCode/models.dev provider catalog.
    Providers {
        query: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Browse models and supported thinking variants for a provider.
    Models {
        provider: String,
        query: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long, hide_env_values = true)]
        api_key: Option<String>,
    },
    /// Connect, inspect or disconnect saved provider credentials.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Exercise real board, tool and consensus mechanics entirely offline.
    Demo(RunArgs),
    /// List saved sessions in the current workspace, or across all workspaces.
    Sessions {
        #[arg(long)]
        workspace: Option<PathBuf>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Read the durable, unfiltered global board by sequence cursor.
    Board {
        /// Defaults to .openraid/openraid.sqlite3.
        #[arg(long)]
        database: Option<PathBuf>,
        #[arg(long, default_value_t = 0)]
        after: u64,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Inject an authenticated owner message and revoke stale completion votes.
    Post {
        body: String,
        /// Defaults to .openraid/openraid.sqlite3.
        #[arg(long)]
        database: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    List,
    /// Sign in to ChatGPT/Codex or GitHub Copilot with a browser code.
    Login {
        provider: String,
        #[arg(long)]
        enterprise_url: Option<String>,
    },
    Connect {
        provider: String,
        #[arg(long, hide_env_values = true)]
        api_key: Option<String>,
    },
    Disconnect {
        provider: String,
    },
}

#[derive(Args)]
struct RunArgs {
    /// Shared objective. All agents start with the same instructions.
    #[arg(conflicts_with = "objective_file")]
    objective: Option<String>,
    #[arg(long)]
    objective_file: Option<PathBuf>,
    #[arg(long)]
    agents: Option<usize>,
    #[arg(long)]
    workspace: Option<PathBuf>,
    /// Relative paths are resolved inside the selected workspace.
    #[arg(long)]
    database: Option<PathBuf>,
    /// Start a separate session with a fresh database in this workspace.
    #[arg(long = "new", conflicts_with_all = ["session", "resume", "database"])]
    new_session: bool,
    /// Open a saved session by ID, including one from another workspace.
    #[arg(long, conflicts_with = "database")]
    session: Option<String>,
    #[arg(long, env = "OPENRAID_PROVIDER")]
    provider: Option<String>,
    #[arg(long, env = "OPENRAID_MODEL")]
    model: Option<String>,
    #[arg(long, env = "OPENRAID_VARIANT")]
    variant: Option<String>,
    #[arg(long, env = "OPENRAID_BASE_URL")]
    base_url: Option<String>,
    /// Prefer the environment variable to passing a secret on the command line.
    #[arg(long, env = "OPENRAID_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    /// Override protocol: chat, responses, anthropic, gemini, or sdk.
    #[arg(long)]
    protocol: Option<String>,
    /// Additional provider options as a JSON object (OpenCode SDK shape).
    #[arg(long, default_value = "{}")]
    provider_options: String,
    /// Additional HTTP header as NAME=VALUE; may be repeated.
    #[arg(long = "header")]
    headers: Vec<String>,
    /// Import custom providers/models/variants from an OpenCode-shaped JSON/JSONC config.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Open the interactive provider/model/thinking selection before running.
    #[arg(long)]
    select: bool,
    /// Override provider concurrency (defaults to max(32, agents)).
    #[arg(long)]
    max_in_flight: Option<usize>,
    /// Override command concurrency (defaults to max(4, agents)).
    #[arg(long)]
    max_processes: Option<usize>,
    /// Override the context budget (defaults to the model's advertised context window).
    #[arg(long)]
    context_budget: Option<usize>,
    /// Override the output reserve (defaults to the model's supported output limit).
    #[arg(long)]
    max_output_tokens: Option<u32>,
    /// Stability grace after 75% completion consensus. Never an operation timeout.
    #[arg(long, default_value_t = 5.0)]
    grace_secs: f64,
    #[arg(long)]
    no_tui: bool,
    #[arg(long)]
    resume: bool,
}

impl RunArgs {
    async fn config(self, mock: bool, force_setup: bool) -> Result<Option<Config>> {
        let auth = AuthStore::load_default()?;
        self.config_with_auth(mock, force_setup, auth).await
    }

    async fn config_with_auth(
        self,
        mock: bool,
        force_setup: bool,
        mut auth: AuthStore,
    ) -> Result<Option<Config>> {
        let requested_session = self
            .session
            .as_deref()
            .map(openraid::session_catalog::find)
            .transpose()?;
        let selected_workspace = self
            .workspace
            .as_deref()
            .or_else(|| {
                requested_session
                    .as_ref()
                    .map(|entry| entry.workspace.as_path())
            })
            .unwrap_or(std::path::Path::new("."));
        let workspace = tokio::fs::canonicalize(selected_workspace)
            .await
            .with_context(|| format!("opening workspace {}", selected_workspace.display()))?;
        if !mock
            && !self.no_tui
            && std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && auth.opencode_import_consent().is_none()
            && (auth.opencode_import_available()
                || !Catalog::opencode_config_paths(&workspace).is_empty())
        {
            let mut ui = SetupUi::new()?;
            let choices = [
                Choice::new(
                    "skip",
                    "Keep OpenRaid separate",
                    "You can import later with /import-opencode",
                ),
                Choice::new(
                    "import",
                    "Use existing OpenCode information",
                    "Use saved API keys, OAuth accounts and provider configuration",
                ),
            ];
            let Some(choice) = ui.pick(
                "Existing OpenCode information found",
                "Should OpenRaid use your OpenCode authentication and providers? Your choice is remembered.",
                &choices,
                Some("skip"),
            )? else {
                return Ok(None);
            };
            auth.set_opencode_import_consent(choice == "import")?;
        }
        if let Some(entry) = &requested_session {
            if entry.workspace != workspace {
                bail!(
                    "session {} belongs to {}; omit --workspace to open it",
                    entry.id,
                    entry.workspace.display()
                );
            }
        }
        let resume_home = (force_setup || requested_session.is_some())
            && !self.select
            && auth.selection_for(&workspace).is_some();
        let mut profile = resume_home
            .then(|| auth.launch_profile_for(&workspace).cloned())
            .flatten();
        let explicit_selection =
            self.provider.is_some() || self.model.is_some() || self.variant.is_some();
        if !self.grace_secs.is_finite() || self.grace_secs < 0.0 {
            bail!("consensus grace must be a finite nonnegative number");
        }
        let grace_period = Duration::try_from_secs_f64(self.grace_secs)
            .context("consensus grace is out of range")?;
        let interactive = !mock && (force_setup || self.select || requested_session.is_some());
        let explicit_context_budget = self.context_budget.is_some();
        let explicit_max_output_tokens = self.max_output_tokens.is_some();
        let objective = match (self.objective, self.objective_file) {
            (Some(text), _) => text,
            (_, Some(path)) => tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("reading objective {}", path.display()))?,
            _ if mock => "Verify every swarm member can collaborate on the global board and agree on completion.".into(),
            _ if interactive => String::new(),
            _ if requested_session.is_some() => String::new(),
            _ => bail!("provide an objective or --objective-file, or run openraid setup"),
        };
        let latest = if force_setup
            && !self.new_session
            && requested_session.is_none()
            && self.database.is_none()
        {
            openraid::session_catalog::list(Some(&workspace))?
                .into_iter()
                .next()
        } else {
            None
        };
        let database = self
            .database
            .or_else(|| {
                requested_session
                    .as_ref()
                    .map(|entry| entry.database.clone())
            })
            .or_else(|| {
                latest.map(|entry| {
                    openraid::config::remembered_database_path(&workspace, &entry.database)
                })
            })
            .unwrap_or_else(|| {
                if self.new_session {
                    return workspace
                        .join(".openraid")
                        .join("sessions")
                        .join(format!("{}.sqlite3", openraid::session_catalog::new_id()));
                }
                profile
                    .as_ref()
                    .filter(|profile| profile.workspace == workspace)
                    .map(|profile| {
                        openraid::config::remembered_database_path(&workspace, &profile.database)
                    })
                    .unwrap_or_else(|| default_database_path(&workspace))
            });
        let database = if database.is_absolute() {
            database
        } else {
            workspace.join(database)
        };
        let database = if database.is_file() {
            std::fs::canonicalize(&database)?
        } else {
            database
        };
        ensure_session_workspace(&workspace, &database)?;
        if resume_home && !self.new_session {
            if let Some(session_profile) = auth.launch_profile_for_session(&database) {
                profile = Some(session_profile.clone());
            }
        }
        let config_path = self
            .config
            .or_else(|| {
                profile
                    .as_ref()
                    .and_then(|profile| profile.config_path.clone())
            })
            .map(std::fs::canonicalize)
            .transpose()
            .context("opening provider configuration")?;
        let mut catalog = load_catalog(&workspace, config_path.as_deref())?;
        let mcp = if mock {
            BTreeMap::new()
        } else {
            openraid::mcp::load(&workspace, config_path.as_deref())?
        };
        let saved = if self.new_session {
            None
        } else {
            auth.selection_for_session(&database)
        }
        .or_else(|| auth.selection_for(&workspace));
        let qualified = self
            .model
            .as_deref()
            .and_then(|value| value.split_once('/'))
            .filter(|(provider, _)| catalog.provider(provider).is_some());
        let inferred_provider = qualified
            .filter(|_| self.provider.is_none())
            .map(|(provider, _)| provider.to_owned());
        let selected_model = if self.provider.is_none() && inferred_provider.is_some() {
            qualified.map(|(_, model)| model.to_owned())
        } else {
            self.model.clone()
        };
        let provider = self
            .provider
            .or(inferred_provider)
            .or_else(|| saved.map(|s| s.provider.clone()))
            .unwrap_or_else(|| "openai".into());
        let model_is_selected = selected_model.is_some()
            || saved.is_some_and(|selection| selection.provider == provider);
        let model = selected_model
            .or_else(|| {
                saved
                    .filter(|s| s.provider == provider)
                    .map(|s| s.model.clone())
            })
            .unwrap_or_else(|| default_model(&catalog, &provider));
        let variant = self
            .variant
            .or_else(|| {
                saved
                    .filter(|s| s.provider == provider && s.model == model)
                    .and_then(|s| s.variant.clone())
            })
            .filter(|value| value != "default");
        let provider_options: Value = serde_json::from_str(&self.provider_options)
            .context("--provider-options must be a JSON object")?;
        if !provider_options.is_object() {
            bail!("--provider-options must be a JSON object");
        }
        let provider_headers = parse_headers(&self.headers)?;
        let mut config = Config {
            agents: self
                .agents
                .or_else(|| profile.as_ref().map(|profile| profile.agents))
                .unwrap_or(8),
            workspace,
            database,
            objective,
            mock,
            model,
            base_url: self.base_url.clone().unwrap_or_default(),
            api_key: self.api_key,
            provider,
            variant,
            provider_options,
            provider_headers,
            max_in_flight: self
                .max_in_flight
                .unwrap_or_else(|| Config::default().max_in_flight),
            max_processes: self
                .max_processes
                .unwrap_or_else(|| Config::default().max_processes),
            explicit_max_in_flight: self.max_in_flight.is_some(),
            explicit_max_processes: self.max_processes.is_some(),
            context_budget: self.context_budget.unwrap_or(32_000),
            max_output_tokens: self
                .max_output_tokens
                .unwrap_or_else(|| Config::default().max_output_tokens),
            grace_period,
            no_tui: self.no_tui
                || !std::io::stdout().is_terminal()
                || !std::io::stdin().is_terminal(),
            resume: self.resume,
            interactive_session: interactive
                && !self.no_tui
                && std::io::stdin().is_terminal()
                && std::io::stdout().is_terminal(),
            explicit_context_budget,
            explicit_max_output_tokens,
            config_path,
            mcp,
            ..Config::default()
        };
        if resume_home {
            if let Some(profile) = profile.as_ref().filter(|_| !explicit_selection) {
                let workspace = config.workspace.clone();
                let database = config.database.clone();
                let agents = config.agents;
                let config_path = config.config_path.clone();
                let api_key = config.api_key.clone();
                profile.apply(&mut config);
                config.workspace = workspace;
                config.database = database;
                config.agents = agents;
                config.config_path = config_path;
                if let Some(endpoint) = &self.base_url {
                    config.base_url = endpoint.clone();
                }
                if let Some(budget) = self.context_budget {
                    config.context_budget = budget;
                    config.explicit_context_budget = true;
                }
                if let Some(budget) = self.max_output_tokens {
                    config.max_output_tokens = budget;
                    config.explicit_max_output_tokens = true;
                }
                if let Some(limit) = self.max_in_flight {
                    config.max_in_flight = limit;
                    config.explicit_max_in_flight = true;
                }
                if let Some(limit) = self.max_processes {
                    config.max_processes = limit;
                    config.explicit_max_processes = true;
                }
                let info = catalog.provider(&config.provider);
                let resolved_key = match api_key {
                    Some(key) => Ok(Some(key)),
                    None => auth.api_key(
                        &config.provider,
                        info.map(|p| p.env.as_slice()).unwrap_or(&[]),
                    ),
                };
                match resolved_key {
                    Ok(key) => {
                        config.api_key = key.or_else(|| {
                            info.and_then(|p| p.options["apiKey"].as_str().map(str::to_owned))
                        })
                    }
                    Err(_) => {
                        config.oauth =
                            openraid::oauth::OAuthSession::from_store(&config.provider, &auth)?;
                        if let Some(session) = &config.oauth {
                            let (protocol, endpoint) = session.protocol_and_base_url(
                                config.api_model.as_deref().unwrap_or(&config.model),
                            );
                            config.protocol = protocol;
                            config.base_url = endpoint;
                        }
                    }
                }
                use_model_context_limit(&catalog, &mut config);
                let output_limit = catalog
                    .model(&config.provider, &config.model)
                    .map(|model| model.limit.output);
                config.use_model_output_limit(output_limit);
                adjust_thinking_budget(&mut config, output_limit.unwrap_or(0))?;
            } else {
                if is_codex_provider(&config.provider) {
                    refresh_codex_models(&mut catalog, &config, &auth).await?;
                }
                configure_provider(&catalog, &mut config, self.protocol.as_deref())?;
            }
        } else if interactive {
            if !wizard(catalog, &mut config, self.protocol.as_deref()).await? {
                return Ok(None);
            }
        } else if !mock {
            if config.provider == "gitlab" {
                let endpoint = (!config.base_url.is_empty()).then_some(config.base_url.as_str());
                let _ = refresh_gitlab_catalog(
                    &mut catalog,
                    &config.workspace,
                    endpoint,
                    config.api_key.as_deref(),
                    &auth,
                )
                .await;
            }
            if is_codex_provider(&config.provider) {
                refresh_codex_models(&mut catalog, &config, &auth).await?;
                if !model_is_selected {
                    config.model = default_model(&catalog, &config.provider);
                }
                if catalog.model(&config.provider, &config.model).is_none() {
                    bail!("model {} is not advertised by your Codex LB server; run openraid models codex-lb --base-url {}", config.model, public_endpoint(&config.base_url));
                }
            }
            configure_provider(&catalog, &mut config, self.protocol.as_deref())?;
        }
        config.resolve_concurrency();
        config.validate()?;
        enable_remembered_recovery(
            &mut config,
            (resume_home || requested_session.is_some()) && !self.new_session,
        )
        .await?;
        Ok(Some(config))
    }
}

async fn enable_remembered_recovery(config: &mut Config, remembered_home: bool) -> Result<()> {
    if remembered_home
        // Opening the console is not permission to restart unfinished work.
        // Explicit --resume is already reflected in config.resume.
        && !config.interactive_session
        && !config.resume
        && config.objective.trim().is_empty()
        && config.database.is_file()
    {
        let store = Store::open(&config.database).await?;
        config.resume = store.unfinished_prompt().await?.is_some();
    }
    Ok(())
}

fn ensure_session_workspace(workspace: &std::path::Path, database: &std::path::Path) -> Result<()> {
    if !database.is_file() {
        return Ok(());
    }
    let database = std::fs::canonicalize(database)?;
    if let Some(entry) = openraid::session_catalog::list(None)?
        .into_iter()
        .find(|entry| entry.database == database && entry.workspace != workspace)
    {
        bail!(
            "this session belongs to {}; open it with --session {}",
            entry.workspace.display(),
            entry.id
        );
    }
    Ok(())
}

pub fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    if let Err(error) = openraid::theme_preferences::initialize() {
        eprintln!("Could not load saved theme; using Openraid: {error:#}");
    }
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(8);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .thread_name("openraid-worker")
        .enable_all()
        .build()
        .context("building shared Tokio runtime")?
        .block_on(dispatch(cli))
}

async fn dispatch(cli: Cli) -> Result<()> {
    let command = match cli.command {
        Some(command) => command,
        None => Cli::parse_from(["openraid", "setup"])
            .command
            .expect("setup subcommand"),
    };
    match command {
        Command::Run(args) => launch(args.config(false, false).await?).await,
        Command::Setup(args) => launch(args.config(false, true).await?).await,
        Command::Demo(args) => launch(args.config(true, false).await?).await,
        Command::Sessions {
            workspace,
            all,
            json: as_json,
        } => {
            let workspace = std::fs::canonicalize(workspace.unwrap_or_else(|| PathBuf::from(".")))
                .context("opening workspace")?;
            let sessions = openraid::session_catalog::list((!all).then_some(workspace.as_path()))?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
            } else if sessions.is_empty() {
                println!(
                    "No saved sessions{}. Start one with openraid setup or openraid setup --new.",
                    if all {
                        String::new()
                    } else {
                        format!(" in {}", workspace.display())
                    }
                );
            } else {
                for entry in sessions {
                    println!(
                        "{}  {}\n  {}\n  {}",
                        entry.id,
                        entry.title,
                        entry.workspace.display(),
                        entry.database.display()
                    );
                }
                println!("\nOpen a session: openraid setup --session ID");
            }
            Ok(())
        }
        Command::Providers {
            query,
            json: as_json,
        } => {
            let catalog = load_catalog(&std::env::current_dir()?, None)?;
            let providers = catalog.search_providers(query.as_deref().unwrap_or(""));
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&redact_json(serde_json::to_value(&providers)?))?
                );
            } else {
                println!("Provider                      Models  Name");
                for provider in providers {
                    println!(
                        "{:<29} {:>6}  {}",
                        provider.id,
                        provider.models.len(),
                        provider.name
                    );
                }
                println!("\nChoose with: openraid setup   |   Inspect: openraid models PROVIDER");
            }
            Ok(())
        }
        Command::Models {
            provider,
            query,
            json: as_json,
            refresh,
            base_url,
            api_key,
        } => {
            let mut catalog = load_catalog(&std::env::current_dir()?, None)?;
            let info = catalog
                .provider(&provider)
                .with_context(|| format!("unknown provider {provider}; run openraid providers"))?;
            if provider == "gitlab" {
                let result = refresh_gitlab_catalog(
                    &mut catalog,
                    &std::env::current_dir()?,
                    base_url.as_deref(),
                    api_key.as_deref(),
                    &AuthStore::load_default()?,
                )
                .await;
                if refresh {
                    result?;
                }
            } else if refresh || is_codex_provider(&provider) {
                let base = base_url.as_deref().unwrap_or(&info.api).to_owned();
                let explicit_key = api_key
                    .as_deref()
                    .or_else(|| info.options["apiKey"].as_str());
                let key = openraid::auth::resolve_api_key(&provider, &info.env, explicit_key)?;
                catalog
                    .refresh_models(&provider, &base, key.as_deref())
                    .await?;
            }
            let models = catalog.search_models(&provider, query.as_deref().unwrap_or(""));
            if as_json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&redact_json(serde_json::to_value(&models)?))?
                );
            } else {
                for model in models {
                    let variants = openraid::variants::ordered_names(&model.variants).join(", ");
                    println!(
                        "{}\n  {}  |  context {}  output {}  |  thinking: {}",
                        model.id,
                        model.name,
                        model.limit.context,
                        model.limit.output,
                        if variants.is_empty() {
                            "default"
                        } else {
                            &variants
                        }
                    );
                }
            }
            Ok(())
        }
        Command::Auth { command } => auth_command(command).await,
        Command::Board {
            database,
            after,
            limit,
        } => {
            let database =
                database.unwrap_or_else(|| default_database_path(std::path::Path::new(".")));
            let store = Store::open(database).await?;
            let messages = store.read_board(after, limit).await?;
            println!("{}", serde_json::to_string_pretty(&messages)?);
            Ok(())
        }
        Command::Post { body, database } => {
            if body.trim().is_empty() {
                bail!("owner message must not be empty");
            }
            let database =
                database.unwrap_or_else(|| default_database_path(std::path::Path::new(".")));
            let store = Store::open(database).await?;
            let message = store.append("owner", &body, true).await?;
            println!("{}", serde_json::to_string_pretty(&message)?);
            Ok(())
        }
    }
}

async fn launch(config: Option<Config>) -> Result<()> {
    let Some(mut config) = config else {
        println!("Setup cancelled.");
        return Ok(());
    };
    loop {
        match run(config).await? {
            None => return Ok(()),
            Some((openraid::session::SessionNavigation::New, active)) => {
                config = active;
                config.database = config
                    .workspace
                    .join(".openraid")
                    .join("sessions")
                    .join(format!("{}.sqlite3", openraid::session_catalog::new_id()));
                config.objective.clear();
                config.resume = false;
                config.interactive_session = true;
            }
            Some((openraid::session::SessionNavigation::Open(id), _)) => {
                let cli = Cli::try_parse_from(["openraid", "setup", "--session", &id])?;
                let Some(Command::Setup(args)) = cli.command else {
                    unreachable!()
                };
                let Some(next) = args.config(false, true).await? else {
                    println!("Setup cancelled.");
                    return Ok(());
                };
                config = next;
            }
        }
    }
}

async fn auth_command(command: AuthCommand) -> Result<()> {
    let mut auth = AuthStore::load_default()?;
    match command {
        AuthCommand::List => {
            let catalog = load_catalog(&std::env::current_dir()?, None)?;
            let providers: std::collections::BTreeSet<_> = catalog
                .providers
                .keys()
                .map(String::as_str)
                .chain(auth.connected_providers())
                .collect();
            for id in providers {
                let provider = catalog.provider(id);
                if provider.is_some_and(|provider| {
                    provider.options["apiKey"]
                        .as_str()
                        .is_some_and(|key| !key.trim().is_empty())
                }) {
                    println!("{id}  connected");
                    continue;
                }
                match auth.api_key(
                    id,
                    provider
                        .map(|provider| provider.env.as_slice())
                        .unwrap_or(&[]),
                ) {
                    Ok(Some(_)) => println!("{id}  connected"),
                    Err(error) => match openraid::oauth::OAuthSession::from_store(id, &auth) {
                        Ok(Some(_)) => println!("{id}  connected OAuth account"),
                        _ => println!("{id}  {error}"),
                    },
                    _ => {}
                }
            }
            println!("Credential file: {}", auth.path().display());
        }
        AuthCommand::Login {
            provider,
            enterprise_url,
        } => {
            let login = openraid::oauth::begin_login(&provider, enterprise_url.as_deref()).await?;
            println!(
                "Open {}\nEnter code: {}\nWaiting for sign-in; Ctrl+C cancels.",
                login.verification_uri, login.user_code
            );
            let credential = tokio::select! {
                result = login.wait() => result?,
                _ = tokio::signal::ctrl_c() => { println!("Sign-in cancelled."); return Ok(()); },
            };
            auth.set_oauth(&provider, &credential)?;
            auth.save()?;
            println!("Connected {provider} account.");
        }
        AuthCommand::Connect { provider, api_key } => {
            let key = match api_key {
                Some(key) => key,
                None => {
                    let mut ui = SetupUi::new()?;
                    match ui.input(&format!("Connect {provider}"), "Paste your API key. It will be saved privately; never displayed in the dashboard.", "", true, false)? {
                        Some(key) => key,
                        None => return Ok(()),
                    }
                }
            };
            auth.set_api_key(&provider, &key)?;
            auth.save()?;
            println!("Connected {provider}.");
        }
        AuthCommand::Disconnect { provider } => {
            if auth.remove_api_key(&provider) {
                auth.save()?;
                println!("Removed saved credentials for {provider}.");
            } else {
                println!("No OpenRaid credential saved for {provider}.");
            }
        }
    }
    Ok(())
}

fn parse_headers(headers: &[String]) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for header in headers {
        let (name, value) = header
            .split_once('=')
            .context("--header must use NAME=VALUE")?;
        if name.trim().is_empty() {
            bail!("header name must not be empty");
        }
        set_header(&mut result, name.trim(), value.trim());
    }
    Ok(result)
}

fn set_header(headers: &mut BTreeMap<String, String>, name: &str, value: &str) {
    headers.retain(|key, _| !key.eq_ignore_ascii_case(name));
    headers.insert(name.to_ascii_lowercase(), value.to_owned());
}

pub fn default_model(catalog: &Catalog, provider: &str) -> String {
    if is_codex_provider(provider) {
        return catalog
            .provider(provider)
            .and_then(|provider| provider.models.values().find(|model| model.tool_call))
            .map(|model| model.id.clone())
            .unwrap_or_default();
    }
    let oauth_openai = provider == "openai"
        && AuthStore::load_default().ok().is_some_and(|auth| {
            catalog
                .provider(provider)
                .is_some_and(|p| auth.api_key(provider, &p.env).is_err())
                && auth.oauth(provider).ok().flatten().is_some()
        });
    let preferred = if oauth_openai {
        "gpt-5.4"
    } else {
        "gpt-4.1-mini"
    };
    if catalog.model(provider, preferred).is_some() {
        return preferred.into();
    }
    catalog
        .provider(provider)
        .and_then(|p| p.models.values().find(|m| m.tool_call))
        .map(|m| m.id.clone())
        .unwrap_or_else(|| preferred.into())
}

pub fn load_catalog(
    workspace: &std::path::Path,
    explicit: Option<&std::path::Path>,
) -> Result<Catalog> {
    Catalog::load(workspace, explicit)
}

fn parse_protocol(value: &str) -> Result<Protocol> {
    match value {
        "chat" | "chat-completions" => Ok(Protocol::ChatCompletions),
        "responses" => Ok(Protocol::Responses),
        "anthropic" | "messages" => Ok(Protocol::Anthropic),
        "gemini" | "google" => Ok(Protocol::Gemini),
        "sdk" => Ok(Protocol::Sdk),
        _ => bail!("unknown protocol {value}; choose chat, responses, anthropic, gemini, or sdk"),
    }
}

pub async fn refresh_gitlab_catalog(
    catalog: &mut Catalog,
    workspace: &std::path::Path,
    explicit_endpoint: Option<&str>,
    explicit_key: Option<&str>,
    auth: &AuthStore,
) -> Result<usize> {
    let info = catalog
        .provider("gitlab")
        .context("GitLab provider missing")?;
    let configured_key = info.options["apiKey"]
        .as_str()
        .filter(|key| !key.is_empty());
    let resolution = explicit_key
        .or(configured_key)
        .map(|key| Some(key.to_owned()))
        .map(Ok)
        .unwrap_or_else(|| auth.api_key("gitlab", &info.env));
    let (key, oauth) = match resolution {
        Ok(key) => (key, None),
        Err(error) => (
            None,
            Some(openraid::oauth::OAuthSession::from_store("gitlab", auth)?.ok_or(error)?),
        ),
    };
    let environment = std::env::var("GITLAB_INSTANCE_URL").ok();
    let base = explicit_endpoint
        .or_else(|| info.options["instanceUrl"].as_str())
        .or(environment.as_deref())
        .unwrap_or(&info.api)
        .to_owned();
    catalog
        .refresh_gitlab_models(workspace, &base, key.as_deref(), oauth.as_ref())
        .await
}

pub fn configure_provider(
    catalog: &Catalog,
    config: &mut Config,
    protocol_override: Option<&str>,
) -> Result<()> {
    configure_provider_with_auth(
        catalog,
        config,
        protocol_override,
        &AuthStore::load_default()?,
    )
}

pub fn configure_provider_with_auth(
    catalog: &Catalog,
    config: &mut Config,
    protocol_override: Option<&str>,
    auth: &AuthStore,
) -> Result<()> {
    let explicit_endpoint = config.base_url.clone();
    let provider = catalog.provider(&config.provider);
    let model = catalog.model(&config.provider, &config.model);
    use_model_context_limit(catalog, config);
    config.use_model_output_limit(model.map(|model| model.limit.output));
    let mut factory_settings = auth.provider_metadata(&config.provider)?;
    let mut headers = provider.map(|p| p.loader_headers()).unwrap_or_default();
    if let Some(provider) = provider {
        merge_json(&mut factory_settings, &provider.options);
    }
    if let Some(map) = factory_settings["headers"].as_object() {
        for (name, value) in map {
            if let Some(value) = value.as_str() {
                set_header(&mut headers, name, value);
            }
        }
    }
    let mut options = json!({});
    if let Some(model) = model {
        config.api_model = Some(model.api_id.clone());
        config.provider_npm.clone_from(&model.npm);
        if config.base_url.is_empty() {
            config.base_url.clone_from(&model.api);
        }
        config.protocol = if let Some(value) = protocol_override {
            parse_protocol(value)?
        } else {
            provider
                .context("provider missing from catalog")?
                .protocol(model)?
        };
        merge_json(&mut options, &model.metadata["options"]);
        if let Some(map) = model.metadata["headers"].as_object() {
            for (name, value) in map {
                if let Some(value) = value.as_str() {
                    set_header(&mut headers, name, value);
                }
            }
        }
    } else {
        if config.base_url.is_empty() {
            bail!("unknown model {}/{}; inspect openraid models {}, or provide --base-url for a custom endpoint", config.provider, config.model, config.provider);
        }
        if config.variant.is_some() {
            bail!("custom model has no catalog variants; pass reasoning settings with --provider-options");
        }
        config.provider_npm = "@ai-sdk/openai-compatible".into();
        config.api_model = None;
        config.protocol = protocol_override
            .map(parse_protocol)
            .transpose()?
            .unwrap_or(Protocol::ChatCompletions);
    }
    let mut env = provider.map(|p| p.env.clone()).unwrap_or_default();
    if config.provider == "snowflake-cortex" {
        env.insert(0, "SNOWFLAKE_CORTEX_TOKEN".into());
    } else if config.provider == "cloudflare-ai-gateway" {
        env.push("CF_AIG_TOKEN".into());
    }
    let configured_key = factory_settings["apiKey"]
        .as_str()
        .filter(|key| !key.is_empty())
        .or_else(|| {
            (config.provider == "snowflake-cortex")
                .then(|| factory_settings["token"].as_str())
                .flatten()
                .filter(|key| !key.is_empty())
        });
    let key = config.api_key.as_deref().or(configured_key);
    let resolution = match key {
        Some(key) if key.trim().is_empty() => bail!("API key must not be empty"),
        Some(key) => Ok(Some(key.to_owned())),
        None => auth.api_key(&config.provider, &env),
    };
    match resolution {
        Ok(key) => {
            config.api_key = key;
            config.oauth = None;
        }
        Err(error) => {
            let mut session =
                openraid::oauth::OAuthSession::from_store(&config.provider, auth)?.ok_or(error)?;
            if matches!(
                config.provider.as_str(),
                "azure" | "azure-cognitive-services"
            ) && !explicit_endpoint.is_empty()
            {
                let mut settings = factory_settings.clone();
                if settings["resourceName"].is_null() {
                    if let Some(credential) = auth.oauth(&config.provider)? {
                        settings["resourceName"] = credential["accountId"].clone();
                    }
                }
                if let Some(endpoint) =
                    openraid::provider_settings::expand_endpoint(&explicit_endpoint, &settings)?
                {
                    session = session.with_base_url(&endpoint)?;
                }
            }
            let api_id = config.api_model.as_deref().unwrap_or(&config.model);
            if !session.supports_model(api_id) {
                bail!("model {} is unavailable on this OAuth account; choose a Codex/Copilot-supported model with openraid setup", config.model);
            }
            let (protocol, base) = session.protocol_and_base_url_with_endpoint(
                api_id,
                model.and_then(|model| model.copilot_endpoint()),
            );
            config.protocol = protocol;
            config.base_url = base;
            if protocol == Protocol::Anthropic {
                config.provider_npm = "@ai-sdk/anthropic".into();
                if !headers
                    .keys()
                    .any(|key| key.eq_ignore_ascii_case("anthropic-beta"))
                {
                    set_header(
                        &mut headers,
                        "anthropic-beta",
                        "interleaved-thinking-2025-05-14",
                    );
                }
            }
            config.oauth = Some(session);
            config.api_key = None;
        }
    }
    if let (Some(provider), Some(model)) = (provider, model) {
        let authenticated = config.mock
            || config.oauth.is_some()
            || config.api_key.as_deref().is_some_and(|key| key != "public");
        if !provider.model_available(model, authenticated) {
            bail!("model {}/{} requires authentication; connect an API key or choose a free OpenCode model", config.provider, config.model);
        }
        if provider.id == "opencode" && !authenticated {
            config.api_key = Some("public".into());
        }
    }
    if matches!(
        config.provider.as_str(),
        "github-copilot" | "github-copilot-enterprise"
    ) && config.oauth.is_none()
        && config.protocol == Protocol::Anthropic
        && explicit_endpoint.is_empty()
    {
        let base = config.base_url.trim_end_matches('/');
        if !base.ends_with("/v1") {
            config.base_url = format!("{base}/v1");
        }
    }
    if let Some(model) = model {
        let variants = effective_variants(
            model,
            config.max_output_tokens as usize,
            &config.provider_npm,
        );
        if let Some(variant) = &config.variant {
            let selected = variants.get(variant).with_context(|| {
                format!(
                    "thinking variant {variant} unavailable for {}/{}; available: {}",
                    config.provider,
                    config.model,
                    openraid::variants::ordered_names(&variants).join(", ")
                )
            })?;
            merge_json(&mut options, selected);
        }
    }
    for (name, value) in &config.provider_headers {
        set_header(&mut headers, name, value);
    }
    config.provider_headers = headers;
    if config.protocol == Protocol::Sdk {
        let custom_settings = config
            .provider_options
            .get("sdkSettings")
            .cloned()
            .unwrap_or_else(|| json!({}));
        merge_json(&mut factory_settings, &custom_settings);
        options["sdkSettings"] = factory_settings.clone();
    }
    merge_json(&mut options, &config.provider_options);
    config.provider_options = options;
    if let Some(session) = &config.oauth {
        let mut validation_body = openraid::variants::wire_options(
            &config.provider_npm,
            &config.provider_options,
            config.protocol == Protocol::Responses,
        );
        session.prepare_request(&mut validation_body)?;
    }
    if let Some(model) = model {
        adjust_thinking_budget(config, model.limit.output)?;
    }
    if config.oauth.is_none() {
        let settings = config
            .provider_options
            .get("sdkSettings")
            .unwrap_or(&factory_settings);
        match openraid::provider_settings::expand_endpoint(&config.base_url, settings)? {
            Some(base) => config.base_url = base,
            None if config.protocol == Protocol::Sdk => {
                config.base_url.clear();
                if let Some(settings) = config.provider_options.get_mut("sdkSettings").and_then(Value::as_object_mut) { settings.remove("baseURL"); }
            },
            None => bail!("provider endpoint requires cloud settings; set the matching environment variables or use --provider-options with sdkSettings"),
        }
    }
    Ok(())
}

pub fn effective_variants(
    model: &openraid::catalog::Model,
    output_limit: usize,
    npm: &str,
) -> BTreeMap<String, Value> {
    if !matches!(npm, "@ai-sdk/anthropic" | "@ai-sdk/google-vertex/anthropic") {
        return model.variants.clone();
    }
    let mut variants = openraid::variants::variants(
        &model.provider_id,
        npm,
        &model.id,
        &model.api_id,
        model.reasoning,
        &model.release_date,
        model.limit.output.min(output_limit),
    );
    openraid::variants::apply_overrides(&mut variants, &model.metadata["variants"]);
    variants
}

pub fn redact_json(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            for (key, value) in map {
                let name = key.to_ascii_lowercase();
                if [
                    "apikey",
                    "api_key",
                    "authorization",
                    "x-api-key",
                    "access",
                    "refresh",
                    "token",
                    "password",
                    "secretaccesskey",
                    "accesskeyid",
                    "sessiontoken",
                    "clientsecret",
                    "client_secret",
                    "secret",
                    "privatekey",
                    "cf-aig-authorization",
                    "x-goog-api-key",
                    "private_key",
                    "credentials",
                    "serviceaccount",
                    "service_account",
                ]
                .contains(&name.as_str())
                {
                    *value = json!("[redacted]");
                } else if name == "headers" && value.is_object() {
                    if let Some(headers) = value.as_object_mut() {
                        for value in headers.values_mut() {
                            *value = json!("[redacted]");
                        }
                    }
                } else if ["api", "baseurl", "url", "endpoint"].contains(&name.as_str())
                    && value.as_str().is_some_and(|text| text.starts_with("http"))
                {
                    *value = json!(public_endpoint(value.as_str().unwrap_or_default()));
                } else {
                    *value = redact_json(std::mem::take(value));
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = redact_json(std::mem::take(item));
            }
        }
        _ => {}
    }
    value
}

fn adjust_thinking_budget(config: &mut Config, model_output: usize) -> Result<()> {
    // Preserve invalid explicit zero for the normal configuration validation.
    if config.max_output_tokens == 0 {
        return Ok(());
    }
    let budget = config
        .provider_options
        .pointer("/thinking/budgetTokens")
        .or_else(|| {
            config
                .provider_options
                .pointer("/reasoningConfig/budgetTokens")
        })
        .or_else(|| {
            config
                .provider_options
                .pointer("/thinkingConfig/thinkingBudget")
        })
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if budget >= u64::from(config.max_output_tokens) {
        if config.explicit_max_output_tokens && budget > 0 {
            bail!("thinking budget {budget} must be below explicit --max-output-tokens {}; raise the output limit or lower the thinking budget", config.max_output_tokens);
        }
        let required = budget.saturating_add(1024);
        if required >= config.context_budget as u64
            || (model_output > 0 && required > model_output as u64)
            || required > u64::from(u32::MAX)
        {
            bail!("thinking variant requires more than {budget} output tokens; raise --context-budget and select a model with sufficient output capacity");
        }
        config.max_output_tokens = required as u32;
    }
    Ok(())
}

fn merge_json(target: &mut Value, source: &Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            if value.is_object() && target.get(key).is_some_and(Value::is_object) {
                merge_json(target.get_mut(key).expect("existing object"), value);
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

fn use_model_context_limit(catalog: &Catalog, config: &mut Config) {
    let limit = catalog
        .model(&config.provider, &config.model)
        .filter(|model| model.limit.context > 0)
        .map(|model| model.limit.context);
    if config.explicit_context_budget {
        if let Some(limit) = limit {
            config.context_budget = config.context_budget.min(limit);
        }
    } else {
        config.context_budget = limit.unwrap_or_else(|| Config::default().context_budget);
    }
}

pub async fn refresh_codex_models(
    catalog: &mut Catalog,
    config: &Config,
    auth: &AuthStore,
) -> Result<()> {
    let provider = catalog
        .provider(&config.provider)
        .context("Codex LB provider missing")?;
    let endpoint = if config.base_url.is_empty() {
        provider.api.clone()
    } else {
        config.base_url.clone()
    };
    let key = config.api_key.clone().or_else(|| {
        provider.options["apiKey"]
            .as_str()
            .filter(|key| !key.is_empty())
            .map(str::to_owned)
    });
    let key = match key {
        Some(key) => Some(key),
        None => auth.api_key(&config.provider, &provider.env)?,
    };
    catalog
        .refresh_models(&config.provider, &endpoint, key.as_deref())
        .await
        .context("could not load models from Codex LB; check your endpoint and API key")?;
    Ok(())
}

fn setup_needs_endpoint(catalog: &Catalog, config: &Config) -> bool {
    config.provider == "codex-lb"
        || (config.base_url.trim().is_empty()
            && catalog
                .provider(&config.provider)
                .is_none_or(|provider| provider.needs_endpoint(Some(&config.model))))
}

fn setup_after_variant(catalog: &Catalog, config: &Config) -> usize {
    if is_codex_provider(&config.provider) {
        5
    } else if setup_needs_endpoint(catalog, config) {
        3
    } else {
        4
    }
}

fn setup_before_key(catalog: &Catalog, config: &Config) -> usize {
    if setup_needs_endpoint(catalog, config) {
        3
    } else if is_codex_provider(&config.provider) {
        0
    } else {
        2
    }
}

async fn wizard(
    mut catalog: Catalog,
    config: &mut Config,
    protocol_override: Option<&str>,
) -> Result<bool> {
    let mut auth = AuthStore::load_default()?;
    let mut ui = SetupUi::new()?;
    let mut step: usize = 0;
    let mut save_key = false;
    let mut discovery_notice = String::new();
    let mut discovered_endpoint = String::new();
    let original_options = config.provider_options.clone();
    let original_headers = config.provider_headers.clone();
    let original_output_budget = config.max_output_tokens;
    let original_context_budget = config.context_budget;
    loop {
        let codex = is_codex_provider(&config.provider);
        let endpoint_prompt = setup_needs_endpoint(&catalog, config);
        let total_steps = if endpoint_prompt { 7 } else { 6 };
        let model_position = if codex { total_steps - 3 } else { 2 };
        let thinking_position = model_position + 1;
        let key_position = if codex {
            total_steps - 4
        } else {
            total_steps - 2
        };
        match step {
            0 => {
                config.provider_options = original_options.clone();
                config.provider_headers = original_headers.clone();
                config.max_output_tokens = original_output_budget;
                config.context_budget = original_context_budget;
                let mut choices: Vec<Choice> = catalog.providers.values().map(|provider| {
                    let connection = match auth.api_key(&provider.id, &provider.env) {
                        Ok(Some(_)) => "connected".to_owned(),
                            Err(_) => "OpenCode OAuth account".to_owned(),
                         _ if provider.options["apiKey"].as_str().is_some_and(|key| !key.is_empty()) => "connected".to_owned(),
                         _ if is_codex_provider(&provider.id) => "local endpoint · optional API key".to_owned(),
                        _ => format!("connect with {}", provider.env.first().map(String::as_str).unwrap_or("an API key")),
                    };
                    let models = if is_codex_provider(&provider.id) { "models from your server".to_owned() } else { format!("{} models", provider.models.len()) };
                    Choice::new(&provider.id, &provider.name, format!("{connection}   {models}"))
                }).collect();
                choices.sort_by_key(|c| (!c.detail.starts_with("connected"), c.name.to_lowercase()));
                choices.insert(0, Choice::new("custom", "Custom OpenAI-compatible endpoint", "Bring your own endpoint and model identifier"));
                match ui.pick(&format!("1 / {total_steps}   Choose your provider"), "Search the same provider catalog used by OpenCode. Connected accounts are first.", &choices, Some(&config.provider))? {
                    Some(id) => {
                        if id != config.provider {
                            config.provider = id;
                            config.model = default_model(&catalog, &config.provider);
                            config.variant = None;
                            config.api_key = None;
                            config.base_url.clear();
                            discovered_endpoint.clear();
                            discovery_notice.clear();
                            save_key = false;
                        }
                        step = if is_codex_provider(&config.provider) {
                            if setup_needs_endpoint(&catalog, config) { 3 } else { 4 }
                        } else { 1 };
                    }
                    None => return Ok(false),
                }
            }
            1 => {
                let result = if config.provider == "custom" {
                    ui.input(&format!("{model_position} / {total_steps}   Model identifier"), "Use the exact model identifier accepted by your endpoint.", &config.model, false, false)?
                } else {
                    if is_codex_provider(&config.provider) {
                        let provider = catalog.provider(&config.provider).context("provider missing")?;
                        let endpoint = if config.base_url.is_empty() { provider.api.clone() } else { config.base_url.clone() };
                        if discovered_endpoint != endpoint {
                            if let Err(error) = refresh_codex_models(&mut catalog, config, &auth).await {
                                let choices = [
                                    Choice::new("retry", "Retry model discovery", "Request the model list from your Codex LB server again"),
                                    Choice::new("endpoint", "Change endpoint or API key", "Reconnect before choosing a model"),
                                    Choice::new("provider", "Choose another provider", "Return to provider selection"),
                                ];
                                step = match ui.pick("Codex LB connection needs attention", &format!("{error:#}"), &choices, Some("endpoint"))?.as_deref() {
                                    Some("retry") => 1,
                                    Some("endpoint") => 3,
                                    _ => 0,
                                };
                                continue;
                            }
                            discovery_notice = "Live models discovered from your Codex endpoint.".into();
                            discovered_endpoint = endpoint;
                        }
                    }
                    if config.provider == "gitlab" && discovered_endpoint.is_empty() {
                        let endpoint = (!config.base_url.is_empty()).then_some(config.base_url.as_str());
                        match refresh_gitlab_catalog(&mut catalog, &config.workspace, endpoint, config.api_key.as_deref(), &auth).await {
                            Ok(count) if count > 0 => discovery_notice = format!("Discovered {count} additional GitLab workflow models for this workspace."),
                            Ok(_) => {},
                            Err(_) => discovery_notice = "GitLab workflow discovery unavailable; configured and catalog models remain available.".into(),
                        }
                        discovered_endpoint = "gitlab-discovery-attempted".into();
                    }
                    let env = catalog.provider(&config.provider).map(|p| p.env.as_slice()).unwrap_or(&[]);
                    let oauth = if config.api_key.is_none() && auth.api_key(&config.provider, env).is_err() {
                        openraid::oauth::OAuthSession::from_store(&config.provider, &auth).ok().flatten()
                    } else { None };
                    let choices: Vec<Choice> = catalog.search_models(&config.provider, "").iter().map(|model| {
                        let mut choice = Choice::new(&model.id, &model.name, format!("{} context   {} output   {}", model.limit.context, model.limit.output,
                            if model.reasoning { "thinking supported" } else { "standard generation" }));
                        if !model.tool_call { choice.disabled_reason = Some("This model does not support tools required by the swarm. Choose a tool-capable model.".into()); }
                        if oauth.as_ref().is_some_and(|session| !session.supports_model(&model.api_id)) { choice.disabled_reason = Some("Unavailable on this OAuth account. Choose a Codex-supported model or connect an API key.".into()); }
                        choice
                    }).collect();
                    ui.pick(&format!("{model_position} / {total_steps}   Choose your model"), if discovery_notice.is_empty() { "Search model names or identifiers. Models without native tool support explain their limits." } else { &discovery_notice }, &choices, Some(&config.model))?
                };
                match result { Some(model) if !model.trim().is_empty() => { if model != config.model { config.variant = None; } config.model = model; step = 2; }, Some(_) => {}, None => step = if codex { 4 } else { 0 } }
            }
            2 => {
                let mut choices = vec![Choice::new("", "Provider default", "Use the model's default thinking settings")];
                if let Some(model) = catalog.model(&config.provider, &config.model) {
                    let env = catalog.provider(&config.provider).map(|p| p.env.as_slice()).unwrap_or(&[]);
                    let oauth = if config.api_key.is_none() && auth.api_key(&config.provider, env).is_err() { openraid::oauth::OAuthSession::from_store(&config.provider, &auth).ok().flatten() } else { None };
                    let npm = if oauth.is_some_and(|session| session.protocol_and_base_url(&model.api_id).0 == Protocol::Anthropic) { "@ai-sdk/anthropic" } else { &model.npm };
                    let variants = effective_variants(model, config.max_output_tokens as usize, npm);
                    for variant in openraid::variants::ordered_names(&variants) {
                        let options = &variants[&variant];
                        choices.push(Choice::new(&variant, &variant, serde_json::to_string(options)?));
                    }
                }
                match ui.pick(&format!("{thinking_position} / {total_steps}   Choose thinking depth"), "Only this provider/model's supported variants are shown. Default leaves the API's choice intact.", &choices, config.variant.as_deref())? {
                    Some(variant) => { config.variant = (!variant.is_empty()).then_some(variant); step = setup_after_variant(&catalog, config); }, None => step = 1,
                }
            }
            3 => {
                let suggested = catalog.model(&config.provider, &config.model).map(|m| m.api.as_str())
                    .or_else(|| catalog.provider(&config.provider).map(|provider| provider.api.as_str()))
                    .unwrap_or("http://127.0.0.1:2455/v1");
                let initial = if config.base_url.is_empty() { suggested } else { &config.base_url };
                match ui.input(&format!("{} / {total_steps}   API endpoint", if codex { 2 } else { 4 }), if codex { "Codex-LB: http://127.0.0.1:2455/v1 uses Responses." } else { "Enter the API base URL for your custom provider." }, initial, false, false)? {
                    Some(base) => { config.base_url = base.trim().to_owned(); discovered_endpoint.clear(); step = 4; }, None => step = if is_codex_provider(&config.provider) { 0 } else { 2 },
                }
            }
            4 => {
                let env = catalog.provider(&config.provider).map(|p| p.env.as_slice()).unwrap_or(&[]);
                let existing = auth.api_key(&config.provider, env).ok().flatten();
                let configured_key = catalog.provider(&config.provider).and_then(|p| p.options["apiKey"].as_str()).is_some_and(|key| !key.is_empty());
                let has_key = config.api_key.is_some() || existing.is_some() || configured_key;
                let has_oauth = auth.oauth(&config.provider)?.is_some();
                if !has_key && !has_oauth && matches!(config.provider.as_str(), "openai" | "github-copilot" | "github-copilot-enterprise") {
                    let method = ui.pick(&format!("{key_position} / {total_steps}   Connect your provider"), "Choose an account sign-in or an API key. Browser sign-in displays a one-time code.", &[
                        Choice::new("login", "Sign in with your account", "ChatGPT/Codex or GitHub Copilot device login"),
                        Choice::new("key", "Use an API key", "Paste an API key or use a public/custom endpoint"),
                    ], Some("login"))?;
                    match method.as_deref() {
                        Some("login") => {
                            let enterprise = if config.provider == "github-copilot-enterprise" {
                                match ui.input("Enterprise domain", "Enter your GitHub Enterprise hostname, such as company.ghe.com.", "", false, false)? { Some(domain) => Some(domain), None => continue }
                            } else { None };
                            let login = openraid::oauth::begin_login(&config.provider, enterprise.as_deref()).await?;
                            let uri = login.verification_uri.clone();
                            let code = login.user_code.clone();
                            if let Some(credential) = ui.wait_for_login(&uri, &code, login.wait()).await? {
                                auth.set_oauth(&config.provider, &credential)?;
                                auth.save()?;
                                let session = openraid::oauth::OAuthSession::from_store(&config.provider, &auth)?.context("signed-in account missing")?;
                                let api_id = catalog.model(&config.provider, &config.model).map(|m| m.api_id.as_str()).unwrap_or(&config.model);
                                if !session.supports_model(api_id) {
                                    config.model = default_model(&catalog, &config.provider);
                                    config.variant = None;
                                    step = 1;
                                } else { step = 5; }
                            }
                            continue;
                        }
                        Some("key") => {},
                        _ => { step = setup_before_key(&catalog, config); continue; },
                    }
                }
                let help = if has_key { "A credential is already available. Enter keeps it; paste a replacement to use another key." }
                    else if has_oauth { "Your OpenCode OAuth account is available. Enter uses it; paste an API key to use API billing instead." }
                    else { "Paste an API key, or leave empty for a public/local endpoint. The key is hidden." };
                match ui.input(&format!("{key_position} / {total_steps}   Connect securely"), help, "", true, false)? {
                    Some(key) => {
                        if !key.trim().is_empty() {
                            config.api_key = Some(key.trim().to_owned());
                            discovered_endpoint.clear();
                            let choice = ui.pick("Remember this API key?", "Session only keeps the key in memory. Save writes your private OpenRaid credential file.", &[
                                Choice::new("session", "Use for this session", "Do not save this API key"),
                                Choice::new("save", "Save this connection", "Reuse the credential in later sessions"),
                            ], Some("session"))?;
                            if choice.is_none() { continue; }
                            save_key = choice.as_deref() == Some("save");
                        } else if config.api_key.is_none() { config.api_key = existing; }
                        step = if is_codex_provider(&config.provider) || (config.provider == "gitlab" && discovered_endpoint.is_empty()) { 1 } else { 5 };
                    }
                    None => step = setup_before_key(&catalog, config),
                }
            }
            5 => {
                match ui.input(&format!("{} / {total_steps}   What should the swarm do?", total_steps - 1), "Give every agent the same clear objective. Paste longer instructions; Shift+Enter adds a line.", &config.objective, false, true)? {
                    Some(objective) if !objective.trim().is_empty() => { config.objective = objective; step = 6; },
                    Some(_) => {}, None => step = if is_codex_provider(&config.provider) { 2 } else { 4 },
                }
            }
            _ => {
                config.provider_options = original_options.clone();
                config.provider_headers = original_headers.clone();
                config.max_output_tokens = original_output_budget;
                config.context_budget = original_context_budget;
                config.resolve_concurrency();
                if let Err(error) = configure_provider(&catalog, config, protocol_override) {
                    let choices = [Choice::new("edit", "Review provider settings", format!("{error:#}")), Choice::new("cancel", "Cancel setup", "Return to your terminal")];
                    if ui.pick("Connection needs attention", "Correct the provider, model, thinking variant or endpoint before launch.", &choices, Some("edit"))?.as_deref() == Some("edit") { step = 0; continue; }
                    return Ok(false);
                }
                let endpoint = public_endpoint(&config.base_url);
                let summary = format!("{} agents   {}/{}   thinking {}\n{}   {:?}\nOutput reserve {} / context {} tokens\nObjective: {}", config.agents, config.provider, config.model,
                    config.variant.as_deref().unwrap_or("default"), endpoint, config.protocol, config.max_output_tokens, config.context_budget, config.objective.lines().next().unwrap_or_default());
                let choices = [Choice::new("launch", "Launch swarm", "Start agents and open the live console"),
                    Choice::new("edit", "Change provider or model", "Return to provider selection"),
                    Choice::new("agents", "Change swarm size", "Choose between 1 and 500 agents"),
                    Choice::new("objective", "Edit the objective", "Change the instructions shared by every agent"),
                    Choice::new("cancel", "Cancel", "Nothing starts until you choose Launch")];
                match ui.pick(&format!("{total_steps} / {total_steps}   Ready to launch"), &summary, &choices, Some("launch"))?.as_deref() {
                    Some("launch") => {
                        config.validate()?;
                        if save_key { if let Some(key) = &config.api_key { auth.set_api_key(&config.provider, key)?; } }
                        auth.set_selection(&config.provider, &config.model, config.variant.as_deref());
                        auth.remember_launch(config);
                        auth.save()?;
                        return Ok(true);
                    }
                    Some("edit") => step = 0,
                    Some("objective") => step = 5,
                    Some("agents") => {
                        let mut initial = config.agents.to_string();
                        let mut help = "Choose 1 to 500 agents. All agents share the same objective and board.".to_owned();
                        while let Some(value) = ui.input("Swarm size", &help, &initial, false, false)? {
                            match value.trim().parse::<usize>() {
                                Ok(agents) if (1..=500).contains(&agents) => { config.agents = agents; break; },
                                _ => { initial = value; help = "Enter a whole number from 1 to 500; Esc keeps the current size.".into(); },
                            }
                        }
                    }
                    Some("cancel") => return Ok(false),
                    _ => step = 5,
                }
            }
        }
    }
}

pub fn public_endpoint(endpoint: &str) -> String {
    match reqwest::Url::parse(endpoint) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.to_string()
        }
        Err(_) => "Custom endpoint".into(),
    }
}

pub fn resolve_live_selection(
    catalog: &Catalog,
    template: &Config,
    provider: &str,
    model: &str,
    variant: Option<&str>,
    auth: &AuthStore,
) -> Result<Config> {
    let info = catalog.provider(provider).context("provider missing")?;
    let mut next = template.clone();
    next.provider = provider.to_owned();
    next.model = model.to_owned();
    next.api_model = None;
    next.variant = variant.filter(|value| !value.is_empty()).map(str::to_owned);
    next.base_url = (provider == template.provider
        && !template.base_url.is_empty()
        && (is_codex_provider(provider)
            || provider == "gitlab"
            || (template.base_url != info.api
                && catalog
                    .model(provider, &template.model)
                    .is_none_or(|model| model.api != template.base_url))))
    .then(|| template.base_url.clone())
    .or_else(|| auth.connection_endpoint(provider).map(str::to_owned))
    .unwrap_or_default();
    next.api_key = (provider == template.provider)
        .then(|| template.api_key.clone())
        .flatten()
        .or_else(|| auth.api_key(provider, &info.env).ok().flatten());
    next.oauth = None;
    next.provider_options = json!({});
    next.provider_headers.clear();
    let protocol_override = catalog
        .model(provider, model)
        .and_then(|model| model.metadata["_openraid_protocol"].as_str());
    configure_provider_with_auth(catalog, &mut next, protocol_override, auth)?;
    next.validate()?;
    Ok(next)
}

async fn run(config: Config) -> Result<Option<(openraid::session::SessionNavigation, Config)>> {
    let no_tui = config.no_tui;
    let session = tui::SessionInfo {
        provider: config.provider.clone(),
        model: config.model.clone(),
        variant: config.variant.clone().unwrap_or_else(|| "default".into()),
        objective: config.objective.clone(),
    };
    let workspace = config.workspace.clone();
    let config_path = config.config_path.clone();
    let interactive_session = config.interactive_session;
    let mut config = config;
    if config.database.is_file() {
        config.database = std::fs::canonicalize(&config.database)?;
    }
    ensure_session_workspace(&config.workspace, &config.database)?;
    let harness = Harness::new(config.clone()).await?;
    config.database = std::fs::canonicalize(&config.database)?;
    openraid::session_catalog::register(&config.workspace, &config.database, &config.objective)?;
    if !config.mock {
        let mut auth = AuthStore::load_default()?;
        auth.remember_launch(&config);
        auth.save()?;
    }
    if no_tui {
        let summary = harness.run().await?;
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(None);
    }
    let store = harness.store.clone();
    let metrics = harness.metrics.clone();
    let control = harness.control.clone();
    let manager = openraid::quick::ProviderManager::new(
        load_catalog(&workspace, config_path.as_deref())?,
        control.clone(),
        resolve_live_selection,
        &AuthStore::load_default()?,
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let worker = tokio::spawn(async move {
        let result = harness.run().await;
        let _ = shutdown_tx.send(true);
        result
    });
    let display = tui::run_with_controls(store, metrics, shutdown_rx, session, manager).await;
    if interactive_session {
        control.detach();
    }
    if let Err(error) = display {
        eprintln!("dashboard error: {error:#}; swarm continues headless");
    }
    let summary = worker.await.context("swarm supervisor task failed")??;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(control.take_navigation().map(|navigation| {
        let mut active = (*control.current().config).clone();
        active.agents = control.members().len();
        (navigation, active)
    }))
}

#[cfg(test)]
#[path = "output_budget_tests.rs"]
mod output_budget_tests;

#[cfg(test)]
#[path = "concurrency_tests.rs"]
mod concurrency_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remembered_workspace_never_overrides_invocation_directory() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old-project");
        std::fs::create_dir(&old).unwrap();
        let mut auth = empty_auth(temp.path());
        auth.remember_launch(&Config {
            workspace: std::fs::canonicalize(&old).unwrap(),
            database: old.join("previous.sqlite3"),
            config_path: Some(old.join("missing-old-config.json")),
            ..Config::default()
        });
        let Some(Command::Setup(args)) = Cli::parse_from(["openraid", "setup", "--no-tui"]).command
        else {
            panic!("setup arguments")
        };
        // Mock lets this exercise the setup resolution without needing a TTY or provider.
        let config = args
            .config_with_auth(true, true, auth)
            .await
            .unwrap()
            .unwrap();
        let cwd = std::fs::canonicalize(".").unwrap();
        assert_eq!(config.workspace, cwd);
        assert_eq!(config.database, default_database_path(&cwd));
        assert_eq!(config.config_path, None);
    }

    #[tokio::test]
    async fn new_session_does_not_reuse_remembered_database() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(temp.path()).unwrap();
        let mut auth = empty_auth(temp.path());
        auth.remember_launch(&Config {
            workspace: workspace.clone(),
            database: workspace.join("old.sqlite3"),
            ..Config::default()
        });
        let Some(Command::Setup(args)) = Cli::parse_from([
            "openraid",
            "setup",
            "--new",
            "--no-tui",
            "--workspace",
            workspace.to_str().unwrap(),
        ])
        .command
        else {
            panic!("setup arguments")
        };
        let config = args
            .config_with_auth(true, true, auth)
            .await
            .unwrap()
            .unwrap();
        assert!(config
            .database
            .starts_with(workspace.join(".openraid").join("sessions")));
        assert!(!config.resume);
        assert!(!config.database.exists());
    }

    #[test]
    fn conflicting_session_controls_are_rejected_by_cli() {
        for args in [
            vec!["openraid", "setup", "--new", "--resume"],
            vec![
                "openraid",
                "setup",
                "--new",
                "--database",
                "existing.sqlite3",
            ],
            vec!["openraid", "setup", "--new", "--session", "session-example"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[tokio::test]
    async fn remembered_tui_requires_explicit_resume_for_unfinished_work() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("remembered.sqlite3");
        let store = Store::open(&database).await?;
        let prompt = store.append("owner", "unfinished task", true).await?;
        let mut config = Config {
            database,
            interactive_session: true,
            ..Config::default()
        };

        enable_remembered_recovery(&mut config, true).await?;
        assert!(!config.resume, "opening the TUI must not resume work");
        assert_eq!(store.unfinished_prompt().await?, Some(prompt.clone()));

        config.resume = true;
        enable_remembered_recovery(&mut config, true).await?;
        assert!(config.resume, "explicit --resume must remain honored");
        assert_eq!(store.unfinished_prompt().await?, Some(prompt));
        Ok(())
    }

    #[test]
    fn live_gitlab_launch_endpoint_survives_saved_connection_without_pinning_other_model_defaults(
    ) -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut auth = empty_auth(temp.path());
        auth.set_connection_endpoint("gitlab", "https://gitlab.saved");
        let catalog = Catalog::from_json(
            r#"{
            "gitlab":{"npm":"gitlab-ai-provider","api":"https://gitlab.catalog","models":{"duo-chat":{"tool_call":true}}},
            "fixture":{"api":"https://provider.default/v1","models":{"alpha":{"provider":{"api":"https://alpha.default/v1"},"tool_call":true},"beta":{"provider":{"api":"https://beta.default/v1"},"tool_call":true}}}
        }"#,
        )?;
        let current = Config {
            objective: "verify selection".into(),
            provider: "gitlab".into(),
            model: "duo-chat".into(),
            base_url: "https://gitlab.explicit".into(),
            api_key: Some("active-key".into()),
            ..Config::default()
        };
        let next = resolve_live_selection(&catalog, &current, "gitlab", "duo-chat", None, &auth)?;
        assert_eq!(next.base_url, "https://gitlab.explicit");
        assert_eq!(next.api_key.as_deref(), Some("active-key"));
        let current = Config {
            objective: "verify selection".into(),
            provider: "fixture".into(),
            model: "alpha".into(),
            base_url: "https://alpha.default/v1".into(),
            api_key: Some("active-key".into()),
            ..Config::default()
        };
        let next = resolve_live_selection(&catalog, &current, "fixture", "beta", None, &auth)?;
        assert_eq!(next.base_url, "https://beta.default/v1");
        Ok(())
    }

    fn empty_auth(path: &std::path::Path) -> AuthStore {
        AuthStore::load_with_opencode(path.join("auth.json"), None).unwrap()
    }

    #[test]
    fn source_loader_free_access_headers_and_snowflake_configured_token() {
        let temp = tempfile::tempdir().unwrap();
        let auth = empty_auth(temp.path());
        let catalog = Catalog::from_json(r#"{
            "opencode":{"api":"http://localhost/v1","models":{"free":{"cost":{"input":0}},"paid":{"cost":{"input":1}}}},
            "anthropic":{"npm":"@ai-sdk/anthropic","models":{"claude":{}}},
            "snowflake-cortex":{"api":"https://${SNOWFLAKE_ACCOUNT}.snowflakecomputing.com/api/v2/cortex/v1","options":{"account":"fixture-account","token":"configured-token"},"models":{"model":{}}}
        }"#).unwrap();
        let mut config = Config {
            provider: "opencode".into(),
            model: "free".into(),
            base_url: String::new(),
            ..Config::default()
        };
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        assert_eq!(config.api_key.as_deref(), Some("public"));
        config.model = "paid".into();
        assert!(
            configure_provider_with_auth(&catalog, &mut config, None, &auth)
                .unwrap_err()
                .to_string()
                .contains("requires authentication")
        );
        config.api_key = Some("private-key".into());
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        let mut config = Config {
            provider: "anthropic".into(),
            model: "claude".into(),
            base_url: String::new(),
            ..Config::default()
        };
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        assert_eq!(
            config.provider_headers["anthropic-beta"],
            "interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14"
        );
        let mut config = Config {
            provider: "snowflake-cortex".into(),
            model: "model".into(),
            base_url: String::new(),
            ..Config::default()
        };
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        assert_eq!(config.api_key.as_deref(), Some("configured-token"));
        assert_eq!(
            config.base_url,
            "https://fixture-account.snowflakecomputing.com/api/v2/cortex/v1"
        );
    }

    #[test]
    fn same_provider_live_switch_keeps_session_key_until_reconnected() {
        let temp = tempfile::tempdir().unwrap();
        let mut auth = empty_auth(temp.path());
        auth.set_api_key("fixture", "saved-key").unwrap();
        auth.set_api_key("other-fixture", "other-key").unwrap();
        let catalog = Catalog::from_json(r#"{
            "fixture":{"env":[],"npm":"@ai-sdk/openai-compatible","api":"http://localhost/v1",
                "models":{"model":{"tool_call":true,"reasoning":true,"_openraid_protocol":"responses"}}},
            "other-fixture":{"env":[],"npm":"@ai-sdk/openai-compatible","api":"http://localhost/v1",
                "models":{"other-model":{"tool_call":true}}}
        }"#).unwrap();
        let template = Config {
            provider: "fixture".into(),
            model: "model".into(),
            api_key: Some("session-key".into()),
            objective: "fixture objective".into(),
            workspace: temp.path().to_owned(),
            ..Config::default()
        };
        let same =
            resolve_live_selection(&catalog, &template, "fixture", "model", Some("high"), &auth)
                .unwrap();
        assert_eq!(same.api_key.as_deref(), Some("session-key"));
        assert_eq!(same.protocol, Protocol::Responses);
        let other = resolve_live_selection(
            &catalog,
            &template,
            "other-fixture",
            "other-model",
            None,
            &auth,
        )
        .unwrap();
        assert_eq!(other.api_key.as_deref(), Some("other-key"));
    }

    #[test]
    fn live_selection_keeps_explicit_session_endpoint_and_changes_model_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let mut auth = empty_auth(temp.path());
        auth.set_connection_endpoint("fixture", "http://saved.example/v1");
        let catalog = Catalog::from_json(
            r#"{
            "fixture":{"env":[],"npm":"@ai-sdk/openai-compatible","api":"http://default.example/v1",
                "models":{"first":{},"second":{"provider":{"api":"http://second.example/v1"}}}}
        }"#,
        )
        .unwrap();
        let mut template = Config {
            provider: "fixture".into(),
            model: "first".into(),
            base_url: "http://explicit-session.example/v1".into(),
            api_key: Some("session-key".into()),
            objective: "fixture objective".into(),
            workspace: temp.path().to_owned(),
            ..Config::default()
        };
        let selected =
            resolve_live_selection(&catalog, &template, "fixture", "second", None, &auth).unwrap();
        assert_eq!(selected.base_url, "http://explicit-session.example/v1");
        template.base_url = "http://default.example/v1".into();
        let selected =
            resolve_live_selection(&catalog, &template, "fixture", "second", None, &auth).unwrap();
        assert_eq!(selected.base_url, "http://saved.example/v1");
        let selected = resolve_live_selection(
            &catalog,
            &template,
            "fixture",
            "second",
            None,
            &empty_auth(temp.path()),
        )
        .unwrap();
        assert_eq!(selected.base_url, "http://second.example/v1");
    }

    #[tokio::test]
    async fn explicit_config_is_retained_for_live_catalog_and_remembered_launches() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("external-provider.jsonc");
        std::fs::write(
            &path,
            r#"{
            "provider":{"external-fixture":{"npm":"@ai-sdk/openai-compatible",
                "options":{"baseURL":"http://localhost/v1"},
                "models":{"alias":{"id":"wire-id","tool_call":true,"reasoning":true,
                    "variants":{"focused":{"reasoningEffort":"high"}}}}}}
        }"#,
        )
        .unwrap();
        let cli = Cli::parse_from([
            "openraid",
            "demo",
            "--no-tui",
            "--workspace",
            temp.path().to_str().unwrap(),
            "--config",
            path.to_str().unwrap(),
        ]);
        let Some(Command::Demo(args)) = cli.command else {
            panic!("demo arguments")
        };
        let config = args.config(true, false).await.unwrap().unwrap();
        assert_eq!(
            config.config_path.as_deref(),
            Some(std::fs::canonicalize(&path).unwrap().as_path())
        );
        let mut auth = empty_auth(temp.path());
        auth.remember_launch(&config);
        auth.save().unwrap();
        let saved = empty_auth(temp.path());
        let mut restored = Config::default();
        saved.launch_profile().unwrap().apply(&mut restored);
        let catalog = load_catalog(temp.path(), restored.config_path.as_deref()).unwrap();
        let model = catalog.model("external-fixture", "alias").unwrap();
        assert_eq!(model.api_id, "wire-id");
        assert_eq!(model.variants["focused"]["reasoningEffort"], "high");
    }

    #[test]
    fn imported_custom_provider_without_endpoint_can_be_connected_in_setup() {
        let catalog = Catalog::from_json(r#"{
            "custom-native":{"npm":"@ai-sdk/openai-compatible","models":{"fixture":{"tool_call":true}}},
            "sdk-default":{"npm":"@ai-sdk/cohere","models":{"fixture":{"tool_call":true}}}
        }"#).unwrap();
        let mut config = Config {
            provider: "custom-native".into(),
            model: "fixture".into(),
            base_url: String::new(),
            ..Config::default()
        };
        assert!(
            setup_needs_endpoint(&catalog, &config),
            "an imported native provider still needs its missing URL"
        );
        assert_eq!(setup_after_variant(&catalog, &config), 3);
        config.base_url = "http://localhost/v1".into();
        assert!(!setup_needs_endpoint(&catalog, &config));
        config.provider = "sdk-default".into();
        config.base_url.clear();
        assert!(
            !setup_needs_endpoint(&catalog, &config),
            "SDK-owned defaults do not need URL input"
        );
    }

    #[test]
    fn native_standard_providers_resolve_sdk_defaults_without_endpoint_input() {
        let catalog = Catalog::bundled().unwrap();
        let temp = tempfile::tempdir().unwrap();
        for (provider, protocol) in [
            ("openai", Protocol::Responses),
            ("anthropic", Protocol::Anthropic),
            ("google", Protocol::Gemini),
        ] {
            let model = catalog
                .provider(provider)
                .unwrap()
                .models
                .values()
                .find(|model| model.tool_call)
                .unwrap();
            let mut config = Config {
                provider: provider.into(),
                model: model.id.clone(),
                base_url: String::new(),
                api_key: Some("fixture".into()),
                ..Config::default()
            };
            configure_provider_with_auth(&catalog, &mut config, None, &empty_auth(temp.path()))
                .unwrap();
            assert!(
                !config.base_url.is_empty(),
                "{provider} needs its SDK default URL"
            );
            assert!(reqwest::Url::parse(&config.base_url).is_ok());
            assert_eq!(config.protocol, protocol);
            assert!(!setup_needs_endpoint(&catalog, &config));
        }
    }

    #[test]
    fn standard_provider_setup_skips_endpoint_and_preserves_custom_and_codex_routes() {
        let mut catalog = Catalog::bundled().unwrap();
        for provider in catalog
            .providers
            .values()
            .filter(|provider| provider.id != "codex-lb")
        {
            let config = Config {
                provider: provider.id.clone(),
                base_url: String::new(),
                ..Config::default()
            };
            assert!(
                !setup_needs_endpoint(&catalog, &config),
                "{} must use its default endpoint",
                provider.id
            );
            assert_eq!(setup_after_variant(&catalog, &config), 4);
            assert_eq!(setup_before_key(&catalog, &config), 2);
        }
        let mut config = Config {
            provider: "codex-lb".into(),
            base_url: String::new(),
            ..Config::default()
        };
        assert!(setup_needs_endpoint(&catalog, &config));
        assert_eq!(setup_after_variant(&catalog, &config), 5);
        assert_eq!(setup_before_key(&catalog, &config), 3);
        config.provider = "custom".into();
        assert!(setup_needs_endpoint(&catalog, &config));
        assert_eq!(setup_after_variant(&catalog, &config), 3);
        config.base_url = "http://localhost/v1".into();
        assert!(!setup_needs_endpoint(&catalog, &config));
        assert_eq!(setup_after_variant(&catalog, &config), 4);
        catalog
            .apply_config(
                &json!({"provider":{"codex-pool":{"options":{"baseURL":"http://localhost/v1"}}}}),
            )
            .unwrap();
        config.provider = "codex-pool".into();
        config.base_url.clear();
        assert!(!setup_needs_endpoint(&catalog, &config));
        assert_eq!(setup_after_variant(&catalog, &config), 5);
        assert_eq!(setup_before_key(&catalog, &config), 0);
    }

    #[test]
    fn model_context_defaults_to_configured_limits_and_respects_explicit_budgets() {
        let mut catalog = Catalog::from_json("{}").unwrap();
        catalog
            .apply_config(&json!({"provider":{"codex-pool":{
                "models":{"gpt-6.1-sol":{"limit":{"context":372000,"output":65536}}}
            }}}))
            .unwrap();
        let mut config = Config {
            provider: "codex-pool".into(),
            model: "gpt-6.1-sol".into(),
            ..Config::default()
        };
        use_model_context_limit(&catalog, &mut config);
        assert_eq!(config.context_budget, 372000);
        config.context_budget = 32000;
        config.explicit_context_budget = true;
        use_model_context_limit(&catalog, &mut config);
        assert_eq!(config.context_budget, 32000);
        config.context_budget = 500000;
        use_model_context_limit(&catalog, &mut config);
        assert_eq!(config.context_budget, 372000);
        config.provider = "openai".into();
        config.explicit_context_budget = false;
        use_model_context_limit(&catalog, &mut config);
        assert_eq!(config.context_budget, 32000);
    }

    #[test]
    fn bundled_provider_context_defaults_use_advertised_capacity() {
        let catalog = Catalog::bundled().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let auth = empty_auth(temp.path());
        for provider in ["openai", "google", "anthropic"] {
            let model = catalog
                .provider(provider)
                .unwrap()
                .models
                .values()
                .find(|model| model.limit.context > 100000 && model.tool_call)
                .unwrap();
            let mut config = Config {
                provider: provider.into(),
                model: model.id.clone(),
                api_key: Some("fixture-key".into()),
                base_url: String::new(),
                ..Config::default()
            };
            configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
            assert_eq!(config.context_budget, model.limit.context, "{provider}");
            config.context_budget = 32000;
            config.explicit_context_budget = true;
            configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
            assert_eq!(config.context_budget, 32000, "{provider}");
        }
    }

    #[test]
    fn live_model_context_defaults_recalculate_upward_downward_and_without_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let auth = empty_auth(temp.path());
        let mut catalog = Catalog::from_json("{}").unwrap();
        catalog
            .apply_config(&json!({"provider":{"fixture":{
                "options":{"baseURL":"http://localhost/v1","apiKey":"fixture-key"},
                "models":{
                    "small":{"tool_call":true,"limit":{"context":64000,"output":4096}},
                    "large":{"tool_call":true,"limit":{"context":1000000,"output":65536}},
                    "unknown":{"tool_call":true,"limit":{"context":0,"output":0}}
                }
            }}}))
            .unwrap();
        let mut config = Config {
            objective: "verify context selection".into(),
            provider: "fixture".into(),
            model: "small".into(),
            base_url: String::new(),
            ..Config::default()
        };
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        assert_eq!(config.context_budget, 64000);
        for (model, budget) in [("large", 1000000), ("small", 64000), ("unknown", 32000)] {
            config =
                resolve_live_selection(&catalog, &config, "fixture", model, None, &auth).unwrap();
            assert_eq!(config.context_budget, budget, "{model}");
        }
        config.context_budget = 24000;
        config.explicit_context_budget = true;
        config =
            resolve_live_selection(&catalog, &config, "fixture", "large", None, &auth).unwrap();
        assert_eq!(config.context_budget, 24000);
    }

    #[tokio::test]
    async fn remembered_context_defaults_follow_catalog_and_cli_overrides_take_precedence() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = std::fs::canonicalize(temp.path()).unwrap();
        let config_path = workspace.join("models.json");
        std::fs::write(
            &config_path,
            json!({"provider":{"fixture":{
                "options":{"baseURL":"http://localhost/v1","apiKey":"fixture-key"},
                "models":{"large":{"tool_call":true,"limit":{"context":1000000,"output":64000}}}
            }}})
            .to_string(),
        )
        .unwrap();
        for (explicit, override_budget, expected) in [
            (false, None, 1000000),
            (true, None, 48000),
            (false, Some("24000"), 24000),
            (true, Some("24000"), 24000),
        ] {
            let mut auth = empty_auth(temp.path());
            auth.set_selection("fixture", "large", None);
            auth.remember_launch(&Config {
                workspace: workspace.clone(),
                database: workspace.join("remembered.sqlite3"),
                objective: "verify remembered context".into(),
                provider: "fixture".into(),
                model: "large".into(),
                base_url: "http://localhost/v1".into(),
                config_path: Some(config_path.clone()),
                context_budget: 48000,
                explicit_context_budget: explicit,
                ..Config::default()
            });
            let mut arguments = vec![
                "openraid",
                "setup",
                "verify remembered context",
                "--no-tui",
                "--workspace",
                workspace.to_str().unwrap(),
            ];
            if let Some(budget) = override_budget {
                arguments.extend(["--context-budget", budget]);
            }
            let Some(Command::Setup(args)) = Cli::parse_from(arguments).command else {
                panic!("setup arguments")
            };
            let config = args
                .config_with_auth(false, true, auth)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(config.context_budget, expected);
            assert_eq!(
                config.explicit_context_budget,
                explicit || override_budget.is_some()
            );
        }
    }

    #[test]
    fn custom_model_alias_defaults_variants_and_explicit_overrides_reach_wire_config() {
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::from_json("{}").unwrap();
        catalog.apply_config(&json!({"provider":{"local":{
            "npm":"@ai-sdk/openai", "options":{"baseURL":"http://localhost:2455/v1","apiKey":"configured-key","headers":{"x-shared":"provider"}},
            "models":{"alias":{"id":"wire-model","name":"Friendly model","tool_call":true,"reasoning":true,
                "limit":{"context":64000,"output":8192},"options":{"temperature":0.4},
                "headers":{"X-Shared":"model"},"variants":{"custom":{"reasoningEffort":"high","textVerbosity":"low"}}}}
        }}})).unwrap();
        let mut config = Config {
            provider: "local".into(),
            model: "alias".into(),
            variant: Some("custom".into()),
            provider_options: json!({"textVerbosity":"high"}),
            provider_headers: BTreeMap::from([("x-shared".into(), "explicit".into())]),
            base_url: String::new(),
            api_key: Some("explicit-key".into()),
            ..Config::default()
        };
        configure_provider_with_auth(&catalog, &mut config, None, &empty_auth(temp.path()))
            .unwrap();
        assert_eq!(config.model, "alias");
        assert_eq!(config.api_model.as_deref(), Some("wire-model"));
        assert_eq!(config.protocol, Protocol::Responses);
        assert_eq!(config.api_key.as_deref(), Some("explicit-key"));
        assert_eq!(config.provider_options["temperature"], 0.4);
        assert_eq!(config.provider_options["reasoningEffort"], "high");
        assert_eq!(config.provider_options["textVerbosity"], "high");
        assert_eq!(config.provider_headers["x-shared"], "explicit");
        assert_eq!(config.provider_headers.len(), 1);
    }

    #[test]
    fn sdk_factory_settings_and_unresolved_endpoint_are_kept_separate_from_generation() {
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = Catalog::from_json("{}").unwrap();
        catalog.apply_config(&json!({"provider":{"cloud-test":{
            "npm":"@ai-sdk/google-vertex", "options":{"baseURL":"https://{UNSET_OPENRAID_TEST_HOST}/v1","project":"project-from-config","location":"global"},
            "models":{"model":{"tool_call":true,"options":{"temperature":0.8},"limit":{"context":2048,"output":512}}}
        }}})).unwrap();
        let mut config = Config {
            provider: "cloud-test".into(),
            model: "model".into(),
            base_url: String::new(),
            provider_options: json!({"sdkSettings":{"location":"us-central1"}}),
            ..Config::default()
        };
        configure_provider_with_auth(&catalog, &mut config, None, &empty_auth(temp.path()))
            .unwrap();
        assert_eq!(config.protocol, Protocol::Sdk);
        assert!(config.base_url.is_empty());
        assert!(config.provider_options["sdkSettings"]
            .get("baseURL")
            .is_none());
        assert_eq!(
            config.provider_options["sdkSettings"]["project"],
            "project-from-config"
        );
        assert_eq!(
            config.provider_options["sdkSettings"]["location"],
            "us-central1"
        );
        assert_eq!(config.provider_options["temperature"], 0.8);
        assert_eq!(config.context_budget, 2048);
        assert_eq!(config.max_output_tokens, 512);
    }

    #[test]
    fn jsonc_config_file_references_resolve_relative_to_config_directory() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("settings");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("key.txt"), "local-file-key\n").unwrap();
        let path = directory.join("provider.jsonc");
        std::fs::write(&path, r#"{
            // OpenCode-shaped custom provider, with trailing commas.
            "provider": {"file-test": {"options": {"apiKey": "{file:key.txt}", "baseURL": "http://localhost/v1"},
                "models": {"local-model": {"name": "Local model",}},}},
        }"#).unwrap();
        let catalog = load_catalog(temp.path(), Some(&path)).unwrap();
        assert_eq!(
            catalog.provider("file-test").unwrap().options["apiKey"],
            "local-file-key"
        );
    }

    #[test]
    fn copilot_claude_oauth_uses_native_thinking_variants_with_effective_output_budget() {
        let temp = tempfile::tempdir().unwrap();
        let mut auth = empty_auth(temp.path());
        auth.set_oauth(
            "github-copilot",
            &json!({"type":"oauth","refresh":"test-github-token"}),
        )
        .unwrap();
        let mut catalog = Catalog::from_json("{}").unwrap();
        catalog.apply_config(&json!({"provider":{"github-copilot":{
            "npm":"@ai-sdk/github-copilot","models":{"claude-sonnet-4.6":{"reasoning":true,"tool_call":true,
                "limit":{"context":200000,"output":64000}}}
        }}})).unwrap();
        let mut config = Config {
            provider: "github-copilot".into(),
            model: "claude-sonnet-4.6".into(),
            variant: Some("high".into()),
            base_url: String::new(),
            ..Config::default()
        };
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        assert!(config.oauth.is_some());
        assert_eq!(config.protocol, Protocol::Anthropic);
        assert_eq!(config.provider_npm, "@ai-sdk/anthropic");
        assert_eq!(config.provider_options["thinking"]["type"], "adaptive");
        assert_eq!(config.provider_options["effort"], "high");
        assert!(config.provider_options.get("reasoningEffort").is_none());
        assert!(config.provider_headers.contains_key("anthropic-beta"));
        auth.set_oauth("openai", &json!({"type":"oauth","access":"test-codex-token","refresh":"test-refresh","expires":u64::MAX})).unwrap();
        catalog.apply_config(&json!({"provider":{"openai":{"npm":"@ai-sdk/openai","models":{
            "gpt-5.4-pro-mode":{"id":"gpt-5.4","reasoning":true,"tool_call":true,"options":{"reasoningMode":"pro"},"limit":{"context":200000,"output":32000}}
        }}}})).unwrap();
        let mut pro = Config {
            provider: "openai".into(),
            model: "gpt-5.4-pro-mode".into(),
            base_url: String::new(),
            ..Config::default()
        };
        let error = configure_provider_with_auth(&catalog, &mut pro, None, &auth).unwrap_err();
        assert!(error.to_string().contains("pro"));
    }

    #[test]
    fn anthropic_thinking_variants_fit_run_reservation_and_preserve_custom_budgets() {
        let mut catalog = Catalog::from_json("{}").unwrap();
        catalog.apply_config(&json!({"provider":{"anthropic-test":{"npm":"@ai-sdk/anthropic","models":{
            "claude-sonnet-4":{"reasoning":true,"tool_call":true,"limit":{"context":200000,"output":64000},
                "variants":{"custom":{"thinking":{"type":"enabled","budgetTokens":12000}}}}
        }}}})).unwrap();
        let model = catalog.model("anthropic-test", "claude-sonnet-4").unwrap();
        let variants = effective_variants(model, 4096, &model.npm);
        assert!(
            variants["high"]["thinking"]["budgetTokens"]
                .as_u64()
                .unwrap()
                < 4096
        );
        assert!(
            variants["max"]["thinking"]["budgetTokens"]
                .as_u64()
                .unwrap()
                < 4096
        );
        assert_eq!(variants["custom"]["thinking"]["budgetTokens"], 12000);
        let mut config = Config {
            provider_options: variants["custom"].clone(),
            ..Config::default()
        };
        adjust_thinking_budget(&mut config, 64000).unwrap();
        assert!(config.max_output_tokens > 12000);
    }

    #[test]
    fn catalog_json_and_review_endpoint_never_expose_credentials() {
        let value = redact_json(
            json!({"options":{"apiKey":"secret","headers":{"Authorization":"Bearer private"}},"nested":[{"refresh":"private-token"}],"name":"Example"}),
        );
        let text = value.to_string();
        assert!(!text.contains("secret"));
        assert!(!text.contains("private"));
        assert_eq!(value["name"], "Example");
        assert_eq!(
            public_endpoint("https://user:password@example.com/v1?api_key=private"),
            "https://example.com/v1"
        );
    }
}
