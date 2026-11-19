//! Provider abstraction: chat + streaming + tools + token accounting.
//!
//! Every model backend (Ollama native, OpenAI-compatible, Anthropic, Gemini)
//! implements [`Provider`]. The agent loop consumes a [`ChatStream`] of
//! [`ChatDelta`]s and never sees provider wire formats.

pub mod anthropic;
pub mod gemini;
pub mod ollama;
pub mod openai_compat;
pub mod tokens;

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool calls the assistant made (assistant messages only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Which call a tool-result message answers (tool messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Pictures that go with the text (user and tool messages): a browser
    /// screenshot, say. Only models that can see receive them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<Image>,
}

/// One picture, base64-encoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Image {
    /// `image/jpeg`, `image/png`, …
    pub media_type: String,
    /// Base64 of the file's bytes (no `data:` prefix).
    pub data: String,
}

impl Image {
    /// A `data:` URL, as OpenAI-compatible APIs take images.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
            images: vec![],
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
            images: vec![],
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
            images: vec![],
        }
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: Some(call_id.into()),
            images: vec![],
        }
    }
}

/// For APIs whose tool messages carry text only (Ollama, OpenAI-compatible):
/// move the images off each run of tool results into one user message
/// right after it, so the model still sees them next to the results.
pub fn lift_tool_images(messages: &[Message]) -> std::borrow::Cow<'_, [Message]> {
    if !messages
        .iter()
        .any(|m| m.role == Role::Tool && !m.images.is_empty())
    {
        return std::borrow::Cow::Borrowed(messages);
    }
    let mut out = Vec::with_capacity(messages.len() + 1);
    let mut pending: Vec<Image> = Vec::new();
    for m in messages {
        if m.role != Role::Tool && !pending.is_empty() {
            out.push(images_message(std::mem::take(&mut pending)));
        }
        let mut m = m.clone();
        if m.role == Role::Tool {
            pending.append(&mut m.images);
        }
        out.push(m);
    }
    if !pending.is_empty() {
        out.push(images_message(pending));
    }
    std::borrow::Cow::Owned(out)
}

fn images_message(images: Vec<Image>) -> Message {
    let mut m = Message::user(if images.len() == 1 {
        "(The image from the tool result above.)"
    } else {
        "(The images from the tool results above.)"
    });
    m.images = images;
    m
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Harness-assigned id; some providers (Ollama) don't supply one.
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// JSON-Schema description of a tool, as sent to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Reasoning-effort tier. Maps per provider: Anthropic thinking budget
/// (Low off / Medium 8k / High 16k), OpenAI `reasoning_effort` string,
/// Ollama gpt-oss think level. Low means "just chat" — thinking off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    #[default]
    High,
}

impl std::str::FromStr for Effort {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Effort::Low),
            "medium" => Ok(Effort::Medium),
            "high" => Ok(Effort::High),
            other => Err(format!("unknown effort `{other}` (low | medium | high)")),
        }
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub max_tokens: Option<u32>,
    /// Context window to allocate (Ollama-specific; ignored by cloud providers).
    pub num_ctx: Option<u32>,
    /// How long to keep the model resident after the request (Ollama-specific),
    /// e.g. "10m", "-1" (pinned), "0" (evict immediately).
    pub keep_alive: Option<String>,
    /// Extended thinking: Some(true) requests reasoning (Ollama `think`,
    /// Anthropic thinking budget). An explicit Some always beats `effort`.
    pub think: Option<bool>,
    /// Reasoning-effort tier; `think: Some(_)` overrides it where they clash.
    #[serde(default)]
    pub effort: Effort,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub params: GenParams,
    /// JSON Schema for constrained output (Ollama `format`); the tool-call
    /// repair pipeline's last-resort path.
    pub format: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Cancelled,
    Other,
}

#[derive(Debug, Clone)]
pub enum ChatDelta {
    Text(String),
    /// Reasoning stream from a thinking model. Display-only: never part of
    /// the conversation history.
    Thinking(String),
    /// Incremental tool-call fragment (SSE providers stream args in pieces).
    ToolCallPartial {
        index: usize,
        name: Option<String>,
        args_fragment: String,
    },
    /// Fully assembled call (Ollama emits these whole).
    ToolCall(ToolCall),
    /// Prompt-processing progress from an in-process runtime (the embedded
    /// llama.cpp): `done` of `total` new prompt tokens read. `total == 0`
    /// means the model itself is still loading. Display-only.
    Progress {
        done: u64,
        total: u64,
    },
    Usage(Usage),
    Done(StopReason),
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("wire format error: {0}")]
    Wire(String),
    #[error("model not found: {0} — pull it on this machine first (ollama pull {0})")]
    ModelNotFound(String),
}

pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatDelta, ProviderError>> + Send>>;

#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub native_tools: bool,
    pub structured_output: bool,
    pub is_local: bool,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &str;
    fn caps(&self) -> Capabilities;
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream, ProviderError>;
    /// Fast approximate token count. Must overestimate rather than under —
    /// the context manager budgets `num_ctx` off this number.
    fn count_tokens(&self, messages: &[Message], tools: &[ToolSchema]) -> usize;
    /// Whether `model` can take images. Messages' images are dropped for a
    /// model that can't (the agent says so in their place).
    async fn sees_images(&self, _model: &str) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(n: u8) -> Image {
        Image {
            media_type: "image/jpeg".into(),
            data: format!("AAA{n}"),
        }
    }

    #[test]
    fn tool_images_move_to_one_user_message_after_the_run() {
        let mut a = Message::tool_result("1", "first");
        a.images = vec![shot(1)];
        let b = Message::tool_result("2", "second");
        let mut c = Message::tool_result("3", "third");
        c.images = vec![shot(3)];
        let msgs = vec![
            Message::user("look"),
            Message::assistant(""),
            a,
            b,
            c,
            Message::assistant("done"),
        ];
        let out = lift_tool_images(&msgs);
        let roles: Vec<Role> = out.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            [
                Role::User,
                Role::Assistant,
                Role::Tool,
                Role::Tool,
                Role::Tool,
                Role::User,
                Role::Assistant
            ]
        );
        assert!(out[2..5].iter().all(|m| m.images.is_empty()));
        assert_eq!(out[5].images, vec![shot(1), shot(3)]);
    }

    #[test]
    fn trailing_tool_images_and_no_images() {
        let mut a = Message::tool_result("1", "shot");
        a.images = vec![shot(1)];
        let out = lift_tool_images(std::slice::from_ref(&a));
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].role, Role::User);

        let plain = vec![Message::user("hi")];
        assert!(matches!(
            lift_tool_images(&plain),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
