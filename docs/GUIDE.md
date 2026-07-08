# Rocinante user guide

Everything past the README: day-to-day usage, every command, and the full
configuration reference.

## Starting a session

```sh
rocinante                     # TUI in the current project (default on a terminal)
rocinante --no-tui            # plain REPL (also used automatically when piped)
rocinante -c                  # continue the most recent session in this project
rocinante --mode plan         # start read-only
rocinante --model kimi-k2.5:cloud
rocinante ask "one-shot question, no tools"
rocinante config              # print the fully-resolved configuration
```

Rocinante works with zero configuration if Ollama is running: the built-in
default model is `glm-5.2:cloud`. Any cloud key in your environment
(`ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `OPENAI_API_KEY`) activates that
provider automatically.

## Modes

| Mode | Reads | Edits | Commands/MCP | Switch |
|---|---|---|---|---|
| `normal` | ✓ | ask | ask | `/mode normal` |
| `auto` | ✓ | ✓ | rules, then ask | `/mode auto` |
| `plan` | ✓ | denied | denied | `/mode plan` |

TUI: Shift+Tab cycles modes mid-session. Permission answers: `y` once,
`a` always (remembered for the session), `n` deny. Edits show a colored
unified diff before you answer. Denials are explained to the model so it
adapts instead of stalling.

## In-session commands

| Command | Effect |
|---|---|
| `/model` | list switchable models (aliases + discovered Ollama tags) |
| `/model <n\|name\|provider/model>` | hot-switch the main model, context preserved |
| `/mode normal\|auto\|plan` | switch permission mode |
| `/think on\|off` | extended thinking (dim reasoning stream) |
| `/init` | explore the project and write `.rocinante/PILOT.md` |
| `/commit` | agent-driven atomic git commit |
| `/loop <interval> <prompt>` | recur a prompt (`30s`, `5m`, `1h`); `/loop` status; `/loop stop` |
| `/quit` | exit (triggers the final BRAINBOX.md update) |

TUI keys: Enter send · Esc cancel the running turn · PgUp/PgDn or mouse
wheel scroll · Ctrl+C twice quits.

## Configuration reference

Layering, later wins: built-in defaults → `~/.rocinante/config.toml` →
`<project>/.rocinante/config.toml` → `ROCINANTE_*` env vars (nested keys
via `__`). API keys are **never stored in config** — only env-var names.

```toml
[defaults]
model = "main"            # alias into [models]
mode = "normal"           # normal | auto | plan
num_ctx = 32768           # context budget (VRAM is the real ceiling)
keep_alive = "10m"        # Ollama model residency
think = false             # extended thinking by default

[providers.ollama]
type = "ollama"
base_url = "http://localhost:11434"

[providers.anthropic]     # auto-injected when ANTHROPIC_API_KEY is set
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[providers.openrouter]    # any OpenAI-compatible endpoint
type = "openai"
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

[models]                  # aliases; per-model overrides
main   = { provider = "ollama", model = "glm-5.2:cloud", num_ctx = 32768 }
scout  = { provider = "ollama", model = "qwen3:8b", num_ctx = 16384 }
oracle = { provider = "anthropic", model = "claude-opus-4-8" }

[agents.explorer]         # subagent profiles → the task tool
description = "Fast read-only codebase exploration."
model = "scout"
tools = ["read", "grep", "glob"]
max_turns = 15
system_prompt = "You are a code scout. Explore and report; never modify."

[permissions]
allow = ["Bash(cargo check:*)", "Bash(cargo test:*)", "Bash(git status)"]
deny  = ["Bash(rm -rf:*)", "Read(**/*.pem)", "Read(./.env)"]

[brainbox]
enabled = true
update_every_turns = 5
model = "scout"           # optional cheaper model for memory updates

[skills]
extra_dirs = ["~/.claude/skills"]

[mcp.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env_from = { GITHUB_PERSONAL_ACCESS_TOKEN = "GITHUB_TOKEN" }
include = ["search_repositories", "get_issue"]

[lsp.rust]                # built-ins exist; override or disable by key
command = "rust-analyzer"
filetypes = ["rs"]
root_markers = ["Cargo.toml"]
```

### Permission rules

`Tool(matcher)` notation. `Bash(cargo test:*)` = prefix match on the
command; `Bash(git status)` = exact; `Read(**/*.pem)` = path glob; a bare
tool name (`task`, `mcp__github__get_issue`) allows every call to it.
Deny rules beat allow rules in every mode.

## Subagents (the crew)

Rocinante ships a default crew of six read-only specialists (named after the
Rocinante's crew in *The Expanse*), available via the `task` tool with zero
config:

| Agent | Role |
|---|---|
| `naomi` | Explorer — read-only code/web exploration and summary |
| `miller` | Researcher — investigate a question, gather sources |
| `alex` | Planner — investigate, then return a numbered plan |
| `bobbie` | Reviewer — adversarial code review |
| `amos` | Debugger — reproduce → isolate → hypothesize → fix → verify |
| `holden` | Oracle — escalate a hard design/correctness call |

All default to the `main` model (delegation still buys context isolation and
parallelism). Repoint any to a stronger model — e.g. `[agents.holden] model
= "oracle"` where `oracle` is an alias for Claude/Gemini — or disable the
whole crew with `[defaults] builtin_agents = false`. Define your own
`[agents.*]` too; a same-named profile overrides the built-in.

The main agent decides when to delegate; subagent activity streams into your
transcript and lights up the sidebar; permission asks bubble up tagged with
the agent name. Multiple read-only delegations issued together run in
parallel, and the VRAM gate stops two big local models from thrashing.

## Skills (built-in)

Seven skills ship embedded — the agent loads one on demand when its
description matches your task:

- **deep-research** — decompose a question, fan out parallel `naomi`/`miller`
  subagents, verify, synthesize a cited answer.
- **code-review**, **debugging**, **writing-tests** — coding playbooks.
- **proof-reading**, **plagiarism-check**, **peer-review** — for research and
  academic writing (Rocinante isn't only for code).

Drop a `SKILL.md` of the same name in `.rocinante/skills/<name>/` to override
a built-in, or set `[defaults] builtin_skills = false` to disable them all.

## Memory

- `.rocinante/PILOT.md` — project instructions, yours to edit, injected
  every session. Create with `/init`.
- `.rocinante/BRAINBOX.md` — agent-maintained memory (goals, state,
  decisions, gotchas, next steps), refreshed in the background and on quit.
  Delete it any time to start fresh.
- Skills — reusable instructions with SKILL.md frontmatter in
  `.rocinante/skills/<name>/` (project) or `~/.rocinante/skills/` (global);
  Claude Code skills in `~/.claude/skills` load too.

## Troubleshooting

- **Logs**: `~/.rocinante/logs/rocinante.log.<date>`; set
  `ROCINANTE_LOG=debug` for verbose tracing.
- **Model gives empty/garbled tool calls**: the repair pipeline handles
  most of it; persistent trouble usually means the model is too small —
  try `/model` to something stronger.
- **Ollama truncation**: rocinante sets `num_ctx` explicitly and warns on
  divergence; raise `[defaults] num_ctx` if you have VRAM headroom.
- **LSP diagnostics say "pending"**: the language server is still
  indexing; ask the agent to use the `lsp` tool with `action=diagnostics`
  to re-check.
- **Unattended `/loop`**: pair with `--mode auto` and allow-rules, or the
  loop will sit waiting on a permission prompt.
