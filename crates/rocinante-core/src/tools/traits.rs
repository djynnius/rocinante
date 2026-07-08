use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::agent::events::{EventSender, ReplyRouter};

/// Permission class of a tool. The permission engine keys off this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Reads files or metadata; never changes state.
    ReadOnly,
    /// Creates or modifies files inside the workspace.
    Edit,
    /// Runs arbitrary commands.
    Execute,
    /// Spawns subagents.
    Spawn,
}

/// Everything a tool may touch besides its own arguments.
pub struct ToolCtx {
    pub cwd: PathBuf,
    pub events: EventSender,
    pub cancel: CancellationToken,
    /// Subagent nesting depth (0 = main agent).
    pub depth: u8,
    /// Permission-reply routing shared across the agent tree, so subagents
    /// spawned by tools can bubble asks to the same frontend.
    pub router: Arc<ReplyRouter>,
    /// Language-server manager for post-edit diagnostics and the `lsp`
    /// tool; None in tests and subagents.
    pub lsp: Option<Arc<crate::lsp::LspManager>>,
}

impl ToolCtx {
    /// Resolve a possibly-relative path against the workspace cwd.
    pub fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    /// Goes into the tool schema the model sees. Keep it short — every byte
    /// costs context on a local model.
    fn description(&self) -> &'static str;
    /// JSON Schema for the arguments object.
    fn schema(&self) -> serde_json::Value;
    fn kind(&self) -> ToolKind;
    /// Human-readable one-line summary of a call, for permission prompts and
    /// transcripts, e.g. `bash: cargo test`.
    fn describe_call(&self, args: &serde_json::Value) -> String;
    /// Optional rich preview shown with a permission prompt (e.g. a unified
    /// diff for file edits). Must be side-effect free.
    async fn preview(&self, _args: &serde_json::Value, _ctx: &ToolCtx) -> Option<String> {
        None
    }
    async fn run(&self, args: serde_json::Value, ctx: &ToolCtx) -> ToolOutput;
}

/// Unified diff for previews, capped for terminal display.
pub fn render_diff(old: &str, new: &str, path: &str) -> String {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(old, new);
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string();
    const MAX_LINES: usize = 120;
    let lines: Vec<&str> = unified.lines().collect();
    if lines.len() > MAX_LINES {
        format!(
            "{}\n… (diff truncated, {} more lines)",
            lines[..MAX_LINES].join("\n"),
            lines.len() - MAX_LINES
        )
    } else {
        unified
    }
}

/// Cap tool output at write time so a single huge result can't blow the
/// context budget. Keeps head and tail; states what was dropped.
pub fn truncate_output(text: &str, max_lines: usize, max_bytes: usize) -> String {
    let over_lines = text.lines().count() > max_lines;
    if !over_lines && text.len() <= max_bytes {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    let keep = max_lines.saturating_sub(max_lines / 5).max(1); // 80% head
    let tail = max_lines - keep;
    let head: Vec<&str> = lines.iter().take(keep).copied().collect();
    let tail_lines: Vec<&str> = if tail > 0 && lines.len() > keep {
        lines[lines.len().saturating_sub(tail)..].to_vec()
    } else {
        vec![]
    };
    let omitted = lines.len().saturating_sub(keep + tail_lines.len());
    let mut out = head.join("\n");
    if omitted > 0 {
        out.push_str(&format!("\n... [{omitted} lines omitted] ...\n"));
        out.push_str(&tail_lines.join("\n"));
    }
    if out.len() > max_bytes {
        let mut cut = max_bytes;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n... [output truncated]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_untouched() {
        assert_eq!(truncate_output("a\nb", 100, 10_000), "a\nb");
    }

    #[test]
    fn long_output_keeps_head_and_tail() {
        let text: String = (0..1000).map(|i| format!("line{i}\n")).collect();
        let out = truncate_output(&text, 100, 100_000);
        assert!(out.contains("line0"));
        assert!(out.contains("line999"));
        assert!(out.contains("lines omitted"));
        assert!(out.lines().count() < 110);
    }

    #[test]
    fn byte_cap_respects_char_boundaries() {
        let text = "é".repeat(10_000);
        let out = truncate_output(&text, 10, 1000);
        assert!(out.len() <= 1030);
    }
}
