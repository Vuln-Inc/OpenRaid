use openraid::{storage::Store, tools::ToolBus, workspace::WorkspaceTools};
use serde_json::json;
use std::{fs, sync::Arc};
use tokio::sync::Barrier;

#[tokio::test]
async fn write_file_creates_nested_unicode_content_and_empty_files_exactly() {
    let root = tempfile::tempdir().unwrap();
    let tools = WorkspaceTools::new(root.path(), 4).unwrap();
    let content = "// İstanbul 🦀\r\nfn main() {}\n";
    let result = tools
        .execute(
            "write_file",
            &json!({"path":"src/nested/new.rs", "content":content}),
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read(root.path().join("src/nested/new.rs")).unwrap(),
        content.as_bytes()
    );
    assert_eq!(result["created"], true);
    assert_eq!(result["bytes_written"], content.len());
    tools
        .execute("write_file", &json!({"path":"empty.txt", "content":""}))
        .await
        .unwrap();
    assert!(fs::read(root.path().join("empty.txt")).unwrap().is_empty());
}

#[tokio::test]
async fn existing_files_are_preserved_and_the_error_directs_agents_to_apply_patch() {
    let root = tempfile::tempdir().unwrap();
    let tools = WorkspaceTools::new(root.path(), 4).unwrap();
    fs::write(root.path().join("active.rs"), "original\n").unwrap();
    let error = tools
        .execute(
            "write_file",
            &json!({"path":"active.rs", "content":"replacement\n"}),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("apply_patch"));
    assert_eq!(
        fs::read_to_string(root.path().join("active.rs")).unwrap(),
        "original\n"
    );
    tools
        .execute(
            "apply_patch",
            &json!({"patch":"*** Begin Patch\n*** Update File: active.rs\n@@\n-original\n+coordinated update\n*** End Patch"}),
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("active.rs")).unwrap(),
        "coordinated update\n"
    );
    fs::create_dir(root.path().join("existing-directory")).unwrap();
    let error = tools
        .execute(
            "write_file",
            &json!({"path":"existing-directory", "content":"not a directory"}),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("apply_patch"));
    assert!(root.path().join("existing-directory").is_dir());
}

#[tokio::test]
async fn independent_competing_writers_have_exactly_one_winner_without_truncation() {
    let root = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(16));
    let mut jobs = tokio::task::JoinSet::new();
    for index in 0..16 {
        // Distinct instances ensure exclusivity comes from atomic file creation,
        // not an in-memory lock or a single shared I/O semaphore.
        let tools = WorkspaceTools::new(root.path(), 4).unwrap();
        let barrier = barrier.clone();
        jobs.spawn(async move {
            let content = format!(
                "writer {index}\n{}",
                format!("payload-{index}\n").repeat(1024)
            );
            barrier.wait().await;
            let result = tools
                .execute("write_file", &json!({"path":"race.txt", "content":content}))
                .await;
            (content, result)
        });
    }
    let mut winning_content = None;
    let mut rejected = 0;
    while let Some(result) = jobs.join_next().await {
        let (content, result) = result.unwrap();
        match result {
            Ok(_) => {
                assert!(
                    winning_content.replace(content).is_none(),
                    "more than one writer claimed the file"
                );
            }
            Err(error) => {
                assert!(format!("{error:#}").contains("apply_patch"));
                rejected += 1;
            }
        }
    }
    assert_eq!(rejected, 15);
    assert_eq!(
        fs::read_to_string(root.path().join("race.txt")).unwrap(),
        winning_content.unwrap()
    );
}

#[tokio::test]
async fn write_file_is_advertised_and_dispatched_through_the_authenticated_tool_bus() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("board.sqlite")).await.unwrap();
    let bus = ToolBus::new(store.clone(), WorkspaceTools::new(root.path(), 4).unwrap());
    let definitions = bus.definitions();
    let schema = definitions
        .iter()
        .find(|definition| definition["function"]["name"] == "write_file")
        .expect("providers must receive the native write_file schema");
    assert_eq!(
        schema["function"]["parameters"]["properties"]["path"]["type"],
        "string"
    );
    assert_eq!(
        schema["function"]["parameters"]["properties"]["content"]["type"],
        "string"
    );
    let required = schema["function"]["parameters"]["required"]
        .as_array()
        .unwrap();
    assert!(required.contains(&json!("path")));
    assert!(required.contains(&json!("content")));
    store.append("owner", "write a module", true).await.unwrap();
    bus.execute("agent-001", "board_read", &json!({}))
        .await
        .unwrap();
    store
        .append("agent-002", "peer status update", false)
        .await
        .unwrap();
    bus.execute(
        "agent-001",
        "write_file",
        &json!({"path":"module.rs", "content":"pub fn created() {}\n"}),
    )
    .await
    .unwrap();
    let read = bus
        .execute("agent-001", "read_file", &json!({"path":"module.rs"}))
        .await
        .unwrap();
    assert!(read["content"]
        .as_str()
        .unwrap()
        .contains("pub fn created() {}"));
    assert!(bus
        .execute(
            "owner",
            "write_file",
            &json!({"path":"impersonation.rs", "content":"x"})
        )
        .await
        .is_err());
    assert!(!root.path().join("impersonation.rs").exists());
}

#[tokio::test]
async fn invalid_arguments_and_oversized_content_do_not_create_files() {
    let root = tempfile::tempdir().unwrap();
    let tools = WorkspaceTools::new(root.path(), 4).unwrap();
    for arguments in [
        json!({"path":"invalid.txt"}),
        json!({"path":"invalid.txt", "content":42}),
        json!({"content":"x"}),
    ] {
        assert!(tools.execute("write_file", &arguments).await.is_err());
    }
    let content = "x".repeat(4 * 1024 * 1024 + 1);
    assert!(tools
        .execute(
            "write_file",
            &json!({"path":"oversize.txt", "content":content})
        )
        .await
        .is_err());
    assert!(!root.path().join("invalid.txt").exists());
    assert!(!root.path().join("oversize.txt").exists());
}

#[tokio::test]
async fn write_file_rejects_relative_and_absolute_workspace_escapes() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let tools = WorkspaceTools::new(root.path(), 4).unwrap();
    let escaped = root.path().parent().unwrap().join(format!(
        "{}.escape",
        root.path().file_name().unwrap().to_string_lossy()
    ));
    let relative = format!("../{}", escaped.file_name().unwrap().to_string_lossy());
    assert!(tools
        .execute("write_file", &json!({"path":relative, "content":"escape"}))
        .await
        .is_err());
    assert!(!escaped.exists());
    let absolute = outside.path().join("escape.txt");
    assert!(tools
        .execute("write_file", &json!({"path":absolute, "content":"escape"}))
        .await
        .is_err());
    assert!(!absolute.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_parents_cannot_escape_and_existing_symlink_targets_are_preserved() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), root.path().join("outside")).unwrap();
    fs::write(root.path().join("active.txt"), "keep this content").unwrap();
    symlink(
        root.path().join("active.txt"),
        root.path().join("alias.txt"),
    )
    .unwrap();
    let tools = WorkspaceTools::new(root.path(), 4).unwrap();
    assert!(tools
        .execute(
            "write_file",
            &json!({"path":"outside/escape.txt", "content":"escape"})
        )
        .await
        .is_err());
    assert!(!outside.path().join("escape.txt").exists());
    assert!(tools
        .execute(
            "write_file",
            &json!({"path":"alias.txt", "content":"overwrite"})
        )
        .await
        .is_err());
    assert_eq!(
        fs::read_to_string(root.path().join("active.txt")).unwrap(),
        "keep this content"
    );
}
