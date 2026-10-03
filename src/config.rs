use anyhow::{bail, Result};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

/// Keep SQLite and its WAL/SHM sidecars together inside the workspace.
/// Root-level databases are ignored unless selected explicitly.
pub fn default_database_path(workspace: &Path) -> PathBuf {
    workspace.join(".openraid").join("openraid.sqlite3")
}

/// Shared settings for the single-process swarm. The grace period applies only
/// to completion consensus, never to an operation or provider request.
#[derive(Clone)]
pub struct Config {
    pub agents: usize,
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub objective: String,
    pub mock: bool,
    pub model: String,
    /// Upstream identifier, when a catalog/config model uses a display alias.
    pub api_model: Option<String>,
    pub base_url: String,
    pub api_key: Option<String>,
    pub oauth: Option<crate::oauth::OAuthSession>,
    pub provider: String,
    pub variant: Option<String>,
    pub provider_npm: String,
    pub protocol: crate::provider::Protocol,
    pub provider_options: serde_json::Value,
    pub provider_headers: BTreeMap<String, String>,
    pub max_in_flight: usize,
    pub max_processes: usize,
    pub context_budget: usize,
    pub max_output_tokens: u32,
    pub grace_period: Duration,
    pub no_tui: bool,
    pub resume: bool,
    /// Interactive home stays available between completed work batches.
    pub interactive_session: bool,
    pub explicit_context_budget: bool,
    /// Explicit provider/MCP configuration retained for live menus and remembered launches.
    pub config_path: Option<PathBuf>,
    pub mcp: BTreeMap<String, crate::mcp::ServerConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agents: 8,
            workspace: PathBuf::from("."),
            database: default_database_path(Path::new(".")),
            objective: String::new(),
            mock: false,
            model: "gpt-4.1-mini".into(),
            api_model: None,
            base_url: "https://api.openai.com/v1".into(),
            api_key: None,
            oauth: None,
            provider: "openai".into(),
            variant: None,
            provider_npm: "@ai-sdk/openai".into(),
            protocol: crate::provider::Protocol::ChatCompletions,
            provider_options: serde_json::json!({}),
            provider_headers: BTreeMap::new(),
            max_in_flight: 32,
            max_processes: 4,
            context_budget: 32_000,
            max_output_tokens: 4_096,
            grace_period: Duration::from_secs(5),
            no_tui: false,
            resume: false,
            interactive_session: false,
            explicit_context_budget: false,
            config_path: None,
            mcp: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if !(1..=500).contains(&self.agents) {
            bail!("agent count must be between 1 and 500");
        }
        if self.max_in_flight == 0 || self.max_processes == 0 {
            bail!("provider and process concurrency must be positive");
        }
        if self.context_budget < 1024 {
            bail!("context budget must be at least 1024 tokens");
        }
        if self.max_output_tokens == 0 {
            bail!("maximum output tokens must be positive");
        }
        if self.max_output_tokens as usize >= self.context_budget {
            bail!("maximum output tokens must be smaller than the context budget");
        }
        if self.objective.trim().is_empty() && !self.interactive_session {
            bail!("an objective is required");
        }
        if !self.workspace.is_dir() {
            bail!(
                "workspace must be an existing directory: {}",
                self.workspace.display()
            );
        }
        if !self.mock
            && self.protocol != crate::provider::Protocol::Sdk
            && self.base_url.trim().is_empty()
        {
            bail!("provider base URL must not be empty");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::default_database_path;

    #[test]
    fn default_store_is_nested_even_when_root_store_exists() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join(".openraid").join("openraid.sqlite3");
        let legacy = directory.path().join("openraid.sqlite3");
        assert_eq!(default_database_path(directory.path()), nested);
        assert!(!nested.parent().unwrap().exists());

        std::fs::write(&legacy, "legacy").unwrap();
        assert_eq!(default_database_path(directory.path()), nested);
        assert!(!nested.parent().unwrap().exists());
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "legacy");

        std::fs::create_dir(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "nested").unwrap();
        assert_eq!(default_database_path(directory.path()), nested);
        assert_eq!(std::fs::read_to_string(legacy).unwrap(), "legacy");
    }
}
