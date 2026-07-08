//! First-run model picker: a minimal, self-contained ratatui screen shown
//! before the main session when no model is remembered and none was passed on
//! the command line. Keyboard-only. Returns the chosen model name, or an error
//! when aborted (Esc / Ctrl+C).

use std::io::Stdout;

use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

/// The two option kinds fed to the picker. Local models (Ollama tags + config
/// aliases) are directly selectable; API providers are hints that prefill the
/// entry line with `provider/` so the user types the exact model.
pub struct PickerOptions {
    pub models: Vec<String>,
    pub providers: Vec<String>,
}

/// The guidance shown / returned when the picker is aborted with no choice.
pub const NO_MODEL_GUIDANCE: &str =
    "no model selected — run `rocinante` interactively to choose one, or pass --model <name>";

enum Item {
    Model(String),
    /// Provider name (anthropic / gemini / openai) for a `provider/…` hint.
    Provider(String),
}

impl Item {
    fn label(&self) -> String {
        match self {
            Item::Model(m) => m.clone(),
            Item::Provider(p) => format!("{p}/…  (type a model name)"),
        }
    }

    /// A model item matches the filter by substring; provider hints always show.
    fn visible(&self, filter: &str) -> bool {
        match self {
            Item::Model(m) => {
                filter.is_empty() || m.to_lowercase().contains(&filter.to_lowercase())
            }
            Item::Provider(_) => true,
        }
    }
}

enum Outcome {
    Pick(String),
    Abort,
}

struct PickerState {
    items: Vec<Item>,
    /// Indices into `items` currently shown (after filtering).
    visible: Vec<usize>,
    selected: usize,
    /// Free-entry / filter buffer.
    input: String,
}

impl PickerState {
    fn new(opts: PickerOptions) -> Self {
        let mut items: Vec<Item> = opts.models.into_iter().map(Item::Model).collect();
        items.extend(opts.providers.into_iter().map(Item::Provider));
        let mut s = Self {
            items,
            visible: Vec::new(),
            selected: 0,
            input: String::new(),
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

    fn on_enter(&mut self) -> Option<Outcome> {
        let trimmed = self.input.trim().to_string();
        // A complete `provider/model` typed by hand → accept it directly.
        if trimmed.contains('/') && !trimmed.ends_with('/') {
            return Some(Outcome::Pick(trimmed));
        }
        match self.selected_item() {
            Some(Item::Model(m)) => Some(Outcome::Pick(m.clone())),
            Some(Item::Provider(p)) => {
                // Prefill the entry line; user continues typing the model name.
                self.input = format!("{p}/");
                self.recompute();
                None
            }
            None => {
                if trimmed.is_empty() {
                    None
                } else {
                    Some(Outcome::Pick(trimmed))
                }
            }
        }
    }

    fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<Outcome> {
        match code {
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => Some(Outcome::Abort),
            KeyCode::Esc => Some(Outcome::Abort),
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
            // Number-jump only on the unfiltered list; once the user starts
            // typing, digits (e.g. in `qwen3:8b`) filter like any character.
            KeyCode::Char(c)
                if c.is_ascii_digit()
                    && self.input.is_empty()
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
/// name, or an error (with [`NO_MODEL_GUIDANCE`]) if the user aborts.
pub async fn pick_model(opts: PickerOptions) -> anyhow::Result<String> {
    let mut terminal = crate::setup_terminal()?;
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
                        Outcome::Abort => anyhow::bail!(NO_MODEL_GUIDANCE),
                    }
                }
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e.into()),
            None => anyhow::bail!(NO_MODEL_GUIDANCE),
        }
    }
}

fn draw(frame: &mut ratatui::Frame, state: &PickerState) {
    let [title_area, list_area, entry_area, help_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "Select a model",
            Style::default()
                .fg(Color::Rgb(0x00, 0xB4, 0xD8))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "   (remembered for next time)",
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    frame.render_widget(title, title_area);

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
        .highlight_style(
            Style::default()
                .fg(Color::Rgb(0xF4, 0x33, 0xAB))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌ ");
    frame.render_stateful_widget(list, list_area, &mut list_state);

    let entry = Paragraph::new(Line::from(vec![
        Span::raw("> "),
        Span::raw(&state.input),
        Span::styled("▏", Style::default().fg(Color::Rgb(0x00, 0xB4, 0xD8))),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" filter / type provider/model or tag "),
    );
    frame.render_widget(entry, entry_area);

    let help = Paragraph::new(Line::from(Span::styled(
        "↑/↓ move · 1-9 jump · type to filter · Enter select · Esc abort",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(help, help_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> PickerOptions {
        PickerOptions {
            models: vec!["glm-5.2:cloud".into(), "qwen3:8b".into(), "main".into()],
            providers: vec!["anthropic".into()],
        }
    }

    fn press(state: &mut PickerState, code: KeyCode) -> Option<Outcome> {
        state.on_key(code, KeyModifiers::NONE)
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
        assert!(matches!(out, Outcome::Pick(m) if m == "main"));
    }

    #[test]
    fn typing_filters_and_digit_is_a_char_once_typing() {
        let mut s = PickerState::new(opts());
        // Filter to "qwen3"; the '3' is appended as a filter char (not a jump
        // to item 3), and qwen3:8b stays matched. Provider hints always show.
        for c in "qwen3".chars() {
            press(&mut s, KeyCode::Char(c));
        }
        // Only qwen3:8b (of the models) matches; the anthropic hint also shows.
        assert!(
            s.visible
                .iter()
                .any(|&i| matches!(&s.items[i], Item::Model(m) if m == "qwen3:8b"))
        );
        assert!(
            !s.visible
                .iter()
                .any(|&i| matches!(&s.items[i], Item::Model(m) if m == "main"))
        );
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "qwen3:8b"));
    }

    #[test]
    fn provider_hint_prefills_then_accepts_typed_model() {
        let mut s = PickerState::new(opts());
        // Move to the provider hint (last visible row) and Enter → prefill.
        press(&mut s, KeyCode::Up); // wraps to last item (anthropic hint)
        assert!(press(&mut s, KeyCode::Enter).is_none());
        assert_eq!(s.input, "anthropic/");
        for c in "claude-opus-4-8".chars() {
            press(&mut s, KeyCode::Char(c));
        }
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "anthropic/claude-opus-4-8"));
    }

    #[test]
    fn free_entry_provider_model_accepted() {
        let mut s = PickerState::new(opts());
        for c in "openai/gpt-4o".chars() {
            press(&mut s, KeyCode::Char(c));
        }
        let out = press(&mut s, KeyCode::Enter).unwrap();
        assert!(matches!(out, Outcome::Pick(m) if m == "openai/gpt-4o"));
    }

    #[test]
    fn esc_aborts() {
        let mut s = PickerState::new(opts());
        assert!(matches!(press(&mut s, KeyCode::Esc), Some(Outcome::Abort)));
    }
}
