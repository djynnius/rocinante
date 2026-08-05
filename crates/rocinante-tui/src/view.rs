//! Pure rendering of the [`App`] model onto a ratatui frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};

use rocinante_core::config::Mode;
use rocinante_core::interval;

use crate::app::{
    App, Cell, MARGIN_X, MARGIN_Y, MAX_INPUT_ROWS, ModelPicker, PermissionPrompt, QUIT_WINDOW,
    SIDEBAR_GAP, SIDEBAR_WIDTH, STATUS_HEIGHT, transcript_lines, wrap_text,
};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Popup panel fill — lighter than the terminal background so modals read
/// as a raised layer, not just a border.
const POPUP_BG: Color = Color::Rgb(0x33, 0x33, 0x33);

/// Minimum frame width for the full block wordmark; narrower terminals get
/// the one-line fallback.
const WORDMARK_MIN_FRAME: u16 = 74;

/// Landing-screen tips, picked deterministically per session.
const TIPS: [&str; 5] = [
    "run /init to teach rocinante this project",
    "BRAINBOX.md remembers — /quit saves your session memory",
    "delegate: define [agents.*] and the task tool appears",
    "automate: /loop 5m <prompt> re-sends on an interval — /loop stop disarms",
    "/config <request> — the agent edits your config (model aliases, providers…)",
];

pub fn view(app: &App, frame: &mut Frame) {
    if !app.interacted {
        draw_landing(app, frame);
        return;
    }
    // Outer margin so nothing hugs the terminal edge.
    let inner = padded(frame.area());
    let (main_area, sidebar_area) = if app.sidebar_visible() {
        // A blank gap column separates the two panes — no divider line.
        let [main, _gap, side] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(SIDEBAR_GAP),
            Constraint::Length(SIDEBAR_WIDTH),
        ])
        .areas(inner);
        (main, Some(side))
    } else {
        (inner, None)
    };
    let [transcript_area, input_area, status_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(app.input_box_height(main_area.width)),
        Constraint::Length(STATUS_HEIGHT),
    ])
    .areas(main_area);

    draw_transcript(app, frame, transcript_area);
    draw_input(app, frame, input_area);
    draw_status(app, frame, status_area, sidebar_area.is_some());
    if let Some(side) = sidebar_area {
        draw_sidebar(app, frame, side);
    }
    if let Some(picker) = &app.model_picker {
        draw_model_picker(picker, frame);
    }
    if let Some(prompt) = app.permissions.front() {
        draw_permission_modal(prompt, app.permission_scroll, frame);
    }
}

/// Inset a rect by the chat-view outer margin.
fn padded(area: Rect) -> Rect {
    Rect {
        x: area.x + MARGIN_X,
        y: area.y + MARGIN_Y,
        width: area.width.saturating_sub(2 * MARGIN_X),
        height: area.height.saturating_sub(2 * MARGIN_Y),
    }
}

fn draw_transcript(app: &App, frame: &mut Frame, area: Rect) {
    let lines = transcript_lines(&app.cells, area.width as usize);
    let height = area.height as usize;
    let scroll = app.scroll.min(lines.len().saturating_sub(height));
    let end = lines.len() - scroll;
    let start = end.saturating_sub(height);
    let visible: Vec<Line> = lines[start..end].to_vec();
    frame.render_widget(Paragraph::new(visible), area);
}

/// Input text with the `▌` cursor inserted, character-wrapped to `width`
/// columns. Past [`MAX_INPUT_ROWS`] the window scrolls vertically so the
/// cursor row stays visible.
fn input_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut chars: Vec<char> = app.input.text().chars().collect();
    let cursor = app.input.cursor().min(chars.len());
    chars.insert(cursor, '▌');
    let rows: Vec<String> = chars.chunks(width).map(|c| c.iter().collect()).collect();
    let visible = MAX_INPUT_ROWS.min(rows.len());
    let end = (cursor / width + 1).max(visible).min(rows.len());
    rows[end - visible..end]
        .iter()
        .cloned()
        .map(Line::raw)
        .collect()
}

fn draw_input(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::DarkGray))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    let lines = input_lines(app, inner.width as usize);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn mode_badge(mode: Mode) -> (&'static str, Color) {
    match mode {
        Mode::Normal => (" NORMAL ", Color::Rgb(0x90, 0xFC, 0xF9)),
        Mode::Auto => (" AUTO ", Color::Rgb(0xFF, 0x59, 0x64)),
        Mode::Plan => (" PLAN ", Color::Rgb(0xCB, 0x04, 0xA5)),
    }
}

/// Legible badge text for a background: black on light colors, white on dark
/// (perceived-luminance threshold at the midpoint).
fn badge_fg(bg: Color) -> Color {
    match bg {
        Color::Rgb(r, g, b) => {
            let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            if lum > 128.0 {
                Color::Black
            } else {
                Color::White
            }
        }
        _ => Color::Black,
    }
}

/// Slim when the sidebar carries model/mode/tokens; full fallback otherwise.
fn draw_status(app: &App, frame: &mut Frame, area: Rect, sidebar: bool) {
    let mut spans = Vec::new();
    if !sidebar {
        let (label, color) = mode_badge(app.mode);
        spans.push(Span::styled(
            format!(" {} ", app.model_name),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            label,
            Style::new().fg(badge_fg(color)).bg(color),
        ));
        spans.push(Span::raw(format!(
            "  ↑{} ↓{} tok",
            fmt_tokens(app.prompt_tokens),
            fmt_tokens(app.completion_tokens)
        )));
        if let Some(armed) = &app.loop_spec {
            spans.push(Span::styled(
                format!("  ⟳ {}", interval::display(armed.every)),
                Style::new().fg(Color::DarkGray),
            ));
        }
        spans.push(Span::styled(
            format!("  ∴ {}", app.effort),
            Style::new().fg(Color::Magenta),
        ));
    }
    if app.running {
        spans.push(Span::styled(
            format!(
                "  {} working · Esc cancels",
                SPINNER[app.spinner % SPINNER.len()]
            ),
            Style::new().fg(Color::Cyan),
        ));
    }
    if app.last_ctrl_c.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
        spans.push(Span::styled(
            "  ctrl+c again to quit",
            Style::new().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---------------------------------------------------------------- wordmark

/// 5-row half-block glyphs, 6 columns each. `ROCINANTE` with 2-space gaps
/// assembles to 70 columns.
fn glyph(c: char) -> [&'static str; 5] {
    match c {
        'R' => [
            "█████▄", //
            "██  ██", //
            "█████▀", //
            "██ ▀█▄", //
            "██  ██", //
        ],
        'O' => [
            "▄████▄", //
            "██  ██", //
            "██  ██", //
            "██  ██", //
            "▀████▀", //
        ],
        'C' => [
            "▄████▄", //
            "██  ▀▀", //
            "██    ", //
            "██  ▄▄", //
            "▀████▀", //
        ],
        'I' => [
            "██████", //
            "  ██  ", //
            "  ██  ", //
            "  ██  ", //
            "██████", //
        ],
        'N' => [
            "██▄ ██", //
            "███▄██", //
            "██▀███", //
            "██ ▀██", //
            "██  ██", //
        ],
        'A' => [
            "▄████▄", //
            "██  ██", //
            "██████", //
            "██  ██", //
            "██  ██", //
        ],
        'T' => [
            "██████", //
            "  ██  ", //
            "  ██  ", //
            "  ██  ", //
            "  ██  ", //
        ],
        'E' => [
            "██████", //
            "██    ", //
            "█████ ", //
            "██    ", //
            "██████", //
        ],
        _ => ["      "; 5],
    }
}

/// The two-tone wordmark: per row, the dark `ROCI` half (with the gap that
/// separates it from `NANTE`) and the bright `NANTE` half.
fn wordmark_rows() -> [(String, String); 5] {
    let assemble = |letters: &str, row: usize| {
        letters
            .chars()
            .map(|c| glyph(c)[row])
            .collect::<Vec<_>>()
            .join("  ")
    };
    std::array::from_fn(|row| {
        (
            format!("{}  ", assemble("ROCI", row)),
            assemble("NANTE", row),
        )
    })
}

/// Deterministic tip pick — no rand dependency, stable within a session.
fn pick_tip(model_name: &str) -> &'static str {
    TIPS[model_name.chars().count() % TIPS.len()]
}

// ----------------------------------------------------------------- landing

/// OpenCode-style landing: centered wordmark, one prominent input box with
/// the mode+model line inside, right-aligned hints, a tip, and a footer.
fn draw_landing(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let dim = Style::new().fg(Color::DarkGray);
    let notices: Vec<&str> = app
        .cells
        .iter()
        .filter_map(|c| match c {
            Cell::Notice(n) => Some(n.as_str()),
            _ => None,
        })
        .collect();

    let wm_h: u16 = if area.width >= WORDMARK_MIN_FRAME {
        5
    } else {
        1
    };
    let notices_h = if notices.is_empty() {
        0
    } else {
        notices.len() as u16 + 1
    };
    // Input box: ~60% width; grows as wrapped input needs more rows, with a
    // mode+model line inside beneath the text.
    let box_w = (area.width * 3 / 5)
        .clamp(44, 84)
        .min(area.width.saturating_sub(2));
    let box_h = app.input_box_height(box_w) + 1; // + mode/model line
    // wordmark · gap · box · hints · gap · tip · notices
    let total = wm_h + 1 + box_h + 1 + 1 + 1 + notices_h;
    let mut y = area.y + area.height.saturating_sub(total) / 2;
    let rect = |height: u16, y: &mut u16| {
        let r = Rect {
            x: area.x,
            y: *y,
            width: area.width,
            height,
        }
        .intersection(area);
        *y += height;
        r
    };

    // Wordmark: ROCI dark, NANTE bright.
    let wm_rect = rect(wm_h, &mut y);
    let wm: Vec<Line> = if wm_h == 5 {
        wordmark_rows()
            .into_iter()
            .map(|(roci, nante)| {
                Line::from(vec![
                    Span::styled(roci, Style::new().fg(Color::Rgb(0xF4, 0x33, 0xAB))),
                    Span::styled(nante, Style::new().fg(Color::Rgb(0x00, 0xB4, 0xD8))),
                ])
            })
            .collect()
    } else {
        vec![Line::styled(
            "▄▀ rocinante",
            Style::new().add_modifier(Modifier::BOLD),
        )]
    };
    frame.render_widget(Paragraph::new(wm).centered(), wm_rect);
    y += 1; // gap

    let box_rect = Rect {
        x: area.x + (area.width - box_w) / 2,
        y,
        width: box_w,
        height: box_h,
    }
    .intersection(area);
    y += box_h;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dim)
        .padding(Padding::horizontal(1));
    let inner = block.inner(box_rect);
    let mut lines = if app.input.is_empty() {
        vec![Line::styled(
            "▌ Ask anything… \"fix a TODO in the codebase\"",
            dim,
        )]
    } else {
        input_lines(app, inner.width as usize)
    };
    let (label, color) = mode_badge(app.mode);
    let mut second = vec![
        Span::styled(label, Style::new().fg(badge_fg(color)).bg(color)),
        Span::styled(format!(" · {}", app.model_name), dim),
    ];
    second.push(Span::styled(
        format!(" ∴ {}", app.effort),
        Style::new().fg(Color::Magenta),
    ));
    lines.push(Line::from(second));
    frame.render_widget(Paragraph::new(lines).block(block), box_rect);

    // Hints, right-aligned to the box edge.
    let key = Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD);
    let hints = Line::from(vec![
        Span::styled("shift+tab", key),
        Span::styled(" mode  ", dim),
        Span::styled("/model", key),
        Span::styled(" models  ", dim),
        Span::styled("/effort", key),
        Span::styled(" reasoning  ", dim),
        Span::styled("/loop", key),
        Span::styled(" repeat", dim),
    ]);
    let hints_rect = Rect {
        x: box_rect.x,
        y,
        width: box_rect.width,
        height: 1,
    }
    .intersection(area);
    y += 2; // hints + gap
    frame.render_widget(Paragraph::new(hints).right_aligned(), hints_rect);

    // Tip.
    let tip = Line::from(vec![
        Span::styled("● ", Style::new().fg(Color::Indexed(208))),
        Span::styled("Tip  ", Style::new().add_modifier(Modifier::BOLD)),
        Span::styled(pick_tip(&app.model_name), dim),
    ]);
    let tip_rect = rect(1, &mut y);
    frame.render_widget(Paragraph::new(tip).centered(), tip_rect);

    // Resume notices, dim, under the tip.
    if !notices.is_empty() {
        y += 1;
        let lines: Vec<Line> = notices
            .iter()
            .map(|n| Line::styled((*n).to_string(), dim))
            .collect();
        let r = rect(notices.len() as u16, &mut y);
        frame.render_widget(Paragraph::new(lines).centered(), r);
    }

    // Footer: `~` bottom-left, version bottom-right.
    let footer = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    }
    .intersection(area);
    frame.render_widget(Paragraph::new(Span::styled(" ~", dim)), footer);
    if !app.session.version.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(format!("v{} ", app.session.version), dim)).right_aligned(),
            footer,
        );
    }
}

// ----------------------------------------------------------------- sidebar

/// 10-segment context gauge, e.g. `▰▰▱▱▱▱▱▱▱▱ 18%`; overfull clamps to 100%.
fn ctx_gauge(used: u64, num_ctx: u32) -> String {
    let pct = if num_ctx == 0 {
        0
    } else {
        (used * 100 / num_ctx as u64).min(100)
    };
    let filled = ((pct + 5) / 10) as usize;
    format!("{}{} {pct}%", "▰".repeat(filled), "▱".repeat(10 - filled))
}

/// Crush-style live sidebar content; pure so tests can assert the sections.
/// Brand colors, shared with the landing wordmark.
const BRAND_MAGENTA: Color = Color::Rgb(0xF4, 0x33, 0xAB);
const BRAND_CYAN: Color = Color::Rgb(0x00, 0xB4, 0xD8);

fn sidebar_lines(app: &App) -> Vec<Line<'static>> {
    const SKILL_CAP: usize = 8;
    let dim = Style::new().fg(Color::DarkGray);
    let body_w = SIDEBAR_WIDTH as usize - 2; // left padding only, no border
    let mut out = Vec::new();

    // Brand logo: ROCI magenta + NANTE cyan, bold and letter-spaced so the
    // wordmark reads larger than the body text. Beneath it a single cyan
    // triple-bar rule (≡) — three lines packed tight into one row's height,
    // like the diagonal strokes in Crush rather than three full rows apart.
    let bold = Modifier::BOLD;
    let roci = "R O C I";
    let nante = "N A N T E";
    out.push(Line::from(vec![
        Span::styled(roci, Style::new().fg(BRAND_MAGENTA).add_modifier(bold)),
        Span::styled(
            format!(" {nante}"),
            Style::new().fg(BRAND_CYAN).add_modifier(bold),
        ),
    ]));
    let rule_w = roci.chars().count() + 1 + nante.chars().count();
    out.push(Line::from(Span::styled(
        "≡".repeat(rule_w),
        Style::new().fg(BRAND_CYAN),
    )));
    if !app.session.version.is_empty() {
        out.push(Line::styled(format!("v{}", app.session.version), dim));
    }
    out.push(Line::default());

    out.push(Line::styled("MODEL", dim));
    for part in wrap_text(&app.model_name, body_w) {
        out.push(Line::raw(part));
    }
    let (label, color) = mode_badge(app.mode);
    let mut mode_line = vec![Span::styled(
        label,
        Style::new().fg(badge_fg(color)).bg(color),
    )];
    mode_line.push(Span::styled(
        format!(" ∴ {}", app.effort),
        Style::new().fg(Color::Magenta),
    ));
    out.push(Line::from(mode_line));
    out.push(Line::default());

    out.push(Line::styled("TOKENS", dim));
    out.push(Line::raw(format!(
        "↑ {} prompt",
        fmt_tokens(app.prompt_tokens)
    )));
    out.push(Line::raw(format!(
        "↓ {} completion",
        fmt_tokens(app.completion_tokens)
    )));
    if app.session.num_ctx > 0 {
        out.push(Line::from(vec![
            Span::styled("ctx ", dim),
            Span::raw(ctx_gauge(app.last_prompt_tokens, app.session.num_ctx)),
        ]));
    }

    // User agents are always listed; built-ins surface only while running
    // (spinner) or after acting this turn (✓) — never as idle rows.
    let builtins_in_use = app
        .session
        .builtin_agents
        .iter()
        .filter(|name| app.running_count(name) > 0 || app.active_agents.contains(*name));
    let listed: Vec<&String> = app.session.agents.iter().chain(builtins_in_use).collect();
    if !listed.is_empty() {
        out.push(Line::default());
        out.push(Line::styled("AGENTS", dim));
        for name in listed {
            let running = app.running_count(name);
            if running > 0 {
                // Running right now: animated spinner + instance count.
                let glyph = SPINNER[app.spinner % SPINNER.len()];
                let mut spans = vec![
                    Span::styled(format!("{glyph} "), Style::new().fg(Color::Cyan)),
                    Span::styled(
                        name.clone(),
                        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                ];
                if running > 1 {
                    spans.push(Span::styled(
                        format!(" ×{running}"),
                        Style::new().fg(Color::Cyan),
                    ));
                }
                out.push(Line::from(spans));
            } else if app.active_agents.contains(name) {
                // Ran this turn, now idle.
                out.push(Line::from(vec![
                    Span::styled("✓ ", Style::new().fg(Color::Green)),
                    Span::styled(name.clone(), Style::new().fg(Color::Green)),
                ]));
            } else {
                out.push(Line::from(vec![
                    Span::styled("○ ", dim),
                    Span::styled(name.clone(), dim),
                ]));
            }
        }
    }

    if !app.session.skills.is_empty() {
        out.push(Line::default());
        out.push(Line::styled("SKILLS", dim));
        for name in app.session.skills.iter().take(SKILL_CAP) {
            out.push(Line::styled(name.clone(), dim));
        }
        if app.session.skills.len() > SKILL_CAP {
            out.push(Line::styled(
                format!("+{} more", app.session.skills.len() - SKILL_CAP),
                dim,
            ));
        }
    }

    let mut session_rows: Vec<Line<'static>> = Vec::new();
    if let Some(armed) = &app.loop_spec {
        session_rows.push(Line::styled(
            format!("⟳ loop {}", interval::display(armed.every)),
            Style::new().fg(Color::Cyan),
        ));
    }
    if let Some(pin) = &app.submodel {
        session_rows.push(Line::styled(
            format!("⚲ subagents: {pin}"),
            Style::new().fg(Color::Cyan),
        ));
    }
    if app.session.mcp_tools > 0 {
        session_rows.push(Line::raw(format!("mcp tools: {}", app.session.mcp_tools)));
    }
    if app.session.lsp_available {
        session_rows.push(Line::raw("lsp ready"));
    }
    if !session_rows.is_empty() {
        out.push(Line::default());
        out.push(Line::styled("SESSION", dim));
        out.append(&mut session_rows);
    }
    out
}

fn draw_sidebar(app: &App, frame: &mut Frame, area: Rect) {
    // Borderless: the gap column to its left provides separation. A small
    // left padding keeps text off the edge.
    let block = Block::default().padding(Padding::left(1));
    frame.render_widget(Paragraph::new(sidebar_lines(app)).block(block), area);
}

/// The `/model` overlay: scroll with Up/Down, Enter switches, Esc closes.
fn draw_model_picker(picker: &ModelPicker, frame: &mut Frame) {
    let area = frame.area();
    let longest = picker
        .entries
        .iter()
        .map(|e| e.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let width = (longest + 16).clamp(36, area.width.saturating_sub(4));
    let max_rows = area.height.saturating_sub(6) as usize;
    let visible = picker.entries.len().min(max_rows.max(1));
    let rect = centered(area, width, visible as u16 + 3);

    // Window the list so the selection stays visible.
    let end = (picker.selected + 1).max(visible).min(picker.entries.len());
    let start = end - visible;
    let mut lines: Vec<Line> = Vec::new();
    for (i, entry) in picker.entries.iter().enumerate().take(end).skip(start) {
        let is_current = *entry == picker.current;
        let label = if is_current {
            format!("{entry} (current)")
        } else {
            entry.clone()
        };
        if i == picker.selected {
            lines.push(Line::from(vec![
                Span::styled("▌ ", Style::new().fg(BRAND_MAGENTA)),
                Span::styled(
                    label,
                    Style::new().fg(BRAND_MAGENTA).add_modifier(Modifier::BOLD),
                ),
            ]));
        } else {
            let style = if is_current {
                Style::new().fg(Color::Gray)
            } else {
                Style::new()
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(label, style),
            ]));
        }
    }
    let footer = if picker.entries.len() > visible {
        format!(
            "↑↓ select · enter switch · esc close · {}/{}",
            picker.selected + 1,
            picker.entries.len()
        )
    } else {
        "↑↓ select · enter switch · esc close".to_string()
    };
    lines.push(Line::styled(footer, Style::new().fg(Color::DarkGray)));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(BRAND_MAGENTA))
        .padding(Padding::horizontal(1))
        .title(" model ");
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::new().bg(POPUP_BG)),
        rect,
    );
}

/// Wrapped detail lines for the permission modal, one styled Line per
/// wrapped row; pure so tests can assert no truncation.
fn permission_detail_lines(detail: &str, body_width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for line in detail.lines() {
        let style = match line.as_bytes().first() {
            Some(b'+') => Style::new().fg(Color::Green),
            Some(b'-') => Style::new().fg(Color::Red),
            Some(b'@') => Style::new().fg(Color::Cyan),
            _ => Style::new().fg(Color::DarkGray),
        };
        let parts = wrap_text(line, body_width);
        if parts.is_empty() {
            out.push(Line::styled(String::new(), style));
        }
        for part in parts {
            out.push(Line::styled(part, style));
        }
    }
    out
}

fn draw_permission_modal(prompt: &PermissionPrompt, scroll: usize, frame: &mut Frame) {
    let area = frame.area();
    let has_detail = prompt.detail.is_some();
    let width = if has_detail {
        area.width.saturating_sub(6).clamp(24, 100)
    } else {
        area.width.saturating_sub(6).clamp(24, 72)
    };
    let body_width = width.saturating_sub(4) as usize;
    let summary = wrap_text(&prompt.summary, body_width);

    // Full wrapped detail — nothing truncated; overflow scrolls instead.
    let detail_lines: Vec<Line> = match &prompt.detail {
        Some(detail) => permission_detail_lines(detail, body_width),
        None => Vec::new(),
    };
    let total = detail_lines.len();
    // Rows the frame spends outside the detail body: borders, summary,
    // blank separators, footer.
    let chrome = 2 + summary.len() + usize::from(has_detail) + 2;
    let budget = (area.height.saturating_sub(2) as usize)
        .saturating_sub(chrome)
        .max(usize::from(has_detail) * 4);
    let shown_n = total.min(budget);
    let offset = scroll.min(total - shown_n);
    let shown: Vec<Line> = detail_lines
        .into_iter()
        .skip(offset)
        .take(shown_n)
        .collect();

    let height = ((chrome + shown_n) as u16).min(area.height.saturating_sub(2));
    let rect = centered(area, width, height);

    let mut lines: Vec<Line> = summary.into_iter().map(Line::from).collect();
    if has_detail {
        lines.push(Line::from(""));
        lines.extend(shown);
    }
    lines.push(Line::from(""));
    let key = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let mut footer = vec![
        Span::styled("[y]", key),
        Span::raw(" allow   "),
        Span::styled("[a]", key),
        Span::raw(" always   "),
        Span::styled("[n]", key),
        Span::raw(" deny"),
    ];
    if total > shown_n {
        footer.push(Span::styled(
            format!("   ↑↓ scroll {}–{}/{}", offset + 1, offset + shown_n, total),
            Style::new().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(footer));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(BRAND_MAGENTA))
        .padding(Padding::horizontal(1))
        .title(format!(" permission · {} ", prompt.tool_name));
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .style(Style::new().bg(POPUP_BG)),
        rect,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{LoopSpec, SessionInfo};
    use std::time::{Duration, Instant};

    #[test]
    fn fmt_tokens_humanizes_large_counts() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(9_999), "9999");
        assert_eq!(fmt_tokens(12_345), "12.3k");
    }

    #[test]
    fn centered_never_exceeds_area() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 5,
        };
        let r = centered(area, 72, 20);
        assert_eq!((r.width, r.height), (10, 5));
        assert_eq!((r.x, r.y), (0, 0));
    }

    #[test]
    fn wordmark_rows_have_equal_width() {
        let rows = wordmark_rows();
        let width = |s: &str| s.chars().count();
        let total = width(&rows[0].0) + width(&rows[0].1);
        for (roci, nante) in &rows {
            assert_eq!(width(roci) + width(nante), total, "ragged wordmark row");
        }
        assert!(
            (63..=72).contains(&total),
            "wordmark width {total} outside 63..=72"
        );
    }

    #[test]
    fn every_glyph_is_five_rows_of_equal_width() {
        for c in "ROCINATE".chars() {
            let g = glyph(c);
            let w = g[0].chars().count();
            assert!(w > 0, "glyph {c} is empty");
            for row in g {
                assert_eq!(row.chars().count(), w, "glyph {c} has ragged rows");
            }
        }
    }

    #[test]
    fn ctx_gauge_math() {
        assert_eq!(ctx_gauge(0, 32_768), "▱▱▱▱▱▱▱▱▱▱ 0%");
        assert_eq!(ctx_gauge(16_384, 32_768), "▰▰▰▰▰▱▱▱▱▱ 50%");
        assert_eq!(ctx_gauge(100_000, 32_768), "▰▰▰▰▰▰▰▰▰▰ 100%");
        assert_eq!(ctx_gauge(123, 0), "▱▱▱▱▱▱▱▱▱▱ 0%", "no num_ctx, no gauge");
    }

    #[test]
    fn pick_tip_is_deterministic() {
        assert_eq!(pick_tip("abcde"), TIPS[0]);
        assert_eq!(pick_tip("abcdef"), TIPS[1]);
        assert_eq!(pick_tip("abcdefg"), TIPS[2]);
        assert_eq!(pick_tip("abcdefgh"), TIPS[3]);
        assert_eq!(pick_tip("abcd"), TIPS[4]);
    }

    fn fixture() -> App {
        let mut a = App::new("glm-5.2:cloud".into(), Mode::Auto, (120, 40), vec![])
            .with_session(SessionInfo {
                agents: vec!["scout".into(), "writer".into()],
                builtin_agents: vec!["naomi".into()],
                skills: (1..=10).map(|i| format!("skill{i}")).collect(),
                mcp_tools: 3,
                lsp_available: true,
                num_ctx: 32_768,
                version: "0.2.0",
                resumed: false,
            })
            .with_resumed();
        a.think = true;
        a.prompt_tokens = 12_345;
        a.completion_tokens = 678;
        a.last_prompt_tokens = 16_384;
        a.active_agents.insert("scout".into());
        a.loop_spec = Some(LoopSpec {
            prompt: "check ci".into(),
            every: Duration::from_secs(300),
            next_due: Instant::now(),
        });
        a
    }

    fn flatten(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn sidebar_logo_is_two_tone_with_cyan_rules() {
        let lines = sidebar_lines(&fixture());
        // Line 0: letter-spaced ROCI magenta + NANTE cyan, both bold.
        assert_eq!(lines[0].spans[0].content, "R O C I");
        assert_eq!(lines[0].spans[0].style.fg, Some(BRAND_MAGENTA));
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(lines[0].spans[1].content, " N A N T E");
        assert_eq!(lines[0].spans[1].style.fg, Some(BRAND_CYAN));
        assert!(
            lines[0].spans[1]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        // Line 1: a single cyan triple-bar rule.
        assert!(lines[1].spans[0].content.chars().all(|c| c == '≡'));
        assert_eq!(lines[1].spans[0].style.fg, Some(BRAND_CYAN));
    }

    #[test]
    fn sidebar_sections_assemble_from_fixture() {
        let rows = flatten(&sidebar_lines(&fixture()));
        let expected = [
            "R O C I N A N T E",
            "≡≡≡≡≡≡≡≡≡≡≡≡≡≡≡≡≡",
            "v0.2.0",
            "",
            "MODEL",
            "glm-5.2:cloud",
            " AUTO  ∴ high",
            "",
            "TOKENS",
            "↑ 12.3k prompt",
            "↓ 678 completion",
            "ctx ▰▰▰▰▰▱▱▱▱▱ 50%",
            "",
            "AGENTS",
            "✓ scout",
            "○ writer",
            "",
            "SKILLS",
            "skill1",
            "skill2",
            "skill3",
            "skill4",
            "skill5",
            "skill6",
            "skill7",
            "skill8",
            "+2 more",
            "",
            "SESSION",
            "⟳ loop 5m",
            "mcp tools: 3",
            "lsp ready",
        ];
        assert_eq!(rows, expected);
    }

    #[test]
    fn sidebar_shows_running_agents_with_counts() {
        let mut a = fixture();
        // Three scout instances running now; writer idle.
        a.running_agents.insert("c1".into(), "scout".into());
        a.running_agents.insert("c2".into(), "scout".into());
        a.running_agents.insert("c3".into(), "scout".into());
        let rows = flatten(&sidebar_lines(&a));
        let agents_start = rows.iter().position(|r| r == "AGENTS").unwrap();
        // Running row: spinner glyph + name + ×3 (scout was also in
        // active_agents, but running wins).
        assert!(
            rows[agents_start + 1].contains("scout") && rows[agents_start + 1].contains("×3"),
            "got: {}",
            rows[agents_start + 1]
        );
        assert_eq!(rows[agents_start + 2], "○ writer");
    }

    #[test]
    fn single_running_instance_has_no_count_suffix() {
        let mut a = fixture();
        a.active_agents.clear();
        a.running_agents.insert("c1".into(), "scout".into());
        let rows = flatten(&sidebar_lines(&a));
        let scout = rows.iter().find(|r| r.contains("scout")).unwrap();
        assert!(!scout.contains("×"), "no count at 1 instance: {scout}");
    }

    #[test]
    fn sidebar_skips_empty_sections() {
        let mut a = fixture();
        a.session.agents.clear();
        a.session.skills.clear();
        a.session.mcp_tools = 0;
        a.session.lsp_available = false;
        a.loop_spec = None;
        a.active_agents.clear();
        let rows = flatten(&sidebar_lines(&a));
        // The idle built-in (naomi) alone must not resurrect the section.
        for header in ["AGENTS", "SKILLS", "SESSION"] {
            assert!(!rows.contains(&header.to_string()), "{header} not skipped");
        }
        assert!(rows.contains(&"TOKENS".to_string()));
    }

    #[test]
    fn builtin_agents_surface_only_while_in_use() {
        let mut a = fixture();
        let rows = flatten(&sidebar_lines(&a));
        assert!(
            !rows.iter().any(|r| r.contains("naomi")),
            "idle built-in listed: {rows:?}"
        );
        // Running: spinner row appears.
        a.running_agents.insert("c9".into(), "naomi".into());
        let rows = flatten(&sidebar_lines(&a));
        assert!(
            rows.iter().any(|r| r.contains("naomi")),
            "running built-in missing"
        );
        // Finished this turn: ✓ row remains.
        a.running_agents.clear();
        a.active_agents.insert("naomi".into());
        let rows = flatten(&sidebar_lines(&a));
        assert!(rows.contains(&"✓ naomi".to_string()), "got: {rows:?}");
    }

    fn type_str(a: &mut App, s: &str) {
        use ratatui::crossterm::event::{KeyEvent, KeyModifiers};
        for c in s.chars() {
            a.update(crate::app::Msg::Key(KeyEvent::new(
                ratatui::crossterm::event::KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
    }

    #[test]
    fn permission_detail_wraps_instead_of_truncating() {
        let long = format!("+{}", "x".repeat(95));
        let detail = format!("{long}\n-short\n@@ hunk @@");
        let lines = permission_detail_lines(&detail, 40);
        // 96 chars at width 40 → 3 wrapped rows, all green, nothing lost.
        let plus_rows: Vec<&Line> = lines
            .iter()
            .filter(|l| l.style.fg == Some(Color::Green))
            .collect();
        assert_eq!(plus_rows.len(), 3, "long + line must wrap, not truncate");
        let rejoined: String = plus_rows
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(rejoined.chars().count(), 96, "no characters dropped");
        assert!(lines.iter().any(|l| l.style.fg == Some(Color::Red)));
        assert!(lines.iter().any(|l| l.style.fg == Some(Color::Cyan)));
    }

    #[test]
    fn input_lines_wrap_at_width() {
        let mut a = App::new("m".into(), Mode::Normal, (80, 24), vec![]);
        type_str(&mut a, "abcdefghij");
        assert_eq!(flatten(&input_lines(&a, 4)), ["abcd", "efgh", "ij▌"]);
    }

    #[test]
    fn input_lines_scroll_to_keep_cursor_visible() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut a = App::new("m".into(), Mode::Normal, (80, 24), vec![]);
        type_str(&mut a, &"x".repeat(4 * (MAX_INPUT_ROWS + 3)));
        // Cursor at the end: window is the last MAX_INPUT_ROWS rows.
        let rows = flatten(&input_lines(&a, 4));
        assert_eq!(rows.len(), MAX_INPUT_ROWS);
        assert!(rows.last().unwrap().contains('▌'), "got: {rows:?}");
        // Cursor jumped home: window snaps back to the top.
        a.update(crate::app::Msg::Key(KeyEvent::new(
            KeyCode::Home,
            KeyModifiers::NONE,
        )));
        let rows = flatten(&input_lines(&a, 4));
        assert_eq!(rows.len(), MAX_INPUT_ROWS);
        assert!(rows[0].starts_with('▌'), "got: {rows:?}");
    }
}
