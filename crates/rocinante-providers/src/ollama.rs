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

    /// List locally available models (name, size in bytes).
    pub async fn list_models(&self) -> Result<Vec<(String, u64)>, ProviderError> {
        #[derive(Deserialize)]
        struct Tags {
            models: Vec<TagModel>,
        }
        #[derive(Deserialize)]
        struct TagModel {
            name: String,
            #[serde(default)]
            size: u64,
        }
        let tags: Tags = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(tags.models.into_iter().map(|m| (m.name, m.size)).collect())
    }
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
        if let Some(think) = req.params.think {
            body["think"] = json!(think);
        }
        if let Some(fmt) = &req.format {
            body["format"] = fmt.clone();
        }

        let estimate = self.count_tokens(&req.messages, &req.tools);
        tracing::debug!(model = %req.model, estimate, num_ctx = ?req.params.num_ctx, "ollama chat request");

        let resp = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let message = resp.text().await.unwrap_or_default();
            if status.as_u16() == 404 && message.contains("not found") {
                return Err(ProviderError::ModelNotFound(req.model));
            }
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
            });
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
