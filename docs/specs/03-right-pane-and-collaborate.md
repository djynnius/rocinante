# Spec 03: Right-pane framework and `/collaborate`

## Goal

A. The right pane becomes switchable: the current status sidebar is one pane among several.
B. `/collaborate` shows the crew: who is alive, what each member is doing, and a message feed
   where the user talks to a member with `@name …` and the member answers.
C. (Deferred) Members message each other to finish a task.

## What exists

- Fixed layout: transcript + input + status on the left, a 30-column borderless sidebar on
  the right when the terminal is wide enough (`tui/src/view.rs:43-57`, `app.rs:29-32`,
  `sidebar_visible()`). `/context` is a modal overlay (`context_open`), not a pane.
- Elm-style TUI: `App::update(Msg) -> Vec<Effect>` is pure and unit-tested; `lib.rs` executes
  effects; the agent runs in `driver.rs` behind `DriverCmd`.
- Subagents are ephemeral: `TaskTool::run` (`agent/subagent.rs:120-370`) builds an `Agent`
  per call, forwards its events as `ToolProgress { call_id: "task[name]" }`, returns its last
  message as the tool result, and drops it. `app.rs` tracks `running_agents` from those
  progress tags for the AGENTS sidebar section.
- Permission asks from subagents already bubble to the frontend through `ReplyRouter`.

Asking `@naomi what are you working on` therefore has nobody to answer today. B requires
persistent members.

## Part A: right-pane framework

```rust
pub enum RightPane { Status, Collaborate, Context }
pub fn right_pane_width(kind: RightPane, total: u16) -> u16 // Status 30; Collaborate max(44, 40%); Context 48
```

- `App.right_pane: RightPane` (default `Status`), `pane_scroll: usize`.
- Commands: `/pane status|collaborate|context`; `/collaborate` ≡ `/pane collaborate`;
  `/collaborate off` ≡ `/pane status`. `Esc` with no overlay open returns to `Status` if
  another pane is showing (Esc still cancels a running turn first).
- `view()` keeps one layout path: compute the pane width from the kind; hide the pane under
  the same minimum-width rule as today (`sidebar_visible()` becomes `pane_visible()`).
- Pane scrolling: `Shift+PgUp/PgDn` (the transcript keeps `PgUp/PgDn`). No focus model is
  needed because the input box stays the only text input.
- Move `/context` into the pane system optionally; keeping it as an overlay is fine for v1.

Tests: width math per kind and terminal size; command parsing; Esc precedence.

## Part B: crew — persistent members and `@name`

### 1. Core: `rocinante-core/src/crew/mod.rs` (manager pattern)

```rust
pub struct CrewManager { members: Mutex<HashMap<String, MemberHandle>>, cfg: CrewConfig, … }
struct MemberHandle { inbox: mpsc::Sender<CrewMsg>, status: watch::Receiver<MemberStatus>, task: JoinHandle<()> }

pub struct CrewMsg { pub id: Uuid, pub from: Addr, pub text: String, pub thread: Uuid,
                     pub reply: Option<oneshot::Sender<String>> }   // Some when `task` awaits it
pub enum Addr { User, Main, Member(String) }

pub enum MemberStatus { Idle, Working { step: String, since: Instant }, WaitingPermission, Queued(u32), WaitingSlot }
```

- A member is spawned lazily: on its first `task` call or first `@name` message. It owns an
  `Agent` built exactly as `TaskTool::run` builds one today (profile model, restricted tools,
  system prompt, `depth = 1`, `Mode::Normal`), plus the member's own event channel.
- The member loop: `inbox.recv()` → `agent.submit(text)` → send the reply to `from` as
  `CrewEvent::Message`. Messages arriving while a turn runs queue in order (`Queued(n)`).
  Members never run two turns at once (`submit` is `&mut self`).
- Memory: context persists across messages, so "what are you working on" and "what did you
  find" have answers. Per-member context hygiene reuses the agent's pruning and compaction
  (`Summarizer`), with `keep_tool_turns = 3`.
- Idle reaping: `crew.max_idle = "30m"` drops a member's `Agent` (its context) after
  inactivity; a later message respawns it fresh and the pane says so.
- Status comes from the member's event stream: `ToolCallStarted` → `Working { step }`,
  `PermissionRequested` → `WaitingPermission`, scheduler wait → `WaitingSlot`, `TurnFinished`
  → `Idle`. Published via `watch` so the pane never polls the model.
- Cancel: `@naomi !stop` interrupts the member's current turn (`Interrupter`) and clears its
  queue. Esc still cancels the main turn only.

### 2. `task` tool integration

With `crew.persistent = true` (default once shipped), `TaskTool::run` sends `CrewMsg {
from: Main, reply: Some(tx) }` to the member and awaits `rx`, so the main model's tool API is
unchanged and the member keeps its memory between delegations. Parallel fan-out to the *same*
member now serializes through its inbox (one member, one brain); the orchestrator is told
this in the task description: "parallel work needs different members". `/submodel` and
per-call `model` overrides apply at spawn; a later override with a different model respawns
the member (its context is summarized into the first message of the new one).

### 3. Routing user input

- Input starting with `@name ` (a known profile) routes to the crew (`Effect::CrewSend { to,
  text }`), not to the main agent, and is echoed in the pane feed as `you → naomi`. Unknown
  name → notice listing members. `@main` is not needed: plain input is the main agent.
- The reply lands in the pane as `naomi → you`. If the pane is not `Collaborate`, the
  transcript gets a one-line dim notice `naomi replied in the crew pane (/collaborate)`.
- Status fast path: `@naomi status` (or `?`) is answered by the manager from `MemberStatus`
  with no model call. Everything else goes to the model and queues if the member is busy.
- Permission asks from members bubble as today, tagged `[naomi]`.

### 4. Pane rendering

```
CREW                          FEED
⠙ naomi    grep src/… 00:42   you → naomi   what are you working on?
○ holden   idle               naomi → you   Tracing how /model switches reach TaskTool…
⏳ miller  waiting for slot   main → miller  Find last year's pricing for…
                              miller → main  (3 sources) …
tokens: naomi 12k · miller 8k
```

Roster on top (name, state glyph, current step or idle, elapsed), feed below (wrapped,
newest at bottom, scrollable, sender → recipient prefix, dim for main↔member traffic so the
user's own thread stands out). The REPL prints `[crew] naomi → you: …` lines.

### 5. Events and config

`AgentEvent::Crew(CrewEvent)` with `MemberSpawned { name, model }`, `Status { name, status
}`, `Message { from, to, text, thread }`, `MemberDropped { name, reason }`.

```toml
[crew]
persistent = true
max_idle = "30m"
```

### 6. Scheduler

Members acquire slots per request through spec 01. The roster's `waiting for slot` state is
what makes an over-subscribed fan-out legible instead of mysterious.

### 7. Sessions

v1: members do not survive restart; `-c` resumes the main agent only, and the pane shows the
roster empty. v2: write member transcripts as `crew/<name>` records in the session JSONL and
rebuild them on resume.

## Part C: member-to-member messaging (deferred)

- A `message` tool for members whose profile lists `can_message = ["holden", …]`; messages
  carry the `thread` id; `crew.max_hops = 6` per thread, after which the manager returns the
  thread to its opener (user or main) with a "needs a decision" note. Only the user and the
  main agent open threads. Every hop is visible in the feed.
- Why deferred: two local models conversing burn tokens and rarely converge; the robust
  pattern is hub-and-spoke (main agent or user coordinates, members report). Build C only if
  B shows a real need, and keep it opt-in per profile.

## Is it useful?

Yes for visibility and steering during long fan-outs (deep research, multi-file review, data
work by `avasarala`/`camina` that stops at decision gates: the user can answer the gate in
the pane instead of waiting for the main agent to relay). The persistent memory also makes
delegation cheaper: a second `task` to `naomi` about the same area skips re-exploration.
Agent-to-agent chat is the least valuable piece.

## Files

- new `core/src/crew/mod.rs`; `agent/subagent.rs` gains the persistent path; `events.rs`.
- `tui/app.rs`: `RightPane`, `@` routing, feed/roster state, slash parsing, scroll.
  `tui/view.rs`: `draw_pane` dispatch, collaborate renderer. `tui/lib.rs`: `Effect::CrewSend`.
- `cli/setup.rs`: build `CrewManager`, pass to `TaskTool`. `cli/repl.rs`: `@` routing + prints.
- `docs/GUIDE.md`: "The crew pane".

## Tests

- Pane width per kind; `/collaborate` toggles; Esc precedence.
- `@naomi text` → `Effect::CrewSend`; unknown name → notice; `@naomi status` → no effect
  beyond a status line.
- Manager: spawn-on-first-message; queueing while busy; reply routing to `User` vs `Main`
  (oneshot answered); idle reaping; `!stop` cancels and clears the queue.
- `task` with `persistent = true` returns the member's reply and the member's `messages` grow
  across two tasks.
- Feed rendering wraps and orders; roster glyphs per status.
- Scripted `--no-tui`: two `@naomi` messages show memory of the first.

## Open questions

1. Should `@name` input also be visible in the main transcript (as a dim line) so scrollback
   stays complete? Recommendation: yes, one dim line each way.
2. Should the main agent be told about user↔member conversations? Recommendation: no by
   default; the user relays, or asks the member to "report to main" (which sends a
   `CrewMsg` to `Main` that is injected as a notice before the next main turn).
