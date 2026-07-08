//! BRAINBOX.md: a bounded, structured memory file at `.rocinante/BRAINBOX.md`
//! that carries session continuity — goals, state, decisions, gotchas, next
//! steps. Refreshed in the background every N turns (never blocking a turn,
//! never stacking updates) and once more at session end.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use rocinante_providers::{ChatDelta, ChatRequest, GenParams, Message, Provider, Role};

pub const FILE_NAME: &str = "BRAINBOX.md";
/// Startup-injection cap; an oversized brainbox must not eat the context.
const LOAD_CAP_BYTES: usize = 4096;
/// How much recent transcript the updater sees.
const SNAPSHOT_MESSAGES: usize = 30;
const SNAPSHOT_CHARS_PER_MESSAGE: usize = 600;
/// Session-end update bound; quitting must never hang.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(30);

pub fn path_for(cwd: &Path) -> PathBuf {
    cwd.join(".rocinante").join(FILE_NAME)
}

/// Read the brainbox for startup injection, capped.
pub fn load(cwd: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path_for(cwd)).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    if content.len() > LOAD_CAP_BYTES {
        let mut cut = LOAD_CAP_BYTES;
        while !content.is_char_boundary(cut) {
            cut -= 1;
        }
        Some(format!(
            "{}\n[BRAINBOX truncated for context]",
            &content[..cut]
        ))
    } else {
        Some(content)
    }
}

pub struct Brainbox {
    path: PathBuf,
    provider: Arc<dyn Provider>,
    model: String,
    params: GenParams,
    update_every_turns: u32,
    turns_since_update: u32,
    in_flight: Arc<AtomicBool>,
}

impl Brainbox {
    pub fn new(
        cwd: &Path,
        provider: Arc<dyn Provider>,
        model: String,
        params: GenParams,
        update_every_turns: u32,
    ) -> Self {
        Self {
            path: path_for(cwd),
            provider,
            model,
            params,
            update_every_turns: update_every_turns.max(1),
            turns_since_update: 0,
            in_flight: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Called after each completed turn. Every N turns, kicks off a
    /// background refresh with a snapshot of the transcript. Skips silently
    /// when a refresh is already running.
    pub fn note_turn(&mut self, messages: &[Message]) {
        self.turns_since_update += 1;
        if self.turns_since_update < self.update_every_turns {
            return;
        }
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tracing::debug!("brainbox update already in flight; skipping this tick");
            return;
        }
        self.turns_since_update = 0;
        let job = self.job(messages, "periodic");
        tokio::spawn(async move {
            if let Err(e) = job.run().await {
                tracing::warn!(error = %e, "background brainbox update failed");
            }
        });
    }

    /// Session-end update: waits out any in-flight refresh, then runs one
    /// final update. Bounded — never hangs a quit.
    pub async fn finalize(&self, messages: &[Message]) {
        let result = tokio::time::timeout(FINALIZE_TIMEOUT, async {
            while self.in_flight.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            self.in_flight.store(true, Ordering::Release);
            self.job(messages, "session end").run().await
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "final brainbox update failed"),
            Err(_) => tracing::warn!("final brainbox update timed out"),
        }
    }

    fn job(&self, messages: &[Message], reason: &str) -> UpdateJob {
        UpdateJob {
            path: self.path.clone(),
            provider: Arc::clone(&self.provider),
            model: self.model.clone(),
            params: self.params.clone(),
            transcript: render_transcript(messages),
            reason: reason.to_string(),
            in_flight: Arc::clone(&self.in_flight),
        }
    }
}

struct UpdateJob {
    path: PathBuf,
    provider: Arc<dyn Provider>,
    model: String,
    params: GenParams,
    transcript: String,
    reason: String,
    in_flight: Arc<AtomicBool>,
}

impl UpdateJob {
    async fn run(self) -> anyhow::Result<()> {
        // Whatever happens, release the guard.
        let _release = ReleaseOnDrop(Arc::clone(&self.in_flight));

        let old = std::fs::read_to_string(&self.path).unwrap_or_default();
        let prompt = update_prompt(&old, &self.transcript);
        let req = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                Message::system(
                    "You maintain a project memory file. Respond with ONLY the new file content, no preamble or code fences.",
                ),
                Message::user(prompt),
            ],
            tools: vec![],
            params: self.params.clone(),
            format: None,
        };
        let mut stream = self.provider.chat(req).await?;
        let mut text = String::new();
        while let Some(delta) = stream.next().await {
            match delta? {
                ChatDelta::Text(t) => text.push_str(&t),
                ChatDelta::Done(_) => break,
                _ => {}
            }
        }

        let Some(content) = sanitize(&text) else {
            anyhow::bail!("model produced unusable brainbox content; keeping previous file");
        };
        write_atomic(&self.path, &content)?;
        tracing::info!(reason = %self.reason, bytes = content.len(), "brainbox updated");
        Ok(())
    }
}

struct ReleaseOnDrop(Arc<AtomicBool>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Recent-transcript rendering for the updater: last N non-system messages,
/// each truncated — tool results are the bulkiest and least memorable.
fn render_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    let recent: Vec<&Message> = messages
        .iter()
        .filter(|m| m.role != Role::System)
        .rev()
        .take(SNAPSHOT_MESSAGES)
        .collect();
    for msg in recent.into_iter().rev() {
        let label = match msg.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
            Role::Tool => "TOOL RESULT",
            Role::System => continue,
        };
        let mut content = msg.content.replace('\n', " ");
        if content.len() > SNAPSHOT_CHARS_PER_MESSAGE {
            let mut cut = SNAPSHOT_CHARS_PER_MESSAGE;
            while !content.is_char_boundary(cut) {
                cut -= 1;
            }
            content.truncate(cut);
            content.push_str(" […]");
        }
        for call in &msg.tool_calls {
            out.push_str(&format!("[{label} calls {}]\n", call.name));
        }
        if !content.trim().is_empty() {
            out.push_str(&format!("[{label}] {content}\n"));
        }
    }
    out
}

fn update_prompt(old: &str, transcript: &str) -> String {
    let old_section = if old.trim().is_empty() {
        "(no existing file — create it)".to_string()
    } else {
        old.to_string()
    };
    format!(
        r#"Update this project memory file using the recent session transcript. Carry forward everything from the current file that is still true and relevant; fold in what changed. Be specific: exact file paths, commands, error messages. Stay under 80 lines total.

Required structure (all five sections, exactly these headings):
# BRAINBOX
## Goals
## State
## Decisions
## Gotchas
## Next steps

CURRENT FILE:
{old_section}

RECENT TRANSCRIPT:
{transcript}

Respond with only the new file content."#
    )
}

/// Accept the model's output only if it looks like a real brainbox.
fn sanitize(text: &str) -> Option<String> {
    let mut cleaned = text.trim();
    // Strip a wrapping code fence if the model added one despite instructions.
    if cleaned.starts_with("```") {
        cleaned = cleaned
            .trim_start_matches("```markdown")
            .trim_start_matches("```md");
        cleaned = cleaned.trim_start_matches("```");
        if let Some(end) = cleaned.rfind("```") {
            cleaned = &cleaned[..end];
        }
        cleaned = cleaned.trim();
    }
    if cleaned.is_empty() || !cleaned.contains("## ") {
        return None;
    }
    Some(format!("{cleaned}\n"))
}

fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_accepts_structured_output() {
        let out = sanitize("# BRAINBOX\n## Goals\n- ship v1\n").unwrap();
        assert!(out.contains("## Goals"));
    }

    #[test]
    fn sanitize_rejects_garbage() {
        assert!(sanitize("").is_none());
        assert!(sanitize("Sorry, I cannot help with that.").is_none());
    }

    #[test]
    fn sanitize_strips_code_fence() {
        let out = sanitize("```markdown\n# BRAINBOX\n## Goals\n- x\n```").unwrap();
        assert!(out.starts_with("# BRAINBOX"));
        assert!(!out.contains("```"));
    }

    #[test]
    fn atomic_write_creates_parents_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".rocinante").join(FILE_NAME);
        write_atomic(&path, "one").unwrap();
        write_atomic(&path, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_extension("md.tmp").exists());
    }

    #[test]
    fn load_caps_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x".repeat(10_000)).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert!(loaded.len() < 5000);
        assert!(loaded.contains("truncated"));
    }

    #[test]
    fn transcript_rendering_truncates_and_labels() {
        let messages = vec![
            Message::system("sys is skipped"),
            Message::user("fix the login bug"),
            Message::tool_result("1", "y".repeat(2000)),
            Message::assistant("done"),
        ];
        let out = render_transcript(&messages);
        assert!(out.contains("[USER] fix the login bug"));
        assert!(out.contains("[…]"));
        assert!(!out.contains("sys is skipped"));
        assert!(out.len() < 1200);
    }
}
