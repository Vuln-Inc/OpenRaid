//! Discoverable session metadata. Credentials and conversations remain in their
//! existing stores; the catalog only records workspace and database locations.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: String,
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub title: String,
    pub updated_ms: u64,
}

fn root() -> Result<PathBuf> {
    let auth = crate::auth::default_auth_path()?;
    Ok(auth.parent().unwrap_or(Path::new(".")).join("sessions"))
}

pub fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("session-{nanos:x}-{:x}", std::process::id())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

pub fn list(workspace: Option<&Path>) -> Result<Vec<SessionEntry>> {
    let root = root()?;
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for path in fs::read_dir(&root).context("reading session catalog")? {
        let path = path?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let entry: SessionEntry = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("reading session metadata {}", path.display()))?;
        if entry.database.is_file()
            && workspace.is_none_or(|workspace| workspace == entry.workspace)
        {
            entries.push(entry);
        }
    }
    entries.sort_by(|a, b| {
        b.updated_ms
            .cmp(&a.updated_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(entries)
}

pub fn find(id: &str) -> Result<SessionEntry> {
    ensure!(valid_id(id), "invalid session ID");
    let path = root()?.join(format!("{id}.json"));
    let entry: SessionEntry = serde_json::from_slice(
        &fs::read(&path)
            .with_context(|| format!("unknown session {id}; use openraid sessions --all"))?,
    )?;
    ensure!(
        entry.database.is_file(),
        "session database is missing: {}",
        entry.database.display()
    );
    Ok(entry)
}

pub fn register(workspace: &Path, database: &Path, title: &str) -> Result<SessionEntry> {
    let workspace_path = fs::canonicalize(workspace).context("opening session workspace")?;
    let database_path = fs::canonicalize(database).context("opening session database")?;
    let workspace = workspace_path.as_path();
    let database = database_path.as_path();
    let mut entry = list(None)?
        .into_iter()
        .find(|entry| entry.database == database)
        .unwrap_or_else(|| SessionEntry {
            id: new_id(),
            workspace: workspace.to_owned(),
            database: database.to_owned(),
            title: "New session".into(),
            updated_ms: 0,
        });
    ensure!(
        entry.workspace == workspace,
        "this session belongs to {}; open it with --session {}",
        entry.workspace.display(),
        entry.id
    );
    if !title.trim().is_empty() {
        entry.title = title
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(120)
            .collect();
    }
    entry.updated_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let root = root()?;
    fs::create_dir_all(&root).context("creating session catalog")?;
    let destination = root.join(format!("{}.json", entry.id));
    let temporary = root.join(format!("{}.{}.tmp", entry.id, new_id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(&entry)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.context("saving session metadata")?;
    Ok(entry)
}
