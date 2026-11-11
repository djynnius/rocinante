# Rocinante

**A terminal coding agent for local and cloud models, written in Rust.**
Rocinante runs one model as its main brain — an Ollama tag, a cloud model, or a
GGUF file it runs itself with its built-in llama.cpp — and lets it hand
subtasks to a crew of specialist subagents on other models. Tools, skills,
permission modes and sessions work the way you'd expect from a modern coding
agent, in a fast full-screen terminal UI.

> *Rocinante: Don Quixote's horse — an ordinary steed carrying outsized
> ambitions, which is exactly the point.*

![The Rocinante chat view, with the live sidebar](rocinante-inner.png)

## Install

Linux and macOS:

```sh
curl -fsSL https://djynnius.github.io/rocinante/install.sh | sh
```

Windows (PowerShell):

```powershell
powershell -c "irm https://djynnius.github.io/rocinante/install.ps1 | iex"
```

Both check SHA-256 checksums against the GitHub release, never need sudo, and
accept `ROCINANTE_VERSION=v1.1.0` to pin a version and `ROCINANTE_INSTALL_DIR`
to choose where it goes. Package managers:

```sh
brew install djynnius/tap/rocinante      # macOS and Linux
scoop install https://raw.githubusercontent.com/djynnius/rocinante/main/scoop/rocinante.json
```

Already installed? `rocinante --update` (or `/update` inside it) fetches the
latest release.

## Quick start

```sh
rocinante                 # interactive agent in the current project
rocinante -c              # resume where you left off
rocinante ask "hello"     # one-shot answer, no UI
```

On first launch Rocinante picks a model from your Ollama server and remembers
it. Any other model works too:

```sh
rocinante --model anthropic/claude-opus-4-8     # cloud: just set ANTHROPIC_API_KEY
rocinante --model local/Qwen3.5-4B-Q4_0         # a GGUF file, run in-process
```

Inside, type `/localmodel` to pick a GGUF model from a folder (the first time it
asks where your models are), `/model` to switch models with the conversation
kept, and Shift+Tab to cycle `normal` → `auto` → `plan` mode.

## What it does

- **[Local models, nothing to install](local-models.html)** — llama.cpp is
  built in: point `/localmodel` at a folder of GGUF files, or at one file.
- **[A crew of specialists](subagents.html)** — naomi explores, miller
  researches, alex plans, bobbie reviews, amos debugs, holden (the captain)
  makes the hard calls. Talk to any of them with `@name`; `/collaborate` shows
  who is doing what.
- **[Critical thinking on tap](scrutiny.html)** — a critic reviews answers and
  finished work before you see them; `/thinking` switches it on or off.
- **[Permission modes](modes.html)** — `normal` asks before edits and commands,
  `auto` runs hands-off (your deny rules still apply), `plan` only reads and
  then proposes a plan. Switch any time with Shift+Tab; it applies at once.
- **[Skills](skills.html)** — 51 built-in playbooks (research, code review,
  data science, documents, …), plus your own and Claude Code's.
- **[MCP, LSP and the toolbox](configuration-reference.html)** — MCP servers,
  language servers, and on-demand installs of the ferraus (audio) and pixelox
  (image) CLIs.
- **[Memory](memory.html)** — per-project `PILOT.md` and `BRAINBOX.md`, and
  global rules you teach it in `LESSONS.md`.

Everything else is in the user guide (left), which is also built into
Rocinante: ask it "how do I … in Rocinante?" and it answers from the same pages.

Source, issues and releases: [github.com/djynnius/rocinante](https://github.com/djynnius/rocinante).
