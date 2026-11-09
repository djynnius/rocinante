//! The crew: persistent subagents. A member is a long-lived `Agent` built
//! from its `[agents.<name>]` profile, spawned lazily on its first `task`
//! delegation or first `@name` message, that keeps its context between
//! messages. Each member owns an inbox and runs one turn at a time;
//! messages that arrive while it works queue in order. Status is published
//! from the member's own event stream (never by asking the model) as
//! `CrewEvent`s on the frontend channel.
//!
//! Manager pattern (CONTRIBUTING): session-lifetime handle held by the
//! frontends and the `task` tool, lazy spawn, idle reaping, and members end
//! when the manager is dropped (their inboxes close).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::events::{
    Addr, AgentEvent, CrewEvent, EventSender, MemberStatus, ReplyRouter, SLOT_WAIT_TAG,
};
use crate::agent::subagent::{
    EMPTY_REPORT_NUDGE, build_subagent, main_model_ref, model_ref_of, profile_writes,
    relay_progress, resolve_subagent_model,
};
use crate::agent::{Agent, Interrupter};
use crate::config::Config;
use crate::permissions::PermissionEngine;
use crate::provider_factory;
use crate::scheduler::Scheduler;

/// Asked of a member by `@name report`: its summary goes to the main agent.
pub const REPORT_PROMPT: &str = "Summarize for the main agent, in a few bullet points, what you found or did in this conversation so far and anything it should know or decide. Plain text only.";

/// One message to a member.
pub struct CrewMsg {
    pub id: Uuid,
    pub from: Addr,
    pub text: String,
    pub thread: Uuid,
    /// Answered with the member's reply when the sender awaits it (`task`).
    pub reply: Option<oneshot::Sender<Result<String, String>>>,
}

/// Internal envelope: the message plus how to handle it.
struct Job {
    msg: CrewMsg,
    /// Where tool progress goes while this job runs (a task card).
    progress: Option<(EventSender, String)>,
    /// Cancels this job (the delegating turn was cancelled).
    cancel: Option<CancellationToken>,
    /// Deliver the reply to the main agent's report queue.
    report_to_main: bool,
    /// `!stop` generation at enqueue; stale jobs are dropped.
    generation: u64,
}

enum Envelope {
    Job(Box<Job>),
    /// Hand the transcript over and stop (model respawn).
    Handover(oneshot::Sender<Vec<rocinante_providers::Message>>),
}

/// Shared between a member's handle, its run loop, and its event relay.
struct MemberShared {
    name: String,
    status: watch::Sender<MemberStatus>,
    queued: AtomicU32,
    tokens: AtomicU64,
    generation: AtomicU64,
    /// Progress sink of the running job.
    sink: Mutex<Option<(EventSender, String)>>,
    frontend: EventSender,
}

impl MemberShared {
    fn set_status(&self, status: MemberStatus) {
        self.status.send_replace(status.clone());
        self.frontend.send(AgentEvent::Crew(CrewEvent::Status {
            name: self.name.clone(),
            status,
            queued: self.queued.load(Ordering::SeqCst),
        }));
    }

    fn republish(&self) {
        let status = self.status.borrow().clone();
        self.set_status(status);
    }
}

struct MemberHandle {
    inbox: mpsc::UnboundedSender<Envelope>,
    /// `provider/model` the member was spawned on.
    model_key: String,
    interrupter: Interrupter,
    shared: Arc<MemberShared>,
}

pub struct CrewManager {
    config: Arc<Config>,
    permissions: Arc<PermissionEngine>,
    scheduler: Option<Arc<Scheduler>>,
    router: Arc<ReplyRouter>,
    /// Frontend channel: crew events and members' permission asks.
    events: EventSender,
    cwd: PathBuf,
    max_idle: Duration,
    /// `/submodel` pin, shared with the frontend and the `task` tool.
    pinned_model: Arc<std::sync::Mutex<Option<String>>>,
    /// Main model tag, shared with the frontend (residency decisions).
    main_model: Arc<std::sync::Mutex<String>>,
    write_gate: Arc<tokio::sync::Mutex<()>>,
    members: Mutex<HashMap<String, MemberHandle>>,
    /// Reports waiting for the main agent's next turn: (member, text).
    main_reports: Arc<Mutex<Vec<(String, String)>>>,
    /// Test seam: run every member on this provider.
    provider_override: Option<Arc<dyn rocinante_providers::Provider>>,
}

impl CrewManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<Config>,
        permissions: Arc<PermissionEngine>,
        scheduler: Option<Arc<Scheduler>>,
        router: Arc<ReplyRouter>,
        events: broadcast::Sender<AgentEvent>,
        cwd: PathBuf,
        pinned_model: Arc<std::sync::Mutex<Option<String>>>,
        main_model: Arc<std::sync::Mutex<String>>,
    ) -> Arc<Self> {
        let max_idle = crate::interval::parse(&config.crew.max_idle).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "bad [crew] max_idle; using 30m");
            Duration::from_secs(30 * 60)
        });
        Arc::new(Self {
            config,
            permissions,
            scheduler,
            router,
            events: EventSender::new(events),
            cwd,
            max_idle,
            pinned_model,
            main_model,
            write_gate: Arc::new(tokio::sync::Mutex::new(())),
            members: Mutex::new(HashMap::new()),
            main_reports: Arc::new(Mutex::new(Vec::new())),
            provider_override: None,
        })
    }

    /// The worktree write gate, shared with the ephemeral `task` path.
    pub fn write_gate(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.write_gate)
    }

    /// Profile names a user may address with `@name`.
    pub fn profiles(&self) -> Vec<String> {
        self.config.agents.keys().cloned().collect()
    }

    pub fn is_member(&self, name: &str) -> bool {
        self.config.agents.contains_key(name)
    }

    /// The member's status, from its published state (no model call).
    pub fn status(&self, name: &str) -> Option<(MemberStatus, u32)> {
        let members = self.members.lock().unwrap();
        let handle = members.get(name).filter(|h| !h.inbox.is_closed())?;
        Some((
            handle.shared.status.borrow().clone(),
            handle.shared.queued.load(Ordering::SeqCst),
        ))
    }

    /// `@name status` answered from published state.
    pub fn status_line(&self, name: &str) -> String {
        if !self.is_member(name) {
            return format!("no crew member `{name}`");
        }
        match self.status(name) {
            None => format!("{name}: not started (no context yet)"),
            Some((status, queued)) => {
                let mut line = format!("{name}: {}", describe_status(&status));
                if queued > 0 {
                    line.push_str(&format!(" · {queued} queued"));
                }
                line
            }
        }
    }

    /// The user talks to a member (`@name text`). Returns at once; the reply
    /// arrives as a `CrewEvent::Message`.
    pub fn send_from_user(&self, name: &str, text: &str) -> Result<(), String> {
        let thread = Uuid::new_v4();
        self.events.send(AgentEvent::Crew(CrewEvent::Message {
            from: Addr::User,
            to: Addr::Member(name.to_string()),
            text: text.to_string(),
            thread,
        }));
        self.enqueue(
            name,
            None,
            CrewMsg {
                id: Uuid::new_v4(),
                from: Addr::User,
                text: text.to_string(),
                thread,
                reply: None,
            },
            None,
            None,
            false,
        )
    }

    /// `@name report`: the member summarizes its conversation for the main
    /// agent; the summary is injected before the main agent's next turn.
    pub fn request_report(&self, name: &str) -> Result<(), String> {
        let thread = Uuid::new_v4();
        self.events.send(AgentEvent::Crew(CrewEvent::Message {
            from: Addr::User,
            to: Addr::Member(name.to_string()),
            text: "report to main".into(),
            thread,
        }));
        self.enqueue(
            name,
            None,
            CrewMsg {
                id: Uuid::new_v4(),
                from: Addr::User,
                text: REPORT_PROMPT.into(),
                thread,
                reply: None,
            },
            None,
            None,
            true,
        )
    }

    /// `@name !stop`: interrupt the current turn and drop queued messages.
    pub fn stop(&self, name: &str) -> String {
        let members = self.members.lock().unwrap();
        match members.get(name).filter(|h| !h.inbox.is_closed()) {
            Some(handle) => {
                handle.shared.generation.fetch_add(1, Ordering::SeqCst);
                handle.shared.queued.store(0, Ordering::SeqCst);
                handle.interrupter.interrupt();
                handle.shared.republish();
                format!("{name}: stopped, queue cleared")
            }
            None => format!("{name} is not running"),
        }
    }

    /// The main agent delegates (`task`): queue to the member and await its
    /// reply. Progress shows on the delegating task card under `tag`.
    pub async fn ask_from_main(
        &self,
        name: &str,
        model_override: Option<&str>,
        prompt: &str,
        progress: EventSender,
        tag: String,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        let thread = Uuid::new_v4();
        self.events.send(AgentEvent::Crew(CrewEvent::Message {
            from: Addr::Main,
            to: Addr::Member(name.to_string()),
            text: prompt.to_string(),
            thread,
        }));
        let (tx, rx) = oneshot::channel();
        self.enqueue(
            name,
            model_override,
            CrewMsg {
                id: Uuid::new_v4(),
                from: Addr::Main,
                text: prompt.to_string(),
                thread,
                reply: Some(tx),
            },
            Some((progress, tag)),
            Some(cancel.clone()),
            false,
        )?;
        tokio::select! {
            r = rx => r.unwrap_or_else(|_| Err("subagent stopped".into())),
            () = cancel.cancelled() => Err("subagent cancelled".into()),
        }
    }

    /// Reports queued for the main agent, oldest first (drained).
    pub fn take_main_reports(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.main_reports.lock().unwrap())
    }

    fn enqueue(
        &self,
        name: &str,
        model_override: Option<&str>,
        msg: CrewMsg,
        progress: Option<(EventSender, String)>,
        cancel: Option<CancellationToken>,
        report_to_main: bool,
    ) -> Result<(), String> {
        let (inbox, shared) = self.ensure_member(name, model_override)?;
        let job = Job {
            msg,
            progress,
            cancel,
            report_to_main,
            generation: shared.generation.load(Ordering::SeqCst),
        };
        shared.queued.fetch_add(1, Ordering::SeqCst);
        if !matches!(*shared.status.borrow(), MemberStatus::Idle) {
            shared.republish();
        }
        inbox
            .send(Envelope::Job(Box::new(job)))
            .map_err(|_| format!("crew member `{name}` stopped"))
    }

    /// The live member for `name`, spawning on first use, after idle
    /// reaping, or when the pin/override names a different model (the old
    /// member hands its transcript to the new one, which carries a summary
    /// into its first message).
    fn ensure_member(
        &self,
        name: &str,
        model_override: Option<&str>,
    ) -> Result<(mpsc::UnboundedSender<Envelope>, Arc<MemberShared>), String> {
        let profile = self
            .config
            .agents
            .get(name)
            .ok_or_else(|| {
                format!(
                    "no crew member `{name}` — members: {}",
                    self.profiles().join(", ")
                )
            })?
            .clone();
        // Same priority as a one-off subagent on every message: pin, then
        // the call's override, then the profile. A different answer than the
        // running member's model respawns it.
        let pinned = self.pinned_model.lock().unwrap().clone();
        let mut resolved = resolve_subagent_model(
            &self.config,
            pinned.as_deref(),
            model_override,
            &profile.model,
        )
        .map_err(|e| format!("cannot start agent `{name}`: {e}"))?;
        let wanted_key = format!("{}/{}", resolved.model.provider, resolved.model.model);
        let mut members = self.members.lock().unwrap();
        if let Some(handle) = members.get(name)
            && !handle.inbox.is_closed()
            && handle.model_key == wanted_key
        {
            return Ok((handle.inbox.clone(), Arc::clone(&handle.shared)));
        }
        let old = members.remove(name);
        let reaped = old.as_ref().is_some_and(|h| h.inbox.is_closed());
        if let Some(provider) = &self.provider_override {
            resolved.provider = Arc::clone(provider);
        }
        let model_key = wanted_key;
        let model_tag = resolved.model.model.clone();
        let params = provider_factory::gen_params(&self.config, &resolved.model, resolved.is_local);
        let sub_ref = model_ref_of(&resolved, &params);
        let summarizer = (
            Arc::clone(&resolved.provider),
            resolved.model.model.clone(),
            params.clone(),
        );
        let (tx, rx) = broadcast::channel::<AgentEvent>(1024);
        let agent = build_subagent(
            &self.config,
            &self.permissions,
            name,
            &profile,
            resolved,
            params,
            &self.cwd,
            1,
            tx,
            Arc::clone(&self.router),
            self.scheduler.as_ref(),
        );
        let (status_tx, _) = watch::channel(MemberStatus::Idle);
        let shared = Arc::new(MemberShared {
            name: name.to_string(),
            status: status_tx,
            queued: AtomicU32::new(0),
            tokens: AtomicU64::new(
                old.as_ref()
                    .map_or(0, |h| h.shared.tokens.load(Ordering::SeqCst)),
            ),
            generation: AtomicU64::new(0),
            sink: Mutex::new(None),
            frontend: self.events.clone(),
        });
        // A model change: the old member hands its transcript over once
        // its current job ends.
        let carry = old.filter(|h| !h.inbox.is_closed()).and_then(|h| {
            let (htx, hrx) = oneshot::channel();
            h.inbox.send(Envelope::Handover(htx)).ok().map(|()| hrx)
        });
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel();
        let interrupter = agent.interrupter();
        tokio::spawn(relay_member_events(rx, Arc::clone(&shared)));
        tokio::spawn(member_loop(
            agent,
            inbox_rx,
            MemberRun {
                shared: Arc::clone(&shared),
                frontend: self.events.clone(),
                max_idle: self.max_idle,
                writes: profile_writes(&profile),
                write_gate: Arc::clone(&self.write_gate),
                scheduler: self.scheduler.clone(),
                sub_ref,
                config: Arc::clone(&self.config),
                main_model: Arc::clone(&self.main_model),
                main_reports: Arc::clone(&self.main_reports),
                carry,
                summarizer,
            },
        ));
        self.events.send(AgentEvent::Crew(CrewEvent::MemberSpawned {
            name: name.to_string(),
            model: model_tag,
        }));
        if reaped {
            self.events.send(AgentEvent::Notice {
                message: format!(
                    "{name} starts fresh: its earlier context was dropped after {} idle",
                    crate::interval::display(self.max_idle)
                ),
            });
        }
        members.insert(
            name.to_string(),
            MemberHandle {
                inbox: inbox_tx.clone(),
                model_key,
                interrupter,
                shared: Arc::clone(&shared),
            },
        );
        Ok((inbox_tx, shared))
    }
}

/// What a member's run loop needs besides its agent and inbox.
struct MemberRun {
    shared: Arc<MemberShared>,
    frontend: EventSender,
    max_idle: Duration,
    writes: bool,
    write_gate: Arc<tokio::sync::Mutex<()>>,
    scheduler: Option<Arc<Scheduler>>,
    sub_ref: crate::scheduler::ModelRef,
    config: Arc<Config>,
    main_model: Arc<std::sync::Mutex<String>>,
    main_reports: Arc<Mutex<Vec<(String, String)>>>,
    /// The predecessor's transcript (model change), summarized into the
    /// first job.
    carry: Option<oneshot::Receiver<Vec<rocinante_providers::Message>>>,
    summarizer: (
        Arc<dyn rocinante_providers::Provider>,
        String,
        rocinante_providers::GenParams,
    ),
}

/// A member's life: take jobs in order, one turn each, until the inbox
/// closes, a handover is requested, or it idles past `max_idle`.
async fn member_loop(
    mut agent: Agent,
    mut inbox: mpsc::UnboundedReceiver<Envelope>,
    mut ctx: MemberRun,
) {
    let name = ctx.shared.name.clone();
    loop {
        let envelope = tokio::select! {
            e = inbox.recv() => e,
            () = tokio::time::sleep(ctx.max_idle) => {
                let reason = format!(
                    "context dropped after {} idle",
                    crate::interval::display(ctx.max_idle)
                );
                ctx.frontend.send(AgentEvent::Crew(CrewEvent::MemberDropped {
                    name: name.clone(),
                    reason,
                }));
                break;
            }
        };
        let mut job = match envelope {
            None => break,
            Some(Envelope::Handover(tx)) => {
                let _ = tx.send(agent.messages().to_vec());
                ctx.frontend
                    .send(AgentEvent::Crew(CrewEvent::MemberDropped {
                        name: name.clone(),
                        reason: "model changed".into(),
                    }));
                break;
            }
            Some(Envelope::Job(job)) => *job,
        };
        let _ = ctx
            .shared
            .queued
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                Some(n.saturating_sub(1))
            });
        let stale = job.generation < ctx.shared.generation.load(Ordering::SeqCst);
        if stale || job.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            if let Some(tx) = job.msg.reply {
                let _ = tx.send(Err("subagent cancelled".into()));
            }
            ctx.shared.republish();
            continue;
        }
        if let Some(rx) = ctx.carry.take()
            && let Some(summary) = carried_summary(rx, &ctx).await
        {
            job.msg.text = format!(
                "Context carried over from your earlier work this session (summarized):\n{summary}\n\n---\n\n{}",
                job.msg.text
            );
        }
        let result = run_job(
            &mut agent,
            &ctx,
            &job.msg.text,
            job.progress,
            job.cancel.as_ref(),
        )
        .await;
        let reply = match &result {
            Ok(t) => t.clone(),
            Err(e) => format!("(failed: {e})"),
        };
        let to = if job.report_to_main {
            if result.is_ok() {
                ctx.main_reports
                    .lock()
                    .unwrap()
                    .push((name.clone(), reply.clone()));
            }
            Addr::Main
        } else {
            job.msg.from.clone()
        };
        ctx.frontend.send(AgentEvent::Crew(CrewEvent::Message {
            from: Addr::Member(name.clone()),
            to,
            text: reply,
            thread: job.msg.thread,
        }));
        if let Some(tx) = job.msg.reply {
            let _ = tx.send(result);
        }
        let queued = ctx.shared.queued.load(Ordering::SeqCst);
        ctx.shared.set_status(if queued > 0 {
            MemberStatus::Queued(queued)
        } else {
            MemberStatus::Idle
        });
    }
}

/// Summarize a predecessor's transcript with this member's model; the
/// plain digest stands in when the summary call fails.
async fn carried_summary(
    rx: oneshot::Receiver<Vec<rocinante_providers::Message>>,
    ctx: &MemberRun,
) -> Option<String> {
    let messages = tokio::time::timeout(Duration::from_secs(600), rx)
        .await
        .ok()?
        .ok()?;
    let digest = transcript_digest(&messages);
    if digest.trim().is_empty() {
        return None;
    }
    let goal = messages
        .iter()
        .find(|m| m.role == rocinante_providers::Role::User)
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let (provider, model, params) = ctx.summarizer.clone();
    let prompt = crate::context::ContextManager::summarize_prompt(&goal, &digest);
    match crate::agent::one_shot_call(
        provider,
        model,
        params,
        "You summarize a subagent's work precisely. Keep exact paths, commands, findings.",
        &prompt,
        ctx.scheduler.as_ref(),
    )
    .await
    {
        Ok(out) if !out.text.trim().is_empty() => Some(out.text),
        _ => Some(digest),
    }
}

/// One turn for one job, holding the write and residency gates it needs.
async fn run_job(
    agent: &mut Agent,
    ctx: &MemberRun,
    text: &str,
    progress: Option<(EventSender, String)>,
    cancel: Option<&CancellationToken>,
) -> Result<String, String> {
    let _write = if ctx.writes {
        Some(ctx.write_gate.lock().await)
    } else {
        None
    };
    let residency = match &ctx.scheduler {
        Some(s) => {
            let main = main_model_ref(&ctx.config, s, &ctx.main_model);
            s.residency(main.as_ref(), &ctx.sub_ref).await
        }
        None => None,
    };
    agent.set_keep_alive(
        residency
            .as_ref()
            .is_some_and(|r| r.evict)
            .then(|| "0".to_string()),
    );
    *ctx.shared.sink.lock().unwrap() = progress;
    ctx.shared.set_status(MemberStatus::Working {
        step: "thinking".into(),
        since: Instant::now(),
    });
    let mut result = submit_cancellable(agent, text, cancel).await;
    if matches!(&result, Ok(t) if t.trim().is_empty()) {
        result = submit_cancellable(agent, EMPTY_REPORT_NUDGE, cancel).await;
    }
    *ctx.shared.sink.lock().unwrap() = None;
    result
}

async fn submit_cancellable(
    agent: &mut Agent,
    text: &str,
    cancel: Option<&CancellationToken>,
) -> Result<String, String> {
    let interrupter = agent.interrupter();
    let watch_cancel = async {
        match cancel {
            Some(c) => c.cancelled().await,
            None => std::future::pending().await,
        }
    };
    let submit = agent.submit(text);
    tokio::pin!(submit);
    let result = tokio::select! {
        r = &mut submit => r,
        () = watch_cancel => {
            interrupter.interrupt();
            submit.await
        }
    };
    match result {
        Ok(turn) => Ok(turn.final_text),
        Err(e) => Err(e.to_string()),
    }
}

/// Turn a member's agent events into status updates, token totals, task-card
/// progress, and bubbled permission asks.
async fn relay_member_events(mut rx: broadcast::Receiver<AgentEvent>, shared: Arc<MemberShared>) {
    loop {
        let event = match rx.recv().await {
            Ok(e) => e,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        };
        let sink = shared.sink.lock().unwrap().clone();
        if let Some((events, tag)) = &sink {
            relay_progress(events, tag, &event);
        }
        match event {
            AgentEvent::ToolCallStarted { summary, .. } => {
                shared.set_status(MemberStatus::Working {
                    step: summary,
                    since: Instant::now(),
                });
            }
            AgentEvent::ToolProgress { call_id, .. } if call_id == SLOT_WAIT_TAG => {
                shared.set_status(MemberStatus::WaitingSlot);
            }
            AgentEvent::PermissionRequested {
                request_id,
                summary,
                tool_name,
                detail,
            } => {
                shared.set_status(MemberStatus::WaitingPermission);
                shared.frontend.send(AgentEvent::PermissionRequested {
                    request_id,
                    summary: format!("[{}] {summary}", shared.name),
                    tool_name,
                    detail,
                });
            }
            AgentEvent::DraftText { .. } | AgentEvent::AssistantText { .. } => {
                if !matches!(*shared.status.borrow(), MemberStatus::Working { .. }) {
                    shared.set_status(MemberStatus::Working {
                        step: "writing".into(),
                        since: Instant::now(),
                    });
                }
            }
            AgentEvent::Usage(u) => {
                let total = shared
                    .tokens
                    .fetch_add(u.prompt_tokens + u.completion_tokens, Ordering::SeqCst)
                    + u.prompt_tokens
                    + u.completion_tokens;
                shared.frontend.send(AgentEvent::Crew(CrewEvent::Tokens {
                    name: shared.name.clone(),
                    total,
                }));
            }
            _ => {}
        }
    }
}

/// A compact view of a member's transcript to seed its successor.
fn transcript_digest(messages: &[rocinante_providers::Message]) -> String {
    use rocinante_providers::Role;
    let mut out = String::new();
    for m in messages.iter().filter(|m| m.role != Role::System) {
        let who = match m.role {
            Role::User => "task",
            Role::Assistant if !m.content.trim().is_empty() => "you",
            _ => continue,
        };
        let text: String = m.content.trim().chars().take(600).collect();
        out.push_str(&format!("[{who}] {text}\n"));
    }
    // Keep the most recent part when long.
    let chars: Vec<char> = out.chars().collect();
    if chars.len() > 4_000 {
        chars[chars.len() - 4_000..].iter().collect()
    } else {
        out
    }
}

/// Human-readable status for `@name status` and the REPL.
pub fn describe_status(status: &MemberStatus) -> String {
    match status {
        MemberStatus::Idle => "idle".into(),
        MemberStatus::Working { step, since } => {
            format!("working: {step} ({}s)", since.elapsed().as_secs())
        }
        MemberStatus::WaitingPermission => "waiting for your permission".into(),
        MemberStatus::Queued(n) => format!("{n} message(s) queued"),
        MemberStatus::WaitingSlot => "waiting for a model slot".into(),
    }
}

/// Prefix the main agent's next input with crew reports, if any.
pub fn with_reports(reports: Vec<(String, String)>, input: &str) -> String {
    if reports.is_empty() {
        return input.to_string();
    }
    let mut out = String::from(
        "[Crew reports since your last turn, from side conversations the user had with crew members]\n",
    );
    for (name, text) in reports {
        out.push_str(&format!("- {name}: {}\n", text.trim()));
    }
    out.push('\n');
    out.push_str(input);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rocinante_providers::{
        Capabilities, ChatDelta, ChatRequest, ChatStream, Message, Provider, ProviderError, Role,
        StopReason, ToolSchema,
    };

    /// Replies "<n> asks; last: <text>" after `delay`: the count shows
    /// whether context persisted.
    struct Counting {
        delay: Duration,
    }

    #[async_trait]
    impl Provider for Counting {
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
            tokio::time::sleep(self.delay).await;
            let asks: Vec<&Message> = req
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .collect();
            let reply = format!(
                "{} asks; last: {}",
                asks.len(),
                asks.last().map(|m| m.content.as_str()).unwrap_or("")
            );
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(ChatDelta::Text(reply)),
                Ok(ChatDelta::Done(StopReason::EndTurn)),
            ])))
        }
        fn count_tokens(&self, _m: &[Message], _t: &[ToolSchema]) -> usize {
            1
        }
    }

    fn manager(
        delay: Duration,
        max_idle: Duration,
    ) -> (Arc<CrewManager>, broadcast::Receiver<AgentEvent>) {
        let config = Arc::new(
            crate::config::load_from(
                std::path::Path::new("/nonexistent/a.toml"),
                std::path::Path::new("/nonexistent/b.toml"),
            )
            .unwrap(),
        );
        let (tx, rx) = broadcast::channel(1024);
        let manager = CrewManager {
            permissions: Arc::new(PermissionEngine::from_config(&Default::default())),
            scheduler: None,
            router: Arc::new(ReplyRouter::default()),
            events: EventSender::new(tx),
            cwd: std::env::temp_dir(),
            max_idle,
            pinned_model: Arc::new(std::sync::Mutex::new(None)),
            main_model: Arc::new(std::sync::Mutex::new("mock".into())),
            write_gate: Arc::new(tokio::sync::Mutex::new(())),
            members: Mutex::new(HashMap::new()),
            main_reports: Arc::new(Mutex::new(Vec::new())),
            provider_override: Some(Arc::new(Counting { delay })),
            config,
        };
        (Arc::new(manager), rx)
    }

    /// Wait for the next crew reply from `name`.
    async fn reply_from(rx: &mut broadcast::Receiver<AgentEvent>, name: &str) -> (Addr, String) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(AgentEvent::Crew(CrewEvent::Message { from, to, text, .. })) =
                    rx.recv().await
                    && from == Addr::Member(name.into())
                {
                    return (to, text);
                }
            }
        })
        .await
        .expect("a reply")
    }

    const LONG: Duration = Duration::from_secs(3600);

    #[tokio::test]
    async fn spawns_on_first_message_and_replies_to_the_user() {
        let (crew, mut rx) = manager(Duration::ZERO, LONG);
        assert!(crew.status("naomi").is_none(), "not started yet");
        crew.send_from_user("naomi", "what are you working on?")
            .unwrap();
        let (to, text) = reply_from(&mut rx, "naomi").await;
        assert_eq!(to, Addr::User);
        assert_eq!(text, "1 asks; last: what are you working on?");
        assert!(crew.status("naomi").is_some());
        assert!(crew.status_line("naomi").starts_with("naomi: "));
    }

    #[tokio::test]
    async fn context_persists_across_delegations() {
        let (crew, _rx) = manager(Duration::ZERO, LONG);
        let ask = |p: &'static str| {
            let crew = Arc::clone(&crew);
            async move {
                let (tx, _) = broadcast::channel(16);
                crew.ask_from_main(
                    "naomi",
                    None,
                    p,
                    EventSender::new(tx),
                    "task[naomi]".into(),
                    CancellationToken::new(),
                )
                .await
            }
        };
        assert_eq!(
            ask("find the parser").await.unwrap(),
            "1 asks; last: find the parser"
        );
        assert_eq!(
            ask("and its tests").await.unwrap(),
            "2 asks; last: and its tests",
            "second task sees the first"
        );
    }

    #[tokio::test]
    async fn messages_queue_while_a_member_works() {
        let (crew, mut rx) = manager(Duration::from_millis(150), LONG);
        crew.send_from_user("holden", "first").unwrap();
        crew.send_from_user("holden", "second").unwrap();
        let (_, a) = reply_from(&mut rx, "holden").await;
        let (_, b) = reply_from(&mut rx, "holden").await;
        assert_eq!(a, "1 asks; last: first");
        assert_eq!(b, "2 asks; last: second", "in order, one turn at a time");
    }

    #[tokio::test]
    async fn stop_interrupts_and_drops_the_queue() {
        let (crew, mut rx) = manager(Duration::from_secs(30), LONG);
        crew.send_from_user("amos", "long job").unwrap();
        let (tx, _) = broadcast::channel(16);
        let queued = {
            let crew = Arc::clone(&crew);
            tokio::spawn(async move {
                crew.ask_from_main(
                    "amos",
                    None,
                    "queued task",
                    EventSender::new(tx),
                    "task[amos]".into(),
                    CancellationToken::new(),
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(crew.stop("amos").contains("stopped"));
        let (_, text) = reply_from(&mut rx, "amos").await;
        assert!(text.contains("failed"), "interrupted turn: {text}");
        let queued = tokio::time::timeout(Duration::from_secs(5), queued)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(queued, Err("subagent cancelled".into()), "queue cleared");
    }

    #[tokio::test]
    async fn idle_members_are_reaped_and_respawn_fresh() {
        let (crew, mut rx) = manager(Duration::ZERO, Duration::from_millis(200));
        crew.send_from_user("bobbie", "one").unwrap();
        reply_from(&mut rx, "bobbie").await;
        let dropped = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(AgentEvent::Crew(CrewEvent::MemberDropped { name, reason })) =
                    rx.recv().await
                    && name == "bobbie"
                {
                    return reason;
                }
            }
        })
        .await
        .unwrap();
        assert!(dropped.contains("idle"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        crew.send_from_user("bobbie", "two").unwrap();
        let (_, text) = reply_from(&mut rx, "bobbie").await;
        assert_eq!(text, "1 asks; last: two", "fresh context after reaping");
    }

    #[tokio::test]
    async fn a_different_model_respawns_with_carried_context() {
        let (crew, mut rx) = manager(Duration::ZERO, LONG);
        let ask = |model: Option<&'static str>, p: &'static str| {
            let crew = Arc::clone(&crew);
            async move {
                let (tx, _) = broadcast::channel(16);
                crew.ask_from_main(
                    "naomi",
                    model,
                    p,
                    EventSender::new(tx),
                    "task[naomi]".into(),
                    CancellationToken::new(),
                )
                .await
                .unwrap()
            }
        };
        assert_eq!(ask(Some("qwen3:8b"), "first").await, "1 asks; last: first");
        // No override now: the profile model, not the override, is wanted.
        let second = ask(None, "second").await;
        assert!(
            second.starts_with("1 asks; last: Context carried over"),
            "fresh member seeded with a summary: {second}"
        );
        assert!(second.ends_with("second"));
        // Same model again: the member is reused.
        assert_eq!(ask(None, "third").await.split(';').next(), Some("2 asks"));
        let mut spawned = 0;
        while let Ok(ev) = rx.try_recv() {
            if matches!(ev, AgentEvent::Crew(CrewEvent::MemberSpawned { .. })) {
                spawned += 1;
            }
        }
        assert_eq!(spawned, 2);
    }

    #[tokio::test]
    async fn reports_reach_the_main_agent() {
        let (crew, mut rx) = manager(Duration::ZERO, LONG);
        crew.request_report("miller").unwrap();
        let (to, _) = reply_from(&mut rx, "miller").await;
        assert_eq!(to, Addr::Main);
        let reports = crew.take_main_reports();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].0, "miller");
        assert!(crew.take_main_reports().is_empty(), "drained");
        let input = with_reports(reports, "next question");
        assert!(input.starts_with("[Crew reports"));
        assert!(input.contains("- miller: ") && input.ends_with("next question"));
        assert_eq!(with_reports(vec![], "plain"), "plain");
    }

    #[test]
    fn unknown_member_lists_the_crew() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (crew, _rx) = manager(Duration::ZERO, LONG);
            let err = crew.send_from_user("julie", "hi").unwrap_err();
            assert!(
                err.contains("no crew member `julie`") && err.contains("naomi"),
                "{err}"
            );
        });
    }
}
