//! Working-tree snapshots use an alternate Git index; HEAD and the user's index
//! are never reset, staged, or committed by the harness.
use anyhow::{ensure, Context, Result};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::{io::AsyncWriteExt, process::Command};
static NEXT: AtomicU64 = AtomicU64::new(0);

struct SnapshotIndex(PathBuf);

impl Drop for SnapshotIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let mut lock = self.0.as_os_str().to_owned();
        lock.push(".lock");
        let _ = std::fs::remove_file(PathBuf::from(lock));
    }
}

async fn git(root: &Path, index: Option<&Path>, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.current_dir(root).args(args).kill_on_drop(true);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
        command.env(
            "GIT_DIR",
            index.parent().context("snapshot repository missing")?,
        );
        command.env("GIT_WORK_TREE", root);
    }
    let result = command
        .output()
        .await
        .context("starting Git snapshot operation")?;
    ensure!(result.status.success(), "Git snapshot operation failed");
    Ok(result.stdout)
}

async fn repository(root: &Path) -> Result<PathBuf> {
    let directory =
        String::from_utf8(git(root, None, &["rev-parse", "--absolute-git-dir"]).await?)?;
    let directory = PathBuf::from(directory.trim()).join("openraid-snapshots");
    tokio::fs::create_dir_all(&directory).await?;
    if !directory.join("HEAD").is_file() {
        let result = Command::new("git")
            .kill_on_drop(true)
            .args(["init", "--bare", "--quiet"])
            .arg(&directory)
            .output()
            .await?;
        ensure!(
            result.status.success(),
            "could not initialize the private snapshot store"
        );
    }
    Ok(directory)
}

pub async fn capture(root: &Path, database: &Path) -> Result<String> {
    // Environment paths must not be interpreted relative to current_dir(root).
    // Canonical parent paths also align Windows verbatim/normal drive prefixes
    // and symlinked workspace aliases before excluding the live database.
    let root = tokio::fs::canonicalize(root).await?;
    let database = std::path::absolute(database)?;
    let database = match (database.parent(), database.file_name()) {
        (Some(parent), Some(name)) => match tokio::fs::canonicalize(parent).await {
            Ok(parent) => parent.join(name),
            Err(_) => database,
        },
        _ => database,
    };
    let root = root.as_path();
    let directory = repository(root).await?;
    let index = directory.join(format!(
        "{}-{}.index",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _index = SnapshotIndex(index.clone());
    let result = async {
        git(root, Some(&index), &["read-tree", "--empty"]).await?;
        let mut exclusions = vec![
            "add".to_owned(),
            "-A".into(),
            "--".into(),
            ".".into(),
            ":(exclude).openraid".into(),
        ];
        if let Ok(relative) = database.strip_prefix(root) {
            let path = relative.to_string_lossy().replace('\\', "/");
            for suffix in ["", "-wal", "-shm"] {
                exclusions.push(format!(":(exclude,literal){path}{suffix}"));
            }
        }
        git(
            root,
            Some(&index),
            &exclusions.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .await?;
        let tree = String::from_utf8(git(root, Some(&index), &["write-tree"]).await?)?;
        let tree = tree.trim().to_owned();
        git(
            root,
            Some(&index),
            &["update-ref", &format!("refs/snapshots/{tree}"), &tree],
        )
        .await?;
        Ok::<_, anyhow::Error>(tree)
    }
    .await;
    let _ = tokio::fs::remove_file(&index).await;
    result
}

pub async fn restore(root: &Path, database: &Path, tree: &str) -> Result<()> {
    ensure!(
        tree.len() >= 40 && tree.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid snapshot identifier"
    );
    let root = tokio::fs::canonicalize(root).await?;
    let root = root.as_path();
    let current = capture(root, database).await?;
    let directory = repository(root).await?;
    let index = directory.join("read-only.index");
    let patch = git(
        root,
        Some(&index),
        &["diff", "--binary", "--no-ext-diff", &current, tree, "--"],
    )
    .await?;
    if patch.is_empty() {
        return Ok(());
    }
    for check in [true, false] {
        let mut command = Command::new("git");
        command.current_dir(root).arg("apply");
        // Snapshot paths are relative to the workspace, which may be nested
        // inside the user's repository. Use the same work tree as capture so
        // Git does not silently skip paths outside the user's repository prefix.
        command.env("GIT_DIR", &directory);
        command.env("GIT_WORK_TREE", root);
        command.env("GIT_INDEX_FILE", &index);
        if check {
            command.arg("--check");
        }
        command
            .arg("--whitespace=nowarn")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;
        let mut input = child.stdin.take().context("snapshot input missing")?;
        input.write_all(&patch).await?;
        drop(input);
        ensure!(
            child.wait().await?.success(),
            "workspace changed or snapshot cannot be restored cleanly"
        );
    }
    Ok(())
}
