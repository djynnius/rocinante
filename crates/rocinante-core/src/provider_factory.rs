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
}

pub async fn catalog(config: &Config) -> ModelCatalog {
    let mut entries: Vec<String> = config.models.keys().cloned().collect();
    for (name, provider) in &config.providers {
        let ProviderConfig::Ollama { base_url } = provider else {
            continue;
        };
        match OllamaProvider::new(name.clone(), base_url.clone())
            .list_models()
            .await
        {
            Ok(models) => {
                for (tag, _) in models {
                    if !entries.contains(&tag) {
                        entries.push(tag);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(provider = name, error = %e, "ollama model discovery failed");
            }
        }
    }
    ModelCatalog { entries }
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
                .map(|(tag, _)| tag.clone())
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
    }
}
