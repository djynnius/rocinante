# Spec 02: Scrutiny gate (critic reviews before the user sees the answer)

## Goal

Before a turn's answer reaches the user, a critic model reads the ask, the draft answer and
what the agent did, and either approves it or sends concrete findings back for one or more
silent revisions. The critic is the active main model by default (self-scrutiny) and can be
pinned to a stronger model (`kimi-k3.1-pro:cloud`). The user sees the final answer and, at
most, a small badge; never the critique.

## What exists

`[verification]` (`config/schema.rs:307-347`), `Verifier` (`verifier.rs`), and
`Agent::iterate_verification` (`agent/agent.rs:590-640`) already do this for *work*: after a
turn that edited files or ran commands, a checker (its own `model` knob) judges the result
against the ask, feeds gaps back as a corrective user turn, repeats up to `max_iterations`,
then emits `VerificationReport`. `/verify` runs it on demand. Infra failures bias to OK so
the checker can never block a turn.

What is missing: scrutiny of **answers** (Q&A, design advice, plans, research briefs — the
turns with no edits), hiding the loop from the user, and not streaming a draft that may be
replaced.

## Design

### 1. Config: `[scrutiny]` (alias `[verification]`)

```toml
[scrutiny]
enabled = true
model = "kimi-k3.1-pro:cloud"  # critic; unset = the active main model (follows /model)
answers = "substantial"         # off | substantial | all   — review the final answer text
work = true                     # review completed edits/commands (today's auto-verification)
max_revisions = 1               # silent answer rewrites before releasing
max_iterations = 3              # corrective fix passes for work (unchanged)
check_command = "cargo test"    # unchanged; trusted knob
timeout_secs = 60
show = "badge"                  # hidden | badge | findings
min_chars = 400                 # answers shorter than this skip review under "substantial"
```

`Config.verification` becomes `scrutiny` with `#[serde(alias = "verification")]`; the struct
keeps `auto` as a deprecated alias for `work`. `substantial` means: the answer is ≥
`min_chars`, or the turn did work, or the turn is a plan-mode plan. Clarifying questions
(answers ending in `?` under 2 lines) are never reviewed.

### 2. Turn flow with answer scrutiny

```
run_loop → final draft text (not yet shown as answer)
  → critic(ask, draft, work_summary, context_digest)
      APPROVE            → release draft as the answer
      REVISE + findings  → hidden corrective turn → run_loop again (tools allowed)
                           → critic again … up to max_revisions → release
  → work scrutiny (existing iterate_verification) if the turn did work
```

**Streaming.** While `answers` is active for a turn, the model's prose streams as a new
event `AgentEvent::DraftText { delta }`, which the TUI renders exactly like `Thinking`:
dim, transient, and dropped when the answer begins. On APPROVE the agent emits the whole
draft as one `AssistantText`. On REVISE the revision streams as `DraftText` again until its
verdict. The REPL prints drafts only with `show = findings`. Net effect: the user sees text
forming (so it does not feel stalled) and the final answer replaces it.

**History hygiene.** The draft, the critic's findings and the corrective prompt are **not**
kept in `messages`: after release, the history holds the user ask, the tool exchanges and the
released answer only (same policy as thinking). Tool calls made during a revision pass stay,
because their results are real context. Session JSONL records what the history records.

**Critic prompt** (new `critic_prompt`, beside `check_prompt`):

> You review a draft answer before the user sees it. Compare it with THE ASK, the agent's
> actions this turn, and the evidence in tool results. Report only concrete defects: a
> requirement not met, a claim contradicted by the evidence, a wrong command or path, an
> unsafe suggestion, a question the user asked that is unanswered. Do not comment on style
> or length. If there are no such defects respond with exactly: APPROVE. Otherwise respond
> `REVISE` followed by a bulleted list of the defects.

Context digest: the last 4 user/assistant pairs truncated to 1,200 chars each, plus
tool-result heads from this turn (the same stubs `prune_old_tool_results` keeps). Cap the
whole critic request at 8k tokens; drop the digest first when over.

**Corrective prompt** (hidden user turn):

> A reviewer found these defects in your draft answer: … Write the corrected final answer.
> Use tools if you need to check something. Do not mention the review.

Parsing reuses `parse_verdict`'s lenient rules (`APPROVE`/`OK` with no bullets = approve;
anything with bullets = revise). Identical text after a revision = release.

### 3. Critic model resolution

`model` unset → the agent's current provider/model (so `/model` switches move the critic).
Set → resolved through `provider_factory::resolve` at setup and on `/scrutiny model`. If
resolution or the call fails, emit one notice per session (`critic kimi-k3.1-pro:cloud
unavailable; self-scrutiny`) and fall back. Critic calls go through the scheduler lane
(spec 01); a cloud critic uses one cloud slot, a different local critic uses the VRAM policy.
`one_shot_call` must start forwarding `Usage` so critic tokens count in the sidebar totals.

### 4. Visibility

| `show` | TUI | REPL |
|---|---|---|
| `hidden` | nothing; status line unchanged | nothing |
| `badge` (default) | status line `⚖ reviewing` while the critic runs; `⚖ 1 revision` dim one-liner after a revised answer; `✓ verified` for work as today | `[scrutiny] revised once` |
| `findings` | adds the critic's bullets as a collapsed dim cell | prints them |

Sidebar SESSION row `⚖ critic: kimi-k3.1-pro` when the critic differs from the main model.

### 5. Commands

- `/scrutiny` → prints the current state (mode, critic, show).
- `/scrutiny off | answers | work | all`.
- `/scrutiny model <alias|tag> | model clear`.
- `/scrutiny show hidden|badge|findings`.
- `/verify` unchanged (on-demand work review).
- `/config set scrutiny.model …` already works through the config-editing prompt.

### 6. Events

`AgentEvent::DraftText { delta }`, `ScrutinyStarted { kind: Answer | Work }`,
`ScrutinyVerdict { kind, approved: bool, revisions: u32, findings: String }`. Keep
`VerificationAttempt`/`VerificationReport` as the work-path events or map them onto these;
either way the TUI keeps `✓ verified`.

### 7. Plan mode

A plan is reviewed before it is presented (`answers` applies regardless of length). Cheapest
place to catch a wrong plan.

### 8. Phase 2: pre-action scrutiny (optional, separate flag)

`actions = "risky"`: in `auto` mode, before executing a `bash` call that no allow rule
matched, or an edit over N lines, ask the critic "is this command/edit safe and consistent
with the ask?" and block on REVISE with the findings returned to the model as a tool error.
Adds one critic call per risky action; off by default.

## Costs and expectations

- Every reviewed turn adds one critic call (seconds on cloud, longer on local) and one
  revision pass when findings arrive. Defaults review only substantial answers.
- Same-model critique catches omissions and self-contradictions reliably; knowledge errors
  rarely. A stronger critic catches more. Both are honest improvements, not guarantees.
- Local main + local critic on one GPU goes through the VRAM policy; expect the critic to
  evict the main model unless both fit.

## Files

- `config/schema.rs`: rename struct to `ScrutinyConfig` with aliases; new fields.
- `verifier.rs` → `scrutiny.rs`: `Critic::review_answer`, keep `check` for work.
- `agent/agent.rs`: `submit` orchestration; `DraftText` emission switch in `call_model`;
  history replacement; `/scrutiny` plumbing via `DriverCmd`.
- `agent/events.rs`: new events.
- `tui/app.rs`, `view.rs`: draft cell (reuse the thinking cell), status text, SESSION row,
  slash parsing. `cli/repl.rs`: prints.
- `docs/GUIDE.md`: replace "Learning & verification" with "Scrutiny".

## Tests

- Config: `[verification]` TOML still loads; `[scrutiny]` loads; both present → error.
- `substantial` gating: short answer skipped; long answer reviewed; question skipped; plan
  reviewed.
- APPROVE releases the draft unchanged; REVISE triggers exactly one hidden turn and the
  history ends `[ask, …tools…, final]` with no draft or corrective prompt.
- `max_revisions` respected; identical revision releases.
- Critic timeout / error → release, one notice.
- TUI: `DraftText` renders dim and is dropped when `AssistantText` arrives; `show = hidden`
  prints nothing extra.
- Critic `Usage` adds to totals.
- Scripted `--no-tui` run with a deliberately wrong draft (fixture provider) showing the
  revision.

## Open questions

1. Should `answers` default to `substantial` or `off` for first release? Recommendation:
   `substantial`, since the badge makes the latency legible.
2. Should the critic see the system prompt's PILOT/LESSONS sections? Recommendation: LESSONS
   yes (user rules are review criteria), PILOT no (too long).
