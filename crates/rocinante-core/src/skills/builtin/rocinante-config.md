---
name: rocinante-config
description: "Create or update Rocinante's own config file (~/.rocinante/config.toml, TOML): add or change model aliases and num_ctx, define providers (ollama, openai, anthropic, gemini), subagent profiles, permission allow/deny rules, skill directories, MCP servers, LSP servers, brainbox settings, and context hygiene (summary model, tool-result pruning). Use when asked to change Rocinante configuration, add a model alias, set a context window, tune temperature, or wire up a provider, MCP server, or LSP server."
---

# Rocinante configuration

Rocinante layers config: built-in defaults → user file → project file → `ROCINANTE_*` env vars. Later wins.
The task prompt gives you the EXACT absolute path to the user config file. Use that path verbatim in every tool call. Never write `~` — the file tools do not expand it. The path is pre-computed for your operating system, so it is already correct on Windows, macOS, and Linux.

Work in this exact order:

1. **Read the file first.** Use the `read` tool with the absolute path from the task. If the file does not exist, you will create it in step 2 — do not treat a missing file as an error.
2. **Make the change.**
   - File exists: use the `edit` tool. Copy the exact existing text into old_string. To add a new section, append it — put old_string as the last existing line and new_string as that line plus the new section.
   - File does not exist: use the `write` tool with the full new content. If write fails because the directory is missing, create it with the `bash` tool (`mkdir -p` the parent directory), then write again.
3. **Copy the matching snippet below** and replace every FILL_IN. Do not invent keys that are not in the snippets.
4. **Re-read the file** with the `read` tool and check the TOML shape: section headers like `[models]` or `[mcp.name]`, strings in double quotes, numbers bare.
5. **Report** exactly what changed. Tell the user: new model aliases appear in `/model` immediately; every other section applies on the next launch.

## Snippets

**Model alias** (the most common request — alias shows in `/model` instead of the raw tag):

```toml
[models]
FILL_IN_ALIAS = { provider = "ollama", model = "FILL_IN_MODEL_TAG", num_ctx = FILL_IN_NUMBER }
# optional extras per alias: temperature = 0.7, top_p = 0.9, top_k = 40
```

Several aliases stack in one `[models]` section, one per line. If a `[models]` section already exists, add the line inside it — never create a second `[models]` header.

**Defaults**:

```toml
[defaults]
model = "FILL_IN_ALIAS_OR_TAG"   # which model starts as the main agent
mode = "normal"                  # normal | auto | plan
num_ctx = 32768                  # context window for models without their own
keep_alive = "10m"
think = false
effort = "high"                  # low | medium | high
```

**Provider — local Ollama** (only needed when the server is not at localhost:11434):

```toml
[providers.ollama]
type = "ollama"
base_url = "http://FILL_IN_HOST:11434"
```

**Provider — cloud** (anthropic | openai | gemini):

```toml
[providers.FILL_IN_NAME]
type = "FILL_IN_TYPE"                  # "anthropic" | "openai" | "gemini"
api_key_env = "FILL_IN_ENV_VAR_NAME"   # the NAME of an environment variable, e.g. "ANTHROPIC_API_KEY"
# openai type also takes: base_url = "https://FILL_IN/v1"
```

**Subagent profile**:

```toml
[agents.FILL_IN_NAME]
description = "FILL_IN what this agent is for"
model = "FILL_IN_ALIAS"
tools = ["read", "grep", "glob"]
max_turns = 15
```

**Permissions**:

```toml
[permissions]
allow = ["Bash(cargo test:*)", "Bash(git status)"]
deny  = ["Bash(rm -rf:*)", "Read(**/*.pem)", "Read(./.env)"]
```

**Skill directories**:

```toml
[skills]
extra_dirs = ["FILL_IN_ABSOLUTE_PATH"]
```

**MCP server** (set exactly one of `command` or `url`, never both):

```toml
[mcp.FILL_IN_NAME]
command = "npx"                        # stdio server: spawn a child process
args = ["-y", "FILL_IN_PACKAGE"]
# env_from = { CHILD_VAR = "HOST_ENV_VAR" }
# url = "https://FILL_IN/mcp"          # OR an HTTP server instead of command
```

**LSP server**:

```toml
[lsp.FILL_IN_LANG]
command = "FILL_IN_SERVER_BINARY"
filetypes = ["FILL_IN_EXT"]
root_markers = ["FILL_IN_MANIFEST"]
```

**Brainbox** (session memory):

```toml
[brainbox]
enabled = true
update_every_turns = 5
# model = "FILL_IN_ALIAS"    # optional cheaper model for updates
```

**Context hygiene** (compaction summaries and tool-result pruning):

```toml
[context]
# model = "FILL_IN_ALIAS"    # optional cheaper model for compaction summaries
keep_tool_turns = 3          # stub tool results older than this many turns; 0 = off
```

**Learning** (global learned rules/preferences in `~/.rocinante/LESSONS.md`):

```toml
[learning]
enabled = true
update_every_turns = 0       # 0 = capture only at session end (default)
# model = "FILL_IN_ALIAS"    # optional cheaper model for the capture pass
```

**Verification** (auto quality-check after substantial turns):

```toml
[verification]
enabled = true
auto = true                  # false = only via /verify
# model = "FILL_IN_ALIAS"    # optional cheaper checker model
# check_command = "FILL_IN"  # trusted test/build cmd (e.g. cargo test); runs in the project dir
timeout_secs = 60
```

## Rules

- Use the exact absolute path from the task prompt in every tool call. Never `~`.
- NEVER write an API key into the file. `api_key_env` takes the NAME of an environment variable, nothing else. If the user pastes a raw key (starts with `sk-`, `AIza`, or similar), refuse to store it and tell them to `export` it in their shell instead.
- Only add a cloud provider whose environment variable is actually set (check with the `bash` tool: `echo ${FILL_IN_ENV_VAR:+set}`). Rocinante refuses to load a config that names an unset key variable, which would break the whole file.
- Read the file before editing it. Never delete, rewrite, or reformat sections unrelated to the request.
- Re-read the file after every edit to verify it.
- Edit the user-wide file from the task prompt unless the user explicitly says "for this project" — then use `.rocinante/config.toml` inside the project directory instead.
- An MCP server sets exactly one of `command` or `url`.
- After any change, tell the user: model aliases show in `/model` immediately; other sections apply on the next launch.
