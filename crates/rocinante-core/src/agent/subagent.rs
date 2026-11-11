//! The `task` tool: lets the main agent delegate a self-contained task to a
//! subagent running a different (or the same) model. Profiles come from
//! config `[agents.*]`. With `[crew] persistent = true` the main agent's
//! delegations go to long-lived crew members (see `crate::crew`) that keep
//! their context between tasks; otherwise each call builds a fresh agent and
//! drops it. Subagent activity streams back as ToolProgress; permission asks
//! bubble to the parent's frontend through the shared ReplyRouter.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use rocinante_providers::GenParams;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::config::{AgentProfileConfig, Config};
use crate::permissions::PermissionEngine;
use crate::provider_factory::{self, ResolvedModel};
use crate::scheduler::{ModelRef, Scheduler};
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutput, ToolRegistry};

use super::agent::{Agent, AgentSettings};
use super::events::{AgentEvent, EventSender, ReplyRouter, SLOT_WAIT_TAG};

/// Maximum agent nesting: main (0) -> subagent (1) -> subagent (2).
const MAX_DEPTH: u8 = 2;

/// Sent once when a subagent ends its turn with no text (thinking models
/// sometimes leave everything in the reasoning channel).
pub(crate) const EMPTY_REPORT_NUDGE: &str =
    "Your last message had no content. State your findings now as plain text.";

pub struct TaskTool {
    config: Arc<Config>,
    permissions: Arc<PermissionEngine>,
    /// Model-call budget shared with the main agent: subagents draw a slot
    /// per request, and different-model local runs pass its residency gate.
    scheduler: Option<Arc<Scheduler>>,
    /// Model the main agent currently runs. Shared with the frontend so
    /// `/model` switches keep it current.
    main_model: Arc<std::sync::Mutex<String>>,
    /// `/submodel` pin shared with the frontend: when Some, EVERY subagent
    /// runs on this model — beats profile models and per-call overrides.
    pinned_model: Arc<std::sync::Mutex<Option<String>>>,
    /// Serializes concurrent subagents whose profiles can mutate state
    /// (edit/write/bash) — parallel writers on one worktree conflict.
    /// Read-only scouts bypass and run truly parallel. Shared with the crew.
    write_gate: Arc<Mutex<()>>,
    /// Persistent members for the main agent's delegations.
    crew: Option<Arc<crate::crew::CrewManager>>,
    /// Leaked once at construction — the Tool trait wants &'static str.
    description: &'static str,
}

#[derive(Deserialize)]
struct Args {
    agent: String,
    prompt: String,
    /// Optional model override (alias or catalog name); falls back to the
    /// profile's configured model.
    #[serde(default)]
    model: Option<String>,
}

impl TaskTool {
    pub fn new(
        config: Arc<Config>,
        permissions: Arc<PermissionEngine>,
        scheduler: Option<Arc<Scheduler>>,
        main_model: Arc<std::sync::Mutex<String>>,
        pinned_model: Arc<std::sync::Mutex<Option<String>>>,
    ) -> Self {
        let description = Box::leak(describe(&config, false).into_boxed_str());
        Self {
            config,
            permissions,
            scheduler,
            main_model,
            pinned_model,
            write_gate: Arc::new(Mutex::new(())),
            crew: None,
            description,
        }
    }

    /// Route the main agent's delegations to persistent crew members.
    pub fn with_crew(mut self, crew: Arc<crate::crew::CrewManager>) -> Self {
        self.write_gate = crew.write_gate();
        self.description = Box::leak(describe(&self.config, true).into_boxed_str());
        self.crew = Some(crew);
        self
    }
}

/// The tool description: the profile list baked in so the model knows its
/// options without a second lookup.
fn describe(config: &Config, persistent: bool) -> String {
    let mut description =
        String::from("Delegate a self-contained task to a specialist subagent. Available agents: ");
    let profiles: Vec<String> = config
        .agents
        .iter()
        .map(|(name, p)| format!("`{name}` ({})", p.description))
        .collect();
    description.push_str(&profiles.join(", "));
    if persistent {
        description.push_str(
            ". Each agent is one persistent crew member: it remembers its earlier tasks in this session, so follow-ups can be brief, but it works on one task at a time — parallel work needs different members. Give the full task in `prompt`; members cannot see this conversation.",
        );
    } else {
        description.push_str(
            ". Give the full task in `prompt` — the subagent cannot see this conversation.",
        );
    }
    description
}

/// Which model name a subagent run should resolve, in priority order:
/// the user's `/submodel` pin, then the call's `model` override, then the
/// profile's configured model.
fn pick_model_ref<'a>(
    pinned: Option<&'a str>,
    call_override: Option<&'a str>,
    profile: &'a str,
) -> &'a str {
    pinned.or(call_override).unwrap_or(profile)
}

/// Resolve the model for a subagent: pin > override > profile. A candidate
/// that fails to resolve falls back to the profile model rather than
/// killing the task.
pub(crate) fn resolve_subagent_model(
    config: &Config,
    pinned: Option<&str>,
    call_override: Option<&str>,
    profile_model: &str,
) -> Result<ResolvedModel, String> {
    let candidate = pick_model_ref(pinned, call_override, profile_model);
    match provider_factory::resolve(config, candidate) {
        Ok(r) => Ok(r),
        Err(e) if candidate == profile_model => Err(e.to_string()),
        Err(_) => {
            tracing::warn!(model = %candidate, "subagent model did not resolve; using profile model");
            provider_factory::resolve(config, profile_model).map_err(|e| e.to_string())
        }
    }
}

/// Whether a profile's tools can change the worktree.
pub(crate) fn profile_writes(profile: &AgentProfileConfig) -> bool {
    profile
        .tools
        .iter()
        .any(|t| matches!(t.as_str(), "edit" | "write" | "bash" | "task"))
}

/// The scheduling identity of a resolved subagent model.
pub(crate) fn model_ref_of(resolved: &ResolvedModel, params: &GenParams) -> ModelRef {
    ModelRef {
        provider: resolved.model.provider.clone(),
        ollama: resolved.is_local,
        model: resolved.model.model.clone(),
        num_ctx: params.num_ctx,
    }
}

/// The main agent's model for residency decisions: the scheduler's record,
/// else the shared tag resolved through config.
pub(crate) fn main_model_ref(
    config: &Config,
    scheduler: &Scheduler,
    main_tag: &std::sync::Mutex<String>,
) -> Option<ModelRef> {
    scheduler.main_model().or_else(|| {
        let tag = main_tag.lock().unwrap().clone();
        provider_factory::resolve(config, &tag)
            .ok()
            .map(|r| ModelRef {
                provider: r.model.provider,
                ollama: r.is_local,
                model: r.model.model,
                num_ctx: None,
            })
    })
}

/// Build the agent a profile describes: restricted tools, its system
/// prompt, `Mode::Normal` (subagents never auto-approve more than the
/// parent), no session (transcripts live in the parent's).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_subagent(
    config: &Arc<Config>,
    permissions: &Arc<PermissionEngine>,
    name: &str,
    profile: &AgentProfileConfig,
    resolved: ResolvedModel,
    params: GenParams,
    cwd: &Path,
    depth: u8,
    events: tokio::sync::broadcast::Sender<AgentEvent>,
    router: Arc<ReplyRouter>,
    scheduler: Option<&Arc<Scheduler>>,
) -> Agent {
    // Restricted toolset. `task` itself is never in core(), so nesting
    // stops here unless a frontend registers it.
    let mut tools = ToolRegistry::core().subset(&profile.tools);
    let mut system_prompt = profile.system_prompt.clone().unwrap_or_else(|| {
        format!(
            "You are the `{name}` subagent: {}. Complete the task and report your findings as plain text. Your final message is returned to the main agent.",
            profile.description
        )
    });
    system_prompt.push_str(&crew_context(config, name));
    // Profiles listing `skill` get the skill tool plus a names-only index,
    // discovered fresh — the described index would cost ~1.5k tokens per
    // subagent request.
    if profile.tools.iter().any(|t| t == "skill") {
        let skills = Arc::new(crate::skills::discover(config, cwd));
        system_prompt.push_str(&crate::skills::preamble_compact(&skills));
        tools.register(Arc::new(crate::skills::SkillTool::with_rescan(
            skills,
            Arc::clone(config),
            cwd.to_path_buf(),
        )));
    }
    if profile.tools.iter().any(|t| t == "toolbox") {
        tools.register(Arc::new(crate::toolbox::ToolboxTool::new(config)));
    }
    let settings = AgentSettings {
        model: resolved.model.model.clone(),
        params,
        system_prompt,
        cwd: cwd.to_path_buf(),
        mode: crate::config::Mode::Normal,
        max_iterations: profile.max_turns,
        depth,
        keep_tool_turns: 3,
    };
    let mut agent = Agent::new(
        resolved.provider,
        tools,
        Arc::clone(permissions),
        settings,
        None,
        events,
        router,
    );
    if let Some(s) = scheduler {
        agent = agent.with_scheduler(Arc::clone(s));
    }
    agent
}

/// Who else is aboard, and where the answers about Rocinante itself live.
/// Every subagent gets it: crew members can point the user at the right
/// crewmate, and anyone can answer "how do I … in Rocinante?" from the guide.
pub(crate) fn crew_context(config: &Config, name: &str) -> String {
    let mut out = String::new();
    let roster: Vec<String> = config
        .agents
        .iter()
        .map(|(member, p)| format!("{member} ({})", short_role(&p.description)))
        .collect();
    if config.agents.contains_key(name) && !roster.is_empty() {
        out.push_str(&format!(
            "\n\nYou are {name}, one of the crew of the Rocinante — the terminal coding agent you \
             run inside. The crew: {}. The captain is holden. The user can talk to any of you \
             directly with @name; when a request belongs to a crewmate, say who to ask \
             (e.g. \"ask @naomi\").",
            roster.join(", ")
        ));
    }
    match crate::skills::rocinante_guide_path() {
        Some(path) => out.push_str(&format!(
            "\n\nQuestions about Rocinante itself (its commands, config, models, crew): grep its \
             user guide at {} and read the matching section, then answer from it — never invent \
             a command or setting. If it is unclear whether a question is about Rocinante or the \
             user's project, ask which they mean.",
            path.display()
        )),
        None => out.push_str(
            "\n\nQuestions about Rocinante itself: say the answer is in its user guide \
             (docs/GUIDE.md); if it is unclear whether a question is about Rocinante or the \
             user's project, ask which they mean.",
        ),
    }
    out
}

/// "Explorer — fast read-only …" → "explorer": the description up to its
/// first dash or clause break, lowercased and capped, so the roster stays one
/// short line.
fn short_role(description: &str) -> String {
    let head = description
        .split(['\u{2014}', '-', '.', ':', ',', ';'])
        .next()
        .unwrap_or("")
        .trim();
    head.chars().take(28).collect::<String>().to_lowercase()
}

/// Relay a subagent's activity to `parent` under `tag`, the way task cards
/// show it: tool starts/finishes, errors, slot waits, and permission asks
/// (unchanged: the shared router delivers the answer straight back).
pub(crate) fn relay_progress(parent: &EventSender, tag: &str, event: &AgentEvent) {
    match event {
        AgentEvent::ToolCallStarted { summary, .. } => parent.send(AgentEvent::ToolProgress {
            call_id: tag.to_string(),
            chunk: format!("→ {summary}"),
        }),
        AgentEvent::ToolFinished {
            output_preview,
            is_error,
            ..
        } => {
            let mark = if *is_error { "✗" } else { "✓" };
            parent.send(AgentEvent::ToolProgress {
                call_id: tag.to_string(),
                chunk: format!("{mark} {}", output_preview.lines().next().unwrap_or("")),
            });
        }
        AgentEvent::Error { message, .. } => parent.send(AgentEvent::ToolProgress {
            call_id: tag.to_string(),
            chunk: format!("! {message}"),
        }),
        AgentEvent::ToolProgress { call_id, chunk } if call_id == SLOT_WAIT_TAG => {
            parent.send(AgentEvent::ToolProgress {
                call_id: tag.to_string(),
                chunk: chunk.clone(),
            })
        }
        _ => {}
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }
    fn description(&self) -> &'static str {
        self.description
    }
    fn schema(&self) -> serde_json::Value {
        let names: Vec<&String> = self.config.agents.keys().collect();
        json!({
            "type": "object",
            "properties": {
                "agent": { "type": "string", "enum": names, "description": "Which subagent profile to use" },
                "prompt": { "type": "string", "description": "Complete, self-contained task description" },
                "model": { "type": "string", "description": "Optional model override — an alias or any model listed for delegation. Pick the smallest adequate model; omit to use the profile default." }
            },
            "required": ["agent", "prompt"]
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Spawn
    }
    fn describe_call(&self, args: &serde_json::Value) -> String {
        let agent = args.get("agent").and_then(|v| v.as_str()).unwrap_or("?");
        let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        let head: String = prompt.chars().take(60).collect();
        match args.get("model").and_then(|v| v.as_str()) {
            Some(model) => format!("task[{agent} @ {model}]: {head}"),
            None => format!("task[{agent}]: {head}"),
        }
    }

    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput {
        let args: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::error(format!("bad arguments: {e}")),
        };
        if ctx.depth >= MAX_DEPTH {
            return ToolOutput::error("subagent nesting limit reached; do this task yourself");
        }
        let Some(profile) = self.config.agents.get(&args.agent) else {
            return ToolOutput::error(format!(
                "unknown agent `{}`. Available: {}",
                args.agent,
                self.config
                    .agents
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        let tag = format!("task[{}]", args.agent);

        // The main agent's delegations go to persistent crew members.
        if ctx.depth == 0
            && let Some(crew) = &self.crew
        {
            return match crew
                .ask_from_main(
                    &args.agent,
                    args.model.as_deref(),
                    &args.prompt,
                    ctx.events.clone(),
                    tag,
                    ctx.cancel.clone(),
                )
                .await
            {
                Ok(text) if text.trim().is_empty() => {
                    ToolOutput::error("subagent finished without a report")
                }
                Ok(text) => ToolOutput::ok(text),
                Err(e) => ToolOutput::error(e),
            };
        }

        let pinned = self.pinned_model.lock().unwrap().clone();
        let resolved = match resolve_subagent_model(
            &self.config,
            pinned.as_deref(),
            args.model.as_deref(),
            &profile.model,
        ) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::error(format!("cannot start agent `{}`: {e}", args.agent));
            }
        };
        let mut params =
            provider_factory::gen_params(&self.config, &resolved.model, resolved.is_local);
        // Residency gate: a local model other than the main one waits for
        // room for the whole run and, unless it is known to fit next to the
        // main model, unloads after each call (keep_alive 0).
        let residency = match &self.scheduler {
            Some(s) => {
                let main = main_model_ref(&self.config, s, &self.main_model);
                s.residency(main.as_ref(), &model_ref_of(&resolved, &params))
                    .await
            }
            None => None,
        };
        if residency.as_ref().is_some_and(|r| r.evict) {
            params.keep_alive = Some("0".into());
        }
        // Write-capable subagents serialize among themselves; read-only
        // scouts (no edit/write/bash) run truly parallel.
        let _write_guard = if profile_writes(profile) {
            tracing::debug!(agent = args.agent, "write-capable subagent serialized");
            Some(self.write_gate.lock().await)
        } else {
            None
        };

        // Fresh event channel for the subagent; forward as ToolProgress.
        let (sub_tx, mut sub_rx) = tokio::sync::broadcast::channel::<AgentEvent>(1024);
        let parent_events = ctx.events.clone();
        let forward_tag = tag.clone();
        let forwarder = tokio::spawn(async move {
            let mut usage_total = rocinante_providers::Usage::default();
            while let Ok(event) = sub_rx.recv().await {
                match event {
                    AgentEvent::PermissionRequested {
                        request_id,
                        summary,
                        tool_name,
                        detail,
                    } => {
                        parent_events.send(AgentEvent::PermissionRequested {
                            request_id,
                            summary: format!("[{forward_tag}] {summary}"),
                            tool_name,
                            detail,
                        });
                    }
                    AgentEvent::Usage(u) => {
                        usage_total.prompt_tokens += u.prompt_tokens;
                        usage_total.completion_tokens += u.completion_tokens;
                    }
                    other => relay_progress(&parent_events, &forward_tag, &other),
                }
            }
            usage_total
        });

        let mut agent = build_subagent(
            &self.config,
            &self.permissions,
            &args.agent,
            profile,
            resolved,
            params,
            &ctx.cwd,
            ctx.depth + 1,
            sub_tx,
            Arc::clone(&ctx.router),
            self.scheduler.as_ref(),
        );

        let mut result = tokio::select! {
            r = agent.submit(&args.prompt) => r,
            () = ctx.cancel.cancelled() => {
                agent.interrupter().interrupt();
                return ToolOutput::error("subagent cancelled");
            }
        };
        if matches!(&result, Ok(turn) if turn.final_text.trim().is_empty()) {
            tracing::info!(
                agent = args.agent,
                "subagent returned empty text; nudging once"
            );
            result = tokio::select! {
                r = agent.submit(EMPTY_REPORT_NUDGE) => r,
                () = ctx.cancel.cancelled() => {
                    agent.interrupter().interrupt();
                    return ToolOutput::error("subagent cancelled");
                }
            };
        }
        drop(agent); // closes sub event channel so the forwarder finishes
        let usage = forwarder.await.unwrap_or_default();
        tracing::info!(
            agent = args.agent,
            prompt_tokens = usage.prompt_tokens,
            completion_tokens = usage.completion_tokens,
            "subagent finished"
        );

        match result {
            Ok(turn) if turn.final_text.trim().is_empty() => {
                ToolOutput::error("subagent finished without a report")
            }
            Ok(turn) => ToolOutput::ok(turn.final_text),
            Err(e) => ToolOutput::error(format!("subagent failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_priority_pin_then_override_then_profile() {
        assert_eq!(
            pick_model_ref(Some("pinned"), Some("override"), "profile"),
            "pinned"
        );
        assert_eq!(
            pick_model_ref(None, Some("override"), "profile"),
            "override"
        );
        assert_eq!(pick_model_ref(None, None, "profile"), "profile");
    }

    #[test]
    fn persistent_description_explains_one_member_one_task() {
        let config = crate::config::load_from(
            Path::new("/nonexistent/a.toml"),
            Path::new("/nonexistent/b.toml"),
        )
        .unwrap();
        assert!(describe(&config, true).contains("parallel work needs different members"));
        assert!(!describe(&config, false).contains("parallel work needs different members"));
    }

    #[test]
    fn crew_context_names_the_crew_and_the_captain() {
        let config = crate::config::load_from(
            Path::new("/nonexistent/a.toml"),
            Path::new("/nonexistent/b.toml"),
        )
        .unwrap();
        let ctx = crew_context(&config, "holden");
        assert!(
            ctx.contains("You are holden, one of the crew of the Rocinante"),
            "{ctx}"
        );
        assert!(ctx.contains("naomi (explorer)"), "{ctx}");
        assert!(ctx.contains("camina (ml engineer)"), "{ctx}");
        assert!(ctx.contains("The captain is holden"), "{ctx}");
        assert!(ctx.contains("Questions about Rocinante itself"), "{ctx}");
        // Not a crew member: no roster, still the guide.
        let other = crew_context(&config, "stranger");
        assert!(!other.contains("one of the crew"), "{other}");
        assert!(
            other.contains("Questions about Rocinante itself"),
            "{other}"
        );
    }

    #[test]
    fn short_role_takes_the_head_of_the_description() {
        assert_eq!(short_role("Explorer — fast read-only search"), "explorer");
        assert_eq!(short_role("ML engineer — preprocessing"), "ml engineer");
        assert_eq!(short_role("my own helper"), "my own helper");
    }

    #[test]
    fn holden_knows_he_is_captain() {
        let holden = crate::config::builtin_agents()
            .into_iter()
            .find(|(n, _)| *n == "holden")
            .unwrap()
            .1;
        let prompt = holden.system_prompt.unwrap();
        assert!(prompt.contains("captain of the Rocinante"), "{prompt}");
        assert!(prompt.contains("crew"), "{prompt}");
    }
}
