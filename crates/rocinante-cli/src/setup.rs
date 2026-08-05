//! Shared agent/session construction for both frontends (REPL and TUI),
//! so their wiring can't drift. Must be called from within a tokio runtime
//! (channel_pair spawns the reply dispatcher).

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::broadcast;

use rocinante_core::agent::events::{AgentEvent, FrontendHandle, channel_pair};
use rocinante_core::agent::subagent::{LocalModelGate, TaskTool};
use rocinante_core::agent::{Agent, AgentSettings};
use rocinante_core::brainbox::{self, Brainbox};
use rocinante_core::config::{Config, Mode};
use rocinante_core::lsp::{LspManager, LspTool};
use rocinante_core::mcp::{McpManager, TOOL_COUNT_WARNING};
use rocinante_core::permissions::PermissionEngine;
use rocinante_core::prompt;
use rocinante_core::provider_factory;
use rocinante_core::session::{SessionStore, ends_mid_turn};
use rocinante_core::skills::{self, SkillTool};
use rocinante_core::tools::ToolRegistry;

pub enum SessionChoice {
    New,
    /// `--continue`: most recent session in this project.
    Continue,
}

pub struct ResumeInfo {
    pub session_id: String,
    pub message_count: usize,
    pub mid_turn: bool,
}

pub struct FrontendSetup {
    pub agent: Agent,
    pub frontend: FrontendHandle,
    /// Clone of the agent's event sender, for frontend-synthesized events.
    pub events: broadcast::Sender<AgentEvent>,
    pub model: String,
    pub cwd: PathBuf,
    pub resume: Option<ResumeInfo>,
    /// Switchable models for `/model` (aliases + discovered Ollama tags).
    pub catalog: Arc<provider_factory::ModelCatalog>,
    pub config: Arc<Config>,
    /// Current main model, shared with the task tool's VRAM gate. Update on
    /// every `/model` switch (see [`switch_model`]).
    pub main_model: Arc<std::sync::Mutex<String>>,
    /// `/submodel` pin shared with the task tool: when Some, every subagent
    /// runs on this model regardless of profiles or per-call overrides.
    pub subagent_model: Arc<std::sync::Mutex<Option<String>>>,
    /// Running MCP server connections; kept alive for the session. Servers
    /// also exit with the process (their stdio closes), so dropping this
    /// without an explicit shutdown is safe.
    pub mcp: Option<McpManager>,
    /// Language-server clients, spawned lazily and kept for the session.
    /// Call `shutdown()` on exit so no server processes are orphaned.
    pub lsp: Arc<LspManager>,
    /// Setup-time metadata for the TUI landing screen and sidebar.
    pub session_info: rocinante_tui::SessionInfo,
    /// Startup notice when PILOT.md predates a build manifest ("run /init").
    pub pilot_stale: Option<String>,
}

/// Resolve a `/model` argument and keep the shared gate model in sync.
/// The caller applies the returned target with `Agent::set_model`.
pub fn switch_model(
    config: &Config,
    main_model: &Arc<std::sync::Mutex<String>>,
    name: &str,
) -> anyhow::Result<provider_factory::SwitchTarget> {
    let target = provider_factory::resolve_switch(config, name)
        .with_context(|| format!("cannot switch to `{name}`"))?;
    *main_model.lock().unwrap() = target.model.clone();
    // Remember the switch so the next launch starts on this model.
    rocinante_core::state::save_last_model(&target.model);
    Ok(target)
}

pub async fn build(
    config: &Config,
    model: &str,
    mode: Mode,
    session_choice: SessionChoice,
) -> anyhow::Result<FrontendSetup> {
    let cwd = std::env::current_dir()?;
    // `model` is the already-resolved startup choice (flag / remembered /
    // picker) — no config default is consulted here.
    let mut alias = model.to_string();

    // `--model <provider-name>`: pick that provider's first model and let
    // /model show the rest. Only enumerable (Ollama) providers qualify.
    if let Some(provider) = config.providers.get(&alias) {
        use rocinante_core::config::ProviderConfig;
        if matches!(provider, ProviderConfig::Ollama { .. }) {
            alias = provider_factory::first_ollama_model(config, &alias)
                .await
                .with_context(|| format!("no models available from provider `{alias}`"))?;
        } else {
            anyhow::bail!(
                "cloud providers can't be enumerated — use `--model {alias}/<model-name>`"
            );
        }
    }

    let resolved = provider_factory::resolve(config, &alias)
        .with_context(|| format!("cannot start model `{alias}`"))?;
    let model = resolved.model.clone();
    let catalog = Arc::new(provider_factory::catalog(config).await);
    let main_model = Arc::new(std::sync::Mutex::new(model.model.clone()));
    let subagent_model = Arc::new(std::sync::Mutex::new(None));

    let permissions = Arc::new(PermissionEngine::from_config(&config.permissions));
    let mut tools = ToolRegistry::core();
    if !config.agents.is_empty() {
        tools.register(Arc::new(TaskTool::new(
            Arc::new(config.clone()),
            Arc::clone(&permissions),
            Arc::new(LocalModelGate::default()),
            Arc::clone(&main_model),
            Arc::clone(&subagent_model),
        )));
    }

    let skills = skills::discover(config, &cwd);
    // User-created names for the sidebar (built-ins stay usable but hidden),
    // captured before the vector moves into SkillTool.
    let skill_names: Vec<String> = skills
        .iter()
        .filter(|s| !s.is_builtin())
        .map(|s| s.name.clone())
        .collect();
    let mut system_prompt =
        prompt::system_prompt(&cwd.display().to_string(), mode, std::env::consts::OS);
    if !skills.is_empty() {
        system_prompt.push_str(&skills::preamble(&skills));
    }
    // Model inventory: lets the main agent delegate each subtask to the
    // cheapest adequate model via the task tool's `model` parameter.
    system_prompt.push_str(&catalog.delegation_briefing(&model.model));
    // Registered even with an empty catalog: the rescan lets skills
    // installed or created mid-session activate without a restart.
    tools.register(Arc::new(SkillTool::with_rescan(
        Arc::new(skills),
        Arc::new(config.clone()),
        cwd.clone(),
    )));
    if let Some(pilot) = load_pilot(&cwd) {
        system_prompt.push_str(&prompt::pilot_section(&pilot));
    }
    if let Some(memory) = brainbox::load(&cwd) {
        system_prompt.push_str(&prompt::brainbox_section(&memory));
    }

    // Construction is cheap (no server spawns); the tool is only worth its
    // schema cost when some server's binary is actually installed.
    let lsp = Arc::new(LspManager::new(config));
    if lsp.any_available() {
        tools.register(Arc::new(LspTool::new(Arc::clone(&lsp))));
    }

    let (mcp, mcp_tool_count) = if config.mcp.is_empty() {
        (None, 0)
    } else {
        let (manager, mcp_tools) = McpManager::connect_all(config).await;
        let count = mcp_tools.len();
        for tool in mcp_tools {
            tools.register(Arc::new(tool));
        }
        (Some(manager), count)
    };
    let tool_count = tools.names().len();
    if tool_count > TOOL_COUNT_WARNING {
        tracing::warn!(
            tool_count,
            "many tools registered — every schema costs context and tool-calling accuracy; consider [mcp.<name>] include filters"
        );
    }

    let settings = AgentSettings {
        model: model.model.clone(),
        params: provider_factory::gen_params(config, &model, resolved.is_local),
        system_prompt,
        cwd: cwd.clone(),
        mode,
        max_iterations: 40,
        depth: 0,
        keep_tool_turns: config.context.keep_tool_turns,
    };

    let (session, resumed) = match session_choice {
        SessionChoice::New => (SessionStore::create(&cwd, &model.model)?, None),
        SessionChoice::Continue => {
            let path = SessionStore::latest(&cwd)?;
            let (store, messages) = SessionStore::resume(&path)?;
            let session_id = store.id.to_string();
            (store, Some((session_id, messages)))
        }
    };

    let (channels, frontend) = channel_pair();
    let events = channels.events.clone();
    let mut agent = Agent::new(
        Arc::clone(&resolved.provider),
        tools,
        permissions,
        settings,
        Some(session),
        channels.events,
        channels.router,
    )
    .with_lsp(Arc::clone(&lsp));
    // Auxiliary-model resolution: config override, else the session's main
    // model (also the fallback when the override can't be resolved).
    let resolve_aux_model = |alias: Option<&String>, what: &str| match alias {
        Some(alias) => match provider_factory::resolve(config, alias) {
            Ok(r) => {
                let params = provider_factory::gen_params(config, &r.model, r.is_local);
                (r.provider, r.model.model, params)
            }
            Err(e) => {
                tracing::warn!(alias, error = %e, "{what} model unavailable; using main model");
                (
                    Arc::clone(&resolved.provider),
                    model.model.clone(),
                    provider_factory::gen_params(config, &model, resolved.is_local),
                )
            }
        },
        None => (
            Arc::clone(&resolved.provider),
            model.model.clone(),
            provider_factory::gen_params(config, &model, resolved.is_local),
        ),
    };
    if config.brainbox.enabled {
        let (bb_provider, bb_model, bb_params) =
            resolve_aux_model(config.brainbox.model.as_ref(), "brainbox");
        agent = agent.with_brainbox(Brainbox::new(
            &cwd,
            bb_provider,
            bb_model,
            bb_params,
            config.brainbox.update_every_turns,
        ));
    }
    if config.context.model.is_some() {
        let (provider, model, params) =
            resolve_aux_model(config.context.model.as_ref(), "context summary");
        agent = agent.with_summarizer(rocinante_core::agent::Summarizer {
            provider,
            model,
            params,
        });
    }
    let resume = match resumed {
        None => None,
        Some((session_id, messages)) => {
            let info = ResumeInfo {
                session_id,
                message_count: messages.len(),
                mid_turn: ends_mid_turn(&messages),
            };
            agent = agent.with_resumed_messages(messages);
            Some(info)
        }
    };

    // An agent whose profile is byte-identical to the injected built-in was
    // injected (injection is `or_insert`, so user definitions always differ
    // or win outright); everything else is user-created. Built-ins stay
    // invocable but only surface in the sidebar while in use.
    let injected: std::collections::HashSet<&str> = rocinante_core::config::builtin_agents()
        .into_iter()
        .filter(|(name, profile)| config.agents.get(*name) == Some(profile))
        .map(|(name, _)| name)
        .collect();
    let (builtin_agent_names, agent_names): (Vec<String>, Vec<String>) = config
        .agents
        .keys()
        .cloned()
        .partition(|name| injected.contains(name.as_str()));

    let session_info = rocinante_tui::SessionInfo {
        agents: agent_names,
        builtin_agents: builtin_agent_names,
        skills: skill_names,
        mcp_tools: mcp_tool_count,
        lsp_available: lsp.any_available(),
        num_ctx: model.num_ctx.unwrap_or(config.defaults.num_ctx),
        version: env!("CARGO_PKG_VERSION"),
        resumed: resume.is_some(),
    };

    let pilot_stale = pilot_staleness(&cwd);
    Ok(FrontendSetup {
        agent,
        frontend,
        events,
        model: model.model,
        cwd,
        resume,
        catalog,
        config: Arc::new(config.clone()),
        main_model,
        subagent_model,
        mcp,
        lsp,
        session_info,
        pilot_stale,
    })
}

/// Root-level files whose change usually invalidates PILOT.md's guidance
/// (build commands, architecture, description).
const PILOT_INPUTS: &[&str] = &[
    "README.md",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "setup.py",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "Makefile",
    "CMakeLists.txt",
    "Gemfile",
    "composer.json",
    "mix.exs",
];

/// A notice when PILOT.md predates a build manifest or the README: the
/// project has moved since /init last wrote it. Purely advisory — mtime
/// comparison only, no file contents read.
fn pilot_staleness(cwd: &std::path::Path) -> Option<String> {
    let pilot = std::fs::metadata(cwd.join(".rocinante/PILOT.md"))
        .and_then(|m| m.modified())
        .ok()?;
    let changed: Vec<&str> = PILOT_INPUTS
        .iter()
        .copied()
        .filter(|name| {
            std::fs::metadata(cwd.join(name))
                .and_then(|m| m.modified())
                .is_ok_and(|t| t > pilot)
        })
        .collect();
    if changed.is_empty() {
        None
    } else {
        Some(format!(
            "PILOT.md may be stale — {} changed since it was written; run /init to refresh",
            changed.join(", ")
        ))
    }
}

/// Read `.rocinante/PILOT.md` for system-prompt injection, capped so a
/// runaway file can't eat the context budget.
fn load_pilot(cwd: &std::path::Path) -> Option<String> {
    const CAP: usize = 8192;
    let content = std::fs::read_to_string(cwd.join(".rocinante/PILOT.md")).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    if content.len() > CAP {
        let mut cut = CAP;
        while !content.is_char_boundary(cut) {
            cut -= 1;
        }
        tracing::warn!("PILOT.md exceeds {CAP} bytes; truncated for context");
        Some(format!("{}\n[PILOT.md truncated]", &content[..cut]))
    } else {
        Some(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn set_mtime(path: &std::path::Path, time: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn pilot_staleness_flags_newer_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let pilot = dir.path().join(".rocinante/PILOT.md");
        std::fs::create_dir_all(pilot.parent().unwrap()).unwrap();
        std::fs::write(&pilot, "# guide").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        // Manifest older than PILOT.md → fresh.
        set_mtime(
            &dir.path().join("Cargo.toml"),
            SystemTime::now() - Duration::from_secs(3600),
        );
        assert!(pilot_staleness(dir.path()).is_none());

        // PILOT.md older than the manifest → stale, naming the file.
        set_mtime(&pilot, SystemTime::now() - Duration::from_secs(7200));
        let notice = pilot_staleness(dir.path()).unwrap();
        assert!(notice.contains("Cargo.toml"), "{notice}");
        assert!(notice.contains("/init"), "{notice}");
    }

    #[test]
    fn pilot_staleness_none_without_pilot() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        assert!(pilot_staleness(dir.path()).is_none());
    }
}
