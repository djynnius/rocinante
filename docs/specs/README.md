# Feature specs: November 2026 batch

Five ideas were evaluated against the code as of v0.22.0. Each gets a verdict here and a
full spec in its own file. Build order is at the bottom.

| # | Idea | Verdict | Value | Effort | Spec |
|---|------|---------|-------|--------|------|
| 1 | Detect `OLLAMA_NUM_PARALLEL`, fan work out to subagents for speed | **Yes, reframed** as a slot-aware scheduler. Detection is heuristic (Ollama has no API for it). | Medium | Medium (2–3 days) | [01-slot-aware-scheduler.md](01-slot-aware-scheduler.md) |
| 2 | A scrutinizing model reviews every answer before the user sees it; configurable critic model | **Yes.** ~70% exists as `[verification]`; this generalizes it to answers and hides it. | High | Medium (2–3 days) | [02-scrutiny-gate.md](02-scrutiny-gate.md) |
| 3 | `/collaborate` right pane: chat with subagents via `@name`, agents chat with each other | **Yes for the pane and `@name` steering; defer agent-to-agent chat.** Needs persistent subagents, which do not exist today. | Medium | Large (1–2 weeks) | [03-right-pane-and-collaborate.md](03-right-pane-and-collaborate.md) |
| 4 | Bake ferraus and pixelox into Rocinante so no separate install is needed | **Yes, as bundled skills plus an on-demand verified installer. No to static linking.** | High (for you) | Medium (2–3 days) | [04-toolbox-ferraus-pixelox.md](04-toolbox-ferraus-pixelox.md) |

## What the code already does (so nothing is rebuilt)

- Read-only tool calls and `task` delegations already run concurrently inside one model
  message (`crates/rocinante-core/src/agent/agent.rs:533-569`). Edits and commands stay
  sequential.
- Subagents on a *different* local model are fully serialized by a mutex and evicted after
  every call (`agent/subagent.rs:22-30`, `:180-205`). Same-model subagents have no cap at all.
  Cloud models have no cap at all.
- A post-work checker already exists: `[verification]` with its own `model` knob, up to 3
  corrective passes, `/verify` on demand (`verifier.rs`, `agent.rs:590-640`). It only runs
  after turns that edited files or ran commands, and it is visible (`✓ verified`).
- The sidebar is a fixed 30-column status panel (`rocinante-tui/src/view.rs:43-57`,
  `app.rs:29`). `/context` is a modal overlay, not a pane.
- Subagents are ephemeral: one `task` call builds an `Agent`, runs it, drops it.
- Skills are embedded with `include_str!`; large companion files with `include_bytes!` and
  written to disk on demand (`skills/mod.rs:60-110`, `scripts/vendor-*.sh`).
- `selfupdate.rs` already has GitHub release lookup, per-OS asset naming, SHA-256
  verification, extraction and binary swap.

## Facts that shaped the verdicts

- Your machine has no GPU and your Ollama tags are almost all `:cloud` (kimi-k3.1-pro, glm-5.2,
  deepseek-v4, …). `OLLAMA_NUM_PARALLEL` is irrelevant for cloud tags. What limits you is
  Ollama Cloud's concurrency: 3 concurrent requests on pay-as-you-go and Pro, 10 on Max and
  Team; extra requests queue, then are rejected when the queue is full
  ([ollama.com/pricing](https://ollama.com/pricing)).
- Local Ollama defaults: `OLLAMA_NUM_PARALLEL` is 1; `OLLAMA_MAX_LOADED_MODELS` is 3 × GPUs (3
  on CPU); `OLLAMA_MAX_QUEUE` is 512; KV memory scales with `num_ctx × num_parallel`
  ([docs.ollama.com/faq](https://docs.ollama.com/faq)). There is no endpoint that reports
  `num_parallel`; `/api/ps` reports loaded models with `size_vram` and the runner's
  `context_length`.
- ferraus is 55 MB, pixelox 43 MB, rocinante 21 MB. ferraus's MCP server exposes 192 tools
  (one per command); pixelox's exposes 8 meta-tools. Rocinante warns above 25 tools and
  CONTRIBUTING treats tool count as a budget.
- A single model response cannot be split across parallel workers. Parallelism only helps
  when the main model fans a task out into independent subtasks, and local models are weak at
  decomposing and merging. The honest speed wins are: not over-subscribing, not thrashing
  VRAM, telling the model how many slots it has, and overlapping work that is already
  independent.

## Recommended build order

1. **Scrutiny gate (02).** Highest quality-per-effort; mostly core; extends code that exists.
2. **Toolbox + ferraus/pixelox skills (04).** Independent of everything else; reuses
   `selfupdate.rs`.
3. **Slot-aware scheduler (01).** The scrutiny critic and the crew both call models
   concurrently, so build the lane/permit API before 03. Can be built before 02 if preferred;
   02 only needs the `acquire()` call site.
4. **Right-pane framework (03, part A).** Small; unlocks 03 part B and future panes (diff,
   files).
5. **Crew: persistent members + `@name` (03, part B).** Largest item; ship without
   agent-to-agent messaging first.
6. **Agent-to-agent messaging (03, part C).** Only if B proves useful in practice.

Each spec ends with a test list and open questions. Config keys are proposed; keep
`deny_unknown_fields` and add serde aliases where a key is renamed.
