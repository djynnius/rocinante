//! Pure terminal-markdown renderer for the transcript. Turns a source string
//! into styled [`Line`]s with a style-preserving greedy word-wrap. No terminal
//! I/O, so every rule is unit-testable.
//!
//! Supported: fenced code blocks, ATX headers, `-`/`*`/`N.` lists, blockquotes,
//! and inline **bold**, *italic*, `code`, and `[link](url)`. Unmatched or
//! half-typed markers render literally, so streaming partial output is fine.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Fenced/inline code color.
const CODE: Color = Color::Rgb(0x35, 0xA7, 0xFF);
/// Bold emphasis color.
const BOLD: Color = Color::Rgb(0xFF, 0x59, 0x64);
/// Link label color.
const LINK: Color = Color::Rgb(0x00, 0xB4, 0xD8);

/// Render `text` to styled lines wrapped to `width`. `base` styles unstyled
/// runs (default for assistant text, dim for notices). Empty input → no lines.
pub fn render(text: &str, width: usize, base: Style) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let width = width.max(1);
    let mut out = Vec::new();
    let mut in_fence = false;
    for raw in text.split('\n') {
        let trimmed = raw.trim_start();

        // Fenced code: toggle on ```; the fence line (and language tag) is
        // dropped and inner lines render verbatim, blue, no inline parsing.
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            let style = Style::new().fg(CODE);
            for chunk in hard_wrap(raw, width) {
                out.push(Line::from(Span::styled(chunk, style)));
            }
            continue;
        }

        if let Some(rest) = strip_header(trimmed) {
            let content = inline(rest, base.add_modifier(Modifier::BOLD));
            out.extend(wrap_styled(content, width, 0, base));
        } else if let Some(rest) = strip_list(trimmed) {
            let mut content = vec![('•', base), (' ', base)];
            content.extend(inline(rest, base));
            out.extend(wrap_styled(content, width, 2, base));
        } else if let Some(rest) = strip_blockquote(trimmed) {
            let dim = base.add_modifier(Modifier::DIM).fg(Color::DarkGray);
            let content = inline(rest, dim);
            out.extend(wrap_styled(content, width, 0, dim));
        } else {
            let content = inline(raw, base);
            out.extend(wrap_styled(content, width, 0, base));
        }
    }
    out
}

/// `#`..`######` header → text after the hashes (and one space).
fn strip_header(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &line[hashes..];
        if rest.is_empty() {
            return Some("");
        }
        if let Some(stripped) = rest.strip_prefix(' ') {
            return Some(stripped.trim_start());
        }
    }
    None
}

/// `- `/`* `/`+ `/`N. ` list marker → the item text.
fn strip_list(line: &str) -> Option<&str> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(m) {
            return Some(rest);
        }
    }
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0
        && let Some(rest) = line[digits..].strip_prefix(". ")
    {
        return Some(rest);
    }
    None
}

/// `> ` (or bare `>`) blockquote → the quoted text.
fn strip_blockquote(line: &str) -> Option<&str> {
    line.strip_prefix("> ").or_else(|| line.strip_prefix('>'))
}

/// Inline pass: source → a per-char `(char, Style)` stream, so the wrapper can
/// break anywhere and still carry styles into spans.
fn inline(text: &str, base: Style) -> Vec<(char, Style)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        // Backslash escape: the next marker renders literally.
        if c == '\\' && i + 1 < chars.len() && "*_`[]()\\".contains(chars[i + 1]) {
            out.push((chars[i + 1], base));
            i += 2;
            continue;
        }

        // Bold `**x**` / `__x__` (match doubles before singles).
        if (c == '*' || c == '_')
            && i + 2 < chars.len()
            && chars[i + 1] == c
            && chars[i + 2] != ' '
            && let Some(close) = find_double(&chars, i + 2, c)
            && close > i + 2
        {
            let style = base.fg(BOLD).add_modifier(Modifier::BOLD);
            out.extend(chars[i + 2..close].iter().map(|&ch| (ch, style)));
            i = close + 2;
            continue;
        }

        // Italic `*x*` / `_x_`.
        if (c == '*' || c == '_')
            && i + 1 < chars.len()
            && chars[i + 1] != ' '
            && let Some(close) = find_single(&chars, i + 1, c)
            && close > i + 1
        {
            let style = base.add_modifier(Modifier::ITALIC);
            out.extend(chars[i + 1..close].iter().map(|&ch| (ch, style)));
            i = close + 1;
            continue;
        }

        // Inline code `x`.
        if c == '`'
            && let Some(close) = find_char(&chars, i + 1, '`')
        {
            let style = base.fg(CODE);
            out.extend(chars[i + 1..close].iter().map(|&ch| (ch, style)));
            i = close + 1;
            continue;
        }

        // Link `[label](url)` → label styled, url dropped.
        if c == '['
            && let Some((label_end, url_end)) = find_link(&chars, i)
        {
            let style = base.fg(LINK).add_modifier(Modifier::UNDERLINED);
            out.extend(chars[i + 1..label_end].iter().map(|&ch| (ch, style)));
            i = url_end + 1;
            continue;
        }

        out.push((c, base));
        i += 1;
    }
    out
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == target)
}

/// First index `j >= from` of two consecutive `marker` chars.
fn find_double(chars: &[char], from: usize, marker: char) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|&j| chars[j] == marker && chars[j + 1] == marker)
}

/// First index `j >= from` of a single `marker`, but not one that opens a `**`.
fn find_single(chars: &[char], from: usize, marker: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == marker)
}

/// `[label](url)` starting at `i` → `(index of ']', index of ')')`.
fn find_link(chars: &[char], i: usize) -> Option<(usize, usize)> {
    let close_br = find_char(chars, i + 1, ']')?;
    if chars.get(close_br + 1) != Some(&'(') {
        return None;
    }
    let close_paren = find_char(chars, close_br + 2, ')')?;
    Some((close_br, close_paren))
}

/// Style-preserving greedy word-wrap. `indent` left-pads continuation lines.
fn wrap_styled(
    content: Vec<(char, Style)>,
    width: usize,
    indent: usize,
    base: Style,
) -> Vec<Line<'static>> {
    if content.is_empty() {
        return vec![Line::default()];
    }
    let width = width.max(1);
    let n = content.len();
    let mut out = Vec::new();
    let mut start = 0;
    let mut first = true;
    while start < n {
        let avail = if first {
            width
        } else {
            width.saturating_sub(indent)
        }
        .max(1);
        let end = (start + avail).min(n);
        let brk = if end < n {
            content[start..end]
                .iter()
                .rposition(|(c, _)| *c == ' ')
                .map(|p| start + p + 1)
                .unwrap_or(end)
        } else {
            end
        };
        let mut seg_end = brk;
        while seg_end > start && content[seg_end - 1].0 == ' ' {
            seg_end -= 1;
        }
        let lead = if first { 0 } else { indent };
        out.push(assemble(&content[start..seg_end], lead, base));
        start = brk;
        while start < n && content[start].0 == ' ' {
            start += 1;
        }
        first = false;
    }
    out
}

/// Coalesce a `(char, Style)` slice into spans of like style, with `lead`
/// leading spaces in `base` style.
fn assemble(seg: &[(char, Style)], lead: usize, base: Style) -> Line<'static> {
    let mut spans = Vec::new();
    if lead > 0 {
        spans.push(Span::styled(" ".repeat(lead), base));
    }
    let mut i = 0;
    while i < seg.len() {
        let style = seg[i].1;
        let mut text = String::new();
        while i < seg.len() && seg[i].1 == style {
            text.push(seg[i].0);
            i += 1;
        }
        spans.push(Span::styled(text, style));
    }
    if spans.is_empty() {
        Line::default()
    } else {
        Line::from(spans)
    }
}

/// Hard wrap (no space breaking) for code-block lines that exceed `width`.
fn hard_wrap(line: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= width {
        return vec![line.to_string()];
    }
    chars
        .chunks(width)
        .map(|c| c.iter().collect::<String>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (text, fg, modifiers) per span, flattened across all lines.
    fn spans(lines: &[Line]) -> Vec<(String, Option<Color>, Modifier)> {
        lines
            .iter()
            .flat_map(|l| {
                l.spans
                    .iter()
                    .map(|s| (s.content.to_string(), s.style.fg, s.style.add_modifier))
            })
            .collect()
    }

    fn text_of(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn bold_is_coral_and_bold() {
        let s = spans(&render("**hi**", 80, Style::new()));
        assert_eq!(s, vec![("hi".into(), Some(BOLD), Modifier::BOLD)]);
    }

    #[test]
    fn italic_is_italic_modifier() {
        let s = spans(&render("*hi*", 80, Style::new()));
        assert_eq!(s, vec![("hi".into(), None, Modifier::ITALIC)]);
    }

    #[test]
    fn code_is_blue_not_bold() {
        let s = spans(&render("`x`", 80, Style::new()));
        assert_eq!(s, vec![("x".into(), Some(CODE), Modifier::empty())]);
    }

    #[test]
    fn link_is_underlined_cyan_with_url_dropped() {
        let s = spans(&render("[docs](https://example.com)", 80, Style::new()));
        assert_eq!(s, vec![("docs".into(), Some(LINK), Modifier::UNDERLINED)]);
        assert!(
            !text_of(&render("[docs](https://example.com)", 80, Style::new()))
                .join("")
                .contains("example")
        );
    }

    #[test]
    fn header_is_bold() {
        let s = spans(&render("# Title", 80, Style::new()));
        assert_eq!(s, vec![("Title".into(), None, Modifier::BOLD)]);
    }

    #[test]
    fn fenced_block_is_verbatim_blue_no_inline() {
        let lines = render("```rust\n**x** and `y`\n```", 80, Style::new());
        let s = spans(&lines);
        assert_eq!(
            s,
            vec![("**x** and `y`".into(), Some(CODE), Modifier::empty())]
        );
    }

    #[test]
    fn wrap_preserves_bold_across_break() {
        let lines = render("**alpha beta gamma**", 8, Style::new());
        assert!(lines.len() > 1, "expected a wrap");
        for line in &lines {
            for span in &line.spans {
                if !span.content.trim().is_empty() {
                    assert!(
                        span.style.add_modifier.contains(Modifier::BOLD),
                        "lost bold on {:?}",
                        span.content
                    );
                }
            }
        }
    }

    #[test]
    fn stray_single_star_is_literal() {
        let t = text_of(&render("a * b", 80, Style::new()));
        assert_eq!(t, vec!["a * b".to_string()]);
    }

    #[test]
    fn adjacent_bold_runs() {
        let lines = render("**a****b**", 80, Style::new());
        let s = spans(&lines);
        // Both letters are bold; no literal asterisks survive.
        assert!(text_of(&lines).join("").chars().all(|c| c != '*'));
        assert!(
            s.iter()
                .all(|(_, fg, m)| *fg == Some(BOLD) && m.contains(Modifier::BOLD))
        );
        assert_eq!(text_of(&lines).join(""), "ab");
    }

    #[test]
    fn empty_input_is_empty_vec() {
        assert!(render("", 80, Style::new()).is_empty());
    }

    #[test]
    fn list_item_gets_bullet_and_inline() {
        let t = text_of(&render("- do **it**", 80, Style::new()));
        assert_eq!(t, vec!["• do it".to_string()]);
    }

    #[test]
    fn escaped_marker_is_literal() {
        let t = text_of(&render("a \\*b\\* c", 80, Style::new()));
        assert_eq!(t, vec!["a *b* c".to_string()]);
    }
}
