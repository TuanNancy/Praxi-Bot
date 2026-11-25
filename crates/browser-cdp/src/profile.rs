//! The managed browser's profile: make Chrome believe its last run ended cleanly.
//!
//! The daemon owns the profile directory and often stops the browser abruptly
//! (a killed process group, a restarted host). Chrome then writes
//! `exit_type: "Crashed"` and shows the "Restore pages? Chrome didn't shut down
//! correctly" bubble over the page on the next start. The bot's browser is not a
//! person's browser, so the profile is rewritten as cleanly closed before every
//! launch and again after every stop.

use std::path::Path;

use serde_json::{Value, json};

/// Rewrite `<profile>/Default/Preferences` so `profile.exit_type` is `Normal`
/// and `profile.exited_cleanly` is `true`. Returns `true` when the file was
/// changed. Nothing is created: a profile that never ran has no file to fix.
/// Only call it while no browser is using the profile.
pub fn mark_clean_exit(profile: &Path) -> std::io::Result<bool> {
    let file = profile.join("Default").join("Preferences");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let Ok(mut prefs) = serde_json::from_str::<Value>(&text) else {
        // A damaged file is Chrome's to repair; leave it as it is.
        return Ok(false);
    };
    if !with_clean_exit(&mut prefs) {
        return Ok(false);
    }
    let tmp = file.with_extension("praxi-tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec(&prefs).map_err(std::io::Error::other)?,
    )?;
    if let Err(e) = std::fs::rename(&tmp, &file) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(true)
}

/// Set the two clean-exit fields in a parsed `Preferences` document; `false`
/// when it already said so (or is not an object).
fn with_clean_exit(prefs: &mut Value) -> bool {
    let Some(root) = prefs.as_object_mut() else {
        return false;
    };
    let section = root.entry("profile").or_insert_with(|| json!({}));
    let Some(section) = section.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    if section.get("exit_type").and_then(Value::as_str) != Some("Normal") {
        section.insert("exit_type".into(), json!("Normal"));
        changed = true;
    }
    if section.get("exited_cleanly").and_then(Value::as_bool) != Some(true) {
        section.insert("exited_cleanly".into(), json!(true));
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_profile(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "praxi-profile-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(dir.join("Default")).unwrap();
        dir
    }

    #[test]
    fn a_crashed_profile_is_rewritten_as_cleanly_closed_and_keeps_its_other_settings() {
        let dir = temp_profile("crashed");
        let file = dir.join("Default").join("Preferences");
        std::fs::write(
            &file,
            r#"{"profile":{"exit_type":"Crashed","exited_cleanly":false,"name":"Praxi"},"homepage":"about:blank"}"#,
        )
        .unwrap();
        assert!(mark_clean_exit(&dir).unwrap());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["profile"]["exit_type"], "Normal");
        assert_eq!(v["profile"]["exited_cleanly"], true);
        assert_eq!(v["profile"]["name"], "Praxi");
        assert_eq!(v["homepage"], "about:blank");
        // Already clean: nothing is written again.
        assert!(!mark_clean_exit(&dir).unwrap());
        assert!(!dir.join("Default").join("Preferences.praxi-tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_or_damaged_preferences_are_left_alone() {
        let dir = temp_profile("odd");
        assert!(!mark_clean_exit(&dir).unwrap());
        assert!(!dir.join("Default").join("Preferences").exists());
        let file = dir.join("Default").join("Preferences");
        std::fs::write(&file, "{not json").unwrap();
        assert!(!mark_clean_exit(&dir).unwrap());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{not json");
        // A profile section that is missing is added.
        std::fs::write(&file, r#"{"a":1}"#).unwrap();
        assert!(mark_clean_exit(&dir).unwrap());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["profile"]["exit_type"], "Normal");
        let _ = std::fs::remove_dir_all(dir);
    }
}
