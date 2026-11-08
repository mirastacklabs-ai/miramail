//! HTTP client, URL validation, caps (task A2).
//!
//! Policy (owner decision 4 in docs/AI_CHAT_PLAN.md): the endpoint is user-configurable (http or
//! https, any host). We never follow redirects, pin `localhost` to 127.0.0.1, validate TLS
//! certificates normally, and only apply credentials at request time. Keys are passed as `&str`
//! per call, never stored, and are scrubbed from every error we produce.
//!
//! Streaming helper [`stream_sse`] yields raw body chunks (`Vec<u8>`); the SSE parser lives in
//! `sse.rs` and is deliberately not a dependency here.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use futures_util::stream::{self, Stream};
use reqwest::redirect::Policy;
pub use reqwest::Client;
use reqwest::{Response, Url};
use tokio::sync::watch;
use tokio::time::Instant;

pub type Result<T> = std::result::Result<T, UserFacingError>;

/// Max server text echoed to the user.
pub const MAX_SERVER_TEXT: usize = 300;

// ------------------------------------------------------------------------------------------
// Errors
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidUrl,
    BadRequest,
    Auth,
    Forbidden,
    NotFound,
    RateLimited,
    Server,
    Redirect,
    Tls,
    Timeout,
    Offline,
    ConnectionRefused,
    TooLarge,
    Cancelled,
    InvalidKey,
    Other,
}

/// An error whose `message` is safe to show in the UI: never contains the API key or the request
/// body, and server-provided text is truncated to [`MAX_SERVER_TEXT`] chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFacingError {
    pub kind: ErrorKind,
    pub message: String,
    pub status: Option<u16>,
    pub retry_after_secs: Option<u64>,
}

impl UserFacingError {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), status: None, retry_after_secs: None }
    }
}

impl fmt::Display for UserFacingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for UserFacingError {}

// ------------------------------------------------------------------------------------------
// URL validation
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlWarning {
    /// http:// to a non-loopback host: "API key and mail content are sent unencrypted".
    CleartextRemote,
    /// Host is in a private / link-local / local-name range.
    LanHost,
}

impl UrlWarning {
    pub fn message(&self) -> &'static str {
        match self {
            UrlWarning::CleartextRemote => "API key and mail content are sent unencrypted",
            UrlWarning::LanHost => "This endpoint is on your local network",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseUrl {
    /// Normalised: no trailing slash, no userinfo/query/fragment. Join with a path starting `/`.
    pub url: String,
    /// Lower-case host without IPv6 brackets. This is what a saved key is bound to.
    pub host: String,
    pub port: u16,
    pub is_loopback: bool,
    pub scheme: Scheme,
    pub warnings: Vec<UrlWarning>,
}

fn bad_url(msg: &str) -> UserFacingError {
    UserFacingError::new(ErrorKind::InvalidUrl, msg)
}

fn is_lan_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private() || ip.is_link_local() || (o[0] == 100 && (o[1] & 0xc0) == 64)
}

fn classify_ip(ip: IpAddr) -> (bool, bool) {
    // (loopback, lan)
    match ip {
        IpAddr::V4(v4) => (v4.is_loopback(), is_lan_v4(v4)),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return (v4.is_loopback(), is_lan_v4(v4));
            }
            let s = v6.segments();
            (v6.is_loopback(), (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80)
        }
    }
}

fn is_localhost_name(d: &str) -> bool {
    let d = d.trim_end_matches('.');
    d == "localhost" || d.ends_with(".localhost")
}

fn is_lan_name(d: &str) -> bool {
    let d = d.trim_end_matches('.');
    !d.contains('.')
        || d.ends_with(".local")
        || d.ends_with(".lan")
        || d.ends_with(".internal")
        || d.ends_with(".home.arpa")
}

pub fn validate_base_url(input: &str) -> Result<BaseUrl> {
    let s = input.trim();
    if s.is_empty() {
        return Err(bad_url("Enter a base URL, for example https://api.openai.com/v1"));
    }
    let url = Url::parse(s).map_err(|_| {
        bad_url("That is not a valid URL. Include http:// or https://, for example https://api.openai.com/v1")
    })?;
    let scheme = match url.scheme() {
        "http" => Scheme::Http,
        "https" => Scheme::Https,
        _ => return Err(bad_url("The URL must start with http:// or https://")),
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad_url("Do not put a username or password in the URL; enter the API key in the key field"));
    }
    if url.query().is_some() {
        return Err(bad_url("The base URL must not contain a query string (?...)"));
    }
    if url.fragment().is_some() {
        return Err(bad_url("The base URL must not contain a fragment (#...)"));
    }
    let raw_host = url.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if raw_host.is_empty() {
        return Err(bad_url("The URL has no host name"));
    }
    let (host, is_loopback, is_lan) = match raw_host.parse::<IpAddr>() {
        Ok(ip) => {
            let (lo, lan) = classify_ip(ip);
            (ip.to_string(), lo, lan)
        }
        Err(_) => {
            let lo = is_localhost_name(&raw_host);
            let lan = !lo && is_lan_name(&raw_host);
            (raw_host, lo, lan)
        }
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let mut warnings = Vec::new();
    if scheme == Scheme::Http && !is_loopback {
        warnings.push(UrlWarning::CleartextRemote);
    }
    if is_lan {
        warnings.push(UrlWarning::LanHost);
    }
    let normalised = url.as_str().trim_end_matches('/').to_string();
    Ok(BaseUrl { url: normalised, host, port, is_loopback, scheme, warnings })
}

impl BaseUrl {
    /// `scheme://host:port` (IPv6 hosts bracketed). A saved API key is bound to this, so changing
    /// scheme, host or port forces the key to be re-entered (an https->http switch must not leak it).
    pub fn origin(&self) -> String {
        let scheme = match self.scheme {
            Scheme::Http => "http",
            Scheme::Https => "https",
        };
        if self.host.contains(':') {
            format!("{scheme}://[{}]:{}", self.host, self.port)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }

    pub fn has_warning(&self, w: UrlWarning) -> bool {
        self.warnings.contains(&w)
    }

    /// Join a path (must start with a single `/`; cannot change the host).
    fn join(&self, path: &str) -> Result<String> {
        if !path.starts_with('/') || path.starts_with("//") || path.contains("://") || path.contains('#') {
            return Err(UserFacingError::new(ErrorKind::InvalidUrl, "Internal error: invalid request path"));
        }
        Ok(format!("{}{}", self.url, path))
    }
}

// ------------------------------------------------------------------------------------------
// Limits and client
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Limits {
    pub connect_timeout: Duration,
    /// Max wait for response headers, and max silence between body chunks.
    pub idle_timeout: Duration,
    /// Cap on the whole request, streaming included.
    pub total_timeout: Duration,
    /// Max bytes read for non-stream bodies (and error bodies are capped far lower).
    pub max_body_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            total_timeout: Duration::from_secs(300),
            max_body_bytes: 4 * 1024 * 1024,
        }
    }
}

pub fn build_client(base: &BaseUrl) -> Result<Client> {
    build_client_with(base, &Limits::default())
}

pub fn build_client_with(base: &BaseUrl, limits: &Limits) -> Result<Client> {
    let mut b = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(limits.connect_timeout)
        .timeout(limits.total_timeout)
        .user_agent("MiraMail")
        .use_rustls_tls();
    // Certificates are validated normally: there is deliberately no accept-invalid-certs anywhere.
    if base.is_loopback {
        // Never route local traffic through a system proxy.
        b = b.no_proxy();
        if is_localhost_name(&base.host) {
            // Anti DNS-rebinding: `localhost` always means 127.0.0.1 regardless of resolver.
            b = b.resolve(&base.host, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), base.port));
        }
    }
    b.build().map_err(|_| UserFacingError::new(ErrorKind::Other, "Could not set up the HTTP client"))
}

// ------------------------------------------------------------------------------------------
// Auth (applied at request time only)
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>` (OpenAI-style).
    Bearer,
    /// `x-api-key: <key>` (Anthropic).
    XApiKey,
    /// Keyless (e.g. local Ollama).
    None,
}

/// Borrowed credentials. Debug is redacted; nothing here is stored or cloned into a String.
#[derive(Clone, Copy)]
pub struct Auth<'a> {
    pub style: AuthStyle,
    pub key: Option<&'a str>,
}

impl<'a> Auth<'a> {
    pub fn none() -> Self {
        Auth { style: AuthStyle::None, key: None }
    }
    pub fn bearer(key: &'a str) -> Self {
        Auth { style: AuthStyle::Bearer, key: Some(key) }
    }
    pub fn x_api_key(key: &'a str) -> Self {
        Auth { style: AuthStyle::XApiKey, key: Some(key) }
    }
}

impl fmt::Debug for Auth<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("style", &self.style)
            .field("key", &if self.key.is_some() { "<redacted>" } else { "<none>" })
            .finish()
    }
}

fn apply_auth(
    mut rb: reqwest::RequestBuilder,
    auth: &Auth<'_>,
) -> Result<reqwest::RequestBuilder> {
    let key = match (auth.style, auth.key) {
        (AuthStyle::None, _) | (_, None) => return Ok(rb),
        (_, Some("")) => return Ok(rb),
        (_, Some(k)) => k.trim(),
    };
    let invalid = || {
        UserFacingError::new(ErrorKind::InvalidKey, "The API key contains characters that cannot be sent in an HTTP header. Re-enter it.")
    };
    match auth.style {
        AuthStyle::Bearer => {
            let mut v = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| invalid())?;
            v.set_sensitive(true);
            rb = rb.header(reqwest::header::AUTHORIZATION, v);
        }
        AuthStyle::XApiKey => {
            let mut v = reqwest::header::HeaderValue::from_str(key).map_err(|_| invalid())?;
            v.set_sensitive(true);
            rb = rb.header("x-api-key", v);
        }
        AuthStyle::None => {}
    }
    Ok(rb)
}

// ------------------------------------------------------------------------------------------
// Error mapping
// ------------------------------------------------------------------------------------------

fn clean_server_text(raw: &str) -> String {
    let mut text = raw.trim().to_string();
    // Prefer a JSON error message if the body is JSON.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        let msg = v
            .pointer("/error/message")
            .or_else(|| v.get("message"))
            .or_else(|| v.get("error").filter(|e| e.is_string()))
            .and_then(|m| m.as_str());
        if let Some(m) = msg {
            text = m.to_string();
        }
    }
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > MAX_SERVER_TEXT {
        let mut t: String = flat.chars().take(MAX_SERVER_TEXT).collect();
        t.push('…');
        t
    } else {
        flat
    }
}

pub fn map_http_error(status: u16, body_snippet: &str) -> UserFacingError {
    map_http_error_with(status, body_snippet, None)
}

/// `retry_after` is the raw `Retry-After` header value, if any.
pub fn map_http_error_with(status: u16, body_snippet: &str, retry_after: Option<&str>) -> UserFacingError {
    let server = clean_server_text(body_snippet);
    let retry = retry_after.and_then(|s| s.trim().parse::<u64>().ok());
    let (kind, mut msg) = match status {
        300..=399 => (
            ErrorKind::Redirect,
            format!("The endpoint tried to redirect the request (HTTP {status}). MiraMail never follows redirects, so your key cannot be sent to another host. Fix the base URL (for example use https:// or the final address)."),
        ),
        400 | 422 => (ErrorKind::BadRequest, format!("The endpoint rejected the request (HTTP {status}). Check the model name and settings.")),
        401 => (ErrorKind::Auth, "The endpoint rejected the API key (401). Check or re-enter the key in Settings > AI.".to_string()),
        403 => (ErrorKind::Forbidden, "Access denied (403). The key may not have permission for this model, or the endpoint blocks this request.".to_string()),
        404 => (ErrorKind::NotFound, "Not found (404). Check the base URL and the model name.".to_string()),
        408 | 504 => (ErrorKind::Timeout, format!("The endpoint timed out (HTTP {status}). Try again.")),
        413 => (ErrorKind::TooLarge, "The request was too large for the endpoint (413). Try a smaller scope or turn on more redaction.".to_string()),
        429 => (
            ErrorKind::RateLimited,
            match retry {
                Some(s) => format!("Rate limited or out of quota (429). Try again in {s} seconds."),
                None => "Rate limited or out of quota (429). Try again in a little while.".to_string(),
            },
        ),
        500..=599 => (ErrorKind::Server, format!("The AI server had a problem (HTTP {status}). Try again later.")),
        _ => (ErrorKind::Other, format!("Unexpected response from the endpoint (HTTP {status}).")),
    };
    if !server.is_empty() && !(300..=399).contains(&status) {
        msg.push_str(&format!(" Server said: \"{server}\""));
    }
    UserFacingError { kind, message: msg, status: Some(status), retry_after_secs: retry }
}

fn error_chain_text(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = String::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        out.push_str(&err.to_string().to_ascii_lowercase());
        out.push(' ');
        cur = err.source();
    }
    out
}

fn io_kind_in_chain(e: &(dyn std::error::Error + 'static)) -> Option<std::io::ErrorKind> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        cur = err.source();
    }
    None
}

/// Map a transport-level reqwest error. Never includes the URL, headers or body.
pub fn map_transport_error(e: &reqwest::Error, base: &BaseUrl) -> UserFacingError {
    let text = error_chain_text(e);
    if e.is_redirect() {
        return map_http_error(302, "");
    }
    let tls = ["certificate", "invalidcert", "unknownissuer", "unknown issuer", "handshake", "tls", "peer sent no certificates", "badcert"]
        .iter()
        .any(|k| text.contains(k));
    if tls && !e.is_timeout() {
        let hint = if base.scheme == Scheme::Https && (base.has_warning(UrlWarning::LanHost) || base.is_loopback) {
            " If this is a self-signed certificate on your own network, MiraMail will not trust it: use a certificate from a trusted CA, or plain http:// on a network you trust."
        } else {
            ""
        };
        return UserFacingError::new(
            ErrorKind::Tls,
            format!("Could not establish a secure connection: the server's certificate was not accepted.{hint}"),
        );
    }
    if e.is_timeout() {
        return UserFacingError::new(ErrorKind::Timeout, "The endpoint did not respond in time. Check the address, or try again.");
    }
    let io = io_kind_in_chain(e);
    if io == Some(std::io::ErrorKind::ConnectionRefused) {
        let msg = if base.is_loopback {
            "Connection refused. Is the local model server (Ollama, LM Studio, ...) running, and is the port right?"
        } else {
            "Connection refused. Check that the server is running and the address and port are right."
        };
        return UserFacingError::new(ErrorKind::ConnectionRefused, msg);
    }
    let offline = matches!(
        io,
        Some(std::io::ErrorKind::NetworkUnreachable | std::io::ErrorKind::HostUnreachable | std::io::ErrorKind::NetworkDown)
    ) || text.contains("dns error")
        || text.contains("failed to lookup")
        || text.contains("name or service not known")
        || text.contains("nodename nor servname");
    if offline {
        return UserFacingError::new(ErrorKind::Offline, "Could not reach the endpoint. Check your internet connection and the host name in the base URL.");
    }
    if e.is_connect() {
        return UserFacingError::new(ErrorKind::ConnectionRefused, "Could not connect to the endpoint. Check the address and port.");
    }
    UserFacingError::new(ErrorKind::Other, "The connection to the endpoint failed or was interrupted.")
}

fn scrub(msg: &mut UserFacingError, auth: &Auth<'_>) {
    if let Some(k) = auth.key.map(str::trim).filter(|k| k.len() >= 4) {
        msg.message = msg.message.replace(k, "<redacted>");
    }
}

fn cancelled_err() -> UserFacingError {
    UserFacingError::new(ErrorKind::Cancelled, "Cancelled.")
}

// ------------------------------------------------------------------------------------------
// Requests
// ------------------------------------------------------------------------------------------

async fn cancelled(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            // Sender dropped: nobody can cancel any more.
            std::future::pending::<()>().await;
        }
    }
}

/// Non-2xx -> friendly error (reads at most 2 KiB of the body). 2xx -> the response.
async fn check_status(resp: Response, auth: &Auth<'_>, stop: &mut watch::Receiver<bool>, limits: &Limits) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut resp = resp;
    let mut snippet: Vec<u8> = Vec::new();
    // Best-effort, bounded and time-limited.
    let _ = tokio::time::timeout(limits.idle_timeout.min(Duration::from_secs(5)), async {
        while snippet.len() < 2048 {
            tokio::select! {
                _ = cancelled(stop) => break,
                c = resp.chunk() => match c {
                    Ok(Some(c)) => snippet.extend_from_slice(&c),
                    _ => break,
                },
            }
        }
    })
    .await;
    snippet.truncate(2048);
    let text = String::from_utf8_lossy(&snippet).into_owned();
    let mut err = map_http_error_with(status.as_u16(), &text, retry_after.as_deref());
    scrub(&mut err, auth);
    Err(err)
}

#[allow(clippy::too_many_arguments)]
async fn do_send(
    client: &Client,
    base: &BaseUrl,
    method: reqwest::Method,
    path: &str,
    headers: &[(&str, &str)],
    auth: &Auth<'_>,
    body: Option<&serde_json::Value>,
    accept_sse: bool,
    mut stop: watch::Receiver<bool>,
    limits: &Limits,
) -> Result<(Response, watch::Receiver<bool>)> {
    let url = base.join(path)?;
    let mut rb = client.request(method, url);
    for (k, v) in headers {
        rb = rb.header(*k, *v);
    }
    if accept_sse {
        rb = rb.header(reqwest::header::ACCEPT, "text/event-stream");
    }
    if let Some(b) = body {
        rb = rb.json(b);
    }
    rb = apply_auth(rb, auth)?;
    if *stop.borrow() {
        return Err(cancelled_err());
    }
    let sent = tokio::select! {
        _ = cancelled(&mut stop) => return Err(cancelled_err()),
        r = tokio::time::timeout(limits.idle_timeout, rb.send()) => r,
    };
    let resp = match sent {
        Err(_) => return Err(UserFacingError::new(ErrorKind::Timeout, "The endpoint did not respond in time. Check the address, or try again.")),
        Ok(Err(e)) => {
            let mut err = map_transport_error(&e, base);
            scrub(&mut err, auth);
            return Err(err);
        }
        Ok(Ok(r)) => r,
    };
    let resp = check_status(resp, auth, &mut stop, limits).await?;
    Ok((resp, stop))
}

/// POST `body` as JSON to `{base}{path}`. Non-2xx statuses (including 3xx) become errors.
pub async fn send_json(
    client: &Client,
    base: &BaseUrl,
    path: &str,
    headers: &[(&str, &str)],
    auth: &Auth<'_>,
    body: &serde_json::Value,
    stop: watch::Receiver<bool>,
) -> Result<Response> {
    send_json_with(client, base, path, headers, auth, body, stop, &Limits::default()).await
}

#[allow(clippy::too_many_arguments)]
pub async fn send_json_with(
    client: &Client,
    base: &BaseUrl,
    path: &str,
    headers: &[(&str, &str)],
    auth: &Auth<'_>,
    body: &serde_json::Value,
    stop: watch::Receiver<bool>,
    limits: &Limits,
) -> Result<Response> {
    do_send(client, base, reqwest::Method::POST, path, headers, auth, Some(body), false, stop, limits)
        .await
        .map(|(r, _)| r)
}

/// GET `{base}{path}` (e.g. `/models`).
pub async fn send_get(
    client: &Client,
    base: &BaseUrl,
    path: &str,
    headers: &[(&str, &str)],
    auth: &Auth<'_>,
    stop: watch::Receiver<bool>,
    limits: &Limits,
) -> Result<Response> {
    do_send(client, base, reqwest::Method::GET, path, headers, auth, None, false, stop, limits)
        .await
        .map(|(r, _)| r)
}

/// Read a non-stream body, enforcing `limits.max_body_bytes`, idle/total time and cancellation.
pub async fn read_body(resp: Response, mut stop: watch::Receiver<bool>, limits: &Limits, base: &BaseUrl) -> Result<Vec<u8>> {
    let too_large = || {
        UserFacingError::new(
            ErrorKind::TooLarge,
            format!("The endpoint's response is larger than {} MB, which is not a normal API response. Check the base URL.", limits.max_body_bytes / (1024 * 1024)),
        )
    };
    if resp.content_length().is_some_and(|n| n > limits.max_body_bytes as u64) {
        return Err(too_large());
    }
    let mut resp = resp;
    let mut out = Vec::new();
    let deadline = Instant::now() + limits.total_timeout;
    loop {
        let next = tokio::select! {
            _ = cancelled(&mut stop) => return Err(cancelled_err()),
            _ = tokio::time::sleep_until(deadline) => return Err(timeout_err()),
            r = tokio::time::timeout(limits.idle_timeout, resp.chunk()) => r,
        };
        match next {
            Err(_) => return Err(timeout_err()),
            Ok(Err(e)) => return Err(map_transport_error(&e, base)),
            Ok(Ok(None)) => return Ok(out),
            Ok(Ok(Some(c))) => {
                if out.len() + c.len() > limits.max_body_bytes {
                    return Err(too_large());
                }
                out.extend_from_slice(&c);
            }
        }
    }
}

fn timeout_err() -> UserFacingError {
    UserFacingError::new(ErrorKind::Timeout, "The endpoint stopped responding. Check the address, or try again.")
}

/// Stream of raw body chunks to feed an SSE parser. After the first `Err` the stream ends.
pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>>> + Send>>;

/// POST `body` and stream the response body. Honours `stop` promptly (dropping the response closes
/// the connection), the idle gap between chunks and the total cap.
pub async fn stream_sse(
    client: &Client,
    base: &BaseUrl,
    path: &str,
    headers: &[(&str, &str)],
    auth: &Auth<'_>,
    body: &serde_json::Value,
    stop: watch::Receiver<bool>,
) -> Result<ChunkStream> {
    stream_sse_with(client, base, path, headers, auth, body, stop, &Limits::default()).await
}

#[allow(clippy::too_many_arguments)]
pub async fn stream_sse_with(
    client: &Client,
    base: &BaseUrl,
    path: &str,
    headers: &[(&str, &str)],
    auth: &Auth<'_>,
    body: &serde_json::Value,
    stop: watch::Receiver<bool>,
    limits: &Limits,
) -> Result<ChunkStream> {
    let (resp, stop) = do_send(client, base, reqwest::Method::POST, path, headers, auth, Some(body), true, stop, limits).await?;
    struct St {
        resp: Option<Response>,
        stop: watch::Receiver<bool>,
        deadline: Instant,
        idle: Duration,
        base: BaseUrl,
    }
    let st = St { resp: Some(resp), stop, deadline: Instant::now() + limits.total_timeout, idle: limits.idle_timeout, base: base.clone() };
    let s = stream::unfold(st, |mut st| async move {
        let mut resp = st.resp.take()?; // None => stream already finished
        let next = tokio::select! {
            _ = cancelled(&mut st.stop) => Err(cancelled_err()),
            _ = tokio::time::sleep_until(st.deadline) => Err(timeout_err()),
            r = tokio::time::timeout(st.idle, resp.chunk()) => match r {
                Err(_) => Err(timeout_err()),
                Ok(Err(e)) => Err(map_transport_error(&e, &st.base)),
                Ok(Ok(c)) => Ok(c),
            },
        };
        match next {
            Ok(Some(c)) => {
                st.resp = Some(resp);
                Some((Ok(c.to_vec()), st))
            }
            Ok(None) => None,
            Err(e) => Some((Err(e), st)), // resp dropped here -> connection closed
        }
    });
    Ok(Box::pin(s))
}
