//! The same models.dev provider and model metadata used by OpenCode.
//!
//! A bundled snapshot makes standard provider selection usable offline. Codex LB
//! models are discovered from the configured endpoint rather than guessed.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::{provider::Protocol, variants};

pub const CATALOG_SOURCE: &str = "https://models.dev/api.json";
pub const CODEX_LB_BASE_URL: &str = "http://127.0.0.1:2455/v1";
const BUNDLED_CATALOG: &str = include_str!("../../data/models.json");

pub fn is_codex_provider(provider_id: &str) -> bool {
    matches!(provider_id, "codex-lb" | "codex-pool")
}

fn adapter_default_api(npm: &str) -> String {
    let (environment, fallback) = match npm {
        "@ai-sdk/openai" => ("OPENAI_BASE_URL", "https://api.openai.com/v1"),
        "@ai-sdk/anthropic" => ("ANTHROPIC_BASE_URL", "https://api.anthropic.com/v1"),
        "@ai-sdk/google" => ("", "https://generativelanguage.googleapis.com/v1beta"),
        "@ai-sdk/xai" => ("", "https://api.x.ai/v1"),
        "@ai-sdk/groq" => ("", "https://api.groq.com/openai/v1"),
        "@ai-sdk/mistral" => ("", "https://api.mistral.ai/v1"),
        "@ai-sdk/deepinfra" => ("", "https://api.deepinfra.com/v1/openai"),
        "@ai-sdk/cerebras" => ("", "https://api.cerebras.ai/v1"),
        "@ai-sdk/togetherai" => ("", "https://api.together.xyz/v1"),
        "@ai-sdk/perplexity" => ("", "https://api.perplexity.ai"),
        "@ai-sdk/alibaba" => ("", "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
        "@openrouter/ai-sdk-provider" => ("", "https://openrouter.ai/api/v1"),
        "venice-ai-sdk-provider" => ("", "https://api.venice.ai/api/v1"),
        _ => ("", ""),
    };
    std::env::var(environment)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelLimit {
    pub context: usize,
    pub output: usize,
    #[serde(default)]
    pub input: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider_id: String,
    pub api_id: String,
    pub api: String,
    pub npm: String,
    pub reasoning: bool,
    pub tool_call: bool,
    pub release_date: String,
    pub limit: ModelLimit,
    pub cost: Value,
    pub variants: BTreeMap<String, Value>,
    /// Preserve less-common metadata rather than dropping catalog capabilities.
    pub metadata: Value,
}

impl Model {
    pub fn copilot_endpoint(&self) -> Option<&str> {
        self.metadata["api"]["endpoint"]
            .as_str()
            .or_else(|| self.metadata["provider"]["endpoint"].as_str())
    }
    pub fn qualified_id(&self) -> String {
        format!("{}/{}", self.provider_id, self.id)
    }

    pub fn variant_options(&self, variant: Option<&str>) -> Result<Value> {
        match variant {
            None | Some("default") => Ok(json!({})),
            Some(name) => self.variants.get(name).cloned().with_context(|| {
                format!(
                    "unknown thinking variant {name:?} for {}; choose {}",
                    self.qualified_id(),
                    self.variants.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub env: Vec<String>,
    pub api: String,
    pub npm: String,
    #[serde(default)]
    pub options: Value,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub models: BTreeMap<String, Model>,
    /// Explicit configuration survives discovery; old server metadata does not.
    #[serde(default, skip_serializing)]
    pub model_overrides: BTreeMap<String, Value>,
}

impl ProviderInfo {
    /// Native custom providers need a URL even when their catalog entry exists.
    /// SDK-owned endpoint defaults and cloud credential chains need no URL prompt.
    pub fn needs_endpoint(&self, model_id: Option<&str>) -> bool {
        if !self.api.trim().is_empty() {
            return false;
        }
        let missing_native_endpoint = |model: &Model| {
            model.api.trim().is_empty()
                && self
                    .protocol(model)
                    .is_ok_and(|protocol| protocol != Protocol::Sdk)
        };
        if let Some(model) = model_id.and_then(|id| self.models.get(id)) {
            return missing_native_endpoint(model);
        }
        self.models.values().any(missing_native_endpoint)
            || (self.models.is_empty() && self.npm == "@ai-sdk/openai-compatible")
    }

    /// The source loader exposes only zero-input-cost Zen models without auth.
    /// Keep the complete catalog intact; enforce this at selection/dispatch.
    pub fn model_available(&self, model: &Model, authenticated: bool) -> bool {
        self.id != "opencode" || authenticated || model.cost["input"].as_f64().unwrap_or(0.0) == 0.0
    }

    pub fn has_public_models(&self) -> bool {
        self.id == "opencode"
            && self
                .models
                .values()
                .any(|model| self.model_available(model, false))
    }

    pub fn loader_headers(&self) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        if self.id == "anthropic" {
            headers.insert(
                "anthropic-beta".into(),
                "interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14".into(),
            );
        }
        if matches!(
            self.id.as_str(),
            "llmgateway" | "openrouter" | "nvidia" | "vercel" | "zenmux" | "kilo"
        ) {
            headers.insert("http-referer".into(), "https://vuln.industries/".into());
            headers.insert("x-title".into(), "openraid".into());
        }
        if self.id == "llmgateway" {
            headers.insert("x-source".into(), "openraid".into());
        }
        if self.id == "nvidia" {
            headers.insert("x-billing-invoke-origin".into(), "OpenRaid".into());
        }
        if self.id == "cerebras" {
            headers.insert("x-cerebras-3rd-party-integration".into(), "openraid".into());
        }
        if matches!(
            self.id.as_str(),
            "github-copilot" | "github-copilot-enterprise"
        ) {
            headers.insert("x-github-api-version".into(), "2026-06-01".into());
            headers.insert("openai-intent".into(), "conversation-edits".into());
            headers.insert("x-initiator".into(), "agent".into());
        }
        for (name, value) in &self.headers {
            headers.insert(name.to_ascii_lowercase(), value.clone());
        }
        headers
    }
    /// Resolve the model-level SDK override, just as OpenCode does.
    pub fn protocol(&self, model: &Model) -> Result<Protocol> {
        if is_codex_provider(&self.id) {
            return Ok(Protocol::Responses);
        }
        if self.id == "cloudflare-ai-gateway" {
            return Ok(Protocol::Sdk);
        }
        if matches!(self.id.as_str(), "meta" | "xai") {
            return Ok(Protocol::Responses);
        }
        if matches!(
            self.id.as_str(),
            "github-copilot" | "github-copilot-enterprise"
        ) || model.npm == "@ai-sdk/github-copilot"
        {
            if let Some(endpoint) = model.copilot_endpoint() {
                match endpoint {
                    "responses" => return Ok(Protocol::Responses),
                    "chat" => return Ok(Protocol::ChatCompletions),
                    _ => {}
                }
            }
            let id = model.api_id.as_str();
            return Ok(if id.starts_with("claude") {
                Protocol::Anthropic
            } else if id
                .strip_prefix("gpt-")
                .and_then(|rest| rest.split(['.', '-']).next())
                .and_then(|major| major.parse::<u32>().ok())
                .is_some_and(|major| major >= 5)
                && !id.starts_with("gpt-5-mini")
            {
                Protocol::Responses
            } else {
                Protocol::ChatCompletions
            });
        }
        match model.npm.as_str() {
            "@ai-sdk/anthropic" => Ok(Protocol::Anthropic),
            "@ai-sdk/google" => Ok(Protocol::Gemini),
            "@ai-sdk/openai" => Ok(Protocol::Responses),
            "@ai-sdk/xai" => Ok(Protocol::Responses),
            "@ai-sdk/openai-compatible"
            | "@ai-sdk/groq"
            | "@ai-sdk/mistral"
            | "@ai-sdk/deepinfra"
            | "@ai-sdk/cerebras"
            | "@ai-sdk/togetherai"
            | "@ai-sdk/perplexity"
            | "@ai-sdk/alibaba"
            | "@openrouter/ai-sdk-provider"
            | "venice-ai-sdk-provider" => Ok(Protocol::ChatCompletions),
            _ => Ok(Protocol::Sdk),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub providers: BTreeMap<String, ProviderInfo>,
}

impl Catalog {
    pub fn load(workspace: &std::path::Path, explicit: Option<&std::path::Path>) -> Result<Self> {
        let enabled =
            crate::auth::AuthStore::load_default()?.opencode_import_consent() == Some(true);
        Self::load_with_opencode(workspace, explicit, enabled)
    }

    /// Explicit paths remain user-selected configuration; automatic OpenCode
    /// discovery requires consent. OpenRaid config is always loaded normally.
    pub fn load_with_opencode(
        workspace: &std::path::Path,
        explicit: Option<&std::path::Path>,
        enabled: bool,
    ) -> Result<Self> {
        let mut catalog = Self::bundled()?;
        let mut paths = Vec::new();
        if enabled {
            paths.extend(Self::opencode_config_paths(workspace));
        }
        let local = explicit.map(std::path::PathBuf::from).or_else(|| {
            ["openraid.json", "openraid.jsonc"]
                .iter()
                .map(|name| workspace.join(name))
                .find(|path| path.is_file())
        });
        if let Some(path) = local {
            paths.retain(|old| old != &path);
            paths.push(path);
        }
        for path in paths {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading provider config {}", path.display()))?;
            let value: Value = json5::from_str(&text).with_context(|| {
                format!("invalid JSON/JSONC provider config {}", path.display())
            })?;
            let providers =
                json!({"provider":value.get("provider").cloned().unwrap_or_else(|| json!({}))});
            let value = crate::provider_settings::expand_config(
                &providers,
                path.parent().unwrap_or(workspace),
            )?;
            catalog.apply_config(&value)?;
        }
        Ok(catalog)
    }

    pub fn opencode_import_available(workspace: &std::path::Path) -> bool {
        !Self::opencode_config_paths(workspace).is_empty()
    }

    pub fn opencode_config_paths(workspace: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut paths = Self::global_opencode_paths();
        paths.extend(
            ["opencode.json", "opencode.jsonc"]
                .iter()
                .map(|name| workspace.join(name))
                .filter(|path| path.is_file()),
        );
        paths
    }

    fn global_opencode_paths() -> Vec<std::path::PathBuf> {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(|home| std::path::PathBuf::from(home).join(".config"))
            });
        root.map(|root| {
            ["opencode.json", "opencode.jsonc"]
                .iter()
                .map(|name| root.join("opencode").join(name))
                .filter(|path| path.is_file())
                .collect()
        })
        .unwrap_or_default()
    }

    pub fn bundled() -> Result<Self> {
        Self::from_json(BUNDLED_CATALOG)
    }

    pub fn from_json(source: &str) -> Result<Self> {
        let raw: BTreeMap<String, Value> =
            serde_json::from_str(source).context("parsing models.dev provider catalog")?;
        let mut providers = BTreeMap::new();
        for (id, value) in raw {
            let api = string(&value, "api", "");
            let npm = string(&value, "npm", "@ai-sdk/openai-compatible");
            let env = value["env"]
                .as_array()
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let mut provider = ProviderInfo {
                name: string(&value, "name", &id),
                id: id.clone(),
                env,
                api,
                npm,
                options: value.get("options").cloned().unwrap_or_else(|| json!({})),
                headers: parse_headers(&value["headers"])?,
                models: BTreeMap::new(),
                model_overrides: BTreeMap::new(),
            };
            if let Some(models) = value["models"].as_object() {
                for (model_id, model) in models {
                    let model = normalize_model(&provider, model_id, model);
                    for mode in expand_modes(&model) {
                        provider.models.insert(mode.id.clone(), mode);
                    }
                    provider.models.insert(model_id.clone(), model);
                }
            }
            if provider.api.is_empty() {
                provider.api = adapter_default_api(&provider.npm);
            }
            providers.insert(id, provider);
        }
        let mut catalog = Self { providers };
        catalog.add_codex_lb();
        Ok(catalog)
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderInfo> {
        self.providers.get(id)
    }

    pub fn model(&self, provider: &str, id: &str) -> Option<&Model> {
        self.provider(provider)?.models.get(id)
    }

    pub fn search_providers(&self, query: &str) -> Vec<&ProviderInfo> {
        let mut found: Vec<_> = self
            .providers
            .values()
            .filter_map(|provider| {
                search_score(query, &[&provider.id, &provider.name]).map(|score| (score, provider))
            })
            .collect();
        found.sort_by(|(a_score, a), (b_score, b)| {
            a_score.cmp(b_score).then_with(|| a.name.cmp(&b.name))
        });
        found.into_iter().map(|(_, provider)| provider).collect()
    }

    pub fn search_models(&self, provider: &str, query: &str) -> Vec<&Model> {
        let Some(provider) = self.provider(provider) else {
            return Vec::new();
        };
        let mut found: Vec<_> = provider
            .models
            .values()
            .filter_map(|model| {
                search_score(query, &[&model.id, &model.name]).map(|score| (score, model))
            })
            .collect();
        found.sort_by(|(a_score, a), (b_score, b)| {
            a_score
                .cmp(b_score)
                .then_with(|| b.release_date.cmp(&a.release_date))
                .then_with(|| a.name.cmp(&b.name))
        });
        found.into_iter().map(|(_, model)| model).collect()
    }

    pub fn model_count(&self) -> usize {
        self.providers
            .values()
            .map(|provider| provider.models.len())
            .sum()
    }

    /// Add any OpenCode-style custom provider, including model-level overrides.
    pub fn add_custom_provider(&mut self, id: &str, config: &Value) -> Result<()> {
        if id.trim().is_empty() {
            bail!("custom provider ID must not be empty");
        }
        let api = config["options"]["baseURL"]
            .as_str()
            .or_else(|| config["api"].as_str())
            .unwrap_or_default();
        let mut provider = self
            .providers
            .get(id)
            .cloned()
            .unwrap_or_else(|| ProviderInfo {
                id: id.to_owned(),
                name: id.to_owned(),
                env: Vec::new(),
                api: String::new(),
                npm: "@ai-sdk/openai-compatible".into(),
                options: json!({}),
                headers: BTreeMap::new(),
                models: BTreeMap::new(),
                model_overrides: BTreeMap::new(),
            });
        provider.name = string(config, "name", &provider.name);
        provider.npm = string(config, "npm", &provider.npm);
        if let Some(options) = config.get("options") {
            if !options.is_object() {
                bail!("provider {id} options must be an object");
            }
            merge_json(&mut provider.options, options);
        }
        provider.headers.extend(parse_headers(&config["headers"])?);
        provider
            .headers
            .extend(parse_headers(&config["options"]["headers"])?);
        if !api.is_empty() {
            provider.api = api.to_owned();
        }
        if let Some(env) = config["env"].as_array() {
            provider.env = env
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
        if let Some(models) = config["models"].as_object() {
            for (model_id, value) in models {
                let overrides = provider
                    .model_overrides
                    .entry(model_id.clone())
                    .or_insert_with(|| json!({}));
                merge_json(overrides, value);
                let mut raw = provider
                    .models
                    .get(model_id)
                    .map(|m| m.metadata.clone())
                    .unwrap_or_else(|| json!({}));
                merge_json(&mut raw, value);
                let model = normalize_model(&provider, model_id, &raw);
                provider.models.insert(model_id.clone(), model);
            }
        }
        // A changed provider endpoint applies to existing inherited models too.
        if provider.api.is_empty() {
            provider.api = adapter_default_api(&provider.npm);
        }
        for model in provider.models.values_mut() {
            if model.metadata["provider"]["api"].as_str().is_none() {
                model.api.clone_from(&provider.api);
            }
            if model.metadata["provider"]["npm"].as_str().is_none() {
                model.npm = effective_npm(
                    &provider.id,
                    &model.id,
                    &provider.npm,
                    &model.metadata["provider"],
                );
                model.variants = model_variants(&provider.id, model);
                apply_variant_overrides(&mut model.variants, &model.metadata["variants"]);
            }
        }
        self.providers.insert(id.to_owned(), provider);
        Ok(())
    }

    /// Merge the provider section of an OpenCode-compatible configuration.
    pub fn apply_config(&mut self, config: &Value) -> Result<()> {
        if let Some(providers) = config["provider"].as_object() {
            for (id, provider) in providers {
                self.add_custom_provider(id, provider)?;
            }
        }
        Ok(())
    }

    /// GitLab discovery augments the static/configured catalog rather than
    /// replacing it. Dynamic workflow references are retained for SDK routing.
    pub async fn refresh_gitlab_models(
        &mut self,
        workspace: &std::path::Path,
        base_url: &str,
        api_key: Option<&str>,
        oauth: Option<&crate::oauth::OAuthSession>,
    ) -> Result<usize> {
        let provider = self.provider("gitlab").context("GitLab provider missing")?;
        let mut settings = provider.options.clone();
        let mut base = base_url.to_owned();
        if let Some(oauth) = oauth.filter(|_| api_key.is_none()) {
            let authorization = oauth.authorization().await?;
            merge_json(&mut settings, &authorization.sdk_settings);
            if base_url.is_empty() || base_url == provider.api {
                if let Some(endpoint) = settings["instanceUrl"].as_str() {
                    base = endpoint.to_owned();
                }
            }
        }
        let models =
            crate::sdk_bridge::discover_gitlab_models(workspace, &base, api_key, &settings).await?;
        self.apply_gitlab_models(&base, &models)
    }

    pub fn apply_gitlab_models(&mut self, base_url: &str, entries: &[Value]) -> Result<usize> {
        let provider = self
            .providers
            .get_mut("gitlab")
            .context("GitLab provider missing")?;
        let mut added = 0;
        for entry in entries {
            let Some(id) = entry["id"].as_str().filter(|id| !id.trim().is_empty()) else {
                continue;
            };
            // Source discovery never overwrites inherited or configured models.
            if provider.models.contains_key(id) {
                continue;
            }
            let mut metadata = entry.clone();
            if let Some(overrides) = provider.model_overrides.get(id) {
                merge_json(&mut metadata, overrides);
            }
            let mut model = normalize_model(provider, id, &metadata);
            model.api = base_url.to_owned();
            provider.models.insert(id.to_owned(), model);
            added += 1;
        }
        Ok(added)
    }

    /// Replace the previous model list with models actually served by a proxy.
    /// Errors leave the existing catalog intact and never include credentials.
    pub async fn refresh_models(
        &mut self,
        provider_id: &str,
        base_url: &str,
        api_key: Option<&str>,
    ) -> Result<usize> {
        let (base, url) = discovery_urls(base_url)?;
        let mut request = reqwest::Client::new().get(url);
        if let Some(key) = api_key.filter(|key| !key.is_empty()) {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("could not connect to model discovery endpoint"))?;
        if !response.status().is_success() {
            bail!("model discovery returned HTTP {}", response.status());
        }
        let body: Value = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("invalid model discovery JSON"))?;
        self.apply_discovered_models(provider_id, base.as_str(), &body)
    }

    pub fn apply_discovered_models(
        &mut self,
        provider_id: &str,
        base_url: &str,
        body: &Value,
    ) -> Result<usize> {
        let entries = body["models"]
            .as_array()
            .filter(|models| !models.is_empty())
            .or_else(|| body["data"].as_array())
            .or_else(|| body.as_array())
            .context("model discovery response must contain a data or models array")?;
        let provider = self
            .providers
            .get(provider_id)
            .with_context(|| format!("unknown provider {provider_id}"))?;
        let mut models = BTreeMap::new();
        for entry in entries {
            let Some(id) = entry["id"]
                .as_str()
                .or_else(|| entry["slug"].as_str())
                .or_else(|| entry.as_str())
            else {
                continue;
            };
            if id.trim().is_empty() {
                continue;
            }
            let mut metadata = if is_codex_provider(provider_id) {
                json!({"id": id, "name": id, "reasoning": true, "tool_call": true})
            } else {
                provider.models.get(id).map(|model| model.metadata.clone()).unwrap_or_else(|| json!({
                "id": id, "name": id, "reasoning": provider_id == "codex-lb", "tool_call": true,
                "limit": {"context": 128000, "output": 16384}
                }))
            };
            if entry.is_object() {
                if entry["metadata"].is_object() {
                    merge_json(&mut metadata, &entry["metadata"]);
                }
                merge_json(&mut metadata, entry);
            }
            if let Some(name) = metadata["display_name"].as_str().map(str::to_owned) {
                metadata["name"] = json!(name);
            }
            if let Some(context) = metadata["input_context_window"]
                .as_u64()
                .or_else(|| metadata["context_window"].as_u64())
                .filter(|n| *n > 0)
            {
                metadata["limit"]["context"] = json!(context);
            }
            if let Some(output) = metadata["max_output_tokens"].as_u64().filter(|n| *n > 0) {
                metadata["limit"]["output"] = json!(output);
            }
            if let Some(reasoning) = metadata["supports_reasoning"].as_bool() {
                metadata["reasoning"] = json!(reasoning);
            }
            if let Some(tools) = metadata["supports_tools"].as_bool() {
                metadata["tool_call"] = json!(tools);
            }
            let overrides = provider
                .model_overrides
                .get(id)
                .cloned()
                .unwrap_or_else(|| json!({}));
            merge_json(&mut metadata, &overrides);
            let mut model = normalize_model(provider, id, &metadata);
            model.api = base_url.to_owned();
            if is_codex_provider(provider_id) {
                model.variants.clear();
            }
            if let Some(efforts) = metadata["supported_reasoning_levels"]
                .as_array()
                .or_else(|| metadata["supported_reasoning_efforts"].as_array())
                .or_else(|| metadata["reasoning_efforts"].as_array())
            {
                model.variants.clear();
                for effort in efforts
                    .iter()
                    .filter(|_| metadata["supports_reasoning"].as_bool() != Some(false))
                {
                    if let Some(name) = effort
                        .as_str()
                        .or_else(|| effort["effort"].as_str())
                        .or_else(|| effort["reasoning_effort"].as_str())
                        .or_else(|| effort["id"].as_str())
                    {
                        let name = name.trim().to_ascii_lowercase();
                        if name.is_empty() {
                            continue;
                        }
                        model
                            .variants
                            .insert(name.clone(), json!({"reasoningEffort": name}));
                    }
                }
                model.reasoning = !model.variants.is_empty();
            }
            apply_variant_overrides(&mut model.variants, &overrides["variants"]);
            models.insert(id.to_owned(), model);
        }
        if models.is_empty() {
            bail!("model discovery returned no usable models; existing catalog retained");
        }
        let count = models.len();
        let provider = self
            .providers
            .get_mut(provider_id)
            .expect("provider validated above");
        provider.api = base_url.to_owned();
        provider.models = models;
        Ok(count)
    }

    fn add_codex_lb(&mut self) {
        if self.providers.contains_key("codex-lb") {
            return;
        }
        let provider = ProviderInfo {
            id: "codex-lb".into(),
            name: "Codex LB (custom Codex API)".into(),
            env: vec!["CODEX_LB_API_KEY".into()],
            api: CODEX_LB_BASE_URL.into(),
            npm: "@ai-sdk/openai".into(),
            options: json!({}),
            headers: BTreeMap::new(),
            models: BTreeMap::new(),
            model_overrides: BTreeMap::new(),
        };
        self.providers.insert(provider.id.clone(), provider);
    }
}

fn string(value: &Value, key: &str, fallback: &str) -> String {
    value[key].as_str().unwrap_or(fallback).to_owned()
}

fn parse_headers(value: &Value) -> Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    if value.is_null() {
        return Ok(headers);
    }
    let object = value
        .as_object()
        .context("provider headers must be an object")?;
    for (name, value) in object {
        let value = value
            .as_str()
            .with_context(|| format!("provider header {name:?} must be a string"))?;
        headers.insert(name.clone(), value.to_owned());
    }
    Ok(headers)
}

fn discovery_urls(base_url: &str) -> Result<(reqwest::Url, reqwest::Url)> {
    let mut base = reqwest::Url::parse(base_url).context("invalid model discovery URL")?;
    if !matches!(base.scheme(), "http" | "https") {
        bail!("model discovery URL must use http or https");
    }
    let path = base.path().trim_end_matches('/');
    let path = path
        .strip_suffix("/responses")
        .or_else(|| path.strip_suffix("/chat/completions"))
        .unwrap_or(path)
        .to_owned();
    base.set_path(&path);
    let mut endpoint = base.clone();
    endpoint.set_path(&format!("{path}/models"));
    Ok((base, endpoint))
}

fn normalize_model(provider: &ProviderInfo, id: &str, value: &Value) -> Model {
    let sdk = &value["provider"];
    let npm = effective_npm(&provider.id, id, &provider.npm, sdk);
    let mut model = Model {
        id: id.to_owned(),
        name: string(value, "name", id),
        provider_id: provider.id.clone(),
        api_id: string(value, "id", id),
        api: sdk["api"]
            .as_str()
            .filter(|api| !api.is_empty())
            .map(str::to_owned)
            .or_else(|| (!provider.api.is_empty()).then(|| provider.api.clone()))
            .unwrap_or_else(|| adapter_default_api(&npm)),
        npm,
        reasoning: value["reasoning"].as_bool().unwrap_or(false),
        tool_call: value["tool_call"].as_bool().unwrap_or(true),
        release_date: string(value, "release_date", ""),
        limit: serde_json::from_value(value["limit"].clone()).unwrap_or_default(),
        cost: value.get("cost").cloned().unwrap_or_else(|| json!({})),
        variants: BTreeMap::new(),
        metadata: value.clone(),
    };
    model.variants = model_variants(&provider.id, &model);
    apply_variant_overrides(&mut model.variants, &value["variants"]);
    model
}

fn effective_npm(
    provider_id: &str,
    model_id: &str,
    provider_npm: &str,
    model_provider: &Value,
) -> String {
    if provider_id == "cloudflare-ai-gateway" && model_id.starts_with("openai/") {
        "@ai-sdk/openai".to_owned()
    } else if provider_id == "cloudflare-ai-gateway" && model_id.starts_with("anthropic/") {
        "@ai-sdk/anthropic".to_owned()
    } else {
        string(model_provider, "npm", provider_npm)
    }
}

fn model_variants(provider_id: &str, model: &Model) -> BTreeMap<String, Value> {
    variants::variants(
        provider_id,
        &model.npm,
        &model.id,
        &model.api_id,
        model.reasoning,
        &model.release_date,
        model.limit.output,
    )
}

fn expand_modes(base: &Model) -> Vec<Model> {
    let Some(modes) = base
        .metadata
        .pointer("/experimental/modes")
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    modes
        .iter()
        .map(|(name, value)| {
            let mut model = base.clone();
            model.id = format!("{}-{name}", base.id);
            model.metadata["id"] = json!(base.api_id);
            let mut characters = name.chars();
            let display = characters
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                .unwrap_or_default();
            model.name = format!("{} {display}", base.name);
            model.metadata["name"] = json!(model.name);
            if value["cost"].is_object() {
                merge_json(&mut model.cost, &value["cost"]);
                model.metadata["cost"] = model.cost.clone();
            }
            if let Some(body) = value.pointer("/provider/body").and_then(Value::as_object) {
                let mut options = serde_json::Map::new();
                for (key, value) in body {
                    options.insert(camel_case(key), value.clone());
                }
                if model.npm == "@ai-sdk/openai" {
                    if let Some(mode) = body
                        .get("reasoning")
                        .and_then(|reasoning| reasoning["mode"].as_str())
                    {
                        options.remove("reasoning");
                        options.insert("reasoningMode".into(), json!(mode));
                    }
                }
                model.metadata["options"] = Value::Object(options);
            }
            if value
                .pointer("/provider/headers")
                .is_some_and(Value::is_object)
            {
                model.metadata["headers"] = value["provider"]["headers"].clone();
            }
            model
        })
        .collect()
}

fn camel_case(input: &str) -> String {
    let mut result = String::new();
    let mut upper = false;
    for character in input.chars() {
        if character == '_' {
            upper = true;
        } else if upper {
            result.extend(character.to_uppercase());
            upper = false;
        } else {
            result.push(character);
        }
    }
    result
}

fn apply_variant_overrides(variants: &mut BTreeMap<String, Value>, value: &Value) {
    if let Some(overrides) = value.as_object() {
        for (name, options) in overrides {
            if options["disabled"].as_bool().unwrap_or(false) {
                variants.remove(name);
                continue;
            }
            let mut options = options.clone();
            if let Some(object) = options.as_object_mut() {
                object.remove("disabled");
            }
            merge_json(
                variants.entry(name.clone()).or_insert_with(|| json!({})),
                &options,
            );
        }
    }
}

fn merge_json(target: &mut Value, source: &Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            merge_json(target.entry(key.clone()).or_insert(Value::Null), value);
        }
    } else {
        *target = source.clone();
    }
}

fn search_score(query: &str, fields: &[&str]) -> Option<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let fields: Vec<String> = fields.iter().map(|field| field.to_lowercase()).collect();
    if fields.iter().any(|field| field == &query) {
        return Some(0);
    }
    let joined = fields.join(" ");
    let mut score = 0;
    for word in query.split_whitespace() {
        if let Some(position) = joined.find(word) {
            score += position + 1;
            continue;
        }
        // Subsequence matching tolerates punctuation: "gpt54" finds "gpt-5.4".
        let mut chars = word.chars();
        let mut next = chars.next();
        for character in joined.chars() {
            if next == Some(character) {
                next = chars.next();
            }
        }
        if next.is_some() {
            return None;
        }
        score += joined.len() + 1;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_opencode_config_requires_consent_but_explicit_paths_are_loaded() -> Result<()> {
        let root = tempfile::tempdir()?;
        let imported = root.path().join("opencode.json");
        std::fs::write(
            &imported,
            r#"{"provider":{"consent-fixture":{"npm":"@ai-sdk/openai-compatible","options":{"baseURL":"https://fixture.invalid/v1","apiKey":"fixture-key"},"models":{"fixture-model":{}}}}}"#,
        )?;
        assert!(Catalog::opencode_import_available(root.path()));
        assert!(Catalog::opencode_config_paths(root.path()).contains(&imported));
        let denied = Catalog::load_with_opencode(root.path(), None, false)?;
        assert!(denied.provider("consent-fixture").is_none());
        let accepted = Catalog::load_with_opencode(root.path(), None, true)?;
        assert_eq!(
            accepted.provider("consent-fixture").unwrap().options["apiKey"],
            "fixture-key"
        );
        let explicit = Catalog::load_with_opencode(root.path(), Some(&imported), false)?;
        assert!(explicit.model("consent-fixture", "fixture-model").is_some());
        std::fs::write(&imported, "not valid json")?;
        assert!(Catalog::load_with_opencode(root.path(), None, false).is_ok());
        assert!(Catalog::load_with_opencode(root.path(), Some(&imported), false).is_err());
        Ok(())
    }

    #[test]
    fn openraid_provider_config_remains_available_without_import_consent() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("openraid.json"),
            r#"{"provider":{"native-fixture":{"npm":"@ai-sdk/openai-compatible","models":{"fixture-model":{}}}}}"#,
        )?;
        std::fs::write(root.path().join("opencode.json"), "invalid ignored config")?;
        let catalog = Catalog::load_with_opencode(root.path(), None, false)?;
        assert!(catalog.model("native-fixture", "fixture-model").is_some());
        Ok(())
    }

    #[test]
    fn imported_local_providers_merge_before_openraid_overrides() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("opencode.json"),
            r#"{"provider":{"import-only-fixture":{"models":{"fixture-model":{}}},"merge-fixture":{"options":{"baseURL":"https://import.invalid/v1","apiKey":"imported-key"},"models":{"fixture-model":{}}}}}"#,
        )?;
        std::fs::write(
            root.path().join("openraid.json"),
            r#"{"provider":{"merge-fixture":{"options":{"baseURL":"https://openraid.invalid/v1"}}}}"#,
        )?;
        let catalog = Catalog::load_with_opencode(root.path(), None, true)?;
        assert!(catalog
            .model("import-only-fixture", "fixture-model")
            .is_some());
        let provider = catalog.provider("merge-fixture").unwrap();
        assert_eq!(provider.api, "https://openraid.invalid/v1");
        assert_eq!(provider.options["apiKey"], "imported-key");
        Ok(())
    }

    #[test]
    fn copilot_loader_routes_family_and_explicit_endpoint_before_adapter_defaults() -> Result<()> {
        let catalog = Catalog::from_json(
            r#"{"github-copilot":{"npm":"@ai-sdk/openai-compatible","models":{
            "gpt-5.4":{},"gpt-5-mini":{},"gpt-10":{},"claude-sonnet":{},
            "forced-chat":{"api":{"endpoint":"chat"},"id":"gpt-5.4"},
            "forced-responses":{"provider":{"endpoint":"responses"},"id":"old-model"}
        }}}"#,
        )?;
        let provider = catalog.provider("github-copilot").unwrap();
        for (model, protocol) in [
            ("gpt-5.4", Protocol::Responses),
            ("gpt-5-mini", Protocol::ChatCompletions),
            ("gpt-10", Protocol::Responses),
            ("claude-sonnet", Protocol::Anthropic),
            ("forced-chat", Protocol::ChatCompletions),
            ("forced-responses", Protocol::Responses),
        ] {
            assert_eq!(
                provider.protocol(&provider.models[model])?,
                protocol,
                "{model}"
            );
        }
        Ok(())
    }

    #[test]
    fn source_loader_public_access_and_headers_preserve_explicit_overrides() -> Result<()> {
        let catalog = Catalog::from_json(
            r#"{
            "opencode":{"models":{"free":{"cost":{"input":0}},"paid":{"cost":{"input":1}}}},
            "anthropic":{"headers":{"Anthropic-Beta":"custom"},"models":{}},
            "nvidia":{"models":{}},"llmgateway":{"models":{}},"cerebras":{"models":{}}
        }"#,
        )?;
        let zen = catalog.provider("opencode").unwrap();
        assert!(zen.has_public_models());
        assert!(zen.model_available(&zen.models["free"], false));
        assert!(!zen.model_available(&zen.models["paid"], false));
        assert!(zen.model_available(&zen.models["paid"], true));
        assert_eq!(
            catalog.provider("anthropic").unwrap().loader_headers()["anthropic-beta"],
            "custom"
        );
        assert_eq!(
            catalog.provider("nvidia").unwrap().loader_headers()["x-billing-invoke-origin"],
            "OpenRaid"
        );
        assert_eq!(
            catalog.provider("llmgateway").unwrap().loader_headers()["x-source"],
            "openraid"
        );
        assert_eq!(
            catalog.provider("cerebras").unwrap().loader_headers()
                ["x-cerebras-3rd-party-integration"],
            "openraid"
        );
        Ok(())
    }

    #[test]
    fn bundled_snapshot_retains_all_providers_models_and_model_sdk_overrides() -> Result<()> {
        let raw: BTreeMap<String, Value> = serde_json::from_str(BUNDLED_CATALOG)?;
        let catalog = Catalog::bundled()?;
        assert_eq!(catalog.providers.len(), raw.len() + 1);
        for (id, provider) in raw {
            let normalized = catalog.provider(&id).unwrap();
            let models = provider["models"].as_object().unwrap();
            let mut expected: std::collections::BTreeSet<_> = models.keys().cloned().collect();
            for (model_id, model) in models {
                if let Some(modes) = model
                    .pointer("/experimental/modes")
                    .and_then(Value::as_object)
                {
                    for mode in modes.keys() {
                        expected.insert(format!("{model_id}-{mode}"));
                    }
                }
            }
            assert_eq!(
                normalized
                    .models
                    .keys()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>(),
                expected,
                "{id}"
            );
            for (model_id, raw_model) in models {
                let model = &normalized.models[model_id];
                assert_eq!(
                    model.reasoning,
                    raw_model["reasoning"].as_bool().unwrap_or(false)
                );
                if let Some(npm) = raw_model["provider"]["npm"].as_str() {
                    if id != "cloudflare-ai-gateway" {
                        assert_eq!(model.npm, npm);
                    }
                }
            }
        }
        assert!(catalog.provider("codex-lb").unwrap().models.is_empty());
        Ok(())
    }

    #[test]
    fn configured_codex_pool_limits_survive_live_discovery_without_adding_unserved_models(
    ) -> Result<()> {
        let mut catalog = Catalog::from_json("{}")?;
        catalog.apply_config(&json!({"provider":{"codex-pool":{
            "npm":"@ai-sdk/openai","options":{"baseURL":CODEX_LB_BASE_URL},
            "models":{
                "gpt-6.1-sol":{"name":"Configured Sol","reasoning":true,
                    "limit":{"context":372000,"output":65536},
                    "options":{"reasoningSummary":"detailed"},
                    "variants":{"focused":{"reasoningEffort":"high"}}
                },
                "unserved-model":{"limit":{"context":372000,"output":65536}}
            }
        }}}))?;
        catalog.apply_discovered_models(
            "codex-pool",
            CODEX_LB_BASE_URL,
            &json!({"data":[
                {"id":"gpt-6.1-sol","metadata":{"context_window":260000,
                    "max_output_tokens":32768,"supported_reasoning_levels":[{"effort":"xhigh"}]}},
                {"id":"server-only-model"}
            ]}),
        )?;
        let provider = catalog.provider("codex-pool").unwrap();
        assert_eq!(provider.models.len(), 2);
        assert!(!provider.models.contains_key("unserved-model"));
        let model = &provider.models["gpt-6.1-sol"];
        assert_eq!(model.name, "Configured Sol");
        assert_eq!(model.limit.context, 372000);
        assert_eq!(model.limit.output, 65536);
        assert_eq!(model.metadata["options"]["reasoningSummary"], "detailed");
        assert!(model.variants.contains_key("xhigh"));
        assert!(model.variants.contains_key("focused"));
        assert!(!model.variants.contains_key("low"));
        assert_eq!(provider.protocol(model)?, Protocol::Responses);
        assert_eq!(provider.models["server-only-model"].limit.context, 0);
        Ok(())
    }

    #[test]
    fn codex_discovery_only_uses_current_server_capabilities() -> Result<()> {
        let mut catalog = Catalog::from_json("{}")?;
        assert!(catalog.provider("codex-lb").unwrap().models.is_empty());
        catalog.apply_discovered_models(
            "codex-lb",
            CODEX_LB_BASE_URL,
            &json!({"data":[{"id":"gpt-6.1-sol","metadata":{
                "context_window":64000,"max_output_tokens":8192,
                "supported_reasoning_levels":[{"effort":"xhigh"}]
            }}]}),
        )?;
        assert_eq!(
            catalog
                .model("codex-lb", "gpt-6.1-sol")
                .unwrap()
                .variants
                .len(),
            1
        );
        catalog.apply_discovered_models(
            "codex-lb",
            CODEX_LB_BASE_URL,
            &json!({"data":[{"id":"gpt-6.1-sol"},{"id":"deployment-only-model"}]}),
        )?;
        let model = catalog.model("codex-lb", "gpt-6.1-sol").unwrap();
        assert!(
            model.variants.is_empty(),
            "previous efforts must not survive a new response"
        );
        assert_eq!(
            model.limit.context, 0,
            "missing limits are unknown, not guessed"
        );
        assert_eq!(model.limit.output, 0);
        assert!(catalog.model("codex-lb", "deployment-only-model").is_some());
        assert_eq!(catalog.provider("codex-lb").unwrap().models.len(), 2);
        Ok(())
    }

    #[test]
    fn discovery_is_transactional_and_keeps_operator_declared_efforts() -> Result<()> {
        let mut catalog = Catalog::bundled()?;
        let original = catalog.provider("codex-lb").unwrap().models.len();
        assert!(catalog
            .apply_discovered_models("codex-lb", CODEX_LB_BASE_URL, &json!({"data":[]}))
            .is_err());
        assert_eq!(catalog.provider("codex-lb").unwrap().models.len(), original);
        catalog.apply_discovered_models(
            "codex-lb",
            "http://localhost:8888/v1",
            &json!({"data":[{
                "id":"gpt-5.6-sol", "supported_reasoning_efforts":["low","high","xhigh"]
            }]}),
        )?;
        let provider = catalog.provider("codex-lb").unwrap();
        assert_eq!(provider.models.len(), 1);
        let model = &provider.models["gpt-5.6-sol"];
        assert_eq!(model.api, "http://localhost:8888/v1");
        assert_eq!(model.variants["xhigh"]["reasoningEffort"], "xhigh");
        assert_eq!(provider.protocol(model)?, Protocol::Responses);
        Ok(())
    }

    #[test]
    fn codex_lb_v124_discovery_normalizes_nested_metadata_and_native_slugs() -> Result<()> {
        let mut catalog = Catalog::bundled()?;
        catalog.apply_discovered_models(
            "codex-lb",
            CODEX_LB_BASE_URL,
            &json!({"data":[{
                "id":"gpt-5.6-sol", "metadata":{
                    "display_name":"GPT-5.6-Sol", "context_window":272000,
                    "max_output_tokens":65536,
                    "supported_reasoning_levels":[{"effort":"low"},{"effort":"xhigh"}]
                }
            }]}),
        )?;
        let model = catalog.model("codex-lb", "gpt-5.6-sol").unwrap();
        assert_eq!(model.name, "GPT-5.6-Sol");
        assert_eq!(model.limit.context, 272000);
        assert_eq!(model.limit.output, 65536);
        assert_eq!(
            model.variants.keys().cloned().collect::<Vec<_>>(),
            ["low", "xhigh"]
        );
        catalog.apply_discovered_models(
            "codex-lb",
            "http://localhost/backend-api/codex",
            &json!({
                "data":[], "models":[{"slug":"custom-source", "display_name":"Custom source",
                    "context_window":64000, "supports_reasoning":false,
                    "supported_reasoning_levels":[{"effort":"high"}]
                }]
            }),
        )?;
        let model = catalog.model("codex-lb", "custom-source").unwrap();
        assert_eq!(model.name, "Custom source");
        assert_eq!(model.limit.context, 64000);
        assert!(!model.reasoning);
        assert!(model.variants.is_empty());
        Ok(())
    }

    #[test]
    fn custom_model_and_variant_overrides_preserve_defaults_and_remove_disabled() -> Result<()> {
        let mut catalog = Catalog::from_json(
            r#"{"custom":{"name":"My API","api":"http://localhost/v1","models":{"test":{"reasoning":true,"limit":{"context":32000,"output":4096}}}}}"#,
        )?;
        catalog.apply_config(&json!({"provider":{"custom":{
            "options":{"baseURL":"http://localhost:2455/v1", "apiKey":"{env:TEST_KEY}","headers":{"x-project":"test"}}, "models":{"test":{
                "name":"Test model", "variants":{"fast":{"reasoningEffort":"low"},"high":{"disabled":true}}
            }}
        }}}))?;
        let model = catalog.model("custom", "test").unwrap();
        let provider = catalog.provider("custom").unwrap();
        assert_eq!(provider.options["apiKey"], "{env:TEST_KEY}");
        assert_eq!(provider.headers["x-project"], "test");
        assert_eq!(model.name, "Test model");
        assert!(model.tool_call);
        assert_eq!(model.limit.context, 32000);
        assert_eq!(model.api, "http://localhost:2455/v1");
        assert_eq!(model.variants["fast"]["reasoningEffort"], "low");
        assert!(!model.variants.contains_key("high"));
        assert!(model.variant_options(Some("missing")).is_err());
        Ok(())
    }

    #[test]
    fn searchable_names_ids_and_punctuation_tolerant_queries() -> Result<()> {
        let catalog = Catalog::bundled()?;
        assert_eq!(catalog.search_providers("anthropic")[0].id, "anthropic");
        assert!(!catalog.search_models("openai", "gpt").is_empty());
        assert!(catalog.search_models("nonexistent", "").is_empty());
        assert!(search_score("gpt54", &["gpt-5.4", "GPT 5.4"]).is_some());
        Ok(())
    }

    #[test]
    fn discovery_endpoint_strips_completion_suffix_without_corrupting_query() -> Result<()> {
        let (base, endpoint) =
            discovery_urls("http://localhost:2455/v1/responses?api-version=2026-01")?;
        assert_eq!(base.path(), "/v1");
        assert_eq!(endpoint.path(), "/v1/models");
        assert_eq!(endpoint.query(), Some("api-version=2026-01"));
        assert!(discovery_urls("file:///tmp/models").is_err());
        Ok(())
    }

    #[test]
    fn custom_openai_sdk_uses_responses_and_cloudflare_keeps_gateway_wrapper() -> Result<()> {
        let mut catalog = Catalog::from_json(
            r#"{
            "custom":{"npm":"@ai-sdk/openai","models":{"gpt-5.4":{"reasoning":true}}},
            "cloudflare-ai-gateway":{"npm":"ai-gateway-provider","models":{
                "openai/gpt-5.4":{"reasoning":true}, "anthropic/claude-sonnet-4.6":{"reasoning":true}
            }}
        }"#,
        )?;
        catalog.add_custom_provider(
            "cloudflare-ai-gateway",
            &json!({"options":{"accountId":"account", "gatewayId":"gateway"}}),
        )?;
        let provider = catalog.provider("custom").unwrap();
        assert_eq!(
            provider.protocol(&provider.models["gpt-5.4"])?,
            Protocol::Responses
        );
        let gateway = catalog.provider("cloudflare-ai-gateway").unwrap();
        for model in gateway.models.values() {
            assert_eq!(gateway.protocol(model)?, Protocol::Sdk);
            assert_ne!(model.npm, "ai-gateway-provider");
            assert!(!model.variants.is_empty());
        }
        Ok(())
    }

    #[test]
    fn experimental_modes_keep_wire_id_and_provider_options_without_losing_capabilities(
    ) -> Result<()> {
        let mut catalog = Catalog::from_json(
            r#"{"custom":{"npm":"@ai-sdk/openai","models":{
            "gpt-5.4":{"name":"GPT 5.4","reasoning":true,"cost":{"input":2,"output":10},
                "experimental":{"modes":{"fast":{"cost":{"output":20},"provider":{
                    "body":{"service_tier":"priority","reasoning":{"mode":"fast"}},
                    "headers":{"x-model-mode":"fast"}
                }}}}
            }, "no-tools":{"tool_call":false}
        }}}"#,
        )?;
        catalog.add_custom_provider(
            "custom",
            &json!({"options":{"baseURL":"http://localhost/v1"}}),
        )?;
        let mode = catalog.model("custom", "gpt-5.4-fast").unwrap();
        assert_eq!(mode.api_id, "gpt-5.4");
        assert_eq!(mode.name, "GPT 5.4 Fast");
        assert!(mode.tool_call);
        assert!(mode.reasoning);
        assert_eq!(mode.cost["input"], 2);
        assert_eq!(mode.cost["output"], 20);
        assert_eq!(mode.metadata["options"]["serviceTier"], "priority");
        assert_eq!(mode.metadata["options"]["reasoningMode"], "fast");
        assert_eq!(mode.metadata["headers"]["x-model-mode"], "fast");
        assert!(!catalog.model("custom", "no-tools").unwrap().tool_call);
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_live_discovery_uses_models_endpoint_and_declared_capabilities(
    ) -> Result<()> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut request = Vec::new();
            loop {
                let mut chunk = [0u8; 1024];
                let count = socket.read(&mut chunk).await?;
                if count == 0 {
                    bail!("discovery request closed early");
                }
                request.extend_from_slice(&chunk[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                if request.len() > 8192 {
                    bail!("discovery request exceeds test limit");
                }
            }
            let request = String::from_utf8(request)?;
            assert!(request.starts_with("GET /v1/models?api-version=2026-01 HTTP/1.1\r\n"));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-discovery-key\r\n"));
            let body = json!({"data":[{"id":"gpt-5.6-sol","metadata":{
                "display_name":"Sol", "context_window":272000,"max_output_tokens":65536,
                "supported_reasoning_levels":[{"effort":"high"},{"effort":"xhigh"}]
            }}]})
            .to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await?;
            Ok::<_, anyhow::Error>(())
        });
        let mut catalog = Catalog::bundled()?;
        let count = catalog
            .refresh_models(
                "codex-lb",
                &format!("http://{address}/v1/responses?api-version=2026-01"),
                Some("test-discovery-key"),
            )
            .await?;
        server.await??;
        assert_eq!(count, 1);
        let model = catalog.model("codex-lb", "gpt-5.6-sol").unwrap();
        assert_eq!(model.limit.context, 272000);
        assert_eq!(model.variants.len(), 2);
        assert!(model.api.ends_with("/v1?api-version=2026-01"));
        Ok(())
    }
    #[test]
    fn gitlab_discovery_augments_models_and_retains_workflow_refs_and_limits() -> Result<()> {
        let mut catalog = Catalog::from_json(
            r#"{"gitlab":{"npm":"gitlab-ai-provider","api":"https://gitlab.com","models":{"configured":{"tool_call":true,"options":{"workflowRef":"keep"}}}}}"#,
        )?;
        let discovered = json!({"id":"duo-workflow-custom","name":"Agent Platform (Custom)","reasoning":true,"tool_call":true,"limit":{"context":99999,"output":4096},"options":{"workflowRef":"custom/ref"},"provider":{"npm":"gitlab-ai-provider","api":"https://gitlab.example"}});
        assert_eq!(
            catalog.apply_gitlab_models("https://gitlab.example", &[discovered])?,
            1
        );
        assert!(catalog.model("gitlab", "configured").is_some());
        let model = catalog.model("gitlab", "duo-workflow-custom").unwrap();
        assert_eq!(model.metadata["options"]["workflowRef"], "custom/ref");
        assert_eq!(model.limit.context, 99999);
        assert_eq!(model.api, "https://gitlab.example");
        assert_eq!(model.npm, "gitlab-ai-provider");
        assert!(model.reasoning && model.tool_call);
        assert_eq!(
            catalog.apply_gitlab_models("https://gitlab.example", &[])?,
            0
        );
        assert_eq!(
            catalog.apply_gitlab_models(
                "https://gitlab.example",
                &[json!({"id":"configured","options":{"workflowRef":"overwrite"}})]
            )?,
            0
        );
        assert_eq!(
            catalog.model("gitlab", "configured").unwrap().metadata["options"]["workflowRef"],
            "keep"
        );
        Ok(())
    }
}
