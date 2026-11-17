//! Turn a reply streaming in as markdown into sentences worth saying aloud.
//!
//! Sentences are released as soon as they are complete, so speech starts
//! while the model is still writing. Code blocks and tables are not read
//! out (a line at the end says they are on screen); markdown markers,
//! link targets and bare URLs are dropped.

/// Abbreviations whose period does not end a sentence.
const ABBREVIATIONS: &[&str] = &[
    "e.g.", "i.e.", "etc.", "vs.", "mr.", "mrs.", "ms.", "dr.", "st.", "no.", "approx.", "cf.",
];

#[derive(Debug, Default)]
pub struct Splitter {
    /// The current, unfinished line.
    line: String,
    /// Bytes of `line` already released as sentences.
    spoken: usize,
    in_code: bool,
    skipped_code: bool,
    skipped_table: bool,
}

impl Splitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add streamed text; returns the sentences it completed.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        let mut out = Vec::new();
        for c in delta.chars() {
            if c == '\n' {
                self.end_line(&mut out);
            } else {
                self.line.push(c);
            }
        }
        // Mid-line: release finished sentences of plain prose now.
        if self.line_is_prose() {
            self.release_sentences(&mut out, false);
        }
        out
    }

    /// The reply ended: everything left, plus what was not read out.
    pub fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        self.end_line(&mut out);
        match (self.skipped_code, self.skipped_table) {
            (true, true) => out.push("The code and the table are on screen.".into()),
            (true, false) => out.push("The code is on screen.".into()),
            (false, true) => out.push("The table is on screen.".into()),
            (false, false) => {}
        }
        *self = Self::default();
        out
    }

    fn end_line(&mut self, out: &mut Vec<String>) {
        let trimmed = self.line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            self.in_code = !self.in_code;
            self.skipped_code = true;
        } else if self.in_code {
        } else if trimmed.starts_with('|') {
            self.skipped_table = true;
        } else {
            self.release_sentences(out, true);
        }
        self.line.clear();
        self.spoken = 0;
    }

    /// Is the line so far ordinary prose (not a fence, table, or code)?
    fn line_is_prose(&self) -> bool {
        let t = self.line.trim_start();
        !self.in_code
            && !t.is_empty()
            && !t.starts_with('`')
            && !t.starts_with('~')
            && !t.starts_with('|')
            && !self.line.starts_with("    ")
            && !self.line.starts_with('\t')
    }

    /// Release complete sentences from the unspoken part of the line; at the
    /// end of a line, the remainder too.
    fn release_sentences(&mut self, out: &mut Vec<String>, line_done: bool) {
        let rest = &self.line[self.spoken..];
        let mut start = 0;
        let bytes: Vec<(usize, char)> = rest.char_indices().collect();
        for (i, &(_, c)) in bytes.iter().enumerate() {
            if !matches!(c, '.' | '!' | '?') {
                continue;
            }
            // A sentence ends at punctuation (plus closing quotes/brackets)
            // followed by whitespace; at the line end only when it's done.
            let mut j = i + 1;
            while j < bytes.len() && matches!(bytes[j].1, '"' | '\'' | ')' | ']' | '”' | '’') {
                j += 1;
            }
            if j >= bytes.len() || !bytes[j].1.is_whitespace() {
                continue;
            }
            let end = bytes[j].0;
            let candidate = &rest[start..end];
            if c == '.' && ends_with_abbreviation(candidate) {
                continue;
            }
            push_spoken(out, candidate);
            start = end;
        }
        if line_done {
            push_spoken(out, &rest[start..]);
            self.spoken = self.line.len();
        } else {
            self.spoken += start;
        }
    }
}

fn ends_with_abbreviation(text: &str) -> bool {
    let last = text
        .split_whitespace()
        .last()
        .unwrap_or("")
        .to_ascii_lowercase();
    ABBREVIATIONS.contains(&last.as_str())
}

fn push_spoken(out: &mut Vec<String>, raw: &str) {
    let text = speakable(raw);
    if text.chars().any(char::is_alphanumeric) {
        out.push(text);
    }
}

/// Strip markdown from one line of prose.
pub fn speakable(raw: &str) -> String {
    let mut line = raw.trim();
    // Block markers: headings, quotes, bullets, numbered items.
    line = line.trim_start_matches('#').trim_start();
    line = line.trim_start_matches('>').trim_start();
    for bullet in ["- [ ] ", "- [x] ", "- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            line = rest;
            break;
        }
    }
    if let Some((num, rest)) = line.split_once(". ")
        && !num.is_empty()
        && num.len() <= 3
        && num.chars().all(|c| c.is_ascii_digit())
    {
        line = rest;
    }

    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // [text](target) → text
        if c == '['
            && let Some(close) = chars[i..].iter().position(|&x| x == ']')
            && chars.get(i + close + 1) == Some(&'(')
            && let Some(paren) = chars[i + close + 1..].iter().position(|&x| x == ')')
        {
            out.extend(&chars[i + 1..i + close]);
            i += close + 1 + paren + 1;
            continue;
        }
        if matches!(c, '`' | '*') || (c == '_' && chars.get(i + 1) == Some(&'_')) {
            i += if c == '_' { 2 } else { 1 };
            continue;
        }
        out.push(c);
        i += 1;
    }
    out.split_whitespace()
        .map(|w| {
            if w.starts_with("http://") || w.starts_with("https://") {
                "a link"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(deltas: &[&str]) -> Vec<String> {
        let mut s = Splitter::new();
        let mut out: Vec<String> = deltas.iter().flat_map(|d| s.push(d)).collect();
        out.extend(s.finish());
        out
    }

    #[test]
    fn releases_sentences_as_they_complete() {
        let mut s = Splitter::new();
        assert!(s.push("Hi Donald. I've put the fi").len() == 1);
        assert_eq!(
            s.push("x in main.rs, and the tests pass. "),
            vec!["I've put the fix in main.rs, and the tests pass."]
        );
        assert_eq!(s.push("Want me to commit"), Vec::<String>::new());
        assert_eq!(s.finish(), vec!["Want me to commit"]);
    }

    #[test]
    fn first_sentence_is_out_before_the_line_ends() {
        let mut s = Splitter::new();
        assert_eq!(s.push("Done! Next"), vec!["Done!"]);
    }

    #[test]
    fn file_names_and_versions_do_not_split() {
        assert_eq!(
            all(&["Edited main.rs and v1.2.3 is out. Ok."]),
            vec!["Edited main.rs and v1.2.3 is out.", "Ok."]
        );
        assert_eq!(
            all(&["Use a tool, e.g. ripgrep, for this. Then stop."]),
            vec!["Use a tool, e.g. ripgrep, for this.", "Then stop."]
        );
    }

    #[test]
    fn code_blocks_are_skipped_and_mentioned() {
        let out = all(&[
            "Here's the fix:\n```rust\nfn main() {\n    println!(\"hi. there\");\n}\n```\nRun it.",
        ]);
        assert_eq!(
            out,
            vec!["Here's the fix:", "Run it.", "The code is on screen."]
        );
    }

    #[test]
    fn tables_are_skipped_and_mentioned() {
        let out = all(&["Results:\n| a | b |\n|---|---|\n| 1 | 2 |\nAll good.\n"]);
        assert_eq!(
            out,
            vec!["Results:", "All good.", "The table is on screen."]
        );
    }

    #[test]
    fn markdown_is_stripped() {
        assert_eq!(speakable("## **Summary**"), "Summary");
        assert_eq!(
            speakable("- Run `cargo test` first"),
            "Run cargo test first"
        );
        assert_eq!(
            speakable("2. See [the guide](https://x.y/z)."),
            "See the guide."
        );
        assert_eq!(
            speakable("Docs at https://example.com today"),
            "Docs at a link today"
        );
        assert_eq!(speakable("> quoted *text*"), "quoted text");
        assert_eq!(
            speakable("keep snake_case and __init__ names"),
            "keep snake_case and init names"
        );
    }

    #[test]
    fn list_items_are_separate_sentences() {
        assert_eq!(
            all(&["Two things:\n- add tests\n- update the docs\n"]),
            vec!["Two things:", "add tests", "update the docs"]
        );
    }

    #[test]
    fn nothing_speakable_says_nothing() {
        assert!(all(&["---\n\n"]).is_empty());
        assert!(all(&[]).is_empty());
    }

    #[test]
    fn deltas_split_anywhere() {
        let text = "First one. Second one? Third!";
        let by_char: Vec<String> = text.chars().map(String::from).collect();
        let refs: Vec<&str> = by_char.iter().map(String::as_str).collect();
        assert_eq!(all(&refs), vec!["First one.", "Second one?", "Third!"]);
    }
}
