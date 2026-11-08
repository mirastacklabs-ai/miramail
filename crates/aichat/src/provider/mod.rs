//! Provider dialects (task A1): pure request builders, stateful stream parsers and error-body
//! parsing. Nothing here touches the network; `http.rs` sends what [`build_request`] returns and
//! feeds the bytes through [`crate::sse::SseParser`] into a [`StreamParser`].
//!
//! Byte stability: tools are emitted sorted by name and JSON objects are built in a fixed order,
//! so the same [`ChatRequest`] always yields the same body.

pub mod anthropic;
pub mod openai;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::http::{ErrorKind, UserFacingError};
use crate::http::MAX_SERVER_TEXT;
use crate::sse::SseEvent;
use crate::types::{ChatRequest, Dialect, StreamEvent, ToolSpec};

/// Default `anthropic-version` header (owner-specified; configurable per profile).
///
/// DOUBT: the only version documented as of this crate's writing is `2023-06-01`. If an endpoint
/// rejects `2026-10-21` with a 400 ("invalid anthropic-version"), set
/// [`ProviderOptions::anthropic_version`] to `2023-06-01` for that profile.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2026-10-21";

/// Which field carries the output-token cap in OpenAI-compatible requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    /// `max_tokens`: understood by Ollama, LM Studio, OpenRouter and most compatible servers.
    #[default]
    MaxTokens,
    /// `max_completion_tokens`: required by OpenAI's reasoning models (max_tokens is deprecated there).
    MaxCompletionTokens,
}

/// Per-profile knobs that change the wire format. Contains no secrets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderOptions {
    pub dialect: Dialect,
    /// Anthropic only.
    pub anthropic_version: String,
    /// OpenAI only.
    pub max_tokens_field: MaxTokensField,
    /// OpenAI only: send `stream_options: {include_usage: true}` (a final usage-only chunk).
    /// Turn off for old servers that reject unknown fields.
    pub include_usage: bool,
    /// OpenAI only (Anthropic requests never carry sampling parameters).
    pub temperature: Option<f32>,
}

impl Default for ProviderOptions {
    fn default() -> Self {
        ProviderOptions {
            dialect: Dialect::OpenAiChat,
            anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_string(),
            max_tokens_field: MaxTokensField::MaxTokens,
            include_usage: true,
            temperature: None,
        }
    }
}

impl ProviderOptions {
    pub fn for_dialect(dialect: Dialect) -> Self {
        ProviderOptions { dialect, ..Default::default() }
    }
}

/// A request ready to send. `path` is appended to the profile's base URL; `headers` never
/// contain credentials (auth is applied by `http.rs` at send time), `Content-Type` or `Accept`.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltRequest {
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl BuiltRequest {
    /// Headers in the borrowed form `http::stream_sse` takes.
    pub fn header_refs(&self) -> Vec<(&str, &str)> {
        self.headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
    }
}

/// Build the streaming request for `opts.dialect`. Pure.
pub fn build_request(req: &ChatRequest, opts: &ProviderOptions) -> BuiltRequest {
    match opts.dialect {
        Dialect::OpenAiChat => openai::build_request(req, opts),
        Dialect::Anthropic => anthropic::build_request(req, opts),
    }
}

/// Map a non-2xx response body to a user-facing error for `dialect`.
pub fn parse_error_body(dialect: Dialect, status: u16, body: &[u8]) -> UserFacingError {
    match dialect {
        Dialect::OpenAiChat => openai::parse_error_body(status, body),
        Dialect::Anthropic => anthropic::parse_error_body(status, body),
    }
}

/// Stateful translator from SSE events to [`StreamEvent`]s.
///
/// `feed` returns `Err` for a provider-reported mid-stream error or a malformed event; the stream
/// should then be abandoned. `finish` must be called at end of body: it emits the final events
/// (closing tool calls, `Done`) or an error if the stream was cut short.
#[derive(Debug)]
pub enum StreamParser {
    OpenAi(openai::OpenAiStream),
    Anthropic(anthropic::AnthropicStream),
}

impl StreamParser {
    pub fn new(dialect: Dialect) -> Self {
        match dialect {
            Dialect::OpenAiChat => StreamParser::OpenAi(openai::OpenAiStream::new()),
            Dialect::Anthropic => StreamParser::Anthropic(anthropic::AnthropicStream::new()),
        }
    }

    pub fn feed(&mut self, ev: SseEvent) -> Result<Vec<StreamEvent>, UserFacingError> {
        match self {
            StreamParser::OpenAi(p) => p.feed(ev),
            StreamParser::Anthropic(p) => p.feed(ev),
        }
    }

    pub fn finish(&mut self) -> Result<Vec<StreamEvent>, UserFacingError> {
        match self {
            StreamParser::OpenAi(p) => p.finish(),
            StreamParser::Anthropic(p) => p.finish(),
        }
    }
}

// ------------------------------------------------------------------------------------------
// Shared helpers
// ------------------------------------------------------------------------------------------

pub(crate) fn sorted_tools(tools: &[ToolSpec]) -> Vec<&ToolSpec> {
    let mut v: Vec<&ToolSpec> = tools.iter().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name)); // stable: duplicates keep registration order
    v
}

pub(crate) fn err(kind: ErrorKind, message: impl Into<String>) -> UserFacingError {
    UserFacingError { kind, message: message.into(), status: None, retry_after_secs: None }
}

pub(crate) fn malformed() -> UserFacingError {
    err(ErrorKind::Other, "The AI endpoint sent a stream event MiraMail could not read. The response was stopped.")
}

pub(crate) fn truncated() -> UserFacingError {
    err(ErrorKind::Other, "The AI response ended unexpectedly (the connection closed before the model finished). Try again.")
}

/// Redact things that look like credentials from server text (some servers echo part of the key,
/// e.g. "Incorrect API key provided: sk-proj-****abcd"). Heuristic: well-known key prefixes, and
/// long mixed letter+digit runs without separators (opaque tokens, not model names).
pub(crate) fn scrub_secrets(s: &str) -> String {
    const PREFIXES: [&str; 7] = ["sk-", "sk_", "gsk_", "xai-", "AIza", "pk-", "Bearer"];
    let is_tok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '*' | '.' | '+' | '/' | '=');
    let mut out = String::with_capacity(s.len());
    let mut tok = String::new();
    let flush = |tok: &mut String, out: &mut String| {
        if tok.is_empty() {
            return;
        }
        let core = tok.trim_end_matches(['.', '*']);
        let alnum = core.chars().all(|c| c.is_ascii_alphanumeric());
        let mixed = core.chars().any(|c| c.is_ascii_digit()) && core.chars().any(|c| c.is_ascii_alphabetic());
        let secret = PREFIXES.iter().any(|p| core.starts_with(p) && core.len() > p.len() + 3)
            || core.contains("***")
            || (core.len() >= 32 && alnum && mixed);
        out.push_str(if secret { "[redacted]" } else { tok });
        tok.clear();
    };
    for c in s.chars() {
        if is_tok(c) {
            tok.push(c);
        } else {
            flush(&mut tok, &mut out);
            out.push(c);
        }
    }
    flush(&mut tok, &mut out);
    out
}

/// Flatten, scrub and truncate server text to [`MAX_SERVER_TEXT`] chars.
pub(crate) fn clean_text(raw: &str) -> String {
    let flat: String = raw.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = scrub_secrets(&flat.split_whitespace().collect::<Vec<_>>().join(" "));
    if flat.chars().count() > MAX_SERVER_TEXT {
        let mut t: String = flat.chars().take(MAX_SERVER_TEXT).collect();
        t.push('…');
        t
    } else {
        flat
    }
}

/// Best-effort message from a JSON error body (`{"error":{"message"}}`, `{"error":"..."}`,
/// `{"message"}`), else the raw text.
pub(crate) fn extract_message(body: &[u8]) -> (Option<String>, String) {
    let text = String::from_utf8_lossy(body);
    let Ok(v) = serde_json::from_str::<Value>(text.trim()) else {
        return (None, text.into_owned());
    };
    let ty = v
        .pointer("/error/type")
        .or_else(|| v.pointer("/error/code"))
        .and_then(|t| t.as_str().map(str::to_string).or_else(|| t.as_i64().map(|n| n.to_string())));
    let msg = v
        .pointer("/error/message")
        .or_else(|| v.get("error").filter(|e| e.is_string()))
        .or_else(|| v.get("message"))
        .or_else(|| v.get("detail"))
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    (ty, msg)
}

/// Common HTTP error mapping, with the server message already cleaned.
pub(crate) fn http_error(status: u16, server_msg: &str) -> UserFacingError {
    // Pass plain, already-scrubbed text: http.rs adds the status-specific advice and truncation.
    crate::http::map_http_error(status, &clean_text(server_msg))
}

pub(crate) fn short(s: &str) -> String {
    s.chars().take(64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub() {
        assert_eq!(scrub_secrets("Incorrect API key provided: sk-proj-****abcd."), "Incorrect API key provided: [redacted]");
        assert_eq!(scrub_secrets("key sk-ant-api03-AAAAbbbb1234 bad"), "key [redacted] bad");
        assert_eq!(scrub_secrets("model claude-sonnet-4-5-20250929 not found"), "model claude-sonnet-4-5-20250929 not found");
        assert_eq!(scrub_secrets("tok a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6!"), "tok [redacted]!");
        assert_eq!(scrub_secrets("Overloaded"), "Overloaded");
    }

    #[test]
    fn clean_truncates() {
        let long = "word ".repeat(200);
        let c = clean_text(&long);
        assert_eq!(c.chars().count(), MAX_SERVER_TEXT + 1);
        assert!(c.ends_with('…'));
    }
}
