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

use app::{App, Effect, Msg, TalkCmd, VoiceMsg};
use driver::DriverCmd;
use view::view;

pub use app::{ContextBreakdown, ScrutinyView, SessionInfo};
pub use picker::{PickerOptions, pick_model};

const TICK: Duration = Duration::from_millis(33);

/// What `/talk`'s setup task and listener send the event loop. Every report
/// carries the generation of the `/talk` that produced it, so reports from a
/// start the user already cancelled are dropped.
#[cfg_attr(not(feature = "voice"), allow(dead_code))]
enum VoiceIn {
    Ui {
        generation: u64,
        msg: VoiceMsg,
    },
    #[cfg(feature = "voice")]
    Started {
        generation: u64,
        /// Boxed: the listener and speaker are large next to `Ui`.
        voice: Box<StartedVoice>,
    },
}

/// A `/talk` that finished starting.
#[cfg(feature = "voice")]
struct StartedVoice {
    listener: rocinante_voice::Listener,
    mic: String,
    /// Phonon and the speakers, when replies can be read aloud.
    speaker: Option<(rocinante_voice::Speaker, String)>,
}

/// The event loop's side of `/talk`.
#[derive(Default)]
struct Voice {
    /// Bumped by every start and stop.
    generation: u64,
    /// A start is downloading or loading.
    starting: bool,
    #[cfg(feature = "voice")]
    listener: Option<rocinante_voice::Listener>,
    #[cfg(feature = "voice")]
    speaker: Option<rocinante_voice::Speaker>,
    /// Reply text waiting to become whole sentences.
    #[cfg(feature = "voice")]
    sentences: rocinante_voice::speech_text::Splitter,
    /// Read replies aloud (`/talk quiet` turns it off).
    #[cfg_attr(not(feature = "voice"), allow(dead_code))]
    aloud: bool,
}

impl Voice {
    /// Feed the reply as it streams to the speaker, a sentence at a time.
    #[cfg(feature = "voice")]
    fn on_agent_event(&mut self, event: &AgentEvent) {
        let Some(speaker) = self.speaker.as_ref().filter(|_| self.aloud) else {
            return;
        };
        let ready = match event {
            AgentEvent::TurnStarted { .. } => {
                self.sentences = Default::default();
                return;
            }
            AgentEvent::AssistantText { delta } => self.sentences.push(delta),
            AgentEvent::TurnFinished { .. } => self.sentences.finish(),
            _ => return,
        };
        for sentence in ready {
            speaker.say(sentence);
        }
    }

    #[cfg(not(feature = "voice"))]
    fn on_agent_event(&mut self, _event: &AgentEvent) {}

    /// The microphone hears the speakers: pause it while a reply plays.
    #[cfg(feature = "voice")]
    fn on_voice_msg(&self, msg: &VoiceMsg) {
        if let Some(listener) = &self.listener {
            match msg {
                VoiceMsg::Speaking => listener.set_paused(true),
                VoiceMsg::SpeechDone => listener.set_paused(false),
                _ => {}
            }
        }
    }

    #[cfg(not(feature = "voice"))]
    fn on_voice_msg(&self, _msg: &VoiceMsg) {}
}

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
    let (voice_tx, mut voice_rx) = mpsc::unbounded_channel::<VoiceIn>();
    let mut voice = Voice::default();

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
                Ok(event) => {
                    voice.on_agent_event(&event);
                    effects = app.update(Msg::Agent(event));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "TUI lagged behind agent events");
                }
                Err(broadcast::error::RecvError::Closed) => events_open = false,
            },
            Some(input) = voice_rx.recv() => match input {
                VoiceIn::Ui { generation, msg } if generation == voice.generation => {
                    if matches!(msg, VoiceMsg::Failed(_)) {
                        voice.starting = false;
                    }
                    voice.on_voice_msg(&msg);
                    effects = app.update(Msg::Voice(msg));
                }
                VoiceIn::Ui { .. } => {}
                #[cfg(feature = "voice")]
                VoiceIn::Started { generation, voice: started } => {
                    let StartedVoice { listener, mic, speaker } = *started;
                    if generation == voice.generation && app.voice.is_some() {
                        voice.starting = false;
                        // The name may have changed while the models loaded.
                        let name = app.voice.as_ref().and_then(|v| v.name.clone());
                        listener.set_prompt(rocinante_voice::prompt_for(name.as_deref()));
                        voice.listener = Some(listener);
                        let (speaker, speakers) = match speaker {
                            Some((speaker, out)) => (Some(speaker), Some(out)),
                            None => (None, None),
                        };
                        voice.speaker = speaker;
                        voice.aloud = voice.speaker.is_some();
                        voice.sentences = Default::default();
                        let chat = rocinante_core::prompt::VoiceChat { name, aloud: voice.aloud };
                        let _ = cmd_tx.send(DriverCmd::SetVoice(Some(chat))).await;
                        effects = app.update(Msg::Voice(VoiceMsg::Ready { mic, speakers }));
                    } else {
                        stop_listener(listener);
                        if let Some((speaker, _)) = speaker {
                            tokio::task::spawn_blocking(move || drop(speaker));
                        }
                    }
                }
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
                Effect::Talk(cmd) => {
                    // `[voice]` is read fresh, so a `/config` change applies
                    // to the next /talk without a restart.
                    let voice_cfg = rocinante_core::config::load(&switcher.project_dir)
                        .map(|c| c.voice)
                        .unwrap_or_else(|_| switcher.config.voice.clone());
                    talk(&mut app, &mut voice, cmd, voice_cfg, &voice_tx, &cmd_tx).await;
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

/// `/talk` start (or new speaker) and stop.
#[cfg(feature = "voice")]
async fn talk(
    app: &mut App,
    voice: &mut Voice,
    cmd: TalkCmd,
    voice_cfg: rocinante_core::config::VoiceConfig,
    tx: &mpsc::UnboundedSender<VoiceIn>,
    cmd_tx: &mpsc::Sender<DriverCmd>,
) {
    use rocinante_core::prompt::VoiceChat;
    match cmd {
        TalkCmd::Stop => {
            voice.generation += 1;
            voice.starting = false;
            if let Some(listener) = voice.listener.take() {
                stop_listener(listener);
            }
            if let Some(speaker) = voice.speaker.take() {
                tokio::task::spawn_blocking(move || drop(speaker));
            }
            let _ = cmd_tx.send(DriverCmd::SetVoice(None)).await;
        }
        TalkCmd::Hush => {
            voice.sentences = Default::default();
            if let Some(speaker) = &voice.speaker {
                speaker.stop();
            }
        }
        TalkCmd::Aloud(on) => {
            if on && voice.speaker.is_none() {
                app.push_notice(
                    "🔇 no voice to read replies with — no speakers were found when /talk started",
                );
                if let Some(v) = &mut app.voice {
                    v.aloud = false;
                }
                return;
            }
            voice.aloud = on;
            voice.sentences = Default::default();
            if !on && let Some(speaker) = &voice.speaker {
                speaker.stop();
            }
            if let Some(listener) = &voice.listener {
                listener.set_paused(false);
            }
            let name = app.voice.as_ref().and_then(|v| v.name.clone());
            let chat = VoiceChat { name, aloud: on };
            let _ = cmd_tx.send(DriverCmd::SetVoice(Some(chat))).await;
        }
        TalkCmd::Start(name) => {
            if let Some(listener) = &voice.listener {
                listener.set_prompt(rocinante_voice::prompt_for(name.as_deref()));
                let chat = VoiceChat {
                    name,
                    aloud: voice.aloud,
                };
                let _ = cmd_tx.send(DriverCmd::SetVoice(Some(chat))).await;
                return;
            }
            // Already starting: the new name is picked up when it is ready.
            if voice.starting {
                return;
            }
            voice.generation += 1;
            voice.starting = true;
            tokio::spawn(start_voice(voice.generation, voice_cfg, tx.clone()));
        }
    }
}

#[cfg(not(feature = "voice"))]
async fn talk(
    app: &mut App,
    voice: &mut Voice,
    cmd: TalkCmd,
    _voice_cfg: rocinante_core::config::VoiceConfig,
    _tx: &mpsc::UnboundedSender<VoiceIn>,
    _cmd_tx: &mpsc::Sender<DriverCmd>,
) {
    voice.generation += 1;
    if let TalkCmd::Start(_) = cmd {
        app.update(Msg::Voice(VoiceMsg::Failed(
            "/talk isn't in this build: it was built without the embedded llama.cpp, which \
             carries the speech recognition"
                .into(),
        )));
    }
}

/// Check the machine, fetch the models on first use, load them and open
/// the microphone, reporting each step; ends with `Started` or `Failed`.
#[cfg(feature = "voice")]
async fn start_voice(
    generation: u64,
    cfg: rocinante_core::config::VoiceConfig,
    tx: mpsc::UnboundedSender<VoiceIn>,
) {
    use rocinante_voice::models::{self, SttModel};
    use rocinante_voice::{Listener, ListenerEvent, ListenerOptions};

    let ui = |msg: VoiceMsg| {
        let _ = tx.send(VoiceIn::Ui { generation, msg });
    };
    let mic = match tokio::task::spawn_blocking(rocinante_voice::check).await {
        Ok(Ok(mic)) => mic,
        Ok(Err(why)) => return ui(VoiceMsg::Failed(why.to_string())),
        Err(e) => return ui(VoiceMsg::Failed(format!("/talk failed to start: {e}"))),
    };
    let model = SttModel::parse(&cfg.stt_model).unwrap_or_else(|| {
        ui(VoiceMsg::Notice(format!(
            "[voice] stt_model = \"{}\" isn't one /talk knows ({}) — using base.en",
            cfg.stt_model,
            SttModel::NAMES.join(", ")
        )));
        SttModel::Base
    });
    let Some(dir) = models::dir() else {
        return ui(VoiceMsg::Failed(
            "/talk: no home directory to keep its speech models in".into(),
        ));
    };
    // Speakers decide whether Phonon is needed at all.
    let speakers = if cfg.speak {
        match tokio::task::spawn_blocking(rocinante_voice::find_speakers).await {
            Ok(Ok(out)) => Some(out),
            Ok(Err(why)) => {
                ui(VoiceMsg::Notice(format!(
                    "🔇 replies stay on screen: {why}"
                )));
                None
            }
            Err(_) => None,
        }
    } else {
        None
    };
    let mut files = vec![models::VAD, model.file()];
    if speakers.is_some() {
        files.extend(models::PHONON);
    }
    let missing = models::missing_bytes(&dir, &files);
    if missing > 0 {
        ui(VoiceMsg::Notice(format!(
            "🎙 first /talk: downloading its speech models ({}) into {}",
            models::megabytes(missing),
            dir.display()
        )));
    }
    let mut paths = Vec::new();
    let mut fetched = 0u64;
    for file in files {
        let needed = models::installed(&dir, &file).is_none();
        let mut last = u64::MAX;
        let got = models::fetch(&dir, &file, |done, _| {
            let pct = (fetched + done) * 100 / missing.max(1);
            if pct != last {
                last = pct;
                ui(VoiceMsg::Progress(format!(
                    "downloading speech models {pct}%"
                )));
            }
        })
        .await;
        match got {
            Ok(path) => paths.push(path),
            Err(e) => return ui(VoiceMsg::Failed(format!("/talk: {e}"))),
        }
        if needed {
            fetched += file.bytes;
        }
    }
    ui(VoiceMsg::Progress("loading the speech models".into()));
    let opts = ListenerOptions {
        vad: paths[0].clone(),
        model: paths[1].clone(),
        prompt: rocinante_voice::prompt_for(None),
        pause_ms: cfg.pause_ms,
    };
    let mic_name = mic.name.clone();
    let events = tx.clone();
    let started = tokio::task::spawn_blocking(move || {
        Listener::start(&mic, opts, move |event| {
            let msg = match event {
                ListenerEvent::Hearing => VoiceMsg::Hearing,
                ListenerEvent::Transcribing => VoiceMsg::Transcribing,
                ListenerEvent::Heard(text) => VoiceMsg::Heard(text),
                ListenerEvent::Ignored => VoiceMsg::Ignored,
                ListenerEvent::Muted => VoiceMsg::Muted,
                ListenerEvent::Failed(why) => VoiceMsg::Failed(format!("/talk stopped: {why}")),
            };
            let _ = events.send(VoiceIn::Ui { generation, msg });
        })
    })
    .await;
    // The voice: a failure here leaves listening on, replies on screen.
    let speaker = match speakers {
        Some(out) => {
            let phonon = dir.join(models::PHONON_DIR);
            let events = tx.clone();
            let name = out.name.clone();
            let loaded = tokio::task::spawn_blocking(move || {
                rocinante_voice::Speaker::start(&phonon, &out, move |event| {
                    let msg = match event {
                        rocinante_voice::SpeakerEvent::Speaking => VoiceMsg::Speaking,
                        rocinante_voice::SpeakerEvent::Done => VoiceMsg::SpeechDone,
                    };
                    let _ = events.send(VoiceIn::Ui { generation, msg });
                })
            })
            .await;
            match loaded {
                Ok(Ok(speaker)) => Some((speaker, name)),
                Ok(Err(e)) => {
                    ui(VoiceMsg::Notice(format!("🔇 replies stay on screen: {e}")));
                    None
                }
                Err(_) => None,
            }
        }
        None => None,
    };
    match started {
        Ok(Ok(listener)) => {
            let _ = tx.send(VoiceIn::Started {
                generation,
                voice: Box::new(StartedVoice {
                    listener,
                    mic: mic_name,
                    speaker,
                }),
            });
        }
        Ok(Err(e)) => ui(VoiceMsg::Failed(format!("/talk could not start: {e}"))),
        Err(e) => ui(VoiceMsg::Failed(format!("/talk could not start: {e}"))),
    }
}

/// Stopping joins the listening thread, which may be mid-sentence: do it
/// off the UI thread.
#[cfg(feature = "voice")]
fn stop_listener(listener: rocinante_voice::Listener) {
    tokio::task::spawn_blocking(move || drop(listener));
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
