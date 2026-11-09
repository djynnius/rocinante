# Spec 01: Slot-aware scheduler (parallelism budget)

## Goal

Make Rocinante run as many model calls concurrently as the backend can actually serve, and no
more: subagent fan-outs, the scrutiny critic, background compaction and brainbox updates all
draw from one budget. Tell the main model what that budget is so it fans out sensibly.

## What this is not

It does not split one response across workers and it does not raise a local model's token
rate. On a CPU-only box the budget for a local model is 1 and nothing here helps; the win
there comes from cloud subagents and the cloud concurrency budget.

## Current behaviour (what changes)

| Today | File | Problem |
|---|---|---|
| `LocalModelGate` is a `Mutex<()>`: subagents on a different local model run one at a time | `agent/subagent.rs:22-30`, `:180-187` | A GPU with room for two models still serializes. |
| Different-model local subagents send `keep_alive = "0"` | `subagent.rs:203-205` | Eviction after every call; a chain of three `naomi @ qwen3:8b` calls reloads qwen three times. |
| Same-model local subagents: no cap | `subagent.rs` | With `OLLAMA_NUM_PARALLEL=1` Ollama queues them server-side; harmless but invisible. |
| Cloud: no cap | everywhere | Ollama Cloud allows 3 concurrent (pay-as-you-go, Pro) or 10 (Max, Team); overflow queues, then **rejects** → `subagent failed: api error (429)`. |
| Main agent's own request is not counted | `agent.rs:900` | A fan-out of 3 while the main model streams is 4 cloud requests. |
| Delegation briefing lists models and cost but not concurrency | `provider_factory.rs:213-262` | The model cannot size a fan-out. |
| Tool dispatch waits for the whole message | `agent.rs:533` | Read-only calls could start while the model is still emitting later calls. |

## Design

### 1. Lanes and permits (`rocinante-core/src/scheduler.rs`)

```rust
pub struct Scheduler { lanes: Mutex<HashMap<LaneKey, Arc<Semaphore>>>, policy: Policy }

#[derive(Hash, Eq, PartialEq, Clone)]
pub enum LaneKey {
    /// One lane per loaded local model: slots = num_parallel.
    LocalModel { provider: String, model: String },
    /// One lane per Ollama server for all `:cloud` tags: slots = cloud_concurrency.
    OllamaCloud { provider: String },
    /// One lane per non-Ollama cloud provider: slots = provider concurrency.
    Provider { provider: String },
}

impl Scheduler {
    pub async fn acquire(&self, m: &ResolvedModel) -> Permit;   // waits; never rejects
    pub fn try_acquire(&self, m: &ResolvedModel) -> Option<Permit>;
    pub fn budget(&self, m: &ResolvedModel) -> usize;            // slots in that lane
    pub fn available(&self, m: &ResolvedModel) -> usize;
}
```

`Permit` is an `OwnedSemaphorePermit` plus the lane key; dropping it releases the slot.
Holders:

- `Agent::call_model` (main agent and every subagent) holds a permit for the duration of the
  stream, released on `Done`, error or cancel.
- `one_shot_call` (verifier/critic, compaction, brainbox, lessons) holds one for the call.
- `TaskTool::run` does **not** hold a permit for the whole subagent run (that would starve the
  subagent's own calls); the subagent's `call_model` acquires per request.

Waiting is visible: when `try_acquire` fails, the caller emits
`ToolProgress { chunk: "⏳ waiting for a model slot" }` once, then `acquire().await`.

### 2. Lane sizing

```toml
[providers.ollama]
type = "ollama"
num_parallel = "auto"        # "auto" | integer; slots per loaded local model
cloud_concurrency = 3        # Ollama Cloud plan: 3 (pay-as-you-go, Pro) or 10 (Max, Team)
max_loaded_models = "auto"   # "auto" | integer; how many local models may be resident at once

[providers.anthropic]
concurrency = 4              # default for non-Ollama cloud providers
```

`num_parallel = "auto"` resolves, in order:

1. `OLLAMA_NUM_PARALLEL` in Rocinante's own environment, if set (covers people who run
   `ollama serve` from the same shell).
2. After the first request to a local model completes, `GET /api/ps`: take that model's
   `context_length` and divide by the `num_ctx` the request sent. Ollama's FAQ states that
   parallel processing "results in increasing the context size by the number of parallel
   requests", so a ratio of 4 means 4 slots. **Verify on the installed server before trusting
   this** (`curl localhost:11434/api/ps` after a chat with a known `num_ctx`); if the ratio is
   not an integer ≥ 1, fall through.
3. Default 1 (Ollama's documented default). A `Capabilities::is_local` provider with no GPU
   evidence (no `size_vram > 0` in `/api/ps`) is always 1.

Lane size is recomputed on `/model` switch and when a new local model first appears.

`max_loaded_models = "auto"` is learned: start at 1 (today's behaviour: different-model
subagents serialize and evict). After a different-model subagent run, poll `/api/ps` once; if
the main model is still resident, raise the budget to 2 and stop sending `keep_alive = "0"`
for that pair. If the main model was evicted, keep serializing and evicting. Persist nothing;
relearn per session (VRAM changes with `num_ctx`).

### 3. Budget briefing

Append to `delegation_briefing` (`provider_factory.rs:213`):

> Parallelism: up to N subagents may run at once on <lane description>. Issue independent
> delegations together in one message, but do not issue more than N; extra ones wait for a
> slot.

N = `available(main_model_lane) - 1` (the main model's own slot) for same-lane subagents, or
the subagent lane's full budget when it differs. Recompute the sentence when `/model` or
`/submodel` changes (the prompt is rebuilt on model switch already).

### 4. Early dispatch (phase 2, optional)

In `run_loop`, when a complete `ChatDelta::ToolCall` arrives mid-stream and its tool is
`ReadOnly` or `Spawn`, validate and start it immediately (`tokio::spawn` into a `JoinSet`
keyed by call index). Results are still pushed in original order after `Done`. If any call in
the message is invalid, the already-started read-only calls are harmless; their results are
discarded along with the repair round (today's `any_invalid && executable.is_empty()` rule
stands). Measured gain is one tool latency per multi-call message; skip if the complexity
looks unjustified after phase 1 ships.

### 5. UI

- Sidebar SESSION row: `slots 2/3 cloud` or `slots 1/1 local` (`available/budget` of the main
  lane), updated from a new `AgentEvent::SchedulerStats { lane: String, used: u32, budget: u32 }`
  emitted on every acquire/release (coalesce in the TUI on `Tick`).
- Permission and task cards show `⏳ waiting for a model slot` while blocked.

## Files

- new `rocinante-core/src/scheduler.rs`; wire an `Arc<Scheduler>` into `Agent::new` (replace
  the `LocalModelGate` parameter in `TaskTool::new`) and into `one_shot_call`.
- `providers/ollama.rs`: add `ps()` returning `Vec<PsModel { name, size_vram, context_length }>`
  with a fixture test.
- `config/schema.rs`: new provider fields (with defaults), `#[serde(default)]`.
- `provider_factory.rs`: briefing sentence.
- `cli/setup.rs`: build the scheduler, resolve `auto` after first call (spawned task).
- `tui/view.rs`, `app.rs`: SESSION row and waiting indicator.

## Tests

- Lane keys: two `:cloud` tags share one lane; two local tags get separate lanes; Anthropic
  gets a provider lane.
- Budget math: `cloud_concurrency = 3`, main streaming → 2 subagent permits; the third waits
  and completes after one releases (use `tokio::time::pause`).
- Permit is released on stream error and on cancel.
- `/api/ps` fixture: `context_length / num_ctx` → slots; non-integer ratio → 1.
- `max_loaded_models` learning: resident → budget 2 and no `keep_alive=0`; evicted → stays 1.
- Briefing contains the N sentence and updates on switch.
- Existing `readonly_calls_run_in_parallel_with_ordered_results` still passes.

## Open questions

1. Should `cloud_concurrency` default to 3 (safe for every plan) or be asked in first-run
   setup? Recommendation: default 3, mention `/config set cloud_concurrency 10` in the
   startup notice when a 429 is ever seen.
2. Non-Ollama providers: fixed `concurrency` or adaptive (back off on 429)? Recommendation:
   fixed now, adaptive later.
