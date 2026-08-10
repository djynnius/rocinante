//! Auto-gated quality check. After a substantial turn (one that edited files
//! or ran a command), a checker model judges whether the result satisfies the
//! original request and optionally runs a configured test command. The agent
//! turns any gaps into a corrective retry (up to `max_iterations`) before
//! certifying the work done — see `Agent::iterate_verification`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rocinante_providers::{GenParams, Provider};

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
    /// Corrective retries allowed before certifying done (0 = report only).
    pub max_iterations: u32,
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
        max_iterations: u32,
    ) -> Self {
        Self {
            provider,
            model,
            params,
            check_command,
            timeout: Duration::from_secs(timeout_secs.max(5)),
            cwd,
            auto,
            max_iterations,
        }
    }

    /// Judge whether `result` satisfies `ask`; returns `(ok, findings)` where
    /// `ok` means done and `findings` is a concrete gap list otherwise. Infra
    /// failures (model error/timeout) bias to OK so they never block a turn; a
    /// failing `check_command` always fails. Awaited inline by the agent.
    pub async fn check(&self, ask: &str, result: &str, work_summary: &str) -> (bool, String) {
        let prompt = check_prompt(ask, result, work_summary);
        let (mut ok, mut findings) = match tokio::time::timeout(
            self.timeout,
            one_shot_call(
                Arc::clone(&self.provider),
                self.model.clone(),
                self.params.clone(),
                VERIFIER_SYSTEM,
                &prompt,
            ),
        )
        .await
        {
            Ok(Ok(text)) => parse_verdict(&text),
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "verifier model call failed");
                (true, String::new())
            }
            Err(_) => {
                tracing::warn!("verifier model call timed out");
                (true, String::new())
            }
        };
        if let Some(cmd) = &self.check_command {
            let report = run_check_command(cmd, &self.cwd, self.timeout).await;
            if report.failed {
                ok = false;
            }
            findings = if findings.is_empty() {
                report.summary
            } else {
                format!("{findings}\n\n{}", report.summary)
            };
        }
        (ok, findings)
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

    /// A provider that streams a fixed verdict string, for `check`.
    struct FixedProvider(&'static str);

    #[async_trait::async_trait]
    impl Provider for FixedProvider {
        fn id(&self) -> &str {
            "fixed"
        }
        fn caps(&self) -> rocinante_providers::Capabilities {
            rocinante_providers::Capabilities {
                native_tools: false,
                structured_output: false,
                is_local: false,
            }
        }
        async fn chat(
            &self,
            _req: rocinante_providers::ChatRequest,
        ) -> Result<rocinante_providers::ChatStream, rocinante_providers::ProviderError> {
            use rocinante_providers::{ChatDelta, StopReason};
            let deltas: Vec<Result<ChatDelta, rocinante_providers::ProviderError>> = vec![
                Ok(ChatDelta::Text(self.0.into())),
                Ok(ChatDelta::Done(StopReason::EndTurn)),
            ];
            Ok(Box::pin(futures::stream::iter(deltas)))
        }
        fn count_tokens(
            &self,
            _m: &[rocinante_providers::Message],
            _t: &[rocinante_providers::ToolSchema],
        ) -> usize {
            1
        }
    }

    fn verifier_with(model_says: &'static str) -> Verifier {
        Verifier::new(
            Arc::new(FixedProvider(model_says)),
            "fixed".into(),
            GenParams::default(),
            None,
            30,
            std::env::temp_dir(),
            true,
            3,
        )
    }

    #[tokio::test]
    async fn check_reports_ok_and_gaps() {
        let (ok, findings) = verifier_with("OK")
            .check("do X", "did X", "[edit] a.rs")
            .await;
        assert!(ok && findings.is_empty());

        let (ok, findings) = verifier_with("- missing the tests the ask required")
            .check("do X with tests", "did X", "[edit] a.rs")
            .await;
        assert!(!ok && findings.contains("missing the tests"));
    }
}
