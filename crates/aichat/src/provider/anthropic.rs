//! Anthropic Messages dialect (task A1).
//!
//! Request: `POST {base}/v1/messages` with `anthropic-version` (auth `x-api-key` is added by
//! `http.rs`); top-level `system`; `max_tokens` required; tools `{name, description,
//! input_schema}` with `tool_choice: {type: "auto"}` (the only choice we use); no sampling
//! parameters (`temperature`/`top_p`/`top_k` are rejected or discouraged on current models).
//! Consecutive same-role messages are merged, so every `tool_result` answering one assistant turn
//! lands in ONE user message, with the `tool_result` blocks first (the API requires that). Thinking
//! blocks ([`Part::Opaque`] of this dialect) are replayed verbatim, in their original position.
//!
//! Stream events: `message_start`, `content_block_start` (`text` | `tool_use` | `thinking` |
//! `redacted_thinking`), `content_block_delta` (`text_delta` | `input_json_delta` |
//! `thinking_delta` | `signature_delta`), `content_block_stop`, `message_delta` (stop_reason,
//! cumulative usage), `message_stop`, `ping`, `error`. Unknown event types are ignored, as the
//! API's versioning policy asks; unknown block types are kept as opaque blocks.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::{clean_text, err, extract_message, http_error, malformed, short, sorted_tools, truncated};
use super::{BuiltRequest, ErrorKind, ProviderOptions, UserFacingError};
use crate::sse::SseEvent;
use crate::types::{ChatRequest, Dialect, Part, Role, StopReason, StreamEvent};

pub const PATH: &str = "/v1/messages";

pub fn build_request(req: &ChatRequest, opts: &ProviderOptions) -> BuiltRequest {
    // (role, blocks) with consecutive same-role messages merged.
    let mut merged: Vec<(Role, Vec<Value>, Vec<Value>)> = Vec::new(); // (role, tool_results, rest)
    for m in &req.messages {
        let mut results = Vec::new();
        let mut rest = Vec::new();
        for p in &m.parts {
            match (m.role, p) {
                // Empty text blocks are rejected by the API.
                (_, Part::Text { text }) if !text.is_empty() => rest.push(json!({"type": "text", "text": text})),
                (Role::Assistant, Part::ToolCall { id, name, args }) => {
                    // `input` must be an object; a call whose args failed to parse is replayed as {}.
                    let input = if args.is_object() { args.clone() } else { json!({}) };
                    rest.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
                }
                (Role::User, Part::ToolResult { call_id, content, is_error }) => {
                    let mut b = Map::new();
                    b.insert("type".into(), json!("tool_result"));
                    b.insert("tool_use_id".into(), json!(call_id));
                    b.insert("content".into(), json!(content));
                    if *is_error {
                        b.insert("is_error".into(), json!(true));
                    }
                    results.push(Value::Object(b));
                }
                (_, Part::Opaque { dialect: Dialect::Anthropic, raw }) => rest.push(raw.clone()),
                // Misplaced parts and other dialects' opaque blocks are dropped.
                _ => {}
            }
        }
        if results.is_empty() && rest.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some((role, r, o)) if *role == m.role => {
                r.extend(results);
                o.extend(rest);
            }
            _ => merged.push((m.role, results, rest)),
        }
    }
    let messages: Vec<Value> = merged
        .into_iter()
        .map(|(role, mut results, rest)| {
            results.extend(rest);
            let role = match role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            json!({"role": role, "content": results})
        })
        .collect();

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("max_tokens".into(), json!(req.max_output_tokens.max(1)));
    body.insert("stream".into(), json!(true));
    if let Some(sys) = req.system.as_deref().filter(|s| !s.is_empty()) {
        body.insert("system".into(), json!(sys));
    }
    body.insert("messages".into(), Value::Array(messages));
    if !req.tools.is_empty() {
        let tools: Vec<Value> = sorted_tools(&req.tools)
            .into_iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.parameters}))
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        body.insert("tool_choice".into(), json!({"type": "auto"}));
    }
    let version = if opts.anthropic_version.trim().is_empty() {
        super::DEFAULT_ANTHROPIC_VERSION.to_string()
    } else {
        opts.anthropic_version.trim().to_string()
    };
    BuiltRequest {
        path: PATH.to_string(),
        headers: vec![("anthropic-version".to_string(), version)],
        body: Value::Object(body),
    }
}

/// `{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}`.
pub fn parse_error_body(status: u16, body: &[u8]) -> UserFacingError {
    let (ty, msg) = extract_message(body);
    let mut e = http_error(status, &msg);
    if status == 529 || ty.as_deref() == Some("overloaded_error") {
        e.kind = ErrorKind::Server;
        e.message = format!("The AI service is temporarily overloaded (HTTP {status}). Try again in a moment.");
    }
    e
}

pub fn map_stop_reason(r: &str) -> StopReason {
    match r {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        // Ran out of context window: same user-facing meaning as hitting the output cap.
        "model_context_window_exceeded" => StopReason::MaxTokens,
        "refusal" => StopReason::Refusal,
        other => StopReason::Other(short(other)), // e.g. "pause_turn"
    }
}

#[derive(Debug)]
enum Block {
    Text,
    Tool { idx: usize },
    /// `thinking` (accumulated) or any other block replayed as-is; `json` collects
    /// `input_json_delta` for unknown blocks that carry an `input`.
    Opaque { raw: Value, thinking: String, signature: String, json: String },
}

#[derive(Debug, Default)]
pub struct AnthropicStream {
    blocks: HashMap<u64, Block>,
    next_tool: usize,
    input: u64,
    cache_creation: u64,
    cache_read: u64,
    output: u64,
    stop: Option<StopReason>,
    started: bool,
    done: bool,
}

impl AnthropicStream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, ev: SseEvent) -> Result<Vec<StreamEvent>, UserFacingError> {
        let mut out = Vec::new();
        if self.done {
            return Ok(out);
        }
        let v: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(_) if ev.event_type() == "ping" => return Ok(out),
            Err(_) => return Err(malformed()),
        };
        let ty = v.get("type").and_then(Value::as_str).unwrap_or_else(|| ev.event_type()).to_string();
        match ty.as_str() {
            "message_start" => {
                self.started = true;
                if let Some(u) = v.pointer("/message/usage") {
                    self.update_usage(u);
                }
            }
            "content_block_start" => {
                let index = v.get("index").and_then(Value::as_u64).ok_or_else(malformed)?;
                let cb = v.get("content_block").ok_or_else(malformed)?;
                let block = match cb.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = cb.get("text").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                            out.push(StreamEvent::TextDelta(t.to_string()));
                        }
                        Block::Text
                    }
                    Some("tool_use") => {
                        let idx = self.next_tool;
                        self.next_tool += 1;
                        let s = |k: &str| cb.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                        out.push(StreamEvent::ToolCallStart { idx, id: s("id"), name: s("name") });
                        // `input` is `{}` at start; the real arguments stream as input_json_delta.
                        // If a server sends a non-empty input here instead, pass it through.
                        if let Some(input) = cb.get("input").filter(|i| i.as_object().is_some_and(|o| !o.is_empty())) {
                            out.push(StreamEvent::ToolCallArgsDelta { idx, frag: input.to_string() });
                        }
                        Block::Tool { idx }
                    }
                    _ => Block::Opaque {
                        thinking: cb.get("thinking").and_then(Value::as_str).unwrap_or("").to_string(),
                        signature: cb.get("signature").and_then(Value::as_str).unwrap_or("").to_string(),
                        raw: cb.clone(),
                        json: String::new(),
                    },
                };
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let index = v.get("index").and_then(Value::as_u64).ok_or_else(malformed)?;
                let d = v.get("delta").ok_or_else(malformed)?;
                let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("");
                let dty = d.get("type").and_then(Value::as_str).unwrap_or("");
                match (self.blocks.get_mut(&index), dty) {
                    (Some(Block::Text), "text_delta") if !s("text").is_empty() => {
                        out.push(StreamEvent::TextDelta(s("text").to_string()));
                    }
                    (Some(Block::Tool { idx }), "input_json_delta") if !s("partial_json").is_empty() => {
                        out.push(StreamEvent::ToolCallArgsDelta { idx: *idx, frag: s("partial_json").to_string() });
                    }
                    (Some(Block::Opaque { thinking, .. }), "thinking_delta") => thinking.push_str(s("thinking")),
                    (Some(Block::Opaque { signature, .. }), "signature_delta") => signature.push_str(s("signature")),
                    (Some(Block::Opaque { json, .. }), "input_json_delta") => json.push_str(s("partial_json")),
                    // citations_delta and future delta types: ignored.
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = v.get("index").and_then(Value::as_u64).ok_or_else(malformed)?;
                match self.blocks.remove(&index) {
                    Some(Block::Tool { idx }) => out.push(StreamEvent::ToolCallEnd { idx }),
                    Some(Block::Opaque { mut raw, thinking, signature, json }) => {
                        if let Some(o) = raw.as_object_mut() {
                            if o.contains_key("thinking") || !thinking.is_empty() {
                                o.insert("thinking".into(), json!(thinking));
                            }
                            if o.contains_key("signature") || !signature.is_empty() {
                                o.insert("signature".into(), json!(signature));
                            }
                            if !json.is_empty() {
                                if let Ok(input) = serde_json::from_str::<Value>(&json) {
                                    o.insert("input".into(), input);
                                }
                            }
                        }
                        out.push(StreamEvent::Opaque { dialect: Dialect::Anthropic, raw });
                    }
                    Some(Block::Text) | None => {}
                }
            }
            "message_delta" => {
                if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop = Some(map_stop_reason(r));
                }
                if let Some(u) = v.get("usage") {
                    self.update_usage(u);
                }
                out.push(self.usage());
            }
            "message_stop" => {
                self.done = true;
                out.push(StreamEvent::Done(self.stop.take().unwrap_or(StopReason::EndTurn)));
            }
            "error" => return Err(stream_error(v.get("error").unwrap_or(&v))),
            _ => {} // ping and future event types
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Result<Vec<StreamEvent>, UserFacingError> {
        if self.done {
            return Ok(Vec::new());
        }
        // Lenient: some proxies drop the final message_stop. Accept if we have a stop_reason and
        // no block is left open; otherwise the stream was cut.
        match self.stop.take() {
            Some(stop) if self.started && self.blocks.is_empty() => {
                self.done = true;
                Ok(vec![StreamEvent::Done(stop)])
            }
            _ => Err(truncated()),
        }
    }

    fn update_usage(&mut self, u: &Value) {
        let set = |field: &mut u64, k: &str| {
            if let Some(n) = u.get(k).and_then(Value::as_u64) {
                *field = n;
            }
        };
        set(&mut self.input, "input_tokens");
        set(&mut self.cache_creation, "cache_creation_input_tokens");
        set(&mut self.cache_read, "cache_read_input_tokens");
        set(&mut self.output, "output_tokens");
    }

    fn usage(&self) -> StreamEvent {
        StreamEvent::Usage {
            input: self.input + self.cache_creation + self.cache_read,
            output: self.output,
            cache_read: self.cache_read,
        }
    }
}

fn stream_error(e: &Value) -> UserFacingError {
    let ty = e.get("type").and_then(Value::as_str).unwrap_or("");
    let msg = clean_text(e.get("message").and_then(Value::as_str).unwrap_or(""));
    let (kind, lead) = match ty {
        "overloaded_error" => (ErrorKind::Server, "The AI service is temporarily overloaded. Try again in a moment."),
        "rate_limit_error" => (ErrorKind::RateLimited, "Rate limited or out of quota. Try again in a little while."),
        "authentication_error" => (ErrorKind::Auth, "The endpoint rejected the API key. Check or re-enter the key in Settings > AI."),
        "permission_error" | "billing_error" => (ErrorKind::Forbidden, "Access denied for this key or model."),
        "invalid_request_error" => (ErrorKind::BadRequest, "The endpoint rejected the request. Check the model name and settings."),
        "not_found_error" => (ErrorKind::NotFound, "Not found. Check the base URL and the model name."),
        "request_too_large" => (ErrorKind::TooLarge, "The request was too large. Try a smaller scope or turn on more redaction."),
        _ => (ErrorKind::Server, "The AI server reported an error during the response."),
    };
    let mut m = lead.to_string();
    if !msg.is_empty() {
        m.push_str(&format!(" Server said: \"{msg}\""));
    }
    err(kind, m)
}
