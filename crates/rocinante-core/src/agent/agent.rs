use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
use crate::tools::{ToolCtx, ToolKind, ToolRegistry};

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
    /// Tool results older than this many user turns are stubbed in context
    /// (full output stays in the session log). 0 disables pruning.
    pub keep_tool_turns: u32,
}

/// Dedicated model for compaction summaries (`[context] model`); when absent
/// the main model summarizes its own history.
pub struct Summarizer {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub params: GenParams,
}

/// A background-produced compaction summary waiting to be spliced in at the
/// next turn boundary.
struct PendingCompaction {
    /// `messages_generation` this summary was computed against; stale
    /// results (a blocking compact happened meanwhile) are discarded.
    generation: u64,
    /// How many messages after the system message were summarized.
    old_len: usize,
    /// Persisted seq range being replaced, as compact() computes it.
    old_range: Option<(u64, u64)>,
    /// [summary-user, ack-assistant] from ContextManager::rebuild.
    replacement: Vec<Message>,
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
    /// Language-server manager, threaded into every ToolCtx; None for
    /// subagents and tests.
    lsp: Option<Arc<crate::lsp::LspManager>>,
    /// Dedicated compaction-summary model; None = main model.
    summarizer: Option<Summarizer>,
    /// Bumped by every structural context rewrite (compact / proactive
    /// splice) — NOT by pruning, which swaps content in place.
    messages_generation: u64,
    /// Result slot for the background proactive summarizer.
    pending_compaction: Arc<Mutex<Option<PendingCompaction>>>,
    /// At most one proactive summarization at a time.
    proactive_in_flight: Arc<AtomicBool>,
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
        let context = ContextManager::new(
            settings.params.num_ctx.unwrap_or(32_768),
            settings.keep_tool_turns,
        );
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
            lsp: None,
            summarizer: None,
            messages_generation: 0,
            pending_compaction: Arc::new(Mutex::new(None)),
            proactive_in_flight: Arc::new(AtomicBool::new(false)),
        };
        let system = Message::system(agent.settings.system_prompt.clone());
        agent.push_message(system);
        agent
    }

    pub fn with_brainbox(mut self, brainbox: Brainbox) -> Self {
        self.brainbox = Some(brainbox);
        self
    }

    pub fn with_lsp(mut self, lsp: Arc<crate::lsp::LspManager>) -> Self {
        self.lsp = Some(lsp);
        self
    }

    pub fn with_summarizer(mut self, summarizer: Summarizer) -> Self {
        self.summarizer = Some(summarizer);
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
        self.context = ContextManager::new(
            params.num_ctx.unwrap_or(32_768),
            self.settings.keep_tool_turns,
        );
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

    /// Set the reasoning-effort tier. Low also forces thinking off; leaving
    /// Low clears a stale explicit off so the tier governs again.
    pub fn set_effort(&mut self, effort: rocinante_providers::Effort) {
        self.settings.params.effort = effort;
        if effort == rocinante_providers::Effort::Low {
            self.settings.params.think = Some(false);
        } else if self.settings.params.think == Some(false) {
            self.settings.params.think = None;
        }
    }

    pub fn effort(&self) -> rocinante_providers::Effort {
        self.settings.params.effort
    }

    pub fn mode(&self) -> Mode {
        self.settings.mode
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Manual `/compact`: fold old turns into a summary now, regardless of
    /// the budget threshold. Progress surfaces via ContextCompacted.
    pub async fn compact_now(&mut self) -> anyhow::Result<()> {
        self.compact().await
    }

    /// Run one user turn to completion: model calls and tool executions
    /// until the model stops asking for tools (or a limit trips).
    pub async fn submit(&mut self, user_input: &str) -> Result<TurnResult, AgentError> {
        let turn_id = Uuid::new_v4();
        self.cancel = CancellationToken::new();
        *self.cancel_slot.lock().unwrap() = self.cancel.clone();
        self.events.send(AgentEvent::TurnStarted { turn_id });
        // Turn boundary housekeeping: splice in any background summary, then
        // stub tool results that just aged out of the keep window.
        self.apply_pending_compaction();
        self.push_message(Message::user(user_input));
        self.prune_old_tool_results();

        let mut final_text = String::new();
        let mut iterations = 0u32;
        let mut repair_rounds = 0u32;

        let result = loop {
            if iterations >= self.settings.max_iterations {
                break Err(AgentError::IterationLimit(self.settings.max_iterations));
            }
            iterations += 1;

            match self.context.plan(&self.messages, &self.tools.schemas()) {
                ContextPlan::NeedsCompaction => {
                    if let Err(e) = self.compact().await {
                        tracing::warn!(error = %e, "compaction failed; continuing uncompacted");
                    }
                }
                ContextPlan::NearBudget => self.maybe_spawn_proactive_compaction(),
                ContextPlan::Fits => {}
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

            // Parallel where safe: ReadOnly and Spawn calls run concurrently
            // (subagents guard their own write conflicts); Edit and Execute
            // run sequentially afterwards, in message order. Results are
            // pushed in the ORIGINAL call order for deterministic transcripts.
            let (concurrent, sequential): (Vec<_>, Vec<_>) =
                executable.into_iter().enumerate().partition(|(_, call)| {
                    self.tools
                        .get(&call.name)
                        .is_some_and(|t| matches!(t.kind(), ToolKind::ReadOnly | ToolKind::Spawn))
                });

            let mut results: Vec<(usize, Message)> = if self.cancel.is_cancelled() {
                Vec::new()
            } else {
                futures::future::join_all(concurrent.iter().map(|(i, call)| {
                    let agent = &*self;
                    async move { (*i, agent.execute_call(call).await) }
                }))
                .await
            };
            for (i, call) in &sequential {
                if self.cancel.is_cancelled() {
                    break;
                }
                results.push((*i, self.execute_call(call).await));
            }
            results.sort_by_key(|(i, _)| *i);
            for (_, msg) in results {
                self.push_message(msg);
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

    /// Stub tool results older than the last `keep_tool_turns` user turns.
    /// The full text stays in the session JSONL; each stub is persisted as a
    /// single-seq Compaction record so a resumed session sees the pruned
    /// view. Deterministic and idempotent — re-runs every turn.
    fn prune_old_tool_results(&mut self) {
        let Some(cut) = self.context.prune_cut(&self.messages) else {
            return;
        };
        // Tool-call id → tool name, for naming the stub.
        let mut names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for msg in &self.messages[..cut] {
            for call in &msg.tool_calls {
                names.insert(call.id.as_str(), call.name.as_str());
            }
        }
        let mut stubs: Vec<(usize, String)> = Vec::new();
        for (i, msg) in self.messages[..cut].iter().enumerate() {
            if msg.role != rocinante_providers::Role::Tool
                || msg.content.len() <= crate::context::PRUNE_MIN_BYTES
                || msg.content.starts_with(crate::context::PRUNE_STUB_PREFIX)
            {
                continue;
            }
            let name = msg
                .tool_call_id
                .as_deref()
                .and_then(|id| names.get(id).copied());
            stubs.push((i, ContextManager::prune_stub(name, &msg.content)));
        }
        if stubs.is_empty() {
            return;
        }
        let count = stubs.len();
        for (i, stub) in stubs {
            self.messages[i].content = stub;
            let seq = self.msg_seqs[i];
            if seq > 0
                && let Some(store) = &mut self.session
                && let Err(e) = store.append(Record::Compaction {
                    from_seq: seq,
                    to_seq: seq,
                    replacement: vec![self.messages[i].clone()],
                })
            {
                tracing::error!(error = %e, "failed to persist prune stub");
            }
        }
        tracing::debug!(count, "pruned old tool results");
    }

    /// At 60% of budget: summarize the old turns in the background so the
    /// blocking compaction at 80% rarely fires. The result lands in
    /// `pending_compaction` and is spliced in at the next turn boundary.
    fn maybe_spawn_proactive_compaction(&self) {
        if self
            .proactive_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let Some((system, old, _kept)) = self.context.split_for_compaction(&self.messages) else {
            self.proactive_in_flight.store(false, Ordering::Release);
            return;
        };
        let system = system.clone();
        let old_len = old.len();
        let original_goal = old
            .iter()
            .find(|m| m.role == rocinante_providers::Role::User)
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let transcript: String = old
            .iter()
            .map(|m| format!("[{:?}] {}\n", m.role, m.content))
            .collect();
        let old_range = {
            let start = 1; // messages[0] is system
            let seqs: Vec<u64> = self.msg_seqs[start..start + old_len]
                .iter()
                .copied()
                .filter(|s| *s > 0)
                .collect();
            seqs.first().copied().zip(seqs.last().copied())
        };
        let (provider, model, params) = self.summary_target();
        let generation = self.messages_generation;
        let slot = Arc::clone(&self.pending_compaction);
        let flag = Arc::clone(&self.proactive_in_flight);
        tokio::spawn(async move {
            let _release = ReleaseFlag(flag);
            let prompt = ContextManager::summarize_prompt(&original_goal, &transcript);
            match one_shot_call(provider, model, params, SUMMARIZER_SYSTEM, &prompt).await {
                Ok(summary) => {
                    let replacement =
                        ContextManager::rebuild(&system, &original_goal, &summary, &[])[1..]
                            .to_vec();
                    *slot.lock().unwrap() = Some(PendingCompaction {
                        generation,
                        old_len,
                        old_range,
                        replacement,
                    });
                    tracing::debug!("proactive compaction summary ready");
                }
                Err(e) => tracing::warn!(error = %e, "proactive compaction failed"),
            }
        });
    }

    /// Splice a background-produced summary into the context. Called at turn
    /// boundaries; a summary computed against an older generation (a blocking
    /// compact happened meanwhile) is discarded.
    fn apply_pending_compaction(&mut self) {
        let Some(pending) = self.pending_compaction.lock().unwrap().take() else {
            return;
        };
        if pending.generation != self.messages_generation {
            tracing::debug!("discarding stale proactive compaction result");
            return;
        }
        let before_tokens = rocinante_providers::tokens::estimate_messages(&self.messages, &[]);
        let mut new_messages = vec![self.messages[0].clone()];
        new_messages.extend(pending.replacement.iter().cloned());
        new_messages.extend_from_slice(&self.messages[1 + pending.old_len..]);

        if let (Some(store), Some((from_seq, to_seq))) = (&mut self.session, pending.old_range) {
            let _ = store.append(Record::Compaction {
                from_seq,
                to_seq,
                replacement: pending.replacement,
            });
        }

        let system_seq = self.msg_seqs.first().copied().unwrap_or(0);
        self.messages = new_messages;
        self.msg_seqs = vec![0; self.messages.len()];
        self.msg_seqs[0] = system_seq;
        self.messages_generation += 1;

        let after_tokens = rocinante_providers::tokens::estimate_messages(&self.messages, &[]);
        self.events.send(AgentEvent::ContextCompacted {
            before_tokens,
            after_tokens,
        });
        tracing::info!(before_tokens, after_tokens, "context compacted (proactive)");
    }

    /// Provider/model/params for summarization: `[context] model` when
    /// configured, else the main model.
    fn summary_target(&self) -> (Arc<dyn Provider>, String, GenParams) {
        match &self.summarizer {
            Some(s) => (Arc::clone(&s.provider), s.model.clone(), s.params.clone()),
            None => (
                Arc::clone(&self.provider),
                self.settings.model.clone(),
                self.settings.params.clone(),
            ),
        }
    }

    /// Fold old turns into a structured summary. Blocking fallback — the
    /// proactive path usually gets there first.
    async fn compact(&mut self) -> anyhow::Result<()> {
        // A pending background result is superseded by this rewrite.
        self.pending_compaction.lock().unwrap().take();
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
                SUMMARIZER_SYSTEM,
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
        self.messages_generation += 1;

        let after_tokens = rocinante_providers::tokens::estimate_messages(&self.messages, &[]);
        self.events.send(AgentEvent::ContextCompacted {
            before_tokens,
            after_tokens,
        });
        tracing::info!(before_tokens, after_tokens, "context compacted");
        Ok(())
    }

    /// A tool-less, non-streaming-to-UI helper call (summarization etc.),
    /// on the summarizer model when configured.
    async fn one_shot(&self, system: &str, user: &str) -> anyhow::Result<String> {
        let (provider, model, params) = self.summary_target();
        one_shot_call(provider, model, params, system, user).await
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
    async fn execute_call(&self, call: &ToolCall) -> Message {
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
            lsp: self.lsp.clone(),
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
        &self,
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

const SUMMARIZER_SYSTEM: &str =
    "You summarize coding sessions precisely. Keep exact paths, commands, errors.";

/// The one-shot body as a free fn so background tasks (proactive compaction)
/// can call it without borrowing the agent.
async fn one_shot_call(
    provider: Arc<dyn Provider>,
    model: String,
    params: GenParams,
    system: &str,
    user: &str,
) -> anyhow::Result<String> {
    let req = ChatRequest {
        model,
        messages: vec![Message::system(system), Message::user(user)],
        tools: vec![],
        params,
        format: None,
    };
    let mut stream = provider.chat(req).await?;
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

/// Clears the proactive-compaction guard whatever happens in the task.
struct ReleaseFlag(Arc<AtomicBool>);
impl Drop for ReleaseFlag {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermissionEngine;
    use crate::tools::{Tool, ToolOutput};
    use async_trait::async_trait;
    use rocinante_providers::{Capabilities, ChatStream, ProviderError, ToolSchema};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// First call: three tool calls. Second call: plain text, end of turn.
    struct MockProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn id(&self) -> &str {
            "mock"
        }
        fn caps(&self) -> Capabilities {
            Capabilities {
                native_tools: true,
                structured_output: false,
                is_local: false,
            }
        }
        async fn chat(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let deltas: Vec<Result<ChatDelta, ProviderError>> = if n == 0 {
                (0..3)
                    .map(|i| {
                        Ok(ChatDelta::ToolCall(ToolCall {
                            id: format!("t{i}"),
                            name: "slow".into(),
                            arguments: serde_json::json!({ "tag": format!("r{i}") }),
                        }))
                    })
                    .chain([Ok(ChatDelta::Done(StopReason::ToolUse))])
                    .collect()
            } else {
                vec![
                    Ok(ChatDelta::Text("done".into())),
                    Ok(ChatDelta::Done(StopReason::EndTurn)),
                ]
            };
            Ok(Box::pin(futures::stream::iter(deltas)))
        }
        fn count_tokens(&self, _m: &[Message], _t: &[ToolSchema]) -> usize {
            10
        }
    }

    /// Sleeps 100ms, echoes its tag. Kind is configurable to test both the
    /// parallel (ReadOnly) and sequential (Edit) paths.
    struct SlowTool {
        kind: ToolKind,
    }

    #[async_trait]
    impl Tool for SlowTool {
        fn name(&self) -> &'static str {
            "slow"
        }
        fn description(&self) -> &'static str {
            "sleeps"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": { "tag": { "type": "string" } } })
        }
        fn kind(&self) -> ToolKind {
            self.kind
        }
        fn describe_call(&self, _args: &serde_json::Value) -> String {
            "slow".into()
        }
        async fn run(&self, args: serde_json::Value, _ctx: &ToolCtx) -> ToolOutput {
            tokio::time::sleep(Duration::from_millis(100)).await;
            ToolOutput::ok(args["tag"].as_str().unwrap_or("?").to_string())
        }
    }

    async fn run_turn(kind: ToolKind) -> (Duration, Vec<Message>) {
        let mut tools = ToolRegistry::default();
        tools.register(Arc::new(SlowTool { kind }));
        let permissions = Arc::new(PermissionEngine::from_config(&Default::default()));
        let settings = AgentSettings {
            model: "mock".into(),
            params: GenParams::default(),
            system_prompt: "sys".into(),
            cwd: std::env::temp_dir(),
            mode: Mode::Auto, // ReadOnly and Edit both auto-approved
            max_iterations: 5,
            depth: 0,
            keep_tool_turns: 3,
        };
        let (events, _rx) = tokio::sync::broadcast::channel(256);
        let mut agent = Agent::new(
            Arc::new(MockProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            permissions,
            settings,
            None,
            events,
            Arc::new(super::super::events::ReplyRouter::default()),
        );
        let start = Instant::now();
        agent.submit("go").await.unwrap();
        (start.elapsed(), agent.messages().to_vec())
    }

    #[tokio::test]
    async fn readonly_calls_run_in_parallel_with_ordered_results() {
        let (elapsed, messages) = run_turn(ToolKind::ReadOnly).await;
        assert!(
            elapsed < Duration::from_millis(250),
            "3 parallel 100ms tools took {elapsed:?}"
        );
        // Tool results land in original call order regardless of completion.
        let results: Vec<(&str, &str)> = messages
            .iter()
            .filter(|m| m.role == rocinante_providers::Role::Tool)
            .map(|m| (m.tool_call_id.as_deref().unwrap(), m.content.as_str()))
            .collect();
        assert_eq!(results, vec![("t0", "r0"), ("t1", "r1"), ("t2", "r2")]);
    }

    #[tokio::test]
    async fn edit_calls_stay_sequential() {
        let (elapsed, messages) = run_turn(ToolKind::Edit).await;
        assert!(
            elapsed >= Duration::from_millis(300),
            "3 sequential 100ms tools took {elapsed:?}"
        );
        let results: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == rocinante_providers::Role::Tool)
            .map(|m| m.tool_call_id.as_deref().unwrap())
            .collect();
        assert_eq!(results, vec!["t0", "t1", "t2"]);
    }

    /// One tool call per turn: a `big` call whenever the last message is the
    /// user's, plain text otherwise. Call ids stay unique across turns.
    struct ToolPerTurnProvider {
        counter: AtomicUsize,
    }

    #[async_trait]
    impl Provider for ToolPerTurnProvider {
        fn id(&self) -> &str {
            "mock"
        }
        fn caps(&self) -> Capabilities {
            Capabilities {
                native_tools: true,
                structured_output: false,
                is_local: false,
            }
        }
        async fn chat(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
            let user_last = req
                .messages
                .last()
                .is_some_and(|m| m.role == rocinante_providers::Role::User);
            let deltas: Vec<Result<ChatDelta, ProviderError>> = if user_last {
                let n = self.counter.fetch_add(1, Ordering::SeqCst);
                vec![
                    Ok(ChatDelta::ToolCall(ToolCall {
                        id: format!("call{n}"),
                        name: "big".into(),
                        arguments: serde_json::json!({}),
                    })),
                    Ok(ChatDelta::Done(StopReason::ToolUse)),
                ]
            } else {
                vec![
                    Ok(ChatDelta::Text("ok".into())),
                    Ok(ChatDelta::Done(StopReason::EndTurn)),
                ]
            };
            Ok(Box::pin(futures::stream::iter(deltas)))
        }
        fn count_tokens(&self, _m: &[Message], _t: &[ToolSchema]) -> usize {
            10
        }
    }

    /// Emits `bytes` of output under a recognizable first line.
    struct BigTool {
        bytes: usize,
    }

    #[async_trait]
    impl Tool for BigTool {
        fn name(&self) -> &'static str {
            "big"
        }
        fn description(&self) -> &'static str {
            "big output"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": {} })
        }
        fn kind(&self) -> ToolKind {
            ToolKind::ReadOnly
        }
        fn describe_call(&self, _args: &serde_json::Value) -> String {
            "big".into()
        }
        async fn run(&self, _args: serde_json::Value, _ctx: &ToolCtx) -> ToolOutput {
            ToolOutput::ok(format!("$ big output\n{}", "x".repeat(self.bytes)))
        }
    }

    fn agent_with(
        provider: Arc<dyn Provider>,
        tool_bytes: Option<usize>,
        session: Option<SessionStore>,
        num_ctx: u32,
        keep_tool_turns: u32,
    ) -> Agent {
        let mut tools = ToolRegistry::default();
        if let Some(bytes) = tool_bytes {
            tools.register(Arc::new(BigTool { bytes }));
        }
        let permissions = Arc::new(PermissionEngine::from_config(&Default::default()));
        let params = GenParams {
            num_ctx: Some(num_ctx),
            ..Default::default()
        };
        let settings = AgentSettings {
            model: "mock".into(),
            params,
            system_prompt: "sys".into(),
            cwd: std::env::temp_dir(),
            mode: Mode::Auto,
            max_iterations: 5,
            depth: 0,
            keep_tool_turns,
        };
        let (events, _rx) = tokio::sync::broadcast::channel(256);
        Agent::new(
            provider,
            tools,
            permissions,
            settings,
            session,
            events,
            Arc::new(super::super::events::ReplyRouter::default()),
        )
    }

    #[tokio::test]
    async fn pruning_stubs_old_results_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::create(dir.path(), "mock").unwrap();
        let path = store.path().to_path_buf();
        let provider = Arc::new(ToolPerTurnProvider {
            counter: AtomicUsize::new(0),
        });
        let mut agent = agent_with(provider, Some(1000), Some(store), 32_768, 3);
        for i in 0..4 {
            agent.submit(&format!("task {i}")).await.unwrap();
        }
        let tools: Vec<&Message> = agent
            .messages()
            .iter()
            .filter(|m| m.role == rocinante_providers::Role::Tool)
            .collect();
        assert_eq!(tools.len(), 4);
        assert!(
            tools[0]
                .content
                .starts_with(crate::context::PRUNE_STUB_PREFIX),
            "turn-1 tool result must be stubbed: {}",
            tools[0].content
        );
        assert!(
            tools[0].content.contains("big output"),
            "stub keeps the head"
        );
        for t in &tools[1..] {
            assert!(
                !t.content.starts_with(crate::context::PRUNE_STUB_PREFIX),
                "recent results stay verbatim"
            );
        }

        // Resume sees the pruned view; the full output is still on disk.
        drop(agent);
        let (_, resumed) = SessionStore::resume(&path).unwrap();
        assert!(
            resumed
                .iter()
                .any(|m| m.content.starts_with(crate::context::PRUNE_STUB_PREFIX)),
            "resume must replay the prune record"
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(&"x".repeat(1000)),
            "full output stays in JSONL"
        );
    }

    #[tokio::test]
    async fn pruning_skips_small_results() {
        let provider = Arc::new(ToolPerTurnProvider {
            counter: AtomicUsize::new(0),
        });
        // 50-byte outputs: under PRUNE_MIN_BYTES, never worth stubbing.
        let mut agent = agent_with(provider, Some(50), None, 32_768, 3);
        for i in 0..4 {
            agent.submit(&format!("task {i}")).await.unwrap();
        }
        assert!(
            agent
                .messages()
                .iter()
                .all(|m| !m.content.starts_with(crate::context::PRUNE_STUB_PREFIX)),
            "small results must not be stubbed"
        );
    }

    /// Text-only provider; every reply is short and instant.
    struct TextProvider;

    #[async_trait]
    impl Provider for TextProvider {
        fn id(&self) -> &str {
            "mock"
        }
        fn caps(&self) -> Capabilities {
            Capabilities {
                native_tools: true,
                structured_output: false,
                is_local: false,
            }
        }
        async fn chat(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
            let deltas: Vec<Result<ChatDelta, ProviderError>> = vec![
                Ok(ChatDelta::Text("ok".into())),
                Ok(ChatDelta::Done(StopReason::EndTurn)),
            ];
            Ok(Box::pin(futures::stream::iter(deltas)))
        }
        fn count_tokens(&self, _m: &[Message], _t: &[ToolSchema]) -> usize {
            10
        }
    }

    #[tokio::test]
    async fn proactive_splice_on_clean_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::create(dir.path(), "mock").unwrap();
        let path = store.path().to_path_buf();
        // usable budget = 6096 - 4096 = 2000 tokens; bands at 1200/1600.
        let mut agent = agent_with(Arc::new(TextProvider), None, Some(store), 6096, 0);
        for i in 0..3 {
            agent
                .submit(&format!("{i} {}", "a".repeat(1500)))
                .await
                .unwrap();
        }
        // Third submit crossed 60%: the background summary job is running.
        tokio::time::sleep(Duration::from_millis(200)).await;
        agent.submit("next").await.unwrap();
        assert!(
            agent.messages()[1]
                .content
                .starts_with("[Conversation compacted"),
            "background summary must splice at the turn boundary: {}",
            agent.messages()[1].content
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("compaction"), "splice must persist a record");
    }

    #[tokio::test]
    async fn stale_pending_discarded() {
        let mut agent = agent_with(Arc::new(TextProvider), None, None, 32_768, 0);
        agent.submit("hello").await.unwrap();
        let before = agent.messages().to_vec();
        // A summary computed against a generation that no longer exists.
        *agent.pending_compaction.lock().unwrap() = Some(PendingCompaction {
            generation: 99,
            old_len: 1,
            old_range: None,
            replacement: vec![Message::user("[stale summary]")],
        });
        agent.apply_pending_compaction();
        assert_eq!(
            agent.messages().len(),
            before.len(),
            "stale pending result must be discarded, not spliced"
        );
        assert!(agent.pending_compaction.lock().unwrap().is_none());
    }
}
