# Contributing to Rocinante

## Building

Rust 1.96+ (pinned in `rust-toolchain.toml` — rustup picks it up
automatically). Then:

```sh
cargo build            # debug build
cargo test             # 135+ unit tests, no network needed
cargo clippy --all-targets   # must be zero warnings
cargo fmt --all --check
```

CI (`.github/workflows/ci.yml`) runs exactly those four on Linux, macOS,
and Windows — green on all three is the bar for merging.

## Architecture in one minute

Four crates, strict dependency direction `cli → {tui, core, providers}`:

- **`rocinante-core`** — the product: agent loop (`agent/agent.rs`, a turn
  state machine), tools + permission engine, sessions/compaction, skills,
  brainbox memory, MCP client (`mcp/`), LSP client (`lsp/`). Pure library:
  frontends talk to it only through an `AgentEvent` broadcast channel out
  and a reply channel in — that surface is the contract, and a future HTTP
  server wraps the same channels.
- **`rocinante-providers`** — the `Provider` trait and four hand-rolled
  wire implementations (Ollama NDJSON, OpenAI/Anthropic SSE, Gemini).
- **`rocinante-tui`** — Elm-style ratatui app (`app.rs` is pure and fully
  unit-tested; `view.rs` renders; the agent lives in a driver task).
- **`rocinante-cli`** — the binary: arg parsing, shared setup, plain REPL.

Run a debug build against a live model:
`./target/debug/rocinante --no-tui --model <tag>` (pipe input for scripted
tests — every feature in this repo was verified that way).

## Conventions

- Comments only for non-obvious constraints — never to narrate what code
  does or why a change is correct.
- Every external wire format gets a lenient parser and a fixture test;
  local models drift, and the repair pipeline is the norm, not the
  exception.
- Tool count is a budget: a ~30B local model loses calling accuracy with
  every added schema. Prefer one tool with an action enum over five tools.
- External-process integrations (MCP, LSP) follow the manager pattern:
  session-lifetime handles kept alive by the frontends, lazy spawn,
  degrade-to-warning, graceful shutdown.
- Exact-pin dependencies that break between minors (`rmcp`, `lsp-types`).

## Pull requests

Keep them atomic; state the user-visible behavior change first. New
features need unit tests for the pure logic and, where a live model is
involved, a scripted `--no-tui` verification in the PR description.
