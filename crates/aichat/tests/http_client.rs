mod support;

use std::time::{Duration, Instant};

use aichat::http::*;
use futures_util::StreamExt;
use support::{head, Mock, Script};
use tokio::sync::watch;

const KEY: &str = "sk-SECRETKEY-0123456789";

fn base(m: &Mock) -> BaseUrl {
    validate_base_url(&format!("{}/v1", m.url())).unwrap()
}
fn stop() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}
fn fast() -> Limits {
    Limits { connect_timeout: Duration::from_secs(2), idle_timeout: Duration::from_millis(400), total_timeout: Duration::from_secs(5), max_body_bytes: 1024 }
}
fn body() -> serde_json::Value {
    serde_json::json!({"model":"m","messages":[{"role":"user","content":"hi"}]})
}

async fn post(m: &Mock, auth: Auth<'_>, limits: &Limits) -> Result<Vec<u8>> {
    let b = base(m);
    let c = build_client_with(&b, limits)?;
    let (_tx, rx) = stop();
    let r = send_json_with(&c, &b, "/chat/completions", &[], &auth, &body(), rx.clone(), limits).await?;
    read_body(r, rx, limits, &b).await
}

#[tokio::test]
async fn success_bearer_and_request_recorded() {
    let m = Mock::start(vec![Script::json(200, r#"{"ok":true}"#)]).await;
    let out = post(&m, Auth::bearer(KEY), &fast()).await.unwrap();
    assert_eq!(out, br#"{"ok":true}"#);
    let r = &m.requests()[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/v1/chat/completions");
    assert_eq!(r.header("authorization"), Some(&*format!("Bearer {KEY}")));
    assert_eq!(r.header("user-agent"), Some("MiraMail"));
    assert!(r.header("content-type").unwrap().starts_with("application/json"));
    assert!(r.body_str().contains("\"hi\""));
}

#[tokio::test]
async fn auth_styles() {
    let m = Mock::start(vec![Script::json(200, "{}")]).await;
    post(&m, Auth::x_api_key(KEY), &fast()).await.unwrap();
    post(&m, Auth::none(), &fast()).await.unwrap();
    // keyless with a key supplied but style None: nothing sent
    post(&m, Auth { style: AuthStyle::None, key: Some(KEY) }, &fast()).await.unwrap();
    let rs = m.requests();
    assert_eq!(rs[0].header("x-api-key"), Some(KEY));
    assert!(rs[0].header("authorization").is_none());
    for r in &rs[1..] {
        assert!(r.header("authorization").is_none() && r.header("x-api-key").is_none());
        assert!(!r.body_str().contains(KEY));
    }
}

#[tokio::test]
async fn localhost_name_works_via_pinning() {
    let m = Mock::start(vec![Script::json(200, "{}")]).await;
    let b = validate_base_url(&format!("http://localhost:{}", m.addr.port())).unwrap();
    let c = build_client(&b).unwrap();
    let (_t, rx) = stop();
    send_json(&c, &b, "/x", &[], &Auth::none(), &body(), rx).await.unwrap();
    assert_eq!(m.requests()[0].header("host"), Some(&*format!("localhost:{}", m.addr.port())));
}

#[tokio::test]
async fn redirect_refused_and_not_followed() {
    for code in [301u16, 302, 307, 308] {
        let target = Mock::start(vec![Script::json(200, "{}")]).await;
        let m = Mock::start(vec![Script::redirect(code, &format!("{}/steal", target.url()))]).await;
        let e = post(&m, Auth::bearer(KEY), &fast()).await.unwrap_err();
        assert_eq!(e.kind, ErrorKind::Redirect, "{code}");
        assert!(e.message.contains("redirect"));
        assert!(!e.message.contains(KEY) && !e.message.contains("steal"));
        assert_eq!(target.connections(), 0, "redirect target must never be contacted");
    }
}

#[tokio::test]
async fn http_status_errors() {
    let cases: Vec<(Script, ErrorKind)> = vec![
        (Script::json(401, r#"{"error":{"message":"Incorrect API key"}}"#), ErrorKind::Auth),
        (Script::json(403, "{}"), ErrorKind::Forbidden),
        (Script::json(404, "{}"), ErrorKind::NotFound),
        (Script::json(500, "boom"), ErrorKind::Server),
        (Script::json(503, "boom"), ErrorKind::Server),
    ];
    for (s, k) in cases {
        let m = Mock::start(vec![s]).await;
        let e = post(&m, Auth::bearer(KEY), &fast()).await.unwrap_err();
        assert_eq!(e.kind, k);
    }
}

#[tokio::test]
async fn rate_limit_retry_after() {
    let m = Mock::start(vec![Script::response(429, &[("Retry-After", "42")], r#"{"error":{"message":"slow down"}}"#)]).await;
    let e = post(&m, Auth::bearer(KEY), &fast()).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::RateLimited);
    assert_eq!(e.retry_after_secs, Some(42));
    assert!(e.message.contains("42"));
}

#[tokio::test]
async fn key_echoed_by_server_is_scrubbed_everywhere() {
    let echo = format!(r#"{{"error":{{"message":"Invalid key {KEY} given"}}}}"#);
    let m = Mock::start(vec![Script::json(401, &echo)]).await;
    let e = post(&m, Auth::bearer(KEY), &fast()).await.unwrap_err();
    for s in [e.message.clone(), format!("{e:?}"), e.to_string()] {
        assert!(!s.contains(KEY), "{s}");
    }
    // Debug of Auth is redacted
    let a = Auth::bearer(KEY);
    assert!(!format!("{a:?}").contains(KEY));
    // long server text is truncated
    let m = Mock::start(vec![Script::json(500, &"Z".repeat(2000))]).await;
    let e = post(&m, Auth::none(), &fast()).await.unwrap_err();
    assert!(e.message.len() < 600);
}

#[tokio::test]
async fn invalid_key_characters_do_not_leak() {
    let m = Mock::start(vec![Script::json(200, "{}")]).await;
    let bad = "sk-bad\nkey-0123";
    let e = post(&m, Auth::bearer(bad), &fast()).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::InvalidKey);
    assert!(!e.message.contains("bad"));
    assert_eq!(m.connections(), 0);
}

#[tokio::test]
async fn connection_refused_local() {
    // Bind then drop to get a closed port.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let b = validate_base_url(&format!("http://127.0.0.1:{port}/v1")).unwrap();
    let c = build_client(&b).unwrap();
    let (_t, rx) = stop();
    let e = send_json(&c, &b, "/x", &[], &Auth::bearer(KEY), &body(), rx).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::ConnectionRefused);
    assert!(e.message.contains("local model server"));
    assert!(!e.message.contains(KEY) && !e.message.contains("127.0.0.1"));
}

#[tokio::test]
async fn tls_against_plain_server_maps_to_tls_error() {
    // https:// pointed at a non-TLS loopback server: handshake fails.
    let m = Mock::start(vec![Script::new().send(b"HTTP/1.1 200 OK\r\n\r\n".to_vec()).close()]).await;
    let b = validate_base_url(&format!("https://127.0.0.1:{}", m.addr.port())).unwrap();
    let c = build_client_with(&b, &fast()).unwrap();
    let (_t, rx) = stop();
    let e = send_json_with(&c, &b, "/x", &[], &Auth::none(), &body(), rx, &fast()).await.unwrap_err();
    assert!(matches!(e.kind, ErrorKind::Tls | ErrorKind::Other | ErrorKind::Timeout), "{e:?}");
    assert!(!e.message.contains("127.0.0.1"));
}

#[tokio::test]
async fn header_timeout() {
    let m = Mock::start(vec![Script::new().hang()]).await;
    let t = Instant::now();
    let e = post(&m, Auth::none(), &fast()).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Timeout);
    assert!(t.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn idle_gap_between_chunks_times_out() {
    let m = Mock::start(vec![Script::sse_head().send(b"data: 1\n\n".to_vec()).hang()]).await;
    let b = base(&m);
    let lim = fast();
    let c = build_client_with(&b, &lim).unwrap();
    let (_t, rx) = stop();
    let mut s = stream_sse_with(&c, &b, "/s", &[], &Auth::none(), &body(), rx, &lim).await.unwrap();
    assert_eq!(s.next().await.unwrap().unwrap(), b"data: 1\n\n");
    let t = Instant::now();
    let e = s.next().await.unwrap().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Timeout);
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(s.next().await.is_none());
    assert!(m.wait_disconnects(1, Duration::from_secs(2)).await);
}

#[tokio::test]
async fn total_timeout_caps_slow_trickle() {
    let mut sc = Script::sse_head();
    for _ in 0..100 {
        sc = sc.send(b": ping\n".to_vec()).delay_ms(50);
    }
    let m = Mock::start(vec![sc.hang()]).await;
    let b = base(&m);
    let lim = Limits { total_timeout: Duration::from_millis(500), ..fast() };
    let c = build_client_with(&b, &lim).unwrap();
    let (_t, rx) = stop();
    let mut s = stream_sse_with(&c, &b, "/s", &[], &Auth::none(), &body(), rx, &lim).await.unwrap();
    let t = Instant::now();
    let mut err = None;
    while let Some(x) = s.next().await {
        if let Err(e) = x {
            err = Some(e);
        }
    }
    assert_eq!(err.unwrap().kind, ErrorKind::Timeout);
    assert!(t.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn stream_chunks_at_arbitrary_boundaries_are_raw_bytes() {
    let payload = "data: {\"t\":\"héllo wörld\"}\n\ndata: [DONE]\n\n".as_bytes();
    let m = Mock::start(vec![Script::sse_head().chunked_by(payload, 3, 2)]).await;
    let b = base(&m);
    let c = build_client(&b).unwrap();
    let (_t, rx) = stop();
    let mut s = stream_sse(&c, &b, "/s", &[], &Auth::bearer(KEY), &body(), rx).await.unwrap();
    let mut got = Vec::new();
    while let Some(x) = s.next().await {
        got.extend(x.unwrap());
    }
    assert_eq!(got, payload);
    assert_eq!(m.requests()[0].header("accept"), Some("text/event-stream"));
}

#[tokio::test]
async fn cancel_mid_stream_is_prompt_and_drops_connection() {
    let m = Mock::start(vec![Script::sse_head().send(b"data: 1\n\n".to_vec()).hang()]).await;
    let b = base(&m);
    let c = build_client(&b).unwrap(); // default (long) limits: only cancel can end this quickly
    let (tx, rx) = stop();
    let mut s = stream_sse(&c, &b, "/s", &[], &Auth::none(), &body(), rx).await.unwrap();
    assert!(s.next().await.unwrap().is_ok());
    let t = Instant::now();
    let h = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        tx.send(true).unwrap();
        tx
    });
    let e = s.next().await.unwrap().unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
    assert!(t.elapsed() < Duration::from_secs(2));
    drop(s);
    assert!(m.wait_disconnects(1, Duration::from_secs(2)).await, "connection must be closed");
    let _tx = h.await.unwrap();
}

#[tokio::test]
async fn cancel_before_headers() {
    let m = Mock::start(vec![Script::new().hang()]).await;
    let b = base(&m);
    let c = build_client(&b).unwrap();
    let (tx, rx) = stop();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = tx.send(true);
        std::future::pending::<()>().await;
    });
    let t = Instant::now();
    let e = send_json(&c, &b, "/x", &[], &Auth::none(), &body(), rx).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(m.wait_disconnects(1, Duration::from_secs(2)).await);
}

#[tokio::test]
async fn already_cancelled_sends_nothing() {
    let m = Mock::start(vec![Script::json(200, "{}")]).await;
    let b = base(&m);
    let c = build_client(&b).unwrap();
    let (tx, rx) = stop();
    tx.send(true).unwrap();
    let e = send_json(&c, &b, "/x", &[], &Auth::none(), &body(), rx).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
    assert_eq!(m.connections(), 0);
}

#[tokio::test]
async fn mid_stream_disconnect_surfaces_error_then_ends() {
    // Declared length longer than what is sent, then close.
    let sc = Script::new().send(head(200, "OK", &[], Some(1000))).send(b"partial".to_vec()).close();
    let m = Mock::start(vec![sc]).await;
    let b = base(&m);
    let c = build_client(&b).unwrap();
    let (_t, rx) = stop();
    let mut s = stream_sse(&c, &b, "/s", &[], &Auth::none(), &body(), rx).await.unwrap();
    let mut errs = 0;
    while let Some(x) = s.next().await {
        if let Err(e) = x {
            errs += 1;
            assert!(!e.message.contains("127.0.0.1"));
        }
    }
    assert_eq!(errs, 1);
}

#[tokio::test]
async fn response_cap_content_length() {
    let big = "x".repeat(2000);
    let m = Mock::start(vec![Script::json(200, &big)]).await;
    let e = post(&m, Auth::none(), &fast()).await.unwrap_err(); // cap 1024
    assert_eq!(e.kind, ErrorKind::TooLarge);
}

#[tokio::test]
async fn response_cap_without_content_length() {
    let sc = Script::new().send(head(200, "OK", &[], None)).chunked_by(&vec![b'y'; 5000], 500, 0).close();
    let m = Mock::start(vec![sc]).await;
    let e = post(&m, Auth::none(), &fast()).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::TooLarge);
}

#[tokio::test]
async fn default_limits_match_brief() {
    let l = Limits::default();
    assert_eq!(l.connect_timeout, Duration::from_secs(10));
    assert_eq!(l.idle_timeout, Duration::from_secs(60));
    assert_eq!(l.total_timeout, Duration::from_secs(300));
    assert_eq!(l.max_body_bytes, 4 * 1024 * 1024);
}

#[tokio::test]
async fn error_body_read_is_bounded() {
    // Huge error body must not hang or be echoed in full.
    let sc = Script::new().send(head(500, "X", &[], None)).chunked_by(&vec![b'q'; 200_000], 10_000, 0).close();
    let m = Mock::start(vec![sc]).await;
    let e = post(&m, Auth::none(), &fast()).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Server);
    assert!(e.message.len() < 700);
}

#[test]
fn source_has_no_invalid_cert_escape_hatch() {
    let src = include_str!("../src/http.rs");
    let lines: Vec<&str> = src.lines().filter(|l| !l.trim_start().starts_with("//")).collect();
    let code = lines.join("\n");
    assert!(!code.contains("danger_accept_invalid"));
    assert!(!code.contains("add_root_certificate"));
}
