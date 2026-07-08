use std::path::{Path, PathBuf};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};

use super::schema::{AgentProfileConfig, Config, ProviderConfig};

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

/// Built-in defaults: a local Ollama provider and a glm-5.2:cloud main model,
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
model = "glm-5.2:cloud"
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
    if config.defaults.builtin_agents {
        inject_builtin_agents(&mut config);
    }
    validate(&config)?;
    Ok(config)
}

/// The default crew — read-only specialist subagents named after the
/// Rocinante's crew in *The Expanse*. Injected into `[agents]` unless the
/// user disabled them or defined an agent of the same name (user wins).
/// All reference the `main` model; repoint with `[agents.<name>] model = …`.
pub fn builtin_agents() -> Vec<(&'static str, AgentProfileConfig)> {
    let read_only = || vec!["read".to_string(), "grep".to_string(), "glob".to_string()];
    let agent = |description: &str, prompt: &str, max_turns: u32| AgentProfileConfig {
        description: description.to_string(),
        model: "main".to_string(),
        tools: read_only(),
        max_turns,
        system_prompt: Some(prompt.to_string()),
    };
    vec![
        (
            "naomi",
            agent(
                "Explorer — fast read-only codebase and web exploration, search, and summary.",
                "You are Naomi, the Rocinante's systems engineer. Explore read-only and report precisely: cite file:line evidence, summarize what you found, never modify anything. You are the workhorse for 'go find and understand X'.",
                15,
            ),
        ),
        (
            "miller",
            agent(
                "Researcher — investigate a question across the repo and web, return a sourced brief.",
                "You are Miller, a detective. Follow the leads: gather evidence from the codebase and, if web/MCP tools are available, from external sources. Return a findings brief with citations. Never modify anything.",
                15,
            ),
        ),
        (
            "alex",
            agent(
                "Planner — investigate read-only, then return a concrete numbered implementation plan.",
                "You are Alex, the pilot. Plot the route: investigate read-only, then return a concrete, numbered implementation plan naming the files to change. Do not write code.",
                12,
            ),
        ),
        (
            "bobbie",
            agent(
                "Reviewer — adversarial read-only code review for bugs, edge cases, and security.",
                "You are Bobbie, a Martian marine. Review adversarially: hunt bugs, unhandled edge cases, security holes, and risky assumptions in the code or diff. Default to skeptical; report concrete findings with locations. Never modify anything.",
                12,
            ),
        ),
        (
            "amos",
            agent(
                "Debugger — root-cause a failure: reproduce, isolate, hypothesize, minimal fix, verify.",
                "You are Amos, the mechanic. Root-cause the problem: reproduce it, isolate it, form one hypothesis, propose the minimal fix, and state how to verify. Blunt and practical. No scope creep. Report only; the main agent applies the fix.",
                12,
            ),
        ),
        (
            "holden",
            agent(
                "Oracle — escalate a hard design or correctness question; returns a reasoned verdict.",
                "You are Holden, the captain. Reason carefully about the hard call put to you and return a clear verdict with the tradeoffs that decided it. Read-only.",
                10,
            ),
        ),
    ]
}

fn inject_builtin_agents(config: &mut Config) {
    for (name, profile) in builtin_agents() {
        // User-defined agent of the same name always wins.
        config.agents.entry(name.to_string()).or_insert(profile);
    }
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
        assert_eq!(m.model, "glm-5.2:cloud");
        assert_eq!(m.provider, "ollama");
    }

    #[test]
    fn builtin_crew_agents_injected_by_default() {
        let missing = Path::new("/nonexistent/a.toml");
        let config = load_from(missing, missing).unwrap();
        for name in ["naomi", "miller", "alex", "bobbie", "amos", "holden"] {
            assert!(
                config.agents.contains_key(name),
                "missing crew agent {name}"
            );
            // All reference `main`, which resolves.
            let profile = &config.agents[name];
            assert_eq!(profile.model, "main");
            assert!(config.resolve_model(&profile.model).is_some());
        }
    }

    #[test]
    fn builtin_agents_disabled_by_flag() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.toml");
        std::fs::write(&project, "[defaults]\nbuiltin_agents = false\n").unwrap();
        let config = load_from(Path::new("/nonexistent/a.toml"), &project).unwrap();
        assert!(config.agents.is_empty());
    }

    #[test]
    fn user_agent_overrides_builtin_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.toml");
        std::fs::write(
            &project,
            "[agents.naomi]\ndescription = \"my naomi\"\nmodel = \"main\"\n",
        )
        .unwrap();
        let config = load_from(Path::new("/nonexistent/a.toml"), &project).unwrap();
        assert_eq!(config.agents["naomi"].description, "my naomi");
        // Other crew still injected alongside the override.
        assert!(config.agents.contains_key("amos"));
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
