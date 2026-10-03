use openraid::{config::Config, runtime::Harness};

async fn idle_harness() -> (tempfile::TempDir, Harness) {
    let workspace = tempfile::tempdir().unwrap();
    let harness = Harness::new(Config {
        agents: 2,
        workspace: workspace.path().to_owned(),
        database: workspace.path().join("controls.sqlite3"),
        objective: String::new(),
        interactive_session: true,
        mock: true,
        no_tui: true,
        ..Config::default()
    })
    .await
    .unwrap();
    (workspace, harness)
}

#[tokio::test]
async fn initial_idle_controls_do_not_pollute_the_board() {
    let (_workspace, harness) = idle_harness().await;
    let control = &harness.control;
    let added = control.add_agents(2).await.unwrap();
    control.remove_agents(added).await.unwrap();
    control.pause().await.unwrap();
    control.resume().await.unwrap();
    let mut config = (*control.current().config).clone();
    config.model = "another-mock-model".into();
    control.switch(config).await.unwrap();
    assert!(harness.store.export_board().await.unwrap().is_empty());
    assert!(!harness.store.has_started().await.unwrap());
}

#[tokio::test]
async fn clearing_idle_board_erases_recovery_without_reusing_sequences() {
    let (_workspace, harness) = idle_harness().await;
    let store = &harness.store;
    let members = harness.control.members();
    let prompt = store.append("owner", "old objective", true).await.unwrap();
    store
        .save_checkpoint(&members[0], &serde_json::json!({"history": "old"}))
        .await
        .unwrap();
    store
        .set_vote(&members[0], false, "not finished")
        .await
        .unwrap();
    let mut last = prompt.seq;
    for index in 0..125 {
        last = store
            .append("agent", &format!("message {index}"), false)
            .await
            .unwrap()
            .seq;
    }
    let exported = store.export_board().await.unwrap();
    assert_eq!(exported.len(), 126);
    assert!(exported.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    assert_eq!(exported.last().unwrap().seq, last);
    assert!(store.has_started().await.unwrap());

    harness.control.clear_board().await.unwrap();
    assert!(store.export_board().await.unwrap().is_empty());
    assert!(!store.has_started().await.unwrap());
    let stats = store.stats().await.unwrap();
    assert_eq!(stats.board_messages, 0);
    assert_eq!(stats.checkpoints, 0);
    assert_eq!(stats.votes, 0);
    assert!(store.load_checkpoint(&members[0]).await.unwrap().is_none());
    assert!(store.vote(&members[0]).await.unwrap().is_none());
    assert_eq!(harness.control.members(), members);

    let next = harness
        .control
        .post_prompt("new objective".into())
        .await
        .unwrap();
    assert!(next > last, "runtime cursors must observe the new prompt");
    assert_eq!(store.read_board(last, 10).await.unwrap()[0].seq, next);
}

#[tokio::test]
async fn acknowledged_prompt_blocks_clear_before_runtime_is_polled() {
    let (_workspace, harness) = idle_harness().await;
    let seq = harness
        .control
        .post_prompt("queued objective".into())
        .await
        .unwrap();
    assert!(harness.control.is_busy());
    assert!(harness.control.clear_board().await.is_err());
    assert_eq!(harness.store.read_board(0, 10).await.unwrap()[0].seq, seq);
}

#[tokio::test]
async fn durable_worker_occupancy_blocks_clear_even_if_control_reports_idle() {
    let (_workspace, harness) = idle_harness().await;
    let id = harness.control.members()[0].clone();
    assert!(harness.store.mark_worker_started(&id).await.unwrap());
    assert!(!harness.control.is_busy());
    assert!(harness.control.clear_board().await.is_err());
    harness.store.mark_worker_finished(&id).await.unwrap();
    harness.control.clear_board().await.unwrap();
}

#[tokio::test]
async fn completed_sessions_keep_operator_audit_notices() {
    let (_workspace, harness) = idle_harness().await;
    harness
        .store
        .append("owner", "completed objective", true)
        .await
        .unwrap();
    assert!(!harness.control.is_busy());
    let added = harness.control.add_agents(1).await.unwrap();
    harness.control.remove_agents(added).await.unwrap();
    harness.control.pause().await.unwrap();
    harness.control.resume().await.unwrap();
    let mut config = (*harness.control.current().config).clone();
    config.model = "another-mock-model".into();
    harness.control.switch(config).await.unwrap();
    let board = harness.store.export_board().await.unwrap();
    assert!(
        board.len() >= 6,
        "started sessions should keep control audit history"
    );
    assert_eq!(board[0].body, "completed objective");
}
