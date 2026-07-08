//! The `task` tool: lets the main agent delegate a self-contained task to a
//! subagent running a different (or the same) model. Profiles come from
//! config `[agents.*]`. Subagent activity streams back as ToolProgress;
//! permission asks bubble to the parent's frontend through the shared
//! ReplyRouter.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::permissions::PermissionEngine;
use crate::provider_factory;
use crate::tools::{Tool, ToolCtx, ToolKind, ToolOutput, ToolRegistry};

use super::agent::{Agent, AgentSettings};
use super::events::AgentEvent;

/// Serializes local (Ollama) subagent calls that use a *different* model
/// than the main agent, so two big models swap once instead of thrashing
/// VRAM. Cloud subagents never touch this.
#[derive(Default)]
pub struct LocalModelGate {
    lock: Mutex<()>,
}

/// Maximum agent nesting: main (0) -> subagent (1) -> subagent (2).
const MAX_DEPTH: u8 = 2;

pub struct TaskTool {
    config: Arc<Config>,
    permissions: Arc<PermissionEngine>,
    gate: Arc<LocalModelGate>,
    /// Model the main agent currently runs, to decide when the gate applies.
    /// Shared with the frontend so `/model` switches keep the gate honest.
    main_model: Arc<std::sync::Mutex<String>>,
    /// Serializes concurrent subagents whose profiles can mutate state
    /// (edit/write/bash) — parallel writers on one worktree conflict.
    /// Read-only scouts bypass and run truly parallel.
    write_gate: Arc<Mutex<()>>,
    /// Leaked once at construction — the Tool trait wants &'static str.
    description: &'static str,
}

#[derive(Deserialize)]
struct Args {
    agent: String,
    prompt: String,
}

impl TaskTool {
    pub fn new(
        config: Arc<Config>,
        permissions: Arc<PermissionEngine>,
        gate: Arc<LocalModelGate>,
        main_model: Arc<std::sync::Mutex<String>>,
    ) -> Self {
        // The profile list is baked into the description so the model knows
        // its options without a second lookup.
        let mut description = String::from(
            "Delegate a self-contained task to a specialist subagent. Available agents: ",
        );
        let profiles: Vec<String> = config
            .agents
            .iter()
            .map(|(name, p)| format!("`{name}` ({})", p.description))
            .collect();
        description.push_str(&profiles.join(", "));
        description.push_str(
            ". Give the full task in `prompt` — the subagent cannot see this conversation.",
        );
        let description: &'static str = Box::leak(description.into_boxed_str());
        Self {
            config,
            permissions,
            gate,
            main_model,
            write_gate: Arc::new(Mutex::new(())),
            description,
        }
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
                "prompt": { "type": "string", "description": "Complete, self-contained task description" }
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
        format!("task[{agent}]: {head}")
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

        let resolved = match provider_factory::resolve(&self.config, &profile.model) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::error(format!("cannot start agent `{}`: {e}", args.agent));
            }
        };

        // VRAM gate: hold for the entire run of a different-model local
        // subagent; tell Ollama to evict it when done.
        let needs_gate =
            resolved.is_local && resolved.model.model != *self.main_model.lock().unwrap();
        let _gate_guard = if needs_gate {
            Some(self.gate.lock.lock().await)
        } else {
            None
        };
        // Write-capable subagents serialize among themselves; read-only
        // scouts (no edit/write/bash) run truly parallel.
        let writes = profile
            .tools
            .iter()
            .any(|t| matches!(t.as_str(), "edit" | "write" | "bash" | "task"));
        let _write_guard = if writes {
            tracing::debug!(agent = args.agent, "write-capable subagent serialized");
            Some(self.write_gate.lock().await)
        } else {
            None
        };
        let mut params =
            provider_factory::gen_params(&self.config, &resolved.model, resolved.is_local);
        if needs_gate {
            params.keep_alive = Some("0".into());
        }

        // Restricted toolset. `task` itself is re-added only if depth allows,
        // via profile tools listing it explicitly — not by default.
        let tools = ToolRegistry::core().subset(&profile.tools);

        let system_prompt = profile.system_prompt.clone().unwrap_or_else(|| {
            format!(
                "You are the `{}` subagent: {}. Complete the task and report your findings as plain text. Your final message is returned to the main agent.",
                args.agent, profile.description
            )
        });

        let settings = AgentSettings {
            model: resolved.model.model.clone(),
            params,
            system_prompt,
            cwd: ctx.cwd.clone(),
            mode: crate::config::Mode::Normal, // subagents never auto-approve more than the parent
            max_iterations: profile.max_turns,
            depth: ctx.depth + 1,
        };

        // Fresh event channel for the subagent; forward as ToolProgress.
        let (sub_tx, mut sub_rx) = tokio::sync::broadcast::channel::<AgentEvent>(1024);
        let parent_events = ctx.events.clone();
        let tag = format!("task[{}]", args.agent);
        let forward_tag = tag.clone();
        let forwarder = tokio::spawn(async move {
            let mut usage_total = rocinante_providers::Usage::default();
            while let Ok(event) = sub_rx.recv().await {
                match event {
                    AgentEvent::ToolCallStarted { summary, .. } => {
                        parent_events.send(AgentEvent::ToolProgress {
                            call_id: forward_tag.clone(),
                            chunk: format!("→ {summary}"),
                        });
                    }
                    AgentEvent::ToolFinished {
                        output_preview,
                        is_error,
                        ..
                    } => {
                        let mark = if is_error { "✗" } else { "✓" };
                        parent_events.send(AgentEvent::ToolProgress {
                            call_id: forward_tag.clone(),
                            chunk: format!(
                                "{mark} {}",
                                output_preview.lines().next().unwrap_or("")
                            ),
                        });
                    }
                    // Bubble asks unchanged: the shared router delivers the
                    // frontend's answer straight to the subagent.
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
                    AgentEvent::Error { message, .. } => {
                        parent_events.send(AgentEvent::ToolProgress {
                            call_id: forward_tag.clone(),
                            chunk: format!("! {message}"),
                        });
                    }
                    _ => {}
                }
            }
            usage_total
        });

        let mut agent = Agent::new(
            resolved.provider,
            tools,
            Arc::clone(&self.permissions),
            settings,
            None, // subagent transcripts live in the parent's session as tool results
            sub_tx,
            Arc::clone(&ctx.router),
        );

        let mut result = tokio::select! {
            r = agent.submit(&args.prompt) => r,
            () = ctx.cancel.cancelled() => {
                agent.interrupter().interrupt();
                return ToolOutput::error("subagent cancelled");
            }
        };
        // Thinking models sometimes end a turn with all their output in the
        // reasoning channel and empty content. One nudge usually fixes it.
        if matches!(&result, Ok(turn) if turn.final_text.trim().is_empty()) {
            tracing::info!(
                agent = args.agent,
                "subagent returned empty text; nudging once"
            );
            result = tokio::select! {
                r = agent.submit(
                    "Your last message had no content. State your findings now as plain text."
                ) => r,
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
