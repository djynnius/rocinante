//! Owns the Agent: `submit` is `&mut self` async, so a single task
//! serializes turns while the TUI stays responsive. Commands come in over
//! an mpsc; everything else flows back through the agent's event broadcast.

use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use rocinante_core::agent::events::AgentEvent;
use rocinante_core::agent::{Agent, AgentError};
use rocinante_core::config::Mode;

pub enum DriverCmd {
    Input {
        text: String,
        /// TUI-side cancel handle for this turn; bridged to the agent's
        /// Interrupter so cancellation runs core's graceful path (child
        /// processes killed, TurnFinished emitted by submit itself).
        cancel: CancellationToken,
    },
    SetMode(Mode),
    SetThink(bool),
    /// `/compact`: manual context compaction.
    Compact,
    /// Hot-switch the main model (resolved by the event loop; the agent
    /// emits ModelChanged which updates the UI).
    SetModel(rocinante_core::provider_factory::SwitchTarget),
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
                DriverCmd::SetMode(mode) => agent.set_mode(mode),
                DriverCmd::SetThink(on) => agent.set_think(on),
                DriverCmd::Compact => {
                    // Success renders via ContextCompacted; failure as Error.
                    if let Err(e) = agent.compact_now().await {
                        let _ = events.send(AgentEvent::Error {
                            message: format!("{e:#}"),
                            fatal: false,
                        });
                    }
                }
                DriverCmd::SetModel(target) => {
                    agent.set_model(target.provider, target.model, target.params)
                }
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
        // Command channel closed = the UI quit. Final brainbox update
        // (bounded internally; the UI awaits us with its own timeout too).
        agent.finalize().await;
    })
}
