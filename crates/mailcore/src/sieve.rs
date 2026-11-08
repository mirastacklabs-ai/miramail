//! ManageSieve (RFC 5804) client + vacation auto-responder script generator.
//!
//! Talks ONLY to the user's own mail server (default port 4190), over TLS (implicit, or STARTTLS before any
//! credential is sent; there is no plaintext fallback). The account password is used for one SASL PLAIN
//! exchange and never logged.
//!
//! Safety rules enforced here (and unit-tested against a fake server):
//! * Only a script named [`SCRIPT_NAME`] is ever written, and only if it is absent or carries our marker.
//! * Turning the responder off is `SETACTIVE ""` (or re-activating the script that was active before).
//!   There is no `DELETESCRIPT` anywhere in this module.
//! * Replacing another active script needs an explicit confirm from the caller.
//! * All user text is normalised and escaped as RFC 5228 quoted strings; sizes are capped.

use crate::{Error, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// The only script name MiraMail ever creates or activates.
pub const SCRIPT_NAME: &str = "miramail-vacation";
pub const DEFAULT_PORT: u16 = 4190;
const MARKER: &str = "# miramail-vacation v1";
const SETTINGS_PREFIX: &str = "# miramail-settings: ";
const PREVIOUS_PREFIX: &str = "# previous-active: ";

pub const MAX_SUBJECT: usize = 200;
pub const MAX_MESSAGE: usize = 4000;
pub const MAX_ADDRESSES: usize = 10;
pub const MAX_SCRIPT: usize = 32 * 1024;
/// Longest script name we will send (names come from the server's own LISTSCRIPTS).
const MAX_NAME: usize = 1024;
const MAX_LINE: usize = 16 * 1024;
const MAX_LITERAL: usize = 1 << 20;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
/// Whole-response limits: a hostile server cannot make one command read forever or buffer without bound.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_DATA_LINES: usize = 10_000;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------ public types

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SieveTls {
    /// TLS from the first byte.
    Implicit,
    /// Plain greeting, STARTTLS, then credentials. Refused if the server does not offer STARTTLS.
    StartTls,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SieveTarget {
    pub host: String,
    pub port: u16,
    pub tls: SieveTls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VacationSettings {
    pub subject: String,
    pub message: String,
    /// `YYYY-MM-DD`, inclusive; empty/None = no start limit.
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    pub days: u32,
    /// Reply only to mail addressed to one of `addresses`.
    pub only_to_me: bool,
    #[serde(default)]
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VacationStatus {
    /// The server has no ManageSieve / vacation support (or cannot be reached). `reason` is user-presentable.
    Unsupported { reason: String },
    #[serde(rename_all = "camelCase")]
    Ready {
        /// Our script is the active one (the responder is on).
        active: bool,
        /// Settings read back from our script on the server.
        settings: Option<VacationSettings>,
        /// Name of a different active script that turning the responder on would replace.
        other_active: Option<String>,
        /// Server supports the `date` + `relational` extensions (start/end dates).
        dates_supported: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnableOutcome {
    #[serde(rename_all = "camelCase")]
    Done { replaced: Option<String> },
    /// Nothing was changed: another script is active and the caller has not confirmed replacing it.
    #[serde(rename_all = "camelCase")]
    NeedsConfirm { active: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisableOutcome {
    /// The previously active script that was re-activated, if any.
    pub restored: Option<String>,
    /// False when our script was not active (nothing changed).
    pub was_active: bool,
}

// ------------------------------------------------------------------ settings validation + script generation

fn clean_line(s: &str, max: usize) -> String {
    let t: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(max).collect()
}

fn clean_body(s: &str, max: usize) -> String {
    let norm = s.replace("\r\n", "\n").replace('\r', "\n");
    let t: String = norm.chars().filter(|c| *c == '\n' || *c == '\t' || !c.is_control()).collect();
    t.trim().chars().take(max).collect()
}

fn valid_address(a: &str) -> bool {
    let Some((l, d)) = a.split_once('@') else { return false };
    a.len() <= 254
        && !l.is_empty()
        && !d.is_empty()
        && !d.contains('@')
        && a.chars().all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '<' | '>' | ',' | ';' | '(' | ')' | '[' | ']'))
}

/// `YYYY-MM-DD` with a real calendar date; returns the canonical string.
fn valid_date(s: &str) -> Result<String> {
    let s = s.trim();
    let bad = || Error::Invalid("Dates must look like 2026-12-24.".into());
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' || !s.char_indices().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()) {
        return Err(bad());
    }
    let (y, m, d): (u32, u32, u32) = (s[0..4].parse().map_err(|_| bad())?, s[5..7].parse().map_err(|_| bad())?, s[8..10].parse().map_err(|_| bad())?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(bad()),
    };
    if y < 2000 || d == 0 || d > dim {
        return Err(bad());
    }
    Ok(s.to_owned())
}

impl VacationSettings {
    /// Normalise and validate everything that will end up in the script.
    pub fn validated(&self) -> Result<VacationSettings> {
        let message = clean_body(&self.message, MAX_MESSAGE);
        if message.is_empty() {
            return Err(Error::Invalid("Write the message people should receive.".into()));
        }
        if !(1..=30).contains(&self.days) {
            return Err(Error::Invalid("Days between replies must be 1 to 30.".into()));
        }
        let opt_date = |d: &Option<String>| -> Result<Option<String>> {
            match d.as_deref().map(str::trim) {
                None | Some("") => Ok(None),
                Some(s) => valid_date(s).map(Some),
            }
        };
        let (start, end) = (opt_date(&self.start)?, opt_date(&self.end)?);
        if let (Some(s), Some(e)) = (&start, &end) {
            if e < s {
                return Err(Error::Invalid("The end date is before the start date.".into()));
            }
        }
        let mut addresses: Vec<String> = Vec::new();
        for a in &self.addresses {
            let a = a.trim().to_lowercase();
            if a.is_empty() || addresses.contains(&a) {
                continue;
            }
            if !valid_address(&a) {
                return Err(Error::Invalid("One of the addresses is not valid.".into()));
            }
            addresses.push(a);
        }
        if addresses.len() > MAX_ADDRESSES {
            return Err(Error::Invalid("Too many addresses.".into()));
        }
        if self.only_to_me && addresses.is_empty() {
            return Err(Error::Invalid("Add at least one of your addresses, or turn off \"only to my addresses\".".into()));
        }
        Ok(VacationSettings { subject: clean_line(&self.subject, MAX_SUBJECT), message, start, end, days: self.days, only_to_me: self.only_to_me, addresses })
    }
}

/// RFC 5228 quoted string. Input must already be normalised (no control chars except `\n`/`\t`).
fn sieve_quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\r\n"),
            c if c.is_control() && c != '\t' => {}
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Build the script text. `previous_active` is recorded in a comment so turning the responder off can restore it.
pub fn build_script(settings: &VacationSettings, previous_active: Option<&str>) -> Result<String> {
    let s = settings.validated()?;
    let mut out = String::new();
    out.push_str(MARKER);
    out.push_str("\r\n");
    out.push_str(SETTINGS_PREFIX);
    out.push_str(&serde_json::to_string(&s).map_err(|e| Error::Parse(e.to_string()))?);
    out.push_str("\r\n");
    if let Some(p) = previous_active {
        // The exact name, JSON-escaped so it survives as one comment line and compares byte for byte.
        if !p.is_empty() && p != SCRIPT_NAME && p.len() <= MAX_NAME && !p.chars().any(|c| c.is_control()) {
            out.push_str(PREVIOUS_PREFIX);
            out.push_str(&serde_json::to_string(p).map_err(|e| Error::Parse(e.to_string()))?);
            out.push_str("\r\n");
        }
    }
    let dated = s.start.is_some() || s.end.is_some();
    out.push_str(if dated { "require [\"vacation\", \"date\", \"relational\"];\r\n" } else { "require [\"vacation\"];\r\n" });
    let mut tests: Vec<String> = Vec::new();
    if let Some(d) = &s.start {
        tests.push(format!("currentdate :value \"ge\" \"date\" \"{d}\""));
    }
    if let Some(d) = &s.end {
        tests.push(format!("currentdate :value \"le\" \"date\" \"{d}\""));
    }
    let mut v = format!("vacation :days {}", s.days);
    if !s.subject.is_empty() {
        v.push_str(&format!(" :subject {}", sieve_quote(&s.subject)));
    }
    if s.only_to_me {
        let list: Vec<String> = s.addresses.iter().map(|a| sieve_quote(a)).collect();
        v.push_str(&format!(" :addresses [{}]", list.join(", ")));
    }
    v.push_str(&format!(" {};\r\n", sieve_quote(&s.message)));
    match tests.len() {
        0 => out.push_str(&v),
        1 => out.push_str(&format!("if {} {{\r\n{v}}}\r\n", tests[0])),
        _ => out.push_str(&format!("if allof({}) {{\r\n{v}}}\r\n", tests.join(", "))),
    }
    if out.len() > MAX_SCRIPT {
        return Err(Error::Invalid("The vacation message is too long.".into()));
    }
    Ok(out)
}

pub fn is_ours(script: &str) -> bool {
    script.starts_with(MARKER)
}

fn line_after<'a>(script: &'a str, prefix: &str) -> Option<&'a str> {
    script.lines().take(8).find_map(|l| l.strip_prefix(prefix))
}

/// Settings stored in our script's comment (untrusted: re-validated).
pub fn parse_settings(script: &str) -> Option<VacationSettings> {
    if !is_ours(script) {
        return None;
    }
    let raw = line_after(script, SETTINGS_PREFIX)?;
    serde_json::from_str::<VacationSettings>(raw).ok()?.validated().ok()
}

fn parse_previous(script: &str) -> Option<String> {
    if !is_ours(script) {
        return None;
    }
    let raw = line_after(script, PREVIOUS_PREFIX)?.trim_end_matches(['\r', '\n']);
    let name = if raw.starts_with('"') { serde_json::from_str::<String>(raw).ok()? } else { raw.trim().to_owned() }; // unquoted: older scripts
    Some(name).filter(|s| !s.is_empty())
}

// ------------------------------------------------------------------ wire parsing

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tok {
    Atom(String),
    Str(String),
}

/// Tokenise one physical line. A trailing `{N}` / `{N+}` is returned as the literal length.
pub(crate) fn tokenize(line: &str) -> Result<(Vec<Tok>, Option<usize>)> {
    let bad = |m: &str| Error::Parse(format!("managesieve: {m}"));
    let ch: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < ch.len() {
        match ch[i] {
            c if c.is_whitespace() => i += 1,
            '"' => {
                i += 1;
                let mut s = String::new();
                loop {
                    match ch.get(i) {
                        None => return Err(bad("unterminated string")),
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\\') => {
                            s.push(*ch.get(i + 1).ok_or_else(|| bad("bad escape"))?);
                            i += 2;
                        }
                        Some(c) => {
                            s.push(*c);
                            i += 1;
                        }
                    }
                }
                out.push(Tok::Str(s));
            }
            '{' => {
                let rest: String = ch[i + 1..].iter().collect();
                let body = rest.strip_suffix('}').ok_or_else(|| bad("bad literal"))?;
                let digits = body.strip_suffix('+').unwrap_or(body);
                let n: usize = digits.parse().map_err(|_| bad("bad literal length"))?;
                return Ok((out, Some(n)));
            }
            '(' | ')' => {
                out.push(Tok::Atom(ch[i].to_string()));
                i += 1;
            }
            _ => {
                let st = i;
                while i < ch.len() && !ch[i].is_whitespace() && !matches!(ch[i], '"' | '(' | ')') {
                    i += 1;
                }
                out.push(Tok::Atom(ch[st..i].iter().collect()));
            }
        }
    }
    Ok((out, None))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Ok,
    No,
    Bye,
}

#[derive(Debug, Clone)]
pub(crate) struct Status {
    pub kind: Kind,
    #[allow(dead_code)] // response code (e.g. QUOTA), kept for diagnostics
    pub code: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Response {
    pub data: Vec<Vec<Tok>>,
    pub status: Status,
}

impl Response {
    /// OK, or an error carrying the server's own explanation.
    fn ok(self) -> Result<Response> {
        match self.status.kind {
            Kind::Ok => Ok(self),
            Kind::No => Err(Error::Invalid(if self.status.text.is_empty() { "The server refused the request.".into() } else { format!("The server refused: {}", self.status.text) })),
            Kind::Bye => Err(Error::Invalid("The server closed the connection.".into())),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caps {
    pub sieve: Vec<String>,
    pub sasl: Vec<String>,
    pub starttls: bool,
}

impl Caps {
    pub fn has_ext(&self, e: &str) -> bool {
        self.sieve.iter().any(|x| x.eq_ignore_ascii_case(e))
    }
}

pub(crate) fn parse_caps(data: &[Vec<Tok>]) -> Caps {
    let mut c = Caps::default();
    for line in data {
        let Some(Tok::Str(k)) = line.first() else { continue };
        let second = match line.get(1) {
            Some(Tok::Str(v)) => v.as_str(),
            _ => "",
        };
        match k.to_ascii_uppercase().as_str() {
            "SIEVE" => c.sieve = second.split_whitespace().map(|s| s.to_ascii_lowercase()).collect(),
            "SASL" => c.sasl = second.split_whitespace().map(|s| s.to_ascii_uppercase()).collect(),
            "STARTTLS" => c.starttls = true,
            _ => {}
        }
    }
    c
}

// ------------------------------------------------------------------ session

pub(crate) struct Session<S> {
    r: BufReader<S>,
    /// Bytes still allowed for the response being read.
    budget: usize,
    pub caps: Caps,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    /// Wrap a stream and read the server's greeting (a capability list).
    pub(crate) async fn new(stream: S) -> Result<Self> {
        let mut s = Session { r: BufReader::new(stream), budget: MAX_RESPONSE_BYTES, caps: Caps::default() };
        let g = s.read_response().await?.ok()?;
        s.caps = parse_caps(&g.data);
        Ok(s)
    }

    async fn read_line(&mut self) -> Result<String> {
        let mut buf = Vec::new();
        let n = timeout(IO_TIMEOUT, (&mut self.r).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf))
            .await
            .map_err(|_| Error::Timeout("managesieve read"))??;
        if n == 0 {
            return Err(Error::Invalid("The server closed the connection.".into()));
        }
        if buf.last() != Some(&b'\n') {
            return Err(Error::Parse("managesieve: line too long".into()));
        }
        self.spend(n)?;
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    fn spend(&mut self, n: usize) -> Result<()> {
        self.budget = self.budget.checked_sub(n).ok_or_else(|| Error::Parse("managesieve: response too large".into()))?;
        Ok(())
    }

    /// One logical line: tokens, with literals read and spliced in as `Tok::Str`.
    async fn read_logical(&mut self) -> Result<Vec<Tok>> {
        let mut toks = Vec::new();
        loop {
            let line = self.read_line().await?;
            let (t, lit) = tokenize(&line)?;
            toks.extend(t);
            let Some(n) = lit else { return Ok(toks) };
            if n > MAX_LITERAL {
                return Err(Error::Parse("managesieve: literal too large".into()));
            }
            self.spend(n)?;
            let mut body = vec![0u8; n];
            timeout(IO_TIMEOUT, self.r.read_exact(&mut body)).await.map_err(|_| Error::Timeout("managesieve read"))??;
            toks.push(Tok::Str(String::from_utf8_lossy(&body).into_owned()));
        }
    }

    pub(crate) async fn read_response(&mut self) -> Result<Response> {
        self.budget = MAX_RESPONSE_BYTES;
        timeout(RESPONSE_TIMEOUT, self.read_response_inner()).await.map_err(|_| Error::Timeout("managesieve response"))?
    }

    async fn read_response_inner(&mut self) -> Result<Response> {
        let mut data = Vec::new();
        loop {
            let toks = self.read_logical().await?;
            let kind = match toks.first() {
                Some(Tok::Atom(a)) if a.eq_ignore_ascii_case("OK") => Some(Kind::Ok),
                Some(Tok::Atom(a)) if a.eq_ignore_ascii_case("NO") => Some(Kind::No),
                Some(Tok::Atom(a)) if a.eq_ignore_ascii_case("BYE") => Some(Kind::Bye),
                _ => None,
            };
            let Some(kind) = kind else {
                if !toks.is_empty() {
                    if data.len() >= MAX_DATA_LINES {
                        return Err(Error::Parse("managesieve: response too long".into()));
                    }
                    data.push(toks);
                }
                continue;
            };
            let mut code = Vec::new();
            let mut depth = 0;
            let mut text = String::new();
            for t in &toks[1..] {
                match t {
                    Tok::Atom(a) if a == "(" => depth += 1,
                    Tok::Atom(a) if a == ")" => depth -= 1,
                    Tok::Atom(a) if depth > 0 => code.push(a.clone()),
                    Tok::Str(s) if depth > 0 => code.push(s.clone()),
                    Tok::Str(s) => text = s.clone(),
                    Tok::Atom(_) => {}
                }
            }
            let code = if code.is_empty() { None } else { Some(code.join(" ")) };
            let text: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
            return Ok(Response { data, status: Status { kind, code, text } });
        }
    }

    async fn send(&mut self, bytes: &[u8]) -> Result<()> {
        let w = self.r.get_mut();
        timeout(IO_TIMEOUT, async {
            w.write_all(bytes).await?;
            w.flush().await
        })
        .await
        .map_err(|_| Error::Timeout("managesieve write"))??;
        Ok(())
    }

    async fn cmd(&mut self, line: &str) -> Result<Response> {
        self.send(format!("{line}\r\n").as_bytes()).await?;
        self.read_response().await
    }

    /// SASL PLAIN. Credentials only ever go out over a TLS stream (callers guarantee this).
    pub(crate) async fn authenticate(&mut self, user: &str, password: &str) -> Result<()> {
        if !self.caps.sasl.iter().any(|m| m == "PLAIN") {
            return Err(Error::Invalid("The server does not offer a sign-in method MiraMail can use here.".into()));
        }
        let raw = format!("\0{user}\0{password}");
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
        let r = self.cmd(&format!("AUTHENTICATE \"PLAIN\" \"{b64}\"")).await?;
        match r.status.kind {
            Kind::Ok => Ok(()),
            Kind::No => Err(Error::AuthFailed),
            Kind::Bye => Err(Error::Invalid("The server closed the connection.".into())),
        }
    }

    pub(crate) async fn list_scripts(&mut self) -> Result<Vec<(String, bool)>> {
        let r = self.cmd("LISTSCRIPTS").await?.ok()?;
        Ok(r.data
            .iter()
            .filter_map(|l| match l.first() {
                Some(Tok::Str(n)) => Some((n.clone(), matches!(l.get(1), Some(Tok::Atom(a)) if a.eq_ignore_ascii_case("ACTIVE")))),
                _ => None,
            })
            .collect())
    }

    pub(crate) async fn get_script(&mut self, name: &str) -> Result<String> {
        let r = self.cmd(&format!("GETSCRIPT {}", name_arg(name)?)).await?.ok()?;
        match r.data.first().and_then(|l| l.first()) {
            Some(Tok::Str(s)) => Ok(s.clone()),
            _ => Err(Error::Parse("managesieve: missing script body".into())),
        }
    }

    async fn script_cmd(&mut self, verb: &str, name: Option<&str>, body: &str) -> Result<()> {
        let mut msg = String::from(verb);
        if let Some(n) = name {
            msg.push(' ');
            msg.push_str(&name_arg(n)?);
        }
        msg.push_str(&format!(" {{{}+}}\r\n{body}\r\n", body.len()));
        self.send(msg.as_bytes()).await?;
        self.read_response().await?.ok().map(|_| ())
    }

    pub(crate) async fn check_script(&mut self, body: &str) -> Result<()> {
        self.script_cmd("CHECKSCRIPT", None, body).await
    }

    pub(crate) async fn put_script(&mut self, name: &str, body: &str) -> Result<()> {
        self.script_cmd("PUTSCRIPT", Some(name), body).await
    }

    /// Empty name deactivates all scripts.
    pub(crate) async fn set_active(&mut self, name: &str) -> Result<()> {
        let arg = if name.is_empty() { "\"\"".to_owned() } else { name_arg(name)? };
        self.cmd(&format!("SETACTIVE {arg}")).await?.ok().map(|_| ())
    }

    pub(crate) async fn logout(&mut self) {
        let _ = self.send(b"LOGOUT\r\n").await;
    }

    pub(crate) fn into_inner(self) -> Result<S> {
        if !self.r.buffer().is_empty() {
            return Err(Error::Tls("unexpected data before the TLS handshake".into()));
        }
        Ok(self.r.into_inner())
    }
}

/// Quoted script name. Names we send come from LISTSCRIPTS or are [`SCRIPT_NAME`]; reject anything odd anyway.
fn name_arg(name: &str) -> Result<String> {
    if name.is_empty() || name.len() > MAX_NAME || name.chars().any(|c| c.is_control()) {
        return Err(Error::Invalid("Invalid script name.".into()));
    }
    Ok(sieve_quote(name))
}

// ------------------------------------------------------------------ operations on an authenticated session

pub(crate) async fn status_on<S: AsyncRead + AsyncWrite + Unpin>(s: &mut Session<S>) -> Result<VacationStatus> {
    if !s.caps.has_ext("vacation") {
        return Ok(VacationStatus::Unsupported { reason: UNSUPPORTED.into() });
    }
    let scripts = s.list_scripts().await?;
    let ours = scripts.iter().find(|(n, _)| n == SCRIPT_NAME);
    let active = ours.is_some_and(|(_, a)| *a);
    let other_active = scripts.iter().find(|(n, a)| *a && n != SCRIPT_NAME).map(|(n, _)| n.clone());
    let settings = if ours.is_some() {
        let body = s.get_script(SCRIPT_NAME).await?;
        parse_settings(&body)
    } else {
        None
    };
    Ok(VacationStatus::Ready { active, settings, other_active, dates_supported: s.caps.has_ext("date") && s.caps.has_ext("relational") })
}

pub(crate) async fn enable_on<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut Session<S>,
    settings: &VacationSettings,
    confirm_replace: bool,
) -> Result<EnableOutcome> {
    if !s.caps.has_ext("vacation") {
        return Err(Error::Invalid(UNSUPPORTED.into()));
    }
    let settings = settings.validated()?;
    if (settings.start.is_some() || settings.end.is_some()) && !(s.caps.has_ext("date") && s.caps.has_ext("relational")) {
        return Err(Error::Invalid("This server cannot limit the reply to dates. Clear the start and end dates, or use webmail.".into()));
    }
    let scripts = s.list_scripts().await?;
    let other_active = scripts.iter().find(|(n, a)| *a && n != SCRIPT_NAME).map(|(n, _)| n.clone());
    if let Some(a) = &other_active {
        if !confirm_replace {
            return Ok(EnableOutcome::NeedsConfirm { active: a.clone() });
        }
    }
    let mut previous = other_active.clone();
    if scripts.iter().any(|(n, _)| n == SCRIPT_NAME) {
        let existing = s.get_script(SCRIPT_NAME).await?;
        if !is_ours(&existing) {
            return Err(Error::Invalid(format!("A script named \"{SCRIPT_NAME}\" already exists on the server and was not made by MiraMail, so it was left alone.")));
        }
        // Carry the remembered script forward only while OUR script is the active one (a plain re-save). If it was
        // turned off, whatever the user left off (or switched on since) is not ours to bring back later.
        let ours_active = scripts.iter().any(|(n, a)| *a && n == SCRIPT_NAME);
        if previous.is_none() && ours_active {
            previous = parse_previous(&existing);
        }
    }
    let body = build_script(&settings, previous.as_deref())?;
    s.check_script(&body).await?;
    s.put_script(SCRIPT_NAME, &body).await?;
    s.set_active(SCRIPT_NAME).await?;
    Ok(EnableOutcome::Done { replaced: other_active })
}

pub(crate) async fn disable_on<S: AsyncRead + AsyncWrite + Unpin>(s: &mut Session<S>, restore_previous: bool) -> Result<DisableOutcome> {
    let scripts = s.list_scripts().await?;
    let Some((_, active)) = scripts.iter().find(|(n, _)| n == SCRIPT_NAME) else {
        return Ok(DisableOutcome { restored: None, was_active: false });
    };
    if !*active {
        return Ok(DisableOutcome { restored: None, was_active: false });
    }
    let mut restored = None;
    if restore_previous {
        // Best effort: any failure to restore falls through to plain deactivation, so turning off never fails here.
        if let Ok(body) = s.get_script(SCRIPT_NAME).await {
            if let Some(prev) = parse_previous(&body) {
                if prev != SCRIPT_NAME && scripts.iter().any(|(n, _)| *n == prev) && s.set_active(&prev).await.is_ok() {
                    restored = Some(prev);
                }
            }
        }
    }
    if restored.is_none() {
        s.set_active("").await?;
    }
    Ok(DisableOutcome { restored, was_active: true })
}

const UNSUPPORTED: &str = "Your mail server does not offer automatic replies to MiraMail (no ManageSieve vacation support). Set the vacation reply up in your provider's webmail instead.";

// ------------------------------------------------------------------ connecting

fn check_target(t: &SieveTarget) -> Result<()> {
    if t.host.is_empty() || t.host.len() > 253 || !t.host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')) {
        return Err(Error::Invalid("Invalid server name.".into()));
    }
    if t.port == 0 {
        return Err(Error::Invalid("Invalid port.".into()));
    }
    Ok(())
}

type Boxed = Box<dyn AsyncReadWrite>;
trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// Connect, secure the channel, authenticate. Never sends credentials unencrypted.
async fn open(t: &SieveTarget, user: &str, password: &str) -> Result<Session<Boxed>> {
    check_target(t)?;
    let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((t.host.as_str(), t.port)))
        .await
        .map_err(|_| Error::Timeout("managesieve connect"))??;
    let _ = tcp.set_nodelay(true);
    let mut sess: Session<Boxed> = match t.tls {
        SieveTls::Implicit => {
            let tls = crate::imap::handshake(tcp, &t.host).await?;
            Session::new(Box::new(tls) as Boxed).await?
        }
        SieveTls::StartTls => {
            let mut plain = Session::new(tcp).await?;
            if !plain.caps.starttls {
                return Err(Error::Tls("The server does not offer STARTTLS for ManageSieve, so MiraMail will not send your password to it.".into()));
            }
            plain.cmd("STARTTLS").await?.ok()?;
            let tcp = plain.into_inner()?;
            let tls = crate::imap::handshake(tcp, &t.host).await?;
            // After STARTTLS the server re-sends its capabilities.
            Session::new(Box::new(tls) as Boxed).await?
        }
    };
    // Read the capability list BEFORE authenticating: a server that cannot do vacation never sees the password.
    require_vacation(&sess.caps)?;
    sess.authenticate(user, password).await?;
    // Capabilities may change after authentication; the greeting ones are what we rely on.
    Ok(sess)
}

fn require_vacation(caps: &Caps) -> Result<()> {
    if caps.has_ext("vacation") {
        Ok(())
    } else {
        Err(Error::Invalid(UNSUPPORTED.into()))
    }
}

fn unreachable_reason(e: &Error) -> String {
    match e {
        Error::AuthFailed => "The server did not accept the saved password for this account.".into(),
        Error::Io(_) | Error::Timeout(_) | Error::Tls(_) => format!("{UNSUPPORTED} (Could not connect on the ManageSieve port; it may be closed.)"),
        _ => UNSUPPORTED.into(),
    }
}

/// Probe + read current state. Connection problems are reported as `Unsupported` with a friendly reason, never as an error.
pub async fn vacation_status(t: &SieveTarget, user: &str, password: &str) -> Result<VacationStatus> {
    match open(t, user, password).await {
        Err(Error::Invalid(m)) if m.starts_with("Invalid ") => Err(Error::Invalid(m)),
        Err(e) => Ok(VacationStatus::Unsupported { reason: unreachable_reason(&e) }),
        Ok(mut s) => {
            let r = status_on(&mut s).await;
            s.logout().await;
            // Past sign-in only a lost connection means "unavailable"; anything else is a real error to surface.
            match r {
                Ok(v) => Ok(v),
                Err(e @ (Error::Io(_) | Error::Timeout(_) | Error::Tls(_))) => Ok(VacationStatus::Unsupported { reason: unreachable_reason(&e) }),
                Err(e) => Err(e),
            }
        }
    }
}

pub async fn vacation_enable(t: &SieveTarget, user: &str, password: &str, settings: &VacationSettings, confirm_replace: bool) -> Result<EnableOutcome> {
    settings.validated()?;
    let mut s = open(t, user, password).await?;
    let r = enable_on(&mut s, settings, confirm_replace).await;
    s.logout().await;
    r
}

pub async fn vacation_disable(t: &SieveTarget, user: &str, password: &str, restore_previous: bool) -> Result<DisableOutcome> {
    let mut s = open(t, user, password).await?;
    let r = disable_on(&mut s, restore_previous).await;
    s.logout().await;
    r
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{duplex, DuplexStream};

    const CAPS: &str = "\"IMPLEMENTATION\" \"Fake\"\r\n\"SASL\" \"PLAIN LOGIN\"\r\n\"SIEVE\" \"fileinto vacation date relational\"\r\n\"STARTTLS\"\r\nOK \"ready\"\r\n";

    /// Fake server: `handler(command_line, literal_body) -> response bytes`; every command is logged.
    fn spawn_server<F>(stream: DuplexStream, greeting: String, mut handler: F) -> Arc<Mutex<Vec<String>>>
    where
        F: FnMut(&str, Option<&str>) -> String + Send + 'static,
    {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(stream);
            r.get_mut().write_all(greeting.as_bytes()).await.unwrap();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let line = line.trim_end().to_owned();
                let (_, lit) = tokenize(&line).unwrap();
                let body = if let Some(n) = lit {
                    let mut b = vec![0u8; n + 2];
                    r.read_exact(&mut b).await.unwrap();
                    Some(String::from_utf8(b[..n].to_vec()).unwrap())
                } else {
                    None
                };
                l2.lock().unwrap().push(match &body {
                    Some(b) => format!("{line}\n{b}"),
                    None => line.clone(),
                });
                if line.eq_ignore_ascii_case("LOGOUT") {
                    return;
                }
                let resp = handler(&line, body.as_deref());
                r.get_mut().write_all(resp.as_bytes()).await.unwrap();
            }
        });
        log
    }

    /// In-memory ManageSieve store.
    #[derive(Default, Clone)]
    struct Store {
        scripts: Vec<(String, String)>,
        active: Option<String>,
    }

    fn store_handler(st: Arc<Mutex<Store>>) -> impl FnMut(&str, Option<&str>) -> String + Send + 'static {
        move |line, body| {
            let (toks, _) = tokenize(line).unwrap();
            let verb = match &toks[0] {
                Tok::Atom(a) => a.to_ascii_uppercase(),
                _ => panic!(),
            };
            let name = toks.get(1).and_then(|t| if let Tok::Str(s) = t { Some(s.clone()) } else { None });
            let mut s = st.lock().unwrap();
            match verb.as_str() {
                "LISTSCRIPTS" => {
                    let mut o = String::new();
                    for (n, _) in &s.scripts {
                        o.push_str(&format!("\"{n}\"{}\r\n", if s.active.as_deref() == Some(n) { " ACTIVE" } else { "" }));
                    }
                    o + "OK\r\n"
                }
                "GETSCRIPT" => match s.scripts.iter().find(|(n, _)| Some(n) == name.as_ref()) {
                    Some((_, c)) => format!("{{{}}}\r\n{c}\r\nOK\r\n", c.len()),
                    None => "NO (NONEXISTENT) \"no such script\"\r\n".into(),
                },
                "CHECKSCRIPT" => {
                    if body.unwrap().contains("BADSYNTAX") {
                        "NO \"line 3: syntax error\"\r\n".into()
                    } else {
                        "OK\r\n".into()
                    }
                }
                "PUTSCRIPT" => {
                    let n = name.unwrap();
                    s.scripts.retain(|(x, _)| *x != n);
                    s.scripts.push((n, body.unwrap().to_owned()));
                    "OK\r\n".into()
                }
                "SETACTIVE" => {
                    let n = name.unwrap();
                    if n.is_empty() {
                        s.active = None;
                        "OK\r\n".into()
                    } else if s.scripts.iter().any(|(x, _)| *x == n) {
                        s.active = Some(n);
                        "OK\r\n".into()
                    } else {
                        "NO \"nope\"\r\n".into()
                    }
                }
                "DELETESCRIPT" => "OK\r\n".into(),
                _ => "NO \"unknown\"\r\n".into(),
            }
        }
    }

    fn settings() -> VacationSettings {
        VacationSettings {
            subject: "Away".into(),
            message: "I am away.\nBack soon.".into(),
            start: None,
            end: None,
            days: 7,
            only_to_me: true,
            addresses: vec!["me@example.org".into()],
        }
    }

    async fn session_with(st: Arc<Mutex<Store>>, caps: &str) -> (Session<DuplexStream>, Arc<Mutex<Vec<String>>>) {
        let (a, b) = duplex(1 << 16);
        let log = spawn_server(b, caps.to_owned(), store_handler(st));
        (Session::new(a).await.unwrap(), log)
    }

    fn foreign_store() -> Arc<Mutex<Store>> {
        Arc::new(Mutex::new(Store { scripts: vec![("main".into(), "require \"fileinto\";".into())], active: Some("main".into()) }))
    }

    #[test]
    fn tokenizer_handles_quotes_escapes_atoms_and_literals() {
        let (t, l) = tokenize("\"a\\\"b\\\\c\" ACTIVE (QUOTA/MAXSIZE) \"x\"").unwrap();
        assert_eq!(l, None);
        assert_eq!(t[0], Tok::Str("a\"b\\c".into()));
        assert_eq!(t[1], Tok::Atom("ACTIVE".into()));
        assert_eq!(t[2], Tok::Atom("(".into()));
        let (t, l) = tokenize("\"PUTSCRIPT\" {12+}").unwrap();
        assert_eq!((t.len(), l), (1, Some(12)));
        assert_eq!(tokenize("{5}").unwrap().1, Some(5));
        assert!(tokenize("\"unterminated").is_err());
        assert!(tokenize("{abc}").is_err());
    }

    #[tokio::test]
    async fn capability_parsing_including_literals() {
        let g = "\"IMPLEMENTATION\" \"Dovecot Pigeonhole\"\r\n\"SIEVE\" {33}\r\nfileinto vacation Date RELATIONAL\r\n\"SASL\" \"PLAIN\"\r\n\"STARTTLS\"\r\n\"VERSION\" \"1.0\"\r\nOK \"Dovecot ready.\"\r\n";
        let (a, b) = duplex(4096);
        spawn_server(b, g.to_owned(), |_, _| "OK\r\n".into());
        let s = Session::new(a).await.unwrap();
        assert!(s.caps.has_ext("vacation") && s.caps.has_ext("date") && s.caps.has_ext("relational"));
        assert!(!s.caps.has_ext("imap4flags"));
        assert_eq!(s.caps.sasl, vec!["PLAIN"]);
        assert!(s.caps.starttls);
    }

    #[tokio::test]
    async fn no_and_bye_responses_become_errors() {
        let (a, b) = duplex(4096);
        spawn_server(b, CAPS.into(), |l, _| if l.starts_with("LISTSCRIPTS") { "NO (QUOTA) \"mailbox over quota\"\r\n".into() } else { "BYE \"shutting down\"\r\n".into() });
        let mut s = Session::new(a).await.unwrap();
        let e = s.list_scripts().await.unwrap_err().to_string();
        assert!(e.contains("mailbox over quota"), "{e}");
        let e = s.get_script("x").await.unwrap_err().to_string();
        assert!(e.contains("closed the connection"), "{e}");
    }

    #[tokio::test]
    async fn bad_greeting_and_auth() {
        let (a, b) = duplex(4096);
        spawn_server(b, "BYE \"go away\"\r\n".into(), |_, _| "OK\r\n".into());
        assert!(Session::new(a).await.is_err());

        let (a, b) = duplex(4096);
        let log = spawn_server(b, CAPS.into(), |l, _| if l.contains("AAB") || l.contains("AHVzZXIAcHc") { "OK\r\n".into() } else { "NO\r\n".into() });
        let mut s = Session::new(a).await.unwrap();
        s.authenticate("user", "pw").await.unwrap();
        assert!(log.lock().unwrap()[0].starts_with("AUTHENTICATE \"PLAIN\" \"AHVzZXIAcHc"));
    }

    #[tokio::test]
    async fn auth_needs_plain_and_maps_no_to_auth_failed() {
        let (a, b) = duplex(4096);
        spawn_server(b, "\"SASL\" \"GSSAPI\"\r\n\"SIEVE\" \"vacation\"\r\nOK\r\n".into(), |_, _| "OK\r\n".into());
        let mut s = Session::new(a).await.unwrap();
        assert!(s.authenticate("u", "p").await.is_err());
        let (a, b) = duplex(4096);
        spawn_server(b, CAPS.into(), |_, _| "NO \"auth failed\"\r\n".into());
        let mut s = Session::new(a).await.unwrap();
        assert!(matches!(s.authenticate("u", "p").await, Err(Error::AuthFailed)));
    }

    /// Strip quoted strings and comments so we can see the structural tokens of a script.
    fn skeleton(script: &str) -> String {
        let mut out = String::new();
        let mut it = script.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '"' => {
                    while let Some(d) = it.next() {
                        if d == '\\' {
                            it.next();
                        } else if d == '"' {
                            break;
                        }
                    }
                    out.push('S');
                }
                '#' => {
                    for d in it.by_ref() {
                        if d == '\n' {
                            break;
                        }
                    }
                    out.push('\n');
                }
                c => out.push(c),
            }
        }
        out
    }

    /// Extract the raw (unescaped) contents of all quoted strings, ignoring comments.
    fn strings(script: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut it = script.chars();
        while let Some(c) = it.next() {
            if c == '#' {
                for d in it.by_ref() {
                    if d == '\n' {
                        break;
                    }
                }
            } else if c == '"' {
                let mut s = String::new();
                while let Some(d) = it.next() {
                    match d {
                        '\\' => s.push(it.next().unwrap()),
                        '"' => break,
                        d => s.push(d),
                    }
                }
                out.push(s);
            }
        }
        out
    }

    #[test]
    fn injection_attempts_stay_inside_strings() {
        let mut s = settings();
        s.subject = "Hi\"; discard; \"\r\nrequire [\"fileinto\"]; fileinto \"x".into();
        s.message = "line1\r\n\"; redirect \"evil@x.org\"; \\\" \\\\ end\u{0}\u{7}\rCR".into();
        s.addresses = vec!["me@example.org".into()];
        let script = build_script(&s, Some("old\"; discard;")).unwrap();
        let sk = skeleton(&script);
        for bad in ["discard", "redirect", "fileinto", "evil"] {
            assert!(!sk.contains(bad), "{bad} leaked outside a string:\n{script}");
        }
        assert_eq!(sk.matches("vacation").count(), 1, "{sk}"); // the action ("vacation" in require is a string)
        assert_eq!(sk.matches("require").count(), 1);
        let strs = strings(&script);
        // require "vacation", subject, address, body
        assert_eq!(strs.len(), 4, "{strs:?}");
        assert_eq!(strs[1], "Hi\"; discard; \" require [\"fileinto\"]; fileinto \"x"); // CR/LF collapsed in the subject
        assert!(strs[3].starts_with("line1\r\n\"; redirect"));
        assert!(!strs[3].contains('\u{0}') && !strs[3].contains('\u{7}'));
        // the previous-active comment cannot start a new line
        assert!(script.lines().filter(|l| l.starts_with("# previous-active:")).count() == 1);
        // settings round-trip through the comment
        let back = parse_settings(&script).unwrap();
        assert_eq!(back.subject, strs[1]);
    }

    #[test]
    fn comment_cannot_be_broken_out_of() {
        let mut s = settings();
        s.message = "x\r\nrequire [\"fileinto\"]; fileinto \"y\";".into();
        let script = build_script(&s, None).unwrap();
        assert!(!skeleton(&script).contains("fileinto"));
        // JSON comment is one physical line
        assert_eq!(script.lines().filter(|l| l.starts_with("# miramail-settings: ")).count(), 1);
        assert!(script.lines().take(2).all(|l| l.starts_with('#')));
    }

    #[test]
    fn validation_rules() {
        let ok = settings();
        assert!(ok.validated().is_ok());
        let mut x = ok.clone();
        x.message = "   \r\n ".into();
        assert!(x.validated().is_err());
        let mut x = ok.clone();
        x.days = 0;
        assert!(x.validated().is_err());
        x.days = 31;
        assert!(x.validated().is_err());
        let mut x = ok.clone();
        x.addresses = vec!["a\"b@x.org".into()];
        assert!(x.validated().is_err());
        x.addresses = vec!["no-at".into()];
        assert!(x.validated().is_err());
        x.addresses = vec![];
        assert!(x.validated().is_err()); // only_to_me with nothing
        x.only_to_me = false;
        assert!(x.validated().is_ok());
        let mut x = ok.clone();
        x.start = Some("2026-02-30".into());
        assert!(x.validated().is_err());
        x.start = Some("2026-12-20".into());
        x.end = Some("2026-12-01".into());
        assert!(x.validated().is_err());
        x.end = Some("2027-01-02".into());
        assert!(x.validated().is_ok());
        let mut x = ok.clone();
        x.message = "a".repeat(MAX_MESSAGE * 3);
        x.subject = "s".repeat(1000);
        let v = x.validated().unwrap();
        assert_eq!(v.message.chars().count(), MAX_MESSAGE);
        assert_eq!(v.subject.chars().count(), MAX_SUBJECT);
        assert!(build_script(&x, None).unwrap().len() <= MAX_SCRIPT);
    }

    #[test]
    fn dated_script_uses_date_extension() {
        let mut s = settings();
        s.start = Some("2026-12-20".into());
        s.end = Some("2027-01-02".into());
        let sc = build_script(&s, None).unwrap();
        assert!(sc.contains("require [\"vacation\", \"date\", \"relational\"];"));
        assert!(sc.contains("allof(currentdate :value \"ge\" \"date\" \"2026-12-20\", currentdate :value \"le\" \"date\" \"2027-01-02\")"));
        s.end = None;
        assert!(build_script(&s, None).unwrap().contains("if currentdate :value \"ge\""));
        s.start = None;
        let plain = build_script(&s, None).unwrap();
        assert!(!plain.contains("currentdate") && plain.contains("vacation :days 7"));
    }

    #[tokio::test]
    async fn unsupported_server_is_reported_not_failed() {
        let st = Arc::new(Mutex::new(Store::default()));
        let (mut s, log) = session_with(st, "\"SIEVE\" \"fileinto\"\r\n\"SASL\" \"PLAIN\"\r\nOK\r\n").await;
        assert!(matches!(status_on(&mut s).await.unwrap(), VacationStatus::Unsupported { .. }));
        assert!(enable_on(&mut s, &settings(), true).await.is_err());
        assert!(log.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn enable_requires_confirm_then_replaces_and_disable_restores() {
        let st = foreign_store();
        let (mut s, log) = session_with(st.clone(), CAPS).await;
        // status: another script active
        match status_on(&mut s).await.unwrap() {
            VacationStatus::Ready { active, other_active, settings, dates_supported } => {
                assert!(!active && settings.is_none() && dates_supported);
                assert_eq!(other_active.as_deref(), Some("main"));
            }
            o => panic!("{o:?}"),
        }
        // not confirmed: nothing written
        let r = enable_on(&mut s, &settings(), false).await.unwrap();
        assert_eq!(r, EnableOutcome::NeedsConfirm { active: "main".into() });
        assert!(log.lock().unwrap().iter().all(|c| !c.starts_with("PUTSCRIPT") && !c.starts_with("SETACTIVE")));
        assert_eq!(st.lock().unwrap().active.as_deref(), Some("main"));
        // confirmed
        let r = enable_on(&mut s, &settings(), true).await.unwrap();
        assert_eq!(r, EnableOutcome::Done { replaced: Some("main".into()) });
        {
            let g = st.lock().unwrap();
            assert_eq!(g.active.as_deref(), Some(SCRIPT_NAME));
            assert_eq!(g.scripts.len(), 2);
            let ours = &g.scripts.iter().find(|(n, _)| n == SCRIPT_NAME).unwrap().1;
            assert!(ours.contains("# previous-active: \"main\""));
            assert_eq!(g.scripts.iter().find(|(n, _)| n == "main").unwrap().1, "require \"fileinto\";"); // untouched
        }
        // CHECKSCRIPT came before PUTSCRIPT before SETACTIVE
        {
            let l = log.lock().unwrap();
            let pos = |p: &str| l.iter().position(|c| c.starts_with(p)).unwrap();
            assert!(pos("CHECKSCRIPT") < pos("PUTSCRIPT") && pos("PUTSCRIPT") < pos("SETACTIVE"));
        }
        // re-saving while active keeps the memory of "main" and needs no confirm
        let mut again = settings();
        again.message = "Updated".into();
        let r = enable_on(&mut s, &again, false).await.unwrap();
        assert_eq!(r, EnableOutcome::Done { replaced: None });
        assert!(st.lock().unwrap().scripts.iter().find(|(n, _)| n == SCRIPT_NAME).unwrap().1.contains("# previous-active: \"main\""));
        // status reads settings back
        match status_on(&mut s).await.unwrap() {
            VacationStatus::Ready { active, settings, other_active, .. } => {
                assert!(active && other_active.is_none());
                assert_eq!(settings.unwrap().message, "Updated");
            }
            o => panic!("{o:?}"),
        }
        // disable with restore
        let d = disable_on(&mut s, true).await.unwrap();
        assert_eq!(d, DisableOutcome { restored: Some("main".into()), was_active: true });
        assert_eq!(st.lock().unwrap().active.as_deref(), Some("main"));
        // never deleted anything
        assert!(log.lock().unwrap().iter().all(|c| !c.contains("DELETESCRIPT")));
        assert_eq!(st.lock().unwrap().scripts.len(), 2);
        // disabling again is a no-op
        let d = disable_on(&mut s, true).await.unwrap();
        assert!(!d.was_active);
    }

    #[tokio::test]
    async fn disable_without_restore_deactivates_with_empty_name() {
        let st = foreign_store();
        let (mut s, log) = session_with(st.clone(), CAPS).await;
        enable_on(&mut s, &settings(), true).await.unwrap();
        let d = disable_on(&mut s, false).await.unwrap();
        assert_eq!(d, DisableOutcome { restored: None, was_active: true });
        assert_eq!(st.lock().unwrap().active, None);
        assert!(log.lock().unwrap().iter().any(|c| c == "SETACTIVE \"\""));
        assert_eq!(st.lock().unwrap().scripts.len(), 2);
    }

    #[tokio::test]
    async fn restore_skips_a_previous_script_that_no_longer_exists() {
        let st = foreign_store();
        let (mut s, _) = session_with(st.clone(), CAPS).await;
        enable_on(&mut s, &settings(), true).await.unwrap();
        st.lock().unwrap().scripts.retain(|(n, _)| n != "main");
        let d = disable_on(&mut s, true).await.unwrap();
        assert_eq!(d.restored, None);
        assert_eq!(st.lock().unwrap().active, None);
    }

    #[tokio::test]
    async fn previous_is_not_carried_forward_when_our_script_is_off() {
        let st = foreign_store();
        let (mut s, _) = session_with(st.clone(), CAPS).await;
        enable_on(&mut s, &settings(), true).await.unwrap(); // main -> ours, remembers main
        let d = disable_on(&mut s, false).await.unwrap(); // off, nothing restored
        assert_eq!(d.restored, None);
        assert_eq!(st.lock().unwrap().active, None);
        enable_on(&mut s, &settings(), false).await.unwrap(); // nothing else active: no confirm needed
        let body = st.lock().unwrap().scripts.iter().find(|(n, _)| n == SCRIPT_NAME).unwrap().1.clone();
        assert!(!body.contains("previous-active"), "stale memory must not be carried: {body}");
        let d = disable_on(&mut s, true).await.unwrap();
        assert_eq!(d.restored, None);
        assert_eq!(st.lock().unwrap().active, None, "main was left off by the user and stays off");
    }

    #[tokio::test]
    async fn odd_previous_names_round_trip_exactly_and_restore_failure_still_deactivates() {
        for name in ["  spaced name  ", "ünï", &"n".repeat(300)] {
            let st = Arc::new(Mutex::new(Store { scripts: vec![(name.to_string(), "keep;".into())], active: Some(name.to_string()) }));
            let (mut s, _) = session_with(st.clone(), CAPS).await;
            enable_on(&mut s, &settings(), true).await.unwrap();
            let d = disable_on(&mut s, true).await.unwrap();
            assert_eq!(d.restored.as_deref(), Some(name));
            assert_eq!(st.lock().unwrap().active.as_deref(), Some(name));
        }
        // Server refuses to re-activate the old script: we still switch the responder off.
        let (a, b) = duplex(1 << 16);
        let st = foreign_store();
        let mut inner = store_handler(st.clone());
        spawn_server(b, CAPS.to_owned(), move |l, body| if l == "SETACTIVE \"main\"" { "NO \"nope\"\r\n".into() } else { inner(l, body) });
        let mut s = Session::new(a).await.unwrap();
        enable_on(&mut s, &settings(), true).await.unwrap();
        let d = disable_on(&mut s, true).await.unwrap();
        assert_eq!(d, DisableOutcome { restored: None, was_active: true });
        assert_eq!(st.lock().unwrap().active, None);
    }

    #[tokio::test]
    async fn oversized_responses_are_refused() {
        let (a, b) = duplex(1 << 16);
        spawn_server(b, CAPS.into(), |_, _| {
            let mut o = String::new();
            for _ in 0..(MAX_DATA_LINES + 5) {
                o.push_str("\"x\"\r\n");
            }
            o + "OK\r\n"
        });
        let mut s = Session::new(a).await.unwrap();
        assert!(s.list_scripts().await.is_err(), "too many lines");
        // Total byte cap: literals of 1 MB each, five of them.
        let (a, b) = duplex(1 << 16);
        spawn_server(b, CAPS.into(), |_, _| {
            let lit = "y".repeat(MAX_LITERAL);
            let mut o = String::new();
            for _ in 0..5 {
                o.push_str(&format!("{{{}}}\r\n{lit}\r\n", lit.len()));
            }
            o + "OK\r\n"
        });
        let mut s = Session::new(a).await.unwrap();
        let e = s.list_scripts().await.unwrap_err().to_string();
        assert!(e.contains("too large"), "{e}");
    }

    #[test]
    fn previous_name_with_quotes_and_backslashes_is_stored_json_escaped() {
        let name = "a \"b\" \\ c\u{e9}";
        let script = build_script(&settings(), Some(name)).unwrap();
        assert_eq!(parse_previous(&script).as_deref(), Some(name));
        assert!(!name_arg(&"n".repeat(300)).is_err());
    }

    #[test]
    fn password_is_only_sent_to_servers_with_vacation() {
        assert!(require_vacation(&Caps { sieve: vec!["fileinto".into()], ..Default::default() }).is_err());
        assert!(require_vacation(&Caps { sieve: vec!["vacation".into()], ..Default::default() }).is_ok());
    }

    #[tokio::test]
    async fn foreign_script_with_our_name_is_never_overwritten() {
        let st = Arc::new(Mutex::new(Store { scripts: vec![(SCRIPT_NAME.into(), "keep \"me\";".into())], active: None }));
        let (mut s, log) = session_with(st.clone(), CAPS).await;
        let e = enable_on(&mut s, &settings(), true).await.unwrap_err().to_string();
        assert!(e.contains("was not made by MiraMail"), "{e}");
        assert_eq!(st.lock().unwrap().scripts[0].1, "keep \"me\";");
        assert!(log.lock().unwrap().iter().all(|c| !c.starts_with("PUTSCRIPT")));
    }

    #[tokio::test]
    async fn checkscript_rejection_aborts_before_upload() {
        let st = Arc::new(Mutex::new(Store::default()));
        let (mut s, log) = session_with(st.clone(), CAPS).await;
        let mut bad = settings();
        bad.message = "BADSYNTAX".into();
        let e = enable_on(&mut s, &bad, true).await.unwrap_err().to_string();
        assert!(e.contains("syntax error"), "{e}");
        assert!(st.lock().unwrap().scripts.is_empty());
        assert!(log.lock().unwrap().iter().all(|c| !c.starts_with("PUTSCRIPT") && !c.starts_with("SETACTIVE")));
    }

    #[tokio::test]
    async fn dates_need_server_extensions() {
        let st = Arc::new(Mutex::new(Store::default()));
        let (mut s, _) = session_with(st, "\"SASL\" \"PLAIN\"\r\n\"SIEVE\" \"vacation\"\r\nOK\r\n").await;
        let mut d = settings();
        d.start = Some("2026-12-20".into());
        assert!(enable_on(&mut s, &d, true).await.is_err());
        assert!(enable_on(&mut s, &settings(), true).await.is_ok());
    }

    #[tokio::test]
    async fn target_validation() {
        let bad = SieveTarget { host: "a b".into(), port: 4190, tls: SieveTls::StartTls };
        assert!(check_target(&bad).is_err());
        assert!(check_target(&SieveTarget { host: "mail.example.org".into(), port: 0, tls: SieveTls::StartTls }).is_err());
        assert!(check_target(&SieveTarget { host: "mail.example.org".into(), port: 4190, tls: SieveTls::StartTls }).is_ok());
    }
}
