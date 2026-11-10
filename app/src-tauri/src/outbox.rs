//! Send pipeline: undo-send hold, persistent outbox, retry and exactly-once safety (docs/ESSENTIALS_SPEC.md).
//!
//! Compose's Send button calls `outbox_submit`; nothing else in the app creates outbox entries (the AI cannot
//! send). An entry lives in `workspace.db` (attachments as blobs) from the click until the SMTP server accepts it:
//!
//! ```text
//! holding --(due)--> sending --ok--> (deleted, copy appended to Sent)
//!    ^                  |--- 4xx/5xx/connect error ---> failed --(backoff, <= 3 times)--> sending
//!    |                  `--- outcome unknown (timeout, dropped after DATA) ---> unknown
//! retry
//! ```
//!
//! * `holding` is the undo window; `outbox_undo` removes the entry and hands everything back to Compose.
//! * `sending` is written BEFORE the SMTP conversation. If the app dies in that state the entry becomes `unknown`
//!   on the next launch: it is never sent again automatically, the user decides (the mail may have gone out).
//! * Entries found `holding` at launch become `interrupted` (the app was quit during the undo window): the UI asks
//!   "You have an unsent message" instead of sending or dropping it. `failed` entries lose their pending retry.
//! * Automatic retries: at most 3 per user action, with growing delays; after that the entry waits for the user.
//! * `scheduled` (Send later): an entry that waits for a chosen time. It is NEVER claimed by `claim_due`; only
//!   `promote_scheduled` (run by the scheduler while the app is open) turns it into `holding`, and only when its time
//!   passed less than `MAX_LATE_MS` ago. Anything later (the app was closed, or the Mac slept through it) becomes
//!   `interrupted`: the same "You have an unsent message" prompt (Send now / Edit / Discard). At launch nothing
//!   scheduled is ever promoted, whatever the time.
//!
//! Message content is user content: never logged, never in error strings.

use crate::composer::{resolve_from, ComposeStore};
use crate::state::{err, AppState};
use crate::wire::{decode_attachments, AttachmentData, Outgoing};
use base64::Engine as _;
use mailcore::{imap::find_role, FolderRole, OutgoingAttachment, OutgoingMessage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Notify;

/// Undo-send delays the UI offers (seconds).
pub const DELAYS: [u32; 4] = [0, 5, 10, 30];
/// Automatic retries after a failed attempt, per user action.
pub const MAX_AUTO_RETRIES: u32 = 3;
/// Seconds to wait before automatic retry 1, 2, 3.
pub const BACKOFF_SECS: [i64; 3] = [30, 120, 300];
pub const MAX_ENTRIES: i64 = 50;
/// A scheduled message whose time passed more than this long ago (while the app was running, e.g. the Mac slept) is
/// not sent silently: the user is asked.
pub const MAX_LATE_MS: i64 = 10 * 60 * 1000;
/// Furthest ahead a message may be scheduled.
pub const MAX_SCHEDULE_AHEAD_MS: i64 = 366 * 86_400_000;
/// Total attachment bytes the outbox keeps on disk across all entries.
pub const MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;
/// Largest compose-restore blob (the pane payload) per entry.
pub const MAX_RESTORE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryState {
    /// In the undo window (or queued to be sent as soon as the scheduler runs).
    Holding,
    Sending,
    /// SMTP refused or could not be reached; the message was NOT sent.
    Failed,
    /// Might have been sent: the user decides.
    Unknown,
    /// The app was closed while the message was held: ask the user.
    Interrupted,
    /// Send later: waiting for `scheduled_at`; sent only while the app is open.
    Scheduled,
}

impl EntryState {
    fn parse(s: &str) -> EntryState {
        match s {
            "holding" => EntryState::Holding,
            "sending" => EntryState::Sending,
            "failed" => EntryState::Failed,
            // `editing`: parked while Compose reopens it; after a reload it is just an unsent message to decide on.
            "interrupted" | "editing" => EntryState::Interrupted,
            "scheduled" => EntryState::Scheduled,
            // Anything unreadable is treated as the most cautious state.
            _ => EntryState::Unknown,
        }
    }
}

/// What the UI sees of an entry (no body, no attachment bytes).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxItem {
    pub id: String,
    pub account: String,
    pub to: Vec<String>,
    pub subject: String,
    pub state: EntryState,
    pub error: Option<String>,
    /// Unix ms at which a `holding` entry is sent.
    pub due_at: i64,
    /// Unix ms of the next automatic retry (only while `failed` with retries left).
    pub next_retry_at: Option<i64>,
    /// Failed attempts since the last user action.
    pub failures: u32,
    pub attachments: u32,
    pub created_at: i64,
    /// Unix ms a `scheduled` (Send later) entry is to be sent.
    pub scheduled_at: Option<i64>,
}

/// What Undo / Edit hands back to Compose.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Restored {
    /// The Compose pane payload exactly as the frontend stored it at Send.
    pub restore: String,
    /// The files the user attached (pasted pictures that were folded into the HTML are not repeated).
    pub attachments: Vec<AttachmentData>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    msg: OutgoingMessage,
    from_address: Option<String>,
}

/// A claimed entry, ready for SMTP.
pub struct Job {
    pub id: String,
    pub account: String,
    pub msg: OutgoingMessage,
    pub from_address: Option<String>,
}

pub struct OutboxStore {
    conn: Mutex<Connection>,
    /// Ids that were `sending` when their account was removed: when the attempt ends, the entry is deleted instead of
    /// being scheduled for a retry (and `deliver` refuses to start one).
    doomed: Mutex<HashSet<String>>,
}

/// Minimal Compose payload for a message whose full restore payload is too big to store: recipients, subject and
/// (truncated) plain text; no pictures, no rich body.
fn reduced_restore(msg: &OutgoingMessage) -> String {
    let text: String = msg.text.chars().take(100_000).collect();
    serde_json::json!({
        "to": msg.to.join(", "), "cc": msg.cc.join(", "), "bcc": msg.bcc.join(", "),
        "subject": msg.subject, "text": text, "mode": "plain", "showCc": !msg.cc.is_empty() || !msg.bcc.is_empty(),
    })
    .to_string()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn new_id() -> String {
    // Local queue ids: nanosecond clock plus a per-process counter (never reused, never shown to the user).
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("o{:x}-{:x}", nanos, N.fetch_add(1, Ordering::Relaxed))
}

impl OutboxStore {
    /// Opens (creating tables) and applies launch recovery: see the module docs.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let s = Self::from_conn(Connection::open(path).map_err(|e| e.to_string())?)?;
        s.recover_on_launch()?;
        Ok(s)
    }
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        Self::from_conn(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }
    fn from_conn(conn: Connection) -> Result<Self, String> {
        // Sent bodies and attachments must not linger in free pages after the entry is deleted.
        conn.execute_batch("PRAGMA secure_delete=ON;").map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS outbox(
                id TEXT PRIMARY KEY, account TEXT NOT NULL, state TEXT NOT NULL, due_at INTEGER NOT NULL,
                next_retry_at INTEGER, failures INTEGER NOT NULL DEFAULT 0, error TEXT,
                created_at INTEGER NOT NULL, subject TEXT NOT NULL, to_list TEXT NOT NULL,
                message TEXT NOT NULL, restore TEXT NOT NULL, inline_count INTEGER NOT NULL DEFAULT 0,
                scheduled_at INTEGER);
             CREATE TABLE IF NOT EXISTS outbox_att(
                entry_id TEXT NOT NULL, idx INTEGER NOT NULL, name TEXT NOT NULL, mime TEXT NOT NULL, data BLOB NOT NULL,
                PRIMARY KEY(entry_id, idx));",
        )
        .map_err(|e| e.to_string())?;
        // Databases created before Send later: add the column (CREATE TABLE IF NOT EXISTS leaves old tables as they were).
        let has_col: bool = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('outbox') WHERE name='scheduled_at'", [], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())? > 0;
        if !has_col {
            conn.execute_batch("ALTER TABLE outbox ADD COLUMN scheduled_at INTEGER;").map_err(|e| e.to_string())?;
        }
        Ok(OutboxStore { conn: Mutex::new(conn), doomed: Mutex::new(HashSet::new()) })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "The outbox is unavailable.".to_string())
    }

    /// A crash or quit leaves no entry in a state that could cause an automatic (re)send.
    pub fn recover_on_launch(&self) -> Result<(), String> {
        self.recover_at(now_ms())
    }

    /// `recover_on_launch` with an explicit clock. A scheduled message whose time has come (<= `now`) is NOT sent: it
    /// becomes `interrupted` and the user is asked. One still in the future stays scheduled.
    pub fn recover_at(&self, now: i64) -> Result<(), String> {
        let c = self.lock()?;
        c.execute_batch(
            "UPDATE outbox SET state='unknown', error='MiraMail was closed while this message was being sent. It may or may not have been delivered: check your Sent folder before sending it again.' WHERE state='sending';
             UPDATE outbox SET state='interrupted' WHERE state IN ('holding','editing');
             UPDATE outbox SET next_retry_at=NULL WHERE state='failed';",
        )
        .map_err(|e| e.to_string())?;
        c.execute("UPDATE outbox SET state='interrupted', error='This message was scheduled for a time when MiraMail was closed.' WHERE state='scheduled' AND COALESCE(scheduled_at,0)<=?1", [now])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Scheduler step (app running): scheduled messages whose time has come become `holding` and due now, so the next
    /// `claim_due` sends them; those more than `MAX_LATE_MS` late become `interrupted` instead. Returns the ids touched.
    pub fn promote_scheduled(&self, now: i64) -> Result<Vec<String>, String> {
        let mut c = self.lock()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        let ids: Vec<(String, i64)> = {
            let mut st = tx.prepare("SELECT id, COALESCE(scheduled_at,0) FROM outbox WHERE state='scheduled' AND COALESCE(scheduled_at,0)<=?1 ORDER BY scheduled_at, id").map_err(|e| e.to_string())?;
            let rows = st.query_map([now], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        let mut out = Vec::new();
        for (id, at) in ids {
            let n = if now - at <= MAX_LATE_MS {
                tx.execute("UPDATE outbox SET state='holding', due_at=?2 WHERE id=?1 AND state='scheduled'", params![id, now])
            } else {
                tx.execute("UPDATE outbox SET state='interrupted', error='This message was scheduled for a time that has already passed.' WHERE id=?1 AND state='scheduled'", [&id])
            }
            .map_err(|e| e.to_string())?;
            if n == 1 {
                out.push(id);
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// Validate limits and store a new `holding` entry.
    #[allow(clippy::too_many_arguments)]
    pub fn submit(
        &self,
        account: &str,
        msg: &OutgoingMessage,
        from_address: Option<&str>,
        restore: &str,
        atts: &[OutgoingAttachment],
        inline_count: usize,
        delay_secs: u32,
        now: i64,
    ) -> Result<OutboxItem, String> {
        if !DELAYS.contains(&delay_secs) {
            return Err("Undo delay must be 0, 5, 10 or 30 seconds.".into());
        }
        self.submit_inner(account, msg, from_address, restore, atts, inline_count, delay_secs, now, None)
    }

    /// Send later: store the message as `scheduled` for `send_at` (Unix ms, in the future, at most a year ahead).
    #[allow(clippy::too_many_arguments)]
    pub fn submit_scheduled(
        &self,
        account: &str,
        msg: &OutgoingMessage,
        from_address: Option<&str>,
        restore: &str,
        atts: &[OutgoingAttachment],
        inline_count: usize,
        send_at: i64,
        now: i64,
    ) -> Result<OutboxItem, String> {
        if send_at <= now {
            return Err("Pick a time in the future.".into());
        }
        if send_at > now + MAX_SCHEDULE_AHEAD_MS {
            return Err("A message can be scheduled at most a year ahead.".into());
        }
        self.submit_inner(account, msg, from_address, restore, atts, inline_count, 0, now, Some(send_at))
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_inner(
        &self,
        account: &str,
        msg: &OutgoingMessage,
        from_address: Option<&str>,
        restore: &str,
        atts: &[OutgoingAttachment],
        inline_count: usize,
        delay_secs: u32,
        now: i64,
        scheduled_at: Option<i64>,
    ) -> Result<OutboxItem, String> {
        if inline_count > atts.len() {
            return Err("Invalid attachment list.".into());
        }
        // An oversize restore payload never blocks the send itself: keep a reduced one (recipients, subject, text).
        let restore = if restore.len() > MAX_RESTORE_BYTES { reduced_restore(msg) } else { restore.to_string() };
        let blob: usize = atts.iter().map(|a| a.data.len()).sum();
        let mut stored = msg.clone();
        stored.attachments.clear();
        let message = serde_json::to_string(&Stored { msg: stored, from_address: from_address.map(String::from) }).map_err(|_| "The message could not be encoded.".to_string())?;
        let mut c = self.lock()?;
        let n: i64 = c.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if n >= MAX_ENTRIES {
            return Err("The outbox is full. Open it and send or discard some messages first.".into());
        }
        let used: i64 = c.query_row("SELECT COALESCE(SUM(LENGTH(data)),0) FROM outbox_att", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if used as usize + blob > MAX_BLOB_BYTES {
            return Err("The outbox holds too many attachments already. Open it and send or discard some messages first.".into());
        }
        let id = new_id();
        let to_list = serde_json::to_string(&msg.to).unwrap_or_else(|_| "[]".into());
        let subject: String = msg.subject.chars().take(200).collect();
        let due_at = scheduled_at.unwrap_or(now + i64::from(delay_secs) * 1000);
        let state = if scheduled_at.is_some() { "scheduled" } else { "holding" };
        let tx = c.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO outbox(id,account,state,due_at,created_at,subject,to_list,message,restore,inline_count,scheduled_at) VALUES (?1,?2,?10,?3,?4,?5,?6,?7,?8,?9,?11)",
            params![id, account, due_at, now, subject, to_list, message, restore, inline_count as i64, state, scheduled_at],
        )
        .map_err(|e| e.to_string())?;
        for (i, a) in atts.iter().enumerate() {
            tx.execute("INSERT INTO outbox_att(entry_id,idx,name,mime,data) VALUES (?1,?2,?3,?4,?5)", params![id, i as i64, a.name, a.mime, a.data]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        drop(c);
        self.item(&id)?.ok_or_else(|| "The outbox is unavailable.".to_string())
    }

    fn is_doomed(&self, id: &str) -> bool {
        self.doomed.lock().unwrap_or_else(|p| p.into_inner()).contains(id)
    }

    fn row_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxItem> {
        let to: String = r.get(3)?;
        Ok(OutboxItem {
            id: r.get(0)?,
            account: r.get(1)?,
            subject: r.get(2)?,
            to: serde_json::from_str(&to).unwrap_or_default(),
            state: EntryState::parse(&r.get::<_, String>(4)?),
            error: r.get(5)?,
            due_at: r.get(6)?,
            next_retry_at: r.get(7)?,
            failures: r.get::<_, i64>(8)? as u32,
            created_at: r.get(9)?,
            attachments: r.get::<_, i64>(10)? as u32,
            scheduled_at: r.get(11)?,
        })
    }
    const COLS: &'static str = "id,account,subject,to_list,state,error,due_at,next_retry_at,failures,created_at,(SELECT COUNT(*) FROM outbox_att a WHERE a.entry_id=outbox.id),scheduled_at";

    fn item(&self, id: &str) -> Result<Option<OutboxItem>, String> {
        let c = self.lock()?;
        c.query_row(&format!("SELECT {} FROM outbox WHERE id=?1", Self::COLS), [id], Self::row_item).optional().map_err(|e| e.to_string())
    }

    pub fn list(&self) -> Result<Vec<OutboxItem>, String> {
        let c = self.lock()?;
        let mut st = c.prepare(&format!("SELECT {} FROM outbox ORDER BY created_at, id", Self::COLS)).map_err(|e| e.to_string())?;
        let rows = st.query_map([], Self::row_item).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Atomically move every due entry to `sending` and return it. An entry is claimed at most once per
    /// attempt, whatever the scheduler does.
    pub fn claim_due(&self, now: i64) -> Result<Vec<Job>, String> {
        let mut c = self.lock()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        let ids: Vec<String> = {
            let mut st = tx
                .prepare(
                    "SELECT id FROM outbox WHERE (state='holding' AND due_at<=?1) OR (state='failed' AND next_retry_at IS NOT NULL AND next_retry_at<=?1) ORDER BY created_at, id",
                )
                .map_err(|e| e.to_string())?;
            let rows = st.query_map([now], |r| r.get(0)).map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        let mut jobs = Vec::new();
        for id in ids {
            let n = tx
                .execute("UPDATE outbox SET state='sending', next_retry_at=NULL, error=NULL WHERE id=?1 AND state IN ('holding','failed')", [&id])
                .map_err(|e| e.to_string())?;
            if n != 1 {
                continue;
            }
            let (account, message): (String, String) = tx.query_row("SELECT account,message FROM outbox WHERE id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?;
            let stored: Stored = match serde_json::from_str(&message) {
                Ok(s) => s,
                Err(_) => {
                    tx.execute("UPDATE outbox SET state='failed', error='This outbox entry is damaged. Edit or discard it.' WHERE id=?1", [&id]).map_err(|e| e.to_string())?;
                    continue;
                }
            };
            let mut msg = stored.msg;
            let mut st = tx.prepare("SELECT name,mime,data FROM outbox_att WHERE entry_id=?1 ORDER BY idx").map_err(|e| e.to_string())?;
            let atts = st.query_map([&id], |r| Ok(OutgoingAttachment { name: r.get(0)?, mime: r.get(1)?, data: r.get(2)? })).map_err(|e| e.to_string())?;
            msg.attachments = atts.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
            drop(st);
            jobs.push(Job { id, account, msg, from_address: stored.from_address });
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(jobs)
    }

    /// The server accepted the message: the entry is gone for good.
    pub fn mark_sent(&self, id: &str) -> Result<(), String> {
        self.doomed.lock().unwrap_or_else(|p| p.into_inner()).remove(id);
        let mut c = self.lock()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM outbox_att WHERE entry_id=?1", [id]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM outbox WHERE id=?1", [id]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// An attempt ended without the server accepting the message. `outcome_unknown`: it may have been delivered,
    /// so no automatic retry is ever scheduled. Returns the resulting state.
    pub fn mark_failed(&self, id: &str, error: &str, outcome_unknown: bool, now: i64) -> Result<EntryState, String> {
        if self.forget_if_doomed(id)? {
            return Ok(EntryState::Failed);
        }
        let c = self.lock()?;
        let failures: i64 = c.query_row("SELECT failures FROM outbox WHERE id=?1 AND state='sending'", [id], |r| r.get(0)).optional().map_err(|e| e.to_string())?.unwrap_or(-1);
        if failures < 0 {
            return Err("That message is no longer being sent.".into());
        }
        let err_text: String = error.chars().take(300).collect();
        if outcome_unknown {
            c.execute("UPDATE outbox SET state='unknown', error=?2 WHERE id=?1", params![id, err_text]).map_err(|e| e.to_string())?;
            return Ok(EntryState::Unknown);
        }
        let failures = failures + 1;
        let next = (failures as usize <= MAX_AUTO_RETRIES as usize).then(|| now + BACKOFF_SECS[(failures as usize - 1).min(BACKOFF_SECS.len() - 1)] * 1000);
        c.execute("UPDATE outbox SET state='failed', error=?2, failures=?3, next_retry_at=?4 WHERE id=?1", params![id, err_text, failures, next]).map_err(|e| e.to_string())?;
        Ok(EntryState::Failed)
    }

    /// If the entry's account was removed while it was being sent, delete it now (no retry, ever).
    fn forget_if_doomed(&self, id: &str) -> Result<bool, String> {
        if !self.doomed.lock().unwrap_or_else(|p| p.into_inner()).remove(id) {
            return Ok(false);
        }
        let mut c = self.lock()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM outbox_att WHERE entry_id=?1", [id]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM outbox WHERE id=?1", [id]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Undo / Edit, step 1: park the entry as `editing` (never sent, shown as "interrupted" if the app reloads or
    /// quits before step 2) and return what Compose needs. Step 2 is `confirm_edit`, once the pane is open.
    pub fn take_for_edit(&self, id: &str) -> Result<Restored, String> {
        let mut c = self.lock()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        let row: Option<(String, String, i64)> = tx.query_row("SELECT state,restore,inline_count FROM outbox WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(|e| e.to_string())?;
        let Some((state, restore, inline_count)) = row else { return Err("That message has already been sent or removed.".into()) };
        if EntryState::parse(&state) == EntryState::Sending {
            return Err("It is being sent right now, too late to undo.".into());
        }
        let mut atts: Vec<(String, String, Vec<u8>)> = {
            let mut st = tx.prepare("SELECT name,mime,data FROM outbox_att WHERE entry_id=?1 ORDER BY idx").map_err(|e| e.to_string())?;
            let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        atts.truncate(atts.len().saturating_sub(inline_count.max(0) as usize));
        tx.execute("UPDATE outbox SET state='editing', next_retry_at=NULL WHERE id=?1", [id]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD;
        Ok(Restored { restore, attachments: atts.into_iter().map(|(name, mime, d)| AttachmentData { name, mime, data_base64: b64.encode(d) }).collect() })
    }

    /// Undo / Edit, step 2: the Compose pane is open, so the parked entry can go. A no-op for anything else.
    pub fn confirm_edit(&self, id: &str) -> Result<(), String> {
        let mut c = self.lock()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM outbox_att WHERE entry_id=?1 AND (SELECT state FROM outbox WHERE id=?1)='editing'", [id]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM outbox WHERE id=?1 AND state='editing'", [id]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// The user asked to send again (Retry, or "Send now" on an interrupted entry): due immediately, with a fresh retry budget.
    pub fn retry(&self, id: &str, now: i64) -> Result<(), String> {
        let c = self.lock()?;
        let n = c
            .execute(
                "UPDATE outbox SET state='holding', due_at=?2, failures=0, next_retry_at=NULL, error=NULL WHERE id=?1 AND state IN ('failed','interrupted','unknown','editing','scheduled')",
                params![id, now],
            )
            .map_err(|e| e.to_string())?;
        if n == 1 { Ok(()) } else { Err("That message cannot be sent again right now.".into()) }
    }

    pub fn discard(&self, id: &str) -> Result<(), String> {
        let mut c = self.lock()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        let state: Option<String> = tx.query_row("SELECT state FROM outbox WHERE id=?1", [id], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        match state.as_deref().map(EntryState::parse) {
            None => return Ok(()),
            Some(EntryState::Sending) => return Err("It is being sent right now, too late to discard.".into()),
            _ => {}
        }
        tx.execute("DELETE FROM outbox_att WHERE entry_id=?1", [id]).map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM outbox WHERE id=?1", [id]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Account removal: nothing of that account may stay queued.
    pub fn delete_for_account(&self, account: &str) -> Result<(), String> {
        let c = self.lock()?;
        let inflight: Vec<String> = {
            let mut st = c.prepare("SELECT id FROM outbox WHERE account=?1 AND state='sending'").map_err(|e| e.to_string())?;
            let rows = st.query_map([account], |r| r.get(0)).map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        self.doomed.lock().unwrap_or_else(|p| p.into_inner()).extend(inflight);
        c.execute("DELETE FROM outbox_att WHERE entry_id IN (SELECT id FROM outbox WHERE account=?1 AND state<>'sending')", [account]).map_err(|e| e.to_string())?;
        c.execute("DELETE FROM outbox WHERE account=?1 AND state<>'sending'", [account]).map_err(|e| e.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------
// Runtime: scheduler + SMTP.
// ---------------------------------------------------------------------------------------------------------

/// Managed Tauri state.
pub struct Outbox {
    pub store: OutboxStore,
    wake: Notify,
    /// Compose ids already submitted (a second submit for the same compose pane is rejected: double click, key repeat,
    /// a pane that moved windows mid-send). Bounded; oldest forgotten first.
    submitted: Mutex<std::collections::VecDeque<String>>,
    /// Per account, what we saw last time: `true` the server filed its own copy in Sent, `false` it did not.
    /// Memory only; it just picks how long to wait before appending our own copy.
    sent_saves: Mutex<std::collections::HashMap<String, bool>>,
}

const MAX_SUBMITTED: usize = 512;

impl Outbox {
    /// True the first time `compose_id` is seen. Ids are short opaque pane ids; anything else is refused.
    fn claim_compose(&self, compose_id: &str) -> Result<bool, String> {
        if compose_id.is_empty() || compose_id.len() > 64 || !compose_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err("Invalid message id.".into());
        }
        let mut q = self.submitted.lock().map_err(|_| "Outbox is unavailable.".to_string())?;
        if q.iter().any(|x| x == compose_id) {
            return Ok(false);
        }
        if q.len() >= MAX_SUBMITTED {
            q.pop_front();
        }
        q.push_back(compose_id.to_string());
        Ok(true)
    }
    fn release_compose(&self, compose_id: &str) {
        if let Ok(mut q) = self.submitted.lock() {
            q.retain(|x| x != compose_id);
        }
    }
}

impl Outbox {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        Ok(Outbox { store: OutboxStore::open(path)?, wake: Notify::new(), submitted: Mutex::new(Default::default()), sent_saves: Mutex::new(Default::default()) })
    }
}

#[derive(Clone, Serialize)]
struct Changed {
    id: String,
    /// "added" | "sent" | "failed" | "unknown" | "removed"
    kind: &'static str,
}

fn emit(app: &AppHandle, id: &str, kind: &'static str) {
    let _ = app.emit("outbox://changed", Changed { id: id.to_string(), kind });
}

/// Start the scheduler (call once from `setup`). It never sends anything that is not already due and `holding`
/// or `failed` with retries left; after a launch nothing is in those states.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            {
                let o = app.state::<Outbox>();
                let wake = o.wake.notified();
                // Send later: scheduled messages whose time came (app open) join the queue; stale ones ask the user.
                for id in o.store.promote_scheduled(now_ms()).unwrap_or_default() {
                    emit(&app, &id, "added");
                }
                let jobs = o.store.claim_due(now_ms()).unwrap_or_default();
                for job in jobs {
                    emit(&app, &job.id, "added");
                    let a = app.clone();
                    tauri::async_runtime::spawn(async move { send_job(a, job).await });
                }
                tokio::select! { _ = tokio::time::sleep(std::time::Duration::from_millis(1000)) => {}, _ = wake => {} }
            }
        }
    });
}

async fn send_job(app: AppHandle, job: Job) {
    let result = deliver(&app, &job).await;
    let o = app.state::<Outbox>();
    match result {
        Ok(raw) => {
            // Record "sent" before anything best-effort, so a crash can never lead to a second send.
            let _ = o.store.mark_sent(&job.id);
            emit(&app, &job.id, "sent");
            save_to_sent(&app, &job.account, &raw).await;
        }
        Err((msg, unknown)) => {
            let state = o.store.mark_failed(&job.id, &msg, unknown, now_ms()).unwrap_or(EntryState::Failed);
            emit(&app, &job.id, if state == EntryState::Unknown { "unknown" } else { "failed" });
        }
    }
}

/// `Err((message, outcome_unknown))`.
async fn deliver(app: &AppHandle, job: &Job) -> Result<Vec<u8>, (String, bool)> {
    let state = app.state::<AppState>();
    let composer = app.state::<ComposeStore>();
    let known = |e: String| (e, false);
    if app.state::<Outbox>().store.is_doomed(&job.id) {
        return Err(known("That account was removed.".into()));
    }
    let mut cfg = state.account(&job.account).map_err(known)?;
    let password = state.password(&job.account).map_err(known)?;
    let from = resolve_from(&cfg.email, &composer.list_identities().map_err(known)?, job.from_address.as_deref()).map_err(known)?;
    cfg.email = from.mailbox;
    let mut msg = job.msg.clone();
    if let Some(rt) = from.reply_to {
        msg.extra_headers.push(("Reply-To".into(), rt));
    }
    mailcore::smtp::send(&cfg, &password, &msg).await.map_err(|e| (err(&e), e.outcome_unknown()))
}

/// Best effort: the mail is sent, a missing Sent copy must not turn into an error. Servers that file SMTP
/// submissions into Sent themselves would hold two copies if we always appended, so first look for the
/// message's Message-ID in Sent (see `mailcore::sentcopy`); any doubt falls back to appending.
pub async fn save_to_sent(app: &AppHandle, account: &str, raw: &[u8]) {
    let state = app.state::<AppState>();
    let folders = state.storage.list_folders(account).unwrap_or_default();
    let Some(sent) = find_role(&folders, FolderRole::Sent) else { return };
    let mid = mailcore::draftsync::extract_message_id(raw);
    let known = app.state::<Outbox>().sent_saves.lock().ok().and_then(|m| m.get(account).copied());
    let decision = mailcore::sentcopy::decide(
        mid.is_some(),
        known,
        || async {
            // A fresh connection per check: its SELECT sees mail filed since the last one, and the account lock
            // is not held while we wait.
            let id = mid.as_deref().unwrap_or_default();
            let attempt = async {
                let mut conn = state.connect(account).await.map_err(mailcore::Error::Invalid)?;
                let r = mailcore::sentcopy::folder_has_message_id(&mut conn.client, &sent.name, id).await;
                conn.close().await;
                r
            };
            tokio::time::timeout(std::time::Duration::from_secs(25), attempt).await.unwrap_or(Err(mailcore::Error::Timeout("sent check")))
        },
        tokio::time::sleep,
    )
    .await;
    if let (Some(l), Ok(mut m)) = (decision.learned, app.state::<Outbox>().sent_saves.lock()) {
        m.insert(account.to_string(), l);
    }
    if decision.append {
        if let Ok(mut conn) = state.connect(account).await {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(60), conn.client.append(&sent.name, raw, true)).await;
            conn.close().await;
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------------------------------------

/// Validate the message and hold it for `delay_secs` (undo window), then send it. `restore` is the Compose pane
/// payload (JSON) that Undo/Edit gives back; the last `inline_count` attachments are pictures folded in from the
/// rich body and are not restored as files.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn outbox_submit(
    app: AppHandle,
    state: State<'_, AppState>,
    composer: State<'_, ComposeStore>,
    outbox: State<'_, Outbox>,
    account: String,
    message: Outgoing,
    restore: String,
    inline_count: usize,
    delay_secs: u32,
    send_at: Option<i64>,
    compose_id: Option<String>,
) -> Result<OutboxItem, String> {
    if message.to.iter().all(|a| a.trim().is_empty()) {
        return Err("Add at least one recipient.".into());
    }
    if message.html.as_ref().is_some_and(|h| h.len() > mailcore::MAX_COMPOSE_HTML) {
        return Err("This message is too large to send. Remove some pictures.".into());
    }
    let cfg = state.account(&account)?;
    state.password(&account)?;
    let from_address = message.from_address.clone();
    let from = resolve_from(&cfg.email, &composer.list_identities()?, from_address.as_deref())?;
    let atts = decode_attachments(&message.attachments)?;
    let mut msg: OutgoingMessage = message.into();
    msg.attachments = atts.clone();
    // Dry run of the MIME build: bad addresses or headers fail now, not after the undo window.
    mailcore::smtp::build_message(&from.mailbox, &msg).map_err(err)?;
    msg.attachments.clear();
    // One submit per compose pane: validation above may be retried freely, an accepted submit may not be repeated.
    if let Some(cid) = compose_id.as_deref() {
        if !outbox.claim_compose(cid)? {
            return Err("This message was already sent.".into());
        }
    }
    // `send_at` (Send later) is the only other way in; the entry still comes from this Compose command.
    let stored = match send_at {
        Some(at) => outbox.store.submit_scheduled(&account, &msg, from_address.as_deref(), &restore, &atts, inline_count, at, now_ms()),
        None => outbox.store.submit(&account, &msg, from_address.as_deref(), &restore, &atts, inline_count, delay_secs, now_ms()),
    };
    let item = match stored {
        Ok(i) => i,
        Err(e) => {
            if let Some(cid) = compose_id.as_deref() {
                outbox.release_compose(cid);
            }
            return Err(e);
        }
    };
    emit(&app, &item.id, "added");
    outbox.wake.notify_one();
    Ok(item)
}

#[tauri::command]
pub fn outbox_list(outbox: State<'_, Outbox>) -> Result<Vec<OutboxItem>, String> {
    outbox.store.list()
}

/// Undo (inside the hold) or Edit (failed / unknown / interrupted): parks the entry (never sent meanwhile) and returns
/// its contents; `outbox_confirm_edit` deletes it once Compose is open.
#[tauri::command]
pub fn outbox_undo(app: AppHandle, outbox: State<'_, Outbox>, id: String) -> Result<Restored, String> {
    let r = outbox.store.take_for_edit(&id)?;
    emit(&app, &id, "removed");
    Ok(r)
}

/// Undo / Edit, step 2: Compose is open with the message, so the parked entry is deleted for good.
#[tauri::command]
pub fn outbox_confirm_edit(app: AppHandle, outbox: State<'_, Outbox>, id: String) -> Result<(), String> {
    outbox.store.confirm_edit(&id)?;
    emit(&app, &id, "removed");
    Ok(())
}

#[tauri::command]
pub fn outbox_retry(app: AppHandle, outbox: State<'_, Outbox>, id: String) -> Result<(), String> {
    outbox.store.retry(&id, now_ms())?;
    emit(&app, &id, "added");
    outbox.wake.notify_one();
    Ok(())
}

#[tauri::command]
pub fn outbox_discard(app: AppHandle, outbox: State<'_, Outbox>, id: String) -> Result<(), String> {
    outbox.store.discard(&id)?;
    emit(&app, &id, "removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> OutgoingMessage {
        OutgoingMessage {
            to: vec!["bob@example.net".into()],
            cc: vec![],
            bcc: vec![],
            subject: "Hello".into(),
            text: "secret body".into(),
            html: Some("<p>secret body</p>".into()),
            attachments: vec![],
            in_reply_to: None,
            calendar: None,
            extra_headers: vec![],
        }
    }
    fn att(name: &str, n: usize) -> OutgoingAttachment {
        OutgoingAttachment { name: name.into(), mime: "application/pdf".into(), data: vec![7u8; n] }
    }
    fn submit(s: &OutboxStore, delay: u32, now: i64) -> String {
        s.submit("me@x.org", &msg(), None, r#"{"to":"bob@example.net"}"#, &[att("a.pdf", 10), att("pic.png", 5)], 1, delay, now).unwrap().id
    }

    #[test]
    fn holds_for_the_delay_then_claims_exactly_once() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = submit(&s, 5, 1_000);
        assert!(s.claim_due(5_999).unwrap().is_empty(), "still inside the undo window");
        let jobs = s.claim_due(6_000).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, id);
        assert_eq!(jobs[0].msg.attachments.len(), 2, "all files travel, including the folded-in picture");
        assert_eq!(jobs[0].msg.text, "secret body");
        assert!(s.claim_due(60_000).unwrap().is_empty(), "a sending entry is never claimed again");
        assert_eq!(s.list().unwrap()[0].state, EntryState::Sending);
    }

    #[test]
    fn zero_delay_is_due_immediately() {
        let s = OutboxStore::open_in_memory().unwrap();
        submit(&s, 0, 1_000);
        assert_eq!(s.claim_due(1_000).unwrap().len(), 1);
    }

    #[test]
    fn rejects_bad_delay_and_an_oversize_restore_never_blocks_the_send() {
        let s = OutboxStore::open_in_memory().unwrap();
        assert!(s.submit("a", &msg(), None, "{}", &[], 0, 7, 0).is_err());
        let big = "x".repeat(MAX_RESTORE_BYTES + 1);
        let it = s.submit("a", &msg(), None, &big, &[], 0, 0, 0).unwrap();
        let jobs = s.claim_due(0).unwrap();
        assert_eq!((jobs.len(), jobs[0].msg.text.as_str()), (1, "secret body"), "the message itself is intact");
        let r = s.take_for_edit(&it.id).unwrap_err(); // sending now
        assert!(r.contains("too late"));
        let id2 = s.submit("a", &msg(), None, &big, &[], 0, 5, 0).unwrap().id;
        let r = s.take_for_edit(&id2).unwrap();
        assert!(r.restore.len() < 1000 && r.restore.contains("secret body") && r.restore.contains("bob@example.net"), "reduced restore kept");
        assert!(s.submit("a", &msg(), None, "{}", &[att("a", 1)], 2, 5, 0).is_err(), "inline_count beyond the list");
    }

    #[test]
    fn undo_within_the_delay_returns_everything_and_removes_the_entry() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = submit(&s, 10, 0);
        let r = s.take_for_edit(&id).unwrap();
        assert_eq!(r.restore, r#"{"to":"bob@example.net"}"#);
        assert_eq!(s.list().unwrap()[0].state, EntryState::Interrupted, "parked until Compose confirms");
        assert!(s.claim_due(1_000_000).unwrap().is_empty(), "a parked message is never sent");
        s.confirm_edit(&id).unwrap();
        assert_eq!(r.attachments.len(), 1, "the folded-in picture is not restored as a file");
        assert_eq!(r.attachments[0].name, "a.pdf");
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&r.attachments[0].data_base64).unwrap(), vec![7u8; 10]);
        assert!(s.list().unwrap().is_empty());
        assert!(s.claim_due(1_000_000).unwrap().is_empty(), "an undone message is never sent");
        assert!(s.take_for_edit(&id).is_err(), "second undo has nothing to undo");
        let conn = s.lock().unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM outbox_att", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "no orphaned attachment blobs");
    }

    #[test]
    fn too_late_once_sending_and_discard_is_refused_too() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = submit(&s, 0, 0);
        s.claim_due(0).unwrap();
        assert!(s.take_for_edit(&id).unwrap_err().contains("too late"));
        assert!(s.discard(&id).is_err());
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn success_removes_the_entry_and_its_blobs() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = submit(&s, 0, 0);
        s.claim_due(0).unwrap();
        s.mark_sent(&id).unwrap();
        assert!(s.list().unwrap().is_empty());
        let n: i64 = s.lock().unwrap().query_row("SELECT COUNT(*) FROM outbox_att", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn failures_back_off_three_times_then_wait_for_the_user() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = submit(&s, 0, 0);
        let mut now = 0;
        for (i, wait) in BACKOFF_SECS.iter().enumerate() {
            assert_eq!(s.claim_due(now).unwrap().len(), 1, "attempt {i}");
            assert_eq!(s.mark_failed(&id, "connection refused", false, now).unwrap(), EntryState::Failed);
            let it = &s.list().unwrap()[0];
            assert_eq!(it.next_retry_at, Some(now + wait * 1000));
            assert!(s.claim_due(now + wait * 1000 - 1).unwrap().is_empty(), "not before the backoff");
            now += wait * 1000;
        }
        // The 4th attempt (3rd automatic retry) fails: no more automatic sends, ever.
        assert_eq!(s.claim_due(now).unwrap().len(), 1);
        s.mark_failed(&id, "connection refused", false, now).unwrap();
        let it = &s.list().unwrap()[0];
        assert_eq!((it.state, it.next_retry_at, it.failures), (EntryState::Failed, None, 4));
        assert!(s.claim_due(now + 365 * 86_400_000).unwrap().is_empty());
        // The user presses Retry: one more send, with a fresh budget.
        s.retry(&id, now).unwrap();
        assert_eq!(s.claim_due(now).unwrap().len(), 1);
        s.mark_failed(&id, "again", false, now).unwrap();
        assert_eq!(s.list().unwrap()[0].failures, 1);
    }

    #[test]
    fn unknown_outcome_is_never_retried_automatically() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = submit(&s, 0, 0);
        s.claim_due(0).unwrap();
        assert_eq!(s.mark_failed(&id, "timed out", true, 0).unwrap(), EntryState::Unknown);
        assert!(s.claim_due(i64::MAX / 2).unwrap().is_empty());
        let it = &s.list().unwrap()[0];
        assert_eq!((it.state, it.next_retry_at), (EntryState::Unknown, None));
        // Only an explicit user action sends it again; Edit hands it back.
        s.retry(&id, 5).unwrap();
        assert_eq!(s.claim_due(5).unwrap().len(), 1);
    }

    #[test]
    fn mark_failed_only_applies_to_a_sending_entry() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = submit(&s, 30, 0);
        assert!(s.mark_failed(&id, "x", false, 0).is_err());
    }

    #[test]
    fn persistence_round_trip_and_crash_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workspace.db");
        let (held, sending, failed) = {
            let s = OutboxStore::open(&p).unwrap();
            let held = submit(&s, 30, 0);
            let sending = submit(&s, 0, 0);
            let failed = submit(&s, 0, 0);
            let claimed = s.claim_due(0).unwrap();
            assert_eq!(claimed.len(), 2);
            // `failed` was claimed too; settle it as failed with a retry pending.
            s.mark_failed(&failed, "refused", false, 0).unwrap();
            assert!(s.list().unwrap().iter().find(|i| i.id == failed).unwrap().next_retry_at.is_some());
            (held, sending, failed)
        };
        // "Crash": reopen the file. The app was running `sending` and `held`.
        let s = OutboxStore::open(&p).unwrap();
        let by = |id: &str| s.list().unwrap().into_iter().find(|i| i.id == id).unwrap();
        assert_eq!(by(&held).state, EntryState::Interrupted);
        assert_eq!(by(&sending).state, EntryState::Unknown);
        assert!(by(&sending).error.unwrap().contains("Sent folder"));
        assert_eq!((by(&failed).state, by(&failed).next_retry_at), (EntryState::Failed, None));
        // Nothing is ever sent by itself after a launch.
        assert!(s.claim_due(i64::MAX / 2).unwrap().is_empty());
        // The message survives intact, attachments included.
        assert_eq!(by(&held).attachments, 2);
        let r = s.take_for_edit(&held).unwrap();
        s.confirm_edit(&held).unwrap();
        assert_eq!(r.attachments.len(), 1);
        assert_eq!(r.restore, r#"{"to":"bob@example.net"}"#);
        // The user can still choose to send an interrupted entry.
        s.retry(&sending, 1).unwrap();
        let jobs = s.claim_due(1).unwrap();
        assert_eq!(jobs[0].msg.text, "secret body");
        assert_eq!(jobs[0].msg.attachments.len(), 2);
    }

    #[test]
    fn list_shows_summaries_without_content() {
        let s = OutboxStore::open_in_memory().unwrap();
        submit(&s, 5, 0);
        let json = serde_json::to_string(&s.list().unwrap()).unwrap();
        assert!(json.contains("bob@example.net") && json.contains("Hello") && json.contains("\"state\":\"holding\""));
        assert!(!json.contains("secret body"));
    }

    #[test]
    fn caps_entries_and_blob_bytes() {
        let s = OutboxStore::open_in_memory().unwrap();
        let big = att("big", MAX_BLOB_BYTES / 2 + 1);
        s.submit("a", &msg(), None, "{}", std::slice::from_ref(&big), 0, 5, 0).unwrap();
        assert!(s.submit("a", &msg(), None, "{}", &[big], 0, 5, 0).is_err());
        let s = OutboxStore::open_in_memory().unwrap();
        for _ in 0..MAX_ENTRIES {
            s.submit("a", &msg(), None, "{}", &[], 0, 5, 0).unwrap();
        }
        assert!(s.submit("a", &msg(), None, "{}", &[], 0, 5, 0).is_err());
    }

    #[test]
    fn account_removal_clears_queued_entries_but_not_one_in_flight() {
        let s = OutboxStore::open_in_memory().unwrap();
        let a = submit(&s, 0, 0);
        submit(&s, 30, 0);
        s.claim_due(0).unwrap();
        s.delete_for_account("me@x.org").unwrap();
        let l = s.list().unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].id, a);
    }

    #[test]
    fn a_reload_between_take_and_confirm_loses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workspace.db");
        let id = {
            let s = OutboxStore::open(&p).unwrap();
            let id = submit(&s, 10, 0);
            s.take_for_edit(&id).unwrap();
            id
        };
        let s = OutboxStore::open(&p).unwrap();
        let it = s.list().unwrap().into_iter().find(|i| i.id == id).unwrap();
        assert_eq!((it.state, it.attachments), (EntryState::Interrupted, 2));
        assert_eq!(s.take_for_edit(&id).unwrap().attachments.len(), 1, "can be taken again");
        s.confirm_edit(&id).unwrap();
        assert!(s.list().unwrap().is_empty());
        s.confirm_edit(&id).unwrap(); // idempotent
    }

    #[test]
    fn confirm_edit_never_deletes_a_real_queued_message() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = submit(&s, 30, 0);
        s.confirm_edit(&id).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn removed_account_in_flight_entry_is_deleted_not_retried() {
        let s = OutboxStore::open_in_memory().unwrap();
        let a = submit(&s, 0, 0);
        s.claim_due(0).unwrap();
        s.delete_for_account("me@x.org").unwrap();
        assert!(s.is_doomed(&a));
        s.mark_failed(&a, "451 try later", false, 0).unwrap();
        assert!(s.list().unwrap().is_empty());
        assert!(s.claim_due(i64::MAX / 2).unwrap().is_empty());
        let n: i64 = s.lock().unwrap().query_row("SELECT COUNT(*) FROM outbox_att", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // Same for an unknown outcome and for success.
        let b = submit(&s, 0, 0);
        s.claim_due(0).unwrap();
        s.delete_for_account("me@x.org").unwrap();
        s.mark_failed(&b, "timeout", true, 0).unwrap();
        assert!(s.list().unwrap().is_empty());
        let c = submit(&s, 0, 0);
        s.claim_due(0).unwrap();
        s.delete_for_account("me@x.org").unwrap();
        s.mark_sent(&c).unwrap();
        assert!(!s.is_doomed(&c) && s.list().unwrap().is_empty());
    }

    #[test]
    fn scheduled_is_never_claimed_until_promoted_by_the_running_app() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = s.submit_scheduled("me@x.org", &msg(), None, "{}", &[att("a.pdf", 3)], 0, 10_000, 1_000).unwrap().id;
        let it = &s.list().unwrap()[0];
        assert_eq!((it.state, it.scheduled_at, it.attachments), (EntryState::Scheduled, Some(10_000), 1));
        assert!(s.claim_due(i64::MAX / 2).unwrap().is_empty(), "claim_due can never send a scheduled entry");
        assert!(s.promote_scheduled(9_999).unwrap().is_empty(), "not before its time");
        assert_eq!(s.promote_scheduled(10_000).unwrap(), vec![id.clone()]);
        let jobs = s.claim_due(10_000).unwrap();
        assert_eq!((jobs.len(), jobs[0].id.as_str(), jobs[0].msg.attachments.len()), (1, id.as_str(), 1));
        assert!(s.promote_scheduled(20_000).unwrap().is_empty() && s.claim_due(20_000).unwrap().is_empty(), "exactly once");
    }

    #[test]
    fn scheduled_in_the_past_at_launch_is_never_sent_silently() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workspace.db");
        let (due, later) = {
            let s = OutboxStore::open(&p).unwrap();
            let due = s.submit_scheduled("me@x.org", &msg(), None, "{}", &[], 0, 5_000, 1_000).unwrap().id;
            let later = s.submit_scheduled("me@x.org", &msg(), None, "{}", &[], 0, 900_000, 1_000).unwrap().id;
            (due, later)
        };
        // Relaunch after the first time passed (fake clock, explicit).
        let s = OutboxStore::from_conn(Connection::open(&p).unwrap()).unwrap();
        s.recover_at(100_000).unwrap();
        let by = |id: &str| s.list().unwrap().into_iter().find(|i| i.id == id).unwrap();
        assert_eq!(by(&due).state, EntryState::Interrupted, "overdue at launch: ask the user");
        assert_eq!(by(&later).state, EntryState::Scheduled, "still in the future: keeps waiting");
        assert!(s.promote_scheduled(100_000).unwrap().is_empty());
        assert!(s.claim_due(i64::MAX / 2).unwrap().is_empty(), "nothing is sent by itself");
        // The user can decide: Send now (retry), Edit or Discard.
        s.retry(&due, 100_000).unwrap();
        assert_eq!(s.claim_due(100_000).unwrap().len(), 1);
        s.discard(&later).unwrap();
        assert!(s.list().unwrap().iter().all(|i| i.id != later));
    }

    #[test]
    fn a_long_sleep_past_the_time_asks_instead_of_sending() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = s.submit_scheduled("me@x.org", &msg(), None, "{}", &[], 0, 10_000, 0).unwrap().id;
        assert_eq!(s.promote_scheduled(10_000 + MAX_LATE_MS + 1).unwrap(), vec![id.clone()]);
        assert_eq!(s.list().unwrap()[0].state, EntryState::Interrupted);
        assert!(s.claim_due(i64::MAX / 2).unwrap().is_empty());
        // Edit hands it back to Compose; scheduled entries can also be edited before their time.
        let id2 = s.submit_scheduled("me@x.org", &msg(), None, r#"{"to":"x"}"#, &[], 0, 50_000, 0).unwrap().id;
        assert_eq!(s.take_for_edit(&id2).unwrap().restore, r#"{"to":"x"}"#);
        assert!(s.claim_due(i64::MAX / 2).unwrap().is_empty(), "parked while editing");
    }

    #[test]
    fn schedule_times_are_validated_and_old_databases_gain_the_column() {
        let s = OutboxStore::open_in_memory().unwrap();
        assert!(s.submit_scheduled("a", &msg(), None, "{}", &[], 0, 1_000, 1_000).is_err());
        assert!(s.submit_scheduled("a", &msg(), None, "{}", &[], 0, 1_000 + MAX_SCHEDULE_AHEAD_MS + 1, 1_000).is_err());
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE outbox(id TEXT PRIMARY KEY, account TEXT NOT NULL, state TEXT NOT NULL, due_at INTEGER NOT NULL, next_retry_at INTEGER, failures INTEGER NOT NULL DEFAULT 0, error TEXT, created_at INTEGER NOT NULL, subject TEXT NOT NULL, to_list TEXT NOT NULL, message TEXT NOT NULL, restore TEXT NOT NULL, inline_count INTEGER NOT NULL DEFAULT 0);").unwrap();
        let s = OutboxStore::from_conn(conn).unwrap();
        s.submit_scheduled("a", &msg(), None, "{}", &[], 0, 5_000, 0).unwrap();
    }

    #[test]
    fn secure_delete_is_on() {
        let s = OutboxStore::open_in_memory().unwrap();
        let v: i64 = s.lock().unwrap().query_row("PRAGMA secure_delete", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn discard_removes_failed_entries() {
        let s = OutboxStore::open_in_memory().unwrap();
        let id = submit(&s, 0, 0);
        s.claim_due(0).unwrap();
        s.mark_failed(&id, "x", false, 0).unwrap();
        s.discard(&id).unwrap();
        assert!(s.list().unwrap().is_empty());
        s.discard(&id).unwrap(); // idempotent
    }

    #[test]
    fn compose_id_is_claimed_once() {
        let dir = tempfile::tempdir().unwrap();
        let o = Outbox::open(dir.path().join("o.db")).unwrap();
        assert!(o.claim_compose("c-abc").unwrap());
        assert!(!o.claim_compose("c-abc").unwrap(), "second submit for the same pane is refused");
        assert!(o.claim_compose("c-def").unwrap());
        o.release_compose("c-abc");
        assert!(o.claim_compose("c-abc").unwrap(), "released after a failed store write");
        assert!(o.claim_compose("").is_err() && o.claim_compose("a b").is_err());
        for i in 0..(MAX_SUBMITTED + 5) {
            o.claim_compose(&format!("x{i}")).unwrap();
        }
        assert!(o.submitted.lock().unwrap().len() <= MAX_SUBMITTED);
    }
}
