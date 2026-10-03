//! A global appearance preference, separate from provider credentials and sessions.

use crate::theme::{find_theme, set_theme, DEFAULT_THEME_ID};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
struct Preference {
    theme: String,
}

/// Resolve the preference file without creating any directories.
pub fn default_path() -> Result<PathBuf> {
    if let Some(path) = nonempty_env("OPENRAID_THEME_FILE") {
        return Ok(path.into());
    }
    if let Some(path) = nonempty_env("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("openraid/theme.json"));
    }
    #[cfg(windows)]
    if let Some(path) = nonempty_env("APPDATA") {
        return Ok(PathBuf::from(path).join("openraid/theme.json"));
    }
    let home = nonempty_env("HOME")
        .or_else(|| nonempty_env("USERPROFILE"))
        .context("set OPENRAID_THEME_FILE or a home directory to save the theme preference")?;
    Ok(PathBuf::from(home).join(".config/openraid/theme.json"))
}

fn nonempty_env(name: &str) -> Option<std::ffi::OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

/// Missing, malformed, or retired preferences use the built-in default.
/// Other I/O errors remain visible to the caller, and loading never writes.
pub fn load_from(path: &Path) -> Result<String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DEFAULT_THEME_ID.to_owned());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading theme preference {}", path.display()));
        }
    };
    Ok(serde_json::from_slice::<Preference>(&bytes)
        .ok()
        .and_then(|preference| find_theme(&preference.theme))
        .map(|theme| theme.id)
        .unwrap_or(DEFAULT_THEME_ID)
        .to_owned())
}

/// Initialize before opening setup or the console. A failed read still leaves
/// a usable default palette active.
pub fn initialize() -> Result<()> {
    set_theme(DEFAULT_THEME_ID);
    let id = load_from(&default_path()?)?;
    set_theme(&id);
    Ok(())
}

/// Persist a confirmed selection. Previewing a palette should not call this.
pub fn save(id: &str) -> Result<()> {
    save_to(&default_path()?, id)
}

/// Atomically replace a preference after writing and flushing its complete JSON.
/// Validation happens first, so an unknown theme cannot overwrite a saved choice.
pub fn save_to(path: &Path, id: &str) -> Result<()> {
    let theme = find_theme(id).with_context(|| format!("unknown theme: {id}"))?;
    let payload = serde_json::to_vec_pretty(&Preference {
        theme: theme.id.to_owned(),
    })
    .context("serializing theme preference")?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .context("theme preference file must have a filename")?
        .to_string_lossy();
    fs::create_dir_all(parent)
        .with_context(|| format!("creating theme preference directory {}", parent.display()))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temporary = parent.join(format!(".{name}.{}.{nonce}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .context("creating temporary theme preference")?;
        file.write_all(&payload)
            .context("writing theme preference")?;
        file.write_all(b"\n")?;
        file.sync_all().context("flushing theme preference")?;
        drop(file);
        fs::rename(&temporary, path)
            .with_context(|| format!("replacing theme preference {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
