//! Ollama native API provider (`/api/chat`, NDJSON streaming).
//!
//! Uses the native API rather than Ollama's OpenAI-compatible `/v1` endpoint
//! because only the native API exposes `num_ctx` (per-request context size),
//! `keep_alive` (model residency), and `format` (JSON-schema constrained
//! output) — all three are load-bearing for a local-model harness.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    Capabilities, ChatDelta, ChatRequest, ChatStream, Message, Provider, ProviderError, Role,
    StopReason, ToolCall, ToolSchema, Usage,
    tokens::{self, TokenCalibrator},
};

pub struct OllamaProvider {
    id: String,
    base_url: String,
    client: reqwest::Client,
    calibrator: Arc<TokenCalibrator>,
    call_counter: AtomicU64,
    /// Cloud tags whose local stub we've already ensured this session, so
    /// the background ensure-pull runs at most once per model.
    ensured_cloud: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl OllamaProvider {
    pub fn new(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            id: id.into(),
            base_url,
            client: reqwest::Client::new(),
            calibrator: Arc::new(TokenCalibrator::default()),
            call_counter: AtomicU64::new(0),
            ensured_cloud: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    pub fn calibrator(&self) -> &TokenCalibrator {
        &self.calibrator
    }

    fn next_call_id(&self) -> String {
        format!("call_{}", self.call_counter.fetch_add(1, Ordering::Relaxed))
    }

    fn wire_message(msg: &Message) -> Value {
        let role = match msg.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut out = json!({ "role": role, "content": msg.content });
        if !msg.tool_calls.is_empty() {
            out["tool_calls"] = Value::Array(
                msg.tool_calls
                    .iter()
                    .map(|c| json!({ "function": { "name": c.name, "arguments": c.arguments } }))
                    .collect(),
            );
        }
        out
    }

    fn wire_tools(tools: &[ToolSchema]) -> Value {
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
                        }
                    })
                })
                .collect(),
        )
    }

    /// POST /api/chat and map the failure statuses; the caller handles the
    /// cloud-stub retry.
    async fn send_chat(
        &self,
        body: &Value,
        model: &str,
    ) -> Result<reqwest::Response, ProviderError> {
        let resp = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let message = resp.text().await.unwrap_or_default();
            if status.as_u16() == 404 && message.contains("not found") {
                return Err(ProviderError::ModelNotFound(model.to_string()));
            }
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
            });
        }
        Ok(resp)
    }

    /// Record that a cloud tag's stub is known-present, so the background
    /// ensure doesn't pull it again this session.
    fn mark_cloud_ensured(&self, model: &str) {
        self.ensured_cloud.lock().unwrap().insert(model.to_string());
    }

    /// Make a cloud tag's stub exist locally so `/api/tags` — and therefore
    /// the model picker — lists it from now on. Newer Ollama servers chat an
    /// unpulled cloud tag fine but never list it; a pull is what creates the
    /// stub. Runs inline at most once per model per session (~1 s on the
    /// first use, free after) and never fails the chat call.
    async fn ensure_cloud_stub(&self, model: &str) {
        let already = !self.ensured_cloud.lock().unwrap().insert(model.to_string());
        if already {
            return;
        }
        match self.pull_model(model).await {
            Ok(()) => tracing::debug!(%model, "cloud stub ensured"),
            Err(e) => {
                tracing::debug!(%model, error = %e, "cloud stub ensure-pull failed — the picker won't list this tag")
            }
        }
    }

    /// Pull a model on the server (`POST /api/pull`, blocking). Used to fetch
    /// the ~300-byte stub of a signed-in cloud tag on its first use on this
    /// machine; bounded so a hang can never wedge a turn.
    pub async fn pull_model(&self, name: &str) -> Result<(), ProviderError> {
        let pull = async {
            let resp = self
                .client
                .post(format!("{}/api/pull", self.base_url))
                .json(&json!({ "model": name, "stream": false }))
                .send()
                .await?;
            let status = resp.status();
            if !status.is_success() {
                let message = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Api {
                    status: status.as_u16(),
                    message: format!(
                        "pull of {name} failed: {message} — run 'ollama signin' on this machine if you haven't"
                    ),
                });
            }
            Ok(())
        };
        match tokio::time::timeout(std::time::Duration::from_secs(120), pull).await {
            Ok(result) => result,
            Err(_) => Err(ProviderError::Api {
                status: 0,
                message: format!("pull of {name} timed out"),
            }),
        }
    }

    /// List locally available models with the /api/tags metadata that the
    /// delegation inventory needs.
    pub async fn list_models(&self) -> Result<Vec<OllamaModel>, ProviderError> {
        #[derive(Deserialize)]
        struct Tags {
            models: Vec<TagModel>,
        }
        #[derive(Deserialize, Default)]
        struct Details {
            #[serde(default)]
            parameter_size: Option<String>,
            #[serde(default)]
            quantization_level: Option<String>,
        }
        #[derive(Deserialize)]
        struct TagModel {
            name: String,
            #[serde(default)]
            size: u64,
            #[serde(default)]
            details: Option<Details>,
        }
        let tags: Tags = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(tags
            .models
            .into_iter()
            .map(|m| {
                let details = m.details.unwrap_or_default();
                OllamaModel {
                    name: m.name,
                    size: m.size,
                    parameter_size: details.parameter_size,
                    quantization: details.quantization_level,
                }
            })
            .collect())
    }
}

/// One locally available model, as reported by /api/tags.
#[derive(Debug, Clone)]
pub struct OllamaModel {
    pub name: String,
    /// On-disk size in bytes (a VRAM-need proxy).
    pub size: u64,
    /// e.g. "8.0B".
    pub parameter_size: Option<String>,
    /// e.g. "Q4_K_M".
    pub quantization: Option<String>,
}

/// One NDJSON line from /api/chat.
#[derive(Deserialize)]
struct WireChunk {
    #[serde(default)]
    message: Option<WireMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct WireMessage {
    #[serde(default)]
    content: String,
    /// Reasoning stream from thinking models; not part of the context.
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[async_trait]
impl Provider for OllamaProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn caps(&self) -> Capabilities {
        Capabilities {
            native_tools: true,
            structured_output: true,
            is_local: true,
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        let mut options = serde_json::Map::new();
        if let Some(t) = req.params.temperature {
            options.insert("temperature".into(), json!(t));
        }
        if let Some(p) = req.params.top_p {
            options.insert("top_p".into(), json!(p));
        }
        if let Some(k) = req.params.top_k {
            options.insert("top_k".into(), json!(k));
        }
        if let Some(n) = req.params.max_tokens {
            options.insert("num_predict".into(), json!(n));
        }
        if let Some(c) = req.params.num_ctx {
            options.insert("num_ctx".into(), json!(c));
        }

        let mut body = json!({
            "model": req.model,
            "messages": req.messages.iter().map(Self::wire_message).collect::<Vec<_>>(),
            "stream": true,
            "options": options,
        });
        if !req.tools.is_empty() {
            body["tools"] = Self::wire_tools(&req.tools);
        }
        if let Some(ka) = &req.params.keep_alive {
            body["keep_alive"] = json!(ka);
        }
        if let Some(think) = wire_think(&req.params, &req.model) {
            body["think"] = think;
        }
        if let Some(fmt) = &req.format {
            body["format"] = fmt.clone();
        }

        let estimate = self.count_tokens(&req.messages, &req.tools);
        tracing::debug!(model = %req.model, estimate, num_ctx = ?req.params.num_ctx, "ollama chat request");

        // Cloud tags (`:cloud` / `-cloud`) only exist locally once their tiny
        // stub has been pulled on this machine. Older servers 404 an unpulled
        // tag — pull the stub and retry once; newer servers chat it fine but
        // still won't LIST it, so ensure the stub in the background either
        // way. Never auto-pull non-cloud (multi-GB) tags.
        let resp = match self.send_chat(&body, &req.model).await {
            Err(ProviderError::ModelNotFound(model)) if is_cloud_tag(&model) => {
                tracing::info!(%model, "cloud tag not present locally — pulling its stub");
                self.pull_model(&model).await?;
                self.mark_cloud_ensured(&model);
                self.send_chat(&body, &req.model).await?
            }
            other => other?,
        };
        if is_cloud_tag(&req.model) {
            self.ensure_cloud_stub(&req.model).await;
        }

        // NDJSON: buffer bytes, emit one WireChunk per newline.
        let byte_stream = resp.bytes_stream();
        let call_id_base = self.next_call_id();
        let calibrator = Arc::clone(&self.calibrator);
        let stream = async_stream::try_stream! {
            futures::pin_mut!(byte_stream);
            let mut buf: Vec<u8> = Vec::new();
            let mut call_seq = 0usize;
            let mut usage = Usage::default();
            let mut saw_tool_call = false;

            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk.map_err(ProviderError::Http)?;
                buf.extend_from_slice(&chunk);
                while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=nl).collect();
                    let line = &line[..line.len() - 1];
                    if line.is_empty() {
                        continue;
                    }
                    let parsed: WireChunk = serde_json::from_slice(line)
                        .map_err(|e| ProviderError::Wire(format!("bad NDJSON line: {e}")))?;

                    if let Some(err) = parsed.error {
                        Err(ProviderError::Api { status: 200, message: err })?;
                    }
                    if let Some(msg) = parsed.message {
                        if let Some(thinking) = msg.thinking
                            && !thinking.is_empty()
                        {
                            yield ChatDelta::Thinking(thinking);
                        }
                        if !msg.content.is_empty() {
                            yield ChatDelta::Text(msg.content);
                        }
                        for tc in msg.tool_calls {
                            saw_tool_call = true;
                            let id = format!("{call_id_base}_{call_seq}");
                            call_seq += 1;
                            yield ChatDelta::ToolCall(ToolCall {
                                id,
                                name: tc.function.name,
                                arguments: tc.function.arguments,
                            });
                        }
                    }
                    if parsed.done {
                        if let Some(p) = parsed.prompt_eval_count {
                            usage.prompt_tokens = p;
                            calibrator.observe(estimate, p);
                        }
                        if let Some(e) = parsed.eval_count {
                            usage.completion_tokens = e;
                        }
                        yield ChatDelta::Usage(usage);
                        let stop = match parsed.done_reason.as_deref() {
                            _ if saw_tool_call => StopReason::ToolUse,
                            Some("stop") | None => StopReason::EndTurn,
                            Some("length") => StopReason::MaxTokens,
                            Some(_) => StopReason::Other,
                        };
                        yield ChatDelta::Done(stop);
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    fn count_tokens(&self, messages: &[Message], tools: &[ToolSchema]) -> usize {
        self.calibrator
            .correct(tokens::estimate_messages(messages, tools))
    }
}

/// Ollama cloud tags — models that run on ollama.com but are addressed by a
/// locally-pulled ~300-byte stub. Both suffix spellings exist in the wild
/// (`minimax-m3:cloud`, `gemma4:31b-cloud`).
fn is_cloud_tag(model: &str) -> bool {
    model.ends_with(":cloud") || model.ends_with("-cloud")
}

/// The wire value for Ollama's `think` field. Activation stays explicit
/// (`Some`) — surprise `think:true` breaks non-thinking local models.
/// Effort refines: Low forces off; the gpt-oss family takes the level as
/// a string instead of a bool.
fn wire_think(params: &crate::GenParams, model: &str) -> Option<serde_json::Value> {
    match params.think {
        Some(true) if params.effort == crate::Effort::Low => Some(serde_json::json!(false)),
        Some(true) if model.starts_with("gpt-oss") => {
            Some(serde_json::json!(params.effort.to_string()))
        }
        Some(think) => Some(serde_json::json!(think)),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChatDelta, ChatRequest, Effort, GenParams, Message};
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn cloud_tags_by_suffix_only() {
        assert!(is_cloud_tag("minimax-m3:cloud"));
        assert!(is_cloud_tag("kimi-k2.7-code:cloud"));
        assert!(is_cloud_tag("gemma4:31b-cloud"));
        assert!(!is_cloud_tag("llama3:8b"));
        assert!(!is_cloud_tag("glm-5.2"));
        assert!(!is_cloud_tag("cloudy:latest"));
    }

    /// Request counts seen by the mock Ollama server.
    struct MockState {
        pulls: usize,
        chats: usize,
        /// Old-server behavior: 404 /api/chat until a pull has been seen.
        /// New servers chat unpulled cloud tags fine (false).
        chat_404_until_pull: bool,
    }

    /// Minimal hand-rolled Ollama mock (no dev-deps): optionally 404s
    /// /api/chat until a /api/pull has been seen, then streams one NDJSON
    /// text chunk + done.
    async fn spawn_mock(state: std::sync::Arc<Mutex<MockState>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let state = std::sync::Arc::clone(&state);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    // Read the head, then the content-length'd body.
                    let head_end = loop {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let content_length: usize = head
                        .lines()
                        .find_map(|l| {
                            let low = l.to_ascii_lowercase();
                            low.strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    while buf.len() - head_end < content_length {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                    let (status_line, body) = {
                        let mut st = state.lock().unwrap();
                        match path.as_str() {
                            "/api/pull" => {
                                st.pulls += 1;
                                ("HTTP/1.1 200 OK", r#"{"status":"success"}"#.to_string())
                            }
                            "/api/chat" if st.chat_404_until_pull && st.pulls == 0 => {
                                st.chats += 1;
                                (
                                    "HTTP/1.1 404 Not Found",
                                    r#"{"error":"model not found, try pulling it first"}"#
                                        .to_string(),
                                )
                            }
                            "/api/chat" => {
                                st.chats += 1;
                                (
                                    "HTTP/1.1 200 OK",
                                    concat!(
                                        r#"{"message":{"role":"assistant","content":"hi"},"done":false}"#,
                                        "\n",
                                        r#"{"done":true,"done_reason":"stop","prompt_eval_count":1,"eval_count":1}"#,
                                        "\n"
                                    )
                                    .to_string(),
                                )
                            }
                            _ => ("HTTP/1.1 404 Not Found", String::new()),
                        }
                    };
                    let resp = format!(
                        "{status_line}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn chat_req(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            params: GenParams::default(),
            format: None,
        }
    }

    fn mock_state(chat_404_until_pull: bool) -> std::sync::Arc<Mutex<MockState>> {
        std::sync::Arc::new(Mutex::new(MockState {
            pulls: 0,
            chats: 0,
            chat_404_until_pull,
        }))
    }

    #[tokio::test]
    async fn cloud_tag_not_found_pulls_stub_and_retries_once() {
        let state = mock_state(true); // old-server behavior: 404 until pulled
        let base = spawn_mock(std::sync::Arc::clone(&state)).await;
        let provider = OllamaProvider::new("ollama", base);

        let mut stream = provider
            .chat(chat_req("gemma4:31b-cloud"))
            .await
            .expect("chat succeeds after auto-pull");
        let mut text = String::new();
        while let Some(delta) = stream.next().await {
            if let ChatDelta::Text(t) = delta.unwrap() {
                text.push_str(&t);
            }
        }
        assert_eq!(text, "hi");

        let st = state.lock().unwrap();
        assert_eq!(st.pulls, 1, "exactly one stub pull, no ensure repeat");
        assert_eq!(st.chats, 2, "one 404 + one retried chat");
    }

    #[tokio::test]
    async fn cloud_chat_without_stub_ensures_it_inline_once() {
        let state = mock_state(false); // new-server behavior: chat works unpulled
        let base = spawn_mock(std::sync::Arc::clone(&state)).await;
        let provider = OllamaProvider::new("ollama", base);

        let mut stream = provider
            .chat(chat_req("minimax-m3:cloud"))
            .await
            .expect("chat succeeds without a stub");
        while stream.next().await.is_some() {}

        // The ensure-pull ran inline, so the stub exists by the time chat()
        // returned — /api/tags (the picker) lists the tag from now on.
        assert_eq!(state.lock().unwrap().pulls, 1, "stub ensured once");

        // A second chat must not pull again.
        let mut stream = provider.chat(chat_req("minimax-m3:cloud")).await.unwrap();
        while stream.next().await.is_some() {}
        let st = state.lock().unwrap();
        assert_eq!(st.pulls, 1, "ensure runs at most once per session");
        assert_eq!(st.chats, 2);
    }

    #[tokio::test]
    async fn non_cloud_not_found_never_pulls() {
        let state = mock_state(true);
        let base = spawn_mock(std::sync::Arc::clone(&state)).await;
        let provider = OllamaProvider::new("ollama", base);

        let err = provider
            .chat(chat_req("llama3:8b"))
            .await
            .err()
            .expect("stays not-found");
        assert!(matches!(err, ProviderError::ModelNotFound(m) if m == "llama3:8b"));

        let st = state.lock().unwrap();
        assert_eq!(st.pulls, 0, "non-cloud tags are never auto-pulled");
        assert_eq!(st.chats, 1, "no retry");
    }

    #[test]
    fn think_stays_explicit_and_effort_refines() {
        let p = |think, effort| GenParams {
            think,
            effort,
            ..Default::default()
        };
        // No explicit think → nothing sent, even at High effort.
        assert_eq!(wire_think(&p(None, Effort::High), "qwen3:8b"), None);
        // Explicit on → true for ordinary models.
        assert_eq!(
            wire_think(&p(Some(true), Effort::High), "qwen3:8b"),
            Some(serde_json::json!(true))
        );
        // gpt-oss family takes the effort level as a string.
        assert_eq!(
            wire_think(&p(Some(true), Effort::High), "gpt-oss:20b"),
            Some(serde_json::json!("high"))
        );
        assert_eq!(
            wire_think(&p(Some(true), Effort::Medium), "gpt-oss:20b"),
            Some(serde_json::json!("medium"))
        );
        // Low effort forces thinking off even when explicitly on.
        assert_eq!(
            wire_think(&p(Some(true), Effort::Low), "qwen3:8b"),
            Some(serde_json::json!(false))
        );
        // Explicit off is passed through.
        assert_eq!(
            wire_think(&p(Some(false), Effort::High), "qwen3:8b"),
            Some(serde_json::json!(false))
        );
    }
}
