//! Reuse OpenCode OAuth accounts without confusing account tokens with API keys.
//! A shared refresh lock keeps an entire swarm on one rotated Codex token.

use crate::{auth::AuthStore, provider::Protocol};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_ISSUER: &str = "https://auth.openai.com";
const CODEX_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex";
const COPILOT_CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";

/// Headless OAuth login. Only the human-facing code and URL are public; device
/// grants remain private and are exchanged without opening a browser process.
pub struct DeviceLogin {
    pub verification_uri: String,
    pub user_code: String,
    provider: String,
    device_code: String,
    origin: String,
    enterprise_domain: Option<String>,
    interval: u64,
    deadline: Option<tokio::time::Instant>,
    client: reqwest::Client,
}

pub async fn begin_login(provider_id: &str, enterprise_url: Option<&str>) -> Result<DeviceLogin> {
    let origin = match provider_id {
        "openai" if enterprise_url.is_none() => CODEX_ISSUER.to_owned(),
        "openai" => bail!("--enterprise-url applies only to GitHub Copilot"),
        "github-copilot" | "github-copilot-enterprise" => {
            if let Some(domain) = enterprise_url {
                let parsed = reqwest::Url::parse(&if domain.contains("://") { domain.to_owned() } else { format!("https://{domain}") })
                    .context("invalid GitHub enterprise domain")?;
                if parsed.host_str().is_none() || parsed.username() != "" || parsed.password().is_some()
                    || parsed.scheme() != "https" || !matches!(parsed.path(), "" | "/") || parsed.query().is_some() || parsed.fragment().is_some()
                { bail!("GitHub enterprise domain must be an HTTPS hostname, such as company.ghe.com"); }
                format!("https://{}", parsed.host_str().unwrap())
            } else if provider_id == "github-copilot-enterprise" { bail!("GitHub Copilot Enterprise login requires --enterprise-url"); }
            else { "https://github.com".to_owned() }
        }
        _ => bail!("device OAuth login supports openai, github-copilot, and github-copilot-enterprise; connect other providers with an API key"),
    };
    begin_login_at(provider_id, origin, enterprise_url.is_some()).await
}

async fn begin_login_at(
    provider_id: &str,
    origin: String,
    enterprise: bool,
) -> Result<DeviceLogin> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("openraid/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let codex = provider_id == "openai";
    let path = if codex {
        "/api/accounts/deviceauth/usercode"
    } else {
        "/login/device/code"
    };
    let body = if codex {
        json!({"client_id":CODEX_CLIENT_ID})
    } else {
        json!({"client_id":COPILOT_CLIENT_ID,"scope":"read:user"})
    };
    let response = client
        .post(format!("{origin}{path}"))
        .header("Accept", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("OAuth device authorization connection failed"))?;
    if !response.status().is_success() {
        bail!(
            "OAuth device authorization returned HTTP {}",
            response.status()
        );
    }
    let result: Value = response
        .json()
        .await
        .context("OAuth device authorization returned invalid JSON")?;
    let user_code = token(&result, "user_code")
        .context("OAuth device authorization returned no user code")?
        .to_owned();
    let device_code = token(
        &result,
        if codex {
            "device_auth_id"
        } else {
            "device_code"
        },
    )
    .context("OAuth device authorization returned no device grant")?
    .to_owned();
    let verification_uri = if codex {
        format!("{origin}/codex/device")
    } else {
        token(&result, "verification_uri")
            .or_else(|| token(&result, "verification_url"))
            .context("OAuth device authorization returned no verification URL")?
            .to_owned()
    };
    let interval = result["interval"]
        .as_u64()
        .or_else(|| result["interval"].as_str().and_then(|v| v.parse().ok()))
        .unwrap_or(5)
        .max(1);
    let deadline = result["expires_in"].as_u64().and_then(|seconds| {
        tokio::time::Instant::now().checked_add(std::time::Duration::from_secs(seconds))
    });
    let enterprise_domain = if enterprise {
        reqwest::Url::parse(&origin)?.host_str().map(str::to_owned)
    } else {
        None
    };
    Ok(DeviceLogin {
        verification_uri,
        user_code,
        provider: provider_id.to_owned(),
        device_code,
        origin,
        enterprise_domain,
        interval,
        deadline,
        client,
    })
}

impl DeviceLogin {
    /// Poll only at the authorization server's requested cadence. Dropping this
    /// future (for example on Ctrl+C) cancels the login without saving a grant.
    pub async fn wait(mut self) -> Result<Value> {
        let codex = self.provider == "openai";
        loop {
            if self
                .deadline
                .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
            {
                bail!("OAuth device login expired; start a new login");
            }
            let path = if codex {
                "/api/accounts/deviceauth/token"
            } else {
                "/login/oauth/access_token"
            };
            let body = if codex {
                json!({"device_auth_id":self.device_code,"user_code":self.user_code})
            } else {
                json!({"client_id":COPILOT_CLIENT_ID,"device_code":self.device_code,"grant_type":"urn:ietf:params:oauth:grant-type:device_code"})
            };
            let response = self
                .client
                .post(format!("{}{path}", self.origin))
                .header("Accept", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("OAuth device login polling connection failed"))?;
            if codex && matches!(response.status().as_u16(), 403 | 404) {
                tokio::time::sleep(std::time::Duration::from_secs(
                    self.interval.saturating_add(3),
                ))
                .await;
                continue;
            }
            if !response.status().is_success() {
                bail!("OAuth device login returned HTTP {}", response.status());
            }
            let result: Value = response
                .json()
                .await
                .context("OAuth device login returned invalid JSON")?;
            if codex {
                let code = token(&result, "authorization_code")
                    .context("Codex device login returned no authorization code")?;
                let verifier = token(&result, "code_verifier")
                    .context("Codex device login returned no code verifier")?;
                let redirect = format!("{}/deviceauth/callback", self.origin);
                let response = self
                    .client
                    .post(format!("{}/oauth/token", self.origin))
                    .form(&[
                        ("grant_type", "authorization_code"),
                        ("code", code),
                        ("redirect_uri", redirect.as_str()),
                        ("client_id", CODEX_CLIENT_ID),
                        ("code_verifier", verifier),
                    ])
                    .send()
                    .await
                    .map_err(|_| anyhow::anyhow!("Codex OAuth token exchange connection failed"))?;
                if !response.status().is_success() {
                    bail!(
                        "Codex OAuth token exchange returned HTTP {}",
                        response.status()
                    );
                }
                let tokens: Value = response
                    .json()
                    .await
                    .context("Codex OAuth token exchange returned invalid JSON")?;
                let access = token(&tokens, "access_token")
                    .context("Codex OAuth token exchange returned no access token")?;
                let refresh = token(&tokens, "refresh_token")
                    .context("Codex OAuth token exchange returned no refresh token")?;
                let mut credential = json!({"type":"oauth","access":access,"refresh":refresh,"expires":now_ms()?.saturating_add(tokens["expires_in"].as_u64().unwrap_or(3600).saturating_mul(1000))});
                if let Some(account) = account_id(&tokens) {
                    credential["accountId"] = json!(account);
                }
                return Ok(credential);
            }
            if let Some(access) = token(&result, "access_token") {
                let mut credential =
                    json!({"type":"oauth","access":access,"refresh":access,"expires":0});
                if let Some(domain) = self.enterprise_domain {
                    credential["enterpriseUrl"] = json!(domain);
                }
                return Ok(credential);
            }
            match result["error"].as_str() {
                Some("authorization_pending") => {}
                Some("slow_down") => {
                    self.interval = result["interval"]
                        .as_u64()
                        .filter(|v| *v > 0)
                        .unwrap_or_else(|| self.interval.saturating_add(5))
                }
                Some("access_denied") => bail!("GitHub OAuth device login was declined"),
                Some("expired_token") => {
                    bail!("GitHub OAuth device login expired; start a new login")
                }
                _ => bail!("GitHub OAuth device login could not complete; start a new login"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(
                self.interval.saturating_add(3),
            ))
            .await;
        }
    }
}

/// Deliberately does not implement Debug: access tokens must never enter logs.
pub struct OAuthAuthorization {
    pub api_key: String,
    pub headers: BTreeMap<String, String>,
    pub sdk_settings: Value,
}

#[derive(Clone)]
pub struct OAuthSession {
    provider: Arc<str>,
    base_url: Arc<str>,
    issuer: Arc<str>,
    credential: Arc<Mutex<Value>>,
    client: reqwest::Client,
    store_path: Option<std::path::PathBuf>,
    azure_authorized: Arc<AtomicBool>,
    azure_command: Option<std::path::PathBuf>,
    refresh_url: Option<Arc<str>>,
}

impl OAuthSession {
    pub fn from_store(provider_id: &str, store: &AuthStore) -> Result<Option<Self>> {
        let Some(credential) = store.oauth(provider_id)? else {
            return Ok(None);
        };
        let mut session = Self::from_credential(provider_id, credential)?;
        session.store_path = Some(store.path().to_owned());
        Ok(Some(session))
    }

    /// Useful for explicit account imports and isolated token-rotation tests.
    /// The caller owns persistence when constructing directly from a value.
    pub fn from_credential(provider_id: &str, mut credential: Value) -> Result<Self> {
        if credential["type"] != "oauth" {
            bail!("provider {provider_id} credential is not an OAuth account");
        }
        if !matches!(
            provider_id,
            "openai"
                | "github-copilot"
                | "github-copilot-enterprise"
                | "azure"
                | "snowflake-cortex"
                | "gitlab"
        ) {
            bail!("OAuth for {provider_id} requires its provider-specific OpenCode integration; use an API key or a custom endpoint");
        }
        if token(&credential, "access").is_none() && token(&credential, "refresh").is_none() {
            bail!("OAuth account for {provider_id} has no access or refresh token; reconnect it in OpenCode");
        }
        let base_url = if provider_id == "openai" {
            CODEX_ENDPOINT.to_owned()
        } else if provider_id == "snowflake-cortex" {
            crate::snowflake_oauth::api_base_url(&credential)?
        } else if provider_id == "gitlab" {
            crate::gitlab_oauth::instance_url(&credential)?
        } else if provider_id == "azure" {
            let resource = field(&credential, "accountId").or_else(|| field(&credential, "resourceName")).map(str::to_owned)
                .or_else(|| std::env::var("AZURE_RESOURCE_NAME").ok()).filter(|value| !value.is_empty())
                .context("Azure CLI OAuth account requires resourceName/accountId or AZURE_RESOURCE_NAME")?;
            if !resource
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                bail!("Azure resourceName must be a resource hostname label");
            }
            credential["accountId"] = json!(resource);
            format!("https://{resource}.openai.azure.com/openai/v1")
        } else if let Some(domain) = field(&credential, "enterpriseUrl") {
            let domain = domain.trim().trim_end_matches('/');
            let parsed = reqwest::Url::parse(&if domain.contains("://") {
                domain.to_owned()
            } else {
                format!("https://{domain}")
            })
            .context("invalid Copilot enterprise domain")?;
            if parsed.username() != ""
                || parsed.password().is_some()
                || parsed.host_str().is_none()
                || !matches!(parsed.scheme(), "https" | "http")
                || !matches!(parsed.path(), "" | "/")
            {
                bail!("Copilot enterprise domain must be a hostname, such as company.ghe.com");
            }
            format!("https://copilot-api.{}", parsed.host_str().unwrap())
        } else {
            "https://api.githubcopilot.com".to_owned()
        };
        Ok(Self {
            provider: Arc::from(provider_id),
            base_url: Arc::from(base_url),
            issuer: Arc::from(CODEX_ISSUER),
            credential: Arc::new(Mutex::new(credential)),
            client: reqwest::Client::builder()
                .user_agent(concat!("openraid/", env!("CARGO_PKG_VERSION")))
                .build()?,
            store_path: None,
            azure_authorized: Arc::new(AtomicBool::new(false)),
            azure_command: None,
            refresh_url: None,
        })
    }

    pub fn provider_id(&self) -> &str {
        &self.provider
    }

    pub fn is_codex(&self) -> bool {
        self.provider.as_ref() == "openai"
    }

    /// Preserve an explicitly configured Azure/Foundry endpoint while selecting
    /// the corresponding Entra scope. Other OAuth account routing stays fixed.
    pub fn with_base_url(mut self, base_url: &str) -> Result<Self> {
        if self.provider.as_ref() == "azure" && !base_url.is_empty() {
            let url = reqwest::Url::parse(base_url).context("invalid Azure OAuth endpoint")?;
            if !matches!(url.scheme(), "http" | "https") {
                bail!("Azure OAuth endpoint must use http or https");
            }
            self.base_url = Arc::from(base_url);
        }
        Ok(self)
    }

    /// Provider-specific body changes belong to the auth integration, keeping
    /// native transport code independent of individual account providers.
    pub fn prepare_request(&self, body: &mut Value) -> Result<()> {
        let Some(body) = body.as_object_mut() else {
            return Ok(());
        };
        match self.provider.as_ref() {
            "openai" => {
                if body
                    .get("reasoning")
                    .is_some_and(|reasoning| reasoning["mode"] == "pro")
                {
                    bail!("pro reasoning mode is unavailable with a ChatGPT Codex OAuth account; choose a supported mode or use an OpenAI API key");
                }
                body.remove("max_output_tokens");
            }
            "snowflake-cortex" => {
                if let Some(tokens) = body.remove("max_tokens") {
                    body.entry("max_completion_tokens").or_insert(tokens);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// An explicit Snowflake refresh URL supports isolated installations and loopback
    /// verification. It is never inferred from model request headers/options.
    pub fn with_refresh_url(mut self, refresh_url: &str) -> Result<Self> {
        if self.provider.as_ref() != "snowflake-cortex" {
            bail!("explicit refresh endpoints are supported for Snowflake OAuth only");
        }
        let url = reqwest::Url::parse(refresh_url).context("invalid OAuth refresh endpoint")?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("OAuth refresh endpoint must use http or https");
        }
        self.refresh_url = Some(Arc::from(refresh_url));
        Ok(self)
    }

    /// Refresh Snowflake once after an unexpected 401. If another admitted
    /// request has already rotated the rejected token, reuse that rotation.
    pub async fn refresh_after_unauthorized(
        &self,
        rejected_access: &str,
    ) -> Result<OAuthAuthorization> {
        if self.provider.as_ref() != "snowflake-cortex" {
            bail!("this OAuth provider does not support forced-refresh replay");
        }
        {
            let mut credential = self.credential.lock().await;
            if token(&credential, "access") == Some(rejected_access) {
                credential["expires"] = json!(0);
            }
        }
        self.authorization().await
    }

    /// Match the Codex OAuth plugin's model allowlist rather than offering API-
    /// key-only models (such as GPT-4.1) to a ChatGPT subscription account.
    pub fn supports_model(&self, api_id: &str) -> bool {
        if !self.is_codex() {
            return true;
        }
        if [
            "gpt-5.5",
            "gpt-5.3-codex-spark",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-6-sol",
            "gpt-6-luna",
        ]
        .contains(&api_id)
        {
            return true;
        }
        if ["gpt-5.5-pro", "gpt-5.6"].contains(&api_id) {
            return false;
        }
        let Some(version) = api_id.strip_prefix("gpt-") else {
            return false;
        };
        let mut parts = version.split(['.', '-']);
        let major: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let minor: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        major > 5 || (major == 5 && minor > 4)
    }

    /// Copilot exposes native Claude Messages and OpenAI-compatible models on
    /// different paths; Codex OAuth always uses Responses.
    pub fn protocol_and_base_url(&self, model_id: &str) -> (Protocol, String) {
        self.protocol_and_base_url_with_endpoint(model_id, None)
    }

    pub fn protocol_and_base_url_with_endpoint(
        &self,
        model_id: &str,
        endpoint: Option<&str>,
    ) -> (Protocol, String) {
        if self.provider.as_ref() == "openai" {
            return (Protocol::Responses, self.base_url.to_string());
        }
        if matches!(self.provider.as_ref(), "azure" | "gitlab") {
            return (Protocol::Sdk, self.base_url.to_string());
        }
        if self.provider.as_ref() == "snowflake-cortex" {
            return (Protocol::ChatCompletions, self.base_url.to_string());
        }
        match endpoint {
            Some("responses") => return (Protocol::Responses, self.base_url.to_string()),
            Some("chat") => return (Protocol::ChatCompletions, self.base_url.to_string()),
            _ => {}
        }
        let model = model_id.to_ascii_lowercase();
        if model.contains("claude") {
            (Protocol::Anthropic, format!("{}/v1", self.base_url))
        } else if model
            .strip_prefix("gpt-")
            .and_then(|version| version.split(['.', '-']).next())
            .and_then(|major| major.parse::<u32>().ok())
            .is_some_and(|major| major >= 5)
            && !model.starts_with("gpt-5-mini")
        {
            (Protocol::Responses, self.base_url.to_string())
        } else {
            (Protocol::ChatCompletions, self.base_url.to_string())
        }
    }

    /// Called after provider admission for every request/retry. Concurrent agent
    /// requests share the lock, so refresh-token rotation happens exactly once.
    pub async fn authorization(&self) -> Result<OAuthAuthorization> {
        let mut credential = self.credential.lock().await;
        if self.provider.as_ref() == "openai" {
            let now = now_ms()?;
            let expires = credential["expires"].as_u64().unwrap_or(0);
            if token(&credential, "access").is_none() || expires <= now.saturating_add(30_000) {
                let refresh = token(&credential, "refresh").context("Codex OAuth access expired without a refresh token; reconnect OpenAI in OpenCode")?;
                let response = self
                    .client
                    .post(format!("{}/oauth/token", self.issuer))
                    .form(&[
                        ("grant_type", "refresh_token"),
                        ("refresh_token", refresh),
                        ("client_id", CODEX_CLIENT_ID),
                    ])
                    .send()
                    .await
                    .map_err(|_| anyhow::anyhow!("Codex OAuth token refresh connection failed"))?;
                if !response.status().is_success() {
                    bail!("Codex OAuth token refresh returned HTTP {}; reconnect OpenAI in OpenCode if the account was revoked", response.status());
                }
                let tokens: Value = response
                    .json()
                    .await
                    .context("Codex OAuth token refresh returned invalid JSON")?;
                let access = token(&tokens, "access_token")
                    .context("Codex OAuth token refresh returned no access token")?;
                let account = account_id(&tokens).or_else(|| account_id(&credential));
                let mut updated = credential.clone();
                updated["access"] = json!(access);
                if let Some(refresh) = token(&tokens, "refresh_token") {
                    updated["refresh"] = json!(refresh);
                }
                updated["expires"] = json!(now_ms()?.saturating_add(
                    tokens["expires_in"]
                        .as_u64()
                        .unwrap_or(3_600)
                        .saturating_mul(1_000)
                ));
                if let Some(account) = account {
                    updated["accountId"] = json!(account);
                }
                self.persist(&updated)?;
                *credential = updated;
            }
        } else if self.provider.as_ref() == "snowflake-cortex" {
            let updated = if let Some(endpoint) = &self.refresh_url {
                crate::snowflake_oauth::refresh_if_needed_at(&self.client, &credential, endpoint)
                    .await?
            } else {
                crate::snowflake_oauth::refresh_if_needed(&self.client, &credential).await?
            };
            if *credential != updated {
                self.persist(&updated)?;
                *credential = updated;
            }
        } else if self.provider.as_ref() == "gitlab" {
            let updated = crate::gitlab_oauth::refresh_if_needed(&self.client, &credential).await?;
            if *credential != updated {
                self.persist(&updated)?;
                *credential = updated;
            }
        } else if self.provider.as_ref() == "azure"
            && (!self.azure_authorized.load(Ordering::Relaxed)
                || credential["expires"].as_u64().unwrap_or(0) <= now_ms()?.saturating_add(60_000))
        {
            let endpoint = reqwest::Url::parse(&self.base_url)?;
            let scope = if endpoint
                .host_str()
                .is_some_and(|host| host.ends_with(".services.ai.azure.com"))
                && !endpoint.path().starts_with("/models")
            {
                "https://ai.azure.com/.default"
            } else {
                "https://cognitiveservices.azure.com/.default"
            };
            let acquired = if let Some(command) = &self.azure_command {
                crate::azure_oauth::acquire_with_command(scope, command).await?
            } else {
                crate::azure_oauth::acquire(scope).await?
            };
            let mut updated = credential.clone();
            updated["access"] = json!(acquired.access);
            updated["expires"] = json!(acquired.expires);
            self.persist(&updated)?;
            *credential = updated;
            self.azure_authorized.store(true, Ordering::Relaxed);
        }
        let mut headers = BTreeMap::new();
        let mut sdk_settings = json!({});
        let api_key = if self.provider.as_ref() == "openai" {
            let access =
                token(&credential, "access").context("Codex OAuth has no usable access token")?;
            if let Some(account) = account_id(&credential) {
                headers.insert("ChatGPT-Account-Id".into(), account);
            }
            if let Some(claims) = jwt_claims(access) {
                if let Some(residency) = claims["https://api.openai.com/auth"]
                    ["chatgpt_compute_residency"]
                    .as_str()
                    .or_else(|| claims["chatgpt_compute_residency"].as_str())
                    .filter(|v| *v != "no_constraint" && !v.is_empty())
                {
                    headers.insert(
                        "x-openai-internal-codex-residency".into(),
                        residency.to_owned(),
                    );
                }
            }
            headers.insert("originator".into(), "opencode".into());
            access.to_owned()
        } else if self.provider.as_ref() == "azure" {
            let access = token(&credential, "access")
                .context("Azure CLI returned no usable bearer token")?;
            sdk_settings = json!({"oauthProvider":"azure","resourceName":field(&credential,"accountId"),"oauthAccessToken":access});
            access.to_owned()
        } else if self.provider.as_ref() == "gitlab" {
            let access =
                token(&credential, "access").context("GitLab OAuth has no usable access token")?;
            sdk_settings = json!({"authType":"oauth","authToken":access,"instanceUrl":crate::gitlab_oauth::instance_url(&credential)?});
            access.to_owned()
        } else if self.provider.as_ref() == "snowflake-cortex" {
            token(&credential, "access")
                .context("Snowflake OAuth has no usable access token")?
                .to_owned()
        } else {
            // The current OpenCode Copilot plugin sends the GitHub OAuth token
            // directly. A Copilot-specific short-lived exchange is not required.
            let access = token(&credential, "refresh")
                .or_else(|| token(&credential, "access"))
                .context("GitHub Copilot OAuth has no usable GitHub token")?;
            headers.insert("X-GitHub-Api-Version".into(), "2026-06-01".into());
            headers.insert("Openai-Intent".into(), "conversation-edits".into());
            headers.insert("x-initiator".into(), "agent".into());
            access.to_owned()
        };
        Ok(OAuthAuthorization {
            api_key,
            headers,
            sdk_settings,
        })
    }

    fn persist(&self, credential: &Value) -> Result<()> {
        if let Some(path) = &self.store_path {
            let mut store = AuthStore::load(path)?;
            store.set_oauth(&self.provider, credential)?;
            store.save()?;
        }
        Ok(())
    }
}

fn token<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str().filter(|value| !value.trim().is_empty())
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    token(value, key).or_else(|| token(&value["metadata"], key))
}

fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis()
        .min(u64::MAX as u128) as u64)
}

fn account_id(value: &Value) -> Option<String> {
    field(value, "accountId")
        .or_else(|| field(value, "account_id"))
        .map(str::to_owned)
        .or_else(|| {
            ["id_token", "access_token", "access"]
                .iter()
                .find_map(|key| {
                    let claims = jwt_claims(token(value, key)?)?;
                    claims["chatgpt_account_id"]
                        .as_str()
                        .or_else(|| {
                            claims["https://api.openai.com/auth"]["chatgpt_account_id"].as_str()
                        })
                        .or_else(|| claims["organizations"][0]["id"].as_str())
                        .map(str::to_owned)
                })
        })
}

/// Decode claims only to recover request-routing metadata; this is not token
/// verification and never changes the supplied bearer token.
fn jwt_claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    parts.next()?;
    let encoded = parts.next()?;
    parts.next()?;
    if parts.next().is_some() || encoded.len() > 256 * 1024 {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len() * 3 / 4);
    let mut bits = 0_u32;
    let mut count = 0;
    for character in encoded.bytes().take_while(|c| *c != b'=') {
        let value = match character {
            b'A'..=b'Z' => character - b'A',
            b'a'..=b'z' => character - b'a' + 26,
            b'0'..=b'9' => character - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
        }
    }
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn login_server(
        responses: Vec<Value>,
    ) -> Result<(String, tokio::task::JoinHandle<Result<Vec<String>>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().await?;
                let mut buffer = Vec::new();
                loop {
                    let mut chunk = [0; 2048];
                    let count = stream.read(&mut chunk).await?;
                    if count == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..count]);
                    if let Some(split) = buffer.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&buffer[..split]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if buffer.len() >= split + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(buffer)?);
                let body = response.to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
            }
            Ok(requests)
        });
        Ok((origin, server))
    }

    #[tokio::test]
    async fn codex_device_login_exchanges_verifier_for_refreshable_account() -> Result<()> {
        let (origin, server) = login_server(vec![
            json!({"device_auth_id":"private-device","user_code":"ABCD-1234","interval":"5"}),
            json!({"authorization_code":"private-code","code_verifier":"private-verifier"}),
            json!({"access_token":"access","refresh_token":"refresh","expires_in":3600,"id_token":"e30.eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50LWEifQ.signature"}),
        ]).await?;
        let login = begin_login_at("openai", origin.clone(), false).await?;
        assert_eq!(login.user_code, "ABCD-1234");
        assert_eq!(login.verification_uri, format!("{origin}/codex/device"));
        let credential = login.wait().await?;
        assert_eq!(credential["type"], "oauth");
        assert_eq!(credential["refresh"], "refresh");
        assert_eq!(credential["accountId"], "account-a");
        let requests = server.await??;
        assert!(requests[0].starts_with("POST /api/accounts/deviceauth/usercode "));
        assert!(requests[1].starts_with("POST /api/accounts/deviceauth/token "));
        assert!(requests[1].contains("private-device"));
        assert!(requests[2].starts_with("POST /oauth/token "));
        assert!(requests[2].contains("grant_type=authorization_code"));
        assert!(requests[2].contains("code_verifier=private-verifier"));
        Ok(())
    }

    #[tokio::test]
    async fn copilot_device_login_keeps_github_bearer_and_enterprise_metadata() -> Result<()> {
        let (origin, server) = login_server(vec![
            json!({"device_code":"private-device","user_code":"EFGH-5678","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}),
            json!({"access_token":"github-bearer"}),
        ]).await?;
        let credential = begin_login_at("github-copilot-enterprise", origin, true)
            .await?
            .wait()
            .await?;
        assert_eq!(credential["access"], "github-bearer");
        assert_eq!(credential["refresh"], "github-bearer");
        assert_eq!(credential["expires"], 0);
        assert_eq!(credential["enterpriseUrl"], "127.0.0.1");
        let requests = server.await??;
        assert!(requests[0].starts_with("POST /login/device/code "));
        assert!(requests[0].contains(COPILOT_CLIENT_ID));
        assert!(requests[1].starts_with("POST /login/oauth/access_token "));
        assert!(requests[1].contains("urn:ietf:params:oauth:grant-type:device_code"));
        Ok(())
    }

    #[tokio::test]
    async fn declined_login_never_exposes_device_grant() -> Result<()> {
        let (origin, server) = login_server(vec![
            json!({"device_code":"private-device","user_code":"CODE","verification_uri":"https://github.com/login/device"}),
            json!({"error":"access_denied","error_description":"private-device"}),
        ]).await?;
        let error = begin_login_at("github-copilot", origin, false)
            .await?
            .wait()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("declined"));
        assert!(!error.contains("private-device"));
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn imported_azure_dummy_always_acquires_real_cli_bearer_once() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let calls = directory.path().join("calls.txt");
        let expires = now_ms()? / 1_000 + 3_600;
        #[cfg(windows)]
        let command = {
            let command = directory.path().join("mock azure.cmd");
            std::fs::write(&command, format!("@echo off\r\necho %* >> \"{}\"\r\necho {{\"accessToken\":\"cli-bearer\",\"expires_on\":{expires}}}\r\n", calls.display()))?;
            command
        };
        #[cfg(not(windows))]
        let command = {
            use std::os::unix::fs::PermissionsExt;
            let command = directory.path().join("mock-azure");
            std::fs::write(&command, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nprintf '%s\\n' '{{\"accessToken\":\"cli-bearer\",\"expires_on\":{expires}}}'\n", calls.display()))?;
            std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o700))?;
            command
        };
        let mut session = OAuthSession::from_credential("azure", json!({"type":"oauth","access":"dummy-key","refresh":"dummy-key","expires":u64::MAX,"accountId":"resource-a"}))?
            .with_base_url("https://resource-a.services.ai.azure.com/openai/v1")?;
        session.azure_command = Some(command);
        let clone = session.clone();
        let (first, second) = tokio::join!(session.authorization(), clone.authorization());
        let first = first?;
        assert_eq!(first.api_key, "cli-bearer");
        assert_eq!(first.sdk_settings["oauthProvider"], "azure");
        assert_eq!(first.sdk_settings["resourceName"], "resource-a");
        assert_eq!(first.sdk_settings["oauthAccessToken"], "cli-bearer");
        assert_eq!(second?.api_key, "cli-bearer");
        let calls = std::fs::read_to_string(calls)?;
        assert_eq!(calls.lines().count(), 1);
        assert!(calls.contains("--scope https://ai.azure.com/.default"));
        assert_eq!(session.protocol_and_base_url("gpt-5.4").0, Protocol::Sdk);
        Ok(())
    }

    #[tokio::test]
    async fn snowflake_rejected_token_rotates_once_and_native_request_is_adapted() -> Result<()> {
        let (origin, server) = login_server(vec![
            json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":600}),
        ])
        .await?;
        let session = OAuthSession::from_credential("snowflake-cortex", json!({"type":"oauth","access":"rejected-access","refresh":"refresh","expires":u64::MAX,"accountId":"org-account"}))?
            .with_refresh_url(&format!("{origin}/oauth/token-request"))?;
        assert_eq!(session.authorization().await?.api_key, "rejected-access");
        let clone = session.clone();
        let (first, second) = tokio::join!(
            session.refresh_after_unauthorized("rejected-access"),
            clone.refresh_after_unauthorized("rejected-access")
        );
        assert_eq!(first?.api_key, "new-access");
        assert_eq!(second?.api_key, "new-access");
        assert_eq!(server.await??.len(), 1);
        let mut request = json!({"model":"test","max_tokens":4096});
        session.prepare_request(&mut request)?;
        assert!(request.get("max_tokens").is_none());
        assert_eq!(request["max_completion_tokens"], 4096);
        let codex = OAuthSession::from_credential(
            "openai",
            json!({"type":"oauth","access":"test","expires":u64::MAX}),
        )?;
        assert!(codex
            .prepare_request(&mut json!({"reasoning":{"mode":"pro"}}))
            .is_err());
        let mut request = json!({"max_output_tokens":4096,"reasoning":{"effort":"high"}});
        codex.prepare_request(&mut request)?;
        assert!(request.get("max_output_tokens").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn copilot_reuses_github_account_and_routes_model_families() -> Result<()> {
        let session = OAuthSession::from_credential(
            "github-copilot",
            json!({"type":"oauth","access":"github-token","refresh":"github-token","expires":0}),
        )?;
        let authorization = session.authorization().await?;
        assert_eq!(authorization.api_key, "github-token");
        assert_eq!(authorization.headers["x-initiator"], "agent");
        assert_eq!(
            session.protocol_and_base_url("claude-sonnet-4.6"),
            (
                Protocol::Anthropic,
                "https://api.githubcopilot.com/v1".into()
            )
        );
        assert_eq!(
            session.protocol_and_base_url("gpt-5.4").0,
            Protocol::Responses
        );
        assert_eq!(
            session.protocol_and_base_url("gpt-5-mini").0,
            Protocol::ChatCompletions
        );
        assert_eq!(
            session.protocol_and_base_url("gpt-10").0,
            Protocol::Responses
        );
        assert_eq!(
            session
                .protocol_and_base_url_with_endpoint("gpt-5.4", Some("chat"))
                .0,
            Protocol::ChatCompletions
        );
        assert_eq!(
            session
                .protocol_and_base_url_with_endpoint("old-model", Some("responses"))
                .0,
            Protocol::Responses
        );
        assert_eq!(
            session.protocol_and_base_url("gemini-3-pro").0,
            Protocol::ChatCompletions
        );
        let enterprise = OAuthSession::from_credential(
            "github-copilot-enterprise",
            json!({"type":"oauth","refresh":"test","metadata":{"enterpriseUrl":"https://company.ghe.com/"}}),
        )?;
        assert_eq!(
            enterprise.protocol_and_base_url("gpt-4.1").1,
            "https://copilot-api.company.ghe.com"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shared_codex_refresh_rotates_once_and_preserves_account() -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("auth.json");
        let mut store = AuthStore::load_with_opencode(&path, None)?;
        store.set_api_key("anthropic", "other-provider-key")?;
        store.set_selection("openai", "gpt-5.4", Some("high"));
        store.set_oauth("openai", &json!({"type":"oauth","access":"expired","refresh":"old-refresh","expires":1,"accountId":"account-a","unknown":"preserved"}))?;
        store.save()?;
        let mut session = OAuthSession::from_store("openai", &store)?.unwrap();
        session.issuer = Arc::from(format!("http://{}", listener.local_addr()?));
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut buffer = Vec::new();
            loop {
                let mut chunk = [0; 2048];
                let count = stream.read(&mut chunk).await?;
                if count == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..count]);
                if let Some(split) = buffer.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&buffer[..split]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buffer.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            let request = String::from_utf8_lossy(&buffer);
            assert!(request.starts_with("POST /oauth/token "));
            assert!(request.contains("grant_type=refresh_token"));
            assert!(request.contains("refresh_token=old-refresh"));
            assert!(request.contains(CODEX_CLIENT_ID));
            let body =
                r#"{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
            Ok::<_, anyhow::Error>(())
        });
        let clone = session.clone();
        let (first, second) = tokio::join!(session.authorization(), clone.authorization());
        assert_eq!(first?.api_key, "new-access");
        assert_eq!(second?.headers["ChatGPT-Account-Id"], "account-a");
        let saved = session.credential.lock().await;
        assert_eq!(saved["refresh"], "new-refresh");
        assert_eq!(saved["unknown"], "preserved");
        assert!(saved["expires"].as_u64().unwrap() > now_ms()?);
        let restored = AuthStore::load_with_opencode(&path, None)?;
        assert_eq!(restored.oauth("openai")?.unwrap()["refresh"], "new-refresh");
        assert_eq!(
            restored.api_key("anthropic", &[])?.as_deref(),
            Some("other-provider-key")
        );
        assert_eq!(
            restored.selection().unwrap().variant.as_deref(),
            Some("high")
        );
        server.await??;
        Ok(())
    }

    #[test]
    fn claims_recover_nested_account_without_revealing_tokens() {
        let claims = "e30.eyJjaGF0Z3B0X2FjY291bnRfaWQiOiJhY2NvdW50LWEifQ.signature";
        assert_eq!(
            account_id(&json!({"access":claims})).as_deref(),
            Some("account-a")
        );
        assert!(jwt_claims("not.a.valid.token").is_none());
        assert!(OAuthSession::from_credential(
            "anthropic",
            json!({"type":"oauth","access":"secret"})
        )
        .err()
        .unwrap()
        .to_string()
        .contains("provider-specific"));
        let codex = OAuthSession::from_credential(
            "openai",
            json!({"type":"oauth","access":"test","expires":u64::MAX}),
        )
        .unwrap();
        assert!(codex.supports_model("gpt-5.4"));
        assert!(codex.supports_model("gpt-6.1-sol"));
        assert!(!codex.supports_model("gpt-4.1-mini"));
        assert!(!codex.supports_model("gpt-5.5-pro"));
    }
}
