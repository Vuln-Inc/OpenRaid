use anyhow::Result;
use openraid::{
    theme::{DEFAULT_THEME_ID, THEMES},
    theme_preferences::{load_from, save_to},
};
use std::fs;

#[test]
fn missing_preferences_use_default_without_creating_files() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("nested/theme.json");

    assert_eq!(load_from(&path)?, DEFAULT_THEME_ID);
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
    Ok(())
}

#[test]
fn malformed_or_unrecognized_preferences_fall_back_without_rewriting() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("theme.json");
    for contents in [
        "",
        "{broken json",
        "null",
        "[]",
        "{}",
        r#"{"theme": 42}"#,
        r#"{"theme": "a-theme-from-a-future-version"}"#,
    ] {
        fs::write(&path, contents)?;
        assert_eq!(load_from(&path)?, DEFAULT_THEME_ID, "{contents}");
        assert_eq!(fs::read_to_string(&path)?, contents);
    }
    Ok(())
}

#[test]
fn every_builtin_round_trips_and_replaces_the_previous_preference() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("new config/openraid/theme.json");

    for theme in THEMES.iter() {
        save_to(&path, theme.id)?;
        assert_eq!(load_from(&path)?, theme.id);
        assert_eq!(fs::read_dir(path.parent().unwrap())?.count(), 1);
    }
    // Returning to the default is an explicit saved choice, too.
    save_to(&path, DEFAULT_THEME_ID)?;
    assert_eq!(load_from(&path)?, DEFAULT_THEME_ID);
    Ok(())
}

#[test]
fn display_name_aliases_are_saved_as_canonical_ids() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("theme.json");

    for theme in THEMES.iter() {
        save_to(&path, &theme.name.to_uppercase())?;
        assert_eq!(load_from(&path)?, theme.id);
        let document: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(document["theme"], theme.id);
    }
    save_to(&path, "default")?;
    assert_eq!(load_from(&path)?, DEFAULT_THEME_ID);
    Ok(())
}

#[test]
fn hand_edited_display_names_load_canonically_without_rewriting() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("theme.json");

    for theme in THEMES.iter() {
        let contents = serde_json::to_string(&serde_json::json!({
            "theme": format!("  {}  ", theme.name.to_uppercase()),
            "future_setting": true,
        }))?;
        fs::write(&path, &contents)?;
        assert_eq!(load_from(&path)?, theme.id);
        assert_eq!(fs::read_to_string(&path)?, contents);
    }
    Ok(())
}

#[test]
fn rejecting_unknown_ids_preserves_existing_preferences_and_creates_nothing() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("theme.json");
    let theme = THEMES
        .iter()
        .find(|theme| theme.id != DEFAULT_THEME_ID)
        .unwrap();
    save_to(&path, theme.id)?;
    let saved = fs::read(&path)?;

    for id in ["", "not-a-theme", "../../other-file"] {
        assert!(save_to(&path, id).is_err());
        assert_eq!(fs::read(&path)?, saved);
        assert_eq!(load_from(&path)?, theme.id);
    }
    assert_eq!(fs::read_dir(directory.path())?.count(), 1);

    let absent_path = directory.path().join("absent/theme.json");
    assert!(save_to(&absent_path, "not-a-theme").is_err());
    assert!(!absent_path.parent().unwrap().exists());
    Ok(())
}

#[test]
fn filesystem_errors_are_reported_and_failed_saves_leave_no_temporary_files() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("theme.json");
    fs::create_dir(&path)?;

    assert!(
        load_from(&path).is_err(),
        "a directory cannot be read as preferences"
    );
    assert!(save_to(&path, DEFAULT_THEME_ID).is_err());
    assert!(path.is_dir());
    assert_eq!(fs::read_dir(directory.path())?.count(), 1);
    assert_eq!(fs::read_dir(&path)?.count(), 0);

    let blocked_parent = directory.path().join("not-a-directory");
    fs::write(&blocked_parent, "user content")?;
    assert!(save_to(&blocked_parent.join("theme.json"), DEFAULT_THEME_ID).is_err());
    assert_eq!(fs::read_to_string(&blocked_parent)?, "user content");
    Ok(())
}
