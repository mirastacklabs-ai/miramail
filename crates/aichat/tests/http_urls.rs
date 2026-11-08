use aichat::http::{map_http_error, map_http_error_with, validate_base_url, ErrorKind, Scheme, UrlWarning, MAX_SERVER_TEXT};

fn ok(s: &str) -> aichat::http::BaseUrl {
    validate_base_url(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}
fn bad(s: &str) {
    let e = validate_base_url(s).expect_err(s);
    assert_eq!(e.kind, ErrorKind::InvalidUrl, "{s}");
}

#[test]
fn rejects_invalid() {
    for s in [
        "",
        "   ",
        "api.openai.com/v1",
        "ftp://example.com",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user@example.com/v1",
        "https://user:pw@example.com/v1",
        "https://example.com/v1?key=abc",
        "https://example.com/v1#frag",
        "https://",
        "http://:8080",
    ] {
        bad(s);
    }
}

#[test]
fn https_public_has_no_warnings_and_normalises() {
    let b = ok("  https://API.OpenAI.com/v1/  ");
    assert_eq!(b.url, "https://api.openai.com/v1");
    assert_eq!(b.host, "api.openai.com");
    assert_eq!(b.port, 443);
    assert_eq!(b.scheme, Scheme::Https);
    assert!(b.warnings.is_empty());
    assert!(!b.is_loopback);
    assert_eq!(ok("https://example.com").url, "https://example.com");
    assert_eq!(ok("https://example.com/").url, "https://example.com");
    assert_eq!(b.origin(), "https://api.openai.com:443");
}

#[test]
fn loopback_http_has_no_warning() {
    for s in ["http://localhost:11434/v1", "http://127.0.0.1:1234", "http://[::1]:8080/v1", "http://foo.localhost/"] {
        let b = ok(s);
        assert!(b.is_loopback, "{s}");
        assert!(b.warnings.is_empty(), "{s}: {:?}", b.warnings);
    }
    let b = ok("http://[::1]:8080/v1/");
    assert_eq!(b.host, "::1");
    assert_eq!(b.url, "http://[::1]:8080/v1");
    assert_eq!(b.origin(), "http://[::1]:8080");
}

#[test]
fn cleartext_remote_warns() {
    let b = ok("http://example.com/v1");
    assert!(b.has_warning(UrlWarning::CleartextRemote));
    assert!(!b.has_warning(UrlWarning::LanHost));
    assert_eq!(b.port, 80);
    assert!(!UrlWarning::CleartextRemote.message().is_empty());
}

#[test]
fn lan_hosts_warn() {
    for s in ["http://192.168.1.20:8000", "http://10.0.0.5/v1", "http://172.16.3.4", "http://169.254.1.1", "http://[fd00::1]/", "http://nas.local/v1", "http://mybox/v1", "https://gpu.lan/v1"] {
        let b = ok(s);
        assert!(b.has_warning(UrlWarning::LanHost), "{s}");
        assert_eq!(b.has_warning(UrlWarning::CleartextRemote), s.starts_with("http:"), "{s}");
    }
    // https LAN: LanHost only
    assert_eq!(ok("https://192.168.1.2").warnings, vec![UrlWarning::LanHost]);
    // public IP is not LAN
    assert!(!ok("https://8.8.8.8").has_warning(UrlWarning::LanHost));
}

#[test]
fn ipv4_mapped_loopback_is_loopback() {
    assert!(ok("http://[::ffff:127.0.0.1]:80").is_loopback);
}

#[test]
fn origin_changes_with_scheme_host_port() {
    let a = ok("https://example.com/v1").origin();
    assert_ne!(a, ok("http://example.com/v1").origin());
    assert_ne!(a, ok("https://example.com:8443/v1").origin());
    assert_ne!(a, ok("https://other.com/v1").origin());
    assert_eq!(a, ok("https://EXAMPLE.com/other/path").origin());
}

#[test]
fn status_mapping() {
    use ErrorKind::*;
    let cases = [(401, Auth), (403, Forbidden), (404, NotFound), (429, RateLimited), (500, Server), (502, Server), (503, Server), (302, Redirect), (301, Redirect), (400, BadRequest), (413, TooLarge), (504, Timeout)];
    for (st, k) in cases {
        let e = map_http_error(st, "");
        assert_eq!(e.kind, k, "{st}");
        assert_eq!(e.status, Some(st));
        assert!(!e.message.is_empty());
    }
    assert!(map_http_error(401, "").message.contains("key"));
    assert!(map_http_error(404, "").message.to_lowercase().contains("model"));
}

#[test]
fn retry_after_parsed() {
    let e = map_http_error_with(429, "", Some(" 17 "));
    assert_eq!(e.retry_after_secs, Some(17));
    assert!(e.message.contains("17"));
    assert_eq!(map_http_error_with(429, "", Some("Wed, 21 Oct 2015")).retry_after_secs, None);
}

#[test]
fn server_text_extracted_and_truncated() {
    let e = map_http_error(400, r#"{"error":{"message":"bad model xyz"}}"#);
    assert!(e.message.contains("bad model xyz"));
    let long = "Z".repeat(5000);
    let e = map_http_error(500, &long);
    assert!(e.message.matches('Z').count() <= MAX_SERVER_TEXT);
    assert!(e.message.matches('Z').count() >= MAX_SERVER_TEXT - 1);
    // control characters are flattened
    let e = map_http_error(500, "line1\n\x1b[31mline2");
    assert!(!e.message.contains('\n') && !e.message.contains('\x1b'));
}

#[test]
fn redirect_message_does_not_echo_body() {
    let e = map_http_error(302, "Location: http://evil.example/steal");
    assert!(!e.message.contains("evil"));
}
