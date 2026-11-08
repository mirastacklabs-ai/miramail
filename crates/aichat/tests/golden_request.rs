//! Request-side goldens (task A1): exact JSON bodies for both dialects from one conversation that
//! exercises system placement, tool-call / tool-result placement, consecutive user messages,
//! thinking replay and tool sorting. Also error-body mapping per dialect.

use std::path::Path;

use aichat::http::ErrorKind;
use aichat::provider::{
    build_request, parse_error_body, MaxTokensField, ProviderOptions, StreamParser, DEFAULT_ANTHROPIC_VERSION,
};
use aichat::sse::SseParser;
use aichat::types::{ChatMessage, ChatRequest, Dialect, Part, Role, StreamEvent, ToolSpec};
use serde_json::{json, Value};

fn golden(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/request").join(name);
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn tools() -> Vec<ToolSpec> {
    // Deliberately unsorted.
    vec![
        ToolSpec {
            name: "search_mail".into(),
            description: "Search mail".into(),
            parameters: json!({"type": "object", "properties": {"query": {"type": "string"}, "limit": {"type": "integer"}}, "required": ["query"]}),
        },
        ToolSpec {
            name: "get_message".into(),
            description: "Read one message".into(),
            parameters: json!({"type": "object", "properties": {"ref": {"type": "string"}}, "required": ["ref"]}),
        },
        ToolSpec { name: "list_folders".into(), description: "List folders".into(), parameters: json!({"type": "object", "properties": {}}) },
    ]
}

fn conversation(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        system: Some("You are MiraMail's assistant.".into()),
        messages: vec![
            ChatMessage::user_text("Find the invoice from ACME"),
            ChatMessage {
                role: Role::Assistant,
                parts: vec![
                    Part::Opaque {
                        dialect: Dialect::Anthropic,
                        raw: json!({"type": "thinking", "thinking": "Search first.", "signature": "sig=="}),
                    },
                    Part::Opaque { dialect: Dialect::Anthropic, raw: json!({"type": "redacted_thinking", "data": "opaque=="}) },
                    Part::text("Searching."),
                    Part::ToolCall { id: "c1".into(), name: "search_mail".into(), args: json!({"query": "ACME invoice", "limit": 5}) },
                    Part::ToolCall { id: "c2".into(), name: "list_folders".into(), args: json!({}) },
                ],
            },
            // Two consecutive user messages: Anthropic must merge them, tool results first.
            ChatMessage {
                role: Role::User,
                parts: vec![Part::ToolResult { call_id: "c1".into(), content: "m:1 ACME invoice #42".into(), is_error: false }],
            },
            ChatMessage {
                role: Role::User,
                parts: vec![
                    Part::text("Also check the archive"),
                    Part::ToolResult { call_id: "c2".into(), content: "account not found".into(), is_error: true },
                ],
            },
            ChatMessage::assistant_text("Found it."),
            ChatMessage::user_text("Thanks"),
        ],
        tools: tools(),
        max_output_tokens: 1024,
    }
}

/// Parse OpenAI `arguments` strings so the comparison does not depend on serde_json key order.
fn normalise_openai(mut v: Value) -> Value {
    if let Some(msgs) = v.get_mut("messages").and_then(Value::as_array_mut) {
        for m in msgs {
            if let Some(calls) = m.get_mut("tool_calls").and_then(Value::as_array_mut) {
                for c in calls {
                    let a = c.pointer_mut("/function/arguments").unwrap();
                    *a = serde_json::from_str(a.as_str().unwrap()).unwrap();
                }
            }
        }
    }
    v
}

#[test]
fn openai_request_exact() {
    let opts = ProviderOptions::for_dialect(Dialect::OpenAiChat);
    let b = build_request(&conversation("gpt-test"), &opts);
    assert_eq!(b.path, "/chat/completions");
    assert!(b.headers.is_empty(), "no extra headers, never auth: {:?}", b.headers);
    assert_eq!(normalise_openai(b.body.clone()), normalise_openai(golden("openai.json")), "\n got: {:#}", b.body);
    // Arguments are a JSON string, as the API requires.
    assert!(b.body["messages"][2]["tool_calls"][0]["function"]["arguments"].is_string());
}

#[test]
fn openai_options() {
    let opts = ProviderOptions {
        max_tokens_field: MaxTokensField::MaxCompletionTokens,
        include_usage: false,
        temperature: Some(0.5),
        ..ProviderOptions::for_dialect(Dialect::OpenAiChat)
    };
    let mut req = conversation("gpt-test");
    req.system = None;
    req.tools.clear();
    let b = build_request(&req, &opts).body;
    assert_eq!(b["max_completion_tokens"], json!(1024));
    assert!(b.get("max_tokens").is_none());
    assert!(b.get("stream_options").is_none());
    assert!(b.get("tools").is_none());
    assert_eq!(b["temperature"], json!(0.5));
    assert_eq!(b["messages"][0]["role"], "user", "no system message when system is None");
}

#[test]
fn openai_tool_only_assistant_has_null_content_and_drops_opaque() {
    let req = ChatRequest {
        model: "m".into(),
        system: None,
        messages: vec![ChatMessage {
            role: Role::Assistant,
            parts: vec![
                Part::Opaque { dialect: Dialect::Anthropic, raw: json!({"type": "thinking"}) },
                Part::ToolCall { id: "x".into(), name: "list_accounts".into(), args: json!({}) },
            ],
        }],
        tools: vec![],
        max_output_tokens: 10,
    };
    let b = build_request(&req, &ProviderOptions::default()).body;
    assert_eq!(b["messages"], json!([{"role": "assistant", "content": null, "tool_calls": [
        {"id": "x", "type": "function", "function": {"name": "list_accounts", "arguments": "{}"}}]}]));
}

#[test]
fn anthropic_request_exact() {
    let opts = ProviderOptions { temperature: Some(0.7), ..ProviderOptions::for_dialect(Dialect::Anthropic) };
    let b = build_request(&conversation("claude-test"), &opts);
    assert_eq!(b.path, "/v1/messages");
    assert_eq!(b.headers, vec![("anthropic-version".to_string(), DEFAULT_ANTHROPIC_VERSION.to_string())]);
    assert_eq!(DEFAULT_ANTHROPIC_VERSION, "2026-10-21");
    assert_eq!(b.body, golden("anthropic.json"), "\n got: {:#}", b.body);
    for k in ["temperature", "top_p", "top_k"] {
        assert!(b.body.get(k).is_none(), "{k} must never be sent to Anthropic");
    }
}

#[test]
fn anthropic_version_configurable_and_no_secret_headers() {
    let opts = ProviderOptions { anthropic_version: "2023-06-01".into(), ..ProviderOptions::for_dialect(Dialect::Anthropic) };
    let b = build_request(&conversation("c"), &opts);
    assert_eq!(b.header_refs(), vec![("anthropic-version", "2023-06-01")]);
    for (k, _) in &b.headers {
        let k = k.to_ascii_lowercase();
        assert!(k != "x-api-key" && k != "authorization");
    }
}

#[test]
fn requests_are_byte_stable_regardless_of_tool_order() {
    for d in [Dialect::OpenAiChat, Dialect::Anthropic] {
        let opts = ProviderOptions::for_dialect(d);
        let a = build_request(&conversation("m"), &opts);
        let mut rev = conversation("m");
        rev.tools.reverse();
        let b = build_request(&rev, &opts);
        assert_eq!(serde_json::to_string(&a.body).unwrap(), serde_json::to_string(&b.body).unwrap());
        assert_eq!(serde_json::to_string(&a.body).unwrap(), serde_json::to_string(&build_request(&conversation("m"), &opts).body).unwrap());
    }
}

/// Collect a streamed assistant turn into a ChatMessage the way the engine will.
fn assemble(events: &[StreamEvent]) -> ChatMessage {
    let mut parts: Vec<Part> = Vec::new();
    let mut args: Vec<(usize, String)> = Vec::new();
    for ev in events {
        match ev {
            StreamEvent::TextDelta(t) => match parts.last_mut() {
                Some(Part::Text { text }) => text.push_str(t),
                _ => parts.push(Part::text(t.clone())),
            },
            StreamEvent::Opaque { dialect, raw } => parts.push(Part::Opaque { dialect: *dialect, raw: raw.clone() }),
            StreamEvent::ToolCallStart { idx, id, name } => {
                args.push((*idx, String::new()));
                parts.push(Part::ToolCall { id: id.clone(), name: name.clone(), args: Value::Null });
            }
            StreamEvent::ToolCallArgsDelta { idx, frag } => args.iter_mut().find(|(i, _)| i == idx).unwrap().1.push_str(frag),
            _ => {}
        }
    }
    let mut it = args.into_iter();
    for p in &mut parts {
        if let Part::ToolCall { args, .. } = p {
            let s = it.next().unwrap().1;
            *args = if s.is_empty() { json!({}) } else { serde_json::from_str(&s).unwrap() };
        }
    }
    ChatMessage { role: Role::Assistant, parts }
}

#[test]
fn anthropic_thinking_replayed_verbatim_in_order() {
    let input = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/stream/anthropic_thinking.sse")).unwrap();
    let mut sse = SseParser::new();
    let mut p = StreamParser::new(Dialect::Anthropic);
    let mut events = Vec::new();
    for ev in sse.feed(&input).unwrap().into_iter().chain(sse.finish().unwrap()) {
        events.extend(p.feed(ev).unwrap());
    }
    events.extend(p.finish().unwrap());
    let turn = assemble(&events);
    let req = ChatRequest {
        model: "claude-test".into(),
        system: None,
        messages: vec![
            ChatMessage::user_text("Invoices?"),
            turn,
            ChatMessage {
                role: Role::User,
                parts: vec![Part::ToolResult { call_id: "toolu_t".into(), content: "none".into(), is_error: false }],
            },
        ],
        tools: vec![],
        max_output_tokens: 100,
    };
    let anth = build_request(&req, &ProviderOptions::for_dialect(Dialect::Anthropic)).body;
    assert_eq!(
        anth["messages"][1]["content"],
        json!([
            {"type": "thinking", "thinking": "The user wants invoices — search first.", "signature": "EqQBCgIYAhIM1gbcDa9GJwZA2b3hGgxBdjrkzLoky3dl1pkiMOYds"},
            {"type": "redacted_thinking", "data": "EmwKAhgBEgy3va3pzix/LafPsn4aDFIT2Xlxh0L5L8rLVyIwxtE3rAFBa8cr3qpP"},
            {"type": "text", "text": "Looking."},
            {"type": "tool_use", "id": "toolu_t", "name": "search_mail", "input": {"query": "invoice"}}
        ])
    );
    assert!(anth.get("tools").is_none() && anth.get("tool_choice").is_none());
    // Switching the same history to OpenAI drops the opaque blocks.
    let oai = build_request(&req, &ProviderOptions::default()).body;
    assert_eq!(oai["messages"][1]["content"], "Looking.");
    assert_eq!(oai["messages"][2], json!({"role": "tool", "tool_call_id": "toolu_t", "content": "none"}));
}

#[test]
fn error_bodies_openai() {
    let e = parse_error_body(
        Dialect::OpenAiChat,
        401,
        br#"{"error":{"message":"Incorrect API key provided: sk-proj-abcd****wxyz. You can find your API key at https://platform.openai.com/account/api-keys.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#,
    );
    assert_eq!(e.kind, ErrorKind::Auth);
    assert_eq!(e.status, Some(401));
    assert!(!e.message.contains("sk-proj") && !e.message.contains("wxyz"), "{}", e.message);
    assert!(e.message.contains("Incorrect API key provided: [redacted]"), "{}", e.message);

    // Ollama: {"error": "string"}
    let e = parse_error_body(Dialect::OpenAiChat, 404, br#"{"error":"model 'llama9' not found"}"#);
    assert_eq!(e.kind, ErrorKind::NotFound);
    assert!(e.message.contains("model 'llama9' not found"));

    // Non-JSON, very long body: truncated to 300 chars of server text.
    let body = format!("<html>{}</html>", "x ".repeat(2000));
    let e = parse_error_body(Dialect::OpenAiChat, 502, body.as_bytes());
    assert_eq!(e.kind, ErrorKind::Server);
    let quoted = e.message.split("Server said: \"").nth(1).unwrap();
    assert!(quoted.chars().count() <= 302, "{}", quoted.chars().count());
}

#[test]
fn error_bodies_anthropic() {
    let e = parse_error_body(Dialect::Anthropic, 529, br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_1"}"#);
    assert_eq!(e.kind, ErrorKind::Server);
    assert!(e.message.contains("overloaded"), "{}", e.message);
    assert_eq!(e.status, Some(529));

    let e = parse_error_body(
        Dialect::Anthropic,
        400,
        br#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: Field required"}}"#,
    );
    assert_eq!(e.kind, ErrorKind::BadRequest);
    assert!(e.message.contains("max_tokens: Field required"));

    let e = parse_error_body(
        Dialect::Anthropic,
        401,
        br#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key sk-ant-api03-SECRETSECRET123"}}"#,
    );
    assert_eq!(e.kind, ErrorKind::Auth);
    assert!(!e.message.contains("SECRET"), "{}", e.message);

    let long = format!(r#"{{"type":"error","error":{{"type":"api_error","message":"{}"}}}}"#, "é".repeat(1000));
    let e = parse_error_body(Dialect::Anthropic, 500, long.as_bytes());
    assert!(e.message.chars().count() < 500);
}
