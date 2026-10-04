use super::*;
use openraid::auth::LaunchProfile;
use std::path::Path;

fn isolated_auth(path: &Path) -> AuthStore {
    AuthStore::load_with_opencode(path.join("output-auth.json"), None).unwrap()
}

fn output_catalog() -> Catalog {
    Catalog::from_json(
        r#"{
        "openai-output":{"env":[],"npm":"@ai-sdk/openai","api":"http://localhost/v1",
            "models":{"reasoning":{"reasoning":true,"tool_call":true,"limit":{"context":200000,"output":64000}},
                "small":{"tool_call":true,"limit":{"context":32000,"output":8192}},
                "unknown-limit":{"tool_call":true,"limit":{"context":200000,"output":0}}}},
        "google-output":{"env":[],"npm":"@ai-sdk/google","api":"http://localhost/v1beta",
            "models":{"reasoning":{"reasoning":true,"tool_call":true,"limit":{"context":1000000,"output":65536}}}},
        "anthropic-output":{"env":[],"npm":"@ai-sdk/anthropic","api":"http://localhost/v1",
            "models":{"claude-sonnet-4":{"reasoning":true,"tool_call":true,"limit":{"context":200000,"output":64000},
                "variants":{"focused":{"thinking":{"type":"enabled","budgetTokens":12000}}}}}}
        }"#,
    )
    .unwrap()
}

fn fixture_config(path: &Path, provider: &str, model: &str) -> Config {
    Config {
        provider: provider.into(),
        model: model.into(),
        workspace: path.to_owned(),
        objective: "verify reasoning output headroom".into(),
        base_url: String::new(),
        api_key: Some("fixture-key".into()),
        ..Config::default()
    }
}

#[test]
fn catalog_output_defaults_reserve_full_reasoning_and_generation_allowance() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = output_catalog();
    let auth = isolated_auth(temp.path());
    for (provider, model, output, protocol) in [
        ("openai-output", "reasoning", 64000, Protocol::Responses),
        ("google-output", "reasoning", 65536, Protocol::Gemini),
        (
            "anthropic-output",
            "claude-sonnet-4",
            64000,
            Protocol::Anthropic,
        ),
    ] {
        let mut config = fixture_config(temp.path(), provider, model);
        if provider == "anthropic-output" {
            config.variant = Some("focused".into());
        }
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        assert_eq!(config.max_output_tokens, output, "{provider}");
        assert_eq!(config.protocol, protocol);
        assert!(!config.explicit_max_output_tokens);
        config.validate().unwrap();
        if provider == "anthropic-output" {
            assert_eq!(config.provider_options["thinking"]["budgetTokens"], 12000);
            assert_eq!(config.max_output_tokens - 12000, 52000);
        }
    }
}

#[test]
fn configured_alias_uses_alias_catalog_output_limit() {
    let temp = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::from_json("{}").unwrap();
    catalog
        .apply_config(&json!({"provider":{"alias-output":{
            "npm":"@ai-sdk/openai", "options":{"baseURL":"http://localhost/v1"},
            "models":{"display-alias":{"id":"wire-reasoning-model","reasoning":true,
                "tool_call":true,"limit":{"context":372000,"output":65536}}}
        }}}))
        .unwrap();
    let mut config = fixture_config(temp.path(), "alias-output", "display-alias");
    configure_provider_with_auth(&catalog, &mut config, None, &isolated_auth(temp.path())).unwrap();
    assert_eq!(config.api_model.as_deref(), Some("wire-reasoning-model"));
    assert_eq!(config.max_output_tokens, 65536);
    assert_eq!(config.context_budget, 372000);
}

#[test]
fn explicit_output_is_preserved_or_clamped_and_zero_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = output_catalog();
    let auth = isolated_auth(temp.path());
    for (requested, expected) in [(4096, 4096), (128000, 64000), (0, 0)] {
        let mut config = fixture_config(temp.path(), "openai-output", "reasoning");
        config.explicit_max_output_tokens = true;
        config.max_output_tokens = requested;
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        assert_eq!(config.max_output_tokens, expected);
        assert!(config.explicit_max_output_tokens);
        assert_eq!(config.validate().is_ok(), requested > 0);
    }
}

#[test]
fn incompatible_explicit_thinking_budget_does_not_silently_raise_output() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = fixture_config(temp.path(), "anthropic-output", "claude-sonnet-4");
    config.variant = Some("focused".into());
    config.explicit_max_output_tokens = true;
    config.max_output_tokens = 4096;
    let error = configure_provider_with_auth(
        &output_catalog(),
        &mut config,
        None,
        &isolated_auth(temp.path()),
    )
    .unwrap_err();
    assert!(error.to_string().contains("thinking"));
    assert_eq!(config.max_output_tokens, 4096);
}

#[test]
fn google_shared_thinking_budget_keeps_headroom_and_respects_explicit_output() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = output_catalog();
    let auth = isolated_auth(temp.path());
    for explicit in [false, true] {
        let mut config = fixture_config(temp.path(), "google-output", "reasoning");
        config.provider_options = json!({"thinkingConfig":{"thinkingBudget":12000}});
        config.explicit_max_output_tokens = explicit;
        config.max_output_tokens = 4096;
        let result = configure_provider_with_auth(&catalog, &mut config, None, &auth);
        if explicit {
            assert!(result.unwrap_err().to_string().contains("thinking"));
            assert_eq!(config.max_output_tokens, 4096);
        } else {
            result.unwrap();
            assert_eq!(config.max_output_tokens, 65536);
            assert_eq!(config.max_output_tokens - 12000, 53536);
        }
    }
}

#[test]
fn unknown_and_zero_catalog_limits_use_reasoning_friendly_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = output_catalog();
    let auth = isolated_auth(temp.path());
    for model in ["unknown-limit", "custom-model"] {
        let mut config = fixture_config(temp.path(), "openai-output", model);
        config.base_url = "http://localhost/v1".into();
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        assert_eq!(config.max_output_tokens, 16384, "{model}");
        config.validate().unwrap();
    }
}

#[test]
fn small_explicit_context_keeps_useful_input_headroom() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = output_catalog();
    let auth = isolated_auth(temp.path());
    for explicit_output in [false, true] {
        let mut config = fixture_config(temp.path(), "openai-output", "reasoning");
        config.context_budget = 8000;
        config.explicit_context_budget = true;
        config.explicit_max_output_tokens = explicit_output;
        config.max_output_tokens = 64000;
        configure_provider_with_auth(&catalog, &mut config, None, &auth).unwrap();
        assert_eq!(config.context_budget, 8000);
        assert_eq!(config.max_output_tokens, 2000);
        assert_eq!(
            config.context_budget - config.max_output_tokens as usize,
            6000
        );
        config.validate().unwrap();
    }
}

#[test]
fn live_model_switch_recalculates_implicit_output_in_both_directions() {
    let temp = tempfile::tempdir().unwrap();
    let catalog = output_catalog();
    let auth = isolated_auth(temp.path());
    let mut initial = fixture_config(temp.path(), "openai-output", "reasoning");
    configure_provider_with_auth(&catalog, &mut initial, None, &auth).unwrap();
    let smaller =
        resolve_live_selection(&catalog, &initial, "openai-output", "small", None, &auth).unwrap();
    assert_eq!(smaller.max_output_tokens, 8192);
    let larger = resolve_live_selection(
        &catalog,
        &smaller,
        "openai-output",
        "reasoning",
        None,
        &auth,
    )
    .unwrap();
    assert_eq!(larger.max_output_tokens, 64000);
    let cross_provider =
        resolve_live_selection(&catalog, &larger, "google-output", "reasoning", None, &auth)
            .unwrap();
    assert_eq!(cross_provider.max_output_tokens, 65536);

    initial.explicit_max_output_tokens = true;
    initial.max_output_tokens = 4096;
    let explicit =
        resolve_live_selection(&catalog, &initial, "openai-output", "small", None, &auth).unwrap();
    assert_eq!(explicit.max_output_tokens, 4096);
    assert!(explicit.explicit_max_output_tokens);
}

#[test]
fn cli_distinguishes_omitted_output_from_explicit_override() {
    let Some(Command::Run(implicit)) = Cli::parse_from(["openraid", "run", "fixture"]).command
    else {
        panic!("run arguments")
    };
    assert_eq!(implicit.max_output_tokens, None);
    let Some(Command::Run(explicit)) =
        Cli::parse_from(["openraid", "run", "fixture", "--max-output-tokens", "4096"]).command
    else {
        panic!("run arguments")
    };
    assert_eq!(explicit.max_output_tokens, Some(4096));
}

#[test]
fn saved_profiles_round_trip_output_intent_and_migrate_legacy_defaults() {
    let temp = tempfile::tempdir().unwrap();
    for explicit in [false, true] {
        let mut auth = isolated_auth(temp.path());
        let mut original = fixture_config(temp.path(), "openai-output", "reasoning");
        original.max_output_tokens = if explicit { 4096 } else { 64000 };
        original.explicit_max_output_tokens = explicit;
        auth.remember_launch(&original);
        auth.save().unwrap();
        let auth = isolated_auth(temp.path());
        let mut restored = Config::default();
        auth.launch_profile().unwrap().apply(&mut restored);
        assert_eq!(restored.max_output_tokens, original.max_output_tokens);
        assert_eq!(restored.explicit_max_output_tokens, explicit);
        restored.use_model_output_limit(Some(8192));
        assert_eq!(
            restored.max_output_tokens,
            if explicit { 4096 } else { 8192 }
        );

        let mut legacy = serde_json::to_value(auth.launch_profile().unwrap()).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("explicit_max_output_tokens");
        for (legacy_output, expected_explicit, expected_large_context) in
            [(4096, false, 64000), (8000, true, 8000)]
        {
            legacy["max_output_tokens"] = json!(legacy_output);
            let profile: LaunchProfile = serde_json::from_value(legacy.clone()).unwrap();
            profile.apply(&mut restored);
            assert_eq!(restored.explicit_max_output_tokens, expected_explicit);
            restored.use_model_output_limit(Some(64000));
            assert_eq!(restored.max_output_tokens, 8000);
            restored.context_budget = 200000;
            restored.use_model_output_limit(Some(64000));
            assert_eq!(restored.max_output_tokens, expected_large_context);
        }
    }
}

#[tokio::test]
async fn remembered_setup_recalculates_defaults_and_honors_cli_override() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = std::fs::canonicalize(temp.path()).unwrap();
    let path = workspace.join("output-provider.json");
    std::fs::write(
        &path,
        json!({"provider":{"saved-output":{
            "npm":"@ai-sdk/openai", "options":{"baseURL":"http://localhost/v1"},
            "models":{"reasoning":{"reasoning":true,"tool_call":true,
                "limit":{"context":200000,"output":64000}}}
        }}})
        .to_string(),
    )
    .unwrap();
    for (saved_explicit, cli_override, expected, expected_explicit) in [
        (false, None, 64000, false),
        (true, None, 4096, true),
        (false, Some("7000"), 7000, true),
        (true, Some("7000"), 7000, true),
    ] {
        let mut auth = isolated_auth(temp.path());
        auth.remember_launch(&Config {
            provider: "saved-output".into(),
            model: "reasoning".into(),
            workspace: workspace.clone(),
            database: default_database_path(&workspace),
            base_url: "http://localhost/v1".into(),
            api_key: Some("fixture-key".into()),
            config_path: Some(path.clone()),
            context_budget: 32000,
            max_output_tokens: 4096,
            explicit_max_output_tokens: saved_explicit,
            ..Config::default()
        });
        let mut args = vec![
            "openraid",
            "setup",
            "fixture objective",
            "--no-tui",
            "--workspace",
            workspace.to_str().unwrap(),
            "--api-key",
            "fixture-key",
        ];
        if let Some(value) = cli_override {
            args.extend(["--max-output-tokens", value]);
        }
        let Some(Command::Setup(args)) = Cli::parse_from(args).command else {
            panic!("setup arguments")
        };
        let config = args
            .config_with_auth(false, true, auth)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(config.context_budget, 200000);
        assert_eq!(config.max_output_tokens, expected);
        assert_eq!(config.explicit_max_output_tokens, expected_explicit);
    }
}
