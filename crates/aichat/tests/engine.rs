mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aichat::budget::Caps;
use aichat::engine::*;
use aichat::http::Limits;
use aichat::provider::ProviderOptions;
use aichat::store::{NewConversation, Store};
use aichat::types::Dialect;
use support::{Mock, Script};

const KEY: &str = "sk-SECRETKEY-0123456789abcdef";

const OPENAI_SSE: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo \u{1f980}\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":21,\"completion_tokens\":7,\"total_tokens\":28}}\n\n\
data: [DONE]\n\n";

const ANTHROPIC_SSE: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"c\",\"stop_reason\":null,\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n\
event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi \"}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"there\"}}\n\n\
event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":9}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

fn sse(body: &str) -> Script {
    Script::sse_head().send(body.as_bytes().to_vec())
}

struct Env {
    store: Arc<Mutex<Store>>,
    conv: i64,
    inflight: InFlight,
    dialect: Dialect,
    base: String,
    caps: Caps,
    key: Option<String>,
    key_origin: Option<String>,
}

fn env(m: &Mock, dialect: Dialect) -> Env {
    let mut st = Store::open_in_memory().unwrap();
    let conv = st
        .create_conversation(
            &NewConversation {
                title: DEFAULT_TITLE.into(),
                profile_id: Some("p1".into()),
                endpoint_host: "127.0.0.1".into(),
                model: "test-model".into(),
                consent_json: "{}".into(),
                accounts: vec![],
            },
            1_000,
        )
        .unwrap();
    let base = format!("{}/v1", m.url());
    let origin = aichat::http::validate_base_url(&base).unwrap().origin();
    Env {
        store: Arc::new(Mutex::new(st)),
        conv,
        inflight: InFlight::new(),
        dialect,
        base,
        caps: Caps::default(),
        key: Some(KEY.into()),
        key_origin: Some(origin),
    }
}

fn limits() -> Limits {
    Limits { connect_timeout: Duration::from_secs(2), idle_timeout: Duration::from_secs(3), total_timeout: Duration::from_secs(10), max_body_bytes: 1 << 20 }
}

fn ctx(e: &Env, ticket: &TurnTicket) -> TurnCtx {
    TurnCtx {
        store: e.store.clone(),
        conv_id: e.conv,
        profile: ProfileRuntime {
            profile_id: "p1".into(),
            base_url: e.base.clone(),
            model: "test-model".into(),
            options: ProviderOptions::for_dialect(e.dialect),
            max_output_tokens: 256,
        },
        api_key: e.key.clone(),
        key_origin: e.key_origin.clone(),
        caps: e.caps,
        limits: limits(),
        clock: Arc::new(|| 1_700_000_000),
        cancel: ticket.cancel.clone(),
        redactions_json: "{\"stripQuotes\":true}".into(),
        tools: None,
        env: None,
    }
}

async fn turn(e: &Env, text: &str) -> (Result<TurnOutcome, TurnError>, VecSink) {
    let ticket = e.inflight.begin(e.conv).unwrap();
    let sink = VecSink::default();
    let r = run_turn(ctx(e, &ticket), text, &sink).await;
    (r, sink)
}

fn dump_db(e: &Env) -> String {
    let st = e.store.lock().unwrap();
    let c = st.raw();
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let mut out = String::new();
    for t in tables {
        let mut stmt = c.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            for i in 0..n {
                out.push_str(&format!("{:?}|", r.get_ref(i).unwrap()));
            }
            out.push('\n');
        }
    }
    out
}

fn messages(e: &Env) -> Vec<(String, String)> {
    let st = e.store.lock().unwrap();
    st.list_messages(e.conv).unwrap().into_iter().map(|m| (m.role, aichat::context::display_text(&m.parts_json))).collect()
}

#[tokio::test]
async fn openai_streams_text_end_to_end() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let e = env(&m, Dialect::OpenAiChat);
    let (r, sink) = turn(&e, "  Say hello  ").await;
    let o = r.unwrap();
    assert_eq!(o.stop, "end_turn");
    assert_eq!((o.input_tokens, o.output_tokens), (21, 7));
    assert_eq!(sink.text(), "Hello \u{1f980}");
    let evs = sink.events();
    assert!(evs.contains(&TurnEvent::Usage { input: 21, output: 7 }));
    assert_eq!(evs.last(), Some(&TurnEvent::Done { stop: "end_turn".into() }));

    let rq = &m.requests()[0];
    assert_eq!(rq.method, "POST");
    assert_eq!(rq.path, "/v1/chat/completions");
    assert_eq!(rq.header("authorization"), Some(&*format!("Bearer {KEY}")));
    let body: serde_json::Value = serde_json::from_slice(&rq.body).unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], aichat::context::SYSTEM_PROMPT);
    assert_eq!(body["messages"][1]["content"], "Say hello");
    assert!(body.get("tools").is_none());

    assert_eq!(messages(&e), vec![("user".to_string(), "Say hello".to_string()), ("assistant".to_string(), "Hello \u{1f980}".to_string())]);
    let st = e.store.lock().unwrap();
    assert_eq!(st.get_conversation(e.conv).unwrap().unwrap().title, "Say hello");
    let u = st.list_usage("2000-01-01").unwrap();
    assert_eq!((u[0].requests, u[0].tokens_in, u[0].tokens_out), (1, 21, 7));
    assert_eq!(st.conv_tokens(e.conv).unwrap(), 28);
    let eg = st.list_egress(Some(e.conv), 10).unwrap();
    assert_eq!(eg.len(), 1);
    assert_eq!(eg[0].endpoint_host, format!("127.0.0.1:{}", m.addr.port()));
    assert_eq!(eg[0].bytes as usize, rq.body.len());
    assert!(eg[0].est_tokens > 0);
    assert_eq!(eg[0].redactions_json, "{\"stripQuotes\":true}");
}

#[tokio::test]
async fn anthropic_streams_text_end_to_end() {
    let m = Mock::start(vec![sse(ANTHROPIC_SSE)]).await;
    let e = env(&m, Dialect::Anthropic);
    let (r, sink) = turn(&e, "hi").await;
    assert_eq!(r.unwrap().stop, "end_turn");
    assert_eq!(sink.text(), "Hi there");
    let rq = &m.requests()[0];
    assert_eq!(rq.path, "/v1/v1/messages");
    assert_eq!(rq.header("x-api-key"), Some(KEY));
    assert_eq!(rq.header("anthropic-version"), Some("2026-10-21"));
    assert!(rq.header("authorization").is_none());
    let body: serde_json::Value = serde_json::from_slice(&rq.body).unwrap();
    assert_eq!(body["system"], aichat::context::SYSTEM_PROMPT);
    assert_eq!(body["max_tokens"], 256);
}

#[tokio::test]
async fn keyless_endpoint_sends_no_auth() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let mut e = env(&m, Dialect::OpenAiChat);
    e.key = None;
    e.key_origin = None;
    turn(&e, "hi").await.0.unwrap();
    let rq = &m.requests()[0];
    assert!(rq.header("authorization").is_none() && rq.header("x-api-key").is_none());
}

#[tokio::test]
async fn history_is_persisted_and_resent_append_only() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let e = env(&m, Dialect::OpenAiChat);
    turn(&e, "first").await.0.unwrap();
    turn(&e, "second").await.0.unwrap();
    let reqs = m.requests();
    let b1: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let b2: serde_json::Value = serde_json::from_slice(&reqs[1].body).unwrap();
    let m1 = b1["messages"].as_array().unwrap();
    let m2 = b2["messages"].as_array().unwrap();
    assert_eq!(m2.len(), m1.len() + 2);
    assert_eq!(&m2[..m1.len()], &m1[..], "second request must extend the first");
    assert_eq!(m2[m1.len()]["content"], "Hello \u{1f980}");
    assert_eq!(m2[m1.len() + 1]["content"], "second");
    assert_eq!(messages(&e).len(), 4);
}

#[tokio::test]
async fn egress_is_recorded_before_the_request_leaves() {
    // The server hangs after reading the request; the moment it has seen it, the row must exist.
    let m = Mock::start(vec![Script::sse_head().hang()]).await;
    let e = env(&m, Dialect::OpenAiChat);
    let ticket = e.inflight.begin(e.conv).unwrap();
    let c = ctx(&e, &ticket);
    let sink = Arc::new(VecSink::default());
    let s2 = sink.clone();
    let h = tokio::spawn(async move { run_turn(c, "hello", &*s2).await });
    for _ in 0..200 {
        if !m.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(m.requests().len(), 1);
    let rows = e.store.lock().unwrap().list_egress(Some(e.conv), 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].request_json.contains("hello"));
    e.inflight.cancel(e.conv);
    let o = h.await.unwrap().unwrap();
    assert_eq!(o.stop, "cancelled");
}

#[tokio::test]
async fn egress_row_exists_even_when_the_connection_fails() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let m = Mock::start(vec![Script::json(200, "{}")]).await;
    let mut e = env(&m, Dialect::OpenAiChat);
    e.base = format!("http://127.0.0.1:{port}/v1");
    e.key = None;
    e.key_origin = None;
    let (r, sink) = turn(&e, "hello").await;
    let err = r.unwrap_err();
    assert_eq!(err.kind, "connectionRefused");
    assert_eq!(e.store.lock().unwrap().list_egress(None, 10).unwrap().len(), 1);
    assert!(messages(&e).is_empty(), "a request that never reached the endpoint leaves no history");
    assert!(matches!(sink.events().last(), Some(TurnEvent::Error { .. })));
}

#[tokio::test]
async fn cancel_mid_stream_is_prompt_and_keeps_the_partial_reply() {
    let partial = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial text\"}}]}\n\n";
    let m = Mock::start(vec![Script::sse_head().send(partial.as_bytes().to_vec()).hang()]).await;
    let e = env(&m, Dialect::OpenAiChat);
    let ticket = e.inflight.begin(e.conv).unwrap();
    let c = ctx(&e, &ticket);
    let sink = Arc::new(VecSink::default());
    let s2 = sink.clone();
    let h = tokio::spawn(async move { run_turn(c, "go", &*s2).await });
    for _ in 0..300 {
        if sink.text() == "partial text" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(sink.text(), "partial text");
    let t0 = std::time::Instant::now();
    assert!(e.inflight.cancel(e.conv));
    let o = tokio::time::timeout(Duration::from_secs(2), h).await.expect("cancel must be prompt").unwrap().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert_eq!(o.stop, "cancelled");
    assert_eq!(sink.events().last(), Some(&TurnEvent::Done { stop: "cancelled".into() }));
    assert!(m.wait_disconnects(1, Duration::from_secs(2)).await, "connection must be closed");
    assert_eq!(messages(&e).last().unwrap(), &("assistant".to_string(), "partial text".to_string()));
}

#[tokio::test]
async fn http_errors_are_friendly_and_leak_nothing() {
    let cases: Vec<(Script, &str)> = vec![
        (Script::json(401, &format!("{{\"error\":{{\"message\":\"Incorrect API key provided: {KEY}\"}}}}")), "auth"),
        (Script::response(429, &[("Retry-After", "7"), ("Content-Type", "application/json")], "{\"error\":{\"message\":\"slow down\"}}"), "rateLimited"),
        (Script::json(503, "{\"error\":{\"message\":\"overloaded\"}}"), "server"),
        (Script::json(400, "{\"error\":{\"message\":\"bad model\"}}"), "badRequest"),
    ];
    for dialect in [Dialect::OpenAiChat, Dialect::Anthropic] {
        for (script, kind) in &cases {
            let m = Mock::start(vec![script.clone()]).await;
            let e = env(&m, dialect);
            let (r, sink) = turn(&e, "hello").await;
            let err = r.unwrap_err();
            assert_eq!(&err.kind, kind, "{dialect:?}");
            assert!(!err.message.contains(KEY), "{}", err.message);
            if *kind == "rateLimited" {
                assert!(err.message.contains('7'));
            }
            assert_eq!(sink.events().last(), Some(&TurnEvent::Error { message: err.message.clone(), kind: err.kind.clone() }));
            assert!(messages(&e).is_empty());
            let dump = dump_db(&e);
            assert!(!dump.contains(KEY), "key leaked into the database");
            assert_eq!(e.store.lock().unwrap().list_egress(None, 10).unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn redirect_is_refused_and_not_followed() {
    let target = Mock::start(vec![Script::json(200, "{}")]).await;
    let m = Mock::start(vec![Script::redirect(307, &format!("{}/steal", target.url()))]).await;
    let e = env(&m, Dialect::OpenAiChat);
    let (r, _) = turn(&e, "hello").await;
    assert_eq!(r.unwrap_err().kind, "redirect");
    assert!(target.requests().is_empty(), "the key must never reach the redirect target");
}

#[tokio::test]
async fn midstream_truncation_is_an_error_and_keeps_partial() {
    let cut = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"abc\"}}]}\n\n";
    let m = Mock::start(vec![Script::sse_head().send(cut.as_bytes().to_vec()).close()]).await;
    let e = env(&m, Dialect::OpenAiChat);
    let (r, sink) = turn(&e, "hello").await;
    assert!(r.is_err());
    assert_eq!(sink.text(), "abc");
    assert_eq!(messages(&e).len(), 2);
}

#[tokio::test]
async fn budget_caps_block_before_any_request() {
    // per turn
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let mut e = env(&m, Dialect::OpenAiChat);
    e.caps = Caps { per_turn: 2000, per_conversation: 300_000, per_day: 1_000_000 };
    let (r, _) = turn(&e, &"word ".repeat(2000)).await;
    let err = r.unwrap_err();
    assert_eq!(err.kind, "budget");
    assert!(err.message.contains("too long"));
    assert!(m.requests().is_empty());

    // per conversation
    let mut e = env(&m, Dialect::OpenAiChat);
    e.caps = Caps { per_turn: 30_000, per_conversation: 5000, per_day: 1_000_000 };
    e.store.lock().unwrap().add_conv_tokens(e.conv, 4900).unwrap();
    let (r, _) = turn(&e, "hello").await;
    let err = r.unwrap_err();
    assert_eq!(err.kind, "budget");
    assert!(err.message.contains("conversation"));

    // per day (usage from another conversation / profile counts)
    let mut e = env(&m, Dialect::OpenAiChat);
    e.caps = Caps { per_turn: 30_000, per_conversation: 300_000, per_day: 5000 };
    e.store.lock().unwrap().add_usage(1_700_000_000, "other", "m", 4000, 990).unwrap();
    let (r, _) = turn(&e, "hello").await;
    let err = r.unwrap_err();
    assert_eq!(err.kind, "budget");
    assert!(err.message.contains("today"));
    assert!(m.requests().is_empty(), "no request may leave when a cap is hit");
    assert!(e.store.lock().unwrap().list_egress(None, 10).unwrap().is_empty());
}

#[tokio::test]
async fn new_segment_starts_with_a_local_summary_when_over_budget() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let mut e = env(&m, Dialect::OpenAiChat);
    e.caps = Caps { per_turn: 3000, per_conversation: 10_000_000, per_day: 10_000_000 };
    {
        let mut st = e.store.lock().unwrap();
        for i in 0..8 {
            let u = serde_json::to_string(&[aichat::types::Part::text(format!("old question {i} {}", "q".repeat(800)))]).unwrap();
            let a = serde_json::to_string(&[aichat::types::Part::text(format!("old answer {i} {}", "a".repeat(800)))]).unwrap();
            st.append_message(e.conv, "user", &u, 5).unwrap();
            st.append_message(e.conv, "assistant", &a, 5).unwrap();
        }
    }
    let (r, _) = turn(&e, "fresh question").await;
    let o = r.unwrap();
    assert!(o.started_new_segment);
    let body = m.requests()[0].body_str();
    assert!(body.contains("Local summary of earlier turns"));
    assert!(body.contains("User: old question"));
    assert!(body.contains("fresh question"));
    assert!(!body.contains(&"q".repeat(800)), "old full text must not be resent");
    // all stored messages are still there (append-only storage)
    assert_eq!(messages(&e).len(), 16 + 2);
    let seg = e.store.lock().unwrap().get_segment(e.conv).unwrap().unwrap();
    assert!(seg.0 > 1);
    assert!(seg.1.contains("old question"));

    // the next turn reuses the same segment without re-summarising
    let (r, _) = turn(&e, "another").await;
    assert!(!r.unwrap().started_new_segment);
    let b2 = m.requests()[1].body_str();
    assert!(b2.contains("Local summary of earlier turns") && b2.contains("fresh question") && b2.contains("another"));
}

#[tokio::test]
async fn key_is_refused_for_a_different_origin() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let mut e = env(&m, Dialect::OpenAiChat);
    e.key_origin = Some("https://api.other.example:443".into());
    let (r, _) = turn(&e, "hello").await;
    let err = r.unwrap_err();
    assert_eq!(err.kind, "auth");
    assert!(m.requests().is_empty());
    assert!(e.store.lock().unwrap().list_egress(None, 10).unwrap().is_empty());
    // a key with no recorded origin is refused too
    e.key_origin = None;
    assert!(turn(&e, "hello").await.0.is_err());
    assert!(m.requests().is_empty());
}

#[tokio::test]
async fn key_never_reaches_db_events_or_errors() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let e = env(&m, Dialect::OpenAiChat);
    let (r, sink) = turn(&e, "my key is not here").await;
    r.unwrap();
    assert!(!format!("{:?}", sink.events()).contains(KEY));
    assert!(!dump_db(&e).contains(KEY));
    let eg = e.store.lock().unwrap().list_egress(None, 10).unwrap();
    assert!(!eg[0].request_json.to_lowercase().contains("authorization"));
}

#[tokio::test]
async fn one_turn_per_conversation() {
    let inf = InFlight::new();
    let t = inf.begin(1).unwrap();
    assert_eq!(inf.begin(1).err().unwrap().kind, "busy");
    assert!(inf.begin(2).is_ok());
    assert!(inf.is_running(1));
    drop(t);
    assert!(!inf.is_running(1));
    assert!(inf.begin(1).is_ok());
    assert!(!inf.cancel(99));
}

#[tokio::test]
async fn rejects_empty_and_missing_conversation() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let mut e = env(&m, Dialect::OpenAiChat);
    assert_eq!(turn(&e, "   ").await.0.unwrap_err().kind, "badRequest");
    e.conv = 4242;
    assert_eq!(turn(&e, "hi").await.0.unwrap_err().kind, "notFound");
    assert!(m.requests().is_empty());
}
