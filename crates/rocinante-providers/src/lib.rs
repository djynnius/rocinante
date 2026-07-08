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
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: Some(call_id.into()),
        }
    }
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
    /// Anthropic thinking budget). Providers without support ignore it.
    pub think: Option<bool>,
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
    #[error("model not found: {0}")]
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
}
