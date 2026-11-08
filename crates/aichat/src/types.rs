//! Normalised chat model shared by all providers (task A1).
//!
//! The model is dialect-neutral: providers translate [`ChatRequest`] into their wire format and
//! translate their stream back into [`StreamEvent`]s. Tool results live as [`Part::ToolResult`]
//! inside a [`Role::User`] message (the Anthropic shape); the OpenAI builder fans them out into
//! `role: "tool"` messages. Provider-specific content that must be replayed verbatim (Anthropic
//! thinking / redacted_thinking blocks) is carried as [`Part::Opaque`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Wire dialect of an endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    /// OpenAI chat-completions (`POST {base}/chat/completions`), also Ollama, LM Studio, OpenRouter.
    OpenAiChat,
    /// Anthropic Messages (`POST {base}/v1/messages`).
    Anthropic,
}

/// Speaker of a message. The system prompt is not a message: see [`ChatRequest::system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

/// One piece of message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    /// A tool call made by the assistant. `args` is the parsed JSON arguments object.
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    /// The app's answer to a tool call (sent in a user message).
    ToolResult {
        call_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    /// A provider-specific block replayed verbatim to the same dialect and dropped for others
    /// (e.g. Anthropic `thinking` with its `signature`, or `redacted_thinking`).
    Opaque {
        dialect: Dialect,
        raw: Value,
    },
}

impl Part {
    pub fn text(t: impl Into<String>) -> Self {
        Part::Text { text: t.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub parts: Vec<Part>,
}

impl ChatMessage {
    pub fn user_text(t: impl Into<String>) -> Self {
        ChatMessage { role: Role::User, parts: vec![Part::text(t)] }
    }
    pub fn assistant_text(t: impl Into<String>) -> Self {
        ChatMessage { role: Role::Assistant, parts: vec![Part::text(t)] }
    }
}

/// A tool offered to the model. `parameters` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// A dialect-neutral chat request. Tools are emitted sorted by name so the request bytes are
/// stable regardless of registration order (helps prompt caching and golden tests).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    #[serde(default)]
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    /// Required by Anthropic; sent as `max_tokens` / `max_completion_tokens` to OpenAI.
    pub max_output_tokens: u32,
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    ContentFilter,
    /// Anything else the provider reported (raw value, truncated to 64 chars).
    Other(String),
}

/// Normalised streaming event.
///
/// Tool calls: `ToolCallStart` then zero or more `ToolCallArgsDelta` (raw JSON text fragments
/// that concatenate to the arguments object; may be empty for `{}`) then `ToolCallEnd`. `idx` is
/// this crate's index of the call within the response (0, 1, ...), not the provider's index.
///
/// `Usage` is a cumulative snapshot; the last one in a stream wins. `input` is the total prompt
/// size including cached tokens (Anthropic `input_tokens + cache_read + cache_creation`, OpenAI
/// `prompt_tokens`); `cache_read` is the cached part of it.
///
/// `Done` is always the final event of a successful stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta(String),
    ToolCallStart { idx: usize, id: String, name: String },
    ToolCallArgsDelta { idx: usize, frag: String },
    ToolCallEnd { idx: usize },
    /// A complete provider-specific block to store as [`Part::Opaque`] in the assistant message,
    /// at this position relative to text and tool calls.
    Opaque { dialect: Dialect, raw: Value },
    Usage { input: u64, output: u64, cache_read: u64 },
    Done(StopReason),
}
