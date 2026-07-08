//! Context budgeting and compaction. The operative ceiling is the configured
//! `num_ctx`, not the model's advertised maximum — VRAM is the real limit.

use rocinante_providers::{Message, Role, ToolSchema, tokens};

/// Reserved for the model's own output within num_ctx.
const OUTPUT_RESERVE: usize = 4096;
/// Compact when estimated usage crosses this fraction of the usable budget.
const COMPACT_THRESHOLD: f64 = 0.80;
/// How many trailing turns survive compaction verbatim.
const KEEP_LAST_TURNS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPlan {
    Fits,
    NeedsCompaction,
}

pub struct ContextManager {
    num_ctx: usize,
}

impl ContextManager {
    pub fn new(num_ctx: u32) -> Self {
        Self {
            num_ctx: num_ctx as usize,
        }
    }

    pub fn usable_budget(&self) -> usize {
        self.num_ctx.saturating_sub(OUTPUT_RESERVE)
    }

    pub fn plan(&self, messages: &[Message], tools: &[ToolSchema]) -> ContextPlan {
        let estimate = tokens::estimate_messages(messages, tools);
        if (estimate as f64) < self.usable_budget() as f64 * COMPACT_THRESHOLD {
            ContextPlan::Fits
        } else {
            ContextPlan::NeedsCompaction
        }
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

    /// Rigid template for the summarizer call — a local model summarizing its
    /// own history drops load-bearing detail without structure to fill in.
    pub fn summarize_prompt(original_goal: &str, transcript: &str) -> String {
        format!(
            r#"Summarize this coding-session transcript. Fill in every section. Be specific: keep exact file paths, command names, and error messages.

ORIGINAL GOAL:
{original_goal}

TRANSCRIPT:
{transcript}

Respond in exactly this format:
FILES TOUCHED: <paths and what changed in each>
DECISIONS: <choices made and why>
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
        let cm = ContextManager::new(32_768);
        let messages = vec![Message::system("sys"), Message::user("hi")];
        assert_eq!(cm.plan(&messages, &[]), ContextPlan::Fits);
    }

    #[test]
    fn oversized_context_needs_compaction() {
        let cm = ContextManager::new(8192);
        let big = "x".repeat(20_000);
        let mut messages = vec![Message::system("sys")];
        messages.extend(msg_turn(&big, &big));
        assert_eq!(cm.plan(&messages, &[]), ContextPlan::NeedsCompaction);
    }

    #[test]
    fn split_keeps_last_two_turns() {
        let cm = ContextManager::new(8192);
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
        let cm = ContextManager::new(8192);
        let mut messages = vec![Message::system("sys")];
        messages.extend(msg_turn("only turn", "done"));
        assert!(cm.split_for_compaction(&messages).is_none());
    }
}
