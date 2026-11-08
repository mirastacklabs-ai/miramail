//! Zoom OAuth 2.0 (authorization code + PKCE S256, loopback redirect) and the Meetings API.
//!
//! Nothing here touches the network unless tokens exist or the user is connecting. Tokens live only in the
//! [`TokenStore`] (the app backs it with the Keychain) under `zoom.access`, `zoom.refresh`, `zoom.expires`.
//! Tokens, secrets, the PKCE verifier and authorization codes are never logged and never put in errors.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use calendar::ZoomMeeting;
use reqwest::{Client, Method, Url};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const KEY_ACCESS: &str = "zoom.access";
pub const KEY_REFRESH: &str = "zoom.refresh";
pub const KEY_EXPIRES: &str = "zoom.expires";
pub const DEFAULT_REDIRECT_PORT: u16 = 53682;
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_BODY: usize = 1024 * 1024;
const SCOPE_HINT: &str = "meeting:write:meeting user:read:user";

pub type Result<T> = std::result::Result<T, ZoomError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ZoomError {
    #[error("Zoom isn't connected. Connect Zoom in Settings first.")]
    NotConnected,
    #[error("Enter your Zoom Client ID first.")]
    NotConfigured,
    #[error("Zoom sign-in expired — connect Zoom again.")]
    Reauthorize,
    #[error("Zoom sign-in was cancelled.")]
    Denied,
    #[error("The Zoom sign-in response did not match this request and was ignored.")]
    StateMismatch,
    #[error("Zoom sign-in timed out. Try connecting again.")]
    CallbackTimeout,
    #[error("Port {0} is already in use, so MiraMail cannot listen for the Zoom sign-in. Close the other program and try again.")]
    PortInUse(u16),
    #[error("Refusing to talk to Zoom over an insecure connection.")]
    Insecure,
    #[error("Can't reach Zoom. Check your internet connection.")]
    Offline,
    #[error("Zoom took too long to answer.")]
    Timeout,
    #[error("Zoom's security certificate is not valid, so MiraMail will not connect.")]
    Tls,
    #[error("Zoom is limiting requests. Try again in a moment.")]
    RateLimited,
    #[error("That Zoom meeting no longer exists.")]
    MeetingNotFound,
    #[error("{0}")]
    Api(String),
    #[error("Invalid input: {0}")]
    Invalid(String),
    #[error("Token storage error: {0}")]
    Store(String),
}

/// Secure token storage supplied by the app (Keychain). Values are secrets.
pub trait TokenStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, val: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

/// For tests and previews only.
#[derive(Default)]
pub struct InMemoryTokenStore(Mutex<HashMap<String, String>>);

impl InMemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TokenStore for InMemoryTokenStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned())
    }
    fn set(&self, key: &str, val: &str) -> Result<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(key.into(), val.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        Ok(())
    }
}

#[derive(Clone)]
pub struct ZoomConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    /// Loopback port for the redirect URI (default 53682). `0` picks a free port (tests).
    pub redirect_port: u16,
    pub auth_base: String,
    pub api_base: String,
    /// Tests only: allow `http://127.0.0.1` / `::1` bases.
    pub allow_insecure_loopback: bool,
}

impl Default for ZoomConfig {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: None,
            redirect_port: DEFAULT_REDIRECT_PORT,
            auth_base: "https://zoom.us".into(),
            api_base: "https://api.zoom.us/v2".into(),
            allow_insecure_loopback: false,
        }
    }
}

impl std::fmt::Debug for ZoomConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoomConfig").field("client_id", &self.client_id).field("client_secret", &self.client_secret.as_ref().map(|_| "<redacted>")).finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoomStatus {
    pub configured: bool,
    pub connected: bool,
    pub client_id: Option<String>,
    pub account: Option<String>,
    pub redirect_uri: String,
}

/// A started authorization: open [`AuthSession::url`] in the system browser, then pass the session to
/// [`ZoomClient::wait_for_callback`]. The loopback port is already bound (no race with the browser).
pub struct AuthSession {
    pub url: String,
    pub state: String,
    pub redirect_uri: String,
    verifier: String,
    listener: std::net::TcpListener,
}

impl std::fmt::Debug for AuthSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthSession").field("redirect_uri", &self.redirect_uri).finish_non_exhaustive()
    }
}

/// RFC 7636 S256: `BASE64URL(SHA256(verifier))` without padding.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_b64(n: usize) -> Result<String> {
    let mut b = vec![0u8; n];
    getrandom::fill(&mut b).map_err(|_| ZoomError::Api("The system random number generator is unavailable.".into()))?;
    Ok(URL_SAFE_NO_PAD.encode(b))
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn clip(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

pub struct ZoomClient {
    cfg: ZoomConfig,
    store: Arc<dyn TokenStore>,
    http: Client,
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
    account: Mutex<Option<String>>,
}

fn map_reqwest(e: reqwest::Error) -> ZoomError {
    let e = e.without_url();
    if e.is_timeout() {
        return ZoomError::Timeout;
    }
    let mut chain = String::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(c) = cur {
        chain.push_str(&c.to_string().to_ascii_lowercase());
        cur = c.source();
    }
    if chain.contains("certificate") || chain.contains("invalid peer") || chain.contains("unknownissuer") {
        return ZoomError::Tls;
    }
    ZoomError::Offline
}

impl ZoomClient {
    pub fn new(cfg: ZoomConfig, store: Arc<dyn TokenStore>) -> Self {
        Self::with_refresh_lock(cfg, store, Arc::new(tokio::sync::Mutex::new(())))
    }

    /// Like [`ZoomClient::new`], but the single-flight refresh lock is shared with other clients over the
    /// same token store (the app rebuilds its client when settings change; an old client that is still
    /// mid-refresh must not race the new one, because refresh tokens rotate).
    pub fn with_refresh_lock(cfg: ZoomConfig, store: Arc<dyn TokenStore>, refresh_lock: Arc<tokio::sync::Mutex<()>>) -> Self {
        let mut b = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .user_agent("MiraMail");
        if cfg.allow_insecure_loopback {
            b = b.no_proxy();
        }
        let http = b.build().unwrap_or_default();
        Self { cfg, store, http, refresh_lock, account: Mutex::new(None) }
    }

    /// `http://127.0.0.1:<port>/callback` — what the user registers in their Zoom OAuth app.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.cfg.redirect_port)
    }

    fn url(&self, base: &str, path: &str) -> Result<Url> {
        let u = Url::parse(&format!("{}{}", base.trim_end_matches('/'), path)).map_err(|_| ZoomError::Invalid("bad Zoom URL".into()))?;
        let host = u.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']').to_string();
        let loop_ok = self.cfg.allow_insecure_loopback && (host == "127.0.0.1" || host == "::1");
        match u.scheme() {
            "https" => Ok(u),
            "http" if loop_ok => Ok(u),
            _ => Err(ZoomError::Insecure),
        }
    }

    // ---- authorization -------------------------------------------------------------------

    /// Generates state + PKCE verifier (OS RNG), binds `127.0.0.1:<redirect_port>` and builds the authorize URL.
    /// No network traffic. Scopes are those configured on the Zoom app (`meeting:write:meeting user:read:user`).
    pub fn begin_authorize(&self) -> Result<AuthSession> {
        if self.cfg.client_id.trim().is_empty() {
            return Err(ZoomError::NotConfigured);
        }
        let listener = std::net::TcpListener::bind(("127.0.0.1", self.cfg.redirect_port)).map_err(|_| ZoomError::PortInUse(self.cfg.redirect_port))?;
        listener.set_nonblocking(true).map_err(|_| ZoomError::PortInUse(self.cfg.redirect_port))?;
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(self.cfg.redirect_port);
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let state = random_b64(24)?;
        let verifier = random_b64(48)?; // 64 chars, within RFC 7636's 43..128
        let mut url = self.url(&self.cfg.auth_base, "/oauth/authorize")?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.cfg.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("state", &state)
            .append_pair("code_challenge", &pkce_challenge(&verifier))
            .append_pair("code_challenge_method", "S256");
        Ok(AuthSession { url: url.to_string(), state, redirect_uri, verifier, listener })
    }

    /// Serves one `/callback` request on 127.0.0.1 only, validates `state`, closes the listener and exchanges
    /// the code for tokens. Other paths (404) and requests with the wrong `state` (400) do not end the wait,
    /// so a stray local request cannot cancel a sign-in. Fails with `CallbackTimeout` after `timeout`.
    pub async fn wait_for_callback(&self, session: AuthSession, timeout: Duration) -> Result<()> {
        let AuthSession { state, redirect_uri, verifier, listener, .. } = session;
        let listener = tokio::net::TcpListener::from_std(listener).map_err(|_| ZoomError::Offline)?;
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let code = tokio::time::timeout(timeout, accept_callback(&listener, &state, port)).await.map_err(|_| ZoomError::CallbackTimeout)??;
        drop(listener);
        let t = self
            .token_request(&[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", &redirect_uri), ("code_verifier", &verifier)], &redirect_uri)
            .await?;
        self.save_tokens(&t)?;
        *self.account.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }

    async fn token_request(&self, form: &[(&str, &str)], redirect_uri: &str) -> Result<Tokens> {
        let url = self.url(&self.cfg.auth_base, "/oauth/token")?;
        let mut form: Vec<(&str, &str)> = form.to_vec();
        let mut rb = self.http.post(url);
        match self.cfg.client_secret.as_deref().filter(|s| !s.is_empty()) {
            Some(secret) => rb = rb.basic_auth(&self.cfg.client_id, Some(secret)),
            None => form.push(("client_id", &self.cfg.client_id)),
        }
        let (status, v) = read_json(rb.form(&form).send().await.map_err(map_reqwest)?).await?;
        if !(200..300).contains(&status) {
            return Err(map_oauth_error(status, &v, redirect_uri));
        }
        let access = v.get("access_token").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| ZoomError::Api("Zoom's sign-in reply was missing a token.".into()))?;
        Ok(Tokens {
            access: access.to_string(),
            refresh: v.get("refresh_token").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from),
            expires_at: now() + v.get("expires_in").and_then(Value::as_i64).unwrap_or(3600),
        })
    }

    fn save_tokens(&self, t: &Tokens) -> Result<()> {
        self.store.set(KEY_ACCESS, &t.access)?;
        if let Some(r) = &t.refresh {
            self.store.set(KEY_REFRESH, r)?;
        }
        self.store.set(KEY_EXPIRES, &t.expires_at.to_string())
    }

    fn valid_access(&self) -> Result<Option<String>> {
        let (Some(a), Some(e)) = (self.store.get(KEY_ACCESS)?, self.store.get(KEY_EXPIRES)?) else { return Ok(None) };
        Ok(e.parse::<i64>().ok().filter(|e| *e > now() + 60).map(|_| a))
    }

    /// A usable access token, refreshing (and rotating the refresh token) when needed. One refresh at a time.
    /// `NotConnected` without any network when no tokens are stored.
    async fn access_token(&self, rejected: Option<&str>) -> Result<String> {
        if rejected.is_none() {
            if let Some(t) = self.valid_access()? {
                return Ok(t);
            }
        }
        let _guard = self.refresh_lock.lock().await;
        if let Some(t) = self.valid_access()? {
            if Some(t.as_str()) != rejected {
                return Ok(t); // another task refreshed while we waited
            }
        }
        let Some(refresh) = self.store.get(KEY_REFRESH)? else {
            return Err(ZoomError::NotConnected);
        };
        match self.token_request(&[("grant_type", "refresh_token"), ("refresh_token", &refresh)], &self.redirect_uri()).await {
            Ok(t) => {
                self.save_tokens(&t)?;
                Ok(t.access)
            }
            Err(ZoomError::Reauthorize) => {
                self.clear_tokens()?;
                Err(ZoomError::Reauthorize)
            }
            Err(e) => Err(e),
        }
    }

    fn clear_tokens(&self) -> Result<()> {
        for k in [KEY_ACCESS, KEY_REFRESH, KEY_EXPIRES] {
            self.store.delete(k)?;
        }
        *self.account.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }

    fn has_tokens(&self) -> Result<bool> {
        Ok(self.store.get(KEY_REFRESH)?.is_some() || self.store.get(KEY_ACCESS)?.is_some())
    }

    // ---- status / disconnect -------------------------------------------------------------

    /// Local state, plus the account email (one `GET /users/me`, cached) only when tokens exist.
    pub async fn status(&self) -> Result<ZoomStatus> {
        let configured = !self.cfg.client_id.trim().is_empty();
        let mut account = None;
        if self.has_tokens()? {
            let cached = self.account.lock().unwrap_or_else(|e| e.into_inner()).clone();
            account = match cached {
                Some(a) => Some(a),
                None => match self.api(Method::GET, "/users/me", None).await {
                    Ok((200, v)) => {
                        let a = v.get("email").and_then(Value::as_str).map(clip);
                        *self.account.lock().unwrap_or_else(|e| e.into_inner()) = a.clone();
                        a
                    }
                    _ => None,
                },
            };
        }
        Ok(ZoomStatus {
            configured,
            connected: self.has_tokens()?,
            client_id: Some(self.cfg.client_id.clone()).filter(|c| !c.is_empty()),
            account,
            redirect_uri: self.redirect_uri(),
        })
    }

    /// Revoke the access token (best effort), then delete all local tokens.
    pub async fn disconnect(&self) -> Result<()> {
        if let Some(access) = self.store.get(KEY_ACCESS)? {
            if let Ok(url) = self.url(&self.cfg.auth_base, "/oauth/revoke") {
                let mut form = vec![("token", access.as_str())];
                let mut rb = self.http.post(url);
                match self.cfg.client_secret.as_deref().filter(|s| !s.is_empty()) {
                    Some(secret) => rb = rb.basic_auth(&self.cfg.client_id, Some(secret)),
                    None => form.push(("client_id", &self.cfg.client_id)),
                }
                let _ = rb.form(&form).send().await;
            }
        }
        self.clear_tokens()
    }

    // ---- Meetings API --------------------------------------------------------------------

    async fn api(&self, method: Method, path: &str, body: Option<String>) -> Result<(u16, Value)> {
        let mut token = self.access_token(None).await?;
        for attempt in 0..2 {
            let url = self.url(&self.cfg.api_base, path)?;
            let mut rb = self.http.request(method.clone(), url).bearer_auth(&token);
            if let Some(b) = &body {
                rb = rb.header("Content-Type", "application/json").body(b.clone());
            }
            let (status, v) = read_json(rb.send().await.map_err(map_reqwest)?).await?;
            if status == 401 && attempt == 0 {
                token = self.access_token(Some(&token)).await?;
                continue;
            }
            return Ok((status, v));
        }
        Err(ZoomError::Reauthorize)
    }

    fn meeting_body(topic: &str, start_rfc3339: &str, duration_minutes: u32, timezone: &str, agenda: Option<&str>, create: bool) -> Result<String> {
        if duration_minutes == 0 {
            return Err(ZoomError::Invalid("duration must be at least 1 minute".into()));
        }
        let start = chrono::DateTime::parse_from_rfc3339(start_rfc3339).map_err(|_| ZoomError::Invalid("start time is not a valid RFC 3339 time".into()))?;
        let mut b = json!({
            "topic": topic,
            "start_time": start.with_timezone(&chrono::Utc).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "duration": duration_minutes,
            "timezone": timezone,
            "agenda": agenda.unwrap_or(""),
        });
        if create {
            b["type"] = json!(2);
            b["settings"] = json!({
                "host_video": true,
                "participant_video": true,
                "join_before_host": true,
                "waiting_room": false,
                "mute_upon_entry": true,
                "approval_type": 2,
                "audio": "both",
                "auto_recording": "none",
            });
        }
        Ok(b.to_string())
    }

    /// Create a scheduled meeting (`type` 2) on the connected account.
    ///
    /// The returned `ZoomMeeting.start_url` (host token) is IN MEMORY ONLY: the caller may show/open it, but
    /// nothing in calsync stores it, and `calendar` never writes it to ICS/SQLite/iTIP/CalDAV.
    pub async fn create_meeting(&self, topic: &str, start_rfc3339: &str, duration_minutes: u32, timezone: &str, agenda: Option<&str>) -> Result<ZoomMeeting> {
        let body = Self::meeting_body(topic, start_rfc3339, duration_minutes, timezone, agenda, true)?;
        let (status, v) = self.api(Method::POST, "/users/me/meetings", Some(body)).await?;
        if !(200..300).contains(&status) {
            return Err(map_api_error(status, &v));
        }
        let id = match v.get("id") {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => return Err(ZoomError::Api("Zoom's reply was missing the meeting id.".into())),
        };
        let join_url = v.get("join_url").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| ZoomError::Api("Zoom's reply was missing the join link.".into()))?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
        Ok(ZoomMeeting { meeting_id: id, join_url: join_url.to_string(), password: s("password"), start_url: s("start_url") })
    }

    /// Update topic/time/agenda of an existing meeting (`PATCH`).
    pub async fn update_meeting(&self, meeting_id: &str, topic: &str, start_rfc3339: &str, duration_minutes: u32, timezone: &str, agenda: Option<&str>) -> Result<()> {
        check_id(meeting_id)?;
        let body = Self::meeting_body(topic, start_rfc3339, duration_minutes, timezone, agenda, false)?;
        let (status, v) = self.api(Method::PATCH, &format!("/meetings/{meeting_id}"), Some(body)).await?;
        match status {
            200..=299 => Ok(()),
            404 => Err(ZoomError::MeetingNotFound),
            _ => Err(map_api_error(status, &v)),
        }
    }

    /// Delete a meeting. A meeting that is already gone (404) counts as success.
    pub async fn delete_meeting(&self, meeting_id: &str) -> Result<()> {
        check_id(meeting_id)?;
        let (status, v) = self.api(Method::DELETE, &format!("/meetings/{meeting_id}"), None).await?;
        match status {
            200..=299 | 404 => Ok(()),
            _ => Err(map_api_error(status, &v)),
        }
    }
}

struct Tokens {
    access: String,
    refresh: Option<String>,
    expires_at: i64,
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 20 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ZoomError::Invalid("meeting id".into()));
    }
    Ok(())
}

async fn read_json(mut resp: reqwest::Response) -> Result<(u16, Value)> {
    let status = resp.status().as_u16();
    let mut out = Vec::new();
    while let Some(c) = resp.chunk().await.map_err(map_reqwest)? {
        if out.len() + c.len() > MAX_BODY {
            return Err(ZoomError::Api("Zoom's reply was too large.".into()));
        }
        out.extend_from_slice(&c);
    }
    Ok((status, serde_json::from_slice(&out).unwrap_or(Value::Null)))
}

fn map_oauth_error(status: u16, v: &Value, redirect_uri: &str) -> ZoomError {
    let err = v.get("error").and_then(Value::as_str).unwrap_or("");
    let reason = ["reason", "error_description", "message"].iter().find_map(|k| v.get(*k).and_then(Value::as_str)).unwrap_or("");
    let low = reason.to_ascii_lowercase();
    if err == "invalid_client" || low.contains("client secret") || low.contains("invalid client") {
        return ZoomError::Api("Check your Client ID/Secret".into());
    }
    if low.contains("redirect") {
        return ZoomError::Api(format!("Zoom rejected the redirect URI. Register exactly this Redirect URL in your Zoom OAuth app: {redirect_uri}"));
    }
    if err == "invalid_grant" {
        return ZoomError::Reauthorize;
    }
    if err == "invalid_scope" || low.contains("scope") {
        return ZoomError::Api(format!("Zoom says a permission is missing. In your Zoom app, add the scopes {SCOPE_HINT}."));
    }
    if status == 429 {
        return ZoomError::RateLimited;
    }
    ZoomError::Api(format!("Zoom sign-in failed (HTTP {status}){}", if reason.is_empty() { String::new() } else { format!(": {}", clip(reason)) }))
}

fn map_api_error(status: u16, v: &Value) -> ZoomError {
    let msg = v.get("message").and_then(Value::as_str).unwrap_or("");
    let code = v.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code == 4711 || msg.to_ascii_lowercase().contains("scope") {
        return ZoomError::Api(format!("Zoom says a permission is missing. In your Zoom app, add the scopes {SCOPE_HINT}."));
    }
    match status {
        401 => ZoomError::Reauthorize,
        429 => ZoomError::RateLimited,
        404 => ZoomError::MeetingNotFound,
        _ => ZoomError::Api(format!("Zoom returned an error (HTTP {status}){}", if msg.is_empty() { String::new() } else { format!(": {}", clip(msg)) })),
    }
}

const PAGE_OK: &str = "<!doctype html><meta charset=utf-8><title>MiraMail</title><body style=\"font:16px system-ui;margin:4em auto;max-width:28em;text-align:center\"><h2>Zoom connected</h2><p>You can close this tab and return to MiraMail.</p>";
const PAGE_FAIL: &str = "<!doctype html><meta charset=utf-8><title>MiraMail</title><body style=\"font:16px system-ui;margin:4em auto;max-width:28em;text-align:center\"><h2>Zoom sign-in failed</h2><p>You can close this tab and return to MiraMail.</p>";

async fn respond(s: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\r\n{body}",
        body.len()
    );
    let _ = s.write_all(out.as_bytes()).await;
    let _ = s.shutdown().await;
}

async fn read_head(s: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    while buf.len() < 8192 {
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        match s.read(&mut tmp).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Accepts connections until one valid-looking `GET /callback` arrives; returns the authorization code.
async fn accept_callback(listener: &tokio::net::TcpListener, state: &str, port: u16) -> Result<String> {
    let mut backoff = Duration::from_millis(20);
    loop {
        let (mut s, peer) = match listener.accept().await {
            Ok(x) => {
                backoff = Duration::from_millis(20);
                x
            }
            Err(_) => {
                // e.g. EMFILE: do not spin; the overall timeout still ends the wait
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(1));
                continue;
            }
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(Some(head)) = tokio::time::timeout(Duration::from_secs(5), read_head(&mut s)).await else { continue };
        let mut lines = head.split("\r\n");
        let mut rl = lines.next().unwrap_or("").split(' ');
        let (method, target) = (rl.next().unwrap_or(""), rl.next().unwrap_or(""));
        let host = lines.find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("host")).map(|(_, v)| v.trim().to_ascii_lowercase()));
        let host_ok = host.as_deref().map(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}") || h == "127.0.0.1" || h == "localhost").unwrap_or(false);
        let Ok(u) = Url::parse(&format!("http://127.0.0.1{}", if target.starts_with('/') { target } else { "/" })) else {
            respond(&mut s, "400 Bad Request", "").await;
            continue;
        };
        if method != "GET" {
            respond(&mut s, "405 Method Not Allowed", "").await;
            continue;
        }
        if u.path() != "/callback" {
            respond(&mut s, "404 Not Found", "").await;
            continue;
        }
        if !host_ok {
            respond(&mut s, "400 Bad Request", "").await;
            continue;
        }
        let q: HashMap<String, String> = u.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        // A wrong `state` is not the browser coming back from Zoom (any local page can fire such a request):
        // answer 400 and keep waiting for the real callback until the timeout.
        if q.get("state").map(String::as_str) != Some(state) {
            respond(&mut s, "400 Bad Request", PAGE_FAIL).await;
            continue;
        }
        // the one genuine callback; whatever its outcome, the listener closes afterwards
        if q.contains_key("error") {
            respond(&mut s, "200 OK", PAGE_FAIL).await;
            return Err(ZoomError::Denied);
        }
        return match q.get("code").filter(|c| !c.is_empty() && c.len() <= 2048) {
            Some(c) => {
                respond(&mut s, "200 OK", PAGE_OK).await;
                Ok(c.clone())
            }
            None => {
                respond(&mut s, "400 Bad Request", PAGE_FAIL).await;
                Err(ZoomError::Api("Zoom's sign-in reply was missing the authorization code.".into()))
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{serve, Mock, Req, Resp};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn cfg(mock: &Mock, secret: Option<&str>) -> ZoomConfig {
        ZoomConfig {
            client_id: "cid".into(),
            client_secret: secret.map(String::from),
            redirect_port: 0,
            auth_base: format!("http://127.0.0.1:{}", mock.port),
            api_base: format!("http://127.0.0.1:{}/v2", mock.port),
            allow_insecure_loopback: true,
        }
    }

    fn new_client(mock: &Mock, secret: Option<&str>) -> (ZoomClient, Arc<InMemoryTokenStore>) {
        let st = Arc::new(InMemoryTokenStore::new());
        (ZoomClient::new(cfg(mock, secret), st.clone()), st)
    }

    async fn raw_get(port: u16, target: &str, host: &str) -> String {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out).await;
        out
    }

    fn port_of(sess: &AuthSession) -> u16 {
        Url::parse(&sess.redirect_uri).unwrap().port().unwrap()
    }

    fn token_json(access: &str, refresh: &str) -> String {
        format!(r#"{{"access_token":"{access}","refresh_token":"{refresh}","expires_in":3600,"token_type":"bearer"}}"#)
    }

    #[test]
    fn pkce_rfc7636_vector() {
        assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[tokio::test]
    async fn authorize_url_and_randomness() {
        let mock = serve(|_| Resp::new(500, "")).await;
        let (c, _) = new_client(&mock, None);
        let a = c.begin_authorize().unwrap();
        let b = c.begin_authorize().unwrap();
        assert_ne!(a.state, b.state);
        assert_ne!(a.verifier, b.verifier);
        assert!((43..=128).contains(&a.verifier.len()));
        let u = Url::parse(&a.url).unwrap();
        let q: HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(u.path(), "/oauth/authorize");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], "cid");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], pkce_challenge(&a.verifier));
        assert_eq!(q["state"], a.state);
        assert_eq!(q["redirect_uri"], a.redirect_uri);
        assert!(a.redirect_uri.starts_with("http://127.0.0.1:") && a.redirect_uri.ends_with("/callback"));
        assert!(!format!("{a:?}").contains(&a.verifier));
        assert!(mock.requests().is_empty());
        // not configured
        let c2 = ZoomClient::new(ZoomConfig::default(), Arc::new(InMemoryTokenStore::new()));
        assert_eq!(c2.begin_authorize().err(), Some(ZoomError::NotConfigured));
        assert_eq!(c2.redirect_uri(), "http://127.0.0.1:53682/callback");
    }

    #[tokio::test]
    async fn port_in_use_is_reported() {
        let mock = serve(|_| Resp::new(500, "")).await;
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut cf = cfg(&mock, None);
        cf.redirect_port = held.local_addr().unwrap().port();
        let c = ZoomClient::new(cf.clone(), Arc::new(InMemoryTokenStore::new()));
        assert_eq!(c.begin_authorize().err(), Some(ZoomError::PortInUse(cf.redirect_port)));
    }

    #[tokio::test]
    async fn state_mismatch_is_rejected_but_the_wait_continues() {
        let mock = serve(|_| Resp::new(200, &token_json("a", "r"))).await;
        let (c, st) = new_client(&mock, None);
        let sess = c.begin_authorize().unwrap();
        let port = port_of(&sess);
        let state = sess.state.clone();
        let h = tokio::spawn(async move { c.wait_for_callback(sess, Duration::from_secs(5)).await });
        let resp = raw_get(port, "/callback?code=abc&state=WRONG", &format!("127.0.0.1:{port}")).await;
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        let resp = raw_get(port, "/callback?code=abc", &format!("127.0.0.1:{port}")).await;
        assert!(resp.starts_with("HTTP/1.1 400"), "missing state: {resp}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!h.is_finished(), "a stray request must not end the sign-in");
        assert!(mock.requests().is_empty(), "no token exchange for a bad state");
        assert_eq!(st.get(KEY_ACCESS).unwrap(), None);
        // the genuine callback still completes
        let resp = raw_get(port, &format!("/callback?code=abc&state={state}"), &format!("127.0.0.1:{port}")).await;
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert_eq!(h.await.unwrap(), Ok(()));
        assert_eq!(st.get(KEY_ACCESS).unwrap().as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn q11_clients_sharing_a_refresh_lock_refresh_once() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let rc = refreshes.clone();
        let mock = serve(move |r: &Req| match r.path.as_str() {
            "/oauth/token" => {
                rc.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(100));
                Resp::new(200, &token_json("new-access", "new-refresh"))
            }
            _ => Resp::new(404, ""),
        })
        .await;
        let store = Arc::new(InMemoryTokenStore::new());
        store.set(KEY_ACCESS, "expired").unwrap();
        store.set(KEY_REFRESH, "old").unwrap();
        store.set(KEY_EXPIRES, "1").unwrap();
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        let a = Arc::new(ZoomClient::with_refresh_lock(cfg(&mock, Some("s")), store.clone(), lock.clone()));
        let b = Arc::new(ZoomClient::with_refresh_lock(cfg(&mock, Some("s")), store.clone(), lock));
        let (a2, b2) = (a.clone(), b.clone());
        let (x, y) = tokio::join!(tokio::spawn(async move { a2.access_token(None).await }), tokio::spawn(async move { b2.access_token(None).await }));
        assert_eq!(x.unwrap().unwrap(), "new-access");
        assert_eq!(y.unwrap().unwrap(), "new-access");
        assert_eq!(refreshes.load(Ordering::SeqCst), 1, "one rotation, not two racing ones");
    }

    #[tokio::test]
    async fn callback_timeout() {
        let mock = serve(|_| Resp::new(500, "")).await;
        let (c, _) = new_client(&mock, None);
        let sess = c.begin_authorize().unwrap();
        assert_eq!(c.wait_for_callback(sess, Duration::from_millis(150)).await, Err(ZoomError::CallbackTimeout));
        assert!(mock.requests().is_empty());
    }

    #[tokio::test]
    async fn full_flow_exchange_stores_tokens() {
        let mock = serve(|r: &Req| match r.path.as_str() {
            "/oauth/token" => Resp::new(200, &token_json("acc1", "ref1")),
            _ => Resp::new(404, ""),
        })
        .await;
        let (c, st) = new_client(&mock, Some("sekret"));
        let sess = c.begin_authorize().unwrap();
        let (port, state, url) = (port_of(&sess), sess.state.clone(), sess.url.clone());
        let challenge = Url::parse(&url).unwrap().query_pairs().find(|(k, _)| k == "code_challenge").unwrap().1.into_owned();
        let h = tokio::spawn(async move { c.wait_for_callback(sess, Duration::from_secs(5)).await });
        // stray requests do not end the wait
        assert!(raw_get(port, "/favicon.ico", &format!("127.0.0.1:{port}")).await.starts_with("HTTP/1.1 404"));
        // DNS-rebinding style Host is refused and does not end the wait either
        assert!(raw_get(port, "/callback?code=x&state=x", "evil.example").await.starts_with("HTTP/1.1 400"));
        let page = raw_get(port, &format!("/callback?code=THECODE&state={state}"), &format!("127.0.0.1:{port}")).await;
        assert!(page.contains("You can close this tab and return to MiraMail"), "{page}");
        h.await.unwrap().unwrap();
        // listener is closed afterwards
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err());
        let reqs = mock.requests();
        assert_eq!(reqs.len(), 1);
        let form: HashMap<_, _> = Url::parse(&format!("http://x/?{}", reqs[0].body)).unwrap().query_pairs().into_owned().collect();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "THECODE");
        assert_eq!(pkce_challenge(&form["code_verifier"]), challenge);
        assert_eq!(form["redirect_uri"], format!("http://127.0.0.1:{port}/callback"));
        assert!(!form.contains_key("client_id"), "secret present -> basic auth only");
        let auth = reqs[0].header("authorization").unwrap();
        assert_eq!(auth, format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("cid:sekret")));
        assert_eq!(st.get(KEY_ACCESS).unwrap().as_deref(), Some("acc1"));
        assert_eq!(st.get(KEY_REFRESH).unwrap().as_deref(), Some("ref1"));
        assert!(st.get(KEY_EXPIRES).unwrap().unwrap().parse::<i64>().unwrap() > now() + 3000);
    }

    #[tokio::test]
    async fn public_client_sends_client_id_in_body() {
        let mock = serve(|_| Resp::new(200, &token_json("a", "r"))).await;
        let (c, _) = new_client(&mock, None);
        let sess = c.begin_authorize().unwrap();
        let (port, state) = (port_of(&sess), sess.state.clone());
        let h = tokio::spawn(async move { c.wait_for_callback(sess, Duration::from_secs(5)).await });
        raw_get(port, &format!("/callback?code=c&state={state}"), "localhost").await;
        h.await.unwrap().unwrap();
        let r = &mock.requests()[0];
        assert!(r.header("authorization").is_none());
        assert!(r.body.contains("client_id=cid"));
    }

    #[tokio::test]
    async fn denied_by_user() {
        let mock = serve(|_| Resp::new(500, "")).await;
        let (c, _) = new_client(&mock, None);
        let sess = c.begin_authorize().unwrap();
        let (port, state) = (port_of(&sess), sess.state.clone());
        let h = tokio::spawn(async move { c.wait_for_callback(sess, Duration::from_secs(5)).await });
        raw_get(port, &format!("/callback?error=access_denied&state={state}"), &format!("127.0.0.1:{port}")).await;
        assert_eq!(h.await.unwrap(), Err(ZoomError::Denied));
    }

    #[tokio::test]
    async fn refresh_rotates_and_is_single_flight() {
        let refreshes = Arc::new(AtomicUsize::new(0));
        let rc = refreshes.clone();
        let mock = serve(move |r: &Req| match r.path.as_str() {
            "/oauth/token" => {
                assert!(r.body.contains("grant_type=refresh_token") && r.body.contains("refresh_token=old-refresh"));
                rc.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(150));
                Resp::new(200, &token_json("new-access", "new-refresh"))
            }
            "/v2/users/me/meetings" => {
                if r.header("authorization") != Some("Bearer new-access") {
                    return Resp::new(401, r#"{"code":124,"message":"Invalid access token."}"#);
                }
                Resp::new(201, r#"{"id":81234567890,"join_url":"https://zoom.us/j/81234567890?pwd=x","password":"pw1","start_url":"https://zoom.us/s/1"}"#)
            }
            _ => Resp::new(404, ""),
        })
        .await;
        let (c, st) = new_client(&mock, Some("s"));
        st.set(KEY_ACCESS, "expired").unwrap();
        st.set(KEY_REFRESH, "old-refresh").unwrap();
        st.set(KEY_EXPIRES, "1").unwrap();
        let c = Arc::new(c);
        let mut hs = vec![];
        for _ in 0..4 {
            let c = c.clone();
            hs.push(tokio::spawn(async move { c.create_meeting("Sync", "2026-11-06T15:00:00+05:30", 30, "Asia/Kolkata", Some("agenda")).await }));
        }
        for h in hs {
            let m = h.await.unwrap().unwrap();
            assert_eq!(m.meeting_id, "81234567890");
            assert_eq!(m.password.as_deref(), Some("pw1"));
            assert_eq!(m.start_url.as_deref(), Some("https://zoom.us/s/1"));
            assert!(m.join_url.starts_with("https://zoom.us/j/"));
        }
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(st.get(KEY_REFRESH).unwrap().as_deref(), Some("new-refresh"));
        assert_eq!(st.get(KEY_ACCESS).unwrap().as_deref(), Some("new-access"));
        let create = mock.requests().into_iter().find(|r| r.path == "/v2/users/me/meetings").unwrap();
        let b: Value = serde_json::from_str(&create.body).unwrap();
        assert_eq!(b["type"], 2);
        assert_eq!(b["topic"], "Sync");
        assert_eq!(b["start_time"], "2026-11-06T09:30:00Z");
        assert_eq!(b["duration"], 30);
        assert_eq!(b["timezone"], "Asia/Kolkata");
        assert_eq!(b["settings"]["join_before_host"], true);
    }

    #[tokio::test]
    async fn rejected_access_token_triggers_one_refresh_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cc = calls.clone();
        let mock = serve(move |r: &Req| match r.path.as_str() {
            "/oauth/token" => Resp::new(200, &token_json("fresh", "r2")),
            p if p.starts_with("/v2/meetings/") => {
                cc.fetch_add(1, Ordering::SeqCst);
                if r.header("authorization") == Some("Bearer fresh") { Resp::new(204, "") } else { Resp::new(401, r#"{"code":124}"#) }
            }
            _ => Resp::new(404, ""),
        })
        .await;
        let (c, st) = new_client(&mock, None);
        st.set(KEY_ACCESS, "revoked").unwrap();
        st.set(KEY_REFRESH, "r1").unwrap();
        st.set(KEY_EXPIRES, &(now() + 3000).to_string()).unwrap();
        c.delete_meeting("123").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn delete_and_update_meeting() {
        let mock = serve(|r: &Req| match (r.method.as_str(), r.path.as_str()) {
            ("DELETE", "/v2/meetings/111") => Resp::new(404, r#"{"code":3001,"message":"Meeting does not exist"}"#),
            ("DELETE", "/v2/meetings/222") => Resp::new(204, ""),
            ("DELETE", "/v2/meetings/333") => Resp::new(500, "{}"),
            ("PATCH", "/v2/meetings/222") => Resp::new(204, ""),
            ("PATCH", "/v2/meetings/111") => Resp::new(404, "{}"),
            _ => Resp::new(400, "{}"),
        })
        .await;
        let (c, st) = new_client(&mock, None);
        st.set(KEY_ACCESS, "a").unwrap();
        st.set(KEY_REFRESH, "r").unwrap();
        st.set(KEY_EXPIRES, &(now() + 3000).to_string()).unwrap();
        c.delete_meeting("111").await.unwrap();
        c.delete_meeting("222").await.unwrap();
        assert!(matches!(c.delete_meeting("333").await, Err(ZoomError::Api(_))));
        c.update_meeting("222", "T", "2026-11-06T10:00:00Z", 45, "UTC", None).await.unwrap();
        assert_eq!(c.update_meeting("111", "T", "2026-11-06T10:00:00Z", 45, "UTC", None).await, Err(ZoomError::MeetingNotFound));
        assert!(matches!(c.delete_meeting("../x").await, Err(ZoomError::Invalid(_))));
        assert!(matches!(c.update_meeting("222", "T", "not a time", 45, "UTC", None).await, Err(ZoomError::Invalid(_))));
        let patch = mock.requests().into_iter().find(|r| r.method == "PATCH").unwrap();
        assert!(!patch.body.contains("\"type\""));
    }

    #[tokio::test]
    async fn no_tokens_means_no_network() {
        let mock = serve(|_| Resp::new(500, "")).await;
        let (c, _) = new_client(&mock, Some("s"));
        assert_eq!(c.create_meeting("T", "2026-11-06T10:00:00Z", 30, "UTC", None).await, Err(ZoomError::NotConnected));
        assert_eq!(c.delete_meeting("123").await, Err(ZoomError::NotConnected));
        let s = c.status().await.unwrap();
        assert!(s.configured && !s.connected && s.account.is_none());
        assert_eq!(s.client_id.as_deref(), Some("cid"));
        c.disconnect().await.unwrap();
        assert!(mock.requests().is_empty(), "{:?}", mock.requests());
    }

    #[tokio::test]
    async fn status_fetches_account_once_and_disconnect_revokes() {
        let mock = serve(|r: &Req| match r.path.as_str() {
            "/v2/users/me" => Resp::new(200, r#"{"email":"me@example.com","id":"x"}"#),
            "/oauth/revoke" => Resp::new(200, "{}"),
            _ => Resp::new(404, ""),
        })
        .await;
        let (c, st) = new_client(&mock, Some("s"));
        st.set(KEY_ACCESS, "a").unwrap();
        st.set(KEY_REFRESH, "r").unwrap();
        st.set(KEY_EXPIRES, &(now() + 3000).to_string()).unwrap();
        let s = c.status().await.unwrap();
        assert!(s.connected);
        assert_eq!(s.account.as_deref(), Some("me@example.com"));
        c.status().await.unwrap();
        assert_eq!(mock.requests().iter().filter(|r| r.path == "/v2/users/me").count(), 1);
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["clientId"], "cid");
        assert!(j["redirectUri"].as_str().unwrap().ends_with("/callback"));
        c.disconnect().await.unwrap();
        assert!(mock.requests().iter().any(|r| r.path == "/oauth/revoke" && r.body.contains("token=a")));
        for k in [KEY_ACCESS, KEY_REFRESH, KEY_EXPIRES] {
            assert_eq!(st.get(k).unwrap(), None);
        }
        assert!(!c.status().await.unwrap().connected);
    }

    #[tokio::test]
    async fn disconnect_works_when_revoke_fails() {
        let mock = serve(|_| Resp::new(500, "")).await;
        let (c, st) = new_client(&mock, None);
        st.set(KEY_ACCESS, "a").unwrap();
        c.disconnect().await.unwrap();
        assert_eq!(st.get(KEY_ACCESS).unwrap(), None);
    }

    #[tokio::test]
    async fn error_mapping_is_friendly_and_secret_free() {
        let redirect = "http://127.0.0.1:53682/callback";
        let e = map_oauth_error(401, &serde_json::json!({"error":"invalid_client","reason":"Invalid client_id or client_secret"}), redirect);
        assert_eq!(e.to_string(), "Check your Client ID/Secret");
        let e = map_oauth_error(400, &serde_json::json!({"error":"invalid_request","reason":"Invalid redirect_uri"}), redirect);
        assert!(e.to_string().contains(redirect));
        assert_eq!(map_oauth_error(400, &serde_json::json!({"error":"invalid_grant"}), redirect), ZoomError::Reauthorize);
        assert!(map_api_error(400, &serde_json::json!({"code":4711,"message":"Invalid access token, does not contain scopes:[meeting:write:meeting]"})).to_string().contains("meeting:write:meeting user:read:user"));
        assert_eq!(map_api_error(429, &Value::Null), ZoomError::RateLimited);
        // exchange failure end to end
        let mock = serve(|_| Resp::new(401, r#"{"error":"invalid_client","reason":"Invalid client secret"}"#)).await;
        let (c, st) = new_client(&mock, Some("bad"));
        st.set(KEY_REFRESH, "r").unwrap();
        st.set(KEY_EXPIRES, "1").unwrap();
        st.set(KEY_ACCESS, "a").unwrap();
        let e = c.delete_meeting("1").await.unwrap_err();
        assert_eq!(e.to_string(), "Check your Client ID/Secret");
        // invalid_grant on refresh clears tokens
        let mock = serve(|_| Resp::new(400, r#"{"error":"invalid_grant","reason":"Invalid refresh token"}"#)).await;
        let (c, st) = new_client(&mock, None);
        st.set(KEY_REFRESH, "r").unwrap();
        assert_eq!(c.create_meeting("T", "2026-11-06T10:00:00Z", 30, "UTC", None).await, Err(ZoomError::Reauthorize));
        assert_eq!(st.get(KEY_REFRESH).unwrap(), None);
    }

    #[tokio::test]
    async fn insecure_urls_refused_by_default() {
        let mock = serve(|_| Resp::new(500, "")).await;
        let mut cf = cfg(&mock, None);
        cf.allow_insecure_loopback = false;
        let st = Arc::new(InMemoryTokenStore::new());
        st.set(KEY_ACCESS, "a").unwrap();
        st.set(KEY_REFRESH, "r").unwrap();
        st.set(KEY_EXPIRES, &(now() + 3000).to_string()).unwrap();
        let c = ZoomClient::new(cf, st);
        assert_eq!(c.begin_authorize().err(), Some(ZoomError::Insecure));
        assert_eq!(c.delete_meeting("1").await, Err(ZoomError::Insecure));
        assert!(mock.requests().is_empty());
        let d = ZoomConfig::default();
        assert_eq!((d.auth_base.as_str(), d.api_base.as_str(), d.redirect_port), ("https://zoom.us", "https://api.zoom.us/v2", 53682));
        assert!(!format!("{:?}", ZoomConfig { client_secret: Some("topsecret".into()), ..d }).contains("topsecret"));
    }
}
