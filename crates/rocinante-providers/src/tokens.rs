//! Approximate token counting with per-model calibration.
//!
//! We don't ship tokenizers. Instead: a byte-ratio heuristic corrected by a
//! running ratio observed from real `prompt_eval_count` values the provider
//! returns. Estimates deliberately overshoot — the context manager uses them
//! to size `num_ctx`, and undershooting means silent truncation.

use std::sync::Mutex;

use crate::{Message, ToolSchema};

/// Starting guess: ~3.5 bytes/token for English-ish text and code.
const BYTES_PER_TOKEN: f64 = 3.5;
/// Per-message wire overhead (role tags, separators).
const PER_MESSAGE_OVERHEAD: usize = 4;
/// Safety margin applied on top of the calibrated estimate.
const SAFETY_MARGIN: f64 = 1.10;

pub fn estimate_text(text: &str) -> usize {
    (text.len() as f64 / BYTES_PER_TOKEN).ceil() as usize
}

pub fn estimate_messages(messages: &[Message], tools: &[ToolSchema]) -> usize {
    let msg_tokens: usize = messages
        .iter()
        .map(|m| {
            let call_bytes: usize = m
                .tool_calls
                .iter()
                .map(|c| c.name.len() + c.arguments.to_string().len())
                .sum();
            estimate_text(&m.content)
                + (call_bytes as f64 / BYTES_PER_TOKEN).ceil() as usize
                + PER_MESSAGE_OVERHEAD
        })
        .sum();
    let tool_tokens: usize = tools
        .iter()
        .map(|t| {
            estimate_text(&t.description)
                + estimate_text(&t.parameters.to_string())
                + estimate_text(&t.name)
        })
        .sum();
    msg_tokens + tool_tokens
}

/// Learns the ratio between our estimates and a model's real prompt token
/// counts, via exponential moving average. One per (provider, model).
pub struct TokenCalibrator {
    /// EMA of observed/estimated; starts at 1.0 (trust the heuristic).
    ratio: Mutex<f64>,
}

impl Default for TokenCalibrator {
    fn default() -> Self {
        Self {
            ratio: Mutex::new(1.0),
        }
    }
}

impl TokenCalibrator {
    const ALPHA: f64 = 0.3;

    /// Record a (our estimate, actual prompt_eval_count) observation.
    pub fn observe(&self, estimated: usize, actual: u64) {
        if estimated == 0 || actual == 0 {
            return;
        }
        let obs = actual as f64 / estimated as f64;
        let mut ratio = self.ratio.lock().unwrap();
        *ratio = *ratio * (1.0 - Self::ALPHA) + obs * Self::ALPHA;
        if obs > 1.05 {
            tracing::warn!(
                estimated,
                actual,
                ratio = *ratio,
                "token estimate undershot actual prompt size"
            );
        }
    }

    /// Correct a raw estimate, always adding the safety margin.
    pub fn correct(&self, estimated: usize) -> usize {
        let ratio = *self.ratio.lock().unwrap();
        (estimated as f64 * ratio.max(1.0) * SAFETY_MARGIN).ceil() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_scales_with_length() {
        assert!(estimate_text("hello world, this is a test") > estimate_text("hi"));
    }

    #[test]
    fn calibrator_learns_undershoot() {
        let cal = TokenCalibrator::default();
        let raw = 1000;
        // Model consistently reports 30% more tokens than we guess.
        for _ in 0..20 {
            cal.observe(raw, 1300);
        }
        let corrected = cal.correct(raw);
        assert!(
            corrected >= 1300,
            "corrected {corrected} should cover actual 1300"
        );
    }

    #[test]
    fn correction_never_shrinks_estimate() {
        let cal = TokenCalibrator::default();
        // Model reports fewer tokens than estimated; we still don't shrink below raw.
        for _ in 0..20 {
            cal.observe(1000, 500);
        }
        assert!(cal.correct(1000) >= 1000);
    }
}
