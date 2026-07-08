//! OpenAI-compatible Chat Completions provider (`POST /chat/completions`, SSE).
//!
//! Speaks the wire protocol shared by OpenAI, OpenRouter, Groq, vLLM, and most
//! other cloud gateways. Tool calls stream as indexed fragments (id/name once,
//! arguments as string pieces); [`ChunkAssembler`] accumulates them and emits
//! whole [`ChatDelta::ToolCall`]s when `finish_reason` arrives.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    Capabilities, ChatDelta, ChatRequest, ChatStream, Message, Provider, ProviderError, Role,
    StopReason, ToolCall, ToolSchema, Usage,
    tokens::{self, TokenCalibrator},
};

pub struct OpenAiCompatProvider {
    id: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    calibrator: Arc<TokenCalibrator>,
    call_counter: AtomicU64,
}

impl OpenAiCompatProvider {
    pub fn new(
        id: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        let mut base_url = base_url.into();
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            id: id.into(),
            base_url,
            api_key: api_key.into(),
            client: reqwest::Client::new(),
            calibrator: Arc::new(TokenCalibrator::default()),
            call_counter: AtomicU64::new(0),
        }
    }

    pub fn calibrator(&self) -> &TokenCalibrator {
        &self.calibrator
    }

    fn wire_message(msg: &Message) -> Value {
        match msg.role {
            Role::System => json!({ "role": "system", "content": msg.content }),
            Role::User => json!({ "role": "user", "content": msg.content }),
            Role::Assistant => {
                // Protocol prefers `content: null` over "" alongside tool calls.
                let content = if msg.content.is_empty() && !msg.tool_calls.is_empty() {
                    Value::Null
                } else {
                    Value::String(msg.content.clone())
                };
                let mut out = json!({ "role": "assistant", "content": content });
                if !msg.tool_calls.is_empty() {
                    out["tool_calls"] = Value::Array(
                        msg.tool_calls
                            .iter()
                            .map(|c| {
                                json!({
                                    "id": c.id,
                                    "type": "function",
                                    // OpenAI wants arguments as a JSON *string*.
                                    "function": { "name": c.name, "arguments": c.arguments.to_string() },
                                })
                            })
                            .collect(),
                    );
                }
                out
            }
            Role::Tool => json!({
                "role": "tool",
                "tool_call_id": msg.tool_call_id.as_deref().unwrap_or(""),
                "content": msg.content,
            }),
        }
    }

    fn build_body(req: &ChatRequest) -> Value {
        let mut body = json!({
            "model": req.model,
            "messages": req.messages.iter().map(Self::wire_message).collect::<Vec<_>>(),
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
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
            );
        }
        if let Some(t) = req.params.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(p) = req.params.top_p {
            body["top_p"] = json!(p);
        }
        if let Some(n) = req.params.max_tokens {
            body["max_tokens"] = json!(n);
        }
        body
    }
}

/// One SSE `data:` payload from /chat/completions.
#[derive(Deserialize)]
struct WireChunk {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCallDelta>,
}

#[derive(Deserialize)]
struct WireToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireFunctionDelta>,
}

#[derive(Default, Deserialize)]
struct WireFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

/// In-progress tool call assembled from streamed fragments.
#[derive(Default)]
struct PartialCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

/// Pure delta-assembly state: feed parsed chunks in, get [`ChatDelta`]s out.
///
/// Tool-call fragments are buffered per index and flushed as whole calls when
/// `finish_reason` arrives; the final stop reason is held in `finish` so the
/// stream can emit `Done` at the `[DONE]` sentinel (after the usage chunk).
#[derive(Default)]
struct ChunkAssembler {
    calls: Vec<PartialCall>,
    saw_tool_call: bool,
    finish: Option<StopReason>,
}

impl ChunkAssembler {
    fn apply(&mut self, chunk: WireChunk, fallback_id_base: &str) -> Vec<ChatDelta> {
        let mut out = Vec::new();
        if let Some(u) = chunk.usage {
            out.push(ChatDelta::Usage(Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
            }));
        }
        let Some(choice) = chunk.choices.into_iter().next() else {
            return out;
        };
        if let Some(text) = choice.delta.content
            && !text.is_empty()
        {
            out.push(ChatDelta::Text(text));
        }
        for frag in choice.delta.tool_calls {
            self.saw_tool_call = true;
            if self.calls.len() <= frag.index {
                self.calls.resize_with(frag.index + 1, PartialCall::default);
            }
            let call = &mut self.calls[frag.index];
            if let Some(id) = frag.id
                && !id.is_empty()
            {
                call.id = Some(id);
            }
            if let Some(f) = frag.function {
                if let Some(name) = f.name {
                    call.name.push_str(&name);
                }
                if let Some(args) = f.arguments {
                    call.arguments.push_str(&args);
                }
            }
        }
        if let Some(reason) = choice.finish_reason {
            for (index, call) in self.calls.drain(..).enumerate() {
                let arguments = match serde_json::from_str(&call.arguments) {
                    Ok(v) => v,
                    Err(_) if call.arguments.trim().is_empty() => json!({}),
                    Err(_) => Value::String(call.arguments),
                };
                out.push(ChatDelta::ToolCall(ToolCall {
                    id: call
                        .id
                        .unwrap_or_else(|| format!("{fallback_id_base}_{index}")),
                    name: call.name,
                    arguments,
                }));
            }
            self.finish = Some(match reason.as_str() {
                "tool_calls" => StopReason::ToolUse,
                "stop" if self.saw_tool_call => StopReason::ToolUse,
                "stop" => StopReason::EndTurn,
                "length" => StopReason::MaxTokens,
                _ => StopReason::Other,
            });
        }
        out
    }
}

fn sse_error(err: eventsource_stream::EventStreamError<reqwest::Error>) -> ProviderError {
    match err {
        eventsource_stream::EventStreamError::Transport(e) => ProviderError::Http(e),
        other => ProviderError::Wire(other.to_string()),
    }
}

#[async_trait]
impl Provider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn caps(&self) -> Capabilities {
        Capabilities {
            native_tools: true,
            structured_output: false,
            is_local: false,
        }
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, ProviderError> {
        let body = Self::build_body(&req);
        let estimate = self.count_tokens(&req.messages, &req.tools);
        tracing::debug!(model = %req.model, estimate, "openai-compat chat request");

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let message = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status: status.as_u16(),
                message,
            });
        }

        let events = resp.bytes_stream().eventsource();
        let fallback_id_base =
            format!("call_{}", self.call_counter.fetch_add(1, Ordering::Relaxed));
        let calibrator = Arc::clone(&self.calibrator);
        let stream = async_stream::try_stream! {
            futures::pin_mut!(events);
            let mut asm = ChunkAssembler::default();
            let mut done = false;
            while let Some(event) = events.next().await {
                let event = event.map_err(sse_error)?;
                if event.data.trim() == "[DONE]" {
                    yield ChatDelta::Done(asm.finish.take().unwrap_or(StopReason::EndTurn));
                    done = true;
                    break;
                }
                if event.data.is_empty() {
                    continue;
                }
                let chunk: WireChunk = serde_json::from_str(&event.data)
                    .map_err(|e| ProviderError::Wire(format!("bad SSE chunk: {e}")))?;
                for delta in asm.apply(chunk, &fallback_id_base) {
                    if let ChatDelta::Usage(u) = &delta {
                        calibrator.observe(estimate, u.prompt_tokens);
                    }
                    yield delta;
                }
            }
            if !done {
                yield ChatDelta::Done(asm.finish.take().unwrap_or(StopReason::Other));
            }
        };

        Ok(Box::pin(stream))
    }

    fn count_tokens(&self, messages: &[Message], tools: &[ToolSchema]) -> usize {
        self.calibrator
            .correct(tokens::estimate_messages(messages, tools))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GenParams;

    fn chunk(raw: &str) -> WireChunk {
        serde_json::from_str(raw).unwrap()
    }

    #[test]
    fn body_carries_protocol_fields() {
        let req = ChatRequest {
            model: "gpt-test".into(),
            messages: vec![Message::system("be brief"), Message::user("hi")],
            tools: vec![ToolSchema {
                name: "echo".into(),
                description: "echoes".into(),
                parameters: json!({ "type": "object" }),
            }],
            params: GenParams {
                temperature: Some(0.5),
                top_p: Some(0.9),
                max_tokens: Some(128),
                num_ctx: Some(4096),
                keep_alive: Some("10m".into()),
                ..Default::default()
            },
            format: None,
        };
        let body = OpenAiCompatProvider::build_body(&req);
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
        assert_eq!(body["temperature"], json!(0.5));
        assert_eq!(body["top_p"], json!(0.9f32));
        assert_eq!(body["max_tokens"], json!(128));
        assert_eq!(
            body["messages"][0],
            json!({ "role": "system", "content": "be brief" })
        );
        assert_eq!(body["tools"][0]["function"]["name"], json!("echo"));
        // Ollama-only knobs must not leak onto the wire.
        assert!(body.get("num_ctx").is_none());
        assert!(body.get("keep_alive").is_none());
    }

    #[test]
    fn wire_message_maps_tool_roles() {
        let assistant = Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "echo".into(),
                arguments: json!({ "s": "x" }),
            }],
            tool_call_id: None,
        };
        let v = OpenAiCompatProvider::wire_message(&assistant);
        assert_eq!(v["content"], Value::Null);
        assert_eq!(v["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(
            v["tool_calls"][0]["function"]["arguments"],
            json!(r#"{"s":"x"}"#)
        );

        let v = OpenAiCompatProvider::wire_message(&Message::tool_result("call_1", "ok"));
        assert_eq!(v["role"], json!("tool"));
        assert_eq!(v["tool_call_id"], json!("call_1"));
        assert_eq!(v["content"], json!("ok"));
    }

    #[test]
    fn assembles_fragmented_tool_calls() {
        let mut asm = ChunkAssembler::default();

        let out = asm.apply(
            chunk(r#"{"choices":[{"delta":{"content":"Hello"}}]}"#),
            "call_0",
        );
        assert!(matches!(&out[..], [ChatDelta::Text(t)] if t == "Hello"));

        // Fragments: id+name first, arguments split across chunks; a second
        // call starts at index 1 while index 0 is still streaming.
        let empty = asm.apply(
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"get_weather","arguments":""}}]}}]}"#,
            ),
            "call_0",
        );
        assert!(empty.is_empty());
        assert!(
            asm.apply(
                chunk(
                    r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}},{"index":1,"id":"call_b","function":{"name":"now","arguments":"{}"}}]}}]}"#
                ),
                "call_0",
            )
            .is_empty()
        );
        assert!(
            asm.apply(
                chunk(
                    r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Oslo\"}"}}]}}]}"#
                ),
                "call_0",
            )
            .is_empty()
        );

        let out = asm.apply(
            chunk(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
            "call_0",
        );
        assert_eq!(out.len(), 2);
        match &out[0] {
            ChatDelta::ToolCall(c) => {
                assert_eq!(c.id, "call_a");
                assert_eq!(c.name, "get_weather");
                assert_eq!(c.arguments, json!({ "city": "Oslo" }));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
        match &out[1] {
            ChatDelta::ToolCall(c) => {
                assert_eq!(c.id, "call_b");
                assert_eq!(c.name, "now");
                assert_eq!(c.arguments, json!({}));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
        assert_eq!(asm.finish, Some(StopReason::ToolUse));

        // Usage arrives in a trailing chunk with empty choices.
        let out = asm.apply(
            chunk(r#"{"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":7}}"#),
            "call_0",
        );
        assert!(
            matches!(&out[..], [ChatDelta::Usage(u)] if u.prompt_tokens == 42 && u.completion_tokens == 7)
        );
    }

    #[test]
    fn maps_finish_reasons() {
        let mut asm = ChunkAssembler::default();
        asm.apply(
            chunk(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
            "b",
        );
        assert_eq!(asm.finish, Some(StopReason::EndTurn));

        let mut asm = ChunkAssembler::default();
        asm.apply(
            chunk(r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#),
            "b",
        );
        assert_eq!(asm.finish, Some(StopReason::MaxTokens));
    }

    #[test]
    fn generates_fallback_call_ids() {
        let mut asm = ChunkAssembler::default();
        asm.apply(
            chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"f","arguments":"{}"}}]}}]}"#,
            ),
            "call_7",
        );
        let out = asm.apply(
            chunk(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
            "call_7",
        );
        match &out[0] {
            ChatDelta::ToolCall(c) => assert_eq!(c.id, "call_7_0"),
            other => panic!("expected tool call, got {other:?}"),
        }
    }
}
