# Changelog

All notable changes to Rocinante are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/).

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
