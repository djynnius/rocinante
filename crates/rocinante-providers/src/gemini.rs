//! Google Gemini provider (`POST /v1beta/models/{model}:streamGenerateContent`, SSE).
//!
//! Gemini has no tool-call ids: function calls arrive whole as `functionCall`
//! parts (we mint `call_g_{n}` ids), and tool results go back as
//! `functionResponse` parts keyed by function *name* — recovered by matching
//! the tool message's `tool_call_id` against the preceding assistant turn.

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

pub struct GeminiProvider {
    id: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    calibrator: Arc<TokenCalibrator>,
    call_counter: Arc<AtomicU64>,
}

impl GeminiProvider {
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
            call_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn calibrator(&self) -> &TokenCalibrator {
        &self.calibrator
    }

    fn build_body(req: &ChatRequest) -> Value {
        let mut system = String::new();
        // (role, parts); consecutive same-role contents are merged so e.g.
        // parallel function responses land in one "user" turn.
        let mut contents: Vec<(&'static str, Vec<Value>)> = Vec::new();
        for (i, msg) in req.messages.iter().enumerate() {
            let (role, parts) = match msg.role {
                Role::System => {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(&msg.content);
                    continue;
                }
                Role::User => ("user", vec![json!({ "text": msg.content })]),
                Role::Assistant => {
                    let mut parts = Vec::new();
                    if !msg.content.is_empty() {
                        parts.push(json!({ "text": msg.content }));
                    }
                    for c in &msg.tool_calls {
                        parts.push(json!({
                            "functionCall": { "name": c.name, "args": c.arguments }
                        }));
                    }
                    ("model", parts)
                }
                Role::Tool => {
                    let name = call_name_for(&req.messages[..i], msg.tool_call_id.as_deref());
                    // functionResponse.response must be an object.
                    let response = match serde_json::from_str::<Value>(&msg.content) {
                        Ok(v @ Value::Object(_)) => v,
                        _ => json!({ "result": msg.content }),
                    };
                    (
                        "user",
                        vec![json!({
                            "functionResponse": { "name": name, "response": response }
                        })],
                    )
                }
            };
            if parts.is_empty() {
                continue;
            }
            match contents.last_mut() {
                Some((last, existing)) if *last == role => existing.extend(parts),
                _ => contents.push((role, parts)),
            }
        }

        let mut body = json!({
            "contents": contents
                .into_iter()
                .map(|(role, parts)| json!({ "role": role, "parts": parts }))
                .collect::<Vec<_>>(),
        });
        if !system.is_empty() {
            body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
        }
        if !req.tools.is_empty() {
            body["tools"] = json!([{
                "functionDeclarations": req.tools
                    .iter()
                    .map(|t| json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": strip_schema_keys(&t.parameters),
                    }))
                    .collect::<Vec<_>>()
            }]);
        }
        let mut gen_cfg = serde_json::Map::new();
        if let Some(t) = req.params.temperature {
            gen_cfg.insert("temperature".into(), json!(t));
        }
        if let Some(p) = req.params.top_p {
            gen_cfg.insert("topP".into(), json!(p));
        }
        if let Some(k) = req.params.top_k {
            gen_cfg.insert("topK".into(), json!(k));
        }
        if let Some(n) = req.params.max_tokens {
            gen_cfg.insert("maxOutputTokens".into(), json!(n));
        }
        if !gen_cfg.is_empty() {
            body["generationConfig"] = Value::Object(gen_cfg);
        }
        body
    }
}

/// Gemini keys function responses by NAME, not id: find the name behind a
/// harness call id in the preceding assistant turns.
fn call_name_for(messages: &[Message], call_id: Option<&str>) -> String {
    let Some(id) = call_id else {
        return "unknown".into();
    };
    messages
        .iter()
        .rev()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| m.tool_calls.iter())
        .find(|c| c.id == id)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "unknown".into())
}

/// Gemini rejects JSON-Schema metadata keys; drop `$schema` recursively.
fn strip_schema_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(k, _)| k.as_str() != "$schema")
                .map(|(k, v)| (k.clone(), strip_schema_keys(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(strip_schema_keys).collect()),
        other => other.clone(),
    }
}

/// One SSE `data:` payload from :streamGenerateContent.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireChunk {
    #[serde(default)]
    candidates: Vec<WireCandidate>,
    #[serde(default)]
    usage_metadata: Option<WireUsage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireCandidate {
    #[serde(default)]
    content: Option<WireContent>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireContent {
    #[serde(default)]
    parts: Vec<WirePart>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    function_call: Option<WireFunctionCall>,
}

#[derive(Deserialize)]
struct WireFunctionCall {
    name: String,
    #[serde(default)]
    args: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireUsage {
    #[serde(default)]
    prompt_token_count: u64,
    #[serde(default)]
    candidates_token_count: u64,
}

/// Pure chunk-assembly state: feed parsed chunks in, get [`ChatDelta`]s out.
///
/// Function calls arrive whole, so they are emitted immediately; usage counts
/// are kept updated for the stream to emit once the response ends.
struct ChunkAssembler {
    call_counter: Arc<AtomicU64>,
    usage: Usage,
    saw_tool_call: bool,
    finish: Option<StopReason>,
}

impl ChunkAssembler {
    fn new(call_counter: Arc<AtomicU64>) -> Self {
        Self {
            call_counter,
            usage: Usage::default(),
            saw_tool_call: false,
            finish: None,
        }
    }

    fn apply(&mut self, chunk: WireChunk) -> Vec<ChatDelta> {
        let mut out = Vec::new();
        if let Some(u) = chunk.usage_metadata {
            self.usage.prompt_tokens = u.prompt_token_count;
            self.usage.completion_tokens = u.candidates_token_count;
        }
        let Some(candidate) = chunk.candidates.into_iter().next() else {
            return out;
        };
        if let Some(content) = candidate.content {
            for part in content.parts {
                if let Some(text) = part.text
                    && !text.is_empty()
                {
                    out.push(ChatDelta::Text(text));
                }
                if let Some(fc) = part.function_call {
                    self.saw_tool_call = true;
                    let id = format!(
                        "call_g_{}",
                        self.call_counter.fetch_add(1, Ordering::Relaxed)
                    );
                    out.push(ChatDelta::ToolCall(ToolCall {
                        id,
                        name: fc.name,
                        arguments: fc.args,
                    }));
                }
            }
        }
        if let Some(reason) = candidate.finish_reason {
            self.finish = Some(match reason.as_str() {
                "STOP" if self.saw_tool_call => StopReason::ToolUse,
                "STOP" => StopReason::EndTurn,
                "MAX_TOKENS" => StopReason::MaxTokens,
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
impl Provider for GeminiProvider {
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
        tracing::debug!(model = %req.model, estimate, "gemini chat request");

        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url, req.model, self.api_key
        );
        let resp = self.client.post(url).json(&body).send().await?;

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
        let call_counter = Arc::clone(&self.call_counter);
        let stream = async_stream::try_stream! {
            futures::pin_mut!(events);
            let mut asm = ChunkAssembler::new(call_counter);
            while let Some(event) = events.next().await {
                let event = event.map_err(sse_error)?;
                if event.data.is_empty() {
                    continue;
                }
                let chunk: WireChunk = serde_json::from_str(&event.data)
                    .map_err(|e| ProviderError::Wire(format!("bad SSE chunk: {e}")))?;
                for delta in asm.apply(chunk) {
                    yield delta;
                }
            }
            calibrator.observe(estimate, asm.usage.prompt_tokens);
            yield ChatDelta::Usage(asm.usage);
            yield ChatDelta::Done(asm.finish.take().unwrap_or(StopReason::EndTurn));
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
    fn body_maps_roles_and_function_responses() {
        let req = ChatRequest {
            model: "gemini-test".into(),
            messages: vec![
                Message::system("be brief"),
                Message::user("weather?"),
                Message {
                    role: Role::Assistant,
                    content: "checking".into(),
                    tool_calls: vec![ToolCall {
                        id: "call_g_0".into(),
                        name: "get_weather".into(),
                        arguments: json!({ "city": "Oslo" }),
                    }],
                    tool_call_id: None,
                },
                Message::tool_result("call_g_0", r#"{"temp": 5}"#),
                Message::tool_result("call_missing", "plain text"),
            ],
            tools: vec![ToolSchema {
                name: "get_weather".into(),
                description: "weather".into(),
                parameters: json!({
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "object",
                    "properties": { "city": { "$schema": "x", "type": "string" } },
                }),
            }],
            params: GenParams {
                temperature: Some(0.5),
                top_p: Some(0.9),
                top_k: Some(40),
                max_tokens: Some(256),
                ..Default::default()
            },
            format: None,
        };
        let body = GeminiProvider::build_body(&req);

        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            json!("be brief")
        );

        let contents = body["contents"].as_array().unwrap();
        // user, model, merged user(functionResponse x2)
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], json!("user"));
        assert_eq!(contents[1]["role"], json!("model"));
        assert_eq!(contents[1]["parts"][0]["text"], json!("checking"));
        assert_eq!(
            contents[1]["parts"][1]["functionCall"]["name"],
            json!("get_weather")
        );
        let responses = contents[2]["parts"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        // Name recovered from the preceding assistant turn by call id.
        assert_eq!(
            responses[0]["functionResponse"]["name"],
            json!("get_weather")
        );
        assert_eq!(
            responses[0]["functionResponse"]["response"],
            json!({ "temp": 5 })
        );
        // Unknown id falls back to "unknown"; non-object content gets wrapped.
        assert_eq!(responses[1]["functionResponse"]["name"], json!("unknown"));
        assert_eq!(
            responses[1]["functionResponse"]["response"],
            json!({ "result": "plain text" })
        );

        // $schema keys stripped recursively from tool parameters.
        let params = &body["tools"][0]["functionDeclarations"][0]["parameters"];
        assert!(params.get("$schema").is_none());
        assert!(params["properties"]["city"].get("$schema").is_none());
        assert_eq!(params["properties"]["city"]["type"], json!("string"));

        assert_eq!(body["generationConfig"]["temperature"], json!(0.5));
        assert_eq!(body["generationConfig"]["topP"], json!(0.9f32));
        assert_eq!(body["generationConfig"]["topK"], json!(40));
        assert_eq!(body["generationConfig"]["maxOutputTokens"], json!(256));
    }

    #[test]
    fn assembles_text_and_function_calls() {
        let mut asm = ChunkAssembler::new(Arc::new(AtomicU64::new(0)));

        let out = asm.apply(chunk(
            r#"{"candidates":[{"content":{"parts":[{"text":"Let me check."}],"role":"model"}}]}"#,
        ));
        assert!(matches!(&out[..], [ChatDelta::Text(t)] if t == "Let me check."));

        let out = asm.apply(chunk(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"get_weather","args":{"city":"Oslo"}}}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":4,"totalTokenCount":13}}"#,
        ));
        match &out[..] {
            [ChatDelta::ToolCall(c)] => {
                assert_eq!(c.id, "call_g_0");
                assert_eq!(c.name, "get_weather");
                assert_eq!(c.arguments, json!({ "city": "Oslo" }));
            }
            other => panic!("expected tool call, got {other:?}"),
        }
        // STOP with a function call seen maps to ToolUse.
        assert_eq!(asm.finish, Some(StopReason::ToolUse));
        assert_eq!(asm.usage.prompt_tokens, 9);
        assert_eq!(asm.usage.completion_tokens, 4);
    }

    #[test]
    fn maps_finish_reasons() {
        let mut asm = ChunkAssembler::new(Arc::new(AtomicU64::new(0)));
        asm.apply(chunk(r#"{"candidates":[{"finishReason":"STOP"}]}"#));
        assert_eq!(asm.finish, Some(StopReason::EndTurn));

        let mut asm = ChunkAssembler::new(Arc::new(AtomicU64::new(0)));
        asm.apply(chunk(r#"{"candidates":[{"finishReason":"MAX_TOKENS"}]}"#));
        assert_eq!(asm.finish, Some(StopReason::MaxTokens));
    }

    #[test]
    fn generated_call_ids_increment() {
        let counter = Arc::new(AtomicU64::new(0));
        let mut asm = ChunkAssembler::new(Arc::clone(&counter));
        let out = asm.apply(chunk(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"a","args":{}}},{"functionCall":{"name":"b","args":{}}}],"role":"model"}}]}"#,
        ));
        match &out[..] {
            [ChatDelta::ToolCall(a), ChatDelta::ToolCall(b)] => {
                assert_eq!(a.id, "call_g_0");
                assert_eq!(b.id, "call_g_1");
            }
            other => panic!("expected two tool calls, got {other:?}"),
        }
    }
}
