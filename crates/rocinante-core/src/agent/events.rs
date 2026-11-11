//! The event stream between the agent core and any frontend (REPL, TUI,
//! future HTTP server). This channel pair is the entire frontend API.
//!
//! Permission answers flow through a [`ReplyRouter`] shared by the main
//! agent and every subagent, so one frontend reply channel serves the whole
//! agent tree: requests carry a Uuid, replies are routed back by that Uuid.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use uuid::Uuid;

use rocinante_providers::Usage;

/// `ToolProgress` call id an agent uses while it waits for a model slot.
/// Subagent forwarders re-tag it with the task's id.
pub const SLOT_WAIT_TAG: &str = "slot";
/// The matching progress text.
pub const SLOT_WAIT_TEXT: &str = "⏳ waiting for a model slot";

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TurnStarted {
        turn_id: Uuid,
    },
    /// Streaming assistant prose.
    AssistantText {
        delta: String,
    },
    /// Streaming reasoning from a thinking model; display-only.
    Thinking {
        delta: String,
    },
    ToolCallStarted {
        call_id: String,
        name: String,
        summary: String,
    },
    /// Streaming output from a running tool (bash output, subagent activity).
    ToolProgress {
        call_id: String,
        chunk: String,
    },
    ToolFinished {
        call_id: String,
        output_preview: String,
        is_error: bool,
    },
    /// The frontend MUST answer by sending `FrontendReply::Permission` with
    /// this request_id on its reply channel.
    PermissionRequested {
        request_id: Uuid,
        summary: String,
        tool_name: String,
        /// Rich preview (unified diff for edits) to show before approval.
        detail: Option<String>,
    },
    ContextCompacted {
        before_tokens: usize,
        after_tokens: usize,
    },
    /// Progress: the verifier found gaps and is starting corrective retry
    /// `attempt` of `max` before it certifies the work done.
    VerificationAttempt {
        attempt: u32,
        max: u32,
    },
    /// Final quality-check verdict: does the finished work match the ask?
    /// `ok` = certified; otherwise `findings` lists the residual gaps.
    VerificationReport {
        ok: bool,
        findings: String,
    },
    /// The main model was hot-switched (context preserved). `model` is the
    /// wire tag; `display` is what the UI shows (the alias when one was used).
    ModelChanged {
        model: String,
        display: String,
    },
    Usage(Usage),
    /// Tokens spent by auxiliary calls (the critic): they count toward the
    /// session totals but say nothing about the main context's fill.
    AuxUsage(Usage),
    /// Scrutiny: prose of a draft answer that a critic will review before
    /// it is released. Render it like `Thinking` (transient); the released
    /// answer arrives as `AssistantText`.
    DraftText {
        delta: String,
    },
    ScrutinyStarted {
        kind: ScrutinyKind,
    },
    /// Final scrutiny outcome for this turn: `revisions` silent rewrites
    /// happened; `findings` is the last critique (empty when approved first
    /// time).
    ScrutinyVerdict {
        kind: ScrutinyKind,
        approved: bool,
        revisions: u32,
        findings: String,
    },
    /// Occupancy of the main model's scheduler lane (spec: slots used of
    /// budget), emitted on every acquire and release.
    SchedulerStats {
        lane: String,
        used: u32,
        budget: u32,
    },
    /// Crew (persistent subagents) activity for the collaborate pane.
    Crew(CrewEvent),
    /// Informational line for the transcript (not an error).
    Notice {
        message: String,
    },
    /// An in-process model is loading (`total == 0`) or reading the prompt
    /// (`done` of `total` new tokens): a status-line indicator.
    PromptProgress {
        done: u64,
        total: u64,
    },
    TurnFinished {
        turn_id: Uuid,
    },
    Error {
        message: String,
        fatal: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrutinyKind {
    /// The final answer text of a turn.
    Answer,
    /// Completed edits/commands (the verification path).
    Work,
    /// A risky action about to run (pre-action review, `actions = "risky"`).
    Action,
}

/// A participant in crew conversations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Addr {
    User,
    Main,
    Member(String),
}

impl std::fmt::Display for Addr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Addr::User => f.write_str("you"),
            Addr::Main => f.write_str("main"),
            Addr::Member(name) => f.write_str(name),
        }
    }
}

/// What a crew member is doing, published without polling the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberStatus {
    Idle,
    Working {
        step: String,
        since: std::time::Instant,
    },
    WaitingPermission,
    /// Not started yet: `n` messages ahead of it in the member's inbox.
    Queued(u32),
    WaitingSlot,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CrewEvent {
    MemberSpawned {
        name: String,
        model: String,
    },
    /// `queued` = messages waiting in the member's inbox behind the current one.
    Status {
        name: String,
        status: MemberStatus,
        queued: u32,
    },
    Message {
        from: Addr,
        to: Addr,
        text: String,
        thread: Uuid,
    },
    /// Cumulative tokens the member has used this session.
    Tokens {
        name: String,
        total: u64,
    },
    MemberDropped {
        name: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    Allow,
    /// Allow and remember for the rest of the session.
    AlwaysAllow,
    Deny,
}

/// Messages from the frontend into the agent tree.
#[derive(Debug)]
pub enum FrontendReply {
    Permission {
        request_id: Uuid,
        decision: PermissionDecision,
    },
}

#[derive(Clone)]
pub struct EventSender {
    tx: broadcast::Sender<AgentEvent>,
}

impl EventSender {
    pub fn new(tx: broadcast::Sender<AgentEvent>) -> Self {
        Self { tx }
    }
    /// Send, ignoring "no receivers" — events are best-effort towards UIs.
    pub fn send(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }
}

/// Routes frontend replies to whichever agent (main or nested) is waiting
/// on that request id.
#[derive(Default)]
pub struct ReplyRouter {
    pending: Mutex<HashMap<Uuid, oneshot::Sender<PermissionDecision>>>,
}

impl ReplyRouter {
    /// Register interest in a request id BEFORE emitting the event, so the
    /// reply can't race past us.
    pub fn register(&self, request_id: Uuid) -> oneshot::Receiver<PermissionDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(request_id, tx);
        rx
    }

    pub fn deliver(&self, request_id: Uuid, decision: PermissionDecision) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&request_id) {
            let _ = tx.send(decision);
        } else {
            tracing::warn!(%request_id, "permission reply for unknown request");
        }
    }

    pub fn forget(&self, request_id: Uuid) {
        self.pending.lock().unwrap().remove(&request_id);
    }
}

/// The agent's ends of the channel pair.
pub struct AgentChannels {
    pub events: broadcast::Sender<AgentEvent>,
    pub router: Arc<ReplyRouter>,
}

/// The frontend's ends. `replies` is cloneable — hand copies to whatever
/// task answers permission prompts.
pub struct FrontendHandle {
    pub events: broadcast::Receiver<AgentEvent>,
    pub replies: mpsc::Sender<FrontendReply>,
}

/// Build the channel pair and spawn the dispatcher that feeds frontend
/// replies into the router. Call from within a tokio runtime.
pub fn channel_pair() -> (AgentChannels, FrontendHandle) {
    let (event_tx, event_rx) = broadcast::channel(1024);
    let (reply_tx, mut reply_rx) = mpsc::channel::<FrontendReply>(64);
    let router = Arc::new(ReplyRouter::default());

    let dispatcher_router = Arc::clone(&router);
    tokio::spawn(async move {
        while let Some(reply) = reply_rx.recv().await {
            match reply {
                FrontendReply::Permission {
                    request_id,
                    decision,
                } => {
                    dispatcher_router.deliver(request_id, decision);
                }
            }
        }
    });

    (
        AgentChannels {
            events: event_tx,
            router,
        },
        FrontendHandle {
            events: event_rx,
            replies: reply_tx,
        },
    )
}
