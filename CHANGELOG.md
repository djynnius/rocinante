# Changelog

All notable changes to Rocinante are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- **Auto-pull of Ollama cloud stubs.** Using a `:cloud` tag that isn't
  present on this machine yet (e.g. `--model minimax-m3:cloud` on a fresh
  install) now just works: if the server 404s the unpulled tag, its
  ~300-byte stub is pulled and the request retried once; and after the first
  successful use the stub is ensured (once per session) so `/api/tags` — and
  the model picker — list the tag from then on. Cloud tags only; non-cloud
  models are never auto-downloaded. Requires `ollama signin` on the machine.

### Fixed
- Fresh machines (commonly Windows) appeared to "hide" cloud models: Ollama
  only lists a cloud tag after its stub has been pulled once per machine, so
  the picker showed on-device models only. Free-typing the tag now works
  first try and gets it listed; the model-not-found error includes the
  `ollama pull` hint.

## [0.17.1] — 2026-08-10

### Fixed
- `@`-file autocomplete now shows forward-slash paths on Windows (the path
  list normalizes the OS separator), fixing a Windows-only test failure.

## [0.17.0] — 2026-08-10

### Added
- **`@` file references** — type `@` in the TUI and a gitignore-aware
  autocomplete lists matching project files and folders (↑↓ to move,
  Tab/Enter to insert, Esc to close). Inserts the path as text; the agent
  reads the file on demand, so context stays lean. The list is built once at
  startup.
- **Question queue** — a prompt typed while a turn is running is now queued
  instead of interrupting. When the turn finishes, Rocinante asks
  `run next queued question? [y/n]` for each queued item in order: `y` runs
  it, `n` skips, Esc pauses the prompts while keeping the queue.
- **`[verification] max_iterations`** (default 3).

### Changed
- **Verification now self-corrects instead of just reporting.** When the
  post-turn checker finds gaps, it feeds them back to the agent as a
  corrective pass and re-checks, up to `max_iterations` times, then certifies
  done. A clean first pass stops immediately. `/verify` drives the same
  iterate-and-fix loop and can be stopped with Esc.
- **Plan mode thinks harder.** It now anticipates likely problems *and their
  solutions* before proposing anything, asks **up to 5** clarifying questions
  **once** (no repeated interrogation), and the plan it presents names the
  pitfalls it foresees and how it will handle each.

## [0.16.1] — 2026-08-10

### Fixed
- Untrack and gitignore `.DS_Store` (it had been committed by accident).

## [0.16.0] — 2026-08-08

### Added
- **`/uninstall`** — remove Rocinante from the machine. Two-step by design:
  bare `/uninstall` only previews; `/uninstall confirm` deletes the binary
  and exits, and `--purge` also wipes `~/.rocinante` (data is kept by
  default). Per-project `.rocinante/` folders are never touched. Homebrew
  and Scoop installs are refused with the right `brew`/`scoop uninstall`
  command; a cargo install is removed with a `cargo uninstall` note.
  (Unix unlinks the running binary; Windows schedules a detached delete.)

## [0.15.0] — 2026-08-08

### Added
- **Learned rules & preferences** — a global `~/.rocinante/LESSONS.md` of
  the user's do/don't rules and preferences, injected into every session
  (shown in `/context`). Populated by **`/remember <rule>`** and by a
  conservative session-end capture that records a rule only on a clear
  signal (a stated preference, a generalizable correction, a recurring
  mistake) — never invented. `[learning]` config (enabled,
  update_every_turns, model).
- **Auto-gated verification** — after a substantial turn (one that edited
  files or ran a command) a background checker compares the result against
  the original ask and posts a non-blocking `✓ matches`/`⚠ gaps` notice;
  **`/verify`** runs it on demand. Optional `[verification] check_command`
  runs the project's tests/build and folds pass/fail into the verdict.
  `[verification]` is stripped from untrusted project config (its
  `check_command` executes a shell command outside the permission engine).

### Changed
- **Markdown tables render as a full bordered box** in the TUI — an outer
  border, `│` column dividers, and a `─` rule between every row (was: only
  column separators and a single header rule).

## [0.14.0] — 2026-08-07

### Changed
- **Clipboard works in the TUI**: mouse capture is dropped so you can
  select transcript text and copy it with the OS shortcut (native terminal
  selection), and bracketed paste is enabled so a multi-line clipboard
  paste lands in the input as one chunk instead of a keystroke stream that
  submitted on the first newline. Trade-off: the mouse wheel no longer
  scrolls the alt-screen — use PgUp/PgDn/arrows.

## [0.13.0] — 2026-08-07

### Added
- **`/clear`**: reset the conversation, keeping the system prompt, so the
  next turn starts fresh (the context gauge resets and `-c` resume picks up
  from the cleared state via a session `Clear` record). **`/clear --all`**
  additionally wipes `.rocinante/BRAINBOX.md` and strips the injected memory
  head from the live prompt, so no prior-session memory carries forward.
  Available in the TUI and the REPL.

## [0.12.0] — 2026-08-07

### Added
- **`/context` dashboard**: a scrollable overlay breaking down what fills
  the context window — system-prompt categories (base, skills index,
  delegation briefing, PILOT, memory), tool schemas, conversation messages,
  and free space, each with tokens and %, plus a per-skill standing-cost
  list, the agents, and the BRAINBOX memory head. The grand total uses the
  provider's real prompt-token count when a turn has run; splits are
  estimates.

### Changed
- **Leaner standing context** (sent every request): the skills index now
  lists each skill with a **short one-line trigger** instead of its full
  trigger-rich description (~2k tokens saved with 40 built-ins; the full
  body still loads when the `skill` tool activates it), and **BRAINBOX**
  injects only its **Goals + Next steps** head with a pointer — the model
  reads the full `.rocinante/BRAINBOX.md` on demand (~1k tokens saved).
  PILOT.md stays fully injected (authoritative project rules).

## [0.11.0] — 2026-08-05

### Changed
- **Thinking text is now transient in the TUI**: the model's grey `∴`
  reasoning streams in while it thinks, then disappears the moment real
  output begins (first assistant token, a tool call, or turn end) instead
  of lingering in the transcript. It was always display-only; now it's
  cleaned up. (The `--no-tui` REPL streams forward-only and is unchanged.)
- **Markdown tables render as aligned columns**: `|…|` rows with a
  `|---|` separator are laid out with each cell padded to its column's max
  width, a rule under the header, and `│` separators — inline styling
  inside cells preserved, and the table shrunk/capped to always fit the
  pane. A pipe block without a separator row falls back to plain text.

## [0.10.0] — 2026-08-05

### Added
- **`github-cli` built-in skill** (built-ins 39 → 40): a weak-model
  checklist for the `gh` CLI — open/review PRs, create/comment issues,
  watch CI/Actions runs, view releases, fork/clone, and script GitHub data
  with `--json … -q`. Complements `git-rescue` (local history) and enforces
  confirmation before outward-facing actions (push to default branch, merge,
  publish a release).

## [0.9.1] — 2026-08-05

### Fixed
- **`/model` no longer lists an aliased model twice**: when an alias points
  at an Ollama tag (e.g. `kimiko` → `kimi-k2.7-code:cloud`), only the alias
  is shown; the raw tag is hidden. Unaliased tags keep their own names, and
  the dedup is scoped per-provider so it never hides an identically-named
  tag on a different provider.

## [0.9.0] — 2026-08-05

### Security
- **Workspace trust for project config**: a project-local
  `.rocinante/config.toml` is now untrusted by default — its
  security-relevant keys (`providers`, `mcp`, `lsp`, `agents`,
  `permissions.allow`, `skills.extra_dirs`, `defaults.mode`) are dropped
  so a cloned repo can't spawn processes, exfiltrate the conversation,
  grant tools, or auto-approve at startup. Harmless keys (`models`,
  `permissions.deny`, `brainbox`, `context`, …) still apply. A startup
  notice lists what was dropped; `/trust` opts a project in (remembered in
  `~/.rocinante/trust.toml`; restart to apply). User-wide config is always
  trusted.
- **Gemini API key** moved from the URL query string to the
  `x-goog-api-key` header, so it can't leak through error messages (query
  strings are not redacted), proxy logs, or surfaced errors.
- **Session transcripts** are created `0600` and `.rocinante/.gitignore`
  is written (`sessions/`, `state.toml`, `*.tmp`) so conversation history
  — which can include approved-secret tool output — stays owner-only and
  out of git.
- **Terminal-escape hardening**: the plain REPL strips control characters
  from model output and tool results before printing, so fetched web
  content can't drive the terminal (e.g. OSC 52 clipboard writes). The TUI
  was already safe.
- **Self-update scratch dir** uses an unpredictable name with exclusive
  create and `0700`, closing a local TOCTOU on the extracted binary.

### Added
- **Instant quit**: the final BRAINBOX.md update is skipped when a
  background refresh already covers every turn — quitting no longer waits
  on a model call unless there is genuinely something new to record.
- **Tool-result pruning**: tool outputs older than the last
  `keep_tool_turns` user turns (default 3) are replaced in context by a
  one-line stub — tool name, size, first line — while the full output
  stays in the session JSONL. Deterministic, instant, survives `-c`
  resume. `[context] keep_tool_turns = 0` disables.
- **Proactive compaction**: at 60% of the context budget old turns are
  summarized in the background and spliced in at the next turn boundary;
  the blocking compaction at 80% becomes a rare fallback. A stale
  background summary (superseded by a manual `/compact`) is discarded, and
  any reload failure falls back to the old context untouched.
- **`[context]` config section**: `model` picks a dedicated (cheaper)
  summarization model with warn-and-fallback to the main model;
  `keep_tool_turns` tunes pruning.
- **PILOT.md staleness check**: at startup, if the README or a root build
  manifest is newer than `.rocinante/PILOT.md`, a notice suggests
  re-running `/init` (mtime comparison only — advisory, never rewrites).
- **`/update` self-update**: user-invoked only — checks GitHub's latest
  release, downloads the platform binary, verifies SHA-256 against the
  published SHA256SUMS, and atomically swaps the executable (restart to
  run it). Every failure mode leaves a runnable binary; Homebrew/Scoop
  installs are deferred to `brew upgrade` / `scoop update` instead of
  being overwritten. No automatic checks, ever.

### Changed
- Compaction summaries now fill a richer template (adds CONSTRAINTS &
  GOTCHAS) with explicit orders: keep exact paths/commands/errors
  verbatim, never reproduce tool-output bodies, drop dead ends.
- **`/config` command**: `/config add alias kimiko for kimi-k2.7-code:cloud
  with num_ctx 256000` submits a canned task and the agent edits
  `~/.rocinante/config.toml` for you (the absolute path is computed
  per-OS, so it works on Windows too); bare `/config` summarizes the
  current config read-only. Available in the TUI and the REPL.
- **`rocinante-config` built-in skill** (built-ins 38 → 39): the `/config`
  command's brain — the full config schema ([models], [defaults],
  [providers], [agents], [permissions], [skills], [mcp], [lsp],
  [brainbox]) as copy-paste TOML snippets with a hard rule set: never
  store API keys (env-var names only), read before editing, surgical
  edits, verify by re-reading.
- **Hot-refreshed `/model`**: every `/model` use (picker or direct switch)
  re-reads the layered config from disk and re-discovers the catalog, so
  aliases added mid-session appear immediately — no restart. Reload
  failures (invalid TOML, unreachable Ollama, >3s discovery) fall back to
  the startup snapshot with a notice; switching to an alias now also
  points the context gauge at its `num_ctx`.

## [0.8.0] — 2026-07-27

### Added
- **Document skills** (built-ins 34 → 38): `docx` (pandoc and python-docx
  routes, tables from DataFrames, edit-under-new-name), `xlsx` (openpyxl
  formatting — number formats, frozen headers, autofilters, formulas,
  charts — plus the pandas fast path), `pptx` (python-pptx decks with the
  one-idea-per-slide discipline, images, tables, speaker notes), and
  `pdf` (route table: quarto for reports, pandoc/weasyprint for prose,
  LibreOffice headless for office-file conversion, reportlab for
  programmatic layout, pdfplumber extraction pointer). All verify their
  output by reopening it and never overwrite input files.

## [0.7.0] — 2026-07-27

### Added
- **Efficiency-aware delegation**: Rocinante inventories every switchable
  model at startup (local sizes and parameter counts from Ollama's
  /api/tags, cloud aliases from config) and briefs the main agent with a
  "models available for delegation" section — and the `task` tool gains an
  optional `model` parameter so a subagent can run on the cheapest
  adequate model (`task[naomi @ qwen3:8b]`). Bad overrides fall back to
  the profile's model instead of failing the delegation.
- **`/effort low|medium|high`** (default **high**; `[defaults] effort`
  configures it): one reasoning knob mapped per provider — Anthropic
  thinking budgets (off / 8k / 16k, with max_tokens headroom), OpenAI
  `reasoning_effort`, Ollama gpt-oss think levels (activation on local
  models stays explicit via `/think`; low always forces thinking off).
  The `∴` indicator in the sidebar, status line, and landing box now
  shows the tier.
- **`/submodel <name>`** pins EVERY subagent run to one model, enforced
  in the task tool itself — it beats profile models and the
  orchestrator's per-call overrides, so "run all subagents with X" is a
  guarantee, not a request. `/submodel clear` releases; the active pin
  shows in the sidebar SESSION panel. Available in both frontends with
  the same validation as `/model`.
- Popups (permission modal, `/model` picker) now sit on a lighter
  `#333333` panel so they read as a raised layer, not just a border.

### Changed
- **Auto mode is now truly hands-off**: commands and subagent spawns run
  without prompting, not just edits. Explicit `[permissions] deny` rules
  remain the guardrail and always block, in every mode. Put protections
  there (e.g. `Bash(git push --force:*)`, `Bash(rm -rf:*)`).
- **Plan mode interrogates before planning**, Claude-CLI style: the agent
  restates the request, investigates read-only, lists assumptions and grey
  areas, asks numbered clarifying questions and waits for answers, then
  presents a file-by-file plan with a verification step and offers to
  execute it in auto mode. The TUI plan-ready notice now points at the
  auto handoff.

## [0.6.0] — 2026-07-27

### Added
- **Web research**: a `web-research` built-in skill lets any model browse
  the internet through the `bash` tool — DuckDuckGo HTML search with a
  stdlib-only parser, page fetch and text extraction, JSON APIs, file
  downloads, and verify-and-cite rules. `miller` (the researcher) gains
  `bash` + `skill` and loads it, so deep-research fan-outs can truly
  search the web.
- **Lakehouse workflow**: `medallion-architecture` (a folder of mixed
  files — spreadsheets, CSV, JSON, Parquet, Word docs — treated as an
  immutable bronze layer; instruction-driven star-schema design with ASK
  gates on grain and schema; silver built as Parquet with reconciliation
  counts) and `ducklake` (DuckLake v1.0: attach, transactional tables,
  snapshots, time travel — syntax verified against the live docs).
  `avasarala` routes to both.
- **Quarto**: `quarto` skill for reproducible .qmd reports, slides, and
  parameterized documents — the natural deliverable step after the
  analysis skills.
- **NLP**: `spacy` (tokenization, NER, lemmas, dependency parses,
  batching, displacy-to-file) and `nltk` (tokenize/clean, stem vs
  lemmatize, frequencies and collocations, VADER sentiment) — each led
  by the model/data-download step that usually strands small models.
- **Recommender systems**: `recommender-systems` — association rules
  with Apriori/FP-Growth (mlxtend) and Eclat (pyECLAT), support/
  confidence/lift with deterministic gates, basket one-hot prep, and
  item-based collaborative filtering evaluated with precision@k against
  a popularity baseline. `camina` routes recommendations and NLP work.
  Built-in skill count: 27 → 34.

### Changed
- Sidebar wordmark is now uniformly letter-spaced — `R O C I N A N T E`
  — instead of a double gap between ROCI and NANTE.

### Fixed
- De-flaked `env_provider_injection`: the test mutated real environment
  variables and raced parallel `load_from` tests; the env lookup is now
  injected so tests never touch the process environment.

## [0.5.0] — 2026-07-26

### Added
- **Skills everywhere**: discovery now scans Claude Code locations
  automatically — `~/.claude/plugins` (deep walk through plugin caches),
  `~/.claude/skills`, and `<project>/.claude/skills` — alongside the native
  `.rocinante` tiers and `[skills] extra_dirs`. Later tiers shadow earlier
  by name; an existing Claude Code install's skills just work, zero config.
- **Install/create skills mid-session**: the `skill` tool rescans all
  directories when asked for a name it doesn't know, so a skill installed
  or created during a session activates without a restart. A new
  `skill-maker` built-in teaches the agent to author skills, install them
  from git or local folders (with a review-before-install rule), and
  troubleshoot loading.
- **Built-in skill library grown from 7 to 27**, all written in a
  weak-model-hardened checklist format (numbered steps, Rules sections,
  copy-paste snippets, deterministic decision tables) so small local models
  can follow them:
  - Data science: `exploratory-data-analysis`, `statistical-modeling`,
    `sql-analytics`, `data-wrangling`
  - Machine learning: `ml-preprocessing`, `ml-modeling`, `ml-evaluation`
  - Tooling: `git-rescue` (safe recovery + recommended deny rules),
    `duckdb`, `ggplot`, `sqlalchemy`, `flask`, `vuejs`, `d3js`,
    `mermaidjs`, `lxc`, `ollama`, `postgresql`
  - `frontend-design`, vendored from anthropics/skills (Apache-2.0)
- **Two new crew members**: `avasarala` (data scientist — EDA, statistics,
  SQL, wrangling) and `camina` (ML engineer — preprocessing, modeling,
  evaluation). Both are write-capable, load the matching skills, and stop
  at decision gates with a recommendation instead of guessing. Subagent
  profiles can now list `"skill"` in `tools` to get the skill tool plus the
  skill index.
- **Prompt history**: Up/Down in the input box recalls previous prompts
  shell-style, restoring the in-progress draft on the way back down.
- **`/model` picker**: `/model` opens a scrollable overlay — arrows to
  move, Enter to switch, Esc to close — instead of printing a text listing.
  `/model <number|name|provider/model>` still switches directly.
- **Wrapping, growing input box**: the input character-wraps and expands
  (up to 8 rows, then scrolls with the cursor kept visible) in both the
  chat view and the landing screen — no more horizontal overflow.

### Changed
- Permission modal: borders in brand magenta, long diff lines wrap instead
  of truncating, and overflowing detail scrolls with
  Up/Down/PageUp/PageDown (`y`/`a`/`n` unchanged).
- Sidebar shows **your** agents and skills by default: built-in crew
  agents appear only while running (spinner) or after acting this turn
  (✓); built-in skills are hidden from the list (still fully usable).
- `/loop` is now discoverable: a landing-screen tip and a `/loop` hint
  next to `/model` and `/think`.
- The original built-in skills' companion prompts and the new library were
  audited for weak local models (glm-class): judgement calls replaced with
  deterministic thresholds, exact function names and JSON tool calls
  spelled out, python3/Agg/savefig fallbacks everywhere.

## [0.4.3] — 2026-07-08

### Changed
- Sidebar brand polish: the `ROCINANTE` wordmark is now bold and
  letter-spaced so it reads larger than the body text, and the three cyan
  rules collapse into a single tight triple-bar rule (`≡`) — the lines sit
  close together like Crush's diagonal strokes instead of a full row apart.
- Mode colors recolored: `NORMAL` #90FCF9, `AUTO` #FF5964, `PLAN` #CB04A5.
  Badge text now auto-picks black or white by background luminance, so every
  mode stays legible (white on the darker Plan magenta).

## [0.4.2] — 2026-07-08

### Changed
- Chat view breathes: a small outer margin keeps the transcript, input box,
  status line, and sidebar from hugging the terminal edges, and the query
  input now pads its text a column off the border. The transcript wrap width
  tracks the new margin so scrolling and wrapping stay exact. Landing screen
  and permission modal are unchanged (their edge-anchored composition is
  deliberate).

## [0.4.1] — 2026-07-08

### Changed
- Sidebar refinement: the divider line is replaced by a whitespace gap
  (cleaner, more modern, matching OpenCode/Crush), and the pane now leads
  with a two-tone `ROCINANTE` brand logo (magenta + cyan) over three cyan
  rules.

## [0.4.0] — 2026-07-08

### Added
- **Markdown rendering** in the TUI transcript: `**bold**` (coral), `*italic*`,
  `` `code` `` and fenced blocks (blue), `# headers` (bold), and
  `[links](url)` (underlined cyan) now render styled instead of showing raw
  syntax. Streaming-safe — half-typed markers render literally until closed.
- **First-run model picker**: on first interactive launch, choose from your
  Ollama models (local + signed-in cloud tags) and any API providers whose
  key is set. The choice is remembered globally in `~/.rocinante/state.toml`
  and becomes the default next time; `/model` switches update it. No more
  hardcoded default model — non-interactive use without a chosen model or
  `--model` gives a clear "select a model" error.

### Changed
- Landing wordmark recolored: `ROCI` magenta (#F433AB), `NANTE` cyan
  (#00B4D8). User prompts now show a cyan `▌` bar instead of `> `.

## [0.3.1] — 2026-07-08

### Changed
- Sidebar AGENTS section now shows agents **running right now** with an
  animated spinner and a live instance count (`⠙ miller ×4`) — so a
  deep-research fan-out of parallel subagents is visible as it happens. A
  finished-this-turn agent shows `✓`, idle shows `○`.

## [0.3.0] — 2026-07-08

### Added
- Built-in crew: six read-only specialist subagents ship by default, named
  after the Rocinante's crew — `naomi` (explorer), `miller` (researcher),
  `alex` (planner), `bobbie` (reviewer), `amos` (debugger), `holden`
  (oracle). They appear in the `task` tool with zero config; repoint any to
  a stronger model with `[agents.<name>] model = …`, or disable with
  `[defaults] builtin_agents = false`.
- Built-in skills embedded in the binary: `deep-research` (fans out parallel
  crew subagents, verifies, synthesizes), `code-review`, `debugging`,
  `writing-tests`, plus research-writing skills `proof-reading`,
  `plagiarism-check`, and `peer-review`. A user SKILL.md of the same name
  overrides; disable all with `[defaults] builtin_skills = false`.
  So "do deep research" now spins up a coordinated multi-agent investigation
  out of the box.

## [0.2.0] — 2026-07-08

### Added
- Redesigned TUI. A landing screen on launch: a two-tone pixel wordmark, a
  centered "Ask anything…" input carrying the mode + model line, keyboard
  hints, a rotating tip, and a `~`/version footer — it dissolves into the
  chat view the moment you type.
- A live right sidebar (terminal width ≥ 96 cols) tracking model and mode,
  token totals with a context-usage gauge, configured agent profiles that
  light up while a subagent runs, available skills, and session state
  (active loop, MCP tool count, LSP readiness). Below 96 cols everything
  folds back into the status line.

## [0.1.2] — 2026-07-08

### Changed
- Release pipeline: the Intel-Mac binary is cross-compiled on Apple
  Silicon runners — `macos-13` Intel runners are scarce enough that both
  prior release attempts stalled in their queue (v0.1.1 never published).

## [0.1.1] — 2026-07-08 *(never published — stalled in the Intel-Mac runner queue)*

### Fixed
- Linux release builds (x86_64 and aarch64 musl): the pinned toolchain was
  missing the cross-compilation target on CI, so v0.1.0 never published
  Linux binaries.

### Added
- `/compact` — manual context compaction in both frontends.
- Plan-mode exit flow: a completed plan-mode turn offers execute-in-normal
  or auto inline (REPL) / a switch hint (TUI).
- Cross-platform shell-tool tests that exercise the Windows command path
  in CI (echo, exit codes, timeout kill, cancellation).

## [0.1.0] — 2026-07-08 *(partial release — Linux binaries missing; superseded by 0.1.1)*

First release: a complete terminal coding agent.

### Core agent
- Agent loop with core tools — `read`, `write`, `edit` (exact-match with
  whitespace-tolerant fallback), `bash`, `grep`/`glob` (embedded ripgrep
  engine, `.gitignore`-aware) — driven by any configured model.
- Permission modes: `normal` (ask), `auto` (auto-approve edits), `plan`
  (read-only), with Claude-Code-style allow/deny rules
  (`Bash(cargo test:*)`); explicit deny always wins.
- Tool-call repair pipeline for local models: prose-scraping of malformed
  calls, JSON-Schema validation with corrective feedback, and Ollama
  constrained-decoding as a last resort.
- Sessions: append-only JSONL transcripts, `-c/--continue` resume,
  automatic context compaction with structured summaries, harness-side
  token accounting with explicit `num_ctx` (defeats silent truncation).
- Parallel execution: read-only tool calls and subagent delegations from
  one message run concurrently with deterministic result ordering; edits
  and commands stay sequential.

### Models and providers
- Providers: Ollama (native API — `num_ctx`, `keep_alive`, `think`,
  structured outputs), OpenAI-compatible, Anthropic, and Gemini; cloud
  providers auto-activate when their API-key env var is set.
- `/model` hot-switching with conversation context preserved; `--model
  ollama` auto-discovers every tag the local server offers.
- Extended thinking: `/think on|off` (Ollama `think` flag, Anthropic
  thinking budget), streamed dim and never stored in context.

### Multi-agent
- `task` tool with config-defined subagent profiles (model, toolset,
  system prompt, turn cap); permission asks bubble to the parent; depth
  cap; VRAM gate serializes cross-model local calls; write-capable
  subagents serialize while read-only scouts run parallel.

### Ecosystem
- MCP client (spec 2025-11-25 via the official `rmcp` SDK): stdio and
  streamable-HTTP servers, tools exposed as `mcp__<server>__<tool>` behind
  the standard permission system.
- LSP integration: lazy per-project language servers (rust-analyzer,
  typescript-language-server, basedpyright/pyright, gopls built in),
  automatic post-edit diagnostics inline in tool results, and an `lsp`
  tool for definition/references/hover/symbols.
- Agent Skills: SKILL.md-compatible discovery (including
  `~/.claude/skills`) with three-tier progressive disclosure.

### Workflow
- `/init` writes `.rocinante/PILOT.md` (project instructions, injected
  every session); `BRAINBOX.md` living memory refreshes in the background
  and at session end for cross-session continuity.
- `/commit`: agent-driven atomic commits; colored unified-diff previews in
  every edit/write permission prompt.
- `/loop <interval> <prompt>` recurring prompts; `/mode`; `/model`;
  `/think`; `/compact` (manual context compaction).
- Plan-mode exit flow: when a plan-mode turn completes, the REPL offers
  execute-in-normal/auto inline and the TUI surfaces a switch hint.

### Interfaces and distribution
- ratatui TUI (default on a TTY) with streaming markdown, tool cards,
  permission modals with diff bodies, mode cycling, token gauge — plus a
  plain REPL (`--no-tui`).
- One-line installers with SHA-256 verification: `install.sh`
  (Linux/macOS, POSIX) and `install.ps1` (Windows); release pipeline
  builds five targets (Linux x86_64/aarch64-musl, macOS x86_64/aarch64,
  Windows x86_64), publishes `SHA256SUMS`, and smoke-tests both installers
  on all three OSes.

[0.16.1]: https://github.com/djynnius/rocinante/releases/tag/v0.16.1
[0.16.0]: https://github.com/djynnius/rocinante/releases/tag/v0.16.0
[0.15.0]: https://github.com/djynnius/rocinante/releases/tag/v0.15.0
[0.14.0]: https://github.com/djynnius/rocinante/releases/tag/v0.14.0
[0.13.0]: https://github.com/djynnius/rocinante/releases/tag/v0.13.0
[0.12.0]: https://github.com/djynnius/rocinante/releases/tag/v0.12.0
[0.11.0]: https://github.com/djynnius/rocinante/releases/tag/v0.11.0
[0.10.0]: https://github.com/djynnius/rocinante/releases/tag/v0.10.0
[0.9.1]: https://github.com/djynnius/rocinante/releases/tag/v0.9.1
[0.9.0]: https://github.com/djynnius/rocinante/releases/tag/v0.9.0
[0.8.0]: https://github.com/djynnius/rocinante/releases/tag/v0.8.0
[0.7.0]: https://github.com/djynnius/rocinante/releases/tag/v0.7.0
[0.6.0]: https://github.com/djynnius/rocinante/releases/tag/v0.6.0
[0.5.0]: https://github.com/djynnius/rocinante/releases/tag/v0.5.0
[0.4.3]: https://github.com/djynnius/rocinante/releases/tag/v0.4.3
[0.4.2]: https://github.com/djynnius/rocinante/releases/tag/v0.4.2
[0.4.1]: https://github.com/djynnius/rocinante/releases/tag/v0.4.1
[0.4.0]: https://github.com/djynnius/rocinante/releases/tag/v0.4.0
[0.3.1]: https://github.com/djynnius/rocinante/releases/tag/v0.3.1
[0.3.0]: https://github.com/djynnius/rocinante/releases/tag/v0.3.0
[0.2.0]: https://github.com/djynnius/rocinante/releases/tag/v0.2.0
[0.1.2]: https://github.com/djynnius/rocinante/releases/tag/v0.1.2
[0.1.1]: https://github.com/djynnius/rocinante/releases/tag/v0.1.1
[0.1.0]: https://github.com/djynnius/rocinante/releases/tag/v0.1.0
