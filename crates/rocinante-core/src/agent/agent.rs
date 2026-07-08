use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use rocinante_providers::{
    ChatDelta, ChatRequest, GenParams, Message, Provider, StopReason, ToolCall,
};

use crate::brainbox::Brainbox;
use crate::config::Mode;
use crate::context::{ContextManager, ContextPlan};
use crate::permissions::{Decision, PermissionEngine};
use crate::session::{Record, SessionStore};
use crate::tools::repair::{self, Validation};
use crate::tools::{ToolCtx, ToolRegistry};

use super::events::{AgentEvent, EventSender, PermissionDecision, ReplyRouter};

/// Static configuration for one agent instance.
pub struct AgentSettings {
    pub model: String,
    pub params: GenParams,
    pub system_prompt: String,
    pub cwd: PathBuf,
    pub mode: Mode,
    /// Hard cap on model-call iterations per user turn; the loop-runaway fuse.
    pub max_iterations: u32,
    /// Subagent nesting depth (0 = main agent).
    pub depth: u8,
}

pub struct TurnResult {
    pub final_text: String,
    pub iterations: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] rocinante_providers::ProviderError),
    #[error("turn cancelled")]
    Cancelled,
    #[error("iteration limit ({0}) reached")]
    IterationLimit(u32),
}

/// How many schema-invalid tool-call rounds we tolerate per turn before
/// switching to constrained decoding, then giving up.
const MAX_REPAIR_ROUNDS: u32 = 2;

/// Cancels whatever turn the agent is currently running. Valid across turns:
/// `submit` installs each turn's fresh token into the shared slot, so a
/// frontend can hold one Interrupter for the agent's whole life (unlike a
/// raw CancellationToken clone, which goes stale after one turn).
#[derive(Clone)]
pub struct Interrupter {
    slot: Arc<Mutex<CancellationToken>>,
}

impl Interrupter {
    pub fn interrupt(&self) {
        self.slot.lock().unwrap().cancel();
    }
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    permissions: Arc<PermissionEngine>,
    settings: AgentSettings,
    context: ContextManager,
    session: Option<SessionStore>,
    events: EventSender,
    router: Arc<ReplyRouter>,
    messages: Vec<Message>,
    /// Session seq per message, aligned with `messages` (0 = not persisted).
    msg_seqs: Vec<u64>,
    cancel: CancellationToken,
    /// Shared view of the active turn's token, for [`Interrupter`]s.
    cancel_slot: Arc<Mutex<CancellationToken>>,
    /// Living memory (BRAINBOX.md); None for subagents and tests.
    brainbox: Option<Brainbox>,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: ToolRegistry,
        permissions: Arc<PermissionEngine>,
        settings: AgentSettings,
        session: Option<SessionStore>,
        events: tokio::sync::broadcast::Sender<AgentEvent>,
        router: Arc<ReplyRouter>,
    ) -> Self {
        let context = ContextManager::new(settings.params.num_ctx.unwrap_or(32_768));
        let mut agent = Self {
            provider,
            tools,
            permissions,
            settings,
            context,
            session,
            events: EventSender::new(events),
            router,
            messages: Vec::new(),
            msg_seqs: Vec::new(),
            cancel: CancellationToken::new(),
            cancel_slot: Arc::new(Mutex::new(CancellationToken::new())),
            brainbox: None,
        };
        let system = Message::system(agent.settings.system_prompt.clone());
        agent.push_message(system);
        agent
    }

    pub fn with_brainbox(mut self, brainbox: Brainbox) -> Self {
        self.brainbox = Some(brainbox);
        self
    }

    pub fn has_brainbox(&self) -> bool {
        self.brainbox.is_some()
    }

    /// Session-end hook: one last brainbox update (bounded internally; a
    /// quit can never hang on it). Call before dropping the agent.
    pub async fn finalize(&self) {
        if let Some(brainbox) = &self.brainbox {
            brainbox.finalize(&self.messages).await;
        }
    }

    /// Rebuild an agent from a resumed session's reconstructed context.
    /// The stored system prompt is replaced with the current one (mode or
    /// cwd may have changed since the session was recorded).
    pub fn with_resumed_messages(mut self, resumed: Vec<Message>) -> Self {
        for msg in resumed
            .into_iter()
            .filter(|m| m.role != rocinante_providers::Role::System)
        {
            // Already on disk; don't re-persist. Seq 0 marks "pre-resume".
            self.messages.push(msg);
            self.msg_seqs.push(0);
        }
        self
    }

    fn push_message(&mut self, message: Message) {
        let seq = match &mut self.session {
            Some(store) => store.append_message(&message).unwrap_or_else(|e| {
                tracing::error!(error = %e, "failed to persist message");
                0
            }),
            None => 0,
        };
        self.messages.push(message);
        self.msg_seqs.push(seq);
    }

    pub fn interrupter(&self) -> Interrupter {
        Interrupter {
            slot: Arc::clone(&self.cancel_slot),
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.settings.mode = mode;
        if let Some(store) = &mut self.session {
            let _ = store.append(Record::ModeChange {
                mode: format!("{mode:?}"),
            });
        }
    }

    /// Hot-switch the main model. Conversation context is preserved — only
    /// the provider, model name, and generation params change; the context
    /// budget is rebuilt from the new `num_ctx`.
    pub fn set_model(&mut self, provider: Arc<dyn Provider>, model: String, params: GenParams) {
        self.provider = provider;
        self.context = ContextManager::new(params.num_ctx.unwrap_or(32_768));
        self.settings.model = model.clone();
        self.settings.params = params;
        if let Some(store) = &mut self.session {
            let _ = store.append(Record::ModelChange {
                model: model.clone(),
            });
        }
        self.events.send(AgentEvent::ModelChanged { model });
    }

    pub fn model(&self) -> &str {
        &self.settings.model
    }

    pub fn set_think(&mut self, on: bool) {
        self.settings.params.think = Some(on);
    }

    pub fn think(&self) -> bool {
        self.settings.params.think == Some(true)
    }

    pub fn mode(&self) -> Mode {
        self.settings.mode
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Run one user turn to completion: model calls and tool executions
    /// until the model stops asking for tools (or a limit trips).
    pub async fn submit(&mut self, user_input: &str) -> Result<TurnResult, AgentError> {
        let turn_id = Uuid::new_v4();
        self.cancel = CancellationToken::new();
        *self.cancel_slot.lock().unwrap() = self.cancel.clone();
        self.events.send(AgentEvent::TurnStarted { turn_id });
        self.push_message(Message::user(user_input));

        let mut final_text = String::new();
        let mut iterations = 0u32;
        let mut repair_rounds = 0u32;

        let result = loop {
            if iterations >= self.settings.max_iterations {
                break Err(AgentError::IterationLimit(self.settings.max_iterations));
            }
            iterations += 1;

            if self.context.plan(&self.messages, &self.tools.schemas())
                == ContextPlan::NeedsCompaction
                && let Err(e) = self.compact().await
            {
                tracing::warn!(error = %e, "compaction failed; continuing uncompacted");
            }

            // Constrained decoding once ordinary repair has failed twice.
            let use_format_fallback = repair_rounds >= MAX_REPAIR_ROUNDS;
            let (text, mut tool_calls, _stop) = match self.call_model(use_format_fallback).await {
                Ok(r) => r,
                Err(e) => break Err(e),
            };

            // Repair path 1: no native calls, but the text may contain some.
            let scraped;
            if tool_calls.is_empty() {
                scraped = repair::scrape_tool_calls(&self.tools, &text);
                tool_calls = scraped;
            }

            let mut assistant = Message::assistant(text.clone());
            assistant.tool_calls = tool_calls.clone();
            self.push_message(assistant);
            if !text.is_empty() && !use_format_fallback {
                final_text = text;
            }

            if tool_calls.is_empty() {
                if use_format_fallback {
                    // Even constrained decoding produced nothing usable.
                    self.events.send(AgentEvent::Error {
                        message: "model could not produce a valid tool call".into(),
                        fatal: false,
                    });
                }
                break Ok(());
            }

            // Repair path 2: schema validation with feedback to the model.
            let mut any_invalid = false;
            let mut executable = Vec::new();
            for call in tool_calls {
                match repair::validate_call(&self.tools, &call) {
                    Validation::Ok => executable.push(call),
                    Validation::Invalid(msg) | Validation::UnknownTool(msg) => {
                        any_invalid = true;
                        tracing::info!(tool = call.name, %msg, "tool call failed validation");
                        self.push_message(Message::tool_result(
                            &call.id,
                            format!("Tool call rejected: {msg}. Re-emit the call correcting this."),
                        ));
                    }
                }
            }
            if any_invalid && executable.is_empty() {
                repair_rounds += 1;
                continue; // let the model retry with the feedback
            }
            repair_rounds = 0;

            for call in executable {
                if self.cancel.is_cancelled() {
                    break;
                }
                let result_msg = self.execute_call(&call).await;
                self.push_message(result_msg);
            }
            if self.cancel.is_cancelled() {
                break Err(AgentError::Cancelled);
            }
        };

        self.events.send(AgentEvent::TurnFinished { turn_id });
        if matches!(result, Ok(()) | Err(AgentError::IterationLimit(_)))
            && let Some(brainbox) = &mut self.brainbox
        {
            brainbox.note_turn(&self.messages);
        }
        match result {
            Ok(()) => Ok(TurnResult {
                final_text,
                iterations,
            }),
            Err(AgentError::IterationLimit(n)) => {
                self.events.send(AgentEvent::Error {
                    message: format!("stopped after {n} model calls without finishing"),
                    fatal: false,
                });
                Ok(TurnResult {
                    final_text,
                    iterations,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Fold old turns into a structured summary produced by the same model.
    async fn compact(&mut self) -> anyhow::Result<()> {
        let Some((system, old, kept)) = self.context.split_for_compaction(&self.messages) else {
            anyhow::bail!("context over budget but nothing old enough to compact");
        };
        let system = system.clone();
        let kept: Vec<Message> = kept.to_vec();

        let original_goal = old
            .iter()
            .find(|m| m.role == rocinante_providers::Role::User)
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let transcript: String = old
            .iter()
            .map(|m| format!("[{:?}] {}\n", m.role, m.content))
            .collect();
        let before_tokens = rocinante_providers::tokens::estimate_messages(&self.messages, &[]);

        // Seq range being replaced (first..last persisted seq among old).
        let old_range = {
            let start = 1; // messages[0] is system
            let end = start + old.len();
            let seqs: Vec<u64> = self.msg_seqs[start..end]
                .iter()
                .copied()
                .filter(|s| *s > 0)
                .collect();
            seqs.first().copied().zip(seqs.last().copied())
        };

        let summary = self
            .one_shot(
                "You summarize coding sessions precisely. Keep exact paths, commands, errors.",
                &ContextManager::summarize_prompt(&original_goal, &transcript),
            )
            .await?;

        let replacement_head = ContextManager::rebuild(&system, &original_goal, &summary, &[]);
        // rebuild() returns [system, summary-user, ack-assistant]; kept turns follow.
        let mut new_messages = replacement_head.clone();
        new_messages.extend(kept.iter().cloned());

        if let (Some(store), Some((from_seq, to_seq))) = (&mut self.session, old_range) {
            let _ = store.append(Record::Compaction {
                from_seq,
                to_seq,
                replacement: replacement_head[1..].to_vec(), // system already on disk
            });
        }

        // Rebuild seq alignment: system keeps seq, synthetic + kept keep "0"
        // (kept messages' history is already on disk; alignment only matters
        // for the *next* compaction, which will re-persist via its record).
        let system_seq = self.msg_seqs.first().copied().unwrap_or(0);
        self.messages = new_messages;
        self.msg_seqs = vec![0; self.messages.len()];
        self.msg_seqs[0] = system_seq;

        let after_tokens = rocinante_providers::tokens::estimate_messages(&self.messages, &[]);
        self.events.send(AgentEvent::ContextCompacted {
            before_tokens,
            after_tokens,
        });
        tracing::info!(before_tokens, after_tokens, "context compacted");
        Ok(())
    }

    /// A tool-less, non-streaming-to-UI helper call (summarization etc.).
    async fn one_shot(&self, system: &str, user: &str) -> anyhow::Result<String> {
        let req = ChatRequest {
            model: self.settings.model.clone(),
            messages: vec![Message::system(system), Message::user(user)],
            tools: vec![],
            params: self.settings.params.clone(),
            format: None,
        };
        let mut stream = self.provider.chat(req).await?;
        let mut text = String::new();
        while let Some(delta) = stream.next().await {
            match delta? {
                ChatDelta::Text(t) => text.push_str(&t),
                ChatDelta::Done(_) => break,
                _ => {}
            }
        }
        Ok(text)
    }

    /// One streaming model call; returns (text, tool calls, stop reason).
    /// With `constrained`, requests JSON-schema output shaped as
    /// {"tool_calls": [...]} — the repair pipeline's last resort — and does
    /// not stream the raw JSON to the UI.
    async fn call_model(
        &mut self,
        constrained: bool,
    ) -> Result<(String, Vec<ToolCall>, StopReason), AgentError> {
        let format = constrained.then(|| self.tool_call_format_schema());
        let req = ChatRequest {
            model: self.settings.model.clone(),
            messages: self.messages.clone(),
            tools: self.tools.schemas(),
            params: self.settings.params.clone(),
            format,
        };
        let mut stream = self.provider.chat(req).await?;

        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut stop = StopReason::EndTurn;

        loop {
            tokio::select! {
                delta = stream.next() => match delta {
                    None => break,
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(ChatDelta::Text(t))) => {
                        if !constrained {
                            self.events.send(AgentEvent::AssistantText { delta: t.clone() });
                        }
                        text.push_str(&t);
                    }
                    // Display-only: never enters `messages` or the session.
                    Some(Ok(ChatDelta::Thinking(t))) => {
                        self.events.send(AgentEvent::Thinking { delta: t });
                    }
                    Some(Ok(ChatDelta::ToolCall(call))) => calls.push(call),
                    Some(Ok(ChatDelta::ToolCallPartial { .. })) => {
                        // SSE providers assemble partials before emitting
                        // whole calls (M4); Ollama never sends these.
                    }
                    Some(Ok(ChatDelta::Usage(u))) => self.events.send(AgentEvent::Usage(u)),
                    Some(Ok(ChatDelta::Done(s))) => stop = s,
                },
                () = self.cancel.cancelled() => return Err(AgentError::Cancelled),
            }
        }
        tracing::debug!(
            depth = self.settings.depth,
            text_len = text.len(),
            calls = calls.len(),
            ?stop,
            "model call complete"
        );
        Ok((text, calls, stop))
    }

    fn tool_call_format_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "tool_calls": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "enum": self.tools.names() },
                            "arguments": { "type": "object" }
                        },
                        "required": ["name", "arguments"]
                    }
                }
            },
            "required": ["tool_calls"]
        })
    }

    /// Permission-check and run a single tool call; always returns a tool
    /// message (errors and denials become tool results the model can react to).
    async fn execute_call(&mut self, call: &ToolCall) -> Message {
        let Some(tool) = self.tools.get(&call.name).cloned() else {
            return Message::tool_result(
                &call.id,
                format!(
                    "unknown tool `{}`. Available tools: {}",
                    call.name,
                    self.tools.names().join(", ")
                ),
            );
        };

        let summary = tool.describe_call(&call.arguments);
        let ctx = ToolCtx {
            cwd: self.settings.cwd.clone(),
            events: self.events.clone(),
            cancel: self.cancel.clone(),
            depth: self.settings.depth,
            router: Arc::clone(&self.router),
        };

        match self
            .permissions
            .evaluate(self.settings.mode, tool.as_ref(), &call.arguments)
        {
            Decision::Deny { reason } => {
                self.events.send(AgentEvent::ToolFinished {
                    call_id: call.id.clone(),
                    output_preview: format!("denied: {reason}"),
                    is_error: true,
                });
                return Message::tool_result(&call.id, format!("Permission denied: {reason}"));
            }
            Decision::Ask => {
                let detail = tool.preview(&call.arguments, &ctx).await;
                let decision = self
                    .ask_permission(&call.id, tool.name(), &summary, detail)
                    .await;
                match decision {
                    PermissionDecision::Allow => {}
                    PermissionDecision::AlwaysAllow => {
                        self.permissions
                            .remember_allow(tool.name(), &call.arguments);
                    }
                    PermissionDecision::Deny => {
                        self.events.send(AgentEvent::ToolFinished {
                            call_id: call.id.clone(),
                            output_preview: "denied by user".into(),
                            is_error: true,
                        });
                        return Message::tool_result(
                            &call.id,
                            "The user declined this action. Ask them how to proceed or try a different approach.",
                        );
                    }
                }
            }
            Decision::Allow => {}
        }

        self.events.send(AgentEvent::ToolCallStarted {
            call_id: call.id.clone(),
            name: tool.name().to_string(),
            summary,
        });

        let output = tool.run(call.arguments.clone(), &ctx).await;

        let preview: String = output.content.chars().take(200).collect();
        self.events.send(AgentEvent::ToolFinished {
            call_id: call.id.clone(),
            output_preview: preview,
            is_error: output.is_error,
        });

        let content = if output.is_error {
            format!("ERROR: {}", output.content)
        } else {
            output.content
        };
        Message::tool_result(&call.id, content)
    }

    /// Emit a permission request and wait for the frontend's answer
    /// (or cancellation).
    async fn ask_permission(
        &mut self,
        call_id: &str,
        tool_name: &str,
        summary: &str,
        detail: Option<String>,
    ) -> PermissionDecision {
        let request_id = Uuid::new_v4();
        let answer = self.router.register(request_id);
        self.events.send(AgentEvent::PermissionRequested {
            request_id,
            summary: summary.to_string(),
            tool_name: tool_name.to_string(),
            detail,
        });
        tracing::debug!(%request_id, call_id, "awaiting permission");

        tokio::select! {
            decision = answer => decision.unwrap_or(PermissionDecision::Deny),
            () = self.cancel.cancelled() => {
                self.router.forget(request_id);
                PermissionDecision::Deny
            }
        }
    }
}
