//! Rocinante's embedded llama.cpp: GGUF models run in-process, no server and
//! nothing to install.
//!
//! One worker thread owns llama.cpp. It keeps one model and one context
//! loaded, runs requests in order, and streams deltas back over a channel.
//! The context's KV cache survives between requests, so an agent loop that
//! resends the whole conversation only decodes the new tail. Chat templating,
//! tool-call parsing and sampling are llama.cpp's own (see
//! `shim/rcn_llama.cpp`).

mod ffi;

use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use async_trait::async_trait;
use rocinante_providers::tokens::{self, TokenCalibrator};
use rocinante_providers::{
    Capabilities, ChatDelta, ChatRequest, ChatStream, Message, Provider, ProviderError, Role,
    StopReason, ToolCall, ToolSchema, Usage,
};
use serde::Deserialize;
use serde_json::{Value, json};

// Link llama.cpp (llama, ggml, common) into the final binary.
use llama_cpp_sys_2 as _;

/// Context window when neither the request nor the model config sets one.
pub const DEFAULT_NUM_CTX: u32 = 16_384;

/// How the embedded runtime runs a model.
#[derive(Debug, Clone, Default)]
pub struct LlamaSettings {
    /// CPU threads; 0 = llama.cpp's choice (the physical cores).
    pub threads: u32,
    /// Layers to offload to the GPU (Metal on Apple Silicon); `None` = all
    /// of them where offload exists, none elsewhere.
    pub gpu_layers: Option<i32>,
}

/// A model the worker currently holds, for status displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedInfo {
    pub path: PathBuf,
    /// llama.cpp's one-line description ("qwen2 7B Q4_K - Medium").
    pub desc: String,
    pub n_ctx: u32,
}

static LOADED: Mutex<Option<LoadedInfo>> = Mutex::new(None);

/// Token counts of the most recent turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TurnStats {
    /// The whole templated prompt.
    pub prompt: u64,
    /// How much of it came from the KV cache (not re-processed).
    pub cached: u64,
    pub generated: u64,
}

static LAST_TURN: Mutex<Option<TurnStats>> = Mutex::new(None);

/// Token counts of the last finished turn, if any.
pub fn last_turn() -> Option<TurnStats> {
    *LAST_TURN.lock().unwrap()
}

/// What the worker has loaded right now, if anything.
pub fn loaded() -> Option<LoadedInfo> {
    LOADED.lock().unwrap().clone()
}

/// "CPU" or "CPU + GPU offload": what this build can run models on.
pub fn backend_summary() -> &'static str {
    // ggml's CPU backend is compiled for AVX2: never initialise it on a CPU
    // that would fault on its first instruction.
    if missing_cpu_features().is_some() {
        return "unsupported CPU";
    }
    ensure_init();
    // SAFETY: plain query after backend init.
    if unsafe { ffi::rcn_gpu_offload_supported() } {
        "CPU + GPU offload"
    } else {
        "CPU"
    }
}

/// A llama.cpp provider. `req.model` is the absolute path of a GGUF file;
/// Rocinante's config layer resolves aliases and bare names to paths before a
/// request gets here.
pub struct LlamaProvider {
    id: String,
    settings: LlamaSettings,
    calibrator: Arc<TokenCalibrator>,
}

impl LlamaProvider {
    pub fn new(id: impl Into<String>, settings: LlamaSettings) -> Self {
        Self {
            id: id.into(),
            settings,
            calibrator: Arc::new(TokenCalibrator::default()),
        }
    }
}

#[async_trait]
impl Provider for LlamaProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn caps(&self) -> Capabilities {
        Capabilities {
            native_tools: true,
            structured_output: false,
            is_local: true,
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        if let Some(missing) = missing_cpu_features() {
            return Err(ProviderError::Api {
                status: 501,
                message: format!(
                    "this CPU lacks {missing}, which Rocinante's embedded llama.cpp needs \
                     (x86-64 from 2013 on) — use a model served by Ollama instead"
                ),
            });
        }
        let path = PathBuf::from(&req.model);
        if !path.is_file() {
            return Err(ProviderError::Api {
                status: 404,
                message: format!("local model file not found: {}", path.display()),
            });
        }
        let estimate = tokens::estimate_messages(&req.messages, &req.tools);
        let cstring = |s: String| {
            CString::new(s).map_err(|_| ProviderError::Wire("message contains a NUL byte".into()))
        };
        let messages_json = cstring(openai_messages(&req.messages).to_string())?;
        let tools_json = cstring(openai_tools(&req.tools).to_string())?;
        let p = &req.params;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let job = Job {
            path,
            n_ctx: p.num_ctx.unwrap_or(DEFAULT_NUM_CTX),
            threads: self.settings.threads,
            gpu_layers: self.settings.gpu_layers,
            messages_json,
            tools_json,
            // Local thinking stays explicit (`/think on`), as with Ollama.
            think: p.think == Some(true),
            temperature: p.temperature.unwrap_or(0.6),
            top_p: p.top_p.unwrap_or(0.95),
            top_k: p.top_k.map(|k| k as i32).unwrap_or(40),
            max_tokens: p.max_tokens.map(|m| m as i32).unwrap_or(0),
            tx,
            estimate,
            calibrator: Arc::clone(&self.calibrator),
        };
        worker()
            .send(job)
            .map_err(|_| ProviderError::Wire("the llama.cpp worker has stopped".into()))?;
        Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })))
    }

    fn count_tokens(&self, messages: &[Message], tools: &[ToolSchema]) -> usize {
        self.calibrator
            .correct(tokens::estimate_messages(messages, tools))
    }
}

/// The conversation as an OpenAI chat-completions `messages` array — the
/// shape llama.cpp's template layer parses.
fn openai_messages(messages: &[Message]) -> Value {
    // Tool results carry the tool's name for templates that print it.
    let mut call_names: std::collections::HashMap<&str, &str> = Default::default();
    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        match m.role {
            Role::System => out.push(json!({"role": "system", "content": m.content})),
            Role::User => out.push(json!({"role": "user", "content": m.content})),
            Role::Assistant => {
                if m.tool_calls.is_empty() {
                    out.push(json!({"role": "assistant", "content": m.content}));
                } else {
                    let calls: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|c| {
                            call_names.insert(c.id.as_str(), c.name.as_str());
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": {"name": c.name, "arguments": c.arguments.to_string()},
                            })
                        })
                        .collect();
                    out.push(
                        json!({"role": "assistant", "content": m.content, "tool_calls": calls}),
                    );
                }
            }
            Role::Tool => {
                let id = m.tool_call_id.as_deref().unwrap_or_default();
                let mut msg = json!({"role": "tool", "tool_call_id": id, "content": m.content});
                if let Some(name) = call_names.get(id) {
                    msg["name"] = json!(name);
                }
                out.push(msg);
            }
        }
    }
    Value::Array(out)
}

fn openai_tools(tools: &[ToolSchema]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    },
                })
            })
            .collect(),
    )
}

// ------------------------------------------------------------------ worker

type DeltaTx = tokio::sync::mpsc::UnboundedSender<Result<ChatDelta, ProviderError>>;

struct Job {
    path: PathBuf,
    n_ctx: u32,
    threads: u32,
    gpu_layers: Option<i32>,
    messages_json: CString,
    tools_json: CString,
    think: bool,
    temperature: f32,
    top_p: f32,
    top_k: i32,
    max_tokens: i32,
    tx: DeltaTx,
    estimate: usize,
    calibrator: Arc<TokenCalibrator>,
}

fn ensure_init() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        // SAFETY: called once, before any other shim call.
        unsafe { ffi::rcn_init(Some(log_cb)) };
    });
}

fn worker() -> &'static mpsc::Sender<Job> {
    static WORKER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("llama".into())
            .spawn(move || run_worker(rx))
            .expect("spawn the llama.cpp worker thread");
        tx
    })
}

extern "C" fn log_cb(level: i32, text: *const c_char) {
    if text.is_null() {
        return;
    }
    // SAFETY: llama.cpp hands a NUL-terminated string valid for this call.
    let text = unsafe { std::ffi::CStr::from_ptr(text) }.to_string_lossy();
    let text = text.trim_end();
    if text.is_empty() {
        return;
    }
    // ggml_log_level: 2 = info, 3 = warn, 4 = error.
    match level {
        4 => tracing::warn!(target: "llama", "{text}"),
        _ => tracing::debug!(target: "llama", "{text}"),
    }
}

/// The model and context the worker holds. Raw pointers: only the worker
/// thread ever touches them.
struct Held {
    path: PathBuf,
    gpu_layers: i32,
    model: *mut ffi::RcnModel,
    n_ctx_train: u32,
    session: *mut ffi::RcnSession,
    /// The n_ctx / threads the session was built with.
    session_key: (u32, i32),
}

impl Drop for Held {
    fn drop(&mut self) {
        // SAFETY: both pointers came from the shim and are freed once, the
        // session (which borrows the model) first.
        unsafe {
            if !self.session.is_null() {
                ffi::rcn_session_free(self.session);
            }
            ffi::rcn_model_free(self.model);
        }
        *LOADED.lock().unwrap() = None;
    }
}

fn run_worker(rx: mpsc::Receiver<Job>) {
    ensure_init();
    let mut held: Option<Held> = None;
    while let Ok(job) = rx.recv() {
        if job.tx.is_closed() {
            continue; // cancelled while queued
        }
        if let Err(message) = prepare(&mut held, &job) {
            let _ = job.tx.send(Err(ProviderError::Api {
                status: 500,
                message,
            }));
            continue;
        }
        let h = held.as_mut().expect("prepared");
        run_turn(h, &job);
    }
}

/// Load the job's model (dropping any other) and a context of the right size.
fn prepare(held: &mut Option<Held>, job: &Job) -> Result<(), String> {
    // SAFETY: plain query after backend init.
    let offload = unsafe { ffi::rcn_gpu_offload_supported() };
    let gpu_layers = job.gpu_layers.unwrap_or(if offload { 999 } else { 0 });
    if held
        .as_ref()
        .is_some_and(|h| h.path != job.path || h.gpu_layers != gpu_layers)
    {
        *held = None; // free the old model before loading the next
    }
    if held.is_none() {
        // Tell the UI the wait is a model load.
        let _ = job.tx.send(Ok(ChatDelta::Progress { done: 0, total: 0 }));
        let path = CString::new(job.path.to_string_lossy().as_bytes())
            .map_err(|_| "model path contains a NUL byte".to_string())?;
        let mut err: *mut c_char = std::ptr::null_mut();
        // SAFETY: valid C string; err receives a shim string on failure.
        let model = unsafe { ffi::rcn_model_load(path.as_ptr(), gpu_layers, &mut err) };
        if model.is_null() {
            // SAFETY: err is null or a shim string.
            return Err(unsafe { ffi::take_string(err) }
                .unwrap_or_else(|| format!("could not load {}", job.path.display())));
        }
        // SAFETY: model is live.
        let n_ctx_train = unsafe { ffi::rcn_model_n_ctx_train(model) }.max(0) as u32;
        *held = Some(Held {
            path: job.path.clone(),
            gpu_layers,
            model,
            n_ctx_train,
            session: std::ptr::null_mut(),
            session_key: (0, 0),
        });
    }
    let h = held.as_mut().expect("loaded above");
    let mut n_ctx = job.n_ctx.max(2048);
    if h.n_ctx_train > 0 {
        n_ctx = n_ctx.min(h.n_ctx_train);
    }
    let threads = if job.threads == 0 {
        // SAFETY: plain query.
        unsafe { ffi::rcn_default_threads() }.max(1)
    } else {
        job.threads as i32
    };
    if h.session.is_null() || h.session_key != (n_ctx, threads) {
        if !h.session.is_null() {
            // SAFETY: freed once, replaced below.
            unsafe { ffi::rcn_session_free(h.session) };
            h.session = std::ptr::null_mut();
        }
        let mut err: *mut c_char = std::ptr::null_mut();
        // SAFETY: model is live; err receives a shim string on failure.
        let session = unsafe { ffi::rcn_session_new(h.model, n_ctx, threads, &mut err) };
        if session.is_null() {
            // SAFETY: err is null or a shim string.
            return Err(unsafe { ffi::take_string(err) }
                .unwrap_or_else(|| "could not create a llama.cpp context".into()));
        }
        h.session = session;
        h.session_key = (n_ctx, threads);
        // SAFETY: model is live.
        let desc = unsafe { ffi::take_string(ffi::rcn_model_desc(h.model)) }.unwrap_or_default();
        *LOADED.lock().unwrap() = Some(LoadedInfo {
            path: h.path.clone(),
            desc,
            // SAFETY: session is live.
            n_ctx: unsafe { ffi::rcn_session_n_ctx(session) },
        });
    }
    Ok(())
}

/// Per-turn state the emit callback sees: the channel, plus bytes of a
/// UTF-8 character split across two tokens, held until it completes.
struct EmitState<'a> {
    tx: &'a DeltaTx,
    pending: [Vec<u8>; 2],
}

extern "C" fn emit_cb(user: *mut c_void, kind: i32, text: *const c_char, len: usize) -> bool {
    // SAFETY: `user` is the &mut EmitState run_turn passed for this call.
    let state = unsafe { &mut *(user as *mut EmitState) };
    if state.tx.is_closed() {
        return false; // the consumer dropped the stream: cancel
    }
    if kind == 2 {
        // SAFETY: the shim passes `len` readable bytes.
        let bytes = unsafe { std::slice::from_raw_parts(text as *const u8, len) };
        let progress = std::str::from_utf8(bytes).ok().and_then(|t| {
            let (done, total) = t.split_once('/')?;
            Some((done.parse().ok()?, total.parse().ok()?))
        });
        if let Some((done, total)) = progress {
            return state
                .tx
                .send(Ok(ChatDelta::Progress { done, total }))
                .is_ok();
        }
        return true;
    }
    let slot = match kind {
        0 | 1 => kind as usize,
        _ => return true,
    };
    // SAFETY: the shim passes `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(text as *const u8, len) };
    let buf = &mut state.pending[slot];
    buf.extend_from_slice(bytes);
    let valid = match std::str::from_utf8(buf) {
        Ok(s) => s.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(), // incomplete tail
        Err(_) => buf.len(),                                  // garbage: pass it on
    };
    if valid == 0 {
        return true;
    }
    let chunk: Vec<u8> = buf.drain(..valid).collect();
    let text = String::from_utf8_lossy(&chunk).into_owned();
    let delta = if slot == 0 {
        ChatDelta::Text(text)
    } else {
        ChatDelta::Thinking(text)
    };
    state.tx.send(Ok(delta)).is_ok()
}

#[derive(Deserialize)]
struct TurnResult {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    tool_calls: Vec<RawCall>,
    #[serde(default)]
    stop: String,
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    cached_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Deserialize)]
struct RawCall {
    name: String,
    arguments: String,
    #[serde(default)]
    id: String,
}

fn run_turn(h: &mut Held, job: &Job) {
    let params = ffi::RcnTurnParams {
        messages_json: job.messages_json.as_ptr(),
        tools_json: job.tools_json.as_ptr(),
        enable_thinking: job.think,
        temperature: job.temperature,
        top_p: job.top_p,
        top_k: job.top_k,
        min_p: 0.05,
        max_tokens: job.max_tokens,
        seed: rand_seed(),
    };
    let mut state = EmitState {
        tx: &job.tx,
        pending: [Vec::new(), Vec::new()],
    };
    // SAFETY: session is live and owned by this thread; params' strings
    // outlive the call; state outlives the call.
    let raw = unsafe {
        ffi::take_string(ffi::rcn_session_chat(
            h.session,
            &params,
            emit_cb,
            &mut state as *mut EmitState as *mut c_void,
        ))
    };
    // Flush a dangling partial character, if any.
    for (slot, buf) in state.pending.iter_mut().enumerate() {
        if !buf.is_empty() {
            let text = String::from_utf8_lossy(buf).into_owned();
            let _ = job.tx.send(Ok(if slot == 0 {
                ChatDelta::Text(text)
            } else {
                ChatDelta::Thinking(text)
            }));
        }
    }
    let result: TurnResult = match raw.as_deref().map(serde_json::from_str) {
        Some(Ok(r)) => r,
        Some(Err(e)) => {
            let _ = job
                .tx
                .send(Err(ProviderError::Wire(format!("llama.cpp result: {e}"))));
            return;
        }
        None => {
            let _ = job.tx.send(Err(ProviderError::Wire(
                "llama.cpp returned nothing".into(),
            )));
            return;
        }
    };
    if let Some(message) = result.error {
        let _ = job.tx.send(Err(ProviderError::Api {
            status: 500,
            message,
        }));
        return;
    }
    tracing::debug!(
        target: "llama",
        prompt = result.prompt_tokens,
        cached = result.cached_tokens,
        generated = result.completion_tokens,
        "turn done"
    );
    *LAST_TURN.lock().unwrap() = Some(TurnStats {
        prompt: result.prompt_tokens,
        cached: result.cached_tokens,
        generated: result.completion_tokens,
    });
    job.calibrator.observe(job.estimate, result.prompt_tokens);
    let has_calls = !result.tool_calls.is_empty();
    for (i, call) in result.tool_calls.into_iter().enumerate() {
        let arguments = serde_json::from_str(&call.arguments)
            .unwrap_or_else(|_| Value::String(call.arguments.clone()));
        let id = if call.id.is_empty() {
            format!("call_{i}")
        } else {
            call.id
        };
        let _ = job.tx.send(Ok(ChatDelta::ToolCall(ToolCall {
            id,
            name: call.name,
            arguments,
        })));
    }
    let _ = job.tx.send(Ok(ChatDelta::Usage(Usage {
        prompt_tokens: result.prompt_tokens,
        completion_tokens: result.completion_tokens,
    })));
    let reason = match result.stop.as_str() {
        "cancelled" => StopReason::Cancelled,
        "length" => StopReason::MaxTokens,
        _ if has_calls => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    };
    let _ = job.tx.send(Ok(ChatDelta::Done(reason)));
}

fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(42);
    nanos.wrapping_mul(2_654_435_761)
}

/// The x86 features ggml's CPU kernels were compiled for (see
/// `.cargo/config.toml`) that this CPU lacks; `None` when it has them all.
/// Loading a model without them would crash on an illegal instruction.
pub fn missing_cpu_features() -> Option<String> {
    #[cfg(target_arch = "x86_64")]
    {
        let missing: Vec<&str> = [
            ("avx", std::arch::is_x86_feature_detected!("avx")),
            ("avx2", std::arch::is_x86_feature_detected!("avx2")),
            ("fma", std::arch::is_x86_feature_detected!("fma")),
            ("f16c", std::arch::is_x86_feature_detected!("f16c")),
            ("bmi2", std::arch::is_x86_feature_detected!("bmi2")),
        ]
        .into_iter()
        .filter(|(_, ok)| !ok)
        .map(|(name, _)| name)
        .collect();
        if !missing.is_empty() {
            return Some(missing.join(", ").to_uppercase());
        }
    }
    None
}

/// Whether `path` looks like a model file this runtime can load.
pub fn is_gguf(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gguf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_become_openai_shape_with_tool_names() {
        let mut assistant = Message::assistant("");
        assistant.tool_calls.push(ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: json!({"path": "a.rs"}),
        });
        let msgs = vec![
            Message::system("sys"),
            Message::user("hi"),
            assistant,
            Message::tool_result("c1", "fn main() {}"),
        ];
        let v = openai_messages(&msgs);
        assert_eq!(v[0]["role"], "system");
        assert_eq!(v[2]["tool_calls"][0]["function"]["name"], "read");
        assert_eq!(
            v[2]["tool_calls"][0]["function"]["arguments"], r#"{"path":"a.rs"}"#,
            "arguments travel as a JSON string, OpenAI style"
        );
        assert_eq!(v[3]["role"], "tool");
        assert_eq!(v[3]["tool_call_id"], "c1");
        assert_eq!(v[3]["name"], "read");
    }

    #[test]
    fn tools_become_function_entries() {
        let v = openai_tools(&[ToolSchema {
            name: "grep".into(),
            description: "search".into(),
            parameters: json!({"type": "object"}),
        }]);
        assert_eq!(v[0]["type"], "function");
        assert_eq!(v[0]["function"]["name"], "grep");
    }

    #[test]
    fn missing_model_file_is_a_clean_error() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let p = LlamaProvider::new("local", LlamaSettings::default());
        let req = ChatRequest {
            model: "/definitely/not/here.gguf".into(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            params: Default::default(),
            format: None,
        };
        let err = rt.block_on(p.chat(req)).err().expect("error");
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn gguf_extension_check() {
        assert!(is_gguf(Path::new("/m/Qwen2.5-7B.Q4_K_M.GGUF")));
        assert!(!is_gguf(Path::new("/m/readme.md")));
    }
}
