//! LESSONS.md: the user's *global* preferences and do/don't rules at
//! `~/.rocinante/LESSONS.md`, injected into every session's system prompt so
//! the agent follows them across all projects. Populated explicitly by
//! `/remember` and by a conservative session-end signal-capture pass that
//! records a rule ONLY when the user stated a preference, corrected the agent,
//! or a mistake recurred — never from the model's own guesses.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use rocinante_providers::{ChatDelta, ChatRequest, GenParams, Message, Provider};

use crate::brainbox::{render_transcript, write_atomic};

pub const FILE_NAME: &str = "LESSONS.md";
/// Standing-context cap; a runaway lessons file must not eat the prompt.
const LOAD_CAP_BYTES: usize = 2048;
/// Session-end capture bound; quitting must never hang.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(30);

/// `~/.rocinante/LESSONS.md` — global, not project-scoped.
pub fn path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".rocinante").join(FILE_NAME))
}

/// The full file (capped) for system-prompt injection and the `/context`
/// panel — it's small curated rules, so no lazy head like BRAINBOX.
pub fn load() -> Option<String> {
    let content = std::fs::read_to_string(path()?).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    if content.len() > LOAD_CAP_BYTES {
        let mut cut = LOAD_CAP_BYTES;
        while !content.is_char_boundary(cut) {
            cut -= 1;
        }
        tracing::warn!("LESSONS.md exceeds {LOAD_CAP_BYTES} bytes; truncated for context");
        Some(format!("{}\n[LESSONS truncated]", &content[..cut]))
    } else {
        Some(content)
    }
}

pub struct Lessons {
    path: PathBuf,
    provider: Arc<dyn Provider>,
    model: String,
    params: GenParams,
    /// Periodic capture cadence; 0 = session-end only.
    update_every_turns: u32,
    turns_since_update: u32,
    in_flight: Arc<AtomicBool>,
    turn_count: u64,
    completed_turn: Arc<AtomicU64>,
}

impl Lessons {
    pub fn new(
        provider: Arc<dyn Provider>,
        model: String,
        params: GenParams,
        update_every_turns: u32,
    ) -> Self {
        Self {
            path: path().unwrap_or_else(|| PathBuf::from(FILE_NAME)),
            provider,
            model,
            params,
            update_every_turns,
            turns_since_update: 0,
            in_flight: Arc::new(AtomicBool::new(false)),
            turn_count: 0,
            completed_turn: Arc::new(AtomicU64::new(0)),
        }
    }

    /// After each turn. Runs a periodic capture only when
    /// `update_every_turns > 0`; otherwise capture happens at session end.
    pub fn note_turn(&mut self, messages: &[Message]) {
        self.turn_count += 1;
        if self.update_every_turns == 0 {
            return;
        }
        self.turns_since_update += 1;
        if self.turns_since_update < self.update_every_turns {
            return;
        }
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.turns_since_update = 0;
        let job = self.job(messages, "periodic");
        tokio::spawn(async move {
            if let Err(e) = job.run().await {
                tracing::warn!(error = %e, "background lessons capture failed");
            }
        });
    }

    /// Session-end capture: waits out any in-flight run, then one final pass —
    /// unless the file already covers every turn. Bounded; never hangs a quit.
    pub async fn finalize(&self, messages: &[Message]) {
        let result = tokio::time::timeout(FINALIZE_TIMEOUT, async {
            while self.in_flight.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if self.completed_turn.load(Ordering::Acquire) >= self.turn_count {
                return Ok(());
            }
            self.in_flight.store(true, Ordering::Release);
            self.job(messages, "session end").run().await
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "final lessons capture failed"),
            Err(_) => tracing::warn!("final lessons capture timed out"),
        }
    }

    fn job(&self, messages: &[Message], reason: &str) -> CaptureJob {
        CaptureJob {
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

struct CaptureJob {
    path: PathBuf,
    provider: Arc<dyn Provider>,
    model: String,
    params: GenParams,
    transcript: String,
    reason: String,
    in_flight: Arc<AtomicBool>,
    snapshot_turn: u64,
    completed_turn: Arc<AtomicU64>,
}

impl CaptureJob {
    async fn run(self) -> anyhow::Result<()> {
        let _release = ReleaseOnDrop(Arc::clone(&self.in_flight));

        let old = std::fs::read_to_string(&self.path).unwrap_or_default();
        let req = ChatRequest {
            model: self.model.clone(),
            messages: vec![
                Message::system(
                    "You maintain a user's global preferences file. You are extremely conservative. Respond with ONLY the file content, no preamble or code fences.",
                ),
                Message::user(capture_prompt(&old, &self.transcript)),
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
            anyhow::bail!("model produced unusable lessons content; keeping previous file");
        };
        // Don't rewrite an unchanged file (the conservative common case).
        if content.trim() != old.trim() {
            write_atomic(&self.path, &content)?;
            tracing::info!(reason = %self.reason, bytes = content.len(), "lessons updated");
        }
        self.completed_turn
            .fetch_max(self.snapshot_turn, Ordering::AcqRel);
        Ok(())
    }
}

struct ReleaseOnDrop(Arc<AtomicBool>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn capture_prompt(old: &str, transcript: &str) -> String {
    let old_section = if old.trim().is_empty() {
        "(no existing file — create it only if a rule qualifies below)".to_string()
    } else {
        old.to_string()
    };
    format!(
        r#"Review the recent session transcript for signals about how THIS USER wants an AI coding agent to behave — across all their projects, not just this one.

Update the file ONLY when the transcript clearly shows one of these:
1. The user EXPLICITLY stated a preference ("always use X", "I prefer Y", "don't do Z").
2. The user CORRECTED the agent's behavior or output in a way that generalizes beyond this one task.
3. The SAME mistake recurred and the user pushed back on it more than once.

Do NOT infer preferences from ordinary task instructions, one-off requests, or the agent's own guesses. If nothing in the transcript qualifies, output the CURRENT FILE UNCHANGED, byte for byte. Never invent preferences.

Keep exactly these headings, each a short bulleted list. Keep the whole file under 40 lines. Merge duplicates; never repeat a rule.

# LESSONS
## Preferences
## Rules

CURRENT FILE:
{old_section}

RECENT TRANSCRIPT:
{transcript}

Respond with only the new file content."#
    )
}

/// Accept the model's output only if it looks like a real lessons file (has a
/// Preferences or Rules heading); otherwise a refusal must not wipe the file.
fn sanitize(text: &str) -> Option<String> {
    let mut cleaned = text.trim();
    if cleaned.starts_with("```") {
        cleaned = cleaned
            .trim_start_matches("```markdown")
            .trim_start_matches("```md")
            .trim_start_matches("```");
        if let Some(end) = cleaned.rfind("```") {
            cleaned = &cleaned[..end];
        }
        cleaned = cleaned.trim();
    }
    if cleaned.is_empty() || !(cleaned.contains("## Preferences") || cleaned.contains("## Rules")) {
        return None;
    }
    Some(format!("{cleaned}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rocinante_providers::{Capabilities, ChatStream, ProviderError, StopReason, ToolSchema};
    use std::sync::atomic::AtomicUsize;

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
                Ok(ChatDelta::Text("# LESSONS\n## Rules\n- x\n".into())),
                Ok(ChatDelta::Done(StopReason::EndTurn)),
            ];
            Ok(Box::pin(futures::stream::iter(deltas)))
        }
        fn count_tokens(&self, _m: &[Message], _t: &[ToolSchema]) -> usize {
            10
        }
    }

    fn lessons_in(dir: &std::path::Path, provider: Arc<CountingProvider>, every: u32) -> Lessons {
        let mut l = Lessons::new(provider, "mock".into(), GenParams::default(), every);
        l.path = dir.join(FILE_NAME); // redirect off the real home dir
        l
    }

    #[tokio::test]
    async fn finalize_runs_once_then_skips_when_clean() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
        });
        let mut l = lessons_in(dir.path(), Arc::clone(&provider), 5);
        l.note_turn(&[Message::user("always use tabs")]); // turn_count=1, no periodic (every=5)
        l.finalize(&[Message::user("always use tabs")]).await;
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "one capture at session end"
        );
        // Nothing new since → skip.
        l.finalize(&[Message::user("always use tabs")]).await;
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn note_turn_is_noop_when_periodic_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
        });
        let mut l = lessons_in(dir.path(), Arc::clone(&provider), 0);
        for _ in 0..20 {
            l.note_turn(&[Message::user("hi")]);
        }
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            0,
            "0 = session-end only"
        );
    }

    #[test]
    fn sanitize_accepts_lessons_rejects_refusal() {
        assert!(sanitize("# LESSONS\n## Rules\n- always run fmt\n").is_some());
        assert!(sanitize("# LESSONS\n## Preferences\n- concise replies\n").is_some());
        assert!(sanitize("Sorry, I can't help with that.").is_none());
        assert!(sanitize("").is_none());
    }

    #[test]
    fn sanitize_strips_code_fence() {
        let out = sanitize("```markdown\n# LESSONS\n## Rules\n- x\n```").unwrap();
        assert!(out.starts_with("# LESSONS"));
        assert!(!out.contains("```"));
    }

    #[test]
    fn capture_prompt_is_conservative() {
        let p = capture_prompt("", "USER: hi");
        assert!(p.contains("output the CURRENT FILE UNCHANGED"));
        assert!(p.contains("Never invent preferences"));
        assert!(p.contains("## Preferences") && p.contains("## Rules"));
    }
}
