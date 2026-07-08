//! Pure rendering of the [`App`] model onto a ratatui frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};

use rocinante_core::config::Mode;
use rocinante_core::interval;

use crate::app::{
    App, Cell, INPUT_HEIGHT, PermissionPrompt, QUIT_WINDOW, SIDEBAR_GAP, SIDEBAR_WIDTH,
    STATUS_HEIGHT, transcript_lines, wrap_text,
};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Minimum frame width for the full block wordmark; narrower terminals get
/// the one-line fallback.
const WORDMARK_MIN_FRAME: u16 = 74;

/// Landing-screen tips, picked deterministically per session.
const TIPS: [&str; 3] = [
    "run /init to teach rocinante this project",
    "BRAINBOX.md remembers — /quit saves your session memory",
    "delegate: define [agents.*] and the task tool appears",
];

pub fn view(app: &App, frame: &mut Frame) {
    if !app.interacted {
        draw_landing(app, frame);
        return;
    }
    let (main_area, sidebar_area) = if app.sidebar_visible() {
        // A blank gap column separates the two panes — no divider line.
        let [main, _gap, side] = Layout::horizontal([
            Constraint::Min(1),
            Constraint::Length(SIDEBAR_GAP),
            Constraint::Length(SIDEBAR_WIDTH),
        ])
        .areas(frame.area());
        (main, Some(side))
    } else {
        (frame.area(), None)
    };
    let [transcript_area, input_area, status_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(INPUT_HEIGHT),
        Constraint::Length(STATUS_HEIGHT),
    ])
    .areas(main_area);

    draw_transcript(app, frame, transcript_area);
    draw_input(app, frame, input_area);
    draw_status(app, frame, status_area, sidebar_area.is_some());
    if let Some(side) = sidebar_area {
        draw_sidebar(app, frame, side);
    }
    if let Some(prompt) = app.permissions.front() {
        draw_permission_modal(prompt, frame);
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

/// Input text with the `▌` cursor inserted, horizontally scrolled so the
/// cursor stays visible in `width` columns.
fn input_display(app: &App, width: usize) -> String {
    let mut chars: Vec<char> = app.input.text().chars().collect();
    let cursor = app.input.cursor().min(chars.len());
    chars.insert(cursor, '▌');
    let start = (cursor + 1).saturating_sub(width);
    chars[start..].iter().take(width.max(1)).collect()
}

fn draw_input(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::DarkGray));
    let inner = block.inner(area);
    let text = input_display(app, inner.width as usize);
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn mode_badge(mode: Mode) -> (&'static str, Color) {
    match mode {
        Mode::Normal => (" NORMAL ", Color::Green),
        Mode::Auto => (" AUTO ", Color::Yellow),
        Mode::Plan => (" PLAN ", Color::Magenta),
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
        spans.push(Span::styled(label, Style::new().fg(Color::Black).bg(color)));
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
        if app.think {
            spans.push(Span::styled("  ∴ think", Style::new().fg(Color::Magenta)));
        }
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
    // wordmark · gap · box(4) · hints · gap · tip · notices
    let total = wm_h + 1 + 4 + 1 + 1 + 1 + notices_h;
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

    // Input box: ~60% width, mode+model line inside.
    let box_w = (area.width * 3 / 5)
        .clamp(44, 84)
        .min(area.width.saturating_sub(2));
    let box_rect = Rect {
        x: area.x + (area.width - box_w) / 2,
        y,
        width: box_w,
        height: 4,
    }
    .intersection(area);
    y += 4;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dim)
        .padding(Padding::horizontal(1));
    let inner = block.inner(box_rect);
    let first = if app.input.is_empty() {
        Line::styled("▌ Ask anything… \"fix a TODO in the codebase\"", dim)
    } else {
        Line::raw(input_display(app, inner.width as usize))
    };
    let (label, color) = mode_badge(app.mode);
    let mut second = vec![
        Span::styled(label, Style::new().fg(Color::Black).bg(color)),
        Span::styled(format!(" · {}", app.model_name), dim),
    ];
    if app.think {
        second.push(Span::styled(" ∴ think", Style::new().fg(Color::Magenta)));
    }
    frame.render_widget(
        Paragraph::new(vec![first, Line::from(second)]).block(block),
        box_rect,
    );

    // Hints, right-aligned to the box edge.
    let key = Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD);
    let hints = Line::from(vec![
        Span::styled("shift+tab", key),
        Span::styled(" mode  ", dim),
        Span::styled("/model", key),
        Span::styled(" models  ", dim),
        Span::styled("/think", key),
        Span::styled(" reasoning", dim),
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

    // Brand logo: ROCI magenta + NANTE cyan, bold; three cyan rule lines
    // beneath; version under that.
    let bold = Modifier::BOLD;
    out.push(Line::from(vec![
        Span::styled("ROCI", Style::new().fg(BRAND_MAGENTA).add_modifier(bold)),
        Span::styled("NANTE", Style::new().fg(BRAND_CYAN).add_modifier(bold)),
    ]));
    let rule: String = "─".repeat("ROCINANTE".len());
    for _ in 0..3 {
        out.push(Line::from(Span::styled(
            rule.clone(),
            Style::new().fg(BRAND_CYAN),
        )));
    }
    if !app.session.version.is_empty() {
        out.push(Line::styled(format!("v{}", app.session.version), dim));
    }
    out.push(Line::default());

    out.push(Line::styled("MODEL", dim));
    for part in wrap_text(&app.model_name, body_w) {
        out.push(Line::raw(part));
    }
    let (label, color) = mode_badge(app.mode);
    let mut mode_line = vec![Span::styled(label, Style::new().fg(Color::Black).bg(color))];
    if app.think {
        mode_line.push(Span::styled(" ∴ thinking", Style::new().fg(Color::Magenta)));
    }
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

    if !app.session.agents.is_empty() {
        out.push(Line::default());
        out.push(Line::styled("AGENTS", dim));
        for name in &app.session.agents {
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

fn draw_permission_modal(prompt: &PermissionPrompt, frame: &mut Frame) {
    let area = frame.area();
    let has_detail = prompt.detail.is_some();
    let width = if has_detail {
        area.width.saturating_sub(6).clamp(24, 100)
    } else {
        area.width.saturating_sub(6).clamp(24, 72)
    };
    let body_width = width.saturating_sub(4) as usize;
    let summary = wrap_text(&prompt.summary, body_width);

    // Detail (diff) body, clamped to what the terminal can show.
    let mut detail_lines: Vec<Line> = Vec::new();
    if let Some(detail) = &prompt.detail {
        let budget = area.height.saturating_sub(summary.len() as u16 + 8) as usize;
        let all: Vec<&str> = detail.lines().collect();
        for line in all.iter().take(budget.max(4)) {
            let style = match line.as_bytes().first() {
                Some(b'+') => Style::new().fg(Color::Green),
                Some(b'-') => Style::new().fg(Color::Red),
                Some(b'@') => Style::new().fg(Color::Cyan),
                _ => Style::new().fg(Color::DarkGray),
            };
            let mut text = line.to_string();
            if text.len() > body_width {
                let mut cut = body_width;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
            }
            detail_lines.push(Line::styled(text, style));
        }
        if all.len() > budget.max(4) {
            detail_lines.push(Line::styled(
                format!("… (+{} more lines)", all.len() - budget.max(4)),
                Style::new().fg(Color::DarkGray),
            ));
        }
    }

    let height =
        (summary.len() as u16 + detail_lines.len() as u16 + 4).min(area.height.saturating_sub(2));
    let rect = centered(area, width, height);

    let mut lines: Vec<Line> = summary.into_iter().map(Line::from).collect();
    if !detail_lines.is_empty() {
        lines.push(Line::from(""));
        lines.append(&mut detail_lines);
    }
    lines.push(Line::from(""));
    let key = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    lines.push(Line::from(vec![
        Span::styled("[y]", key),
        Span::raw(" allow   "),
        Span::styled("[a]", key),
        Span::raw(" always   "),
        Span::styled("[n]", key),
        Span::raw(" deny"),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Yellow))
        .padding(Padding::horizontal(1))
        .title(format!(" permission · {} ", prompt.tool_name));
    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
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
        assert_eq!(pick_tip("abc"), TIPS[0]);
        assert_eq!(pick_tip("abcd"), TIPS[1]);
        assert_eq!(pick_tip("abcde"), TIPS[2]);
    }

    fn fixture() -> App {
        let mut a = App::new("glm-5.2:cloud".into(), Mode::Auto, (120, 40), vec![])
            .with_session(SessionInfo {
                agents: vec!["scout".into(), "writer".into()],
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
        // Line 0: ROCI magenta + NANTE cyan.
        assert_eq!(lines[0].spans[0].content, "ROCI");
        assert_eq!(lines[0].spans[0].style.fg, Some(BRAND_MAGENTA));
        assert_eq!(lines[0].spans[1].content, "NANTE");
        assert_eq!(lines[0].spans[1].style.fg, Some(BRAND_CYAN));
        // Lines 1..=3: three cyan rules.
        for line in &lines[1..=3] {
            assert!(line.spans[0].content.chars().all(|c| c == '─'));
            assert_eq!(line.spans[0].style.fg, Some(BRAND_CYAN));
        }
    }

    #[test]
    fn sidebar_sections_assemble_from_fixture() {
        let rows = flatten(&sidebar_lines(&fixture()));
        let expected = [
            "ROCINANTE",
            "─────────",
            "─────────",
            "─────────",
            "v0.2.0",
            "",
            "MODEL",
            "glm-5.2:cloud",
            " AUTO  ∴ thinking",
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
        let rows = flatten(&sidebar_lines(&a));
        for header in ["AGENTS", "SKILLS", "SESSION"] {
            assert!(!rows.contains(&header.to_string()), "{header} not skipped");
        }
        assert!(rows.contains(&"TOKENS".to_string()));
    }
}
