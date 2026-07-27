//! Construct providers from config. Shared by the CLI frontends and the
//! task tool (which builds providers for subagent profiles at call time).

use std::sync::Arc;

use rocinante_providers::{GenParams, Provider, ollama::OllamaProvider};

use crate::config::{Config, ModelConfig, ProviderConfig};

#[derive(Debug, thiserror::Error)]
pub enum FactoryError {
    #[error("unknown model alias `{0}`")]
    UnknownModel(String),
    #[error("model references undefined provider `{0}`")]
    UnknownProvider(String),
    #[error("environment variable `{0}` is not set")]
    MissingKey(String),
}

pub struct ResolvedModel {
    pub provider: Arc<dyn Provider>,
    pub model: ModelConfig,
    pub is_local: bool,
}

pub fn resolve(config: &Config, alias: &str) -> Result<ResolvedModel, FactoryError> {
    let model = config
        .resolve_model(alias)
        .ok_or_else(|| FactoryError::UnknownModel(alias.into()))?;
    let provider_config = config
        .providers
        .get(&model.provider)
        .ok_or_else(|| FactoryError::UnknownProvider(model.provider.clone()))?;

    let key = |var: &str| std::env::var(var).map_err(|_| FactoryError::MissingKey(var.to_string()));

    let (provider, is_local): (Arc<dyn Provider>, bool) = match provider_config {
        ProviderConfig::Ollama { base_url } => (
            Arc::new(OllamaProvider::new(
                model.provider.clone(),
                base_url.clone(),
            )),
            true,
        ),
        ProviderConfig::Openai {
            base_url,
            api_key_env,
        } => (
            Arc::new(
                rocinante_providers::openai_compat::OpenAiCompatProvider::new(
                    model.provider.clone(),
                    base_url.clone(),
                    key(api_key_env)?,
                ),
            ),
            false,
        ),
        ProviderConfig::Anthropic {
            base_url,
            api_key_env,
        } => (
            Arc::new(rocinante_providers::anthropic::AnthropicProvider::new(
                model.provider.clone(),
                base_url.clone(),
                key(api_key_env)?,
            )),
            false,
        ),
        ProviderConfig::Gemini {
            base_url,
            api_key_env,
        } => (
            Arc::new(rocinante_providers::gemini::GeminiProvider::new(
                model.provider.clone(),
                base_url.clone(),
                key(api_key_env)?,
            )),
            false,
        ),
    };
    Ok(ResolvedModel {
        provider,
        model,
        is_local,
    })
}

/// Everything a `/model` switch needs, resolvable from a user-typed name
/// (alias, `provider/model`, or bare Ollama tag).
pub struct SwitchTarget {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub params: GenParams,
}

pub fn resolve_switch(config: &Config, name: &str) -> Result<SwitchTarget, FactoryError> {
    let resolved = resolve(config, name)?;
    let params = gen_params(config, &resolved.model, resolved.is_local);
    Ok(SwitchTarget {
        provider: resolved.provider,
        model: resolved.model.model,
        params,
    })
}

/// The switchable-model list shown by `/model`: config aliases first, then
/// every tag each Ollama provider reports (local *and* signed-in cloud
/// tags). Discovery failure degrades to aliases-only.
pub struct ModelCatalog {
    pub entries: Vec<String>,
    /// Inventory metadata parallel to `entries` (aliases + local tags).
    pub info: Vec<ModelInfo>,
}

/// What we know about one switchable model, for the delegation briefing.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub origin: ModelOrigin,
}

#[derive(Debug, Clone)]
pub enum ModelOrigin {
    /// Ollama tag with /api/tags metadata.
    Local {
        size_bytes: u64,
        parameter_size: Option<String>,
    },
    /// `[models]` alias pointing at a provider.
    Alias { provider: String },
}

impl ModelCatalog {
    /// Numbered listing with the current model marked.
    pub fn listing(&self, current: &str) -> String {
        let mut out = String::from("models (switch with /model <number|name>):\n");
        for (i, entry) in self.entries.iter().enumerate() {
            let marker = if entry == current { " ← current" } else { "" };
            out.push_str(&format!("  {:>2}. {entry}{marker}\n", i + 1));
        }
        out.push_str("  (or any provider/model, e.g. anthropic/claude-opus-4-8)");
        out
    }

    /// Look up a `/model` argument: 1-based number or name.
    pub fn pick<'a>(&'a self, arg: &'a str) -> &'a str {
        arg.parse::<usize>()
            .ok()
            .and_then(|n| self.entries.get(n.wrapping_sub(1)))
            .map(String::as_str)
            .unwrap_or(arg)
    }

    /// Compact "models available for delegation" section for the system
    /// prompt: one line per model with a deterministic strength tier, so the
    /// main agent can hand subtasks to the cheapest adequate model.
    pub fn delegation_briefing(&self, main: &str) -> String {
        const CAP: usize = 12;
        if self.info.is_empty() {
            return String::new();
        }
        let mut out =
            String::from("\n\nModels available for delegation (task tool `model` parameter):\n");
        for info in self.info.iter().take(CAP) {
            let current = if info.name == main {
                " (current main)"
            } else {
                ""
            };
            let line = match &info.origin {
                ModelOrigin::Local {
                    size_bytes,
                    parameter_size,
                } => {
                    let tier = match parameter_size.as_deref().and_then(parse_billions) {
                        Some(b) if b < 8.0 => "fast — simple/exploration tasks",
                        Some(b) if b <= 32.0 => "general work",
                        Some(_) => "strong — hard reasoning",
                        None => "local model",
                    };
                    let size = match parameter_size {
                        Some(p) => format!("{p}, {:.1} GB", *size_bytes as f64 / 1e9),
                        None => format!("{:.1} GB", *size_bytes as f64 / 1e9),
                    };
                    format!("- {}{current}: local, {size} — {tier}\n", info.name)
                }
                ModelOrigin::Alias { provider } => format!(
                    "- {}{current}: alias via {provider} — cloud aliases cost money per token; strong reasoning\n",
                    info.name
                ),
            };
            out.push_str(&line);
        }
        if self.info.len() > CAP {
            out.push_str(&format!(
                "  (+{} more — /model lists all)\n",
                self.info.len() - CAP
            ));
        }
        out.push_str(
            "When delegating with the task tool you may set `model` to any name above. \
             Pick the SMALLEST model adequate for the subtask; omit `model` to use the \
             profile default. Local models cost time only; cloud models cost money.",
        );
        out
    }
}

/// Parse "8.0B" / "70B" / "672.9M" into billions of parameters.
fn parse_billions(s: &str) -> Option<f64> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let value: f64 = num.trim().parse().ok()?;
    match unit {
        "B" | "b" => Some(value),
        "M" | "m" => Some(value / 1000.0),
        _ => None,
    }
}

pub async fn catalog(config: &Config) -> ModelCatalog {
    let mut entries: Vec<String> = config.models.keys().cloned().collect();
    let mut info: Vec<ModelInfo> = config
        .models
        .iter()
        .map(|(alias, m)| ModelInfo {
            name: alias.clone(),
            origin: ModelOrigin::Alias {
                provider: m.provider.clone(),
            },
        })
        .collect();
    for (name, provider) in &config.providers {
        let ProviderConfig::Ollama { base_url } = provider else {
            continue;
        };
        match OllamaProvider::new(name.clone(), base_url.clone())
            .list_models()
            .await
        {
            Ok(models) => {
                for m in models {
                    if !entries.contains(&m.name) {
                        entries.push(m.name.clone());
                        info.push(ModelInfo {
                            name: m.name,
                            origin: ModelOrigin::Local {
                                size_bytes: m.size,
                                parameter_size: m.parameter_size,
                            },
                        });
                    }
                }
            }
            Err(e) => {
                tracing::warn!(provider = name, error = %e, "ollama model discovery failed");
            }
        }
    }
    ModelCatalog { entries, info }
}

/// First tag an Ollama provider reports, for `--model <provider-name>`.
pub async fn first_ollama_model(
    config: &Config,
    provider_name: &str,
) -> Result<String, FactoryError> {
    match config.providers.get(provider_name) {
        Some(ProviderConfig::Ollama { base_url }) => {
            let models = OllamaProvider::new(provider_name.to_string(), base_url.clone())
                .list_models()
                .await
                .map_err(|_| FactoryError::UnknownModel(provider_name.into()))?;
            models
                .first()
                .map(|m| m.name.clone())
                .ok_or_else(|| FactoryError::UnknownModel(provider_name.into()))
        }
        _ => Err(FactoryError::UnknownProvider(provider_name.into())),
    }
}

/// Generation params for a resolved model, applying config defaults.
/// `keep_alive` only matters for local models.
pub fn gen_params(config: &Config, model: &ModelConfig, is_local: bool) -> GenParams {
    GenParams {
        temperature: model.temperature,
        top_p: model.top_p,
        top_k: model.top_k,
        max_tokens: None,
        num_ctx: is_local.then(|| model.num_ctx.unwrap_or(config.defaults.num_ctx)),
        keep_alive: is_local.then(|| config.defaults.keep_alive.clone()),
        think: config.defaults.think.then_some(true),
        effort: config.defaults.effort,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_parameter_sizes() {
        assert_eq!(parse_billions("8.0B"), Some(8.0));
        assert_eq!(parse_billions("70B"), Some(70.0));
        let m = parse_billions("672.9M").unwrap();
        assert!((m - 0.6729).abs() < 1e-9, "got {m}");
        assert_eq!(parse_billions("unknown"), None);
    }

    #[test]
    fn delegation_briefing_tiers_and_caps() {
        let mut info: Vec<ModelInfo> = vec![
            ModelInfo {
                name: "tiny:3b".into(),
                origin: ModelOrigin::Local {
                    size_bytes: 2_000_000_000,
                    parameter_size: Some("3.0B".into()),
                },
            },
            ModelInfo {
                name: "mid:14b".into(),
                origin: ModelOrigin::Local {
                    size_bytes: 9_000_000_000,
                    parameter_size: Some("14B".into()),
                },
            },
            ModelInfo {
                name: "oracle".into(),
                origin: ModelOrigin::Alias {
                    provider: "anthropic".into(),
                },
            },
        ];
        for i in 0..12 {
            info.push(ModelInfo {
                name: format!("extra{i}"),
                origin: ModelOrigin::Alias {
                    provider: "ollama".into(),
                },
            });
        }
        let catalog = ModelCatalog {
            entries: vec![],
            info,
        };
        let brief = catalog.delegation_briefing("mid:14b");
        assert!(brief.contains("tiny:3b: local, 3.0B, 2.0 GB — fast"));
        assert!(brief.contains("mid:14b (current main)"));
        assert!(brief.contains("general work"));
        assert!(brief.contains("oracle: alias via anthropic"));
        assert!(brief.contains("(+3 more"), "cap at 12: {brief}");
        assert!(brief.contains("Pick the SMALLEST model"));
    }

    #[test]
    fn empty_inventory_adds_nothing() {
        let catalog = ModelCatalog {
            entries: vec![],
            info: vec![],
        };
        assert_eq!(catalog.delegation_briefing("x"), "");
    }
}
