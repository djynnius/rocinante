//! Context budgeting and compaction. The operative ceiling is the configured
//! `num_ctx`, not the model's advertised maximum — VRAM is the real limit.

use rocinante_providers::{Message, Role, ToolSchema, tokens};

/// Reserved for the model's own output within num_ctx.
const OUTPUT_RESERVE: usize = 4096;
/// Compact when estimated usage crosses this fraction of the usable budget.
const COMPACT_THRESHOLD: f64 = 0.80;
/// Start summarizing in the background at this fraction, so the blocking
/// compaction at COMPACT_THRESHOLD becomes a rare fallback.
const PROACTIVE_THRESHOLD: f64 = 0.60;
/// Stub old tool results only above this fraction. Below it, history is left
/// byte-identical between turns so Ollama's KV prefix cache keeps its hits;
/// each mid-history rewrite forces re-evaluation from the edit point on.
const PRUNE_THRESHOLD: f64 = 0.35;
/// Per-message cap for the summarizer's transcript input.
const TRANSCRIPT_MSG_CAP: usize = 1_500;
/// Total cap for the summarizer's transcript input (~13.7k tokens) — has to
/// fit the summarizer's own window, or Ollama silently drops the tail (the
/// recent state, exactly what the summary needs most).
const TRANSCRIPT_MAX_BYTES: usize = 48_000;
/// How many trailing turns survive compaction verbatim.
const KEEP_LAST_TURNS: usize = 2;
/// Tool results at or under this size are never pruned — a stub saves nothing.
pub const PRUNE_MIN_BYTES: usize = 200;
/// Every prune stub starts with this; also the idempotency check.
pub const PRUNE_STUB_PREFIX: &str = "[pruned tool result";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPlan {
    Fits,
    /// 60–80% of budget: summarize old turns in the background.
    NearBudget,
    NeedsCompaction,
}

pub struct ContextManager {
    num_ctx: usize,
    /// Tool results older than this many user turns get stubbed (0 = off).
    keep_tool_turns: usize,
}

impl ContextManager {
    pub fn new(num_ctx: u32, keep_tool_turns: u32) -> Self {
        // Never stub inside compaction's verbatim tail.
        let keep_tool_turns = match keep_tool_turns as usize {
            0 => 0,
            n => n.max(KEEP_LAST_TURNS),
        };
        Self {
            num_ctx: num_ctx as usize,
            keep_tool_turns,
        }
    }

    pub fn usable_budget(&self) -> usize {
        self.num_ctx.saturating_sub(OUTPUT_RESERVE)
    }

    pub fn plan(&self, messages: &[Message], tools: &[ToolSchema]) -> ContextPlan {
        let estimate = tokens::estimate_messages(messages, tools) as f64;
        let budget = self.usable_budget() as f64;
        if estimate >= budget * COMPACT_THRESHOLD {
            ContextPlan::NeedsCompaction
        } else if estimate >= budget * PROACTIVE_THRESHOLD {
            ContextPlan::NearBudget
        } else {
            ContextPlan::Fits
        }
    }

    /// Whether to run a prune pass now. The ladder: 35% batch-stub old tool
    /// results → 60% background summarize → 80% blocking compaction.
    pub fn should_prune(&self, messages: &[Message], tools: &[ToolSchema]) -> bool {
        self.keep_tool_turns != 0
            && tokens::estimate_messages(messages, tools) as f64
                >= self.usable_budget() as f64 * PRUNE_THRESHOLD
    }

    /// Index into `messages` before which Tool results are prunable: the
    /// start of the last `keep_tool_turns` user turns. None when pruning is
    /// disabled or the conversation is too short to have an "old" region.
    pub fn prune_cut(&self, messages: &[Message]) -> Option<usize> {
        if self.keep_tool_turns == 0 {
            return None;
        }
        let user_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == Role::User)
            .map(|(i, _)| i)
            .collect();
        if user_indices.len() <= self.keep_tool_turns {
            return None;
        }
        Some(user_indices[user_indices.len() - self.keep_tool_turns])
    }

    /// One-line replacement for an old tool result. The full output stays in
    /// the session JSONL; the head line keeps enough scent to re-run the tool
    /// if the model needs the detail again.
    pub fn prune_stub(tool_name: Option<&str>, content: &str) -> String {
        let lines = content.lines().count();
        let bytes = content.len();
        let mut head: String = content.lines().next().unwrap_or("").trim().to_string();
        if head.len() > 80 {
            let mut cut = 80;
            while !head.is_char_boundary(cut) {
                cut -= 1;
            }
            head.truncate(cut);
            head.push('…');
        }
        let name = tool_name.unwrap_or("tool");
        format!(
            "{PRUNE_STUB_PREFIX}: {name} — {lines} lines / {bytes} bytes — first line: \"{head}\" — full output in session log; re-run the tool if you need it]"
        )
    }

    /// Split messages into (system, to_summarize, keep_verbatim).
    /// Boundaries land on user messages so kept turns are complete, and the
    /// original goal (first user message) is preserved with the summary.
    pub fn split_for_compaction<'a>(
        &self,
        messages: &'a [Message],
    ) -> Option<(&'a Message, &'a [Message], &'a [Message])> {
        let (system, rest) = match messages.split_first() {
            Some((s, rest)) if s.role == Role::System => (s, rest),
            _ => return None,
        };
        // Find the start of the last KEEP_LAST_TURNS user turns.
        let user_indices: Vec<usize> = rest
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == Role::User)
            .map(|(i, _)| i)
            .collect();
        if user_indices.len() <= KEEP_LAST_TURNS {
            return None; // nothing old enough to fold away
        }
        let cut = user_indices[user_indices.len() - KEEP_LAST_TURNS];
        Some((system, &rest[..cut], &rest[cut..]))
    }

    /// Transcript for the summarizer, bounded so its own prompt fits the
    /// model window: each message capped at TRANSCRIPT_MSG_CAP chars, then
    /// oldest messages dropped whole until the total fits — the original
    /// goal is passed to the summarizer separately, so recent state is what
    /// must survive here.
    pub fn render_summary_transcript(old: &[Message]) -> String {
        let rendered: Vec<String> = old
            .iter()
            .map(|m| {
                let mut content = m.content.clone();
                if content.len() > TRANSCRIPT_MSG_CAP {
                    let mut cut = TRANSCRIPT_MSG_CAP;
                    while !content.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    content.truncate(cut);
                    content.push('…');
                }
                format!("[{:?}] {}\n", m.role, content)
            })
            .collect();
        let mut total: usize = rendered.iter().map(String::len).sum();
        let mut dropped = 0usize;
        while total > TRANSCRIPT_MAX_BYTES && dropped < rendered.len() {
            total -= rendered[dropped].len();
            dropped += 1;
        }
        let mut out = String::with_capacity(total + 40);
        if dropped > 0 {
            out.push_str(&format!("[earliest {dropped} messages omitted]\n"));
        }
        for line in &rendered[dropped..] {
            out.push_str(line);
        }
        out
    }

    /// Rigid template for the summarizer call — a local model summarizing its
    /// own history drops load-bearing detail without structure to fill in.
    pub fn summarize_prompt(original_goal: &str, transcript: &str) -> String {
        format!(
            r#"Summarize this coding-session transcript so the session can continue from the summary alone. Fill in every section. Keep exact file paths, command lines, and error messages verbatim — never paraphrase an identifier. Do NOT reproduce tool output bodies (file contents, command output, diffs); extract only the conclusion each output led to. Drop dead ends and abandoned explorations unless they produced a constraint worth remembering.

ORIGINAL GOAL:
{original_goal}

TRANSCRIPT:
{transcript}

Respond in exactly this format:
FILES TOUCHED: <paths and what changed in each>
DECISIONS: <choices made and why>
CONSTRAINTS & GOTCHAS: <requirements, invariants, environment limits, sharp edges hit>
STATE: <what currently works / fails, with the latest evidence>
OPEN ITEMS: <what remains to be done, in order>"#
        )
    }

    /// Rebuild the message list after summarization.
    pub fn rebuild(
        system: &Message,
        original_goal: &str,
        summary: &str,
        kept: &[Message],
    ) -> Vec<Message> {
        let mut out = vec![system.clone()];
        out.push(Message::user(format!(
            "[Conversation compacted. Original goal: {original_goal}]\n\n[Summary of earlier work:]\n{summary}"
        )));
        out.push(Message::assistant(
            "Understood. Continuing from the summarized state.",
        ));
        out.extend_from_slice(kept);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg_turn(user: &str, assistant: &str) -> Vec<Message> {
        vec![Message::user(user), Message::assistant(assistant)]
    }

    #[test]
    fn small_context_fits() {
        let cm = ContextManager::new(32_768, 3);
        let messages = vec![Message::system("sys"), Message::user("hi")];
        assert_eq!(cm.plan(&messages, &[]), ContextPlan::Fits);
    }

    #[test]
    fn oversized_context_needs_compaction() {
        let cm = ContextManager::new(8192, 3);
        let big = "x".repeat(20_000);
        let mut messages = vec![Message::system("sys")];
        messages.extend(msg_turn(&big, &big));
        assert_eq!(cm.plan(&messages, &[]), ContextPlan::NeedsCompaction);
    }

    #[test]
    fn plan_three_bands() {
        // usable budget = 8192 - 4096 = 4096 tokens; bands at 60% / 80%.
        let cm = ContextManager::new(8192, 3);
        let sized = |bytes: usize| {
            vec![
                Message::system("sys"),
                Message::user("x".repeat(bytes)),
                Message::assistant("ok"),
            ]
        };
        // ~3.5 bytes/token: well under 60%.
        assert_eq!(cm.plan(&sized(1000), &[]), ContextPlan::Fits);
        // ~2900 tokens: between 60% (2457) and 80% (3276).
        assert_eq!(cm.plan(&sized(10_000), &[]), ContextPlan::NearBudget);
        // Far over 80%.
        assert_eq!(cm.plan(&sized(20_000), &[]), ContextPlan::NeedsCompaction);
    }

    #[test]
    fn split_keeps_last_two_turns() {
        let cm = ContextManager::new(8192, 3);
        let mut messages = vec![Message::system("sys")];
        for i in 0..5 {
            messages.extend(msg_turn(&format!("turn {i}"), "done"));
        }
        let (system, old, kept) = cm.split_for_compaction(&messages).unwrap();
        assert_eq!(system.role, Role::System);
        assert_eq!(kept.iter().filter(|m| m.role == Role::User).count(), 2);
        assert!(old.iter().any(|m| m.content == "turn 0"));
        assert!(kept.iter().any(|m| m.content == "turn 4"));
    }

    #[test]
    fn split_refuses_when_too_short() {
        let cm = ContextManager::new(8192, 3);
        let mut messages = vec![Message::system("sys")];
        messages.extend(msg_turn("only turn", "done"));
        assert!(cm.split_for_compaction(&messages).is_none());
    }

    #[test]
    fn prune_cut_respects_keep_turns() {
        let cm = ContextManager::new(32_768, 3);
        let mut messages = vec![Message::system("sys")];
        for i in 0..5 {
            messages.push(Message::user(format!("turn {i}")));
            messages.push(Message::tool_result("id", "out"));
            messages.push(Message::assistant("done"));
        }
        let cut = cm.prune_cut(&messages).unwrap();
        // 5 user turns at indices 1,4,7,10,13; keep last 3 → cut at "turn 2".
        assert_eq!(messages[cut].content, "turn 2");
        // Everything before the cut is the prunable region.
        assert!(messages[..cut].iter().any(|m| m.role == Role::Tool));
    }

    #[test]
    fn prune_cut_none_when_short_or_disabled() {
        let cm = ContextManager::new(32_768, 3);
        let mut messages = vec![Message::system("sys")];
        for i in 0..3 {
            messages.extend(msg_turn(&format!("turn {i}"), "done"));
        }
        assert!(
            cm.prune_cut(&messages).is_none(),
            "3 turns, keep 3 — none old"
        );

        let off = ContextManager::new(32_768, 0);
        for i in 3..10 {
            messages.extend(msg_turn(&format!("turn {i}"), "done"));
        }
        assert!(off.prune_cut(&messages).is_none(), "0 disables pruning");
    }

    #[test]
    fn keep_tool_turns_clamped_to_compaction_tail() {
        // 1 would let stubs leak into compaction's verbatim last-2-turns tail.
        let cm = ContextManager::new(32_768, 1);
        let mut messages = vec![Message::system("sys")];
        for i in 0..3 {
            messages.extend(msg_turn(&format!("turn {i}"), "done"));
        }
        let cut = cm.prune_cut(&messages).unwrap();
        assert_eq!(messages[cut].content, "turn 1", "clamped to keep 2 turns");
    }

    #[test]
    fn prune_stub_format() {
        let content = format!("$ cargo test --workspace\n{}", "line\n".repeat(399));
        let stub = ContextManager::prune_stub(Some("bash"), &content);
        assert!(stub.starts_with(PRUNE_STUB_PREFIX));
        assert!(stub.contains("bash"));
        assert!(stub.contains("400 lines"));
        assert!(stub.contains("cargo test --workspace"));
        assert!(stub.contains("session log"));
        assert!(!stub.contains('\n'), "stub must be one line");
        assert!(stub.len() < 250, "stub must be short: {}", stub.len());
    }

    #[test]
    fn should_prune_gated_by_threshold() {
        // usable budget = 8192 - 4096 = 4096 tokens; 35% = 1434 tokens.
        let cm = ContextManager::new(8192, 3);
        let sized = |bytes: usize| {
            vec![
                Message::system("sys"),
                Message::user("x".repeat(bytes)),
                Message::assistant("ok"),
            ]
        };
        // ~300 tokens: well under the threshold — leave history untouched.
        assert!(!cm.should_prune(&sized(1000), &[]));
        // ~2900 tokens: over it.
        assert!(cm.should_prune(&sized(10_000), &[]));

        let off = ContextManager::new(8192, 0);
        assert!(
            !off.should_prune(&sized(10_000), &[]),
            "keep_tool_turns = 0 disables pruning"
        );
    }

    #[test]
    fn summary_transcript_passes_small_input_through() {
        let old = vec![Message::user("fix the bug"), Message::assistant("done")];
        let t = ContextManager::render_summary_transcript(&old);
        assert_eq!(t, "[User] fix the bug\n[Assistant] done\n");
    }

    #[test]
    fn summary_transcript_caps_each_message() {
        let old = vec![Message::tool_result("1", "y".repeat(10_000))];
        let t = ContextManager::render_summary_transcript(&old);
        assert!(t.len() < 2000, "{}", t.len());
        assert!(t.ends_with("…\n"));
    }

    #[test]
    fn summary_transcript_drops_oldest_when_over_budget() {
        // 60 messages × ~1.5KB kept each ≈ 90KB > 48KB cap.
        let old: Vec<Message> = (0..60)
            .map(|i| Message::user(format!("{i}:{}", "x".repeat(2000))))
            .collect();
        let t = ContextManager::render_summary_transcript(&old);
        assert!(t.len() <= TRANSCRIPT_MAX_BYTES + 40, "{}", t.len());
        assert!(t.starts_with("[earliest "), "{}", &t[..40]);
        assert!(t.contains("messages omitted"));
        assert!(!t.contains("[User] 0:"), "oldest dropped");
        assert!(t.contains("[User] 59:"), "newest kept");
    }

    #[test]
    fn summary_transcript_utf8_safe() {
        let old = vec![Message::user("é".repeat(TRANSCRIPT_MSG_CAP))];
        let t = ContextManager::render_summary_transcript(&old);
        assert!(t.ends_with("…\n"));
    }

    #[test]
    fn summarize_prompt_has_new_sections() {
        let p = ContextManager::summarize_prompt("goal", "transcript");
        assert!(p.contains("CONSTRAINTS & GOTCHAS"));
        assert!(p.contains("Do NOT reproduce tool output"));
        assert!(p.contains("verbatim"));
    }
}
