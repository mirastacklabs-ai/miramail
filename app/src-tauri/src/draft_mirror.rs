//! IMAP Drafts mirror for compose drafts (docs/ESSENTIALS_SPEC.md). On by default; can be turned off in Preferences > Composing.
//!
//! The frontend calls `draft_mirror_save` at most every 20 s of idle time for a pane that has content, and
//! `draft_mirror_remove` when the pane is sent or discarded. The server copy of each draft is tracked by the exact
//! UID we appended (`mailcore::draftsync`): replacing and removing delete only that UID, verified by Message-ID and
//! UIDVALIDITY, and a server without UIDPLUS is skipped silently. Nothing here ever blocks composing; every
//! failure is reported as a quiet status string. Draft content is user content: never logged.

use crate::composer::{resolve_from, ComposeStore};
use crate::state::{err, AppState};
use crate::wire::Outgoing;
use mailcore::draftsync::{mirror_draft, remove_draft, DraftRef, UNLOCATED};
use mailcore::{imap::find_role, FolderRole, OutgoingMessage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use tauri::State;

const MAX_ROWS: i64 = 200;
const MAX_TOMBSTONES: usize = 1000;

pub struct DraftMirror {
    conn: Mutex<Connection>,
    /// Draft ids that were removed: a save that was already scheduled must not bring them back.
    removed: Mutex<HashSet<String>>,
    /// One IMAP mirror operation at a time.
    gate: tokio::sync::Mutex<()>,
    /// Set for the rest of the session once an appended copy could not be located again (an untracked duplicate
    /// exists on the server): mirroring stops rather than adding one more duplicate per save.
    stopped: AtomicBool,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl DraftMirror {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        Self::from_conn(Connection::open(path).map_err(|e| e.to_string())?)
    }
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        Self::from_conn(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }
    fn from_conn(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS draft_mirror(
                draft_id TEXT PRIMARY KEY, account TEXT NOT NULL, folder TEXT NOT NULL,
                validity INTEGER NOT NULL, uid INTEGER NOT NULL, message_id TEXT NOT NULL,
                remove_pending INTEGER NOT NULL DEFAULT 0);",
        )
        .map_err(|e| e.to_string())?;
        // Databases created before `remove_pending` existed (the duplicate-column error is the "already there" case).
        let _ = conn.execute("ALTER TABLE draft_mirror ADD COLUMN remove_pending INTEGER NOT NULL DEFAULT 0", []);
        Ok(DraftMirror { conn: Mutex::new(conn), removed: Mutex::new(HashSet::new()), gate: tokio::sync::Mutex::new(()), stopped: AtomicBool::new(false) })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "Draft storage is unavailable.".to_string())
    }
    fn tombstones(&self) -> MutexGuard<'_, HashSet<String>> {
        self.removed.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self, draft_id: &str) -> Result<Option<(String, DraftRef)>, String> {
        self.lock()?
            .query_row("SELECT account,folder,validity,uid,message_id FROM draft_mirror WHERE draft_id=?1", [draft_id], |r| {
                Ok((r.get::<_, String>(0)?, DraftRef { folder: r.get(1)?, validity: r.get::<_, i64>(2)? as u32, uid: r.get::<_, i64>(3)? as u32, message_id: r.get(4)? }))
            })
            .optional()
            .map_err(|e| e.to_string())
    }
    pub fn put(&self, draft_id: &str, account: &str, r: &DraftRef) -> Result<(), String> {
        let c = self.lock()?;
        let n: i64 = c.query_row("SELECT COUNT(*) FROM draft_mirror", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        let exists = self.get_exists(&c, draft_id)?;
        if n >= MAX_ROWS && !exists {
            return Err("Too many mirrored drafts.".into());
        }
        c.execute(
            "INSERT INTO draft_mirror(draft_id,account,folder,validity,uid,message_id) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(draft_id) DO UPDATE SET account=excluded.account, folder=excluded.folder, validity=excluded.validity, uid=excluded.uid, message_id=excluded.message_id, remove_pending=0",
            params![draft_id, account, r.folder, r.validity as i64, r.uid as i64, r.message_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn get_exists(&self, c: &Connection, id: &str) -> Result<bool, String> {
        Ok(c.query_row("SELECT 1 FROM draft_mirror WHERE draft_id=?1", [id], |_| Ok(true)).optional().map_err(|e| e.to_string())?.unwrap_or(false))
    }
    /// Account removal: nothing of that account may stay tracked.
    pub fn delete_for_account(&self, account: &str) -> Result<(), String> {
        self.lock()?.execute("DELETE FROM draft_mirror WHERE account=?1", [account]).map_err(|e| e.to_string())?;
        Ok(())
    }
    fn mark_remove_pending(&self, draft_id: &str) -> Result<(), String> {
        self.lock()?.execute("UPDATE draft_mirror SET remove_pending=1 WHERE draft_id=?1", [draft_id]).map_err(|e| e.to_string())?;
        Ok(())
    }
    /// Rows whose server copy still has to be removed, for one account.
    fn pending(&self, account: &str) -> Result<Vec<(String, DraftRef)>, String> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT draft_id,folder,validity,uid,message_id FROM draft_mirror WHERE account=?1 AND remove_pending=1").map_err(|e| e.to_string())?;
        let rows = st
            .query_map([account], |r| Ok((r.get::<_, String>(0)?, DraftRef { folder: r.get(1)?, validity: r.get::<_, i64>(2)? as u32, uid: r.get::<_, i64>(3)? as u32, message_id: r.get(4)? })))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }
    fn pending_accounts(&self) -> Result<Vec<String>, String> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT DISTINCT account FROM draft_mirror WHERE remove_pending=1").map_err(|e| e.to_string())?;
        let rows = st.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
    }
    pub fn forget_row(&self, draft_id: &str) -> Result<(), String> {
        self.lock()?.execute("DELETE FROM draft_mirror WHERE draft_id=?1", [draft_id]).map_err(|e| e.to_string())?;
        Ok(())
    }
    /// Mark removed; a later save for this id is refused.
    pub fn tombstone(&self, draft_id: &str) {
        let mut t = self.tombstones();
        if t.len() >= MAX_TOMBSTONES {
            t.clear();
        }
        t.insert(draft_id.to_string());
    }
    pub fn is_tombstoned(&self, draft_id: &str) -> bool {
        self.tombstones().contains(draft_id)
    }
}

/// True when a draft has anything worth keeping on the server.
pub fn has_content(m: &Outgoing) -> bool {
    m.to.iter().chain(&m.cc).chain(&m.bcc).any(|a| !a.trim().is_empty()) || !m.subject.trim().is_empty() || !m.text.trim().is_empty()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MirrorStatus {
    /// The server copy is current.
    Saved,
    /// The server cannot do it safely (no UIDPLUS) or the account has no Drafts folder: nothing was changed.
    Unsupported,
    /// The draft has no content (or was removed): no server copy remains.
    Empty,
}

#[tauri::command]
pub async fn draft_mirror_save(
    state: State<'_, AppState>,
    composer: State<'_, ComposeStore>,
    mirror: State<'_, DraftMirror>,
    draft_id: String,
    account: String,
    message: Outgoing,
) -> Result<MirrorStatus, String> {
    if !valid_id(&draft_id) {
        return Err("Invalid draft id.".into());
    }
    if mirror.is_tombstoned(&draft_id) {
        return Ok(MirrorStatus::Empty);
    }
    if mirror.stopped.load(Ordering::Relaxed) {
        return Ok(MirrorStatus::Unsupported);
    }
    if !has_content(&message) {
        remove_inner(&state, &mirror, &draft_id).await?;
        return Ok(MirrorStatus::Empty);
    }
    let _gate = mirror.gate.lock().await;
    if mirror.is_tombstoned(&draft_id) {
        return Ok(MirrorStatus::Empty);
    }
    let cfg = state.account(&account)?;
    let folders = state.storage.list_folders(&account).map_err(err)?;
    let Some(drafts) = find_role(&folders, FolderRole::Drafts).map(|f| f.name.clone()) else {
        return Ok(MirrorStatus::Unsupported);
    };
    // The sender is the account address unless the user picked one of their own identities.
    let from = resolve_from(&cfg.email, &composer.list_identities()?, message.from_address.as_deref()).or_else(|_| resolve_from(&cfg.email, &[], None))?;
    let msg: OutgoingMessage = message.into();
    let raw = mailcore::smtp::build_draft(&from.mailbox, &msg).map_err(err)?.formatted();
    let prev = mirror.get(&draft_id)?.filter(|(a, _)| *a == account).map(|(_, r)| r);
    let mut conn = state.connect(&account).await?;
    let res = mirror_draft(&mut conn.client, &drafts, &raw, prev.as_ref()).await;
    if res.is_ok() {
        sweep_account(&mirror, &account, &mut conn.client).await; // a good connection: retry removals that failed earlier
    }
    conn.close().await;
    if matches!(&res, Err(mailcore::Error::Imap(m)) if m == UNLOCATED) {
        mirror.stopped.store(true, Ordering::Relaxed);
    }
    match res.map_err(err)? {
        Some(r) => {
            mirror.put(&draft_id, &account, &r)?;
            Ok(MirrorStatus::Saved)
        }
        None => Ok(MirrorStatus::Unsupported),
    }
}

async fn remove_inner(state: &AppState, mirror: &DraftMirror, draft_id: &str) -> Result<(), String> {
    let _gate = mirror.gate.lock().await;
    let Some((account, r)) = mirror.get(draft_id)? else { return Ok(()) };
    // Keep the row (flagged) until the server copy is verifiably gone; a failed attempt is retried on the next
    // successful connection (a later save/remove) and at launch.
    mirror.mark_remove_pending(draft_id)?;
    let mut conn = state.connect(&account).await?;
    let res = remove_draft(&mut conn.client, &r).await;
    if res.is_ok() {
        mirror.forget_row(draft_id)?;
        sweep_account(mirror, &account, &mut conn.client).await;
    }
    conn.close().await;
    res.map_err(err)?;
    Ok(())
}

/// Retry every removal of `account` that failed earlier. The caller holds the gate. Rows go only after a clean result.
async fn sweep_account(mirror: &DraftMirror, account: &str, client: &mut mailcore::ImapClient) {
    for (id, r) in mirror.pending(account).unwrap_or_default() {
        if remove_draft(client, &r).await.is_ok() {
            let _ = mirror.forget_row(&id);
        }
    }
}

/// At launch (a few seconds after start): finish removals that could not be done last time.
pub fn start_sweep(app: &tauri::AppHandle) {
    use tauri::Manager;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        let (state, mirror) = (app.state::<AppState>(), app.state::<DraftMirror>());
        let Ok(accounts) = mirror.pending_accounts() else { return };
        for account in accounts {
            let _gate = mirror.gate.lock().await;
            if let Ok(mut conn) = state.connect(&account).await {
                sweep_account(&mirror, &account, &mut conn.client).await;
                conn.close().await;
            }
        }
    });
}

/// Pane sent or discarded: delete its server copy (if we made one) and refuse any save still in flight.
#[tauri::command]
pub async fn draft_mirror_remove(state: State<'_, AppState>, mirror: State<'_, DraftMirror>, draft_id: String) -> Result<(), String> {
    if !valid_id(&draft_id) {
        return Err("Invalid draft id.".into());
    }
    mirror.tombstone(&draft_id);
    remove_inner(&state, &mirror, &draft_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(to: &[&str], subject: &str, text: &str) -> Outgoing {
        serde_json::from_value(serde_json::json!({"to": to, "cc": [], "bcc": [], "subject": subject, "text": text, "inReplyTo": null})).unwrap()
    }

    #[test]
    fn only_panes_with_content_are_mirrored() {
        assert!(!has_content(&out(&[], "", "")));
        assert!(!has_content(&out(&[" "], "  ", "\n")));
        assert!(has_content(&out(&["a@b.c"], "", "")));
        assert!(has_content(&out(&[], "Hi", "")));
        assert!(has_content(&out(&[], "", "text")));
    }

    #[test]
    fn rows_round_trip_and_tombstones_stick() {
        let m = DraftMirror::open_in_memory().unwrap();
        let r = DraftRef { folder: "Drafts".into(), validity: 7, uid: 42, message_id: "a@b".into() };
        m.put("c-1", "me@x.org", &r).unwrap();
        assert_eq!(m.get("c-1").unwrap(), Some(("me@x.org".into(), r.clone())));
        let r2 = DraftRef { uid: 43, ..r };
        m.put("c-1", "me@x.org", &r2).unwrap();
        assert_eq!(m.get("c-1").unwrap().unwrap().1.uid, 43);
        m.forget_row("c-1").unwrap();
        assert_eq!(m.get("c-1").unwrap(), None);
        assert!(!m.is_tombstoned("c-1"));
        m.tombstone("c-1");
        assert!(m.is_tombstoned("c-1"));
    }

    #[test]
    fn pending_removals_are_kept_and_account_rows_cleared() {
        let m = DraftMirror::open_in_memory().unwrap();
        let r = DraftRef { folder: "Drafts".into(), validity: 7, uid: 42, message_id: "a@b".into() };
        m.put("c-1", "me@x.org", &r).unwrap();
        m.put("c-2", "me@x.org", &r).unwrap();
        m.put("c-3", "you@x.org", &r).unwrap();
        assert!(m.pending("me@x.org").unwrap().is_empty());
        m.mark_remove_pending("c-1").unwrap();
        assert_eq!(m.pending("me@x.org").unwrap(), vec![("c-1".to_string(), r.clone())]);
        assert_eq!(m.pending_accounts().unwrap(), vec!["me@x.org".to_string()]);
        m.put("c-1", "me@x.org", &r).unwrap(); // saved again: live, no longer pending
        assert!(m.pending("me@x.org").unwrap().is_empty());
        m.delete_for_account("me@x.org").unwrap();
        assert_eq!((m.get("c-1").unwrap(), m.get("c-2").unwrap()), (None, None));
        assert!(m.get("c-3").unwrap().is_some());
    }

    #[test]
    fn persists_in_a_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workspace.db");
        let r = DraftRef { folder: "Drafts".into(), validity: 1, uid: 2, message_id: "x@y".into() };
        DraftMirror::open(&p).unwrap().put("c-9", "a@b.c", &r).unwrap();
        assert_eq!(DraftMirror::open(&p).unwrap().get("c-9").unwrap().unwrap().1, r);
    }
}
