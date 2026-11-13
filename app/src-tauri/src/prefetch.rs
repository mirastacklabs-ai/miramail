//! Download new mail in advance: bodies of the newest cached messages are fetched in the background so the first tap
//! opens from the local cache. Settings > Sync > "Download new mail in advance" (default on).
//!
//! Rules: background lane only (`AppState::connect`, never the interactive lane), `BODY.PEEK[]` (never sets \Seen, moves
//! nothing), messages from the last 14 days, at most 50 per account per pass, none larger than 2 MB, newest first, stored
//! through the normal cache path (`ImapClient::get_body` -> `Storage::set_body`), so the account's cache limits apply and
//! nothing is ever evicted. iPhone: only while the user is present and Low Data Mode is off; the background check
//! (`bgcheck.rs`) prefetches only the few messages it announces. Logs counts, durations and bytes only.

use crate::state::{AppState, INBOX};
use mailcore::{CachePolicy, Error, FolderRole, MessageSummary};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

pub const WINDOW_DAYS: i64 = 14;
pub const MAX_PER_PASS: usize = 50;
pub const MAX_MESSAGE_BYTES: u32 = 2 * 1024 * 1024;
/// Total bytes per pass on an unmetered path.
pub const MAX_PASS_BYTES: u64 = 25 * 1024 * 1024;
/// Metered path (iOS: cellular / hotspot): small messages only, and little per pass.
pub const METERED_MESSAGE_BYTES: u32 = 256 * 1024;
pub const METERED_PASS_BYTES: u64 = 5 * 1024 * 1024;
/// How many of the newest cached rows a pass looks at (more than `MAX_PER_PASS` so cached/oversized rows do not hide older ones).
const SCAN_ROWS: u32 = 300;
/// Messages per background connection: the account's lock is released between batches so queued commands get their turn.
const BATCH: usize = 3;
/// One batch (connect + fetches) never runs longer than this; LOGOUT adds at most `LOGOUT_TIME`.
const BATCH_TIME: Duration = Duration::from_secs(20);
const MESSAGE_TIME: Duration = Duration::from_secs(30);
const CONNECT_TIME: Duration = Duration::from_secs(20);
const LOGOUT_TIME: Duration = Duration::from_secs(3);
/// After a failed pass the account is left alone this long.
pub const BACKOFF: Duration = Duration::from_secs(300);
/// A date further in the future than this is not trusted (a bogus Date header must not sort first forever).
const FUTURE_SLACK: i64 = 86_400;
/// How long after a new-mail sync the pass starts: lets the rules / Junk moves (`automation::notify`) move mail away first.
pub const RULES_GRACE: Duration = Duration::from_secs(4);
const MAX_SKIPS: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cand {
    pub uid: u32,
    /// Unix seconds.
    pub date: i64,
    /// RFC822.SIZE from the summary; 0 = unknown.
    pub size: u32,
    pub has_body: bool,
}

impl From<&MessageSummary> for Cand {
    fn from(m: &MessageSummary) -> Self {
        Cand { uid: m.uid, date: m.date, size: m.size, has_body: m.has_body }
    }
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub window_days: i64,
    pub max_count: usize,
    pub max_message_bytes: u32,
    pub max_pass_bytes: u64,
    /// Only these UIDs (the background check's announced messages); None = any.
    pub only: Option<Vec<u32>>,
    pub policy: CachePolicy,
    /// Bytes of bodies the account already caches (for the policy's size cap).
    pub cached_bytes: u64,
}

impl Limits {
    pub fn standard(policy: CachePolicy, cached_bytes: u64, metered: bool) -> Self {
        Limits {
            window_days: WINDOW_DAYS,
            max_count: MAX_PER_PASS,
            max_message_bytes: if metered { METERED_MESSAGE_BYTES } else { MAX_MESSAGE_BYTES },
            max_pass_bytes: if metered { METERED_PASS_BYTES } else { MAX_PASS_BYTES },
            only: None,
            policy,
            cached_bytes,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// UIDs to fetch, newest first.
    pub fetch: Vec<u32>,
    pub skipped_big: usize,
}

/// Pure: which UIDs to download. Not yet cached, dated within the window (and not more than a day in the future), at
/// most `max_count`, `size` known and within the per-message cap (an unknown size is skipped: the sync always stores
/// RFC822.SIZE), the pass total within `max_pass_bytes`, and only while the account's cache policy would still keep the
/// body. Newest first (ties: higher UID first).
pub fn select(cands: &[Cand], now: i64, lim: &Limits) -> Selection {
    let mut rows: Vec<&Cand> = cands.iter().filter(|c| lim.only.as_ref().is_none_or(|o| o.contains(&c.uid))).collect();
    rows.sort_by(|a, b| b.date.cmp(&a.date).then(b.uid.cmp(&a.uid)));
    let mut sel = Selection::default();
    let mut added: u64 = 0;
    for c in rows {
        if sel.fetch.len() >= lim.max_count {
            break;
        }
        if c.has_body || c.date <= 0 || c.date > now + FUTURE_SLACK || now - c.date > lim.window_days * 86_400 {
            continue;
        }
        if c.size == 0 {
            continue;
        }
        if c.size > lim.max_message_bytes {
            sel.skipped_big += 1;
            continue;
        }
        if added + u64::from(c.size) > lim.max_pass_bytes {
            continue;
        }
        if !lim.policy.allows(now, c.date, lim.cached_bytes + added) {
            continue;
        }
        added += u64::from(c.size);
        sel.fetch.push(c.uid);
    }
    sel
}

/// Pure: may prefetch look at this folder at all? Never Junk or Trash.
pub fn folder_allowed(role: FolderRole) -> bool {
    !matches!(role, FolderRole::Junk | FolderRole::Trash)
}

/// Pure: whether a failed `get_body` concerns one message only (gone, UIDVALIDITY changed, a refusal for that UID) and
/// the pass goes on. Everything else (timeout, I/O, TLS, auth, storage) means the connection or the cache is bad.
pub fn skippable(e: &Error) -> bool {
    matches!(e, Error::NotFound(_) | Error::Imap(_) | Error::Parse(_))
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub fetched: usize,
    pub skipped_big: usize,
    pub bytes: u64,
    /// The pass ended on a connection / timeout / storage problem (not on a limit or the user leaving).
    pub failed: bool,
}

// ---------------------------------------------------------------- one pass per account

#[derive(Debug, PartialEq, Eq)]
pub enum Admit {
    /// Nobody is running for this account: the caller runs the pass.
    Start,
    /// A pass is running; the folder waits behind it.
    Queued,
    /// The account is in backoff after a failed pass.
    Backoff,
}

/// The pure state machine behind "one pass per account at a time, INBOX first, backoff after a failure".
#[derive(Debug, Default)]
pub struct Queue {
    running: HashMap<String, Vec<String>>,
    backoff: HashMap<String, Instant>,
}

impl Queue {
    pub fn request(&mut self, account: &str, folder: &str, now: Instant) -> Admit {
        if self.backoff.get(account).is_some_and(|&until| now < until) {
            return Admit::Backoff;
        }
        self.backoff.remove(account);
        match self.running.get_mut(account) {
            Some(q) => {
                if !q.iter().any(|f| f == folder) {
                    q.push(folder.to_string());
                }
                Admit::Queued
            }
            None => {
                self.running.insert(account.to_string(), Vec::new());
                Admit::Start
            }
        }
    }

    /// The running pass finished. `failed`: back off and drop what waits. `keep` false (setting off, user left) drops it
    /// too. Returns the next folder to run (INBOX first), or None when the account is idle again.
    pub fn finish(&mut self, account: &str, failed: bool, keep: bool, now: Instant) -> Option<String> {
        if failed {
            self.backoff.insert(account.to_string(), now + BACKOFF);
        }
        let q = self.running.get_mut(account)?;
        if failed || !keep || q.is_empty() {
            self.running.remove(account);
            return None;
        }
        let i = q.iter().position(|f| f == INBOX).unwrap_or(0);
        Some(q.remove(i))
    }

    /// Drop the running entry (panic / cancel). Starts no backoff.
    pub fn abandon(&mut self, account: &str) {
        self.running.remove(account);
    }
}

static QUEUE: Mutex<Option<Queue>> = Mutex::new(None);

fn with_queue<T>(f: impl FnOnce(&mut Queue) -> T) -> T {
    let mut g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Queue::default))
}

/// Removes the account's running entry if the task dies (panic, cancel) before it finished normally.
struct RunGuard {
    account: String,
    armed: bool,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if self.armed {
            with_queue(|q| q.abandon(&self.account));
        }
    }
}

// ---------------------------------------------------------------- environment

/// Prefetch is on in the settings, the user is present and (iPhone) Low Data Mode is off / the path is known.
fn allowed_now(st: &AppState) -> bool {
    st.sync_cfg.borrow().prefetch && crate::presence::user_present() && !low_data()
}

fn low_data() -> bool {
    #[cfg(target_os = "ios")]
    {
        tauri_plugin_mailcheck::low_data_mode()
    }
    #[cfg(not(target_os = "ios"))]
    {
        false
    }
}

fn metered() -> bool {
    #[cfg(target_os = "ios")]
    {
        tauri_plugin_mailcheck::path_expensive()
    }
    #[cfg(not(target_os = "ios"))]
    {
        false
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Background-lane connection that gives up when `keep_going` turns false or `until` passes (the lock wait and the
/// connect have no timeout of their own).
async fn connect_bounded(st: &AppState, account: &str, until: Instant, keep_going: &impl Fn() -> bool) -> Option<crate::state::Conn> {
    let fut = st.connect(account);
    tokio::pin!(fut);
    loop {
        tokio::select! {
            r = &mut fut => return r.ok(),
            _ = tokio::time::sleep(Duration::from_millis(300)) => {
                if !keep_going() || Instant::now() >= until {
                    return None;
                }
            }
        }
    }
}

/// Polite LOGOUT, bounded: the account's lock is not held longer than `LOGOUT_TIME` for it. Error paths just drop.
async fn close_bounded(conn: crate::state::Conn) {
    let _ = tokio::time::timeout(LOGOUT_TIME, conn.close()).await;
}

/// One pass over `folder` of `account`. `keep_going` is asked before every message; `deadline` caps the whole pass.
pub async fn pass(
    st: &AppState,
    account: &str,
    folder: &str,
    only: Option<Vec<u32>>,
    max_count: usize,
    deadline: Option<Instant>,
    keep_going: impl Fn() -> bool,
) -> Stats {
    let storage = st.storage.clone();
    let mut stats = Stats::default();
    let role = storage.list_folders(account).ok().and_then(|v| v.into_iter().find(|f| f.name == folder).map(|f| f.role));
    if role.is_some_and(|r| !folder_allowed(r)) {
        return stats;
    }
    let Ok(rows) = storage.list_messages(account, folder, SCAN_ROWS, 0) else { return stats };
    let mut lim = Limits::standard(storage.cache_policy(account), storage.cached_body_bytes_fast(account), metered());
    lim.only = only;
    lim.max_count = max_count;
    let cands: Vec<Cand> = rows.iter().map(Cand::from).collect();
    let sel = select(&cands, now_secs(), &lim);
    stats.skipped_big = sel.skipped_big;
    let expired = || deadline.is_some_and(|d| Instant::now() >= d);
    let mut skips = 0usize;
    'batches: for chunk in sel.fetch.chunks(BATCH) {
        if !keep_going() || expired() {
            break;
        }
        let batch_end = {
            let b = Instant::now() + BATCH_TIME;
            deadline.map_or(b, |d| d.min(b))
        };
        let until = batch_end.min(Instant::now() + CONNECT_TIME);
        let Some(mut conn) = connect_bounded(st, account, until, &keep_going).await else {
            // No connection in time: a failure unless it was just the user leaving or the budget ending.
            stats.failed = keep_going() && !expired();
            break;
        };
        for uid in chunk {
            if !keep_going() || Instant::now() >= batch_end {
                close_bounded(conn).await;
                break 'batches;
            }
            // The row may be gone (a rule moved it, the user deleted it) or already cached; the policy may have filled up.
            let Ok(Some(m)) = storage.get_message(account, folder, *uid) else { continue };
            if m.has_body || !lim.policy.allows(now_secs(), m.date, storage.cached_body_bytes_fast(account)) {
                continue;
            }
            let left = batch_end.saturating_duration_since(Instant::now()).min(MESSAGE_TIME);
            match tokio::time::timeout(left, conn.client.get_body(&storage, account, folder, *uid)).await {
                Ok(Ok(raw)) => {
                    // Counted only when the cache really kept it (the policy can decline inside `set_body`).
                    if storage.get_message(account, folder, *uid).ok().flatten().is_some_and(|m| m.has_body) {
                        stats.fetched += 1;
                        stats.bytes += raw.len() as u64;
                    } else {
                        close_bounded(conn).await;
                        break 'batches;
                    }
                }
                Ok(Err(e)) if skippable(&e) => {
                    skips += 1;
                    if skips >= MAX_SKIPS {
                        stats.failed = true;
                        close_bounded(conn).await;
                        break 'batches;
                    }
                }
                // Timeout / I/O / storage: the connection may be dead. Drop it without LOGOUT.
                Ok(Err(_)) | Err(_) => {
                    stats.failed = !expired() && keep_going();
                    drop(conn);
                    break 'batches;
                }
            }
        }
        close_bounded(conn).await;
    }
    stats
}

fn log_pass(account_n: usize, folder_kind: &str, s: &Stats, took: Duration) {
    crate::safety_cmds::perf::log(&format!(
        "prefetch: account_n={account_n} folder={folder_kind} fetched={} skipped_big={} bytes={} took={}ms",
        s.fetched, s.skipped_big, s.bytes, took.as_millis()
    ));
}

fn account_n(st: &AppState, account: &str) -> usize {
    st.storage.list_accounts().ok().and_then(|v| v.iter().position(|a| a.email == account)).map_or(0, |i| i + 1)
}

/// Ask for a pass over `folder` (INBOX, or a live folder) after `delay`. One pass per account at a time; a request that
/// arrives meanwhile is queued (INBOX first). No-op when the setting is off, the user is away, Low Data Mode is on, or
/// the account is in backoff after a failed pass.
pub fn kick(app: &AppHandle, account: &str, folder: &str, delay: Duration) {
    let (app, account, folder) = (app.clone(), account.to_string(), folder.to_string());
    tauri::async_runtime::spawn(async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let st = app.state::<AppState>();
        if !allowed_now(&st) {
            return;
        }
        if with_queue(|q| q.request(&account, &folder, Instant::now())) != Admit::Start {
            return;
        }
        let mut guard = RunGuard { account: account.clone(), armed: true };
        let mut next = Some(folder);
        while let Some(f) = next.take() {
            let mut failed = false;
            if allowed_now(&st) {
                let t = Instant::now();
                let s = pass(&st, &account, &f, None, MAX_PER_PASS, None, || allowed_now(&st)).await;
                failed = s.failed;
                if s.fetched + s.skipped_big > 0 {
                    log_pass(account_n(&st, &account), if f == INBOX { "inbox" } else { "live" }, &s, t.elapsed());
                }
            }
            let keep = allowed_now(&st);
            next = with_queue(|q| q.finish(&account, failed, keep, Instant::now()));
        }
        guard.armed = false;
    });
}

/// Kick an INBOX pass for every account (the setting was just turned on).
pub fn kick_all(app: &AppHandle) {
    let st = app.state::<AppState>();
    for a in st.storage.list_accounts().unwrap_or_default() {
        kick(app, &a.email, INBOX, Duration::ZERO);
    }
}

/// Background check (iPhone): bodies of the announced messages only, within `deadline`. Returns the stats.
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub async fn for_announced(st: &AppState, account: &str, uids: Vec<u32>, deadline: Instant, cancelled: impl Fn() -> bool) -> Stats {
    if uids.is_empty() || !st.sync_cfg.borrow().prefetch || low_data() {
        return Stats::default();
    }
    let t = Instant::now();
    let n = uids.len().min(crate::notify::MAX_INDIVIDUAL_BG);
    let s = pass(st, account, INBOX, Some(uids), n, Some(deadline), || !cancelled()).await;
    log_pass(account_n(st, account), "bg", &s, t.elapsed());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn c(uid: u32, days_ago: i64, size: u32, has_body: bool) -> Cand {
        Cand { uid, date: NOW - days_ago * DAY, size, has_body }
    }
    fn lim() -> Limits {
        Limits::standard(CachePolicy::default(), 0, false)
    }

    #[test]
    fn newest_first_uncached_in_window_only() {
        let cands = [c(1, 20, 1000, false), c(2, 3, 1000, false), c(3, 1, 1000, false), c(4, 2, 1000, true), c(5, 14, 1000, false), c(6, 15, 1000, false)];
        let s = select(&cands, NOW, &lim());
        assert_eq!(s.fetch, vec![3, 2, 5], "cached (4) and older than 14 days (1, 6) are left out");
        assert_eq!(s.skipped_big, 0);
    }

    #[test]
    fn at_most_fifty_and_big_or_unknown_sizes_are_skipped() {
        let mut cands: Vec<Cand> = (1..=80).map(|u| Cand { uid: u, date: NOW - u as i64, size: 5000, has_body: false }).collect();
        cands.push(c(900, 0, MAX_MESSAGE_BYTES + 1, false));
        cands.push(c(901, 0, MAX_MESSAGE_BYTES, false));
        cands.push(c(902, 0, 0, false));
        let s = select(&cands, NOW, &lim());
        assert_eq!(s.fetch.len(), 50);
        assert_eq!(s.fetch[0], 901, "exactly 2 MB is allowed and newest");
        assert_eq!(s.skipped_big, 1, "only the 2 MB + 1 message counts as big; unknown size is not fetched");
        assert!(!s.fetch.contains(&902) && !s.fetch.contains(&900));
    }

    #[test]
    fn the_cache_policy_is_respected_including_what_this_pass_adds() {
        let p = CachePolicy { keep_days: Some(2), max_mb: None };
        let s = select(&[c(1, 1, 10, false), c(2, 5, 10, false)], NOW, &Limits::standard(p, 0, false));
        assert_eq!(s.fetch, vec![1], "older than keep_days is not cached, so not fetched");
        let p = CachePolicy { keep_days: None, max_mb: Some(1) };
        let mb = 1024 * 1024;
        let s = select(&[c(1, 0, 600 * 1024, false), c(2, 1, 600 * 1024, false), c(3, 2, 600 * 1024, false)], NOW, &Limits::standard(p, 0, false));
        assert_eq!(s.fetch, vec![1, 2], "the cap is reached after the second (the policy stops at the cap, nothing is evicted)");
        let s = select(&[c(1, 0, 10, false)], NOW, &Limits::standard(p, mb, false));
        assert!(s.fetch.is_empty(), "cap already reached");
    }

    #[test]
    fn only_restricts_to_the_announced_messages() {
        let cands = [c(1, 0, 10, false), c(2, 1, 10, false), c(3, 2, 10, false)];
        let mut l = lim();
        l.only = Some(vec![3, 1]);
        l.max_count = 5;
        assert_eq!(select(&cands, NOW, &l).fetch, vec![1, 3]);
        l.only = Some(vec![]);
        assert!(select(&cands, NOW, &l).fetch.is_empty());
    }

    #[test]
    fn messages_without_a_date_are_not_guessed_at() {
        let s = select(&[Cand { uid: 1, date: 0, size: 10, has_body: false }], NOW, &lim());
        assert!(s.fetch.is_empty());
    }

    #[test]
    fn future_dated_mail_is_not_trusted() {
        let s = select(&[c(1, -2, 10, false), Cand { uid: 2, date: NOW + 3600, size: 10, has_body: false }], NOW, &lim());
        assert_eq!(s.fetch, vec![2], "one hour ahead (clock skew) is fine; two days ahead is not");
    }

    #[test]
    fn metered_paths_take_small_messages_and_a_small_total() {
        let l = Limits::standard(CachePolicy::default(), 0, true);
        let mut cands: Vec<Cand> = (1..=30).map(|u| Cand { uid: u, date: NOW - u as i64, size: 200 * 1024, has_body: false }).collect();
        cands.push(c(99, 0, 300 * 1024, false));
        let s = select(&cands, NOW, &l);
        assert_eq!(s.fetch.len(), 25, "5 MB / 200 KB");
        assert_eq!(s.skipped_big, 1, "300 KB is over the metered per-message cap");
        assert_eq!(select(&cands, NOW, &lim()).fetch.len(), 31, "unmetered: everything fits in 25 MB");
        let many: Vec<Cand> = (1..=50).map(|u| Cand { uid: u, date: NOW - u as i64, size: 1024 * 1024, has_body: false }).collect();
        assert_eq!(select(&many, NOW, &lim()).fetch.len(), 25, "25 MB per pass");
    }

    #[test]
    fn junk_and_trash_are_never_prefetched() {
        assert!(!folder_allowed(FolderRole::Junk) && !folder_allowed(FolderRole::Trash));
        assert!(folder_allowed(FolderRole::Inbox) && folder_allowed(FolderRole::Other) && folder_allowed(FolderRole::Archive));
    }

    #[test]
    fn only_a_single_messages_trouble_is_skipped() {
        assert!(skippable(&Error::NotFound("x".into())) && skippable(&Error::Imap("UIDVALIDITY changed".into())));
        assert!(!skippable(&Error::Timeout("fetch")) && !skippable(&Error::AuthFailed));
        assert!(!skippable(&Error::Io(std::io::Error::other("x"))));
    }

    #[test]
    fn queue_runs_one_pass_per_account_inbox_first() {
        let t = Instant::now();
        let mut q = Queue::default();
        assert_eq!(q.request("a", "INBOX", t), Admit::Start);
        assert_eq!(q.request("a", "Work", t), Admit::Queued);
        assert_eq!(q.request("a", "Work", t), Admit::Queued, "no duplicates");
        assert_eq!(q.request("a", "INBOX", t), Admit::Queued);
        assert_eq!(q.request("b", "INBOX", t), Admit::Start, "accounts are independent");
        assert_eq!(q.finish("a", false, true, t).as_deref(), Some("INBOX"), "INBOX jumps the queue");
        assert_eq!(q.finish("a", false, true, t).as_deref(), Some("Work"));
        assert_eq!(q.finish("a", false, true, t), None, "idle again");
        assert_eq!(q.request("a", "INBOX", t), Admit::Start);
    }

    #[test]
    fn queue_backs_off_after_a_failure_and_drops_what_waited() {
        let t = Instant::now();
        let mut q = Queue::default();
        assert_eq!(q.request("a", "INBOX", t), Admit::Start);
        assert_eq!(q.request("a", "Work", t), Admit::Queued);
        assert_eq!(q.finish("a", true, true, t), None, "failed: nothing else runs");
        assert_eq!(q.request("a", "INBOX", t + Duration::from_secs(60)), Admit::Backoff);
        assert_eq!(q.request("a", "INBOX", t + BACKOFF + Duration::from_secs(1)), Admit::Start, "after the backoff");
    }

    #[test]
    fn queue_drops_waiters_when_prefetch_stops_and_abandon_frees_the_account() {
        let t = Instant::now();
        let mut q = Queue::default();
        q.request("a", "INBOX", t);
        q.request("a", "Work", t);
        assert_eq!(q.finish("a", false, false, t), None);
        assert_eq!(q.request("a", "INBOX", t), Admit::Start, "no backoff for a stop");
        q.abandon("a");
        assert_eq!(q.request("a", "INBOX", t), Admit::Start, "a dead task does not wedge the account");
    }
}
