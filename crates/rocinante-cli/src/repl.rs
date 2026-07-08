//! Plain-terminal REPL frontend: the debug harness and `--no-tui` mode.
//! Drives the agent purely through the AgentEvent/FrontendReply channels —
//! exactly the surface the TUI and a future server use.
//!
//! Stdin discipline: the main loop reads a line only between turns; while a
//! turn runs, stdin is free, so the event task can read the answer to a
//! permission prompt without contention.

use std::io::Write as _;
use std::sync::Arc;

use rocinante_core::agent::events::{AgentEvent, FrontendReply, PermissionDecision};
use rocinante_core::config::{Config, Mode};
use rocinante_core::interval;

use crate::setup::{self, SessionChoice};

/// One recurring `/loop` prompt; at most one per session.
struct LoopState {
    prompt: String,
    every: std::time::Duration,
    next_due: tokio::time::Instant,
}

pub async fn run(
    config: &Config,
    model: &str,
    mode: Mode,
    session_choice: SessionChoice,
) -> anyhow::Result<()> {
    let setup::FrontendSetup {
        mut agent,
        frontend,
        model,
        cwd,
        resume,
        catalog,
        main_model,
        mcp,
        lsp,
        ..
    } = setup::build(config, model, mode, session_choice).await?;
    // MCP server connections must outlive the session: dropping the manager
    // drops the transports, which kills the child servers mid-conversation.
    let _mcp_keepalive = mcp;
    // LSP clients likewise live for the session; shut down gracefully at
    // the end of run() so no server processes are orphaned.
    let _lsp_keepalive = Arc::clone(&lsp);
    if let Some(r) = &resume {
        println!(
            "resuming session {} ({} messages)",
            r.session_id, r.message_count
        );
        if r.mid_turn {
            println!(
                "\x1b[33mnote: previous session ended mid-turn; the model will pick up from the last completed step\x1b[0m"
            );
        }
    }

    // Ctrl+C cancels the running turn instead of killing the process.
    let interrupter = agent.interrupter();
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                break;
            }
            interrupter.interrupt();
        }
    });

    spawn_event_printer(frontend.events, frontend.replies.clone());

    println!(
        "rocinante · {model} · {mode:?} mode · {} (Ctrl+D or /quit to exit)",
        cwd.display()
    );

    let mut loop_state: Option<LoopState> = None;
    loop {
        // One blocking read per prompt display. When an armed loop fires we
        // keep selecting on this same handle (polling a JoinHandle is
        // cancel-safe), so a partially-typed line is never lost and the
        // prompt is not re-printed.
        let mut pending = spawn_read_line("\x1b[1m> \x1b[0m");
        let line = loop {
            let deadline = loop_state.as_ref().map(|l| l.next_due);
            tokio::select! {
                read = &mut pending => break read?,
                _ = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)),
                    if deadline.is_some() =>
                {
                    let armed = loop_state.as_mut().expect("deadline implies armed loop");
                    armed.next_due = tokio::time::Instant::now() + armed.every;
                    let prompt = armed.prompt.clone();
                    println!("\x1b[36m⟳ {prompt}\x1b[0m");
                    // Submitting runs the whole turn, exactly like typed
                    // input; stdin stays free for permission prompts.
                    if let Err(e) = agent.submit(&prompt).await {
                        eprintln!("\x1b[31merror: {e}\x1b[0m");
                    }
                }
            }
        };
        let Some(line) = line else {
            break;
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line == "/quit" || line == "/exit" {
            break;
        }
        // Split "/cmd arg" on the first whitespace so /mode can't swallow /model.
        let (cmd, arg) = match line.split_once(char::is_whitespace) {
            Some((c, a)) => (c, a.trim()),
            None => (line.as_str(), ""),
        };
        match cmd {
            "/mode" => {
                match arg {
                    "normal" => agent.set_mode(Mode::Normal),
                    "auto" => agent.set_mode(Mode::Auto),
                    "plan" => agent.set_mode(Mode::Plan),
                    "" => {}
                    other => {
                        eprintln!("unknown mode `{other}` (normal | auto | plan)");
                        continue;
                    }
                }
                println!("mode: {:?}", agent.mode());
                continue;
            }
            "/model" => {
                if arg.is_empty() {
                    println!("{}", catalog.listing(agent.model()));
                } else {
                    match setup::switch_model(config, &main_model, catalog.pick(arg)) {
                        Ok(target) => agent.set_model(target.provider, target.model, target.params),
                        Err(e) => eprintln!("\x1b[31m{e:#}\x1b[0m"),
                    }
                }
                continue;
            }
            "/loop" => {
                match arg {
                    "" => match &loop_state {
                        Some(l) => {
                            let left = l
                                .next_due
                                .saturating_duration_since(tokio::time::Instant::now());
                            println!(
                                "loop: every {} — {} (next in {})",
                                interval::display(l.every),
                                l.prompt,
                                interval::display(std::time::Duration::from_secs(
                                    left.as_secs().max(1)
                                ))
                            );
                        }
                        None => println!("no loop armed"),
                    },
                    "stop" => {
                        if loop_state.take().is_some() {
                            println!("loop stopped");
                        } else {
                            println!("no loop armed");
                        }
                    }
                    _ => {
                        let (spec, prompt) = match arg.split_once(char::is_whitespace) {
                            Some((s, p)) => (s, p.trim()),
                            None => (arg, ""),
                        };
                        if prompt.is_empty() {
                            eprintln!("usage: /loop <interval> <prompt> | /loop stop | /loop");
                            continue;
                        }
                        match interval::parse(spec) {
                            Ok(every) => {
                                println!(
                                    "loop armed: every {} — {prompt}",
                                    interval::display(every)
                                );
                                loop_state = Some(LoopState {
                                    prompt: prompt.to_string(),
                                    every,
                                    next_due: tokio::time::Instant::now() + every,
                                });
                            }
                            Err(e) => eprintln!("\x1b[31m{e}\x1b[0m"),
                        }
                    }
                }
                continue;
            }
            "/init" => {
                if let Err(e) = agent.submit(rocinante_core::prompt::init_prompt()).await {
                    eprintln!("\x1b[31merror: {e}\x1b[0m");
                }
                continue;
            }
            "/commit" => {
                if let Err(e) = agent.submit(rocinante_core::prompt::commit_prompt()).await {
                    eprintln!("\x1b[31merror: {e}\x1b[0m");
                }
                continue;
            }
            "/compact" => {
                // Success prints via the ContextCompacted event.
                if let Err(e) = agent.compact_now().await {
                    eprintln!("\x1b[33m{e:#}\x1b[0m");
                }
                continue;
            }
            "/think" => {
                match arg {
                    "on" => agent.set_think(true),
                    "off" => agent.set_think(false),
                    "" => {}
                    other => {
                        eprintln!("unknown /think arg `{other}` (on | off)");
                        continue;
                    }
                }
                println!("thinking: {}", if agent.think() { "on" } else { "off" });
                continue;
            }
            _ => {}
        }
        if let Err(e) = agent.submit(&line).await {
            eprintln!("\x1b[31merror: {e}\x1b[0m");
        }
        // Plan-mode exit flow: the plan is on screen — offer to run it.
        if agent.mode() == Mode::Plan {
            let answer = spawn_read_line(
                "\x1b[33mplan ready — [e]xecute (normal) · [a]uto · Enter to stay in plan: \x1b[0m",
            )
            .await?;
            let switched = match answer.as_deref().map(str::trim) {
                Some("e") | Some("E") => {
                    agent.set_mode(Mode::Normal);
                    true
                }
                Some("a") | Some("A") => {
                    agent.set_mode(Mode::Auto);
                    true
                }
                _ => false,
            };
            if switched {
                println!("mode: {:?}", agent.mode());
                if let Err(e) = agent
                    .submit("Proceed with the plan you just presented.")
                    .await
                {
                    eprintln!("\x1b[31merror: {e}\x1b[0m");
                }
            }
        }
    }
    if agent.has_brainbox() {
        println!("\x1b[90mupdating BRAINBOX.md…\x1b[0m");
        agent.finalize().await;
    }
    lsp.shutdown().await;
    println!();
    Ok(())
}

/// First-run model picker for the `--no-tui` interactive path: a numbered
/// stdin menu. `models` (Ollama tags + config aliases) are numbered and
/// directly selectable; API providers are shown as `provider/…` hints. The
/// user may also type any number, a bare tag, or `provider/model`. Aborts
/// (Ctrl+D / EOF) return an error with the standard guidance.
pub async fn pick_model(models: Vec<String>, providers: Vec<String>) -> anyhow::Result<String> {
    use std::io::Write as _;

    let guidance =
        "no model selected — run `rocinante` interactively to choose one, or pass --model <name>";

    println!("\x1b[1mSelect a model\x1b[0m (remembered for next time):");
    for (i, m) in models.iter().enumerate() {
        println!("  {:>2}. {m}", i + 1);
    }
    if !providers.is_empty() {
        println!("  or type a full model for a configured API provider:");
        for p in &providers {
            println!("      {p}/<model>");
        }
    }
    println!("  or type any provider/model or tag.");

    loop {
        let models = models.clone();
        let line = tokio::task::spawn_blocking(move || {
            print!("\x1b[1mmodel> \x1b[0m");
            std::io::stdout().flush().ok();
            let mut buf = String::new();
            match std::io::stdin().read_line(&mut buf) {
                Ok(0) => None,
                Ok(_) => Some(buf),
                Err(_) => None,
            }
        })
        .await
        .unwrap_or(None);

        let Some(line) = line else {
            anyhow::bail!(guidance);
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // A number selects a listed model; out-of-range re-prompts.
        if let Ok(n) = trimmed.parse::<usize>() {
            match models.get(n.wrapping_sub(1)) {
                Some(m) => return Ok(m.clone()),
                None => {
                    eprintln!("\x1b[31mno option {n}\x1b[0m");
                    continue;
                }
            }
        }
        // Otherwise take it as a literal model name (tag or provider/model).
        return Ok(trimmed.to_string());
    }
}

/// Blocking line read off the runtime. Resolves to None on EOF. Returns the
/// JoinHandle so the caller can select against it repeatedly without losing
/// a partially-typed line.
fn spawn_read_line(prompt_text: &'static str) -> tokio::task::JoinHandle<Option<String>> {
    tokio::task::spawn_blocking(move || {
        print!("{prompt_text}");
        std::io::stdout().flush().ok();
        let mut buf = String::new();
        match std::io::stdin().read_line(&mut buf) {
            Ok(0) => None,
            Ok(_) => Some(buf),
            Err(_) => None,
        }
    })
}

fn spawn_event_printer(
    mut events: tokio::sync::broadcast::Receiver<AgentEvent>,
    replies: tokio::sync::mpsc::Sender<FrontendReply>,
) {
    tokio::spawn(async move {
        let mut mid_text = false;
        let end_text = |mid: &mut bool| {
            if *mid {
                println!();
                *mid = false;
            }
        };
        while let Ok(event) = events.recv().await {
            match event {
                AgentEvent::AssistantText { delta } => {
                    mid_text = true;
                    print!("{delta}");
                    std::io::stdout().flush().ok();
                }
                AgentEvent::Thinking { delta } => {
                    // Dim; shares the streaming-text line discipline.
                    mid_text = true;
                    print!("\x1b[2m{delta}\x1b[0m");
                    std::io::stdout().flush().ok();
                }
                AgentEvent::ToolCallStarted { summary, .. } => {
                    end_text(&mut mid_text);
                    println!("\x1b[36m⏺ {summary}\x1b[0m");
                }
                AgentEvent::ToolFinished {
                    output_preview,
                    is_error,
                    ..
                } => {
                    end_text(&mut mid_text);
                    let (mark, color) = if is_error {
                        ("✗", "\x1b[31m")
                    } else {
                        ("✓", "\x1b[32m")
                    };
                    let first = output_preview.lines().next().unwrap_or("(no output)");
                    println!("{color}  {mark} {first}\x1b[0m");
                }
                AgentEvent::ToolProgress { call_id, chunk } => {
                    // Subagent activity and streaming bash output, indented
                    // under the running tool call.
                    if call_id.starts_with("task[") {
                        end_text(&mut mid_text);
                        println!("\x1b[90m    {call_id} {chunk}\x1b[0m");
                    }
                }
                AgentEvent::ContextCompacted {
                    before_tokens,
                    after_tokens,
                } => {
                    end_text(&mut mid_text);
                    println!(
                        "\x1b[90m[context compacted: ~{before_tokens} → ~{after_tokens} tokens]\x1b[0m"
                    );
                }
                AgentEvent::ModelChanged { model } => {
                    end_text(&mut mid_text);
                    println!("\x1b[90m[model: {model} — context preserved]\x1b[0m");
                }
                AgentEvent::PermissionRequested {
                    request_id,
                    summary,
                    detail,
                    ..
                } => {
                    end_text(&mut mid_text);
                    if let Some(detail) = detail {
                        for line in detail.lines() {
                            let color = match line.as_bytes().first() {
                                Some(b'+') => "\x1b[32m",
                                Some(b'-') => "\x1b[31m",
                                Some(b'@') => "\x1b[36m",
                                _ => "\x1b[90m",
                            };
                            println!("{color}{line}\x1b[0m");
                        }
                    }
                    let decision = tokio::task::spawn_blocking(move || {
                        print!("\x1b[33m? {summary}\x1b[0m — allow? [y]es / [a]lways / [n]o: ");
                        std::io::stdout().flush().ok();
                        let mut buf = String::new();
                        std::io::stdin().read_line(&mut buf).ok();
                        match buf.trim().to_ascii_lowercase().as_str() {
                            "y" | "yes" => PermissionDecision::Allow,
                            "a" | "always" => PermissionDecision::AlwaysAllow,
                            _ => PermissionDecision::Deny,
                        }
                    })
                    .await
                    .unwrap_or(PermissionDecision::Deny);
                    let _ = replies
                        .send(FrontendReply::Permission {
                            request_id,
                            decision,
                        })
                        .await;
                }
                AgentEvent::Usage(u) => {
                    tracing::info!(u.prompt_tokens, u.completion_tokens, "usage");
                }
                AgentEvent::Error { message, .. } => {
                    end_text(&mut mid_text);
                    eprintln!("\x1b[31m! {message}\x1b[0m");
                }
                AgentEvent::TurnFinished { .. } => end_text(&mut mid_text),
                _ => {}
            }
        }
    });
}
