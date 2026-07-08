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
    /// The main model was hot-switched (context preserved).
    ModelChanged {
        model: String,
    },
    Usage(Usage),
    TurnFinished {
        turn_id: Uuid,
    },
    Error {
        message: String,
        fatal: bool,
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
