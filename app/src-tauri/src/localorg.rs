//! Local organisation (docs/ESSENTIALS_SPEC.md "Conversations and senders"): conversation threads, muted
//! threads, pinned messages, VIP senders and blocked senders.
//!
//! Everything here is local to this Mac: SQLite tables in `workspace.db`, no network, no server-side flags.
//! Threads are derived on demand from the message cache (`mailcore::threading`); a mute is a set of
//! Message-IDs of the thread, so later replies (whose `References` name any of them) are muted too.
//! Blocked mail is MOVED to the Junk folder through the set-safe `mailcore::batch` path, only for INBOX mail
//! that arrives while the app is open, and is never deleted.

use crate::badge::{Recent, INBOX};
use crate::plat_prefs::PlatformPrefs;
use crate::state::{err, AppState};
use crate::wire::WireSummary;
use mailcore::threading::{addresses_of, thread_keys, ThreadRow};
use mailcore::{imap::find_role, BatchItem, FlagChange, FolderRole, Storage};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use tauri::{AppHandle, Emitter, Manager, State};

type CmdResult<T> = Result<T, String>;

/// Newest cached messages of an account the threader looks at.
const THREAD_ROWS: u32 = 30_000;
const MAX_THREAD_MEMBERS: usize = 200;
const MAX_MUTED_IDS: i64 = 50_000;
const MAX_PINNED: i64 = 2_000;
const MAX_VIP: i64 = 2_000;
const MAX_BLOCKED: i64 = 5_000;
/// Most unread INBOX rows inspected when subtracting muted mail from the unread badge.
const MUTED_SCAN: u32 = 5_000;

// ---------------------------------------------------------------------------------------------- store

/// Cheap to clone (shared connection). Opened on `workspace.db` next to drafts, outbox, signatures.
#[derive(Clone)]
pub struct LocalOrg {
    conn: Arc<Mutex<Connection>>,
}

/// Lower-cased Message-ID without angle brackets.
pub fn norm_id(s: &str) -> String {
    s.trim().trim_matches(['<', '>']).trim().to_ascii_lowercase()
}

/// A single plain address (`a@b.c`, lower case), or None. Accepts `Name <a@b.c>` and takes the first address.
pub fn clean_address(s: &str) -> Option<String> {
    let a = addresses_of(s).into_iter().next().or_else(|| {
        let t = s.trim().to_ascii_lowercase();
        t.contains('@').then_some(t)
    })?;
    let (local, domain) = a.split_once('@')?;
    let bad = |c: char| c.is_whitespace() || c.is_control() || "<>,;\"()[]\\".contains(c);
    (a.len() <= 254 && !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && !domain.contains('@') && !a.chars().any(bad)).then_some(a)
}

/// Does a message belong to a muted thread: its own id, its parent or any referenced id is in the set.
pub fn message_muted(muted: &HashSet<String>, message_id: &str, in_reply_to: &str, references: &str) -> bool {
    if muted.is_empty() {
        return false;
    }
    let own = norm_id(message_id);
    (!own.is_empty() && muted.contains(&own))
        || in_reply_to.split_whitespace().chain(references.split_whitespace()).any(|r| muted.contains(&norm_id(r)))
}

impl LocalOrg {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        Self::from_conn(Connection::open(path).map_err(|e| e.to_string())?)
    }
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        Self::from_conn(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }
    fn from_conn(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS lo_muted(account TEXT NOT NULL, message_id TEXT NOT NULL, PRIMARY KEY(account, message_id)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS lo_pinned(account TEXT NOT NULL, message_id TEXT NOT NULL, pinned_at INTEGER NOT NULL, PRIMARY KEY(account, message_id)) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS lo_vip(address TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT '', added_at INTEGER NOT NULL) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS lo_blocked(address TEXT PRIMARY KEY, added_at INTEGER NOT NULL) WITHOUT ROWID;",
        )
        .map_err(|e| e.to_string())?;
        Ok(LocalOrg { conn: Arc::new(Mutex::new(conn)) })
    }
    fn lock(&self) -> CmdResult<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| "Local organisation storage is unavailable.".to_string())
    }
    fn count(c: &Connection, table: &str) -> CmdResult<i64> {
        c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).map_err(err)
    }

    // ---- muted threads ----
    pub fn set_muted(&self, account: &str, ids: &[String], on: bool) -> CmdResult<()> {
        let mut c = self.lock()?;
        if on && Self::count(&c, "lo_muted")? + ids.len() as i64 > MAX_MUTED_IDS {
            return Err("Too many muted conversations. Unmute some first.".into());
        }
        let tx = c.transaction().map_err(err)?;
        for id in ids {
            if on {
                tx.execute("INSERT OR IGNORE INTO lo_muted(account, message_id) VALUES (?1, ?2)", params![account, id]).map_err(err)?;
            } else {
                tx.execute("DELETE FROM lo_muted WHERE account=?1 AND message_id=?2", params![account, id]).map_err(err)?;
            }
        }
        tx.commit().map_err(err)
    }
    pub fn muted_set(&self, account: &str) -> CmdResult<HashSet<String>> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT message_id FROM lo_muted WHERE account=?1").map_err(err)?;
        let rows = st.query_map([account], |r| r.get::<_, String>(0)).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    // ---- pinned messages ----
    pub fn set_pinned(&self, account: &str, ids: &[String], on: bool, now: i64) -> CmdResult<()> {
        let mut c = self.lock()?;
        if on && Self::count(&c, "lo_pinned")? + ids.len() as i64 > MAX_PINNED {
            return Err("Too many pinned messages. Unpin some first.".into());
        }
        let tx = c.transaction().map_err(err)?;
        for id in ids {
            if on {
                tx.execute("INSERT OR IGNORE INTO lo_pinned(account, message_id, pinned_at) VALUES (?1, ?2, ?3)", params![account, id, now]).map_err(err)?;
            } else {
                tx.execute("DELETE FROM lo_pinned WHERE account=?1 AND message_id=?2", params![account, id]).map_err(err)?;
            }
        }
        tx.commit().map_err(err)
    }
    pub fn pinned_set(&self, account: &str) -> CmdResult<HashSet<String>> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT message_id FROM lo_pinned WHERE account=?1").map_err(err)?;
        let rows = st.query_map([account], |r| r.get::<_, String>(0)).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    // ---- VIP + blocked senders (global, by address) ----
    pub fn set_vip(&self, address: &str, name: &str, on: bool, now: i64) -> CmdResult<()> {
        let c = self.lock()?;
        if on {
            if Self::count(&c, "lo_vip")? >= MAX_VIP {
                return Err("The VIP list is full.".into());
            }
            let name: String = name.chars().filter(|ch| !ch.is_control()).take(120).collect();
            c.execute("INSERT INTO lo_vip(address, name, added_at) VALUES (?1, ?2, ?3) ON CONFLICT(address) DO UPDATE SET name=excluded.name", params![address, name, now]).map_err(err)?;
        } else {
            c.execute("DELETE FROM lo_vip WHERE address=?1", [address]).map_err(err)?;
        }
        Ok(())
    }
    pub fn vip_list(&self) -> CmdResult<Vec<(String, String)>> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT address, name FROM lo_vip ORDER BY address").map_err(err)?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }
    pub fn set_blocked(&self, address: &str, on: bool, now: i64) -> CmdResult<()> {
        let c = self.lock()?;
        if on {
            if Self::count(&c, "lo_blocked")? >= MAX_BLOCKED {
                return Err("The blocked senders list is full.".into());
            }
            c.execute("INSERT OR IGNORE INTO lo_blocked(address, added_at) VALUES (?1, ?2)", params![address, now]).map_err(err)?;
        } else {
            c.execute("DELETE FROM lo_blocked WHERE address=?1", [address]).map_err(err)?;
        }
        Ok(())
    }
    pub fn blocked_list(&self) -> CmdResult<Vec<(String, i64)>> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT address, added_at FROM lo_blocked ORDER BY address").map_err(err)?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    /// Unread INBOX messages of `account` that belong to a muted thread (subtracted from the unread badge).
    pub fn muted_unseen(&self, storage: &Storage, account: &str) -> u32 {
        let Ok(muted) = self.muted_set(account) else { return 0 };
        if muted.is_empty() {
            return 0;
        }
        let rows = storage.unseen_link_rows(account, INBOX, MUTED_SCAN).unwrap_or_default();
        rows.iter().filter(|(_, mid, irt, refs)| message_muted(&muted, mid, irt, refs)).count() as u32
    }
}

// ---------------------------------------------------------------------------------------------- threads

/// `(uid, key)` of the messages of `folder` that share a conversation with at least one other message of
/// the same folder (single messages need no key).
pub fn folder_thread_entries(rows: &[ThreadRow], keys: &[String], folder: &str) -> Vec<(u32, String)> {
    let mut count: HashMap<&str, u32> = HashMap::new();
    for (r, k) in rows.iter().zip(keys) {
        if r.folder == folder {
            *count.entry(k.as_str()).or_default() += 1;
        }
    }
    rows.iter().zip(keys).filter(|(r, k)| r.folder == folder && count.get(k.as_str()).copied().unwrap_or(0) > 1).map(|(r, k)| (r.uid, k.clone())).collect()
}

/// Rows of the conversation `(folder, uid)` belongs to, oldest first (all folders the threader covers).
pub fn members_of<'a>(rows: &'a [ThreadRow], keys: &[String], folder: &str, uid: u32) -> Vec<&'a ThreadRow> {
    let Some(key) = rows.iter().zip(keys).find(|(r, _)| r.folder == folder && r.uid == uid).map(|(_, k)| k) else { return Vec::new() };
    let mut v: Vec<&ThreadRow> = rows.iter().zip(keys).filter(|(_, k)| *k == key).map(|(r, _)| r).collect();
    v.sort_by_key(|r| (r.item.date, r.uid));
    v.truncate(MAX_THREAD_MEMBERS);
    v
}

/// Ids stored when a thread is muted: every member's Message-ID plus the conversation's root id.
pub fn mute_ids(members: &[&ThreadRow], key: Option<&str>) -> Vec<String> {
    let mut ids: Vec<String> = members.iter().map(|r| norm_id(&r.item.message_id)).filter(|s| !s.is_empty()).collect();
    if let Some(k) = key.filter(|k| !k.is_empty() && !k.starts_with('~')) {
        ids.push(norm_id(k));
    }
    ids.sort();
    ids.dedup();
    ids
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireThreadEntry {
    pub uid: u32,
    pub key: String,
}

async fn load_threads(state: &AppState, account: &str) -> CmdResult<(Vec<ThreadRow>, Vec<String>)> {
    let (storage, acc) = (state.storage.clone(), account.to_string());
    tauri::async_runtime::spawn_blocking(move || {
        let rows = storage.thread_rows(&acc, THREAD_ROWS).map_err(err)?;
        let items: Vec<_> = rows.iter().map(|r| r.item.clone()).collect();
        let keys = thread_keys(&items, &acc);
        Ok::<_, String>((rows, keys))
    })
    .await
    .map_err(err)?
}

/// Thread keys for the messages of the open folder (only messages that share a conversation appear).
#[tauri::command]
pub async fn thread_map(state: State<'_, AppState>, account: String, folder: String) -> CmdResult<Vec<WireThreadEntry>> {
    let (rows, keys) = load_threads(&state, &account).await?;
    Ok(folder_thread_entries(&rows, &keys, &folder).into_iter().map(|(uid, key)| WireThreadEntry { uid, key }).collect())
}

/// The conversation of one message, oldest first, across the account's folders (Trash, Junk, Drafts excluded).
#[tauri::command]
pub async fn thread_members(state: State<'_, AppState>, account: String, folder: String, uid: u32) -> CmdResult<Vec<WireSummary>> {
    let (rows, keys) = load_threads(&state, &account).await?;
    let out = members_of(&rows, &keys, &folder, uid)
        .iter()
        .filter_map(|r| state.storage.get_message(&account, &r.folder, r.uid).ok().flatten())
        .map(|m| WireSummary::from(&m))
        .collect();
    Ok(out)
}

/// Mute or unmute the conversation of one message. Local only. Returns how many Message-IDs were touched.
#[tauri::command]
pub async fn mute_thread(state: State<'_, AppState>, lo: State<'_, LocalOrg>, account: String, folder: String, uid: u32, muted: bool) -> CmdResult<u32> {
    let (rows, keys) = load_threads(&state, &account).await?;
    let members = members_of(&rows, &keys, &folder, uid);
    let key = rows.iter().zip(&keys).find(|(r, _)| r.folder == folder && r.uid == uid).map(|(_, k)| k.as_str());
    let mut ids = mute_ids(&members, key);
    if ids.is_empty() {
        // The open message may live in a folder the threader skips (Trash/Junk): fall back to its own id.
        let m = state.storage.get_message(&account, &folder, uid).map_err(err)?.ok_or("That message is no longer in the cache.")?;
        ids = m.message_id.iter().map(|s| norm_id(s)).filter(|s| !s.is_empty()).collect();
    }
    if ids.is_empty() {
        return Err("This message has no Message-ID, so its conversation can't be tracked.".into());
    }
    lo.set_muted(&account, &ids, muted)?;
    Ok(ids.len() as u32)
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireMarks {
    pub muted: Vec<u32>,
    pub pinned: Vec<u32>,
}

/// UIDs of the open folder that are in a muted thread or pinned.
#[tauri::command]
pub async fn local_marks(state: State<'_, AppState>, lo: State<'_, LocalOrg>, account: String, folder: String) -> CmdResult<WireMarks> {
    let (muted, pins) = (lo.muted_set(&account)?, lo.pinned_set(&account)?);
    if muted.is_empty() && pins.is_empty() {
        return Ok(WireMarks::default());
    }
    let storage = state.storage.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let rows = storage.list_with_headers(&account, &folder, 0, 30_000, false).map_err(err)?;
        let mut out = WireMarks::default();
        for (m, h) in rows {
            let mid = m.message_id.as_deref().unwrap_or("");
            if !pins.is_empty() && pins.contains(&norm_id(mid)) {
                out.pinned.push(m.uid);
            }
            if message_muted(&muted, mid, &h.in_reply_to, &h.references) {
                out.muted.push(m.uid);
            }
        }
        Ok::<_, String>(out)
    })
    .await
    .map_err(err)?
}

/// Pin or unpin messages (by Message-ID, so a pin follows the message). Returns how many were changed.
#[tauri::command]
pub fn pin_messages(state: State<'_, AppState>, lo: State<'_, LocalOrg>, account: String, folder: String, uids: Vec<u32>, pinned: bool) -> CmdResult<u32> {
    if uids.is_empty() || uids.len() > 1000 {
        return Err("Pick between 1 and 1000 messages.".into());
    }
    let ids: Vec<String> = uids
        .iter()
        .filter_map(|u| state.storage.get_message(&account, &folder, *u).ok().flatten())
        .filter_map(|m| m.message_id)
        .map(|s| norm_id(&s))
        .filter(|s| !s.is_empty())
        .collect();
    if ids.is_empty() {
        return Err("These messages have no Message-ID, so they can't be pinned.".into());
    }
    lo.set_pinned(&account, &ids, pinned, chrono::Utc::now().timestamp())?;
    Ok(ids.len() as u32)
}

// ---------------------------------------------------------------------------------------------- senders

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireVip {
    pub address: String,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireBlocked {
    pub address: String,
    pub added_at: i64,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WireSenders {
    pub vip: Vec<WireVip>,
    pub blocked: Vec<WireBlocked>,
}

#[tauri::command]
pub fn sender_lists(lo: State<'_, LocalOrg>) -> CmdResult<WireSenders> {
    Ok(WireSenders {
        vip: lo.vip_list()?.into_iter().map(|(address, name)| WireVip { address, name }).collect(),
        blocked: lo.blocked_list()?.into_iter().map(|(address, added_at)| WireBlocked { address, added_at }).collect(),
    })
}

#[tauri::command]
pub fn set_vip(lo: State<'_, LocalOrg>, address: String, name: Option<String>, on: bool) -> CmdResult<()> {
    let a = clean_address(&address).ok_or("That doesn't look like an email address.")?;
    lo.set_vip(&a, name.as_deref().unwrap_or(""), on, chrono::Utc::now().timestamp())
}

#[tauri::command]
pub fn set_blocked(state: State<'_, AppState>, lo: State<'_, LocalOrg>, address: String, on: bool) -> CmdResult<()> {
    let a = clean_address(&address).ok_or("That doesn't look like an email address.")?;
    if on && state.storage.list_accounts().map_err(err)?.iter().any(|acc| acc.email.eq_ignore_ascii_case(&a)) {
        return Err("That is one of your own accounts; it can't be blocked.".into());
    }
    lo.set_blocked(&a, on, chrono::Utc::now().timestamp())
}

// ---------------------------------------------------------------------------------------------- arrivals

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArrivalFacts {
    pub blocked: bool,
    pub muted: bool,
    pub vip: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Goes on to the normal notification logic.
    Notify,
    /// No notification (muted, or a non-VIP on a "VIP only" account).
    Silent,
    /// Muted thread and the user enabled auto-read: no notification, marked as read.
    SilentRead,
    /// Blocked sender: moved to Junk, no notification.
    MoveToJunk,
}

/// Block beats everything; mute beats VIP; "VIP only" accounts drop everyone else.
pub fn decide(f: ArrivalFacts, account_vip_only: bool, mute_auto_read: bool) -> Verdict {
    if f.blocked {
        Verdict::MoveToJunk
    } else if f.muted {
        if mute_auto_read { Verdict::SilentRead } else { Verdict::Silent }
    } else if account_vip_only && !f.vip {
        Verdict::Silent
    } else {
        Verdict::Notify
    }
}

/// Move-to-Junk and auto-read act on INBOX uids only: for an arrival in any other folder they are dropped (the mail
/// stays untouched, just no notification) rather than applied to whatever shares that uid in the Inbox.
pub fn route(v: Verdict, folder: &str) -> Verdict {
    match v {
        Verdict::MoveToJunk | Verdict::SilentRead if !folder.eq_ignore_ascii_case(INBOX) => Verdict::Silent,
        v => v,
    }
}

fn facts_for(storage: &Storage, muted: &HashMap<String, HashSet<String>>, vip: &HashSet<String>, blocked: &HashSet<String>, r: &Recent) -> Option<ArrivalFacts> {
    let m = storage.get_message(&r.account, &r.folder, r.uid).ok().flatten()?;
    let h = storage.get_extra_headers(&r.account, &r.folder, r.uid).ok().flatten().unwrap_or_default();
    // The real address of the sender (a display name can never stand in for it, see `addresses_of`).
    let addr = addresses_of(&m.from).into_iter().last().unwrap_or_default();
    let empty = HashSet::new();
    let set = muted.get(&r.account).unwrap_or(&empty);
    Some(ArrivalFacts {
        blocked: !addr.is_empty() && blocked.contains(&addr),
        muted: message_muted(set, m.message_id.as_deref().unwrap_or(""), &h.in_reply_to, &h.references),
        vip: !addr.is_empty() && vip.contains(&addr),
    })
}

/// New unread INBOX mail was detected (badge poller). Applies block / mute / VIP: blocked mail is moved to Junk
/// and muted mail optionally marked read (both in the background, set-safe, never deleting). Returns the arrivals
/// that may still produce a notification.
pub fn on_arrivals(app: &AppHandle, prefs: &PlatformPrefs, arrivals: Vec<Recent>) -> Vec<Recent> {
    let Some(lo) = app.try_state::<LocalOrg>() else { return arrivals };
    let st = app.state::<AppState>();
    let (vip, blocked) = match (lo.vip_list(), lo.blocked_list()) {
        (Ok(v), Ok(b)) => (v.into_iter().map(|x| x.0).collect::<HashSet<_>>(), b.into_iter().map(|x| x.0).collect::<HashSet<_>>()),
        _ => return arrivals,
    };
    let mut muted: HashMap<String, HashSet<String>> = HashMap::new();
    for r in &arrivals {
        if !muted.contains_key(&r.account) {
            muted.insert(r.account.clone(), lo.muted_set(&r.account).unwrap_or_default());
        }
    }
    let (mut keep, mut moves, mut reads) = (Vec::new(), Vec::new(), Vec::new());
    for r in arrivals {
        let Some(f) = facts_for(&st.storage, &muted, &vip, &blocked, &r) else {
            keep.push(r);
            continue;
        };
        match route(decide(f, prefs.notify_vip_only.contains(&r.account), prefs.mute_auto_read), &r.folder) {
            Verdict::Notify => keep.push(r),
            Verdict::Silent => {}
            Verdict::SilentRead => reads.push((r.account.clone(), r.uid)),
            Verdict::MoveToJunk => moves.push((r.account.clone(), r.uid)),
        }
    }
    if !moves.is_empty() || !reads.is_empty() {
        let app = app.clone();
        tauri::async_runtime::spawn(async move { run_actions(&app, moves, reads).await });
    }
    keep
}

fn group(v: Vec<(String, u32)>) -> BTreeMap<String, Vec<u32>> {
    let mut m: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (a, u) in v {
        m.entry(a).or_default().push(u);
    }
    m
}

async fn run_actions(app: &AppHandle, moves: Vec<(String, u32)>, reads: Vec<(String, u32)>) {
    let state = app.state::<AppState>();
    for (account, uids) in group(moves) {
        let known = state.storage.list_folders(&account).unwrap_or_default();
        let Some(junk) = find_role(&known, FolderRole::Junk).map(|f| f.name.clone()) else { continue }; // no Junk folder: leave it alone
        let items: Vec<BatchItem> = uids
            .iter()
            .map(|u| BatchItem { uid: Some(*u), message_id: state.storage.get_message(&account, INBOX, *u).ok().flatten().and_then(|m| m.message_id) })
            .collect();
        let validity = uids.first().and_then(|u| state.storage.get_message(&account, INBOX, *u).ok().flatten()).map(|m| m.uidvalidity);
        let Ok(mut conn) = state.connect(&account).await else { continue };
        let r = conn.client.batch_move(INBOX, validity, &junk, &items).await;
        conn.close().await;
        if let Ok(report) = r {
            crate::organise::apply_move_outcomes(&state.storage, &account, INBOX, &report.outcomes);
            let _ = app.emit("mail://synced", crate::wire::SyncedEvent { account: account.clone(), folders: vec![INBOX.to_string(), junk.clone()] });
        }
    }
    for (account, uids) in group(reads) {
        let validity = uids.first().and_then(|u| state.storage.get_message(&account, INBOX, *u).ok().flatten()).map(|m| m.uidvalidity);
        let Ok(mut conn) = state.connect(&account).await else { continue };
        let r = conn.client.batch_flag(INBOX, validity, &uids, FlagChange::Seen(true)).await;
        conn.close().await;
        if let Ok(report) = r {
            crate::organise::apply_flag_outcomes(&state.storage, &account, INBOX, &report.outcomes, FlagChange::Seen(true));
            let _ = app.emit("mail://synced", crate::wire::SyncedEvent { account: account.clone(), folders: vec![INBOX.to_string()] });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::threading::Item;

    fn row(folder: &str, uid: u32, id: &str, refs: &str, subject: &str, date: i64) -> ThreadRow {
        ThreadRow {
            folder: folder.into(),
            uid,
            uidvalidity: 1,
            seen: false,
            item: Item { message_id: id.into(), in_reply_to: refs.split(' ').next_back().unwrap_or("").into(), references: refs.into(), subject: subject.into(), from: "ann@x.org".into(), to: "me@x.org".into(), cc: String::new(), date },
        }
    }
    fn keys(rows: &[ThreadRow]) -> Vec<String> {
        thread_keys(&rows.iter().map(|r| r.item.clone()).collect::<Vec<_>>(), "me@x.org")
    }

    #[test]
    fn mute_survives_new_replies_and_unmute_clears_the_thread() {
        let lo = LocalOrg::open_in_memory().unwrap();
        let rows = vec![row("INBOX", 1, "a@x", "", "Plan", 1), row("Sent", 1, "b@x", "a@x", "Re: Plan", 2), row("INBOX", 2, "u@x", "", "Other", 3)];
        let k = keys(&rows);
        let members = members_of(&rows, &k, "INBOX", 1);
        assert_eq!(members.len(), 2, "the Sent reply is part of the conversation");
        let ids = mute_ids(&members, Some(&k[0]));
        assert_eq!(ids, vec!["a@x".to_string(), "b@x".to_string()]);
        lo.set_muted("me@x.org", &ids, true).unwrap();
        let set = lo.muted_set("me@x.org").unwrap();
        assert!(message_muted(&set, "a@x", "", ""));
        assert!(message_muted(&set, "<new@x>", "b@x", "a@x b@x"), "a later reply names a muted id");
        assert!(message_muted(&set, "new2@x", "", "a@x"));
        assert!(!message_muted(&set, "u@x", "", ""), "other conversations are untouched");
        assert!(!message_muted(&set, "", "", ""));
        assert!(lo.muted_set("other@x.org").unwrap().is_empty(), "per account");
        lo.set_muted("me@x.org", &ids, false).unwrap();
        assert!(lo.muted_set("me@x.org").unwrap().is_empty());
    }

    #[test]
    fn folder_entries_only_list_multi_message_threads_of_that_folder() {
        let rows = vec![
            row("INBOX", 1, "a@x", "", "Plan", 1),
            row("INBOX", 2, "b@x", "a@x", "Re: Plan", 2),
            row("INBOX", 3, "c@x", "", "Solo", 3),
            row("Sent", 1, "d@x", "c@x", "Re: Solo", 4),
        ];
        let k = keys(&rows);
        let e = folder_thread_entries(&rows, &k, "INBOX");
        assert_eq!(e.iter().map(|x| x.0).collect::<Vec<_>>(), vec![1, 2], "Solo has only one message in INBOX");
        assert_eq!(e[0].1, e[1].1);
        assert_eq!(members_of(&rows, &k, "INBOX", 3).len(), 2, "but the reader still sees its Sent reply");
        assert!(members_of(&rows, &k, "INBOX", 99).is_empty());
    }

    #[test]
    fn pins_vip_and_blocked_roundtrip_in_a_temp_db_and_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workspace.db");
        let lo = LocalOrg::open(&p).unwrap();
        lo.set_pinned("a@x", &["m1@x".into(), "m2@x".into()], true, 5).unwrap();
        lo.set_vip("boss@x.org", "The Boss", true, 5).unwrap();
        lo.set_vip("boss@x.org", "Boss", true, 6).unwrap();
        lo.set_blocked("spam@x.org", true, 7).unwrap();
        lo.set_blocked("spam@x.org", true, 8).unwrap();
        let again = LocalOrg::open(&p).unwrap();
        assert_eq!(again.pinned_set("a@x").unwrap().len(), 2);
        assert!(again.pinned_set("b@x").unwrap().is_empty());
        assert_eq!(again.vip_list().unwrap(), vec![("boss@x.org".to_string(), "Boss".to_string())]);
        assert_eq!(again.blocked_list().unwrap(), vec![("spam@x.org".to_string(), 7)]);
        again.set_pinned("a@x", &["m1@x".into()], false, 9).unwrap();
        again.set_vip("boss@x.org", "", false, 9).unwrap();
        again.set_blocked("spam@x.org", false, 9).unwrap();
        assert_eq!(again.pinned_set("a@x").unwrap(), HashSet::from(["m2@x".to_string()]));
        assert!(again.vip_list().unwrap().is_empty() && again.blocked_list().unwrap().is_empty());
    }

    #[test]
    fn list_caps_hold() {
        let lo = LocalOrg::open_in_memory().unwrap();
        for i in 0..MAX_BLOCKED {
            lo.set_blocked(&format!("u{i}@x.org"), true, 1).unwrap();
        }
        assert!(lo.set_blocked("one-more@x.org", true, 1).is_err());
        lo.set_blocked("u0@x.org", false, 1).unwrap();
        lo.set_blocked("one-more@x.org", true, 1).unwrap();
    }

    #[test]
    fn addresses_are_cleaned_and_junk_is_refused() {
        assert_eq!(clean_address("  Ann Lee <Ann@Example.ORG> ").as_deref(), Some("ann@example.org"));
        assert_eq!(clean_address("bob@x.org").as_deref(), Some("bob@x.org"));
        for bad in ["", "nobody", "a@b", "@x.org", "a@@x.org", "<>"] {
            assert!(clean_address(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_display_name_cannot_pose_as_a_vip_or_blocked_address() {
        // The sender wrote the boss's address into the display name; the real address is the last one.
        assert_eq!(addresses_of("<boss@corp.com> <evil@x.org>").into_iter().last().as_deref(), Some("evil@x.org"));
        assert_eq!(clean_address("\"<boss@corp.com>\" <evil@x.org>").as_deref(), Some("evil@x.org"));
        assert_eq!(clean_address("<boss@corp.com> <evil@x.org>").as_deref(), Some("evil@x.org"));
    }

    #[test]
    fn junk_and_auto_read_only_act_on_inbox_arrivals() {
        assert_eq!(route(Verdict::MoveToJunk, "INBOX"), Verdict::MoveToJunk);
        assert_eq!(route(Verdict::MoveToJunk, "Archive"), Verdict::Silent);
        assert_eq!(route(Verdict::SilentRead, "Archive"), Verdict::Silent);
        assert_eq!(route(Verdict::Notify, "Archive"), Verdict::Notify);
    }

    #[test]
    fn verdicts_follow_block_then_mute_then_vip() {
        let f = |blocked, muted, vip| ArrivalFacts { blocked, muted, vip };
        assert_eq!(decide(f(false, false, false), false, false), Verdict::Notify);
        assert_eq!(decide(f(true, true, true), false, true), Verdict::MoveToJunk, "block beats everything");
        assert_eq!(decide(f(false, true, true), false, false), Verdict::Silent, "mute beats VIP");
        assert_eq!(decide(f(false, true, false), false, true), Verdict::SilentRead, "auto-read only when enabled");
        assert_eq!(decide(f(false, false, false), true, false), Verdict::Silent, "VIP-only account drops others");
        assert_eq!(decide(f(false, false, true), true, false), Verdict::Notify, "VIP gets through");
    }

    #[test]
    fn the_unread_badge_skips_muted_threads() {
        use mailcore::mime::ExtraHeaders;
        use mailcore::*;
        let storage = Storage::open_in_memory().unwrap();
        storage
            .upsert_account(&AccountConfig {
                email: "me@x.org".into(),
                username: "me".into(),
                imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
                smtp: ServerConfig { host: "s".into(), port: 587, tls: Tls::StartTls },
            })
            .unwrap();
        storage.replace_folders("me@x.org", &[Folder { name: "INBOX".into(), delimiter: None, role: FolderRole::Inbox, selectable: true }]).unwrap();
        for (uid, id, refs) in [(1u32, "a@x", ""), (2, "b@x", "a@x"), (3, "c@x", "")] {
            storage
                .upsert_message(&MessageSummary {
                    account: "me@x.org".into(),
                    folder: "INBOX".into(),
                    uid,
                    uidvalidity: 1,
                    message_id: Some(id.into()),
                    subject: "s".into(),
                    from: "ann@x.org".into(),
                    to: "me@x.org".into(),
                    date: uid as i64,
                    flags: Flags::default(),
                    snippet: String::new(),
                    has_body: false,
                    size: 1,
                })
                .unwrap();
            storage.set_extra_headers("me@x.org", "INBOX", uid, &ExtraHeaders { references: refs.into(), in_reply_to: refs.into(), ..Default::default() }).unwrap();
        }
        let lo = LocalOrg::open_in_memory().unwrap();
        assert_eq!(lo.muted_unseen(&storage, "me@x.org"), 0);
        lo.set_muted("me@x.org", &["a@x".into()], true).unwrap();
        assert_eq!(lo.muted_unseen(&storage, "me@x.org"), 2, "a@x and its reply b@x");
    }
}
