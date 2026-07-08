use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelConfig>,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentProfileConfig>,
    #[serde(default)]
    pub permissions: PermissionsConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub brainbox: BrainboxConfig,
    /// MCP servers: `[mcp.<name>]` sections.
    #[serde(default)]
    pub mcp: BTreeMap<String, McpServerConfig>,
    /// LSP servers: `[lsp.<name>]` sections, merged over builtin defaults
    /// (rust, typescript, python, go) by key.
    #[serde(default)]
    pub lsp: BTreeMap<String, LspServerConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Model alias (key into [models]) used for the main agent.
    pub model: String,
    pub mode: Mode,
    pub num_ctx: u32,
    pub keep_alive: String,
    /// Extended thinking on by default (toggle in-session with /think).
    #[serde(default)]
    pub think: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            model: "main".into(),
            mode: Mode::Normal,
            num_ctx: 32_768,
            keep_alive: "10m".into(),
            think: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Normal,
    Auto,
    Plan,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum ProviderConfig {
    Ollama {
        #[serde(default = "default_ollama_url")]
        base_url: String,
    },
    /// Any OpenAI-compatible endpoint (OpenAI, OpenRouter, Groq, vLLM...).
    Openai {
        base_url: String,
        api_key_env: String,
    },
    Anthropic {
        #[serde(default = "default_anthropic_url")]
        base_url: String,
        api_key_env: String,
    },
    Gemini {
        #[serde(default = "default_gemini_url")]
        base_url: String,
        api_key_env: String,
    },
}

fn default_ollama_url() -> String {
    "http://localhost:11434".into()
}
fn default_anthropic_url() -> String {
    "https://api.anthropic.com".into()
}
fn default_gemini_url() -> String {
    "https://generativelanguage.googleapis.com".into()
}

/// A model alias: which provider serves it, the real model name, overrides.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub num_ctx: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub top_k: Option<u32>,
}

/// A subagent profile exposed through the `task` tool.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfileConfig {
    pub description: String,
    /// Model alias (key into [models]).
    pub model: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default)]
    pub system_prompt: Option<String>,
}

fn default_max_turns() -> u32 {
    15
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionsConfig {
    /// Rules like "Bash(cargo test:*)", "Read(./.env)".
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// One MCP server. Exactly one of `command` (stdio child process) or `url`
/// (streamable HTTP) must be set — validated at load.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    /// Stdio transport: executable to spawn.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Literal (non-secret) env vars for the child process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Env vars resolved from OUR environment at spawn: child var name →
    /// host env var name. Secrets stay out of config files.
    #[serde(default)]
    pub env_from: BTreeMap<String, String>,
    /// Streamable-HTTP transport: server URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Only expose these tool names (unprefixed); default = all.
    #[serde(default)]
    pub include: Option<Vec<String>>,
}

/// One LSP server: `[lsp.<name>]`. Builtin defaults exist for rust,
/// typescript, python, and go — reuse the key to override fields (unset
/// lists keep the builtin values) or set `disabled = true` to opt out.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LspServerConfig {
    /// Executable to spawn (stdio transport).
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// File extensions (without the dot) routed to this server.
    #[serde(default)]
    pub filetypes: Vec<String>,
    /// Files/dirs whose presence, walking upward from the edited file,
    /// marks the workspace root the server is started in.
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub disabled: bool,
}

/// BRAINBOX.md: living session memory (see brainbox module).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrainboxConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_update_every_turns")]
    pub update_every_turns: u32,
    /// Model alias for the updater; defaults to the session's main model.
    #[serde(default)]
    pub model: Option<String>,
}

fn default_true() -> bool {
    true
}
fn default_update_every_turns() -> u32 {
    5
}

impl Default for BrainboxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            update_every_turns: 5,
            model: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsConfig {
    /// Extra skill directories beyond ~/.rocinante/skills and .rocinante/skills
    /// (e.g. ~/.claude/skills for compatibility).
    #[serde(default)]
    pub extra_dirs: Vec<String>,
}

impl Config {
    /// Resolve a model alias to its config, falling back to treating the
    /// name as "provider/model" or a bare Ollama model name.
    pub fn resolve_model(&self, alias: &str) -> Option<ModelConfig> {
        if let Some(m) = self.models.get(alias) {
            return Some(m.clone());
        }
        if let Some((provider, model)) = alias.split_once('/')
            && self.providers.contains_key(provider)
        {
            return Some(ModelConfig {
                provider: provider.into(),
                model: model.into(),
                num_ctx: None,
                temperature: None,
                top_p: None,
                top_k: None,
            });
        }
        // Bare name: assume the first (usually only) Ollama provider.
        self.providers
            .iter()
            .find(|(_, p)| matches!(p, ProviderConfig::Ollama { .. }))
            .map(|(name, _)| ModelConfig {
                provider: name.clone(),
                model: alias.into(),
                num_ctx: None,
                temperature: None,
                top_p: None,
                top_k: None,
            })
    }
}
