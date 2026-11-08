//! ai.db storage (task A2).
//!
//! Own SQLite file, separate from mail data. NO secrets are ever stored here: API keys live in the
//! macOS Keychain; the DB only records `has_key` and `key_host` (the base-URL host the key was
//! saved for, so a host change can be detected and the key re-entered).
//!
//! `Store` wraps a single `rusqlite::Connection` (not `Sync`); callers share it behind a Mutex.
//! All times are unix seconds passed in by the caller (testable with a fake clock).

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ai.db was written by a newer MiraMail (schema {found}, this build supports {supported})")]
    NewerSchema { found: i64, supported: i64 },
    #[error("profile base URL is invalid")]
    InvalidProfile,
    #[error("not found")]
    NotFound,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Ordered migrations; `user_version` = number applied.
const MIGRATIONS: &[&str] = &[
    // v1
    "
    CREATE TABLE provider_profiles (
        id          TEXT PRIMARY KEY,
        name        TEXT NOT NULL,
        dialect     TEXT NOT NULL,
        base_url    TEXT NOT NULL,
        model       TEXT NOT NULL DEFAULT '',
        key_host    TEXT,
        has_key     INTEGER NOT NULL DEFAULT 0,
        extra_json  TEXT NOT NULL DEFAULT '{}',
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );
    CREATE TABLE settings (
        name   TEXT PRIMARY KEY,
        value  TEXT NOT NULL
    );
    CREATE TABLE scopes (
        account  TEXT NOT NULL,
        folder   TEXT NOT NULL,
        allowed  INTEGER NOT NULL,
        PRIMARY KEY (account, folder)
    );
    CREATE TABLE conversations (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        title         TEXT NOT NULL DEFAULT '',
        profile_id    TEXT,
        endpoint_host TEXT NOT NULL DEFAULT '',
        model         TEXT NOT NULL DEFAULT '',
        consent_json  TEXT NOT NULL DEFAULT '{}',
        accounts_json TEXT NOT NULL DEFAULT '[]',
        created_at    INTEGER NOT NULL,
        updated_at    INTEGER NOT NULL
    );
    CREATE TABLE chat_messages (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        conv_id     INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        seq         INTEGER NOT NULL,
        role        TEXT NOT NULL,
        parts_json  TEXT NOT NULL,
        created_at  INTEGER NOT NULL,
        UNIQUE (conv_id, seq)
    );
    CREATE TABLE conv_refs (
        conv_id      INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        ref          TEXT NOT NULL,
        account      TEXT NOT NULL,
        folder       TEXT NOT NULL,
        uid          INTEGER NOT NULL,
        uidvalidity  INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (conv_id, ref)
    );
    CREATE INDEX conv_refs_account ON conv_refs(account);
    CREATE TABLE egress (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conv_id         INTEGER REFERENCES conversations(id) ON DELETE CASCADE,
        ts              INTEGER NOT NULL,
        profile_id      TEXT,
        endpoint_host   TEXT NOT NULL DEFAULT '',
        model           TEXT NOT NULL DEFAULT '',
        request_json    TEXT NOT NULL,
        bytes           INTEGER NOT NULL DEFAULT 0,
        est_tokens      INTEGER NOT NULL DEFAULT 0,
        items_json      TEXT NOT NULL DEFAULT '[]',
        redactions_json TEXT NOT NULL DEFAULT '{}'
    );
    CREATE INDEX egress_ts ON egress(ts);
    CREATE TABLE usage_daily (
        day         TEXT NOT NULL,
        profile_id  TEXT NOT NULL,
        model       TEXT NOT NULL,
        requests    INTEGER NOT NULL DEFAULT 0,
        tokens_in   INTEGER NOT NULL DEFAULT 0,
        tokens_out  INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (day, profile_id, model)
    );
    ",
    // v2: per-conversation history segment (start of the live segment + local summary of what came before)
    "
    CREATE TABLE IF NOT EXISTS conv_segments (
        conv_id    INTEGER PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
        start_seq  INTEGER NOT NULL DEFAULT 0,
        summary    TEXT NOT NULL DEFAULT '',
        used_total INTEGER NOT NULL DEFAULT 0
    );
    ",
    // v3 (P1 tools): request round within a turn (0 = first request of the turn) and the record of
    // tool calls shown as chips in the UI.
    "
    ALTER TABLE egress ADD COLUMN round INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE tool_runs (
        conv_id  INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        call_id  TEXT NOT NULL,
        name     TEXT NOT NULL,
        label    TEXT NOT NULL DEFAULT '',
        summary  TEXT NOT NULL DEFAULT '',
        ok       INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY (conv_id, call_id)
    );
    ",
    // v4 (P2 proposals): approval queue + action log with the data an undo needs.
    "
    CREATE TABLE proposals (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        conv_id     INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        kind        TEXT NOT NULL,
        state       TEXT NOT NULL,
        params_json TEXT NOT NULL DEFAULT '{}',
        title       TEXT NOT NULL,
        reason      TEXT NOT NULL DEFAULT '',
        warnings_json TEXT NOT NULL DEFAULT '[]',
        error       TEXT NOT NULL DEFAULT '',
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        expires_at  INTEGER NOT NULL
    );
    CREATE INDEX proposals_conv ON proposals(conv_id);
    CREATE TABLE proposal_items (
        proposal_id INTEGER NOT NULL REFERENCES proposals(id) ON DELETE CASCADE,
        idx         INTEGER NOT NULL,
        account     TEXT NOT NULL,
        folder      TEXT NOT NULL,
        uid         INTEGER NOT NULL,
        uidvalidity INTEGER NOT NULL,
        sender      TEXT NOT NULL DEFAULT '',
        subject     TEXT NOT NULL DEFAULT '',
        seen        INTEGER NOT NULL DEFAULT 0,
        flagged     INTEGER NOT NULL DEFAULT 0,
        to_folder   TEXT NOT NULL DEFAULT '',
        selected    INTEGER NOT NULL DEFAULT 1,
        status      TEXT NOT NULL DEFAULT 'pending',
        error       TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (proposal_id, idx)
    );
    CREATE TABLE action_log (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        proposal_id INTEGER NOT NULL REFERENCES proposals(id) ON DELETE CASCADE,
        item_idx    INTEGER NOT NULL DEFAULT -1,
        ts          INTEGER NOT NULL,
        op          TEXT NOT NULL,
        account     TEXT NOT NULL,
        src_folder  TEXT NOT NULL DEFAULT '',
        src_uid     INTEGER,
        src_validity INTEGER,
        dest_folder TEXT NOT NULL DEFAULT '',
        dest_uid    INTEGER,
        dest_validity INTEGER,
        message_id  TEXT,
        flag        TEXT NOT NULL DEFAULT '',
        flag_old    INTEGER,
        flag_new    INTEGER,
        outcome     TEXT NOT NULL,
        error       TEXT NOT NULL DEFAULT '',
        undone      INTEGER NOT NULL DEFAULT 0,
        undo_error  TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX action_log_proposal ON action_log(proposal_id);
    ",
    // v5 (P3 drafts): drafts the assistant wrote. Recipients, subject and quote are computed by the app;
    // `state` is only proposed / opened / dismissed (sending happens in Compose, never here).
    "
    CREATE TABLE ai_drafts (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        conv_id       INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        kind          TEXT NOT NULL,
        state         TEXT NOT NULL,
        title         TEXT NOT NULL,
        account       TEXT NOT NULL,
        to_json       TEXT NOT NULL DEFAULT '[]',
        cc_json       TEXT NOT NULL DEFAULT '[]',
        subject       TEXT NOT NULL DEFAULT '',
        body          TEXT NOT NULL DEFAULT '',
        quote         TEXT NOT NULL DEFAULT '',
        in_reply_to   TEXT,
        refs          TEXT NOT NULL DEFAULT '',
        reason        TEXT NOT NULL DEFAULT '',
        warnings_json TEXT NOT NULL DEFAULT '[]',
        new_json      TEXT NOT NULL DEFAULT '[]',
        created_at    INTEGER NOT NULL,
        updated_at    INTEGER NOT NULL
    );
    CREATE INDEX ai_drafts_conv ON ai_drafts(conv_id);
    ",
    // v6 (P4 automation): local rules, their per-folder cursors and run history, and the opt-in template
    // auto-reply with its audit log. Rule runs execute through the proposal executor, which needs a
    // conversation row: id 0 is a reserved, hidden "system" conversation (never listed, never purged).
    "
    INSERT OR IGNORE INTO conversations (id, title, profile_id, endpoint_host, model, consent_json, accounts_json, created_at, updated_at)
    VALUES (0, 'Rules (system)', NULL, '', '', '{}', '[]', 0, 0);
    CREATE TABLE rules (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        account     TEXT NOT NULL,
        name        TEXT NOT NULL,
        enabled     INTEGER NOT NULL DEFAULT 0,
        spec_json   TEXT NOT NULL,
        origin      TEXT NOT NULL DEFAULT 'user',
        conv_id     INTEGER,
        reason      TEXT NOT NULL DEFAULT '',
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        last_run_at INTEGER
    );
    CREATE TABLE rule_cursor (
        rule_id     INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
        account     TEXT NOT NULL,
        folder      TEXT NOT NULL,
        uidvalidity INTEGER NOT NULL,
        last_uid    INTEGER NOT NULL,
        PRIMARY KEY (rule_id, account, folder)
    );
    CREATE TABLE rule_runs (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        rule_id      INTEGER NOT NULL,
        rule_name    TEXT NOT NULL,
        account      TEXT NOT NULL,
        folder       TEXT NOT NULL,
        ts           INTEGER NOT NULL,
        matched      INTEGER NOT NULL DEFAULT 0,
        summary      TEXT NOT NULL DEFAULT '',
        proposal_ids TEXT NOT NULL DEFAULT '[]'
    );
    CREATE INDEX rule_runs_ts ON rule_runs(ts);
    CREATE TABLE autoreply_cfg (
        account  TEXT PRIMARY KEY,
        cfg_json TEXT NOT NULL
    );
    CREATE TABLE autoreply_cursor (
        account     TEXT PRIMARY KEY,
        uidvalidity INTEGER NOT NULL,
        last_uid    INTEGER NOT NULL
    );
    CREATE TABLE autoreply_log (
        id      INTEGER PRIMARY KEY AUTOINCREMENT,
        account TEXT NOT NULL,
        ts      INTEGER NOT NULL,
        sender  TEXT NOT NULL DEFAULT '',
        subject TEXT NOT NULL DEFAULT '',
        outcome TEXT NOT NULL,
        reason  TEXT NOT NULL DEFAULT '',
        detail  TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX autoreply_log_account ON autoreply_log(account, ts);
    ",
    // v7 (review fixes): the second address (envelope sender) of an answered mail for the 7-day limit, and
    // a fallback ping-pong guard for rule-moved mail that has no Message-ID (fingerprint of From/Date/Subject/size).
    "
    ALTER TABLE autoreply_log ADD COLUMN sender2 TEXT NOT NULL DEFAULT '';
    CREATE TABLE rule_moved (
        account TEXT NOT NULL,
        fp      TEXT NOT NULL,
        run_id  INTEGER NOT NULL,
        ts      INTEGER NOT NULL
    );
    CREATE INDEX rule_moved_fp ON rule_moved(account, fp);
    ",
    // v8 (calendar tools): what an approved calendar proposal did, with the data its Undo needs. `snapshot` is
    // opaque JSON written by the app (the event before a change / cancellation).
    "
    CREATE TABLE cal_log (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        proposal_id   INTEGER NOT NULL REFERENCES proposals(id) ON DELETE CASCADE,
        ts            INTEGER NOT NULL,
        op            TEXT NOT NULL,
        calendar_id   TEXT NOT NULL,
        uid           TEXT NOT NULL DEFAULT '',
        version_after TEXT NOT NULL DEFAULT '',
        snapshot      TEXT NOT NULL DEFAULT '',
        zoom_id       TEXT NOT NULL DEFAULT '',
        notified      INTEGER NOT NULL DEFAULT 0,
        outcome       TEXT NOT NULL,
        error         TEXT NOT NULL DEFAULT '',
        undone        INTEGER NOT NULL DEFAULT 0,
        undo_error    TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX cal_log_proposal ON cal_log(proposal_id);
    ",
    // v9 (local rules, colour label action): previous colour of every message a rule labelled, for per-run undo.
    "
    CREATE TABLE rule_colour_log (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id     INTEGER NOT NULL,
        account    TEXT NOT NULL,
        message_id TEXT NOT NULL,
        prev       INTEGER NOT NULL,
        new        INTEGER NOT NULL,
        undone     INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX rule_colour_log_run ON rule_colour_log(run_id);
    ",
    // v10 (created folders join the conversation): folders MIRA created in a conversation after the user approved
    // the create-folder card. They are added to THAT conversation's allowed set only.
    "
    CREATE TABLE conv_folders (
        conv_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        account TEXT NOT NULL,
        folder  TEXT NOT NULL,
        PRIMARY KEY (conv_id, account, folder)
    );
    CREATE INDEX conv_folders_account ON conv_folders(account);
    ",
];

pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

const DAY: i64 = 86_400;

// ------------------------------------------------------------------------------------------
// Types
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// Dialect tag, e.g. "openai" / "anthropic" (owned by the provider module).
    pub dialect: String,
    pub base_url: String,
    pub model: String,
    /// Origin (`scheme://host:port`) the saved key is bound to; `None` when no key was saved.
    pub key_host: Option<String>,
    pub has_key: bool,
    /// Dialect-specific options as JSON (e.g. anthropic-version).
    pub extra_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What the UI edits. `has_key`/`key_host` are NOT editable here; see `mark_key_saved`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInput {
    pub id: String,
    pub name: String,
    pub dialect: String,
    pub base_url: String,
    pub model: String,
    pub extra_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeSettings {
    /// `None` = keep egress logs forever.
    pub egress_retention_days: Option<u32>,
    /// `None` = keep conversations forever.
    pub conversation_retention_days: Option<u32>,
    pub purge_on_account_removal: bool,
}

impl Default for PurgeSettings {
    fn default() -> Self {
        Self { egress_retention_days: Some(30), conversation_retention_days: None, purge_on_account_removal: true }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeReport {
    pub egress_deleted: usize,
    pub conversations_deleted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewConversation {
    pub title: String,
    pub profile_id: Option<String>,
    pub endpoint_host: String,
    pub model: String,
    pub consent_json: String,
    /// Accounts in the consent scope (used by `purge_account`).
    pub accounts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub id: i64,
    pub title: String,
    pub profile_id: Option<String>,
    pub endpoint_host: String,
    pub model: String,
    pub consent_json: String,
    pub accounts: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: i64,
    pub conv_id: i64,
    pub seq: i64,
    pub role: String,
    pub parts_json: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvRef {
    pub reference: String,
    pub account: String,
    pub folder: String,
    pub uid: i64,
    pub uidvalidity: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEgress {
    /// Request index within the user turn (0 = first; tool rounds count up).
    pub round: i64,
    pub conv_id: Option<i64>,
    pub profile_id: Option<String>,
    pub endpoint_host: String,
    pub model: String,
    /// Request body only. Headers/auth must never be passed; top-level auth-like keys are removed anyway.
    pub request_json: String,
    pub bytes: i64,
    pub est_tokens: i64,
    pub items_json: String,
    pub redactions_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRecord {
    pub round: i64,
    pub id: i64,
    pub conv_id: Option<i64>,
    pub ts: i64,
    pub profile_id: Option<String>,
    pub endpoint_host: String,
    pub model: String,
    pub request_json: String,
    pub bytes: i64,
    pub est_tokens: i64,
    pub items_json: String,
    pub redactions_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRun {
    pub call_id: String,
    pub name: String,
    /// Short description of the call, derived from validated arguments (shown in the UI chip).
    pub label: String,
    /// Short outcome ("12 results"), written by the app (never mail text).
    pub summary: String,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRow {
    pub day: String,
    pub profile_id: String,
    pub model: String,
    pub requests: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
}

/// Does the saved key still belong to this profile's endpoint? If the base-URL origin changed since
/// the key was saved, the key must be re-entered (and must not be sent).
pub fn needs_key_reentry(p: &Profile) -> bool {
    if !p.has_key {
        return false;
    }
    match (&p.key_host, crate::http::validate_base_url(&p.base_url)) {
        (Some(saved), Ok(b)) => !saved.eq_ignore_ascii_case(&b.origin()),
        _ => true,
    }
}

/// `YYYY-MM-DD` (UTC) for unix seconds.
pub fn day_string(ts: i64) -> String {
    let z = ts.div_euclid(DAY) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Remove auth-looking top-level keys from a request JSON (defence in depth), re-serialising.
/// Non-JSON input is returned unchanged.
fn scrub_request_json(s: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(serde_json::Value::Object(mut m)) => {
            m.retain(|k, _| {
                let k = k.to_ascii_lowercase();
                !(k == "authorization" || k == "x-api-key" || k == "api_key" || k == "headers")
            });
            serde_json::Value::Object(m).to_string()
        }
        _ => s.to_string(),
    }
}

// ------------------------------------------------------------------------------------------
// Store
// ------------------------------------------------------------------------------------------

pub struct Store {
    conn: Connection,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store { .. }")
    }
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        let conn = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.pragma_update(None, "foreign_keys", true)?;
        let mut s = Store { conn };
        s.migrate()?;
        Ok(s)
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    fn migrate(&mut self) -> Result<()> {
        let found = self.schema_version()?;
        if found > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema { found, supported: SCHEMA_VERSION });
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(found as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Connection for the proposals module (same crate only).
    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Raw connection for read-only inspection in tests.
    #[doc(hidden)]
    pub fn raw(&self) -> &Connection {
        &self.conn
    }

    // ---- profiles ---------------------------------------------------------------------------

    fn row_profile(r: &rusqlite::Row<'_>) -> rusqlite::Result<Profile> {
        Ok(Profile {
            id: r.get(0)?,
            name: r.get(1)?,
            dialect: r.get(2)?,
            base_url: r.get(3)?,
            model: r.get(4)?,
            key_host: r.get(5)?,
            has_key: r.get::<_, i64>(6)? != 0,
            extra_json: r.get(7)?,
            created_at: r.get(8)?,
            updated_at: r.get(9)?,
        })
    }
    const PROFILE_COLS: &'static str =
        "id, name, dialect, base_url, model, key_host, has_key, extra_json, created_at, updated_at";

    /// Insert or update. Never touches `has_key` / `key_host`: after a base-URL host change the
    /// profile reports `needs_key_reentry` until the caller stores a new key and calls `mark_key_saved`.
    pub fn save_profile(&mut self, p: &ProfileInput, now: i64) -> Result<Profile> {
        self.conn.execute(
            "INSERT INTO provider_profiles (id, name, dialect, base_url, model, extra_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(id) DO UPDATE SET name=?2, dialect=?3, base_url=?4, model=?5, extra_json=?6, updated_at=?7",
            params![p.id, p.name, p.dialect, p.base_url, p.model, p.extra_json, now],
        )?;
        self.get_profile(&p.id)?.ok_or(StoreError::NotFound)
    }

    pub fn get_profile(&self, id: &str) -> Result<Option<Profile>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {} FROM provider_profiles WHERE id = ?1", Self::PROFILE_COLS),
                params![id],
                Self::row_profile,
            )
            .optional()?)
    }

    pub fn list_profiles(&self) -> Result<Vec<Profile>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {} FROM provider_profiles ORDER BY created_at, id", Self::PROFILE_COLS))?;
        let rows = st.query_map([], Self::row_profile)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record that a key for this profile is now in the Keychain, bound to the profile's current
    /// base-URL host. Returns the bound host.
    pub fn mark_key_saved(&mut self, id: &str, now: i64) -> Result<String> {
        let p = self.get_profile(id)?.ok_or(StoreError::NotFound)?;
        let host = crate::http::validate_base_url(&p.base_url)
            .map(|b| b.origin())
            .map_err(|_| StoreError::InvalidProfile)?;
        self.conn.execute(
            "UPDATE provider_profiles SET has_key=1, key_host=?2, updated_at=?3 WHERE id=?1",
            params![id, host, now],
        )?;
        Ok(host)
    }

    /// Record that the Keychain entry was removed.
    pub fn mark_key_cleared(&mut self, id: &str, now: i64) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE provider_profiles SET has_key=0, key_host=NULL, updated_at=?2 WHERE id=?1",
            params![id, now],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Delete a profile. Conversations keep their history (profile_id is just a label).
    pub fn delete_profile(&mut self, id: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM provider_profiles WHERE id=?1", params![id])? > 0)
    }

    // ---- settings ---------------------------------------------------------------------------

    pub fn get_setting(&self, name: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE name=?1", params![name], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&mut self, name: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (name, value) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET value=?2",
            params![name, value],
        )?;
        Ok(())
    }

    pub fn purge_settings(&self) -> Result<PurgeSettings> {
        match self.get_setting("purge")? {
            Some(s) => Ok(serde_json::from_str(&s).unwrap_or_default()),
            None => Ok(PurgeSettings::default()),
        }
    }

    pub fn set_purge_settings(&mut self, p: &PurgeSettings) -> Result<()> {
        self.set_setting("purge", &serde_json::to_string(p)?)
    }

    // ---- scopes -----------------------------------------------------------------------------

    pub fn set_scope(&mut self, account: &str, folder: &str, allowed: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO scopes (account, folder, allowed) VALUES (?1, ?2, ?3)
             ON CONFLICT(account, folder) DO UPDATE SET allowed=?3",
            params![account, folder, allowed as i64],
        )?;
        Ok(())
    }

    /// Remember a folder created by an approved create-folder card as part of the conversation's scope.
    pub fn add_conv_folder(&mut self, conv_id: i64, account: &str, folder: &str) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO conv_folders (conv_id, account, folder) VALUES (?1, ?2, ?3)", params![conv_id, account, folder])?;
        Ok(())
    }

    /// Folders added to this conversation's scope by approved create-folder cards, as `(account, folder)`.
    pub fn list_conv_folders(&self, conv_id: i64) -> Result<Vec<(String, String)>> {
        let mut st = self.conn.prepare("SELECT account, folder FROM conv_folders WHERE conv_id=?1 ORDER BY rowid")?;
        let rows = st.query_map(params![conv_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Replace a conversation's consent (user action "Edit permissions") in ONE transaction: the new
    /// `consent_json`, the account list (the caller passes old + new accounts so a later account purge still
    /// finds content that was shared earlier), the folders MIRA created earlier (`conv_folders`, now part of
    /// the saved consent) cleared, and a visible `notice` transcript row. Earlier messages, refs and proposals
    /// are not touched. Returns the notice's `seq`.
    pub fn update_consent(&mut self, conv_id: i64, consent_json: &str, accounts: &[String], notice_parts_json: &str, now: i64) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE conversations SET consent_json=?2, accounts_json=?3, updated_at=?4 WHERE id=?1 AND id > 0",
            params![conv_id, consent_json, serde_json::to_string(accounts)?, now],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        tx.execute("DELETE FROM conv_folders WHERE conv_id=?1", params![conv_id])?;
        let seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM chat_messages WHERE conv_id=?1", params![conv_id], |r| r.get(0))?;
        tx.execute(
            "INSERT INTO chat_messages (conv_id, seq, role, parts_json, created_at) VALUES (?1, ?2, 'notice', ?3, ?4)",
            params![conv_id, seq, notice_parts_json, now],
        )?;
        tx.commit()?;
        Ok(seq)
    }

    pub fn list_scopes(&self) -> Result<Vec<(String, String, bool)>> {
        let mut st = self.conn.prepare("SELECT account, folder, allowed FROM scopes ORDER BY account, folder")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- conversations ----------------------------------------------------------------------

    fn row_conv(r: &rusqlite::Row<'_>) -> rusqlite::Result<Conversation> {
        let accounts: String = r.get(6)?;
        Ok(Conversation {
            id: r.get(0)?,
            title: r.get(1)?,
            profile_id: r.get(2)?,
            endpoint_host: r.get(3)?,
            model: r.get(4)?,
            consent_json: r.get(5)?,
            accounts: serde_json::from_str(&accounts).unwrap_or_default(),
            created_at: r.get(7)?,
            updated_at: r.get(8)?,
        })
    }
    const CONV_COLS: &'static str =
        "id, title, profile_id, endpoint_host, model, consent_json, accounts_json, created_at, updated_at";

    pub fn create_conversation(&mut self, c: &NewConversation, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO conversations (title, profile_id, endpoint_host, model, consent_json, accounts_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![c.title, c.profile_id, c.endpoint_host, c.model, c.consent_json, serde_json::to_string(&c.accounts)?, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_conversation(&self, id: i64) -> Result<Option<Conversation>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {} FROM conversations WHERE id=?1", Self::CONV_COLS), params![id], Self::row_conv)
            .optional()?)
    }

    /// Newest first.
    pub fn list_conversations(&self) -> Result<Vec<Conversation>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {} FROM conversations WHERE id > 0 ORDER BY updated_at DESC, id DESC", Self::CONV_COLS))?;
        let rows = st.query_map([], Self::row_conv)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_conversation(&mut self, id: i64, title: &str) -> Result<()> {
        let n = self.conn.execute("UPDATE conversations SET title=?2 WHERE id=?1", params![id, title])?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Cascades to messages, refs and egress rows of the conversation.
    pub fn delete_conversation(&mut self, id: i64) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM conversations WHERE id=?1 AND id > 0", params![id])? > 0)
    }

    /// Append a message; `seq` is assigned atomically. Returns the new seq.
    pub fn append_message(&mut self, conv_id: i64, role: &str, parts_json: &str, now: i64) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM chat_messages WHERE conv_id=?1",
            params![conv_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO chat_messages (conv_id, seq, role, parts_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![conv_id, seq, role, parts_json, now],
        )?;
        tx.execute("UPDATE conversations SET updated_at=?2 WHERE id=?1", params![conv_id, now])?;
        tx.commit()?;
        Ok(seq)
    }

    pub fn list_messages(&self, conv_id: i64) -> Result<Vec<StoredMessage>> {
        let mut st = self.conn.prepare(
            "SELECT id, conv_id, seq, role, parts_json, created_at FROM chat_messages WHERE conv_id=?1 ORDER BY seq",
        )?;
        let rows = st.query_map(params![conv_id], |r| {
            Ok(StoredMessage {
                id: r.get(0)?,
                conv_id: r.get(1)?,
                seq: r.get(2)?,
                role: r.get(3)?,
                parts_json: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn put_ref(&mut self, conv_id: i64, r: &ConvRef) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conv_refs (conv_id, ref, account, folder, uid, uidvalidity) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(conv_id, ref) DO UPDATE SET account=?3, folder=?4, uid=?5, uidvalidity=?6",
            params![conv_id, r.reference, r.account, r.folder, r.uid, r.uidvalidity],
        )?;
        Ok(())
    }

    pub fn get_ref(&self, conv_id: i64, reference: &str) -> Result<Option<ConvRef>> {
        Ok(self
            .conn
            .query_row(
                "SELECT ref, account, folder, uid, uidvalidity FROM conv_refs WHERE conv_id=?1 AND ref=?2",
                params![conv_id, reference],
                |r| {
                    Ok(ConvRef { reference: r.get(0)?, account: r.get(1)?, folder: r.get(2)?, uid: r.get(3)?, uidvalidity: r.get(4)? })
                },
            )
            .optional()?)
    }

    /// All refs of a conversation (the engine loads them once per turn).
    pub fn list_refs(&self, conv_id: i64) -> Result<Vec<ConvRef>> {
        let mut st = self.conn.prepare("SELECT ref, account, folder, uid, uidvalidity FROM conv_refs WHERE conv_id=?1")?;
        let rows = st.query_map(params![conv_id], |r| {
            Ok(ConvRef { reference: r.get(0)?, account: r.get(1)?, folder: r.get(2)?, uid: r.get(3)?, uidvalidity: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn put_tool_run(&mut self, conv_id: i64, t: &ToolRun) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tool_runs (conv_id, call_id, name, label, summary, ok) VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(conv_id, call_id) DO UPDATE SET name=?3, label=?4, summary=?5, ok=?6",
            params![conv_id, t.call_id, t.name, t.label, t.summary, t.ok],
        )?;
        Ok(())
    }

    pub fn list_tool_runs(&self, conv_id: i64) -> Result<Vec<ToolRun>> {
        let mut st = self.conn.prepare("SELECT call_id, name, label, summary, ok FROM tool_runs WHERE conv_id=?1")?;
        let rows = st.query_map(params![conv_id], |r| {
            Ok(ToolRun { call_id: r.get(0)?, name: r.get(1)?, label: r.get(2)?, summary: r.get(3)?, ok: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- egress -----------------------------------------------------------------------------

    pub fn record_egress(&mut self, e: &NewEgress, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO egress (conv_id, ts, profile_id, endpoint_host, model, request_json, bytes, est_tokens, items_json, redactions_json, round)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                e.conv_id,
                now,
                e.profile_id,
                e.endpoint_host,
                e.model,
                scrub_request_json(&e.request_json),
                e.bytes,
                e.est_tokens,
                e.items_json,
                e.redactions_json,
                e.round
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Newest first; optionally for one conversation.
    pub fn list_egress(&self, conv_id: Option<i64>, limit: u32) -> Result<Vec<EgressRecord>> {
        let mut st = self.conn.prepare(
            "SELECT id, conv_id, ts, profile_id, endpoint_host, model, request_json, bytes, est_tokens, items_json, redactions_json, round
             FROM egress WHERE (?1 IS NULL OR conv_id = ?1) ORDER BY ts DESC, id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![conv_id, limit], |r| {
            Ok(EgressRecord {
                id: r.get(0)?,
                conv_id: r.get(1)?,
                ts: r.get(2)?,
                profile_id: r.get(3)?,
                endpoint_host: r.get(4)?,
                model: r.get(5)?,
                request_json: r.get(6)?,
                bytes: r.get(7)?,
                est_tokens: r.get(8)?,
                items_json: r.get(9)?,
                redactions_json: r.get(10)?,
                round: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- history segments -------------------------------------------------------------------

    /// `(start_seq, summary)` of the conversation's live segment; `None` = the whole history.
    pub fn get_segment(&self, conv_id: i64) -> Result<Option<(i64, String)>> {
        Ok(self
            .conn
            .query_row("SELECT start_seq, summary FROM conv_segments WHERE conv_id=?1", params![conv_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?)
    }

    pub fn set_segment(&mut self, conv_id: i64, start_seq: i64, summary: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conv_segments (conv_id, start_seq, summary) VALUES (?1,?2,?3)
             ON CONFLICT(conv_id) DO UPDATE SET start_seq=?2, summary=?3",
            params![conv_id, start_seq, summary],
        )?;
        Ok(())
    }

    /// Tokens (in + out) counted for the conversation so far (survives egress-log purges).
    pub fn conv_tokens(&self, conv_id: i64) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT used_total FROM conv_segments WHERE conv_id=?1", params![conv_id], |r| r.get(0))
            .optional()?
            .unwrap_or(0))
    }

    pub fn add_conv_tokens(&mut self, conv_id: i64, n: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conv_segments (conv_id, used_total) VALUES (?1,?2)
             ON CONFLICT(conv_id) DO UPDATE SET used_total = used_total + ?2",
            params![conv_id, n],
        )?;
        Ok(())
    }

    /// Tokens (in + out, all profiles) counted for the UTC day of `ts`.
    pub fn tokens_on_day(&self, ts: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(tokens_in + tokens_out),0) FROM usage_daily WHERE day=?1",
            params![day_string(ts)],
            |r| r.get(0),
        )?)
    }

    // ---- usage ------------------------------------------------------------------------------

    pub fn add_usage(&mut self, now: i64, profile_id: &str, model: &str, tokens_in: i64, tokens_out: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO usage_daily (day, profile_id, model, requests, tokens_in, tokens_out) VALUES (?1,?2,?3,1,?4,?5)
             ON CONFLICT(day, profile_id, model) DO UPDATE SET
               requests = requests + 1, tokens_in = tokens_in + ?4, tokens_out = tokens_out + ?5",
            params![day_string(now), profile_id, model, tokens_in, tokens_out],
        )?;
        Ok(())
    }

    /// Rows with `day >= since_day` (YYYY-MM-DD), oldest first.
    pub fn list_usage(&self, since_day: &str) -> Result<Vec<UsageRow>> {
        let mut st = self.conn.prepare(
            "SELECT day, profile_id, model, requests, tokens_in, tokens_out FROM usage_daily WHERE day >= ?1 ORDER BY day, profile_id, model",
        )?;
        let rows = st.query_map(params![since_day], |r| {
            Ok(UsageRow {
                day: r.get(0)?,
                profile_id: r.get(1)?,
                model: r.get(2)?,
                requests: r.get(3)?,
                tokens_in: r.get(4)?,
                tokens_out: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- purge ------------------------------------------------------------------------------

    /// Apply retention. Usage counters are aggregate numbers only and are kept.
    pub fn purge_now(&mut self, p: &PurgeSettings, now: i64) -> Result<PurgeReport> {
        let tx = self.conn.transaction()?;
        let mut rep = PurgeReport::default();
        if let Some(days) = p.egress_retention_days {
            let cutoff = now - i64::from(days) * DAY;
            rep.egress_deleted = tx.execute("DELETE FROM egress WHERE ts < ?1", params![cutoff])?;
        }
        if let Some(days) = p.conversation_retention_days {
            let cutoff = now - i64::from(days) * DAY;
            rep.conversations_deleted = tx.execute("DELETE FROM conversations WHERE id > 0 AND updated_at < ?1", params![cutoff])?;
        }
        tx.commit()?;
        Ok(rep)
    }

    /// Remove everything tied to a removed mail account: its scopes, refs, and every conversation
    /// that was scoped to it or holds a ref into it (cascading to messages and egress).
    pub fn purge_account(&mut self, account: &str) -> Result<PurgeReport> {
        let tx: Transaction<'_> = self.conn.transaction()?;
        tx.execute("DELETE FROM scopes WHERE account=?1", params![account])?;
        tx.execute("DELETE FROM conv_folders WHERE account=?1", params![account])?;
        let n = tx.execute(
            "DELETE FROM conversations WHERE id > 0 AND (
               EXISTS (SELECT 1 FROM json_each(conversations.accounts_json) WHERE json_each.value = ?1)
               OR id IN (SELECT conv_id FROM conv_refs WHERE account = ?1))",
            params![account],
        )?;
        tx.commit()?;
        Ok(PurgeReport { egress_deleted: 0, conversations_deleted: n })
    }

    /// Delete all conversations and egress logs (the "purge now" button with everything selected).
    pub fn purge_everything(&mut self) -> Result<PurgeReport> {
        let tx = self.conn.transaction()?;
        let egress_deleted = tx.execute("DELETE FROM egress", [])?;
        let conversations_deleted = tx.execute("DELETE FROM conversations WHERE id > 0", [])?;
        tx.commit()?;
        Ok(PurgeReport { egress_deleted, conversations_deleted })
    }
}
