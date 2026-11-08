//! Glue tests: in-memory store, InMemorySecretStore, loopback mock LLM. Never the real Keychain,
//! network or app data dir.

#[path = "../../../../crates/aichat/tests/support/mod.rs"]
pub(crate) mod support;

use super::*;
use aichat::engine::VecSink;
use mailcore::InMemorySecretStore;
use support::{Mock, Script};

pub(crate) const KEY: &str = "sk-SECRETKEY-0123456789abcdef";

const OPENAI_SSE: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" there\"},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

fn fast() -> Limits {
    Limits { connect_timeout: Duration::from_secs(2), idle_timeout: Duration::from_secs(3), total_timeout: Duration::from_secs(10), max_body_bytes: 1 << 20 }
}

pub(crate) fn state() -> (AiState, Arc<InMemorySecretStore>) {
    let secrets = Arc::new(InMemorySecretStore::new());
    let st = AiState::new(Store::open_in_memory().unwrap(), secrets.clone()).with_limits(fast());
    (st, secrets)
}

pub(crate) fn prof(id: &str, base: &str) -> Profile {
    Profile {
        id: id.into(),
        name: "Local".into(),
        kind: "openai_chat".into(),
        base_url: base.into(),
        model: "m1".into(),
        max_output_tokens: 0,
        anthropic_version: None,
        has_key: false,
        key_origin: None,
        needs_key_reentry: false,
        warnings: vec![],
        tested_at: None,
        test_ok: None,
    }
}

fn consent() -> Consent {
    Consent { scopes: vec![Scope { account: "a@x.org".into(), folder: "INBOX".into(), allowed: true }], redaction: Redaction::default(), tools: false, suggest: false, calendar: Default::default() }
}

fn sse(body: &str) -> Script {
    Script::sse_head().send(body.as_bytes().to_vec())
}

#[tokio::test]
async fn new_state_contacts_nothing_and_has_defaults() {
    let m = Mock::start(vec![Script::json(200, "{}")]).await;
    let (st, _) = state();
    st.startup_purge();
    assert!(st.list_profiles().unwrap().is_empty());
    assert_eq!(st.get_settings().unwrap(), Settings::default());
    assert_eq!(m.connections(), 0);
}

#[tokio::test]
async fn save_profile_stores_key_only_in_keychain_and_binds_origin() {
    let (st, secrets) = state();
    let p = st.save_profile(prof("p1", "https://api.example.com/v1"), Some(format!("  {KEY} "))).await.unwrap();
    assert!(p.has_key && !p.needs_key_reentry);
    assert_eq!(p.key_origin.as_deref(), Some("https://api.example.com:443"));
    assert_eq!(p.max_output_tokens, DEFAULT_MAX_OUTPUT);
    assert_eq!(secrets.get("ai-provider:p1").unwrap().as_deref(), Some(KEY));
    // nothing secret in the wire value or the database
    assert!(!serde_json::to_string(&p).unwrap().contains(KEY));
    let dump = st.db(|s| {
        let mut out = String::new();
        let c = s.raw();
        let mut stmt = c.prepare("SELECT id,name,dialect,base_url,model,key_host,has_key,extra_json FROM provider_profiles").unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            for i in 0..8 {
                out.push_str(&format!("{:?}|", r.get_ref(i).unwrap()));
            }
        }
        Ok(out)
    });
    assert!(!dump.unwrap().contains(KEY));
    // keeping (None) leaves the key; the profile can be renamed
    let mut edit = p.clone();
    edit.name = "Renamed".into();
    let p2 = st.save_profile(edit, None).await.unwrap();
    assert!(p2.has_key);
    assert_eq!(secrets.get("ai-provider:p1").unwrap().as_deref(), Some(KEY));
}

#[tokio::test]
async fn invalid_profile_never_touches_the_keychain() {
    let (st, secrets) = state();
    for (url, model) in [("ftp://x/v1", "m"), ("not a url", "m"), ("https://api.example.com/v1", " ")] {
        let mut p = prof("p1", url);
        p.model = model.into();
        assert!(st.save_profile(p, Some(KEY.into())).await.is_err(), "{url}");
    }
    assert!(st.save_profile(prof("bad@id", "https://a.example/v1"), Some(KEY.into())).await.is_err());
    assert!(st.save_profile(prof("p1", "https://a.example/v1"), Some("has space".into())).await.is_err());
    assert_eq!(secrets.get("ai-provider:p1").unwrap(), None);
    assert!(st.list_profiles().unwrap().is_empty());
}

#[tokio::test]
async fn changing_the_origin_requires_reentering_the_key() {
    let (st, secrets) = state();
    st.save_profile(prof("p1", "https://api.example.com/v1"), Some(KEY.into())).await.unwrap();
    // same host, new path: fine
    st.save_profile(prof("p1", "https://api.example.com/other"), None).await.unwrap();
    // scheme downgrade / new host without a key: refused, nothing changes
    for url in ["http://api.example.com/v1", "https://evil.example.net/v1", "https://api.example.com:8443/v1"] {
        let e = st.save_profile(prof("p1", url), None).await.unwrap_err();
        assert!(e.contains("Enter the key again"), "{url}: {e}");
    }
    assert_eq!(st.list_profiles().unwrap()[0].base_url, "https://api.example.com/other");
    // clearing is allowed and removes the Keychain entry
    let p = st.save_profile(prof("p1", "https://evil.example.net/v1"), Some(String::new())).await.unwrap();
    assert!(!p.has_key && p.key_origin.is_none());
    assert_eq!(secrets.get("ai-provider:p1").unwrap(), None);
    // a new key binds to the new origin
    let p = st.save_profile(prof("p1", "https://evil.example.net/v1"), Some("k2".into())).await.unwrap();
    assert_eq!(p.key_origin.as_deref(), Some("https://evil.example.net:443"));
}

#[tokio::test]
async fn stale_database_origin_is_flagged() {
    let (st, _) = state();
    st.save_profile(prof("p1", "https://api.example.com/v1"), Some(KEY.into())).await.unwrap();
    // simulate a base-url edit that bypassed save_profile
    st.db(|s| s.raw().execute("UPDATE provider_profiles SET base_url='https://other.example.org/v1'", []).map(|_| ()).map_err(Into::into)).unwrap();
    assert!(st.list_profiles().unwrap()[0].needs_key_reentry);
    let conv_err = {
        // conversation creation is fine, sending is refused before any network
        let c = st.new_conversation("p1", consent()).unwrap();
        let sink = VecSink::default();
        st.send(c.id, "hi".into(), &sink).await.unwrap_err()
    };
    assert!(conv_err.contains("Enter the key again"));
}

#[tokio::test]
async fn warnings_for_cleartext_lan() {
    let (st, _) = state();
    let p = st.save_profile(prof("lan", "http://192.168.1.5:11434/v1"), None).await.unwrap();
    let kinds: Vec<&str> = p.warnings.iter().map(|w| w.kind.as_str()).collect();
    assert_eq!(kinds, vec!["cleartextRemote", "lanHost"]);
    assert!(p.warnings[0].message.contains("unencrypted"));
    let local = st.save_profile(prof("lo", "http://localhost:11434/v1"), None).await.unwrap();
    assert!(local.warnings.is_empty());
}

#[tokio::test]
async fn delete_profile_removes_the_keychain_entry() {
    let (st, secrets) = state();
    st.save_profile(prof("p1", "https://api.example.com/v1"), Some(KEY.into())).await.unwrap();
    st.delete_profile("p1").await.unwrap();
    assert_eq!(secrets.get("ai-provider:p1").unwrap(), None);
    assert!(st.list_profiles().unwrap().is_empty());
    st.delete_profile("p1").await.unwrap(); // idempotent
}

#[tokio::test]
async fn test_profile_openai_lists_models_and_pings_without_persisting() {
    let models = Script::json(200, "{\"data\":[{\"id\":\"m2\"},{\"id\":\"m1\"},{\"id\":\"m1\"}]}");
    let m = Mock::start(vec![models, sse(OPENAI_SSE)]).await;
    let (st, secrets) = state();
    let r = st.test_profile(prof("t", &format!("{}/v1", m.url())), Some(KEY.into())).await;
    assert!(r.ok, "{}", r.message);
    assert_eq!(r.models, vec!["m1", "m2"]);
    assert!(r.supports_tools);
    let reqs = m.requests();
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[0].path, "/v1/models");
    assert_eq!(reqs[0].header("authorization"), Some(&*format!("Bearer {KEY}")));
    assert_eq!(reqs[1].path, "/v1/chat/completions");
    let body: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert_eq!(body["max_tokens"], 8);
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert!(!reqs[1].body_str().to_lowercase().contains("mail"));
    // persists nothing
    assert!(st.list_profiles().unwrap().is_empty());
    assert_eq!(secrets.get("ai-provider:t").unwrap(), None);
    assert!(st.db(|s| s.list_egress(None, 10)).unwrap().is_empty());
}

#[tokio::test]
async fn test_profile_with_empty_model_detects_one_and_skips_embedding_models() {
    let models = Script::json(200, "{\"data\":[{\"id\":\"qwen3.8-27b\"},{\"id\":\"all-minilm-l6-v2\"}]}");
    let m = Mock::start(vec![models, sse(OPENAI_SSE)]).await;
    let (st, _secrets) = state();
    let mut p = prof("t", &format!("{}/v1", m.url()));
    p.model = String::new();
    let r = st.test_profile(p, Some(KEY.into())).await;
    assert!(r.ok, "{}", r.message);
    assert_eq!(r.models, vec!["qwen3.8-27b"]);
    let reqs = m.requests();
    let body: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    assert_eq!(body["model"], "qwen3.8-27b");
}

#[tokio::test]
async fn test_profile_with_empty_model_and_no_list_asks_for_a_name() {
    let m = Mock::start(vec![Script::json(404, "{}")]).await;
    let (st, _secrets) = state();
    let mut p = prof("t", &format!("{}/v1", m.url()));
    p.model = String::new();
    let r = st.test_profile(p, Some(KEY.into())).await;
    assert!(!r.ok);
    assert!(r.message.contains("model name"), "{}", r.message);
}

#[tokio::test]
async fn test_profile_falls_back_when_tools_are_rejected() {
    let m = Mock::start(vec![
        Script::json(200, "{\"data\":[]}"),
        Script::json(400, "{\"error\":{\"message\":\"tools not supported\"}}"),
        sse(OPENAI_SSE),
    ])
    .await;
    let (st, _) = state();
    let r = st.test_profile(prof("t", &format!("{}/v1", m.url())), None).await;
    assert!(r.ok && !r.supports_tools, "{}", r.message);
    assert!(r.message.contains("plain chat"));
    assert!(m.requests()[2].body_str().find("\"tools\"").is_none());
}

#[tokio::test]
async fn test_profile_anthropic_pings_messages_only() {
    let body = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"c\",\"stop_reason\":null,\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n\
event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let m = Mock::start(vec![sse(body)]).await;
    let (st, _) = state();
    let mut p = prof("t", &m.url());
    p.kind = "anthropic".into();
    let r = st.test_profile(p, Some(KEY.into())).await;
    assert!(r.ok, "{}", r.message);
    let reqs = m.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].path, "/v1/messages");
    assert_eq!(reqs[0].header("x-api-key"), Some(KEY));
    assert_eq!(reqs[0].header("anthropic-version"), Some("2026-10-21"));
}

#[tokio::test]
async fn test_profile_reports_failures_without_the_key() {
    let m = Mock::start(vec![Script::json(401, &format!("{{\"error\":{{\"message\":\"bad key {KEY}\"}}}}"))]).await;
    let (st, _) = state();
    let r = st.test_profile(prof("t", &format!("{}/v1", m.url())), Some(KEY.into())).await;
    assert!(!r.ok);
    assert!(r.message.contains("401") && !r.message.contains(KEY), "{}", r.message);
    let r = st.test_profile(prof("t", "nonsense"), None).await;
    assert!(!r.ok && r.message.contains("URL"));
}

#[tokio::test]
async fn test_profile_uses_saved_key_only_for_the_same_origin() {
    let m = Mock::start(vec![Script::json(200, "{\"data\":[]}"), sse(OPENAI_SSE)]).await;
    let (st, _) = state();
    let url = format!("{}/v1", m.url());
    st.save_profile(prof("p1", &url), Some(KEY.into())).await.unwrap();
    let r = st.test_profile(prof("p1", &url), None).await;
    assert!(r.ok, "{}", r.message);
    assert_eq!(m.requests()[0].header("authorization"), Some(&*format!("Bearer {KEY}")));
    let before = m.requests().len();
    let r = st.test_profile(prof("p1", "http://127.0.0.1:9/v1"), None).await;
    assert!(!r.ok && r.message.contains("Enter the key again"));
    assert_eq!(m.requests().len(), before);
}

async fn chat_env(m: &Mock) -> (AiState, Arc<InMemorySecretStore>, i64) {
    let (st, secrets) = state();
    st.save_profile(prof("p1", &format!("{}/v1", m.url())), Some(KEY.into())).await.unwrap();
    let c = st.new_conversation("p1", consent()).unwrap();
    (st, secrets, c.id)
}

#[tokio::test]
async fn send_streams_persists_and_logs_egress() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let (st, _, cid) = chat_env(&m).await;
    let sink = VecSink::default();
    st.send(cid, "Hello assistant".into(), &sink).await.unwrap();
    assert_eq!(sink.text(), "Hello there");
    assert!(matches!(sink.events().last(), Some(engine::TurnEvent::Done { stop }) if stop == "end_turn"));
    let c = st.get_conversation(cid).unwrap();
    assert_eq!(c.conversation.title, "Hello assistant");
    assert_eq!(c.messages.len(), 2);
    assert_eq!((c.messages[0].role.as_str(), c.messages[0].text.as_str()), ("user", "Hello assistant"));
    assert_eq!((c.messages[1].role.as_str(), c.messages[1].text.as_str()), ("assistant", "Hello there"));
    let eg = st.egress(cid, 50).unwrap();
    assert_eq!(eg.len(), 1);
    assert!(eg[0].host.starts_with("127.0.0.1:"));
    assert_eq!(eg[0].model, "m1");
    assert!(eg[0].request_json.contains("Hello assistant") && !eg[0].request_json.contains(KEY));
    assert!(eg[0].items.is_empty());
    assert_eq!(m.requests()[0].header("authorization"), Some(&*format!("Bearer {KEY}")));
    assert_eq!(st.list_conversations().unwrap().len(), 1);
    st.delete_conversation(cid).unwrap();
    assert!(st.get_conversation(cid).is_err());
    assert!(st.egress(cid, 10).unwrap().is_empty(), "deleting a conversation removes its egress log");
}

#[tokio::test]
async fn send_failures_arrive_as_error_events_and_preflight_rejects() {
    let m = Mock::start(vec![Script::json(429, "{\"error\":{\"message\":\"slow\"}}")]).await;
    let (st, _, cid) = chat_env(&m).await;
    let sink = VecSink::default();
    st.send(cid, "hi".into(), &sink).await.unwrap();
    match sink.events().last().unwrap() {
        engine::TurnEvent::Error { kind, message } => {
            assert_eq!(kind, "rateLimited");
            assert!(!message.contains(KEY));
        }
        e => panic!("{e:?}"),
    }
    assert!(st.send(cid, "  ".into(), &sink).await.is_err());
    assert!(st.send(9999, "hi".into(), &sink).await.unwrap_err().contains("no longer exists"));
    st.delete_profile("p1").await.unwrap();
    assert!(st.send(cid, "hi".into(), &sink).await.unwrap_err().contains("deleted"));
}

#[tokio::test]
async fn send_refuses_when_profile_endpoint_or_model_changed_since_consent() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let (st, _) = state();
    st.save_profile(prof("p1", &format!("{}/v1", m.url())), None).await.unwrap();
    let c = st.new_conversation("p1", consent()).unwrap();
    // Model changed.
    let mut p = prof("p1", &format!("{}/v1", m.url()));
    p.model = "other-model".into();
    st.save_profile(p, None).await.unwrap();
    let sink = VecSink::default();
    assert!(st.send(c.id, "hi".into(), &sink).await.unwrap_err().contains("start a new conversation"));
    // Host changed.
    st.save_profile(prof("p1", "http://127.0.0.1:1/v1"), None).await.unwrap();
    assert!(st.send(c.id, "hi".into(), &sink).await.unwrap_err().contains("start a new conversation"));
    assert_eq!(m.connections(), 0, "nothing was sent to either endpoint");
}

#[tokio::test]
async fn second_send_is_rejected_while_one_runs_and_cancel_stops_it() {
    let m = Mock::start(vec![Script::sse_head().hang()]).await;
    let (st, _, cid) = chat_env(&m).await;
    let st = Arc::new(st);
    let sink = Arc::new(VecSink::default());
    let (s2, k2) = (st.clone(), sink.clone());
    let h = tokio::spawn(async move { s2.send(cid, "first".into(), &*k2).await });
    for _ in 0..300 {
        if !m.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let other = VecSink::default();
    let e = st.send(cid, "second".into(), &other).await.unwrap_err();
    assert!(e.contains("still answering"));
    assert!(other.events().is_empty());
    st.cancel(cid);
    h.await.unwrap().unwrap();
    assert!(matches!(sink.events().last(), Some(engine::TurnEvent::Done { stop }) if stop == "cancelled"));
    // free again
    assert!(!st.inflight.is_running(cid));
}

#[tokio::test]
async fn caps_from_settings_apply() {
    let m = Mock::start(vec![sse(OPENAI_SSE)]).await;
    let (st, _, cid) = chat_env(&m).await;
    let mut s = st.get_settings().unwrap();
    s.per_turn_tokens = 1000;
    s.per_conversation_tokens = 5000;
    st.set_settings(s).unwrap();
    let sink = VecSink::default();
    st.send(cid, "word ".repeat(1500), &sink).await.unwrap();
    assert!(matches!(sink.events().last(), Some(engine::TurnEvent::Error { kind, .. }) if kind == "budget"));
    assert!(m.requests().is_empty());
}

#[test]
fn settings_roundtrip_and_validation() {
    let (st, _) = state();
    let d = st.get_settings().unwrap();
    assert_eq!((d.per_turn_tokens, d.per_conversation_tokens, d.per_day_tokens), (30_000, 300_000, 1_000_000));
    assert_eq!(d.purge.egress_retention_days, Some(30));
    assert_eq!(d.purge.conversation_retention_days, None);
    assert!(d.purge.purge_on_account_removal);
    assert!(d.default_redaction.strip_quotes && d.default_redaction.shorten_links && !d.default_redaction.mask_addresses);
    let mut s = d;
    s.per_turn_tokens = 50_000;
    s.default_redaction.mask_addresses = true;
    s.purge.egress_retention_days = None;
    s.purge.conversation_retention_days = Some(90);
    assert_eq!(st.set_settings(s).unwrap(), s);
    assert_eq!(st.get_settings().unwrap(), s);
    let mut bad = s;
    bad.per_turn_tokens = 10;
    assert!(st.set_settings(bad).is_err());
    let mut bad = s;
    bad.per_conversation_tokens = 100;
    assert!(st.set_settings(bad).is_err());
    let mut bad = s;
    bad.purge.egress_retention_days = Some(99_999);
    assert!(st.set_settings(bad).is_err());
}

#[test]
fn purge_follows_settings() {
    let (st, _) = state();
    st.db(|s| {
        s.create_conversation(&NewConversation { title: "t".into(), profile_id: None, endpoint_host: "h".into(), model: "m".into(), consent_json: "{}".into(), accounts: vec!["a@x.org".into()] }, 1_000)?;
        s.record_egress(
            &aichat::store::NewEgress { round: 0, conv_id: None, profile_id: None, endpoint_host: "h".into(), model: "m".into(), request_json: "{}".into(), bytes: 1, est_tokens: 1, items_json: "[]".into(), redactions_json: "{}".into() },
            1_000,
        )?;
        Ok(())
    })
    .unwrap();
    // defaults: egress 30 days -> old row goes, conversations kept
    let r = st.purge_now().unwrap();
    assert_eq!((r.egress_deleted, r.conversations_deleted), (1, 0));
    let mut s = st.get_settings().unwrap();
    s.purge.conversation_retention_days = Some(1);
    st.set_settings(s).unwrap();
    st.startup_purge();
    assert!(st.list_conversations().unwrap().is_empty());
}

#[test]
fn account_removal_purges_only_when_enabled() {
    let (st, _) = state();
    let mk = |st: &AiState| {
        st.db(|s| s.create_conversation(&NewConversation { title: "t".into(), profile_id: None, endpoint_host: "h".into(), model: "m".into(), consent_json: "{}".into(), accounts: vec!["a@x.org".into()] }, engine::system_clock()()))
            .unwrap()
    };
    mk(&st);
    st.purge_account("a@x.org");
    assert!(st.list_conversations().unwrap().is_empty());
    let mut s = st.get_settings().unwrap();
    s.purge.purge_on_account_removal = false;
    st.set_settings(s).unwrap();
    mk(&st);
    st.purge_account("a@x.org");
    assert_eq!(st.list_conversations().unwrap().len(), 1);
}

#[test]
fn new_conversation_needs_a_profile_and_keeps_consent() {
    let (st, _) = state();
    assert!(st.new_conversation("nope", consent()).is_err());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(st.save_profile(prof("p1", "http://localhost:11434/v1"), None)).unwrap();
    let c = st.new_conversation("p1", consent()).unwrap();
    assert_eq!((c.endpoint_host.as_str(), c.model.as_str(), c.profile_id.as_str()), ("localhost:11434", "m1", "p1"));
    let stored = st.db(|s| s.get_conversation(c.id)).unwrap().unwrap();
    let v: Value = serde_json::from_str(&stored.consent_json).unwrap();
    assert_eq!(v["scopes"][0]["account"], "a@x.org");
    assert_eq!(v["redaction"]["stripQuotes"], true);
    assert_eq!(stored.accounts, vec!["a@x.org"]);
}

#[test]
fn wire_shapes_are_camel_case() {
    use commands::map_event;
    let j = |e: AiEvent| serde_json::to_value(e).unwrap();
    assert_eq!(j(map_event(engine::TurnEvent::Text("x".into()))), json!({"type": "text", "delta": "x"}));
    assert_eq!(j(map_event(engine::TurnEvent::Usage { input: 1, output: 2 })), json!({"type": "usage", "input": 1, "output": 2}));
    assert_eq!(j(AiEvent::Done { stop: "end_turn".into() }), json!({"type": "done", "stop": "end_turn"}));
    assert_eq!(j(AiEvent::Error { message: "m".into(), kind: "auth".into() }), json!({"type": "error", "message": "m", "kind": "auth"}));
    assert_eq!(j(AiEvent::ToolStart { id: "1".into(), name: "n".into(), label: "l".into() })["type"], "toolStart");
    let p = serde_json::to_value(prof("p", "https://a.example/v1")).unwrap();
    for k in ["id", "name", "kind", "baseUrl", "model", "maxOutputTokens", "hasKey", "needsKeyReentry", "warnings"] {
        assert!(p.get(k).is_some(), "{k}");
    }
    assert!(p.get("keyOrigin").is_none() && p.get("testedAt").is_none());
    // input without computed fields deserialises
    let back: Profile = serde_json::from_value(json!({"id":"","name":"n","kind":"anthropic","baseUrl":"https://api.anthropic.com","model":"m"})).unwrap();
    assert_eq!(back.max_output_tokens, 0);
    let s = serde_json::to_value(Settings::default()).unwrap();
    assert_eq!(s["purge"]["egressRetentionDays"], 30);
    assert!(s["purge"]["conversationRetentionDays"].is_null());
    assert_eq!(s["defaultRedaction"]["stripQuotes"], true);
    let row = serde_json::to_value(EgressRow { id: 1, round: 0, at: 2, host: "h".into(), model: "m".into(), bytes: 3, est_tokens: 4, request_json: "{}".into(), items: vec![EgressItem { reference: "m:1".into(), subject: "s".into(), from: "f".into() }] }).unwrap();
    assert_eq!(row["estTokens"], 4);
    assert_eq!(row["items"][0]["ref"], "m:1");
}

#[test]
fn key_account_cannot_collide_with_mail_accounts() {
    assert_eq!(key_account("p-1"), "ai-provider:p-1");
    assert!(!new_id().contains('@') && valid_id(&new_id()));
    assert_ne!(new_id(), new_id());
}

// ---------------------------------------------------------------------------------------------
// P1: read-only mail tools over the real mailcore storage + AppMailHost (no network to a mail server)
// ---------------------------------------------------------------------------------------------

mod tools_p1 {
    use super::*;
    use crate::ai::host::AppMailHost;
    use mailcore::{AccountConfig, Flags, Folder, FolderRole, MessageSummary, ServerConfig, Storage, Tls};

    const ACC: &str = "me@example.org";
    const INJECTION_EML: &str = include_str!("../../../../crates/aichat/tests/fixtures/injection.eml");

    fn seeded_storage() -> Arc<Storage> {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_account(&AccountConfig {
            email: ACC.into(),
            username: "me".into(),
            imap: ServerConfig { host: "imap.invalid".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "smtp.invalid".into(), port: 587, tls: Tls::StartTls },
        })
        .unwrap();
        let f = |n: &str, r| Folder { name: n.into(), delimiter: Some("/".into()), role: r, selectable: true };
        s.replace_folders(ACC, &[f("INBOX", FolderRole::Inbox), f("Archive", FolderRole::Archive)]).unwrap();
        s.set_folder_sync_state(ACC, "INBOX", 7, Some(100)).unwrap();
        s.set_folder_sync_state(ACC, "Archive", 7, Some(100)).unwrap();
        let put = |folder: &str, uid: u32, subject: &str, from: &str, raw: &[u8]| {
            s.upsert_message(&MessageSummary {
                account: ACC.into(),
                folder: folder.into(),
                uid,
                uidvalidity: 7,
                message_id: Some(format!("m{uid}@x")),
                subject: subject.into(),
                from: from.into(),
                to: ACC.into(),
                date: 1_793_000_000 + i64::from(uid),
                flags: Flags::default(),
                snippet: String::new(),
                has_body: false,
                size: raw.len() as u32,
            })
            .unwrap();
            s.set_body(ACC, folder, uid, 7, raw, "").unwrap();
        };
        put("INBOX", 1, "Invoice 4471 URGENT action required", "Mallory <mallory@evil.test>", INJECTION_EML.as_bytes());
        put("INBOX", 2, "Lunch plans", "Bob <bob@shop.test>", b"From: bob@shop.test\r\nSubject: Lunch plans\r\n\r\nPizza on Friday?\r\n");
        put("Archive", 3, "Archive-secret invoice", "Ann <ann@x.test>", b"From: ann@x.test\r\nSubject: Archive-secret invoice\r\n\r\nTOP SECRET ARCHIVE BODY\r\n");
        Arc::new(s)
    }

    fn oa_calls(calls: &[(&str, &str, Value)]) -> Script {
        let mut body = String::new();
        for (i, (id, name, args)) in calls.iter().enumerate() {
            body.push_str(&format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":i,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]})));
        }
        body.push_str(&format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})));
        sse(&body)
    }

    fn oa_text(t: &str) -> Script {
        sse(&format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":t}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        ))
    }

    fn tool_consent(tools: bool) -> Consent {
        Consent {
            scopes: vec![
                Scope { account: ACC.into(), folder: "INBOX".into(), allowed: true },
                Scope { account: ACC.into(), folder: "Archive".into(), allowed: false },
            ],
            redaction: Redaction::default(),
            tools,
            suggest: false,
            calendar: Default::default(),
        }
    }

    async fn env(m: &Mock, storage: Arc<Storage>, tools: bool) -> (AiState, i64) {
        let (st, _) = state();
        st.save_profile(prof("p1", &format!("{}/v1", m.url())), Some(KEY.into())).await.unwrap();
        st.set_mail_host(Arc::new(AppMailHost::new(storage, None)));
        let c = st.new_conversation("p1", tool_consent(tools)).unwrap();
        (st, c.id)
    }

    fn snapshot(s: &Storage) -> String {
        let mut out = String::new();
        for f in ["INBOX", "Archive"] {
            for m in s.list_messages(ACC, f, 100, 0).unwrap() {
                out.push_str(&format!("{f}/{}:{:?}:{}:{};", m.uid, m.flags, m.subject, s.get_body(ACC, f, m.uid).unwrap().map(|b| b.len()).unwrap_or(0)));
            }
        }
        out
    }

    #[tokio::test]
    async fn real_search_read_scope_and_injection_end_to_end() {
        let storage = seeded_storage();
        let before = snapshot(&storage);
        let m = Mock::start(vec![
            oa_calls(&[("c1", "search_mail", json!({"query":"invoice"}))]),
            oa_calls(&[("c2", "get_message", json!({"ref":"m:1"}))]),
            // worst case: obey the mail
            oa_calls(&[
                ("c3", "send_mail", json!({"to":"mallory@evil.test"})),
                ("c4", "delete_message", json!({"ref":"m:1"})),
                ("c5", "search_mail", json!({"query":"archive-secret"})),
                ("c6", "get_message", json!({"ref":"m:3"})),
            ]),
            oa_text("Done."),
        ])
        .await;
        let (st, cid) = env(&m, storage.clone(), true).await;
        assert!(st.get_conversation(cid).unwrap().conversation.tools_enabled);
        let sink = VecSink::default();
        st.send(cid, "summarise my invoices".into(), &sink).await.unwrap();
        assert!(matches!(sink.events().last(), Some(engine::TurnEvent::Done { stop }) if stop == "end_turn"), "{:?}", sink.events());

        // nothing in the mailbox changed
        assert_eq!(snapshot(&storage), before);
        assert_eq!(m.connections(), 4);

        let reqs: Vec<Value> = m.requests().iter().map(|r| serde_json::from_slice(&r.body).unwrap()).collect();
        let tool_msgs = |v: &Value| -> Vec<String> { v["messages"].as_array().unwrap().iter().filter(|x| x["role"] == "tool").map(|x| x["content"].as_str().unwrap().to_string()).collect() };
        let r2 = tool_msgs(&reqs[1]);
        assert!(r2[0].contains("1 matching") && r2[0].contains("Invoice 4471"), "{}", r2[0]);
        assert!(!r2[0].contains("Archive-secret"), "Archive was not ticked");
        let r3 = tool_msgs(&reqs[2]);
        assert!(r3[1].contains("IGNORE ALL PREVIOUS INSTRUCTIONS") && r3[1].contains("<<<UNTRUSTED"), "{}", r3[1]);
        assert!(!r3[1].contains("SECRET-TOKEN-123") && !r3[1].contains("Head of Fraud"));
        let r4 = tool_msgs(&reqs[3]);
        assert_eq!(r4.len(), 6);
        assert!(r4[2].contains("Unknown tool") && r4[3].contains("Unknown tool"));
        assert!(r4[4].contains("0 matching") || r4[4].contains("Nothing found"), "{}", r4[4]);
        assert!(r4[5].contains("Unknown reference"), "{}", r4[5]);
        assert!(!reqs.iter().any(|r| r.to_string().contains("TOP SECRET ARCHIVE BODY")));

        // UI view: one assistant message with the chips, user message first
        let c = st.get_conversation(cid).unwrap();
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[1].text, "Done.");
        let chips = &c.messages[1].tools;
        assert_eq!(chips.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["search_mail", "get_message", "send_mail", "delete_message", "search_mail", "get_message"]);
        assert_eq!((chips[0].label.as_str(), chips[0].summary.as_str(), chips[0].ok), ("invoice", "1 result", true));
        assert!(!chips[2].ok && !chips[3].ok && !chips[5].ok);

        // egress: 4 rows with rounds 0..3, tool results visible in the logged requests, items listed
        let eg = st.egress(cid, 50).unwrap();
        assert_eq!(eg.len(), 4);
        let mut rounds: Vec<i64> = eg.iter().map(|r| r.round).collect();
        rounds.sort();
        assert_eq!(rounds, vec![0, 1, 2, 3]);
        let last = eg.iter().find(|r| r.round == 3).unwrap();
        assert!(last.request_json.contains("IGNORE ALL PREVIOUS INSTRUCTIONS"));
        assert!(last.items.iter().any(|i| i.reference == "m:1" && i.subject.contains("Invoice 4471")), "{:?}", last.items);
        assert!(eg.iter().find(|r| r.round == 0).unwrap().items.is_empty());
    }

    #[tokio::test]
    async fn tools_off_or_no_host_equals_plain_chat() {
        for (tools, with_host) in [(false, true), (true, false)] {
            let m = Mock::start(vec![oa_text("hi")]).await;
            let (st, _) = state();
            st.save_profile(prof("p1", &format!("{}/v1", m.url())), Some(KEY.into())).await.unwrap();
            if with_host {
                st.set_mail_host(Arc::new(AppMailHost::new(seeded_storage(), None)));
            }
            let c = st.new_conversation("p1", tool_consent(tools)).unwrap();
            let sink = VecSink::default();
            st.send(c.id, "hello".into(), &sink).await.unwrap();
            let body: Value = serde_json::from_slice(&m.requests()[0].body).unwrap();
            assert!(body.get("tools").is_none(), "tools={tools} host={with_host}");
            assert!(!body.to_string().contains("UNTRUSTED"));
        }
    }

    #[tokio::test]
    async fn legacy_consent_without_tools_field_stays_plain() {
        let c: Consent = serde_json::from_str(r#"{"scopes":[{"account":"a","folder":"INBOX","allowed":true}],"redaction":{"stripQuotes":true,"shortenLinks":true,"maskAddresses":false}}"#).unwrap();
        assert!(!c.tools);
    }

    #[tokio::test]
    async fn host_search_and_thread_respect_scope_and_validity() {
        let storage = seeded_storage();
        let host = AppMailHost::new(storage.clone(), None);
        let inbox = vec![aichat::tools::Scope { account: ACC.into(), folder: "INBOX".into() }];
        let r = host.search(&inbox, "from:bob", 10, 0).await.unwrap();
        assert_eq!(r.hits.len(), 1);
        assert!(host.search(&inbox, "archive-secret", 10, 0).await.unwrap().hits.is_empty());
        assert_eq!(MailToolHost::folder_validity(&host, ACC, "INBOX").unwrap(), Some(7));
        // stale validity: the message is refused
        let scope = inbox[0].clone();
        assert!(host.get_message(&scope, 2, 8).await.is_err());
        assert!(host.get_message(&scope, 2, 7).await.unwrap().body.contains("Pizza"));
        // no connection factory: an uncached body cannot be fetched, and server search is unavailable
        assert!(host.server_search(&scope, Some(7), &aichat::tools::ServerSearchArgs { from: Some("x".into()), ..Default::default() }, 5).await.is_err());
        // hostile server-search criteria are rejected before any connection
        let bad = aichat::tools::ServerSearchArgs { subject: Some("x\r\nA1 DELETE INBOX".into()), ..Default::default() };
        assert!(host.server_search(&scope, Some(7), &bad, 5).await.is_err());
    }
}

// ---------------------------------------------------------------------------------------------
// P2: proposals through AiState + the real AppMailHost (no mail server: connect is None, so any
// attempt to change mail fails instead of reaching a network)
// ---------------------------------------------------------------------------------------------

mod p2 {
    use super::*;
    use crate::ai::host::AppMailHost;
    use mailcore::{AccountConfig, Flags, Folder, FolderRole, MessageSummary, ServerConfig, Storage, Tls};

    pub(super) const ACC: &str = "me@example.org";

    pub(super) fn storage() -> Arc<Storage> {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_account(&AccountConfig {
            email: ACC.into(),
            username: "me".into(),
            imap: ServerConfig { host: "imap.invalid".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "smtp.invalid".into(), port: 587, tls: Tls::StartTls },
        })
        .unwrap();
        let f = |n: &str, r| Folder { name: n.into(), delimiter: Some("/".into()), role: r, selectable: true };
        s.replace_folders(ACC, &[f("INBOX", FolderRole::Inbox), f("Archive", FolderRole::Archive)]).unwrap();
        s.set_folder_sync_state(ACC, "INBOX", 7, Some(100)).unwrap();
        for (uid, subject) in [(1u32, "Invoice 1"), (2, "Invoice 2")] {
            s.upsert_message(&MessageSummary {
                account: ACC.into(),
                folder: "INBOX".into(),
                uid,
                uidvalidity: 7,
                message_id: Some(format!("m{uid}@x")),
                subject: subject.into(),
                from: "Bob <bob@shop.test>".into(),
                to: ACC.into(),
                date: 1_793_000_000,
                flags: Flags::default(),
                snippet: String::new(),
                has_body: false,
                size: 10,
            })
            .unwrap();
            s.set_body(ACC, "INBOX", uid, 7, format!("From: bob@shop.test\r\nSubject: {subject}\r\n\r\nPay up\r\n").as_bytes(), "").unwrap();
        }
        Arc::new(s)
    }

    pub(super) fn oa_calls(calls: &[(&str, &str, Value)]) -> Script {
        let mut body = String::new();
        for (i, (id, name, args)) in calls.iter().enumerate() {
            body.push_str(&format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":i,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]})));
        }
        body.push_str(&format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})));
        sse(&body)
    }

    pub(super) fn oa_text(t: &str) -> Script {
        sse(&format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{"content":t}}]}), json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})))
    }

    pub(super) fn consent(suggest: bool) -> Consent {
        Consent { scopes: vec![Scope { account: ACC.into(), folder: "INBOX".into(), allowed: true }], redaction: Redaction::default(), tools: true, suggest, calendar: Default::default() }
    }

    async fn setup(m: &Mock, storage: Arc<Storage>, suggest: bool) -> (AiState, i64) {
        let (st, _) = state();
        st.save_profile(prof("p1", &format!("{}/v1", m.url())), Some(KEY.into())).await.unwrap();
        let host = Arc::new(AppMailHost::new(storage, None));
        st.set_mail_host(host.clone());
        st.set_action_host(host);
        let c = st.new_conversation("p1", consent(suggest)).unwrap();
        (st, c.id)
    }

    pub(super) fn snapshot(s: &Storage) -> String {
        s.list_messages(ACC, "INBOX", 100, 0).unwrap().iter().map(|m| format!("{}:{:?}", m.uid, m.flags)).collect::<Vec<_>>().join(";")
    }

    #[tokio::test]
    async fn consent_flag_gates_the_proposal_tools_and_old_consents_stay_read_only() {
        let c: Consent = serde_json::from_str(r#"{"scopes":[],"redaction":{"stripQuotes":true,"shortenLinks":true,"maskAddresses":false},"tools":true}"#).unwrap();
        assert!(c.tools && !c.suggest, "consents saved before P2 are read-only");
        for (suggest, n) in [(false, 7usize), (true, 14)] {
            let m = Mock::start(vec![oa_text("hi")]).await;
            let (st, cid) = setup(&m, storage(), suggest).await;
            assert_eq!(st.get_conversation(cid).unwrap().conversation.suggest_enabled, suggest);
            st.send(cid, "hello".into(), &VecSink::default()).await.unwrap();
            let body: Value = serde_json::from_slice(&m.requests()[0].body).unwrap();
            assert_eq!(body["tools"].as_array().unwrap().len(), n, "suggest={suggest}");
        }
    }

    #[tokio::test]
    async fn propose_then_approve_runs_through_the_executor_and_failures_are_honest() {
        let storage = storage();
        let before = snapshot(&storage);
        let m = Mock::start(vec![
            oa_calls(&[("c1", "search_mail", json!({"query":"invoice"}))]),
            oa_calls(&[("c2", "propose_archive", json!({"refs":["m:1","m:2"],"reason":"Both are paid."}))]),
            oa_text("I queued a suggestion."),
        ])
        .await;
        let (st, cid) = setup(&m, storage.clone(), true).await;
        let sink = VecSink::default();
        st.send(cid, "archive my paid invoices".into(), &sink).await.unwrap();
        let ev: Vec<_> = sink.events().into_iter().filter_map(|e| if let engine::TurnEvent::Proposal { id, title } = e { Some((id, title)) } else { None }).collect();
        assert_eq!(ev.len(), 1);
        let (pid, title) = ev.into_iter().next().unwrap();
        assert_eq!(title, "Archive 2 messages");
        assert_eq!(snapshot(&storage), before, "proposing changed nothing");

        // dry run: sender, subject, from-folder, to-folder; model text only as the reason
        let d = st.get_proposal(pid).unwrap();
        assert_eq!((d.summary.state.as_str(), d.summary.kind.as_str(), d.summary.reason.as_str()), ("proposed", "archive", "Both are paid."));
        assert_eq!(d.items.len(), 2);
        let mut subjects: Vec<&str> = d.items.iter().map(|i| i.subject.as_str()).collect();
        subjects.sort();
        assert_eq!(subjects, vec!["Invoice 1", "Invoice 2"]);
        assert!(d.items.iter().all(|i| i.folder == "INBOX" && i.to_folder == "Archive" && i.sender.contains("bob@shop.test")));
        assert!(d.summary.warnings.iter().any(|w| w.contains("includes 2 unread")), "{:?}", d.summary.warnings);
        assert_eq!(st.list_proposals(Some(cid), 10).unwrap().len(), 1);
        assert_eq!(st.list_proposals(None, 10).unwrap()[0].id, pid);

        // approve: there is no mail server in this test, so the batch fails; nothing changed and the
        // outcome says so honestly instead of pretending
        let events = Arc::new(Mutex::new(Vec::<i64>::new()));
        let e2 = events.clone();
        let progress = move |id: i64| e2.lock().unwrap().push(id);
        let (res, affected) = st.approve_proposal(pid, vec![0], &progress).await.unwrap();
        assert_eq!(res.proposal.summary.state, "failed");
        assert_eq!((res.ok, res.failed), (0, 1));
        assert!(affected.is_empty());
        assert!(events.lock().unwrap().len() >= 2, "progress events were emitted");
        assert_eq!(snapshot(&storage), before);
        let d = st.get_proposal(pid).unwrap();
        assert!(!d.items[1].selected && d.items[1].status == "pending");
        assert!(d.items[0].error.contains("not available"), "{}", d.items[0].error);
        // a decided proposal cannot be approved again
        assert!(st.approve_proposal(pid, vec![0, 1], &progress).await.is_err());
        assert!(st.undo_proposal(pid, &progress).await.is_err());
    }

    #[tokio::test]
    async fn approve_is_refused_once_suggestions_are_turned_off_and_selects_fail_closed() {
        let storage = storage();
        let before = snapshot(&storage);
        let m = Mock::start(vec![
            oa_calls(&[("c1", "search_mail", json!({"query":"invoice"}))]),
            oa_calls(&[("c2", "propose_flag", json!({"refs":["m:1"],"flag":"read"}))]),
            oa_text("ok"),
        ])
        .await;
        let (st, cid) = setup(&m, storage.clone(), true).await;
        st.send(cid, "mark read".into(), &VecSink::default()).await.unwrap();
        let pid = st.list_proposals(Some(cid), 10).unwrap().remove(0).id;
        let progress = |_: i64| {};
        // nothing selected = nothing runs (an empty / missing list used to mean "everything")
        let e = st.approve_proposal(pid, vec![], &progress).await.unwrap_err();
        assert!(e.contains("Select at least one"), "{e}");
        // more indexes than a proposal can hold
        assert!(st.approve_proposal(pid, (0..(proposals::MAX_ITEMS as i64 + 1)).collect(), &progress).await.is_err());
        assert_eq!(st.get_proposal(pid).unwrap().summary.state, "proposed");
        // a card that belongs to a conversation where suggestions are off (turned off after it was queued)
        let off = st.new_conversation("p1", consent(false)).unwrap().id;
        let params = proposals::Params { account: Some(ACC.into()), new_folder: Some("Tax".into()), ..Default::default() };
        let np = proposals::NewProposal { conv_id: off, kind: proposals::Kind::CreateFolder, params, title: "t".into(), reason: String::new(), warnings: vec![], items: vec![] };
        let pid2 = st.store.lock().unwrap().create_proposal(&np, system_clock()()).unwrap();
        let e = st.approve_proposal(pid2, vec![0], &progress).await.unwrap_err();
        assert!(e.contains("turned off"), "{e}");
        assert_eq!(st.get_proposal(pid2).unwrap().summary.state, "proposed", "refusal changes nothing");
        assert_eq!(snapshot(&storage), before);
    }

    #[tokio::test]
    async fn reject_and_event_snapshot() {
        let storage = storage();
        let m = Mock::start(vec![
            oa_calls(&[("c1", "search_mail", json!({"query":"invoice"}))]),
            oa_calls(&[("c2", "propose_flag", json!({"refs":["m:1"],"flag":"read"}))]),
            oa_text("ok"),
        ])
        .await;
        let (st, cid) = setup(&m, storage.clone(), true).await;
        st.send(cid, "mark read".into(), &VecSink::default()).await.unwrap();
        let p = st.list_proposals(Some(cid), 10).unwrap().remove(0);
        assert_eq!((p.title.as_str(), p.state.as_str()), ("Mark 1 message as read", "proposed"));
        let d = st.reject_proposal(p.id).unwrap();
        assert_eq!(d.summary.state, "rejected");
        assert_eq!(st.proposal_event(p.id).unwrap().state, "rejected");
        assert!(st.reject_proposal(p.id).is_err());
        assert!(st.get_proposal(9999).is_err());
    }

    #[tokio::test]
    async fn host_lookup_roles_and_exists_use_the_local_cache() {
        let host = AppMailHost::new(storage(), None);
        assert_eq!(MailActionHost::folder_validity(&host, ACC, "INBOX").unwrap(), Some(7));
        let m = host.lookup(ACC, "INBOX", 1).unwrap().unwrap();
        assert_eq!((m.subject.as_str(), m.uidvalidity, m.seen), ("Invoice 1", 7, false));
        assert!(host.lookup(ACC, "INBOX", 99).unwrap().is_none());
        assert_eq!(host.role_folder(ACC, proposals::RoleFolder::Archive).unwrap().as_deref(), Some("Archive"));
        assert_eq!(host.role_folder(ACC, proposals::RoleFolder::Trash).unwrap(), None);
        assert!(host.folder_exists(ACC, "Archive").unwrap() && !host.folder_exists(ACC, "Nope").unwrap());
        // without a connection factory nothing can be changed
        let r = host.create_folder(ACC, "X").await;
        assert!(r.is_err());
        let req = proposals::MoveReq { account: ACC.into(), from_folder: "INBOX".into(), from_validity: Some(7), to_folder: "Archive".into(), items: vec![] };
        assert!(host.move_batch(req).await.is_err());
    }
}

// ---------------------------------------------------------------------------------------------
// P3: drafts through AiState + the real AppMailHost. The model can only write drafts; the user opens
// them in Compose. Nothing here can reach SMTP (connect is None and no sender exists in this module).
// ---------------------------------------------------------------------------------------------

mod p3 {
    use super::p2::{consent, oa_calls, oa_text, snapshot, storage, ACC};
    use super::*;
    use crate::ai::host::AppMailHost;
    use mailcore::{Flags, MessageSummary, Storage};

    fn storage_with_reply_to() -> Arc<Storage> {
        let s = storage();
        s.upsert_message(&MessageSummary {
            account: ACC.into(),
            folder: "INBOX".into(),
            uid: 3,
            uidvalidity: 7,
            message_id: Some("m3@x".into()),
            subject: "Invoice 3".into(),
            from: "Bob <bob@shop.test>".into(),
            to: "me@example.org, carol@other.test".into(),
            date: 1_793_000_000,
            flags: Flags::default(),
            snippet: String::new(),
            has_body: false,
            size: 10,
        })
        .unwrap();
        let raw = "From: Bob <bob@shop.test>\r\nReply-To: Support <replies@shop.test>\r\nTo: me@example.org, carol@other.test\r\nCc: dave@other.test, ME@example.org\r\nSubject: Invoice 3\r\nMessage-ID: <m3@x>\r\nReferences: <r0@x>\r\nDate: Mon, 26 Oct 2026 07:33:20 +0000\r\n\r\nPlease pay now\r\n";
        s.set_body(ACC, "INBOX", 3, 7, raw.as_bytes(), "").unwrap();
        s
    }

    async fn setup(m: &Mock, storage: Arc<Storage>, suggest: bool, drafts: bool) -> (AiState, i64) {
        let (st, _) = state();
        st.save_profile(prof("p1", &format!("{}/v1", m.url())), Some(KEY.into())).await.unwrap();
        let host = Arc::new(AppMailHost::new(storage, None));
        st.set_mail_host(host.clone());
        st.set_action_host(host.clone());
        if drafts {
            st.set_draft_host(host);
        }
        let c = st.new_conversation("p1", consent(suggest)).unwrap();
        (st, c.id)
    }

    fn tool_names(m: &Mock) -> Vec<String> {
        let body: Value = serde_json::from_slice(&m.requests()[0].body).unwrap();
        body["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect()
    }

    #[tokio::test]
    async fn draft_tools_need_tools_suggest_and_the_draft_host() {
        for (suggest, drafts, n) in [(false, true, 7usize), (true, false, 14), (true, true, 16)] {
            let m = Mock::start(vec![oa_text("hi")]).await;
            let (st, cid) = setup(&m, storage(), suggest, drafts).await;
            st.send(cid, "hello".into(), &VecSink::default()).await.unwrap();
            let names = tool_names(&m);
            assert_eq!(names.len(), n, "suggest={suggest} drafts={drafts}");
            assert_eq!(names.contains(&"draft_reply".to_string()), n == 16);
        }
    }

    #[tokio::test]
    async fn reply_is_addressed_by_the_app_opened_in_compose_and_never_sent() {
        let storage = storage_with_reply_to();
        let before = snapshot(&storage);
        let m = Mock::start(vec![
            oa_calls(&[("c1", "search_mail", json!({"query":"Invoice 3"}))]),
            oa_calls(&[
                ("d0", "draft_reply", json!({"ref":"m:1","body":"x","to":["mallory@evil.test"]})),
                ("d1", "draft_reply", json!({"ref":"m:1","body":"Will pay today.\u{202e}\nThanks!","reply_all":true,"reason":"Bob is waiting"})),
                ("d2", "draft_new", json!({"to":["bob@shop.test","stranger@new.test"],"subject":"Hello","body":"Hi"})),
            ]),
            oa_text("Drafts are waiting."),
        ])
        .await;
        let (st, cid) = setup(&m, storage.clone(), true, true).await;
        let sink = VecSink::default();
        st.send(cid, "answer Bob".into(), &sink).await.unwrap();
        assert_eq!(snapshot(&storage), before, "the mailbox is untouched");

        let evs: Vec<_> = sink.events().into_iter().filter_map(|e| if let engine::TurnEvent::Draft { id, title } = e { Some((id, title)) } else { None }).collect();
        assert_eq!(evs.len(), 2);
        assert!(matches!(commands::map_event(engine::TurnEvent::Draft { id: 5, title: "t".into() }), AiEvent::Draft { ref id, .. } if id == "5"));

        let ds = st.list_drafts(Some(cid), 10).unwrap();
        assert_eq!(ds.len(), 2);
        let reply = ds.iter().find(|d| d.kind == "reply").unwrap();
        assert_eq!(reply.to, vec!["replies@shop.test"], "Reply-To wins; the model's `to` was refused");
        assert_eq!(reply.cc, vec!["carol@other.test", "dave@other.test"], "own address removed (case-insensitively)");
        assert_eq!(reply.subject, "Re: Invoice 3");
        assert_eq!(reply.body, "Will pay today.\nThanks!");
        assert_eq!(reply.reason, "Bob is waiting");
        assert!(reply.warnings.iter().any(|w| w.contains("instead of the sender") && w.contains("bob@shop.test")), "{:?}", reply.warnings);
        assert!(reply.quote.contains("> Please pay now"), "{}", reply.quote);
        let new = ds.iter().find(|d| d.kind == "new").unwrap();
        assert_eq!(new.new_addresses, vec!["stranger@new.test"], "bob@shop.test is in the mailbox, the stranger is not");
        assert_eq!(new.account, ACC);

        // Open in Compose: prefill = body + quote, threading headers, banner flag; the state moves to opened.
        let (pre, ev) = st.open_draft(reply.id).unwrap();
        assert_eq!(ev.state, "opened");
        assert!(pre.ai_draft && pre.bcc.is_empty());
        assert_eq!((pre.to.clone(), pre.account.as_str()), (vec!["replies@shop.test".to_string()], ACC));
        assert!(pre.text.starts_with("Will pay today.\nThanks!\n\nOn 2026-10-26 07:33 UTC, Bob <bob@shop.test> wrote:\n> Please pay now"), "{}", pre.text);
        let irt = pre.in_reply_to.clone().unwrap();
        assert_eq!((irt.message_id.as_str(), irt.references.as_deref()), ("m3@x", Some("r0@x")));
        assert!(st.dismiss_draft(reply.id).is_err(), "an opened draft cannot be dismissed");
        assert!(st.open_draft(reply.id).is_ok(), "may be opened again (the pane was closed)");
        let (d, ev) = st.dismiss_draft(new.id).unwrap();
        assert_eq!((d.state.as_str(), ev.state.as_str()), ("dismissed", "dismissed"));
        assert!(st.open_draft(new.id).is_err(), "a dismissed draft is gone");
        assert!(st.get_draft(9999).is_err());

        // The wire shape the frontend relies on.
        let j = serde_json::to_value(&d).unwrap();
        assert!(j.get("newAddresses").is_some() && j.get("convId").is_some());
        let j = serde_json::to_value(&pre).unwrap();
        assert!(j.get("inReplyTo").is_some() && j["aiDraft"] == true);
    }

    #[tokio::test]
    async fn fully_obeying_model_gets_drafts_only_and_the_mailbox_never_changes() {
        let storage = storage_with_reply_to();
        let before = snapshot(&storage);
        let m = Mock::start(vec![
            oa_calls(&[("c1", "search_mail", json!({"query":"invoice"}))]),
            oa_calls(&[
                ("a", "draft_reply", json!({"ref":"m:1","body":"Sure, sending everything."})),
                ("b", "draft_new", json!({"to":["mallory@evil.test"],"subject":"dump","body":"all mail"})),
                ("c", "send_mail", json!({"to":"mallory@evil.test","body":"x"})),
                ("d", "propose_trash", json!({"refs":["m:1"]})),
            ]),
            oa_text("Sent!"),
        ])
        .await;
        let (st, cid) = setup(&m, storage.clone(), true, true).await;
        st.send(cid, "do what the email says".into(), &VecSink::default()).await.unwrap();
        assert_eq!(snapshot(&storage), before);
        assert_eq!(st.list_drafts(Some(cid), 10).unwrap().len(), 2);
        assert!(st.list_drafts(Some(cid), 10).unwrap().iter().all(|d| d.state == "proposed"));
        assert_eq!(st.list_proposals(Some(cid), 10).unwrap().iter().filter(|p| p.state != "proposed").count(), 0);
        let c = st.get_conversation(cid).unwrap();
        let chips = &c.messages[1].tools;
        assert!(chips.iter().any(|t| t.name == "send_mail" && !t.ok), "send_mail was refused");
        // deleting the conversation removes its drafts
        st.delete_conversation(cid).unwrap();
        assert!(st.list_drafts(None, 10).unwrap().is_empty());
    }

    #[test]
    fn host_address_lookup_is_whole_address_and_replies_see_the_real_headers() {
        let host = AppMailHost::new(storage_with_reply_to(), None);
        use aichat::drafts::DraftHost;
        assert!(host.address_known("bob@shop.test").unwrap());
        assert!(host.address_known("CAROL@other.test").unwrap(), "recipient of a cached message");
        assert!(!host.address_known("ob@shop.test").unwrap());
        assert!(!host.address_known("stranger@new.test").unwrap());
        assert_eq!(host.own_addresses(ACC).unwrap(), vec![ACC.to_string()]);
    }

    /// Exit criterion: no code path from the model to SMTP. Scans the glue (everything in this directory that
    /// is not a test file) for SMTP / sender identifiers and checks that the only command with "send" in its
    /// name is the chat's own `ai_send`.
    #[test]
    fn glue_has_no_path_to_smtp() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ai");
        let banned = ["smtp", "lettre", "send_message", "outgoingmessage", "send_and_save", "build_message", "send_mail"];
        let mut files = 0;
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            // calhost.rs is the one deliberate exception: the approved-card writer of the calendar (checked below).
            if p.extension().and_then(|x| x.to_str()) != Some("rs") || matches!(p.file_name().and_then(|x| x.to_str()), Some("tests.rs" | "calhost.rs")) {
                continue;
            }
            files += 1;
            for (i, line) in std::fs::read_to_string(&p).unwrap().lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let low = line.to_lowercase();
                for b in banned {
                    assert!(!low.contains(b), "{}:{}: `{b}` in `{}`", p.display(), i + 1, line.trim());
                }
            }
        }
        assert!(files >= 4, "scanned {files} files");
        // calhost.rs may mail invitations/answers, but only inside the write half that the calendar executor calls
        // after a click (finish_save <- create/change/restore, cancel_event, respond_invite).
        let cal = std::fs::read_to_string(dir.join("calhost.rs")).unwrap();
        let prod = cal.split("#[cfg(test)]").next().unwrap();
        let mut current_fn = String::new();
        for (i, line) in prod.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue;
            }
            if let Some(pos) = t.find("fn ") {
                current_fn = t[pos + 3..].split(['(', '<']).next().unwrap_or("").to_string();
            }
            if line.contains(".send_itip(") {
                assert!(["finish_save", "cancel_event", "respond_invite"].contains(&current_fn.as_str()), "calhost.rs:{}: mail path in `{current_fn}`: {}", i + 1, line.trim());
            }
        }
        let cmds = std::fs::read_to_string(dir.join("commands.rs")).unwrap();
        let names: Vec<&str> = cmds.lines().filter_map(|l| l.strip_prefix("pub async fn ")).filter_map(|l| l.split('(').next()).filter(|n| n.contains("send")).collect();
        assert_eq!(names, vec!["ai_send"]);
    }
}

/// Diagnostic, opt-in: open a COPY of a real user's ai.db (path in `MIRAMAIL_AI_DB_COPY`; the test copies it again
/// into the repo's target/ dir so even that copy is never written) and run the exact calls Preferences > AI and the AI pane make
/// on load. Prints the JSON the webview receives. Never touches the app data dir or the Keychain.
/// Run: MIRAMAIL_AI_DB_COPY=/path/to/copy.db cargo test -p miramail real_ai_db_copy -- --ignored --nocapture
#[test]
#[ignore]
fn real_ai_db_copy_load_path() {
    let Ok(src) = std::env::var("MIRAMAIL_AI_DB_COPY") else { return };
    let tmp = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/miramail-ai-copy-{}.db", std::process::id()));
    std::fs::copy(&src, &tmp).unwrap();
    let st = AiState::open(&tmp, Arc::new(InMemorySecretStore::new())).expect("open + migrate");
    st.startup_purge();
    let p = st.list_profiles().expect("ai_list_profiles");
    let s = st.get_settings().expect("ai_get_settings");
    let c = st.list_conversations().expect("ai_list_conversations");
    println!("ai_list_profiles => {}", serde_json::to_string(&p).unwrap());
    println!("ai_get_settings => {}", serde_json::to_string(&s).unwrap());
    println!("ai_list_conversations => {}", serde_json::to_string(&c).unwrap());
    {
        let h = st.store_handle();
        let mut g = h.lock().unwrap();
        let ar = g.autoreply_config("someone@example.org").expect("ai_autoreply_get");
        println!("ai_autoreply_get => {}", serde_json::to_string(&ar).unwrap());
        println!("ai_automation_state => rulesPaused={:?} autoreplyPaused={:?}", g.rules_paused(), g.autoreply_paused());
    }
    let _ = std::fs::remove_file(&tmp);
}


// ------------------------------------------------------------------------------------------ calendar tools

mod cal_p {
    use super::p2::{oa_calls, oa_text, storage, ACC};
    use super::*;
    use crate::ai::calhost::tests::{host_at, ts};
    use crate::ai::host::AppMailHost;

    const CONSENT_JSON: &str = r#"{"scopes":[{"account":"me@example.org","folder":"INBOX","allowed":true}],"redaction":{"stripQuotes":true,"shortenLinks":true,"maskAddresses":false},"tools":true,"suggest":true,"calendar":{"enabled":true,"calendars":[{"id":"personal","allowed":true}]}}"#;

    async fn setup(m: &Mock, consent_json: &str) -> (AiState, i64, Arc<crate::ai::calhost::tests::FakeEnv>) {
        let (st, _) = state();
        st.save_profile(prof("p1", &format!("{}/v1", m.url())), Some(KEY.into())).await.unwrap();
        let mail = Arc::new(AppMailHost::new(storage(), None));
        st.set_mail_host(mail.clone());
        st.set_action_host(mail.clone());
        st.set_draft_host(mail);
        let (env, host) = host_at("2026-11-02T08:00:00+01:00", "Europe/Berlin");
        let host = Arc::new(host);
        st.set_calendar_hosts(host.clone(), host);
        st.set_env_source(Arc::new(move || crate::ai::calhost::environment_info(ts("2026-11-02T08:00:00+01:00"), &"Europe/Berlin".parse().unwrap(), &calendar::avail::WorkHours::default())));
        let consent: Consent = serde_json::from_str(consent_json).unwrap();
        let c = st.new_conversation("p1", consent).unwrap();
        (st, c.id, env)
    }

    fn tool_names(m: &Mock) -> Vec<String> {
        let b: Value = serde_json::from_slice(&m.requests()[0].body).unwrap();
        b["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect()
    }

    #[tokio::test]
    async fn camel_case_consent_gates_the_calendar_tools_and_old_consents_have_none() {
        let old: Consent = serde_json::from_str(r#"{"scopes":[],"redaction":{"stripQuotes":true,"shortenLinks":true,"maskAddresses":false},"tools":true,"suggest":true}"#).unwrap();
        assert!(!old.calendar.enabled && old.calendar.calendars.is_empty());
        for (json, calendar, n) in [(CONSENT_JSON, true, 25usize), (&CONSENT_JSON.replace("\"enabled\":true", "\"enabled\":false"), false, 14 + 2)] {
            let m = Mock::start(vec![oa_text("hi")]).await;
            let (st, cid, _env) = setup(&m, json).await;
            assert_eq!(st.get_conversation(cid).unwrap().conversation.calendar_enabled, calendar);
            st.send(cid, "hello".into(), &VecSink::default()).await.unwrap();
            let names = tool_names(&m);
            assert_eq!(names.contains(&"list_events".to_string()), calendar, "{names:?}");
            assert_eq!(names.contains(&"propose_event".to_string()), calendar);
            assert_eq!(names.len(), if calendar { n } else { 16 }, "{names:?}");
            // the environment note travels with the user message, never in the system prompt
            let body: Value = serde_json::from_slice(&m.requests()[0].body).unwrap();
            let sys = body["messages"][0]["content"].as_str().unwrap();
            let user = body["messages"][1]["content"].as_str().unwrap();
            assert!(!sys.contains("Monday") && !sys.contains("2026-11-02"));
            assert!(user.contains("Now: Monday 2026-11-02 08:00") && user.contains("Europe/Berlin (UTC+01:00)") && user.ends_with("hello"), "{user}");
            // the UI sees only what was typed
            assert_eq!(st.get_conversation(cid).unwrap().messages[0].text, "hello");
        }
        // without the mail tools there is no calendar either
        let m = Mock::start(vec![oa_text("hi")]).await;
        let (st, cid, _e) = setup(&m, &CONSENT_JSON.replace("\"tools\":true", "\"tools\":false")).await;
        st.send(cid, "hello".into(), &VecSink::default()).await.unwrap();
        let b: Value = serde_json::from_slice(&m.requests()[0].body).unwrap();
        assert!(b.get("tools").is_none_or(|t| t.as_array().unwrap().is_empty()));
    }

    #[tokio::test]
    async fn suggest_approve_undo_in_the_real_path_with_numeric_ids_and_camel_case_cards() {
        let m = Mock::start(vec![
            oa_calls(&[("c1", "find_free_slots", json!({"duration_minutes":60,"from":"2026-11-03","to":"2026-11-03"})), ("c2", "list_events", json!({"from":"2026-11-03","to":"2026-11-03"}))]),
            oa_calls(&[("c3", "propose_event", json!({"title":"Lunch with Ravi","start":"2026-11-03T13:00","end":"2026-11-03T14:00","attendees":["ravi@known.test","new@stranger.test"],"reason":"asked"}))]),
            oa_text("A suggestion is waiting."),
        ])
        .await;
        let (st, cid, env) = setup(&m, CONSENT_JSON).await;
        let sink = VecSink::default();
        st.send(cid, "schedule lunch with Ravi tomorrow".into(), &sink).await.unwrap();
        // proposing changed nothing and sent nothing
        assert!(env.sent.lock().unwrap().is_empty());
        assert!(env.cal.db(|c| c.list_occurrences("2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z", false)).unwrap().is_empty());
        let p = st.list_proposals(Some(cid), 10).unwrap().remove(0);
        assert_eq!((p.kind.as_str(), p.state.as_str()), ("event_create", "proposed"));
        assert_eq!(p.title, "Create 'Lunch with Ravi' Tue 3 Nov 13:00\u{2013}14:00 on Personal");
        assert!(!p.undoable);
        let wire = serde_json::to_value(&p).unwrap();
        assert!(wire["id"].is_i64() && wire["event"]["notify"] == true && wire["event"]["calendarName"] == "Personal", "{wire}");
        assert_eq!(wire["event"]["rows"][0]["label"], "Title");
        assert!(wire["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("never appears in your mail")));
        assert!(wire["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("Approving will send invitations by email to 2 people")));
        // approve (selected list ignored for calendar cards)
        let (res, affected) = st.approve_proposal(p.id, vec![], &|_| {}).await.unwrap();
        assert!(affected.is_empty() && res.ok == 1 && res.proposal.summary.state == "done", "{:?}", res.proposal.summary);
        assert!(res.note.contains("invitations sent"), "{}", res.note);
        assert_eq!(env.sent.lock().unwrap().len(), 1);
        assert_eq!(env.cal.db(|c| c.list_occurrences("2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z", false)).unwrap().len(), 1);
        assert!(res.proposal.summary.undoable);
        assert!(st.approve_proposal(p.id, vec![], &|_| {}).await.is_err(), "never twice");
        let u = st.undo_proposal(p.id, &|_| {}).await.unwrap().0;
        assert_eq!((u.undone, u.failed, u.proposal.summary.state.as_str()), (1, 0, "undone"));
        assert!(env.cal.db(|c| c.list_occurrences("2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z", false)).unwrap().is_empty());
        assert_eq!(env.sent.lock().unwrap().len(), 2, "undo sends the cancellation the card announced");
        let conv = st.get_conversation(cid).unwrap();
        let chips = &conv.messages.last().unwrap().tools;
        assert_eq!(chips.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["find_free_slots", "list_events", "propose_event"]);
        assert_eq!(chips[1].summary, "0 events");
    }

    #[tokio::test]
    async fn approval_is_refused_when_the_calendar_or_suggestions_were_turned_off() {
        let m = Mock::start(vec![oa_calls(&[("c3", "propose_event", json!({"title":"Solo","start":"2026-11-03T13:00"}))]), oa_text("ok")]).await;
        let (st, cid, env) = setup(&m, CONSENT_JSON).await;
        st.send(cid, "add solo".into(), &VecSink::default()).await.unwrap();
        let p = st.list_proposals(Some(cid), 10).unwrap().remove(0);
        // a reject never touches the calendar
        let off = st.new_conversation("p1", serde_json::from_str(&CONSENT_JSON.replace("\"suggest\":true", "\"suggest\":false")).unwrap()).unwrap().id;
        let np = proposals::NewProposal { conv_id: off, kind: proposals::Kind::EventCreate, params: p_params(&st, p.id), title: "t".into(), reason: String::new(), warnings: vec![], items: vec![] };
        let pid2 = st.store.lock().unwrap().create_proposal(&np, system_clock()()).unwrap();
        let e = st.approve_proposal(pid2, vec![], &|_| {}).await.unwrap_err();
        assert!(e.contains("turned off"), "{e}");
        assert!(env.cal.db(|c| c.list_occurrences("2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z", false)).unwrap().is_empty());
        let r = st.reject_proposal(p.id).unwrap();
        assert_eq!(r.summary.state, "rejected");
        assert!(st.approve_proposal(p.id, vec![], &|_| {}).await.is_err());
        let _ = ACC;
    }

    #[tokio::test]
    async fn calendar_approve_claims_before_reading_consent_so_a_racing_edit_is_refused() {
        let m = Mock::start(vec![oa_calls(&[("c3", "propose_event", json!({"title":"Solo","start":"2026-11-03T13:00"}))]), oa_text("ok")]).await;
        let (st, cid, _env) = setup(&m, CONSENT_JSON).await;
        let st = Arc::new(st);
        st.send(cid, "add solo".into(), &VecSink::default()).await.unwrap();
        let pid = st.list_proposals(Some(cid), 10).unwrap().remove(0).id;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let (s2, seen2) = (st.clone(), seen.clone());
        *st.after_claim_hook.lock().unwrap() = Some(Arc::new(move || {
            let (s3, seen3) = (s2.clone(), seen2.clone());
            Box::pin(async move {
                let held = s3.busy.lock().unwrap().contains(&pid);
                let mut c: Consent = serde_json::from_str(CONSENT_JSON).unwrap();
                c.calendar.enabled = false;
                let r = s3.update_consent(cid, c).await;
                seen3.lock().unwrap().push(format!("held={held} refused={}", r.as_ref().is_err_and(|e| e.contains("approved suggestion"))));
            })
        }));
        let _ = st.approve_proposal(pid, vec![], &|_| {}).await;
        assert_eq!(*seen.lock().unwrap(), vec!["held=true refused=true".to_string()], "the claim is held while the consent was read");
        assert!(st.get_conversation(cid).unwrap().consent.calendar.enabled, "the racing save did not land");
        assert!(!st.busy.lock().unwrap().contains(&pid), "claim released afterwards");
    }

    fn p_params(st: &AiState, id: i64) -> proposals::Params {
        st.store.lock().unwrap().get_proposal(id).unwrap().unwrap().params
    }
}

/// "Edit permissions": `ai_update_consent`. In-memory store, fake endpoint, no real mailbox.
mod edit_perms {
    use super::p2::{consent, oa_calls, oa_text, snapshot, storage, ACC};
    use super::*;
    use crate::ai::host::AppMailHost;
    use mailcore::Storage;

    async fn setup(m: &Mock, storage: Arc<Storage>) -> (Arc<AiState>, i64) {
        let (st, _) = state();
        st.save_profile(prof("p1", &format!("{}/v1", m.url())), Some(KEY.into())).await.unwrap();
        let host = Arc::new(AppMailHost::new(storage, None));
        st.set_mail_host(host.clone());
        st.set_action_host(host);
        let c = st.new_conversation("p1", consent(true)).unwrap();
        (Arc::new(st), c.id)
    }

    fn with_folders(folders: &[&str]) -> Consent {
        let mut c = consent(true);
        c.scopes = folders.iter().map(|f| Scope { account: ACC.into(), folder: (*f).into(), allowed: true }).collect();
        c
    }

    fn stored(st: &AiState, cid: i64) -> Consent {
        serde_json::from_str(&st.store.lock().unwrap().get_conversation(cid).unwrap().unwrap().consent_json).unwrap()
    }

    fn list_folders_turn() -> Script {
        oa_calls(&[("a", "list_accounts", json!({})), ("b", "list_folders", json!({"account":"a:1"}))])
    }

    #[tokio::test]
    async fn invalid_consent_is_refused_and_changes_nothing() {
        let m = Mock::start(vec![oa_text("unused")]).await;
        let (st, cid) = setup(&m, storage()).await;
        let before = serde_json::to_string(&st.get_conversation(cid).unwrap().messages).unwrap();
        let bad: Vec<(Consent, &str)> = vec![
            (with_folders(&["Nope"]), "folder"),
            ({ let mut c = with_folders(&["INBOX"]); c.scopes[0].account = "ghost@x.org".into(); c }, "account"),
            ({ let mut c = with_folders(&["INBOX"]); c.scopes[0].folder = "x".repeat(600); c }, "too many or too long"),
            ({ let mut c = with_folders(&["INBOX"]); c.calendar.calendars = (0..201).map(|i| CalScope { id: format!("c{i}"), allowed: false }).collect(); c }, "too many or too long"),
            ({ let mut c = with_folders(&["INBOX"]); c.calendar = CalendarConsent { enabled: true, calendars: vec![CalScope { id: "c1".into(), allowed: true }] }; c }, "alendar"),
        ];
        for (c, needle) in bad {
            let e = st.update_consent(cid, c).await.unwrap_err();
            assert!(e.contains(needle), "{e}");
        }
        assert!(st.update_consent(9999, with_folders(&["INBOX"])).await.is_err());
        assert_eq!(stored(&st, cid), consent(true), "consent unchanged");
        assert_eq!(serde_json::to_string(&st.get_conversation(cid).unwrap().messages).unwrap(), before, "no transcript line");
    }

    #[tokio::test]
    async fn widening_and_narrowing_apply_on_the_next_turn_and_write_one_visible_line() {
        let m = Mock::start(vec![list_folders_turn(), oa_text("ok"), list_folders_turn(), oa_text("ok"), list_folders_turn(), oa_text("ok"), oa_text("plain")]).await;
        let (st, cid) = setup(&m, storage()).await;
        let seen = |i: usize| String::from_utf8_lossy(&m.requests()[i].body).to_string();
        st.send(cid, "folders".into(), &VecSink::default()).await.unwrap();
        assert!(seen(1).contains("INBOX [") && !seen(1).contains("Archive ["), "not yet in scope");

        // widen
        let c = st.update_consent(cid, with_folders(&["INBOX", "Archive"])).await.unwrap();
        let last = c.messages.last().unwrap();
        assert_eq!((last.role.as_str(), last.text.as_str()), ("notice", "Permissions changed by you: +1 folder"));
        assert_eq!(c.consent, with_folders(&["INBOX", "Archive"]));
        st.send(cid, "folders again".into(), &VecSink::default()).await.unwrap();
        assert!(seen(3).contains("Archive ["), "widening took effect on the very next turn");

        // narrow
        let c = st.update_consent(cid, with_folders(&["INBOX"])).await.unwrap();
        assert_eq!(c.messages.last().unwrap().text, "Permissions changed by you: -1 folder");
        st.send(cid, "folders third".into(), &VecSink::default()).await.unwrap();
        assert_eq!(seen(5).matches("Archive [").count(), 0, "the earlier Archive listing is no longer replayed after narrowing");
        assert!(seen(5).contains("earlier result removed: folder no longer allowed in this chat"), "a placeholder stands in for it");

        // earlier messages are kept in order; the notice rows never reach the endpoint
        st.send(cid, "last".into(), &VecSink::default()).await.unwrap();
        assert!(!seen(6).contains("Permissions changed"));
        let roles: Vec<String> = st.get_conversation(cid).unwrap().messages.iter().map(|x| x.role.clone()).collect();
        assert_eq!(roles, ["user", "assistant", "notice", "user", "assistant", "notice", "user", "assistant", "user", "assistant"]);
        // saving the same thing again writes nothing
        let n = st.get_conversation(cid).unwrap().messages.len();
        assert_eq!(st.update_consent(cid, with_folders(&["INBOX"])).await.unwrap().messages.len(), n);
    }

    #[tokio::test]
    async fn refused_while_a_turn_is_running() {
        let m = Mock::start(vec![Script::sse_head().hang()]).await;
        let (st, cid) = setup(&m, storage()).await;
        let (s2, sink) = (st.clone(), Arc::new(VecSink::default()));
        let k2 = sink.clone();
        let h = tokio::spawn(async move { s2.send(cid, "first".into(), &*k2).await });
        for _ in 0..300 {
            if !m.requests().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let e = st.update_consent(cid, with_folders(&["INBOX", "Archive"])).await.unwrap_err();
        assert!(e.contains("still answering"), "{e}");
        assert_eq!(stored(&st, cid), consent(true));
        st.cancel(cid);
        h.await.unwrap().unwrap();
        assert!(st.update_consent(cid, with_folders(&["INBOX", "Archive"])).await.is_ok(), "free again");
    }

    #[tokio::test]
    async fn refused_while_a_proposal_is_executing() {
        let m = Mock::start(vec![oa_text("unused")]).await;
        let (st, cid) = setup(&m, storage()).await;
        let params = proposals::Params { account: Some(ACC.into()), new_folder: Some("Tax".into()), ..Default::default() };
        let np = proposals::NewProposal { conv_id: cid, kind: proposals::Kind::CreateFolder, params, title: "t".into(), reason: String::new(), warnings: vec![], items: vec![] };
        let pid = st.store.lock().unwrap().create_proposal(&np, system_clock()()).unwrap();
        // a proposal that is merely pending does not block
        // (checked last); mark it executing
        {
            let mut s = st.store.lock().unwrap();
            let now = system_clock()();
            s.transition_proposal(pid, proposals::State::Proposed, proposals::State::Approved, now, "").unwrap();
            s.transition_proposal(pid, proposals::State::Approved, proposals::State::Executing, now, "").unwrap();
        }
        let e = st.update_consent(cid, with_folders(&["INBOX", "Archive"])).await.unwrap_err();
        assert!(e.contains("approved suggestion"), "{e}");
        assert_eq!(stored(&st, cid), consent(true));
        // in-flight by the busy set (calendar executor path) also blocks
        st.store.lock().unwrap().transition_proposal(pid, proposals::State::Executing, proposals::State::Failed, system_clock()(), "").unwrap();
        st.busy.lock().unwrap().insert(pid);
        assert!(st.update_consent(cid, with_folders(&["INBOX", "Archive"])).await.is_err());
        st.busy.lock().unwrap().remove(&pid);
        assert!(st.update_consent(cid, with_folders(&["INBOX", "Archive"])).await.is_ok());
    }

    fn pending_create_folder(st: &AiState, cid: i64) -> i64 {
        let params = proposals::Params { account: Some(ACC.into()), new_folder: Some("Tax".into()), ..Default::default() };
        let np = proposals::NewProposal { conv_id: cid, kind: proposals::Kind::CreateFolder, params, title: "t".into(), reason: String::new(), warnings: vec![], items: vec![] };
        let id = st.store.lock().unwrap().create_proposal(&np, system_clock()()).unwrap();
        id
    }

    /// Wraps the real host; `accounts()` blocks until released, like a hung IMAP server.
    struct SlowHost {
        inner: AppMailHost,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl aichat::tools::MailToolHost for SlowHost {
        fn accounts(&self) -> aichat::tools::BoxFut<'_, Result<Vec<String>, String>> {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                aichat::tools::MailToolHost::accounts(&self.inner).await
            })
        }
        fn folders(&self, account: &str) -> aichat::tools::BoxFut<'_, Result<Vec<aichat::tools::FolderMeta>, String>> {
            aichat::tools::MailToolHost::folders(&self.inner, account)
        }
        fn folder_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String> {
            aichat::tools::MailToolHost::folder_validity(&self.inner, account, folder)
        }
        fn search(&self, scopes: &[aichat::tools::Scope], query: &str, limit: u32, offset: u32) -> aichat::tools::BoxFut<'_, Result<aichat::tools::SearchOut, String>> {
            aichat::tools::MailToolHost::search(&self.inner, scopes, query, limit, offset)
        }
        fn server_search(&self, scope: &aichat::tools::Scope, v: Option<u32>, q: &aichat::tools::ServerSearchArgs, limit: u32) -> aichat::tools::BoxFut<'_, Result<Vec<aichat::tools::MsgMeta>, String>> {
            aichat::tools::MailToolHost::server_search(&self.inner, scope, v, q, limit)
        }
        fn stats(&self, scopes: &[aichat::tools::Scope]) -> aichat::tools::BoxFut<'_, Result<aichat::tools::StatsOut, String>> {
            aichat::tools::MailToolHost::stats(&self.inner, scopes)
        }
        fn get_message(&self, scope: &aichat::tools::Scope, uid: u32, v: u32) -> aichat::tools::BoxFut<'_, Result<aichat::tools::MsgText, String>> {
            aichat::tools::MailToolHost::get_message(&self.inner, scope, uid, v)
        }
        fn get_thread(&self, scopes: &[aichat::tools::Scope], scope: &aichat::tools::Scope, uid: u32, v: u32, limit: u32) -> aichat::tools::BoxFut<'_, Result<Vec<aichat::tools::ThreadItem>, String>> {
            aichat::tools::MailToolHost::get_thread(&self.inner, scopes, scope, uid, v, limit)
        }
    }

    #[tokio::test]
    async fn a_slow_mail_server_in_edit_permissions_does_not_block_approvals() {
        let m = Mock::start(vec![oa_text("unused")]).await;
        let (st, cid) = setup(&m, storage()).await;
        let (entered, release) = (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
        st.set_mail_host(Arc::new(SlowHost { inner: AppMailHost::new(storage(), None), entered: entered.clone(), release: release.clone() }));
        let pid = pending_create_folder(&st, cid);
        let (s2, e2) = (st.clone(), with_folders(&["INBOX", "Archive"]));
        let h = tokio::spawn(async move { s2.update_consent(cid, e2).await });
        entered.notified().await; // update_consent is now stuck inside host.accounts()
        // a second approve must not wait for the global gate (any outcome but a hang is fine here)
        let r = tokio::time::timeout(Duration::from_secs(5), st.approve_proposal(pid, vec![0], &|_| {})).await;
        assert!(r.is_ok(), "approve was blocked behind the slow edit-permissions check");
        release.notify_one();
        let _ = h.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn edit_permissions_fails_closed_when_the_mail_server_never_answers() {
        let m = Mock::start(vec![oa_text("unused")]).await;
        let (st, cid) = setup(&m, storage()).await;
        let (entered, release) = (Arc::new(tokio::sync::Notify::new()), Arc::new(tokio::sync::Notify::new()));
        st.set_mail_host(Arc::new(SlowHost { inner: AppMailHost::new(storage(), None), entered, release }));
        let e = st.update_consent(cid, with_folders(&["INBOX", "Archive"])).await.unwrap_err();
        assert!(e.contains("took too long"), "{e}");
        assert_eq!(stored(&st, cid), consent(true));
    }

    #[tokio::test]
    async fn approve_claims_before_reading_consent_so_a_racing_narrowing_is_refused() {
        let m = Mock::start(vec![oa_text("unused")]).await;
        let (st, cid) = setup(&m, storage()).await;
        let pid = pending_create_folder(&st, cid);
        // Deterministic interleaving: the hook runs after approve has claimed the proposal and read the
        // consent, at the exact point where an "Edit permissions" save would race it.
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let (s2, seen2) = (st.clone(), seen.clone());
        *st.after_claim_hook.lock().unwrap() = Some(Arc::new(move || {
            let (s3, seen3) = (s2.clone(), seen2.clone());
            Box::pin(async move {
                let held = s3.busy.lock().unwrap().contains(&pid);
                let r = s3.update_consent(cid, with_folders(&["INBOX"])).await;
                seen3.lock().unwrap().push(format!("held={held} refused={}", r.as_ref().is_err_and(|e| e.contains("approved suggestion"))));
            })
        }));
        let _ = st.approve_proposal(pid, vec![0], &|_| {}).await;
        assert_eq!(*seen.lock().unwrap(), vec!["held=true refused=true".to_string()], "the claim is held while the consent was read");
        assert_eq!(stored(&st, cid), consent(true), "the racing save did not land");
        assert!(!st.busy.lock().unwrap().contains(&pid), "claim released afterwards");
    }

    #[tokio::test]
    async fn a_narrowing_saved_before_the_claim_is_honoured_by_approve() {
        let m = Mock::start(vec![oa_text("unused")]).await;
        let (st, cid) = setup(&m, storage()).await;
        let pid = pending_create_folder(&st, cid);
        // narrowed to a different folder set that no longer lets MIRA create folders in the account
        let mut narrow = with_folders(&["INBOX"]);
        narrow.suggest = false;
        st.update_consent(cid, narrow).await.unwrap();
        let e = st.approve_proposal(pid, vec![0], &|_| {}).await.unwrap_err();
        assert!(e.contains("turned off"), "{e}");
        assert_eq!(st.get_proposal(pid).unwrap().summary.state, "proposed", "nothing ran");
        assert!(!st.busy.lock().unwrap().contains(&pid));
    }

    #[tokio::test]
    async fn a_pending_proposal_is_refused_at_approval_after_narrowing() {
        let storage = storage();
        let before = snapshot(&storage);
        let m = Mock::start(vec![
            oa_calls(&[("c1", "search_mail", json!({"query":"invoice"}))]),
            oa_calls(&[("c2", "propose_archive", json!({"refs":["m:1","m:2"],"reason":"paid"}))]),
            oa_text("queued"),
        ])
        .await;
        let (st, cid) = setup(&m, storage.clone()).await;
        st.send(cid, "archive".into(), &VecSink::default()).await.unwrap();
        let pid = st.list_proposals(Some(cid), 10).unwrap().remove(0).id;
        // narrow to a folder that does not contain the messages
        st.update_consent(cid, with_folders(&["Archive"])).await.unwrap();
        // the card itself is untouched by the edit
        assert_eq!(st.get_proposal(pid).unwrap().summary.state, "proposed");
        let (res, affected) = st.approve_proposal(pid, vec![0, 1], &|_| {}).await.unwrap();
        assert_eq!(res.ok, 0);
        assert!(affected.is_empty());
        let d = st.get_proposal(pid).unwrap();
        assert!(d.items.iter().all(|i| i.status == "skipped" && i.error.contains("outside the folders allowed")), "{:?}", d.items.iter().map(|i| (&i.status, &i.error)).collect::<Vec<_>>());
        assert_eq!(snapshot(&storage), before);
    }

    #[tokio::test]
    async fn created_folders_show_ticked_and_never_duplicate_or_conflict_with_the_edit() {
        let m = Mock::start(vec![oa_text("unused")]).await;
        let (st, cid) = setup(&m, storage()).await;
        st.store.lock().unwrap().add_conv_folder(cid, ACC, "Archive").unwrap();
        // prefill: the created folder appears ticked, once
        let c = st.get_conversation(cid).unwrap().consent;
        assert_eq!(c, with_folders(&["INBOX", "Archive"]));
        assert_eq!(st.consent_scopes(cid).unwrap().len(), 2);
        // keeping it: saved into the consent, conv_folders cleared, no duplicates, no change line (same effective scope)
        let n0 = st.get_conversation(cid).unwrap().messages.len();
        let c = st.update_consent(cid, with_folders(&["INBOX", "Archive"])).await.unwrap();
        assert_eq!(c.messages.len(), n0);
        assert_eq!(st.consent_scopes(cid).unwrap().len(), 2);
        // unticking it narrows for real: the stored conv_folders row must not bring it back
        let c = st.update_consent(cid, with_folders(&["INBOX"])).await.unwrap();
        assert_eq!(c.messages.last().unwrap().text, "Permissions changed by you: -1 folder");
        assert!(st.store.lock().unwrap().list_conv_folders(cid).unwrap().is_empty());
        assert_eq!(st.consent_scopes(cid).unwrap(), vec![ToolScope { account: ACC.into(), folder: "INBOX".into() }]);
        assert_eq!(stored(&st, cid), with_folders(&["INBOX"]));
    }

    #[tokio::test]
    async fn calendar_toggle_and_other_switches_are_described_and_stored() {
        let m = Mock::start(vec![oa_text("unused")]).await;
        let (st, _) = state();
        st.save_profile(prof("p1", &format!("{}/v1", m.url())), Some(KEY.into())).await.unwrap();
        st.set_mail_host(Arc::new(AppMailHost::new(storage(), None)));
        let mut c0 = consent(true);
        c0.calendar = CalendarConsent { enabled: true, calendars: vec![CalScope { id: "c1".into(), allowed: true }] };
        let cid = st.new_conversation("p1", c0.clone()).unwrap().id;
        assert!(st.get_conversation(cid).unwrap().conversation.calendar_enabled);
        let mut off = c0.clone();
        off.calendar.enabled = false;
        off.suggest = false;
        let c = st.update_consent(cid, off.clone()).await.unwrap();
        assert_eq!(c.messages.last().unwrap().text, "Permissions changed by you: calendar off, suggestions off");
        assert!(!c.conversation.calendar_enabled && !c.conversation.suggest_enabled);
        assert_eq!(stored(&st, cid), off);
        // turning the calendar on needs a calendar host and real calendars
        let mut on = off.clone();
        on.calendar.enabled = true;
        assert!(st.update_consent(cid, on).await.is_err());
        assert_eq!(stored(&st, cid), off);
    }
}
