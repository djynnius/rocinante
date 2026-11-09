# Spec 04: Toolbox — ferraus and pixelox without a manual install

## Goal

Any model running in Rocinante can edit audio (ferraus) and images (pixelox) the way it can
already build three.js pages: the know-how ships inside the binary, and the tool binary is
fetched, verified and installed on first use after one yes from the user. The user never
reads an install page.

## Options considered

| Option | Verdict | Why |
|---|---|---|
| **A. Static-link the ferraus/pixelox crates into rocinante** | No | Binary grows from 21 MB to ~120 MB (ferraus 55 MB, pixelox 43 MB); drags in ONNX Runtime, codecs, font shaping and their platform quirks (glibc 2.38+ for `ort`, musl builds without ML, MSVC CRT) into Rocinante's release matrix; three repos' release cadences become one; every Rocinante user pays for features most never use. |
| **B. Bundle the skills, install the CLI on demand** | **Yes** | Both tools are built for agents through `--json` CLIs, their skills are already SKILL.md files, and Rocinante already embeds skills (`skills/mod.rs:312-355`), writes companion files on demand (`write_bundled_files`), and has verified GitHub-release download code (`selfupdate.rs`). |
| **C. Auto-register their MCP servers** | pixelox optional, ferraus no | `ferraus mcp serve` exposes 192 tools (one per command): far over Rocinante's 25-tool warning and fatal for a local model's calling accuracy. `pixelox mcp serve` exposes 8 meta-tools (`pixelox_run`, `_schema`, `_info`, `_preview`, `_detect`, `_jobs`, …) which is fine, but `pixelox_preview` returns an image and `Message.content` is text-only, so the main benefit is lost. Bash + `--json` is the path the skills were written for. |

## Design (option B)

### 1. Bundled skills

Vendor these SKILL.md files and their references into `crates/rocinante-core/src/skills/builtin/`
with a `scripts/vendor-skills.sh` pinned to release tags (same pattern as `vendor-three.sh`):

| From | Skills | Companion files |
|---|---|---|
| `djynnius/ferraus@v0.1.4` | `ferraus`, `ferraus-mix`, `ferraus-master`, `ferraus-vocals`, `ferraus-stems`, `ferraus-beats`, `ferraus-mix-review` | `ferraus-mix/references/*` (72 KB), `ferraus-mix/scripts/bands.py`, `ferraus-master/references/*`, `ferraus-beats/references/*`, `ferraus-mix-review/references/*` |
| `djynnius/pixelox@v0.4.0` | `pixelox`, `pixelox-retouch` | none |

Companion files go through the existing `BundledFile`/`write_bundled_files` mechanism (the
skill body's relative links are rewritten to the files root at load, as threejs does).
Context cost: one index line per skill (9 lines); bodies load on demand. Skip `evals/`.

Precedence: discovery already shadows by name with later tiers winning
(`skills/mod.rs:198`, `add_skill:293`), and built-ins are the lowest tier, so a user who
installed the ferraus plugin under `~/.claude/plugins` keeps their (possibly newer) copy.
Log the shadowing at debug level.

### 2. Skill frontmatter extension: `requires`

```yaml
---
name: ferraus
description: …
requires: [ferraus]          # Rocinante-only; other agents ignore unknown keys
---
```

`SkillFrontmatter` gains `requires: Option<Vec<String>>` (the parser does not deny unknown
fields, so Claude Code compatibility is unaffected). When the `skill` tool loads a skill
whose `requires` names a tool that is not on PATH or in `~/.rocinante/bin`, the skill body
is returned with a header:

> `ferraus` is not installed. Call `toolbox` with `{"action":"install","name":"ferraus"}`
> before using the commands below. The user will be asked to approve the download.

### 3. The `toolbox` tool and registry (`rocinante-core/src/toolbox.rs`)

One tool with an action enum (CONTRIBUTING: prefer one tool with actions over several):

```json
{ "action": "status | install | update | remove", "name": "ferraus | pixelox" }
```

`ToolKind::Execute` so `normal` mode prompts and `deny` rules apply. The permission summary
is explicit: `Install ferraus 0.1.4 (55 MB) from github.com/djynnius/ferraus into
~/.rocinante/bin`. In `auto` mode it proceeds unless denied.

Built-in registry (compile-time, extendable from config):

```rust
pub struct ToolSpec {
    name: &'static str,          // "ferraus"
    repo: &'static str,          // "djynnius/ferraus"
    version_cmd: &'static [&'static str],  // ["ferraus", "--version"]
    doctor_cmd: Option<&'static [&'static str]>, // ["ferraus", "doctor", "--json"]
    bin_names: &'static [&'static str],   // ["ferraus", "ferraus-plughost"]
}
```

```toml
[toolbox]
dir = "~/.rocinante/bin"            # install target; prepended to PATH for bash/skills
auto_install = "ask"                # ask | never | always (always only honoured in auto mode)
[toolbox.tools.mytool]              # user additions: same shape, repo + bin names
repo = "someone/mytool"
```

Install procedure (reuse `selfupdate.rs` helpers, generalized to take a repo and asset name):

1. `GET https://api.github.com/repos/<repo>/releases/latest` → tag.
2. Both projects release with cargo-dist, so fetch `dist-manifest.json` from the release:
   it lists every artifact with its target triple and SHA-256. Pick the asset for
   `(os, arch)` preferring glibc on Linux and falling back to the musl build when
   `ldd --version` reports glibc < 2.38 (ferraus/pixelox's own installers do the same).
3. Download the archive, verify the SHA-256 from the manifest (or the `.sha256` sidecar),
   extract `bin_names` into `toolbox.dir`, `chmod +x`, run `doctor_cmd` if present, record
   `{name, version, installed_at}` in `~/.rocinante/toolbox.toml`.
4. Never run the upstream `*-installer.sh`/`.ps1` scripts: download the archive directly.

`status` reports installed version vs latest. `update` reinstalls when newer. `remove`
deletes the binaries and the record. `/toolbox` slash command maps to the same actions for
the user (`/toolbox`, `/toolbox install ferraus`, `/toolbox update`).

### 4. PATH injection

The `bash` tool spawns `bash -c` / `powershell -Command` with the inherited environment
(`tools/bash.rs:151-172`); add `toolbox.dir` to the front of `PATH` for those children and
for `version_cmd`/`doctor_cmd`. Windows: binaries are `.exe`; `PATH` separator `;`.

### 5. Optional MCP for pixelox

```toml
[toolbox.tools.pixelox]
mcp = true   # registers `pixelox mcp serve --allow <cwd>` when installed
```

Adds 8 tools behind the normal permission system. Default off. No ferraus MCP by default; a
curated `include` list of ≤ 12 ferraus tools is possible with the existing `[mcp.<name>]
include = […]` filter if someone wants it.

### 6. Optional crew member

A ninth built-in profile mirroring `camina`/`avasarala`, so delegation works for media jobs:

```
monica — Media producer: audio and image work with ferraus and pixelox; runs the CLI,
         loads the matching skill, checks results numerically (compare/verify).
tools: read, grep, glob, bash, write, skill   max_turns: 30
```

(Monica Stuart, the documentarian, fits the homage; `naomi`/`amos` are taken.) Optional; the
main model can also load the skills itself.

### 7. First-run notice and docs

Startup notice when neither tool is installed and the user has audio/image files in the
project: none. Keep it quiet; the skill header does the prompting at the moment of need.
GUIDE.md gains "Toolbox: external CLIs Rocinante can install for you" with the trust note
(release checksums; user approval; `auto_install = never` to forbid).

## Security notes

- Downloads are pinned to a GitHub repo baked into the binary (or the user's own config,
  which is a trusted-config key: ignore `[toolbox.tools]` from an untrusted project config,
  like `mcp` and `lsp`).
- Checksum verification is mandatory; mismatch aborts and removes the partial file.
- The tool binaries themselves never prompt and never overwrite without `--overwrite`
  (their own contracts), and their skills already tell the model to use `--dry-run` and to
  ask before `-y` model downloads.

## Files

- `skills/builtin/{ferraus*,pixelox*}.md` + `builtin/ferraus-mix/…` references;
  `scripts/vendor-skills.sh`; `skills/mod.rs`: new entries, `requires`, header injection.
- new `core/src/toolbox.rs` (+ `tools/toolbox.rs` tool); `selfupdate.rs`: extract
  `download_verified(repo, tag, asset) -> PathBuf` and `dist_manifest()` helpers.
- `tools/bash.rs`: PATH prefix. `config/schema.rs`: `[toolbox]`; `config/trust.rs`: strip
  `toolbox.tools` when untrusted.
- `tui/app.rs`, `cli/repl.rs`: `/toolbox`. `config/load.rs`: `monica` (optional).
- `docs/GUIDE.md`, `README.md` feature bullet.

## Tests

- Frontmatter: `requires` parsed; absent → `None`; unknown keys still ignored.
- Skill load with missing tool returns the header; with the tool present, no header.
- Registry asset selection per `(os, arch, glibc)` against a fixture `dist-manifest.json`
  from a real ferraus release; checksum mismatch aborts and cleans up.
- `toolbox status` output; `toolbox.toml` round-trip.
- PATH prefix reaches the bash child (`bash -c 'echo $PATH'` fixture).
- Untrusted project config cannot add `[toolbox.tools]`.
- Shadowing: an on-disk `ferraus` skill wins over the built-in.
- Scripted `--no-tui` run: ask for a stem split in a temp dir → the model loads `ferraus`,
  calls `toolbox install` (approved), runs `ferraus doctor --json`.

## Open questions

1. Pin the vendored skills to specific tags or track `main`? Recommendation: tags, bumped
   with a `scripts/vendor-skills.sh` run per Rocinante release.
2. Should `toolbox` also cover `ollama` itself (first-run "Ollama is not running / not
   installed")? Out of scope here but the registry shape allows it.
