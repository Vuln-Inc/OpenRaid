use anyhow::Result;
use openraid::snapshots;
use std::{path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn restore_reverts_workspace_changes_without_touching_user_index_or_database() -> Result<()> {
    let root = tempfile::tempdir()?;
    git(root.path(), &["init", "--quiet"]);
    let file = root.path().join("file.txt");
    std::fs::write(&file, "staged user content")?;
    git(root.path(), &["add", "file.txt"]);
    let original_index = git(root.path(), &["write-tree"]);
    std::fs::write(&file, "user content before the prompt")?;
    let database = root.path().join("session.sqlite3");
    std::fs::write(&database, "before")?;
    let activity = openraid::metrics::activity_directory(&database);
    std::fs::create_dir(&activity)?;
    let transcript = activity.join("agent-001.log");
    std::fs::write(&transcript, "before prompt")?;
    let tree = snapshots::capture(root.path(), &database).await?;
    git(root.path(), &["gc", "--prune=now"]);
    std::fs::write(&file, "agent change")?;
    std::fs::write(root.path().join("added.txt"), "agent created this")?;
    std::fs::write(&database, "new database state")?;
    std::fs::write(&transcript, "complete activity after prompt")?;
    std::fs::write(activity.join("agent-002.log"), "new agent history")?;
    snapshots::restore(root.path(), &database, &tree).await?;
    assert_eq!(
        std::fs::read_to_string(&file)?,
        "user content before the prompt"
    );
    assert!(!root.path().join("added.txt").exists());
    assert_eq!(std::fs::read_to_string(&database)?, "new database state");
    assert_eq!(
        std::fs::read_to_string(&transcript)?,
        "complete activity after prompt"
    );
    assert_eq!(
        std::fs::read_to_string(activity.join("agent-002.log"))?,
        "new agent history"
    );
    assert_eq!(git(root.path(), &["write-tree"]), original_index);
    Ok(())
}

#[tokio::test]
async fn restore_supports_a_workspace_nested_inside_a_repository() -> Result<()> {
    let repository = tempfile::tempdir()?;
    git(repository.path(), &["init", "--quiet"]);
    let root = repository.path().join("nested workspace");
    std::fs::create_dir(&root)?;
    let file = root.join("file.txt");
    std::fs::write(&file, "before prompt")?;
    let database = root.join("session.sqlite3");
    let tree = snapshots::capture(&root, &database).await?;
    std::fs::write(&file, "agent edit")?;
    std::fs::write(root.join("added.txt"), "new file")?;
    snapshots::restore(&root, &database, &tree).await?;
    assert_eq!(std::fs::read_to_string(&file)?, "before prompt");
    assert!(!root.join("added.txt").exists());
    Ok(())
}

#[tokio::test]
async fn relative_workspace_paths_restore_files_and_preserve_database_sidecars() -> Result<()> {
    let root = tempfile::tempdir_in(".")?;
    let workspace = root.path().strip_prefix(std::env::current_dir()?)?;
    assert!(workspace.is_relative());
    git(root.path(), &["init", "--quiet"]);
    let file = root.path().join("file.txt");
    std::fs::write(&file, "before prompt")?;
    let database = workspace.join("session.sqlite3");
    for suffix in ["", "-wal", "-shm"] {
        std::fs::write(
            root.path().join(format!("session.sqlite3{suffix}")),
            "before",
        )?;
    }
    let tree = snapshots::capture(workspace, &database).await?;
    std::fs::write(&file, "agent edit")?;
    for suffix in ["", "-wal", "-shm"] {
        std::fs::write(
            root.path().join(format!("session.sqlite3{suffix}")),
            "after",
        )?;
    }
    snapshots::restore(workspace, &database, &tree).await?;
    assert_eq!(std::fs::read_to_string(&file)?, "before prompt");
    for suffix in ["", "-wal", "-shm"] {
        assert_eq!(
            std::fs::read_to_string(root.path().join(format!("session.sqlite3{suffix}")))?,
            "after"
        );
    }
    Ok(())
}

#[tokio::test]
async fn canonical_workspace_preserves_an_explicit_absolute_database() -> Result<()> {
    let root = tempfile::tempdir()?;
    git(root.path(), &["init", "--quiet"]);
    let workspace = std::fs::canonicalize(root.path())?;
    let database = root.path().join("session.sqlite3");
    std::fs::write(&database, "before")?;
    std::fs::write(root.path().join("file.txt"), "before prompt")?;
    let tree = snapshots::capture(&workspace, &database).await?;
    std::fs::write(&database, "after")?;
    std::fs::write(root.path().join("file.txt"), "agent edit")?;
    snapshots::restore(&workspace, &database, &tree).await?;
    assert_eq!(std::fs::read_to_string(&database)?, "after");
    assert_eq!(
        std::fs::read_to_string(root.path().join("file.txt"))?,
        "before prompt"
    );
    Ok(())
}
