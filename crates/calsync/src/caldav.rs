//! CalDAV client (RFC 4791 / 6764 / 4918) over HTTPS with Basic auth.
//!
//! Security rules enforced centrally in [`CalDavClient::send`]:
//! * HTTPS only (plain HTTP only for `127.0.0.1`/`::1` when `allow_insecure_loopback`, tests only).
//! * Redirects are never followed by the HTTP stack; they are followed manually (max 5) and only to a
//!   URL on the same site (the exact same host, or another host of a small allow-list of big providers'
//!   registrable domains). Every URL the server hands us (redirects, hrefs) must pass the same check
//!   before credentials are sent to it.
//! * Timeouts: connect 5 s, total 20 s. Response bodies are capped at 8 MB.
//! * XML is parsed with quick-xml; DOCTYPE (hence entity declarations) is rejected, depth/node counts are capped.

use calendar::{build_ics_series, CalEvent, Calendar, CalendarInfo, CalendarKind, EventOverride, ParseOptions};
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::NsReader;
use reqwest::{Client, Method, Url};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;

pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MULTIGET_BATCH: usize = 50;
const MAX_XML_DEPTH: usize = 64;
const MAX_XML_NODES: usize = 200_000;

const NS_DAV: &str = "DAV:";
const NS_CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const NS_CS: &str = "http://calendarserver.org/ns/";
const NS_APPLE: &str = "http://apple.com/ns/ical/";

pub type Result<T> = std::result::Result<T, CalDavError>;

/// Error messages are user-facing and never contain credentials, ICS bodies or event text.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CalDavError {
    #[error("Wrong username or password (401). Check the password; use an app-specific password if you use two-factor authentication")]
    Unauthorized,
    #[error("The server refused access (403). Check that CalDAV is enabled and the app password may use calendars.")]
    Forbidden,
    #[error("Calendar not found on the server (404). Check the server address.")]
    NotFound,
    #[error("The server's security certificate is not valid, so MiraMail will not connect.")]
    Tls,
    #[error("Can't reach the calendar server. Check your internet connection.")]
    Offline,
    #[error("The calendar server took too long to answer.")]
    Timeout,
    #[error("The event changed on the server (precondition failed).")]
    PreconditionFailed,
    #[error("Refusing to send your password over an insecure connection.")]
    Insecure,
    #[error("The server tried to redirect to a different site; MiraMail will not send your password there.")]
    CrossSiteRedirect,
    #[error("The server redirected too many times.")]
    TooManyRedirects,
    #[error("The server's response was too large.")]
    TooLarge,
    #[error("The server sent a response MiraMail could not understand ({0}).")]
    BadResponse(String),
    #[error("The calendar server returned an error (HTTP {0}).")]
    Http(u16),
    #[error("No calendars were found on this account.")]
    NoCalendars,
    #[error("Invalid calendar settings: {0}")]
    Config(String),
    #[error("Local calendar storage error: {0}")]
    Store(String),
    /// A strict ICS build refused the event (over-cap RRULE / attendees / EXDATEs, unparsable time, > 1 MB).
    /// Nothing was sent and nothing was truncated; the local event is kept (and stays queued for the next sync).
    #[error("This event can't be saved to the calendar server: {0}")]
    Rejected(String),
}

/// Friendly, content-free mapping of the strict-build errors of the `calendar` crate (they only ever
/// carry short field names, never event text).
fn map_build_err(e: calendar::Error) -> CalDavError {
    match e {
        calendar::Error::TooLarge { max } => CalDavError::Rejected(format!("it is larger than {} KB", max / 1024)),
        calendar::Error::Invalid(m) if m.contains("RRULE") => CalDavError::Rejected("its repeat rule is too long".into()),
        calendar::Error::Invalid(m) if m.contains("attendee") || m.contains("ATTENDEE") => {
            CalDavError::Rejected(format!("it has too many attendees (more than {})", calendar::MAX_ATTENDEES))
        }
        calendar::Error::Invalid(m) if m.contains("EXDATE") => CalDavError::Rejected("it has too many excluded dates".into()),
        calendar::Error::Invalid(m) if m.contains("time") => CalDavError::Rejected("its start or end time can't be read".into()),
        calendar::Error::Invalid(m) => CalDavError::Rejected(m),
        e => CalDavError::Store(e.to_string()),
    }
}

/// Zoom trust model. `X-MIRAMAIL-ZOOM-*` properties in a pulled resource are only honoured when the
/// calendar is a CalDAV calendar the user added themselves AND it is writable: on such a calendar only
/// the user (or their own devices) can put data there. Read-only / shared / subscribed calendars can be
/// written by third parties, so their Zoom props are ignored (an attacker could otherwise plant a
/// phishing "join" link that the UI shows as the user's own Zoom meeting).
pub fn zoom_trusted(cal: &CalendarInfo) -> bool {
    cal.kind == CalendarKind::Caldav && !cal.read_only
}

fn owned_key(cal: &str) -> String {
    format!("calsync.owned.{cal}")
}

/// Persist whether discovery found the calendar to be the user's own (no sharing markers, owner = the user).
pub fn set_owned(store: &Mutex<Calendar>, calendar_id: &str, owned: bool) -> Result<()> {
    with(store, |c| c.set_setting(&owned_key(calendar_id), if owned { "1" } else { "0" }))
}

/// Has discovery marked the calendar as owned? Unknown (never discovered) counts as NOT owned.
pub fn is_owned(c: &Calendar, calendar_id: &str) -> bool {
    c.get_setting(&owned_key(calendar_id)).ok().flatten().as_deref() == Some("1")
}

/// [`zoom_trusted`] plus the persisted `owned` flag: a calendar that is shared with / from somebody else
/// (or whose ownership was never established) never gets its `X-MIRAMAIL-ZOOM-*` properties honoured.
pub fn zoom_trusted_in(c: &Calendar, cal: &CalendarInfo) -> bool {
    zoom_trusted(cal) && is_owned(c, &cal.id)
}

/// Remove the bookkeeping this module keeps for a calendar (pending changes, owned flag).
pub fn forget_calendar(c: &Calendar, calendar_id: &str) -> calendar::Result<()> {
    c.delete_setting(&dirty_key(calendar_id))?;
    c.delete_setting(&tomb_key(calendar_id))?;
    c.delete_setting(&owned_key(calendar_id))
}


// ---------------------------------------------------------------------------------------------
// Diagnostics (step names, HTTP statuses, hosts; never credentials, query strings or bodies)
// ---------------------------------------------------------------------------------------------

type ProgressFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct DiagInner {
    lines: Vec<String>,
    step: String,
    host: String,
    refused: Option<String>,
}

/// Collects a non-secret trail of what discovery did, and reports each step as it starts.
#[derive(Clone, Default)]
pub struct Diag {
    inner: Arc<Mutex<DiagInner>>,
    progress: Option<ProgressFn>,
}

const MAX_DIAG_LINES: usize = 60;

impl Diag {
    pub fn new() -> Self {
        Self::default()
    }
    /// Call `f(step)` whenever a new step starts (used for live progress in the UI).
    pub fn with_progress(mut self, f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(f));
        self
    }
    fn g(&self) -> std::sync::MutexGuard<'_, DiagInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn note(&self, line: impl Into<String>) {
        let mut g = self.g();
        if g.lines.len() < MAX_DIAG_LINES {
            g.lines.push(line.into());
        }
    }
    /// Start a named step (shown to the user as progress and recorded in the trail).
    pub fn step(&self, name: &str) {
        {
            let mut g = self.g();
            g.step = name.to_string();
        }
        self.note(format!("step: {name}"));
        if let Some(f) = &self.progress {
            f(name);
        }
    }
    pub fn lines(&self) -> Vec<String> {
        self.g().lines.clone()
    }
    fn set_host(&self, host: &str) {
        self.g().host = host.to_string();
    }
    fn refuse(&self, host: &str) {
        self.g().refused = Some(host.to_string());
    }
    /// Plain-words explanation of a failure: which step failed and why. Never includes credentials.
    pub fn explain(&self, e: &CalDavError) -> String {
        let g = self.g();
        let step = if g.step.is_empty() { "Connecting".to_string() } else { g.step.clone() };
        let host = if g.host.is_empty() { "the server".to_string() } else { g.host.clone() };
        let why = match e {
            CalDavError::Unauthorized => format!(
                "{host} rejected your username or password (401). Check the password. If you use two-factor authentication, create an app-specific password and use that instead."
            ),
            CalDavError::Forbidden => format!("{host} refused access (403). Check that CalDAV is enabled for this account."),
            CalDavError::NotFound => format!(
                "none of the usual calendar addresses on {host} answered (404). Check the server address or, for Zoho, the data centre (region)."
            ),
            CalDavError::Offline => format!("MiraMail couldn't reach {host}. Check your internet connection and the server address."),
            CalDavError::Timeout => format!("{host} took too long to answer."),
            CalDavError::Tls => format!("the security certificate of {host} is not valid, so MiraMail will not connect."),
            CalDavError::CrossSiteRedirect => match &g.refused {
                Some(r) => format!("{host} tried to send your password to a different site ({r}); MiraMail refused."),
                None => "the server tried to redirect to a different site; MiraMail refused to send your password there.".to_string(),
            },
            CalDavError::NoCalendars => format!("you are signed in to {host}, but it lists no event calendars."),
            other => other.to_string(),
        };
        format!("{step} failed: {why}")
    }
}

fn status_err(code: u16) -> CalDavError {
    match code {
        401 => CalDavError::Unauthorized,
        403 => CalDavError::Forbidden,
        404 | 410 => CalDavError::NotFound,
        412 => CalDavError::PreconditionFailed,
        c => CalDavError::Http(c),
    }
}

#[derive(Clone)]
pub struct Config {
    pub base_url: String,
    pub username: String,
    pub password: String,
    /// Tests only. Honoured ONLY for `127.0.0.1` / `::1`. Default false.
    pub allow_insecure_loopback: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config").field("base_url", &self.base_url).field("username", &self.username).field("password", &"<redacted>").finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredCalendar {
    /// Absolute collection URL (ends with `/`).
    pub url: String,
    pub name: String,
    /// `#rrggbb`
    pub color: Option<String>,
    pub read_only: bool,
    pub ctag: Option<String>,
    /// No sharing / invite / foreign-owner markers: only the user (and their devices) can write here.
    /// Gates trust in `X-MIRAMAIL-ZOOM-*` properties (see [`zoom_trusted_in`]).
    pub owned: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSummary {
    pub pulled: usize,
    pub pushed: usize,
    pub deleted: usize,
    pub conflicts: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutOutcome {
    pub href: String,
    pub etag: Option<String>,
}

/// Result of [`CalDavClient::push_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    Pushed(PutOutcome),
    /// Server had a newer version (HTTP 412); it was fetched and stored locally (server wins).
    Conflict,
}

/// Zoho data centres (the part after `calendar.zoho.`).
/// Source: https://www.zoho.com/calendar/help/setup-caldav-sync.html names `.com` (and `.eu`/`.au` as examples of
/// country-coded domains). UNVERIFIED: `in`, `jp`, `ca`, `sa` are inferred from Zoho's usual data-centre domains
/// (`zoho.in`, `zoho.jp`, `zohocloud.ca`, `zoho.sa`); `com.au` is the documented Australian domain.
pub const ZOHO_REGIONS: &[&str] = &["com", "in", "eu", "com.au", "jp", "ca", "sa"];

/// Is `region` one of [`ZOHO_REGIONS`]?
pub fn valid_zoho_region(region: &str) -> bool {
    ZOHO_REGIONS.contains(&region)
}

/// Infer a Zoho data-centre region from a host name (e.g. `imap.zoho.in` -> `in`, `smtp.zoho.com.au` -> `com.au`).
/// `None` when the host is not a Zoho host.
pub fn zoho_region_from_host(host: &str) -> Option<&'static str> {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let in_domain = |d: &str| h == d || h.ends_with(&format!(".{d}"));
    // longest suffixes first so `zoho.com.au` is not taken for `zoho.com`
    [("zoho.com.au", "com.au"), ("zoho.com", "com"), ("zoho.in", "in"), ("zoho.eu", "eu"), ("zoho.jp", "jp"), ("zohocloud.ca", "ca"), ("zoho.ca", "ca"), ("zoho.sa", "sa")]
        .into_iter()
        .find(|(d, _)| in_domain(d))
        .map(|(_, r)| r)
}

/// Provider presets. The exact principal/calendar paths always come from discovery.
/// Returns the base URL to give to [`Config::base_url`]; `None` for `"custom"` (user supplies the URL) or unknown names.
/// Zoho defaults to the `.com` data centre; use [`preset_url_region`] for the others.
pub fn preset_url(preset: &str, username: &str) -> Option<String> {
    preset_url_region(preset, username, None)
}

/// [`preset_url`] with a Zoho data-centre `region` (`com`, `in`, `eu`, ... see [`ZOHO_REGIONS`]; `None` = `com`).
/// Ignored for other presets. An unknown region returns `None`. Zoho's base is the host root: discovery then
/// tries `/.well-known/caldav` first (what Zoho documents for clients such as eM Client), the root, then `/caldav/`.
pub fn preset_url_region(preset: &str, _username: &str, region: Option<&str>) -> Option<String> {
    match preset {
        "icloud" => Some("https://caldav.icloud.com".into()),
        "fastmail" => Some("https://caldav.fastmail.com".into()),
        "zoho" => {
            let r = region.map(str::trim).filter(|r| !r.is_empty()).unwrap_or("com").to_ascii_lowercase();
            valid_zoho_region(&r).then(|| format!("https://calendar.zoho.{r}/"))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// URL policy
// ---------------------------------------------------------------------------------------------

fn ip_of(u: &Url) -> Option<std::net::IpAddr> {
    u.host_str()?.trim_start_matches('[').trim_end_matches(']').parse().ok()
}

fn is_loopback(u: &Url) -> bool {
    match ip_of(u) {
        Some(std::net::IpAddr::V4(a)) => a.octets() == [127, 0, 0, 1],
        Some(std::net::IpAddr::V6(a)) => a == std::net::Ipv6Addr::LOCALHOST,
        None => false,
    }
}

fn is_ip(u: &Url) -> bool {
    ip_of(u).is_some()
}

const MULTI_PART_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "com.au", "net.au", "org.au", "co.nz", "co.jp", "co.in", "co.za", "com.br", "com.cn", "com.mx",
    "com.tr", "com.sg", "co.il", "co.kr", "com.hk", "com.tw",
];

/// Heuristic registrable domain (no public-suffix list): last two labels, or three for common multi-part suffixes.
pub fn registrable_domain(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() <= 2 {
        return host;
    }
    let last2 = labels[labels.len() - 2..].join(".");
    let n = if MULTI_PART_SUFFIXES.contains(&last2.as_str()) { 3 } else { 2 };
    labels[labels.len().saturating_sub(n)..].join(".")
}

/// Registrable domains of providers whose CalDAV front doors legitimately redirect between sibling hosts
/// (`caldav.icloud.com` -> `p12-caldav.icloud.com`). Everywhere else only the exact host is accepted, so a
/// server on a shared-hosting domain cannot bounce credentials to a neighbour host.
const SHARED_SITES: &[&str] = &["icloud.com", "fastmail.com", "zoho.com", "zoho.eu", "zoho.in", "zoho.jp", "zoho.sa", "zohocloud.ca", "zoho.com.au"];

fn same_site(a: &Url, b: &Url) -> bool {
    let (Some(ha), Some(hb)) = (a.host_str(), b.host_str()) else { return false };
    if ha.eq_ignore_ascii_case(hb) {
        return true;
    }
    if is_ip(a) || is_ip(b) {
        return false;
    }
    let (ra, rb) = (registrable_domain(ha), registrable_domain(hb));
    ra == rb && SHARED_SITES.contains(&ra.as_str())
}

// ---------------------------------------------------------------------------------------------
// XML (namespace aware, no entity expansion)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct Node {
    ns: String,
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn is(&self, ns: &str, name: &str) -> bool {
        self.ns == ns && self.name == name
    }
    fn child(&self, ns: &str, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.is(ns, name))
    }
    fn text_trim(&self) -> &str {
        self.text.trim()
    }
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn node_has(n: &Node, name: &str) -> bool {
    n.name == name || n.children.iter().any(|c| node_has(c, name))
}

fn bad(what: &str) -> CalDavError {
    CalDavError::BadResponse(what.to_string())
}

fn make_node(res: ResolveResult<'_>, e: &quick_xml::events::BytesStart<'_>) -> Node {
    let ns = match res {
        ResolveResult::Bound(n) => String::from_utf8_lossy(n.as_ref()).into_owned(),
        _ => String::new(),
    };
    let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
    let mut attrs = Vec::new();
    for a in e.attributes().flatten() {
        let k = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
        let v = a.unescape_value().map(|c| c.into_owned()).unwrap_or_default();
        attrs.push((k, v));
    }
    Node { ns, name, attrs, ..Default::default() }
}

/// Parses to a tree. Rejects DOCTYPE (so no entity declarations), deep nesting and huge documents. Never panics.
fn parse_xml(data: &[u8]) -> Result<Node> {
    let mut reader = NsReader::from_reader(data);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    let mut nodes = 0usize;
    loop {
        buf.clear();
        let (res, ev) = reader.read_resolved_event_into(&mut buf).map_err(|_| bad("malformed XML"))?;
        match ev {
            Event::Start(e) => {
                nodes += 1;
                if stack.len() >= MAX_XML_DEPTH || nodes > MAX_XML_NODES {
                    return Err(bad("XML too complex"));
                }
                stack.push(make_node(res, &e));
            }
            Event::Empty(e) => {
                nodes += 1;
                if nodes > MAX_XML_NODES {
                    return Err(bad("XML too complex"));
                }
                let n = make_node(res, &e);
                match stack.last_mut() {
                    Some(p) => p.children.push(n),
                    None => {
                        if root.is_none() {
                            root = Some(n);
                        }
                    }
                }
            }
            Event::End(_) => {
                let Some(n) = stack.pop() else { return Err(bad("malformed XML")) };
                match stack.last_mut() {
                    Some(p) => p.children.push(n),
                    None => {
                        if root.is_none() {
                            root = Some(n);
                        }
                    }
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    // unknown entity: keep nothing rather than expanding anything
                    if let Ok(s) = t.unescape() {
                        top.text.push_str(&s);
                    }
                }
            }
            Event::CData(c) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(c.as_ref()));
                }
            }
            Event::DocType(_) => return Err(bad("DOCTYPE not allowed")),
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(bad("truncated XML"));
    }
    root.ok_or_else(|| bad("empty XML"))
}

struct MsResponse {
    href: String,
    /// Response-level status (sync-collection style 404s), if any.
    status: Option<u16>,
    /// Children of `prop` from propstats with a 2xx (or missing) status.
    props: Vec<Node>,
}

impl MsResponse {
    fn prop(&self, ns: &str, name: &str) -> Option<&Node> {
        self.props.iter().find(|p| p.is(ns, name))
    }
}

fn parse_status(s: &str) -> Option<u16> {
    s.split_whitespace().nth(1)?.parse().ok()
}

fn parse_multistatus(data: &[u8]) -> Result<Vec<MsResponse>> {
    let root = parse_xml(data)?;
    if !root.is(NS_DAV, "multistatus") {
        return Err(bad("not a multistatus"));
    }
    let mut out = Vec::new();
    for r in root.children.iter().filter(|c| c.is(NS_DAV, "response")) {
        let Some(href) = r.child(NS_DAV, "href").map(|h| h.text_trim().to_string()).filter(|h| !h.is_empty()) else { continue };
        let status = r.child(NS_DAV, "status").and_then(|s| parse_status(s.text_trim()));
        let mut props = Vec::new();
        for ps in r.children.iter().filter(|c| c.is(NS_DAV, "propstat")) {
            let ok = ps.child(NS_DAV, "status").and_then(|s| parse_status(s.text_trim())).map(|c| (200..300).contains(&c)).unwrap_or(true);
            if ok {
                if let Some(p) = ps.child(NS_DAV, "prop") {
                    props.extend(p.children.iter().cloned());
                }
            }
        }
        out.push(MsResponse { href, status, props });
    }
    Ok(out)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16)) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Comparison key for hrefs: path only, percent-decoded, no trailing slash.
fn norm_href(h: &str) -> String {
    let path = if h.starts_with("http://") || h.starts_with("https://") {
        Url::parse(h).map(|u| u.path().to_string()).unwrap_or_else(|_| h.to_string())
    } else {
        h.split(['?', '#']).next().unwrap_or(h).to_string()
    };
    pct_decode(&path).trim_end_matches('/').to_string()
}

fn normalize_color(c: &str) -> Option<String> {
    let c = c.trim().strip_prefix('#')?;
    if !c.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match c.len() {
        3 => Some(format!("#{}", c.chars().flat_map(|x| [x, x]).collect::<String>().to_ascii_lowercase())),
        6 | 8 => Some(format!("#{}", c[..6].to_ascii_lowercase())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Pending-change bookkeeping (settings keys; no schema change)
// ---------------------------------------------------------------------------------------------

fn dirty_key(cal: &str) -> String {
    format!("calsync.dirty.{cal}")
}
fn tomb_key(cal: &str) -> String {
    format!("calsync.tomb.{cal}")
}

fn lock(store: &Mutex<Calendar>) -> std::sync::MutexGuard<'_, Calendar> {
    store.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run a store operation without ever holding the lock across an `.await`.
fn with<T>(store: &Mutex<Calendar>, f: impl FnOnce(&Calendar) -> calendar::Result<T>) -> Result<T> {
    let g = lock(store);
    f(&g).map_err(|e| CalDavError::Store(e.to_string()))
}

fn load_dirty(c: &Calendar, cal: &str) -> Vec<String> {
    c.get_setting(&dirty_key(cal)).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn save_dirty(c: &Calendar, cal: &str, v: &[String]) -> calendar::Result<()> {
    if v.is_empty() {
        c.delete_setting(&dirty_key(cal))
    } else {
        c.set_setting(&dirty_key(cal), &serde_json::to_string(v).unwrap_or_else(|_| "[]".into()))
    }
}
fn load_tombs(c: &Calendar, cal: &str) -> Vec<(String, Option<String>)> {
    c.get_setting(&tomb_key(cal)).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn save_tombs(c: &Calendar, cal: &str, v: &[(String, Option<String>)]) -> calendar::Result<()> {
    if v.is_empty() {
        c.delete_setting(&tomb_key(cal))
    } else {
        c.set_setting(&tomb_key(cal), &serde_json::to_string(v).unwrap_or_else(|_| "[]".into()))
    }
}

/// Mark an event as locally modified; the next sync pushes it (`If-Match` its etag).
/// Events without an href are pushed anyway; this is for edits to events that already exist on the server.
pub fn mark_dirty(store: &Mutex<Calendar>, calendar_id: &str, uid: &str) -> Result<()> {
    with(store, |c| {
        let mut v = load_dirty(c, calendar_id);
        if !v.iter().any(|u| u == uid) {
            v.push(uid.to_string());
        }
        save_dirty(c, calendar_id, &v)
    })
}

/// Record that a server resource was deleted locally; the next sync sends `DELETE` (`If-Match` etag).
pub fn mark_deleted(store: &Mutex<Calendar>, calendar_id: &str, href: &str, etag: Option<&str>) -> Result<()> {
    with(store, |c| {
        let mut v = load_tombs(c, calendar_id);
        if !v.iter().any(|(h, _)| norm_href(h) == norm_href(href)) {
            v.push((href.to_string(), etag.map(String::from)));
        }
        save_tombs(c, calendar_id, &v)
    })
}

/// Number of locally pending changes (dirty events + queued deletions) for a calendar.
pub fn pending_changes(store: &Mutex<Calendar>, calendar_id: &str) -> Result<usize> {
    with(store, |c| Ok(load_dirty(c, calendar_id).len() + load_tombs(c, calendar_id).len()))
}

// ---------------------------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------------------------

struct Resp {
    status: u16,
    etag: Option<String>,
    body: Vec<u8>,
    url: Url,
}

pub struct CalDavClient {
    http: Client,
    cfg: Config,
    base: Url,
    diag: Diag,
}

fn map_reqwest(e: reqwest::Error) -> CalDavError {
    if e.is_timeout() {
        return CalDavError::Timeout;
    }
    let mut chain = String::new();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(c) = cur {
        chain.push_str(&c.to_string().to_ascii_lowercase());
        chain.push('|');
        cur = c.source();
    }
    if chain.contains("certificate") || chain.contains("invalid peer") || chain.contains("unknownissuer") || chain.contains("tls handshake") {
        return CalDavError::Tls;
    }
    if e.is_connect() || e.is_request() {
        return CalDavError::Offline;
    }
    CalDavError::BadResponse("network error".into())
}

fn diag_kind(e: &CalDavError) -> &'static str {
    match e {
        CalDavError::Timeout => "timeout",
        CalDavError::Tls => "TLS/certificate problem",
        CalDavError::Offline => "unreachable",
        _ => "network error",
    }
}

fn propfind() -> Method {
    Method::from_bytes(b"PROPFIND").expect("valid method")
}
fn report() -> Method {
    Method::from_bytes(b"REPORT").expect("valid method")
}

const XML_CT: (&str, &str) = ("Content-Type", "application/xml; charset=utf-8");

impl CalDavClient {
    pub fn new(cfg: Config) -> Result<Self> {
        let base = Url::parse(cfg.base_url.trim()).map_err(|_| CalDavError::Config("server address is not a valid URL".into()))?;
        if base.host_str().is_none() {
            return Err(CalDavError::Config("server address has no host".into()));
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(CalDavError::Config("do not put credentials in the server address".into()));
        }
        let loop_ok = cfg.allow_insecure_loopback && is_loopback(&base);
        match base.scheme() {
            "https" => {}
            "http" if loop_ok => {}
            _ => return Err(CalDavError::Insecure),
        }
        let mut b = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .user_agent("MiraMail");
        if loop_ok {
            b = b.no_proxy();
        }
        let http = b.build().map_err(|_| CalDavError::Config("could not initialise HTTP client".into()))?;
        Ok(Self { http, cfg, base, diag: Diag::default() })
    }

    /// Record a non-secret trail of requests (and report discovery steps) into `diag`.
    pub fn with_diag(mut self, diag: Diag) -> Self {
        self.diag = diag;
        self
    }

    fn check_url(&self, u: &Url) -> Result<()> {
        if !u.username().is_empty() || u.password().is_some() {
            return Err(CalDavError::Insecure);
        }
        let loop_ok = self.cfg.allow_insecure_loopback && is_loopback(u);
        match u.scheme() {
            "https" => {}
            "http" if loop_ok => {}
            _ => return Err(CalDavError::Insecure),
        }
        if !same_site(u, &self.base) {
            return Err(CalDavError::CrossSiteRedirect);
        }
        Ok(())
    }

    /// One request with manual redirect handling. Method and body are preserved across redirects.
    async fn send(&self, method: Method, url: &Url, headers: &[(&str, &str)], body: Option<&str>) -> Result<Resp> {
        let mut url = url.clone();
        let mut method = method;
        let mut body = body;
        let mut headers: Vec<(&str, &str)> = headers.to_vec();
        for hop in 0..=MAX_REDIRECTS {
            if let Err(e) = self.check_url(&url) {
                let host = url.host_str().unwrap_or("?").to_string();
                self.diag.refuse(&host);
                self.diag.note(format!("refused {} (not allowed: {})", host, if e == CalDavError::Insecure { "insecure" } else { "different site" }));
                return Err(e);
            }
            self.diag.set_host(url.host_str().unwrap_or("?"));
            let mut rb = self.http.request(method.clone(), url.clone()).basic_auth(&self.cfg.username, Some(&self.cfg.password));
            for (k, v) in &headers {
                rb = rb.header(*k, *v);
            }
            if let Some(b) = body {
                rb = rb.body(b.to_owned());
            }
            let mut resp = match rb.send().await {
                Ok(r) => r,
                Err(e) => {
                    let e = map_reqwest(e.without_url());
                    self.diag.note(format!("{} {}{} -> no response ({})", method, url.host_str().unwrap_or("?"), url.path(), diag_kind(&e)));
                    return Err(e);
                }
            };
            let status = resp.status().as_u16();
            self.diag.note(format!("{} {}{} -> HTTP {}", method, url.host_str().unwrap_or("?"), url.path(), status));
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if hop == MAX_REDIRECTS {
                    return Err(CalDavError::TooManyRedirects);
                }
                let loc = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).ok_or_else(|| bad("redirect without location"))?;
                url = url.join(loc).map_err(|_| bad("bad redirect location"))?;
                if status == 303 && method != Method::GET && method != Method::HEAD {
                    // RFC 9110 15.4.4: "see other" means fetch the result with GET, without the body or its preconditions.
                    method = Method::GET;
                    body = None;
                    headers.retain(|(k, _)| !["content-type", "if-match", "if-none-match", "depth"].contains(&k.to_ascii_lowercase().as_str()));
                }
                continue;
            }
            if resp.content_length().map(|l| l as usize > MAX_BODY_BYTES).unwrap_or(false) {
                return Err(CalDavError::TooLarge);
            }
            let etag = resp.headers().get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).map(|s| s.to_string());
            let mut out = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|e| map_reqwest(e.without_url()))? {
                if out.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(CalDavError::TooLarge);
                }
                out.extend_from_slice(&chunk);
            }
            return Ok(Resp { status, etag, body: out, url });
        }
        Err(CalDavError::TooManyRedirects)
    }

    async fn dav(&self, method: Method, url: &Url, depth: &str, body: &str) -> Result<Vec<MsResponse>> {
        let r = self.send(method, url, &[("Depth", depth), XML_CT], Some(body)).await?;
        if r.status != 207 {
            return Err(status_err(r.status));
        }
        parse_multistatus(&r.body)
    }

    fn resolve(&self, base: &Url, href: &str) -> Result<Url> {
        let u = base.join(href).map_err(|_| bad("bad href"))?;
        self.check_url(&u)?;
        Ok(u)
    }

    // ---- discovery ------------------------------------------------------------------------

    /// RFC 6764 / 4791 discovery: `/.well-known/caldav` and the base URL, current-user-principal,
    /// calendar-home-set, then PROPFIND depth 1 on the home set. Only VEVENT calendars are returned.
    ///
    /// Probe order: for a host-root base (`https://host/`, the presets) `/.well-known/caldav`, the root, then
    /// `/caldav/`; for a base with a path, that URL first, then `/.well-known/caldav`, then `/caldav/`.
    pub async fn discover(&self) -> Result<Vec<DiscoveredCalendar>> {
        let mut well_known = self.base.clone();
        well_known.set_path("/.well-known/caldav");
        well_known.set_query(None);
        let mut caldav_dir = well_known.clone();
        caldav_dir.set_path("/caldav/");
        let root = matches!(self.base.path(), "" | "/");
        let mut candidates = if root { vec![well_known, self.base.clone()] } else { vec![self.base.clone(), well_known] };
        if !candidates.contains(&caldav_dir) {
            candidates.push(caldav_dir);
        }
        self.diag.step("Looking up your calendar server");

        const Q: &str = r#"<?xml version="1.0" encoding="utf-8"?><D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:current-user-principal/><C:calendar-home-set/><D:resourcetype/></D:prop></D:propfind>"#;
        let mut last_err = CalDavError::NotFound;
        let mut found: Option<(Url, Vec<MsResponse>)> = None;
        for cand in &candidates {
            match self.send(propfind(), cand, &[("Depth", "0"), XML_CT], Some(Q)).await {
                Ok(r) if r.status == 207 => match parse_multistatus(&r.body) {
                    Ok(ms) => {
                        found = Some((r.url, ms));
                        break;
                    }
                    Err(e) => last_err = e,
                },
                Ok(r) => {
                    let e = status_err(r.status);
                    if matches!(e, CalDavError::NotFound | CalDavError::Http(_)) {
                        last_err = e;
                    } else {
                        return Err(e);
                    }
                }
                Err(e @ (CalDavError::NotFound | CalDavError::BadResponse(_) | CalDavError::Http(_))) => last_err = e,
                Err(e) => return Err(e),
            }
        }
        let (at, ms) = found.ok_or(last_err)?;
        self.diag.step("Signing in");

        let first_href = |ms: &[MsResponse], parent: &str| -> Option<String> {
            ms.iter().find_map(|r| {
                r.props.iter().find(|p| p.name == parent).and_then(|p| p.children.iter().find(|c| c.is(NS_DAV, "href"))).map(|h| h.text_trim().to_string())
            })
        };
        let principal_href = first_href(&ms, "current-user-principal");
        let home = if let Some(h) = first_href(&ms, "calendar-home-set") {
            self.resolve(&at, &h)?
        } else if let Some(p) = first_href(&ms, "current-user-principal") {
            let principal = self.resolve(&at, &p)?;
            self.diag.step("Finding your calendars");
            let ms2 = self.dav(propfind(), &principal, "0", Q).await?;
            match first_href(&ms2, "calendar-home-set") {
                Some(h) => self.resolve(&principal, &h)?,
                None => principal,
            }
        } else {
            at
        };

        const LIST: &str = r#"<?xml version="1.0" encoding="utf-8"?><D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/" xmlns:A="http://apple.com/ns/ical/"><D:prop><D:resourcetype/><D:displayname/><A:calendar-color/><CS:getctag/><C:supported-calendar-component-set/><D:current-user-privilege-set/><D:owner/><CS:shared-url/><CS:invite/></D:prop></D:propfind>"#;
        self.diag.step("Listing your calendars");
        let listing = self.dav(propfind(), &home, "1", LIST).await?;
        let mut out = Vec::new();
        for r in listing {
            let is_cal = r.prop(NS_DAV, "resourcetype").map(|t| t.child(NS_CALDAV, "calendar").is_some()).unwrap_or(false);
            if !is_cal {
                continue;
            }
            if let Some(set) = r.prop(NS_CALDAV, "supported-calendar-component-set") {
                if !set.children.iter().any(|c| c.is(NS_CALDAV, "comp") && c.attr("name").map(|n| n.eq_ignore_ascii_case("VEVENT")).unwrap_or(false)) {
                    continue;
                }
            }
            let Ok(mut url) = self.resolve(&home, &r.href) else { continue };
            if !url.path().ends_with('/') {
                let p = format!("{}/", url.path());
                url.set_path(&p);
            }
            let name = r
                .prop(NS_DAV, "displayname")
                .map(|n| n.text_trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| pct_decode(url.path().trim_end_matches('/').rsplit('/').next().unwrap_or("Calendar")));
            let read_only = r
                .prop(NS_DAV, "current-user-privilege-set")
                .map(|ps| {
                    !ps.children.iter().filter(|p| p.is(NS_DAV, "privilege")).any(|p| p.children.iter().any(|c| c.is(NS_DAV, "write") || c.is(NS_DAV, "write-content") || c.is(NS_DAV, "all")))
                })
                .unwrap_or(false);
            // Owned = nobody else can write here: not a shared/subscribed collection, not shared out to others,
            // and (when the server says) owned by the signed-in principal.
            let shared_rt = r
                .prop(NS_DAV, "resourcetype")
                .map(|t| t.children.iter().any(|c| matches!(c.name.as_str(), "shared" | "shared-owner" | "subscribed")))
                .unwrap_or(false);
            let shared_url = r.prop(NS_CS, "shared-url").is_some();
            let shared_out = r
                .prop(NS_CS, "invite")
                .map(|i| i.children.iter().filter(|u| u.name == "user").any(|u| !node_has(u, "shared-owner")))
                .unwrap_or(false);
            let owner_ok = match r.prop(NS_DAV, "owner").and_then(|o| o.child(NS_DAV, "href")).map(|h| h.text_trim().to_string()).filter(|h| !h.is_empty()) {
                None => true,
                Some(h) => principal_href.as_deref().is_some_and(|p| norm_href(p) == norm_href(&h)),
            };
            out.push(DiscoveredCalendar {
                owned: !shared_rt && !shared_url && !shared_out && owner_ok,
                url: url.to_string(),
                name,
                color: r.prop(NS_APPLE, "calendar-color").and_then(|c| normalize_color(c.text_trim())),
                read_only,
                ctag: r.prop(NS_CS, "getctag").map(|c| c.text_trim().to_string()).filter(|c| !c.is_empty()),
            });
        }
        if out.is_empty() {
            return Err(CalDavError::NoCalendars);
        }
        self.diag.step(&format!("Found {} calendar{}", out.len(), if out.len() == 1 { "" } else { "s" }));
        Ok(out)
    }

    // ---- immediate writes -----------------------------------------------------------------

    fn collection(&self, url: &str) -> Result<Url> {
        let mut u = Url::parse(url).map_err(|_| CalDavError::Config("calendar address is not a valid URL".into()))?;
        self.check_url(&u)?;
        if !u.path().ends_with('/') {
            let p = format!("{}/", u.path());
            u.set_path(&p);
        }
        Ok(u)
    }

    /// PUT one event (with its RECURRENCE-ID overrides). `If-Match: <etag>` when `ev.href` and `ev.etag`
    /// are set, otherwise `If-None-Match: *`. A missing `href` becomes `<collection>/<safe uid>.ics`.
    /// HTTP 412 -> `Err(PreconditionFailed)`.
    ///
    /// NOTE: this builds the body from the given structs with the LENIENT builder (`build_ics_series`),
    /// so server-side properties that `CalEvent` cannot express (X- props, extra VALARMs, ATTACH,
    /// VTIMEZONE, ...) are NOT preserved and over-cap data is truncated by the builder. For events that
    /// live in the local store use [`CalDavClient::push_event`] / [`CalDavClient::push_event_opts`],
    /// which PUT the server's original resource patched with the edits.
    pub async fn put_event(&self, collection_url: &str, ev: &CalEvent, overrides: &[EventOverride]) -> Result<PutOutcome> {
        let coll = self.collection(collection_url)?;
        let fresh = !(ev.href.as_deref().map(|h| !h.is_empty()).unwrap_or(false) && ev.etag.is_some());
        let ics = build_ics_series(Some(ev), overrides, None);
        let if_match = if fresh { None } else { ev.etag.as_deref() };
        self.put_body(&coll, ev.href.as_deref(), &ev.uid, &ics, if_match).await
    }

    /// PUT a ready-made ICS resource. `href_or_uid`: a server path / absolute URL (starts with `/` or
    /// contains `://`) is used as the resource address; anything else is treated as a UID and becomes
    /// `<collection>/<safe uid>.ics`. `if_match = Some(etag)` sends `If-Match`, `None` sends
    /// `If-None-Match: *` (create only). HTTP 412 -> `Err(PreconditionFailed)`. Bodies above
    /// `calendar::MAX_ICS_BYTES` are refused (`Rejected`) without contacting the server.
    pub async fn put_raw(&self, collection_url: &str, href_or_uid: &str, ics: &str, if_match: Option<&str>) -> Result<PutOutcome> {
        let coll = self.collection(collection_url)?;
        let (href, uid) = if href_or_uid.starts_with('/') || href_or_uid.contains("://") { (Some(href_or_uid), "") } else { (None, href_or_uid) };
        self.put_body(&coll, href, uid, ics, if_match).await
    }

    async fn put_body(&self, coll: &Url, href: Option<&str>, uid: &str, ics: &str, if_match: Option<&str>) -> Result<PutOutcome> {
        if ics.len() > calendar::MAX_ICS_BYTES {
            return Err(map_build_err(calendar::Error::TooLarge { max: calendar::MAX_ICS_BYTES }));
        }
        let href = match href.filter(|h| !h.is_empty()) {
            Some(h) => h.to_string(),
            None => format!("{}{}.ics", coll.path(), safe_name(uid)),
        };
        let url = self.resolve(coll, &href)?;
        let mut headers = vec![("Content-Type", "text/calendar; charset=utf-8")];
        match if_match {
            Some(e) => headers.push(("If-Match", e)),
            None => headers.push(("If-None-Match", "*")),
        }
        let r = self.send(Method::PUT, &url, &headers, Some(ics)).await?;
        if !(200..300).contains(&r.status) {
            return Err(status_err(r.status));
        }
        let mut etag = r.etag;
        if etag.is_none() {
            if let Ok(g) = self.send(Method::GET, &url, &[], None).await {
                if g.status == 200 {
                    etag = g.etag;
                }
            }
        }
        Ok(PutOutcome { href, etag })
    }

    /// DELETE a resource (`If-Match` when an etag is given). 404 counts as success. 412 -> `Err(PreconditionFailed)`.
    pub async fn delete_event(&self, collection_url: &str, href: &str, etag: Option<&str>) -> Result<()> {
        let coll = self.collection(collection_url)?;
        let url = self.resolve(&coll, href)?;
        let mut headers = vec![];
        if let Some(e) = etag {
            headers.push(("If-Match", e));
        }
        let r = self.send(Method::DELETE, &url, &headers, None).await?;
        match r.status {
            200..=299 | 404 | 410 => Ok(()),
            c => Err(status_err(c)),
        }
    }

    /// Push one stored event right after a local save. On 412 the server copy is fetched and stored
    /// (server wins) and `Conflict` is returned. On any other failure the event is marked dirty for the next sync
    /// and the error is returned.
    pub async fn push_event(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, uid: &str) -> Result<PushOutcome> {
        self.push_event_opts(store, cal, uid, false).await
    }

    /// [`CalDavClient::push_event`] with `schedule_agent_client` (adds `SCHEDULE-AGENT=CLIENT` to added /
    /// rebuilt ATTENDEEs, for when MiraMail sends the iTIP mails itself). The body is the server's
    /// original resource patched with the local edits (`Calendar::resource_ics_for_put_opts`), so unknown
    /// properties and the server's overrides survive. Over-cap RRULE / attendees / EXDATEs, an unreadable
    /// time or a > 1 MB result give `Err(Rejected(..))`: nothing is sent, nothing is truncated, and the
    /// event stays queued as dirty.
    pub async fn push_event_opts(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, uid: &str, schedule_agent_client: bool) -> Result<PushOutcome> {
        let coll = self.collection(cal.url.as_deref().ok_or_else(|| CalDavError::Config("calendar has no URL".into()))?)?;
        let ev = with(store, |c| c.get_event(uid, &cal.id)?.ok_or_else(|| calendar::Error::NotFound("event".into())))?;
        let built = lock(store).resource_ics_for_put_opts(uid, &cal.id, schedule_agent_client);
        let body = match built {
            Ok(b) => b,
            Err(e) => {
                let _ = mark_dirty(store, &cal.id, uid);
                return Err(map_build_err(e));
            }
        };
        let fresh = !(ev.href.as_deref().map(|h| !h.is_empty()).unwrap_or(false) && ev.etag.is_some());
        let if_match = if fresh { None } else { ev.etag.as_deref() };
        match self.put_body(&coll, ev.href.as_deref(), uid, &body, if_match).await {
            Ok(o) => {
                self.record_put(store, cal, ev, &body, &o)?;
                Ok(PushOutcome::Pushed(o))
            }
            Err(CalDavError::PreconditionFailed) => {
                let href = ev.href.clone().unwrap_or_else(|| format!("{}{}.ics", coll.path(), safe_name(uid)));
                self.refetch(store, cal, &coll, &href).await?;
                with(store, |c| {
                    let mut v = load_dirty(c, &cal.id);
                    v.retain(|u| u != uid);
                    save_dirty(c, &cal.id, &v)
                })?;
                Ok(PushOutcome::Conflict)
            }
            Err(e) => {
                let _ = mark_dirty(store, &cal.id, uid);
                Err(e)
            }
        }
    }

    /// Delete a server resource after the local event was deleted. On failure the deletion is queued for the next sync.
    pub async fn delete_remote(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, href: &str, etag: Option<&str>) -> Result<()> {
        let url = cal.url.as_deref().ok_or_else(|| CalDavError::Config("calendar has no URL".into()))?;
        match self.delete_event(url, href, etag).await {
            Ok(()) => Ok(()),
            Err(CalDavError::PreconditionFailed) => Err(CalDavError::PreconditionFailed),
            Err(e) => {
                let _ = mark_deleted(store, &cal.id, href, etag);
                Err(e)
            }
        }
    }

    /// Store what the server now holds: the PUT body becomes the new original (`raw_ics`) so the next
    /// edit patches it; href / etag are recorded. Falls back to the plain event row if the body cannot be re-parsed.
    fn record_put(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, mut ev: CalEvent, body: &str, o: &PutOutcome) -> Result<()> {
        let cal_id = cal.id.as_str();
        ev.href = Some(o.href.clone());
        ev.etag = o.etag.clone();
        with(store, |c| {
            if c.upsert_ics_opts(cal_id, body.as_bytes(), Some(&o.href), o.etag.as_deref(), ParseOptions { trust_zoom_props: zoom_trusted_in(c, cal) }).is_err() {
                c.sync_upsert_event(&ev)?;
            }
            let mut v = load_dirty(c, cal_id);
            v.retain(|u| u != &ev.uid);
            save_dirty(c, cal_id, &v)
        })
    }

    // ---- fetch ----------------------------------------------------------------------------

    /// GET one resource and store it (server wins). 404 removes the local copy.
    async fn refetch(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, coll: &Url, href: &str) -> Result<()> {
        let url = self.resolve(coll, href)?;
        let r = self.send(Method::GET, &url, &[], None).await?;
        match r.status {
            200 => {
                with(store, |c| store_resource(c, &cal.id, href, r.etag.as_deref(), &r.body, zoom_trusted_in(c, cal)))?;
                Ok(())
            }
            404 | 410 => {
                with(store, |c| remove_href(c, &cal.id, href).map(|_| ()))?;
                Ok(())
            }
            c => Err(status_err(c)),
        }
    }

    async fn multiget(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, coll: &Url, hrefs: &[String]) -> Result<usize> {
        let mut n = 0;
        for batch in hrefs.chunks(MULTIGET_BATCH) {
            let mut body = String::from(r#"<?xml version="1.0" encoding="utf-8"?><C:calendar-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:getetag/><C:calendar-data/></D:prop>"#);
            for h in batch {
                body.push_str(&format!("<D:href>{}</D:href>", xml_escape(h)));
            }
            body.push_str("</C:calendar-multiget>");
            let ms = self.dav(report(), coll, "1", &body).await?;
            for r in ms {
                if r.status.map(|s| !(200..300).contains(&s)).unwrap_or(false) {
                    continue;
                }
                let (Some(data), etag) = (r.prop(NS_CALDAV, "calendar-data"), r.prop(NS_DAV, "getetag")) else { continue };
                let etag = etag.map(|e| e.text_trim().to_string()).filter(|e| !e.is_empty());
                let stored = with(store, |c| Ok(store_resource(c, &cal.id, &r.href, etag.as_deref(), data.text.as_bytes(), zoom_trusted_in(c, cal)).is_ok()))?;
                if stored {
                    n += 1;
                }
            }
        }
        Ok(n)
    }

    async fn list_server(&self, coll: &Url) -> Result<Vec<(String, String)>> {
        const Q: &str = r#"<?xml version="1.0" encoding="utf-8"?><C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:getetag/></D:prop><C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT"/></C:comp-filter></C:filter></C:calendar-query>"#;
        let ms = self.dav(report(), coll, "1", Q).await?;
        let me = norm_href(coll.path());
        let mut out = Vec::new();
        for r in ms {
            if r.status.map(|s| !(200..300).contains(&s)).unwrap_or(false) || norm_href(&r.href) == me {
                continue;
            }
            if self.resolve(coll, &r.href).is_err() {
                continue;
            }
            if let Some(e) = r.prop(NS_DAV, "getetag").map(|e| e.text_trim().to_string()).filter(|e| !e.is_empty()) {
                out.push((r.href, e));
            }
        }
        Ok(out)
    }

    async fn server_ctag(&self, coll: &Url) -> Result<Option<String>> {
        const Q: &str = r#"<?xml version="1.0" encoding="utf-8"?><D:propfind xmlns:D="DAV:" xmlns:CS="http://calendarserver.org/ns/"><D:prop><CS:getctag/></D:prop></D:propfind>"#;
        let ms = self.dav(propfind(), coll, "0", Q).await?;
        Ok(ms.iter().find_map(|r| r.prop(NS_CS, "getctag").map(|c| c.text_trim().to_string()).filter(|c| !c.is_empty())))
    }

    // ---- sync -----------------------------------------------------------------------------

    /// Two-way sync of one CalDAV calendar (`cal.url` is the collection URL).
    /// Order: ctag check -> pull changes/deletions (server wins) -> push local-only/dirty events and queued deletions.
    /// The outcome (error text or success time) is recorded in the calendar's sync state.
    pub async fn sync_calendar(&self, store: &Mutex<Calendar>, cal: &CalendarInfo) -> Result<SyncSummary> {
        let res = self.sync_inner(store, cal).await;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let _ = with(store, |c| {
            let st = c.get_calendar_sync_state(&cal.id)?;
            match &res {
                Ok((_, ctag)) => c.set_calendar_sync_state(&cal.id, ctag.as_deref().or(st.ctag.as_deref()), st.sync_marker.as_deref(), None, Some(now)),
                Err(e) => c.set_calendar_sync_state(&cal.id, st.ctag.as_deref(), st.sync_marker.as_deref(), Some(&e.to_string()), st.last_sync),
            }
        });
        res.map(|(s, _)| s)
    }

    async fn sync_inner(&self, store: &Mutex<Calendar>, cal: &CalendarInfo) -> Result<(SyncSummary, Option<String>)> {
        let coll = self.collection(cal.url.as_deref().ok_or_else(|| CalDavError::Config("calendar has no URL".into()))?)?;
        let id = cal.id.as_str();
        let mut sum = SyncSummary::default();
        let local_ctag = with(store, |c| c.get_calendar_sync_state(id))?.ctag;
        let ctag = self.server_ctag(&coll).await?;
        let mut vanished_dirty: HashSet<String> = HashSet::new();

        if ctag.is_none() || ctag != local_ctag {
            let server = self.list_server(&coll).await?;
            let (refs, dirty) = with(store, |c| Ok((c.list_event_refs(id)?, load_dirty(c, id))))?;
            // norm href -> (uids, etag)
            let mut local: HashMap<String, (Vec<String>, Option<String>)> = HashMap::new();
            for r in &refs {
                if let Some(h) = &r.href {
                    let e = local.entry(norm_href(h)).or_default();
                    e.0.push(r.uid.clone());
                    if e.1.is_none() {
                        e.1 = r.etag.clone();
                    }
                }
            }
            let server_norm: HashSet<String> = server.iter().map(|(h, _)| norm_href(h)).collect();
            let mut changed = Vec::new();
            let mut cleared: Vec<String> = Vec::new();
            for (h, e) in &server {
                match local.get(&norm_href(h)) {
                    Some((_, le)) if le.as_deref() == Some(e.as_str()) => {}
                    Some((uids, _)) => {
                        for u in uids.iter().filter(|u| dirty.contains(u)) {
                            sum.conflicts += 1;
                            cleared.push(u.clone());
                        }
                        changed.push(h.clone());
                    }
                    None => changed.push(h.clone()),
                }
            }
            sum.pulled = self.multiget(store, cal, &coll, &changed).await?;

            // deletions on the server
            let mut gone: Vec<String> = Vec::new();
            for (nh, (uids, _)) in &local {
                if !server_norm.contains(nh) {
                    if uids.iter().any(|u| dirty.contains(u)) {
                        vanished_dirty.extend(uids.iter().cloned());
                    } else {
                        gone.push(nh.clone());
                    }
                }
            }
            if !gone.is_empty() || !cleared.is_empty() {
                sum.deleted += with(store, |c| {
                    let mut n = 0;
                    for r in c.list_event_refs(id)? {
                        if r.href.as_deref().map(norm_href).map(|h| gone.contains(&h)).unwrap_or(false) && c.sync_delete_event(&r.uid, id)? {
                            n += 1;
                        }
                    }
                    let mut v = load_dirty(c, id);
                    v.retain(|u| !cleared.contains(u));
                    save_dirty(c, id, &v)?;
                    Ok(n)
                })?;
            }
        }

        if !cal.read_only {
            self.push_pending(store, cal, &coll, &vanished_dirty, &mut sum).await?;
        }
        Ok((sum, ctag))
    }

    async fn push_pending(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, coll: &Url, vanished: &HashSet<String>, sum: &mut SyncSummary) -> Result<()> {
        let id = cal.id.as_str();
        let mut first_err: Option<CalDavError> = None;
        // queued remote deletions: one bad item never blocks the rest
        let tombs = with(store, |c| Ok(load_tombs(c, id)))?;
        let mut kept: Vec<(String, Option<String>)> = Vec::new();
        let mut abort: Option<CalDavError> = None;
        for (i, (href, etag)) in tombs.iter().enumerate() {
            let res = async {
                let url = self.resolve(coll, href)?;
                let mut hdrs = vec![];
                if let Some(e) = etag {
                    hdrs.push(("If-Match", e.as_str()));
                }
                let r = self.send(Method::DELETE, &url, &hdrs, None).await?;
                match r.status {
                    200..=299 | 404 | 410 => Ok(true),
                    412 => Ok(false),
                    c => Err(status_err(c)),
                }
            }
            .await;
            match res {
                Ok(true) => sum.deleted += 1,
                Ok(false) => sum.conflicts += 1, // changed on the server: server wins, drop the tombstone
                Err(e) => match classify(&e) {
                    Failure::Abort => {
                        kept.extend(tombs[i..].iter().cloned());
                        abort = Some(e);
                        break;
                    }
                    Failure::Transient => {
                        kept.push((href.clone(), etag.clone()));
                        first_err.get_or_insert(e);
                    }
                    // permanent (4xx, URL policy): retrying can never succeed -> drop the tombstone
                    Failure::Permanent => {
                        first_err.get_or_insert(e);
                    }
                },
            }
        }
        with(store, |c| save_tombs(c, id, &kept))?;
        if let Some(e) = abort {
            return Err(e);
        }
        // events
        let (refs, dirty) = with(store, |c| Ok((c.list_event_refs(id)?, load_dirty(c, id))))?;
        let mut uids: Vec<String> = refs.iter().filter(|r| r.href.is_none() || dirty.contains(&r.uid)).map(|r| r.uid.clone()).collect();
        uids.dedup();
        for uid in uids {
            let Some(ev) = with(store, |c| c.get_event(&uid, id))? else {
                continue;
            };
            let fresh = ev.href.is_none() || vanished.contains(&uid);
            // MiraMail sends the iMIP mails itself, so the server must not schedule on its own (RFC 6638).
            let built = lock(store).resource_ics_for_put_opts(&uid, id, true);
            let body = match built {
                Ok(b) => b,
                Err(e) => {
                    // Strict build refused (over-cap data): keep the event dirty, keep syncing the rest, report at the end.
                    first_err.get_or_insert(map_build_err(e));
                    continue;
                }
            };
            let if_match = if fresh { None } else { ev.etag.as_deref() };
            match self.put_body(coll, ev.href.as_deref(), &uid, &body, if_match).await {
                Ok(o) => {
                    self.record_put(store, cal, ev, &body, &o)?;
                    sum.pushed += 1;
                }
                Err(CalDavError::PreconditionFailed) => {
                    let href = ev.href.clone().unwrap_or_else(|| format!("{}{}.ics", coll.path(), safe_name(&uid)));
                    self.refetch(store, cal, coll, &href).await?;
                    with(store, |c| {
                        let mut v = load_dirty(c, id);
                        v.retain(|u| u != &uid);
                        save_dirty(c, id, &v)
                    })?;
                    sum.conflicts += 1;
                }
                Err(e) => match classify(&e) {
                    Failure::Abort => return Err(e),
                    // The event stays dirty (nothing is lost); the others still go out.
                    Failure::Transient | Failure::Permanent => {
                        let _ = mark_dirty(store, id, &uid);
                        first_err.get_or_insert(e);
                    }
                },
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// How one failed request affects the rest of a push pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// The whole server/session is unusable (auth, network, TLS): stop and keep everything queued.
    Abort,
    /// Might work next time (5xx, 408, 429): keep the item, continue with the others.
    Transient,
    /// Will never work (other 4xx, URL policy, bad config): drop a tombstone / skip the event, continue.
    Permanent,
}

fn classify(e: &CalDavError) -> Failure {
    match e {
        CalDavError::Unauthorized | CalDavError::Offline | CalDavError::Timeout | CalDavError::Tls | CalDavError::TooManyRedirects => Failure::Abort,
        CalDavError::BadResponse(m) if m == "network error" => Failure::Abort,
        CalDavError::Http(c) if *c >= 500 || *c == 408 || *c == 429 => Failure::Transient,
        CalDavError::BadResponse(_) | CalDavError::TooLarge => Failure::Transient,
        _ => Failure::Permanent,
    }
}

fn safe_name(uid: &str) -> String {
    let s: String = uid.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).take(120).collect();
    if s.is_empty() || s.chars().all(|c| c == '.') {
        "event".into()
    } else {
        s
    }
}

/// Store a pulled resource. `upsert_ics_opts` is the sync path and works on read-only calendars;
/// `trust_zoom` comes from [`zoom_trusted`].
fn store_resource(c: &Calendar, cal_id: &str, href: &str, etag: Option<&str>, body: &[u8], trust_zoom: bool) -> calendar::Result<()> {
    let uids = c.upsert_ics_opts(cal_id, body, Some(href), etag, ParseOptions { trust_zoom_props: trust_zoom })?;
    let nh = norm_href(href);
    for r in c.list_event_refs(cal_id)? {
        if r.href.as_deref().map(norm_href).as_deref() == Some(nh.as_str()) && !uids.contains(&r.uid) {
            c.sync_delete_event(&r.uid, cal_id)?;
        }
    }
    Ok(())
}

fn remove_href(c: &Calendar, cal_id: &str, href: &str) -> calendar::Result<usize> {
    let nh = norm_href(href);
    let mut n = 0;
    for r in c.list_event_refs(cal_id)? {
        if r.href.as_deref().map(norm_href).as_deref() == Some(nh.as_str()) && c.sync_delete_event(&r.uid, cal_id)? {
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{serve, Mock, Req, Resp};
    use calendar::{CalendarKind, NewCalendar};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn ics(uid: &str, summary: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261106T100000Z\r\nDTEND:20261106T110000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
    }

    fn client(mock: &Mock, path: &str) -> CalDavClient {
        CalDavClient::new(Config { base_url: mock.url(path), username: "u".into(), password: "p".into(), allow_insecure_loopback: true }).unwrap()
    }

    fn setup(url: &str) -> (Mutex<Calendar>, CalendarInfo) {
        let c = Calendar::open_in_memory().unwrap();
        let info = c
            .create_calendar(&NewCalendar { id: Some("c1".into()), name: "W".into(), color: "#112233".into(), kind: CalendarKind::Caldav, url: Some(url.into()), username: Some("u".into()), read_only: false })
            .unwrap();
        (Mutex::new(c), info)
    }

    fn local_event(uid: &str, summary: &str) -> CalEvent {
        CalEvent { uid: uid.into(), calendar_id: "c1".into(), summary: summary.into(), start: "2026-11-07T10:00:00+00:00".into(), end: "2026-11-07T11:00:00+00:00".into(), ..Default::default() }
    }

    // ---- fake CalDAV collection at /cal/work/ ----------------------------------------------

    #[derive(Default)]
    struct Fake {
        res: BTreeMap<String, (String, u32)>, // name -> (ics, etag counter)
        ctag: u32,
        counter: u32,
        puts: Vec<(String, Option<String>, Option<String>)>, // name, if-match, if-none-match
        deletes: Vec<(String, Option<String>)>,
        bump_on_write: bool,
    }

    impl Fake {
        fn put(&mut self, name: &str, body: String) {
            self.counter += 1;
            self.res.insert(name.into(), (body, self.counter));
            self.ctag += 1;
        }
        fn etag(n: u32) -> String {
            format!("\"e{n}\"")
        }
    }

    async fn fake_server() -> (Mock, Arc<Mutex<Fake>>) {
        let st = Arc::new(Mutex::new(Fake { bump_on_write: true, ..Default::default() }));
        let s2 = st.clone();
        let mock = serve(move |r: &Req| {
            if r.header("authorization").map(|a| a.starts_with("Basic ")) != Some(true) {
                return Resp::new(401, "");
            }
            let mut f = s2.lock().unwrap();
            let name = r.path.strip_prefix("/cal/work/").map(String::from);
            match (r.method.as_str(), name.as_deref()) {
                ("PROPFIND", Some("")) => Resp::new(
                    207,
                    &format!(
                        r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/"><d:response><d:href>/cal/work/</d:href><d:propstat><d:prop><cs:getctag>ct-{}</cs:getctag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
                        f.ctag
                    ),
                ),
                ("REPORT", Some("")) if r.body.contains("calendar-query") => {
                    let mut x = String::from(r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/cal/work/</D:href><D:propstat><D:prop><D:getetag>"coll"</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#);
                    for (n, (_, e)) in &f.res {
                        x.push_str(&format!(r#"<D:response><D:href>/cal/work/{n}</D:href><D:propstat><D:prop><D:getetag>{}</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"#, Fake::etag(*e)));
                    }
                    x.push_str("</D:multistatus>");
                    Resp::new(207, &x)
                }
                ("REPORT", Some("")) if r.body.contains("calendar-multiget") => {
                    // default-namespace variant on purpose
                    let mut x = String::from(r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">"#);
                    for part in r.body.split("<D:href>").skip(1) {
                        let h = part.split("</D:href>").next().unwrap();
                        let n = h.strip_prefix("/cal/work/").unwrap_or("");
                        if let Some((data, e)) = f.res.get(n) {
                            let esc = data.replace('&', "&amp;").replace('<', "&lt;");
                            x.push_str(&format!("<response><href>{h}</href><propstat><prop><getetag>{}</getetag><C:calendar-data>{esc}</C:calendar-data></prop><status>HTTP/1.1 200 OK</status></propstat></response>", Fake::etag(*e)));
                        } else {
                            x.push_str(&format!("<response><href>{h}</href><status>HTTP/1.1 404 Not Found</status></response>"));
                        }
                    }
                    x.push_str("</multistatus>");
                    Resp::new(207, &x)
                }
                ("GET", Some(n)) => match f.res.get(n) {
                    Some((d, e)) => Resp::new(200, d).with("ETag", &Fake::etag(*e)),
                    None => Resp::new(404, ""),
                },
                ("PUT", Some(n)) => {
                    let im = r.header("if-match").map(String::from);
                    let inm = r.header("if-none-match").map(String::from);
                    f.puts.push((n.into(), im.clone(), inm.clone()));
                    let cur = f.res.get(n).map(|(_, e)| Fake::etag(*e));
                    if inm.as_deref() == Some("*") && cur.is_some() || im.is_some() && im != cur {
                        return Resp::new(412, "");
                    }
                    let bump = f.bump_on_write;
                    f.counter += 1;
                    let c = f.counter;
                    f.res.insert(n.into(), (r.body.clone(), c));
                    if bump {
                        f.ctag += 1;
                    }
                    Resp::new(201, "").with("ETag", &Fake::etag(c))
                }
                ("DELETE", Some(n)) => {
                    let im = r.header("if-match").map(String::from);
                    f.deletes.push((n.into(), im.clone()));
                    match f.res.get(n).map(|(_, e)| Fake::etag(*e)) {
                        None => Resp::new(404, ""),
                        Some(cur) if im.is_some() && im.as_deref() != Some(cur.as_str()) => Resp::new(412, ""),
                        Some(_) => {
                            f.res.remove(n);
                            f.ctag += 1;
                            Resp::new(204, "")
                        }
                    }
                }
                _ => Resp::new(404, ""),
            }
        })
        .await;
        (mock, st)
    }

    #[tokio::test]
    async fn sync_pull_skip_and_delete() {
        let (mock, fake) = fake_server().await;
        {
            let mut f = fake.lock().unwrap();
            for i in 0..3 {
                f.put(&format!("e{i}.ics"), ics(&format!("u{i}@t"), &format!("Ev{i}")));
            }
        }
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!((s.pulled, s.pushed, s.deleted, s.conflicts), (3, 0, 0, 0));
        {
            let g = store.lock().unwrap();
            assert_eq!(g.list_events("c1").unwrap().len(), 3);
            let st = g.get_calendar_sync_state("c1").unwrap();
            assert_eq!(st.ctag.as_deref(), Some("ct-3"));
            assert!(st.last_sync.is_some() && st.error.is_none());
        }
        // unchanged ctag: no REPORT at all
        let before = mock.requests().len();
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!(s, SyncSummary::default());
        let new: Vec<_> = mock.requests().into_iter().skip(before).collect();
        assert!(new.iter().all(|r| r.method != "REPORT"), "{new:?}");
        // server edits one, deletes one
        {
            let mut f = fake.lock().unwrap();
            f.put("e0.ics", ics("u0@t", "Changed"));
            f.res.remove("e2.ics");
            f.ctag += 1;
        }
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!((s.pulled, s.deleted), (1, 1));
        let g = store.lock().unwrap();
        assert_eq!(g.get_event("u0@t", "c1").unwrap().unwrap().summary, "Changed");
        assert!(g.get_event("u2@t", "c1").unwrap().is_none());
        assert_eq!(g.list_events("c1").unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sync_push_new_and_dirty_then_tombstone() {
        let (mock, fake) = fake_server().await;
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        store.lock().unwrap().upsert_event(&local_event("new@miramail", "Mine")).unwrap();
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!((s.pulled, s.pushed), (0, 1));
        {
            let f = fake.lock().unwrap();
            assert_eq!(f.puts[0], ("new_miramail.ics".into(), None, Some("*".into())));
            assert!(f.res["new_miramail.ics"].0.contains("SUMMARY:Mine"));
        }
        let ev = store.lock().unwrap().get_event("new@miramail", "c1").unwrap().unwrap();
        assert_eq!(ev.href.as_deref(), Some("/cal/work/new_miramail.ics"));
        let etag1 = ev.etag.clone().unwrap();
        // edit + dirty -> If-Match
        let mut ev2 = ev.clone();
        ev2.summary = "Mine v2".into();
        store.lock().unwrap().upsert_event(&ev2).unwrap();
        mark_dirty(&store, "c1", "new@miramail").unwrap();
        assert_eq!(pending_changes(&store, "c1").unwrap(), 1);
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!((s.pushed, s.conflicts), (1, 0));
        assert_eq!(fake.lock().unwrap().puts[1].1.as_deref(), Some(etag1.as_str()));
        assert_eq!(pending_changes(&store, "c1").unwrap(), 0);
        // local delete -> tombstone -> DELETE with If-Match
        let ev = store.lock().unwrap().get_event("new@miramail", "c1").unwrap().unwrap();
        store.lock().unwrap().delete_event("new@miramail", "c1").unwrap();
        mark_deleted(&store, "c1", ev.href.as_deref().unwrap(), ev.etag.as_deref()).unwrap();
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!(s.deleted, 1);
        assert_eq!(fake.lock().unwrap().deletes[0].1, ev.etag);
        assert!(fake.lock().unwrap().res.is_empty());
        assert_eq!(pending_changes(&store, "c1").unwrap(), 0);
    }

    #[tokio::test]
    async fn push_412_conflict_server_wins() {
        let (mock, fake) = fake_server().await;
        fake.lock().unwrap().put("e0.ics", ics("u0@t", "Original"));
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        cl.sync_calendar(&store, &cal).await.unwrap();
        // server changes behind our back WITHOUT bumping ctag (so pull is skipped), local edit is dirty
        {
            let mut f = fake.lock().unwrap();
            let ct = f.ctag;
            f.put("e0.ics", ics("u0@t", "Server edit"));
            f.ctag = ct;
        }
        let mut ev = store.lock().unwrap().get_event("u0@t", "c1").unwrap().unwrap();
        ev.summary = "Local edit".into();
        store.lock().unwrap().upsert_event(&ev).unwrap();
        mark_dirty(&store, "c1", "u0@t").unwrap();
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!((s.pushed, s.conflicts), (0, 1));
        assert_eq!(store.lock().unwrap().get_event("u0@t", "c1").unwrap().unwrap().summary, "Server edit");
        assert_eq!(pending_changes(&store, "c1").unwrap(), 0);
        // push_event path: same thing
        {
            let mut f = fake.lock().unwrap();
            f.put("e0.ics", ics("u0@t", "Server edit 2"));
        }
        let mut ev = store.lock().unwrap().get_event("u0@t", "c1").unwrap().unwrap();
        ev.summary = "Local again".into();
        store.lock().unwrap().upsert_event(&ev).unwrap();
        assert_eq!(cl.push_event(&store, &cal, "u0@t").await.unwrap(), PushOutcome::Conflict);
        assert_eq!(store.lock().unwrap().get_event("u0@t", "c1").unwrap().unwrap().summary, "Server edit 2");
    }

    #[tokio::test]
    async fn pull_conflict_when_both_changed_and_push_event_ok() {
        let (mock, fake) = fake_server().await;
        fake.lock().unwrap().put("e0.ics", ics("u0@t", "Original"));
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        cl.sync_calendar(&store, &cal).await.unwrap();
        fake.lock().unwrap().put("e0.ics", ics("u0@t", "Server"));
        mark_dirty(&store, "c1", "u0@t").unwrap();
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!((s.pulled, s.conflicts, s.pushed), (1, 1, 0));
        // immediate write
        let mut ev = store.lock().unwrap().get_event("u0@t", "c1").unwrap().unwrap();
        ev.summary = "Immediate".into();
        store.lock().unwrap().upsert_event(&ev).unwrap();
        assert!(matches!(cl.push_event(&store, &cal, "u0@t").await.unwrap(), PushOutcome::Pushed(_)));
        assert!(fake.lock().unwrap().res["e0.ics"].0.contains("Immediate"));
        // delete helper: 404 is success
        cl.delete_event(&mock.url("/cal/work/"), "/cal/work/missing.ics", None).await.unwrap();
        // push_event failure marks dirty
        let dead = CalDavClient::new(Config { base_url: "http://127.0.0.1:1/".into(), username: "u".into(), password: "p".into(), allow_insecure_loopback: true }).unwrap();
        let (st2, mut cal2) = setup("http://127.0.0.1:1/cal/work/");
        cal2.url = Some("http://127.0.0.1:1/cal/work/".into());
        st2.lock().unwrap().upsert_event(&local_event("x@t", "X")).unwrap();
        assert_eq!(dead.push_event(&st2, &cal2, "x@t").await.unwrap_err(), CalDavError::Offline);
        assert_eq!(pending_changes(&st2, "c1").unwrap(), 1);
    }

    #[tokio::test]
    async fn server_deleted_dirty_event_is_recreated() {
        let (mock, fake) = fake_server().await;
        fake.lock().unwrap().put("e0.ics", ics("u0@t", "Original"));
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        cl.sync_calendar(&store, &cal).await.unwrap();
        {
            let mut f = fake.lock().unwrap();
            f.res.clear();
            f.ctag += 1;
        }
        mark_dirty(&store, "c1", "u0@t").unwrap();
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!((s.pushed, s.deleted), (1, 0));
        assert_eq!(fake.lock().unwrap().puts.last().unwrap().2.as_deref(), Some("*"));
    }

    #[tokio::test]
    async fn auth_failure_is_recorded_and_friendly() {
        let mock = serve(|_| Resp::new(401, "")).await;
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        let e = cl.sync_calendar(&store, &cal).await.unwrap_err();
        assert_eq!(e, CalDavError::Unauthorized);
        assert!(e.to_string().contains("app-specific password"));
        let st = store.lock().unwrap().get_calendar_sync_state("c1").unwrap();
        assert!(st.error.unwrap().contains("Wrong username or password"));
        for (code, want) in [(403u16, CalDavError::Forbidden), (404, CalDavError::NotFound), (500, CalDavError::Http(500))] {
            let m = serve(move |_| Resp::new(code, "")).await;
            assert_eq!(client(&m, "/").discover().await.unwrap_err(), want);
        }
    }

    // ---- hardening: read-only, PUT preservation, zoom trust, error mapping ------------------

    fn setup_with(url: &str, read_only: bool) -> (Mutex<Calendar>, CalendarInfo) {
        let c = Calendar::open_in_memory().unwrap();
        let info = c
            .create_calendar(&NewCalendar { id: Some("c1".into()), name: "W".into(), color: "#112233".into(), kind: CalendarKind::Caldav, url: Some(url.into()), username: Some("u".into()), read_only })
            .unwrap();
        (Mutex::new(c), info)
    }

    fn zoom_ics(uid: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261106T100000Z\r\nDTEND:20261106T110000Z\r\nSUMMARY:Z\r\nX-MIRAMAIL-ZOOM-ID:123\r\nX-MIRAMAIL-ZOOM-URL:https://zoom.us/j/123\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
    }

    /// Server resource with data `CalEvent` cannot express: X- props, a second VALARM, ATTACH, RRULE and an override.
    fn rich_ics(uid: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//srv//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261106T100000Z\r\nDTEND:20261106T110000Z\r\nRRULE:FREQ=DAILY;COUNT=5\r\nSUMMARY:Orig\r\nX-SERVER-PROP;X-PARAM=1:keepme\r\nATTACH:https://example.com/a.pdf\r\nCLASS:PRIVATE\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nDESCRIPTION:a\r\nEND:VALARM\r\nBEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER:-PT1H\r\nX-ALARM-EXTRA:yes\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nRECURRENCE-ID:20261107T100000Z\r\nDTSTART:20261107T120000Z\r\nDTEND:20261107T130000Z\r\nSUMMARY:Moved\r\nX-OVERRIDE-PROP:keep2\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    #[tokio::test]
    async fn read_only_calendar_still_syncs_down() {
        let (mock, fake) = fake_server().await;
        {
            let mut f = fake.lock().unwrap();
            f.put("e0.ics", rich_ics("r0@t"));
            f.put("e1.ics", ics("r1@t", "Two"));
        }
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup_with(&mock.url("/cal/work/"), true);
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!((s.pulled, s.pushed, s.deleted), (2, 0, 0));
        {
            let g = store.lock().unwrap();
            assert_eq!(g.list_events("c1").unwrap().len(), 2);
            assert_eq!(g.get_overrides("r0@t", "c1").unwrap().len(), 1);
            // direct user writes are refused...
            assert!(g.upsert_event(&local_event("x@t", "X")).is_err());
            assert!(g.delete_event("r1@t", "c1").is_err());
        }
        // ...but a server-side change / delete is applied
        {
            let mut f = fake.lock().unwrap();
            f.put("e0.ics", ics("r0@t", "Changed"));
            f.res.remove("e1.ics");
            f.ctag += 1;
        }
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!((s.pulled, s.deleted), (1, 1));
        let g = store.lock().unwrap();
        assert_eq!(g.get_event("r0@t", "c1").unwrap().unwrap().summary, "Changed");
        assert!(g.get_overrides("r0@t", "c1").unwrap().is_empty());
        assert!(g.get_event("r1@t", "c1").unwrap().is_none());
        assert!(fake.lock().unwrap().puts.is_empty(), "never writes to a read-only calendar");
    }

    #[tokio::test]
    async fn push_preserves_unknown_properties_and_overrides() {
        let (mock, fake) = fake_server().await;
        fake.lock().unwrap().put("e0.ics", rich_ics("rich@t"));
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        cl.sync_calendar(&store, &cal).await.unwrap();
        let mut ev = store.lock().unwrap().get_event("rich@t", "c1").unwrap().unwrap();
        ev.summary = "Edited".into();
        store.lock().unwrap().upsert_event(&ev).unwrap();
        assert!(matches!(cl.push_event(&store, &cal, "rich@t").await.unwrap(), PushOutcome::Pushed(_)));
        let body = fake.lock().unwrap().res["e0.ics"].0.clone();
        for keep in ["X-SERVER-PROP;X-PARAM=1:keepme", "ATTACH:https://example.com/a.pdf", "CLASS:PRIVATE", "X-ALARM-EXTRA:yes", "ACTION:AUDIO", "RECURRENCE-ID", "X-OVERRIDE-PROP:keep2", "SUMMARY:Moved", "SUMMARY:Edited"] {
            assert!(body.contains(keep), "lost {keep} in:\n{body}");
        }
        assert!(!body.contains("SUMMARY:Orig"));
        // the PUT body became the new original: a second edit still preserves everything
        let mut ev = store.lock().unwrap().get_event("rich@t", "c1").unwrap().unwrap();
        ev.summary = "Edited2".into();
        store.lock().unwrap().upsert_event(&ev).unwrap();
        cl.push_event_opts(&store, &cal, "rich@t", true).await.unwrap();
        let body = fake.lock().unwrap().res["e0.ics"].0.clone();
        assert!(body.contains("X-SERVER-PROP") && body.contains("X-OVERRIDE-PROP:keep2") && body.contains("SUMMARY:Edited2"), "{body}");
        // sync-driven push (dirty) goes through the same patched path
        let mut ev = store.lock().unwrap().get_event("rich@t", "c1").unwrap().unwrap();
        ev.summary = "Edited3".into();
        store.lock().unwrap().upsert_event(&ev).unwrap();
        mark_dirty(&store, "c1", "rich@t").unwrap();
        let s = cl.sync_calendar(&store, &cal).await.unwrap();
        assert_eq!(s.pushed, 1);
        let body = fake.lock().unwrap().res["e0.ics"].0.clone();
        assert!(body.contains("ATTACH:") && body.contains("SUMMARY:Edited3"), "{body}");
    }

    #[tokio::test]
    async fn push_412_still_conflicts_with_patched_body() {
        let (mock, fake) = fake_server().await;
        fake.lock().unwrap().put("e0.ics", rich_ics("c412@t"));
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        cl.sync_calendar(&store, &cal).await.unwrap();
        let mut ev = store.lock().unwrap().get_event("c412@t", "c1").unwrap().unwrap();
        ev.summary = "Mine".into();
        store.lock().unwrap().upsert_event(&ev).unwrap();
        fake.lock().unwrap().put("e0.ics", ics("c412@t", "Theirs"));
        assert_eq!(cl.push_event(&store, &cal, "c412@t").await.unwrap(), PushOutcome::Conflict);
        assert_eq!(store.lock().unwrap().get_event("c412@t", "c1").unwrap().unwrap().summary, "Theirs");
        assert_eq!(pending_changes(&store, "c1").unwrap(), 0);
    }

    #[tokio::test]
    async fn zoom_props_trusted_only_on_own_writable_calendars() {
        for (read_only, expect_zoom) in [(false, true), (true, false)] {
            let (mock, fake) = fake_server().await;
            fake.lock().unwrap().put("z.ics", zoom_ics("z@t"));
            let cl = client(&mock, "/cal/work/");
            let (store, cal) = setup_with(&mock.url("/cal/work/"), read_only);
            set_owned(&store, "c1", true).unwrap();
            assert_eq!(zoom_trusted(&cal), expect_zoom);
            cl.sync_calendar(&store, &cal).await.unwrap();
            let ev = store.lock().unwrap().get_event("z@t", "c1").unwrap().unwrap();
            assert_eq!(ev.zoom.is_some(), expect_zoom, "read_only={read_only}");
            assert!(ev.zoom.map(|z| z.start_url.is_none()).unwrap_or(true));
        }
        // local calendars are never trusted
        let mut local = setup("http://x/").1;
        local.kind = CalendarKind::Local;
        assert!(!zoom_trusted(&local));
        // a refetch (after a 412) uses the same trust decision
        let (mock, fake) = fake_server().await;
        fake.lock().unwrap().put("z.ics", zoom_ics("z@t"));
        let (store, cal) = setup_with(&mock.url("/cal/work/"), true);
        let cl = client(&mock, "/cal/work/");
        let coll = cl.collection(&mock.url("/cal/work/")).unwrap();
        cl.refetch(&store, &cal, &coll, "/cal/work/z.ics").await.unwrap();
        assert!(store.lock().unwrap().get_event("z@t", "c1").unwrap().unwrap().zoom.is_none());
    }

    #[test]
    fn strict_build_errors_get_friendly_messages() {
        use calendar::{try_build_ics, Attendee, AttendeeRole, AttendeeStatus};
        let mut ev = local_event("big@t", "Big");
        ev.rrule = Some(format!("FREQ=DAILY;BYDAY={}", "MO,".repeat(2000)));
        let e = map_build_err(try_build_ics(&ev, None).unwrap_err());
        assert_eq!(e, CalDavError::Rejected("its repeat rule is too long".into()));
        ev.rrule = None;
        ev.attendees = (0..=calendar::MAX_ATTENDEES)
            .map(|i| Attendee { email: format!("a{i}@x.org"), name: None, role: AttendeeRole::Required, status: AttendeeStatus::NeedsAction })
            .collect();
        let e = map_build_err(try_build_ics(&ev, None).unwrap_err());
        assert!(matches!(&e, CalDavError::Rejected(m) if m.contains("too many attendees")), "{e:?}");
        assert!(e.to_string().starts_with("This event can't be saved"));
        let e = map_build_err(calendar::Error::TooLarge { max: calendar::MAX_ICS_BYTES });
        assert!(matches!(e, CalDavError::Rejected(_)));
        assert!(matches!(map_build_err(calendar::Error::NotFound("event".into())), CalDavError::Store(_)));
    }

    #[tokio::test]
    async fn put_raw_addressing_if_match_and_size_cap() {
        let (mock, fake) = fake_server().await;
        let cl = client(&mock, "/cal/work/");
        let url = mock.url("/cal/work/");
        // uid -> <coll>/<safe uid>.ics, create-only
        let o = cl.put_raw(&url, "a b@t", &ics("a b@t", "A"), None).await.unwrap();
        assert_eq!(o.href, "/cal/work/a_b_t.ics");
        assert_eq!(fake.lock().unwrap().puts.last().unwrap().2.as_deref(), Some("*"));
        // create-only again -> 412
        assert_eq!(cl.put_raw(&url, "a b@t", &ics("a b@t", "A"), None).await.unwrap_err(), CalDavError::PreconditionFailed);
        // href + If-Match
        let o2 = cl.put_raw(&url, &o.href, &ics("a b@t", "B"), o.etag.as_deref()).await.unwrap();
        assert_eq!(o2.href, o.href);
        assert_eq!(fake.lock().unwrap().puts.last().unwrap().1, o.etag);
        // oversize: refused locally
        let n = fake.lock().unwrap().puts.len();
        let big = "X".repeat(calendar::MAX_ICS_BYTES + 1);
        assert!(matches!(cl.put_raw(&url, "big@t", &big, None).await.unwrap_err(), CalDavError::Rejected(_)));
        assert_eq!(fake.lock().unwrap().puts.len(), n);
    }

    // ---- discovery -------------------------------------------------------------------------

    #[tokio::test]
    async fn discovery_multi_step() {
        let mock = serve(|r: &Req| {
            let depth = r.header("depth").unwrap_or("");
            match (r.method.as_str(), r.path.as_str(), depth) {
                ("PROPFIND", "/.well-known/caldav", _) => Resp::new(301, "").with("Location", "/dav/"),
                ("PROPFIND", "/dav/", "0") => Resp::new(207, r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:"><D:response><D:href>/dav/</D:href><D:propstat><D:prop><D:current-user-principal><D:href>/dav/principals/u/</D:href></D:current-user-principal></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#),
                ("PROPFIND", "/dav/principals/u/", "0") => Resp::new(207, r#"<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/dav/principals/u/</d:href><d:propstat><d:prop><cal:calendar-home-set><d:href>/dav/cals/u/</d:href></cal:calendar-home-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#),
                ("PROPFIND", "/dav/cals/u/", "1") => Resp::new(
                    207,
                    r#"<?xml version="1.0"?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/" xmlns:A="http://apple.com/ns/ical/">
 <response><href>/dav/cals/u/</href><propstat><prop><resourcetype><collection/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response>
 <response><href>/dav/cals/u/work/</href><propstat><prop><resourcetype><collection/><C:calendar/></resourcetype><displayname>Work &amp; Co</displayname><A:calendar-color>#FF8800FF</A:calendar-color><CS:getctag>ctag-9</CS:getctag>
   <C:supported-calendar-component-set><C:comp name="VEVENT"/><C:comp name="VTODO"/></C:supported-calendar-component-set>
   <current-user-privilege-set><privilege><read/></privilege><privilege><write/></privilege></current-user-privilege-set></prop><status>HTTP/1.1 200 OK</status></propstat></response>
 <response><href>/dav/cals/u/tasks/</href><propstat><prop><resourcetype><collection/><C:calendar/></resourcetype><displayname>Tasks</displayname>
   <C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set></prop><status>HTTP/1.1 200 OK</status></propstat></response>
 <response><href>/dav/cals/u/holidays/</href><propstat><prop><resourcetype><collection/><C:calendar/></resourcetype><displayname>Holidays</displayname>
   <C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set>
   <current-user-privilege-set><privilege><read/></privilege></current-user-privilege-set></prop><status>HTTP/1.1 200 OK</status></propstat></response>
 <response><href>/dav/cals/u/inbox/</href><propstat><prop><resourcetype><collection/><C:schedule-inbox/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response>
 <response><href>https://evil.example/steal/</href><propstat><prop><resourcetype><collection/><C:calendar/></resourcetype></prop><status>HTTP/1.1 200 OK</status></propstat></response>
</multistatus>"#,
                ),
                _ => Resp::new(404, ""),
            }
        })
        .await;
        let cals = client(&mock, "/").discover().await.unwrap();
        assert_eq!(cals.len(), 2, "{cals:?}");
        assert_eq!(cals[0].name, "Work & Co");
        assert_eq!(cals[0].url, mock.url("/dav/cals/u/work/"));
        assert_eq!(cals[0].color.as_deref(), Some("#ff8800"));
        assert_eq!(cals[0].ctag.as_deref(), Some("ctag-9"));
        assert!(!cals[0].read_only);
        assert_eq!(cals[1].name, "Holidays");
        assert!(cals[1].read_only);
        assert!(cals[1].color.is_none());
        let reqs = mock.requests();
        assert_eq!(reqs[0].path, "/.well-known/caldav");
        assert!(reqs.iter().all(|r| r.header("authorization").is_some()));
    }

    #[test]
    fn presets() {
        assert_eq!(preset_url("icloud", "a@icloud.com").unwrap(), "https://caldav.icloud.com");
        assert_eq!(preset_url("fastmail", "a").unwrap(), "https://caldav.fastmail.com");
        assert_eq!(preset_url("zoho", "a").unwrap(), "https://calendar.zoho.com/");
        assert!(preset_url("custom", "a").is_none());
    }


    // ---- region / discovery order / diagnostics --------------------------------------------

    #[test]
    fn zoho_region_urls_and_inference() {
        assert_eq!(preset_url_region("zoho", "a", None).unwrap(), "https://calendar.zoho.com/");
        assert_eq!(preset_url_region("zoho", "a", Some("eu")).unwrap(), "https://calendar.zoho.eu/");
        assert_eq!(preset_url_region("zoho", "a", Some("IN")).unwrap(), "https://calendar.zoho.in/");
        assert_eq!(preset_url_region("zoho", "a", Some("com.au")).unwrap(), "https://calendar.zoho.com.au/");
        assert!(preset_url_region("zoho", "a", Some("evil.example/x#")).is_none());
        assert_eq!(preset_url_region("icloud", "a", Some("eu")).unwrap(), "https://caldav.icloud.com");
        assert_eq!(zoho_region_from_host("imappro.zoho.in"), Some("in"));
        assert_eq!(zoho_region_from_host("smtp.zoho.com.au"), Some("com.au"));
        assert_eq!(zoho_region_from_host("imap.zoho.eu"), Some("eu"));
        assert_eq!(zoho_region_from_host("imap.zoho.com"), Some("com"));
        assert_eq!(zoho_region_from_host("imap.notzoho.com"), None);
        assert_eq!(zoho_region_from_host("imap.fastmail.com"), None);
    }

    const HOME_Q: &str = r#"<d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/c/</d:href><d:propstat><d:prop><cal:calendar-home-set><d:href>/c/u/</d:href></cal:calendar-home-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    const ONE_CAL: &str = r#"<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><response><href>/c/u/main/</href><propstat><prop><resourcetype><collection/><C:calendar/></resourcetype><displayname>Main</displayname></prop><status>HTTP/1.1 200 OK</status></propstat></response></multistatus>"#;

    #[tokio::test]
    async fn discovery_falls_back_from_404_well_known_to_root_then_caldav_dir() {
        // well-known 404, root 404, /caldav/ works
        let mock = serve(|r: &Req| match (r.method.as_str(), r.path.as_str(), r.header("depth").unwrap_or("")) {
            ("PROPFIND", "/caldav/", "0") => Resp::new(207, HOME_Q),
            ("PROPFIND", "/c/u/", "1") => Resp::new(207, ONE_CAL),
            _ => Resp::new(404, ""),
        })
        .await;
        let diag = Diag::new();
        let cals = client(&mock, "/").with_diag(diag.clone()).discover().await.unwrap();
        assert_eq!(cals.len(), 1);
        let paths: Vec<String> = mock.requests().iter().map(|r| r.path.clone()).take(3).collect();
        assert_eq!(paths, ["/.well-known/caldav", "/", "/caldav/"]);
        let lines = diag.lines().join("\n");
        assert!(lines.contains("HTTP 404") && lines.contains("HTTP 207") && lines.contains("Found 1 calendar"), "{lines}");
    }

    #[tokio::test]
    async fn discovery_from_root_when_well_known_is_missing() {
        let mock = serve(|r: &Req| match (r.method.as_str(), r.path.as_str(), r.header("depth").unwrap_or("")) {
            ("PROPFIND", "/", "0") => Resp::new(207, HOME_Q),
            ("PROPFIND", "/c/u/", "1") => Resp::new(207, ONE_CAL),
            _ => Resp::new(404, ""),
        })
        .await;
        assert_eq!(client(&mock, "/").discover().await.unwrap().len(), 1);
        assert_eq!(mock.requests()[1].path, "/");
    }

    #[tokio::test]
    async fn all_paths_404_gives_plain_explanation_without_secrets() {
        let mock = serve(|_| Resp::new(404, "")).await;
        let diag = Diag::new();
        let e = client(&mock, "/").with_diag(diag.clone()).discover().await.unwrap_err();
        assert_eq!(e, CalDavError::NotFound);
        let msg = diag.explain(&e);
        assert!(msg.contains("404") && msg.contains("127.0.0.1"), "{msg}");
        assert_eq!(mock.requests().len(), 3);
    }

    #[tokio::test]
    async fn diagnostics_and_explanations_never_contain_the_password() {
        let secret = "s3cr3t-pw-xyz";
        let cfg = |m: &Mock| Config { base_url: m.url("/"), username: "me@x.org".into(), password: secret.into(), allow_insecure_loopback: true };
        for status in [401u16, 403, 404, 500] {
            let mock = serve(move |_| Resp::new(status, "")).await;
            let diag = Diag::new();
            let e = CalDavClient::new(cfg(&mock)).unwrap().with_diag(diag.clone()).discover().await.unwrap_err();
            let all = format!("{}\n{}\n{e}", diag.lines().join("\n"), diag.explain(&e));
            assert!(!all.contains(secret) && !all.contains("c2VjcjN0"), "{status}: {all}");
        }
        // refused redirect names the host, still no secret
        let mock = serve(|_| Resp::new(302, "").with("Location", "https://evil.example/x")).await;
        let diag = Diag::new();
        let e = CalDavClient::new(cfg(&mock)).unwrap().with_diag(diag.clone()).discover().await.unwrap_err();
        let msg = diag.explain(&e);
        assert!(msg.contains("evil.example") && !msg.contains(secret), "{msg}");
        // 401 explanation mentions the two-factor hint but not "always"
        let m401 = serve(|_| Resp::new(401, "")).await;
        let d = Diag::new();
        let e = client(&m401, "/").with_diag(d.clone()).discover().await.unwrap_err();
        let msg = d.explain(&e);
        assert!(msg.contains("401") && msg.contains("two-factor"), "{msg}");
    }

    #[tokio::test]
    async fn discovery_never_sends_delete() {
        let mock = serve(|r: &Req| match (r.method.as_str(), r.path.as_str(), r.header("depth").unwrap_or("")) {
            ("PROPFIND", "/.well-known/caldav", _) => Resp::new(207, HOME_Q),
            ("PROPFIND", "/c/u/", "1") => Resp::new(207, ONE_CAL),
            _ => Resp::new(404, ""),
        })
        .await;
        client(&mock, "/").discover().await.unwrap();
        assert!(mock.requests().iter().all(|r| r.method == "PROPFIND"));
    }

    // ---- redirect / URL rules --------------------------------------------------------------

    #[tokio::test]
    async fn redirects_cross_site_and_http_are_refused() {
        for target in ["https://evil.example/x", "http://evil.example/x", "https://127.0.0.1.evil.example/x", "ftp://127.0.0.1/x"] {
            let t = target.to_string();
            let mock = serve(move |_| Resp::new(302, "").with("Location", &t)).await;
            let e = client(&mock, "/").discover().await.unwrap_err();
            assert!(matches!(e, CalDavError::CrossSiteRedirect | CalDavError::Insecure), "{target}: {e:?}");
            assert_eq!(mock.requests().len(), 1, "no request may follow: {target}");
        }
    }

    #[tokio::test]
    async fn redirect_loop_is_bounded() {
        let mock = serve(|_| Resp::new(307, "").with("Location", "/again")).await;
        assert_eq!(client(&mock, "/").discover().await.unwrap_err(), CalDavError::TooManyRedirects);
    }

    #[test]
    fn url_policy() {
        let mk = |u: &str, allow: bool| CalDavClient::new(Config { base_url: u.into(), username: "u".into(), password: "p".into(), allow_insecure_loopback: allow });
        assert!(mk("https://caldav.icloud.com", false).is_ok());
        assert_eq!(mk("http://caldav.icloud.com", false).err(), Some(CalDavError::Insecure));
        assert_eq!(mk("http://caldav.icloud.com", true).err(), Some(CalDavError::Insecure));
        assert_eq!(mk("http://127.0.0.1:5/", false).err(), Some(CalDavError::Insecure));
        assert_eq!(mk("http://localhost:5/", true).err(), Some(CalDavError::Insecure));
        assert!(mk("http://127.0.0.1:5/", true).is_ok());
        assert!(mk("http://[::1]:5/", true).is_ok());
        assert!(matches!(mk("https://u:p@host.example/", false).err(), Some(CalDavError::Config(_))));
        assert!(mk("not a url", false).is_err());
        let c = mk("https://caldav.icloud.com", false).unwrap();
        let ok = |s: &str| c.check_url(&Url::parse(s).unwrap());
        assert!(ok("https://p12-caldav.icloud.com/a").is_ok());
        assert_eq!(ok("https://evil-icloud.com/a"), Err(CalDavError::CrossSiteRedirect));
        assert_eq!(ok("https://icloud.com.evil.example/a"), Err(CalDavError::CrossSiteRedirect));
        assert_eq!(ok("http://caldav.icloud.com/a"), Err(CalDavError::Insecure));
        assert_eq!(registrable_domain("a.b.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("a.b.example.com"), "example.com");
        assert_ne!(registrable_domain("x.co.uk"), registrable_domain("y.co.uk"));
        let dbg = format!("{:?}", Config { base_url: "https://h".into(), username: "u".into(), password: "hunter2".into(), allow_insecure_loopback: false });
        assert!(!dbg.contains("hunter2"));
    }

    #[tokio::test]
    async fn oversized_response_is_capped() {
        let mock = serve(|_| Resp::new(207, &"x".repeat(MAX_BODY_BYTES + 1024))).await;
        assert_eq!(client(&mock, "/").discover().await.unwrap_err(), CalDavError::TooLarge);
    }

    // ---- hostile XML -----------------------------------------------------------------------

    #[test]
    fn hostile_xml_never_panics_or_expands() {
        let lol = r#"<?xml version="1.0"?><!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">]><D:multistatus xmlns:D="DAV:"><D:response><D:href>&lol2;</D:href></D:response></D:multistatus>"#;
        assert!(parse_multistatus(lol.as_bytes()).is_err());
        let xxe = r#"<!DOCTYPE x [<!ENTITY e SYSTEM "file:///etc/passwd">]><D:multistatus xmlns:D="DAV:"><D:response><D:href>&e;</D:href></D:response></D:multistatus>"#;
        assert!(parse_multistatus(xxe.as_bytes()).is_err());
        // undeclared entity in text: dropped, never expanded
        let und = r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/a&e;b</D:href></D:response></D:multistatus>"#;
        let r = parse_multistatus(und.as_bytes()).unwrap();
        assert!(r.iter().all(|x| !x.href.contains("passwd")));
        let deep = format!("{}{}", "<a>".repeat(10_000), "</a>".repeat(10_000));
        assert!(parse_xml(deep.as_bytes()).is_err());
        let wide = format!("<D:multistatus xmlns:D=\"DAV:\">{}</D:multistatus>", "<D:x/>".repeat(300_000));
        assert!(parse_xml(wide.as_bytes()).is_err());
        for bad in [
            &b""[..],
            b"\xff\xfe\x00garbage",
            b"<",
            b"<a><b></a>",
            b"<D:multistatus xmlns:D=\"DAV:\"><D:response>",
            b"<unbound:x/>",
            b"<a x=\"1\" x=\"2\"/>",
            b"</a>",
            b"<![CDATA[ unterminated",
            b"<?xml version=\"1.0\"?><x xmlns=\"DAV:\">&#xFFFFFFFF;</x>",
        ] {
            let _ = parse_multistatus(bad);
            let _ = parse_xml(bad);
        }
        // wrong root
        assert!(parse_multistatus(b"<html/>").is_err());
        // prefix / default-namespace variants resolve to the same thing
        for x in [
            r#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>/x</D:href></D:response></D:multistatus>"#,
            r#"<multistatus xmlns="DAV:"><response><href>/x</href></response></multistatus>"#,
            r#"<zz:multistatus xmlns:zz="DAV:"><zz:response><zz:href>/x</zz:href></zz:response></zz:multistatus>"#,
        ] {
            assert_eq!(parse_multistatus(x.as_bytes()).unwrap()[0].href, "/x");
        }
    }

    #[tokio::test]
    async fn hostile_server_response_is_an_error() {
        let mock = serve(|_| Resp::new(207, "<!DOCTYPE x [<!ENTITY a \"b\">]><D:multistatus xmlns:D=\"DAV:\"/>")).await;
        assert!(matches!(client(&mock, "/").discover().await.unwrap_err(), CalDavError::BadResponse(_)));
    }

    #[test]
    fn helpers() {
        assert_eq!(norm_href("https://h.example/a%20b/c/"), "/a b/c");
        assert_eq!(norm_href("/a%20b/c"), "/a b/c");
        assert_eq!(pct_decode("%zz%4"), "%zz%4");
        assert_eq!(normalize_color("#abc").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_color("red"), None);
        assert_eq!(safe_name("a@b/c"), "a_b_c");
        assert_eq!(safe_name(".."), "event");
    }

    // ---- review round 2 --------------------------------------------------------------------

    #[tokio::test]
    async fn q03_zoom_props_ignored_when_calendar_is_not_owned() {
        let (mock, fake) = fake_server().await;
        fake.lock().unwrap().put("z.ics", zoom_ics("z@t"));
        let cl = client(&mock, "/cal/work/");
        // never discovered -> unknown -> not owned
        let (store, cal) = setup_with(&mock.url("/cal/work/"), false);
        assert!(!is_owned(&store.lock().unwrap(), "c1"));
        cl.sync_calendar(&store, &cal).await.unwrap();
        assert!(store.lock().unwrap().get_event("z@t", "c1").unwrap().unwrap().zoom.is_none());
        // explicitly shared -> not owned; owned -> honoured on the next pull
        set_owned(&store, "c1", false).unwrap();
        assert!(!zoom_trusted_in(&store.lock().unwrap(), &cal));
        set_owned(&store, "c1", true).unwrap();
        assert!(zoom_trusted_in(&store.lock().unwrap(), &cal));
        forget_calendar(&store.lock().unwrap(), "c1").unwrap();
        assert!(!is_owned(&store.lock().unwrap(), "c1"));
    }

    #[tokio::test]
    async fn q03_discovery_marks_shared_and_foreign_calendars_not_owned() {
        let mock = serve(|r: &Req| match (r.method.as_str(), r.path.as_str(), r.header("depth").unwrap_or("")) {
            ("PROPFIND", "/", "0") => Resp::new(207, r#"<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:response><D:href>/</D:href><D:propstat><D:prop><D:current-user-principal><D:href>/p/me/</D:href></D:current-user-principal><C:calendar-home-set><D:href>/h/</D:href></C:calendar-home-set></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>"#),
            ("PROPFIND", "/h/", "1") => Resp::new(
                207,
                r#"<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/">
 <D:response><D:href>/h/own/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype><D:displayname>Own</D:displayname><D:owner><D:href>/p/me/</D:href></D:owner></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>/h/plain/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype><D:displayname>Plain</D:displayname></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>/h/sharee/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/><C:calendar/><CS:shared/></D:resourcetype><D:displayname>Sharee</D:displayname></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>/h/surl/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype><D:displayname>SharedUrl</D:displayname><CS:shared-url><D:href>/x/</D:href></CS:shared-url></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>/h/foreign/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype><D:displayname>Foreign</D:displayname><D:owner><D:href>/p/other/</D:href></D:owner></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>/h/out/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/><C:calendar/><CS:shared-owner/></D:resourcetype><D:displayname>SharedOut</D:displayname></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
 <D:response><D:href>/h/invited/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype><D:displayname>Invited</D:displayname><CS:invite><CS:user><D:href>mailto:me@x</D:href><CS:access><CS:shared-owner/></CS:access></CS:user><CS:user><D:href>mailto:eve@x</D:href><CS:access><CS:read-write/></CS:access></CS:user></CS:invite></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>
</D:multistatus>"#,
            ),
            _ => Resp::new(404, ""),
        })
        .await;
        let cals = client(&mock, "/").discover().await.unwrap();
        let owned = |n: &str| cals.iter().find(|c| c.name == n).unwrap().owned;
        assert!(owned("Own") && owned("Plain"));
        for n in ["Sharee", "SharedUrl", "Foreign", "SharedOut", "Invited"] {
            assert!(!owned(n), "{n} must not be owned");
        }
    }

    #[test]
    fn q12_redirects_need_the_exact_host_except_for_known_providers() {
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(same_site(&u("https://caldav.example.com/a"), &u("https://CALDAV.example.com/b")));
        assert!(!same_site(&u("https://dav.example.com/"), &u("https://evil.example.com/")), "sibling host on an arbitrary domain");
        assert!(!same_site(&u("https://alice.hosting.co.uk/"), &u("https://mallory.hosting.co.uk/")));
        assert!(same_site(&u("https://caldav.icloud.com/"), &u("https://p12-caldav.icloud.com/")));
        assert!(same_site(&u("https://caldav.fastmail.com/"), &u("https://x.fastmail.com/")));
        assert!(same_site(&u("https://calendar.zoho.eu/"), &u("https://b.zoho.eu/")));
        assert!(!same_site(&u("https://caldav.icloud.com/"), &u("https://caldav.icloud.com.evil.example/")));
    }

    #[tokio::test]
    async fn q13_303_turns_a_put_into_a_bodyless_get() {
        let mock = serve(|r: &Req| match (r.method.as_str(), r.path.as_str()) {
            ("PUT", "/cal/work/a.ics") => Resp::new(303, "").with("Location", "/cal/work/b.ics"),
            ("GET", "/cal/work/b.ics") => Resp::new(200, "").with("ETag", "\"e9\""),
            _ => Resp::new(500, ""),
        })
        .await;
        let cl = client(&mock, "/cal/work/");
        let o = cl.put_raw(&mock.url("/cal/work/"), "a", &ics("a", "A"), None).await.unwrap();
        assert_eq!(o.etag.as_deref(), Some("\"e9\""));
        let reqs = mock.requests();
        assert_eq!(reqs[0].method, "PUT");
        assert!(!reqs[0].body.is_empty());
        assert_eq!(reqs[1].method, "GET");
        assert!(reqs[1].body.is_empty(), "no body on the followed GET");
        assert!(reqs[1].header("if-none-match").is_none() && reqs[1].header("content-type").is_none());
    }

    #[tokio::test]
    async fn q07_one_permanent_failure_never_blocks_the_rest_of_the_queue() {
        let mock = serve(|r: &Req| match (r.method.as_str(), r.path.as_str()) {
            ("DELETE", "/cal/work/forbidden.ics") => Resp::new(403, ""),
            ("DELETE", "/cal/work/flaky.ics") => Resp::new(503, ""),
            ("DELETE", "/cal/work/ok.ics") => Resp::new(204, ""),
            ("PUT", p) if p.starts_with("/cal/work/") => Resp::new(201, "").with("ETag", "\"n1\""),
            _ => Resp::new(500, ""),
        })
        .await;
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        for h in ["https://evil.example/steal.ics", "/cal/work/forbidden.ics", "/cal/work/flaky.ics", "/cal/work/ok.ics"] {
            mark_deleted(&store, "c1", h, None).unwrap();
        }
        store.lock().unwrap().upsert_event(&local_event("new@t", "New")).unwrap();
        let coll = cl.collection(&mock.url("/cal/work/")).unwrap();
        let mut sum = SyncSummary::default();
        let res = cl.push_pending(&store, &cal, &coll, &HashSet::new(), &mut sum).await;
        assert!(res.is_err(), "the first failure is still reported");
        assert_eq!((sum.deleted, sum.pushed), (1, 1), "ok delete + new event went through: {sum:?}");
        // permanent failures (URL policy, 403) are dropped; the transient 503 stays queued
        let left = with(&store, |c| Ok(load_tombs(c, "c1"))).unwrap();
        assert_eq!(left, vec![("/cal/work/flaky.ics".to_string(), None)]);
        assert!(!mock.requests().iter().any(|r| r.path.contains("steal")), "foreign host never contacted");
    }

    #[tokio::test]
    async fn q07_a_permanently_rejected_event_does_not_block_other_events() {
        let mock = serve(|r: &Req| match (r.method.as_str(), r.path.as_str()) {
            ("PUT", p) if p.contains("bad") => Resp::new(403, ""),
            ("PUT", _) => Resp::new(201, "").with("ETag", "\"n1\""),
            _ => Resp::new(500, ""),
        })
        .await;
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        store.lock().unwrap().upsert_event(&local_event("bad@t", "Bad")).unwrap();
        store.lock().unwrap().upsert_event(&local_event("good@t", "Good")).unwrap();
        let coll = cl.collection(&mock.url("/cal/work/")).unwrap();
        let mut sum = SyncSummary::default();
        assert_eq!(cl.push_pending(&store, &cal, &coll, &HashSet::new(), &mut sum).await, Err(CalDavError::Forbidden));
        assert_eq!(sum.pushed, 1);
        assert!(store.lock().unwrap().get_event("good@t", "c1").unwrap().unwrap().etag.is_some());
        assert!(load_dirty(&store.lock().unwrap(), "c1").contains(&"bad@t".to_string()), "nothing is lost");
    }

    #[tokio::test]
    async fn q01_push_pending_asks_the_server_not_to_schedule() {
        let mock = serve(|r: &Req| if r.method == "PUT" { Resp::new(201, "").with("ETag", "\"n1\"") } else { Resp::new(500, "") }).await;
        let cl = client(&mock, "/cal/work/");
        let (store, cal) = setup(&mock.url("/cal/work/"));
        let mut e = local_event("sched@t", "S");
        e.organizer = Some(calendar::Attendee { email: "me@x.org".into(), name: None, role: calendar::AttendeeRole::Chair, status: calendar::AttendeeStatus::Accepted });
        e.attendees = vec![calendar::Attendee { email: "bob@y.org".into(), name: None, role: calendar::AttendeeRole::Required, status: calendar::AttendeeStatus::NeedsAction }];
        store.lock().unwrap().upsert_event(&e).unwrap();
        let coll = cl.collection(&mock.url("/cal/work/")).unwrap();
        cl.push_pending(&store, &cal, &coll, &HashSet::new(), &mut SyncSummary::default()).await.unwrap();
        let body = mock.requests().iter().find(|r| r.method == "PUT").unwrap().body.replace("\r\n ", "");
        assert!(body.contains("ATTENDEE") && body.matches("SCHEDULE-AGENT=CLIENT").count() >= 2, "{body}");
    }
}
