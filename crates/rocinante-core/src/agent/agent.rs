use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use rocinante_providers::{
    ChatDelta, ChatRequest, GenParams, Message, Provider, ProviderError, Role, StopReason,
    ToolCall, Usage,
};

use crate::brainbox::Brainbox;
use crate::config::Mode;
use crate::context::{ContextManager, ContextPlan};
use crate::lessons::Lessons;
use crate::permissions::{Decision, PermissionEngine};
use crate::scheduler::{ModelRef, Permit, Scheduler};
use crate::scrutiny::{self, CriticTarget, ReviewInput, Scrutiny, Verdict};
use crate::session::{Record, SessionStore};
use crate::tools::repair::{self, Validation};
use crate::tools::{ToolCtx, ToolKind, ToolRegistry};

use super::events::{
    AgentEvent, EventSender, PermissionDecision, ReplyRouter, SLOT_WAIT_TAG, ScrutinyKind,
};

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

/// Result of one `run_loop` pass: the model/tool loop outcome plus the text
/// it settled on, whether it did real work (edits/commands), and how many
/// model calls it took.
struct LoopOutcome {
    result: Result<(), AgentError>,
    final_text: String,
    did_work: bool,
    iterations: u32,
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
    /// Global learned rules/preferences (~/.rocinante/LESSONS.md).
    lessons: Option<Lessons>,
    /// Answer and work review; None = off (subagents, tests).
    scrutiny: Option<Arc<Scrutiny>>,
    /// Model-call concurrency budget; None = unlimited (tests).
    scheduler: Option<Arc<Scheduler>>,
    /// Default subagent model, for the parallelism briefing.
    sub_model: Option<ModelRef>,
    /// This turn's prose streams as `DraftText` and its final answer is held
    /// back from the session until scrutiny releases it.
    drafting: bool,
    /// (role, content) of messages pushed this turn but not yet persisted:
    /// drafts and hidden corrective prompts.
    unpersisted: Vec<(Role, String)>,
    /// The running turn's ask, for pre-action review.
    current_ask: String,
    /// Crew manager whose reports to the main agent are injected before the
    /// next turn; None outside the main agent.
    crew: Option<Arc<crate::crew::CrewManager>>,
    /// `keep_alive` as configured, restored by `set_keep_alive(None)`.
    base_keep_alive: Option<String>,
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
            lessons: None,
            scrutiny: None,
            scheduler: None,
            sub_model: None,
            drafting: false,
            unpersisted: Vec::new(),
            current_ask: String::new(),
            crew: None,
            base_keep_alive: None,
            messages_generation: 0,
            pending_compaction: Arc::new(Mutex::new(None)),
            proactive_in_flight: Arc::new(AtomicBool::new(false)),
        };
        agent.base_keep_alive = agent.settings.params.keep_alive.clone();
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

    pub fn with_lessons(mut self, lessons: Lessons) -> Self {
        self.lessons = Some(lessons);
        self
    }

    pub fn with_scrutiny(mut self, scrutiny: Scrutiny) -> Self {
        self.scrutiny = Some(Arc::new(scrutiny));
        self
    }

    /// Draw every model call from `scheduler`'s lanes. On the main agent
    /// (depth 0) its lane becomes the one the sidebar reports.
    pub fn with_scheduler(mut self, scheduler: Arc<Scheduler>) -> Self {
        if self.settings.depth == 0 {
            scheduler.set_main(&self.model_ref());
        }
        self.scheduler = Some(scheduler);
        self
    }

    pub fn with_crew(mut self, crew: Arc<crate::crew::CrewManager>) -> Self {
        self.crew = Some(crew);
        self
    }

    pub fn scheduler(&self) -> Option<&Arc<Scheduler>> {
        self.scheduler.as_ref()
    }

    /// The scheduling identity of the model this agent runs.
    pub fn model_ref(&self) -> ModelRef {
        ModelRef::new(
            self.provider.as_ref(),
            &self.settings.model,
            self.settings.params.num_ctx,
        )
    }

    /// Set the default subagent model and rewrite the parallelism sentence
    /// of the system prompt to match.
    pub fn set_sub_model(&mut self, sub: Option<ModelRef>) {
        self.sub_model = sub;
        self.refresh_parallelism();
    }

    fn refresh_parallelism(&mut self) {
        let Some(scheduler) = &self.scheduler else {
            return;
        };
        let note = scheduler.parallelism_note(&self.model_ref(), self.sub_model.as_ref());
        if let Some(sys) = self.messages.first_mut()
            && sys.role == Role::System
            && sys.content.contains(crate::scheduler::PARALLELISM_PREFIX)
        {
            sys.content = crate::scheduler::replace_parallelism_line(&sys.content, &note);
        }
    }

    /// Current scrutiny settings, when scrutiny is configured.
    pub fn scrutiny(&self) -> Option<&Scrutiny> {
        self.scrutiny.as_deref()
    }

    /// Change scrutiny settings between turns (`/scrutiny …`).
    pub fn update_scrutiny(&mut self, f: impl FnOnce(&mut Scrutiny)) {
        if let Some(s) = &mut self.scrutiny {
            f(Arc::make_mut(s));
        }
    }

    /// `/verify`: run the quality-check on the last completed task on demand,
    /// recovering the ask + result from the message history. Drives the same
    /// iterate-and-fix loop as an auto-verify, wrapped in its own turn frame so
    /// the UI shows activity and Esc can cancel it.
    pub async fn verify_last(&mut self) {
        if self.scrutiny.is_none() {
            self.events.send(AgentEvent::Error {
                message: "scrutiny is not configured for this agent".into(),
                fatal: false,
            });
            return;
        }
        let ask = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == rocinante_providers::Role::User)
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let mut result = self
            .messages
            .iter()
            .rev()
            .find(|m| {
                m.role == rocinante_providers::Role::Assistant && !m.content.trim().is_empty()
            })
            .map(|m| m.content.clone())
            .unwrap_or_default();

        let turn_id = Uuid::new_v4();
        self.cancel = CancellationToken::new();
        *self.cancel_slot.lock().unwrap() = self.cancel.clone();
        self.events.send(AgentEvent::TurnStarted { turn_id });
        self.iterate_verification(&ask, &mut result, None).await;
        self.events.send(AgentEvent::TurnFinished { turn_id });
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
    pub fn set_model(
        &mut self,
        provider: Arc<dyn Provider>,
        model: String,
        display: String,
        params: GenParams,
    ) {
        self.provider = provider;
        self.context = ContextManager::new(
            params.num_ctx.unwrap_or(32_768),
            self.settings.keep_tool_turns,
        );
        self.settings.model = model.clone();
        self.settings.params = params;
        if let Some(scheduler) = &self.scheduler
            && self.settings.depth == 0
        {
            scheduler.set_main(&self.model_ref());
        }
        self.refresh_parallelism();
        if let Some(store) = &mut self.session {
            // The session record keeps the wire tag: resume must not depend
            // on an alias that may no longer exist in config.
            let _ = store.append(Record::ModelChange {
                model: model.clone(),
            });
        }
        self.events
            .send(AgentEvent::ModelChanged { model, display });
    }

    pub fn model(&self) -> &str {
        &self.settings.model
    }

    /// Override Ollama residency for this agent's later requests (`None`
    /// restores the configured value). Crew members unload after each call
    /// while a residency gate says so.
    pub fn set_keep_alive(&mut self, keep_alive: Option<String>) {
        self.settings.params.keep_alive = keep_alive.or_else(|| self.base_keep_alive.clone());
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

    /// `/clear`: discard the conversation, keeping only the system message,
    /// so the next turn starts fresh. With `all`, also strip the injected
    /// BRAINBOX head from the system prompt and wipe BRAINBOX.md. A `Clear`
    /// record is written so `-c` resume starts from the cleared state.
    pub fn clear_context(&mut self, all: bool) {
        // Keep messages[0] (the system prompt); drop the rest.
        self.messages.truncate(1);
        self.msg_seqs.truncate(1);
        // A pending proactive-compaction result is now stale.
        self.pending_compaction.lock().unwrap().take();
        self.messages_generation += 1;

        if all {
            if let Some(sys) = self.messages.first_mut()
                && let Some(cut) = sys.content.find(crate::prompt::BRAINBOX_SECTION_MARKER)
            {
                sys.content.truncate(cut);
            }
            if let Some(brainbox) = &mut self.brainbox {
                brainbox.clear();
            }
        }

        if let Some(store) = &mut self.session {
            let _ = store.append(Record::Clear);
        }
    }

    /// Run one user turn to completion: model calls and tool executions
    /// until the model stops asking for tools (or a limit trips). With answer
    /// scrutiny on, the answer streams as a draft and a critic reviews it
    /// before release; with work scrutiny on and real work done, an inline
    /// fix loop corrects any gaps before certifying done.
    pub async fn submit(&mut self, user_input: &str) -> Result<TurnResult, AgentError> {
        let turn_id = Uuid::new_v4();
        self.cancel = CancellationToken::new();
        *self.cancel_slot.lock().unwrap() = self.cancel.clone();
        self.events.send(AgentEvent::TurnStarted { turn_id });
        // Turn boundary housekeeping: splice in any background summary, then
        // stub aged-out tool results — but only under context pressure, so
        // history stays byte-stable and Ollama's KV prefix cache keeps hits.
        self.apply_pending_compaction();
        let input = match &self.crew {
            Some(crew) => crate::crew::with_reports(crew.take_main_reports(), user_input),
            None => user_input.to_string(),
        };
        self.push_message(Message::user(input));
        if self
            .context
            .should_prune(&self.messages, &self.tools.schemas())
        {
            self.prune_old_tool_results();
        }

        let ask = user_input.to_string();
        self.current_ask = ask.clone();
        let turn_start = self.messages.len();
        let digest_end = turn_start.saturating_sub(1);
        self.unpersisted.clear();
        self.drafting = self
            .scrutiny
            .as_ref()
            .is_some_and(|s| s.answers != crate::config::AnswerScrutiny::Off);

        let LoopOutcome {
            result,
            mut final_text,
            did_work,
            iterations,
        } = self.run_loop().await;

        if self.drafting {
            if result.is_ok() && !self.cancel.is_cancelled() {
                final_text = self
                    .scrutinize_answer(&ask, final_text, did_work, turn_start, digest_end)
                    .await;
            } else {
                self.finalize_drafts(None);
            }
            self.drafting = false;
            if !final_text.trim().is_empty() {
                self.events.send(AgentEvent::AssistantText {
                    delta: final_text.clone(),
                });
            }
        }

        // Iterate verification INSIDE the turn (before TurnFinished), so the
        // corrective passes stream normally and the queue confirm that follows
        // sees a fully-certified result.
        if did_work && result.is_ok() && !self.cancel.is_cancelled() {
            self.iterate_verification(&ask, &mut final_text, Some(turn_start))
                .await;
        }

        self.events.send(AgentEvent::TurnFinished { turn_id });
        let completed = matches!(result, Ok(()) | Err(AgentError::IterationLimit(_)));
        if completed {
            if let Some(brainbox) = &mut self.brainbox {
                brainbox.note_turn(&self.messages);
            }
            if let Some(lessons) = &mut self.lessons {
                lessons.note_turn(&self.messages);
            }
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

    /// The model/tool loop for a single pass (initial turn or a verify fix
    /// pass). Streams events and executes tools until the model stops calling
    /// them or a limit trips. Does NOT emit TurnStarted/TurnFinished — the
    /// caller frames the turn.
    async fn run_loop(&mut self) -> LoopOutcome {
        let mut did_work = false;
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
            let (model_result, mut early) = self.call_model_dispatching(use_format_fallback).await;
            let (text, mut tool_calls, _stop) = match model_result {
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
            if self.drafting && tool_calls.is_empty() {
                // The draft answer: held back from the session until the
                // critic releases it (or a revision replaces it).
                self.push_unpersisted(assistant);
            } else {
                self.push_message(assistant);
            }
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
            // Edit/Execute calls (the sequential set) mean the turn did real
            // work — the gate for auto-verification.
            if !sequential.is_empty() {
                did_work = true;
            }

            // Calls already started mid-stream (early dispatch) reuse their
            // result; the rest run now.
            let mut results: Vec<(usize, Message)> = if self.cancel.is_cancelled() {
                Vec::new()
            } else {
                futures::future::join_all(concurrent.iter().map(|(i, call)| {
                    let agent = &*self;
                    let done = early.remove(&call.id);
                    async move {
                        match done {
                            Some(msg) => (*i, msg),
                            None => (*i, agent.execute_call(call).await),
                        }
                    }
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

        LoopOutcome {
            result,
            final_text,
            did_work,
            iterations,
        }
    }

    /// A message that is part of the history now but reaches the session
    /// only if scrutiny keeps it (see [`Self::finalize_drafts`]).
    fn push_unpersisted(&mut self, message: Message) {
        self.unpersisted
            .push((message.role, message.content.clone()));
        self.messages.push(message);
        self.msg_seqs.push(0);
    }

    /// Settle this turn's held-back messages. With `keep = Some(answer)`,
    /// every held draft and corrective prompt leaves the history except the
    /// last assistant message whose text is `answer`, which is persisted.
    /// With `None`, everything held is persisted where it stands.
    fn finalize_drafts(&mut self, keep: Option<&str>) {
        let mut pending = std::mem::take(&mut self.unpersisted);
        if pending.is_empty() {
            return;
        }
        // Match held messages from the end of the history: they are the
        // newest seq-0 messages with exactly that role and content.
        let mut held: Vec<usize> = Vec::new();
        for i in (0..self.messages.len()).rev() {
            if pending.is_empty() {
                break;
            }
            if self.msg_seqs[i] != 0 {
                continue;
            }
            let m = &self.messages[i];
            if let Some(pos) = pending
                .iter()
                .rposition(|(role, content)| *role == m.role && *content == m.content)
            {
                pending.remove(pos);
                held.push(i);
            }
        }
        held.sort_unstable();
        let kept = keep.and_then(|answer| {
            held.iter().rev().copied().find(|&i| {
                let m = &self.messages[i];
                m.role == Role::Assistant && m.tool_calls.is_empty() && m.content == answer
            })
        });
        let persist = |agent: &mut Self, i: usize| {
            if let Some(store) = &mut agent.session {
                agent.msg_seqs[i] = store
                    .append_message(&agent.messages[i])
                    .unwrap_or_else(|e| {
                        tracing::error!(error = %e, "failed to persist message");
                        0
                    });
            }
        };
        match keep {
            None => {
                for &i in &held {
                    persist(self, i);
                }
            }
            Some(_) => {
                for &i in held.iter().rev() {
                    if Some(i) != kept {
                        self.messages.remove(i);
                        self.msg_seqs.remove(i);
                    }
                }
                if let Some(k) = kept {
                    let removed_before = held.iter().filter(|&&i| i < k).count();
                    persist(self, k - removed_before);
                }
            }
        }
    }

    /// The critic for this agent's current model: scrutiny pins its own,
    /// otherwise the main model reviews itself.
    fn critic_target(&self) -> CriticTarget {
        CriticTarget {
            provider: Arc::clone(&self.provider),
            model: self.settings.model.clone(),
            params: self.settings.params.clone(),
        }
    }

    /// Forward a critic exchange's tokens and notice to the frontend.
    fn report_critic(&self, reply: &scrutiny::CriticReply) {
        if reply.usage.prompt_tokens + reply.usage.completion_tokens > 0 {
            self.events.send(AgentEvent::AuxUsage(reply.usage));
        }
        if let Some(message) = &reply.notice {
            self.events.send(AgentEvent::Notice {
                message: message.clone(),
            });
        }
    }

    /// The user's standing rules (the LESSONS section of the system prompt);
    /// the critic treats a violation as a defect. PILOT.md is left out.
    fn lessons_text(&self) -> String {
        let Some(sys) = self.messages.first().filter(|m| m.role == Role::System) else {
            return String::new();
        };
        let Some(start) = sys.content.find(crate::prompt::LESSONS_SECTION_MARKER) else {
            return String::new();
        };
        let rest = &sys.content[start + crate::prompt::LESSONS_SECTION_MARKER.len()..];
        let end = rest
            .find(crate::prompt::BRAINBOX_SECTION_MARKER)
            .unwrap_or(rest.len());
        let body = &rest[..end];
        // Drop the marker line's tail (" — always follow these):").
        body.split_once('\n')
            .map(|(_, b)| b)
            .unwrap_or("")
            .trim()
            .to_string()
    }

    /// First lines of this turn's tool results, as pruning would stub them.
    fn tool_heads(&self, turn_start: usize) -> String {
        let mut names: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        let slice = &self.messages[turn_start.min(self.messages.len())..];
        for m in slice {
            for c in &m.tool_calls {
                names.insert(c.id.as_str(), c.name.as_str());
            }
        }
        slice
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| {
                let name = m
                    .tool_call_id
                    .as_deref()
                    .and_then(|id| names.get(id).copied());
                ContextManager::prune_stub(name, &m.content)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Review the draft answer; on findings run a hidden corrective pass and
    /// review again, up to `max_revisions`. Returns the answer to release.
    /// The draft, the findings and the corrective prompts never stay in the
    /// history; tool exchanges from a revision do.
    async fn scrutinize_answer(
        &mut self,
        ask: &str,
        draft: String,
        did_work: bool,
        turn_start: usize,
        digest_end: usize,
    ) -> String {
        let Some(scrutiny) = self.scrutiny.clone() else {
            self.finalize_drafts(None);
            return draft;
        };
        let plan = self.settings.mode == crate::config::Mode::Plan;
        if !scrutiny.should_review_answer(&draft, did_work, plan) {
            self.finalize_drafts(None);
            return draft;
        }
        self.events.send(AgentEvent::ScrutinyStarted {
            kind: ScrutinyKind::Answer,
        });
        let digest =
            scrutiny::conversation_digest(&self.messages[..digest_end.min(self.messages.len())]);
        let lessons = self.lessons_text();
        let mut current = draft;
        let mut revisions = 0u32;
        let mut findings = String::new();
        let approved = loop {
            let input = ReviewInput {
                ask: ask.to_string(),
                draft: current.clone(),
                work_summary: self.work_summary(turn_start),
                digest: digest.clone(),
                tool_heads: self.tool_heads(turn_start),
                lessons: lessons.clone(),
            };
            let target = self.critic_target();
            let review = tokio::select! {
                r = scrutiny.review_answer(&target, &input) => r,
                () = self.cancel.cancelled() => break false,
            };
            let (verdict, reply) = review;
            self.report_critic(&reply);
            let Verdict::Revise(found) = verdict else {
                break true;
            };
            findings = found;
            if revisions >= scrutiny.max_revisions || self.cancel.is_cancelled() {
                break false;
            }
            revisions += 1;
            self.push_unpersisted(Message::user(scrutiny::answer_corrective_prompt(&findings)));
            let outcome = self.run_loop().await;
            let revised = outcome.final_text;
            if matches!(outcome.result, Err(AgentError::Cancelled)) {
                break false;
            }
            if revised.trim().is_empty() {
                break false;
            }
            if revised == current {
                // Nothing changed: release as is.
                break false;
            }
            current = revised;
            if outcome.result.is_err() {
                break false;
            }
        };
        self.finalize_drafts(Some(&current));
        self.events.send(AgentEvent::ScrutinyVerdict {
            kind: ScrutinyKind::Answer,
            approved,
            revisions,
            findings,
        });
        current
    }

    /// Ask the checker whether the work satisfies `ask`; while it finds gaps,
    /// feed them back as a corrective user turn and re-run — up to the
    /// scrutiny's `max_iterations`, then certify done. A first-pass OK stops
    /// immediately with zero modifications. Emits `VerificationAttempt` per
    /// retry and one final `VerificationReport`. `final_text` is updated with
    /// each fix pass's summary. Cancellable via `self.cancel`.
    ///
    /// `turn_start`: message index the turn's work began at (for the work
    /// summary), or `None` for an on-demand `/verify` with no prior actions.
    async fn iterate_verification(
        &mut self,
        ask: &str,
        final_text: &mut String,
        turn_start: Option<usize>,
    ) {
        let Some(scrutiny) = self.scrutiny.clone() else {
            return;
        };
        if scrutiny.work || turn_start.is_none() {
            self.events.send(AgentEvent::ScrutinyStarted {
                kind: ScrutinyKind::Work,
            });
            let max = scrutiny.max_iterations;
            let mut mods = 0u32;
            loop {
                let work_summary = match turn_start {
                    Some(s) => self.work_summary(s),
                    None => "(on-demand /verify)".to_string(),
                };
                let target = self.critic_target();
                let (ok, findings, reply) = tokio::select! {
                    r = scrutiny.check(&target, ask, final_text, &work_summary) => r,
                    () = self.cancel.cancelled() => break,
                };
                self.report_critic(&reply);
                if ok {
                    self.events.send(AgentEvent::VerificationReport {
                        ok: true,
                        findings: String::new(),
                    });
                    break;
                }
                if mods >= max {
                    // Certified with residual gaps: retries exhausted.
                    self.events.send(AgentEvent::VerificationReport {
                        ok: false,
                        findings,
                    });
                    break;
                }
                if self.cancel.is_cancelled() {
                    break;
                }
                mods += 1;
                self.events
                    .send(AgentEvent::VerificationAttempt { attempt: mods, max });
                self.push_message(Message::user(corrective_prompt(&findings)));
                let outcome = self.run_loop().await;
                if !outcome.final_text.is_empty() {
                    *final_text = outcome.final_text;
                }
                if matches!(outcome.result, Err(AgentError::Cancelled)) {
                    break;
                }
            }
        }
    }

    /// A one-line-per-tool summary of the actions taken since `turn_start`,
    /// for the verifier ("[edit] path", "[bash] cmd …").
    fn work_summary(&self, turn_start: usize) -> String {
        let mut out = String::new();
        for msg in &self.messages[turn_start.min(self.messages.len())..] {
            for call in &msg.tool_calls {
                let arg = call.arguments.to_string();
                let arg: String = arg.chars().take(100).collect();
                out.push_str(&format!("[{}] {arg}\n", call.name));
            }
        }
        if out.is_empty() {
            out.push_str("(no tool actions recorded)");
        }
        out
    }

    /// Stub tool results older than the last `keep_tool_turns` user turns.
    /// The full text stays in the session JSONL; each stub is persisted as a
    /// single-seq Compaction record so a resumed session sees the pruned
    /// view. Deterministic and idempotent; runs only when `should_prune`
    /// says there's context pressure, so all aged results are stubbed in one
    /// batch — mid-history rewrites invalidate Ollama's KV prefix cache, so
    /// they must be rare.
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
        let transcript = ContextManager::render_summary_transcript(old);
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
        let scheduler = self.scheduler.clone();
        tokio::spawn(async move {
            let _release = ReleaseFlag(flag);
            let prompt = ContextManager::summarize_prompt(&original_goal, &transcript);
            match one_shot_call(
                provider,
                model,
                params,
                SUMMARIZER_SYSTEM,
                &prompt,
                scheduler.as_ref(),
            )
            .await
            {
                Ok(OneShot { text: summary, .. }) => {
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
        let transcript = ContextManager::render_summary_transcript(old);
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
        one_shot_call(
            provider,
            model,
            params,
            system,
            user,
            self.scheduler.as_ref(),
        )
        .await
        .map(|out| out.text)
    }

    /// One streaming model call; returns (text, tool calls, stop reason).
    /// With `constrained`, requests JSON-schema output shaped as
    /// {"tool_calls": [...]} — the repair pipeline's last resort — and does
    /// not stream the raw JSON to the UI.
    /// One model call with early dispatch: every complete, valid
    /// read-only or `task` call starts while the model is still emitting
    /// the rest of the message. Returns the call's outcome and the results
    /// of the calls already run, by call id.
    async fn call_model_dispatching(
        &self,
        constrained: bool,
    ) -> (
        Result<(String, Vec<ToolCall>, StopReason), AgentError>,
        std::collections::HashMap<String, Message>,
    ) {
        if constrained {
            return (self.call_model(true, None).await, Default::default());
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ToolCall>();
        let model = self.call_model(false, Some(tx));
        let runner = async {
            let mut running = futures::stream::FuturesUnordered::new();
            let mut done = std::collections::HashMap::new();
            let mut seen = std::collections::HashSet::new();
            let mut open = true;
            loop {
                tokio::select! {
                    call = rx.recv(), if open => match call {
                        // A repeated id never shares a result; it runs later.
                        Some(call) if self.dispatches_early(&call) && seen.insert(call.id.clone()) => {
                            running.push(async move {
                                let msg = self.execute_call(&call).await;
                                (call.id, msg)
                            });
                        }
                        Some(_) => {}
                        None => open = false,
                    },
                    Some((id, msg)) = running.next(), if !running.is_empty() => {
                        done.insert(id, msg);
                    }
                    else => break,
                }
            }
            done
        };
        futures::join!(model, runner)
    }

    /// Safe to start before the message ends: a known read-only or spawn
    /// tool whose arguments validate.
    fn dispatches_early(&self, call: &ToolCall) -> bool {
        !self.cancel.is_cancelled()
            && self
                .tools
                .get(&call.name)
                .is_some_and(|t| matches!(t.kind(), ToolKind::ReadOnly | ToolKind::Spawn))
            && matches!(repair::validate_call(&self.tools, call), Validation::Ok)
    }

    async fn call_model(
        &self,
        constrained: bool,
        early: Option<tokio::sync::mpsc::UnboundedSender<ToolCall>>,
    ) -> Result<(String, Vec<ToolCall>, StopReason), AgentError> {
        // Held until the stream ends, errors, or is cancelled.
        let _slot = self.acquire_slot().await?;
        let format = constrained.then(|| self.tool_call_format_schema());
        let req = ChatRequest {
            model: self.settings.model.clone(),
            messages: self.messages.clone(),
            tools: self.tools.schemas(),
            params: self.settings.params.clone(),
            format,
        };
        // Cancellable while waiting for the response to start, too: a slow
        // or queued backend must not make Esc wait for its first byte.
        let opened = tokio::select! {
            r = self.provider.chat(req) => r,
            () = self.cancel.cancelled() => return Err(AgentError::Cancelled),
        };
        let mut stream = match opened {
            Ok(stream) => stream,
            Err(e) => {
                self.note_rejection(&e);
                return Err(e.into());
            }
        };

        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut stop = StopReason::EndTurn;

        loop {
            tokio::select! {
                delta = stream.next() => match delta {
                    None => break,
                    Some(Err(e)) => {
                        self.note_rejection(&e);
                        return Err(e.into());
                    }
                    Some(Ok(ChatDelta::Text(t))) => {
                        if self.drafting {
                            self.events.send(AgentEvent::DraftText { delta: t.clone() });
                        } else if !constrained {
                            self.events.send(AgentEvent::AssistantText { delta: t.clone() });
                        }
                        text.push_str(&t);
                    }
                    // Display-only: never enters `messages` or the session.
                    Some(Ok(ChatDelta::Thinking(t))) => {
                        self.events.send(AgentEvent::Thinking { delta: t });
                    }
                    Some(Ok(ChatDelta::ToolCall(call))) => {
                        if let Some(tx) = &early {
                            let _ = tx.send(call.clone());
                        }
                        calls.push(call);
                    }
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

    /// A slot in this model's scheduler lane. When none is free, say so once
    /// (task cards and the crew roster show it) and wait; Esc still cancels.
    async fn acquire_slot(&self) -> Result<Option<Permit>, AgentError> {
        let Some(scheduler) = &self.scheduler else {
            return Ok(None);
        };
        let model = self.model_ref();
        if let Some(permit) = scheduler.try_acquire(&model) {
            return Ok(Some(permit));
        }
        self.events.send(AgentEvent::ToolProgress {
            call_id: SLOT_WAIT_TAG.into(),
            chunk: super::events::SLOT_WAIT_TEXT.into(),
        });
        tokio::select! {
            permit = scheduler.acquire(&model) => Ok(Some(permit)),
            () = self.cancel.cancelled() => Err(AgentError::Cancelled),
        }
    }

    fn note_rejection(&self, e: &ProviderError) {
        if let (Some(scheduler), ProviderError::Api { status, .. }) = (&self.scheduler, e) {
            scheduler.note_rejected(&self.model_ref(), *status);
        }
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
            mode: self.settings.mode,
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
            Decision::Allow => {
                if let Some(blocked) = self
                    .review_risky_action(tool.as_ref(), call, &summary, &ctx)
                    .await
                {
                    return blocked;
                }
            }
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

    /// Pre-action scrutiny (`[scrutiny] actions = "risky"`): in auto mode,
    /// a bash command no allow rule covers, or an edit/write longer than
    /// `risky_edit_lines`, is shown to the critic before it runs. A REVISE
    /// verdict blocks the call and returns the findings as its result.
    async fn review_risky_action(
        &self,
        tool: &dyn crate::tools::Tool,
        call: &ToolCall,
        summary: &str,
        ctx: &ToolCtx,
    ) -> Option<Message> {
        let scrutiny = self.scrutiny.as_ref()?;
        if scrutiny.actions != crate::config::ActionScrutiny::Risky
            || self.settings.mode != crate::config::Mode::Auto
        {
            return None;
        }
        let risky = match tool.name() {
            "bash" => !self.permissions.allow_rule_matches("bash", &call.arguments),
            "edit" | "write" => edit_line_count(&call.arguments) > scrutiny.risky_edit_lines,
            _ => false,
        };
        if !risky {
            return None;
        }
        self.events.send(AgentEvent::ScrutinyStarted {
            kind: ScrutinyKind::Action,
        });
        let detail = tool.preview(&call.arguments, ctx).await;
        let target = self.critic_target();
        let (verdict, reply) = tokio::select! {
            r = scrutiny.review_action(&target, &self.current_ask, summary, detail.as_deref()) => r,
            () = self.cancel.cancelled() => return None,
        };
        self.report_critic(&reply);
        let (approved, findings) = match verdict {
            Verdict::Approve => (true, String::new()),
            Verdict::Revise(f) => (false, f),
        };
        self.events.send(AgentEvent::ScrutinyVerdict {
            kind: ScrutinyKind::Action,
            approved,
            revisions: 0,
            findings: findings.clone(),
        });
        if approved {
            return None;
        }
        self.events.send(AgentEvent::ToolFinished {
            call_id: call.id.clone(),
            output_preview: "blocked by pre-action review".into(),
            is_error: true,
        });
        Some(Message::tool_result(
            &call.id,
            format!(
                "Blocked by pre-action review before running:\n{findings}\nChange the action to address this, or ask the user how to proceed."
            ),
        ))
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

/// Lines an edit or write touches: the larger side of an edit, or the whole
/// written content.
fn edit_line_count(args: &serde_json::Value) -> usize {
    let lines = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .map_or(0, |s| s.lines().count())
    };
    lines("content")
        .max(lines("old_string"))
        .max(lines("new_string"))
}

const SUMMARIZER_SYSTEM: &str =
    "You summarize coding sessions precisely. Keep exact paths, commands, errors.";

/// A one-shot call's text and the tokens it spent.
pub(crate) struct OneShot {
    pub text: String,
    pub usage: Usage,
}

/// The one-shot body as a free fn so background tasks (proactive compaction,
/// the critic) can call it without borrowing the agent. Holds a scheduler
/// slot for the duration of the call when a scheduler is given.
pub(crate) async fn one_shot_call(
    provider: Arc<dyn Provider>,
    model: String,
    params: GenParams,
    system: &str,
    user: &str,
    scheduler: Option<&Arc<Scheduler>>,
) -> anyhow::Result<OneShot> {
    let model_ref = ModelRef::new(provider.as_ref(), &model, params.num_ctx);
    let _slot = match scheduler {
        Some(s) => Some(s.acquire(&model_ref).await),
        None => None,
    };
    let req = ChatRequest {
        model,
        messages: vec![Message::system(system), Message::user(user)],
        tools: vec![],
        params,
        format: None,
    };
    let reject = |e: ProviderError| {
        if let (Some(s), ProviderError::Api { status, .. }) = (scheduler, &e) {
            s.note_rejected(&model_ref, *status);
        }
        e
    };
    let mut stream = provider.chat(req).await.map_err(reject)?;
    let mut text = String::new();
    let mut usage = Usage::default();
    while let Some(delta) = stream.next().await {
        match delta.map_err(reject)? {
            ChatDelta::Text(t) => text.push_str(&t),
            ChatDelta::Usage(u) => {
                usage.prompt_tokens += u.prompt_tokens;
                usage.completion_tokens += u.completion_tokens;
            }
            ChatDelta::Done(_) => break,
            _ => {}
        }
    }
    Ok(OneShot { text, usage })
}

/// The corrective user message injected when verification finds gaps: the
/// concrete shortfalls plus an instruction to actually fix them this pass.
fn corrective_prompt(findings: &str) -> String {
    format!(
        "A verification check of the work you just finished found these gaps against the original request:\n\n{findings}\n\nFix them now — make the actual code changes, do not just explain. When done, briefly report what you changed."
    )
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
    async fn clear_context_resets_to_system_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::create(dir.path(), "mock").unwrap();
        let path = store.path().to_path_buf();
        let mut agent = agent_with(Arc::new(TextProvider), None, Some(store), 32_768, 0);
        agent.submit("hello").await.unwrap();
        assert!(agent.messages().len() > 1, "turn added messages");

        agent.clear_context(false);
        assert_eq!(agent.messages().len(), 1, "only the system message remains");
        assert_eq!(agent.messages()[0].role, rocinante_providers::Role::System);

        // A Clear record was written; resume starts empty (no system in the
        // reconstructed non-system list).
        drop(agent);
        let (_, resumed) = SessionStore::resume(&path).unwrap();
        assert!(
            resumed.is_empty(),
            "resume after clear is empty: {resumed:?}"
        );
    }

    #[test]
    fn clear_all_strips_brainbox_head_from_system_prompt() {
        let sys = format!(
            "base rules{}\n- goals here\n\nFull project memory is in .rocinante/BRAINBOX.md — read it.",
            crate::prompt::BRAINBOX_SECTION_MARKER
        );
        let permissions = Arc::new(PermissionEngine::from_config(&Default::default()));
        let settings = AgentSettings {
            model: "mock".into(),
            params: GenParams::default(),
            system_prompt: sys,
            cwd: std::env::temp_dir(),
            mode: Mode::Auto,
            max_iterations: 5,
            depth: 0,
            keep_tool_turns: 0,
        };
        let (events, _rx) = tokio::sync::broadcast::channel(16);
        let mut agent = Agent::new(
            Arc::new(TextProvider),
            ToolRegistry::default(),
            permissions,
            settings,
            None,
            events,
            Arc::new(super::super::events::ReplyRouter::default()),
        );
        agent.clear_context(true);
        let sys = &agent.messages()[0].content;
        assert_eq!(sys, "base rules", "brainbox head stripped");
        assert!(!sys.contains("BRAINBOX"));
    }

    #[tokio::test]
    async fn pruning_stubs_old_results_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::create(dir.path(), "mock").unwrap();
        let path = store.path().to_path_buf();
        let provider = Arc::new(ToolPerTurnProvider {
            counter: AtomicUsize::new(0),
        });
        // Sized so the 4th submit sits between the 35% prune threshold and
        // the 60% proactive band: usable = 8192-4096 = 4096 tokens, and
        // 3×1800B of kept tool output ≈ 1.6k tokens ≥ 35% (1434).
        let mut agent = agent_with(provider, Some(1800), Some(store), 8192, 3);
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
            raw.contains(&"x".repeat(1800)),
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

    /// Main model: first call does one Edit tool call (so the turn "did work"),
    /// every later call is plain text — a no-op fix pass.
    struct WorkThenTextProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for WorkThenTextProvider {
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
                vec![
                    Ok(ChatDelta::ToolCall(ToolCall {
                        id: "e0".into(),
                        name: "slow".into(),
                        arguments: serde_json::json!({ "tag": "x" }),
                    })),
                    Ok(ChatDelta::Done(StopReason::ToolUse)),
                ]
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

    /// Checker model: returns each scripted verdict in turn, then "OK".
    struct ScriptedChecker {
        calls: AtomicUsize,
        verdicts: Vec<&'static str>,
    }

    #[async_trait]
    impl Provider for ScriptedChecker {
        fn id(&self) -> &str {
            "checker"
        }
        fn caps(&self) -> Capabilities {
            Capabilities {
                native_tools: false,
                structured_output: false,
                is_local: false,
            }
        }
        async fn chat(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let v = self.verdicts.get(n).copied().unwrap_or("OK");
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(ChatDelta::Text(v.into())),
                Ok(ChatDelta::Usage(Usage {
                    prompt_tokens: 50,
                    completion_tokens: 2,
                })),
                Ok(ChatDelta::Done(StopReason::EndTurn)),
            ])))
        }
        fn count_tokens(&self, _m: &[Message], _t: &[ToolSchema]) -> usize {
            1
        }
    }

    /// Build an Edit-doing agent wired to a checker with the given verdict
    /// script; run one turn and return (attempts, final_ok).
    async fn run_verified_turn(verdicts: Vec<&'static str>) -> (u32, bool) {
        let mut tools = ToolRegistry::default();
        tools.register(Arc::new(SlowTool {
            kind: ToolKind::Edit,
        }));
        let permissions = Arc::new(PermissionEngine::from_config(&Default::default()));
        let settings = AgentSettings {
            model: "mock".into(),
            params: GenParams::default(),
            system_prompt: "sys".into(),
            cwd: std::env::temp_dir(),
            mode: Mode::Auto,
            max_iterations: 5,
            depth: 0,
            keep_tool_turns: 3,
        };
        let (events, mut rx) = tokio::sync::broadcast::channel(256);
        let verifier = scrutiny_with(
            crate::config::ScrutinyConfig {
                answers: crate::config::AnswerScrutiny::Off,
                ..Default::default()
            },
            Some(checker(verdicts)),
        );
        let mut agent = Agent::new(
            Arc::new(WorkThenTextProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            permissions,
            settings,
            None,
            events,
            Arc::new(super::super::events::ReplyRouter::default()),
        )
        .with_scrutiny(verifier);
        agent.submit("do the thing").await.unwrap();

        let mut attempts = 0u32;
        let mut final_ok = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                AgentEvent::VerificationAttempt { .. } => attempts += 1,
                AgentEvent::VerificationReport { ok, .. } => final_ok = ok,
                _ => {}
            }
        }
        (attempts, final_ok)
    }

    #[tokio::test]
    async fn first_pass_ok_does_zero_fixes() {
        let (attempts, ok) = run_verified_turn(vec!["OK"]).await;
        assert_eq!(attempts, 0, "a clean first pass must not iterate");
        assert!(ok);
    }

    #[tokio::test]
    async fn one_gap_then_ok_runs_exactly_one_fix() {
        let (attempts, ok) = run_verified_turn(vec!["- missing a case", "OK"]).await;
        assert_eq!(attempts, 1, "one gap then clean = one fix pass");
        assert!(ok, "final verdict is certified");
    }

    #[tokio::test]
    async fn persistent_gaps_stop_at_max_iterations() {
        let (attempts, ok) =
            run_verified_turn(vec!["- gap", "- gap", "- gap", "- gap", "- gap"]).await;
        assert_eq!(attempts, 3, "capped at max_iterations modifications");
        assert!(!ok, "certified with residual gaps after the cap");
    }

    // ------------------------------------------------ answer scrutiny

    fn checker(verdicts: Vec<&'static str>) -> crate::scrutiny::CriticModel {
        crate::scrutiny::CriticModel {
            provider: Arc::new(ScriptedChecker {
                calls: AtomicUsize::new(0),
                verdicts,
            }),
            model: "checker".into(),
            params: GenParams::default(),
            display: "checker".into(),
        }
    }

    fn scrutiny_with(
        config: crate::config::ScrutinyConfig,
        critic: Option<crate::scrutiny::CriticModel>,
    ) -> Scrutiny {
        Scrutiny::from_config(&config, critic, std::env::temp_dir())
    }

    /// The main model, scripted by the request it sees.
    struct FnProvider<F>(F);

    #[async_trait]
    impl<F> Provider for FnProvider<F>
    where
        F: Fn(&ChatRequest) -> Result<Vec<ChatDelta>, ProviderError> + Send + Sync,
    {
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
            let deltas = (self.0)(&req)?;
            Ok(Box::pin(futures::stream::iter(
                deltas.into_iter().map(Ok).collect::<Vec<_>>(),
            )))
        }
        fn count_tokens(&self, _m: &[Message], _t: &[ToolSchema]) -> usize {
            10
        }
    }

    fn text(t: &str) -> Vec<ChatDelta> {
        vec![
            ChatDelta::Text(t.into()),
            ChatDelta::Done(StopReason::EndTurn),
        ]
    }

    fn call(id: &str) -> Vec<ChatDelta> {
        vec![
            ChatDelta::ToolCall(ToolCall {
                id: id.into(),
                name: "slow".into(),
                arguments: serde_json::json!({ "tag": id }),
            }),
            ChatDelta::Done(StopReason::ToolUse),
        ]
    }

    fn last_is_corrective(req: &ChatRequest) -> bool {
        req.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .is_some_and(|m| m.content.starts_with("A reviewer found"))
    }

    /// First pass: one tool call, then the draft. After the corrective
    /// prompt: another tool call, then `revised`.
    fn reviser(
        revised: &'static str,
    ) -> impl Fn(&ChatRequest) -> Result<Vec<ChatDelta>, ProviderError> {
        move |req| {
            let last = req.messages.last().unwrap();
            Ok(match (last_is_corrective(req), last.role) {
                (false, Role::User) => call("t0"),
                (false, _) => text("draft answer"),
                (true, Role::User) => call("r0"),
                (true, _) => text(revised),
            })
        }
    }

    struct Turn {
        agent: Agent,
        events: Vec<AgentEvent>,
        final_text: String,
        path: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    async fn scrutinized_turn(
        main: Arc<dyn Provider>,
        config: crate::config::ScrutinyConfig,
        critic: Option<crate::scrutiny::CriticModel>,
    ) -> Turn {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::create(dir.path(), "mock").unwrap();
        let path = store.path().to_path_buf();
        let mut tools = ToolRegistry::default();
        tools.register(Arc::new(SlowTool {
            kind: ToolKind::ReadOnly,
        }));
        let permissions = Arc::new(PermissionEngine::from_config(&Default::default()));
        let settings = AgentSettings {
            model: "mock".into(),
            params: GenParams::default(),
            system_prompt: "sys".into(),
            cwd: std::env::temp_dir(),
            mode: Mode::Auto,
            max_iterations: 8,
            depth: 0,
            keep_tool_turns: 3,
        };
        let (events, mut rx) = tokio::sync::broadcast::channel(1024);
        let mut agent = Agent::new(
            main,
            tools,
            permissions,
            settings,
            Some(store),
            events,
            Arc::new(super::super::events::ReplyRouter::default()),
        )
        .with_scrutiny(scrutiny_with(config, critic));
        let turn = agent.submit("explain the module").await.unwrap();
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        Turn {
            agent,
            events,
            final_text: turn.final_text,
            path,
            _dir: dir,
        }
    }

    fn all_answers() -> crate::config::ScrutinyConfig {
        crate::config::ScrutinyConfig {
            answers: crate::config::AnswerScrutiny::All,
            work: false,
            ..Default::default()
        }
    }

    fn released(events: &[AgentEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::AssistantText { delta } => Some(delta.clone()),
                _ => None,
            })
            .collect()
    }

    fn verdict(events: &[AgentEvent]) -> (bool, u32, String) {
        events
            .iter()
            .find_map(|e| match e {
                AgentEvent::ScrutinyVerdict {
                    kind: ScrutinyKind::Answer,
                    approved,
                    revisions,
                    findings,
                } => Some((*approved, *revisions, findings.clone())),
                _ => None,
            })
            .expect("a verdict")
    }

    fn shape(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .map(|m| match m.role {
                Role::System => "sys".to_string(),
                Role::User => format!("user:{}", m.content),
                Role::Tool => format!("tool:{}", m.tool_call_id.as_deref().unwrap_or("")),
                Role::Assistant if !m.tool_calls.is_empty() => {
                    format!("call:{}", m.tool_calls[0].id)
                }
                Role::Assistant => format!("answer:{}", m.content),
            })
            .collect()
    }

    #[tokio::test]
    async fn approve_releases_the_draft_unchanged() {
        let t = scrutinized_turn(
            Arc::new(FnProvider(reviser("unused"))),
            all_answers(),
            Some(checker(vec!["APPROVE"])),
        )
        .await;
        assert_eq!(t.final_text, "draft answer");
        assert_eq!(
            released(&t.events),
            vec!["draft answer"],
            "released once, whole"
        );
        assert!(
            t.events
                .iter()
                .any(|e| matches!(e, AgentEvent::DraftText { delta } if delta == "draft answer")),
            "the draft streamed as DraftText"
        );
        assert_eq!(verdict(&t.events), (true, 0, String::new()));
        assert_eq!(
            shape(t.agent.messages()),
            [
                "sys",
                "user:explain the module",
                "call:t0",
                "tool:t0",
                "answer:draft answer"
            ]
        );
    }

    #[tokio::test]
    async fn revise_runs_one_hidden_turn_and_keeps_only_real_context() {
        let t = scrutinized_turn(
            Arc::new(FnProvider(reviser("fixed answer"))),
            all_answers(),
            Some(checker(vec!["REVISE\n- the path is wrong", "APPROVE"])),
        )
        .await;
        assert_eq!(t.final_text, "fixed answer");
        assert_eq!(released(&t.events), vec!["fixed answer"]);
        assert_eq!(verdict(&t.events), (true, 1, "- the path is wrong".into()));
        let expected = [
            "sys",
            "user:explain the module",
            "call:t0",
            "tool:t0",
            "call:r0",
            "tool:r0",
            "answer:fixed answer",
        ];
        assert_eq!(shape(t.agent.messages()), expected);
        // The session records exactly what the history holds.
        let raw = std::fs::read_to_string(&t.path).unwrap();
        assert!(!raw.contains("draft answer"), "draft never persisted");
        assert!(
            !raw.contains("A reviewer found"),
            "corrective prompt never persisted"
        );
        let (_, resumed) = SessionStore::resume(&t.path).unwrap();
        assert_eq!(shape(&resumed), expected);
        // Critic tokens count toward totals, not context fill.
        let aux: u64 = t
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::AuxUsage(u) => Some(u.prompt_tokens),
                _ => None,
            })
            .sum();
        assert_eq!(aux, 100, "two critic calls × 50");
    }

    #[tokio::test]
    async fn max_revisions_caps_the_rewrites() {
        let t = scrutinized_turn(
            Arc::new(FnProvider(reviser("second try"))),
            all_answers(),
            Some(checker(vec!["- still wrong"; 5])),
        )
        .await;
        assert_eq!(
            t.final_text, "second try",
            "the revision is released anyway"
        );
        let (approved, revisions, findings) = verdict(&t.events);
        assert!(!approved);
        assert_eq!(revisions, 1, "default max_revisions = 1");
        assert!(findings.contains("still wrong"));
        assert_eq!(
            t.agent
                .messages()
                .iter()
                .filter(|m| m.role == Role::User)
                .count(),
            1,
            "no corrective prompt left behind"
        );
    }

    #[tokio::test]
    async fn identical_revision_is_released() {
        let same = |req: &ChatRequest| -> Result<Vec<ChatDelta>, ProviderError> {
            let _ = req;
            Ok(text("same answer"))
        };
        let t = scrutinized_turn(
            Arc::new(FnProvider(same)),
            all_answers(),
            Some(checker(vec!["- nit", "- nit"])),
        )
        .await;
        assert_eq!(t.final_text, "same answer");
        assert_eq!(verdict(&t.events).1, 1);
        assert_eq!(
            shape(t.agent.messages()),
            ["sys", "user:explain the module", "answer:same answer"]
        );
    }

    #[tokio::test]
    async fn unreachable_critic_releases_with_one_notice() {
        // Self-scrutiny: the main model fails only for the critic's request.
        let main = |req: &ChatRequest| -> Result<Vec<ChatDelta>, ProviderError> {
            if req.messages[0]
                .content
                .starts_with("You review a draft answer")
            {
                return Err(ProviderError::Api {
                    status: 503,
                    message: "overloaded".into(),
                });
            }
            Ok(text("plain answer"))
        };
        let t = scrutinized_turn(Arc::new(FnProvider(main)), all_answers(), None).await;
        assert_eq!(t.final_text, "plain answer");
        assert_eq!(released(&t.events), vec!["plain answer"]);
        let notices: Vec<&AgentEvent> = t
            .events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Notice { .. }))
            .collect();
        assert_eq!(notices.len(), 1, "{notices:?}");
        let raw = std::fs::read_to_string(&t.path).unwrap();
        assert!(raw.contains("plain answer"), "released answer persisted");
    }

    #[tokio::test]
    async fn short_answers_skip_review_under_substantial() {
        let config = crate::config::ScrutinyConfig {
            work: false,
            ..Default::default()
        };
        let t = scrutinized_turn(
            Arc::new(FnProvider(|_: &ChatRequest| Ok(text("short")))),
            config,
            Some(checker(vec!["- would revise"])),
        )
        .await;
        assert_eq!(t.final_text, "short");
        assert!(
            !t.events
                .iter()
                .any(|e| matches!(e, AgentEvent::ScrutinyStarted { .. })),
            "not reviewed"
        );
        let raw = std::fs::read_to_string(&t.path).unwrap();
        assert!(raw.contains("short"), "the unreviewed answer is persisted");
    }

    /// A tool named `bash` (Execute) for pre-action review.
    struct FakeBash;

    #[async_trait]
    impl Tool for FakeBash {
        fn name(&self) -> &'static str {
            "bash"
        }
        fn description(&self) -> &'static str {
            "fake"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": { "command": { "type": "string" } } })
        }
        fn kind(&self) -> ToolKind {
            ToolKind::Execute
        }
        fn describe_call(&self, args: &serde_json::Value) -> String {
            format!("bash: {}", args["command"].as_str().unwrap_or(""))
        }
        async fn run(&self, _args: serde_json::Value, _ctx: &ToolCtx) -> ToolOutput {
            ToolOutput::ok("ran")
        }
    }

    #[tokio::test]
    async fn risky_action_review_blocks_on_revise() {
        let main = |req: &ChatRequest| -> Result<Vec<ChatDelta>, ProviderError> {
            Ok(match req.messages.last().unwrap().role {
                Role::User => vec![
                    ChatDelta::ToolCall(ToolCall {
                        id: "b0".into(),
                        name: "bash".into(),
                        arguments: serde_json::json!({ "command": "rm -rf ./data" }),
                    }),
                    ChatDelta::Done(StopReason::ToolUse),
                ],
                _ => text("done"),
            })
        };
        let mut tools = ToolRegistry::default();
        tools.register(Arc::new(FakeBash));
        let settings = AgentSettings {
            model: "mock".into(),
            params: GenParams::default(),
            system_prompt: "sys".into(),
            cwd: std::env::temp_dir(),
            mode: Mode::Auto,
            max_iterations: 5,
            depth: 0,
            keep_tool_turns: 3,
        };
        let config = crate::config::ScrutinyConfig {
            answers: crate::config::AnswerScrutiny::Off,
            work: false,
            actions: crate::config::ActionScrutiny::Risky,
            ..Default::default()
        };
        let (events, _rx) = tokio::sync::broadcast::channel(256);
        let mut agent = Agent::new(
            Arc::new(FnProvider(main)),
            tools,
            Arc::new(PermissionEngine::from_config(&Default::default())),
            settings,
            None,
            events,
            Arc::new(super::super::events::ReplyRouter::default()),
        )
        .with_scrutiny(scrutiny_with(
            config,
            Some(checker(vec![
                "REVISE\n- deletes data the ask did not mention",
            ])),
        ));
        agent.submit("tidy the logs").await.unwrap();
        let result = agent
            .messages()
            .iter()
            .find(|m| m.role == Role::Tool)
            .unwrap();
        assert!(
            result.content.contains("Blocked by pre-action review")
                && result.content.contains("deletes data"),
            "{}",
            result.content
        );
    }

    // ------------------------------------------------ scheduler

    fn sched() -> Arc<Scheduler> {
        let config = crate::config::load_from(
            std::path::Path::new("/nonexistent/a.toml"),
            std::path::Path::new("/nonexistent/b.toml"),
        )
        .unwrap();
        Scheduler::new(&config)
    }

    #[tokio::test]
    async fn slot_is_released_when_the_stream_errors() {
        let failing = |_: &ChatRequest| -> Result<Vec<ChatDelta>, ProviderError> {
            Err(ProviderError::Api {
                status: 500,
                message: "boom".into(),
            })
        };
        let scheduler = sched();
        let mut agent = agent_with(Arc::new(FnProvider(failing)), None, None, 32_768, 0)
            .with_scheduler(Arc::clone(&scheduler));
        let m = agent.model_ref();
        let budget = scheduler.budget(&m);
        assert!(agent.submit("hi").await.is_err());
        assert_eq!(
            scheduler.available(&m),
            budget,
            "permit returned after the error"
        );
    }

    #[test]
    fn model_switch_rewrites_the_parallelism_line() {
        let scheduler = sched();
        let sys = format!(
            "rules\nModels available for delegation:\n- x\n{}",
            scheduler.parallelism_note(
                &crate::scheduler::ModelRef {
                    provider: "mock".into(),
                    ollama: false,
                    model: "mock".into(),
                    num_ctx: None,
                },
                None
            )
        );
        let permissions = Arc::new(PermissionEngine::from_config(&Default::default()));
        let settings = AgentSettings {
            model: "mock".into(),
            params: GenParams::default(),
            system_prompt: sys,
            cwd: std::env::temp_dir(),
            mode: Mode::Auto,
            max_iterations: 5,
            depth: 0,
            keep_tool_turns: 0,
        };
        let (events, _rx) = tokio::sync::broadcast::channel(64);
        let mut agent = Agent::new(
            Arc::new(TextProvider),
            ToolRegistry::default(),
            permissions,
            settings,
            None,
            events,
            Arc::new(super::super::events::ReplyRouter::default()),
        )
        .with_scheduler(Arc::clone(&scheduler));
        assert!(
            agent.messages()[0].content.contains("up to 3 subagents"),
            "provider lane: 4 - 1"
        );
        // Switch to a local Ollama model: one slot, one at a time.
        let ollama: Arc<dyn Provider> = Arc::new(rocinante_providers::ollama::OllamaProvider::new(
            "ollama",
            "http://127.0.0.1:1",
        ));
        agent.set_model(
            ollama,
            "qwen3:8b".into(),
            "qwen3:8b".into(),
            GenParams::default(),
        );
        let sys = &agent.messages()[0].content;
        assert!(
            sys.contains("one at a time") && sys.contains("qwen3:8b"),
            "{sys}"
        );
        assert_eq!(
            sys.matches("Parallelism: ").count(),
            1,
            "replaced, not appended"
        );
        assert_eq!(scheduler.main_stats().unwrap().0, "local");
    }

    // ------------------------------------------------ early dispatch

    /// Emits t0, waits 300ms (still "generating"), emits t1, ends.
    struct SlowStreamProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for SlowStreamProvider {
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
            if n > 0 {
                return Ok(Box::pin(futures::stream::iter(vec![
                    Ok(ChatDelta::Text("done".into())),
                    Ok(ChatDelta::Done(StopReason::EndTurn)),
                ])));
            }
            let call = |id: &str, ms: u64| {
                ChatDelta::ToolCall(ToolCall {
                    id: id.into(),
                    name: "timed".into(),
                    arguments: serde_json::json!({ "ms": ms }),
                })
            };
            let script = vec![
                (0u64, call("t0", 500)),
                (300, call("t1", 100)),
                (0, ChatDelta::Done(StopReason::ToolUse)),
            ];
            Ok(Box::pin(futures::stream::iter(script).then(
                |(wait, d)| async move {
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                    Ok(d)
                },
            )))
        }
        fn count_tokens(&self, _m: &[Message], _t: &[ToolSchema]) -> usize {
            10
        }
    }

    /// Sleeps `ms`, counts its runs.
    struct TimedTool {
        runs: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for TimedTool {
        fn name(&self) -> &'static str {
            "timed"
        }
        fn description(&self) -> &'static str {
            "sleeps"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object", "properties": { "ms": { "type": "integer" } } })
        }
        fn kind(&self) -> ToolKind {
            ToolKind::ReadOnly
        }
        fn describe_call(&self, _args: &serde_json::Value) -> String {
            "timed".into()
        }
        async fn run(&self, args: serde_json::Value, _ctx: &ToolCtx) -> ToolOutput {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let ms = args["ms"].as_u64().unwrap_or(0);
            tokio::time::sleep(Duration::from_millis(ms)).await;
            ToolOutput::ok(format!("slept {ms}"))
        }
    }

    #[tokio::test]
    async fn read_only_calls_start_while_the_model_still_streams() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut tools = ToolRegistry::default();
        tools.register(Arc::new(TimedTool {
            runs: Arc::clone(&runs),
        }));
        let settings = AgentSettings {
            model: "mock".into(),
            params: GenParams::default(),
            system_prompt: "sys".into(),
            cwd: std::env::temp_dir(),
            mode: Mode::Auto,
            max_iterations: 5,
            depth: 0,
            keep_tool_turns: 3,
        };
        let (events, _rx) = tokio::sync::broadcast::channel(256);
        let mut agent = Agent::new(
            Arc::new(SlowStreamProvider {
                calls: AtomicUsize::new(0),
            }),
            tools,
            Arc::new(PermissionEngine::from_config(&Default::default())),
            settings,
            None,
            events,
            Arc::new(super::super::events::ReplyRouter::default()),
        );
        let start = Instant::now();
        agent.submit("go").await.unwrap();
        let elapsed = start.elapsed();
        // Sequential after the stream: 300ms + 500ms. Early: t0 runs during
        // the 300ms the model keeps streaming, so ~500ms.
        assert!(elapsed < Duration::from_millis(700), "took {elapsed:?}");
        assert_eq!(runs.load(Ordering::SeqCst), 2, "each call ran exactly once");
        let results: Vec<(&str, &str)> = agent
            .messages()
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| (m.tool_call_id.as_deref().unwrap(), m.content.as_str()))
            .collect();
        assert_eq!(
            results,
            vec![("t0", "slept 500"), ("t1", "slept 100")],
            "original order"
        );
    }
}
