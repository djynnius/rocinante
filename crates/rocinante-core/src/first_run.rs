//! The first launch: which model to start on when none was passed with
//! `--model`, remembered from an earlier session, or set in config.
//!
//! When Ollama answers, the choice is its models (plus config aliases and
//! the API providers whose key is set). When it doesn't — or it has no
//! models yet — the choice is what `/localmodel` offers: GGUF files on this
//! machine, run by the embedded llama.cpp. Either picker can take a folder
//! or a model file typed by hand, and both warn before quitting with nothing
//! chosen.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{Config, ProviderConfig};
use crate::local_models as lm;
use crate::provider_factory::{self, ModelEntry};

/// Whether this build can run GGUF files itself.
pub const EMBEDDED_LLAMA: bool = cfg!(feature = "llama");

/// How long the first launch waits for Ollama to list its models.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

/// Shown once when the picker is left with nothing chosen; leaving again
/// quits.
pub const QUIT_WARNING: &str = "No model chosen. Rocinante can't run without one and will quit: \
     press Esc again to quit, or pick a model.";

/// The same warning where the entry line asks for a folder or a model file
/// (there, an empty Enter leaves too).
pub const QUIT_WARNING_PATH: &str = "No model given. Rocinante can't run without one and will \
     quit: press Enter again to quit, or type a folder or a .gguf file.";

/// The error Rocinante quits with when no model was chosen.
pub const NO_MODEL: &str = "no model chosen — run `rocinante` again to pick one, or pass --model \
     <name> (an Ollama tag, provider/model, or local/<path to a .gguf file>)";

/// Where the first launch's choices came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Ollama answered with at least one model (or config names some).
    Ollama,
    /// Ollama answered but has no models and config names none.
    OllamaEmpty,
    /// Ollama didn't answer.
    OllamaDown,
}

/// What the first-launch picker offers.
#[derive(Debug, Clone)]
pub struct Choices {
    pub source: Source,
    /// Selectable rows: Ollama tags and aliases, or (Ollama unusable) GGUF
    /// files and aliases that don't need Ollama.
    pub models: Vec<ModelEntry>,
    /// API providers whose key is set, for `provider/…` hints.
    pub providers: Vec<String>,
    /// The model folder the GGUF rows came from.
    pub local_dir: Option<PathBuf>,
    /// The Ollama address that was asked, for the banner.
    pub ollama_url: Option<String>,
}

impl Choices {
    /// Nothing to list: ask for a folder or a model file straight away.
    pub fn ask_path(&self) -> bool {
        self.source != Source::Ollama && self.models.is_empty()
    }

    /// Whether a bare model name typed into the picker can work: Ollama
    /// must be there to serve (or pull) it.
    pub fn takes_ollama_names(&self) -> bool {
        self.source != Source::OllamaDown
    }

    /// The line above the list saying why it shows what it shows.
    pub fn banner(&self) -> Option<String> {
        let why = match self.source {
            Source::Ollama => return None,
            Source::OllamaDown => format!(
                "Ollama isn't running{}.",
                self.ollama_url
                    .as_deref()
                    .map(|u| format!(" at {u}"))
                    .unwrap_or_default()
            ),
            Source::OllamaEmpty => {
                "Ollama has no models yet (`ollama pull qwen3` adds one).".to_string()
            }
        };
        let what = if !EMBEDDED_LLAMA {
            " Start Ollama and run rocinante again, or type provider/model.".to_string()
        } else if let (false, Some(dir)) = (self.models.is_empty(), &self.local_dir) {
            format!(" Models on this machine, from {}:", dir.display())
        } else if self.models.is_empty() {
            " Type the folder that holds your GGUF models, or the path to one model file."
                .to_string()
        } else {
            " Models that don't need it:".to_string()
        };
        Some(format!("{why}{what}"))
    }
}

/// Work out what the first launch offers.
pub async fn choices(config: &Config) -> Choices {
    let providers = api_providers(config);
    let ollama_url = config.providers.values().find_map(|p| match p {
        ProviderConfig::Ollama { base_url, .. } => Some(base_url.clone()),
        _ => None,
    });
    let catalog = tokio::time::timeout(DISCOVERY_TIMEOUT, provider_factory::catalog(config))
        .await
        .ok();
    if let Some(catalog) = &catalog
        && catalog.ollama_reachable
        && !catalog.entries.is_empty()
    {
        return Choices {
            source: Source::Ollama,
            models: catalog.picker_entries(),
            providers,
            local_dir: None,
            ollama_url,
        };
    }
    let source = match &catalog {
        Some(c) if c.ollama_reachable => Source::OllamaEmpty,
        _ => Source::OllamaDown,
    };
    // Without Ollama: the GGUF files in the model folder, then aliases
    // whose provider isn't Ollama (a local file, an API model).
    let local_dir = lm::model_dir(config).filter(|d| d.is_dir());
    let mut models: Vec<ModelEntry> = match (&local_dir, EMBEDDED_LLAMA) {
        (Some(dir), true) => local_entries(&absolute(dir), config),
        _ => Vec::new(),
    };
    for (alias, m) in &config.models {
        let needs_ollama = matches!(
            config.providers.get(&m.provider),
            Some(ProviderConfig::Ollama { .. })
        );
        let local = lm::llama_providers(config).contains(&m.provider.as_str());
        if needs_ollama || (local && !EMBEDDED_LLAMA) {
            continue;
        }
        // GGUF rows already carry their alias.
        if models.iter().any(|e| &e.value == alias) {
            continue;
        }
        models.push(ModelEntry {
            value: alias.clone(),
            label: format!("{}  ({alias})", m.model),
        });
    }
    let local_dir = local_dir.filter(|_| EMBEDDED_LLAMA);
    Choices {
        source,
        models,
        providers,
        local_dir,
        ollama_url,
    }
}

/// Names of configured API providers (their keys were checked at load), for
/// `provider/…` hints.
fn api_providers(config: &Config) -> Vec<String> {
    let mut providers: Vec<String> = config
        .providers
        .iter()
        .filter(|(_, p)| {
            matches!(
                p,
                ProviderConfig::Anthropic { .. }
                    | ProviderConfig::Gemini { .. }
                    | ProviderConfig::Openai { .. }
            )
        })
        .map(|(name, _)| name.clone())
        .collect();
    providers.sort();
    providers
}

/// Picker rows for the GGUF files in `dir`: the alias when one points at
/// the file, else `local/<absolute path>`, which resolves without the model
/// folder being configured yet.
fn local_entries(dir: &Path, config: &Config) -> Vec<ModelEntry> {
    lm::list(dir, config)
        .into_iter()
        .map(|m| ModelEntry {
            value: m
                .aliases
                .first()
                .cloned()
                .unwrap_or_else(|| format!("{}/{}", lm::PROVIDER, absolute(&m.path).display())),
            label: m.label(),
        })
        .collect()
}

fn absolute(p: &Path) -> PathBuf {
    let p = lm::expand_tilde(&p.to_string_lossy());
    if p.is_relative() {
        std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
    } else {
        p
    }
}

/// Whether typed text means a place on disk (a folder or a model file)
/// rather than a model name — `anthropic/claude-…` has a slash too.
pub fn is_fs_path(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    t.starts_with('/')
        || t.starts_with('~')
        || t.starts_with('.')
        || t.starts_with('\\')
        || Path::new(t).is_absolute()
        || t.to_ascii_lowercase().ends_with(".gguf")
        || absolute(Path::new(t)).exists()
}

/// What a typed folder or file path gives.
#[derive(Debug)]
pub enum PathAnswer {
    /// A model file: start on it.
    Model(String),
    /// A folder of GGUF models: list these. The folder was saved as
    /// `[defaults] local_model_dir` (so `/localmodel` knows it), or `saved`
    /// says why not.
    Folder {
        dir: PathBuf,
        models: Vec<ModelEntry>,
        saved: Result<PathBuf, String>,
    },
    /// Nothing usable there: say why and ask again.
    Retry(String),
}

/// Interpret a folder or model file typed into the first-launch picker.
pub fn answer_path(text: &str, config: &Config) -> PathAnswer {
    answer_path_with(text, config, lm::save_model_dir)
}

fn answer_path_with(
    text: &str,
    config: &Config,
    save: impl FnOnce(&Path) -> Result<PathBuf, String>,
) -> PathAnswer {
    if !EMBEDDED_LLAMA {
        return PathAnswer::Retry(
            "this build of Rocinante can't run model files (no embedded llama.cpp) — start Ollama instead"
                .into(),
        );
    }
    let path = absolute(Path::new(text.trim()));
    if path.is_file() {
        let is_gguf = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("gguf"));
        return if is_gguf {
            PathAnswer::Model(format!("{}/{}", lm::PROVIDER, path.display()))
        } else {
            PathAnswer::Retry(format!("{} is not a GGUF model file", path.display()))
        };
    }
    if path.is_dir() {
        let models = local_entries(&path, config);
        if models.is_empty() {
            return PathAnswer::Retry(format!(
                "no GGUF models in {} — try another folder, or the path to a .gguf file",
                path.display()
            ));
        }
        let saved = save(&path);
        return PathAnswer::Folder {
            dir: path,
            models,
            saved,
        };
    }
    PathAnswer::Retry(format!("nothing at {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        let missing = Path::new("/nonexistent/a.toml");
        crate::config::load_from(missing, missing).unwrap()
    }

    fn folder_with_models() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("qwen3-8b-q4.gguf"), b"GGUF").unwrap();
        std::fs::create_dir(dir.path().join("lmstudio")).unwrap();
        std::fs::write(dir.path().join("lmstudio/gemma-4b.GGUF"), b"GGUF").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"hi").unwrap();
        dir
    }

    #[test]
    fn paths_are_told_apart_from_model_names() {
        let dir = folder_with_models();
        for path in [
            "/models",
            "~/models",
            "./models",
            "../models",
            "qwen.gguf",
            "sub/qwen.GGUF",
        ] {
            assert!(is_fs_path(path), "{path}");
        }
        assert!(is_fs_path(&dir.path().display().to_string()));
        for name in ["", "qwen3:8b", "anthropic/claude-opus-4-8", "hf.co/x/y:Q4"] {
            assert!(!is_fs_path(name), "{name}");
        }
    }

    #[test]
    #[cfg_attr(not(feature = "llama"), ignore = "needs the embedded llama.cpp")]
    fn a_model_file_starts_on_it() {
        let dir = folder_with_models();
        let file = dir.path().join("qwen3-8b-q4.gguf");
        let answer = answer_path_with(&file.display().to_string(), &config(), |_| {
            panic!("a file is not saved as the folder")
        });
        match answer {
            PathAnswer::Model(m) => assert_eq!(m, format!("local/{}", file.display())),
            other => panic!("{other:?}"),
        }
        let txt = dir.path().join("notes.txt");
        assert!(matches!(
            answer_path_with(&txt.display().to_string(), &config(), |_| unreachable!()),
            PathAnswer::Retry(m) if m.contains("not a GGUF")
        ));
    }

    #[test]
    #[cfg_attr(not(feature = "llama"), ignore = "needs the embedded llama.cpp")]
    fn a_folder_lists_its_models_and_is_saved() {
        let dir = folder_with_models();
        let mut saved_as = None;
        let answer = answer_path_with(&dir.path().display().to_string(), &config(), |d| {
            saved_as = Some(d.to_path_buf());
            Ok(PathBuf::from("/home/me/.rocinante/config.toml"))
        });
        let PathAnswer::Folder {
            dir: got,
            models,
            saved,
        } = answer
        else {
            panic!("expected a folder");
        };
        assert_eq!(got, dir.path());
        assert_eq!(saved_as.as_deref(), Some(dir.path()));
        assert!(saved.is_ok());
        let values: Vec<&str> = models.iter().map(|m| m.value.as_str()).collect();
        assert_eq!(values.len(), 2, "{values:?}");
        // `local/<absolute path>` (`local//tmp/…`, or `local/C:\…` on Windows).
        assert!(
            values.iter().all(|v| v
                .strip_prefix("local/")
                .is_some_and(|p| Path::new(p).is_absolute())),
            "{values:?}"
        );
        assert!(models[0].label.starts_with("gemma-4b"), "{:?}", models[0]);
        // Each row resolves on its own: no model folder configured.
        let cfg = config();
        for v in values {
            let r = provider_factory::resolve(&cfg, v).unwrap();
            assert!(r.model.model.to_lowercase().ends_with(".gguf"));
        }
    }

    #[test]
    #[cfg_attr(not(feature = "llama"), ignore = "needs the embedded llama.cpp")]
    fn empty_or_missing_places_ask_again() {
        let empty = tempfile::tempdir().unwrap();
        assert!(matches!(
            answer_path_with(&empty.path().display().to_string(), &config(), |_| unreachable!()),
            PathAnswer::Retry(m) if m.contains("no GGUF models")
        ));
        assert!(matches!(
            answer_path_with("/no/such/place", &config(), |_| unreachable!()),
            PathAnswer::Retry(m) if m.contains("nothing at")
        ));
    }

    #[tokio::test]
    #[cfg_attr(not(feature = "llama"), ignore = "needs the embedded llama.cpp")]
    async fn without_ollama_the_model_folder_is_offered() {
        let dir = folder_with_models();
        let mut cfg = config();
        // Nothing listens here: Ollama is "down".
        cfg.providers.insert(
            "ollama".into(),
            ProviderConfig::ollama("http://127.0.0.1:9"),
        );
        cfg.defaults.local_model_dir = Some(dir.path().display().to_string());
        cfg.models.insert(
            "cloudy".into(),
            crate::config::ModelConfig {
                provider: "ollama".into(),
                model: "glm-5.3:cloud".into(),
                num_ctx: None,
                temperature: None,
                top_p: None,
                top_k: None,
            },
        );
        let c = choices(&cfg).await;
        assert_eq!(c.source, Source::OllamaDown);
        assert!(!c.takes_ollama_names());
        assert_eq!(
            c.models.len(),
            2,
            "GGUFs only, no Ollama alias: {:?}",
            c.models
        );
        assert!(!c.ask_path());
        let banner = c.banner().unwrap();
        assert!(
            banner.contains("Ollama isn't running at http://127.0.0.1:9"),
            "{banner}"
        );
        assert!(
            banner.contains(&dir.path().display().to_string()),
            "{banner}"
        );

        // No folder configured: ask for one.
        cfg.defaults.local_model_dir = None;
        let c = choices(&cfg).await;
        assert!(c.models.is_empty());
        assert!(c.ask_path());
        assert!(c.banner().unwrap().contains("Type the folder"));
    }
}
