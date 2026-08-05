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

On first interactive launch, Rocinante shows a **model picker** listing the
models your Ollama server offers (local and signed-in cloud tags) plus any
API providers whose key is set. Your choice is remembered globally in
`~/.rocinante/state.toml` and becomes the default next time — no hardcoded
model. Override per-run with `--model`; hot-switch mid-session with
`/model` (the new choice is remembered too). Non-interactive use
(`rocinante ask …`, piped input) needs `--model` or a previously-chosen
model, otherwise it asks you to pick one. Any cloud key in your environment
(`ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `OPENAI_API_KEY`) activates that
provider automatically and it appears in the picker.

## Modes

| Mode | Reads | Edits | Commands/MCP | Switch |
|---|---|---|---|---|
| `normal` | ✓ | ask | ask | `/mode normal` |
| `auto` | ✓ | ✓ | ✓ (deny rules still block) | `/mode auto` |
| `plan` | ✓ | denied | denied | `/mode plan` |

`auto` is hands-off: everything runs without prompts except what your
`[permissions] deny` rules block — put the guardrails there (e.g.
`Bash(git push --force:*)`, `Bash(rm -rf:*)`). `plan` interrogates before
planning: the agent investigates read-only, lists its assumptions, asks
numbered clarifying questions about any grey areas and waits for your
answers, then presents the plan and offers to execute it in auto mode.

TUI: Shift+Tab cycles modes mid-session. Permission answers: `y` once,
`a` always (remembered for the session), `n` deny. Edits show a colored
unified diff before you answer. Denials are explained to the model so it
adapts instead of stalling.

## In-session commands

| Command | Effect |
|---|---|
| `/model` | open the model picker overlay (↑↓ move, Enter switch, Esc close) |
| `/model <n\|name\|provider/model>` | hot-switch the main model directly, context preserved |
| `/mode normal\|auto\|plan` | switch permission mode |
| `/think on\|off` | extended thinking (dim reasoning stream) |
| `/effort [low\|medium\|high]` | reasoning-effort tier (bare shows current; default `high`) |
| `/submodel [<name>\|clear]` | pin EVERY subagent to one model (bare shows; overrides profiles) |
| `/config <request>` | agent edits `~/.rocinante/config.toml` (aliases, providers, permissions…) |
| `/config` | agent summarizes your current config |
| `/init` | explore the project and write `.rocinante/PILOT.md` |
| `/commit` | agent-driven atomic git commit |
| `/loop <interval> <prompt>` | recur a prompt (`30s`, `5m`, `1h`); `/loop` status; `/loop stop` |
| `/compact` | fold old turns into a summary now |
| `/update` | check the latest GitHub release and update the binary in place |
| `/trust` | trust this project's `.rocinante/config.toml` (see Workspace trust) |
| `/quit` | exit (triggers the final BRAINBOX.md update) |

TUI keys: Enter send · ↑/↓ recall previous prompts (shell-style history,
your draft is restored on the way back down) · Esc cancel the running turn
· PgUp/PgDn or mouse wheel scroll · Ctrl+C twice quits. The input box wraps
and grows as you type (up to 8 rows, then scrolls with the cursor kept
visible). When a permission modal is open, ↑/↓/PgUp/PgDn scroll a long diff
while `y`/`a`/`n` answer.

The transcript renders markdown: headers, lists, blockquotes, fenced code,
inline **bold**/*italic*/`code`/links, and GitHub-style tables (`|…|` rows
with a `|---|` separator are drawn as aligned columns with a header rule,
sized to fit the pane). Extended-thinking output streams dim while the
model reasons and then disappears once the answer begins — it's never kept
in the scrollback or the session.

## Effort

`/effort low|medium|high` sets how hard the model reasons — low for chat,
medium for routine work, high (the default) for research and coding. It
maps per provider:

| Provider | low | medium | high |
|---|---|---|---|
| Anthropic | thinking off | 8k thinking budget | 16k thinking budget |
| OpenAI-compatible | `reasoning_effort: low` | `medium` | `high` |
| Ollama (gpt-oss family) | thinking off | `think: "medium"` | `think: "high"` |
| Ollama (other models) | thinking off | `think: true`* | `think: true`* |
| Gemini | — | — | — (not yet wired) |

\* On local models thinking activation stays explicit — `/think on` turns
it on (not every local model supports it), and `/effort` sets the level.
`/effort low` always forces thinking off. Explicit `/think on|off` beats
the tier. Default via `[defaults] effort = "high"`.

## Configuration reference

Layering, later wins: built-in defaults → `~/.rocinante/config.toml` →
`<project>/.rocinante/config.toml` → `ROCINANTE_*` env vars (nested keys
via `__`). API keys are **never stored in config** — only env-var names.

You can also let the agent edit this file: `/config add alias kimiko for
kimi-k2.7-code:cloud with num_ctx 256000` writes the user-wide file for you.
`/model` re-reads config from disk every time it runs, so model aliases
added mid-session appear immediately; other sections (providers used at
startup, permissions, agents, MCP/LSP) apply on the next launch. When an
alias points at an Ollama tag, the picker lists only the alias and hides
the raw tag (no duplicate); the tag is still switchable if you type it.

```toml
[defaults]
model = "main"            # alias into [models]
mode = "normal"           # normal | auto | plan
num_ctx = 32768           # context budget (VRAM is the real ceiling)
keep_alive = "10m"        # Ollama model residency
think = false             # extended thinking by default (local models)
effort = "high"           # reasoning tier: low | medium | high

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

[context]
model = "scout"           # optional cheaper model for compaction summaries
keep_tool_turns = 3       # stub tool results older than this many turns (0 = off)

[skills]
# ~/.claude/skills, ~/.claude/plugins, and <project>/.claude/skills are
# scanned automatically; extra_dirs adds any other folder (restart to apply)
extra_dirs = ["~/team-skills"]

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

Rocinante ships a default crew of eight specialists (named after the
Rocinante's crew in *The Expanse*), available via the `task` tool with zero
config:

| Agent | Role |
|---|---|
| `naomi` | Explorer — read-only code/web exploration and summary |
| `miller` | Researcher — investigate across the repo and the web, return a cited brief |
| `alex` | Planner — investigate, then return a numbered plan |
| `bobbie` | Reviewer — adversarial code review |
| `amos` | Debugger — reproduce → isolate → hypothesize → fix → verify |
| `holden` | Oracle — escalate a hard design/correctness call |
| `avasarala` | Data scientist — EDA, statistics, SQL/DuckDB, wrangling; runs Python/R |
| `camina` | ML engineer — preprocessing, model selection/tuning, evaluation |

`naomi`, `alex`, `bobbie`, `amos`, and `holden` are read-only. `miller`
carries `bash` solely for web research (the `web-research` skill: curl
search/fetch with cited sources). `avasarala` and `camina` can run code
and write files, and they load the matching data-science/ML skills before
working; at decision gates (which variables to drop, scaling, model
family, …) they stop and report options with a recommendation instead of
guessing. Every bash/write call still goes through your permission mode.

All default to the `main` model (delegation still buys context isolation and
parallelism). Repoint any to a stronger model — e.g. `[agents.holden] model
= "oracle"` where `oracle` is an alias for Claude/Gemini — or disable the
whole crew with `[defaults] builtin_agents = false`. Define your own
`[agents.*]` too; a same-named profile overrides the built-in. A profile
whose `tools` list includes `"skill"` gets the skill tool plus the skill
index in its prompt.

The main agent decides when to delegate — and to which model. At startup
Rocinante inventories every switchable model (local sizes and parameter
counts from Ollama, cloud aliases from config) and injects a "models
available for delegation" briefing into the system prompt, so the agent
can pass `model` in a `task` call to run a subagent on the cheapest
adequate model (`task[naomi @ qwen3:8b]` in the transcript). Local models
cost time only; cloud models cost money — the briefing says so and tells
it to pick the smallest model that fits the subtask. To take that choice
away entirely, `/submodel glm-5.2:cloud` pins EVERY subagent to one model
— enforced by the harness, beating both profile models and the
orchestrator's own overrides (`/submodel clear` releases; the pin shows in
the sidebar's SESSION panel). Subagent activity streams into your
transcript. The sidebar's AGENTS section lists **your** `[agents.*]`
profiles all the time; built-in crew members appear only while working — an
animated spinner with a live instance count (`⠙ miller ×4`) during a
parallel fan-out, `✓` after finishing this turn — and disappear when idle.
Permission asks bubble up tagged with the agent name. Multiple read-only
delegations issued together run in parallel, and the VRAM gate stops two
big local models from thrashing.

## Skills

### Where skills come from

Discovery scans these locations at startup (lowest → highest precedence;
a same-named skill in a higher tier shadows the lower one):

1. `~/.claude/plugins` — Claude Code plugin caches, scanned deep
2. `~/.claude/skills` — your Claude Code user skills
3. `<project>/.claude/skills`
4. `~/.rocinante/skills` — global rocinante skills
5. `<project>/.rocinante/skills` — project skills
6. `[skills] extra_dirs` from config — any other folder you point at
7. Built-ins fill whatever names remain

An existing Claude Code install's skills just work, zero config. The
`skill` tool **rescans on demand** when asked for a name it doesn't know,
so a skill installed or created mid-session activates immediately (the
sidebar and index refresh on next launch). Ask the agent to "create a
skill for X" or "install the Y skill from github" — the `skill-maker`
built-in walks it through authoring, git installs (with a
review-before-install rule), and troubleshooting.

### Built-in library (40)

All written as explicit checklists so even small local models follow them:

- **Research & writing**: `deep-research` (parallel `naomi`/`miller`
  fan-out, verify, synthesize), `web-research` (search/fetch/cite the
  internet via curl), `proof-reading`, `plagiarism-check`, `peer-review`,
  `quarto` (reproducible .qmd reports and slides)
- **Coding**: `code-review`, `debugging`, `writing-tests`, `git-rescue`
  (safe recovery from git mistakes + merge conflicts, with recommended
  deny rules), `github-cli` (PRs, issues, CI/Actions runs, releases via
  the `gh` CLI), `frontend-design` (vendored from anthropics/skills,
  Apache-2.0)
- **Data science**: `exploratory-data-analysis`, `statistical-modeling`,
  `sql-analytics`, `data-wrangling`, `medallion-architecture` (mixed-file
  folder → bronze/silver star schema), `ducklake` (versioned lakehouse
  over Parquet)
- **Machine learning**: `ml-preprocessing`, `ml-modeling`,
  `ml-evaluation`, `recommender-systems` (Apriori/FP-Growth/Eclat rules +
  collaborative filtering)
- **NLP**: `spacy`, `nltk`
- **Documents**: `docx`, `xlsx`, `pptx`, `pdf` (creation, formatting,
  conversion via pandoc/LibreOffice; `quarto` for reproducible reports)
- **Tools & frameworks**: `duckdb`, `ggplot`, `sqlalchemy`, `flask`,
  `vuejs`, `d3js`, `mermaidjs`, `lxc`, `ollama`, `postgresql`
- **Meta**: `skill-maker`, `rocinante-config` (the `/config` command's
  brain: edits `~/.rocinante/config.toml` safely — full schema, never
  stores API keys)

Drop a `SKILL.md` of the same name in any higher tier to override a
built-in, or set `[defaults] builtin_skills = false` to disable them all.
The sidebar lists only your own skills; built-ins stay out of the way but
remain loadable.

## Memory

- `.rocinante/PILOT.md` — project instructions, yours to edit, injected
  every session. Create with `/init`. At startup Rocinante compares its
  age against the README and root build manifests (Cargo.toml,
  package.json, pyproject.toml, go.mod, Makefile, …) and shows a "may be
  stale — run /init to refresh" notice when the project has moved since it
  was written. Advisory only; nothing is rewritten automatically.
- `.rocinante/BRAINBOX.md` — agent-maintained memory (goals, state,
  decisions, gotchas, next steps), refreshed in the background and on quit.
  Quit is instant when a background refresh already covers the whole
  session — the final update only runs when there are unrecorded turns.
  Delete it any time to start fresh.

## Context hygiene

Three mechanisms keep the context window lean without losing the thread:

1. **Tool-result pruning** — once a tool result is older than the last
   `keep_tool_turns` user turns (default 3), it's replaced in context by a
   one-line stub (tool name, size, first line). The full output stays in
   the session JSONL, and the model can always re-run the tool. Set
   `[context] keep_tool_turns = 0` to disable.
2. **Proactive compaction** — at 60% of the context budget, old turns are
   summarized **in the background** (the session keeps flowing) and the
   summary splices in at the next turn boundary. The blocking compaction
   at 80% still exists as a fallback, but rarely fires.
3. **Structured summaries** — compaction fills a rigid template (files
   touched, decisions, constraints & gotchas, state, open items) that keeps
   exact paths/commands/errors and explicitly drops tool-output bodies.

Summaries run on the main model unless you point `[context] model` at a
cheaper one — recommended on a single-GPU Ollama box, where a background
summary on the main model queues behind your live turn.
- Skills — reusable instructions with SKILL.md frontmatter in
  `.rocinante/skills/<name>/` (project) or `~/.rocinante/skills/` (global);
  Claude Code skills (`~/.claude/skills`, `~/.claude/plugins`, project
  `.claude/skills`) load automatically too. See the Skills section above.

## Workspace trust

A project-local `.rocinante/config.toml` is loaded automatically — which is
convenient, but a cloned repo could ship one that spawns MCP servers at
startup, redirects providers to exfiltrate your conversation (reusing an API
key you have set), grants subagents extra tools, or auto-approves tool calls.
So Rocinante treats a project's config as **untrusted by default** and drops
its security-relevant keys, keeping only the harmless ones:

| Dropped until trusted | Always applied |
|---|---|
| `[providers]`, `[mcp]`, `[lsp]`, `[agents]` | `[models]`, `[brainbox]`, `[context]` |
| `permissions.allow`, `skills.extra_dirs` | `permissions.deny` (only restricts) |
| `defaults.mode` (could force `auto`) | rest of `[defaults]`, `[skills]` |

When anything is dropped you get a startup notice listing it. If you trust the
repo, run **`/trust`** (remembered per-project in `~/.rocinante/trust.toml`) and
restart — the full config then applies. Your **user-wide**
`~/.rocinante/config.toml` is always trusted; only project configs are gated.

## Updating

`/update` checks GitHub's latest release and, if it's newer than the running
build, downloads the right binary for your platform, verifies its SHA-256
against the published `SHA256SUMS`, and atomically replaces the executable —
then tells you to restart. It only ever runs when you invoke it; there is no
automatic check or background phoning home. The running session is unaffected
(the old binary keeps running until you restart). Homebrew and Scoop installs
are never touched — `/update` prints `brew upgrade rocinante` /
`scoop update rocinante` instead, since those binaries belong to the package
manager. A `cargo install`ed binary updates in place with a note that a
future `cargo install` will overwrite it.

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
- **Unattended `/loop`**: pair with `--mode auto` (hands-off — only deny
  rules block), or the loop will sit waiting on a permission prompt.
