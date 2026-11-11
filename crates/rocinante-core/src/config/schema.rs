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
    /// Context hygiene: `[context]` — summarization model, pruning window.
    #[serde(default)]
    pub context: ContextConfig,
    /// `[learning]`: global learned rules/preferences.
    #[serde(default)]
    pub learning: LearningConfig,
    /// `[scrutiny]` (formerly `[verification]`): a critic reviews answers
    /// and completed work before the user sees them.
    #[serde(default, alias = "verification")]
    pub scrutiny: ScrutinyConfig,
    /// `[crew]`: persistent subagents for `task` and `@name` messages.
    #[serde(default)]
    pub crew: CrewConfig,
    /// `[toolbox]`: external CLIs (ferraus, pixelox, …) Rocinante installs
    /// on demand.
    #[serde(default)]
    pub toolbox: ToolboxConfig,
    /// MCP servers: `[mcp.<name>]` sections.
    #[serde(default)]
    pub mcp: BTreeMap<String, McpServerConfig>,
    /// LSP servers: `[lsp.<name>]` sections, merged over builtin defaults
    /// (rust, typescript, python, go) by key.
    #[serde(default)]
    pub lsp: BTreeMap<String, LspServerConfig>,
    /// Security-relevant keys dropped from an untrusted project config (see
    /// `config::trust`). Not persisted; set at load time so the frontend can
    /// tell the user what was ignored and suggest `/trust`.
    #[serde(skip)]
    pub untrusted_stripped: Vec<String>,
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
    /// Reasoning-effort tier (low | medium | high); switch in-session with
    /// /effort. High by default.
    #[serde(default)]
    pub effort: rocinante_providers::Effort,
    /// Inject the built-in crew agents (naomi, miller, …) into `[agents]`.
    #[serde(default = "default_true")]
    pub builtin_agents: bool,
    /// Merge the embedded skills (deep-research, code-review, …) into discovery.
    #[serde(default = "default_true")]
    pub builtin_skills: bool,
    /// TUI mouse capture: the wheel scrolls the transcript or pane under the
    /// pointer. Selecting text then needs Shift+drag (Option+drag on macOS);
    /// `/mouse` toggles it in-session.
    #[serde(default = "default_true")]
    pub mouse: bool,
    /// Folder of GGUF models for `/localmodel` (the embedded llama.cpp).
    /// Asked for the first time `/localmodel` runs; `~` is expanded.
    #[serde(default)]
    pub local_model_dir: Option<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            model: "main".into(),
            mode: Mode::Normal,
            num_ctx: 32_768,
            keep_alive: "10m".into(),
            think: false,
            effort: rocinante_providers::Effort::High,
            builtin_agents: true,
            builtin_skills: true,
            mouse: true,
            local_model_dir: None,
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
        /// Concurrent requests one loaded local model serves ("auto" probes
        /// `OLLAMA_NUM_PARALLEL`, then `/api/ps`; Ollama's default is 1).
        #[serde(default)]
        num_parallel: Slots,
        /// Ollama Cloud plan concurrency shared by every `:cloud` tag:
        /// 3 on pay-as-you-go and Pro, 10 on Max and Team.
        #[serde(default = "default_cloud_concurrency")]
        cloud_concurrency: u32,
        /// Local models that may be resident at once ("auto" learns it per
        /// session, starting at 1: different-model subagents serialize).
        #[serde(default)]
        max_loaded_models: Slots,
    },
    /// Any OpenAI-compatible endpoint (OpenAI, OpenRouter, Groq, vLLM...).
    Openai {
        base_url: String,
        api_key_env: String,
        /// Concurrent requests Rocinante sends this provider.
        #[serde(default = "default_concurrency")]
        concurrency: u32,
    },
    Anthropic {
        #[serde(default = "default_anthropic_url")]
        base_url: String,
        api_key_env: String,
        #[serde(default = "default_concurrency")]
        concurrency: u32,
    },
    Gemini {
        #[serde(default = "default_gemini_url")]
        base_url: String,
        api_key_env: String,
        #[serde(default = "default_concurrency")]
        concurrency: u32,
    },
    /// The embedded llama.cpp: GGUF files run inside Rocinante, nothing to
    /// install. Built in as `[providers.local]`; models are `local/<name>`
    /// (see `/localmodel`).
    Llamacpp {
        /// CPU threads; 0 = the physical cores.
        #[serde(default)]
        threads: u32,
        /// Layers offloaded to the GPU where this build has one (Metal on
        /// Apple Silicon); unset = all of them.
        #[serde(default)]
        gpu_layers: Option<i32>,
    },
}

impl ProviderConfig {
    /// A zero-config Ollama provider at `base_url` with default lane sizing.
    pub fn ollama(base_url: impl Into<String>) -> Self {
        ProviderConfig::Ollama {
            base_url: base_url.into(),
            num_parallel: Slots::Auto,
            cloud_concurrency: default_cloud_concurrency(),
            max_loaded_models: Slots::Auto,
        }
    }
}

/// A slot count that is either fixed or learned at runtime: TOML `"auto"`
/// or a positive integer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Slots {
    #[default]
    Auto,
    Fixed(u32),
}

impl Serialize for Slots {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Slots::Auto => s.serialize_str("auto"),
            Slots::Fixed(n) => s.serialize_u32(*n),
        }
    }
}

impl<'de> Deserialize<'de> for Slots {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Slots;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("\"auto\" or a positive integer")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Slots, E> {
                if v.eq_ignore_ascii_case("auto") {
                    return Ok(Slots::Auto);
                }
                v.trim()
                    .parse::<u64>()
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Str(v), &self))
                    .and_then(|n| self.visit_u64(n))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Slots, E> {
                match u32::try_from(v) {
                    Ok(n) if n >= 1 => Ok(Slots::Fixed(n)),
                    _ => Err(E::invalid_value(serde::de::Unexpected::Unsigned(v), &self)),
                }
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Slots, E> {
                match u64::try_from(v) {
                    Ok(n) => self.visit_u64(n),
                    Err(_) => Err(E::invalid_value(serde::de::Unexpected::Signed(v), &self)),
                }
            }
        }
        d.deserialize_any(V)
    }
}

fn default_cloud_concurrency() -> u32 {
    3
}
fn default_concurrency() -> u32 {
    4
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
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
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

/// Context hygiene: summarization model and tool-result pruning.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextConfig {
    /// Model alias for compaction summaries; defaults to the main model.
    #[serde(default)]
    pub model: Option<String>,
    /// Tool results older than this many user turns are stubbed in context
    /// (the full output stays in the session log). 0 disables pruning.
    #[serde(default = "default_keep_tool_turns")]
    pub keep_tool_turns: u32,
}

fn default_keep_tool_turns() -> u32 {
    3
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            model: None,
            keep_tool_turns: 3,
        }
    }
}

/// `[learning]`: the global learned-rules store (~/.rocinante/LESSONS.md).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LearningConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Periodic signal-capture cadence in turns; 0 disables automatic
    /// capture (`/remember` still works).
    #[serde(default = "default_learning_update_every_turns")]
    pub update_every_turns: u32,
    /// Model alias for the capture pass; defaults to the main model.
    #[serde(default)]
    pub model: Option<String>,
}

fn default_learning_update_every_turns() -> u32 {
    10
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            update_every_turns: default_learning_update_every_turns(),
            model: None,
        }
    }
}

/// `[scrutiny]`: before a turn's answer reaches the user, a critic reads
/// the ask, the draft and what the agent did, and either approves it or
/// sends findings back for a silent revision. `[verification]` and its
/// `auto` key are accepted as the old names of the table and of `work`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScrutinyConfig {
    /// `false` starts the session with answer and work review off
    /// (`/scrutiny` and `/verify` still work).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Critic model alias or tag; unset = the active main model (follows
    /// `/model`).
    #[serde(default)]
    pub model: Option<String>,
    /// Which final answers are reviewed.
    #[serde(default)]
    pub answers: AnswerScrutiny,
    /// Review completed edits/commands (the old auto-verification).
    #[serde(default = "default_true", alias = "auto")]
    pub work: bool,
    /// Silent answer rewrites before the answer is released anyway.
    #[serde(default = "default_max_revisions")]
    pub max_revisions: u32,
    /// Corrective fix passes for work before certifying done. A first-pass
    /// success stops immediately (0 modifications).
    #[serde(default = "default_max_verify_iterations")]
    pub max_iterations: u32,
    /// Trusted test/build command run in the project dir (e.g. `cargo test`).
    /// Bypasses the permission engine — never set from an untrusted project.
    #[serde(default)]
    pub check_command: Option<String>,
    #[serde(default = "default_verify_timeout")]
    pub timeout_secs: u64,
    /// How much of the review the user sees.
    #[serde(default)]
    pub show: ScrutinyShow,
    /// Under `answers = "substantial"`, shorter answers skip review unless
    /// the turn did work or produced a plan.
    #[serde(default = "default_min_chars")]
    pub min_chars: usize,
    /// Pre-action review (phase 2): `risky` asks the critic about auto-mode
    /// commands no allow rule matched and large edits before they run.
    #[serde(default)]
    pub actions: ActionScrutiny,
    /// Line count above which an edit/write counts as risky.
    #[serde(default = "default_risky_edit_lines")]
    pub risky_edit_lines: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnswerScrutiny {
    Off,
    /// Answers of at least `min_chars`, answers of turns that did work, and
    /// plan-mode plans.
    #[default]
    Substantial,
    All,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrutinyShow {
    /// Nothing beyond the final answer.
    Hidden,
    /// A status-line indicator and a one-line note after a revision.
    #[default]
    Badge,
    /// The badge plus the critic's findings.
    Findings,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionScrutiny {
    #[default]
    Off,
    Risky,
}

fn default_verify_timeout() -> u64 {
    60
}

fn default_max_verify_iterations() -> u32 {
    3
}

fn default_max_revisions() -> u32 {
    1
}

fn default_min_chars() -> usize {
    400
}

fn default_risky_edit_lines() -> usize {
    40
}

impl Default for ScrutinyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: None,
            answers: AnswerScrutiny::Substantial,
            work: true,
            max_revisions: default_max_revisions(),
            max_iterations: default_max_verify_iterations(),
            check_command: None,
            timeout_secs: default_verify_timeout(),
            show: ScrutinyShow::Badge,
            min_chars: default_min_chars(),
            actions: ActionScrutiny::Off,
            risky_edit_lines: default_risky_edit_lines(),
        }
    }
}

/// `[crew]`: subagents as long-lived members with their own context.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CrewConfig {
    /// The main agent's `task` calls go to persistent members that remember
    /// earlier tasks; `false` restores one fresh subagent per call.
    #[serde(default = "default_true")]
    pub persistent: bool,
    /// A member idle this long drops its context (`30m`, `2h`, …).
    #[serde(default = "default_max_idle")]
    pub max_idle: String,
}

fn default_max_idle() -> String {
    "30m".into()
}

impl Default for CrewConfig {
    fn default() -> Self {
        Self {
            persistent: true,
            max_idle: default_max_idle(),
        }
    }
}

/// `[toolbox]`: CLIs installed from their GitHub releases on first use.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolboxConfig {
    /// Install target; prepended to PATH for `bash` and tool commands.
    #[serde(default = "default_toolbox_dir")]
    pub dir: String,
    /// `ask`: confirm every install (also in auto mode). `never`: refuse.
    /// `always`: install without asking, honoured in auto mode only.
    #[serde(default)]
    pub auto_install: AutoInstall,
    /// Overrides for the built-in tools and user additions:
    /// `[toolbox.tools.<name>]`.
    #[serde(default)]
    pub tools: BTreeMap<String, ToolboxToolConfig>,
}

fn default_toolbox_dir() -> String {
    "~/.rocinante/bin".into()
}

impl Default for ToolboxConfig {
    fn default() -> Self {
        Self {
            dir: default_toolbox_dir(),
            auto_install: AutoInstall::Ask,
            tools: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoInstall {
    #[default]
    Ask,
    Never,
    Always,
}

/// One toolbox entry. Built-ins (ferraus, pixelox) need no fields; a user
/// addition needs `repo` (a cargo-dist GitHub release).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolboxToolConfig {
    /// `owner/name` on GitHub.
    #[serde(default)]
    pub repo: Option<String>,
    /// Command (argv) printing the version; default `<name> --version`.
    #[serde(default)]
    pub version_cmd: Option<Vec<String>>,
    /// Health check run after install, e.g. `["ferraus", "doctor", "--json"]`.
    #[serde(default)]
    pub doctor_cmd: Option<Vec<String>>,
    /// Register the tool's MCP server when installed (pixelox: 8 tools).
    #[serde(default)]
    pub mcp: bool,
    /// Arguments that start its MCP server; `{cwd}` is the project dir.
    #[serde(default)]
    pub mcp_args: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsConfig {
    /// Extra skill directories beyond ~/.rocinante/skills and .rocinante/skills
    /// (e.g. ~/.claude/skills for compatibility).
    #[serde(default)]
    pub extra_dirs: Vec<String>,
    /// Names of skills found on disk to leave out of Rocinante (e.g. a Claude
    /// Code skill written for a frontier model). Claude Code itself still
    /// sees them. Built-ins are unaffected.
    #[serde(default)]
    pub hide: Vec<String>,
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
