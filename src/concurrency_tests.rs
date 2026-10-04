//! Regression coverage for agent-scaled provider and subprocess concurrency.
use super::*;
use openraid::auth::LaunchProfile;

fn auth_at(root: &std::path::Path) -> AuthStore {
    AuthStore::load_with_opencode(root.join("concurrency-auth.json"), None).unwrap()
}

fn args_at(root: &std::path::Path, extras: &[&str]) -> RunArgs {
    let mut argv = vec![
        "openraid",
        "demo",
        "--no-tui",
        "--workspace",
        root.to_str().unwrap(),
    ];
    argv.extend_from_slice(extras);
    let Some(Command::Demo(args)) = Cli::try_parse_from(argv).unwrap().command else {
        panic!("expected demo arguments");
    };
    args
}

#[tokio::test]
async fn omitted_concurrency_scales_with_final_agent_count() {
    let root = tempfile::tempdir().unwrap();
    for agents in [1usize, 8, 32, 50, 100, 500] {
        let count = agents.to_string();
        let args = args_at(root.path(), &["--agents", &count]);
        assert_eq!(args.max_in_flight, None);
        assert_eq!(args.max_processes, None);
        let config = args
            .config_with_auth(true, false, auth_at(root.path()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(config.max_in_flight, agents.max(32));
        assert_eq!(config.max_processes, agents.max(4));
        assert!(!config.explicit_max_in_flight);
        assert!(!config.explicit_max_processes);
    }
}

#[tokio::test]
async fn explicit_provider_and_process_limits_are_independent() {
    let root = tempfile::tempdir().unwrap();
    for (extras, expected) in [
        (vec!["--max-in-flight", "2"], (2, 100, true, false)),
        (vec!["--max-processes", "3"], (100, 3, false, true)),
        (
            vec!["--max-in-flight", "200", "--max-processes", "250"],
            (200, 250, true, true),
        ),
        (
            vec!["--max-in-flight", "32", "--max-processes", "4"],
            (32, 4, true, true),
        ),
    ] {
        let mut flags = vec!["--agents", "100"];
        flags.extend(extras);
        let config = args_at(root.path(), &flags)
            .config_with_auth(true, false, auth_at(root.path()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                config.max_in_flight,
                config.max_processes,
                config.explicit_max_in_flight,
                config.explicit_max_processes,
            ),
            expected
        );
    }
}

#[tokio::test]
async fn explicit_zero_concurrency_is_rejected_instead_of_defaulted() {
    let root = tempfile::tempdir().unwrap();
    for flag in ["--max-in-flight", "--max-processes"] {
        let error = args_at(root.path(), &["--agents", "100", flag, "0"])
            .config_with_auth(true, false, auth_at(root.path()))
            .await
            .err()
            .expect("zero must fail validation");
        assert!(error.to_string().contains("concurrency must be positive"));
    }
}

fn remember(root: &std::path::Path, explicit: bool) -> AuthStore {
    let mut config = Config {
        workspace: std::fs::canonicalize(root).unwrap(),
        database: default_database_path(root),
        objective: "concurrency fixture".into(),
        agents: 50,
        max_in_flight: if explicit { 2 } else { 50 },
        max_processes: if explicit { 3 } else { 50 },
        explicit_max_in_flight: explicit,
        explicit_max_processes: explicit,
        ..Config::default()
    };
    config.resolve_concurrency();
    let mut auth = auth_at(root);
    auth.remember_launch(&config);
    auth.save().unwrap();
    auth_at(root)
}

#[tokio::test]
async fn remembered_implicit_limits_recompute_and_cli_overrides_win() {
    let root = tempfile::tempdir().unwrap();
    remember(root.path(), false);
    let config = args_at(root.path(), &["--agents", "100"])
        .config_with_auth(true, true, auth_at(root.path()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((config.max_in_flight, config.max_processes), (100, 100));
    assert!(!config.explicit_max_in_flight);
    assert!(!config.explicit_max_processes);

    let config = args_at(
        root.path(),
        &[
            "--agents",
            "100",
            "--max-in-flight",
            "7",
            "--max-processes",
            "9",
        ],
    )
    .config_with_auth(true, true, auth_at(root.path()))
    .await
    .unwrap()
    .unwrap();
    assert_eq!((config.max_in_flight, config.max_processes), (7, 9));
    assert!(config.explicit_max_in_flight);
    assert!(config.explicit_max_processes);
}

#[tokio::test]
async fn remembered_explicit_limits_survive_agent_count_changes_and_cli_override() {
    let root = tempfile::tempdir().unwrap();
    remember(root.path(), true);
    let config = args_at(root.path(), &["--agents", "100"])
        .config_with_auth(true, true, auth_at(root.path()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((config.max_in_flight, config.max_processes), (2, 3));
    assert!(config.explicit_max_in_flight);
    assert!(config.explicit_max_processes);

    let config = args_at(root.path(), &["--agents", "100", "--max-processes", "4"])
        .config_with_auth(true, true, auth_at(root.path()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((config.max_in_flight, config.max_processes), (2, 4));
}

#[test]
fn legacy_profiles_distinguish_historical_defaults_from_custom_limits() {
    let root = tempfile::tempdir().unwrap();
    let auth = remember(root.path(), false);
    let mut legacy = serde_json::to_value(auth.launch_profile().unwrap()).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("explicit_max_in_flight");
    legacy
        .as_object_mut()
        .unwrap()
        .remove("explicit_max_processes");
    for (http, processes, expected) in [(32, 4, (100, 100)), (2, 3, (2, 3))] {
        legacy["max_in_flight"] = json!(http);
        legacy["max_processes"] = json!(processes);
        let profile: LaunchProfile = serde_json::from_value(legacy.clone()).unwrap();
        let mut config = Config::default();
        profile.apply(&mut config);
        config.agents = 100;
        config.resolve_concurrency();
        assert_eq!((config.max_in_flight, config.max_processes), expected);
    }
}

#[tokio::test]
async fn library_harness_scales_defaults_and_preserves_custom_or_explicit_limits() {
    for (limits, explicit, expected) in [
        (None, false, (100, 100)),
        (Some((2, 3)), false, (2, 3)),
        (Some((32, 8)), true, (32, 8)),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config {
            workspace: root.path().to_owned(),
            database: default_database_path(root.path()),
            objective: "verify library launch concurrency".into(),
            mock: true,
            no_tui: true,
            agents: 100,
            explicit_max_in_flight: explicit,
            explicit_max_processes: explicit,
            ..Config::default()
        };
        if let Some((http, processes)) = limits {
            config.max_in_flight = http;
            config.max_processes = processes;
        }
        let harness = Harness::new(config).await.unwrap();
        let active = harness.control.current();
        assert_eq!(
            (active.config.max_in_flight, active.config.max_processes),
            expected
        );
    }
}
