//! Pure rendering of the [`App`] model onto a ratatui frame.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};

use rocinante_core::config::Mode;
use rocinante_core::interval;

use crate::app::{
    App, INPUT_HEIGHT, LineKind, PermissionPrompt, QUIT_WINDOW, STATUS_HEIGHT, transcript_lines,
    wrap_text,
};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn view(app: &App, frame: &mut Frame) {
    let [transcript_area, input_area, status_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(INPUT_HEIGHT),
        Constraint::Length(STATUS_HEIGHT),
    ])
    .areas(frame.area());

    draw_transcript(app, frame, transcript_area);
    draw_input(app, frame, input_area);
    draw_status(app, frame, status_area);
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
    let visible: Vec<Line> = lines[start..end]
        .iter()
        .map(|l| Line::styled(l.text.clone(), line_style(l.kind)))
        .collect();
    frame.render_widget(Paragraph::new(visible), area);
}

fn line_style(kind: LineKind) -> Style {
    match kind {
        LineKind::User => Style::new().add_modifier(Modifier::BOLD),
        LineKind::Assistant | LineKind::Blank => Style::new(),
        LineKind::ToolHead => Style::new().fg(Color::Cyan),
        LineKind::ToolOk => Style::new().fg(Color::Green),
        LineKind::ToolErr | LineKind::Error => Style::new().fg(Color::Red),
        LineKind::ToolProgress | LineKind::Notice => Style::new().fg(Color::DarkGray),
        LineKind::Thinking => Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
    }
}

fn draw_input(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::DarkGray));
    let inner = block.inner(area);

    let mut chars: Vec<char> = app.input.text().chars().collect();
    let cursor = app.input.cursor().min(chars.len());
    chars.insert(cursor, '▌');
    // Horizontal scroll: keep the cursor visible in long input.
    let width = inner.width as usize;
    let start = (cursor + 1).saturating_sub(width);
    let text: String = chars[start..].iter().take(width.max(1)).collect();

    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn draw_status(app: &App, frame: &mut Frame, area: Rect) {
    let (label, color) = match app.mode {
        Mode::Normal => (" NORMAL ", Color::Green),
        Mode::Auto => (" AUTO ", Color::Yellow),
        Mode::Plan => (" PLAN ", Color::Magenta),
    };
    let mut spans = vec![
        Span::styled(
            format!(" {} ", app.model_name),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::styled(label, Style::new().fg(Color::Black).bg(color)),
        Span::raw(format!(
            "  ↑{} ↓{} tok",
            fmt_tokens(app.prompt_tokens),
            fmt_tokens(app.completion_tokens)
        )),
    ];
    if let Some(armed) = &app.loop_spec {
        spans.push(Span::styled(
            format!("  ⟳ {}", interval::display(armed.every)),
            Style::new().fg(Color::DarkGray),
        ));
    }
    if app.think {
        spans.push(Span::styled("  ∴ think", Style::new().fg(Color::Magenta)));
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
}
