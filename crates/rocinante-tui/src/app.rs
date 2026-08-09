//! Elm-style core: Model ([`App`]) + [`Msg`] + [`App::update`], no terminal
//! I/O. Side effects are returned as [`Effect`]s for the event loop to run,
//! which keeps every state transition unit-testable.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use uuid::Uuid;

use rocinante_core::agent::events::{AgentEvent, PermissionDecision};
use rocinante_core::config::Mode;
use rocinante_core::interval;
use rocinante_providers::Effort;

// The markdown renderer lives in its own file. Declared here (rather than in
// `lib.rs`, which another workstream owns) via `#[path]` so this crate builds
// standalone; the module is only used by the transcript pipeline below.
#[path = "markdown.rs"]
mod markdown;

pub const INPUT_HEIGHT: u16 = 3;
/// Input content rows before the box stops growing and scrolls vertically.
pub const MAX_INPUT_ROWS: usize = 8;
pub const STATUS_HEIGHT: u16 = 1;
/// Fixed sidebar width when visible.
pub const SIDEBAR_WIDTH: u16 = 30;
/// Blank columns between the transcript and the sidebar (separation by
/// space, not a divider line).
pub const SIDEBAR_GAP: u16 = 2;
/// Outer margin around the chat view so text never hugs the terminal edge.
pub const MARGIN_X: u16 = 2;
pub const MARGIN_Y: u16 = 1;
/// Minimum frame width for the sidebar to appear.
pub const SIDEBAR_MIN_FRAME: u16 = 96;
/// Second Ctrl+C within this window quits.
pub const QUIT_WINDOW: Duration = Duration::from_secs(1);
/// Progress lines shown under a still-running tool card.
const PROGRESS_TAIL: usize = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    User(String),
    Assistant(String),
    /// Streaming model reasoning; rendered dim, never part of history.
    Thinking(String),
    Tool(ToolCell),
    Notice(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCell {
    pub call_id: String,
    pub summary: String,
    /// `task[...]` subagent activity lines, kept in full; the view shows a tail.
    pub progress: Vec<String>,
    /// First preview line + is_error, set by ToolFinished.
    pub result: Option<(String, bool)>,
}

/// Session metadata gathered at setup time by the CLI; feeds the landing
/// screen and the sidebar. Never updated by agent events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionInfo {
    /// User-defined subagent profile names from `[agents.*]`.
    pub agents: Vec<String>,
    /// Injected built-in agents (the crew); shown in the sidebar only while
    /// running or after acting this turn.
    pub builtin_agents: Vec<String>,
    /// User-created skill names (built-ins are usable but not listed).
    pub skills: Vec<String>,
    /// Count of registered `mcp__` tools.
    pub mcp_tools: usize,
    pub lsp_available: bool,
    /// Context window for the ctx gauge (resolved model's `num_ctx` or the
    /// config default).
    pub num_ctx: u32,
    /// CLI crate version, so the footer tracks the binary.
    pub version: &'static str,
    /// Resumed sessions open straight into the transcript.
    pub resumed: bool,
    /// Standing context-window cost by category, measured at setup — feeds
    /// the `/context` dashboard.
    pub breakdown: ContextBreakdown,
}

/// Token cost (estimated) of each always-on part of the context window,
/// computed once at setup from the assembled system prompt and tool schemas.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContextBreakdown {
    /// Base system prompt (rules, mode, cwd).
    pub system_base: usize,
    /// The skills index (trimmed one-liner per skill).
    pub skills_preamble: usize,
    /// Per-skill standing cost (its index line), highest first.
    pub per_skill: Vec<(String, usize)>,
    /// Model delegation briefing.
    pub delegation: usize,
    /// PILOT.md project instructions.
    pub pilot: usize,
    /// BRAINBOX head injected into the prompt (post-lazy).
    pub brainbox: usize,
    /// Global learned rules/preferences (LESSONS.md).
    pub lessons: usize,
    /// Tool JSON schemas.
    pub tools: usize,
    /// Full BRAINBOX text for the dashboard's memory panel (display only).
    pub memory_preview: Option<String>,
    /// LESSONS.md text for the dashboard's rules panel (display only).
    pub lessons_preview: Option<String>,
}

/// One recurring `/loop` prompt; at most one per session.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopSpec {
    pub prompt: String,
    pub every: Duration,
    pub next_due: Instant,
}

/// In-session `/model` picker overlay state.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelPicker {
    pub entries: Vec<String>,
    pub selected: usize,
    pub current: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionPrompt {
    pub request_id: Uuid,
    pub tool_name: String,
    pub summary: String,
    /// Rich preview (unified diff) rendered in the modal body.
    pub detail: Option<String>,
}

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    /// Bracketed-paste payload from the terminal, inserted at the cursor.
    Paste(String),
    Agent(AgentEvent),
    Resize(u16, u16),
    Tick,
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    Submit(String),
    SetMode(Mode),
    /// `/model` with no argument: show the catalog.
    ListModels,
    /// `/model <arg>`: resolve and hot-switch the main model.
    SwitchModel(String),
    /// `/think on|off`: toggle extended thinking.
    SetThink(bool),
    /// `/effort low|medium|high`: set the reasoning-effort tier.
    SetEffort(Effort),
    /// `/submodel <name>`: pin every subagent to one model.
    PinSubmodel(String),
    /// `/submodel clear`: release the pin.
    UnpinSubmodel,
    /// `/compact`: fold old turns into a summary now.
    Compact,
    /// `/clear` (`--all` also wipes BRAINBOX.md): reset the conversation.
    Clear(bool),
    /// `/verify`: quality-check the last task against its ask.
    Verify,
    /// `/update`: check GitHub for a newer release and self-update.
    Update,
    /// `/trust`: mark this project's config as trusted.
    Trust,
    /// `/context`: open the context-usage dashboard.
    OpenContextDashboard,
    Reply {
        request_id: Uuid,
        decision: PermissionDecision,
    },
    CancelTurn,
    Quit,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Input {
    chars: Vec<char>,
    cursor: usize,
}

impl Input {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }
    fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }
    /// Insert a run of text (a paste) at the cursor, newlines and all.
    fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.chars.insert(self.cursor, c);
            self.cursor += 1;
        }
    }
    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }
    fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.chars).into_iter().collect()
    }
    fn set(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }
}

/// Rows an input of `len` chars occupies when character-wrapped to `width`
/// columns, counting the `▌` cursor glyph; clamped to 1..=[`MAX_INPUT_ROWS`].
pub fn input_rows(len: usize, width: usize) -> usize {
    let width = width.max(1);
    (len + 1).div_ceil(width).clamp(1, MAX_INPUT_ROWS)
}

pub struct App {
    pub model_name: String,
    pub mode: Mode,
    pub cells: Vec<Cell>,
    /// Whether the last cell is an assistant cell still receiving deltas.
    live_text: bool,
    pub input: Input,
    pub running: bool,
    pub spinner: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Wrapped lines scrolled up from the bottom; 0 = follow new output.
    pub scroll: usize,
    pub permissions: VecDeque<PermissionPrompt>,
    /// Scroll offset (detail lines from the top) inside the permission modal.
    pub permission_scroll: usize,
    /// `/model` picker overlay, when open; captures the keyboard.
    pub model_picker: Option<ModelPicker>,
    /// Submitted prompts, oldest first, recalled with Up/Down.
    pub history: Vec<String>,
    /// Recall position in `history`; `None` = editing a fresh draft.
    history_pos: Option<usize>,
    /// Draft stashed when Up first enters history browsing.
    history_draft: String,
    /// Armed `/loop` recurring prompt, if any.
    pub loop_spec: Option<LoopSpec>,
    /// Extended thinking on (status-line indicator).
    pub think: bool,
    /// Reasoning-effort tier (status/sidebar indicator).
    pub effort: Effort,
    /// `/submodel` pin (sidebar indicator); the shared handle is the truth.
    pub submodel: Option<String>,
    /// Setup-time metadata for the landing screen and sidebar.
    pub session: SessionInfo,
    /// False until the first submit or agent event; the view renders the
    /// landing screen while unset.
    pub interacted: bool,
    /// Subagent profiles seen active (`task[…]` progress) this turn.
    pub active_agents: HashSet<String>,
    /// Live subagent instances: task `call_id` → agent name. Populated on a
    /// task ToolCallStarted, drained on ToolFinished — so per-agent counts
    /// reflect instances running *right now* (parallel fan-out shows ×N).
    pub running_agents: HashMap<String, String>,
    /// `prompt_tokens` of the latest Usage event — a context-fill estimate,
    /// distinct from the cumulative `prompt_tokens` total.
    pub last_prompt_tokens: u64,
    pub last_ctrl_c: Option<Instant>,
    /// Terminal (width, height); kept for scroll math between resizes.
    pub viewport: (u16, u16),
    /// `/context` dashboard open; captures the keyboard when true.
    pub context_open: bool,
    /// Scroll offset inside the `/context` dashboard.
    pub context_scroll: usize,
    pub dirty: bool,
}

impl App {
    pub fn new(model_name: String, mode: Mode, viewport: (u16, u16), notices: Vec<String>) -> Self {
        Self {
            model_name,
            mode,
            cells: notices.into_iter().map(Cell::Notice).collect(),
            live_text: false,
            input: Input::default(),
            running: false,
            spinner: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            scroll: 0,
            permissions: VecDeque::new(),
            permission_scroll: 0,
            model_picker: None,
            history: Vec::new(),
            history_pos: None,
            history_draft: String::new(),
            loop_spec: None,
            think: false,
            effort: Effort::default(),
            submodel: None,
            session: SessionInfo::default(),
            interacted: false,
            active_agents: HashSet::new(),
            running_agents: HashMap::new(),
            last_prompt_tokens: 0,
            last_ctrl_c: None,
            viewport,
            context_open: false,
            context_scroll: 0,
            dirty: true,
        }
    }

    /// Open the `/context` usage dashboard.
    pub fn open_context_dashboard(&mut self) {
        self.context_open = true;
        self.context_scroll = 0;
        self.dirty = true;
    }

    /// Attach setup-time session metadata (landing footer + sidebar data).
    pub fn with_session(mut self, session: SessionInfo) -> Self {
        self.session = session;
        self
    }

    /// Open the `/model` overlay; preselects the current model when listed.
    pub fn open_model_picker(&mut self, entries: Vec<String>, current: String) {
        if entries.is_empty() {
            self.push_notice("no models found — configure a provider or use /model provider/name");
            return;
        }
        let selected = entries.iter().position(|e| *e == current).unwrap_or(0);
        self.model_picker = Some(ModelPicker {
            entries,
            selected,
            current,
        });
        self.dirty = true;
    }

    /// Chat-view input box height (wrapped content rows + borders) for a box
    /// spanning `box_width` columns; never below [`INPUT_HEIGHT`].
    pub fn input_box_height(&self, box_width: u16) -> u16 {
        // Inner text width: 2 border columns + 2 padding columns.
        let inner = box_width.saturating_sub(4).max(1) as usize;
        (input_rows(self.input.chars.len(), inner) as u16 + 2).max(INPUT_HEIGHT)
    }

    /// Resumed sessions skip the landing and open into the transcript.
    pub fn with_resumed(mut self) -> Self {
        self.interacted = true;
        self
    }

    /// Frontend-local notice (e.g. the /model catalog); not agent-sourced.
    pub fn push_notice(&mut self, text: impl Into<String>) {
        self.live_text = false;
        self.cells.push(Cell::Notice(text.into()));
        self.dirty = true;
    }

    /// Drop the transient reasoning cell once the model moves on to real
    /// output (assistant text, a tool call, or turn end). Thinking streams in
    /// grey while the model reasons, then vanishes — it's never history.
    fn drop_live_thinking(&mut self) {
        while matches!(self.cells.last(), Some(Cell::Thinking(_))) {
            self.cells.pop();
        }
    }

    /// How many instances of `agent` are running right now (parallel
    /// fan-out counts each spawn).
    pub fn running_count(&self, agent: &str) -> u32 {
        self.running_agents.values().filter(|a| *a == agent).count() as u32
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Agent(event) => {
                self.dirty = true;
                // Any agent activity means a turn is underway; leave the
                // landing screen (seed notices arrive via App::new, not here).
                self.interacted = true;
                self.on_agent(event);
                vec![]
            }
            Msg::Key(k) if matches!(k.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.on_key(k)
            }
            Msg::Key(_) => vec![],
            Msg::Paste(text) => {
                // Paste lands in the chat box, unless an overlay owns the keys.
                if self.context_open
                    || self.model_picker.is_some()
                    || self.permissions.front().is_some()
                {
                    return vec![];
                }
                self.interacted = true;
                self.input.insert_str(&text);
                self.dirty = true;
                vec![]
            }
            Msg::Resize(w, h) => {
                self.viewport = (w, h);
                self.dirty = true;
                vec![]
            }
            Msg::Tick => {
                if self.running {
                    self.spinner = self.spinner.wrapping_add(1);
                    self.dirty = true;
                }
                if let Some(t) = self.last_ctrl_c
                    && t.elapsed() >= QUIT_WINDOW
                {
                    self.last_ctrl_c = None;
                    self.dirty = true;
                }
                // A due loop waits for the running turn to finish, then
                // fires on the next tick.
                if !self.running
                    && let Some(armed) = &mut self.loop_spec
                    && Instant::now() >= armed.next_due
                {
                    armed.next_due = Instant::now() + armed.every;
                    let prompt = armed.prompt.clone();
                    self.live_text = false;
                    self.scroll = 0;
                    self.cells.push(Cell::User(prompt.clone()));
                    self.dirty = true;
                    return vec![Effect::Submit(prompt)];
                }
                vec![]
            }
        }
    }

    fn on_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted { .. } => {
                self.running = true;
                self.active_agents.clear();
                self.running_agents.clear();
            }
            AgentEvent::AssistantText { delta } => {
                // Thinking is transient: once the answer starts, drop it.
                self.drop_live_thinking();
                if self.live_text
                    && let Some(Cell::Assistant(text)) = self.cells.last_mut()
                {
                    text.push_str(&delta);
                } else {
                    self.cells.push(Cell::Assistant(delta));
                    self.live_text = true;
                }
            }
            AgentEvent::Thinking { delta } => {
                if self.live_text
                    && let Some(Cell::Thinking(text)) = self.cells.last_mut()
                {
                    text.push_str(&delta);
                } else {
                    self.cells.push(Cell::Thinking(delta));
                    self.live_text = true;
                }
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                summary,
            } => {
                self.live_text = false;
                // Thinking that led up to this tool call has served its purpose.
                self.drop_live_thinking();
                // A top-level `task` spawn: one running subagent instance.
                if name == "task"
                    && let Some(agent) = agent_from_summary(&summary)
                {
                    self.running_agents.insert(call_id.clone(), agent);
                }
                self.cells.push(Cell::Tool(ToolCell {
                    call_id,
                    summary,
                    progress: Vec::new(),
                    result: None,
                }));
            }
            AgentEvent::ToolProgress { call_id, chunk } => {
                // Subagent activity lights up the sidebar's agent row.
                if let Some(rest) = call_id.strip_prefix("task[")
                    && let Some(end) = rest.find(']')
                {
                    self.active_agents.insert(rest[..end].to_string());
                }
                // Only subagent activity gets a line on the card; bash output
                // is too chatty (matches the REPL's filter).
                if call_id.starts_with("task[")
                    && let Some(tool) = self.last_running_tool()
                {
                    tool.progress.push(format!("{call_id} {chunk}"));
                }
            }
            AgentEvent::ToolFinished {
                call_id,
                output_preview,
                is_error,
            } => {
                // A finishing task instance stops counting as running (it
                // stays in active_agents as "ran this turn").
                self.running_agents.remove(&call_id);
                let first = output_preview
                    .lines()
                    .next()
                    .unwrap_or("(no output)")
                    .to_string();
                if let Some(tool) = self.tool_mut(&call_id) {
                    tool.result = Some((first, is_error));
                }
            }
            AgentEvent::PermissionRequested {
                request_id,
                summary,
                tool_name,
                detail,
            } => {
                self.permissions.push_back(PermissionPrompt {
                    request_id,
                    tool_name,
                    summary,
                    detail,
                });
            }
            AgentEvent::ContextCompacted {
                before_tokens,
                after_tokens,
            } => {
                self.live_text = false;
                self.cells.push(Cell::Notice(format!(
                    "context compacted: ~{before_tokens} → ~{after_tokens} tokens"
                )));
            }
            AgentEvent::VerificationReport { ok, findings } => {
                self.live_text = false;
                if ok {
                    self.cells
                        .push(Cell::Notice("✓ verification: matches the ask".into()));
                } else {
                    self.cells.push(Cell::Notice(format!(
                        "⚠ verification found gaps:\n{}",
                        findings.trim()
                    )));
                }
            }
            AgentEvent::ModelChanged { model } => {
                self.live_text = false;
                self.model_name = model.clone();
                self.cells
                    .push(Cell::Notice(format!("model: {model} — context preserved")));
            }
            AgentEvent::Usage(u) => {
                self.prompt_tokens += u.prompt_tokens;
                self.completion_tokens += u.completion_tokens;
                self.last_prompt_tokens = u.prompt_tokens;
            }
            AgentEvent::TurnFinished { .. } => {
                self.running = false;
                self.live_text = false;
                // No orphan thinking if the turn produced no visible output.
                self.drop_live_thinking();
                // A cancelled turn can leave cards open; close them so they
                // stop rendering as in-flight.
                for cell in &mut self.cells {
                    if let Cell::Tool(t) = cell
                        && t.result.is_none()
                    {
                        t.result = Some(("(interrupted)".into(), true));
                    }
                }
                if self.mode == Mode::Plan {
                    self.cells.push(Cell::Notice(
                        "plan ready — Shift+Tab twice to AUTO (hands-off), then say 'proceed'"
                            .into(),
                    ));
                }
            }
            AgentEvent::Error { message, .. } => {
                self.live_text = false;
                self.cells.push(Cell::Error(message));
            }
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Vec<Effect> {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return self.on_ctrl_c();
        }
        // A pending permission prompt captures the keyboard (except Ctrl+C);
        // arrows scroll the modal body, y/a/n decide.
        if let Some(prompt) = self.permissions.front() {
            let decision = match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Some(PermissionDecision::Allow),
                KeyCode::Char('a') | KeyCode::Char('A') => Some(PermissionDecision::AlwaysAllow),
                KeyCode::Char('n') | KeyCode::Char('N') => Some(PermissionDecision::Deny),
                _ => None,
            };
            if let Some(decision) = decision {
                let request_id = prompt.request_id;
                self.permissions.pop_front();
                self.permission_scroll = 0;
                self.dirty = true;
                return vec![Effect::Reply {
                    request_id,
                    decision,
                }];
            }
            match k.code {
                KeyCode::Up => self.permission_scroll = self.permission_scroll.saturating_sub(1),
                KeyCode::Down => self.permission_scroll += 1,
                KeyCode::PageUp => {
                    self.permission_scroll = self.permission_scroll.saturating_sub(10)
                }
                KeyCode::PageDown => self.permission_scroll += 10,
                _ => return vec![],
            }
            self.dirty = true;
            return vec![];
        }
        // The `/context` dashboard captures the keyboard: scroll or dismiss.
        if self.context_open {
            match k.code {
                KeyCode::Up => self.context_scroll = self.context_scroll.saturating_sub(1),
                KeyCode::Down => self.context_scroll += 1,
                KeyCode::PageUp => self.context_scroll = self.context_scroll.saturating_sub(10),
                KeyCode::PageDown => self.context_scroll += 10,
                KeyCode::Esc | KeyCode::Enter => {
                    self.context_open = false;
                    self.context_scroll = 0;
                }
                _ => {}
            }
            self.dirty = true;
            return vec![];
        }
        // The `/model` picker overlay captures the keyboard next.
        if let Some(picker) = &mut self.model_picker {
            let len = picker.entries.len();
            match k.code {
                KeyCode::Up => {
                    picker.selected = (picker.selected + len - 1) % len;
                    self.dirty = true;
                }
                KeyCode::Down => {
                    picker.selected = (picker.selected + 1) % len;
                    self.dirty = true;
                }
                KeyCode::Enter => {
                    let choice = picker.entries[picker.selected].clone();
                    let current = picker.current.clone();
                    self.model_picker = None;
                    self.dirty = true;
                    if choice != current {
                        return vec![Effect::SwitchModel(choice)];
                    }
                }
                KeyCode::Esc => {
                    self.model_picker = None;
                    self.dirty = true;
                }
                _ => {}
            }
            return vec![];
        }
        match k.code {
            KeyCode::Enter => {
                let text = self.input.text().trim().to_string();
                if text.is_empty() {
                    return vec![];
                }
                // Shell-style recall: remember every submit, skipping
                // immediate repeats, and drop out of history browsing.
                if self.history.last() != Some(&text) {
                    self.history.push(text.clone());
                }
                self.history_pos = None;
                self.history_draft.clear();
                // Any submit — prompt or slash command — leaves the landing.
                self.interacted = true;
                self.input.take();
                self.scroll = 0;
                self.live_text = false;
                self.cells.push(Cell::User(text.clone()));
                self.dirty = true;
                if let Some(rest) = text.strip_prefix("/model")
                    && (rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    let arg = rest.trim();
                    return if arg.is_empty() {
                        vec![Effect::ListModels]
                    } else {
                        vec![Effect::SwitchModel(arg.to_string())]
                    };
                }
                if let Some(rest) = text.strip_prefix("/loop")
                    && (rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    return self.on_loop_command(rest.trim());
                }
                if let Some(rest) = text.strip_prefix("/think")
                    && (rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    match rest.trim() {
                        "on" => self.think = true,
                        "off" => self.think = false,
                        "" => {
                            self.push_notice(format!(
                                "thinking: {}",
                                if self.think { "on" } else { "off" }
                            ));
                            return vec![];
                        }
                        other => {
                            self.cells.push(Cell::Error(format!(
                                "unknown /think arg `{other}` (on | off)"
                            )));
                            return vec![];
                        }
                    }
                    self.push_notice(format!(
                        "thinking: {}",
                        if self.think { "on" } else { "off" }
                    ));
                    return vec![Effect::SetThink(self.think)];
                }
                if let Some(rest) = text.strip_prefix("/submodel")
                    && (rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    let arg = rest.trim();
                    return match arg {
                        "" => {
                            self.push_notice(format!(
                                "subagent model: {}",
                                self.submodel.as_deref().unwrap_or("none (profiles decide)")
                            ));
                            vec![]
                        }
                        "clear" | "off" => vec![Effect::UnpinSubmodel],
                        name => vec![Effect::PinSubmodel(name.to_string())],
                    };
                }
                if let Some(rest) = text.strip_prefix("/effort")
                    && (rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    let arg = rest.trim();
                    if arg.is_empty() {
                        self.push_notice(format!("effort: {}", self.effort));
                        return vec![];
                    }
                    match arg.parse::<Effort>() {
                        Ok(effort) => {
                            self.effort = effort;
                            if effort == Effort::Low {
                                self.think = false;
                            }
                            self.push_notice(format!("effort: {effort}"));
                            return vec![Effect::SetEffort(effort)];
                        }
                        Err(e) => {
                            self.cells.push(Cell::Error(e));
                            return vec![];
                        }
                    }
                }
                if let Some(rest) = text.strip_prefix("/config")
                    && (rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    return vec![Effect::Submit(rocinante_core::prompt::config_prompt(
                        rest.trim(),
                    ))];
                }
                if text == "/init" {
                    return vec![Effect::Submit(
                        rocinante_core::prompt::init_prompt().to_string(),
                    )];
                }
                if text == "/commit" {
                    return vec![Effect::Submit(
                        rocinante_core::prompt::commit_prompt().to_string(),
                    )];
                }
                if text == "/compact" {
                    return vec![Effect::Compact];
                }
                if text == "/clear" {
                    return vec![Effect::Clear(false)];
                }
                if text == "/clear --all" {
                    return vec![Effect::Clear(true)];
                }
                if text == "/update" {
                    return vec![Effect::Update];
                }
                if text == "/trust" {
                    return vec![Effect::Trust];
                }
                if text == "/context" {
                    return vec![Effect::OpenContextDashboard];
                }
                if text == "/verify" {
                    return vec![Effect::Verify];
                }
                if let Some(rest) = text.strip_prefix("/remember")
                    && (rest.is_empty() || rest.starts_with(char::is_whitespace))
                {
                    let rule = rest.trim();
                    if rule.is_empty() {
                        self.push_notice("usage: /remember <a preference or rule to record>");
                        return vec![];
                    }
                    return vec![Effect::Submit(rocinante_core::prompt::remember_prompt(
                        rule,
                    ))];
                }
                vec![Effect::Submit(text)]
            }
            KeyCode::Char('d') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.input.is_empty() {
                    vec![Effect::Quit]
                } else {
                    vec![]
                }
            }
            KeyCode::Char(c)
                if !k
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.input.insert(c);
                self.scroll = 0;
                self.dirty = true;
                vec![]
            }
            KeyCode::Backspace => {
                self.input.backspace();
                self.scroll = 0;
                self.dirty = true;
                vec![]
            }
            KeyCode::Left => {
                self.input.cursor = self.input.cursor.saturating_sub(1);
                self.dirty = true;
                vec![]
            }
            KeyCode::Right => {
                self.input.cursor = (self.input.cursor + 1).min(self.input.chars.len());
                self.dirty = true;
                vec![]
            }
            KeyCode::Home => {
                self.input.cursor = 0;
                self.dirty = true;
                vec![]
            }
            KeyCode::End => {
                self.input.cursor = self.input.chars.len();
                self.dirty = true;
                vec![]
            }
            KeyCode::Up => {
                if self.history.is_empty() {
                    return vec![];
                }
                let pos = match self.history_pos {
                    None => {
                        self.history_draft = self.input.text();
                        self.history.len() - 1
                    }
                    Some(i) => i.saturating_sub(1),
                };
                self.history_pos = Some(pos);
                let recalled = self.history[pos].clone();
                self.input.set(&recalled);
                self.scroll = 0;
                self.dirty = true;
                vec![]
            }
            KeyCode::Down => {
                let Some(i) = self.history_pos else {
                    return vec![];
                };
                if i + 1 < self.history.len() {
                    self.history_pos = Some(i + 1);
                    let recalled = self.history[i + 1].clone();
                    self.input.set(&recalled);
                } else {
                    self.history_pos = None;
                    let draft = std::mem::take(&mut self.history_draft);
                    self.input.set(&draft);
                }
                self.scroll = 0;
                self.dirty = true;
                vec![]
            }
            KeyCode::BackTab => {
                self.mode = match self.mode {
                    Mode::Normal => Mode::Auto,
                    Mode::Auto => Mode::Plan,
                    Mode::Plan => Mode::Normal,
                };
                self.dirty = true;
                vec![Effect::SetMode(self.mode)]
            }
            KeyCode::Esc => {
                if self.running {
                    self.cells.push(Cell::Notice("cancelling turn".into()));
                    self.dirty = true;
                    vec![Effect::CancelTurn]
                } else {
                    vec![]
                }
            }
            KeyCode::PageUp => {
                self.scroll_by(self.transcript_height() as i32);
                vec![]
            }
            KeyCode::PageDown => {
                self.scroll_by(-(self.transcript_height() as i32));
                vec![]
            }
            _ => vec![],
        }
    }

    /// `/loop` — bare shows status, `stop` disarms, `<interval> <prompt>` arms.
    fn on_loop_command(&mut self, arg: &str) -> Vec<Effect> {
        match arg {
            "" => match &self.loop_spec {
                Some(armed) => {
                    let left = armed.next_due.saturating_duration_since(Instant::now());
                    self.push_notice(format!(
                        "loop: every {} — {} (next in {})",
                        interval::display(armed.every),
                        armed.prompt,
                        interval::display(Duration::from_secs(left.as_secs().max(1)))
                    ));
                }
                None => self.push_notice("no loop armed"),
            },
            "stop" => {
                if self.loop_spec.take().is_some() {
                    self.push_notice("loop stopped");
                } else {
                    self.push_notice("no loop armed");
                }
            }
            _ => {
                let (spec, prompt) = match arg.split_once(char::is_whitespace) {
                    Some((s, p)) => (s, p.trim()),
                    None => (arg, ""),
                };
                if prompt.is_empty() {
                    self.push_notice("usage: /loop <interval> <prompt> | /loop stop | /loop");
                    return vec![];
                }
                match interval::parse(spec) {
                    Ok(every) => {
                        self.loop_spec = Some(LoopSpec {
                            prompt: prompt.to_string(),
                            every,
                            next_due: Instant::now() + every,
                        });
                        self.push_notice(format!(
                            "loop armed: every {} — {prompt}",
                            interval::display(every)
                        ));
                    }
                    Err(e) => {
                        self.live_text = false;
                        self.cells.push(Cell::Error(e));
                        self.dirty = true;
                    }
                }
            }
        }
        vec![]
    }

    fn on_ctrl_c(&mut self) -> Vec<Effect> {
        let now = Instant::now();
        if self
            .last_ctrl_c
            .is_some_and(|t| now.duration_since(t) < QUIT_WINDOW)
        {
            return vec![Effect::Quit];
        }
        self.last_ctrl_c = Some(now);
        self.dirty = true;
        vec![]
    }

    fn scroll_by(&mut self, delta: i32) {
        let target = self.scroll as i64 + delta as i64;
        self.scroll = target.clamp(0, self.max_scroll() as i64) as usize;
        self.dirty = true;
    }

    pub fn transcript_height(&self) -> usize {
        self.viewport
            .1
            .saturating_sub(INPUT_HEIGHT + STATUS_HEIGHT)
            .max(1) as usize
    }

    /// Whether the chat view has room for the right sidebar.
    pub fn sidebar_visible(&self) -> bool {
        self.viewport.0 >= SIDEBAR_MIN_FRAME
    }

    /// Transcript column width; shared with the view so scroll math and
    /// rendering can't disagree about wrapping.
    pub fn transcript_width(&self) -> usize {
        let inner = self.viewport.0.saturating_sub(2 * MARGIN_X);
        if self.sidebar_visible() {
            inner.saturating_sub(SIDEBAR_WIDTH + SIDEBAR_GAP) as usize
        } else {
            inner as usize
        }
    }

    fn max_scroll(&self) -> usize {
        transcript_lines(&self.cells, self.transcript_width())
            .len()
            .saturating_sub(self.transcript_height())
    }

    fn tool_mut(&mut self, call_id: &str) -> Option<&mut ToolCell> {
        self.cells.iter_mut().rev().find_map(|c| match c {
            Cell::Tool(t) if t.call_id == call_id && t.result.is_none() => Some(t),
            _ => None,
        })
    }

    fn last_running_tool(&mut self) -> Option<&mut ToolCell> {
        self.cells.iter_mut().rev().find_map(|c| match c {
            Cell::Tool(t) if t.result.is_none() => Some(t),
            _ => None,
        })
    }
}

/// Extract the agent name from a task tool's summary, `task[<agent>]: …`.
fn agent_from_summary(summary: &str) -> Option<String> {
    let rest = summary.strip_prefix("task[")?;
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

/// Cyan used for the user-prompt bar and tool heads.
const CYAN: Color = Color::Rgb(0x00, 0xB4, 0xD8);

/// Flatten cells into wrapped, styled display lines for a given width. Pure, so
/// the scroll math in `update` and the renderer in `view` can't disagree.
/// Assistant and Notice cells run through the markdown renderer; every other
/// cell builds its single-style spans inline (bar/marker + body).
pub fn transcript_lines(cells: &[Cell], width: usize) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut out: Vec<Line<'static>> = Vec::new();
    for cell in cells {
        match cell {
            Cell::User(text) => {
                let bar = Style::new().fg(CYAN);
                let body = Style::new().add_modifier(Modifier::BOLD);
                out.extend(wrapped_lines("▌ ", bar, text, body, width));
            }
            Cell::Assistant(text) => {
                if text.is_empty() {
                    continue;
                }
                out.extend(markdown::render(text, width, Style::new()));
            }
            Cell::Thinking(text) => {
                if text.is_empty() {
                    continue;
                }
                let s = Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM);
                out.extend(wrapped_lines("∴ ", s, text, s, width));
            }
            Cell::Tool(t) => {
                let head = Style::new().fg(Color::Cyan);
                out.extend(wrapped_lines("⏺ ", head, &t.summary, head, width));
                match &t.result {
                    Some((preview, is_error)) => {
                        let (prefix, s) = if *is_error {
                            ("  ✗ ", Style::new().fg(Color::Red))
                        } else {
                            ("  ✓ ", Style::new().fg(Color::Green))
                        };
                        out.extend(wrapped_lines(prefix, s, preview, s, width));
                    }
                    None => {
                        let skip = t.progress.len().saturating_sub(PROGRESS_TAIL);
                        let s = Style::new().fg(Color::DarkGray);
                        for line in &t.progress[skip..] {
                            out.extend(wrapped_lines("    ", s, line, s, width));
                        }
                    }
                }
            }
            Cell::Notice(text) => {
                out.extend(markdown::render(
                    text,
                    width,
                    Style::new().fg(Color::DarkGray),
                ));
            }
            Cell::Error(text) => {
                let s = Style::new().fg(Color::Red);
                out.extend(wrapped_lines("! ", s, text, s, width));
            }
        }
        out.push(Line::default());
    }
    while out.last().is_some_and(is_blank_line) {
        out.pop();
    }
    out
}

/// A display line with no visible content (the inter-cell spacer, or a trailing
/// blank from a rendered cell).
fn is_blank_line(line: &Line) -> bool {
    line.spans.iter().all(|s| s.content.is_empty())
}

/// Wrap `body` to `width` behind a styled `prefix`, keeping continuation lines
/// indented to the prefix width. The prefix (bar/marker) and body carry their
/// own styles so a cyan bar can front bold text.
fn wrapped_lines(
    prefix: &str,
    prefix_style: Style,
    body: &str,
    body_style: Style,
    width: usize,
) -> Vec<Line<'static>> {
    let prefix_width = prefix.chars().count();
    let body_width = width.saturating_sub(prefix_width).max(4);
    let indent = " ".repeat(prefix_width);
    let mut out = Vec::new();
    for (i, line) in wrap_text(body, body_width).into_iter().enumerate() {
        let mut spans = Vec::new();
        if i == 0 {
            if !prefix.is_empty() {
                spans.push(Span::styled(prefix.to_string(), prefix_style));
            }
        } else if prefix_width > 0 {
            spans.push(Span::styled(indent.clone(), body_style));
        }
        spans.push(Span::styled(line, body_style));
        out.push(Line::from(spans));
    }
    out
}

/// Greedy word wrap on char count (v1: no grapheme/display-width handling).
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let chars: Vec<char> = raw.chars().collect();
        if chars.len() <= width {
            out.push(raw.to_string());
            continue;
        }
        let mut start = 0;
        while start < chars.len() {
            let end = (start + width).min(chars.len());
            let brk = if end < chars.len() {
                chars[start..end]
                    .iter()
                    .rposition(|c| *c == ' ')
                    .map(|p| start + p + 1)
                    .unwrap_or(end)
            } else {
                end
            };
            out.push(
                chars[start..brk]
                    .iter()
                    .collect::<String>()
                    .trim_end()
                    .to_string(),
            );
            start = brk;
            while start < chars.len() && chars[start] == ' ' {
                start += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocinante_providers::Usage;

    fn app() -> App {
        App::new("test-model".into(), Mode::Normal, (80, 24), vec![])
    }

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(c: char) -> Msg {
        Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn agent(event: AgentEvent) -> Msg {
        Msg::Agent(event)
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.update(key(KeyCode::Char(c)));
        }
    }

    fn started(app: &mut App) {
        app.update(agent(AgentEvent::TurnStarted {
            turn_id: Uuid::new_v4(),
        }));
    }

    fn tool_started(app: &mut App, call_id: &str, summary: &str) {
        app.update(agent(AgentEvent::ToolCallStarted {
            call_id: call_id.into(),
            name: "bash".into(),
            summary: summary.into(),
        }));
    }

    #[test]
    fn input_rows_counts_cursor_and_caps() {
        assert_eq!(input_rows(0, 10), 1); // empty: just the cursor glyph
        assert_eq!(input_rows(9, 10), 1); // 9 chars + cursor fill one row
        assert_eq!(input_rows(10, 10), 2); // cursor wraps onto a second row
        assert_eq!(input_rows(500, 10), MAX_INPUT_ROWS);
        assert_eq!(input_rows(5, 0), 6); // degenerate width clamps to 1 col
    }

    #[test]
    fn input_box_height_grows_with_text() {
        let mut a = app();
        // Box width 24 → 20 inner columns after borders + padding.
        assert_eq!(a.input_box_height(24), INPUT_HEIGHT);
        type_str(&mut a, &"x".repeat(45)); // 46 display chars → 3 rows
        assert_eq!(a.input_box_height(24), 5);
        type_str(&mut a, &"x".repeat(400));
        assert_eq!(a.input_box_height(24), MAX_INPUT_ROWS as u16 + 2);
    }

    #[test]
    fn history_recall_up_down() {
        let mut a = app();
        type_str(&mut a, "first prompt");
        a.update(key(KeyCode::Enter));
        type_str(&mut a, "second prompt");
        a.update(key(KeyCode::Enter));
        // Draft in progress, then browse history.
        type_str(&mut a, "dra");
        a.update(key(KeyCode::Up));
        assert_eq!(a.input.text(), "second prompt");
        a.update(key(KeyCode::Up));
        assert_eq!(a.input.text(), "first prompt");
        a.update(key(KeyCode::Up)); // floor at oldest
        assert_eq!(a.input.text(), "first prompt");
        a.update(key(KeyCode::Down));
        assert_eq!(a.input.text(), "second prompt");
        a.update(key(KeyCode::Down)); // past newest: restore the draft
        assert_eq!(a.input.text(), "dra");
    }

    #[test]
    fn history_skips_consecutive_duplicates() {
        let mut a = app();
        type_str(&mut a, "same");
        a.update(key(KeyCode::Enter));
        type_str(&mut a, "same");
        a.update(key(KeyCode::Enter));
        assert_eq!(a.history, vec!["same".to_string()]);
    }

    #[test]
    fn model_picker_captures_keys_and_switches() {
        let mut a = app();
        a.open_model_picker(
            vec!["alpha".into(), "beta".into(), "gamma".into()],
            "beta".into(),
        );
        // Preselected on the current model.
        assert_eq!(a.model_picker.as_ref().unwrap().selected, 1);
        // Typing must not reach the input while open.
        type_str(&mut a, "xyz");
        assert!(a.input.is_empty());
        a.update(key(KeyCode::Down));
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects, vec![Effect::SwitchModel("gamma".into())]);
        assert!(a.model_picker.is_none());
    }

    #[test]
    fn model_picker_wraps_and_esc_closes_without_switch() {
        let mut a = app();
        a.open_model_picker(vec!["alpha".into(), "beta".into()], "alpha".into());
        a.update(key(KeyCode::Up)); // wraps 0 -> 1
        assert_eq!(a.model_picker.as_ref().unwrap().selected, 1);
        let effects = a.update(key(KeyCode::Esc));
        assert_eq!(effects, vec![]);
        assert!(a.model_picker.is_none());
        // Enter on the current model closes without switching.
        a.open_model_picker(vec!["alpha".into()], "alpha".into());
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects, vec![]);
    }

    #[test]
    fn permission_prompt_scrolls_with_arrows() {
        let mut a = app();
        a.update(agent(AgentEvent::PermissionRequested {
            request_id: Uuid::new_v4(),
            tool_name: "write".into(),
            summary: "write: big.rs".into(),
            detail: Some("+line\n".repeat(200)),
        }));
        a.update(key(KeyCode::Down));
        a.update(key(KeyCode::PageDown));
        assert_eq!(a.permission_scroll, 11);
        a.update(key(KeyCode::Up));
        assert_eq!(a.permission_scroll, 10);
        // Deciding resets the scroll.
        let effects = a.update(key(KeyCode::Char('y')));
        assert_eq!(effects.len(), 1);
        assert_eq!(a.permission_scroll, 0);
    }

    #[test]
    fn effort_command_sets_tier_and_low_clears_think() {
        let mut a = app();
        // Bare shows current (default high), no effect.
        type_str(&mut a, "/effort");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![]);
        assert!(matches!(a.cells.last(), Some(Cell::Notice(n)) if n.contains("high")));
        // Set low: effect emitted, think indicator cleared.
        a.think = true;
        type_str(&mut a, "/effort low");
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects, vec![Effect::SetEffort(Effort::Low)]);
        assert_eq!(a.effort, Effort::Low);
        assert!(!a.think);
        // Bad arg errors without an effect.
        type_str(&mut a, "/effort extreme");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![]);
        assert!(matches!(a.cells.last(), Some(Cell::Error(_))));
    }

    #[test]
    fn submodel_command_pins_and_clears() {
        let mut a = app();
        // Bare shows current pin state.
        type_str(&mut a, "/submodel");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![]);
        assert!(matches!(a.cells.last(), Some(Cell::Notice(n)) if n.contains("none")));
        // Name emits the pin effect (validation happens in the event loop).
        type_str(&mut a, "/submodel glm-5.2:cloud");
        assert_eq!(
            a.update(key(KeyCode::Enter)),
            vec![Effect::PinSubmodel("glm-5.2:cloud".into())]
        );
        // Clear emits the unpin effect.
        type_str(&mut a, "/submodel clear");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![Effect::UnpinSubmodel]);
    }

    #[test]
    fn model_command_bare_lists() {
        let mut a = app();
        type_str(&mut a, "/model");
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects, vec![Effect::ListModels]);
    }

    #[test]
    fn model_command_with_arg_switches() {
        let mut a = app();
        type_str(&mut a, "/model glm-5.2:cloud");
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects, vec![Effect::SwitchModel("glm-5.2:cloud".into())]);
    }

    #[test]
    fn model_changed_updates_status_and_notices() {
        let mut a = app();
        a.update(agent(AgentEvent::ModelChanged {
            model: "kimi-k2.5:cloud".into(),
        }));
        assert_eq!(a.model_name, "kimi-k2.5:cloud");
        assert!(matches!(a.cells.last(), Some(Cell::Notice(n)) if n.contains("kimi-k2.5:cloud")));
    }

    #[test]
    fn plan_mode_turn_finish_offers_execution() {
        let mut a = app();
        a.mode = Mode::Plan;
        a.update(agent(AgentEvent::TurnFinished {
            turn_id: Uuid::new_v4(),
        }));
        assert!(matches!(a.cells.last(), Some(Cell::Notice(n)) if n.contains("plan ready")));

        let mut b = app();
        b.update(agent(AgentEvent::TurnFinished {
            turn_id: Uuid::new_v4(),
        }));
        assert!(b.cells.is_empty(), "no offer outside plan mode");
    }

    #[test]
    fn compact_command_emits_effect() {
        let mut a = app();
        type_str(&mut a, "/compact");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![Effect::Compact]);
    }

    #[test]
    fn streaming_deltas_accumulate_in_one_cell() {
        let mut a = app();
        started(&mut a);
        a.update(agent(AgentEvent::AssistantText {
            delta: "Hel".into(),
        }));
        a.update(agent(AgentEvent::AssistantText { delta: "lo".into() }));
        assert_eq!(a.cells, vec![Cell::Assistant("Hello".into())]);
        assert!(a.running);
    }

    #[test]
    fn thinking_dropped_when_answer_starts() {
        let mut a = app();
        started(&mut a);
        a.update(agent(AgentEvent::Thinking {
            delta: "let me reason".into(),
        }));
        assert_eq!(a.cells, vec![Cell::Thinking("let me reason".into())]);
        a.update(agent(AgentEvent::AssistantText {
            delta: "the answer".into(),
        }));
        // Thinking is gone; only the answer remains.
        assert_eq!(a.cells, vec![Cell::Assistant("the answer".into())]);
    }

    #[test]
    fn thinking_dropped_on_tool_call() {
        let mut a = app();
        started(&mut a);
        a.update(agent(AgentEvent::Thinking {
            delta: "i should list files".into(),
        }));
        tool_started(&mut a, "c1", "bash: ls");
        assert!(
            !a.cells.iter().any(|c| matches!(c, Cell::Thinking(_))),
            "thinking must be cleared before a tool call: {:?}",
            a.cells
        );
    }

    #[test]
    fn text_after_tool_call_starts_new_cell() {
        let mut a = app();
        started(&mut a);
        a.update(agent(AgentEvent::AssistantText {
            delta: "before".into(),
        }));
        tool_started(&mut a, "c1", "bash: ls");
        a.update(agent(AgentEvent::AssistantText {
            delta: "after".into(),
        }));
        assert_eq!(a.cells.len(), 3);
        assert_eq!(a.cells[0], Cell::Assistant("before".into()));
        assert_eq!(a.cells[2], Cell::Assistant("after".into()));
    }

    #[test]
    fn text_after_turn_finished_starts_new_cell() {
        let mut a = app();
        started(&mut a);
        a.update(agent(AgentEvent::AssistantText {
            delta: "one".into(),
        }));
        a.update(agent(AgentEvent::TurnFinished {
            turn_id: Uuid::new_v4(),
        }));
        assert!(!a.running);
        started(&mut a);
        a.update(agent(AgentEvent::AssistantText {
            delta: "two".into(),
        }));
        assert_eq!(a.cells.len(), 2);
    }

    #[test]
    fn tool_finished_sets_result_on_matching_card() {
        let mut a = app();
        tool_started(&mut a, "c1", "bash: cargo test");
        a.update(agent(AgentEvent::ToolFinished {
            call_id: "c1".into(),
            output_preview: "ok\nmore".into(),
            is_error: false,
        }));
        let Cell::Tool(t) = &a.cells[0] else {
            panic!("expected tool cell")
        };
        assert_eq!(t.result, Some(("ok".into(), false)));
    }

    #[test]
    fn tool_finished_empty_preview_shows_placeholder() {
        let mut a = app();
        tool_started(&mut a, "c1", "bash: true");
        a.update(agent(AgentEvent::ToolFinished {
            call_id: "c1".into(),
            output_preview: String::new(),
            is_error: true,
        }));
        let Cell::Tool(t) = &a.cells[0] else {
            panic!("expected tool cell")
        };
        assert_eq!(t.result, Some(("(no output)".into(), true)));
    }

    #[test]
    fn task_progress_attaches_to_running_tool() {
        let mut a = app();
        tool_started(&mut a, "c1", "task[explore]: look around");
        a.update(agent(AgentEvent::ToolProgress {
            call_id: "task[explore]".into(),
            chunk: "reading files".into(),
        }));
        a.update(agent(AgentEvent::ToolProgress {
            call_id: "bash:ls".into(),
            chunk: "noise".into(),
        }));
        let Cell::Tool(t) = &a.cells[0] else {
            panic!("expected tool cell")
        };
        assert_eq!(t.progress, vec!["task[explore] reading files".to_string()]);
    }

    #[test]
    fn turn_finished_closes_interrupted_tool_cards() {
        let mut a = app();
        started(&mut a);
        tool_started(&mut a, "c1", "bash: sleep 100");
        a.update(agent(AgentEvent::TurnFinished {
            turn_id: Uuid::new_v4(),
        }));
        let Cell::Tool(t) = &a.cells[0] else {
            panic!("expected tool cell")
        };
        assert_eq!(t.result, Some(("(interrupted)".into(), true)));
    }

    #[test]
    fn usage_events_sum_into_totals() {
        let mut a = app();
        a.update(agent(AgentEvent::Usage(Usage {
            prompt_tokens: 100,
            completion_tokens: 7,
        })));
        a.update(agent(AgentEvent::Usage(Usage {
            prompt_tokens: 50,
            completion_tokens: 3,
        })));
        assert_eq!((a.prompt_tokens, a.completion_tokens), (150, 10));
    }

    #[test]
    fn compaction_and_error_cells() {
        let mut a = app();
        a.update(agent(AgentEvent::ContextCompacted {
            before_tokens: 900,
            after_tokens: 300,
        }));
        a.update(agent(AgentEvent::Error {
            message: "boom".into(),
            fatal: false,
        }));
        assert_eq!(
            a.cells[0],
            Cell::Notice("context compacted: ~900 → ~300 tokens".into())
        );
        assert_eq!(a.cells[1], Cell::Error("boom".into()));
    }

    #[test]
    fn permission_modal_answers_and_blocks_input() {
        let mut a = app();
        let id = Uuid::new_v4();
        a.update(agent(AgentEvent::PermissionRequested {
            request_id: id,
            summary: "run `rm -rf /tmp/x`".into(),
            tool_name: "bash".into(),
            detail: None,
        }));
        assert_eq!(a.permissions.len(), 1);
        // Typing does not reach the input while the modal is open.
        let effects = a.update(key(KeyCode::Char('x')));
        assert!(effects.is_empty());
        assert!(a.input.is_empty());
        let effects = a.update(key(KeyCode::Char('y')));
        assert_eq!(
            effects,
            vec![Effect::Reply {
                request_id: id,
                decision: PermissionDecision::Allow
            }]
        );
        assert!(a.permissions.is_empty());
    }

    #[test]
    fn permission_always_and_deny() {
        for (c, decision) in [
            ('a', PermissionDecision::AlwaysAllow),
            ('n', PermissionDecision::Deny),
        ] {
            let mut a = app();
            let id = Uuid::new_v4();
            a.update(agent(AgentEvent::PermissionRequested {
                request_id: id,
                summary: "s".into(),
                tool_name: "bash".into(),
                detail: None,
            }));
            let effects = a.update(key(KeyCode::Char(c)));
            assert_eq!(
                effects,
                vec![Effect::Reply {
                    request_id: id,
                    decision
                }]
            );
        }
    }

    #[test]
    fn queued_permission_prompts_answer_in_order() {
        let mut a = app();
        let (id1, id2) = (Uuid::new_v4(), Uuid::new_v4());
        for id in [id1, id2] {
            a.update(agent(AgentEvent::PermissionRequested {
                request_id: id,
                summary: "s".into(),
                tool_name: "bash".into(),
                detail: None,
            }));
        }
        let effects = a.update(key(KeyCode::Char('n')));
        assert_eq!(
            effects,
            vec![Effect::Reply {
                request_id: id1,
                decision: PermissionDecision::Deny
            }]
        );
        assert_eq!(a.permissions.front().map(|p| p.request_id), Some(id2));
    }

    #[test]
    fn shift_tab_cycles_mode() {
        let mut a = app();
        assert_eq!(
            a.update(key(KeyCode::BackTab)),
            vec![Effect::SetMode(Mode::Auto)]
        );
        assert_eq!(
            a.update(key(KeyCode::BackTab)),
            vec![Effect::SetMode(Mode::Plan)]
        );
        assert_eq!(
            a.update(key(KeyCode::BackTab)),
            vec![Effect::SetMode(Mode::Normal)]
        );
        assert_eq!(a.mode, Mode::Normal);
    }

    #[test]
    fn input_editing_cursor_movement() {
        let mut a = app();
        type_str(&mut a, "hxello");
        a.update(key(KeyCode::Home));
        a.update(key(KeyCode::Right));
        a.update(key(KeyCode::Right));
        a.update(key(KeyCode::Backspace));
        assert_eq!(a.input.text(), "hello");
        a.update(key(KeyCode::End));
        type_str(&mut a, "!");
        assert_eq!(a.input.text(), "hello!");
        a.update(key(KeyCode::Left));
        type_str(&mut a, "o");
        assert_eq!(a.input.text(), "helloo!");
    }

    #[test]
    fn enter_submits_and_clears_input() {
        let mut a = app();
        type_str(&mut a, "  do the thing  ");
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects, vec![Effect::Submit("do the thing".into())]);
        assert!(a.input.is_empty());
        assert_eq!(a.cells, vec![Cell::User("do the thing".into())]);
    }

    #[test]
    fn enter_on_blank_input_does_nothing() {
        let mut a = app();
        type_str(&mut a, "   ");
        assert!(a.update(key(KeyCode::Enter)).is_empty());
        assert!(a.cells.is_empty());
    }

    #[test]
    fn ctrl_c_twice_within_window_quits() {
        let mut a = app();
        assert!(a.update(ctrl('c')).is_empty());
        assert_eq!(a.update(ctrl('c')), vec![Effect::Quit]);
    }

    #[test]
    fn stale_ctrl_c_does_not_quit() {
        let mut a = app();
        a.last_ctrl_c = Instant::now().checked_sub(Duration::from_secs(2));
        assert!(a.last_ctrl_c.is_some(), "clock too young to backdate");
        assert!(a.update(ctrl('c')).is_empty());
    }

    #[test]
    fn ctrl_d_quits_only_on_empty_input() {
        let mut a = app();
        type_str(&mut a, "x");
        assert!(a.update(ctrl('d')).is_empty());
        a.update(key(KeyCode::Backspace));
        assert_eq!(a.update(ctrl('d')), vec![Effect::Quit]);
    }

    #[test]
    fn esc_cancels_only_while_running() {
        let mut a = app();
        assert!(a.update(key(KeyCode::Esc)).is_empty());
        started(&mut a);
        assert_eq!(a.update(key(KeyCode::Esc)), vec![Effect::CancelTurn]);
    }

    #[test]
    fn typing_while_scrolled_snaps_to_bottom() {
        let mut a = app();
        for i in 0..100 {
            a.update(agent(AgentEvent::Error {
                message: format!("line {i}"),
                fatal: false,
            }));
        }
        a.update(key(KeyCode::PageUp));
        assert!(a.scroll > 0);
        a.update(key(KeyCode::Char('x')));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn scroll_clamps_to_content() {
        let mut a = app();
        a.scroll_by(50);
        assert_eq!(a.scroll, 0, "empty transcript cannot scroll");
        for i in 0..100 {
            a.update(agent(AgentEvent::Error {
                message: format!("line {i}"),
                fatal: false,
            }));
        }
        a.scroll_by(100_000);
        let max = a.max_scroll();
        assert_eq!(a.scroll, max);
        a.scroll_by(-3);
        assert_eq!(a.scroll, max - 3);
    }

    /// A LoopSpec that is already due; tests can't sleep, and backdating
    /// Instant::now() can panic on some platforms, so the fire condition's
    /// `>=` lets `next_due = now` count as due.
    fn due_loop(prompt: &str, every_secs: u64) -> LoopSpec {
        LoopSpec {
            prompt: prompt.into(),
            every: Duration::from_secs(every_secs),
            next_due: Instant::now(),
        }
    }

    #[test]
    fn slash_update_emits_update_effect() {
        let mut a = app();
        type_str(&mut a, "/update");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![Effect::Update]);
    }

    #[test]
    fn slash_trust_emits_trust_effect() {
        let mut a = app();
        type_str(&mut a, "/trust");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![Effect::Trust]);
    }

    #[test]
    fn paste_inserts_into_input_preserving_newlines() {
        let mut a = app();
        type_str(&mut a, "cmd: ");
        a.update(Msg::Paste("line1\nline2".into()));
        assert_eq!(a.input.text(), "cmd: line1\nline2");
        // A multi-line paste does NOT submit on its own.
        assert!(!a.interacted || a.cells.is_empty());
    }

    #[test]
    fn paste_ignored_while_overlay_open() {
        let mut a = app();
        a.open_context_dashboard();
        a.update(Msg::Paste("stuff".into()));
        assert_eq!(
            a.input.text(),
            "",
            "paste must not leak into a captured input"
        );
    }

    #[test]
    fn slash_verify_and_remember() {
        let mut a = app();
        type_str(&mut a, "/verify");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![Effect::Verify]);

        type_str(&mut a, "/remember always run cargo fmt");
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        let Effect::Submit(p) = &effects[0] else {
            panic!("expected Submit, got {effects:?}");
        };
        assert!(p.contains("always run cargo fmt"));
        assert!(p.contains("LESSONS.md"));

        // Bare /remember shows usage, no Submit.
        type_str(&mut a, "/remember");
        assert!(a.update(key(KeyCode::Enter)).is_empty());
    }

    #[test]
    fn slash_clear_variants() {
        let mut a = app();
        type_str(&mut a, "/clear");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![Effect::Clear(false)]);
        type_str(&mut a, "/clear --all");
        assert_eq!(a.update(key(KeyCode::Enter)), vec![Effect::Clear(true)]);
    }

    #[test]
    fn slash_context_opens_and_esc_closes_dashboard() {
        let mut a = app();
        type_str(&mut a, "/context");
        assert_eq!(
            a.update(key(KeyCode::Enter)),
            vec![Effect::OpenContextDashboard]
        );
        a.open_context_dashboard();
        assert!(a.context_open);
        // Scroll then dismiss.
        a.update(key(KeyCode::Down));
        assert_eq!(a.context_scroll, 1);
        a.update(key(KeyCode::Esc));
        assert!(!a.context_open);
    }

    #[test]
    fn slash_config_submits_canned_prompt() {
        let mut a = app();
        type_str(&mut a, "/config add alias kimiko with num_ctx 256000");
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        let Effect::Submit(prompt) = &effects[0] else {
            panic!("expected Submit, got {effects:?}");
        };
        assert!(prompt.contains("config.toml"));
        assert!(prompt.contains("add alias kimiko with num_ctx 256000"));
        assert!(prompt.contains("rocinante-config"));
    }

    #[test]
    fn slash_config_bare_submits_summary_prompt() {
        let mut a = app();
        type_str(&mut a, "/config");
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects.len(), 1);
        let Effect::Submit(prompt) = &effects[0] else {
            panic!("expected Submit, got {effects:?}");
        };
        assert!(prompt.contains("summarize"));
        assert!(prompt.contains("Do not edit"));
    }

    #[test]
    fn loop_command_arms_and_notices() {
        let mut a = app();
        type_str(&mut a, "/loop 5m check git status");
        let effects = a.update(key(KeyCode::Enter));
        assert!(effects.is_empty(), "arming submits nothing");
        let armed = a.loop_spec.as_ref().expect("loop armed");
        assert_eq!(armed.prompt, "check git status");
        assert_eq!(armed.every, Duration::from_secs(300));
        assert!(armed.next_due > Instant::now() + Duration::from_secs(290));
        assert!(
            matches!(a.cells.last(), Some(Cell::Notice(n)) if n == "loop armed: every 5m — check git status")
        );
    }

    #[test]
    fn loop_bare_shows_status_or_none() {
        let mut a = app();
        type_str(&mut a, "/loop");
        a.update(key(KeyCode::Enter));
        assert!(matches!(a.cells.last(), Some(Cell::Notice(n)) if n == "no loop armed"));
        type_str(&mut a, "/loop 1h poll ci");
        a.update(key(KeyCode::Enter));
        type_str(&mut a, "/loop");
        a.update(key(KeyCode::Enter));
        assert!(
            matches!(a.cells.last(), Some(Cell::Notice(n)) if n.contains("every 1h — poll ci") && n.contains("next in"))
        );
    }

    #[test]
    fn loop_stop_disarms() {
        let mut a = app();
        type_str(&mut a, "/loop 30s ping");
        a.update(key(KeyCode::Enter));
        assert!(a.loop_spec.is_some());
        type_str(&mut a, "/loop stop");
        let effects = a.update(key(KeyCode::Enter));
        assert!(effects.is_empty());
        assert!(a.loop_spec.is_none());
        assert!(matches!(a.cells.last(), Some(Cell::Notice(n)) if n == "loop stopped"));
        type_str(&mut a, "/loop stop");
        a.update(key(KeyCode::Enter));
        assert!(matches!(a.cells.last(), Some(Cell::Notice(n)) if n == "no loop armed"));
    }

    #[test]
    fn loop_bad_interval_shows_error() {
        let mut a = app();
        type_str(&mut a, "/loop 5x do things");
        let effects = a.update(key(KeyCode::Enter));
        assert!(effects.is_empty());
        assert!(a.loop_spec.is_none());
        assert!(matches!(a.cells.last(), Some(Cell::Error(_))));
    }

    #[test]
    fn loop_missing_prompt_shows_usage() {
        let mut a = app();
        type_str(&mut a, "/loop 5m");
        assert!(a.update(key(KeyCode::Enter)).is_empty());
        assert!(a.loop_spec.is_none());
        assert!(matches!(a.cells.last(), Some(Cell::Notice(n)) if n.starts_with("usage:")));
    }

    #[test]
    fn loop_prefix_word_is_not_intercepted() {
        let mut a = app();
        type_str(&mut a, "/loopy stuff");
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects, vec![Effect::Submit("/loopy stuff".into())]);
        assert!(a.loop_spec.is_none());
    }

    #[test]
    fn tick_fires_due_loop_when_idle() {
        let mut a = app();
        a.loop_spec = Some(due_loop("check things", 300));
        let effects = a.update(Msg::Tick);
        assert_eq!(effects, vec![Effect::Submit("check things".into())]);
        assert!(matches!(a.cells.last(), Some(Cell::User(t)) if t == "check things"));
    }

    #[test]
    fn tick_advances_next_due_after_fire() {
        let mut a = app();
        a.loop_spec = Some(due_loop("p", 300));
        a.update(Msg::Tick);
        let next = a.loop_spec.as_ref().unwrap().next_due;
        assert!(next > Instant::now() + Duration::from_secs(290));
        // No longer due: the next tick must not fire again.
        assert!(a.update(Msg::Tick).is_empty());
    }

    #[test]
    fn tick_does_not_fire_while_running() {
        let mut a = app();
        started(&mut a);
        a.loop_spec = Some(due_loop("p", 300));
        let before = a.loop_spec.as_ref().unwrap().next_due;
        assert!(a.update(Msg::Tick).is_empty());
        assert_eq!(a.loop_spec.as_ref().unwrap().next_due, before);
        // Turn ends; the still-due loop fires on the next tick.
        a.update(agent(AgentEvent::TurnFinished {
            turn_id: Uuid::new_v4(),
        }));
        assert_eq!(a.update(Msg::Tick), vec![Effect::Submit("p".into())]);
    }

    #[test]
    fn tick_does_not_fire_before_due() {
        let mut a = app();
        a.loop_spec = Some(LoopSpec {
            prompt: "p".into(),
            every: Duration::from_secs(300),
            next_due: Instant::now() + Duration::from_secs(300),
        });
        assert!(a.update(Msg::Tick).is_empty());
        assert!(a.cells.is_empty());
    }

    #[test]
    fn landing_transitions_to_chat_on_enter() {
        let mut a = app();
        assert!(!a.interacted, "fresh session starts on the landing");
        type_str(&mut a, "hello");
        assert!(!a.interacted, "typing alone stays on the landing");
        let effects = a.update(key(KeyCode::Enter));
        assert_eq!(effects, vec![Effect::Submit("hello".into())]);
        assert!(a.interacted, "submit enters the chat view");
    }

    #[test]
    fn slash_command_leaves_landing() {
        let mut a = app();
        type_str(&mut a, "/model");
        a.update(key(KeyCode::Enter));
        assert!(a.interacted);
    }

    #[test]
    fn blank_enter_stays_on_landing() {
        let mut a = app();
        assert!(a.update(key(KeyCode::Enter)).is_empty());
        assert!(!a.interacted);
    }

    #[test]
    fn agent_event_leaves_landing() {
        let mut a = app();
        a.update(agent(AgentEvent::Usage(Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
        })));
        assert!(a.interacted);
    }

    #[test]
    fn resumed_session_skips_landing() {
        let a = app().with_resumed();
        assert!(a.interacted);
        let b = app().with_session(SessionInfo {
            resumed: true,
            ..SessionInfo::default()
        });
        // with_session only stores metadata; the caller applies resumed.
        assert!(b.session.resumed);
    }

    #[test]
    fn active_agents_fill_from_task_progress_and_clear_on_turn_start() {
        let mut a = app();
        started(&mut a);
        tool_started(&mut a, "c1", "task[scout]: look around");
        a.update(agent(AgentEvent::ToolProgress {
            call_id: "task[scout]".into(),
            chunk: "reading".into(),
        }));
        a.update(agent(AgentEvent::ToolProgress {
            call_id: "bash:ls".into(),
            chunk: "noise".into(),
        }));
        assert_eq!(a.active_agents.len(), 1);
        assert!(a.active_agents.contains("scout"));
        started(&mut a);
        assert!(a.active_agents.is_empty(), "new turn clears active agents");
    }

    fn task_started(a: &mut App, call_id: &str, agent_name: &str) {
        a.update(agent(AgentEvent::ToolCallStarted {
            call_id: call_id.into(),
            name: "task".into(),
            summary: format!("task[{agent_name}]: work"),
        }));
    }

    fn task_finished(a: &mut App, call_id: &str) {
        a.update(agent(AgentEvent::ToolFinished {
            call_id: call_id.into(),
            output_preview: "done".into(),
            is_error: false,
        }));
    }

    #[test]
    fn running_agents_count_parallel_instances() {
        let mut a = app();
        started(&mut a);
        task_started(&mut a, "c1", "miller");
        task_started(&mut a, "c2", "miller");
        task_started(&mut a, "c3", "miller");
        task_started(&mut a, "c4", "naomi");
        assert_eq!(a.running_count("miller"), 3);
        assert_eq!(a.running_count("naomi"), 1);

        task_finished(&mut a, "c2");
        assert_eq!(a.running_count("miller"), 2);

        task_finished(&mut a, "c1");
        task_finished(&mut a, "c3");
        assert_eq!(a.running_count("miller"), 0);
    }

    #[test]
    fn turn_start_clears_running_agents() {
        let mut a = app();
        started(&mut a);
        task_started(&mut a, "c1", "miller");
        assert_eq!(a.running_count("miller"), 1);
        started(&mut a);
        assert_eq!(a.running_count("miller"), 0);
    }

    #[test]
    fn finished_without_start_is_a_noop() {
        let mut a = app();
        started(&mut a);
        task_finished(&mut a, "never-started");
        assert_eq!(a.running_count("miller"), 0);
    }

    #[test]
    fn last_prompt_tokens_tracks_latest_usage() {
        let mut a = app();
        for (p, c) in [(100, 7), (150, 3)] {
            a.update(agent(AgentEvent::Usage(Usage {
                prompt_tokens: p,
                completion_tokens: c,
            })));
        }
        assert_eq!(a.last_prompt_tokens, 150, "latest, not cumulative");
        assert_eq!((a.prompt_tokens, a.completion_tokens), (250, 10));
    }

    #[test]
    fn sidebar_geometry_thresholds() {
        let mut a = app();
        a.viewport = (95, 30);
        assert!(!a.sidebar_visible());
        assert_eq!(a.transcript_width(), 95 - 2 * MARGIN_X as usize);
        a.viewport = (96, 30);
        assert!(a.sidebar_visible());
        assert_eq!(
            a.transcript_width(),
            96 - 2 * MARGIN_X as usize - SIDEBAR_WIDTH as usize - SIDEBAR_GAP as usize
        );
    }

    #[test]
    fn wrap_text_breaks_at_spaces_and_hard_breaks() {
        assert_eq!(wrap_text("hello world", 20), vec!["hello world"]);
        assert_eq!(wrap_text("hello world", 8), vec!["hello", "world"]);
        assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap_text("a\n\nb", 10), vec!["a", "", "b"]);
    }

    /// Concatenate a line's span contents into its display text.
    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Foreground color of a line's first non-empty (visible) span.
    fn line_fg(line: &Line) -> Option<Color> {
        line.spans
            .iter()
            .find(|s| !s.content.is_empty())
            .and_then(|s| s.style.fg)
    }

    #[test]
    fn transcript_collapses_finished_tool_to_two_lines() {
        let mut a = app();
        tool_started(&mut a, "c1", "task[explore]: scan");
        a.update(agent(AgentEvent::ToolProgress {
            call_id: "task[explore]".into(),
            chunk: "working".into(),
        }));
        let lines = transcript_lines(&a.cells, 80);
        assert_eq!(lines.len(), 2, "tool head + one progress line");
        assert_eq!(line_fg(&lines[0]), Some(Color::Cyan), "tool head cyan");
        assert_eq!(line_fg(&lines[1]), Some(Color::DarkGray), "progress dim");
        a.update(agent(AgentEvent::ToolFinished {
            call_id: "c1".into(),
            output_preview: "done".into(),
            is_error: false,
        }));
        let lines = transcript_lines(&a.cells, 80);
        assert_eq!(lines.len(), 2, "tool head + result line");
        assert_eq!(line_fg(&lines[0]), Some(Color::Cyan), "tool head cyan");
        assert_eq!(line_fg(&lines[1]), Some(Color::Green), "ok result green");
        assert_eq!(line_text(&lines[1]), "  ✓ done");
    }

    #[test]
    fn user_cell_renders_cyan_bar_then_bold_text() {
        let mut a = app();
        a.cells.push(Cell::User("hello".into()));
        let lines = transcript_lines(&a.cells, 80);
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans[0].content.as_ref(), "▌ ");
        assert_eq!(spans[0].style.fg, Some(CYAN));
        assert_eq!(spans[1].content.as_ref(), "hello");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn assistant_cell_is_markdown_rendered() {
        let mut a = app();
        a.cells.push(Cell::Assistant("**bold** text".into()));
        let lines = transcript_lines(&a.cells, 80);
        let bold = lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "bold")
            .expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(bold.style.fg, Some(Color::Rgb(0xFF, 0x59, 0x64)));
    }
}
