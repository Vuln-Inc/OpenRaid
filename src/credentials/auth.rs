//! Local credentials and remembered launcher choices. OpenCode API credentials
//! are read as a fallback, so a connected provider works in either application.

use crate::config::Config;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedSelection {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct LaunchProfile {
    pub agents: usize,
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub base_url: String,
    #[serde(default)]
    pub api_model: Option<String>,
    pub protocol: crate::provider::Protocol,
    pub provider_npm: String,
    pub provider_options: serde_json::Value,
    pub provider_headers: BTreeMap<String, String>,
    pub context_budget: usize,
    pub explicit_context_budget: bool,
    pub max_output_tokens: u32,
    pub max_in_flight: usize,
    pub max_processes: usize,
    pub grace_secs: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<PathBuf>,
}

impl LaunchProfile {
    pub fn apply(&self, config: &mut Config) {
        config.agents = self.agents;
        config.workspace = self.workspace.clone();
        config.database = self.database.clone();
        config.base_url = self.base_url.clone();
        config.api_model = self.api_model.clone();
        config.protocol = self.protocol;
        config.provider_npm = self.provider_npm.clone();
        config.provider_options = self.provider_options.clone();
        config.provider_headers = self.provider_headers.clone();
        config.context_budget = self.context_budget;
        config.explicit_context_budget = self.explicit_context_budget;
        config.max_output_tokens = self.max_output_tokens;
        config.max_in_flight = self.max_in_flight;
        config.max_processes = self.max_processes;
        config.config_path = self.config_path.clone();
        config.grace_period = std::time::Duration::try_from_secs_f64(self.grace_secs)
            .unwrap_or(std::time::Duration::from_secs(5));
    }
}

// Deliberately no Debug implementation: errors and diagnostics must never print
// credentials, including inherited OpenCode tokens.
#[derive(Default, Serialize, Deserialize)]
struct AuthData {
    #[serde(default)]
    providers: BTreeMap<String, Credential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selection: Option<SavedSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launch: Option<LaunchProfile>,
    #[serde(default)]
    endpoints: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Credential {
    Api {
        key: String,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        metadata: serde_json::Value,
    },
    Oauth {
        #[serde(flatten)]
        fields: BTreeMap<String, serde_json::Value>,
    },
    #[serde(other)]
    Unsupported,
}

pub struct AuthStore {
    path: PathBuf,
    opencode_path: Option<PathBuf>,
    data: AuthData,
}

impl AuthStore {
    pub fn load_default() -> Result<Self> {
        Self::load(default_auth_path()?)
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        Self::load_with_opencode(path, default_opencode_path())
    }

    /// A separate OpenCode path also makes migration and isolated installations
    /// possible without changing global process environment variables.
    pub fn load_with_opencode(
        path: impl Into<PathBuf>,
        opencode_path: Option<PathBuf>,
    ) -> Result<Self> {
        let path = path.into();
        let data = read_json(&path)?.unwrap_or_default();
        Ok(Self {
            path,
            opencode_path,
            data,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn import_path(&self) -> Option<&Path> {
        self.opencode_path.as_deref()
    }
    pub fn connection_endpoint(&self, provider: &str) -> Option<&str> {
        self.data.endpoints.get(provider).map(String::as_str)
    }
    pub fn set_connection_endpoint(&mut self, provider: &str, endpoint: &str) {
        self.data
            .endpoints
            .insert(provider.to_owned(), endpoint.to_owned());
    }

    /// Resolve provider credentials in the same order as an explicit connected
    /// provider: environment, OpenRaid credentials, then OpenCode credentials.
    /// OAuth tokens are not API keys and cannot be used by these transports.
    pub fn api_key(&self, provider_id: &str, env_names: &[String]) -> Result<Option<String>> {
        self.api_key_with_env(provider_id, env_names, &|name| env::var(name).ok())
    }

    fn api_key_with_env(
        &self,
        provider_id: &str,
        env_names: &[String],
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<String>> {
        let loader_env: &[&str] = match provider_id {
            "snowflake-cortex" => &["SNOWFLAKE_CORTEX_TOKEN", "SNOWFLAKE_CORTEX_PAT"],
            "cloudflare-ai-gateway" => &["CLOUDFLARE_API_TOKEN", "CF_AIG_TOKEN"],
            _ => &[],
        };
        for name in loader_env
            .iter()
            .copied()
            .chain(env_names.iter().map(String::as_str))
        {
            if !is_api_key_env(name) {
                continue;
            }
            if let Some(key) = lookup(name) {
                if !key.trim().is_empty() {
                    return Ok(Some(key));
                }
            }
        }
        if let Some(credential) = self.data.providers.get(provider_id) {
            return credential_key(credential, provider_id);
        }
        if let Some(path) = &self.opencode_path {
            let imported: BTreeMap<String, Credential> = read_json(path)?.unwrap_or_default();
            if let Some(credential) = imported.get(provider_id) {
                return credential_key(credential, provider_id);
            }
        }
        Ok(None)
    }

    pub fn set_api_key(&mut self, provider_id: &str, key: &str) -> Result<()> {
        if provider_id.trim().is_empty() {
            bail!("provider ID must not be empty");
        }
        if key.trim().is_empty() {
            bail!("API key must not be empty");
        }
        self.data.providers.insert(
            provider_id.to_owned(),
            Credential::Api {
                key: key.to_owned(),
                metadata: self.provider_metadata(provider_id)?,
            },
        );
        Ok(())
    }

    /// OAuth material is intentionally available only through this dedicated
    /// accessor; callers must refresh/exchange it for the provider's protocol.
    pub fn oauth(&self, provider_id: &str) -> Result<Option<serde_json::Value>> {
        if let Some(credential) = self.data.providers.get(provider_id) {
            return oauth_value(credential);
        }
        if let Some(path) = &self.opencode_path {
            let imported: BTreeMap<String, Credential> = read_json(path)?.unwrap_or_default();
            if let Some(credential) = imported.get(provider_id) {
                return oauth_value(credential);
            }
        }
        Ok(None)
    }

    /// OpenCode connects some providers with non-secret account identifiers
    /// alongside the API key (Azure resourceName, Cloudflare accountId/gatewayId).
    pub fn provider_metadata(&self, provider_id: &str) -> Result<serde_json::Value> {
        if let Some(credential) = self.data.providers.get(provider_id) {
            return Ok(credential_metadata(credential));
        }
        if let Some(path) = &self.opencode_path {
            let imported: BTreeMap<String, Credential> = read_json(path)?.unwrap_or_default();
            if let Some(credential) = imported.get(provider_id) {
                return Ok(credential_metadata(credential));
            }
        }
        Ok(serde_json::json!({}))
    }

    /// Refreshed inherited credentials are saved in OpenRaid's file, leaving
    /// the OpenCode account and any other provider records intact.
    pub fn set_oauth(&mut self, provider_id: &str, value: &serde_json::Value) -> Result<()> {
        if provider_id.trim().is_empty() {
            bail!("provider ID must not be empty");
        }
        let credential: Credential =
            serde_json::from_value(value.clone()).context("invalid OAuth credential record")?;
        if !matches!(credential, Credential::Oauth { .. }) {
            bail!("OAuth credential record must have type oauth");
        }
        self.data
            .providers
            .insert(provider_id.to_owned(), credential);
        Ok(())
    }

    pub fn remove_api_key(&mut self, provider_id: &str) -> bool {
        self.data.providers.remove(provider_id).is_some()
    }

    /// IDs only; this is safe to display in provider-management commands.
    pub fn connected_providers(&self) -> Vec<&str> {
        self.data.providers.keys().map(String::as_str).collect()
    }

    pub fn selection(&self) -> Option<&SavedSelection> {
        self.data.selection.as_ref()
    }

    pub fn launch_profile(&self) -> Option<&LaunchProfile> {
        self.data.launch.as_ref()
    }

    pub fn remember_launch(&mut self, config: &Config) {
        self.set_selection(&config.provider, &config.model, config.variant.as_deref());
        self.data.launch = Some(LaunchProfile {
            agents: config.agents,
            workspace: config.workspace.clone(),
            database: config.database.clone(),
            base_url: config.base_url.clone(),
            api_model: config.api_model.clone(),
            protocol: config.protocol,
            provider_npm: config.provider_npm.clone(),
            provider_options: config.provider_options.clone(),
            provider_headers: config.provider_headers.clone(),
            context_budget: config.context_budget,
            explicit_context_budget: config.explicit_context_budget,
            max_output_tokens: config.max_output_tokens,
            max_in_flight: config.max_in_flight,
            max_processes: config.max_processes,
            grace_secs: config.grace_period.as_secs_f64(),
            config_path: config.config_path.clone(),
        });
    }

    pub fn set_selection(&mut self, provider: &str, model: &str, variant: Option<&str>) {
        self.data.selection = Some(SavedSelection {
            provider: provider.to_owned(),
            model: model.to_owned(),
            variant: variant.filter(|value| !value.is_empty()).map(str::to_owned),
        });
    }

    /// Write through a private temporary file and rename only after a complete
    /// flush, preserving the previous credentials if serialization fails.
    pub fn save(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("creating credential directory {}", parent.display()))?;
        let payload = serde_json::to_vec_pretty(&self.data).context("serializing credentials")?;
        let name = self
            .path
            .file_name()
            .context("credential file must have a filename")?
            .to_string_lossy();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let temporary = parent.join(format!(".{name}.{}.{nonce}.tmp", std::process::id()));
        let result = (|| -> Result<()> {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary)
                .context("creating private credential file")?;
            file.write_all(&payload).context("writing credentials")?;
            file.write_all(b"\n")?;
            file.sync_all().context("flushing credentials")?;
            drop(file);
            fs::rename(&temporary, &self.path).context("replacing credential file")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

/// Models.dev's `env` list includes cloud connection settings and signing
/// credential chains as well as API keys. Only the latter may become bearer
/// authentication; SDK adapters consume the former from their environment.
pub fn is_api_key_env(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    if matches!(
        name.as_str(),
        "AWS_ACCESS_KEY_ID"
            | "AWS_SECRET_ACCESS_KEY"
            | "AWS_SESSION_TOKEN"
            | "GOOGLE_APPLICATION_CREDENTIALS"
            | "AZURE_CLIENT_SECRET"
            | "AZURE_CREDENTIALS"
    ) {
        return false;
    }
    ![
        "_PROJECT",
        "_PROJECT_ID",
        "_LOCATION",
        "_REGION",
        "_PROFILE",
        "_HOST",
        "_URL",
        "_ENDPOINT",
        "_ACCOUNT",
        "_ACCOUNT_ID",
        "_GATEWAY_ID",
        "_RESOURCE_NAME",
        "_PRODUCT_ID",
        "_DEPLOYMENT_ID",
        "_INSTANCE_URL",
        "_TENANT_ID",
        "_CLIENT_ID",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

pub fn resolve_api_key(
    provider_id: &str,
    env_names: &[String],
    explicit: Option<&str>,
) -> Result<Option<String>> {
    if let Some(key) = explicit {
        if key.trim().is_empty() {
            bail!("API key must not be empty");
        }
        return Ok(Some(key.to_owned()));
    }
    AuthStore::load_default()?.api_key(provider_id, env_names)
}

pub fn default_auth_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("OPENRAID_AUTH_FILE").filter(|value| !value.is_empty()) {
        return Ok(path.into());
    }
    Ok(data_directory()
        .context("set OPENRAID_AUTH_FILE or a home directory to store provider credentials")?
        .join("openraid/auth.json"))
}

pub fn default_opencode_path() -> Option<PathBuf> {
    data_directory().map(|path| path.join("opencode/auth.json"))
}

fn data_directory() -> Option<PathBuf> {
    if let Some(path) = env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Some(path.into());
    }
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
        .map(|path| PathBuf::from(path).join(".local/share"))
}

fn credential_key(credential: &Credential, provider_id: &str) -> Result<Option<String>> {
    match credential {
        Credential::Api { key, .. } if !key.trim().is_empty() => Ok(Some(key.clone())),
        Credential::Api { .. } => Ok(None),
        Credential::Oauth { .. } | Credential::Unsupported => bail!("provider {provider_id} uses OAuth or another unsupported credential type; resolve its OAuth account or connect an API key"),
    }
}

fn credential_metadata(credential: &Credential) -> serde_json::Value {
    match credential {
        Credential::Api { metadata, .. } if metadata.is_object() => metadata.clone(),
        _ => serde_json::json!({}),
    }
}

fn oauth_value(credential: &Credential) -> Result<Option<serde_json::Value>> {
    match credential {
        Credential::Oauth { .. } => Ok(Some(serde_json::to_value(credential)?)),
        _ => Ok(None),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("reading credentials {}", path.display()))
        }
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid credentials file {}", path.display()))
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_loader_token_aliases_override_saved_keys_without_treating_account_as_secret(
    ) -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut auth = AuthStore::load_with_opencode(temp.path().join("auth.json"), None)?;
        auth.set_api_key("snowflake-cortex", "saved")?;
        let env = BTreeMap::from([
            ("SNOWFLAKE_ACCOUNT", "account"),
            ("SNOWFLAKE_CORTEX_TOKEN", "token"),
            ("SNOWFLAKE_CORTEX_PAT", "pat"),
            ("CF_AIG_TOKEN", "gateway-token"),
        ]);
        let lookup = |name: &str| env.get(name).map(|value| (*value).to_owned());
        assert_eq!(
            auth.api_key_with_env(
                "snowflake-cortex",
                &["SNOWFLAKE_ACCOUNT".into(), "SNOWFLAKE_CORTEX_PAT".into()],
                &lookup
            )?
            .as_deref(),
            Some("token")
        );
        assert_eq!(
            auth.api_key_with_env(
                "cloudflare-ai-gateway",
                &["CLOUDFLARE_ACCOUNT_ID".into()],
                &lookup
            )?
            .as_deref(),
            Some("gateway-token")
        );
        Ok(())
    }

    #[test]
    fn cloud_signing_and_project_settings_never_become_api_keys() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
        let env = BTreeMap::from([
            ("AWS_ACCESS_KEY_ID", "signing-id"),
            ("AWS_SECRET_ACCESS_KEY", "signing-secret"),
            ("AWS_REGION", "us-east-1"),
            ("GOOGLE_VERTEX_PROJECT", "project-not-a-token"),
            ("AZURE_RESOURCE_NAME", "resource-not-a-token"),
            ("AWS_BEARER_TOKEN_BEDROCK", "bearer-token"),
            ("ANTHROPIC_API_KEY", "env-api-key"),
        ]);
        let lookup = |name: &str| env.get(name).map(|value| (*value).to_owned());
        for (provider, names) in [
            (
                "amazon-bedrock",
                vec!["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_REGION"],
            ),
            ("google-vertex", vec!["GOOGLE_VERTEX_PROJECT"]),
            ("azure", vec!["AZURE_RESOURCE_NAME"]),
        ] {
            let names = names.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(store.api_key_with_env(provider, &names, &lookup)?.is_none());
        }
        assert_eq!(
            store
                .api_key_with_env(
                    "amazon-bedrock",
                    &["AWS_BEARER_TOKEN_BEDROCK".into()],
                    &lookup
                )?
                .as_deref(),
            Some("bearer-token")
        );
        store.set_api_key("anthropic", "saved-api-key")?;
        assert_eq!(
            store
                .api_key_with_env("anthropic", &["ANTHROPIC_API_KEY".into()], &lookup)?
                .as_deref(),
            Some("env-api-key")
        );
        Ok(())
    }

    #[test]
    fn credentials_and_selection_survive_atomic_replacement() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("nested/auth.json");
        let mut store = AuthStore::load_with_opencode(&path, None)?;
        store.set_api_key("anthropic", "test-secret")?;
        store.set_selection("anthropic", "claude-sonnet-4-5", Some("high"));
        store.save()?;
        store.set_api_key("anthropic", "replacement-secret")?;
        store.save()?;
        let restored = AuthStore::load_with_opencode(&path, None)?;
        assert_eq!(
            restored.api_key("anthropic", &[])?.as_deref(),
            Some("replacement-secret")
        );
        assert_eq!(
            restored.selection().unwrap().variant.as_deref(),
            Some("high")
        );
        assert_eq!(restored.connected_providers(), vec!["anthropic"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        }
        Ok(())
    }

    #[test]
    fn opencode_api_import_fallback_and_oauth_are_distinct() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let opencode = directory.path().join("opencode.json");
        fs::write(
            &opencode,
            r#"{"openai":{"type":"api","key":"inherited"},"anthropic":{"type":"oauth","access":"not-an-api-key","refresh":"secret","expires":42}}"#,
        )?;
        let mut store =
            AuthStore::load_with_opencode(directory.path().join("auth.json"), Some(opencode))?;
        assert_eq!(store.api_key("openai", &[])?.as_deref(), Some("inherited"));
        let error = store.api_key("anthropic", &[]).unwrap_err().to_string();
        assert!(error.contains("OAuth"));
        assert!(!error.contains("not-an-api-key"));
        let mut account = store.oauth("anthropic")?.unwrap();
        account["access"] = serde_json::json!("refreshed-access");
        account["accountId"] = serde_json::json!("account-1");
        store.set_oauth("anthropic", &account)?;
        store.save()?;
        let reloaded = AuthStore::load_with_opencode(store.path(), None)?;
        assert_eq!(
            reloaded.oauth("anthropic")?.unwrap()["accountId"],
            "account-1"
        );
        assert_eq!(
            reloaded.oauth("anthropic")?.unwrap()["access"],
            "refreshed-access"
        );
        store.set_api_key("openai", "local")?;
        assert_eq!(store.api_key("openai", &[])?.as_deref(), Some("local"));
        assert!(store.remove_api_key("openai"));
        assert_eq!(store.api_key("openai", &[])?.as_deref(), Some("inherited"));
        assert!(store.api_key("missing", &[])?.is_none());
        Ok(())
    }

    #[test]
    fn corrupt_credentials_are_not_silently_replaced() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("auth.json");
        fs::write(&path, "not valid json")?;
        assert!(AuthStore::load_with_opencode(&path, None).is_err());
        assert_eq!(fs::read_to_string(&path)?, "not valid json");
        Ok(())
    }
}
