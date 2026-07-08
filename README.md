# Rocinante

An ironman suit for local models. Rocinante is a terminal coding agent that
runs a local Ollama model (default: `gemma4:31b`) as its main brain and lets
it **delegate subtasks to other models** — local or cloud — through
configurable subagent profiles. Claude Code-style operation (tools, skills,
permission modes, sessions), Opencode-style terminal experience, written in
Rust.

> *Rocinante: Don Quixote's horse — an ordinary steed carrying outsized
> ambitions, which is exactly the point.*

## Features

- **Agent loop** with core tools: `read`, `write`, `edit`, `bash`, `grep`,
  `glob` (ripgrep engine built in, `.gitignore`-aware)
- **Delegation**: a `task` tool exposing subagent profiles from your config —
  the main agent decides when to hand work to a scout model, a cloud
  heavyweight, or any other configured specialist
- **Providers**: Ollama (native API: `num_ctx`, `keep_alive`, structured
  outputs), plus OpenAI-compatible, Anthropic, and Gemini for subagents or as
  the main model
- **Modes**: `normal` (ask before edits/commands), `auto` (auto-approve
  edits), `plan` (read-only) — with allow/deny rules like `Bash(cargo test:*)`
- **Tool-call repair**: local models drift; malformed calls are scraped from
  prose, schema-validated, bounced back with errors, and as a last resort
  forced through Ollama's constrained decoding
- **Sessions**: append-only JSONL transcripts, `-c/--continue` to resume,
  automatic context compaction with structured summaries
- **Skills**: SKILL.md-compatible (reads `~/.claude/skills` too) with
  progressive disclosure
- **VRAM-aware**: cross-model local subagent calls are serialized so two big
  models don't evict each other

## Quick start

```sh
# Prereqs: Rust 1.96+, Ollama running with a tool-capable model
cargo build --release

# One-shot smoke test
./target/release/rocinante ask "hello"

# Interactive agent in the current project
./target/release/rocinante

# Resume where you left off
./target/release/rocinante -c
```

## MCP servers

Rocinante is an MCP client (spec 2025-11-25, via the official `rmcp` SDK).
Configured servers' tools appear to the agent as `mcp__<server>__<tool>`,
behind the same permission system as everything else — allow specific ones
with rules like `allow = ["mcp__github__search_repositories"]`. Subagent
profiles can list MCP tool names too.

```toml
[mcp.github]                      # stdio: spawn a child process
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env_from = { GITHUB_PERSONAL_ACCESS_TOKEN = "GITHUB_TOKEN" }  # child var ← host env var
include = ["search_repositories", "get_issue"]                # optional tool filter

[mcp.docs]                        # streamable HTTP
url = "https://example.com/mcp"
```

A server that fails to start is skipped with a warning; a hung call times
out at 60s. Keep total tool count modest (a warning fires above 25) — every
schema costs context and calling accuracy on a local main model.

## Recurring prompts: /loop

`/loop 5m check git status and summarize new changes` re-submits that prompt
every 5 minutes while the session is open (never mid-turn; fires after the
current turn finishes if due). `/loop` shows status, `/loop stop` disarms.
One loop at a time. For unattended loops, pair with `--mode auto` plus
permission allow-rules so it doesn't sit waiting on an ask.

## Project memory: PILOT.md and BRAINBOX.md

Both live in `.rocinante/` and are injected into every session:

- **`PILOT.md`** — project instructions (Rocinante's CLAUDE.md). Run `/init`
  and the agent explores the project and writes it: what the project is,
  build/test commands, architecture map, conventions. Edit it by hand any
  time; it's loaded at startup.
- **`BRAINBOX.md`** — living session memory: goals, state, decisions,
  gotchas, next steps. Refreshed **in the background** every 5 turns
  (configurable, never blocks a turn) and once more when you quit, so the
  next session picks up where you left off.

```toml
[brainbox]
enabled = true            # default
update_every_turns = 5
model = "scout"           # optional: use a cheaper model for updates
```

## Switching models

Any model can be the main agent — local or cloud:

```sh
rocinante --model gemma4:31b                      # bare Ollama tag
rocinante --model ollama                          # auto-pick from your Ollama server
rocinante --model anthropic/claude-opus-4-8       # cloud, zero config needed*
rocinante --model oracle                          # an alias from [models]
```

\* Cloud providers activate automatically when their key is in the
environment — `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, or `OPENAI_API_KEY` —
no config file required.

Mid-session, `/model` lists everything switchable (config aliases plus every
tag your Ollama server reports, including signed-in `:cloud` models), and
`/model <number|name|provider/model>` hot-switches with **conversation
context preserved**:

```
> /model
models (switch with /model <number|name>):
   1. main
   2. glm-5.2:cloud ← current
   3. kimi-k2.5:cloud
   ...
> /model kimi-k2.5:cloud
[model: kimi-k2.5:cloud — context preserved]
```

## Configuration

Layered: built-in defaults → `~/.rocinante/config.toml` →
`<project>/.rocinante/config.toml` → `ROCINANTE_*` env vars. API keys are
**never** stored in config — only env-var names.

```toml
[defaults]
model = "main"          # alias into [models]
mode = "normal"         # normal | auto | plan
num_ctx = 32768         # context window budget (VRAM is the real ceiling)
keep_alive = "10m"

[providers.ollama]
type = "ollama"
base_url = "http://localhost:11434"

[providers.anthropic]
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[providers.openrouter]
type = "openai"                       # any OpenAI-compatible endpoint
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

[models]
main   = { provider = "ollama", model = "gemma4:31b" }
scout  = { provider = "ollama", model = "qwen3:8b", num_ctx = 16384 }
oracle = { provider = "anthropic", model = "claude-sonnet-4-5" }

# Subagent profiles the main agent can delegate to via the `task` tool
[agents.explorer]
description = "Fast read-only codebase exploration and summarization."
model = "scout"
tools = ["read", "grep", "glob"]
max_turns = 15

[agents.oracle]
description = "Escalation target for hard reasoning and design questions."
model = "oracle"
tools = ["read", "grep", "glob"]
max_turns = 10

[permissions]
allow = ["Bash(cargo check:*)", "Bash(cargo test:*)", "Bash(git status)"]
deny  = ["Bash(rm -rf:*)", "Read(**/*.pem)", "Read(./.env)"]

[skills]
extra_dirs = ["~/.claude/skills"]     # optional compatibility
```

## Workspace layout

| Crate | Role |
|---|---|
| `rocinante-core` | Agent loop, tools, permissions, sessions, context, skills — a pure library over event channels |
| `rocinante-providers` | `Provider` trait + Ollama/OpenAI/Anthropic/Gemini implementations |
| `rocinante-tui` | ratatui terminal UI |
| `rocinante-cli` | The `rocinante` binary (TUI by default, `--no-tui` for the plain REPL) |

The core emits `AgentEvent`s on a broadcast channel and receives replies on an
mpsc — every frontend (REPL, TUI, and an eventual HTTP server) is a thin
client over that pair.

## Status

Working: agent loop, all four providers, delegation, repair pipeline,
sessions/resume, compaction, modes, skills. In progress: TUI polish,
packaged releases.
