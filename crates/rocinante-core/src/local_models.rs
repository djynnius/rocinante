//! GGUF models on disk for the embedded llama.cpp (`/localmodel`): list the
//! model folder, turn a name or path into a file, and remember the folder
//! and aliases in `~/.rocinante/config.toml`.
//!
//! A local model is addressed as `local/<name>`: `<name>` is a file stem
//! from the model folder (`local/Qwen3.5-4B-Q4_0`), a path relative to it,
//! or any absolute path (`local//models/x.gguf`). `[models]` aliases with
//! `provider = "local"` work like every other alias.

use std::path::{Path, PathBuf};

use crate::config::{Config, ProviderConfig};

/// The built-in llama.cpp provider's name.
pub const PROVIDER: &str = "local";

/// How deep `/localmodel` looks below the model folder (LM Studio keeps
/// `publisher/model/file.gguf`).
const MAX_DEPTH: usize = 4;

/// One model file found in the folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModel {
    pub path: PathBuf,
    /// File stem: what `local/<name>` and the picker use.
    pub name: String,
    pub size: u64,
    /// `[models]` aliases pointing at this file.
    pub aliases: Vec<String>,
}

impl LocalModel {
    /// The `/model` argument that selects this file.
    pub fn switch_name(&self) -> String {
        self.aliases
            .first()
            .cloned()
            .unwrap_or_else(|| format!("{PROVIDER}/{}", self.name))
    }

    /// Picker label: `name · 2.4 GB · alias qwen`.
    pub fn label(&self) -> String {
        let mut label = format!("{} · {}", self.name, human_size(self.size));
        if !self.aliases.is_empty() {
            label.push_str(&format!(" · alias {}", self.aliases.join(", ")));
        }
        label
    }
}

/// `~` and `~/…` expanded against the home directory.
pub fn expand_tilde(s: &str) -> PathBuf {
    let s = s.trim();
    if s == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(s));
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(s)
}

/// The configured model folder, `~` expanded.
pub fn model_dir(config: &Config) -> Option<PathBuf> {
    config
        .defaults
        .local_model_dir
        .as_deref()
        .filter(|d| !d.trim().is_empty())
        .map(expand_tilde)
}

/// Names of providers that run on the embedded llama.cpp.
pub fn llama_providers(config: &Config) -> Vec<&str> {
    config
        .providers
        .iter()
        .filter(|(_, p)| matches!(p, ProviderConfig::Llamacpp { .. }))
        .map(|(n, _)| n.as_str())
        .collect()
}

/// Whether `arg` names a file rather than a model in the folder: absolute,
/// `~`, `./`, `../`, or anything with a path separator or `.gguf` ending.
pub fn is_path_like(arg: &str) -> bool {
    let a = arg.trim();
    a.starts_with('/')
        || a.starts_with('~')
        || a.starts_with("./")
        || a.starts_with("../")
        || Path::new(a).is_absolute()
        || a.contains(std::path::MAIN_SEPARATOR)
        || a.contains('/')
        || a.to_ascii_lowercase().ends_with(".gguf")
}

/// Every loadable GGUF under `dir`, sorted by name. Skips hidden folders,
/// multimodal projectors (`mmproj-*`), vocab-only files (`ggml-vocab-*`) and
/// the non-first parts of split models.
pub fn list(dir: &Path, config: &Config) -> Vec<LocalModel> {
    let mut found = Vec::new();
    walk(dir, 0, &mut found);
    let aliases = aliases(config);
    let mut models: Vec<LocalModel> = found
        .into_iter()
        .map(|(path, size)| {
            let name = stem(&path);
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            let aliases = aliases
                .iter()
                .filter(|(_, p)| *p == canonical)
                .map(|(a, _)| a.clone())
                .collect();
            LocalModel {
                path,
                name,
                size,
                aliases,
            }
        })
        .collect();
    models.sort_by_key(|a| a.name.to_lowercase());
    models
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                walk(&path, depth + 1, out);
            }
        } else if is_model_file(&name) {
            out.push((path, meta.len()));
        }
    }
}

fn is_model_file(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    if !lower.ends_with(".gguf") || lower.starts_with("mmproj") || lower.starts_with("ggml-vocab") {
        return false;
    }
    // `model-00002-of-00003.gguf`: llama.cpp loads the rest from part 1.
    match split_part(&lower) {
        Some(part) => part == 1,
        None => true,
    }
}

/// The part number of a split GGUF (`-00002-of-00003.gguf` → 2).
fn split_part(lower: &str) -> Option<u32> {
    let stem = lower.strip_suffix(".gguf")?;
    let (head, total) = stem.rsplit_once("-of-")?;
    let (_, part) = head.rsplit_once('-')?;
    if total.len() == 5 && part.len() == 5 {
        part.parse().ok()
    } else {
        None
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The display name of a model file: its stem.
pub fn display_name(path: &Path) -> String {
    stem(path)
}

/// `[models]` aliases on a llama.cpp provider, with their files.
pub fn aliases(config: &Config) -> Vec<(String, PathBuf)> {
    let providers = llama_providers(config);
    let dir = model_dir(config);
    config
        .models
        .iter()
        .filter(|(_, m)| providers.contains(&m.provider.as_str()))
        .filter_map(|(alias, m)| {
            let path = resolve(&m.model, dir.as_deref()).ok()?;
            Some((alias.clone(), path.canonicalize().unwrap_or(path)))
        })
        .collect()
}

/// The file a local model name or path refers to.
pub fn resolve(name: &str, dir: Option<&Path>) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("no model given".into());
    }
    let direct = expand_tilde(name);
    if direct.is_absolute()
        || name.starts_with('~')
        || name.starts_with("./")
        || name.starts_with("../")
    {
        return if direct.is_file() {
            Ok(direct)
        } else {
            Err(format!("no model file at {}", direct.display()))
        };
    }
    let Some(dir) = dir else {
        return if direct.is_file() {
            Ok(direct)
        } else {
            Err(format!(
                "`{name}` is not a file and no model folder is set — run /localmodel to choose one"
            ))
        };
    };
    for candidate in [dir.join(name), dir.join(format!("{name}.gguf"))] {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    let wanted = name.trim_end_matches(".gguf").to_lowercase();
    let mut found = Vec::new();
    walk(dir, 0, &mut found);
    let matches: Vec<PathBuf> = found
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| stem(p).to_lowercase() == wanted)
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "no model `{name}` in {} — /localmodel lists what is there",
            dir.display()
        )),
        many => Err(format!(
            "`{name}` matches {} files in {} — use a path: {}",
            many.len(),
            dir.display(),
            many.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// `2.4 GB` / `730 MB`.
pub fn human_size(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", (b / MB).max(1.0))
    }
}

// ------------------------------------------------------------ config edits

/// The user config file (`~/.rocinante/config.toml`).
pub fn user_config_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rocinante").join("config.toml"))
}

/// Set `[defaults] local_model_dir` in the user config, keeping the rest of
/// the file (comments included) as it is.
pub fn save_model_dir(dir: &Path) -> Result<PathBuf, String> {
    let path = user_config_path().ok_or("no home directory")?;
    edit_config(&path, |doc| {
        let defaults = table(doc, "defaults");
        defaults["local_model_dir"] = toml_edit::value(dir.display().to_string());
    })?;
    Ok(path)
}

/// Add or replace `[models.<alias>]` pointing at a local model file.
pub fn save_alias(alias: &str, file: &Path) -> Result<PathBuf, String> {
    if alias.is_empty()
        || !alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "`{alias}` is not a usable alias (letters, digits, - _ . only)"
        ));
    }
    let path = user_config_path().ok_or("no home directory")?;
    edit_config(&path, |doc| {
        let models = table(doc, "models");
        models.set_implicit(true);
        let mut entry = toml_edit::Table::new();
        entry["provider"] = toml_edit::value(PROVIDER);
        entry["model"] = toml_edit::value(file.display().to_string());
        models.insert(alias, toml_edit::Item::Table(entry));
    })?;
    Ok(path)
}

fn table<'a>(doc: &'a mut toml_edit::DocumentMut, key: &str) -> &'a mut toml_edit::Table {
    if !doc.contains_table(key) {
        doc.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    doc[key].as_table_mut().expect("just ensured a table")
}

fn edit_config(path: &Path, f: impl FnOnce(&mut toml_edit::DocumentMut)) -> Result<(), String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    f(&mut doc);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write {}: {e}", path.display())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    fn config_with(dir: &Path, extra: &str) -> Config {
        let tmp = tempdir();
        let user = tmp.join("user.toml");
        std::fs::write(
            &user,
            format!(
                "[defaults]\nlocal_model_dir = \"{}\"\n{extra}",
                dir.display()
            ),
        )
        .unwrap();
        crate::config::load_from(&user, Path::new("/nonexistent/p.toml")).unwrap()
    }

    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("rcn-local-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lists_models_and_skips_projectors_vocab_split_parts_and_hidden() {
        let dir = tempdir();
        touch(&dir.join("Qwen3.5-4B-Q4_0.gguf"), 10);
        touch(&dir.join("lmstudio/qwen/Coder-7B.Q4_K_M.gguf"), 20);
        touch(&dir.join("mmproj-Coder-7B.gguf"), 1);
        touch(&dir.join("llama.cpp/models/ggml-vocab-gemma-4.gguf"), 1);
        touch(&dir.join("big-00001-of-00002.gguf"), 5);
        touch(&dir.join("big-00002-of-00002.gguf"), 5);
        touch(&dir.join(".cache/hidden.gguf"), 1);
        touch(&dir.join("notes.txt"), 1);
        let config = config_with(&dir, "");
        let names: Vec<String> = list(&dir, &config).into_iter().map(|m| m.name).collect();
        assert_eq!(
            names,
            vec!["big-00001-of-00002", "Coder-7B.Q4_K_M", "Qwen3.5-4B-Q4_0"]
        );
    }

    #[test]
    fn resolves_stems_relative_paths_and_absolute_paths() {
        let dir = tempdir();
        let qwen = dir.join("Qwen3.5-4B-Q4_0.gguf");
        let coder = dir.join("lmstudio/qwen/Coder-7B.gguf");
        touch(&qwen, 1);
        touch(&coder, 1);
        assert_eq!(resolve("Qwen3.5-4B-Q4_0", Some(&dir)).unwrap(), qwen);
        assert_eq!(resolve("qwen3.5-4b-q4_0.gguf", Some(&dir)).unwrap(), qwen);
        assert_eq!(resolve("coder-7b", Some(&dir)).unwrap(), coder);
        assert_eq!(
            resolve("lmstudio/qwen/Coder-7B.gguf", Some(&dir)).unwrap(),
            coder
        );
        assert_eq!(resolve(&qwen.display().to_string(), None).unwrap(), qwen);
        assert!(resolve("/no/such/model.gguf", Some(&dir)).is_err());
        let err = resolve("missing", Some(&dir)).unwrap_err();
        assert!(err.contains("/localmodel"), "{err}");
        let err = resolve("missing", None).unwrap_err();
        assert!(err.contains("no model folder"), "{err}");
    }

    #[test]
    fn local_models_resolve_through_the_builtin_provider_and_aliases() {
        let dir = tempdir();
        let qwen = dir.join("Qwen3.5-4B-Q4_0.gguf");
        touch(&qwen, 1);
        let config = config_with(
            &dir,
            &format!(
                "[models.qwen]\nprovider = \"local\"\nmodel = \"{}\"\n",
                qwen.display()
            ),
        );
        assert!(llama_providers(&config).contains(&PROVIDER));
        let listed = list(&dir, &config);
        assert_eq!(listed[0].aliases, vec!["qwen".to_string()]);
        assert_eq!(listed[0].switch_name(), "qwen");
        assert!(listed[0].label().contains("alias qwen"));
    }

    #[test]
    fn path_like_detection() {
        assert!(is_path_like("/models/x.gguf"));
        assert!(is_path_like("~/Models/x.gguf"));
        assert!(is_path_like("x.gguf"));
        assert!(is_path_like("sub/x"));
        assert!(!is_path_like("Qwen3.5-4B-Q4_0"));
        assert!(!is_path_like("qwen"));
    }

    #[test]
    fn split_part_numbers() {
        assert_eq!(split_part("m-00001-of-00003.gguf"), Some(1));
        assert_eq!(split_part("m-00003-of-00003.gguf"), Some(3));
        assert_eq!(split_part("qwen3.5-4b-q4_0.gguf"), None);
    }

    #[test]
    fn config_edits_keep_comments_and_add_tables() {
        let dir = tempdir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# mine\n[defaults]\nmodel = \"main\" # keep\n").unwrap();
        edit_config(&path, |doc| {
            table(doc, "defaults")["local_model_dir"] = toml_edit::value("/m");
        })
        .unwrap();
        edit_config(&path, |doc| {
            let models = table(doc, "models");
            models.set_implicit(true);
            let mut e = toml_edit::Table::new();
            e["provider"] = toml_edit::value("local");
            e["model"] = toml_edit::value("/m/q.gguf");
            models.insert("q", toml_edit::Item::Table(e));
        })
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# mine") && text.contains("# keep"), "{text}");
        assert!(text.contains("local_model_dir = \"/m\""), "{text}");
        assert!(text.contains("[models.q]"), "{text}");
        let parsed: toml::Value = text.parse().unwrap();
        assert_eq!(parsed["models"]["q"]["provider"].as_str(), Some("local"));
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human_size(2_583_221_408), "2.4 GB");
        assert_eq!(human_size(700 * 1024 * 1024), "700 MB");
    }
}
