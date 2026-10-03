use openraid::{storage::Store, tools::ToolBus, workspace::WorkspaceTools};
use serde_json::json;

#[tokio::test]
async fn peer_posts_do_not_block_workspace_progress_or_refresh_completion_evidence() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("swarm.sqlite")).await.unwrap();
    let bus = ToolBus::new(store.clone(), WorkspaceTools::new(root.path(), 2).unwrap());
    store
        .append("owner", "coordinate and implement", true)
        .await
        .unwrap();
    bus.execute("agent-001", "board_read", &json!({}))
        .await
        .unwrap();

    // Reproduce the livelock deterministically: a peer posts after the agent's
    // board delivery, before each workspace operation. No quiet period needed.
    for iteration in 0..8 {
        store
            .append("agent-002", format!("peer update {iteration}"), false)
            .await
            .unwrap();
        let patch = format!(
            "*** Begin Patch\n*** Add File: progress-{iteration}.txt\n+iteration {iteration}\n*** End Patch"
        );
        bus.execute("agent-001", "apply_patch", &json!({"patch":patch}))
            .await
            .unwrap();
        let file = bus
            .execute(
                "agent-001",
                "read_file",
                &json!({"path":format!("progress-{iteration}.txt")}),
            )
            .await
            .unwrap();
        assert!(file.to_string().contains(&format!("iteration {iteration}")));
        bus.execute("agent-001", "list_files", &json!({}))
            .await
            .unwrap();
        bus.execute("agent-001", "search_files", &json!({"query":"iteration"}))
            .await
            .unwrap();
        let command = bus
            .execute(
                "agent-001",
                "run_command",
                &json!({"command":"echo workspace-progress"}),
            )
            .await
            .unwrap();
        assert!(command.to_string().contains("workspace-progress"));
        assert!(bus
            .execute(
                "agent-001",
                "vote_done",
                &json!({"done":true,"evidence":"workspace operation succeeded"})
            )
            .await
            .is_err());
    }
    assert!(store.votes().await.unwrap().is_empty());
    bus.execute(
        "agent-001",
        "vote_done",
        &json!({"done":false,"evidence":"reviewing peer updates"}),
    )
    .await
    .unwrap();

    // Reading only the first page must still leave completion blocked.
    let page = bus
        .execute("agent-001", "board_read", &json!({"after":1,"limit":1}))
        .await
        .unwrap();
    assert_eq!(page["has_more"], true);
    assert!(bus
        .execute(
            "agent-001",
            "vote_done",
            &json!({"done":true,"evidence":"partial board review"})
        )
        .await
        .is_err());
    let mut cursor = page["next_cursor"].as_u64().unwrap();
    loop {
        let page = bus
            .execute(
                "agent-001",
                "board_read",
                &json!({"after":cursor,"limit":2}),
            )
            .await
            .unwrap();
        cursor = page["next_cursor"].as_u64().unwrap();
        if page["has_more"] == false {
            break;
        }
    }
    bus.execute(
        "agent-001",
        "vote_done",
        &json!({"done":true,"evidence":"all peer updates reviewed and workspace verified"}),
    )
    .await
    .unwrap();
    let vote = store.vote("agent-001").await.unwrap().unwrap();
    assert!(vote.done);
    assert_eq!(vote.board_seq, store.latest_seq().await.unwrap());
}
