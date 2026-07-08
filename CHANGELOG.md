# Changelog

All notable changes to Rocinante are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
adheres to [Semantic Versioning](https://semver.org/).

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

[0.3.1]: https://github.com/djynnius/rocinante/releases/tag/v0.3.1
[0.3.0]: https://github.com/djynnius/rocinante/releases/tag/v0.3.0
[0.2.0]: https://github.com/djynnius/rocinante/releases/tag/v0.2.0
[0.1.2]: https://github.com/djynnius/rocinante/releases/tag/v0.1.2
[0.1.1]: https://github.com/djynnius/rocinante/releases/tag/v0.1.1
[0.1.0]: https://github.com/djynnius/rocinante/releases/tag/v0.1.0
