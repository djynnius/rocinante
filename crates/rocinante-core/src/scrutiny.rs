//! Scrutiny: a critic model reviews a turn's answer before the user sees it
//! and checks completed work before it is certified done. The agent drives
//! the loops (`Agent::scrutinize_answer`, `Agent::iterate_verification`);
//! this module owns the critic call, its prompts and verdict parsing.
//!
//! The critic is the active main model unless one is pinned
//! (`[scrutiny] model`, `/scrutiny model`). A pinned critic that fails falls
//! back to self-scrutiny for the rest of the session. Infrastructure
//! failures never block a turn: the answer is released unreviewed and the
//! work is treated as OK.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rocinante_providers::{GenParams, Provider, Usage};

use crate::agent::one_shot_call;
use crate::config::{ActionScrutiny, AnswerScrutiny, ScrutinyConfig, ScrutinyShow};
use crate::scheduler::Scheduler;

const VERIFIER_SYSTEM: &str = "You verify whether completed coding work satisfies the user's request. Be concise and concrete.";

const CRITIC_SYSTEM: &str = "You review a draft answer before the user sees it. Compare it with THE ASK, the agent's actions this turn, and the evidence in tool results. Report only concrete defects: a requirement not met, a claim contradicted by the evidence, a wrong command or path, an unsafe suggestion, a question the user asked that is unanswered. Do not comment on style or length. If there are no such defects respond with exactly: APPROVE. Otherwise respond `REVISE` followed by a bulleted list of the defects.";

const ACTION_SYSTEM: &str = "You check one action a coding agent is about to take on the user's machine. Judge only whether it is safe and consistent with what the user asked. If it is, respond with exactly: APPROVE. Otherwise respond `REVISE` followed by a bulleted list of the concrete problems (destructive or irreversible effects the ask does not call for, wrong target, data loss, secrets exposure).";

/// The critic request is capped at this many (estimated) tokens; the
/// conversation digest is dropped first when over.
const CRITIC_TOKEN_CAP: usize = 8_000;
/// Per-message cap in the conversation digest.
const DIGEST_MESSAGE_CHARS: usize = 1_200;
/// User/assistant pairs in the digest.
const DIGEST_PAIRS: usize = 4;

/// A model pinned as critic.
#[derive(Clone)]
pub struct CriticModel {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub params: GenParams,
    /// The name the user chose (alias or tag), for notices and the sidebar.
    pub display: String,
}

/// The model one critic call runs on.
#[derive(Clone)]
pub struct CriticTarget {
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub params: GenParams,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    Revise(String),
}

/// Everything the answer critic reads.
#[derive(Debug, Clone, Default)]
pub struct ReviewInput {
    pub ask: String,
    pub draft: String,
    /// One line per tool action this turn.
    pub work_summary: String,
    /// Recent conversation before this turn.
    pub digest: String,
    /// Heads of this turn's tool results (the stubs pruning keeps).
    pub tool_heads: String,
    /// The user's LESSONS.md rules; a violation is a defect.
    pub lessons: String,
}

/// One critic exchange: the reply (None = no critic reachable), tokens
/// spent, and a notice for the user when something degraded.
pub struct CriticReply {
    pub text: Option<String>,
    pub usage: Usage,
    pub notice: Option<String>,
}

#[derive(Clone)]
pub struct Scrutiny {
    pub answers: AnswerScrutiny,
    pub work: bool,
    pub max_revisions: u32,
    pub max_iterations: u32,
    pub show: ScrutinyShow,
    pub min_chars: usize,
    pub actions: ActionScrutiny,
    pub risky_edit_lines: usize,
    critic: Option<CriticModel>,
    check_command: Option<String>,
    timeout: Duration,
    cwd: PathBuf,
    scheduler: Option<Arc<Scheduler>>,
    /// The pinned critic failed: self-scrutiny for the rest of the session.
    pinned_failed: Arc<AtomicBool>,
    /// At most one "critic unreachable" notice per session.
    noticed: Arc<AtomicBool>,
}

impl Scrutiny {
    pub fn from_config(config: &ScrutinyConfig, critic: Option<CriticModel>, cwd: PathBuf) -> Self {
        Self {
            answers: if config.enabled {
                config.answers
            } else {
                AnswerScrutiny::Off
            },
            work: config.enabled && config.work,
            max_revisions: config.max_revisions,
            max_iterations: config.max_iterations,
            show: config.show,
            min_chars: config.min_chars,
            actions: config.actions,
            risky_edit_lines: config.risky_edit_lines,
            critic,
            check_command: config.check_command.clone(),
            timeout: Duration::from_secs(config.timeout_secs.max(5)),
            cwd,
            scheduler: None,
            pinned_failed: Arc::new(AtomicBool::new(false)),
            noticed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_scheduler(mut self, scheduler: Arc<Scheduler>) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// Pin a critic model (`None` = self-scrutiny with the main model).
    pub fn set_critic(&mut self, critic: Option<CriticModel>) {
        self.critic = critic;
        self.pinned_failed = Arc::new(AtomicBool::new(false));
    }

    /// The pinned critic's display name, while it is in use.
    pub fn critic_display(&self) -> Option<&str> {
        if self.pinned_failed.load(Ordering::SeqCst) {
            return None;
        }
        self.critic.as_ref().map(|c| c.display.as_str())
    }

    /// Does this turn's answer get reviewed?
    pub fn should_review_answer(&self, draft: &str, did_work: bool, plan: bool) -> bool {
        let draft = draft.trim();
        if draft.is_empty() || is_clarifying_question(draft) {
            return false;
        }
        match self.answers {
            AnswerScrutiny::Off => false,
            AnswerScrutiny::All => true,
            AnswerScrutiny::Substantial => {
                plan || did_work || draft.chars().count() >= self.min_chars
            }
        }
    }

    /// Ask the critic (pinned first, then `main`), bounded by the timeout.
    async fn ask(&self, main: &CriticTarget, system: &str, prompt: &str) -> CriticReply {
        let mut usage = Usage::default();
        let mut notice = None;
        if let Some(critic) = &self.critic
            && !self.pinned_failed.load(Ordering::SeqCst)
        {
            let target = CriticTarget {
                provider: Arc::clone(&critic.provider),
                model: critic.model.clone(),
                params: critic.params.clone(),
            };
            match self.call(&target, system, prompt).await {
                Ok((text, u)) => {
                    return CriticReply {
                        text: Some(text),
                        usage: u,
                        notice: None,
                    };
                }
                Err(e) => {
                    tracing::warn!(critic = %critic.display, error = %e, "pinned critic failed");
                    self.pinned_failed.store(true, Ordering::SeqCst);
                    notice = Some(format!(
                        "critic {} unavailable; self-scrutiny",
                        critic.display
                    ));
                }
            }
        }
        match self.call(main, system, prompt).await {
            Ok((text, u)) => {
                usage.prompt_tokens += u.prompt_tokens;
                usage.completion_tokens += u.completion_tokens;
                CriticReply {
                    text: Some(text),
                    usage,
                    notice,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "critic call failed");
                let unreachable = (!self.noticed.swap(true, Ordering::SeqCst))
                    .then(|| format!("critic unavailable ({e}); released without review"));
                CriticReply {
                    text: None,
                    usage,
                    notice: notice.or(unreachable),
                }
            }
        }
    }

    async fn call(
        &self,
        target: &CriticTarget,
        system: &str,
        prompt: &str,
    ) -> anyhow::Result<(String, Usage)> {
        let call = one_shot_call(
            Arc::clone(&target.provider),
            target.model.clone(),
            target.params.clone(),
            system,
            prompt,
            self.scheduler.as_ref(),
        );
        match tokio::time::timeout(self.timeout, call).await {
            Ok(Ok(out)) => Ok((out.text, out.usage)),
            Ok(Err(e)) => Err(e),
            Err(_) => anyhow::bail!("timed out after {}s", self.timeout.as_secs()),
        }
    }

    /// Review a draft answer. No reachable critic = approve (release).
    pub async fn review_answer(
        &self,
        main: &CriticTarget,
        input: &ReviewInput,
    ) -> (Verdict, CriticReply) {
        let prompt = critic_prompt(input);
        let reply = self.ask(main, CRITIC_SYSTEM, &prompt).await;
        let verdict = match &reply.text {
            Some(text) => parse_review(text),
            None => Verdict::Approve,
        };
        (verdict, reply)
    }

    /// Review one risky action before it runs (phase-2 pre-action
    /// scrutiny). No reachable critic = approve.
    pub async fn review_action(
        &self,
        main: &CriticTarget,
        ask: &str,
        action: &str,
        detail: Option<&str>,
    ) -> (Verdict, CriticReply) {
        let mut prompt = format!("THE ASK:\n{ask}\n\nTHE ACTION ABOUT TO RUN:\n{action}\n");
        if let Some(detail) = detail {
            prompt.push_str(&format!("\nDETAIL:\n{}\n", head_chars(detail, 4_000)));
        }
        let reply = self.ask(main, ACTION_SYSTEM, &prompt).await;
        let verdict = match &reply.text {
            Some(text) => parse_review(text),
            None => Verdict::Approve,
        };
        (verdict, reply)
    }

    /// Judge whether `result` satisfies `ask`; returns `(ok, findings,
    /// reply)` where `ok` means done and `findings` is a concrete gap list
    /// otherwise. Infra failures bias to OK so they never block a turn; a
    /// failing `check_command` always fails.
    pub async fn check(
        &self,
        main: &CriticTarget,
        ask: &str,
        result: &str,
        work_summary: &str,
    ) -> (bool, String, CriticReply) {
        let prompt = check_prompt(ask, result, work_summary);
        let reply = self.ask(main, VERIFIER_SYSTEM, &prompt).await;
        let (mut ok, mut findings) = match &reply.text {
            Some(text) => parse_verdict(text),
            None => (true, String::new()),
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
        (ok, findings, reply)
    }
}

/// A short question back to the user (ends in `?`, under two lines) is a
/// clarification, not an answer: never reviewed.
pub fn is_clarifying_question(text: &str) -> bool {
    let t = text.trim();
    t.ends_with('?') && t.lines().filter(|l| !l.trim().is_empty()).count() < 2
}

/// Assemble the critic request, dropping the digest (then the tool heads)
/// when it would exceed [`CRITIC_TOKEN_CAP`].
pub fn critic_prompt(input: &ReviewInput) -> String {
    let render = |digest: bool, heads: bool, draft: &str| {
        let mut out = format!("THE ASK:\n{}\n", input.ask.trim());
        if !input.lessons.trim().is_empty() {
            out.push_str(&format!(
                "\nUSER RULES (the user's standing instructions; breaking one is a defect):\n{}\n",
                input.lessons.trim()
            ));
        }
        if digest && !input.digest.trim().is_empty() {
            out.push_str(&format!(
                "\nEARLIER CONVERSATION (oldest first, truncated):\n{}\n",
                input.digest.trim()
            ));
        }
        out.push_str(&format!(
            "\nWHAT THE AGENT DID THIS TURN:\n{}\n",
            if input.work_summary.trim().is_empty() {
                "(no tool actions)"
            } else {
                input.work_summary.trim()
            }
        ));
        if heads && !input.tool_heads.trim().is_empty() {
            out.push_str(&format!(
                "\nTOOL RESULTS THIS TURN (first lines):\n{}\n",
                input.tool_heads.trim()
            ));
        }
        out.push_str(&format!("\nTHE DRAFT ANSWER:\n{}\n", draft.trim()));
        out
    };
    let fits = |s: &str| rocinante_providers::tokens::estimate_text(s) <= CRITIC_TOKEN_CAP;
    for (digest, heads) in [(true, true), (false, true), (false, false)] {
        let p = render(digest, heads, &input.draft);
        if fits(&p) {
            return p;
        }
    }
    // Still over: keep the draft's head and tail.
    render(
        false,
        false,
        &middle_out(&input.draft, CRITIC_TOKEN_CAP * 2),
    )
}

/// The digest of the conversation before this turn: the last
/// [`DIGEST_PAIRS`] user/assistant exchanges, each message truncated.
pub fn conversation_digest(messages: &[rocinante_providers::Message]) -> String {
    use rocinante_providers::Role;
    let mut picked: Vec<String> = Vec::new();
    let mut users = 0;
    for m in messages.iter().rev() {
        let who = match m.role {
            Role::User => "user",
            Role::Assistant if !m.content.trim().is_empty() => "assistant",
            _ => continue,
        };
        picked.push(format!(
            "[{who}] {}",
            head_chars(m.content.trim(), DIGEST_MESSAGE_CHARS)
        ));
        if m.role == Role::User {
            users += 1;
            if users >= DIGEST_PAIRS {
                break;
            }
        }
    }
    picked.reverse();
    picked.join("\n")
}

/// The hidden user turn that asks for a corrected answer.
pub fn answer_corrective_prompt(findings: &str) -> String {
    format!(
        "A reviewer found these defects in your draft answer:\n\n{findings}\n\nWrite the corrected final answer. Use tools if you need to check something. Do not mention the review."
    )
}

/// Lenient answer verdict: `APPROVE`/`OK` with no bullets approves, any
/// bullet list revises, a bare `REVISE …` revises; anything else approves
/// (an unparseable critic must not block the answer).
pub fn parse_review(text: &str) -> Verdict {
    let t = text.trim();
    let has_bullets = t.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("- ") || l.starts_with("* ") || l.starts_with("• ")
    });
    let upper = t.to_ascii_uppercase();
    if has_bullets || upper.starts_with("REVISE") {
        let findings = t
            .strip_prefix("REVISE")
            .or_else(|| t.strip_prefix("Revise"))
            .unwrap_or(t)
            .trim_start_matches([':', ' ', '\n'])
            .trim()
            .to_string();
        if findings.is_empty() {
            return Verdict::Approve;
        }
        return Verdict::Revise(findings);
    }
    Verdict::Approve
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

/// Interpret a work verdict: `OK` (no bullet lines) → passed; anything with
/// bullets → gaps. Ambiguous output biases to OK to avoid false-positive noise.
fn parse_verdict(text: &str) -> (bool, String) {
    let t = text.trim();
    let has_bullets = t
        .lines()
        .any(|l| l.trim_start().starts_with("- ") || l.trim_start().starts_with("* "));
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

fn head_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    format!("{head}…")
}

/// Keep the first and last halves of `s` within `bytes`.
fn middle_out(s: &str, bytes: usize) -> String {
    if s.len() <= bytes {
        return s.to_string();
    }
    let half = bytes / 2;
    let mut head = half;
    while !s.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = s.len() - half;
    while !s.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}\n… [middle omitted] …\n{}", &s[..head], &s[tail..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocinante_providers::Message;

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
    fn parse_review_variants() {
        assert_eq!(parse_review("APPROVE"), Verdict::Approve);
        assert_eq!(parse_review(" approve "), Verdict::Approve);
        assert_eq!(parse_review("OK"), Verdict::Approve);
        assert_eq!(parse_review(""), Verdict::Approve);
        assert_eq!(parse_review("Looks fine to me."), Verdict::Approve);
        assert_eq!(
            parse_review("REVISE\n- the path is wrong\n- step 3 missing"),
            Verdict::Revise("- the path is wrong\n- step 3 missing".into())
        );
        assert_eq!(
            parse_review("- unanswered: which version?"),
            Verdict::Revise("- unanswered: which version?".into())
        );
        assert_eq!(
            parse_review("REVISE: the command deletes the wrong dir"),
            Verdict::Revise("the command deletes the wrong dir".into())
        );
        assert_eq!(parse_review("REVISE"), Verdict::Approve, "no findings");
    }

    #[test]
    fn clarifying_questions_are_detected() {
        assert!(is_clarifying_question("Which branch should I use?"));
        assert!(!is_clarifying_question(
            "Here is the plan.\nDoes that work?"
        ));
        assert!(!is_clarifying_question("Done."));
    }

    fn scrutiny(answers: AnswerScrutiny) -> Scrutiny {
        let config = ScrutinyConfig {
            answers,
            ..Default::default()
        };
        Scrutiny::from_config(&config, None, std::env::temp_dir())
    }

    #[test]
    fn substantial_gating() {
        let s = scrutiny(AnswerScrutiny::Substantial);
        let long = "x".repeat(400);
        assert!(
            !s.should_review_answer("short answer", false, false),
            "short skipped"
        );
        assert!(s.should_review_answer(&long, false, false), "long reviewed");
        assert!(
            s.should_review_answer("did it", true, false),
            "work reviewed"
        );
        assert!(
            s.should_review_answer("1. step\n2. step", false, true),
            "plan reviewed"
        );
        assert!(
            !s.should_review_answer("Which file do you mean?", true, true),
            "questions never reviewed"
        );
        assert!(!s.should_review_answer("   ", true, true));
        let all = scrutiny(AnswerScrutiny::All);
        assert!(all.should_review_answer("short", false, false));
        let off = scrutiny(AnswerScrutiny::Off);
        assert!(!off.should_review_answer(&long, true, true));
    }

    #[test]
    fn disabled_config_turns_both_reviews_off() {
        let config = ScrutinyConfig {
            enabled: false,
            ..Default::default()
        };
        let s = Scrutiny::from_config(&config, None, std::env::temp_dir());
        assert_eq!(s.answers, AnswerScrutiny::Off);
        assert!(!s.work);
    }

    #[test]
    fn critic_prompt_includes_parts_and_drops_digest_when_over_cap() {
        let input = ReviewInput {
            ask: "explain X".into(),
            draft: "X is Y".into(),
            work_summary: "[read] {\"path\":\"a.rs\"}".into(),
            digest: "[user] earlier".into(),
            tool_heads: "[pruned tool result: read] fn main".into(),
            lessons: "- never use unwrap".into(),
        };
        let p = critic_prompt(&input);
        for part in [
            "explain X",
            "X is Y",
            "a.rs",
            "earlier",
            "fn main",
            "never use unwrap",
        ] {
            assert!(p.contains(part), "missing {part}: {p}");
        }
        let big = ReviewInput {
            digest: "d".repeat(40_000),
            ..input.clone()
        };
        let p = critic_prompt(&big);
        assert!(!p.contains("EARLIER CONVERSATION"), "digest dropped first");
        assert!(p.contains("fn main"), "tool heads kept");
        let huge = ReviewInput {
            draft: "z".repeat(100_000),
            ..input
        };
        let p = critic_prompt(&huge);
        assert!(rocinante_providers::tokens::estimate_text(&p) <= CRITIC_TOKEN_CAP + 100);
        assert!(p.contains("middle omitted"));
    }

    #[test]
    fn digest_keeps_last_pairs_truncated() {
        let mut msgs = vec![Message::system("sys")];
        for i in 0..6 {
            msgs.push(Message::user(format!("question {i}")));
            msgs.push(Message::assistant(format!(
                "answer {i} {}",
                "a".repeat(2000)
            )));
        }
        let d = conversation_digest(&msgs);
        assert!(!d.contains("question 1"), "only the last 4 pairs: {d}");
        assert!(d.contains("question 2") && d.contains("question 5"));
        assert!(d.lines().all(|l| l.chars().count() <= 1_220));
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

    /// Streams a fixed verdict, or fails every call.
    struct FixedProvider(Option<&'static str>);

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
            use rocinante_providers::{ChatDelta, ProviderError, StopReason};
            let Some(text) = self.0 else {
                return Err(ProviderError::Api {
                    status: 503,
                    message: "down".into(),
                });
            };
            let deltas: Vec<Result<ChatDelta, ProviderError>> = vec![
                Ok(ChatDelta::Text(text.into())),
                Ok(ChatDelta::Usage(Usage {
                    prompt_tokens: 100,
                    completion_tokens: 5,
                })),
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

    fn target(says: Option<&'static str>) -> CriticTarget {
        CriticTarget {
            provider: Arc::new(FixedProvider(says)),
            model: "fixed".into(),
            params: GenParams::default(),
        }
    }

    #[tokio::test]
    async fn check_reports_ok_and_gaps() {
        let s = scrutiny(AnswerScrutiny::Off);
        let (ok, findings, _) = s
            .check(&target(Some("OK")), "do X", "did X", "[edit] a.rs")
            .await;
        assert!(ok && findings.is_empty());
        let (ok, findings, _) = s
            .check(
                &target(Some("- missing the tests the ask required")),
                "do X with tests",
                "did X",
                "[edit] a.rs",
            )
            .await;
        assert!(!ok && findings.contains("missing the tests"));
    }

    #[tokio::test]
    async fn critic_usage_is_reported() {
        let s = scrutiny(AnswerScrutiny::All);
        let (verdict, reply) = s
            .review_answer(&target(Some("APPROVE")), &ReviewInput::default())
            .await;
        assert_eq!(verdict, Verdict::Approve);
        assert_eq!(reply.usage.prompt_tokens, 100);
        assert!(reply.notice.is_none());
    }

    #[tokio::test]
    async fn pinned_critic_failure_falls_back_once_with_a_notice() {
        let mut s = scrutiny(AnswerScrutiny::All);
        s.set_critic(Some(CriticModel {
            provider: Arc::new(FixedProvider(None)),
            model: "kimi-k3.1-pro:cloud".into(),
            params: GenParams::default(),
            display: "kimi-k3.1-pro:cloud".into(),
        }));
        assert_eq!(s.critic_display(), Some("kimi-k3.1-pro:cloud"));
        let (verdict, reply) = s
            .review_answer(&target(Some("REVISE\n- wrong")), &ReviewInput::default())
            .await;
        assert_eq!(
            verdict,
            Verdict::Revise("- wrong".into()),
            "main model reviewed"
        );
        assert_eq!(
            reply.notice.as_deref(),
            Some("critic kimi-k3.1-pro:cloud unavailable; self-scrutiny")
        );
        assert_eq!(s.critic_display(), None, "self-scrutiny from now on");
        let (_, reply) = s
            .review_answer(&target(Some("APPROVE")), &ReviewInput::default())
            .await;
        assert!(reply.notice.is_none(), "one notice per session");
    }

    #[tokio::test]
    async fn unreachable_critic_releases_with_one_notice() {
        let s = scrutiny(AnswerScrutiny::All);
        let (verdict, reply) = s
            .review_answer(&target(None), &ReviewInput::default())
            .await;
        assert_eq!(verdict, Verdict::Approve, "release unreviewed");
        assert!(reply.notice.unwrap().contains("critic unavailable"));
        let (_, reply) = s
            .review_answer(&target(None), &ReviewInput::default())
            .await;
        assert!(reply.notice.is_none());
    }
}
