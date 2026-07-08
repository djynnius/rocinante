mod repl;
mod setup;

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use rocinante_core::config::{self, Config, Mode, ProviderConfig};
use rocinante_core::provider_factory;
use rocinante_core::state;
use rocinante_providers::{ChatDelta, ChatRequest, Message};

#[derive(Parser)]
#[command(
    name = "rocinante",
    version,
    about = "An ironman suit for local models"
)]
struct Cli {
    /// Model alias (from [models]) or provider/model or bare Ollama name.
    #[arg(long, global = true)]
    model: Option<String>,

    /// Start in this mode: normal, auto, or plan.
    #[arg(long, global = true)]
    mode: Option<String>,

    /// Continue the most recent session in this project.
    #[arg(long, short = 'c')]
    r#continue: bool,

    /// Use the plain-terminal REPL instead of the TUI.
    #[arg(long)]
    no_tui: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// One-shot question: stream a completion and exit (M0 smoke test).
    Ask { prompt: Vec<String> },
    /// Show the resolved configuration.
    Config,
}

fn init_tracing() {
    let log_dir = dirs::home_dir()
        .map(|h| h.join(".rocinante/logs"))
        .unwrap_or_else(|| PathBuf::from(".rocinante-logs"));
    let _ = std::fs::create_dir_all(&log_dir);
    let file = tracing_appender::rolling::daily(log_dir, "rocinante.log");
    tracing_subscriber::fmt()
        .with_writer(file)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ROCINANTE_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    let config = config::load(&cwd)?;

    match cli.command {
        None => {
            let mode = match cli.mode.as_deref() {
                None => config.defaults.mode,
                Some("normal") => Mode::Normal,
                Some("auto") => Mode::Auto,
                Some("plan") => Mode::Plan,
                Some(other) => bail!("unknown mode `{other}` (normal | auto | plan)"),
            };
            let session = if cli.r#continue {
                setup::SessionChoice::Continue
            } else {
                setup::SessionChoice::New
            };
            let use_tui = std::io::stdout().is_terminal() && !cli.no_tui;
            let picker = startup_picker(use_tui, cli.no_tui);
            let model = resolve_startup_model(&config, cli.model.as_deref(), picker).await?;
            if use_tui {
                run_tui(&config, &model, mode, session).await
            } else {
                repl::run(&config, &model, mode, session).await
            }
        }
        Some(Command::Config) => {
            println!("{}", toml::to_string_pretty(&config)?);
            Ok(())
        }
        Some(Command::Ask { prompt }) => {
            let prompt = prompt.join(" ");
            if prompt.is_empty() {
                bail!("usage: rocinante ask <prompt>");
            }
            // `ask` is non-interactive: a model must come from the flag or the
            // remembered state, else the guidance error (no picker).
            let model = resolve_startup_model(&config, cli.model.as_deref(), Picker::None).await?;
            ask(&config, &model, &prompt).await
        }
    }
}

/// Which first-run picker to show when no model is remembered.
enum Picker {
    /// ratatui pre-session screen (TTY, TUI frontend).
    Tui,
    /// Numbered stdin menu (TTY, `--no-tui`).
    Repl,
    /// Non-interactive (piped, or `ask`): no picker, the guidance error.
    None,
}

/// Pick the interactive picker for the default (no-subcommand) path. When
/// stdout is not a TTY, or stdin is piped for the REPL menu, there is no way
/// to prompt, so return `Picker::None` (→ guidance error if a model is needed).
fn startup_picker(use_tui: bool, no_tui: bool) -> Picker {
    if use_tui {
        Picker::Tui
    } else if no_tui && std::io::stdout().is_terminal() && std::io::stdin().is_terminal() {
        Picker::Repl
    } else {
        Picker::None
    }
}

/// The model to start on:
/// 1. `--model` flag → use it (and remember it).
/// 2. else remembered `state.last_model` → use it.
/// 3. else interactive → run the picker, remember the choice.
/// 4. else → a clear guidance error (no hardcoded default).
async fn resolve_startup_model(
    config: &Config,
    model_flag: Option<&str>,
    picker: Picker,
) -> anyhow::Result<String> {
    if let Some(m) = model_flag {
        state::save_last_model(m);
        return Ok(m.to_string());
    }
    if let Some(m) = state::load().last_model {
        return Ok(m);
    }
    // Only build the option list (which probes Ollama) when we will prompt.
    let chosen = match picker {
        Picker::Tui => {
            let (models, providers) = picker_options(config).await;
            rocinante_tui::pick_model(rocinante_tui::PickerOptions { models, providers }).await?
        }
        Picker::Repl => {
            let (models, providers) = picker_options(config).await;
            repl::pick_model(models, providers).await?
        }
        Picker::None => bail!(
            "no model selected — run `rocinante` interactively to choose one, or pass --model <name>"
        ),
    };
    state::save_last_model(&chosen);
    Ok(chosen)
}

/// Build the picker option lists: local models (config aliases + discovered
/// Ollama tags) first, then the names of configured API providers (whose keys
/// were validated at load) as `provider/…` free-entry hints.
async fn picker_options(config: &Config) -> (Vec<String>, Vec<String>) {
    let models = provider_factory::catalog(config).await.entries;
    let mut providers: Vec<String> = config
        .providers
        .iter()
        .filter(|(_, p)| {
            matches!(
                p,
                ProviderConfig::Anthropic { .. }
                    | ProviderConfig::Gemini { .. }
                    | ProviderConfig::Openai { .. }
            )
        })
        .map(|(name, _)| name.clone())
        .collect();
    providers.sort();
    (models, providers)
}

async fn run_tui(
    config: &Config,
    model: &str,
    mode: Mode,
    session: setup::SessionChoice,
) -> anyhow::Result<()> {
    let s = setup::build(config, model, mode, session).await?;
    // The landing screen replaced the banner; only resume info seeds the
    // transcript now.
    let mut notices = Vec::new();
    if let Some(r) = &s.resume {
        notices.push(format!(
            "resuming session {} ({} messages)",
            r.session_id, r.message_count
        ));
        if r.mid_turn {
            notices.push(
                "note: previous session ended mid-turn; the model will pick up from the last completed step"
                    .into(),
            );
        }
    }
    let switcher = rocinante_tui::ModelSwitcher {
        config: s.config,
        catalog: s.catalog,
        main_model: s.main_model,
    };
    // MCP server connections must outlive the TUI (drop kills the servers).
    let _mcp_keepalive = s.mcp;
    // LSP clients too; graceful shutdown after the TUI exits so no server
    // processes are orphaned.
    let _lsp_keepalive = std::sync::Arc::clone(&s.lsp);
    let result = rocinante_tui::run(
        s.agent,
        s.frontend,
        s.events,
        s.model,
        notices,
        switcher,
        s.session_info,
    )
    .await;
    s.lsp.shutdown().await;
    result
}

async fn ask(config: &Config, alias: &str, prompt: &str) -> anyhow::Result<()> {
    let resolved = provider_factory::resolve(config, alias)
        .with_context(|| format!("cannot start model `{alias}`"))?;
    let model = resolved.model;

    let req = ChatRequest {
        model: model.model.clone(),
        messages: vec![Message::user(prompt)],
        tools: vec![],
        params: provider_factory::gen_params(config, &model, resolved.is_local),
        format: None,
    };

    let mut stream = resolved.provider.chat(req).await?;
    let mut stdout = std::io::stdout();
    while let Some(delta) = stream.next().await {
        match delta? {
            ChatDelta::Text(text) => {
                stdout.write_all(text.as_bytes())?;
                stdout.flush()?;
            }
            ChatDelta::Usage(usage) => {
                tracing::info!(
                    prompt_tokens = usage.prompt_tokens,
                    completion_tokens = usage.completion_tokens,
                    "usage"
                );
                eprintln!(
                    "\n[{}: {} prompt + {} completion tokens]",
                    model.model, usage.prompt_tokens, usage.completion_tokens
                );
            }
            ChatDelta::Done(_) => break,
            ChatDelta::Thinking(_) | ChatDelta::ToolCall(_) | ChatDelta::ToolCallPartial { .. } => {
            }
        }
    }
    println!();
    Ok(())
}
