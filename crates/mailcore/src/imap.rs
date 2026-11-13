//! IMAP client (async-imap over tokio-rustls).
//!
//! Why async-imap rather than a hand-rolled client: it already implements the
//! tagged-response/literal parsing (via imap-proto) which is the risky part;
//! we own only the pure decision logic (folder roles, sync planning,
//! reconciliation) which is unit-tested offline below.

use crate::mime::{make_snippet, parse_headers, parse_message};
use crate::storage::Storage;
use crate::types::*;
use crate::{Error, Result};
use async_imap::types::{Fetch, Flag, NameAttribute};
use async_imap::{Client, Session};
use futures::TryStreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::{client::TlsStream, TlsConnector};

pub type Sess = Session<TlsStream<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TLS_TIMEOUT: Duration = Duration::from_secs(15);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);
pub const DEFAULT_SYNC_LIMIT: u32 = 200;
const HEADER_QUERY: &str = "(UID FLAGS RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER])";

// ---------------------------------------------------------------- pure logic

/// Map a mailbox to a role from RFC 6154 special-use attributes, falling back
/// to well-known names (last path component, case-insensitive).
pub fn classify_folder(name: &str, delimiter: Option<&str>, special_use: &[&str]) -> FolderRole {
    if name.eq_ignore_ascii_case("INBOX") {
        return FolderRole::Inbox;
    }
    for a in special_use {
        match a.to_ascii_lowercase().as_str() {
            "\\sent" => return FolderRole::Sent,
            "\\drafts" => return FolderRole::Drafts,
            "\\trash" => return FolderRole::Trash,
            "\\archive" | "\\all" => return FolderRole::Archive,
            "\\junk" => return FolderRole::Junk,
            _ => {}
        }
    }
    let last = match delimiter {
        Some(d) if !d.is_empty() => name.rsplit(d).next().unwrap_or(name),
        _ => name,
    };
    match last.to_ascii_lowercase().as_str() {
        "sent" | "sent items" | "sent messages" | "sent mail" => FolderRole::Sent,
        "drafts" | "draft" => FolderRole::Drafts,
        "trash" | "deleted items" | "deleted messages" | "bin" => FolderRole::Trash,
        "archive" | "archives" | "all mail" => FolderRole::Archive,
        "junk" | "spam" | "junk e-mail" | "junk email" | "bulk mail" => FolderRole::Junk,
        _ => FolderRole::Other,
    }
}

/// First folder with the given role.
pub fn find_role(folders: &[Folder], role: FolderRole) -> Option<&Folder> {
    folders.iter().find(|f| f.role == role && f.selectable)
}

fn attr_str(a: &NameAttribute<'_>) -> Option<String> {
    Some(match a {
        NameAttribute::Sent => "\\Sent".into(),
        NameAttribute::Drafts => "\\Drafts".into(),
        NameAttribute::Trash => "\\Trash".into(),
        NameAttribute::Archive => "\\Archive".into(),
        NameAttribute::Junk => "\\Junk".into(),
        NameAttribute::All => "\\All".into(),
        NameAttribute::Extension(s) => s.to_string(),
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchRange {
    /// Nothing to fetch.
    None,
    /// Sequence-number set (initial sync of the newest N).
    Seq(String),
    /// UID set (incremental).
    Uid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPlan {
    /// Drop the local cache of this folder first (UIDVALIDITY changed).
    pub reset: bool,
    pub fetch: FetchRange,
}

/// Search hits that may be cached without affecting incremental sync: only UIDs the last sync
/// already covered (below the stored UIDNEXT). Newer mail arrives through the normal sync.
pub fn cacheable_uids(hits: &[u32], synced_uidnext: u32) -> Vec<u32> {
    hits.iter().copied().filter(|u| *u < synced_uidnext).collect()
}

/// Decide how to sync. `max_local_uid` is None if nothing is cached.
pub fn plan_sync(
    stored_validity: Option<u32>,
    max_local_uid: Option<u32>,
    server_validity: u32,
    exists: u32,
    limit: u32,
) -> SyncPlan {
    let reset = matches!(stored_validity, Some(v) if v != server_validity);
    let max_local = if reset || stored_validity.is_none() { None } else { max_local_uid };
    let fetch = if exists == 0 {
        FetchRange::None
    } else if let Some(m) = max_local {
        FetchRange::Uid(format!("{}:*", m.saturating_add(1)))
    } else {
        let start = exists.saturating_sub(limit.max(1)).saturating_add(1);
        FetchRange::Seq(format!("{start}:*"))
    };
    SyncPlan { reset, fetch }
}

/// Compare cached (uid, flags) with the server's current ones.
/// Returns (uids to delete locally, uids whose flags changed with new flags).
pub fn reconcile(local: &[(u32, Flags)], remote: &[(u32, Flags)]) -> (Vec<u32>, Vec<(u32, Flags)>) {
    let rmap: std::collections::HashMap<u32, Flags> = remote.iter().copied().collect();
    let mut del = Vec::new();
    let mut upd = Vec::new();
    for (uid, f) in local {
        match rmap.get(uid) {
            None => del.push(*uid),
            Some(rf) if rf != f => upd.push((*uid, *rf)),
            _ => {}
        }
    }
    (del, upd)
}

pub fn flags_from_iter<'a>(it: impl Iterator<Item = Flag<'a>>) -> Flags {
    let mut f = Flags::default();
    for x in it {
        match x {
            Flag::Seen => f.seen = true,
            Flag::Flagged => f.flagged = true,
            Flag::Answered => f.answered = true,
            Flag::Draft => f.draft = true,
            Flag::Deleted => f.deleted = true,
            _ => {}
        }
    }
    f
}

fn display_addrs(a: &[EmailAddress]) -> String {
    a.iter()
        .map(|x| match &x.name {
            // Angle brackets in a display name could pose as another address once the string is re-parsed.
            Some(n) => format!("{} <{}>", n.replace(['<', '>'], ""), x.address),
            None => x.address.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Where a message lives.
#[derive(Debug, Clone, Copy)]
pub struct MsgLoc<'a> {
    pub account: &'a str,
    pub folder: &'a str,
    pub uid: u32,
    pub uidvalidity: u32,
}

/// Build a list row from a fetched header block.
pub fn summary_from_header(loc: &MsgLoc<'_>, flags: Flags, size: u32, internal_date: Option<i64>, header: &[u8]) -> MessageSummary {
    let h = parse_headers(header).unwrap_or_default();
    MessageSummary {
        account: loc.account.into(),
        folder: loc.folder.into(),
        uid: loc.uid,
        uidvalidity: loc.uidvalidity,
        message_id: h.message_id,
        subject: h.subject,
        from: display_addrs(&h.from),
        to: display_addrs(&h.to),
        date: h.date.or(internal_date).unwrap_or(0),
        flags,
        snippet: String::new(),
        has_body: false,
        size,
    }
}

/// UIDs flagged \Deleted by someone else (everything but `mine`), sorted.
fn others_deleted(found: impl IntoIterator<Item = u32>, mine: u32) -> Vec<u32> {
    let mut v: Vec<u32> = found.into_iter().filter(|u| *u != mine).collect();
    v.sort_unstable();
    v
}

fn uid_set(uids: &[u32]) -> String {
    uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
}

pub(crate) fn flag_cmd(c: FlagChange) -> String {
    let (flag, on) = match c {
        FlagChange::Seen(on) => ("\\Seen", on),
        FlagChange::Flagged(on) => ("\\Flagged", on),
    };
    format!("{}FLAGS.SILENT ({flag})", if on { "+" } else { "-" })
}


// ------------------------------------------------------- server search (pure query building)

/// Criteria for [`ImapClient::server_search`]. Text values must be printable ASCII (no literals or
/// charsets are negotiated); dates are `YYYY-MM-DD`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    /// Matches the message body (IMAP `BODY`).
    pub text: Option<String>,
    /// On or after.
    pub since: Option<String>,
    /// Strictly before.
    pub before: Option<String>,
    pub unseen: Option<bool>,
    pub flagged: Option<bool>,
}

const MAX_SEARCH_VALUE: usize = 200;

/// IMAP quoted string. Anything that could break out of the quotes or the command line is rejected
/// rather than escaped: control characters (incl. CR/LF/NUL) and non-ASCII (would need a literal).
pub fn imap_quote(s: &str) -> Result<String> {
    let s = s.trim();
    if s.is_empty() {
        return Err(Error::Invalid("empty search value".into()));
    }
    if s.len() > MAX_SEARCH_VALUE {
        return Err(Error::Invalid("search value too long".into()));
    }
    if !s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(Error::Invalid("server search supports printable ASCII only; use local search for other text".into()));
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    Ok(out)
}

fn imap_date(s: &str) -> Result<String> {
    const MON: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let bad = || Error::Invalid("dates must be YYYY-MM-DD".into());
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' || !b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit()) {
        return Err(bad());
    }
    let (y, m, d): (u32, usize, u32) = (s[0..4].parse().map_err(|_| bad())?, s[5..7].parse().map_err(|_| bad())?, s[8..10].parse().map_err(|_| bad())?);
    if !(1970..=2200).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    Ok(format!("{d}-{}-{y}", MON[m - 1]))
}

/// Build the SEARCH key list. At least one criterion is required.
pub fn build_search_criteria(q: &ServerQuery) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for (key, v) in [("FROM", &q.from), ("TO", &q.to), ("SUBJECT", &q.subject), ("BODY", &q.text)] {
        if let Some(v) = v {
            parts.push(format!("{key} {}", imap_quote(v)?));
        }
    }
    if let Some(d) = &q.since {
        parts.push(format!("SINCE {}", imap_date(d)?));
    }
    if let Some(d) = &q.before {
        parts.push(format!("BEFORE {}", imap_date(d)?));
    }
    match q.unseen {
        Some(true) => parts.push("UNSEEN".into()),
        Some(false) => parts.push("SEEN".into()),
        None => {}
    }
    match q.flagged {
        Some(true) => parts.push("FLAGGED".into()),
        Some(false) => parts.push("UNFLAGGED".into()),
        None => {}
    }
    if parts.is_empty() {
        return Err(Error::Invalid("give at least one search criterion".into()));
    }
    Ok(parts.join(" "))
}

// ------------------------------------------------------- load older (pure planning)

/// Largest batch one `load_older` call fetches.
pub const MAX_LOAD_OLDER: u32 = 1000;
/// UIDs per `UID FETCH` command (keeps command lines and responses small).
const FETCH_CHUNK: usize = 200;
/// Per-round-trip timeouts for load_older.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Cumulative UID window `(lo, hi)` (inclusive) directly below `above` (exclusive) to search on
/// the given attempt. The window grows 8x per attempt (UIDs are not contiguous, so a window can
/// hold fewer than `count` messages); from attempt 5 on it spans down to UID 1.
/// `None` when nothing can lie below `above`.
pub fn older_window(above: u32, count: u32, attempt: u32) -> Option<(u32, u32)> {
    if above <= 1 {
        return None;
    }
    let hi = above - 1;
    let lo = if attempt >= 5 {
        1
    } else {
        let span = (count.max(1) as u64) * 4 * 8u64.pow(attempt);
        (hi as u64 + 1).saturating_sub(span).max(1) as u32
    };
    Some((lo, hi))
}

/// The highest `count` UIDs of `found` (ascending, deduplicated) and how many were left over.
pub fn pick_highest(mut found: Vec<u32>, count: usize) -> (Vec<u32>, usize) {
    found.sort_unstable();
    found.dedup();
    let left = found.len().saturating_sub(count);
    (found.split_off(left), left)
}

/// `(remaining_on_server, reached_start)` after a load.
/// `exact`: the search covered the range down to UID 1, so `leftover` is the true number of older
/// messages. Otherwise estimate from totals, never claiming the start was reached.
pub fn older_progress(exact: bool, leftover: usize, server_total: u32, cached_after: u32) -> (u32, bool) {
    if exact {
        (leftover as u32, leftover == 0)
    } else {
        (server_total.saturating_sub(cached_after).max(1), false)
    }
}

/// Compact IMAP sequence set for UIDs: `1,2,3,7,9,10` -> `1:3,7,9:10`.
pub fn uid_ranges(uids: &[u32]) -> String {
    let mut v = uids.to_vec();
    v.sort_unstable();
    v.dedup();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1] == v[j] + 1 {
            j += 1;
        }
        out.push(if j > i { format!("{}:{}", v[i], v[j]) } else { v[i].to_string() });
        i = j + 1;
    }
    out.join(",")
}

/// One fetched header block (transport-independent so the load logic can be tested with a fake).
#[derive(Debug, Clone)]
pub(crate) struct HeaderRow {
    pub uid: u32,
    pub flags: Flags,
    pub size: u32,
    pub internal_date: Option<i64>,
    pub header: Vec<u8>,
}

/// The server operations `load_older` needs.
pub(crate) trait OlderServer {
    /// Returns (uidvalidity, exists, uidnext).
    async fn select(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)>;
    /// UIDs present in `lo..=hi`.
    async fn search_uids(&mut self, lo: u32, hi: u32) -> Result<Vec<u32>>;
    async fn fetch_headers(&mut self, uids: &[u32]) -> Result<Vec<HeaderRow>>;
}

/// Load up to `count` headers older than the oldest cached message (or the newest `count` when the
/// cache is empty). Existing rows and the folder's sync state are untouched (except recording the
/// state when the folder had none); a UIDVALIDITY mismatch aborts without modifying the cache.
pub(crate) async fn load_older_with<S: OlderServer>(
    srv: &mut S,
    storage: &Storage,
    account: &str,
    folder: &str,
    count: u32,
) -> Result<LoadOlderReport> {
    let count = count.clamp(1, MAX_LOAD_OLDER);
    let (validity, exists, uidnext) = srv.select(folder).await?;
    let (stored_validity, _) = storage.folder_sync_state(account, folder)?;
    if stored_validity.is_some_and(|v| v != validity) || storage.has_foreign_validity(account, folder, validity)? {
        return Err(Error::Imap("UIDVALIDITY changed; resync the folder".into()));
    }
    let min_local = storage.min_uid(account, folder)?;
    if exists == 0 {
        return Ok(LoadOlderReport {
            loaded: 0,
            remaining_on_server: 0,
            server_total: 0,
            oldest_uid: min_local,
            reached_start: true,
        });
    }
    // Empty cache: behave like a first sync (newest `count`), i.e. search below UIDNEXT.
    let above = min_local.unwrap_or_else(|| uidnext.unwrap_or(u32::MAX));

    let mut found: Vec<u32> = Vec::new();
    let mut exact = false;
    let mut prev_lo: Option<u32> = None;
    for attempt in 0..8 {
        let Some((lo, hi)) = older_window(above, count, attempt) else {
            exact = true;
            break;
        };
        let upper = prev_lo.map_or(hi, |p| p - 1);
        found.extend(srv.search_uids(lo, upper).await?.into_iter().filter(|u| (lo..above).contains(u)));
        prev_lo = Some(lo);
        if lo == 1 {
            exact = true;
            break;
        }
        if found.len() >= count as usize {
            break;
        }
    }
    let (pick, leftover) = pick_highest(found, count as usize);

    // Highest UIDs first: if a later chunk fails, the cache still reaches down contiguously.
    let mut loaded = 0u32;
    for chunk in pick.chunks(FETCH_CHUNK).rev() {
        for row in srv.fetch_headers(chunk).await? {
            if !chunk.contains(&row.uid) {
                continue;
            }
            let s = summary_from_header(
                &MsgLoc { account, folder, uid: row.uid, uidvalidity: validity },
                row.flags,
                row.size,
                row.internal_date,
                &row.header,
            );
            storage.upsert_message_with_headers(&s, &crate::mime::parse_extra_headers(&row.header))?;
            loaded += 1;
        }
    }
    if stored_validity.is_none() {
        storage.set_folder_sync_state(account, folder, validity, uidnext)?;
    }
    let cached_after = storage.message_count(account, folder)?;
    let (remaining_on_server, reached_start) = older_progress(exact, leftover, exists, cached_after);
    Ok(LoadOlderReport {
        loaded,
        remaining_on_server,
        server_total: exists,
        oldest_uid: storage.min_uid(account, folder)?,
        reached_start,
    })
}

impl OlderServer for ImapClient {
    async fn select(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)> {
        ImapClient::select(self, folder).await
    }

    async fn search_uids(&mut self, lo: u32, hi: u32) -> Result<Vec<u32>> {
        let set = timeout(SEARCH_TIMEOUT, self.session.uid_search(format!("UID {lo}:{hi}")))
            .await
            .map_err(|_| Error::Timeout("search"))??;
        Ok(set.into_iter().collect())
    }

    async fn fetch_headers(&mut self, uids: &[u32]) -> Result<Vec<HeaderRow>> {
        let fetches: Vec<Fetch> = timeout(FETCH_TIMEOUT, async {
            self.session.uid_fetch(uid_ranges(uids), HEADER_QUERY).await?.try_collect().await
        })
        .await
        .map_err(|_| Error::Timeout("fetch"))??;
        Ok(fetches
            .iter()
            .filter_map(|f| {
                Some(HeaderRow {
                    uid: f.uid?,
                    flags: flags_from_iter(f.flags()),
                    size: f.size.unwrap_or(0),
                    internal_date: f.internal_date().map(|d| d.timestamp()),
                    header: f.header()?.to_vec(),
                })
            })
            .collect())
    }
}

// ------------------------------------------------------------------ network

pub(crate) fn tls_connector() -> Result<TlsConnector> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let cfg = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(cfg)))
}

pub(crate) async fn handshake(tcp: TcpStream, host: &str) -> Result<TlsStream<TcpStream>> {
    let name = ServerName::try_from(host.to_owned()).map_err(|e| Error::Tls(e.to_string()))?;
    timeout(TLS_TIMEOUT, tls_connector()?.connect(name, tcp))
        .await
        .map_err(|_| Error::Timeout("tls handshake"))?
        .map_err(|e| Error::Tls(e.to_string()))
}

/// Phase durations (ms) and byte count of this connection, for the app's `perf:` log lines. No content.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImapTimings {
    pub tcp_ms: u32,
    pub tls_ms: u32,
    pub login_ms: u32,
    pub select_ms: u32,
    pub fetch_ms: u32,
    pub store_ms: u32,
    pub fetched_bytes: usize,
}

fn ms_since(t: std::time::Instant) -> u32 {
    t.elapsed().as_millis().min(u32::MAX as u128) as u32
}

pub struct ImapClient {
    session: Sess,
    caps: Caps,
    pub timings: ImapTimings,
    /// Folder (and its UIDVALIDITY) this session last SELECTed through [`ImapClient::select`]; cleared before every
    /// SELECT / EXAMINE and whenever a sibling module takes the raw session.
    selected: Option<(String, u32)>,
    /// Opt-in ([`ImapClient::reuse_selection`]): `fetch_body` skips the SELECT when the folder is already selected
    /// under the expected UIDVALIDITY. Only the app's warm interactive session turns it on.
    reuse_select: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct Caps {
    idle: bool,
    mv: bool,
    uidplus: bool,
}

/// May a fetch skip its SELECT? Only when the session opted in, `folder` is the one selected, and the caller's
/// UIDVALIDITY is known and equals the one that SELECT returned (an unknown one always re-selects).
pub fn selection_reusable(opted_in: bool, selected: Option<&(String, u32)>, folder: &str, expected_validity: Option<u32>) -> bool {
    opted_in && matches!((selected, expected_validity), (Some((f, v)), Some(e)) if f == folder && *v == e)
}

/// A destructive command needs the UIDVALIDITY the UID was observed under; "unknown" is refused.
pub fn require_validity(v: Option<u32>) -> Result<u32> {
    v.ok_or_else(|| Error::Imap("This message isn't in the local cache any more, so it can't be safely deleted. Refresh the folder and try again.".into()))
}

impl ImapClient {
    /// Connect (TLS), LOGIN. The password is used for this call only.
    pub async fn connect(cfg: &ServerConfig, username: &str, password: &str) -> Result<Self> {
        let mut timings = ImapTimings::default();
        let t = std::time::Instant::now();
        let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((cfg.host.as_str(), cfg.port)))
            .await
            .map_err(|_| Error::Timeout("tcp connect"))??;
        timings.tcp_ms = ms_since(t);
        let _ = tcp.set_nodelay(true);
        let t = std::time::Instant::now();
        let tls = match cfg.tls {
            Tls::Implicit => handshake(tcp, &cfg.host).await?,
            Tls::StartTls => {
                let mut c = Client::new(tcp);
                timeout(CONNECT_TIMEOUT, c.read_response())
                    .await
                    .map_err(|_| Error::Timeout("greeting"))??;
                timeout(CONNECT_TIMEOUT, c.run_command_and_check_ok("STARTTLS", None))
                    .await
                    .map_err(|_| Error::Timeout("starttls"))??;
                handshake(c.into_inner(), &cfg.host).await?
            }
        };
        let mut client = Client::new(tls);
        if cfg.tls == Tls::Implicit {
            timeout(CONNECT_TIMEOUT, client.read_response())
                .await
                .map_err(|_| Error::Timeout("greeting"))??;
        }
        timings.tls_ms = ms_since(t); // handshake + greeting (STARTTLS: also the STARTTLS exchange)
        let t = std::time::Instant::now();
        let mut session = match timeout(LOGIN_TIMEOUT, client.login(username, password)).await {
            Err(_) => return Err(Error::Timeout("login")),
            Ok(Ok(s)) => s,
            Ok(Err((async_imap::error::Error::No(_), _))) => return Err(Error::AuthFailed),
            Ok(Err((e, _))) => return Err(e.into()),
        };
        let c = session.capabilities().await?;
        let caps = Caps { idle: c.has_str("IDLE"), mv: c.has_str("MOVE"), uidplus: c.has_str("UIDPLUS") };
        timings.login_ms = ms_since(t); // LOGIN + CAPABILITY
        Ok(ImapClient { session, caps, timings, selected: None, reuse_select: false })
    }

    pub fn supports_idle(&self) -> bool {
        self.caps.idle
    }

    /// Raw session for sibling modules (folder management); they own their own commands and timeouts.
    pub(crate) fn session_mut(&mut self) -> &mut Sess {
        self.selected = None; // the caller may SELECT, CLOSE or rename anything
        &mut self.session
    }

    pub(crate) fn has_uidplus(&self) -> bool {
        self.caps.uidplus
    }

    pub(crate) fn into_session(self) -> Sess {
        self.session
    }

    pub(crate) fn from_session(session: Sess, idle: bool) -> Self {
        ImapClient { session, caps: Caps { idle, ..Caps::default() }, timings: ImapTimings::default(), selected: None, reuse_select: false }
    }

    pub async fn logout(mut self) -> Result<()> {
        self.session.logout().await?;
        Ok(())
    }

    pub async fn noop(&mut self) -> Result<()> {
        timeout(Duration::from_secs(20), self.session.noop())
            .await
            .map_err(|_| Error::Timeout("noop"))??;
        Ok(())
    }

    pub async fn list_folders(&mut self) -> Result<Vec<Folder>> {
        let names: Vec<_> = self.session.list(Some(""), Some("*")).await?.try_collect().await?;
        let mut out: Vec<Folder> = names
            .iter()
            .map(|n| {
                let attrs: Vec<String> = n.attributes().iter().filter_map(attr_str).collect();
                let refs: Vec<&str> = attrs.iter().map(String::as_str).collect();
                Folder {
                    name: n.name().to_owned(),
                    delimiter: n.delimiter().map(str::to_owned),
                    role: classify_folder(n.name(), n.delimiter(), &refs),
                    selectable: !n.attributes().iter().any(|a| matches!(a, NameAttribute::NoSelect)),
                }
            })
            .collect();
        // Keep a single folder per role: first special-use wins (list order).
        let mut seen = std::collections::HashSet::new();
        for f in out.iter_mut() {
            if f.role != FolderRole::Other && !seen.insert(f.role) {
                f.role = FolderRole::Other;
            }
        }
        Ok(out)
    }

    /// Returns (uidvalidity, exists, uidnext).
    /// Unread (UNSEEN) count per selectable folder via IMAP STATUS (does not select
    /// the folder, so it cannot change flags). Folders that fail are skipped.
    pub async fn unseen_counts(&mut self, folders: &[Folder]) -> Result<Vec<(String, u32)>> {
        let mut out = Vec::new();
        for f in folders.iter().filter(|f| f.selectable) {
            let st = timeout(Duration::from_secs(10), self.session.status(&f.name, "(UNSEEN)")).await;
            if let Ok(Ok(mb)) = st {
                out.push((f.name.clone(), mb.unseen.unwrap_or(0)));
            }
        }
        Ok(out)
    }

    pub async fn select(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)> {
        self.selected = None; // a failed SELECT leaves no mailbox selected (RFC 3501 6.3.1)
        let mb = timeout(Duration::from_secs(20), self.session.select(folder)).await.map_err(|_| Error::Timeout("select"))??;
        let v = mb.uid_validity.ok_or_else(|| Error::Imap("server gave no UIDVALIDITY".into()))?;
        self.selected = Some((folder.to_string(), v));
        Ok((v, mb.exists, mb.uid_next))
    }

    /// Let `fetch_body` reuse the current SELECT (see [`selection_reusable`]). For a session kept open between reader
    /// fetches; every other caller keeps the SELECT per fetch.
    pub fn reuse_selection(&mut self, on: bool) {
        self.reuse_select = on;
    }

    /// Like [`select`](Self::select) but read-only (EXAMINE): nothing can change on the server.
    pub async fn examine(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)> {
        self.selected = None; // read-only state: never reused for a fetch that expects SELECT
        let mb = timeout(Duration::from_secs(20), self.session.examine(folder)).await.map_err(|_| Error::Timeout("examine"))??;
        let v = mb.uid_validity.ok_or_else(|| Error::Imap("server gave no UIDVALIDITY".into()))?;
        Ok((v, mb.exists, mb.uid_next))
    }

    /// Sync headers of `folder` into `storage`: newest `limit` on first run,
    /// then incremental by UID; refreshes flags and removes expunged messages;
    /// discards the cache when UIDVALIDITY changed.
    pub async fn sync_folder(
        &mut self,
        storage: &Storage,
        account: &str,
        folder: &str,
        limit: u32,
    ) -> Result<SyncReport> {
        let (validity, exists, uidnext) = self.select(folder).await?;
        let (stored_validity, _) = storage.folder_sync_state(account, folder)?;
        // Cached rows carrying another UIDVALIDITY (even if the stored folder
        // state says otherwise) are stale: treat exactly like a validity change.
        let foreign = storage.has_foreign_validity(account, folder, validity)?;
        let mut plan = plan_sync(
            if foreign { None } else { stored_validity },
            if foreign { None } else { storage.max_uid(account, folder)? },
            validity,
            exists,
            limit,
        );
        plan.reset |= foreign;
        let mut report = SyncReport { server_total: exists, ..SyncReport::default() };
        if plan.reset {
            storage.clear_folder_messages(account, folder)?;
            report.uidvalidity_reset = true;
        }
        let known: std::collections::HashSet<u32> = storage.uids(account, folder)?.into_iter().collect();

        let fetches: Vec<Fetch> = match &plan.fetch {
            FetchRange::None => Vec::new(),
            FetchRange::Seq(s) => self.session.fetch(s, HEADER_QUERY).await?.try_collect().await?,
            FetchRange::Uid(s) => self.session.uid_fetch(s, HEADER_QUERY).await?.try_collect().await?,
        };
        let min_new = match &plan.fetch {
            FetchRange::Uid(s) => s.split(':').next().and_then(|x| x.parse::<u32>().ok()),
            _ => None,
        };
        for f in &fetches {
            let Some(uid) = f.uid else { continue };
            if min_new.is_some_and(|m| uid < m) {
                continue; // "N:*" echoes the last message when N > max UID
            }
            let Some(header) = f.header() else { continue };
            let s = summary_from_header(
                &MsgLoc { account, folder, uid, uidvalidity: validity },
                flags_from_iter(f.flags()),
                f.size.unwrap_or(0),
                f.internal_date().map(|d| d.timestamp()),
                header,
            );
            storage.upsert_message_with_headers(&s, &crate::mime::parse_extra_headers(header))?;
            if known.contains(&uid) { report.updated += 1 } else { report.new += 1 }
        }

        // Reconcile flags / expunges for what we already had cached.
        if exists > 0 && !known.is_empty() && !plan.reset {
            let min_local = *known.iter().min().unwrap_or(&1);
            let remote: Vec<(u32, Flags)> = self
                .session
                .uid_fetch(format!("{min_local}:*"), "(UID FLAGS)")
                .await?
                .try_collect::<Vec<Fetch>>()
                .await?
                .iter()
                .filter_map(|f| f.uid.filter(|u| *u >= min_local).map(|u| (u, flags_from_iter(f.flags()))))
                .collect();
            let mut local = Vec::new();
            for u in &known {
                if let Some(m) = storage.get_message(account, folder, *u)? {
                    local.push((*u, m.flags));
                }
            }
            let (del, upd) = reconcile(&local, &remote);
            for u in del {
                storage.delete_message(account, folder, u)?;
                report.removed += 1;
            }
            for (u, f) in upd {
                storage.update_flags(account, folder, u, f)?;
                report.updated += 1;
            }
        } else if exists == 0 {
            for u in &known {
                storage.delete_message(account, folder, *u)?;
                report.removed += 1;
            }
        }
        storage.set_folder_sync_state(account, folder, validity, uidnext)?;
        Ok(report)
    }

    /// Number of messages in `folder` on the server (STATUS MESSAGES; does not select the folder).
    pub async fn folder_status_total(&mut self, folder: &str) -> Result<u32> {
        let mb = timeout(Duration::from_secs(10), self.session.status(folder, "(MESSAGES)"))
            .await
            .map_err(|_| Error::Timeout("status"))??;
        Ok(mb.exists)
    }

    /// Fetch the next `count` (1..=1000) headers older than the oldest cached message of `folder`
    /// into the cache. See [`load_older_with`] for the guarantees.
    pub async fn load_older(&mut self, storage: &Storage, account: &str, folder: &str, count: u32) -> Result<LoadOlderReport> {
        load_older_with(self, storage, account, folder, count).await
    }

    /// `UID SEARCH` on the server for the criteria in `q` (see [`build_search_criteria`]), newest
    /// UIDs first, at most `limit`. The headers of the hits are fetched and cached (so they can be
    /// referenced and read later). Aborts if the folder's UIDVALIDITY differs from `expected_validity`
    /// or from what the cache holds. Returns `(uidvalidity, uids)`.
    pub async fn server_search(
        &mut self,
        storage: &Storage,
        account: &str,
        folder: &str,
        expected_validity: Option<u32>,
        q: &ServerQuery,
        limit: u32,
    ) -> Result<(u32, Vec<u32>)> {
        let criteria = build_search_criteria(q)?;
        let (validity, _, _) = self.examine(folder).await?;
        if expected_validity.is_some_and(|v| v != validity) {
            return Err(Error::Imap("UIDVALIDITY changed; resync the folder".into()));
        }
        let (stored, stored_next) = storage.folder_sync_state(account, folder)?;
        if stored.is_some_and(|v| v != validity) || storage.has_foreign_validity(account, folder, validity)? {
            return Err(Error::Imap("UIDVALIDITY changed; resync the folder".into()));
        }
        // Caching is only allowed in a way sync planning cannot be misled by: the folder must have
        // been synced, and only UIDs below the synced UIDNEXT are cached. This method never writes
        // folder sync state.
        let (true, Some(synced_next)) = (stored.is_some(), stored_next) else {
            return Err(Error::Invalid("This folder has not been synced yet. Open it in the mail app once, then search again.".into()));
        };
        let found = timeout(SEARCH_TIMEOUT, self.session.uid_search(&criteria))
            .await
            .map_err(|_| Error::Timeout("search"))??;
        let mut uids: Vec<u32> = found.into_iter().collect();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        uids.truncate(limit.clamp(1, 100) as usize);
        uids = cacheable_uids(&uids, synced_next);
        if uids.is_empty() {
            return Ok((validity, uids));
        }
        let fetches: Vec<Fetch> = timeout(FETCH_TIMEOUT, async {
            self.session.uid_fetch(uid_ranges(&uids), HEADER_QUERY).await?.try_collect().await
        })
        .await
        .map_err(|_| Error::Timeout("fetch"))??;
        for f in &fetches {
            let Some(uid) = f.uid.filter(|u| uids.contains(u)) else { continue };
            let Some(header) = f.header() else { continue };
            let s = summary_from_header(
                &MsgLoc { account, folder, uid, uidvalidity: validity },
                flags_from_iter(f.flags()),
                f.size.unwrap_or(0),
                f.internal_date().map(|d| d.timestamp()),
                header,
            );
            storage.upsert_message_with_headers(&s, &crate::mime::parse_extra_headers(header))?;
        }
        Ok((validity, uids))
    }

    /// Full raw RFC 822 message (BODY.PEEK[] - does not set \Seen).
    pub async fn fetch_body(&mut self, folder: &str, uid: u32, expected_validity: Option<u32>) -> Result<Vec<u8>> {
        if !selection_reusable(self.reuse_select, self.selected.as_ref(), folder, expected_validity) {
            let t = std::time::Instant::now();
            let (validity, _, _) = self.select(folder).await?;
            self.timings.select_ms = ms_since(t);
            if expected_validity.is_some_and(|v| v != validity) {
                return Err(Error::Imap("UIDVALIDITY changed; resync the folder".into()));
            }
        } else {
            self.timings.select_ms = 0;
        }
        let t = std::time::Instant::now();
        let fetches: Vec<Fetch> = timeout(FETCH_TIMEOUT, async { self.session.uid_fetch(uid.to_string(), "BODY.PEEK[]").await?.try_collect().await })
            .await
            .map_err(|_| Error::Timeout("fetch"))??;
        self.timings.fetch_ms = ms_since(t);
        self.timings.fetched_bytes = fetches.iter().filter_map(|f| f.body()).map(<[u8]>::len).sum();
        fetches
            .iter()
            .find(|f| f.uid == Some(uid))
            .and_then(|f| f.body())
            .map(<[u8]>::to_vec)
            .ok_or_else(|| Error::NotFound(format!("message uid {uid}")))
    }

    /// Cached body if present, else fetch + cache (also fills the snippet).
    pub async fn get_body(&mut self, storage: &Storage, account: &str, folder: &str, uid: u32) -> Result<Vec<u8>> {
        if let Some(b) = storage.get_body(account, folder, uid)? {
            return Ok(b);
        }
        let validity = storage.get_message(account, folder, uid)?.map(|m| m.uidvalidity);
        let raw = self.fetch_body(folder, uid, validity).await?;
        let snippet = parse_message(&raw).ok().and_then(|p| p.text).map(|t| make_snippet(&t)).unwrap_or_default();
        if let Some(v) = validity {
            let t = std::time::Instant::now();
            storage.set_body(account, folder, uid, v, &raw, &snippet)?;
            self.timings.store_ms = ms_since(t);
        }
        Ok(raw)
    }

    /// SELECT `folder` and abort if its UIDVALIDITY differs from the one the
    /// caller's UID was observed under (the UID may now name another message).
    async fn select_checked(&mut self, folder: &str, expected_validity: Option<u32>) -> Result<()> {
        let (validity, _, _) = self.select(folder).await?;
        if expected_validity.is_some_and(|v| v != validity) {
            return Err(Error::Imap("UIDVALIDITY changed; resync the folder".into()));
        }
        Ok(())
    }

    pub async fn set_flag(&mut self, folder: &str, uid: u32, expected_validity: Option<u32>, change: FlagChange) -> Result<()> {
        let t = std::time::Instant::now();
        self.select_checked(folder, expected_validity).await?;
        self.timings.select_ms = ms_since(t);
        let t = std::time::Instant::now();
        let _: Vec<Fetch> = timeout(Duration::from_secs(20), async { self.session.uid_store(uid.to_string(), flag_cmd(change)).await?.try_collect().await })
            .await
            .map_err(|_| Error::Timeout("store"))??;
        self.timings.store_ms = ms_since(t);
        Ok(())
    }

    /// Move to `dest` (UID MOVE if supported, else COPY + \Deleted + expunge).
    pub async fn move_message(&mut self, folder: &str, uid: u32, expected_validity: Option<u32>, dest: &str) -> Result<()> {
        self.select_checked(folder, expected_validity).await?;
        let set = uid.to_string();
        if self.caps.mv {
            self.session.uid_mv(&set, dest).await?;
        } else {
            self.session.uid_copy(&set, dest).await?;
            self.expunge_uid(uid).await?;
        }
        Ok(())
    }

    /// Permanently delete a message from `folder`. Fails closed: the UIDVALIDITY the caller saw the UID under
    /// is required (an unknown one would let a reset mailbox hand the UID to another message).
    pub async fn delete_message(&mut self, folder: &str, uid: u32, expected_validity: Option<u32>) -> Result<()> {
        let v = require_validity(expected_validity)?;
        self.select_checked(folder, Some(v)).await?;
        self.expunge_uid(uid).await
    }

    /// Remove exactly one UID. With UIDPLUS this is `UID EXPUNGE`. Without it a
    /// plain EXPUNGE would also destroy every other \Deleted message in the
    /// folder (flagged by the user or another client), so we first
    /// `UID SEARCH DELETED`, temporarily clear \Deleted on those others,
    /// EXPUNGE, and restore the flag afterwards (also on error, best effort).
    async fn expunge_uid(&mut self, uid: u32) -> Result<()> {
        let set = uid.to_string();
        if self.caps.uidplus {
            let _: Vec<Fetch> = self.session.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await?.try_collect().await?;
            let _: Vec<u32> = self.session.uid_expunge(&set).await?.try_collect().await?;
            return Ok(());
        }
        let others = others_deleted(self.session.uid_search("DELETED").await?, uid);
        let other_set = uid_set(&others);
        if !others.is_empty() {
            let _: Vec<Fetch> = self.session.uid_store(&other_set, "-FLAGS.SILENT (\\Deleted)").await?.try_collect().await?;
        }
        let res: Result<()> = async {
            let _: Vec<Fetch> = self.session.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await?.try_collect().await?;
            let _: Vec<u32> = self.session.expunge().await?.try_collect().await?;
            Ok(())
        }
        .await;
        if !others.is_empty() {
            let restore: Result<()> = async {
                let _: Vec<Fetch> = self.session.uid_store(&other_set, "+FLAGS.SILENT (\\Deleted)").await?.try_collect().await?;
                Ok(())
            }
            .await;
            res?;
            return restore;
        }
        res
    }

    /// APPEND a raw message (e.g. a copy of a sent mail) to `folder`.
    pub async fn append(&mut self, folder: &str, raw: &[u8], seen: bool) -> Result<()> {
        self.session.append(folder, if seen { Some("(\\Seen)") } else { None }, None, raw).await?;
        Ok(())
    }

    /// APPEND with an explicit flag list such as `(\\Seen \\Draft)`.
    pub async fn append_flagged(&mut self, folder: &str, raw: &[u8], flags: &str) -> Result<()> {
        // Bounded: the draft mirror holds its gate mutex across this call, so a stalled server must not hang it forever.
        timeout(FETCH_TIMEOUT, self.session.append(folder, Some(flags), None, raw)).await.map_err(|_| Error::Timeout("append"))??;
        Ok(())
    }
}

impl crate::batch::BatchServer for ImapClient {
    fn supports_move(&self) -> bool {
        self.caps.mv
    }

    fn supports_uidplus(&self) -> bool {
        self.caps.uidplus
    }

    async fn select(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)> {
        ImapClient::select(self, folder).await
    }

    async fn examine(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)> {
        ImapClient::examine(self, folder).await
    }

    async fn status(&mut self, folder: &str) -> Result<(Option<u32>, Option<u32>)> {
        let mb = timeout(Duration::from_secs(10), self.session.status(folder, "(UIDVALIDITY UIDNEXT)"))
            .await
            .map_err(|_| Error::Timeout("status"))??;
        Ok((mb.uid_validity, mb.uid_next))
    }

    async fn present(&mut self, set: &str) -> Result<Vec<u32>> {
        let found = timeout(SEARCH_TIMEOUT, self.session.uid_search(format!("UID {set}")))
            .await
            .map_err(|_| Error::Timeout("search"))??;
        Ok(found.into_iter().collect())
    }

    async fn uid_move(&mut self, set: &str, dest: &str) -> Result<()> {
        timeout(FETCH_TIMEOUT, self.session.uid_mv(set, dest)).await.map_err(|_| Error::Timeout("move"))??;
        Ok(())
    }

    async fn uid_copy(&mut self, set: &str, dest: &str) -> Result<()> {
        timeout(FETCH_TIMEOUT, self.session.uid_copy(set, dest)).await.map_err(|_| Error::Timeout("copy"))??;
        Ok(())
    }

    async fn uid_store(&mut self, set: &str, query: &str) -> Result<()> {
        let _: Vec<Fetch> = timeout(FETCH_TIMEOUT, async { self.session.uid_store(set, query).await?.try_collect().await })
            .await
            .map_err(|_| Error::Timeout("store"))??;
        Ok(())
    }

    async fn uid_expunge(&mut self, set: &str) -> Result<()> {
        let _: Vec<u32> = timeout(FETCH_TIMEOUT, async { self.session.uid_expunge(set).await?.try_collect().await })
            .await
            .map_err(|_| Error::Timeout("expunge"))??;
        Ok(())
    }

    async fn message_ids_from(&mut self, lo: u32) -> Result<Vec<(u32, Option<String>)>> {
        let fetches: Vec<Fetch> = timeout(FETCH_TIMEOUT, async { self.session.uid_fetch(format!("{lo}:*"), HEADER_QUERY).await?.try_collect().await })
            .await
            .map_err(|_| Error::Timeout("fetch"))??;
        Ok(fetches
            .iter()
            .filter_map(|f| {
                let uid = f.uid.filter(|u| *u >= lo)?;
                Some((uid, f.header().and_then(|h| parse_headers(h).ok()).and_then(|h| h.message_id)))
            })
            .collect())
    }

    async fn message_ids_of(&mut self, set: &str) -> Result<Vec<(u32, Option<String>)>> {
        let fetches: Vec<Fetch> = timeout(FETCH_TIMEOUT, async { self.session.uid_fetch(set, HEADER_QUERY).await?.try_collect().await })
            .await
            .map_err(|_| Error::Timeout("fetch"))??;
        Ok(fetches.iter().filter_map(|f| Some((f.uid?, f.header().and_then(|h| parse_headers(h).ok()).and_then(|h| h.message_id)))).collect())
    }

    async fn flags_of(&mut self, set: &str) -> Result<Vec<(u32, bool, bool)>> {
        let fetches: Vec<Fetch> = timeout(FETCH_TIMEOUT, async { self.session.uid_fetch(set, "(UID FLAGS)").await?.try_collect().await })
            .await
            .map_err(|_| Error::Timeout("fetch"))??;
        Ok(fetches
            .iter()
            .filter_map(|f| {
                let fl = flags_from_iter(f.flags());
                Some((f.uid?, fl.seen, fl.flagged))
            })
            .collect())
    }

    async fn search_message_id(&mut self, id: &str) -> Result<Vec<u32>> {
        let q = format!("HEADER Message-ID {}", imap_quote(id)?);
        let found = timeout(SEARCH_TIMEOUT, self.session.uid_search(q)).await.map_err(|_| Error::Timeout("search"))??;
        Ok(found.into_iter().collect())
    }

    async fn create(&mut self, name: &str) -> Result<()> {
        timeout(Duration::from_secs(20), self.session.create(name)).await.map_err(|_| Error::Timeout("create"))??;
        Ok(())
    }

    async fn list_names(&mut self) -> Result<Vec<String>> {
        let names: Vec<_> = self.session.list(Some(""), Some("*")).await?.try_collect().await?;
        Ok(names.iter().map(|n| n.name().to_owned()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_is_reused_only_for_the_same_folder_and_known_validity() {
        let sel = ("INBOX".to_string(), 7);
        assert!(selection_reusable(true, Some(&sel), "INBOX", Some(7)));
        assert!(!selection_reusable(false, Some(&sel), "INBOX", Some(7)), "not opted in: always SELECT");
        assert!(!selection_reusable(true, Some(&sel), "Sent", Some(7)), "another folder");
        assert!(!selection_reusable(true, Some(&sel), "INBOX", Some(8)), "UIDVALIDITY differs");
        assert!(!selection_reusable(true, Some(&sel), "INBOX", None), "unknown validity re-selects");
        assert!(!selection_reusable(true, None, "INBOX", Some(7)), "nothing selected");
    }

    #[test]
    fn roles_from_special_use() {
        assert_eq!(classify_folder("INBOX", Some("/"), &[]), FolderRole::Inbox);
        assert_eq!(classify_folder("Inbox", Some("."), &[]), FolderRole::Inbox);
        assert_eq!(classify_folder("Gesendet", Some("/"), &["\\Sent"]), FolderRole::Sent);
        assert_eq!(classify_folder("Entw", Some("/"), &["\\Drafts"]), FolderRole::Drafts);
        assert_eq!(classify_folder("Papierkorb", None, &["\\TRASH"]), FolderRole::Trash);
        assert_eq!(classify_folder("Alle", None, &["\\All"]), FolderRole::Archive);
        assert_eq!(classify_folder("Archief", None, &["\\Archive"]), FolderRole::Archive);
        assert_eq!(classify_folder("Pourriel", None, &["\\Junk"]), FolderRole::Junk);
        assert_eq!(classify_folder("X", None, &["\\HasNoChildren", "\\Flagged"]), FolderRole::Other);
    }

    #[test]
    fn roles_from_names_fallback() {
        assert_eq!(classify_folder("INBOX.Sent", Some("."), &[]), FolderRole::Sent);
        assert_eq!(classify_folder("INBOX/Sent Items", Some("/"), &[]), FolderRole::Sent);
        assert_eq!(classify_folder("Deleted Items", Some("/"), &[]), FolderRole::Trash);
        assert_eq!(classify_folder("Spam", None, &[]), FolderRole::Junk);
        assert_eq!(classify_folder("Archives", None, &[]), FolderRole::Archive);
        assert_eq!(classify_folder("Drafts", Some("/"), &[]), FolderRole::Drafts);
        assert_eq!(classify_folder("Projects/Sentinel", Some("/"), &[]), FolderRole::Other);
    }

    #[test]
    fn find_role_skips_unselectable() {
        let f = |n: &str, r, s| Folder { name: n.into(), delimiter: None, role: r, selectable: s };
        let v = vec![f("A", FolderRole::Archive, false), f("B", FolderRole::Archive, true)];
        assert_eq!(find_role(&v, FolderRole::Archive).unwrap().name, "B");
        assert!(find_role(&v, FolderRole::Trash).is_none());
    }

    #[test]
    fn plan_initial_takes_newest_n() {
        let p = plan_sync(None, None, 5, 1000, 200);
        assert_eq!(p, SyncPlan { reset: false, fetch: FetchRange::Seq("801:*".into()) });
        assert_eq!(plan_sync(None, None, 5, 50, 200).fetch, FetchRange::Seq("1:*".into()));
        assert_eq!(plan_sync(None, None, 5, 0, 200).fetch, FetchRange::None);
        assert_eq!(plan_sync(None, None, 5, 3, 0).fetch, FetchRange::Seq("3:*".into()));
    }

    #[test]
    fn plan_incremental() {
        let p = plan_sync(Some(5), Some(120), 5, 300, 200);
        assert_eq!(p, SyncPlan { reset: false, fetch: FetchRange::Uid("121:*".into()) });
        // validity known but cache empty -> initial
        assert_eq!(plan_sync(Some(5), None, 5, 10, 200).fetch, FetchRange::Seq("1:*".into()));
        assert_eq!(plan_sync(Some(5), Some(u32::MAX), 5, 10, 200).fetch, FetchRange::Uid(format!("{}:*", u32::MAX)));
    }

    #[test]
    fn plan_uidvalidity_change_resets() {
        let p = plan_sync(Some(5), Some(120), 6, 300, 200);
        assert!(p.reset);
        assert_eq!(p.fetch, FetchRange::Seq("101:*".into()));
        let p = plan_sync(Some(5), Some(120), 6, 0, 200);
        assert!(p.reset && p.fetch == FetchRange::None);
    }

    #[test]
    fn reconcile_detects_expunge_and_flag_changes() {
        let seen = Flags { seen: true, ..Default::default() };
        let none = Flags::default();
        let local = [(1, none), (2, none), (3, seen)];
        let remote = [(2, seen), (3, seen), (9, none)];
        let (del, upd) = reconcile(&local, &remote);
        assert_eq!(del, vec![1]);
        assert_eq!(upd, vec![(2, seen)]);
        assert_eq!(reconcile(&[], &remote), (vec![], vec![]));
    }

    #[test]
    fn flag_mapping() {
        let f = flags_from_iter(
            vec![Flag::Seen, Flag::Flagged, Flag::Custom("$Junk".into()), Flag::Recent].into_iter(),
        );
        assert_eq!(f, Flags { seen: true, flagged: true, ..Default::default() });
        assert_eq!(flags_from_iter(std::iter::empty()), Flags::default());
    }

    #[test]
    fn permanent_delete_requires_known_uidvalidity() {
        assert!(require_validity(None).is_err());
        assert_eq!(require_validity(Some(7)).unwrap(), 7);
    }

    #[test]
    fn q04_expunge_protects_other_deleted_messages() {
        assert_eq!(others_deleted([7, 3, 9, 5], 5), vec![3, 7, 9]);
        assert!(others_deleted([5], 5).is_empty());
        assert_eq!(uid_set(&[3, 7, 9]), "3,7,9");
    }

    #[test]
    fn flag_commands() {
        assert_eq!(flag_cmd(FlagChange::Seen(true)), "+FLAGS.SILENT (\\Seen)");
        assert_eq!(flag_cmd(FlagChange::Flagged(false)), "-FLAGS.SILENT (\\Flagged)");
    }

    #[test]
    fn summary_from_header_block() {
        let h = b"From: Ann <ann@example.org>\r\nTo: me@example.net\r\nSubject: =?UTF-8?Q?Gr=C3=BC=C3=9Fe?=\r\nMessage-ID: <1@x>\r\n\r\n";
        let s = summary_from_header(&MsgLoc { account: "me@example.net", folder: "INBOX", uid: 4, uidvalidity: 9 }, Flags::default(), 321, Some(1000), h);
        assert_eq!(s.subject, "Gr\u{fc}\u{df}e");
        assert_eq!(s.from, "Ann <ann@example.org>");
        assert_eq!(s.to, "me@example.net");
        assert_eq!(s.date, 1000); // no Date header -> INTERNALDATE
        assert_eq!(s.message_id.as_deref(), Some("1@x"));
        assert_eq!((s.uid, s.uidvalidity, s.size), (4, 9, 321));
        // garbage header still yields a row
        let g = summary_from_header(&MsgLoc { account: "a", folder: "INBOX", uid: 1, uidvalidity: 1 }, Flags::default(), 0, None, b"\x00\x01");
        assert_eq!(g.uid, 1);
    }

    // ---- load older ----

    #[test]
    fn older_window_grows_and_bottoms_out() {
        assert_eq!(older_window(1, 200, 0), None);
        assert_eq!(older_window(0, 200, 0), None);
        assert_eq!(older_window(2, 200, 0), Some((1, 1)));
        assert_eq!(older_window(10_000, 200, 0), Some((9200, 9999)));
        assert_eq!(older_window(10_000, 200, 1), Some((3600, 9999)));
        assert_eq!(older_window(10_000, 200, 2), Some((1, 9999)));
        assert_eq!(older_window(u32::MAX, 1000, 5), Some((1, u32::MAX - 1)));
        assert_eq!(older_window(u32::MAX, 1000, 0), Some((u32::MAX - 4000, u32::MAX - 1)));
        assert_eq!(older_window(500, 0, 0), Some((496, 499))); // count 0 treated as 1
    }

    #[test]
    fn pick_highest_takes_top_count() {
        assert_eq!(pick_highest(vec![5, 1, 9, 3, 9], 2), (vec![5, 9], 2));
        assert_eq!(pick_highest(vec![4, 2], 5), (vec![2, 4], 0));
        assert_eq!(pick_highest(vec![], 3), (vec![], 0));
    }

    #[test]
    fn older_progress_exact_and_estimated() {
        assert_eq!(older_progress(true, 0, 500, 500), (0, true));
        assert_eq!(older_progress(true, 40, 500, 460), (40, false));
        assert_eq!(older_progress(false, 0, 500, 300), (200, false));
        assert_eq!(older_progress(false, 0, 500, 500), (1, false)); // never claims the start unverified
    }

    #[test]
    fn uid_ranges_compact() {
        assert_eq!(uid_ranges(&[]), "");
        assert_eq!(uid_ranges(&[7]), "7");
        assert_eq!(uid_ranges(&[10, 1, 2, 3, 7, 9, 3]), "1:3,7,9:10");
    }

    struct Fake {
        uids: Vec<u32>,
        validity: u32,
        uidnext: Option<u32>,
        fetches: Vec<Vec<u32>>,
        searches: Vec<(u32, u32)>,
        fail_fetch_call: Option<usize>,
    }
    impl Fake {
        fn new(uids: Vec<u32>) -> Self {
            Fake { uids, validity: 7, uidnext: None, fetches: vec![], searches: vec![], fail_fetch_call: None }
        }
    }
    impl OlderServer for Fake {
        async fn select(&mut self, _f: &str) -> Result<(u32, u32, Option<u32>)> {
            let next = self.uidnext.or_else(|| self.uids.iter().max().map(|m| m + 1));
            Ok((self.validity, self.uids.len() as u32, next))
        }
        async fn search_uids(&mut self, lo: u32, hi: u32) -> Result<Vec<u32>> {
            self.searches.push((lo, hi));
            Ok(self.uids.iter().copied().filter(|u| (lo..=hi).contains(u)).collect())
        }
        async fn fetch_headers(&mut self, uids: &[u32]) -> Result<Vec<HeaderRow>> {
            if self.fail_fetch_call == Some(self.fetches.len()) {
                return Err(Error::Imap("boom".into()));
            }
            self.fetches.push(uids.to_vec());
            Ok(uids
                .iter()
                .map(|u| HeaderRow {
                    uid: *u,
                    flags: Flags { seen: u % 2 == 0, ..Default::default() },
                    size: 100,
                    internal_date: Some(1000 + *u as i64),
                    header: format!("Subject: s{u}\r\nFrom: a@x.org\r\nList-Id: <l{u}.x>\r\nReferences: <r{u}@x>\r\n\r\n").into_bytes(),
                })
                .collect())
        }
    }

    const ACC: &str = "me@example.org";
    fn store() -> Storage {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_account(&AccountConfig {
            email: ACC.into(),
            username: "me".into(),
            imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "s".into(), port: 465, tls: Tls::Implicit },
        })
        .unwrap();
        s.replace_folders(ACC, &[Folder { name: "INBOX".into(), delimiter: None, role: FolderRole::Inbox, selectable: true }])
            .unwrap();
        s
    }
    fn cached(s: &Storage, uid: u32, validity: u32) {
        let mut m = summary_from_header(&MsgLoc { account: ACC, folder: "INBOX", uid, uidvalidity: validity }, Flags::default(), 1, Some(5), b"Subject: old\r\n\r\n");
        m.snippet = String::new();
        s.upsert_message(&m).unwrap();
    }

    #[tokio::test]
    async fn load_older_empty_cache_is_first_sync_of_newest() {
        let s = store();
        let mut f = Fake::new((1..=50).collect());
        let r = load_older_with(&mut f, &s, ACC, "INBOX", 20).await.unwrap();
        assert_eq!((r.loaded, r.server_total, r.oldest_uid, r.reached_start), (20, 50, Some(31), false));
        assert!(r.remaining_on_server >= 1);
        assert_eq!(s.uids(ACC, "INBOX").unwrap(), (31..=50).collect::<Vec<_>>());
        // sync state recorded so a later sync_folder is incremental from the top
        assert_eq!(s.folder_sync_state(ACC, "INBOX").unwrap(), (Some(7), Some(51)));
    }

    #[tokio::test]
    async fn load_older_walks_down_to_start_and_keeps_existing_rows() {
        let s = store();
        s.set_folder_sync_state(ACC, "INBOX", 7, Some(101)).unwrap();
        for u in 91..=100 {
            cached(&s, u, 7);
        }
        s.update_flags(ACC, "INBOX", 95, Flags { seen: true, flagged: true, ..Default::default() }).unwrap();
        let mut f = Fake::new((1..=100).collect());
        let r = load_older_with(&mut f, &s, ACC, "INBOX", 30).await.unwrap();
        assert_eq!((r.loaded, r.oldest_uid, r.reached_start), (30, Some(61), false));
        assert_eq!(r.remaining_on_server, 60);
        assert_eq!(f.fetches.len(), 1);
        // existing row (flags) and sync state untouched
        assert!(s.get_message(ACC, "INBOX", 95).unwrap().unwrap().flags.flagged);
        assert_eq!(s.folder_sync_state(ACC, "INBOX").unwrap(), (Some(7), Some(101)));
        let r = load_older_with(&mut f, &s, ACC, "INBOX", 1000).await.unwrap();
        assert_eq!((r.loaded, r.oldest_uid, r.reached_start, r.remaining_on_server), (60, Some(1), true, 0));
        assert_eq!(s.message_count(ACC, "INBOX").unwrap(), 100);
        // flags from the server were stored: even uids are seen
        assert!(s.get_message(ACC, "INBOX", 10).unwrap().unwrap().flags.seen);
        assert!(!s.get_message(ACC, "INBOX", 11).unwrap().unwrap().flags.seen);
        // header fields that used to be dropped are kept, and the new rows are searchable
        let h = s.get_extra_headers(ACC, "INBOX", 10).unwrap().unwrap();
        assert_eq!((h.list_id.as_str(), h.references.as_str()), ("<l10.x>", "r10@x"));
        let found = s
            .search_messages(&crate::search::SearchParams { query: "s10".into(), scopes: vec![crate::search::ScopeKey::new(ACC, "INBOX")], limit: 5, offset: 0, now: 0, tz_offset: 0 })
            .unwrap();
        assert_eq!(found.hits.len(), 1);
        assert!(found.hits[0].bulk);
        // a big batch is fetched in chunks of FETCH_CHUNK, highest first
        let s = store();
        let mut f = Fake::new((1..=450).collect());
        load_older_with(&mut f, &s, ACC, "INBOX", 450).await.unwrap();
        assert_eq!(f.fetches.iter().map(Vec::len).collect::<Vec<_>>(), vec![50, 200, 200]);
        assert!(f.fetches[0][0] > f.fetches[1][0] && f.fetches[1][0] > f.fetches[2][0]);
    }

    #[tokio::test]
    async fn load_older_handles_sparse_uids_and_fewer_than_count() {
        let s = store();
        let mut uids: Vec<u32> = vec![3, 40, 41, 90_000, 90_001, 2_000_000];
        s.set_folder_sync_state(ACC, "INBOX", 7, Some(2_000_001)).unwrap();
        cached(&s, 2_000_000, 7);
        let mut f = Fake::new(uids.clone());
        let r = load_older_with(&mut f, &s, ACC, "INBOX", 100).await.unwrap();
        uids.pop();
        assert_eq!(s.uids(ACC, "INBOX").unwrap(), [vec![3, 40, 41, 90_000, 90_001], vec![2_000_000]].concat());
        assert_eq!((r.loaded, r.reached_start, r.remaining_on_server, r.oldest_uid), (5, true, 0, Some(3)));
        assert!(f.searches.len() >= 3, "window widened: {:?}", f.searches);
        // searched ranges never overlap
        for w in f.searches.windows(2) {
            assert!(w[1].1 < w[0].0);
        }
    }

    #[tokio::test]
    async fn load_older_min_uid_one_and_empty_folder() {
        let s = store();
        s.set_folder_sync_state(ACC, "INBOX", 7, Some(10)).unwrap();
        cached(&s, 1, 7);
        let mut f = Fake::new(vec![1, 2]);
        let r = load_older_with(&mut f, &s, ACC, "INBOX", 200).await.unwrap();
        assert_eq!((r.loaded, r.reached_start, r.remaining_on_server, r.oldest_uid), (0, true, 0, Some(1)));
        assert!(f.searches.is_empty() && f.fetches.is_empty());

        let s = store();
        let mut f = Fake::new(vec![]);
        let r = load_older_with(&mut f, &s, ACC, "INBOX", 200).await.unwrap();
        assert_eq!(r, LoadOlderReport { loaded: 0, remaining_on_server: 0, server_total: 0, oldest_uid: None, reached_start: true });
    }

    #[tokio::test]
    async fn load_older_aborts_on_uidvalidity_change_without_touching_cache() {
        let s = store();
        s.set_folder_sync_state(ACC, "INBOX", 7, Some(11)).unwrap();
        for u in 5..=10 {
            cached(&s, u, 7);
        }
        let mut f = Fake::new((1..=10).collect());
        f.validity = 8; // server renumbered
        let e = load_older_with(&mut f, &s, ACC, "INBOX", 50).await.unwrap_err();
        assert!(e.to_string().contains("UIDVALIDITY changed"));
        assert_eq!(s.uids(ACC, "INBOX").unwrap(), (5..=10).collect::<Vec<_>>());
        assert_eq!(s.folder_sync_state(ACC, "INBOX").unwrap(), (Some(7), Some(11)));
        assert!(f.fetches.is_empty());
        // cached rows carrying a foreign validity (state row says otherwise) also abort
        let s = store();
        cached(&s, 5, 3);
        let mut f = Fake::new((1..=10).collect());
        assert!(load_older_with(&mut f, &s, ACC, "INBOX", 5).await.is_err());
    }

    #[tokio::test]
    async fn load_older_failure_keeps_cache_contiguous_from_the_top() {
        let s = store();
        s.set_folder_sync_state(ACC, "INBOX", 7, Some(1001)).unwrap();
        cached(&s, 1000, 7);
        let mut f = Fake::new((1..=1000).collect());
        f.fail_fetch_call = Some(1); // second chunk fails
        assert!(load_older_with(&mut f, &s, ACC, "INBOX", 500).await.is_err());
        let have = s.uids(ACC, "INBOX").unwrap();
        // picked 500..=999 in chunks 500..700, 700..900, 900..1000: only the top chunk landed, no gaps
        assert_eq!(have.first(), Some(&900));
        assert_eq!(have.len(), 101);
    }

    #[tokio::test]
    async fn load_older_clamps_count() {
        let s = store();
        let mut f = Fake::new((1..=1500).collect());
        let r = load_older_with(&mut f, &s, ACC, "INBOX", 99_999).await.unwrap();
        assert_eq!(r.loaded, MAX_LOAD_OLDER);
        let s = store();
        let mut f = Fake::new((1..=5).collect());
        assert_eq!(load_older_with(&mut f, &s, ACC, "INBOX", 0).await.unwrap().loaded, 1);
    }
}

#[cfg(test)]
mod server_search_tests {
    use super::*;

    #[test]
    fn caching_search_hits_leaves_sync_planning_unchanged() {
        assert_eq!(cacheable_uids(&[120, 100, 7], 101), vec![100, 7]);
        assert!(cacheable_uids(&[101, 500], 101).is_empty());
        // Only sub-UIDNEXT hits are cached, so the cached max UID (and so the plan) is unchanged.
        let before = plan_sync(Some(7), Some(100), 7, 100, 50);
        assert!(cacheable_uids(&[300, 50], 101).iter().all(|u| *u <= 100));
        let after = plan_sync(Some(7), Some(100), 7, 100, 50);
        assert_eq!(before.fetch, after.fetch);
    }

    #[test]
    fn quoting_escapes_and_rejects() {
        assert_eq!(imap_quote("bob").unwrap(), "\"bob\"");
        assert_eq!(imap_quote("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
        for bad in ["a\r\nA1 DELETE", "a\nb", "x\0y", "caf\u{e9}", "tab\there", "", "   ", &"x".repeat(201)] {
            assert!(imap_quote(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn criteria_are_built_safely() {
        let q = ServerQuery {
            from: Some("alice@example.com".into()),
            subject: Some("say \"hi\" (now)".into()),
            since: Some("2026-11-05".into()),
            unseen: Some(true),
            ..Default::default()
        };
        assert_eq!(
            build_search_criteria(&q).unwrap(),
            "FROM \"alice@example.com\" SUBJECT \"say \\\"hi\\\" (now)\" SINCE 5-Nov-2026 UNSEEN"
        );
        assert!(build_search_criteria(&ServerQuery::default()).is_err());
        // an injection attempt stays inside one quoted string or is rejected
        let q = ServerQuery { text: Some("x\" OR ALL \"".into()), ..Default::default() };
        assert_eq!(build_search_criteria(&q).unwrap(), "BODY \"x\\\" OR ALL \\\"\"");
        for d in ["2026-13-01", "2026-1-1", "26-01-01", "2026-01-01 UNSEEN", "2026-01-1x", "1969-01-01"] {
            let q = ServerQuery { before: Some(d.into()), ..Default::default() };
            assert!(build_search_criteria(&q).is_err(), "{d}");
        }
    }
}
