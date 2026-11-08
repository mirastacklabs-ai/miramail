//! OpenAI chat-completions dialect (task A1). Also covers Ollama, LM Studio and OpenRouter.
//!
//! Request: `POST {base}/chat/completions` with `stream: true`; the system prompt is the first
//! message; tools are `{type:"function", function:{name, description, parameters}}`; tool
//! results become `role:"tool"` messages with `tool_call_id`.
//!
//! Stream: `data: {chat.completion.chunk}` lines ending with `data: [DONE]`. Tool calls arrive as
//! fragments keyed by `delta.tool_calls[].index` (interleaving allowed); `id`/`name` come with the
//! first fragment of each call. Ollama may send a whole call in one fragment and (older
//! versions) omit `index`. With `stream_options.include_usage` a last chunk has `choices: []`
//! and `usage`. OpenRouter interleaves `: OPENROUTER PROCESSING` comments (dropped by the SSE
//! parser) and reports mid-stream failures as a chunk with a top-level `error` object.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::{clean_text, err, extract_message, http_error, malformed, short, sorted_tools, truncated};
use super::{BuiltRequest, ErrorKind, MaxTokensField, ProviderOptions, UserFacingError};
use crate::sse::SseEvent;
use crate::types::{ChatRequest, Part, Role, StopReason, StreamEvent};

pub const PATH: &str = "/chat/completions";

pub fn build_request(req: &ChatRequest, opts: &ProviderOptions) -> BuiltRequest {
    let mut messages = Vec::new();
    if let Some(sys) = req.system.as_deref().filter(|s| !s.is_empty()) {
        messages.push(json!({"role": "system", "content": sys}));
    }
    for m in &req.messages {
        match m.role {
            Role::User => push_user(&mut messages, &m.parts),
            Role::Assistant => push_assistant(&mut messages, &m.parts),
        }
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(true));
    if opts.include_usage {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    let field = match opts.max_tokens_field {
        MaxTokensField::MaxTokens => "max_tokens",
        MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
    };
    body.insert(field.into(), json!(req.max_output_tokens));
    if let Some(t) = opts.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = sorted_tools(&req.tools)
            .into_iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters
                }})
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
    }
    BuiltRequest { path: PATH.to_string(), headers: Vec::new(), body: Value::Object(body) }
}

/// Tool results first (each its own `role:"tool"` message, which must directly follow the
/// assistant's `tool_calls`), then any user text as one user message.
fn push_user(out: &mut Vec<Value>, parts: &[Part]) {
    let mut texts = Vec::new();
    for p in parts {
        match p {
            Part::ToolResult { call_id, content, is_error } => {
                // No is_error field in this dialect: say it in the content.
                let content = if *is_error { format!("Error: {content}") } else { content.clone() };
                out.push(json!({"role": "tool", "tool_call_id": call_id, "content": content}));
            }
            Part::Text { text } if !text.is_empty() => texts.push(text.as_str()),
            // Tool calls in a user message are invalid; opaque parts belong to other dialects.
            _ => {}
        }
    }
    if !texts.is_empty() {
        out.push(json!({"role": "user", "content": texts.join("\n\n")}));
    }
}

fn push_assistant(out: &mut Vec<Value>, parts: &[Part]) {
    let mut texts = Vec::new();
    let mut calls = Vec::new();
    for p in parts {
        match p {
            Part::Text { text } if !text.is_empty() => texts.push(text.as_str()),
            Part::ToolCall { id, name, args } => {
                let args = if args.is_null() { "{}".to_string() } else { args.to_string() };
                calls.push(json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}}));
            }
            _ => {}
        }
    }
    if texts.is_empty() && calls.is_empty() {
        return;
    }
    let mut m = Map::new();
    m.insert("role".into(), json!("assistant"));
    // `content: null` is the documented form when only tool calls are present.
    m.insert("content".into(), if texts.is_empty() { Value::Null } else { json!(texts.join("\n\n")) });
    if !calls.is_empty() {
        m.insert("tool_calls".into(), Value::Array(calls));
    }
    out.push(Value::Object(m));
}

/// `{"error":{"message","type","code"}}` (OpenAI, OpenRouter, LM Studio) or `{"error":"..."}` (Ollama).
pub fn parse_error_body(status: u16, body: &[u8]) -> UserFacingError {
    let (_ty, msg) = extract_message(body);
    http_error(status, &msg)
}

/// Map `finish_reason`.
pub fn map_finish_reason(r: &str) -> StopReason {
    match r {
        "stop" => StopReason::EndTurn,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        "content_filter" => StopReason::ContentFilter,
        other => StopReason::Other(short(other)),
    }
}

#[derive(Debug, Default)]
struct Call {
    /// Our index once `ToolCallStart` has been emitted.
    idx: Option<usize>,
    id: String,
    name: String,
    /// Argument fragments received before the name was known.
    pending: String,
    ended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Index(u64),
    /// Calls without an `index` (older Ollama): one key per call.
    Seq(usize),
}

#[derive(Debug, Default)]
pub struct OpenAiStream {
    calls: BTreeMap<Key, Call>,
    /// Keys in our idx order (order of `ToolCallStart`).
    started: Vec<Key>,
    next_seq: usize,
    last_key: Option<Key>,
    finish: Option<StopReason>,
    refusal: bool,
    done: bool,
}

impl OpenAiStream {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, ev: SseEvent) -> Result<Vec<StreamEvent>, UserFacingError> {
        let mut out = Vec::new();
        if self.done {
            return Ok(out);
        }
        if ev.is_done() {
            self.complete(&mut out);
            return Ok(out);
        }
        if ev.data.trim().is_empty() {
            return Ok(out);
        }
        let v: Value = serde_json::from_str(&ev.data).map_err(|_| malformed())?;
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            return Err(stream_error(e));
        }
        if ev.event_type() == "error" {
            return Err(stream_error(&v));
        }
        if let Some(choice) = v.get("choices").and_then(Value::as_array).and_then(|c| first_choice(c)) {
            if let Some(delta) = choice.get("delta") {
                self.on_delta(delta, &mut out);
            }
            if let Some(r) = choice.get("finish_reason").and_then(Value::as_str) {
                self.close_calls(&mut out);
                self.finish = Some(map_finish_reason(r));
            }
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let n = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
            out.push(StreamEvent::Usage {
                input: n("/prompt_tokens"),
                output: n("/completion_tokens"),
                cache_read: n("/prompt_tokens_details/cached_tokens"),
            });
        }
        Ok(out)
    }

    pub fn finish(&mut self) -> Result<Vec<StreamEvent>, UserFacingError> {
        let mut out = Vec::new();
        if self.done {
            return Ok(out);
        }
        // No `[DONE]`: accept only if the model reported a finish_reason.
        if self.finish.is_none() {
            return Err(truncated());
        }
        self.complete(&mut out);
        Ok(out)
    }

    fn on_delta(&mut self, delta: &Value, out: &mut Vec<StreamEvent>) {
        if let Some(t) = delta.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            out.push(StreamEvent::TextDelta(t.to_string()));
        }
        // Structured-output refusals (OpenAI): show the text, report Refusal.
        if let Some(t) = delta.get("refusal").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            self.refusal = true;
            out.push(StreamEvent::TextDelta(t.to_string()));
        }
        // `reasoning` / `reasoning_content` (OpenRouter, DeepSeek, Ollama) is deliberately ignored.
        let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) else { return };
        for tc in tcs {
            let id = tc.get("id").and_then(Value::as_str).filter(|s| !s.is_empty());
            let func = tc.get("function");
            let name = func.and_then(|f| f.get("name")).and_then(Value::as_str).filter(|s| !s.is_empty());
            let args = match func.and_then(|f| f.get("arguments")) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                // Some servers send the arguments as a JSON object.
                Some(other) => other.to_string(),
            };
            let key = match tc.get("index").and_then(Value::as_u64) {
                Some(i) => Key::Index(i),
                None if id.is_some() || name.is_some() => {
                    self.next_seq += 1;
                    Key::Seq(self.next_seq)
                }
                None => match self.last_key.clone() {
                    Some(k) => k,
                    None => continue, // stray fragment with nothing to attach to
                },
            };
            self.last_key = Some(key.clone());
            let call = self.calls.entry(key.clone()).or_default();
            if call.ended {
                continue; // fragment after finish_reason: ignore
            }
            if let Some(id) = id {
                if call.id.is_empty() {
                    call.id = id.to_string();
                }
            }
            if let Some(name) = name {
                if call.name.is_empty() {
                    call.name = name.to_string();
                }
            }
            match call.idx {
                Some(idx) => {
                    if !args.is_empty() {
                        out.push(StreamEvent::ToolCallArgsDelta { idx, frag: args });
                    }
                }
                None => {
                    call.pending.push_str(&args);
                    if !call.name.is_empty() {
                        Self::start(&mut self.started, key, call, out);
                    }
                }
            }
        }
    }

    fn start(started: &mut Vec<Key>, key: Key, call: &mut Call, out: &mut Vec<StreamEvent>) {
        let idx = started.len();
        started.push(key);
        call.idx = Some(idx);
        if call.id.is_empty() {
            call.id = format!("call_{idx}"); // some local servers omit ids
        }
        out.push(StreamEvent::ToolCallStart { idx, id: call.id.clone(), name: call.name.clone() });
        let pending = std::mem::take(&mut call.pending);
        if !pending.is_empty() {
            out.push(StreamEvent::ToolCallArgsDelta { idx, frag: pending });
        }
    }

    /// Start any call still waiting for a name, then end every open call in idx order.
    fn close_calls(&mut self, out: &mut Vec<StreamEvent>) {
        for (key, call) in self.calls.iter_mut() {
            if call.idx.is_none() && !call.ended {
                Self::start(&mut self.started, key.clone(), call, out);
            }
        }
        for key in &self.started {
            if let Some(call) = self.calls.get_mut(key) {
                if !call.ended {
                    call.ended = true;
                    if let Some(idx) = call.idx {
                        out.push(StreamEvent::ToolCallEnd { idx });
                    }
                }
            }
        }
    }

    fn complete(&mut self, out: &mut Vec<StreamEvent>) {
        self.close_calls(out);
        let had_calls = !self.started.is_empty();
        let stop = match self.finish.take() {
            // Some servers (older Ollama) report "stop" even when the turn ended in tool calls.
            Some(StopReason::EndTurn) | None if had_calls => StopReason::ToolUse,
            Some(StopReason::EndTurn) | None if self.refusal => StopReason::Refusal,
            Some(s) => s,
            None => StopReason::EndTurn,
        };
        out.push(StreamEvent::Done(stop));
        self.done = true;
    }
}

fn first_choice(choices: &[Value]) -> Option<&Value> {
    // We never request n > 1; take choice 0 (or the first one if `index` is missing).
    choices.iter().find(|c| c.get("index").and_then(Value::as_u64) == Some(0)).or_else(|| choices.first())
}

fn stream_error(e: &Value) -> UserFacingError {
    let msg = e
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| e.as_str())
        .or_else(|| e.pointer("/error/message").and_then(Value::as_str))
        .unwrap_or("");
    let code = e.get("code").and_then(|c| c.as_u64().or_else(|| c.as_str().and_then(|s| s.parse().ok())));
    let kind = match code {
        Some(429) => ErrorKind::RateLimited,
        Some(401) => ErrorKind::Auth,
        Some(403) => ErrorKind::Forbidden,
        Some(400) => ErrorKind::BadRequest,
        _ => ErrorKind::Server,
    };
    let text = clean_text(msg);
    let mut m = "The AI server reported an error during the response.".to_string();
    if !text.is_empty() {
        m.push_str(&format!(" Server said: \"{text}\""));
    }
    err(kind, m)
}
