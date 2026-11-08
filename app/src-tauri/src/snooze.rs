//! Snooze: a message leaves the way until a chosen time, then comes back to the Inbox as unread and a
//! notification says so (docs/ESSENTIALS_SPEC.md, Tier 2). Non-destructive by construction:
//!
//! * `moved`: the account has a "Snoozed" folder; the message is moved there with the set-safe batch move
//!   (UIDVALIDITY of the source checked) and moved back the same way when due (found again by Message-ID, which
//!   names the message regardless of UID renumbering);
//! * `hidden`: no Snoozed folder (or the user declined to create one): the message stays where it is on the server and is
//!   hidden in MiraMail's own lists until due. Other mail apps still show it.
//!
//! Entries live in `workspace.db`. The scheduler runs only while MiraMail is open (the UI says so); at launch every
//! overdue entry returns at once. Nothing is ever deleted from a mailbox; the only server change besides moves
//! is clearing the \Seen flag when a message comes back. Message content never reaches logs or error strings.

use crate::organise::apply_move_outcomes;
use crate::state::{err, AppState, INBOX};
use crate::wire::NewMailEvent;
use mailcore::{BatchItem, FlagChange, FolderRole};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tauri::{AppHandle, Emitter, Manager, State};

/// The scheduler wakes this often (the UI promises "within a minute").
const TICK_SECS: u64 = 20;
const MAX_ENTRIES: i64 = 2000;
/// A transient wake error is retried this many times (backoff 1 min doubling up to 1 h) before it becomes `failed`.
const MAX_ATTEMPTS: u32 = 12;

fn backoff_ms(attempts: u32) -> i64 {
    (60_000i64 << attempts.saturating_sub(1).min(6)).min(3_600_000)
}
/// Furthest a snooze may be set (one year).
const MAX_AHEAD_MS: i64 = 366 * 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Moved,
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snooze {
    pub id: i64,
    pub account: String,
    /// Folder the message was snoozed from.
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    pub message_id: Option<String>,
    pub mode: Mode,
    /// Where it waits (`moved` mode).
    pub snooze_folder: Option<String>,
    pub subject: String,
    pub from: String,
    /// Unix ms.
    pub wake_at: i64,
    pub created_at: i64,
    /// `pending` (row reserved while the server move runs; never listed) | `snoozed` | `waking` | `failed`
    pub state: String,
    pub error: Option<String>,
    /// Where the message landed in the Snoozed folder, and that folder's UIDVALIDITY then (`moved` mode, when the
    /// server told us). Wake uses them while the validity still matches, and the Message-ID otherwise.
    pub dest_uid: Option<u32>,
    pub dest_validity: Option<u32>,
    /// Transient wake failures so far (retried with backoff).
    pub attempts: u32,
    /// Earliest time (Unix ms) of the next retry after a transient failure.
    pub next_try: Option<i64>,
}

pub struct NewSnooze {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    pub message_id: Option<String>,
    pub mode: Mode,
    pub snooze_folder: Option<String>,
    pub subject: String,
    pub from: String,
    pub wake_at: i64,
}

pub struct SnoozeStore {
    conn: Mutex<Connection>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

const COLS: &str = "id,account,folder,uid,uidvalidity,message_id,mode,snooze_folder,subject,from_addr,wake_at,created_at,state,error,dest_uid,dest_validity,attempts,next_try";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Snooze> {
    Ok(Snooze {
        id: r.get(0)?,
        account: r.get(1)?,
        folder: r.get(2)?,
        uid: r.get(3)?,
        uidvalidity: r.get(4)?,
        message_id: r.get(5)?,
        mode: if r.get::<_, String>(6)? == "moved" { Mode::Moved } else { Mode::Hidden },
        snooze_folder: r.get(7)?,
        subject: r.get(8)?,
        from: r.get(9)?,
        wake_at: r.get(10)?,
        created_at: r.get(11)?,
        state: r.get(12)?,
        error: r.get(13)?,
        dest_uid: r.get(14)?,
        dest_validity: r.get(15)?,
        attempts: r.get(16)?,
        next_try: r.get(17)?,
    })
}

impl SnoozeStore {
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
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS snooze(
                id INTEGER PRIMARY KEY AUTOINCREMENT, account TEXT NOT NULL, folder TEXT NOT NULL, uid INTEGER NOT NULL,
                uidvalidity INTEGER NOT NULL, message_id TEXT, mode TEXT NOT NULL, snooze_folder TEXT,
                subject TEXT NOT NULL DEFAULT '', from_addr TEXT NOT NULL DEFAULT '', wake_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL, state TEXT NOT NULL DEFAULT 'snoozed', error TEXT);",
        )
        .map_err(|e| e.to_string())?;
        // Columns added later (existing databases keep their rows).
        let have: HashSet<String> = {
            let mut st = conn.prepare("PRAGMA table_info(snooze)").map_err(|e| e.to_string())?;
            let it = st.query_map([], |r| r.get::<_, String>(1)).map_err(|e| e.to_string())?;
            it.flatten().collect()
        };
        for (col, ddl) in [
            ("dest_uid", "dest_uid INTEGER"),
            ("dest_validity", "dest_validity INTEGER"),
            ("attempts", "attempts INTEGER NOT NULL DEFAULT 0"),
            ("next_try", "next_try INTEGER"),
        ] {
            if !have.contains(col) {
                conn.execute(&format!("ALTER TABLE snooze ADD COLUMN {ddl}"), []).map_err(|e| e.to_string())?;
            }
        }
        Ok(SnoozeStore { conn: Mutex::new(conn) })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "Snooze is unavailable.".to_string())
    }

    /// A crash mid-wake leaves `waking`; put it back so the next pass retries (waking is idempotent). A crash
    /// between reserving a snooze and finishing its server move leaves `pending`: the message may or may not have
    /// moved, so the entry is shown as failed (never silently dropped) and the user decides (Try again / Forget).
    pub fn recover_on_launch(&self) -> Result<(), String> {
        let c = self.lock()?;
        c.execute("UPDATE snooze SET state='snoozed' WHERE state='waking'", []).map_err(|e| e.to_string())?;
        c.execute(
            "UPDATE snooze SET state='failed', error='MiraMail closed while snoozing this message. Try again to bring it back, or Forget this snooze.' WHERE state='pending'",
            [],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Check everything that can be checked before touching the server (horizon, cap, bad times) and insert the row
    /// as `pending`. Call `commit` after the server move succeeded, or `abort` when it did not.
    pub fn reserve(&self, n: NewSnooze, now: i64) -> Result<i64, String> {
        if n.wake_at <= now {
            return Err("Pick a time in the future.".into());
        }
        if n.wake_at > now + MAX_AHEAD_MS {
            return Err("Snooze can be at most a year ahead.".into());
        }
        let c = self.lock()?;
        let count: i64 = c.query_row("SELECT COUNT(*) FROM snooze", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        if count >= MAX_ENTRIES {
            return Err("Too many snoozed messages. Unsnooze some first.".into());
        }
        let mode = if n.mode == Mode::Moved { "moved" } else { "hidden" };
        let subject: String = n.subject.chars().take(200).collect();
        let from: String = n.from.chars().take(200).collect();
        c.execute(
            "INSERT INTO snooze(account,folder,uid,uidvalidity,message_id,mode,snooze_folder,subject,from_addr,wake_at,created_at,state) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending')",
            params![n.account, n.folder, n.uid, n.uidvalidity, n.message_id, mode, n.snooze_folder, subject, from, n.wake_at, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(c.last_insert_rowid())
    }

    /// The server move succeeded: the entry is live. Records where the message landed.
    pub fn commit(&self, id: i64, dest_uid: Option<u32>, dest_validity: Option<u32>) -> Result<Snooze, String> {
        let c = self.lock()?;
        let n = c
            .execute("UPDATE snooze SET state='snoozed', dest_uid=?2, dest_validity=?3 WHERE id=?1 AND state='pending'", params![id, dest_uid, dest_validity])
            .map_err(|e| e.to_string())?;
        if n != 1 {
            return Err("The snooze entry disappeared.".into());
        }
        c.query_row(&format!("SELECT {COLS} FROM snooze WHERE id=?1"), [id], row).map_err(|e| e.to_string())
    }

    /// The server move failed: remove the reserved row (it never became live).
    pub fn abort(&self, id: i64) {
        if let Ok(c) = self.lock() {
            let _ = c.execute("DELETE FROM snooze WHERE id=?1 AND state='pending'", [id]);
        }
    }

    /// Reserve and commit in one step (tests).
    #[cfg(test)]
    pub fn add(&self, n: NewSnooze, now: i64) -> Result<Snooze, String> {
        let id = self.reserve(n, now)?;
        self.commit(id, None, None)
    }

    /// "Forget this snooze": remove the entry only; the message is not touched.
    pub fn forget(&self, id: i64) -> Result<(), String> {
        let n = self.lock()?.execute("DELETE FROM snooze WHERE id=?1 AND state IN ('snoozed','failed')", [id]).map_err(|e| e.to_string())?;
        if n != 1 {
            return Err("That snooze is being woken right now. Try again in a moment.".into());
        }
        Ok(())
    }

    /// Snoozes still pending for an account (the remove-account warning).
    pub fn count_for_account(&self, account: &str) -> usize {
        let Ok(c) = self.lock() else { return 0 };
        c.query_row("SELECT COUNT(*) FROM snooze WHERE account=?1 AND state!='pending'", [account], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize
    }

    pub fn list(&self) -> Result<Vec<Snooze>, String> {
        let c = self.lock()?;
        let mut st = c.prepare(&format!("SELECT {COLS} FROM snooze WHERE state!='pending' ORDER BY wake_at, id")).map_err(|e| e.to_string())?;
        let rows = st.query_map([], row).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// `(uid, uidvalidity)` of messages of this folder that are hidden by a pending snooze. A `failed` entry no
    /// longer hides its message (it must stay reachable).
    pub fn hidden(&self, account: &str, folder: &str) -> HashSet<(u32, u32)> {
        let Ok(c) = self.lock() else { return HashSet::new() };
        let Ok(mut st) = c.prepare("SELECT uid,uidvalidity FROM snooze WHERE account=?1 AND folder=?2 AND mode='hidden' AND state IN ('snoozed','waking')") else { return HashSet::new() };
        st.query_map(params![account, folder], |r| Ok((r.get(0)?, r.get(1)?))).map(|it| it.flatten().collect()).unwrap_or_default()
    }

    /// Atomically take every due, waiting entry (`snoozed` -> `waking`): each is woken at most once per pass.
    pub fn claim_due(&self, now: i64) -> Result<Vec<Snooze>, String> {
        let mut c = self.lock()?;
        let tx = c.transaction().map_err(|e| e.to_string())?;
        let ids: Vec<i64> = {
            let mut st = tx.prepare("SELECT id FROM snooze WHERE state='snoozed' AND wake_at<=?1 AND (next_try IS NULL OR next_try<=?1) ORDER BY wake_at, id").map_err(|e| e.to_string())?;
            let rows = st.query_map([now], |r| r.get(0)).map_err(|e| e.to_string())?;
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
        };
        let mut out = Vec::new();
        for id in ids {
            tx.execute("UPDATE snooze SET state='waking', error=NULL WHERE id=?1 AND state='snoozed'", [id]).map_err(|e| e.to_string())?;
            out.push(tx.query_row(&format!("SELECT {COLS} FROM snooze WHERE id=?1"), [id], row).map_err(|e| e.to_string())?);
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// Manual unsnooze: take one entry that is waiting or failed.
    pub fn claim_one(&self, id: i64) -> Result<Snooze, String> {
        let c = self.lock()?;
        let n = c.execute("UPDATE snooze SET state='waking', error=NULL, attempts=0, next_try=NULL WHERE id=?1 AND state IN ('snoozed','failed')", [id]).map_err(|e| e.to_string())?;
        if n != 1 {
            return Err("That message is no longer snoozed.".into());
        }
        c.query_row(&format!("SELECT {COLS} FROM snooze WHERE id=?1"), [id], row).map_err(|e| e.to_string())
    }

    pub fn finish(&self, id: i64) -> Result<(), String> {
        self.lock()?.execute("DELETE FROM snooze WHERE id=?1", [id]).map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn fail(&self, id: i64, error: &str) -> Result<(), String> {
        let e: String = error.chars().take(300).collect();
        self.lock()?.execute("UPDATE snooze SET state='failed', error=?2, next_try=NULL WHERE id=?1", params![id, e]).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Record a failed wake. Transient errors (offline, connection) go back to `snoozed` with exponential backoff
    /// and are retried; `failed` is reserved for permanent errors and for giving up after `MAX_ATTEMPTS`.
    /// Returns `true` when the entry is now `failed` (the user should be told).
    pub fn record_failure(&self, id: i64, err: &WakeError, now: i64) -> Result<bool, String> {
        match err {
            WakeError::Permanent(m) => {
                self.fail(id, m)?;
                Ok(true)
            }
            WakeError::Transient(m) => {
                let attempts: u32 = self.lock()?.query_row("SELECT attempts FROM snooze WHERE id=?1", [id], |r| r.get(0)).map_err(|e| e.to_string())?;
                let attempts = attempts + 1;
                if attempts >= MAX_ATTEMPTS {
                    self.fail(id, &format!("Gave up after {attempts} tries: {m}"))?;
                    return Ok(true);
                }
                let e: String = m.chars().take(300).collect();
                self.lock()?
                    .execute(
                        "UPDATE snooze SET state='snoozed', error=?2, attempts=?3, next_try=?4 WHERE id=?1",
                        params![id, e, attempts, now + backoff_ms(attempts)],
                    )
                    .map_err(|e| e.to_string())?;
                Ok(false)
            }
        }
    }

    pub fn delete_for_account(&self, account: &str) -> Result<(), String> {
        self.lock()?.execute("DELETE FROM snooze WHERE account=?1", [account]).map(|_| ()).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------------------------------------
// Scheduler core (testable with a fake clock and fake server operations).
// ---------------------------------------------------------------------------------------------------------

/// Why a wake did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeError {
    /// Offline, connection or server hiccup: retried later with backoff.
    Transient(String),
    /// Will not fix itself (message gone from the Snoozed folder, ambiguous Message-ID, ...): the entry is kept as `failed`.
    Permanent(String),
}

/// How a successful wake ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeOutcome {
    /// The message is in the Inbox again, unread.
    Back,
    /// `hidden` mode only: the message is no longer where it was hidden (the user moved or deleted it meanwhile).
    /// Nothing to bring back; the entry is finished without an "is back" notification.
    Gone,
}

/// What waking one snoozed message needs from the outside world.
pub trait SnoozeOps {
    /// Return the message to the Inbox as unread.
    fn wake(&self, s: &Snooze) -> impl std::future::Future<Output = Result<WakeOutcome, WakeError>> + Send;
}

/// Result of one scheduler pass.
#[derive(Debug, Default)]
pub struct Pass {
    pub back: Vec<Snooze>,
    pub gone: Vec<Snooze>,
    /// Entries that just became `failed` (with the reason), to tell the user once.
    pub failed: Vec<(Snooze, String)>,
}

/// Apply one wake result to the store (shared by the scheduler and manual unsnooze).
pub fn settle(store: &SnoozeStore, s: Snooze, res: Result<WakeOutcome, WakeError>, now: i64, pass: &mut Pass) {
    match res {
        Ok(WakeOutcome::Back) => {
            let _ = store.finish(s.id);
            pass.back.push(s);
        }
        Ok(WakeOutcome::Gone) => {
            let _ = store.finish(s.id);
            pass.gone.push(s);
        }
        Err(e) => {
            let msg = match &e {
                WakeError::Transient(m) | WakeError::Permanent(m) => m.clone(),
            };
            if store.record_failure(s.id, &e, now).unwrap_or(true) {
                pass.failed.push((s, msg));
            }
        }
    }
}

/// One scheduler pass at `now`: wake every due entry. Transient failures are retried later; permanent ones stay
/// in the list as `failed`.
pub async fn run_due<O: SnoozeOps>(store: &SnoozeStore, ops: &O, now: i64) -> Pass {
    let mut pass = Pass::default();
    for s in store.claim_due(now).unwrap_or_default() {
        let res = ops.wake(&s).await;
        settle(store, s, res, now, &mut pass);
    }
    pass
}

// ---------------------------------------------------------------------------------------------------------
// Runtime.
// ---------------------------------------------------------------------------------------------------------

pub struct SnoozeState {
    pub store: SnoozeStore,
}

struct AppOps {
    app: AppHandle,
}

fn snoozed_folder(folders: &[mailcore::Folder]) -> Option<String> {
    // The last path component, whatever the server's delimiter or namespace prefix ("INBOX.Snoozed", "Snoozed").
    let leaf = |f: &mailcore::Folder| -> String {
        let d = f.delimiter.as_deref().and_then(|d| d.chars().next()).unwrap_or('/');
        f.name.rsplit(d).next().unwrap_or("").to_string()
    };
    folders.iter().find(|f| f.selectable && leaf(f).eq_ignore_ascii_case("snoozed")).map(|f| f.name.clone())
}

impl SnoozeOps for AppOps {
    async fn wake(&self, s: &Snooze) -> Result<WakeOutcome, WakeError> {
        let state = self.app.state::<AppState>();
        let known = state.storage.list_folders(&s.account).map_err(|e| WakeError::Transient(err(e)))?;
        let inbox = mailcore::imap::find_role(&known, FolderRole::Inbox).map(|f| f.name.clone()).unwrap_or_else(|| INBOX.to_string());
        let mut conn = state.connect(&s.account).await.map_err(WakeError::Transient)?;
        let res = wake_on(&state, &mut conn, s, &inbox).await;
        conn.close().await;
        res
    }
}

/// Classify a mailcore error: network-ish problems are retried, the rest are permanent.
fn classify(e: &mailcore::Error) -> WakeError {
    use mailcore::Error as E;
    let m = err(e);
    match e {
        E::Io(_) | E::Tls(_) | E::Imap(_) | E::Timeout(_) | E::AuthFailed | E::Storage(_) | E::Secret(_) => WakeError::Transient(m),
        _ => WakeError::Permanent(m),
    }
}

/// How one move-back attempt ended.
enum Step {
    Moved(mailcore::MoveReport),
    /// The message is not in that folder (any more).
    NotFound,
    /// The folder's UIDVALIDITY is not the one we recorded: the stored UID means nothing.
    ValidityChanged,
}

/// Sort a per-item move failure into "not there" / "ambiguous" / "try again later". Typed here, once, so no caller
/// ever treats error text as success.
fn classify_outcome(e: Option<&str>) -> Result<Step, WakeError> {
    let e = e.unwrap_or("").trim();
    if e.is_empty() {
        return Err(WakeError::Transient("The message could not be moved back.".into()));
    }
    if e.contains("not found") || e.contains("no longer in the folder") {
        return Ok(Step::NotFound);
    }
    if e.contains("More than one message") {
        return Err(WakeError::Permanent(
            "More than one message in that folder has this Message-ID, so MiraMail will not guess. Move the right one to your Inbox yourself, then Forget this snooze.".into(),
        ));
    }
    Err(WakeError::Transient(e.to_string()))
}

async fn try_move(
    state: &AppState,
    conn: &mut crate::state::Conn,
    account: &str,
    from: &str,
    validity: Option<u32>,
    inbox: &str,
    item: &BatchItem,
) -> Result<Step, WakeError> {
    let report = match conn.client.batch_move(from, validity, inbox, std::slice::from_ref(item)).await {
        Ok(r) => r,
        Err(mailcore::Error::Imap(m)) if validity.is_some() && m.contains("UIDVALIDITY changed") => return Ok(Step::ValidityChanged),
        Err(e) => return Err(classify(&e)),
    };
    let Some(o) = report.outcomes.first() else { return Err(WakeError::Transient("The server gave no answer.".into())) };
    if !o.ok {
        return classify_outcome(o.error.as_deref());
    }
    apply_move_outcomes(&state.storage, account, from, &report.outcomes);
    Ok(Step::Moved(report))
}

async fn wake_on(state: &AppState, conn: &mut crate::state::Conn, s: &Snooze, inbox: &str) -> Result<WakeOutcome, WakeError> {
    // 1. Back to the Inbox.
    let by_id = BatchItem { uid: None, message_id: s.message_id.clone() };
    let mut unread_uid: Option<(u32, Option<u32>)>;
    let from_folder = match s.mode {
        Mode::Moved => s.snooze_folder.clone().ok_or_else(|| WakeError::Permanent("The snooze folder is unknown.".into()))?,
        Mode::Hidden => s.folder.clone(),
    };
    if from_folder == inbox {
        unread_uid = Some((s.uid, Some(s.uidvalidity)));
    } else {
        // Where to look first: the exact UID (while the folder's UIDVALIDITY is the recorded one), then the Message-ID.
        let first = match s.mode {
            Mode::Moved => match (s.dest_uid, s.dest_validity) {
                (Some(u), Some(v)) => Some((BatchItem { uid: Some(u), message_id: s.message_id.clone() }, Some(v))),
                _ => None,
            },
            Mode::Hidden => Some((BatchItem { uid: Some(s.uid), message_id: s.message_id.clone() }, Some(s.uidvalidity))),
        };
        let mut step = match first {
            Some((item, v)) => try_move(state, conn, &s.account, &from_folder, v, inbox, &item).await?,
            None => Step::ValidityChanged,
        };
        if !matches!(step, Step::Moved(_)) && s.message_id.is_some() && !(matches!(step, Step::NotFound) && s.mode == Mode::Hidden) {
            // The UID is stale (or never known): the Message-ID names the message. A duplicate is reported, not guessed.
            step = try_move(state, conn, &s.account, &from_folder, None, inbox, &by_id).await?;
        }
        let report = match step {
            Step::Moved(r) => r,
            Step::NotFound | Step::ValidityChanged => {
                return match s.mode {
                    // The user moved or deleted a hidden message meanwhile: nothing to bring back, nothing to retry.
                    Mode::Hidden => Ok(WakeOutcome::Gone),
                    Mode::Moved => Err(WakeError::Permanent(
                        "This message is no longer in your Snoozed folder (it was moved or deleted). Use Forget this snooze to remove the entry.".into(),
                    )),
                };
            }
        };
        let o = report.outcomes.first();
        unread_uid = o.and_then(|o| o.new_uid).map(|u| (u, report.dest_validity));
    }
    // 2. Make the Inbox cache see it, then mark it unread.
    let _ = conn.client.sync_folder(&state.storage, &s.account, inbox, state.initial_sync_limit()).await;
    if unread_uid.is_none() {
        if let Some(mid) = &s.message_id {
            let norm = mid.trim().trim_start_matches('<').trim_end_matches('>');
            if let Ok(Some(u)) = state.storage.uid_by_message_id(&s.account, inbox, norm) {
                unread_uid = Some((u, None));
            }
        }
    }
    if let Some((uid, v)) = unread_uid {
        let validity = v.or_else(|| state.storage.get_message(&s.account, inbox, uid).ok().flatten().map(|m| m.uidvalidity));
        if let Ok(rep) = conn.client.batch_flag(inbox, validity, &[uid], FlagChange::Seen(false)).await {
            crate::organise::apply_flag_outcomes(&state.storage, &s.account, inbox, &rep.outcomes, FlagChange::Seen(false));
        }
    }
    Ok(WakeOutcome::Back)
}

/// Tell the user about snoozed messages whose return failed for good (once, when the entry becomes `failed`).
fn announce_failed(app: &AppHandle, failed: &[(Snooze, String)]) {
    let _ = app.emit("snooze://changed", ());
    if failed.is_empty() {
        return;
    }
    let (title, body) = if failed.len() == 1 {
        ("A snoozed message could not come back".to_string(), crate::textsafe::clean_display(&failed[0].0.subject).chars().take(120).collect::<String>())
    } else {
        (format!("{} snoozed messages could not come back", failed.len()), String::new())
    };
    let _ = crate::notify::deliver(&title, &body, true);
}

fn announce(app: &AppHandle, back: &[Snooze]) {
    for s in back {
        let _ = app.emit("mail://new", NewMailEvent { account: s.account.clone(), folder: INBOX.to_string() });
    }
    let _ = app.emit("snooze://changed", ());
    if back.is_empty() {
        return;
    }
    let (title, body) = if back.len() == 1 {
        ("Snoozed message is back".to_string(), crate::textsafe::clean_display(&back[0].subject).chars().take(120).collect::<String>())
    } else {
        (format!("{} snoozed messages are back", back.len()), String::new())
    };
    let _ = crate::notify::deliver(&title, &body, true);
}

/// Start the scheduler (call once from `setup`). The first pass runs immediately: overdue snoozes return at launch.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Give the account watchers a moment to start so the first pass does not race the setup thread.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let ops = AppOps { app: app.clone() };
        loop {
            let pass = {
                let st = app.state::<SnoozeState>();
                run_due(&st.store, &ops, now_ms()).await
            };
            if !pass.back.is_empty() {
                announce(&app, &pass.back);
            }
            if !pass.failed.is_empty() {
                announce_failed(&app, &pass.failed);
            } else if !pass.gone.is_empty() {
                let _ = app.emit("snooze://changed", ());
            }
            tokio::time::sleep(std::time::Duration::from_secs(TICK_SECS)).await;
        }
    });
}

// ---------------------------------------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnoozeInfo {
    /// Name of the account's Snoozed folder, when it has one.
    pub folder: Option<String>,
}

#[tauri::command]
pub fn snooze_info(state: State<'_, AppState>, account: String) -> Result<SnoozeInfo, String> {
    let known = state.storage.list_folders(&account).map_err(err)?;
    Ok(SnoozeInfo { folder: snoozed_folder(&known) })
}

/// Snooze one cached message until `wake_at` (Unix ms). `use_folder`: move it into the account's Snoozed folder
/// (must exist; the UI asks before creating one); otherwise it is hidden locally.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri-injected State params + flat wire contract
pub async fn snooze_add(
    app: AppHandle,
    state: State<'_, AppState>,
    snooze: State<'_, SnoozeState>,
    account: String,
    folder: String,
    uid: u32,
    wake_at: i64,
    use_folder: bool,
) -> Result<Snooze, String> {
    let sum = state.storage.get_message(&account, &folder, uid).map_err(err)?.ok_or("Message not found. Refresh the folder.")?;
    let known = state.storage.list_folders(&account).map_err(err)?;
    let message_id = sum.message_id.clone().filter(|m| !m.trim().is_empty());
    let now = now_ms();
    let mut new = NewSnooze {
        account: account.clone(),
        folder: folder.clone(),
        uid,
        uidvalidity: sum.uidvalidity,
        message_id: message_id.clone(),
        mode: Mode::Hidden,
        snooze_folder: None,
        subject: sum.subject.clone(),
        from: sum.from.clone(),
        wake_at,
    };
    // Everything that can be validated is validated BEFORE the server is touched; the row is reserved as `pending`
    // first, so a refusal (horizon, cap, DB error) never leaves a moved message without an entry.
    let mut dest_folder: Option<String> = None;
    if use_folder {
        let dest = snoozed_folder(&known).ok_or("This account has no Snoozed folder.")?;
        if dest == folder {
            return Err("This message is already in the Snoozed folder.".into());
        }
        // Moving needs a Message-ID to find the message again; without one it is hidden instead.
        if message_id.is_some() {
            new.mode = Mode::Moved;
            new.snooze_folder = Some(dest.clone());
            dest_folder = Some(dest);
        }
    }
    let id = snooze.store.reserve(new, now)?;
    let (mut dest_uid, mut dest_validity) = (None, None);
    if let Some(dest) = dest_folder {
        let item = BatchItem { uid: Some(uid), message_id };
        let moved = async {
            let mut conn = state.connect(&account).await?;
            let r = conn.client.batch_move(&folder, Some(sum.uidvalidity), &dest, std::slice::from_ref(&item)).await;
            conn.close().await;
            let report = r.map_err(err)?;
            let o = report.outcomes.first().ok_or("The server gave no answer.")?;
            if !o.ok {
                return Err(o.error.clone().unwrap_or_else(|| "The message could not be moved.".into()));
            }
            apply_move_outcomes(&state.storage, &account, &folder, &report.outcomes);
            Ok::<_, String>((o.new_uid, report.dest_validity))
        }
        .await;
        match moved {
            Ok((u, v)) => (dest_uid, dest_validity) = (u, v),
            Err(e) => {
                snooze.store.abort(id);
                return Err(e);
            }
        }
    }
    let s = snooze.store.commit(id, dest_uid, dest_validity)?;
    let _ = app.emit("snooze://changed", ());
    Ok(s)
}

#[tauri::command]
pub fn snooze_list(snooze: State<'_, SnoozeState>) -> Result<Vec<Snooze>, String> {
    snooze.store.list()
}

/// Bring a snoozed message back now (also retries one that failed to return).
#[tauri::command]
pub async fn snooze_cancel(app: AppHandle, snooze: State<'_, SnoozeState>, id: i64) -> Result<(), String> {
    let s = snooze.store.claim_one(id)?;
    let ops = AppOps { app: app.clone() };
    let res = ops.wake(&s).await;
    let account = s.account.clone();
    let mut pass = Pass::default();
    let outcome = res.as_ref().map(|_| ()).map_err(|e| match e {
        WakeError::Transient(m) | WakeError::Permanent(m) => m.clone(),
    });
    settle(&snooze.store, s, res, now_ms(), &mut pass);
    let _ = app.emit("snooze://changed", ());
    if outcome.is_ok() {
        let _ = app.emit("mail://new", NewMailEvent { account, folder: INBOX.to_string() });
    }
    outcome
}

/// Remove a snooze entry only (the message is not touched): for a message that was moved or deleted by hand.
#[tauri::command]
pub fn snooze_forget(app: AppHandle, snooze: State<'_, SnoozeState>, id: i64) -> Result<(), String> {
    snooze.store.forget(id)?;
    let _ = app.emit("snooze://changed", ());
    Ok(())
}

/// How many snoozes an account has pending (the remove-account warning).
#[tauri::command]
pub fn snooze_count(snooze: State<'_, SnoozeState>, account: String) -> usize {
    snooze.store.count_for_account(&account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        calls: AtomicUsize,
        fail_ids: Vec<i64>,
        /// Ids that fail permanently instead of transiently.
        perm: Vec<i64>,
        gone: Vec<i64>,
    }
    impl SnoozeOps for Fake {
        async fn wake(&self, s: &Snooze) -> Result<WakeOutcome, WakeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.perm.contains(&s.id) {
                Err(WakeError::Permanent("gone for good".into()))
            } else if self.fail_ids.contains(&s.id) {
                Err(WakeError::Transient("offline".into()))
            } else if self.gone.contains(&s.id) {
                Ok(WakeOutcome::Gone)
            } else {
                Ok(WakeOutcome::Back)
            }
        }
    }
    fn fake(fail_ids: Vec<i64>) -> Fake {
        Fake { calls: AtomicUsize::new(0), fail_ids, perm: vec![], gone: vec![] }
    }
    fn new(uid: u32, at: i64, mode: Mode) -> NewSnooze {
        NewSnooze {
            account: "a@x.org".into(),
            folder: "INBOX".into(),
            uid,
            uidvalidity: 7,
            message_id: Some(format!("m{uid}@x")),
            mode,
            snooze_folder: (mode == Mode::Moved).then(|| "Snoozed".to_string()),
            subject: "Hi".into(),
            from: "bob@x".into(),
            wake_at: at,
        }
    }

    #[tokio::test]
    async fn wakes_only_when_due_and_only_once_with_a_fake_clock() {
        let s = SnoozeStore::open_in_memory().unwrap();
        let ops = fake(vec![]);
        s.add(new(1, 10_000, Mode::Moved), 1_000).unwrap();
        s.add(new(2, 20_000, Mode::Hidden), 1_000).unwrap();
        assert!(run_due(&s, &ops, 9_999).await.back.is_empty(), "not before the time");
        assert_eq!(run_due(&s, &ops, 10_000).await.back.len(), 1);
        assert!(run_due(&s, &ops, 10_001).await.back.is_empty(), "woken entries are gone");
        assert_eq!(ops.calls.load(Ordering::SeqCst), 1);
        let left = s.list().unwrap();
        assert_eq!((left.len(), left[0].uid), (1, 2));
        assert_eq!(run_due(&s, &ops, 99_999).await.back.len(), 1);
        assert!(s.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn overdue_entries_return_at_launch_and_interrupted_ones_are_retried() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workspace.db");
        {
            let s = SnoozeStore::open(&p).unwrap();
            s.add(new(1, 5_000, Mode::Moved), 1_000).unwrap();
            // Crash while waking entry 1.
            assert_eq!(s.claim_due(5_000).unwrap().len(), 1);
        }
        let s = SnoozeStore::open(&p).unwrap(); // "launch" a week later
        let pass = run_due(&s, &fake(vec![]), 7 * 86_400_000).await;
        assert_eq!(pass.back.len(), 1, "the interrupted one is retried");
        assert!(s.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn transient_failures_back_off_and_retry_permanent_ones_fail_and_are_reported() {
        let s = SnoozeStore::open_in_memory().unwrap();
        let a = s.add(new(1, 5_000, Mode::Moved), 1_000).unwrap();
        let b = s.add(new(2, 5_000, Mode::Moved), 1_000).unwrap();
        let ops = Fake { calls: AtomicUsize::new(0), fail_ids: vec![a.id], perm: vec![b.id], gone: vec![] };
        let pass = run_due(&s, &ops, 5_000).await;
        assert!(pass.back.is_empty());
        assert_eq!(pass.failed.len(), 1, "only the permanent failure is reported");
        assert_eq!((pass.failed[0].0.id, pass.failed[0].1.as_str()), (b.id, "gone for good"));
        let l = s.list().unwrap();
        let (la, lb) = (&l[0], &l[1]);
        assert_eq!((la.state.as_str(), la.error.as_deref(), la.attempts), ("snoozed", Some("offline"), 1));
        assert_eq!(lb.state, "failed");
        // Backoff: not retried before next_try, retried (and woken) after.
        assert!(run_due(&s, &ops, 5_001).await.back.is_empty());
        assert_eq!(ops.calls.load(Ordering::SeqCst), 2, "no second call inside the backoff window");
        let later = la.next_try.unwrap();
        let ok = fake(vec![]);
        assert_eq!(run_due(&s, &ok, later).await.back.len(), 1);
        // A failed entry waits for the user; manual retry works, Forget removes only the entry.
        assert!(run_due(&s, &ok, later + 10_000_000).await.back.is_empty());
        assert!(s.forget(lb.id).is_ok());
        assert!(s.list().unwrap().is_empty());
        assert!(s.forget(lb.id).is_err());
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts_and_a_gone_wake_is_not_announced_as_back() {
        let s = SnoozeStore::open_in_memory().unwrap();
        let a = s.add(new(1, 5_000, Mode::Hidden), 1_000).unwrap();
        let mut now = 5_000;
        let ops = fake(vec![a.id]);
        let mut failed = false;
        for _ in 0..MAX_ATTEMPTS {
            let pass = run_due(&s, &ops, now).await;
            failed = !pass.failed.is_empty();
            now += 4_000_000;
        }
        assert!(failed);
        assert_eq!(s.list().unwrap()[0].state, "failed");
        assert!(backoff_ms(1) == 60_000 && backoff_ms(40) == 3_600_000);
        let g = s.add(new(2, 6_000, Mode::Hidden), 1_000).unwrap();
        let ops = Fake { calls: AtomicUsize::new(0), fail_ids: vec![], perm: vec![], gone: vec![g.id] };
        let pass = run_due(&s, &ops, now).await;
        assert!(pass.back.is_empty() && pass.gone.len() == 1 && pass.failed.is_empty());
    }

    #[test]
    fn failed_hidden_entries_no_longer_hide_the_message() {
        let s = SnoozeStore::open_in_memory().unwrap();
        let a = s.add(new(1, 10_000, Mode::Hidden), 0).unwrap();
        assert!(s.hidden("a@x.org", "INBOX").contains(&(1, 7)));
        s.fail(a.id, "x").unwrap();
        assert!(s.hidden("a@x.org", "INBOX").is_empty());
    }

    #[test]
    fn reserve_validates_before_any_move_and_pending_rows_are_not_live() {
        let s = SnoozeStore::open_in_memory().unwrap();
        assert!(s.reserve(new(1, 500, Mode::Moved), 1_000).is_err());
        assert!(s.reserve(new(1, 1_000 + 400 * 86_400_000, Mode::Moved), 1_000).is_err());
        assert!(s.list().unwrap().is_empty(), "a refusal leaves no row");
        let id = s.reserve(new(1, 5_000, Mode::Moved), 1_000).unwrap();
        assert!(s.list().unwrap().is_empty(), "pending is not listed");
        assert!(s.claim_due(9_000).unwrap().is_empty(), "pending is never woken");
        s.abort(id);
        assert!(s.forget(id).is_err());
        let id = s.reserve(new(2, 5_000, Mode::Moved), 1_000).unwrap();
        let c = s.commit(id, Some(42), Some(9)).unwrap();
        assert_eq!((c.state.as_str(), c.dest_uid, c.dest_validity), ("snoozed", Some(42), Some(9)));
        assert_eq!(s.count_for_account("a@x.org"), 1);
    }

    #[test]
    fn a_crash_between_reserve_and_commit_surfaces_as_failed_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("workspace.db");
        {
            let s = SnoozeStore::open(&p).unwrap();
            s.reserve(new(1, 5_000, Mode::Moved), 1_000).unwrap();
        }
        let s = SnoozeStore::open(&p).unwrap();
        let l = s.list().unwrap();
        assert_eq!((l.len(), l[0].state.as_str()), (1, "failed"));
    }

    #[test]
    fn outcome_text_is_classified_not_swallowed() {
        assert!(matches!(classify_outcome(Some("The message was not found in the folder.")), Ok(Step::NotFound)));
        assert!(matches!(classify_outcome(Some("The message is no longer in the folder.")), Ok(Step::NotFound)));
        assert!(matches!(classify_outcome(Some("More than one message has that Message-ID; not touching any of them.")), Err(WakeError::Permanent(_))));
        assert!(matches!(classify_outcome(Some("connection reset")), Err(WakeError::Transient(_))));
        assert!(matches!(classify_outcome(None), Err(WakeError::Transient(_))));
    }

    #[test]
    fn hidden_set_is_per_folder_and_validity() {
        let s = SnoozeStore::open_in_memory().unwrap();
        s.add(new(1, 10_000, Mode::Hidden), 0).unwrap();
        s.add(new(2, 10_000, Mode::Moved), 0).unwrap();
        let h = s.hidden("a@x.org", "INBOX");
        assert!(h.contains(&(1, 7)) && !h.contains(&(2, 7)) && !h.contains(&(1, 8)));
        assert!(s.hidden("a@x.org", "Other").is_empty());
        assert!(s.hidden("b@x.org", "INBOX").is_empty());
    }

    #[test]
    fn validates_times_and_account_removal() {
        let s = SnoozeStore::open_in_memory().unwrap();
        assert!(s.add(new(1, 500, Mode::Hidden), 1_000).is_err());
        assert!(s.add(new(1, 1_000 + 400 * 86_400_000, Mode::Hidden), 1_000).is_err());
        s.add(new(1, 5_000, Mode::Hidden), 1_000).unwrap();
        s.delete_for_account("a@x.org").unwrap();
        assert!(s.list().unwrap().is_empty());
    }

    #[test]
    fn finds_the_snoozed_folder_case_insensitively() {
        let f = |n: &str, sel: bool| mailcore::Folder { name: n.into(), delimiter: None, role: FolderRole::Other, selectable: sel };
        assert_eq!(snoozed_folder(&[f("INBOX", true), f("snoozed", true)]), Some("snoozed".into()));
        assert_eq!(snoozed_folder(&[f("Snoozed", false)]), None);
        let dotted = mailcore::Folder { name: "INBOX.Snoozed".into(), delimiter: Some(".".into()), role: FolderRole::Other, selectable: true };
        assert_eq!(snoozed_folder(&[dotted]), Some("INBOX.Snoozed".into()));
        assert_eq!(snoozed_folder(&[f("Not Snoozed", true)]), None);
        assert_eq!(snoozed_folder(&[f("INBOX", true)]), None);
    }
}
