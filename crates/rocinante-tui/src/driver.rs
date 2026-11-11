//! Owns the Agent: `submit` is `&mut self` async, so a single task
//! serializes turns while the TUI stays responsive. Commands come in over
//! an mpsc; everything else flows back through the agent's event broadcast.

use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use rocinante_core::agent::events::AgentEvent;
use rocinante_core::agent::{Agent, AgentError};

pub enum DriverCmd {
    Input {
        text: String,
        /// TUI-side cancel handle for this turn; bridged to the agent's
        /// Interrupter so cancellation runs core's graceful path (child
        /// processes killed, TurnFinished emitted by submit itself).
        cancel: CancellationToken,
    },
    SetThink(bool),
    SetEffort(rocinante_providers::Effort),
    /// `/compact`: manual context compaction.
    Compact,
    /// `/clear` (`all` = also wipe BRAINBOX.md): reset the conversation.
    Clear {
        all: bool,
    },
    /// `/verify`: run the quality check on the last task. Carries a cancel
    /// handle (bridged like `Input`) so Esc can stop a long fix loop.
    Verify {
        cancel: CancellationToken,
    },
    /// Hot-switch the main model (resolved by the event loop; the agent
    /// emits ModelChanged which updates the UI).
    SetModel(rocinante_core::provider_factory::SwitchTarget),
    /// `/scrutiny off|answers|work|all`.
    ScrutinyMode {
        answers: rocinante_core::config::AnswerScrutiny,
        work: bool,
    },
    /// `/scrutiny show …`.
    ScrutinyShow(rocinante_core::config::ScrutinyShow),
    /// `/scrutiny model …` (resolved by the event loop; None = self).
    SetCritic(Option<rocinante_core::scrutiny::CriticModel>),
    /// `/submodel` changed the default subagent model: refresh the
    /// parallelism sentence of the system prompt.
    SetSubModel(Option<rocinante_core::scheduler::ModelRef>),
}

pub fn spawn(
    mut agent: Agent,
    mut cmds: mpsc::Receiver<DriverCmd>,
    events: broadcast::Sender<AgentEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interrupter = agent.interrupter();
        while let Some(cmd) = cmds.recv().await {
            match cmd {
                DriverCmd::SetThink(on) => agent.set_think(on),
                DriverCmd::SetEffort(e) => agent.set_effort(e),
                DriverCmd::Compact => {
                    // Success renders via ContextCompacted; failure as Error.
                    if let Err(e) = agent.compact_now().await {
                        let _ = events.send(AgentEvent::Error {
                            message: format!("{e:#}"),
                            fatal: false,
                        });
                    }
                }
                DriverCmd::Clear { all } => agent.clear_context(all),
                DriverCmd::Verify { cancel } => {
                    let bridge = {
                        let interrupter = interrupter.clone();
                        tokio::spawn(async move {
                            cancel.cancelled().await;
                            interrupter.interrupt();
                        })
                    };
                    agent.verify_last().await;
                    bridge.abort();
                }
                DriverCmd::SetModel(target) => {
                    agent.set_model(target.provider, target.model, target.display, target.params)
                }
                DriverCmd::ScrutinyMode { answers, work } => agent.update_scrutiny(|s| {
                    s.answers = answers;
                    s.work = work;
                }),
                DriverCmd::ScrutinyShow(show) => agent.update_scrutiny(|s| s.show = show),
                DriverCmd::SetCritic(critic) => agent.update_scrutiny(|s| s.set_critic(critic)),
                DriverCmd::SetSubModel(sub) => agent.set_sub_model(sub),
                DriverCmd::Input { text, cancel } => {
                    let bridge = {
                        let interrupter = interrupter.clone();
                        tokio::spawn(async move {
                            cancel.cancelled().await;
                            interrupter.interrupt();
                        })
                    };
                    match agent.submit(&text).await {
                        Ok(_) | Err(AgentError::Cancelled) => {}
                        Err(e) => {
                            let _ = events.send(AgentEvent::Error {
                                message: e.to_string(),
                                fatal: false,
                            });
                        }
                    }
                    bridge.abort();
                }
            }
        }
        // Command channel closed = the UI quit; return immediately. Any
        // in-flight background memory update is simply abandoned.
    })
}
