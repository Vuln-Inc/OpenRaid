use std::{collections::HashSet, time::Duration};

use openraid::{config::Config, runtime::Harness, storage::Store};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_one_hundred_and_five_hundred_workers_deliver_global_board_and_drain() {
    let workspace = tempfile::tempdir().unwrap();
    // Reuse the same database, including a smaller run after 500, to prove that
    // old handshakes remain globally readable without contaminating run evidence.
    for agents in [50, 100, 500, 8] {
        let config = Config {
            agents,
            workspace: workspace.path().to_owned(),
            database: workspace.path().join("scale.sqlite"),
            objective: format!("offline global delivery verification with {agents} participants"),
            mock: true,
            no_tui: true,
            grace_period: Duration::ZERO,
            ..Config::default()
        };
        let harness = Harness::new(config).await.unwrap();
        let metrics = harness.metrics.clone();
        let store = harness.store.clone();
        let before = store.latest_seq().await.unwrap();
        let summary = harness.run().await.unwrap();
        assert_eq!(summary.agents, agents);
        assert_eq!(summary.finished_agents, agents);
        assert!(summary.votes >= (agents * 3).div_ceil(4));
        assert_eq!(metrics.snapshot().finished, agents);
        let mut cursor = before;
        let mut senders = HashSet::new();
        let mut quitting = HashSet::new();
        loop {
            let page = store.read_board(cursor, 17).await.unwrap();
            if page.is_empty() {
                break;
            }
            for entry in page {
                cursor = entry.seq;
                if entry.body == "offline smoke: present and cooperating" {
                    senders.insert(entry.sender.clone());
                }
                if entry.body == "quitting after harness completion consensus" {
                    quitting.insert(entry.sender);
                }
            }
        }
        assert_eq!(senders.len(), agents);
        assert_eq!(quitting, senders);
        for id in &senders {
            if let Some(vote) = store.vote(id).await.unwrap().filter(|vote| vote.done) {
                assert!(vote.reason.contains(&format!("all {agents} peer messages")));
            }
        }
        assert!(metrics.snapshot().tools >= (agents * 3) as u64);
        store.flush().await.unwrap();
    }
    // A second handle exercises durable visibility beyond in-process watch state.
    let reopened = Store::open(workspace.path().join("scale.sqlite"))
        .await
        .unwrap();
    assert!(reopened.latest_seq().await.unwrap() > 1_000);
}

#[tokio::test]
async fn library_validation_rejects_invalid_configuration_before_database_creation() {
    let workspace = tempfile::tempdir().unwrap();
    let database = workspace.path().join("invalid.sqlite");
    let config = Config {
        agents: 501,
        objective: "test".into(),
        mock: true,
        workspace: workspace.path().to_owned(),
        database: database.clone(),
        ..Config::default()
    };
    assert!(Harness::new(config).await.is_err());
    assert!(!database.exists());
}

#[tokio::test]
async fn oversized_immutable_system_is_rejected_before_creating_state() {
    let workspace = tempfile::tempdir().unwrap();
    let database = workspace.path().join("oversized.sqlite");
    let config = Config {
        objective: "oversized owner instruction ".repeat(10_000),
        mock: true,
        workspace: workspace.path().to_owned(),
        database: database.clone(),
        ..Config::default()
    };
    let error = Harness::new(config)
        .await
        .err()
        .expect("oversized system must fail validation");
    assert!(error.to_string().contains("immutable system prompt"));
    assert!(!database.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_owner_post_revokes_votes_and_wakes_parked_workers() {
    let workspace = tempfile::tempdir().unwrap();
    let database = workspace.path().join("owner.sqlite");
    let config = Config {
        agents: 2,
        objective: "verify delivery".into(),
        mock: true,
        workspace: workspace.path().to_owned(),
        database: database.clone(),
        grace_period: Duration::from_millis(300),
        ..Config::default()
    };
    let harness = Harness::new(config).await.unwrap();
    let external = Store::open(&database).await.unwrap();
    let run = tokio::spawn(harness.run());
    loop {
        if external
            .votes()
            .await
            .unwrap()
            .iter()
            .filter(|vote| vote.done)
            .count()
            == 2
        {
            break;
        }
        // Observation cadence only; no request or operation deadline.
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let owner = external
        .append("owner", "recheck delivery before completing", true)
        .await
        .unwrap();
    let summary = run.await.unwrap().unwrap();
    assert_eq!(summary.finished_agents, 2);
    let votes = external.votes().await.unwrap();
    assert_eq!(votes.iter().filter(|vote| vote.done).count(), 2);
    assert!(votes.iter().all(|vote| vote.board_seq >= owner.seq));
    let entries = external.read_board(owner.seq, 100).await.unwrap();
    assert!(entries
        .iter()
        .any(|entry| entry.sender == "harness" && entry.body.contains("consensus")));
}
