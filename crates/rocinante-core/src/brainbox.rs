//! BRAINBOX.md: a bounded, structured memory file at `.rocinante/BRAINBOX.md`
//! that carries session continuity — goals, state, decisions, gotchas, next
//! steps. Refreshed in the background every N turns (never blocking a turn,
//! never stacking updates) and once more at session end.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Compact head for the system prompt: just the `## Goals` and `## Next
/// steps` sections (the "where we were / what's next" the model most needs at
/// startup). The rest of BRAINBOX.md stays on disk for the model to `read`
/// on demand — keeping standing context small. Falls back to the first ~800
/// bytes when those headings are absent (older files).
pub fn load_head(cwd: &Path) -> Option<String> {
    const HEAD_CAP: usize = 1200;
    let content = std::fs::read_to_string(path_for(cwd)).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    let mut head = String::new();
    for heading in ["## Goals", "## Next steps"] {
        if let Some(section) = extract_section(&content, heading) {
            if !head.is_empty() {
                head.push('\n');
            }
            head.push_str(section.trim_end());
            head.push('\n');
        }
    }
    if head.trim().is_empty() {
        // Unstructured/older file: take a small prefix.
        let mut cut = HEAD_CAP.min(content.len());
        while !content.is_char_boundary(cut) {
            cut -= 1;
        }
        return Some(content[..cut].trim_end().to_string());
    }
    if head.len() > HEAD_CAP {
        let mut cut = HEAD_CAP;
        while !head.is_char_boundary(cut) {
            cut -= 1;
        }
        head.truncate(cut);
    }
    Some(head.trim_end().to_string())
}

/// The body of a `## Heading` section (heading line through the next `## `
/// or EOF), heading line included.
fn extract_section<'a>(content: &'a str, heading: &str) -> Option<&'a str> {
    let start = content.find(heading)?;
    let after = &content[start + heading.len()..];
    let end = after.find("\n## ").map(|i| start + heading.len() + i);
    Some(match end {
        Some(e) => &content[start..e],
        None => &content[start..],
    })
}

/// Read the brainbox for display/sizing (`/context`), capped.
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
    /// Total turns seen; compared against `completed_turn` so finalize can
    /// skip when the file already reflects the whole session.
    turn_count: u64,
    /// Highest turn count covered by a successfully written update.
    completed_turn: Arc<AtomicU64>,
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
            turn_count: 0,
            completed_turn: Arc::new(AtomicU64::new(0)),
        }
    }

    /// `/clear --all`: delete BRAINBOX.md and mark it current so the
    /// session-end finalize won't recreate it from the (now cleared)
    /// conversation. Fresh memory still accrues from turns after this.
    pub fn clear(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.completed_turn
            .store(self.turn_count, Ordering::Release);
        self.turns_since_update = 0;
    }

    /// Called after each completed turn. Every N turns, kicks off a
    /// background refresh with a snapshot of the transcript. Skips silently
    /// when a refresh is already running.
    pub fn note_turn(&mut self, messages: &[Message]) {
        self.turn_count += 1;
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
    /// final update — unless the file already covers every turn (a background
    /// update just landed, or the session had no turns), in which case quit
    /// is instant. Bounded — never hangs a quit.
    pub async fn finalize(&self, messages: &[Message]) {
        let result = tokio::time::timeout(FINALIZE_TIMEOUT, async {
            while self.in_flight.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if self.completed_turn.load(Ordering::Acquire) >= self.turn_count {
                tracing::debug!("brainbox already current; skipping final update");
                return Ok(());
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
            snapshot_turn: self.turn_count,
            completed_turn: Arc::clone(&self.completed_turn),
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
    /// Turn count this job's snapshot covers.
    snapshot_turn: u64,
    completed_turn: Arc<AtomicU64>,
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
        // fetch_max: a slow old job must not regress a newer completion.
        self.completed_turn
            .fetch_max(self.snapshot_turn, Ordering::AcqRel);
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
    use async_trait::async_trait;
    use rocinante_providers::{
        Capabilities, ChatRequest, ChatStream, ProviderError, StopReason, ToolSchema,
    };

    #[test]
    fn load_head_extracts_goals_and_next_steps() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "# BRAINBOX\n\
             ## Goals\n- ship the dashboard\n\
             ## State\n- half done\n\
             ## Decisions\n- use a grid\n\
             ## Gotchas\n- watch the umask\n\
             ## Next steps\n- write tests\n",
        )
        .unwrap();
        let head = load_head(dir.path()).unwrap();
        assert!(head.contains("ship the dashboard"), "{head}");
        assert!(head.contains("write tests"), "{head}");
        // The bulky middle sections are left on disk.
        assert!(!head.contains("use a grid"), "{head}");
        assert!(!head.contains("watch the umask"), "{head}");
    }

    #[tokio::test]
    async fn clear_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
        });
        let mut bb = brainbox_with(dir.path(), Arc::clone(&provider), 5);
        write_atomic(&path_for(dir.path()), "# BRAINBOX\n## Goals\n- x\n").unwrap();
        assert!(path_for(dir.path()).exists());
        bb.clear();
        assert!(!path_for(dir.path()).exists(), "clear removes BRAINBOX.md");
    }

    #[test]
    fn load_head_falls_back_without_headings() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "just some freeform memory notes").unwrap();
        assert_eq!(
            load_head(dir.path()).unwrap(),
            "just some freeform memory notes"
        );
    }
    use std::sync::atomic::AtomicUsize;

    /// Counts chat calls; always returns a valid brainbox body.
    struct CountingProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Provider for CountingProvider {
        fn id(&self) -> &str {
            "mock"
        }
        fn caps(&self) -> Capabilities {
            Capabilities {
                native_tools: false,
                structured_output: false,
                is_local: false,
            }
        }
        async fn chat(&self, _req: ChatRequest) -> Result<ChatStream, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let deltas: Vec<Result<ChatDelta, ProviderError>> = vec![
                Ok(ChatDelta::Text("# BRAINBOX\n## Goals\n- x\n".into())),
                Ok(ChatDelta::Done(StopReason::EndTurn)),
            ];
            Ok(Box::pin(futures::stream::iter(deltas)))
        }
        fn count_tokens(&self, _m: &[Message], _t: &[ToolSchema]) -> usize {
            10
        }
    }

    fn brainbox_with(
        dir: &Path,
        provider: Arc<CountingProvider>,
        update_every_turns: u32,
    ) -> Brainbox {
        Brainbox::new(
            dir,
            provider,
            "mock".into(),
            GenParams::default(),
            update_every_turns,
        )
    }

    #[tokio::test]
    async fn finalize_skips_when_clean() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
        });
        let mut bb = brainbox_with(dir.path(), Arc::clone(&provider), 1);
        bb.note_turn(&[Message::user("hi")]);
        // Wait for the background update to complete and cover turn 1.
        while bb.completed_turn.load(Ordering::Acquire) < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        bb.finalize(&[Message::user("hi")]).await;
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "clean brainbox must skip the final update"
        );
    }

    #[tokio::test]
    async fn finalize_runs_when_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
        });
        // Threshold 5: one turn never triggers a background update.
        let mut bb = brainbox_with(dir.path(), Arc::clone(&provider), 5);
        bb.note_turn(&[Message::user("hi")]);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        bb.finalize(&[Message::user("hi")]).await;
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "dirty brainbox must run the final update"
        );
    }

    #[tokio::test]
    async fn finalize_skips_on_empty_session() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
        });
        let bb = brainbox_with(dir.path(), Arc::clone(&provider), 5);
        bb.finalize(&[]).await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }

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
