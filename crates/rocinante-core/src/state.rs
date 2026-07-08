//! Global runtime state: `~/.rocinante/state.toml`. Currently just the last
//! model chosen, so the next launch starts on it without re-prompting. Never
//! stores secrets — only a model name (alias/`provider/model`/tag).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "state.toml";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct State {
    /// Model name (alias, `provider/model`, or bare tag) last selected — the
    /// default for the next launch. `None` until the user picks one.
    #[serde(default)]
    pub last_model: Option<String>,
}

/// `~/.rocinante/state.toml`, or `None` if the home directory is unknown.
fn path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rocinante").join(FILE_NAME))
}

/// Load global state. A missing or corrupt file yields the default
/// (`last_model: None`); this never errors, so a bad file can't block startup.
pub fn load() -> State {
    match path() {
        Some(p) => load_from(&p),
        None => State::default(),
    }
}

/// Load from a specific path (test seam; also used by [`load`]).
pub fn load_from(path: &Path) -> State {
    use figment::{
        Figment,
        providers::{Format, Toml},
    };
    // A missing file parses as empty (→ default); a corrupt file errors → default.
    Figment::new()
        .merge(Toml::file(path))
        .extract()
        .unwrap_or_default()
}

/// Persist the last-selected model. Best-effort: creates `~/.rocinante` if
/// needed, writes atomically (tmp + rename), logs on failure, never panics.
pub fn save_last_model(name: &str) {
    let Some(path) = path() else {
        tracing::warn!("cannot persist last model: home directory unknown");
        return;
    };
    if let Err(e) = save_to(&path, name) {
        tracing::warn!(error = %e, "failed to persist last model to state.toml");
    }
}

/// Save to a specific path (test seam; also used by [`save_last_model`]).
pub fn save_to(path: &Path, name: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = format!("last_model = {}\n", toml_string(name));
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

/// Minimal TOML basic-string encoding (quote + escape) for one value. Model
/// names never contain control characters in practice, but escape anyway so a
/// stray character can't corrupt the file.
fn toml_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.toml");
        assert!(load_from(&p).last_model.is_none());
    }

    #[test]
    fn corrupt_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.toml");
        std::fs::write(&p, "this = = not valid toml").unwrap();
        assert!(load_from(&p).last_model.is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        // Also exercises parent-directory creation.
        let p = dir.path().join("nested").join("state.toml");
        save_to(&p, "glm-5.2:cloud").unwrap();
        assert_eq!(load_from(&p).last_model.as_deref(), Some("glm-5.2:cloud"));
        assert!(!p.with_extension("toml.tmp").exists());
    }

    #[test]
    fn save_overwrites_and_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.toml");
        save_to(&p, "anthropic/claude-opus-4-8").unwrap();
        save_to(&p, "provider/\"weird\"").unwrap();
        assert_eq!(
            load_from(&p).last_model.as_deref(),
            Some("provider/\"weird\"")
        );
    }
}
