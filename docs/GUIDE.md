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
API providers whose key is set. Config aliases appear as
`kimi-k3:cloud  (kimiko)` — the real model name first, the alias in parens —
and filtering matches either name; once a session runs, the sidebar and
status line show the alias. Your choice is remembered globally in
`~/.rocinante/state.toml` and becomes the default next time — no hardcoded
model. Override per-run with `--model`; hot-switch mid-session with
`/model` (the new choice is remembered too). Non-interactive use
(`rocinante ask …`, piped input) needs `--model` or a previously-chosen
model, otherwise it asks you to pick one. Any cloud key in your environment
(`ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `OPENAI_API_KEY`) activates that
provider automatically and it appears in the picker.

**Ollama cloud tags are per-machine.** Ollama only lists a `:cloud` model
(e.g. `minimax-m3:cloud`) after its tiny stub has been pulled once on that
machine — so on a fresh install the picker shows only on-device models even
when you're signed in. Sign in with `ollama signin`, then just use the tag:
free-type it (`/model minimax-m3:cloud` or `--model minimax-m3:cloud`) and
Rocinante auto-pulls the ~300-byte stub on first use; from then on the tag
appears in the picker like any local model.

## Modes

| Mode | Reads | Edits | Commands/MCP | Switch |
|---|---|---|---|---|
| `normal` | ✓ | ask | ask | `/mode normal` |
| `auto` | ✓ | ✓ | ✓ (deny rules still block) | `/mode auto` |
| `plan` | ✓ | denied | denied | `/mode plan` |

`auto` is hands-off: everything runs without prompts except what your
`[permissions] deny` rules block — put the guardrails there (e.g.
`Bash(git push --force:*)`, `Bash(rm -rf:*)`). `plan` interrogates before
planning: the agent investigates read-only, thinks hard about the best
approach and anticipates the likely problems *and their solutions*, and if
anything is still unclear asks **up to 5** numbered clarifying questions
**once** and waits. The plan it then presents names the files to change and
the pitfalls it foresees with how it will handle each — and offers to execute
it in auto mode.

TUI: Shift+Tab cycles modes mid-session. Permission answers: `y` once,
`a` always (remembered for the session), `n` deny. Edits show a colored
unified diff before you answer. Denials are explained to the model so it
adapts instead of stalling.

## In-session commands

| Command | Effect |
|---|---|
| `/model` | open the model picker overlay (↑↓ move, Enter switch, Esc close); rows read `model  (alias)` |
| `/model <n\|name\|provider/model>` | hot-switch the main model directly, context preserved |
| `/localmodel` | pick a GGUF model from your model folder — run in-process by the embedded llama.cpp (asks for the folder the first time); `/localmodel <name\|path.gguf>` switches directly, `/localmodel alias <alias> <model>` saves an alias, `/localmodel dir [path]` changes the folder (see Local models) |
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
| `/clear` | reset the conversation (keeps the system prompt); `/clear --all` also wipes BRAINBOX.md |
| `/remember <rule>` | record a global preference/rule in `~/.rocinante/LESSONS.md` |
| `/verify` | run the iterate-and-fix quality check on the last task (Esc stops it) |
| `/thinking [on\|off\|status]` | critical thinking: a bare `/thinking` toggles answer + work review in one go (faster when off); the status line always shows `⚖ thinking` or `⚖ thinking off` (see Scrutiny) |
| `/scrutiny` | show answer/work review settings; `/scrutiny off\|answers\|work\|all`, `/scrutiny model <name>\|clear`, `/scrutiny show hidden\|badge\|findings` (see Scrutiny) |
| `/toolbox` | status of external CLIs (ferraus, pixelox); `/toolbox install\|update\|remove <name>` (see Toolbox) |
| `/collaborate` | open the crew pane on the right; `/collaborate` again, `/collaborate off` or Esc returns to the status pane |
| `/pane status\|collaborate\|context` | choose what the right pane shows |
| `@name <message>` | talk to a crew member directly (`@naomi status`, `@naomi !stop`, `@naomi report`) |
| `/update` | check the latest GitHub release and update the binary in place (also `rocinante --update` from the shell) |
| `/uninstall` | preview removal; `/uninstall confirm` removes the binary, add `--purge` to also wipe `~/.rocinante` |
| `/trust` | trust this project's `.rocinante/config.toml` (see Workspace trust) |
| `/context` | open the context-usage dashboard (↑↓/PgUp/PgDn scroll, Esc close) |
| `/mouse [on\|off]` | mouse capture: on (default), the wheel scrolls whatever is under the pointer; off restores native click-drag selection |
| `/quit` | exit immediately (memory updates happen in the background during the session) |

TUI keys: Enter send · ↑/↓ recall previous prompts (shell-style history,
your draft is restored on the way back down) · Esc cancel the running turn
(when nothing runs, Esc returns the right pane to status) · PgUp/PgDn scroll
the transcript · Shift+PgUp/PgDn scroll the right pane · Ctrl+C twice quits. The input box wraps and grows as
you type (up to 8 rows, then scrolls with the cursor kept visible). When a
permission modal is open, ↑/↓/PgUp/PgDn scroll a long diff while `y`/`a`/`n`
answer. Paste into the input with your OS shortcut — multi-line pastes arrive
intact and don't submit until you press Enter.

**Mouse.** The mouse wheel scrolls whatever is under the pointer: over the
conversation it scrolls the responses, over the right pane it scrolls the crew
feed or the context breakdown, and over the input box it steps through your
previous prompts (like ↑/↓). Because Rocinante captures the mouse for this,
select text with **Shift+drag** (Option+drag in macOS Terminal and iTerm2) and
copy it with your OS shortcut. `/mouse off` hands the mouse back to the
terminal for plain click-drag selection (the wheel then recalls prompts
again); `[defaults] mouse = false` makes that the default.

**`@` file references.** Type `@` followed by part of a path to reference a
file or folder from the project. A cyan overlay lists matches (gitignore-aware,
built at startup); ↑/↓ move, Tab/Enter insert the highlighted path, Esc closes
it. It inserts the path as text — the agent reads the file on demand, so your
context stays lean. New files added after launch need a restart to appear.

**Queuing questions.** Ask something while a turn is still running and it's
*queued* instead of interrupting — you'll see a `queued (#n)` note. When the
turn finishes, Rocinante asks `run next queued question? [y/n]` for each queued
item in order: `y` runs it, `n` skips to the next, Esc pauses the prompts while
keeping the queue. (To stop the current turn instead, press Esc while it runs.)

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

## Local models (embedded llama.cpp)

Rocinante has llama.cpp built in: a GGUF file runs inside Rocinante itself, with
no Ollama, no `llama-server` and nothing else to install.

**First time.** Type `/localmodel`. Rocinante asks where your models are; type
the folder (for example `~/Models`) and press Enter. It saves the folder as
`[defaults] local_model_dir` in `~/.rocinante/config.toml` (so `/config set
local_model_dir ~/Models` works too) and opens a picker of every GGUF in it
(subfolders included, so LM Studio's `publisher/model/file.gguf` layout works).
Pick one with ↑/↓ and Enter. After that, `/localmodel` goes straight to the
picker; `/localmodel dir` asks for a new folder.

**Straight to a file.** `/localmodel /path/to/model.gguf` (or `~/…`, or a
relative path) uses that file without touching the folder setting.
`/localmodel Qwen3.5-4B-Q4_0` picks a file in the folder by name.

**Names and aliases.** A local model is `local/<file name without .gguf>`, so
`/model local/Qwen3.5-4B-Q4_0` and `rocinante --model local/Qwen3.5-4B-Q4_0`
work as well. Save a short alias with `/localmodel alias qwen Qwen3.5-4B-Q4_0`;
it lands in `[models]` like any other alias, so `/model qwen` switches to it and
you can give it its own `num_ctx`, `temperature`, `top_p` or `top_k`:

```toml
[models.qwen]
provider = "local"
model = "/home/me/Models/Qwen3.5-4B-Q4_0.gguf"
num_ctx = 16384
```

**How it runs.** llama.cpp formats the conversation with the model's own chat
template and parses its tool calls with the parser for that model family (the
same code `llama-server` uses), so tool use works on models trained for it.
The KV cache is kept between turns: an agent loop that resends the
conversation only processes what is new. Hybrid, recurrent and sliding-window
models (Qwen3.5, Mamba-style, Gemma) can't trim their cache, so Rocinante
checkpoints their state at message boundaries — after the system prompt, at
your last message and at the end of the conversation — and resumes from the
latest one that still matches. One local model is loaded at a time and runs
one request at a time; crew members on a local model wait their turn. Thinking
stays off unless you `/think on`, as with Ollama.

**The first reply is the slow one.** It loads the model and reads Rocinante's
whole system prompt and tool list (a few thousand tokens) once; the status line
shows `⧗ loading model`, then `⧗ reading prompt 1024/3345`. On a laptop CPU that
can take a few minutes; after it, each step only reads what's new and replies
start in seconds.

**Hardware.** Models run on the CPU, using every physical core (set
`[providers.local] threads` to use fewer), with GPU offload through Metal on
Apple Silicon. On x86-64 the CPU needs AVX2 (Intel Haswell / AMD Excavator,
2013 or later); on older chips Rocinante says so instead of loading. The
context size comes from `num_ctx` (the model's alias, else `[defaults]
num_ctx`), capped at what the model was trained for. A large model with a
large `num_ctx` needs RAM for both, so lower `num_ctx` if a load fails.

The picker skips multimodal projector files (`mmproj-*`), vocabulary-only files
(`ggml-vocab-*`), hidden folders and the later parts of split models
(`…-00002-of-00003.gguf`; llama.cpp loads the rest from part 1). A project's
`.rocinante/config.toml` can never point Rocinante at a local model file (see
Workspace trust): llama.cpp parses GGUF files in-process, so only you choose
which ones it opens.

## Configuration reference

Layering, later wins: built-in defaults → `~/.rocinante/config.toml` →
`<project>/.rocinante/config.toml` → `ROCINANTE_*` env vars (nested keys
via `__`). API keys are **never stored in config** — only env-var names.

You can also let the agent edit this file: `/config add alias kimiko for
kimi-k2.7-code:cloud with num_ctx 256000` writes the user-wide file for you.
`/model` re-reads config from disk every time it runs, so model aliases
added mid-session appear immediately; other sections (providers used at
startup, permissions, agents, MCP/LSP) apply on the next launch. When an
alias points at an Ollama tag, the picker shows one row for it —
`tag  (alias)` — and hides the bare raw tag (no duplicate); the tag is
still switchable if you type it. The work interface (sidebar, status,
`/context`) shows the alias while it's the active model, and switches are
remembered by the picked name so alias settings like `num_ctx` survive a
restart.

```toml
[defaults]
model = "main"            # alias into [models]
mode = "normal"           # normal | auto | plan
num_ctx = 32768           # context budget (VRAM is the real ceiling)
keep_alive = "10m"        # Ollama model residency
think = false             # extended thinking by default (local models)
effort = "high"           # reasoning tier: low | medium | high
mouse = true              # wheel scrolls what's under the pointer (Shift+drag selects)
local_model_dir = "~/Models"  # GGUF folder for /localmodel (asked for the first time)

[providers.ollama]
type = "ollama"
base_url = "http://localhost:11434"
num_parallel = "auto"     # requests one loaded local model serves at once ("auto" | n)
cloud_concurrency = 3     # Ollama Cloud plan: 3 (pay-as-you-go, Pro) or 10 (Max, Team)
max_loaded_models = "auto" # local models resident at once ("auto" learns it per session)

[providers.local]         # built in: the embedded llama.cpp (see Local models)
type = "llamacpp"
threads = 0               # CPU threads; 0 = all physical cores
# gpu_layers = 99         # layers offloaded where the build has a GPU (Metal on Apple Silicon)

[providers.anthropic]     # auto-injected when ANTHROPIC_API_KEY is set
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
concurrency = 4           # concurrent requests to this provider

[providers.openrouter]    # any OpenAI-compatible endpoint
type = "openai"
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

[models]                  # aliases; per-model overrides
main   = { provider = "ollama", model = "glm-5.2:cloud", num_ctx = 32768 }
scout  = { provider = "ollama", model = "qwen3:8b", num_ctx = 16384 }
oracle = { provider = "anthropic", model = "claude-opus-4-8" }
qwen   = { provider = "local", model = "/home/me/Models/Qwen3.5-4B-Q4_0.gguf", num_ctx = 16384 }

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
hide = ["vue"]            # skills found on disk to leave out of Rocinante

[mcp.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env_from = { GITHUB_PERSONAL_ACCESS_TOKEN = "GITHUB_TOKEN" }
include = ["search_repositories", "get_issue"]

[lsp.rust]                # built-ins exist; override or disable by key
command = "rust-analyzer"
filetypes = ["rs"]
root_markers = ["Cargo.toml"]

[scrutiny]                # (formerly [verification]) see Scrutiny
answers = "substantial"   # off | substantial | all
work = true
model = "kimi-k3.1-pro:cloud"  # optional critic; unset = the main model reviews itself
show = "badge"            # hidden | badge | findings

[crew]                    # see Subagents
persistent = true         # task delegations go to long-lived members
max_idle = "30m"          # a member idle this long drops its context

[toolbox]                 # see Toolbox
dir = "~/.rocinante/bin"  # installs land here; prepended to PATH for bash
auto_install = "ask"      # ask | never | always (always only in auto mode)
```

### Permission rules

`Tool(matcher)` notation. `Bash(cargo test:*)` = prefix match on the
command; `Bash(git status)` = exact; `Read(**/*.pem)` = path glob; a bare
tool name (`task`, `mcp__github__get_issue`) allows every call to it.
Deny rules beat allow rules in every mode.

## Subagents (the crew)

Rocinante ships a default crew of nine specialists (named after the
Rocinante's crew in *The Expanse*), available via the `task` tool with zero
config:

| Agent | Role |
|---|---|
| `naomi` | Explorer — read-only code/web exploration and summary |
| `miller` | Researcher — investigate across the repo and the web, return a cited brief |
| `alex` | Planner — investigate, then return a numbered plan |
| `bobbie` | Reviewer — adversarial code review |
| `amos` | Debugger — reproduce → isolate → hypothesize → fix → verify |
| `holden` | Captain and oracle — escalate a hard design/correctness call |
| `avasarala` | Data scientist — EDA, statistics, SQL/DuckDB, wrangling; runs Python/R |
| `camina` | ML engineer — preprocessing, model selection/tuning, evaluation |
| `monica` | Media producer — audio and image work with ferraus and pixelox (see Toolbox) |

The crew know each other. Every member is told the roster and that `holden`
is the captain of the Rocinante, so a member asked about something that is
really a crewmate's job says who to ask (`ask @naomi`). Every subagent, crew or
not, can also answer "how do I … in Rocinante?" from this guide, which ships
inside the binary; if it's unclear whether you mean Rocinante or your project,
it asks.

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
delegations issued together run in parallel, and the residency gate stops
two big local models from thrashing (below).

### Persistent members

Crew members are **persistent** (`[crew] persistent = true`, the default):
the first `task` to `naomi` starts her, and later tasks go to the same
member, who remembers what she already explored — so a follow-up can be
brief and skips re-reading the same files. A member works on one task at a
time; messages that arrive while she works queue in order (the task tool's
description tells the main agent that parallel work needs *different*
members). A member's context is pruned and compacted like the main agent's,
and dropped after `[crew] max_idle` (default `30m`) without messages; the
next message starts her fresh and says so. Each message picks its model the
way a one-off subagent would (the `/submodel` pin, then the task's `model`,
then the profile); when that differs from the running member's model, a new
member starts and receives a summary of the old one's work. Members do not
survive a restart (`-c` resumes the main agent only). Set `persistent =
false` to get the old behaviour: a fresh subagent per `task` call.

### The crew pane

**`/collaborate`** swaps the status sidebar for the crew pane (40% of the
terminal, at least 44 columns): a roster on top — each member's state
(`⠙` working with the current step and elapsed time, `○` idle, `?` waiting
for your permission, `⏳` waiting for a model slot, `✕` context dropped),
queue length and token use — and below it a feed of every message, newest at
the bottom (Shift+PgUp/PgDn or the mouse wheel over the pane scrolls it).
Delegations from the main agent show dim (`main → miller`); your own
conversations stand out. Each member gets a colour for the session in the
order you first talk to them — the first blue, the second magenta, then green,
amber, violet and so on — and keeps it in the roster, the feed and the
transcript lines, so you can tell at a glance who said what.

Talk to a member by starting a message with **`@name`**: `@naomi what are you
working on?` goes to naomi, not the main agent, even while a turn is running.
The reply lands in the pane (`naomi → you`), and the transcript keeps one dim
line each way so your scrollback stays complete. `@naomi status` (or `@naomi
?`) answers from the roster without calling a model; `@naomi !stop`
interrupts her current turn and clears her queue; `@naomi report` asks her to
summarize the conversation for the main agent, which receives it before your
next message (the main agent is otherwise not told about your side
conversations). `@` followed by a project path is still a file reference.
`/pane context` shows the `/context` breakdown in the pane instead;
`/collaborate` again, `/collaborate off`, `/pane status` or Esc (when nothing
runs) returns to the status sidebar; the pane's bottom line says so. In
`--no-tui` mode, `@name` works the same and replies print as `[crew] naomi →
you: …` lines.

### Parallelism and model slots

Every model request — the main agent, subagents, crew members, the critic,
compaction, memory and learning updates — takes a **slot** in the lane its
model runs in, so Rocinante never sends a backend more requests than it can
serve: one lane per Ollama server for all `:cloud` tags (`cloud_concurrency`,
default 3 — Ollama Cloud rejects the overflow with 429 errors), one per
loaded local model (`num_parallel`), one per other provider
(`concurrency`). A request without a free slot waits its turn; task cards
and the status line show `⏳ waiting for a model slot`, and the sidebar's
SESSION panel shows the main lane (`slots 2/3 cloud` = 2 of 3 free). The
system prompt tells the main agent how many delegations can run at once, and
the sentence is rewritten on `/model` and `/submodel`. Read-only calls and
delegations also start the moment the model finishes emitting each one,
while it is still writing the rest of the message (results still land in
call order).

`num_parallel = "auto"` uses `OLLAMA_NUM_PARALLEL` when it is set in
Rocinante's environment; otherwise, after the first request to a local model,
it reads `/api/ps` (the runner's context divided by the requested `num_ctx`;
a CPU-only runner is always 1). `max_loaded_models = "auto"` starts by
serializing different-model local subagents and unloading them after each
call; once it sees the main model stay loaded next to a subagent's model, that
pair stops unloading for the session. If Ollama Cloud ever answers 429, the
next startup suggests `/config set cloud_concurrency 10` for Max and Team
plans.

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

An existing Claude Code install's skills just work, zero config. When one
of them is wrong for a local model — say a Claude Code `vue` skill that
insists on npm projects and crowds out the offline `vuejs` built-in — list
it in `[skills] hide = ["vue"]`: Rocinante skips it, Claude Code still uses
it. `hide` only applies to skills found on disk; if a hidden skill shared a
built-in's name, the built-in comes back. To drop every built-in, use
`[defaults] builtin_skills = false`.

The
`skill` tool **rescans on demand** when asked for a name it doesn't know,
so a skill installed or created mid-session activates immediately (the
sidebar and index refresh on next launch). Ask the agent to "create a
skill for X" or "install the Y skill from github" — the `skill-maker`
built-in walks it through authoring, git installs (with a
review-before-install rule), and troubleshooting.

### Built-in library (51)

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
  `vuejs` (offline single pages or npm projects — see below), `d3js`,
  `mermaidjs`, `lxc`, `ollama`, `postgresql`, `threejs` (3D scenes and
  charts, offline — see below)
- **Media**: `ferraus` (audio editing, stems, tuning, transcription,
  notation) with `ferraus-mix`, `ferraus-master`, `ferraus-vocals`,
  `ferraus-stems`, `ferraus-beats`, `ferraus-mix-review` for whole jobs;
  `pixelox` (photo editing, graphics, SVG) and `pixelox-retouch`. They drive
  the ferraus and pixelox CLIs, which Rocinante installs on first use (see
  Toolbox).
- **Meta**: `skill-maker`, `rocinante-config` (the `/config` command's
  brain: edits `~/.rocinante/config.toml` safely — full schema, never
  stores API keys), `rocinante-help` (answers "how do I … in Rocinante?" from
  this guide, which ships inside the binary)

Two built-ins ship their library inside the binary, so the pages they build
work with no internet and open with a double-click:

- `threejs` — three.js r187 with OrbitControls and CSS2DRenderer, bundled
  as one classic script with a global `THREE` (browsers refuse ES modules
  on a page opened from disk). Pages that load files at runtime (textures,
  models) still need a local server; the skill tells the agent when.
- `vuejs` — Vue 3.5.45's official global builds. For a page, tool or
  prototype the skill builds one `index.html` on the development build,
  whose warnings catch mistakes like a name missing from `setup()`'s
  return; the production build is swapped in only for pages that will be
  hosted. Existing Vue projects, or requests for an npm/Vite project, still
  get single-file components as before.

Loading either skill writes its files to `~/.rocinante/skill-files/<name>/`
(rewritten only when a new Rocinante version changes them); the agent
copies them next to the page and checks the page in headless Chrome/Edge
before reporting. Maintainers refresh the libraries with
`scripts/vendor-three.sh <version>` and `scripts/vendor-vue.sh <version>`.

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
  decisions, gotchas, next steps), refreshed in the background every few
  turns; quitting never waits on it.
  Only the **Goals + Next steps** head is injected into the system prompt;
  the model reads the full file on demand (via the `read` tool) when it
  needs earlier decisions, state, or gotchas — so memory doesn't sit in
  every request. Delete it any time to start fresh.

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

Start over any time with **`/clear`** — it drops the conversation (keeping the
system prompt) so the next turn begins fresh; the context gauge resets and
`-c` resume picks up from the cleared state. **`/clear --all`** additionally
wipes `.rocinante/BRAINBOX.md` and strips the injected memory head from the
live prompt, so no prior-session memory carries forward.

Standing context (the always-on system prompt) is kept lean too: the skills
index lists each skill with a **short one-line trigger** (the full body only
loads when the `skill` tool activates it), and BRAINBOX injects only its head
(above). Tool output is capped to fit the context window, and old tool
results are stubbed out in one batch only once the context actually fills —
leaving history byte-stable between turns so a local server's prompt cache
keeps its hits. Run **`/context`** any time for a live grid of what fills the
window — system-prompt categories, per-skill standing cost, agents, tool
schemas, conversation messages, and free space — so you can see where tokens
go. The grand total uses the provider's real prompt-token count once a turn
has run; category splits are estimates.
- Skills — reusable instructions with SKILL.md frontmatter in
  `.rocinante/skills/<name>/` (project) or `~/.rocinante/skills/` (global);
  Claude Code skills (`~/.claude/skills`, `~/.claude/plugins`, project
  `.claude/skills`) load automatically too. See the Skills section above.

## Learning

**Global rules — `~/.rocinante/LESSONS.md`.** A small, user-wide file of your
preferences and do/don't rules, injected into every session across all
projects (shown in `/context`). Add one with **`/remember <rule>`** — the
agent files it under `## Preferences` or `## Rules`. It's also grown by a
**conservative** periodic background pass that records a rule *only* when you
explicitly stated a preference, corrected the agent, or a mistake recurred —
never from the model's guesses; a session with no such signal leaves the file
untouched. Edit or trim the file by hand any time. Config:

```toml
[learning]
enabled = true
update_every_turns = 10    # 0 disables automatic capture (/remember still works)
model = "scout"            # optional cheaper model for the capture pass
```

## Scrutiny

Before an answer reaches you, a **critic** reads your ask, the draft answer
and what the agent did this turn (its tool calls and the first lines of their
results), and either approves it or sends concrete defects back — a
requirement not met, a claim the evidence contradicts, a wrong command or
path, an unsafe suggestion, a question left unanswered. Findings trigger a
silent revision (the agent may use tools to check), then a second review, up
to `max_revisions`; then the answer is released either way. Your global rules
(LESSONS.md) count as review criteria; PILOT.md does not.

**`/thinking` — critical thinking on or off.** Review makes answers slower,
so it is one keystroke away: a bare `/thinking` turns answer and work review
off together, and the next `/thinking` restores exactly what was on.
`/thinking status` says which it is, and the status line always shows
`⚖ thinking` (on) or `⚖ thinking off`. The agent knows the setting too, so
"are we in thinking mode?" gets a true answer. (`/think` is different: it
turns the model's own reasoning stream on or off.) The finer controls are
below, under `/scrutiny`.

While a reviewed turn runs, its prose streams dim (like thinking) so you can
see it forming; the final answer replaces it. Your history and the session
file keep only your ask, the tool exchanges and the released answer — never
the draft, the critique or the hidden corrective prompt.

Which answers are reviewed: `answers = "substantial"` (default) means answers
of at least `min_chars` (400), answers of turns that edited files or ran
commands, and plan-mode plans (a plan is reviewed before it is presented,
whatever its length); `all` reviews every answer; `off` none. A short
question back to you is never reviewed.

**Work review (self-correcting).** After a turn that edited files or ran a
command, the critic also checks the *work* against your ask. If it finds
gaps, it feeds them back as a corrective pass and re-checks, **up to
`max_iterations` times** (default 3), then certifies with `✓ verified`. A
clean first pass stops immediately. **`/verify`** runs the same loop on the
last task on demand; **Esc** stops it. Set `check_command` to also run your
tests/build and fold pass/fail into each verdict.

The critic is the **main model reviewing itself** unless you pin a stronger
one with `model` (or `/scrutiny model kimi-k3.1-pro:cloud`). An unpinned
critic follows `/model`. If a pinned critic can't be reached, you get one
notice (`critic … unavailable; self-scrutiny`) and the main model takes over
for the session; if no critic is reachable, the answer is released unreviewed
with one notice. Critic tokens count in the sidebar totals. Same-model review
reliably catches omissions and self-contradictions, rarely knowledge errors; a
stronger critic catches more. Neither is a guarantee.

```toml
[scrutiny]                 # [verification] is still accepted (auto = work)
enabled = true             # false starts with answer and work review off
answers = "substantial"    # off | substantial | all
work = true                # review completed edits/commands
model = "kimi-k3.1-pro:cloud"  # optional critic; unset = the main model
max_revisions = 1          # silent answer rewrites before release
max_iterations = 3         # corrective fix passes for work (0 = report only)
min_chars = 400            # "substantial" threshold for plain answers
show = "badge"             # hidden | badge | findings
check_command = "cargo test"   # optional; runs in the project dir
timeout_secs = 60          # per critic call
actions = "off"            # "risky": pre-action review (below)
risky_edit_lines = 40
```

What you see (`show`, or `/scrutiny show …`): `hidden` — nothing beyond the
answer; `badge` (default) — `⚖ reviewing` in the status line while the critic
runs, a dim `⚖ 1 revision` line after a revised answer, `✓ verified` for
work, and `⚖ critic: <model>` in the sidebar when one is pinned; `findings` —
the badge plus the critic's bullets in a collapsed dim cell. In `--no-tui`
mode, `badge` prints `[scrutiny] revised once` and `findings` also prints the
drafts and the critique.

**Pre-action review** (`actions = "risky"`, off by default): in auto mode,
before running a `bash` command that no allow rule covers, or an edit or
write longer than `risky_edit_lines`, the critic is asked whether the action
is safe and consistent with your ask; a REVISE blocks it and hands the
findings back to the agent. It costs one critic call per risky action.

`/scrutiny` alone shows the current settings; `/scrutiny off|answers|work|all`
chooses what is reviewed (`/scrutiny answers all` reviews every answer).

`check_command` runs a shell command outside the permission prompt, so it's a
**trusted** knob — `[scrutiny]` (and `[verification]`) is ignored from an
untrusted project's config (only your user-wide config or a `/trust`ed
project can set it).

## Toolbox

Rocinante's models can edit audio and images through two companion CLIs by
the same author — **ferraus** (audio workstation: edit, repair, separate
stems, tune, transcribe, mix, master) and **pixelox** (image workstation:
layers, masks, retouching, type, SVG) — without you installing anything
first. Their skills ship inside Rocinante; the tool binaries do not (they are
large and most sessions never need them). When the agent loads a ferraus or
pixelox skill and the CLI is missing, the skill tells it to call the
`toolbox` tool, which:

1. looks up the tool's latest GitHub release and its `dist-manifest.json`,
2. picks the archive for your platform (on Linux the glibc build, or the
   static musl build when glibc is older than 2.38),
3. asks you — `Install ferraus 0.1.4 (55 MB) from github.com/djynnius/ferraus
   into ~/.rocinante/bin` — then downloads it and **verifies its SHA-256**
   against the manifest (a mismatch aborts before anything is written),
4. puts the binaries in `[toolbox] dir` (default `~/.rocinante/bin`, which
   is prepended to PATH for every `bash` command), runs `ferraus doctor
   --json`, and records the install in `~/.rocinante/toolbox.toml`.

The upstream installer scripts are never run. A copy already on your PATH is
used as is. `/toolbox` shows each tool's installed and latest version;
`/toolbox install|update|remove <name>` does the rest yourself (`remove` only
deletes what the toolbox installed). The `monica` crew member is set up for
media jobs: she loads the right skill, installs the tool when needed, and
checks results numerically.

`auto_install`: `ask` (default) asks before every install, in auto mode too;
`always` installs without asking, honoured only in auto mode; `never` refuses
and tells the agent to point you at the release page. Add any other
cargo-dist-released CLI with `[toolbox.tools.<name>] repo = "owner/name"`
(optional `version_cmd`, `doctor_cmd`). `[toolbox.tools.pixelox] mcp = true`
also registers pixelox's 8-tool MCP server (`pixelox mcp serve --allow
<project>`) once it is installed; ferraus's 192-tool server is never
registered by default (far over a local model's tool budget). A skill of the
same name on disk (for example a newer ferraus plugin under
`~/.claude/plugins`) replaces the bundled copy. `[toolbox]` is a trusted
section: an untrusted project config cannot set it.

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
| `[scrutiny]`/`[verification]` (`check_command` runs a shell command) | `[learning]`, `[crew]` |
| `[toolbox]` (its `dir` goes on PATH; `tools` name downloads) | `permissions.deny` (only restricts) |
| `permissions.allow`, `skills.extra_dirs` | rest of `[defaults]`, `[skills]` |
| `defaults.mode` (could force `auto`) | |
| `defaults.local_model_dir`, and any `[models]` entry or `defaults.model` that loads a local GGUF (llama.cpp parses the file in-process) | |

When anything is dropped you get a startup notice listing it. If you trust the
repo, run **`/trust`** (remembered per-project in `~/.rocinante/trust.toml`) and
restart — the full config then applies. Your **user-wide**
`~/.rocinante/config.toml` is always trusted; only project configs are gated.

## Updating

`/update` in a session — or **`rocinante --update`** from the shell, which
runs the same flow and exits without starting a session — checks GitHub's
latest release and, if it's newer than the running build, downloads the
right binary for your platform, verifies its SHA-256 against the published
`SHA256SUMS`, and atomically replaces the executable — then tells you to
restart. It only ever runs when you invoke it; there is no
automatic check or background phoning home. The running session is unaffected
(the old binary keeps running until you restart). Homebrew and Scoop installs
are never touched — `/update` prints `brew upgrade rocinante` /
`scoop update rocinante` instead, since those binaries belong to the package
manager. A `cargo install`ed binary updates in place with a note that a
future `cargo install` will overwrite it.

## Uninstalling

`/uninstall` removes Rocinante. It's two-step so nothing is deleted by
accident: bare **`/uninstall`** only *previews* what would go (the binary
path, and whether your data would be kept). **`/uninstall confirm`** then
deletes the binary and exits; add **`--purge`** (`/uninstall confirm
--purge`) to also delete `~/.rocinante` (config, learned rules, trusted
projects, logs). Your data is **kept by default**, and per-project
`.rocinante/` folders (PILOT.md, BRAINBOX.md, session transcripts) are never
touched. Homebrew and Scoop installs aren't deleted — it prints
`brew uninstall rocinante` / `scoop uninstall rocinante` instead. Afterward
you can remove the install dir from your PATH (the uninstall prints where).

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
