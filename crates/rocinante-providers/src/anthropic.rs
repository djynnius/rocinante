//! Anthropic Messages API provider (`POST /v1/messages`, SSE).
//!
//! System prompts travel as a top-level `system` field; tool calls are
//! `tool_use` content blocks on assistant turns and `tool_result` blocks on
//! user turns. Tool inputs stream as `input_json_delta` string fragments per
//! block index; [`EventAssembler`] accumulates them and emits a whole
//! [`ChatDelta::ToolCall`] at `content_block_stop`.

use std::collections::HashMap;
use std::sync::Arc;

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

/// Extended-thinking budget when `think` is on.
const THINKING_BUDGET: u32 = 8192;
/// The API rejects requests without `max_tokens`; used when params leave it unset.
const DEFAULT_MAX_TOKENS: u32 = 8192;

pub struct AnthropicProvider {
    id: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    calibrator: Arc<TokenCalibrator>,
}

impl AnthropicProvider {
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
        }
    }

    pub fn calibrator(&self) -> &TokenCalibrator {
        &self.calibrator
    }

    fn build_body(req: &ChatRequest) -> Value {
        let mut system = String::new();
        // (role, content blocks); consecutive same-role turns are merged so
        // e.g. parallel tool results land in one user message.
        let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();
        for msg in &req.messages {
            let (role, blocks) = match msg.role {
                Role::System => {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&msg.content);
                    continue;
                }
                Role::User => ("user", vec![json!({ "type": "text", "text": msg.content })]),
                Role::Assistant => {
                    let mut blocks = Vec::new();
                    if !msg.content.is_empty() {
                        blocks.push(json!({ "type": "text", "text": msg.content }));
                    }
                    for c in &msg.tool_calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": c.id,
                            "name": c.name,
                            "input": c.arguments,
                        }));
                    }
                    ("assistant", blocks)
                }
                Role::Tool => (
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                        "content": msg.content,
                    })],
                ),
            };
            if blocks.is_empty() {
                continue;
            }
            match turns.last_mut() {
                Some((last, existing)) if *last == role => existing.extend(blocks),
                _ => turns.push((role, blocks)),
            }
        }
        let messages: Vec<Value> = turns
            .into_iter()
            .map(|(role, blocks)| json!({ "role": role, "content": blocks }))
            .collect();

        let think = req.params.think == Some(true);
        // Thinking needs headroom: max_tokens must exceed the budget.
        let max_tokens = if think {
            req.params
                .max_tokens
                .unwrap_or(DEFAULT_MAX_TOKENS)
                .max(THINKING_BUDGET + 8192)
        } else {
            req.params.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)
        };
        let mut body = json!({
            "model": req.model,
            "max_tokens": max_tokens,
            "messages": messages,
            "stream": true,
        });
        if think {
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": THINKING_BUDGET });
        }
        if !system.is_empty() {
            body["system"] = json!(system);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": t.parameters,
                        })
                    })
                    .collect(),
            );
        }
        // The API rejects sampling overrides when thinking is enabled.
        if !think {
            if let Some(t) = req.params.temperature {
                body["temperature"] = json!(t);
            }
            if let Some(p) = req.params.top_p {
                body["top_p"] = json!(p);
            }
            if let Some(k) = req.params.top_k {
                body["top_k"] = json!(k);
            }
        }
        body
    }
}

/// One SSE event payload; the `data` JSON carries a matching `type` tag.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum WireEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: WireStartMessage },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: WireBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: usize, delta: WireBlockDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: WireMessageDelta,
        #[serde(default)]
        usage: Option<WireOutputUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: WireApiError },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct WireStartMessage {
    #[serde(default)]
    usage: WireInputUsage,
}

#[derive(Default, Deserialize)]
struct WireInputUsage {
    #[serde(default)]
    input_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum WireBlock {
    #[serde(rename = "text")]
    Text {
        #[serde(default)]
        text: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum WireBlockDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct WireMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireOutputUsage {
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Deserialize)]
struct WireApiError {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: String,
}

/// Tool_use block whose `input` JSON is still streaming.
struct ToolBlock {
    id: String,
    name: String,
    json: String,
}

/// Pure event-assembly state: feed parsed SSE events in, get [`ChatDelta`]s out.
#[derive(Default)]
struct EventAssembler {
    tool_blocks: HashMap<usize, ToolBlock>,
    usage: Usage,
    stop: Option<StopReason>,
}

impl EventAssembler {
    fn apply(&mut self, event: WireEvent) -> Result<Vec<ChatDelta>, ProviderError> {
        let mut out = Vec::new();
        match event {
            WireEvent::MessageStart { message } => {
                self.usage.prompt_tokens = message.usage.input_tokens;
            }
            WireEvent::ContentBlockStart {
                index,
                content_block,
            } => match content_block {
                WireBlock::ToolUse { id, name } => {
                    self.tool_blocks.insert(
                        index,
                        ToolBlock {
                            id,
                            name,
                            json: String::new(),
                        },
                    );
                }
                WireBlock::Text { text } => {
                    if !text.is_empty() {
                        out.push(ChatDelta::Text(text));
                    }
                }
                WireBlock::Thinking { thinking } => {
                    if !thinking.is_empty() {
                        out.push(ChatDelta::Thinking(thinking));
                    }
                }
                WireBlock::Unknown => {}
            },
            WireEvent::ContentBlockDelta { index, delta } => match delta {
                WireBlockDelta::TextDelta { text } => {
                    if !text.is_empty() {
                        out.push(ChatDelta::Text(text));
                    }
                }
                WireBlockDelta::InputJsonDelta { partial_json } => {
                    if let Some(block) = self.tool_blocks.get_mut(&index) {
                        block.json.push_str(&partial_json);
                    }
                }
                WireBlockDelta::ThinkingDelta { thinking } => {
                    if !thinking.is_empty() {
                        out.push(ChatDelta::Thinking(thinking));
                    }
                }
                WireBlockDelta::Unknown => {}
            },
            WireEvent::ContentBlockStop { index } => {
                if let Some(block) = self.tool_blocks.remove(&index) {
                    let arguments = match serde_json::from_str(&block.json) {
                        Ok(v) => v,
                        Err(_) if block.json.trim().is_empty() => json!({}),
                        Err(_) => Value::String(block.json),
                    };
                    out.push(ChatDelta::ToolCall(ToolCall {
                        id: block.id,
                        name: block.name,
                        arguments,
                    }));
                }
            }
            WireEvent::MessageDelta { delta, usage } => {
                if let Some(u) = usage {
                    self.usage.completion_tokens = u.output_tokens;
                }
                if let Some(reason) = delta.stop_reason {
                    self.stop = Some(match reason.as_str() {
                        "end_turn" => StopReason::EndTurn,
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::MaxTokens,
                        _ => StopReason::Other,
                    });
                }
                out.push(ChatDelta::Usage(self.usage));
            }
            WireEvent::MessageStop => {
                out.push(ChatDelta::Done(
                    self.stop.take().unwrap_or(StopReason::EndTurn),
                ));
            }
            WireEvent::Error { error } => {
                return Err(ProviderError::Api {
                    status: 200,
                    message: format!("{}: {}", error.kind, error.message),
                });
            }
            WireEvent::Ping | WireEvent::Unknown => {}
        }
        Ok(out)
    }
}

fn sse_error(err: eventsource_stream::EventStreamError<reqwest::Error>) -> ProviderError {
    match err {
        eventsource_stream::EventStreamError::Transport(e) => ProviderError::Http(e),
        other => ProviderError::Wire(other.to_string()),
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
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
        tracing::debug!(model = %req.model, estimate, "anthropic chat request");

        let resp = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
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
        let calibrator = Arc::clone(&self.calibrator);
        let stream = async_stream::try_stream! {
            futures::pin_mut!(events);
            let mut asm = EventAssembler::default();
            let mut done = false;
            while let Some(event) = events.next().await {
                let event = event.map_err(sse_error)?;
                if event.data.is_empty() {
                    continue;
                }
                let parsed: WireEvent = serde_json::from_str(&event.data)
                    .map_err(|e| ProviderError::Wire(format!("bad SSE event: {e}")))?;
                for delta in asm.apply(parsed)? {
                    if let ChatDelta::Usage(u) = &delta {
                        calibrator.observe(estimate, u.prompt_tokens);
                    }
                    if matches!(delta, ChatDelta::Done(_)) {
                        done = true;
                    }
                    yield delta;
                }
                if done {
                    break;
                }
            }
            if !done {
                yield ChatDelta::Done(asm.stop.take().unwrap_or(StopReason::Other));
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

    fn ev(raw: &str) -> WireEvent {
        serde_json::from_str(raw).unwrap()
    }

    #[test]
    fn body_lifts_system_and_maps_tool_turns() {
        let req = ChatRequest {
            model: "claude-test".into(),
            messages: vec![
                Message::system("be brief"),
                Message::user("weather?"),
                Message {
                    role: Role::Assistant,
                    content: "checking".into(),
                    tool_calls: vec![
                        ToolCall {
                            id: "toolu_1".into(),
                            name: "get_weather".into(),
                            arguments: json!({ "city": "Oslo" }),
                        },
                        ToolCall {
                            id: "toolu_2".into(),
                            name: "now".into(),
                            arguments: json!({}),
                        },
                    ],
                    tool_call_id: None,
                },
                Message::tool_result("toolu_1", "5C"),
                Message::tool_result("toolu_2", "12:00"),
            ],
            tools: vec![ToolSchema {
                name: "get_weather".into(),
                description: "weather".into(),
                parameters: json!({ "type": "object" }),
            }],
            params: GenParams::default(),
            format: None,
        };
        let body = AnthropicProvider::build_body(&req);

        assert_eq!(body["system"], json!("be brief"));
        assert_eq!(body["max_tokens"], json!(8192));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(
            body["tools"][0]["input_schema"],
            json!({ "type": "object" })
        );

        let messages = body["messages"].as_array().unwrap();
        // user, assistant, merged user(tool_result x2)
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"][0]["type"], json!("text"));
        assert_eq!(messages[1]["content"][1]["type"], json!("tool_use"));
        assert_eq!(
            messages[1]["content"][1]["input"],
            json!({ "city": "Oslo" })
        );
        let results = messages[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["type"], json!("tool_result"));
        assert_eq!(results[0]["tool_use_id"], json!("toolu_1"));
        assert_eq!(results[1]["tool_use_id"], json!("toolu_2"));
    }

    #[test]
    fn body_respects_explicit_max_tokens() {
        let req = ChatRequest {
            model: "claude-test".into(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            params: GenParams {
                max_tokens: Some(100),
                temperature: Some(0.5),
                ..Default::default()
            },
            format: None,
        };
        let body = AnthropicProvider::build_body(&req);
        assert_eq!(body["max_tokens"], json!(100));
        assert_eq!(body["temperature"], json!(0.5));
    }

    #[test]
    fn assembles_streamed_tool_use() {
        let mut asm = EventAssembler::default();

        let out = asm
            .apply(ev(
                r#"{"type":"message_start","message":{"usage":{"input_tokens":25}}}"#,
            ))
            .unwrap();
        assert!(out.is_empty());

        let out = asm
            .apply(ev(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ))
            .unwrap();
        assert!(out.is_empty());

        let out = asm
            .apply(ev(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
            ))
            .unwrap();
        assert!(matches!(&out[..], [ChatDelta::Text(t)] if t == "Hi"));

        assert!(
            asm.apply(ev(r#"{"type":"content_block_stop","index":0}"#))
                .unwrap()
                .is_empty()
        );

        assert!(
            asm.apply(ev(
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get_weather","input":{}}}"#
            ))
            .unwrap()
            .is_empty()
        );
        assert!(
            asm.apply(ev(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\""}}"#
            ))
            .unwrap()
            .is_empty()
        );
        assert!(
            asm.apply(ev(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":":\"Oslo\"}"}}"#
            ))
            .unwrap()
            .is_empty()
        );

        let out = asm
            .apply(ev(r#"{"type":"content_block_stop","index":1}"#))
            .unwrap();
        match &out[..] {
            [ChatDelta::ToolCall(c)] => {
                assert_eq!(c.id, "toolu_1");
                assert_eq!(c.name, "get_weather");
                assert_eq!(c.arguments, json!({ "city": "Oslo" }));
            }
            other => panic!("expected tool call, got {other:?}"),
        }

        let out = asm
            .apply(ev(
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}}"#,
            ))
            .unwrap();
        assert!(
            matches!(&out[..], [ChatDelta::Usage(u)] if u.prompt_tokens == 25 && u.completion_tokens == 12)
        );

        let out = asm.apply(ev(r#"{"type":"message_stop"}"#)).unwrap();
        assert!(matches!(&out[..], [ChatDelta::Done(StopReason::ToolUse)]));
    }

    #[test]
    fn maps_stop_reasons_and_errors() {
        let mut asm = EventAssembler::default();
        asm.apply(ev(r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":1}}"#))
            .unwrap();
        let out = asm.apply(ev(r#"{"type":"message_stop"}"#)).unwrap();
        assert!(matches!(&out[..], [ChatDelta::Done(StopReason::MaxTokens)]));

        assert!(asm.apply(ev(r#"{"type":"ping"}"#)).unwrap().is_empty());

        let err = asm
            .apply(ev(
                r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
            ))
            .unwrap_err();
        assert!(matches!(err, ProviderError::Api { message, .. } if message.contains("busy")));
    }
}
