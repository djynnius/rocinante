//! Rocinante TUI (ratatui). Talks to the agent exclusively through the
//! AgentEvent/FrontendReply channel pair; the agent itself lives in a
//! driver task because `Agent::submit` needs `&mut self` for a whole turn.

mod app;
mod driver;
mod picker;
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

pub use app::SessionInfo;
pub use picker::{PickerOptions, pick_model};

const TICK: Duration = Duration::from_millis(33);

/// Everything `/model` needs: the config to resolve names against, the
/// switchable-model catalog, and the shared handle the task tool's VRAM
/// gate reads (kept in sync on every switch).
pub struct ModelSwitcher {
    pub config: std::sync::Arc<Config>,
    pub catalog: std::sync::Arc<ModelCatalog>,
    /// Project dir for mid-session config reloads (`config::load` layering).
    pub project_dir: std::path::PathBuf,
    pub main_model: std::sync::Arc<std::sync::Mutex<String>>,
    /// `/submodel` pin shared with the task tool.
    pub subagent_model: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

/// Reload the layered config and re-discover the model catalog so aliases
/// added mid-session (e.g. via /config) appear in /model without a restart.
/// On any failure the startup snapshots stay in place and the returned
/// notice says why. Only model switching (and the /submodel pin check) sees
/// the fresh config; the startup `Arc<Config>` held by the task tool and
/// skill rescan is intentionally untouched until the next launch.
async fn refresh_switcher(switcher: &mut ModelSwitcher) -> Result<(), String> {
    let fresh = rocinante_core::config::load(&switcher.project_dir).map_err(|e| {
        format!("config reload failed ({e}) — /model uses the config loaded at startup")
    })?;
    let catalog = tokio::time::timeout(Duration::from_secs(3), provider_factory::catalog(&fresh))
        .await
        .map_err(|_| "model discovery timed out — /model uses the startup catalog".to_string())?;
    switcher.config = std::sync::Arc::new(fresh);
    switcher.catalog = std::sync::Arc::new(catalog);
    Ok(())
}

/// Run the TUI until the user quits. `events` is a clone of the agent's
/// event sender so the driver can synthesize events (submit errors, the
/// TurnFinished a cancelled turn never emitted). `notices` seed the
/// transcript (session-resume info); `session_info` feeds the landing
/// screen and the sidebar.
pub async fn run(
    agent: Agent,
    frontend: FrontendHandle,
    events: broadcast::Sender<AgentEvent>,
    model: String,
    notices: Vec<String>,
    switcher: ModelSwitcher,
    session_info: SessionInfo,
) -> anyhow::Result<()> {
    let mode = agent.mode();
    let effort = agent.effort();
    let think = agent.think();
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let driver = driver::spawn(agent, cmd_rx, events);

    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let size = terminal.size()?;
    let resumed = session_info.resumed;
    let mut app =
        App::new(model, mode, (size.width, size.height), notices).with_session(session_info);
    app.effort = effort;
    app.think = think;
    if resumed {
        app = app.with_resumed();
    }
    let result = event_loop(&mut terminal, frontend, cmd_tx, app, switcher).await;
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
    mut app: App,
    mut switcher: ModelSwitcher,
) -> anyhow::Result<()> {
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
                Effect::SetEffort(e) => {
                    let _ = cmd_tx.send(DriverCmd::SetEffort(e)).await;
                }
                Effect::PinSubmodel(arg) => {
                    // Validate like /model does before pinning.
                    let name = switcher.catalog.pick(&arg).to_string();
                    match provider_factory::resolve(&switcher.config, &name) {
                        Ok(_) => {
                            *switcher.subagent_model.lock().unwrap() = Some(name.clone());
                            app.submodel = Some(name.clone());
                            app.push_notice(format!("subagents pinned to {name}"));
                        }
                        Err(e) => {
                            app.update(Msg::Agent(AgentEvent::Error {
                                message: format!("cannot pin subagents to `{name}`: {e}"),
                                fatal: false,
                            }));
                        }
                    }
                }
                Effect::UnpinSubmodel => {
                    *switcher.subagent_model.lock().unwrap() = None;
                    app.submodel = None;
                    app.push_notice("subagent pin cleared — profiles decide again");
                }
                Effect::Compact => {
                    let _ = cmd_tx.send(DriverCmd::Compact).await;
                }
                Effect::Update => {
                    app.push_notice("checking for updates…");
                    // Render the notice before the long await (redraws are
                    // frozen for the download's duration, bounded below).
                    terminal.draw(|f| view(&app, f))?;
                    app.dirty = false;
                    match tokio::time::timeout(Duration::from_secs(180), self_update()).await {
                        Ok(lines) => {
                            for line in lines {
                                app.push_notice(line);
                            }
                        }
                        Err(_) => app.push_notice("update timed out — try again"),
                    }
                }
                Effect::ListModels => {
                    if let Err(notice) = refresh_switcher(&mut switcher).await {
                        app.push_notice(notice);
                    }
                    let entries = switcher.catalog.entries.clone();
                    let current = app.model_name.clone();
                    app.open_model_picker(entries, current);
                }
                Effect::SwitchModel(arg) => {
                    if let Err(notice) = refresh_switcher(&mut switcher).await {
                        app.push_notice(notice);
                    }
                    let name = switcher.catalog.pick(&arg);
                    match provider_factory::resolve_switch(&switcher.config, name) {
                        Ok(target) => {
                            // The ctx gauge should reflect the new model's window.
                            if let Some(n) = target.params.num_ctx {
                                app.session.num_ctx = n;
                            }
                            *switcher.main_model.lock().unwrap() = target.model.clone();
                            // Remember the switch so the next launch starts here.
                            rocinante_core::state::save_last_model(&target.model);
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

/// The whole `/update` flow as user-facing lines. The binary swap never
/// touches the running process, so this is safe to run mid-session.
async fn self_update() -> Vec<String> {
    use rocinante_core::selfupdate::{self, UpdateCheck};
    let (exe, guard) = match selfupdate::current_exe_classified() {
        Ok(v) => v,
        Err(e) => return vec![format!("update failed: {e:#}")],
    };
    if let Some(g) = guard
        && g.blocks_update()
    {
        return vec![g.advice().to_string()];
    }
    match selfupdate::check().await {
        Ok(UpdateCheck::UpToDate { current }) => {
            vec![format!("already up to date (v{current})")]
        }
        Ok(UpdateCheck::Available {
            current,
            latest,
            tag,
        }) => {
            let mut lines = Vec::new();
            if let Some(g) = guard {
                lines.push(g.advice().to_string());
            }
            match selfupdate::apply(&tag, &exe).await {
                Ok(()) => lines.push(format!(
                    "updated v{current} → v{latest} — restart rocinante to run it"
                )),
                Err(e) => lines.push(format!("update failed: {e:#}")),
            }
            lines
        }
        Err(e) => vec![format!("update failed: {e:#}")],
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
