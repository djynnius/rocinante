//! First-run model picker: a minimal, self-contained ratatui screen shown
//! before the main session when no model was passed, remembered, or set in
//! config. Keyboard-only. Lists Ollama's models; when Ollama is down (or
//! empty) it offers the GGUF files on this machine instead, asking for their
//! folder when it doesn't know it. Returns the chosen model name, or an
//! error once the user leaves with nothing chosen (Esc twice / Ctrl+C).

use std::io::Stdout;

use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use rocinante_core::config::Config;
use rocinante_core::first_run::{self, Choices, PathAnswer};
use rocinante_core::provider_factory::ModelEntry;

/// What the picker offers (see [`first_run::choices`]) and the config a
/// typed folder's models are listed against.
pub struct PickerOptions {
    pub choices: Choices,
    pub config: Config,
}

enum Item {
    Model(ModelEntry),
    /// Provider name (anthropic / gemini / openai) for a `provider/…` hint.
    Provider(String),
}

impl Item {
    fn label(&self) -> String {
        match self {
            Item::Model(m) => m.label.clone(),
            Item::Provider(p) => format!("{p}/…  (type a model name)"),
        }
    }

    /// A model item matches the filter by substring against the label (which
    /// carries both the tag and the alias); provider hints always show.
    fn visible(&self, filter: &str) -> bool {
        match self {
            Item::Model(m) => {
                filter.is_empty() || m.label.to_lowercase().contains(&filter.to_lowercase())
            }
            Item::Provider(_) => true,
        }
    }
}

#[derive(Debug)]
enum Outcome {
    Pick(String),
    Abort,
}

/// The line under the entry box.
#[derive(Debug, Clone, PartialEq)]
enum Notice {
    Info(String),
    Problem(String),
    /// The quit warning: leaving again quits.
    Leaving,
}

struct PickerState {
    items: Vec<Item>,
    /// Indices into `items` currently shown (after filtering).
    visible: Vec<usize>,
    selected: usize,
    /// Free-entry / filter buffer.
    input: String,
    /// Why the list shows what it shows (Ollama down, the model folder).
    banner: Option<String>,
    /// Nothing to list: the entry line takes a folder or a model file.
    asking_path: bool,
    /// Whether a bare name can work (Ollama is there to serve it).
    takes_names: bool,
    notice: Option<Notice>,
    config: Config,
}

impl PickerState {
    fn new(opts: PickerOptions) -> Self {
        let PickerOptions { choices, config } = opts;
        let banner = choices.banner();
        let asking_path = choices.ask_path();
        let takes_names = choices.takes_ollama_names();
        let mut items: Vec<Item> = choices.models.into_iter().map(Item::Model).collect();
        items.extend(choices.providers.into_iter().map(Item::Provider));
        let mut s = Self {
            items,
            visible: Vec::new(),
            selected: 0,
            input: String::new(),
            banner,
            asking_path,
            takes_names,
            notice: None,
            config,
        };
        s.recompute();
        s
    }

    fn recompute(&mut self) {
        self.visible = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.visible(&self.input))
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
    }

    fn selected_item(&self) -> Option<&Item> {
        self.visible.get(self.selected).map(|&i| &self.items[i])
    }

    fn move_sel(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len);
        self.selected = next as usize;
    }

    /// Nothing chosen: warn the first time, quit the second.
    fn leave(&mut self) -> Option<Outcome> {
        if self.notice == Some(Notice::Leaving) {
            return Some(Outcome::Abort);
        }
        self.notice = Some(Notice::Leaving);
        None
    }

    /// A typed folder or model file.
    fn take_path(&mut self, text: &str) -> Option<Outcome> {
        match first_run::answer_path(text, &self.config) {
            PathAnswer::Model(m) => Some(Outcome::Pick(m)),
            PathAnswer::Folder { dir, models, saved } => {
                // The folder's models replace the list; provider hints stay.
                let hints: Vec<Item> = std::mem::take(&mut self.items)
                    .into_iter()
                    .filter(|i| matches!(i, Item::Provider(_)))
                    .collect();
                self.items = models.into_iter().map(Item::Model).chain(hints).collect();
                self.banner = Some(format!("Models in {}:", dir.display()));
                self.notice = Some(match saved {
                    Ok(path) => Notice::Info(format!(
                        "remembered as your model folder in {} — /localmodel lists it",
                        path.display()
                    )),
                    Err(e) => Notice::Problem(format!("could not remember the folder: {e}")),
                });
                self.asking_path = false;
                self.input.clear();
                self.selected = 0;
                self.recompute();
                None
            }
            PathAnswer::Retry(why) => {
                self.notice = Some(Notice::Problem(why));
                None
            }
        }
    }

    fn on_enter(&mut self) -> Option<Outcome> {
        let trimmed = self.input.trim().to_string();
        if first_run::is_fs_path(&trimmed) {
            return self.take_path(&trimmed);
        }
        // A complete `provider/model` typed by hand → accept it directly.
        if trimmed.contains('/') && !trimmed.ends_with('/') {
            return Some(Outcome::Pick(trimmed));
        }
        if self.asking_path {
            if trimmed.is_empty() {
                return self.leave();
            }
            if !self.takes_names {
                self.notice = Some(Notice::Problem(format!(
                    "`{trimmed}` isn't a folder or a .gguf file, and Ollama isn't there to run it"
                )));
                return None;
            }
            return Some(Outcome::Pick(trimmed));
        }
        match self.selected_item() {
            Some(Item::Model(m)) => Some(Outcome::Pick(m.value.clone())),
            Some(Item::Provider(p)) => {
                // Prefill the entry line; user continues typing the model name.
                self.input = format!("{p}/");
                self.recompute();
                None
            }
            None => {
                if trimmed.is_empty() {
                    None
                } else if self.takes_names {
                    Some(Outcome::Pick(trimmed))
                } else {
                    self.notice = Some(Notice::Problem(format!(
                        "no model matches `{trimmed}` — type a folder or a .gguf file"
                    )));
                    None
                }
            }
        }
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<Outcome> {
        if matches!(code, KeyCode::Char('c')) && mods.contains(KeyModifiers::CONTROL) {
            return Some(Outcome::Abort);
        }
        if code == KeyCode::Esc {
            return self.leave();
        }
        // Any other key takes back the quit warning.
        if self.notice == Some(Notice::Leaving) && code != KeyCode::Enter {
            self.notice = None;
        }
        match code {
            KeyCode::Up => {
                self.move_sel(-1);
                None
            }
            KeyCode::Down => {
                self.move_sel(1);
                None
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.recompute();
                None
            }
            KeyCode::Enter => self.on_enter(),
            // Number-jump only on a shown, unfiltered list; once the user
            // starts typing, digits (e.g. in `qwen3:8b`) filter like any
            // character.
            KeyCode::Char(c)
                if c.is_ascii_digit()
                    && self.input.is_empty()
                    && !self.visible.is_empty()
                    && !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let n = c.to_digit(10).unwrap() as usize;
                if n >= 1 && n <= self.visible.len() {
                    self.selected = n - 1;
                }
                None
            }
            KeyCode::Char(c) if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.input.push(c);
                self.recompute();
                None
            }
            _ => None,
        }
    }
}

/// Run the model picker on its own alternate screen. Returns the chosen model
/// name, or an error (with [`first_run::NO_MODEL`]) if the user leaves.
pub async fn pick_model(opts: PickerOptions) -> anyhow::Result<String> {
    // The startup picker is keyboard-only: no mouse capture.
    let mut terminal = crate::setup_terminal(false)?;
    let result = run_loop(&mut terminal, opts).await;
    crate::restore_terminal();
    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    opts: PickerOptions,
) -> anyhow::Result<String> {
    let mut state = PickerState::new(opts);
    let mut term_events = EventStream::new();
    loop {
        terminal.draw(|f| draw(f, &state))?;
        match term_events.next().await {
            Some(Ok(Event::Key(k))) if k.kind == KeyEventKind::Press => {
                if let Some(outcome) = state.on_key(k.code, k.modifiers) {
                    match outcome {
                        Outcome::Pick(m) => return Ok(m),
                        Outcome::Abort => anyhow::bail!(first_run::NO_MODEL),
                    }
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.into()),
            None => anyhow::bail!(first_run::NO_MODEL),
        }
    }
}

const CYAN: Color = Color::Rgb(0x00, 0xB4, 0xD8);
const PINK: Color = Color::Rgb(0xF4, 0x33, 0xAB);
const AMBER: Color = Color::Rgb(0xF5, 0xA6, 0x23);

fn draw(frame: &mut ratatui::Frame, state: &PickerState) {
    let banner_rows = if state.banner.is_some() { 2 } else { 0 };
    let [
        title_area,
        banner_area,
        list_area,
        entry_area,
        notice_area,
        help_area,
    ] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(banner_rows),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "Select a model",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "   (remembered for next time)",
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    frame.render_widget(title, title_area);

    if let Some(banner) = &state.banner {
        frame.render_widget(
            Paragraph::new(banner.as_str())
                .style(Style::default().fg(AMBER))
                .wrap(Wrap { trim: true }),
            banner_area,
        );
    }

    if state.items.is_empty() {
        let hint = Paragraph::new(vec![
            Line::from("Type a folder of GGUF models, e.g. ~/models"),
            Line::from("or one model file, e.g. ~/models/qwen3-8b-q4_k_m.gguf"),
            Line::from("and press Enter."),
        ])
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(hint, list_area);
    } else {
        let rows: Vec<ListItem> = state
            .visible
            .iter()
            .enumerate()
            .map(|(pos, &idx)| {
                let item = &state.items[idx];
                let number = if pos < 9 {
                    format!("{}. ", pos + 1)
                } else {
                    "   ".to_string()
                };
                let style = match item {
                    Item::Provider(_) => Style::default().fg(Color::DarkGray),
                    Item::Model(_) => Style::default(),
                };
                ListItem::new(Line::from(vec![
                    Span::styled(number, Style::default().fg(Color::DarkGray)),
                    Span::styled(item.label(), style),
                ]))
            })
            .collect();

        let mut list_state = ListState::default();
        if !state.visible.is_empty() {
            list_state.select(Some(state.selected));
        }
        let list = List::new(rows)
            .block(Block::default().borders(Borders::ALL))
            .highlight_style(Style::default().fg(PINK).add_modifier(Modifier::BOLD))
            .highlight_symbol("▌ ");
        frame.render_stateful_widget(list, list_area, &mut list_state);
    }

    let entry_title = if state.asking_path {
        " folder or .gguf file "
    } else {
        " filter · or type a model name, a folder, or a .gguf file "
    };
    let entry = Paragraph::new(Line::from(vec![
        Span::raw("> "),
        Span::raw(&state.input),
        Span::styled("▏", Style::default().fg(CYAN)),
    ]))
    .block(Block::default().borders(Borders::ALL).title(entry_title));
    frame.render_widget(entry, entry_area);

    if let Some(notice) = &state.notice {
        let (text, color) = match notice {
            Notice::Info(t) => (t.as_str(), Color::DarkGray),
            Notice::Problem(t) => (t.as_str(), Color::Red),
            Notice::Leaving if state.asking_path => (first_run::QUIT_WARNING_PATH, AMBER),
            Notice::Leaving => (first_run::QUIT_WARNING, AMBER),
        };
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(color))
                .wrap(Wrap { trim: true }),
            notice_area,
        );
    }

    let help = if state.asking_path {
        "Enter use · Esc quit"
    } else {
        "↑/↓ move · 1-9 jump · type to filter · Enter select · Esc quit"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help,
            Style::default().fg(Color::DarkGray),
        ))),
        help_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocinante_core::first_run::Source;

    fn entry(value: &str, label: &str) -> ModelEntry {
        ModelEntry {
            value: value.into(),
            label: label.into(),
        }
    }

    fn config() -> Config {
        let missing = std::path::Path::new("/nonexistent/a.toml");
        rocinante_core::config::load_from(missing, missing).unwrap()
    }

    fn choices(source: Source, models: Vec<ModelEntry>) -> Choices {
        Choices {
            source,
            models,
            providers: vec!["anthropic".into()],
            local_dir: None,
            ollama_url: Some("http://localhost:11434".into()),
        }
    }

    fn opts() -> PickerOptions {
        PickerOptions {
            choices: choices(
                Source::Ollama,
                vec![
                    entry("glm-5.2:cloud", "glm-5.2:cloud"),
                    entry("qwen3:8b", "qwen3:8b"),
                    // Alias entry: label shows the real model, value is the key.
                    entry("kimiko", "kimi-k3:cloud  (kimiko)"),
                ],
            ),
            config: config(),
        }
    }

    /// Ollama down and no model folder known: nothing to list.
    fn no_ollama() -> PickerOptions {
        PickerOptions {
            choices: Choices {
                providers: vec![],
                ..choices(Source::OllamaDown, vec![])
            },
            config: config(),
        }
    }

    fn press(state: &mut PickerState, code: KeyCode) -> Option<Outcome> {
        state.on_key(code, KeyModifiers::NONE)
    }

    fn type_str(state: &mut PickerState, text: &str) {
        for c in text.chars() {
            press(state, KeyCode::Char(c));
        }
    }

    fn models_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("qwen3-8b-q4.gguf"), b"GGUF").unwrap();
        std::fs::write(dir.path().join("gemma-4b.gguf"), b"GGUF").unwrap();
        dir
    }

    #[test]
    fn enter_selects_highlighted_model() {
        let mut s = PickerState::new(opts());
        // First visible row is the first model.
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "glm-5.2:cloud"));
    }

    #[test]
    fn arrow_moves_selection() {
        let mut s = PickerState::new(opts());
        press(&mut s, KeyCode::Down);
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "qwen3:8b"));
    }

    #[test]
    fn number_jumps_on_unfiltered_list() {
        let mut s = PickerState::new(opts());
        press(&mut s, KeyCode::Char('3'));
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "kimiko"));
    }

    #[test]
    fn typing_filters_and_digit_is_a_char_once_typing() {
        let mut s = PickerState::new(opts());
        // Filter to "qwen3"; the '3' is appended as a filter char (not a jump
        // to item 3), and qwen3:8b stays matched. Provider hints always show.
        type_str(&mut s, "qwen3");
        assert!(
            s.visible
                .iter()
                .any(|&i| matches!(&s.items[i], Item::Model(m) if m.value == "qwen3:8b"))
        );
        assert!(
            !s.visible
                .iter()
                .any(|&i| matches!(&s.items[i], Item::Model(m) if m.value == "kimiko"))
        );
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "qwen3:8b"));
    }

    #[test]
    fn alias_entry_filters_by_both_names_and_picks_the_value() {
        // Filtering by the underlying tag keeps the alias row…
        let mut s = PickerState::new(opts());
        type_str(&mut s, "kimi-k3");
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "kimiko"));

        // …and so does filtering by the alias itself.
        let mut s = PickerState::new(opts());
        type_str(&mut s, "kimiko");
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "kimiko"));
    }

    #[test]
    fn provider_hint_prefills_then_accepts_typed_model() {
        let mut s = PickerState::new(opts());
        // Move to the provider hint (last visible row) and Enter → prefill.
        press(&mut s, KeyCode::Up); // wraps to last item (anthropic hint)
        assert!(press(&mut s, KeyCode::Enter).is_none());
        assert_eq!(s.input, "anthropic/");
        type_str(&mut s, "claude-opus-4-8");
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "anthropic/claude-opus-4-8"));
    }

    #[test]
    fn free_entry_provider_model_accepted() {
        let mut s = PickerState::new(opts());
        type_str(&mut s, "openai/gpt-4o");
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "openai/gpt-4o"));
    }

    #[test]
    fn esc_warns_before_quitting() {
        let mut s = PickerState::new(opts());
        assert!(press(&mut s, KeyCode::Esc).is_none());
        assert_eq!(s.notice, Some(Notice::Leaving));
        assert!(matches!(press(&mut s, KeyCode::Esc), Some(Outcome::Abort)));
    }

    #[test]
    fn any_other_key_takes_the_warning_back() {
        let mut s = PickerState::new(opts());
        press(&mut s, KeyCode::Esc);
        press(&mut s, KeyCode::Down);
        assert_eq!(s.notice, None);
        // So the next Esc warns again rather than quitting.
        assert!(press(&mut s, KeyCode::Esc).is_none());
    }

    #[test]
    fn ctrl_c_quits_at_once() {
        let mut s = PickerState::new(opts());
        let out = s.on_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(out, Some(Outcome::Abort)));
    }

    #[test]
    fn without_ollama_it_asks_for_a_path() {
        let s = PickerState::new(no_ollama());
        assert!(s.asking_path);
        assert!(s.items.is_empty());
        let banner = s.banner.as_deref().unwrap();
        assert!(banner.contains("Ollama isn't running"), "{banner}");
    }

    #[test]
    fn nothing_supplied_warns_then_quits() {
        let mut s = PickerState::new(no_ollama());
        assert!(press(&mut s, KeyCode::Enter).is_none());
        assert_eq!(s.notice, Some(Notice::Leaving));
        assert!(matches!(
            press(&mut s, KeyCode::Enter),
            Some(Outcome::Abort)
        ));
    }

    #[test]
    fn a_model_file_starts_on_it() {
        if !first_run::EMBEDDED_LLAMA {
            return; // model files need the embedded llama.cpp
        }
        let dir = models_dir();
        let file = dir.path().join("gemma-4b.gguf");
        let mut s = PickerState::new(no_ollama());
        type_str(&mut s, &file.display().to_string());
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(
            matches!(&out, Outcome::Pick(m) if *m == format!("local/{}", file.display())),
            "{out:?}"
        );
    }

    #[test]
    fn a_bad_path_says_why_and_stays() {
        let mut s = PickerState::new(no_ollama());
        type_str(&mut s, "/no/such/folder");
        assert!(press(&mut s, KeyCode::Enter).is_none());
        assert!(matches!(&s.notice, Some(Notice::Problem(m)) if m.contains("nothing at")));
        assert!(s.asking_path);
    }

    #[test]
    fn a_bare_name_needs_ollama() {
        let mut s = PickerState::new(no_ollama());
        type_str(&mut s, "qwen3:8b");
        assert!(press(&mut s, KeyCode::Enter).is_none());
        assert!(matches!(&s.notice, Some(Notice::Problem(m)) if m.contains("Ollama")));

        // With Ollama up but empty, a typed tag goes through (it may pull).
        let mut s = PickerState::new(PickerOptions {
            choices: choices(Source::OllamaEmpty, vec![]),
            config: config(),
        });
        assert!(s.asking_path);
        type_str(&mut s, "minimax-m3:cloud");
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "minimax-m3:cloud"));
    }

    // Saves the folder to the real ~/.rocinante/config.toml, so it is not
    // run by default; `answer_path_with` covers the logic in core.
    #[test]
    #[ignore = "writes ~/.rocinante/config.toml"]
    fn a_folder_lists_its_models() {
        let dir = models_dir();
        let mut s = PickerState::new(no_ollama());
        type_str(&mut s, &dir.path().display().to_string());
        assert!(press(&mut s, KeyCode::Enter).is_none());
        assert!(!s.asking_path);
        assert_eq!(s.visible.len(), 2);
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m.ends_with("gemma-4b.gguf")));
    }
}
