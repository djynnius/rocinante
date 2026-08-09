//! Auto-gated quality check. After a substantial turn (one that edited files
//! or ran a command), a background pass asks a checker model whether the
//! result satisfies the original request, optionally runs a configured test
//! command, and emits a non-blocking [`AgentEvent::VerificationReport`]. Never
//! blocks the turn; a dropped check is simply not shown.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rocinante_providers::{GenParams, Provider};

use crate::agent::events::{AgentEvent, EventSender};
use crate::agent::one_shot_call;

const VERIFIER_SYSTEM: &str = "You verify whether completed coding work satisfies the user's request. Be concise and concrete.";

pub struct Verifier {
    provider: Arc<dyn Provider>,
    model: String,
    params: GenParams,
    /// Optional trusted test/build command run in `cwd` (e.g. `cargo test`).
    check_command: Option<String>,
    timeout: Duration,
    cwd: PathBuf,
    /// Run automatically after substantial turns (else only via `/verify`).
    pub auto: bool,
    events: EventSender,
}

impl Verifier {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn Provider>,
        model: String,
        params: GenParams,
        check_command: Option<String>,
        timeout_secs: u64,
        cwd: PathBuf,
        auto: bool,
        events: EventSender,
    ) -> Self {
        Self {
            provider,
            model,
            params,
            check_command,
            timeout: Duration::from_secs(timeout_secs.max(5)),
            cwd,
            auto,
            events,
        }
    }

    /// Spawn a background check comparing `result` against `ask`; emits a
    /// `VerificationReport`. Owned clones only — never borrows the agent.
    pub fn spawn_check(&self, ask: String, result: String, work_summary: String) {
        let provider = Arc::clone(&self.provider);
        let model = self.model.clone();
        let params = self.params.clone();
        let check_command = self.check_command.clone();
        let timeout = self.timeout;
        let cwd = self.cwd.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let prompt = check_prompt(&ask, &result, &work_summary);
            let mut ok;
            let mut findings;
            match tokio::time::timeout(
                timeout,
                one_shot_call(provider, model, params, VERIFIER_SYSTEM, &prompt),
            )
            .await
            {
                Ok(Ok(text)) => {
                    let (o, f) = parse_verdict(&text);
                    ok = o;
                    findings = f;
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "verifier model call failed");
                    return;
                }
                Err(_) => {
                    tracing::warn!("verifier model call timed out");
                    return;
                }
            }
            if let Some(cmd) = check_command {
                let report = run_check_command(&cmd, &cwd, timeout).await;
                if report.failed {
                    ok = false;
                }
                findings = if findings.is_empty() {
                    report.summary
                } else {
                    format!("{findings}\n\n{}", report.summary)
                };
            }
            events.send(AgentEvent::VerificationReport { ok, findings });
        });
    }
}

fn check_prompt(ask: &str, result: &str, work_summary: &str) -> String {
    format!(
        r#"A coding agent was asked to do a task and reported it done. Judge whether the result satisfies the ask.

THE ASK:
{ask}

WHAT THE AGENT DID (tool actions this turn):
{work_summary}

THE AGENT'S FINAL REPORT:
{result}

If the result fully satisfies the ask, respond with exactly: OK
Otherwise respond with a short bulleted list of concrete gaps — only real, specific shortfalls (a file the ask named but wasn't produced, a case the ask required but wasn't handled, a direct contradiction with the request). Do not invent work that wasn't asked for. Do not nitpick style. If you are unsure, respond OK."#
    )
}

/// Interpret a verdict: `OK` (no bullet lines) → passed; anything with bullets
/// → gaps. Ambiguous output biases to OK to avoid false-positive noise.
fn parse_verdict(text: &str) -> (bool, String) {
    let t = text.trim();
    let has_bullets = t
        .lines()
        .any(|l| l.trim_start().starts_with("- ") || l.trim_start().starts_with("* "));
    // Passed when empty, or starts with OK and lists no gaps; else the gaps.
    if t.is_empty() || (!has_bullets && t.to_ascii_uppercase().starts_with("OK")) {
        (true, String::new())
    } else {
        (false, t.to_string())
    }
}

struct CommandReport {
    failed: bool,
    summary: String,
}

/// Run a trusted check command through the shell in `cwd`, bounded; report
/// pass/fail plus a tail of the output.
async fn run_check_command(cmd: &str, cwd: &PathBuf, timeout: Duration) -> CommandReport {
    let mut command = tokio::process::Command::new("sh");
    command.arg("-c").arg(cmd).current_dir(cwd);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(out)) => {
            let ok = out.status.success();
            let mut body = String::from_utf8_lossy(&out.stdout).into_owned();
            body.push_str(&String::from_utf8_lossy(&out.stderr));
            let tail = tail_chars(&body, 1500);
            CommandReport {
                failed: !ok,
                summary: if ok {
                    format!("check ({cmd}): PASS")
                } else {
                    format!("check ({cmd}): FAIL\n{tail}")
                },
            }
        }
        Ok(Err(e)) => CommandReport {
            failed: true,
            summary: format!("check ({cmd}): could not run ({e})"),
        },
        Err(_) => CommandReport {
            failed: true,
            summary: format!("check ({cmd}): timed out"),
        },
    }
}

fn tail_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    s.chars().skip(count - n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_prompt_has_parts_and_ok_bias() {
        let p = check_prompt("do X", "did X", "[edit] a.rs");
        assert!(p.contains("do X") && p.contains("did X") && p.contains("[edit] a.rs"));
        assert!(p.contains("respond with exactly: OK"));
        assert!(p.contains("If you are unsure, respond OK"));
    }

    #[test]
    fn parse_verdict_variants() {
        assert_eq!(parse_verdict("OK"), (true, String::new()));
        assert_eq!(parse_verdict("  OK  "), (true, String::new()));
        assert_eq!(parse_verdict("OK, looks complete"), (true, String::new()));
        assert_eq!(parse_verdict(""), (true, String::new()));
        let (ok, f) = parse_verdict("- missing tests\n- no error handling");
        assert!(!ok && f.contains("missing tests"));
    }

    #[test]
    fn tail_keeps_last_n() {
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("ab", 5), "ab");
    }

    #[tokio::test]
    async fn check_command_pass_and_fail() {
        let cwd = std::env::temp_dir();
        let pass = run_check_command("exit 0", &cwd, Duration::from_secs(5)).await;
        assert!(!pass.failed && pass.summary.contains("PASS"));
        let fail = run_check_command("echo boom >&2; exit 1", &cwd, Duration::from_secs(5)).await;
        assert!(fail.failed && fail.summary.contains("FAIL") && fail.summary.contains("boom"));
    }
}
