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
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream,
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

pub use app::{ContextBreakdown, ScrutinyView, SessionInfo};
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
    /// Persistent subagents for `@name` messages; None when the crew is off.
    pub crew: Option<std::sync::Arc<rocinante_core::crew::CrewManager>>,
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
#[allow(clippy::too_many_arguments)]
pub async fn run(
    agent: Agent,
    frontend: FrontendHandle,
    events: broadcast::Sender<AgentEvent>,
    model: String,
    notices: Vec<String>,
    switcher: ModelSwitcher,
    session_info: SessionInfo,
    project_files: Vec<String>,
) -> anyhow::Result<()> {
    let mode = agent.mode();
    // Shift+Tab flips this directly, so a mode switch applies mid-turn
    // instead of waiting for the driver to be free.
    let mode_handle = agent.mode_handle();
    let effort = agent.effort();
    let think = agent.think();
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let loop_events = events.clone();
    let driver = driver::spawn(agent, cmd_rx, events);

    install_panic_hook();
    let mouse = switcher.config.defaults.mouse;
    let mut terminal = setup_terminal(mouse)?;
    let size = terminal.size()?;
    let resumed = session_info.resumed;
    let mut app = App::new(model, mode, (size.width, size.height), notices)
        .with_session(session_info)
        .with_files(project_files);
    app.effort = effort;
    app.think = think;
    app.mouse = mouse;
    if resumed {
        app = app.with_resumed();
    }
    let result = event_loop(
        &mut terminal,
        frontend,
        cmd_tx,
        app,
        switcher,
        loop_events,
        mode_handle,
    )
    .await;
    restore_terminal();
    // Driver exits as soon as its channel drains; brief grace, then abandon
    // it (and any in-flight background memory update) so quit is instant.
    let _ = tokio::time::timeout(Duration::from_secs(2), driver).await;
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    frontend: FrontendHandle,
    cmd_tx: mpsc::Sender<DriverCmd>,
    mut app: App,
    mut switcher: ModelSwitcher,
    events: broadcast::Sender<AgentEvent>,
    mode_handle: rocinante_core::agent::ModeHandle,
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
                Some(Ok(Event::Mouse(m))) => effects = app.update(Msg::Mouse(m)),
                Some(Ok(Event::Paste(text))) => effects = app.update(Msg::Paste(text)),
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
                // Straight to the agent's live mode: the next tool call
                // (even in the running turn) sees it.
                Effect::SetMode(mode) => mode_handle.set(mode),
                Effect::SetThink(on) => {
                    let _ = cmd_tx.send(DriverCmd::SetThink(on)).await;
                }
                Effect::SetMouse(on) => {
                    let _ = if on {
                        execute!(io::stdout(), EnableMouseCapture)
                    } else {
                        execute!(io::stdout(), DisableMouseCapture)
                    };
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
                            let sub = provider_factory::default_sub_model(&switcher.config, Some(&name));
                            let _ = cmd_tx.send(DriverCmd::SetSubModel(sub)).await;
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
                    let sub = provider_factory::default_sub_model(&switcher.config, None);
                    let _ = cmd_tx.send(DriverCmd::SetSubModel(sub)).await;
                }
                Effect::Scrutiny(cmd) => match cmd {
                    app::ScrutinyCmd::Mode { answers, work } => {
                        let _ = cmd_tx.send(DriverCmd::ScrutinyMode { answers, work }).await;
                    }
                    app::ScrutinyCmd::Show(show) => {
                        let _ = cmd_tx.send(DriverCmd::ScrutinyShow(show)).await;
                    }
                    app::ScrutinyCmd::Critic(None) => {
                        app.scrutiny.critic = None;
                        app.push_notice("critic cleared — the main model reviews its own answers");
                        let _ = cmd_tx.send(DriverCmd::SetCritic(None)).await;
                    }
                    app::ScrutinyCmd::Critic(Some(arg)) => {
                        let name = switcher.catalog.pick(&arg).to_string();
                        let main_tag = switcher.main_model.lock().unwrap().clone();
                        match provider_factory::critic_model(&switcher.config, &name, &main_tag) {
                            Ok(critic) => {
                                app.scrutiny.critic = Some(name.clone());
                                app.push_notice(format!("critic: {name}"));
                                let _ = cmd_tx.send(DriverCmd::SetCritic(Some(critic))).await;
                            }
                            Err(e) => {
                                app.update(Msg::Agent(AgentEvent::Error {
                                    message: format!("cannot use `{name}` as critic: {e}"),
                                    fatal: false,
                                }));
                            }
                        }
                    }
                },
                Effect::Toolbox(args) => {
                    app.push_notice(format!(
                        "toolbox {}…",
                        if args.is_empty() { "status" } else { args.as_str() }
                    ));
                    // Downloads can take a while: run in the background and
                    // report through the event channel.
                    let config = std::sync::Arc::clone(&switcher.config);
                    let events = events.clone();
                    tokio::spawn(async move {
                        let lines = tokio::time::timeout(
                            Duration::from_secs(900),
                            rocinante_core::toolbox::user_command(&config, &args),
                        )
                        .await
                        .unwrap_or_else(|_| vec!["toolbox: timed out".into()]);
                        for message in lines {
                            let _ = events.send(AgentEvent::Notice { message });
                        }
                    });
                }
                Effect::CrewSend { to, text } => match &switcher.crew {
                    Some(crew) => {
                        if let Err(e) = crew.send_from_user(&to, &text) {
                            app.push_notice(e);
                        }
                    }
                    None => app.push_notice("the crew is off ([crew] persistent = false)"),
                },
                Effect::CrewStop(name) => match &switcher.crew {
                    Some(crew) => {
                        let line = crew.stop(&name);
                        app.push_notice(line);
                    }
                    None => app.push_notice("the crew is off ([crew] persistent = false)"),
                },
                Effect::CrewReport(name) => match &switcher.crew {
                    Some(crew) => match crew.request_report(&name) {
                        Ok(()) => app.push_notice(format!(
                            "{name} is writing a report; it reaches the main agent before your next message"
                        )),
                        Err(e) => app.push_notice(e),
                    },
                    None => app.push_notice("the crew is off ([crew] persistent = false)"),
                },
                Effect::Compact => {
                    let _ = cmd_tx.send(DriverCmd::Compact).await;
                }
                Effect::Clear(all) => {
                    // Reset the transcript and the context gauge; the agent
                    // drops its messages when it processes the command.
                    app.cells.clear();
                    app.active_agents.clear();
                    app.running_agents.clear();
                    app.last_prompt_tokens = 0;
                    app.scroll = 0;
                    // push_notice also clears live_text and marks dirty.
                    app.push_notice(if all {
                        "context and BRAINBOX.md cleared"
                    } else {
                        "context cleared"
                    });
                    let _ = cmd_tx.send(DriverCmd::Clear { all }).await;
                }
                Effect::OpenContextDashboard => app.open_context_dashboard(),
                Effect::Uninstall { confirmed, purge } => {
                    if !confirmed {
                        for line in rocinante_core::selfupdate::uninstall_preview(purge) {
                            app.push_notice(line);
                        }
                    } else {
                        match rocinante_core::selfupdate::uninstall_apply(purge) {
                            Ok(outcome) => {
                                for line in outcome.lines {
                                    app.push_notice(line);
                                }
                                if outcome.removed {
                                    // Show the result, then exit hard — killing any
                                    // background memory update so a --purge doesn't
                                    // recreate the files we just deleted.
                                    terminal.draw(|f| view(&app, f))?;
                                    restore_terminal();
                                    std::process::exit(0);
                                }
                            }
                            Err(e) => app.push_notice(format!("uninstall failed: {e:#}")),
                        }
                    }
                }
                Effect::Verify => {
                    app.push_notice("verifying the last task…");
                    let cancel = CancellationToken::new();
                    current_cancel = Some(cancel.clone());
                    let _ = cmd_tx.send(DriverCmd::Verify { cancel }).await;
                }
                Effect::Trust => match rocinante_core::config::trust::trust(&switcher.project_dir) {
                    Ok(true) => app.push_notice(
                        "project trusted — restart rocinante to apply its config (providers, MCP, permissions…)",
                    ),
                    Ok(false) => app.push_notice("project is already trusted"),
                    Err(e) => app.push_notice(format!("could not record trust: {e}")),
                },
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
                    let entries = switcher.catalog.picker_entries();
                    let current = switcher.catalog.current_index(&app.model_name);
                    app.open_model_picker(entries, current);
                }
                Effect::SwitchModel(arg) => {
                    if let Err(notice) = refresh_switcher(&mut switcher).await {
                        app.push_notice(notice);
                    }
                    let name = switcher.catalog.pick(&arg).to_string();
                    switch_model(&mut app, &switcher, &cmd_tx, &name).await;
                }
                Effect::LocalModel(cmd) => {
                    local_model(&mut app, &mut switcher, &cmd_tx, cmd).await;
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

/// Resolve `name` and hot-switch the main model to it.
async fn switch_model(
    app: &mut App,
    switcher: &ModelSwitcher,
    cmd_tx: &mpsc::Sender<DriverCmd>,
    name: &str,
) {
    match provider_factory::resolve_switch(&switcher.config, name) {
        Ok(target) => {
            // The ctx gauge should reflect the new model's window.
            if let Some(n) = target.params.num_ctx {
                app.session.num_ctx = n;
            }
            *switcher.main_model.lock().unwrap() = target.model.clone();
            // Remember the picked name (not the wire tag) so an alias — and
            // its num_ctx — survives restarts.
            rocinante_core::state::save_last_model(name);
            let local = target.provider.id() == rocinante_core::local_models::PROVIDER
                || provider_factory::is_llama_provider(&switcher.config, target.provider.id());
            let _ = cmd_tx.send(DriverCmd::SetModel(target)).await;
            if local {
                app.push_notice(
                    "runs in Rocinante's embedded llama.cpp — the first reply loads the model",
                );
            }
        }
        Err(e) => {
            app.update(Msg::Agent(AgentEvent::Error {
                message: format!("cannot switch to `{name}`: {e}"),
                fatal: false,
            }));
        }
    }
}

/// `/localmodel …`: list the GGUF folder (asking for it the first time),
/// remember a folder, switch to a local model, or save an alias.
async fn local_model(
    app: &mut App,
    switcher: &mut ModelSwitcher,
    cmd_tx: &mpsc::Sender<DriverCmd>,
    cmd: app::LocalModelCmd,
) {
    use app::LocalModelCmd;
    use rocinante_core::local_models as lm;
    if let Err(notice) = refresh_switcher(switcher).await {
        app.push_notice(notice);
    }
    let absolute = |raw: &str| {
        let p = lm::expand_tilde(raw);
        if p.is_relative() {
            std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
        } else {
            p
        }
    };
    match cmd {
        LocalModelCmd::List => list_local_models(app, switcher),
        LocalModelCmd::SetDir(raw) => {
            let dir = absolute(&raw);
            if !dir.is_dir() {
                app.push_notice(format!("{} is not a folder", dir.display()));
                app.ask_local_dir();
                return;
            }
            match lm::save_model_dir(&dir) {
                Ok(path) => app.push_notice(format!(
                    "model folder: {} — saved to {}",
                    dir.display(),
                    path.display()
                )),
                Err(e) => app.push_notice(format!(
                    "could not save the model folder ({e}) — using it for this session"
                )),
            }
            if let Err(notice) = refresh_switcher(switcher).await {
                app.push_notice(notice);
            }
            // Whatever the save did, this session uses the folder.
            if lm::model_dir(&switcher.config).as_deref() != Some(dir.as_path()) {
                let mut config = (*switcher.config).clone();
                config.defaults.local_model_dir = Some(dir.display().to_string());
                switcher.config = std::sync::Arc::new(config);
            }
            list_local_models(app, switcher);
        }
        LocalModelCmd::Use(arg) => {
            let name = if lm::is_path_like(&arg) {
                format!("{}/{}", lm::PROVIDER, absolute(&arg).display())
            } else if switcher.config.models.contains_key(&arg) {
                arg
            } else {
                format!("{}/{arg}", lm::PROVIDER)
            };
            switch_model(app, switcher, cmd_tx, &name).await;
        }
        LocalModelCmd::Alias { alias, model } => {
            let file = if lm::is_path_like(&model) {
                let p = absolute(&model);
                if p.is_file() {
                    Ok(p)
                } else {
                    Err(format!("no model file at {}", p.display()))
                }
            } else {
                lm::resolve(&model, lm::model_dir(&switcher.config).as_deref())
            };
            let saved = file.and_then(|f| lm::save_alias(&alias, &f).map(|cfg| (f, cfg)));
            match saved {
                Ok((file, cfg)) => {
                    app.push_notice(format!(
                        "alias {alias} → {} (saved to {}) — `/model {alias}` or `/localmodel {alias}` switches to it",
                        file.display(),
                        cfg.display()
                    ));
                    if let Err(notice) = refresh_switcher(switcher).await {
                        app.push_notice(notice);
                    }
                }
                Err(e) => app.push_notice(format!("alias not saved: {e}")),
            }
        }
    }
}

fn list_local_models(app: &mut App, switcher: &ModelSwitcher) {
    use rocinante_core::local_models as lm;
    let Some(dir) = lm::model_dir(&switcher.config) else {
        app.ask_local_dir();
        return;
    };
    if !dir.is_dir() {
        app.push_notice(format!("the model folder {} does not exist", dir.display()));
        app.ask_local_dir();
        return;
    }
    let models = lm::list(&dir, &switcher.config);
    let current = models.iter().position(|m| {
        app.model_name == m.switch_name()
            || app.model_name == format!("{}/{}", lm::PROVIDER, m.name)
    });
    let entries = models
        .iter()
        .map(|m| provider_factory::ModelEntry {
            value: m.switch_name(),
            label: m.label(),
        })
        .collect();
    app.open_local_picker(&dir, entries, current);
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

fn setup_terminal(mouse: bool) -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Bracketed paste lets a multi-line clipboard paste arrive as one event
    // instead of a stream of keystrokes (which would submit on the first
    // newline).
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    // Mouse capture (`[defaults] mouse`, `/mouse`) makes the wheel scroll
    // whatever is under the pointer; the price is that selecting text needs
    // Shift+drag (Option+drag on macOS). Off, the terminal keeps native
    // click-drag selection and turns the wheel into arrow keys.
    if mouse {
        execute!(stdout, EnableMouseCapture)?;
    }
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

/// Best-effort teardown; safe to call twice (normal exit and panic hook).
fn restore_terminal() {
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info);
    }));
}
