//! Workspace trust. A project-local `.rocinante/config.toml` can define MCP
//! servers (spawned at startup), redirect providers (exfiltrating the
//! conversation and reusing your API key), grant tools via subagent profiles,
//! or auto-approve tool calls. So security-relevant keys from an *untrusted*
//! project config are dropped at load time; the rest (models, skills, etc.)
//! still applies. The user opts a project in with `/trust`, remembered here.
//!
//! The user-wide `~/.rocinante/config.toml` is always fully trusted — it's the
//! user's own file, not something that ships with a cloned repo.

use std::path::{Path, PathBuf};

fn store_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rocinante/trust.toml"))
}

/// Canonical string key for a project directory (falls back to the given path
/// when it can't be canonicalized, e.g. it doesn't exist yet).
fn key(project_dir: &Path) -> String {
    project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf())
        .display()
        .to_string()
}

fn read_trusted() -> Vec<String> {
    let Some(path) = store_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.parse::<toml::Value>()
        .ok()
        .and_then(|v| v.get("trusted").cloned())
        .and_then(|v| v.as_array().cloned())
        .map(|arr| {
            arr.into_iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Has the user opted this project into applying its full config?
pub fn is_trusted(project_dir: &Path) -> bool {
    read_trusted().contains(&key(project_dir))
}

/// Remember this project as trusted. Idempotent. Returns `true` if it was
/// newly added (was not already trusted).
pub fn trust(project_dir: &Path) -> std::io::Result<bool> {
    let k = key(project_dir);
    let mut trusted = read_trusted();
    if trusted.contains(&k) {
        return Ok(false);
    }
    trusted.push(k);
    let Some(path) = store_path() else {
        return Ok(false);
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let doc = toml::Value::try_from(std::collections::BTreeMap::from([(
        "trusted".to_string(),
        trusted,
    )]))
    .expect("trusted list serializes");
    // Atomic write: tmp then rename.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string(&doc).expect("trust store serializes"))?;
    std::fs::rename(&tmp, &path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_for_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(key(dir.path()), key(dir.path()));
    }
}
