use std::path::{Path, PathBuf};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};

use super::schema::{Config, ProviderConfig};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config error: {0}")]
    Figment(#[from] Box<figment::Error>),
    #[error("provider `{provider}` needs environment variable `{var}` (not set)")]
    MissingApiKey { provider: String, var: String },
    #[error("default model alias `{0}` is not defined in [models]")]
    UnknownDefaultModel(String),
    #[error("mcp server `{0}` must set exactly one of `command` (stdio) or `url` (http)")]
    BadMcpServer(String),
}

/// Built-in defaults: a local Ollama provider and a gemma4:31b main model,
/// so Rocinante works with zero config on a machine running Ollama.
const BUILTIN: &str = r#"
[defaults]
model = "main"
mode = "normal"
num_ctx = 32768
keep_alive = "10m"

[providers.ollama]
type = "ollama"

[models.main]
provider = "ollama"
model = "gemma4:31b"
"#;

/// Layering, later wins: builtin -> ~/.rocinante/config.toml ->
/// <project>/.rocinante/config.toml -> ROCINANTE_* env vars.
pub fn load(project_dir: &Path) -> Result<Config, ConfigError> {
    let user_config = dirs::home_dir()
        .map(|h| h.join(".rocinante/config.toml"))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    load_from(&user_config, &project_dir.join(".rocinante/config.toml"))
}

pub fn load_from(user_config: &Path, project_config: &Path) -> Result<Config, ConfigError> {
    let mut config: Config = Figment::new()
        .merge(Toml::string(BUILTIN))
        .merge(Toml::file(user_config))
        .merge(Toml::file(project_config))
        // LOG is the tracing filter (read by the CLI), not a config key.
        .merge(Env::prefixed("ROCINANTE_").ignore(&["LOG"]).split("__"))
        .extract()
        .map_err(Box::new)?;
    inject_env_providers(&mut config);
    validate(&config)?;
    Ok(config)
}

/// Cloud providers whose API key is already in the environment work with
/// zero config: `--model anthropic/claude-opus-4-8` just resolves. A
/// user-defined provider of the same name always wins.
fn inject_env_providers(config: &mut Config) {
    use super::schema::ProviderConfig as P;
    let candidates = [
        (
            "anthropic",
            "ANTHROPIC_API_KEY",
            P::Anthropic {
                base_url: "https://api.anthropic.com".into(),
                api_key_env: "ANTHROPIC_API_KEY".into(),
            },
        ),
        (
            "gemini",
            "GEMINI_API_KEY",
            P::Gemini {
                base_url: "https://generativelanguage.googleapis.com".into(),
                api_key_env: "GEMINI_API_KEY".into(),
            },
        ),
        (
            "openai",
            "OPENAI_API_KEY",
            P::Openai {
                base_url: "https://api.openai.com/v1".into(),
                api_key_env: "OPENAI_API_KEY".into(),
            },
        ),
    ];
    for (name, key_env, provider) in candidates {
        if !config.providers.contains_key(name) && std::env::var_os(key_env).is_some() {
            config.providers.insert(name.to_string(), provider);
        }
    }
}

fn validate(config: &Config) -> Result<(), ConfigError> {
    for (name, provider) in &config.providers {
        let key_env = match provider {
            ProviderConfig::Ollama { .. } => None,
            ProviderConfig::Openai { api_key_env, .. }
            | ProviderConfig::Anthropic { api_key_env, .. }
            | ProviderConfig::Gemini { api_key_env, .. } => Some(api_key_env),
        };
        if let Some(var) = key_env
            && std::env::var_os(var).is_none()
        {
            return Err(ConfigError::MissingApiKey {
                provider: name.clone(),
                var: var.clone(),
            });
        }
    }
    if config.resolve_model(&config.defaults.model).is_none() {
        return Err(ConfigError::UnknownDefaultModel(
            config.defaults.model.clone(),
        ));
    }
    for (name, server) in &config.mcp {
        match (&server.command, &server.url) {
            (Some(_), None) | (None, Some(_)) => {}
            _ => return Err(ConfigError::BadMcpServer(name.clone())),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;

    #[test]
    fn builtin_defaults_load() {
        let missing = Path::new("/nonexistent/a.toml");
        let config = load_from(missing, missing).unwrap();
        assert_eq!(config.defaults.mode, Mode::Normal);
        let m = config.resolve_model("main").unwrap();
        assert_eq!(m.model, "gemma4:31b");
        assert_eq!(m.provider, "ollama");
    }

    #[test]
    fn project_overrides_user() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        let project = dir.path().join("project.toml");
        std::fs::write(&user, "[defaults]\nnum_ctx = 8192\n").unwrap();
        std::fs::write(&project, "[defaults]\nnum_ctx = 16384\n").unwrap();
        let config = load_from(&user, &project).unwrap();
        assert_eq!(config.defaults.num_ctx, 16384);
    }

    #[test]
    fn missing_api_key_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.toml");
        std::fs::write(
            &project,
            "[providers.anthropic]\ntype = \"anthropic\"\napi_key_env = \"ROCINANTE_TEST_SURELY_UNSET\"\n",
        )
        .unwrap();
        let err = load_from(Path::new("/nonexistent/a.toml"), &project).unwrap_err();
        assert!(
            matches!(err, ConfigError::MissingApiKey { .. }),
            "got: {err}"
        );
    }

    #[test]
    fn env_provider_injection() {
        use crate::config::ProviderConfig;
        let missing = Path::new("/nonexistent/a.toml");

        // One test body covers set/unset/user-wins to avoid env races
        // between parallel tests.
        // SAFETY: single-threaded manipulation of a test-scoped variable.
        unsafe { std::env::set_var("GEMINI_API_KEY", "test-key") };
        let config = load_from(missing, missing).unwrap();
        assert!(
            matches!(
                config.providers.get("gemini"),
                Some(ProviderConfig::Gemini { .. })
            ),
            "gemini should be injected when GEMINI_API_KEY is set"
        );

        // User-defined provider of the same name wins over injection.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.toml");
        std::fs::write(
            &project,
            "[providers.gemini]\ntype = \"gemini\"\nbase_url = \"http://proxy.local\"\napi_key_env = \"GEMINI_API_KEY\"\n",
        )
        .unwrap();
        let config = load_from(missing, &project).unwrap();
        match config.providers.get("gemini") {
            Some(ProviderConfig::Gemini { base_url, .. }) => {
                assert_eq!(base_url, "http://proxy.local")
            }
            other => panic!("expected user gemini provider, got {other:?}"),
        }

        unsafe { std::env::remove_var("GEMINI_API_KEY") };
        let config = load_from(missing, missing).unwrap();
        assert!(
            !config.providers.contains_key("gemini"),
            "gemini should not be injected without its key"
        );
    }

    #[test]
    fn bare_model_name_resolves_to_ollama() {
        let missing = Path::new("/nonexistent/a.toml");
        let config = load_from(missing, missing).unwrap();
        let m = config.resolve_model("qwen3:8b").unwrap();
        assert_eq!(m.provider, "ollama");
        assert_eq!(m.model, "qwen3:8b");
    }
}
