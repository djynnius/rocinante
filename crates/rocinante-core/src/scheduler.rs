//! Model-call concurrency budget. Every model request (main agent, subagents,
//! crew members, the critic, compaction, brainbox, lessons) holds a permit
//! from the lane its model runs in, so Rocinante never sends a backend more
//! concurrent requests than it serves. Ollama Cloud queues and then rejects
//! the overflow with 429; a local runner queues it invisibly. Waiting here is
//! FIFO and never fails.
//!
//! Lanes: one per loaded local model (`num_parallel` slots), one per Ollama
//! server for every `:cloud` tag (`cloud_concurrency`), one per other
//! provider (`concurrency`). Separately, a residency gate keeps
//! different-model local subagents from thrashing VRAM (`max_loaded_models`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use rocinante_providers::Provider;
use rocinante_providers::ollama::{OllamaProvider, PsModel, is_cloud_tag};

use crate::agent::events::{AgentEvent, EventSender};
use crate::config::{Config, ProviderConfig, Slots};

/// Leading text of the briefing sentence, so a model switch can find and
/// replace it in the system prompt.
pub const PARALLELISM_PREFIX: &str = "Parallelism: ";

/// Upper bound on a probed `num_parallel`; a wild `/api/ps` ratio is noise.
const MAX_PROBED_SLOTS: usize = 32;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum LaneKey {
    /// One lane per loaded local model: slots = num_parallel.
    LocalModel { provider: String, model: String },
    /// One lane per Ollama server for all `:cloud` tags.
    OllamaCloud { provider: String },
    /// One lane per non-Ollama provider.
    Provider { provider: String },
}

impl LaneKey {
    /// Sidebar label: `cloud`, `local`, or the provider name.
    pub fn short(&self) -> String {
        match self {
            LaneKey::LocalModel { .. } => "local".into(),
            LaneKey::OllamaCloud { .. } => "cloud".into(),
            LaneKey::Provider { provider } => provider.clone(),
        }
    }

    /// Briefing wording.
    pub fn describe(&self) -> String {
        match self {
            LaneKey::LocalModel { model, .. } => format!("the local model {model}"),
            LaneKey::OllamaCloud { .. } => {
                "Ollama Cloud (one budget shared by every :cloud model)".into()
            }
            LaneKey::Provider { provider } => format!("the {provider} provider"),
        }
    }
}

/// The scheduling identity of a model: who serves it, the tag, and the
/// context window it requests (for the local slot probe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    /// Provider name from config (`Provider::id`).
    pub provider: String,
    /// Served by an Ollama server (local runner or `:cloud` passthrough).
    pub ollama: bool,
    pub model: String,
    pub num_ctx: Option<u32>,
}

impl ModelRef {
    pub fn new(provider: &dyn Provider, model: &str, num_ctx: Option<u32>) -> Self {
        Self {
            provider: provider.id().to_string(),
            ollama: provider.caps().is_local,
            model: model.to_string(),
            num_ctx,
        }
    }

    /// Runs on this machine's Ollama runner, holding VRAM/RAM while loaded.
    pub fn is_resident_local(&self) -> bool {
        self.ollama && !is_cloud_tag(&self.model)
    }

    pub fn lane(&self) -> LaneKey {
        if !self.ollama {
            LaneKey::Provider {
                provider: self.provider.clone(),
            }
        } else if is_cloud_tag(&self.model) {
            LaneKey::OllamaCloud {
                provider: self.provider.clone(),
            }
        } else {
            LaneKey::LocalModel {
                provider: self.provider.clone(),
                model: self.model.clone(),
            }
        }
    }
}

/// Lane sizing for one configured provider.
#[derive(Debug, Clone)]
struct ProviderPolicy {
    /// Ollama servers only: where `/api/ps` lives.
    base_url: Option<String>,
    num_parallel: Slots,
    cloud_concurrency: usize,
    max_loaded_models: Slots,
    concurrency: usize,
}

impl Default for ProviderPolicy {
    fn default() -> Self {
        Self {
            base_url: None,
            num_parallel: Slots::Auto,
            cloud_concurrency: 3,
            max_loaded_models: Slots::Auto,
            concurrency: 4,
        }
    }
}

struct Lane {
    sem: Arc<Semaphore>,
    budget: AtomicUsize,
    used: AtomicUsize,
    /// Slot count is final (fixed, from the environment, or probed once).
    settled: AtomicBool,
}

impl Lane {
    fn new(budget: usize, settled: bool) -> Self {
        let budget = budget.max(1);
        Self {
            sem: Arc::new(Semaphore::new(budget)),
            budget: AtomicUsize::new(budget),
            used: AtomicUsize::new(0),
            settled: AtomicBool::new(settled),
        }
    }

    /// Grow immediately; shrink by retiring free permits (held ones retire
    /// as they are released and simply are not re-issued beyond the cap).
    fn resize(&self, new: usize) {
        let new = new.max(1);
        let old = self.budget.swap(new, Ordering::SeqCst);
        if new > old {
            self.sem.add_permits(new - old);
        } else if new < old {
            self.sem.forget_permits(old - new);
        }
    }
}

/// Residency gate for one Ollama server: how many local models may be
/// loaded at once, learned per session when `max_loaded_models = "auto"`.
struct Residency {
    gate: Arc<Semaphore>,
    max_loaded: AtomicUsize,
    fixed: bool,
    /// (main, sub) pairs observed to fit in memory together.
    fits: Mutex<HashSet<(String, String)>>,
}

pub struct Scheduler {
    policies: HashMap<String, ProviderPolicy>,
    /// `OLLAMA_NUM_PARALLEL` from Rocinante's own environment.
    env_num_parallel: Option<usize>,
    lanes: Mutex<HashMap<LaneKey, Arc<Lane>>>,
    residency: Mutex<HashMap<String, Arc<Residency>>>,
    /// Frontend channel for `SchedulerStats`.
    events: Mutex<Option<EventSender>>,
    /// The main agent's model; its lane is reported in `SchedulerStats`.
    main: Mutex<Option<ModelRef>>,
    rejected_429: AtomicBool,
}

impl Scheduler {
    pub fn new(config: &Config) -> Arc<Self> {
        let env = std::env::var("OLLAMA_NUM_PARALLEL")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n >= 1);
        Arc::new(Self::from_parts(config, env))
    }

    fn from_parts(config: &Config, env_num_parallel: Option<usize>) -> Self {
        let policies = config
            .providers
            .iter()
            .map(|(name, p)| {
                let policy = match p {
                    ProviderConfig::Ollama {
                        base_url,
                        num_parallel,
                        cloud_concurrency,
                        max_loaded_models,
                    } => ProviderPolicy {
                        base_url: Some(base_url.clone()),
                        num_parallel: *num_parallel,
                        cloud_concurrency: (*cloud_concurrency).max(1) as usize,
                        max_loaded_models: *max_loaded_models,
                        ..Default::default()
                    },
                    ProviderConfig::Openai { concurrency, .. }
                    | ProviderConfig::Anthropic { concurrency, .. }
                    | ProviderConfig::Gemini { concurrency, .. } => ProviderPolicy {
                        concurrency: (*concurrency).max(1) as usize,
                        ..Default::default()
                    },
                };
                (name.clone(), policy)
            })
            .collect();
        Self {
            policies,
            env_num_parallel,
            lanes: Mutex::new(HashMap::new()),
            residency: Mutex::new(HashMap::new()),
            events: Mutex::new(None),
            main: Mutex::new(None),
            rejected_429: AtomicBool::new(false),
        }
    }

    /// Route `SchedulerStats` to the frontend's channel.
    pub fn set_events(&self, events: EventSender) {
        *self.events.lock().unwrap() = Some(events);
        self.emit_stats();
    }

    /// The main agent's model; its lane is what the sidebar reports.
    pub fn set_main(&self, main: &ModelRef) {
        self.lane(&main.lane());
        *self.main.lock().unwrap() = Some(main.clone());
        self.emit_stats();
    }

    /// The main agent's model, once set.
    pub fn main_model(&self) -> Option<ModelRef> {
        self.main.lock().unwrap().clone()
    }

    fn policy(&self, provider: &str) -> ProviderPolicy {
        self.policies.get(provider).cloned().unwrap_or_default()
    }

    fn lane(&self, key: &LaneKey) -> Arc<Lane> {
        let mut lanes = self.lanes.lock().unwrap();
        if let Some(lane) = lanes.get(key) {
            return Arc::clone(lane);
        }
        let (budget, settled) = match key {
            LaneKey::LocalModel { provider, .. } => match self.policy(provider).num_parallel {
                Slots::Fixed(n) => (n as usize, true),
                Slots::Auto => match self.env_num_parallel {
                    Some(n) => (n, true),
                    None => (1, false),
                },
            },
            LaneKey::OllamaCloud { provider } => (self.policy(provider).cloud_concurrency, true),
            LaneKey::Provider { provider } => (self.policy(provider).concurrency, true),
        };
        let lane = Arc::new(Lane::new(budget, settled));
        lanes.insert(key.clone(), Arc::clone(&lane));
        lane
    }

    /// Slots in the lane `m` runs in.
    pub fn budget(&self, m: &ModelRef) -> usize {
        self.lane(&m.lane()).budget.load(Ordering::SeqCst)
    }

    /// Free slots in the lane `m` runs in right now.
    pub fn available(&self, m: &ModelRef) -> usize {
        self.lane(&m.lane()).sem.available_permits()
    }

    /// A slot if one is free now; never waits.
    pub fn try_acquire(self: &Arc<Self>, m: &ModelRef) -> Option<Permit> {
        let lane = self.lane(&m.lane());
        let permit = Arc::clone(&lane.sem).try_acquire_owned().ok()?;
        Some(self.issue(m, lane, permit))
    }

    /// Wait (FIFO) for a slot. Never fails.
    pub async fn acquire(self: &Arc<Self>, m: &ModelRef) -> Permit {
        let lane = self.lane(&m.lane());
        let permit = Arc::clone(&lane.sem)
            .acquire_owned()
            .await
            .expect("scheduler semaphores are never closed");
        self.issue(m, lane, permit)
    }

    fn issue(
        self: &Arc<Self>,
        m: &ModelRef,
        lane: Arc<Lane>,
        permit: OwnedSemaphorePermit,
    ) -> Permit {
        lane.used.fetch_add(1, Ordering::SeqCst);
        // The first completed call to an unsettled local lane probes the
        // server for its real slot count.
        let probe = match (m.lane(), m.num_ctx) {
            (LaneKey::LocalModel { .. }, Some(num_ctx)) if !lane.settled.load(Ordering::SeqCst) => {
                self.policy(&m.provider).base_url.map(|base_url| SlotProbe {
                    provider: m.provider.clone(),
                    base_url,
                    model: m.model.clone(),
                    num_ctx,
                })
            }
            _ => None,
        };
        self.emit_stats();
        Permit {
            _permit: Some(permit),
            lane,
            sched: Arc::clone(self),
            probe,
        }
    }

    /// `(lane label, used, budget)` of the main lane.
    pub fn main_stats(&self) -> Option<(String, u32, u32)> {
        let key = self.main.lock().unwrap().as_ref()?.lane();
        let lane = self.lane(&key);
        Some((
            key.short(),
            lane.used.load(Ordering::SeqCst) as u32,
            lane.budget.load(Ordering::SeqCst) as u32,
        ))
    }

    fn emit_stats(&self) {
        let Some(events) = self.events.lock().unwrap().clone() else {
            return;
        };
        if let Some((lane, used, budget)) = self.main_stats() {
            events.send(AgentEvent::SchedulerStats { lane, used, budget });
        }
    }

    /// A request in `m`'s lane was rejected with `status`. A 429 from Ollama
    /// Cloud means the plan's concurrency is lower than `cloud_concurrency`
    /// (or something else shares the account); remembered for the startup
    /// notice.
    pub fn note_rejected(&self, m: &ModelRef, status: u16) {
        if status == 429
            && matches!(m.lane(), LaneKey::OllamaCloud { .. })
            && !self.rejected_429.swap(true, Ordering::SeqCst)
        {
            tracing::warn!(model = %m.model, "Ollama Cloud rejected a request (429)");
            crate::state::note_cloud_429();
        }
    }

    /// The briefing sentence: how many subagents may usefully run at once.
    /// Same lane as the main model: its budget minus the main model's own
    /// slot. Different lane: that lane's whole budget.
    pub fn parallelism_note(&self, main: &ModelRef, sub: Option<&ModelRef>) -> String {
        let main_lane = main.lane();
        let sub_lane = sub.map(ModelRef::lane).unwrap_or_else(|| main_lane.clone());
        let budget = self.lane(&sub_lane).budget.load(Ordering::SeqCst);
        let n = if sub_lane == main_lane {
            budget.saturating_sub(1).max(1)
        } else {
            budget
        };
        parallelism_sentence(n, &sub_lane.describe())
    }

    /// Gate a different-model local subagent run against the main model's
    /// residency. `None` when no gate applies (cloud or same model).
    pub async fn residency(
        self: &Arc<Self>,
        main: Option<&ModelRef>,
        sub: &ModelRef,
    ) -> Option<ResidencyGuard> {
        if !sub.is_resident_local() || main.is_some_and(|m| m.model == sub.model) {
            return None;
        }
        let residency = self.residency_for(&sub.provider);
        let permit = Arc::clone(&residency.gate)
            .acquire_owned()
            .await
            .expect("residency semaphore is never closed");
        let main_local = main.filter(|m| m.is_resident_local() && m.provider == sub.provider);
        let pair = main_local.map(|m| (m.model.clone(), sub.model.clone()));
        let fits = pair
            .as_ref()
            .is_some_and(|p| residency.fits.lock().unwrap().contains(p));
        let max_loaded = residency.max_loaded.load(Ordering::SeqCst);
        // Evict only to protect a resident main model that is not known to
        // fit alongside this one.
        let evict = main_local.is_some() && !fits && !(residency.fixed && max_loaded >= 2);
        let probe = match (&pair, residency.fixed, fits) {
            (Some(pair), false, false) => {
                self.policy(&sub.provider)
                    .base_url
                    .map(|base_url| ResidencyProbe {
                        residency: Arc::clone(&residency),
                        base_url,
                        provider: sub.provider.clone(),
                        pair: pair.clone(),
                    })
            }
            _ => None,
        };
        Some(ResidencyGuard {
            _permit: permit,
            evict,
            probe,
        })
    }

    fn residency_for(&self, provider: &str) -> Arc<Residency> {
        let mut map = self.residency.lock().unwrap();
        Arc::clone(map.entry(provider.to_string()).or_insert_with(|| {
            let (max_loaded, fixed) = match self.policy(provider).max_loaded_models {
                Slots::Fixed(n) => (n as usize, true),
                Slots::Auto => (1, false),
            };
            Arc::new(Residency {
                gate: Arc::new(Semaphore::new(max_loaded.saturating_sub(1).max(1))),
                max_loaded: AtomicUsize::new(max_loaded),
                fixed,
                fits: Mutex::new(HashSet::new()),
            })
        }))
    }
}

fn parallelism_sentence(n: usize, lane: &str) -> String {
    if n <= 1 {
        format!(
            "{PARALLELISM_PREFIX}subagents on {lane} run one at a time; delegations issued together wait and run in turn."
        )
    } else {
        format!(
            "{PARALLELISM_PREFIX}up to {n} subagents may run at once on {lane}. Issue independent delegations together in one message, but do not issue more than {n}; extra ones wait for a slot."
        )
    }
}

/// Swap the parallelism sentence in a system prompt (append when absent).
pub fn replace_parallelism_line(system: &str, note: &str) -> String {
    let mut out = String::with_capacity(system.len() + note.len());
    let mut replaced = false;
    for (i, line) in system.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if !replaced && line.starts_with(PARALLELISM_PREFIX) {
            out.push_str(note);
            replaced = true;
        } else {
            out.push_str(line);
        }
    }
    if !replaced {
        out.push('\n');
        out.push_str(note);
    }
    out
}

/// Slots a local model has, read off `/api/ps` after a request that asked
/// for `num_ctx`: Ollama allocates `num_ctx × num_parallel` of context per
/// runner. `None` when the reading is unusable (model gone, odd ratio).
/// A CPU-only runner (no VRAM) is always 1.
pub fn slots_from_ps(models: &[PsModel], model: &str, num_ctx: u32) -> Option<usize> {
    let bare = |n: &str| n.strip_suffix(":latest").unwrap_or(n).to_string();
    let entry = models.iter().find(|m| bare(&m.name) == bare(model))?;
    if entry.size_vram == 0 {
        return Some(1);
    }
    let ctx = entry.context_length?;
    let num_ctx = u64::from(num_ctx);
    if num_ctx == 0 || ctx < num_ctx || ctx % num_ctx != 0 {
        return None;
    }
    Some(((ctx / num_ctx) as usize).min(MAX_PROBED_SLOTS))
}

struct SlotProbe {
    provider: String,
    base_url: String,
    model: String,
    num_ctx: u32,
}

/// A model slot; dropping it frees the slot.
pub struct Permit {
    _permit: Option<OwnedSemaphorePermit>,
    lane: Arc<Lane>,
    sched: Arc<Scheduler>,
    probe: Option<SlotProbe>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        drop(self._permit.take());
        self.lane.used.fetch_sub(1, Ordering::SeqCst);
        if let Some(probe) = self.probe.take()
            && !self.lane.settled.swap(true, Ordering::SeqCst)
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let lane = Arc::clone(&self.lane);
            let sched = Arc::clone(&self.sched);
            handle.spawn(async move {
                let ollama = OllamaProvider::new(probe.provider, probe.base_url);
                match ollama.ps().await {
                    Ok(models) => {
                        let slots =
                            slots_from_ps(&models, &probe.model, probe.num_ctx).unwrap_or(1);
                        tracing::info!(model = %probe.model, slots, "local model slots probed");
                        lane.resize(slots);
                        sched.emit_stats();
                    }
                    Err(e) => tracing::debug!(error = %e, "slot probe failed; keeping 1"),
                }
            });
        }
        self.sched.emit_stats();
    }
}

struct ResidencyProbe {
    residency: Arc<Residency>,
    base_url: String,
    provider: String,
    pair: (String, String),
}

/// Held for a whole different-model local subagent run.
pub struct ResidencyGuard {
    _permit: OwnedSemaphorePermit,
    /// Send `keep_alive = "0"` so the subagent's model unloads after each
    /// call instead of evicting the main model.
    pub evict: bool,
    probe: Option<ResidencyProbe>,
}

impl Drop for ResidencyGuard {
    fn drop(&mut self) {
        let Some(probe) = self.probe.take() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // Main model still resident after the subagent's model loaded: both
        // fit, so this pair stops evicting for the rest of the session.
        handle.spawn(async move {
            let ollama = OllamaProvider::new(probe.provider, probe.base_url);
            if let Ok(models) = ollama.ps().await
                && models.iter().any(|m| m.name == probe.pair.0)
            {
                probe.residency.fits.lock().unwrap().insert(probe.pair);
                probe.residency.max_loaded.fetch_max(2, Ordering::SeqCst);
                tracing::info!("main and subagent models fit together; eviction off for the pair");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn config(toml: &str) -> Config {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.toml");
        std::fs::write(&project, toml).unwrap();
        crate::config::load_from(&dir.path().join("none.toml"), &project).unwrap()
    }

    fn ollama(model: &str) -> ModelRef {
        ModelRef {
            provider: "ollama".into(),
            ollama: true,
            model: model.into(),
            num_ctx: Some(8192),
        }
    }

    #[test]
    fn lane_keys_group_cloud_split_local_and_name_providers() {
        assert_eq!(
            ollama("kimi-k3:cloud").lane(),
            ollama("glm-5.2:cloud").lane()
        );
        assert_eq!(
            ollama("gemma4:31b-cloud").lane(),
            ollama("glm-5.2:cloud").lane()
        );
        assert_ne!(ollama("qwen3:8b").lane(), ollama("llama3:8b").lane());
        assert_ne!(ollama("qwen3:8b").lane(), ollama("glm-5.2:cloud").lane());
        let anthropic = ModelRef {
            provider: "anthropic".into(),
            ollama: false,
            model: "claude-opus-4-8".into(),
            num_ctx: None,
        };
        assert_eq!(
            anthropic.lane(),
            LaneKey::Provider {
                provider: "anthropic".into()
            }
        );
        assert_eq!(anthropic.lane().short(), "anthropic");
        assert_eq!(ollama("kimi-k3:cloud").lane().short(), "cloud");
        assert_eq!(ollama("qwen3:8b").lane().short(), "local");
    }

    #[test]
    fn budgets_come_from_config_env_and_defaults() {
        let sched = Scheduler::from_parts(&config(""), None);
        assert_eq!(sched.budget(&ollama("kimi-k3:cloud")), 3, "cloud default");
        assert_eq!(sched.budget(&ollama("qwen3:8b")), 1, "local default");

        let sched = Scheduler::from_parts(
            &config(
                "[providers.ollama]\ntype = \"ollama\"\ncloud_concurrency = 10\nnum_parallel = 4\n",
            ),
            None,
        );
        assert_eq!(sched.budget(&ollama("kimi-k3:cloud")), 10);
        assert_eq!(sched.budget(&ollama("qwen3:8b")), 4);

        let sched = Scheduler::from_parts(&config(""), Some(2));
        assert_eq!(sched.budget(&ollama("qwen3:8b")), 2, "OLLAMA_NUM_PARALLEL");
    }

    #[test]
    fn slots_config_accepts_auto_or_positive_integers() {
        let c = config(
            "[providers.ollama]\ntype = \"ollama\"\nnum_parallel = \"auto\"\nmax_loaded_models = 2\n",
        );
        match &c.providers["ollama"] {
            ProviderConfig::Ollama {
                num_parallel,
                max_loaded_models,
                cloud_concurrency,
                ..
            } => {
                assert_eq!(*num_parallel, Slots::Auto);
                assert_eq!(*max_loaded_models, Slots::Fixed(2));
                assert_eq!(*cloud_concurrency, 3);
            }
            other => panic!("{other:?}"),
        }
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.toml");
        std::fs::write(
            &project,
            "[providers.ollama]\ntype = \"ollama\"\nnum_parallel = 0\n",
        )
        .unwrap();
        assert!(crate::config::load_from(&dir.path().join("none.toml"), &project).is_err());
    }

    #[tokio::test]
    async fn main_streaming_leaves_two_cloud_slots_and_the_third_waits() {
        let sched = Arc::new(Scheduler::from_parts(&config(""), None));
        let main = ollama("kimi-k3:cloud");
        let _main_stream = sched.acquire(&main).await;
        let a = sched.acquire(&ollama("glm-5.2:cloud")).await;
        let _b = sched.acquire(&ollama("glm-5.2:cloud")).await;
        assert_eq!(sched.available(&main), 0);
        assert!(
            sched.try_acquire(&main).is_none(),
            "fourth request must wait"
        );

        let sched2 = Arc::clone(&sched);
        let waiter = tokio::spawn(async move {
            let _c = sched2.acquire(&ollama("deepseek-v4-pro:cloud")).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "third subagent waits for a slot");
        drop(a);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter completes after a release")
            .unwrap();
    }

    #[tokio::test]
    async fn permit_released_on_drop_even_mid_stream() {
        let sched = Arc::new(Scheduler::from_parts(&config(""), None));
        let m = ollama("kimi-k3:cloud");
        let fut = {
            let sched = Arc::clone(&sched);
            let m = m.clone();
            async move {
                let _p = sched.acquire(&m).await;
                std::future::pending::<()>().await;
            }
        };
        let handle = tokio::spawn(fut);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(sched.available(&m), 2);
        handle.abort(); // a cancelled stream drops its permit
        let _ = handle.await;
        assert_eq!(sched.available(&m), 3);
    }

    #[test]
    fn stats_report_the_main_lane() {
        let sched = Arc::new(Scheduler::from_parts(&config(""), None));
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        sched.set_events(EventSender::new(tx));
        sched.set_main(&ollama("kimi-k3:cloud"));
        let p = sched.try_acquire(&ollama("glm-5.2:cloud")).unwrap();
        drop(p);
        let mut seen = Vec::new();
        while let Ok(AgentEvent::SchedulerStats { lane, used, budget }) = rx.try_recv() {
            seen.push((lane, used, budget));
        }
        assert!(seen.contains(&("cloud".into(), 1, 3)), "{seen:?}");
        assert_eq!(seen.last(), Some(&("cloud".into(), 0, 3)));
    }

    #[test]
    fn ps_ratio_gives_slots_and_odd_ratios_fall_through() {
        let gpu = |ctx| PsModel {
            name: "qwen3:8b".into(),
            size_vram: 5_000_000_000,
            context_length: Some(ctx),
        };
        assert_eq!(slots_from_ps(&[gpu(32768)], "qwen3:8b", 8192), Some(4));
        assert_eq!(slots_from_ps(&[gpu(8192)], "qwen3:8b", 8192), Some(1));
        assert_eq!(slots_from_ps(&[gpu(12000)], "qwen3:8b", 8192), None);
        assert_eq!(slots_from_ps(&[gpu(4096)], "qwen3:8b", 8192), None);
        assert_eq!(slots_from_ps(&[gpu(32768)], "other:1b", 8192), None);
        let cpu = PsModel {
            name: "qwen3:8b".into(),
            size_vram: 0,
            context_length: Some(32768),
        };
        assert_eq!(
            slots_from_ps(&[cpu], "qwen3:8b", 8192),
            Some(1),
            "no GPU evidence"
        );
        let latest = PsModel {
            name: "llama3:latest".into(),
            size_vram: 1,
            context_length: Some(16384),
        };
        assert_eq!(slots_from_ps(&[latest], "llama3", 8192), Some(2));
    }

    #[test]
    fn lane_resize_adds_and_retires_permits() {
        let lane = Lane::new(1, false);
        lane.resize(4);
        assert_eq!(lane.sem.available_permits(), 4);
        lane.resize(2);
        assert_eq!(lane.sem.available_permits(), 2);
        assert_eq!(lane.budget.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn briefing_counts_the_main_slot_only_on_a_shared_lane() {
        let sched = Scheduler::from_parts(&config(""), None);
        let main = ollama("kimi-k3:cloud");
        let note = sched.parallelism_note(&main, None);
        assert!(note.starts_with(PARALLELISM_PREFIX));
        assert!(note.contains("up to 2 subagents"), "{note}");
        assert!(note.contains("Ollama Cloud"), "{note}");

        // Local main, cloud subagents: the cloud lane's whole budget.
        let note = sched.parallelism_note(&ollama("qwen3:8b"), Some(&main));
        assert!(note.contains("up to 3 subagents"), "{note}");

        // Single-slot local lane.
        let note = sched.parallelism_note(&ollama("qwen3:8b"), None);
        assert!(note.contains("one at a time"), "{note}");
    }

    #[test]
    fn parallelism_line_is_replaced_or_appended() {
        let sys = "rules\nModels available:\n- a\nParallelism: up to 2 subagents\nmore";
        let out = replace_parallelism_line(sys, "Parallelism: one at a time");
        assert_eq!(
            out,
            "rules\nModels available:\n- a\nParallelism: one at a time\nmore"
        );
        let out = replace_parallelism_line("rules", "Parallelism: x");
        assert_eq!(out, "rules\nParallelism: x");
    }

    #[tokio::test]
    async fn residency_gates_only_different_local_models() {
        let sched = Arc::new(Scheduler::from_parts(&config(""), None));
        let main = ollama("qwen3:8b");
        assert!(
            sched
                .residency(Some(&main), &ollama("qwen3:8b"))
                .await
                .is_none()
        );
        assert!(
            sched
                .residency(Some(&main), &ollama("kimi-k3:cloud"))
                .await
                .is_none(),
            "cloud subagents never touch VRAM"
        );
        let guard = sched
            .residency(Some(&main), &ollama("llama3:8b"))
            .await
            .unwrap();
        assert!(guard.evict, "auto starts at 1 loaded model: evict");
        // A cloud main has nothing resident to protect.
        let cloud_main = ollama("kimi-k3:cloud");
        drop(guard);
        let guard = sched
            .residency(Some(&cloud_main), &ollama("llama3:8b"))
            .await
            .unwrap();
        assert!(!guard.evict);
    }

    #[tokio::test]
    async fn learned_fit_stops_eviction_for_the_pair() {
        let sched = Arc::new(Scheduler::from_parts(&config(""), None));
        let residency = sched.residency_for("ollama");
        residency
            .fits
            .lock()
            .unwrap()
            .insert(("qwen3:8b".into(), "llama3:8b".into()));
        let guard = sched
            .residency(Some(&ollama("qwen3:8b")), &ollama("llama3:8b"))
            .await
            .unwrap();
        assert!(!guard.evict, "resident pair keeps both loaded");

        // Fixed max_loaded_models >= 2 never evicts.
        let sched = Arc::new(Scheduler::from_parts(
            &config("[providers.ollama]\ntype = \"ollama\"\nmax_loaded_models = 3\n"),
            None,
        ));
        let guard = sched
            .residency(Some(&ollama("qwen3:8b")), &ollama("llama3:8b"))
            .await
            .unwrap();
        assert!(!guard.evict);
        assert_eq!(sched.residency_for("ollama").gate.available_permits(), 1);
    }
}
