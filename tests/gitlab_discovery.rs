use anyhow::{ensure, Result};
use openraid::{
    auth::AuthStore, catalog::Catalog, config::Config, quick::ProviderManager, runtime::Harness,
};
use serde_json::{json, Value};

fn resolve(
    catalog: &Catalog,
    current: &Config,
    provider: &str,
    model: &str,
    _: Option<&str>,
    auth: &AuthStore,
) -> Result<Config> {
    let metadata = catalog.model(provider, model).unwrap();
    ensure!(
        metadata.limit.context == 99999 && metadata.tool_call,
        "discovery capabilities lost"
    );
    let mut next = current.clone();
    next.provider = provider.to_owned();
    next.model = model.to_owned();
    next.base_url = auth
        .connection_endpoint(provider)
        .unwrap_or(&metadata.api)
        .to_owned();
    next.provider_options = metadata.metadata["options"].clone();
    Ok(next)
}

#[tokio::test]
#[ignore = "requires Node.js; validates actual public CLI and live menu through the shared discovery sidecar"]
async fn gitlab_discovery_reaches_public_listing_and_live_menu_without_replacing_catalog(
) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let bridge = directory.path().join("bridge");
    std::fs::create_dir(&bridge)?;
    std::fs::write(
        bridge.join("sdk-bridge.mjs"),
        r#"
import {createInterface} from 'node:readline';
for await (const line of createInterface({input:process.stdin})) {
  const {id,request}=JSON.parse(line);
  const meta=request.options._openraid_sdk;
  const expected=request.apiKey==='discovery-key'?'https://gitlab.explicit':request.apiKey==='reconnected-key'?'https://gitlab.reconnected':null;
  if(meta.action!=='discover-models'||!meta.workspace||!expected||request.baseURL!==expected) {
    process.stdout.write(JSON.stringify({id,error:{message:'discovery request contract violated'}})+'\n');continue;
  }
  const models=[{id:'duo-workflow-discovered',name:'Agent Platform (Discovered)',reasoning:true,tool_call:true,
    limit:{context:99999,output:4096},provider:{npm:'gitlab-ai-provider',api:request.baseURL},options:{workflowRef:'custom/ref'}}];
  process.stdout.write(JSON.stringify({id,completion:{content:'',tool_calls:[],usage:{input_tokens:0,output_tokens:0,cached_tokens:0},finish_reason:'stop',response_items:models}})+'\n');
}
"#,
    )?;
    let auth_path = directory.path().join("auth.json");
    let mut auth = AuthStore::load_with_opencode(&auth_path, None)?;
    auth.set_api_key("gitlab", "discovery-key")?;
    auth.set_connection_endpoint("gitlab", "https://gitlab.saved");
    auth.save()?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_openraid"))
        .args([
            "models",
            "gitlab",
            "--json",
            "--refresh",
            "--api-key",
            "discovery-key",
            "--base-url",
            "https://gitlab.explicit",
        ])
        .current_dir(directory.path())
        .env("OPENRAID_AUTH_FILE", &auth_path)
        .env("OPENRAID_SDK_BRIDGE_DIR", &bridge)
        .env("XDG_CONFIG_HOME", directory.path())
        .env("XDG_DATA_HOME", directory.path())
        .output()?;
    ensure!(
        output.status.success(),
        "public discovery failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let public: Vec<Value> = serde_json::from_slice(&output.stdout)?;
    let discovered = public
        .iter()
        .find(|model| model["id"] == "duo-workflow-discovered")
        .unwrap();
    assert_eq!(
        discovered["metadata"]["options"]["workflowRef"],
        "custom/ref"
    );
    assert!(public.len() > 1, "static catalog must remain available");

    // This integration binary contains one test, so its sidecar environment is
    // isolated from other Cargo test binaries and starts before first bridge use.
    std::env::set_var("OPENRAID_SDK_BRIDGE_DIR", &bridge);
    let mut catalog = Catalog::from_json(
        r#"{"gitlab":{"npm":"gitlab-ai-provider","api":"https://gitlab.example","env":[],"models":{"static":{"tool_call":true}}}}"#,
    )?;
    catalog.providers.remove("codex-lb");
    let config = Config {
        provider: "gitlab".into(),
        model: "static".into(),
        api_key: Some("discovery-key".into()),
        base_url: "https://gitlab.explicit".into(),
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
    assert!(models.iter().any(|model| model.id == "gitlab/static"));
    assert!(models
        .iter()
        .any(|model| model.id == "gitlab/duo-workflow-discovered"));
    manager
        .select("gitlab/duo-workflow-discovered", None)
        .await?;
    assert_eq!(
        harness.control.current().config.provider_options,
        json!({"workflowRef":"custom/ref"})
    );
    assert_eq!(
        harness.control.current().config.base_url,
        "https://gitlab.explicit"
    );
    manager
        .connect(
            "gitlab",
            "reconnected-key",
            Some("https://gitlab.reconnected"),
        )
        .await?;
    assert!(manager
        .models()
        .await?
        .iter()
        .any(|model| model.id == "gitlab/duo-workflow-discovered"));
    manager
        .select("gitlab/duo-workflow-discovered", None)
        .await?;
    assert_eq!(
        harness.control.current().config.base_url,
        "https://gitlab.reconnected"
    );
    assert_eq!(
        harness.control.current().config.api_key.as_deref(),
        Some("reconnected-key")
    );
    assert!(harness.store.prompts().await?.is_empty());
    std::env::remove_var("OPENRAID_SDK_BRIDGE_DIR");
    Ok(())
}
