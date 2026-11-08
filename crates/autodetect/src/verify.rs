//! Credential verification (spec section 4).
//!
//! Design choice: a minimal hand-written IMAP (AUTHENTICATE PLAIN / LOGIN) and
//! SMTP (AUTH PLAIN / LOGIN) client over tokio-rustls instead of async-imap +
//! lettre. We only need "does this login work", not a session; this avoids two
//! large dependency trees, keeps the release binary small, and gives exact
//! control over TLS errors so an invalid certificate is a distinct, hard-stop
//! error. The full IMAP/SMTP stacks belong to crates/mailcore (phase 2).
//!
//! Safety: the password is only ever written to a TLS-protected socket, is
//! never logged, never stored in any returned type, and no mail is sent.

use crate::error::{AuthHint, Stage, VerifyError};
use crate::providers::ProviderDb;
use crate::types::*;
use rustls::pki_types::ServerName;
use rustls::{CertificateError, ClientConfig, RootCertStore};
use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{timeout, timeout_at, Instant};
use tokio_rustls::TlsConnector;

/// Total budget for the whole verification (spec: 5s). IMAP gets the first
/// half; SMTP gets whatever remains (so a fast IMAP leaves more for SMTP).
const VERIFY_BUDGET: Duration = Duration::from_secs(5);
/// Per-step cap (TCP connect, TLS handshake, each read/write). Always further
/// bounded by the candidate's own deadline.
const STEP_TIMEOUT: Duration = Duration::from_millis(2000);
const MAX_CANDIDATES: usize = 3;
const MAX_LINE: usize = 8192;

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type Conn = BufReader<Box<dyn Io>>;

#[derive(Debug, PartialEq, Eq)]
pub enum StepError {
    Cert { host: String, reason: String },
    Unreachable(String),
    Auth(String),
}

/// The server's most recent explicit credential rejection for the attempt in
/// flight. Lives outside the attempt future so it survives cancellation by a
/// deadline: a later hang-up or timeout must not mask a real "wrong password".
type Rejected = Arc<Mutex<Option<String>>>;

fn note_rejection(r: &Rejected, msg: &str) {
    if let Ok(mut g) = r.lock() {
        *g = Some(msg.to_string());
    }
}

fn take_rejection(r: &Rejected) -> Option<String> {
    r.lock().ok().and_then(|g| g.clone())
}

// ------------------------------------------------------------------ public API

/// Verify credentials against the ranked candidates (budget 5s). IMAP login
/// first, then SMTP AUTH; nothing is sent. Username: full email first, then the
/// local part only if the candidate allows it.
///
/// Once a server has explicitly rejected the credentials, the password is not
/// sent to any host outside the same bundled/MX provider entry.
pub async fn verify(det: &DetectionResult, email: &str, password: &str) -> Result<VerifiedSettings, VerifyError> {
    verify_with_budget(det, email, password, VERIFY_BUDGET).await
}

pub(crate) async fn verify_with_budget(
    det: &DetectionResult,
    email: &str,
    password: &str,
    total: Duration,
) -> Result<VerifiedSettings, VerifyError> {
    let start = Instant::now();
    // Per-candidate deadlines enforce the budget and keep their details; this
    // outer cap is only a safety net.
    match timeout_at(start + total + Duration::from_millis(250), verify_inner(det, email, password, start, total)).await {
        Ok(r) => r,
        Err(_) => Err(VerifyError::Unreachable { details: vec!["verification timed out".into()] }),
    }
}

async fn verify_inner(
    det: &DetectionResult,
    email: &str,
    password: &str,
    start: Instant,
    total: Duration,
) -> Result<VerifiedSettings, VerifyError> {
    let local = email.rsplit_once('@').map(|(l, _)| l).unwrap_or(email);
    let imap_deadline = start + total / 2;
    let smtp_deadline = start + total;
    let (imap, imap_user) = run_stage(Stage::Imap, &det.imap, det, email, local, None, password, imap_deadline).await?;
    let (smtp, smtp_user) =
        run_stage(Stage::Smtp, &det.smtp, det, email, local, Some(&imap_user), password, smtp_deadline).await?;
    Ok(VerifiedSettings {
        email: email.to_string(),
        provider: det.provider.clone(),
        imap: ServerSettings { host: imap.host.clone(), port: imap.port, tls: imap.tls, username: imap_user },
        smtp: ServerSettings { host: smtp.host.clone(), port: smtp.port, tls: smtp.tls, username: smtp_user },
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_stage<'a>(
    stage: Stage,
    cands: &'a [Candidate],
    det: &DetectionResult,
    email: &str,
    local: &str,
    preferred_user: Option<&str>,
    password: &str,
    deadline: Instant,
) -> Result<(&'a Candidate, String), VerifyError> {
    run_stage_with(stage, cands, det, email, local, preferred_user, deadline, |cand, users, rej| async move {
        match stage {
            Stage::Imap => try_imap(&cand, &users, password, rej).await,
            Stage::Smtp => try_smtp(&cand, &users, password, rej).await,
        }
    })
    .await
}

/// Hosts of the bundled provider entry that `det.provider` names (placeholders
/// expanded). Only these may receive the password after a rejection elsewhere
/// in the same entry (e.g. Zoho `imappro` then `imap`).
fn provider_hosts(det: &DetectionResult) -> HashSet<String> {
    let mut set = HashSet::new();
    if let Some(p) = det.provider.as_deref().and_then(|id| ProviderDb::bundled().by_id(id)) {
        for e in p
            .imap
            .iter()
            .chain(&p.smtp)
            .chain(p.personal_imap.iter().flatten())
            .chain(p.personal_smtp.iter().flatten())
        {
            set.insert(e.host.replace("{domain}", &det.domain).to_ascii_lowercase());
        }
    }
    set
}

#[allow(clippy::too_many_arguments)]
async fn run_stage_with<'a, F, Fut>(
    stage: Stage,
    cands: &'a [Candidate],
    det: &DetectionResult,
    email: &str,
    local: &str,
    preferred_user: Option<&str>,
    deadline: Instant,
    mut attempt: F,
) -> Result<(&'a Candidate, String), VerifyError>
where
    F: FnMut(Candidate, Vec<String>, Rejected) -> Fut,
    Fut: Future<Output = Result<String, StepError>>,
{
    if cands.is_empty() {
        return Err(VerifyError::NoCandidates);
    }
    let trusted = provider_hosts(det);
    let same_entry = |c: &Candidate| {
        matches!(c.source, Source::Bundled | Source::Mx) && trusted.contains(&c.host.to_ascii_lowercase())
    };
    let n = cands.len().min(MAX_CANDIDATES);
    let mut unreachable = Vec::new();
    let mut auth_fail: Option<String> = None;
    for (idx, cand) in cands.iter().take(n).enumerate() {
        // After an explicit rejection, only further hosts of the same provider
        // entry may see the password. Anything else (SRV/autoconfig/heuristic
        // hosts, other providers) is never tried.
        if auth_fail.is_some() && !same_entry(cand) {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            unreachable.push(format!("{}:{}: not tried (time budget used up)", cand.host, cand.port));
            break;
        }
        // Split what is left of the stage evenly over the remaining candidates.
        let cand_deadline = now + (deadline - now) / (n - idx) as u32;
        let mut users = usernames(cand, email, local);
        if let Some(p) = preferred_user {
            if let Some(i) = users.iter().position(|u| u == p) {
                users.swap(0, i);
            }
        }
        let rej: Rejected = Arc::default();
        let res = match timeout_at(cand_deadline, attempt(cand.clone(), users, rej.clone())).await {
            Ok(r) => r,
            Err(_) => Err(StepError::Unreachable("timed out".into())),
        };
        // A hang-up/timeout/421 after the server already said "wrong password"
        // is still a wrong password, not an unreachable server.
        let res = match (res, take_rejection(&rej)) {
            (Err(StepError::Unreachable(_)), Some(m)) => Err(StepError::Auth(m)),
            (r, _) => r,
        };
        match res {
            Ok(user) => return Ok((cand, user)),
            Err(StepError::Cert { host, reason }) => return Err(VerifyError::CertificateInvalid { host, reason }),
            Err(StepError::Auth(m)) => {
                auth_fail = auth_fail.or(Some(m));
                if !same_entry(cand) {
                    break;
                }
            }
            Err(StepError::Unreachable(m)) => unreachable.push(format!("{}:{}: {m}", cand.host, cand.port)),
        }
    }
    match auth_fail {
        Some(server_message) => Err(VerifyError::AuthFailed { stage, server_message, hints: hints_for(det, stage) }),
        None => Err(VerifyError::Unreachable { details: unreachable }),
    }
}

pub fn usernames(cand: &Candidate, email: &str, local: &str) -> Vec<String> {
    let mut v = vec![email.to_string()];
    if cand.username_hint == UsernameHint::LocalPartFallback && local != email && !local.is_empty() {
        v.push(local.to_string());
    }
    v
}

pub fn hints_for(det: &DetectionResult, stage: Stage) -> Vec<AuthHint> {
    let Some(p) = det.provider.as_deref().and_then(|id| ProviderDb::bundled().by_id(id)) else {
        return vec![];
    };
    let mut h = Vec::new();
    if stage == Stage::Imap {
        if let Some(url) = &p.imap_disabled_url {
            h.push(AuthHint::ZohoImapDisabled { url: url.clone() });
        }
    }
    if let Some(url) = &p.app_password_url {
        h.push(AuthHint::AppPassword { provider: p.name.clone(), url: url.clone() });
    }
    h
}

// ------------------------------------------------------------------------- TLS

fn tls_config() -> Arc<ClientConfig> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        // webpki-roots (bundled Mozilla roots): deterministic and never touches
        // the macOS Keychain. Private/corporate CAs are therefore not trusted.
        let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let cfg = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring provider supports default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

fn describe_cert_error(e: &CertificateError) -> String {
    match e {
        CertificateError::UnknownIssuer => "certificate is not issued by a trusted authority".into(),
        CertificateError::Expired | CertificateError::ExpiredContext { .. } => "certificate has expired".into(),
        CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => "certificate is not valid yet".into(),
        CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
            "certificate does not match the server name".into()
        }
        CertificateError::Revoked => "certificate has been revoked".into(),
        other => format!("certificate rejected ({other:?})"),
    }
}

/// Distinguish certificate problems (hard stop) from other TLS/IO failures.
pub fn classify_tls_err(host: &str, e: io::Error) -> StepError {
    if let Some(re) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
        match re {
            rustls::Error::InvalidCertificate(ce) => {
                return StepError::Cert { host: host.into(), reason: describe_cert_error(ce) }
            }
            rustls::Error::NoCertificatesPresented => {
                return StepError::Cert { host: host.into(), reason: "server presented no certificate".into() }
            }
            other => return StepError::Unreachable(format!("TLS handshake failed: {other}")),
        }
    }
    StepError::Unreachable(format!("TLS handshake failed: {e}"))
}

async fn tls_upgrade(host: &str, io: Box<dyn Io>) -> Result<Conn, StepError> {
    let name = ServerName::try_from(host.to_string()).map_err(|_| StepError::Unreachable("invalid host name".into()))?;
    let tls = match timeout(STEP_TIMEOUT, TlsConnector::from(tls_config()).connect(name, io)).await {
        Err(_) => return Err(StepError::Unreachable("TLS handshake timed out".into())),
        Ok(r) => r.map_err(|e| classify_tls_err(host, e))?,
    };
    Ok(BufReader::new(Box::new(tls)))
}

async fn tcp(host: &str, port: u16) -> Result<Box<dyn Io>, StepError> {
    match timeout(STEP_TIMEOUT, TcpStream::connect((host, port))).await {
        Err(_) => Err(StepError::Unreachable("connect timed out".into())),
        Ok(Err(e)) => Err(StepError::Unreachable(format!("connect failed: {e}"))),
        Ok(Ok(s)) => Ok(Box::new(s)),
    }
}

// ---------------------------------------------------------------- line helpers

fn io_err(e: io::Error) -> StepError {
    StepError::Unreachable(format!("i/o error: {e}"))
}

pub async fn read_line(c: &mut Conn) -> Result<String, StepError> {
    match timeout(STEP_TIMEOUT, read_line_inner(c)).await {
        Ok(r) => r,
        Err(_) => Err(StepError::Unreachable("read timed out".into())),
    }
}

async fn read_line_inner(c: &mut Conn) -> Result<String, StepError> {
    let mut buf = Vec::new();
    loop {
        let avail = c.fill_buf().await.map_err(io_err)?;
        if avail.is_empty() {
            return Err(StepError::Unreachable("connection closed by server".into()));
        }
        if let Some(i) = avail.iter().position(|b| *b == b'\n') {
            buf.extend_from_slice(&avail[..i]);
            c.consume(i + 1);
            break;
        }
        let n = avail.len();
        buf.extend_from_slice(avail);
        c.consume(n);
        if buf.len() > MAX_LINE {
            return Err(StepError::Unreachable("server line too long".into()));
        }
    }
    if buf.len() > MAX_LINE {
        return Err(StepError::Unreachable("server line too long".into()));
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub async fn send(c: &mut Conn, line: &str) -> Result<(), StepError> {
    let w = c.get_mut();
    let write = async {
        w.write_all(line.as_bytes()).await?;
        w.write_all(b"\r\n").await?;
        w.flush().await
    };
    match timeout(STEP_TIMEOUT, write).await {
        Ok(r) => r.map_err(io_err),
        Err(_) => Err(StepError::Unreachable("write timed out".into())),
    }
}

pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for ch in input.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if ch.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if ch.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn sasl_plain(user: &str, pass: &str) -> String {
    let mut v = Vec::with_capacity(user.len() + pass.len() + 2);
    v.push(0);
    v.extend_from_slice(user.as_bytes());
    v.push(0);
    v.extend_from_slice(pass.as_bytes());
    base64(&v)
}

fn clip(s: &str) -> String {
    s.chars().take(200).collect()
}

// ------------------------------------------------------------------------ IMAP

async fn try_imap(cand: &Candidate, users: &[String], password: &str, rej: Rejected) -> Result<String, StepError> {
    let io = tcp(&cand.host, cand.port).await?;
    let mut c = match cand.tls {
        TlsMode::Implicit => tls_upgrade(&cand.host, io).await?,
        TlsMode::StartTls => {
            let mut c = BufReader::new(io);
            imap_greeting(&mut c).await?;
            send(&mut c, "s0 STARTTLS").await?;
            loop {
                let l = read_line(&mut c).await?;
                if let Some(rest) = l.strip_prefix("s0 ") {
                    if !rest.to_ascii_uppercase().starts_with("OK") {
                        return Err(StepError::Unreachable("server refused STARTTLS".into()));
                    }
                    break;
                }
            }
            if !c.buffer().is_empty() {
                return Err(StepError::Unreachable("unexpected data before TLS".into()));
            }
            tls_upgrade(&cand.host, c.into_inner()).await?
        }
    };
    if cand.tls == TlsMode::Implicit {
        imap_greeting(&mut c).await?;
    }
    let r = imap_login_inner(&mut c, users, password, &rej).await;
    let _ = send(&mut c, "z LOGOUT").await;
    r
}

pub async fn imap_greeting(c: &mut Conn) -> Result<(), StepError> {
    let l = read_line(c).await?;
    let u = l.to_ascii_uppercase();
    if u.starts_with("* OK") {
        Ok(())
    } else {
        Err(StepError::Unreachable(format!("unexpected IMAP greeting: {}", clip(&l))))
    }
}

/// Read until the tagged completion for `tag`; returns (untagged lines, status, text).
async fn imap_finish(c: &mut Conn, tag: &str) -> Result<(Vec<String>, String, String), StepError> {
    let mut untagged = Vec::new();
    let prefix = format!("{tag} ");
    for _ in 0..200 {
        let l = read_line(c).await?;
        if let Some(rest) = l.strip_prefix(&prefix) {
            let (status, text) = rest.split_once(' ').unwrap_or((rest, ""));
            return Ok((untagged, status.to_ascii_uppercase(), clip(text)));
        }
        untagged.push(l);
    }
    Err(StepError::Unreachable("too many untagged responses".into()))
}

#[cfg(test)]
pub async fn imap_login(c: &mut Conn, users: &[String], password: &str) -> Result<String, StepError> {
    imap_login_inner(c, users, password, &Rejected::default()).await
}

/// One credential attempt. `Ok(Err(text))` is an explicit server rejection.
async fn imap_attempt(c: &mut Conn, has_plain: bool, tag: &str, user: &str, password: &str) -> Result<Result<(), String>, StepError> {
    if has_plain {
        send(c, &format!("{tag} AUTHENTICATE PLAIN")).await?;
        // Wait for the "+" continuation (or an immediate tagged refusal).
        loop {
            let l = read_line(c).await?;
            if l.starts_with('+') {
                break;
            }
            if let Some(rest) = l.strip_prefix(&format!("{tag} ")) {
                return Ok(Err(clip(rest)));
            }
        }
        send(c, &sasl_plain(user, password)).await?;
        let (_, status, text) = imap_finish(c, tag).await?;
        Ok(if status == "OK" { Ok(()) } else { Err(text) })
    } else {
        let quoted = quote_imap(password)
            .ok_or_else(|| StepError::Auth("password contains characters this server login cannot carry".into()))?;
        let qu = quote_imap(user).ok_or_else(|| StepError::Auth("unsupported characters in username".into()))?;
        send(c, &format!("{tag} LOGIN {qu} {quoted}")).await?;
        let (_, status, text) = imap_finish(c, tag).await?;
        Ok(if status == "OK" { Ok(()) } else { Err(text) })
    }
}

async fn imap_login_inner(c: &mut Conn, users: &[String], password: &str, rej: &Rejected) -> Result<String, StepError> {
    send(c, "c1 CAPABILITY").await?;
    let (lines, status, _) = imap_finish(c, "c1").await?;
    if status != "OK" {
        return Err(StepError::Unreachable("CAPABILITY failed".into()));
    }
    let caps = lines.join(" ").to_ascii_uppercase();
    let has_plain = caps.split_whitespace().any(|w| w == "AUTH=PLAIN");
    let login_disabled = caps.split_whitespace().any(|w| w == "LOGINDISABLED");
    if !has_plain && login_disabled {
        return Err(StepError::Unreachable("server offers no password login".into()));
    }
    let mut last_fail: Option<String> = None;
    for (n, user) in users.iter().enumerate() {
        let tag = format!("a{}", n + 1);
        match imap_attempt(c, has_plain, &tag, user, password).await {
            Ok(Ok(())) => return Ok(user.clone()),
            Ok(Err(m)) => {
                note_rejection(rej, &m);
                last_fail = Some(m);
            }
            // The server already rejected an earlier username and then hung up,
            // timed out or sent BYE: report the rejection, not "unreachable".
            Err(StepError::Unreachable(_)) if last_fail.is_some() => {
                return Err(StepError::Auth(last_fail.unwrap_or_default()))
            }
            Err(e) => return Err(e),
        }
    }
    Err(StepError::Auth(last_fail.unwrap_or_default()))
}

fn quote_imap(s: &str) -> Option<String> {
    if !s.is_ascii() || s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return None;
    }
    Some(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

// ------------------------------------------------------------------------ SMTP

pub async fn smtp_reply(c: &mut Conn) -> Result<(u16, Vec<String>), StepError> {
    let mut lines = Vec::new();
    for _ in 0..100 {
        let l = read_line(c).await?;
        let code: u16 = l.get(..3).and_then(|s| s.parse().ok()).ok_or_else(|| {
            StepError::Unreachable(format!("malformed SMTP reply: {}", clip(&l)))
        })?;
        let more = l.as_bytes().get(3) == Some(&b'-');
        lines.push(l.get(4..).unwrap_or("").to_string());
        if !more {
            return Ok((code, lines));
        }
    }
    Err(StepError::Unreachable("SMTP reply too long".into()))
}

async fn ehlo(c: &mut Conn) -> Result<Vec<String>, StepError> {
    send(c, "EHLO miramail.invalid").await?;
    let (code, lines) = smtp_reply(c).await?;
    if code != 250 {
        return Err(StepError::Unreachable(format!("EHLO refused ({code})")));
    }
    Ok(lines.into_iter().map(|l| l.to_ascii_uppercase()).collect())
}

async fn try_smtp(cand: &Candidate, users: &[String], password: &str, rej: Rejected) -> Result<String, StepError> {
    let io = tcp(&cand.host, cand.port).await?;
    let mut c = match cand.tls {
        TlsMode::Implicit => tls_upgrade(&cand.host, io).await?,
        TlsMode::StartTls => BufReader::new(io),
    };
    let (code, _) = smtp_reply(&mut c).await?;
    if code != 220 {
        return Err(StepError::Unreachable(format!("SMTP greeting refused ({code})")));
    }
    let mut caps = ehlo(&mut c).await?;
    if cand.tls == TlsMode::StartTls {
        if !caps.iter().any(|l| l == "STARTTLS") {
            return Err(StepError::Unreachable("server does not offer STARTTLS".into()));
        }
        send(&mut c, "STARTTLS").await?;
        let (code, _) = smtp_reply(&mut c).await?;
        if code != 220 {
            return Err(StepError::Unreachable("server refused STARTTLS".into()));
        }
        if !c.buffer().is_empty() {
            return Err(StepError::Unreachable("unexpected data before TLS".into()));
        }
        c = tls_upgrade(&cand.host, c.into_inner()).await?;
        caps = ehlo(&mut c).await?;
    }
    let r = smtp_auth_inner(&mut c, &caps, users, password, &rej).await;
    let _ = send(&mut c, "QUIT").await;
    r
}

#[cfg(test)]
pub async fn smtp_auth(c: &mut Conn, caps: &[String], users: &[String], password: &str) -> Result<String, StepError> {
    smtp_auth_inner(c, caps, users, password, &Rejected::default()).await
}

async fn smtp_attempt(c: &mut Conn, plain: bool, user: &str, password: &str) -> Result<(u16, Vec<String>), StepError> {
    if plain {
        send(c, &format!("AUTH PLAIN {}", sasl_plain(user, password))).await?;
        return smtp_reply(c).await;
    }
    send(c, "AUTH LOGIN").await?;
    let (code, l) = smtp_reply(c).await?;
    if code != 334 {
        return Ok((code, l));
    }
    send(c, &base64(user.as_bytes())).await?;
    let (code, l) = smtp_reply(c).await?;
    if code != 334 {
        return Ok((code, l));
    }
    send(c, &base64(password.as_bytes())).await?;
    smtp_reply(c).await
}

async fn smtp_auth_inner(
    c: &mut Conn,
    caps: &[String],
    users: &[String],
    password: &str,
    rej: &Rejected,
) -> Result<String, StepError> {
    let mechs: Vec<&str> = caps
        .iter()
        .filter_map(|l| l.strip_prefix("AUTH ").or_else(|| l.strip_prefix("AUTH=")))
        .flat_map(|l| l.split([' ', '=']))
        .collect();
    let plain = mechs.contains(&"PLAIN");
    if !plain && !mechs.contains(&"LOGIN") {
        return Err(StepError::Unreachable("server offers no PLAIN/LOGIN auth".into()));
    }
    let mut last_fail: Option<String> = None;
    for user in users {
        let (code, lines) = match smtp_attempt(c, plain, user, password).await {
            Ok(v) => v,
            // Earlier username was rejected, then the connection dropped / timed out.
            Err(StepError::Unreachable(_)) if last_fail.is_some() => {
                return Err(StepError::Auth(last_fail.unwrap_or_default()))
            }
            Err(e) => return Err(e),
        };
        match code {
            235 => return Ok(user.clone()),
            535 | 534 | 530 | 454 | 504 | 538 => {
                let m = clip(&lines.join(" "));
                note_rejection(rej, &m);
                last_fail = Some(m);
            }
            // 421 (service closing) etc. after a rejection: still the rejection.
            _ if last_fail.is_some() => return Err(StepError::Auth(last_fail.unwrap_or_default())),
            _ => return Err(StepError::Unreachable(format!("unexpected SMTP reply {code}"))),
        }
    }
    Err(StepError::Auth(last_fail.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncReadExt};

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(sasl_plain("a", "b"), "AGEAYg==");
    }

    #[test]
    fn imap_quoting() {
        assert_eq!(quote_imap(r#"a"b\c"#).unwrap(), r#""a\"b\\c""#);
        assert!(quote_imap("pä").is_none());
        assert!(quote_imap("a\r\nb").is_none());
    }

    #[test]
    fn cert_errors_are_distinguished() {
        let mk = |ce| io::Error::new(io::ErrorKind::InvalidData, rustls::Error::InvalidCertificate(ce));
        match classify_tls_err("h.example", mk(CertificateError::UnknownIssuer)) {
            StepError::Cert { host, reason } => {
                assert_eq!(host, "h.example");
                assert!(reason.contains("trusted"));
            }
            o => panic!("{o:?}"),
        }
        assert!(matches!(classify_tls_err("h", mk(CertificateError::Expired)), StepError::Cert { .. }));
        assert!(matches!(classify_tls_err("h", mk(CertificateError::NotValidForName)), StepError::Cert { .. }));
        let other = io::Error::new(io::ErrorKind::InvalidData, rustls::Error::DecryptError);
        assert!(matches!(classify_tls_err("h", other), StepError::Unreachable(_)));
        assert!(matches!(
            classify_tls_err("h", io::Error::new(io::ErrorKind::ConnectionReset, "x")),
            StepError::Unreachable(_)
        ));
    }

    fn pair() -> (Conn, tokio::io::DuplexStream) {
        let (a, b) = duplex(4096);
        (BufReader::new(Box::new(a)), b)
    }

    async fn read_until_crlf(s: &mut tokio::io::DuplexStream) -> String {
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        while !out.ends_with(b"\r\n") {
            s.read_exact(&mut b).await.unwrap();
            out.push(b[0]);
        }
        String::from_utf8(out).unwrap().trim_end().to_string()
    }

    #[tokio::test]
    async fn imap_plain_second_username_succeeds() {
        let (mut c, mut s) = pair();
        let server = tokio::spawn(async move {
            assert_eq!(read_until_crlf(&mut s).await, "c1 CAPABILITY");
            s.write_all(b"* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\nc1 OK done\r\n").await.unwrap();
            assert_eq!(read_until_crlf(&mut s).await, "a1 AUTHENTICATE PLAIN");
            s.write_all(b"+ \r\n").await.unwrap();
            assert_eq!(read_until_crlf(&mut s).await, sasl_plain("bob@x.org", "pw"));
            s.write_all(b"a1 NO [AUTHENTICATIONFAILED] bad\r\n").await.unwrap();
            assert_eq!(read_until_crlf(&mut s).await, "a2 AUTHENTICATE PLAIN");
            s.write_all(b"+ \r\n").await.unwrap();
            assert_eq!(read_until_crlf(&mut s).await, sasl_plain("bob", "pw"));
            s.write_all(b"a2 OK logged in\r\n").await.unwrap();
        });
        let users = vec!["bob@x.org".to_string(), "bob".to_string()];
        assert_eq!(imap_login(&mut c, &users, "pw").await.unwrap(), "bob");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn imap_login_fallback_and_auth_failure() {
        let (mut c, mut s) = pair();
        let server = tokio::spawn(async move {
            read_until_crlf(&mut s).await;
            s.write_all(b"* CAPABILITY IMAP4rev1\r\nc1 OK\r\n").await.unwrap();
            assert_eq!(read_until_crlf(&mut s).await, r#"a1 LOGIN "bob@x.org" "p\"w""#);
            s.write_all(b"a1 NO IMAP access is disabled\r\n").await.unwrap();
        });
        let users = vec!["bob@x.org".to_string()];
        match imap_login(&mut c, &users, "p\"w").await {
            Err(StepError::Auth(m)) => assert!(m.contains("disabled")),
            o => panic!("{o:?}"),
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn smtp_auth_plain_and_login() {
        // PLAIN
        let (mut c, mut s) = pair();
        let server = tokio::spawn(async move {
            assert_eq!(read_until_crlf(&mut s).await, format!("AUTH PLAIN {}", sasl_plain("u@x.org", "pw")));
            s.write_all(b"235 2.7.0 ok\r\n").await.unwrap();
        });
        let caps = vec!["AUTH PLAIN LOGIN".to_string()];
        assert_eq!(smtp_auth(&mut c, &caps, &["u@x.org".into()], "pw").await.unwrap(), "u@x.org");
        server.await.unwrap();

        // LOGIN only, rejected
        let (mut c, mut s) = pair();
        let server = tokio::spawn(async move {
            assert_eq!(read_until_crlf(&mut s).await, "AUTH LOGIN");
            s.write_all(b"334 VXNlcm5hbWU6\r\n").await.unwrap();
            assert_eq!(read_until_crlf(&mut s).await, base64(b"u@x.org"));
            s.write_all(b"334 UGFzc3dvcmQ6\r\n").await.unwrap();
            assert_eq!(read_until_crlf(&mut s).await, base64(b"pw"));
            s.write_all(b"535 5.7.8 nope\r\n").await.unwrap();
        });
        let caps = vec!["AUTH=LOGIN".to_string()];
        assert!(matches!(
            smtp_auth(&mut c, &caps, &["u@x.org".into()], "pw").await,
            Err(StepError::Auth(m)) if m.contains("nope")
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn smtp_multiline_reply() {
        let (mut c, mut s) = pair();
        s.write_all(b"250-mail.x\r\n250-STARTTLS\r\n250 AUTH PLAIN\r\n").await.unwrap();
        let (code, lines) = smtp_reply(&mut c).await.unwrap();
        assert_eq!(code, 250);
        assert_eq!(lines, ["mail.x", "STARTTLS", "AUTH PLAIN"]);
    }

    fn det(provider: Option<&str>) -> DetectionResult {
        DetectionResult {
            email: "a@b.co".into(),
            domain: "b.co".into(),
            provider: provider.map(Into::into),
            imap: vec![],
            smtp: vec![],
            sources: vec![],
            elapsed_ms: 0,
        }
    }

    #[test]
    fn hints_and_error_messages() {
        let h = hints_for(&det(Some("zoho-us")), Stage::Imap);
        assert!(matches!(h[0], AuthHint::ZohoImapDisabled { .. }));
        assert!(matches!(h[1], AuthHint::AppPassword { .. }));
        assert_eq!(hints_for(&det(Some("zoho-us")), Stage::Smtp).len(), 1);
        assert!(hints_for(&det(Some("fastmail")), Stage::Imap).iter().all(|h| matches!(h, AuthHint::AppPassword { .. })));
        assert!(hints_for(&det(Some("migadu")), Stage::Imap).is_empty());
        assert!(hints_for(&det(None), Stage::Imap).is_empty());

        let e = VerifyError::AuthFailed { stage: Stage::Imap, server_message: "x".into(), hints: h };
        let m = e.user_message();
        assert!(m.contains("IMAP is off in Zoho") && m.contains("App Password"));
        let c = VerifyError::CertificateInvalid { host: "h.x".into(), reason: "expired".into() };
        assert!(c.user_message().contains("h.x") && c.user_message().contains("expired"));
        assert!(VerifyError::Unreachable { details: vec![] }.user_message().starts_with("Couldn't reach"));
    }

    #[test]
    fn username_order() {
        let mk = |h| Candidate { host: "h".into(), port: 993, tls: TlsMode::Implicit, source: Source::Srv, username_hint: h };
        assert_eq!(usernames(&mk(UsernameHint::Email), "a@b.co", "a"), ["a@b.co"]);
        assert_eq!(usernames(&mk(UsernameHint::LocalPartFallback), "a@b.co", "a"), ["a@b.co", "a"]);
    }

    #[test]
    fn errors_roundtrip_serde_shape() {
        // Compile-time check that the public types are Serialize + Deserialize.
        fn ok<T: serde::Serialize + serde::de::DeserializeOwned>() {}
        ok::<VerifyError>();
        ok::<crate::DetectError>();
        ok::<VerifiedSettings>();
        ok::<DetectionResult>();
    }

    // ------------------------------------------------ security-review regressions

    fn cand(host: &str, port: u16, source: Source) -> Candidate {
        Candidate { host: host.into(), port, tls: TlsMode::Implicit, source, username_hint: UsernameHint::Email }
    }

    fn det_with(provider: Option<&str>, domain: &str, imap: Vec<Candidate>) -> DetectionResult {
        DetectionResult {
            email: format!("a@{domain}"),
            domain: domain.into(),
            provider: provider.map(Into::into),
            imap,
            smtp: vec![],
            sources: vec![],
            elapsed_ms: 0,
        }
    }

    /// Run run_stage_with where every attempt is rejected; returns hosts tried.
    async fn reject_all(d: &DetectionResult) -> (Vec<String>, Result<String, VerifyError>) {
        let tried = Arc::new(Mutex::new(Vec::new()));
        let t2 = tried.clone();
        let r = run_stage_with(
            Stage::Imap,
            &d.imap,
            d,
            "a@x.org",
            "a",
            None,
            Instant::now() + Duration::from_secs(5),
            move |c, _u, _r| {
                t2.lock().unwrap().push(c.host.clone());
                async { Err(StepError::Auth("nope".into())) }
            },
        )
        .await
        .map(|(c, _)| c.host.clone());
        let v = tried.lock().unwrap().clone();
        (v, r)
    }

    #[tokio::test]
    async fn auth_failure_does_not_leak_password_to_other_hosts() {
        // Untrusted hosts: stop after the first rejection.
        let d = det_with(
            None,
            "example.com",
            vec![
                cand("imap.example.com", 993, Source::Autoconfig),
                cand("imap.attacker.net", 993, Source::Srv),
                cand("mail.example.com", 993, Source::Heuristic),
            ],
        );
        let (tried, r) = reject_all(&d).await;
        assert_eq!(tried, ["imap.example.com"]);
        assert!(matches!(r, Err(VerifyError::AuthFailed { .. })));

        // Bundled provider hosts of the same entry: Zoho pro then non-pro.
        let d = det_with(
            Some("zoho-us"),
            "acme.io",
            vec![cand("imappro.zoho.com", 993, Source::Mx), cand("imap.zoho.com", 993, Source::Mx)],
        );
        let (tried, r) = reject_all(&d).await;
        assert_eq!(tried, ["imappro.zoho.com", "imap.zoho.com"]);
        assert!(matches!(r, Err(VerifyError::AuthFailed { .. })));

        // Same-entry host rejected, then an SRV host: never sent.
        let d = det_with(
            Some("zoho-us"),
            "acme.io",
            vec![cand("imappro.zoho.com", 993, Source::Mx), cand("imap.acme.io", 993, Source::Srv)],
        );
        let (tried, _) = reject_all(&d).await;
        assert_eq!(tried, ["imappro.zoho.com"]);

        // A bundled-looking source with a host from another provider is not "same entry".
        let d = det_with(
            Some("zoho-us"),
            "acme.io",
            vec![cand("imappro.zoho.com", 993, Source::Mx), cand("imap.fastmail.com", 993, Source::Mx)],
        );
        let (tried, _) = reject_all(&d).await;
        assert_eq!(tried, ["imappro.zoho.com"]);
    }

    #[tokio::test]
    async fn unreachable_candidates_still_fall_through_before_any_rejection() {
        let d = det_with(None, "example.com", vec![cand("a.example.com", 993, Source::Srv), cand("b.example.com", 993, Source::Srv)]);
        let tried = Arc::new(Mutex::new(Vec::new()));
        let t2 = tried.clone();
        let r = run_stage_with(Stage::Imap, &d.imap, &d, "a@x.org", "a", None, Instant::now() + Duration::from_secs(5), move |c, _u, _r| {
            t2.lock().unwrap().push(c.host.clone());
            let ok = c.host == "b.example.com";
            async move { if ok { Ok("a@x.org".to_string()) } else { Err(StepError::Unreachable("down".into())) } }
        })
        .await
        .map(|(c, u)| (c.host.clone(), u));
        assert_eq!(r.unwrap().0, "b.example.com");
        assert_eq!(tried.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rejection_survives_unreachable_retry_at_stage_level() {
        let d = det_with(None, "example.com", vec![cand("a.example.com", 993, Source::Srv)]);
        let r = run_stage_with(Stage::Imap, &d.imap, &d, "a@x.org", "a", None, Instant::now() + Duration::from_secs(5), |_c, _u, rej| async move {
            note_rejection(&rej, "bad password");
            Err(StepError::Unreachable("connection closed".into()))
        })
        .await;
        assert!(matches!(r, Err(VerifyError::AuthFailed { server_message, .. }) if server_message == "bad password"));
    }

    #[tokio::test]
    async fn imap_hangup_after_rejection_is_auth_error() {
        for caps in ["* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\nc1 OK\r\n", "* CAPABILITY IMAP4rev1\r\nc1 OK\r\n"] {
            let (mut c, mut s) = pair();
            let plain = caps.contains("AUTH=PLAIN");
            let server = tokio::spawn(async move {
                read_until_crlf(&mut s).await;
                s.write_all(caps.as_bytes()).await.unwrap();
                read_until_crlf(&mut s).await; // a1 ...
                if plain {
                    s.write_all(b"+ \r\n").await.unwrap();
                    read_until_crlf(&mut s).await;
                }
                s.write_all(b"a1 NO [AUTHENTICATIONFAILED] bad\r\n").await.unwrap();
                read_until_crlf(&mut s).await; // second attempt, then BYE + hang up
                s.write_all(b"* BYE too many failures\r\n").await.unwrap();
            });
            let users = vec!["bob@x.org".to_string(), "bob".to_string()];
            match imap_login(&mut c, &users, "pw").await {
                Err(StepError::Auth(m)) => assert!(m.contains("bad"), "{m}"),
                o => panic!("{o:?}"),
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn smtp_421_or_hangup_after_rejection_is_auth_error() {
        for reply in [&b"421 4.7.0 too many failures\r\n"[..], &b""[..]] {
            let (mut c, mut s) = pair();
            let server = tokio::spawn(async move {
                read_until_crlf(&mut s).await;
                s.write_all(b"535 5.7.8 wrong\r\n").await.unwrap();
                read_until_crlf(&mut s).await;
                s.write_all(reply).await.unwrap();
            });
            let caps = vec!["AUTH PLAIN".to_string()];
            let users = vec!["bob@x.org".to_string(), "bob".to_string()];
            match smtp_auth(&mut c, &caps, &users, "pw").await {
                Err(StepError::Auth(m)) => assert!(m.contains("wrong"), "{m}"),
                o => panic!("{o:?}"),
            }
            server.await.unwrap();
        }
        // Without any prior rejection a 421 is still "unreachable".
        let (mut c, mut s) = pair();
        let server = tokio::spawn(async move {
            read_until_crlf(&mut s).await;
            s.write_all(b"421 go away\r\n").await.unwrap();
        });
        let caps = vec!["AUTH PLAIN".to_string()];
        assert!(matches!(smtp_auth(&mut c, &caps, &["bob@x.org".into()], "pw").await, Err(StepError::Unreachable(_))));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn read_step_times_out() {
        let (mut c, _s) = pair();
        let started = Instant::now();
        let r = read_line(&mut c).await;
        assert!(matches!(r, Err(StepError::Unreachable(m)) if m.contains("timed out")));
        assert!(started.elapsed() < STEP_TIMEOUT + Duration::from_millis(800));
    }

    #[tokio::test]
    async fn overall_timeout_keeps_per_candidate_details() {
        // Two local listeners that accept but never speak: TLS handshake stalls.
        let l1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let l2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (p1, p2) = (l1.local_addr().unwrap().port(), l2.local_addr().unwrap().port());
        let hold = tokio::spawn(async move {
            let mut keep = Vec::new();
            loop {
                tokio::select! {
                    Ok((s, _)) = l1.accept() => keep.push(s),
                    Ok((s, _)) = l2.accept() => keep.push(s),
                }
            }
        });
        let mut d = det_with(None, "example.com", vec![cand("127.0.0.1", p1, Source::Srv), cand("127.0.0.1", p2, Source::Srv)]);
        d.smtp = d.imap.clone();
        let started = std::time::Instant::now();
        let r = verify_with_budget(&d, "a@example.com", "pw", Duration::from_millis(800)).await;
        hold.abort();
        match r {
            Err(VerifyError::Unreachable { details }) => {
                assert_eq!(details.len(), 2, "{details:?}");
                assert!(details[0].contains(&p1.to_string()) && details[1].contains(&p2.to_string()), "{details:?}");
                assert!(details.iter().all(|d| d.contains("timed out")), "{details:?}");
            }
            o => panic!("{o:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
