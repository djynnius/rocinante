//! Rocinante TUI (ratatui). Talks to the agent exclusively through the
//! AgentEvent/FrontendReply channel pair; the agent itself lives in a
//! driver task because `Agent::submit` needs `&mut self` for a whole turn.

mod app;
mod driver;
mod view;

use std::io::{self, Stdout};
use std::time::Duration;

use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use rocinante_core::agent::Agent;
use rocinante_core::agent::events::{AgentEvent, FrontendHandle, FrontendReply};
use rocinante_core::config::Config;
use rocinante_core::provider_factory::{self, ModelCatalog};

use app::{App, Effect, Msg};
use driver::DriverCmd;
use view::view;

const TICK: Duration = Duration::from_millis(33);

/// Everything `/model` needs: the config to resolve names against, the
/// switchable-model catalog, and the shared handle the task tool's VRAM
/// gate reads (kept in sync on every switch).
pub struct ModelSwitcher {
    pub config: std::sync::Arc<Config>,
    pub catalog: std::sync::Arc<ModelCatalog>,
    pub main_model: std::sync::Arc<std::sync::Mutex<String>>,
}

/// Run the TUI until the user quits. `events` is a clone of the agent's
/// event sender so the driver can synthesize events (submit errors, the
/// TurnFinished a cancelled turn never emitted). `notices` seed the
/// transcript (banner, session-resume info).
pub async fn run(
    agent: Agent,
    frontend: FrontendHandle,
    events: broadcast::Sender<AgentEvent>,
    model: String,
    notices: Vec<String>,
    switcher: ModelSwitcher,
) -> anyhow::Result<()> {
    let mode = agent.mode();
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let driver = driver::spawn(agent, cmd_rx, events);

    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let result = event_loop(
        &mut terminal,
        frontend,
        cmd_tx,
        model,
        mode,
        notices,
        switcher,
    )
    .await;
    restore_terminal();
    // event_loop returning dropped cmd_tx, so the driver is running its
    // final brainbox update; give it a bounded window, then cut it loose.
    eprintln!("updating BRAINBOX.md…");
    if tokio::time::timeout(Duration::from_secs(35), driver)
        .await
        .is_err()
    {
        tracing::warn!("driver shutdown timed out; abandoning final brainbox update");
    }
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    frontend: FrontendHandle,
    cmd_tx: mpsc::Sender<DriverCmd>,
    model: String,
    mode: rocinante_core::config::Mode,
    notices: Vec<String>,
    switcher: ModelSwitcher,
) -> anyhow::Result<()> {
    let size = terminal.size()?;
    let mut app = App::new(model, mode, (size.width, size.height), notices);
    let FrontendHandle {
        events: mut agent_events,
        replies,
    } = frontend;
    let mut term_events = EventStream::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Cancel handle for the turn currently in (or queued for) the driver.
    let mut current_cancel: Option<CancellationToken> = None;
    let mut events_open = true;

    terminal.draw(|f| view(&app, f))?;
    app.dirty = false;

    loop {
        let mut effects = Vec::new();
        tokio::select! {
            maybe = term_events.next() => match maybe {
                Some(Ok(Event::Key(k))) => effects = app.update(Msg::Key(k)),
                Some(Ok(Event::Mouse(m))) => match m.kind {
                    MouseEventKind::ScrollUp => effects = app.update(Msg::Scroll(3)),
                    MouseEventKind::ScrollDown => effects = app.update(Msg::Scroll(-3)),
                    _ => {}
                },
                Some(Ok(Event::Resize(w, h))) => effects = app.update(Msg::Resize(w, h)),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            },
            event = agent_events.recv(), if events_open => match event {
                Ok(event) => effects = app.update(Msg::Agent(event)),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "TUI lagged behind agent events");
                }
                Err(broadcast::error::RecvError::Closed) => events_open = false,
            },
            _ = tick.tick() => {
                effects = app.update(Msg::Tick);
                if app.dirty {
                    app.dirty = false;
                    terminal.draw(|f| view(&app, f))?;
                }
            }
        }

        for effect in effects {
            match effect {
                Effect::Submit(text) => {
                    let cancel = CancellationToken::new();
                    current_cancel = Some(cancel.clone());
                    if cmd_tx
                        .send(DriverCmd::Input { text, cancel })
                        .await
                        .is_err()
                    {
                        app.update(Msg::Agent(AgentEvent::Error {
                            message: "agent driver stopped".into(),
                            fatal: true,
                        }));
                    }
                }
                Effect::SetMode(mode) => {
                    let _ = cmd_tx.send(DriverCmd::SetMode(mode)).await;
                }
                Effect::SetThink(on) => {
                    let _ = cmd_tx.send(DriverCmd::SetThink(on)).await;
                }
                Effect::ListModels => {
                    app.push_notice(switcher.catalog.listing(&app.model_name));
                }
                Effect::SwitchModel(arg) => {
                    let name = switcher.catalog.pick(&arg);
                    match provider_factory::resolve_switch(&switcher.config, name) {
                        Ok(target) => {
                            *switcher.main_model.lock().unwrap() = target.model.clone();
                            let _ = cmd_tx.send(DriverCmd::SetModel(target)).await;
                        }
                        Err(e) => {
                            app.update(Msg::Agent(AgentEvent::Error {
                                message: format!("cannot switch to `{name}`: {e}"),
                                fatal: false,
                            }));
                        }
                    }
                }
                Effect::Reply {
                    request_id,
                    decision,
                } => {
                    let _ = replies
                        .send(FrontendReply::Permission {
                            request_id,
                            decision,
                        })
                        .await;
                }
                Effect::CancelTurn => {
                    if let Some(cancel) = &current_cancel {
                        cancel.cancel();
                    }
                }
                Effect::Quit => {
                    if let Some(cancel) = &current_cancel {
                        cancel.cancel();
                    }
                    return Ok(());
                }
            }
        }
    }
}

fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

/// Best-effort teardown; safe to call twice (normal exit and panic hook).
fn restore_terminal() {
    let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}
