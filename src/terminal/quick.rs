//! Provider-management backend for the console's searchable quick menus.
use crate::{
    auth::AuthStore,
    catalog::{is_codex_provider, Catalog},
    config::Config,
    session::SessionControl,
};
use anyhow::{ensure, Context, Result};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub type Resolver = fn(&Catalog, &Config, &str, &str, Option<&str>, &AuthStore) -> Result<Config>;

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub label: String,
    pub detail: String,
}

#[derive(Clone)]
pub struct ProviderManager {
    catalog: Arc<Mutex<Catalog>>,
    pub control: SessionControl,
    resolve: Resolver,
    auth_path: PathBuf,
    import_path: Option<PathBuf>,
    overrides: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
    endpoint_overrides: Arc<Mutex<std::collections::BTreeMap<String, String>>>,
}

impl ProviderManager {
    pub fn new(
        mut catalog: Catalog,
        control: SessionControl,
        resolve: Resolver,
        auth: &AuthStore,
    ) -> Self {
        let active = control.current();
        if !is_codex_provider(&active.config.provider)
            && catalog
                .model(&active.config.provider, &active.config.model)
                .is_none()
        {
            let config = &active.config;
            let variants = config
                .variant
                .as_ref()
                .map(|variant| {
                    std::collections::BTreeMap::from([(
                        variant.clone(),
                        config.provider_options.clone(),
                    )])
                })
                .unwrap_or_default();
            let definition = serde_json::json!({"models":{config.model.clone():{
                "id":config.api_model.as_ref().unwrap_or(&config.model),"name":config.model,
                "reasoning":config.variant.is_some(),"tool_call":true,
                "provider":{"api":config.base_url,"npm":config.provider_npm},
                "options":config.provider_options,"headers":config.provider_headers,
                "variants":variants,"_openraid_protocol":config.protocol,
                "limit":{"context":config.context_budget,"output":config.max_output_tokens}
            }}});
            let _ = catalog.add_custom_provider(&config.provider, &definition);
        }
        for provider in catalog.providers.values_mut() {
            let endpoint = auth.connection_endpoint(&provider.id).or_else(|| {
                (is_codex_provider(&provider.id)
                    && active.config.provider == provider.id
                    && !active.config.base_url.is_empty())
                .then_some(active.config.base_url.as_str())
            });
            if let Some(endpoint) = endpoint {
                provider.api = endpoint.to_owned();
                for model in provider.models.values_mut() {
                    model.api = endpoint.to_owned();
                }
            }
        }
        Self {
            catalog: Arc::new(Mutex::new(catalog)),
            control,
            resolve,
            auth_path: auth.path().to_owned(),
            import_path: auth.import_path().map(PathBuf::from),
            overrides: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            endpoint_overrides: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        }
    }
    fn auth(&self) -> Result<AuthStore> {
        AuthStore::load_with_opencode(self.auth_path.clone(), self.import_path.clone())
    }
    fn connected(&self, provider: &crate::catalog::ProviderInfo, auth: &AuthStore) -> bool {
        let active = self.control.current();
        (active.config.provider == provider.id
            && (active.config.api_key.is_some()
                || active.config.oauth.is_some()
                || active.config.mock))
            || provider.options["apiKey"]
                .as_str()
                .is_some_and(|key| !key.is_empty())
            || auth
                .api_key(&provider.id, &provider.env)
                .ok()
                .flatten()
                .is_some()
            || auth.oauth(&provider.id).ok().flatten().is_some()
            || (is_codex_provider(&provider.id) && auth.connection_endpoint(&provider.id).is_some())
            || cloud_credentials(&provider.npm)
            || provider.has_public_models()
    }
    pub fn providers(&self) -> Result<Vec<Entry>> {
        let auth = self.auth()?;
        let catalog = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
        let mut choices: Vec<_> = catalog
            .providers
            .values()
            .map(|provider| Entry {
                id: provider.id.clone(),
                label: provider.name.clone(),
                detail: format!(
                    "{} · {}",
                    provider.id,
                    if self.connected(provider, &auth) {
                        "connected"
                    } else {
                        "connect with a key"
                    }
                ),
            })
            .collect();
        choices.sort_by_key(|choice| choice.label.to_lowercase());
        Ok(choices)
    }
    pub fn endpoint(&self, provider: &str) -> Result<String> {
        let auth = self.auth()?;
        if let Some(endpoint) = auth.connection_endpoint(provider) {
            return Ok(endpoint.to_owned());
        }
        Ok(self
            .catalog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .provider(provider)
            .context("provider missing")?
            .api
            .clone())
    }
    pub fn needs_endpoint(&self, provider: &str) -> Result<bool> {
        if provider == "codex-lb" {
            return Ok(true);
        }
        let active = self.control.current();
        if (active.config.provider == provider && !active.config.base_url.trim().is_empty())
            || self
                .auth()?
                .connection_endpoint(provider)
                .is_some_and(|url| !url.trim().is_empty())
        {
            return Ok(false);
        }
        Ok(self
            .catalog
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .provider(provider)
            .context("provider missing")?
            .needs_endpoint(None))
    }
    pub async fn connect(&self, provider: &str, key: &str, endpoint: Option<&str>) -> Result<()> {
        let mut auth = self.auth()?;
        if !key.trim().is_empty() {
            auth.set_api_key(provider, key.trim())?;
        }
        if let Some(endpoint) = endpoint {
            let url = reqwest::Url::parse(endpoint).context("invalid API endpoint")?;
            ensure!(
                matches!(url.scheme(), "http" | "https"),
                "endpoint must use HTTP or HTTPS"
            );
            auth.set_connection_endpoint(provider, endpoint);
        }
        {
            let mut catalog = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
            let info = catalog
                .providers
                .get_mut(provider)
                .context("provider missing")?;
            if let Some(endpoint) = endpoint {
                info.api = endpoint.to_owned();
                for model in info.models.values_mut() {
                    model.api = endpoint.to_owned();
                }
            }
            ensure!(
                self.connected(info, &auth),
                "enter a key to connect this provider"
            );
        }
        auth.save()?;
        if let Some(endpoint) = endpoint {
            self.endpoint_overrides
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(provider.to_owned(), endpoint.to_owned());
        }
        if !key.trim().is_empty() {
            self.overrides
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(provider.to_owned(), key.trim().to_owned());
        }
        Ok(())
    }
    async fn refresh(&self, provider: &str) -> Result<()> {
        let auth = self.auth()?;
        let active = self.control.current();
        let info = self
            .catalog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .provider(provider)
            .cloned()
            .context("provider missing")?;
        let environment = (provider == "gitlab")
            .then(|| std::env::var("GITLAB_INSTANCE_URL").ok())
            .flatten();
        let reconnected_endpoint = self
            .endpoint_overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider)
            .cloned();
        let endpoint = reconnected_endpoint
            .as_deref()
            .or_else(|| {
                (provider == "gitlab"
                    && active.config.provider == provider
                    && !active.config.base_url.is_empty())
                .then_some(active.config.base_url.as_str())
            })
            .or_else(|| auth.connection_endpoint(provider))
            .or_else(|| {
                (provider == "gitlab")
                    .then(|| info.options["instanceUrl"].as_str())
                    .flatten()
            })
            .or(environment.as_deref())
            .unwrap_or(&info.api)
            .to_owned();
        let explicit = self
            .overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider)
            .cloned();
        let key = explicit
            .or_else(|| {
                if active.config.provider == provider {
                    active.config.api_key.clone()
                } else {
                    None
                }
            })
            .or_else(|| info.options["apiKey"].as_str().map(str::to_owned))
            .or(auth.api_key(provider, &info.env).ok().flatten());
        let mut single = Catalog {
            providers: std::collections::BTreeMap::from([(provider.to_owned(), info)]),
        };
        if provider == "gitlab" {
            let oauth = if key.is_none() {
                crate::oauth::OAuthSession::from_store(provider, &auth)?
            } else {
                None
            };
            single
                .refresh_gitlab_models(
                    &active.config.workspace,
                    &endpoint,
                    key.as_deref(),
                    oauth.as_ref(),
                )
                .await?;
        } else {
            single
                .refresh_models(provider, &endpoint, key.as_deref())
                .await?;
        }
        self.catalog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .providers
            .insert(
                provider.to_owned(),
                single.providers.remove(provider).unwrap(),
            );
        Ok(())
    }
    pub async fn models(&self) -> Result<Vec<Entry>> {
        let auth = self.auth()?;
        let codex: Vec<_> = self
            .catalog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .providers
            .values()
            .filter(|provider| {
                (is_codex_provider(&provider.id) || provider.id == "gitlab")
                    && self.connected(provider, &auth)
            })
            .map(|provider| provider.id.clone())
            .collect();
        let mut unavailable = Vec::new();
        for provider in codex {
            if self.refresh(&provider).await.is_err() && is_codex_provider(&provider) {
                unavailable.push(provider);
            }
        }
        let catalog = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
        let current = self.control.current();
        let mut choices = Vec::new();
        for provider in catalog.providers.values().filter(|provider| {
            self.connected(provider, &auth) && !unavailable.contains(&provider.id)
        }) {
            let authenticated = provider.options["apiKey"]
                .as_str()
                .is_some_and(|key| !key.is_empty() && key != "public")
                || auth
                    .api_key(&provider.id, &provider.env)
                    .ok()
                    .flatten()
                    .is_some_and(|key| key != "public")
                || auth.oauth(&provider.id)?.is_some()
                || (current.config.provider == provider.id
                    && (current.config.mock
                        || current.config.oauth.is_some()
                        || current
                            .config
                            .api_key
                            .as_deref()
                            .is_some_and(|key| key != "public")));
            let oauth = if auth.api_key(&provider.id, &provider.env).is_err() {
                crate::oauth::OAuthSession::from_store(&provider.id, &auth)
                    .ok()
                    .flatten()
            } else {
                None
            };
            for model in provider.models.values().filter(|model| {
                model.tool_call
                    && provider.model_available(model, authenticated)
                    && oauth
                        .as_ref()
                        .is_none_or(|session| session.supports_model(&model.api_id))
            }) {
                choices.push(Entry {
                    id: model.qualified_id(),
                    label: model.name.clone(),
                    detail: format!(
                        "{}/{} · {} · context {}",
                        provider.id, model.id, provider.name, model.limit.context
                    ),
                });
            }
        }
        choices.sort_by_key(|entry| {
            (
                !entry
                    .id
                    .starts_with(&format!("{}/", current.config.provider)),
                entry.label.to_lowercase(),
            )
        });
        ensure!(!choices.is_empty() || unavailable.is_empty(), "could not load live models from connected Codex providers; check the server or update its key with /connect");
        Ok(choices)
    }
    pub async fn variants(&self) -> Result<Vec<Entry>> {
        let active = self.control.current();
        if is_codex_provider(&active.config.provider) {
            self.refresh(&active.config.provider).await?;
        }
        let catalog = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
        let model = catalog
            .model(&active.config.provider, &active.config.model)
            .context("model metadata unavailable; reopen /models to discover it")?;
        let mut choices = vec![Entry {
            id: String::new(),
            label: "Provider default".into(),
            detail: "Leave the model's default thinking settings intact".into(),
        }];
        let variants = if matches!(
            active.config.provider_npm.as_str(),
            "@ai-sdk/anthropic" | "@ai-sdk/google-vertex/anthropic"
        ) {
            let mut variants = crate::variants::variants(
                &model.provider_id,
                &active.config.provider_npm,
                &model.id,
                &model.api_id,
                model.reasoning,
                &model.release_date,
                model
                    .limit
                    .output
                    .min(active.config.max_output_tokens as usize),
            );
            crate::variants::apply_overrides(&mut variants, &model.metadata["variants"]);
            variants
        } else {
            model.variants.clone()
        };
        for variant in crate::variants::ordered_names(&variants) {
            choices.push(Entry {
                id: variant.clone(),
                label: variant.clone(),
                detail: variants[&variant].to_string(),
            });
        }
        Ok(choices)
    }
    pub async fn select(&self, qualified: &str, variant: Option<&str>) -> Result<()> {
        let (provider, model) = qualified
            .split_once('/')
            .context("invalid provider/model selection")?;
        if is_codex_provider(provider) {
            self.refresh(provider).await?;
        } else if provider == "gitlab" {
            // Optional workflow discovery never hides the static catalog.
            let _ = self.refresh(provider).await;
        }
        let auth = self.auth()?;
        let current = self.control.current();
        let mut config = {
            let catalog = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
            let info = catalog.provider(provider).context("provider missing")?;
            ensure!(
                self.connected(info, &auth),
                "connect this provider with /connect first"
            );
            (self.resolve)(&catalog, &current.config, provider, model, variant, &auth)?
        };
        if let Some(key) = self
            .overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider)
        {
            config.api_key = Some(key.clone());
            config.oauth = None;
        }
        if let Some(endpoint) = self
            .endpoint_overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(provider)
        {
            config.base_url = endpoint.clone();
        } else if provider == "gitlab"
            && current.config.provider == provider
            && !current.config.base_url.is_empty()
        {
            config.base_url = current.config.base_url.clone();
        }
        self.control.switch(config.clone()).await?;
        let mut latest = self.auth()?;
        latest.remember_launch(&config);
        latest.save()?;
        Ok(())
    }
    pub async fn cycle_variant(&self) -> Result<()> {
        let choices = self.variants().await?;
        if choices.len() <= 1 {
            return Ok(());
        }
        let active = self.control.current();
        let current = active.config.variant.as_deref().unwrap_or("");
        let index = choices
            .iter()
            .position(|choice| choice.id == current)
            .unwrap_or(0);
        let next = &choices[(index + 1) % choices.len()].id;
        self.select(
            &format!("{}/{}", active.config.provider, active.config.model),
            (!next.is_empty()).then_some(next.as_str()),
        )
        .await
    }
    pub async fn submit(&self, text: String) -> Result<u64> {
        let auth = self.auth()?;
        let active = self.control.current();
        {
            let catalog = self.catalog.lock().unwrap_or_else(|e| e.into_inner());
            let provider = catalog
                .provider(&active.config.provider)
                .context("choose a model with /models first")?;
            ensure!(
                self.connected(provider, &auth),
                "connect a provider with /connect before sending a prompt"
            );
        }
        self.control.post_prompt(text).await
    }
}

fn cloud_credentials(npm: &str) -> bool {
    let exists = |name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.is_empty())
    };
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if npm.contains("amazon-bedrock") {
        (exists("AWS_ACCESS_KEY_ID") && exists("AWS_SECRET_ACCESS_KEY"))
            || (exists("AWS_ROLE_ARN") && exists("AWS_WEB_IDENTITY_TOKEN_FILE"))
            || home
                .as_ref()
                .is_some_and(|home| home.join(".aws/credentials").is_file())
    } else if npm.contains("google-vertex") {
        std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS")
            .is_some_and(|path| PathBuf::from(path).is_file())
            || home.as_ref().is_some_and(|home| {
                home.join(".config/gcloud/application_default_credentials.json")
                    .is_file()
            })
    } else {
        false
    }
}
