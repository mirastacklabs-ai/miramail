//! Offline-cache controls: how big the local cache is per account, a per-account policy that STOPS caching
//! message bodies beyond a limit, and the explicit "clear cached bodies" action.
//!
//! Nothing here ever deletes on its own: the policy only decides whether a body that was just fetched is
//! stored (headers and snippets are always kept), and [`Storage::clear_cached_bodies`] runs only when the user
//! asks for it. Clearing touches body BLOBs and extracted body text only; the messages stay on the server and
//! the header rows stay in the cache, so the folder lists keep working.

use crate::{Result, Storage};
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// Per-account limits. `None` = no limit. `keep_days`: bodies of messages older than this many days are not cached.
/// `max_mb`: once the account's cached bodies reach this many megabytes no further bodies are cached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CachePolicy {
    pub keep_days: Option<u32>,
    pub max_mb: Option<u32>,
}

impl CachePolicy {
    pub const MAX_DAYS: u32 = 36_500;
    pub const MAX_MB: u32 = 1_000_000;

    /// Reject nonsense (0 days / 0 MB would silently disable caching; use a real limit or none).
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.keep_days.is_some_and(|d| d == 0 || d > Self::MAX_DAYS) {
            return Err("Keep bodies for at least 1 day.".into());
        }
        if self.max_mb.is_some_and(|m| m == 0 || m > Self::MAX_MB) {
            return Err("The size limit must be at least 1 MB.".into());
        }
        Ok(())
    }

    /// Pure decision: may a body of a message dated `msg_date` (Unix seconds) be cached at `now` (Unix seconds),
    /// given that the account already caches `cached_bytes` of bodies?
    pub fn allows(&self, now: i64, msg_date: i64, cached_bytes: u64) -> bool {
        if let Some(d) = self.keep_days {
            // A message without a usable date (<= 0) is always allowed: we cannot tell that it is old.
            if msg_date > 0 && now - msg_date > i64::from(d) * 86_400 {
                return false;
            }
        }
        if let Some(m) = self.max_mb {
            if cached_bytes >= u64::from(m) * 1024 * 1024 {
                return false;
            }
        }
        true
    }
}

/// What the cache holds for one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheUsage {
    /// Cached message rows (headers) for the account.
    pub messages: u64,
    /// Rows whose body is cached.
    pub with_body: u64,
    /// Bytes of cached raw bodies.
    pub body_bytes: u64,
    /// Bytes of extracted search text.
    pub text_bytes: u64,
}

const BODY_BYTES_TTL: std::time::Duration = std::time::Duration::from_secs(30);

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Storage {
    /// Install (or with a default policy, remove) the limits for an account. Held in memory; the app persists them.
    pub fn set_cache_policy(&self, account: &str, policy: CachePolicy) {
        let mut m = self.policies.lock().unwrap_or_else(|e| e.into_inner());
        if policy == CachePolicy::default() {
            m.remove(account);
        } else {
            m.insert(account.to_string(), policy);
        }
    }

    pub fn cache_policy(&self, account: &str) -> CachePolicy {
        self.policies.lock().unwrap_or_else(|e| e.into_inner()).get(account).copied().unwrap_or_default()
    }

    /// Bytes of cached raw bodies for the account, from a short-lived in-memory counter (re-measured with one
    /// SUM after `BODY_BYTES_TTL`, bumped by `note_body_stored`), so a batch of body stores does not rescan the table each time.
    fn cached_body_bytes_fast(&self, account: &str) -> u64 {
        {
            let m = self.body_bytes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((n, at)) = m.get(account) {
                if at.elapsed() < BODY_BYTES_TTL {
                    return *n;
                }
            }
        }
        let n = self.cached_body_bytes(account).unwrap_or(0);
        self.body_bytes.lock().unwrap_or_else(|e| e.into_inner()).insert(account.to_string(), (n, std::time::Instant::now()));
        n
    }

    /// Add a just-stored body to the running counter (no-op until the first measurement).
    pub(crate) fn note_body_stored(&self, account: &str, len: u64) {
        if let Some((n, _)) = self.body_bytes.lock().unwrap_or_else(|e| e.into_inner()).get_mut(account) {
            *n += len;
        }
    }

    /// Whether a raw body is cached for the message, without loading the blob.
    pub fn has_cached_body(&self, account: &str, folder: &str, uid: u32) -> Result<bool> {
        let n: i64 = self.lock()?.query_row(
            "SELECT COUNT(*) FROM messages WHERE account=?1 AND folder=?2 AND uid=?3 AND body_raw IS NOT NULL",
            params![account, folder, uid],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Bytes of cached raw bodies for the account (exact: one SUM over the account's rows).
    pub fn cached_body_bytes(&self, account: &str) -> Result<u64> {
        let n: i64 = self.lock()?.query_row("SELECT COALESCE(SUM(LENGTH(body_raw)),0) FROM messages WHERE account=?1", [account], |r| r.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// Whether a freshly fetched body of a message dated `msg_date` may be stored under the account's policy.
    pub(crate) fn body_cache_allowed(&self, account: &str, msg_date: i64) -> bool {
        let p = self.cache_policy(account);
        if p == CachePolicy::default() {
            return true;
        }
        let bytes = if p.max_mb.is_some() { self.cached_body_bytes_fast(account) } else { 0 };
        p.allows(now_secs(), msg_date, bytes)
    }

    pub fn cache_usage(&self, account: &str) -> Result<CacheUsage> {
        let c = self.lock()?;
        let (messages, with_body, body, text): (i64, i64, i64, i64) = c.query_row(
            "SELECT COUNT(*), COALESCE(SUM(body_raw IS NOT NULL),0), COALESCE(SUM(LENGTH(body_raw)),0), COALESCE(SUM(LENGTH(body_text)),0) FROM messages WHERE account=?1",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        Ok(CacheUsage { messages: messages as u64, with_body: with_body as u64, body_bytes: body.max(0) as u64, text_bytes: text.max(0) as u64 })
    }

    /// Size in bytes of the whole database (all accounts) as SQLite sees it: pages in use plus free pages.
    pub fn db_size_bytes(&self) -> Result<u64> {
        let c = self.lock()?;
        let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok((pages.max(0) as u64) * (size.max(0) as u64))
    }

    /// User-requested: drop the cached raw bodies and extracted body text of one account. Rows (headers, flags,
    /// snippets) stay; nothing on the server is touched. Returns how many rows had a body.
    pub fn clear_cached_bodies(&self, account: &str) -> Result<u64> {
        let c = self.lock()?;
        let n = c.execute(
            "UPDATE messages SET body_raw=NULL, body_text='', body_indexed=0 WHERE account=?1 AND (body_raw IS NOT NULL OR body_text<>'')",
            [account],
        )?;
        // Hand the freed pages back to the OS-visible file as far as SQLite can without a full VACUUM.
        let _ = c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        self.body_bytes.lock().unwrap_or_else(|e| e.into_inner()).remove(account);
        Ok(n as u64)
    }

    /// `(uid, date)` of every cached message of a folder, oldest first (mailbox export order).
    pub fn uids_with_dates(&self, account: &str, folder: &str) -> Result<Vec<(u32, i64)>> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT uid, date FROM messages WHERE account=?1 AND folder=?2 ORDER BY uid")?;
        let rows = st.query_map(params![account, folder], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// The UID of a cached message with this Message-ID (ids without `<>`), if exactly one row matches.
    pub fn uid_by_message_id(&self, account: &str, folder: &str, message_id: &str) -> Result<Option<u32>> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT uid FROM messages WHERE account=?1 AND folder=?2 AND message_id=?3 LIMIT 2")?;
        let v: Vec<u32> = st.query_map(params![account, folder, message_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        Ok(if v.len() == 1 { Some(v[0]) } else { None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn acct(s: &Storage, email: &str) {
        s.upsert_account(&AccountConfig {
            email: email.into(),
            imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "s".into(), port: 465, tls: Tls::Implicit },
            username: email.into(),
        })
        .unwrap();
        s.replace_folders(email, &[Folder { name: "INBOX".into(), delimiter: None, role: FolderRole::Inbox, selectable: true }]).unwrap();
    }
    fn msg(account: &str, uid: u32, date: i64) -> MessageSummary {
        MessageSummary {
            account: account.into(),
            folder: "INBOX".into(),
            uid,
            uidvalidity: 7,
            message_id: Some(format!("m{uid}@x")),
            subject: "s".into(),
            from: "a@x".into(),
            to: "b@x".into(),
            date,
            flags: Flags::default(),
            snippet: String::new(),
            has_body: false,
            size: 0,
        }
    }
    const RAW: &[u8] = b"From: a@x\r\nSubject: s\r\n\r\nhello body\r\n";

    #[test]
    fn policy_decisions() {
        let now = 1_000_000_000;
        let p = CachePolicy { keep_days: Some(30), max_mb: None };
        assert!(p.allows(now, now - 29 * 86_400, 0));
        assert!(!p.allows(now, now - 31 * 86_400, 0));
        let p = CachePolicy { keep_days: None, max_mb: Some(1) };
        assert!(p.allows(now, 0, 1024 * 1024 - 1));
        assert!(!p.allows(now, 0, 1024 * 1024));
        assert!(CachePolicy::default().allows(now, 0, u64::MAX));
        assert!(CachePolicy { keep_days: Some(0), max_mb: None }.validate().is_err());
        assert!(CachePolicy { keep_days: None, max_mb: Some(0) }.validate().is_err());
        assert!(CachePolicy { keep_days: Some(30), max_mb: Some(500) }.validate().is_ok());
    }

    #[test]
    fn undated_messages_are_allowed_and_running_counter_tracks_stores() {
        let now = 1_000_000_000;
        let p = CachePolicy { keep_days: Some(1), max_mb: None };
        assert!(p.allows(now, 0, 0) && p.allows(now, -5, 0), "no usable date: allowed");
        let s = Storage::open_in_memory().unwrap();
        acct(&s, "a@x.org");
        for u in 1..=2 {
            s.upsert_message(&msg("a@x.org", u, now_secs())).unwrap();
        }
        s.set_cache_policy("a@x.org", CachePolicy { keep_days: None, max_mb: Some(10) });
        s.set_body("a@x.org", "INBOX", 1, 7, RAW, "snip").unwrap(); // first store measures (0), then notes +len
        assert!(s.body_cache_allowed("a@x.org", now_secs())); // measures
        s.set_body("a@x.org", "INBOX", 2, 7, RAW, "snip").unwrap();
        let counted = s.body_bytes.lock().unwrap().get("a@x.org").unwrap().0;
        assert_eq!(counted, 2 * RAW.len() as u64);
        assert_eq!(counted, s.cached_body_bytes("a@x.org").unwrap());
    }

    #[test]
    fn usage_counts_per_account_with_a_temp_db() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path().join("t.db")).unwrap();
        acct(&s, "a@x.org");
        acct(&s, "b@x.org");
        for u in 1..=3 {
            s.upsert_message(&msg("a@x.org", u, now_secs())).unwrap();
        }
        s.upsert_message(&msg("b@x.org", 1, now_secs())).unwrap();
        s.set_body("a@x.org", "INBOX", 1, 7, RAW, "snip").unwrap();
        s.set_body("a@x.org", "INBOX", 2, 7, RAW, "snip").unwrap();
        let u = s.cache_usage("a@x.org").unwrap();
        assert_eq!((u.messages, u.with_body, u.body_bytes), (3, 2, 2 * RAW.len() as u64));
        assert!(u.text_bytes > 0);
        assert_eq!(s.cache_usage("b@x.org").unwrap(), CacheUsage { messages: 1, ..Default::default() });
        assert!(s.db_size_bytes().unwrap() > 0);
    }

    #[test]
    fn clear_drops_bodies_of_one_account_only_and_keeps_rows() {
        let s = Storage::open_in_memory().unwrap();
        acct(&s, "a@x.org");
        acct(&s, "b@x.org");
        s.upsert_message(&msg("a@x.org", 1, now_secs())).unwrap();
        s.upsert_message(&msg("b@x.org", 1, now_secs())).unwrap();
        s.set_body("a@x.org", "INBOX", 1, 7, RAW, "snip").unwrap();
        s.set_body("b@x.org", "INBOX", 1, 7, RAW, "snip").unwrap();
        assert_eq!(s.clear_cached_bodies("a@x.org").unwrap(), 1);
        assert!(s.get_body("a@x.org", "INBOX", 1).unwrap().is_none());
        assert!(s.get_body("b@x.org", "INBOX", 1).unwrap().is_some(), "other accounts untouched");
        let row = s.get_message("a@x.org", "INBOX", 1).unwrap().unwrap();
        assert_eq!(row.subject, "s", "the header row stays");
        assert!(!row.has_body);
        assert_eq!(s.cache_usage("a@x.org").unwrap().body_bytes, 0);
        // The cleared body is re-fetchable: set_body works again.
        s.set_body("a@x.org", "INBOX", 1, 7, RAW, "snip").unwrap();
        assert!(s.get_body("a@x.org", "INBOX", 1).unwrap().is_some());
    }

    #[test]
    fn policy_stops_caching_old_bodies_but_never_removes_an_existing_one() {
        let s = Storage::open_in_memory().unwrap();
        acct(&s, "a@x.org");
        let old = now_secs() - 100 * 86_400;
        s.upsert_message(&msg("a@x.org", 1, old)).unwrap();
        s.upsert_message(&msg("a@x.org", 2, old)).unwrap();
        s.upsert_message(&msg("a@x.org", 3, now_secs())).unwrap();
        // uid 2 was cached before the policy existed.
        s.set_body("a@x.org", "INBOX", 2, 7, RAW, "snip").unwrap();
        s.set_cache_policy("a@x.org", CachePolicy { keep_days: Some(30), max_mb: None });
        s.set_body("a@x.org", "INBOX", 1, 7, RAW, "snip").unwrap();
        s.set_body("a@x.org", "INBOX", 3, 7, RAW, "snip").unwrap();
        assert!(s.get_body("a@x.org", "INBOX", 1).unwrap().is_none(), "old message: body not cached");
        assert!(s.get_body("a@x.org", "INBOX", 3).unwrap().is_some(), "recent message cached");
        // Re-running set_body for an already cached old row (lazy backfill) must not wipe it.
        s.set_body("a@x.org", "INBOX", 2, 7, RAW, "snip").unwrap();
        assert!(s.get_body("a@x.org", "INBOX", 2).unwrap().is_some(), "policy never deletes");
        // The skipped message still gets its snippet and header-derived fields.
        assert_eq!(s.get_message("a@x.org", "INBOX", 1).unwrap().unwrap().snippet, "snip");
    }

    #[test]
    fn size_cap_stops_further_caching() {
        let s = Storage::open_in_memory().unwrap();
        acct(&s, "a@x.org");
        for u in 1..=2 {
            s.upsert_message(&msg("a@x.org", u, now_secs())).unwrap();
        }
        let big = vec![b'x'; 1024 * 1024 + 10];
        s.set_cache_policy("a@x.org", CachePolicy { keep_days: None, max_mb: Some(1) });
        s.set_body("a@x.org", "INBOX", 1, 7, &big, "snip").unwrap();
        assert!(s.get_body("a@x.org", "INBOX", 1).unwrap().is_some(), "first body fits (cap checked before storing)");
        s.set_body("a@x.org", "INBOX", 2, 7, RAW, "snip").unwrap();
        assert!(s.get_body("a@x.org", "INBOX", 2).unwrap().is_none(), "cap reached: not cached");
        assert_eq!(s.cache_policy("a@x.org").max_mb, Some(1));
        s.set_cache_policy("a@x.org", CachePolicy::default());
        s.set_body("a@x.org", "INBOX", 2, 7, RAW, "snip").unwrap();
        assert!(s.get_body("a@x.org", "INBOX", 2).unwrap().is_some());
    }

    #[test]
    fn lookups_by_message_id_and_dates() {
        let s = Storage::open_in_memory().unwrap();
        acct(&s, "a@x.org");
        s.upsert_message(&msg("a@x.org", 5, 100)).unwrap();
        assert_eq!(s.uid_by_message_id("a@x.org", "INBOX", "m5@x").unwrap(), Some(5));
        assert_eq!(s.uid_by_message_id("a@x.org", "INBOX", "nope").unwrap(), None);
        assert_eq!(s.uids_with_dates("a@x.org", "INBOX").unwrap(), vec![(5, 100)]);
    }
}
