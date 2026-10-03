use anyhow::Result;
use openraid::{
    auth::AuthStore, catalog::Catalog, config::Config, quick::ProviderManager, runtime::Harness,
};

fn resolve(
    catalog: &Catalog,
    current: &Config,
    provider: &str,
    model: &str,
    variant: Option<&str>,
    auth: &AuthStore,
) -> Result<Config> {
    let mut config = current.clone();
    config.provider = provider.into();
    config.model = model.into();
    config.variant = variant.map(str::to_owned);
    let info = catalog.provider(provider).unwrap();
    config.api_key = auth.api_key(provider, &info.env)?;
    config.provider_options = catalog
        .model(provider, model)
        .unwrap()
        .variant_options(variant)?;
    Ok(config)
}

#[tokio::test]
async fn public_opencode_live_menu_only_exposes_free_models_until_connected() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut catalog = Catalog::from_json(
        r#"{
        "fixture":{"env":[],"api":"http://localhost/v1","models":{"active":{}}},
        "opencode":{"env":[],"api":"http://localhost/v1","models":{
            "free":{"cost":{"input":0},"tool_call":true},
            "paid":{"cost":{"input":1},"tool_call":true}
        }}
    }"#,
    )?;
    catalog.providers.remove("codex-lb");
    let auth = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    let harness = Harness::new(Config {
        provider: "fixture".into(),
        model: "active".into(),
        base_url: "http://localhost/v1".into(),
        workspace: directory.path().to_owned(),
        database: directory.path().join("public.sqlite3"),
        mock: true,
        interactive_session: true,
        objective: String::new(),
        agents: 1,
        ..Config::default()
    })
    .await?;
    let manager = ProviderManager::new(catalog, harness.control.clone(), resolve, &auth);
    let models = manager.models().await?;
    assert!(models.iter().any(|model| model.id == "opencode/free"));
    assert!(!models.iter().any(|model| model.id == "opencode/paid"));
    manager
        .connect("opencode", "private-fixture-key", None)
        .await?;
    assert!(manager
        .models()
        .await?
        .iter()
        .any(|model| model.id == "opencode/paid"));
    Ok(())
}

#[tokio::test]
async fn models_are_connected_only_and_connect_switch_and_variants_persist() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut catalog = Catalog::from_json(
        r#"{
        "saved":{"env":[],"npm":"@ai-sdk/openai-compatible","models":{"alpha":{"reasoning":true,"tool_call":true}}},
        "other":{"env":[],"npm":"@ai-sdk/openai-compatible","models":{"beta":{"reasoning":true,"tool_call":true}}}
    }"#,
    )?;
    catalog.providers.remove("codex-lb");
    let mut auth = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    auth.set_api_key("saved", "saved-key")?;
    auth.save()?;
    let config = Config {
        provider: "saved".into(),
        model: "alpha".into(),
        api_key: Some("saved-key".into()),
        workspace: directory.path().to_owned(),
        database: directory.path().join("quick.sqlite3"),
        mock: true,
        interactive_session: true,
        objective: String::new(),
        agents: 1,
        ..Config::default()
    };
    let harness = Harness::new(config).await?;
    let manager = ProviderManager::new(catalog, harness.control.clone(), resolve, &auth);
    let models = manager.models().await?;
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "saved/alpha");
    assert_eq!(
        manager.providers()?.len(),
        2,
        "connect lists all available providers"
    );
    assert!(
        manager.needs_endpoint("other")?,
        "known native custom provider still needs its missing URL"
    );
    manager
        .connect("other", "new-key", Some("http://localhost/v1"))
        .await?;
    assert!(
        !manager.needs_endpoint("other")?,
        "connected endpoint is reused"
    );
    assert_eq!(manager.models().await?.len(), 2);
    manager.select("other/beta", Some("high")).await?;
    assert_eq!(harness.control.current().config.provider, "other");
    assert_eq!(
        harness.control.current().config.api_key.as_deref(),
        Some("new-key")
    );
    assert_eq!(
        harness.control.current().config.variant.as_deref(),
        Some("high")
    );
    assert!(
        harness.store.prompts().await?.is_empty(),
        "switching is not a user prompt"
    );
    let saved = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    assert_eq!(saved.selection().unwrap().model, "beta");
    assert_eq!(saved.launch_profile().unwrap().agents, 1);
    manager.cycle_variant().await?;
    assert_ne!(
        harness.control.current().config.variant.as_deref(),
        Some("high")
    );
    Ok(())
}

#[test]
fn launch_profile_persists_preferences_without_storing_ephemeral_key_or_objective() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut auth = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    let config = Config {
        agents: 50,
        provider: "openai".into(),
        model: "gpt-5.4".into(),
        api_key: Some("session-only-secret".into()),
        objective: "do not automatically run this again".into(),
        ..Config::default()
    };
    auth.remember_launch(&config);
    auth.save()?;
    let bytes = std::fs::read_to_string(auth.path())?;
    assert!(!bytes.contains("session-only-secret"));
    assert!(!bytes.contains(&config.objective));
    let reloaded = AuthStore::load_with_opencode(auth.path(), None)?;
    assert_eq!(reloaded.launch_profile().unwrap().agents, 50);
    assert_eq!(reloaded.selection().unwrap().model, "gpt-5.4");
    Ok(())
}

#[tokio::test]
async fn active_uncatalogued_model_is_selectable_without_overwriting_inherited_models() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let mut catalog = Catalog::from_json(
        r#"{
        "fixture":{"env":[],"npm":"@ai-sdk/openai-compatible","api":"http://catalog.example/v1",
            "models":{"catalog-model":{"tool_call":true}}}
    }"#,
    )?;
    catalog.providers.remove("codex-lb");
    let mut auth = AuthStore::load_with_opencode(directory.path().join("auth.json"), None)?;
    auth.set_api_key("fixture", "fixture-key")?;
    auth.save()?;
    let config = Config {
        provider: "fixture".into(),
        model: "server-only-model".into(),
        api_model: Some("wire-model".into()),
        api_key: Some("fixture-key".into()),
        base_url: "http://localhost/custom".into(),
        protocol: openraid::provider::Protocol::Responses,
        provider_npm: "@ai-sdk/openai-compatible".into(),
        variant: Some("focused".into()),
        provider_options: serde_json::json!({"reasoningEffort":"high"}),
        workspace: directory.path().to_owned(),
        database: directory.path().join("quick.sqlite3"),
        mock: true,
        interactive_session: true,
        agents: 1,
        ..Config::default()
    };
    let harness = Harness::new(config).await?;
    fn inspect_resolver(
        catalog: &Catalog,
        current: &Config,
        provider: &str,
        model: &str,
        variant: Option<&str>,
        auth: &AuthStore,
    ) -> Result<Config> {
        assert_eq!(
            catalog.model(provider, "catalog-model").unwrap().api,
            "http://catalog.example/v1"
        );
        let metadata = catalog.model(provider, model).unwrap();
        assert_eq!(metadata.api, "http://localhost/custom");
        assert_eq!(metadata.api_id, "wire-model");
        assert_eq!(metadata.metadata["_openraid_protocol"], "responses");
        resolve(catalog, current, provider, model, variant, auth)
    }
    let manager = ProviderManager::new(catalog, harness.control.clone(), inspect_resolver, &auth);
    let choices = manager.models().await?;
    assert!(choices
        .iter()
        .any(|choice| choice.id == "fixture/server-only-model"));
    assert!(manager
        .variants()
        .await?
        .iter()
        .any(|choice| choice.id == "focused"));
    manager
        .select("fixture/server-only-model", Some("focused"))
        .await?;
    assert_eq!(
        harness.control.current().config.provider_options["reasoningEffort"],
        "high"
    );
    Ok(())
}
