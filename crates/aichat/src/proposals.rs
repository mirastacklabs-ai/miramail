//! Proposals: the approval queue for changes the assistant suggests (AI chat P2).
//!
//! The model can only ask for a change; it cannot make one. The flow is:
//!
//! 1. a `propose_*` tool validates references (scope + UIDVALIDITY), looks the messages up and stores
//!    a `Proposed` row. Title, count and warnings are computed HERE from the resolved messages
//!    ([`build_plan`]); the model's text is kept only as the "AI's reason";
//! 2. the user approves (optionally with some rows deselected) through [`Executor::approve`], the
//!    only code path that mutates mail. It re-resolves and re-validates every row at execution time
//!    (still in scope, UIDVALIDITY unchanged, message still there) and writes an action log;
//! 3. [`Executor::undo`] replays the log backwards and reports partial failure honestly.
//!
//! There is no send tool and no permanent-delete operation anywhere in this module: "trash" is a
//! move into the Trash folder. State changes are compare-and-set in SQL, so a proposal can be
//! approved (and therefore executed) at most once.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::engine::Clock;
use crate::envelope::clean_line;
use crate::store::{Result as StoreResult, Store};
use crate::tools::{BoxFut, MsgMeta, Scope};

/// Most message rows in one proposal.
pub const MAX_ITEMS: usize = 5000;
/// Rows per server round trip while executing / undoing.
pub const EXEC_CHUNK: usize = 200;
/// A proposal that is not decided within this time expires.
pub const TTL_SECS: i64 = 24 * 3600;
pub const MAX_REASON_CHARS: usize = 500;
/// Most proposals the model can queue in one user turn.
pub const MAX_PER_TURN: usize = 10;
const MAX_NAME_CHARS: usize = 100;

// ------------------------------------------------------------------------------------------
// Kinds, states, params
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Move,
    Archive,
    Trash,
    Flag,
    CreateFolder,
    /// Calendar (executed by [`crate::calexec::CalExecutor`], never by the mail [`Executor`]).
    EventCreate,
    EventChange,
    EventCancel,
    InviteResponse,
}

impl Kind {
    /// A calendar proposal: no message rows, payload in [`Params::cal`].
    pub fn is_calendar(self) -> bool {
        matches!(self, Kind::EventCreate | Kind::EventChange | Kind::EventCancel | Kind::InviteResponse)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Move => "move",
            Kind::Archive => "archive",
            Kind::Trash => "trash",
            Kind::Flag => "flag",
            Kind::CreateFolder => "create_folder",
            Kind::EventCreate => "event_create",
            Kind::EventChange => "event_change",
            Kind::EventCancel => "event_cancel",
            Kind::InviteResponse => "invite_response",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "move" => Kind::Move,
            "archive" => Kind::Archive,
            "trash" => Kind::Trash,
            "flag" => Kind::Flag,
            "create_folder" => Kind::CreateFolder,
            "event_create" => Kind::EventCreate,
            "event_change" => Kind::EventChange,
            "event_cancel" => Kind::EventCancel,
            "invite_response" => Kind::InviteResponse,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Proposed,
    Approved,
    Executing,
    Done,
    Failed,
    PartiallyDone,
    Rejected,
    Expired,
    Undone,
}

impl State {
    pub const ALL: [State; 9] = [State::Proposed, State::Approved, State::Executing, State::Done, State::Failed, State::PartiallyDone, State::Rejected, State::Expired, State::Undone];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Proposed => "proposed",
            State::Approved => "approved",
            State::Executing => "executing",
            State::Done => "done",
            State::Failed => "failed",
            State::PartiallyDone => "partially_done",
            State::Rejected => "rejected",
            State::Expired => "expired",
            State::Undone => "undone",
        }
    }
    pub fn parse(s: &str) -> Option<State> {
        State::ALL.into_iter().find(|x| x.as_str() == s)
    }
    /// The complete transition table. Everything not listed is illegal.
    pub fn can_become(self, to: State) -> bool {
        use State::*;
        matches!(
            (self, to),
            (Proposed, Approved)
                | (Proposed, Rejected)
                | (Proposed, Expired)
                | (Approved, Executing)
                | (Executing, Done)
                | (Executing, Failed)
                | (Executing, PartiallyDone)
                | (Done, Undone)
                | (PartiallyDone, Undone)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagOp {
    Read,
    Unread,
    Flagged,
    Unflagged,
}

impl FlagOp {
    pub fn parse(s: &str) -> Option<FlagOp> {
        Some(match s {
            "read" => FlagOp::Read,
            "unread" => FlagOp::Unread,
            "flagged" => FlagOp::Flagged,
            "unflagged" => FlagOp::Unflagged,
            _ => return None,
        })
    }
    /// `seen` or `flagged`.
    pub fn flag(self) -> &'static str {
        match self {
            FlagOp::Read | FlagOp::Unread => "seen",
            FlagOp::Flagged | FlagOp::Unflagged => "flagged",
        }
    }
    /// The value the flag gets.
    pub fn value(self) -> bool {
        matches!(self, FlagOp::Read | FlagOp::Flagged)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Params {
    /// Move: account and name of the destination folder.
    #[serde(default)]
    pub dest_account: Option<String>,
    #[serde(default)]
    pub dest_folder: Option<String>,
    /// Move: create the destination first if it does not exist.
    #[serde(default)]
    pub create_dest: bool,
    #[serde(default)]
    pub flag: Option<FlagOp>,
    /// CreateFolder: account and (validated) name.
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub new_folder: Option<String>,
    /// Calendar kinds: what approval does.
    #[serde(default)]
    pub cal: Option<crate::cal::CalParams>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewItem {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    pub sender: String,
    pub subject: String,
    pub seen: bool,
    pub flagged: bool,
    /// Where it goes (display only; Trash / Archive are looked up again at execution).
    pub to_folder: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProposal {
    pub conv_id: i64,
    pub kind: Kind,
    pub params: Params,
    pub title: String,
    pub reason: String,
    pub warnings: Vec<String>,
    pub items: Vec<NewItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStatus {
    Pending,
    Done,
    Failed,
    /// Not executed because revalidation refused it (stale, out of scope, gone) or an earlier step failed.
    Skipped,
    /// Nothing to do (already in the destination / already has the flag).
    Noop,
    Undone,
    UndoFailed,
    /// A move was in flight when the connection failed; the server may or may not have done it.
    Unknown,
}

impl ItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemStatus::Pending => "pending",
            ItemStatus::Done => "done",
            ItemStatus::Failed => "failed",
            ItemStatus::Skipped => "skipped",
            ItemStatus::Noop => "noop",
            ItemStatus::Undone => "undone",
            ItemStatus::UndoFailed => "undo_failed",
            ItemStatus::Unknown => "unknown",
        }
    }
    pub fn parse(s: &str) -> ItemStatus {
        match s {
            "done" => ItemStatus::Done,
            "failed" => ItemStatus::Failed,
            "skipped" => ItemStatus::Skipped,
            "noop" => ItemStatus::Noop,
            "undone" => ItemStatus::Undone,
            "undo_failed" => ItemStatus::UndoFailed,
            "unknown" => ItemStatus::Unknown,
            _ => ItemStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub idx: i64,
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    pub sender: String,
    pub subject: String,
    pub seen: bool,
    pub flagged: bool,
    pub to_folder: String,
    pub selected: bool,
    pub status: ItemStatus,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub id: i64,
    pub conv_id: i64,
    pub kind: Kind,
    pub state: State,
    pub params: Params,
    pub title: String,
    pub reason: String,
    pub warnings: Vec<String>,
    pub error: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: u32,
    pub selected: u32,
    pub done: u32,
    pub failed: u32,
    pub skipped: u32,
    pub noop: u32,
    pub undone: u32,
    pub undo_failed: u32,
    /// Moves whose result could not be confirmed.
    pub unknown: u32,
}

/// One executed (or refused) step. Everything an undo needs is here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub id: i64,
    pub proposal_id: i64,
    pub item_idx: i64,
    pub ts: i64,
    /// `move` | `flag` | `create_folder`
    pub op: String,
    pub account: String,
    pub src_folder: String,
    pub src_uid: Option<u32>,
    pub src_validity: Option<u32>,
    pub dest_folder: String,
    pub dest_uid: Option<u32>,
    pub dest_validity: Option<u32>,
    pub message_id: Option<String>,
    /// `seen` | `flagged` (flag ops)
    pub flag: String,
    pub flag_old: Option<bool>,
    pub flag_new: Option<bool>,
    /// `ok` | `ok-unverified` (a move whose result could not be confirmed; Undo still tries it by
    /// Message-ID) | `failed` | `skipped` | `exists` (create_folder: it was already there)
    pub outcome: String,
    pub error: String,
    /// 0 = not undone, 1 = undone, 2 = undo failed
    pub undone: i64,
    pub undo_error: String,
}

impl LogRow {
    fn new(proposal_id: i64, item_idx: i64, op: &str, account: &str) -> LogRow {
        LogRow {
            id: 0,
            proposal_id,
            item_idx,
            ts: 0,
            op: op.into(),
            account: account.into(),
            src_folder: String::new(),
            src_uid: None,
            src_validity: None,
            dest_folder: String::new(),
            dest_uid: None,
            dest_validity: None,
            message_id: None,
            flag: String::new(),
            flag_old: None,
            flag_new: None,
            outcome: "ok".into(),
            error: String::new(),
            undone: 0,
            undo_error: String::new(),
        }
    }
}

// ------------------------------------------------------------------------------------------
// Errors
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalError {
    NotFound,
    Illegal { from: State, to: State },
    Expired,
    NothingSelected,
    Busy,
    TooMany,
    NothingToUndo,
    Storage,
}

impl std::fmt::Display for ProposalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposalError::NotFound => f.write_str("That suggestion no longer exists."),
            ProposalError::Illegal { from, .. } => write!(f, "This suggestion can't be changed any more (it is {}).", from.as_str().replace('_', " ")),
            ProposalError::Expired => f.write_str("This suggestion expired. Ask the assistant again."),
            ProposalError::NothingSelected => f.write_str("Select at least one message."),
            ProposalError::NothingToUndo => f.write_str("Nothing in this suggestion can be undone (a new folder stays; delete it in your mail account if you don't want it)."),
            ProposalError::Busy => f.write_str("This suggestion is already being worked on."),
            ProposalError::TooMany => write!(f, "A suggestion can hold at most {MAX_ITEMS} messages."),
            ProposalError::Storage => f.write_str("MiraMail couldn't read or write its AI chat database."),
        }
    }
}
impl std::error::Error for ProposalError {}

fn db<E>(_: E) -> ProposalError {
    ProposalError::Storage
}

// ------------------------------------------------------------------------------------------
// Pure planning: title, warnings, validation (computed by the app, never by the model)
// ------------------------------------------------------------------------------------------

/// A new folder name: letters, digits, space, `-`, `_` (mailcore enforces the same rule on the server side).
pub fn validate_folder_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("The folder name is empty.".into());
    }
    if n.chars().count() > MAX_NAME_CHARS {
        return Err(format!("The folder name is longer than {MAX_NAME_CHARS} characters."));
    }
    if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_') || n.contains("  ") || n.eq_ignore_ascii_case("inbox") {
        return Err("Use only letters, digits, single spaces, '-' and '_' in a new folder name (not INBOX).".into());
    }
    Ok(n.to_string())
}

fn msgs(n: usize) -> String {
    if n == 1 {
        "1 message".to_string()
    } else {
        format!("{n} messages")
    }
}

/// Title shown on the approval card. `n` and `dest_label` are app-computed.
pub fn title_for(kind: Kind, params: &Params, n: usize, dest_label: &str) -> String {
    match kind {
        Kind::Move => format!("Move {} to {}", msgs(n), dest_label),
        Kind::Archive => format!("Archive {}", msgs(n)),
        Kind::Trash => format!("Move {} to Trash", msgs(n)),
        Kind::Flag => match params.flag {
            Some(FlagOp::Read) => format!("Mark {} as read", msgs(n)),
            Some(FlagOp::Unread) => format!("Mark {} as unread", msgs(n)),
            Some(FlagOp::Flagged) => format!("Flag {}", msgs(n)),
            Some(FlagOp::Unflagged) | None => format!("Remove the flag from {}", msgs(n)),
        },
        Kind::CreateFolder => format!("Create folder \"{}\"", clean_line(params.new_folder.as_deref().unwrap_or(""), 80)),
        // Calendar cards are titled by the planner in tools::cal (it has the formatted times).
        Kind::EventCreate | Kind::EventChange | Kind::EventCancel | Kind::InviteResponse => String::new(),
    }
}

/// Warnings computed from the resolved messages. `left_out`: references that did not resolve;
/// `already_there`: messages dropped because they already are where the change would put them.
pub fn warnings_for(kind: Kind, params: &Params, items: &[NewItem], left_out: usize, already_there: usize, dest_label: &str) -> Vec<String> {
    let mut w = Vec::new();
    if matches!(kind, Kind::Move | Kind::Archive | Kind::Trash) {
        let unread = items.iter().filter(|i| !i.seen).count();
        if unread > 0 {
            w.push(format!("includes {unread} unread"));
        }
        let flagged = items.iter().filter(|i| i.flagged).count();
        if flagged > 0 {
            w.push(format!("includes {flagged} flagged"));
        }
        let folders: HashSet<(&str, &str)> = items.iter().map(|i| (i.account.as_str(), i.folder.as_str())).collect();
        if folders.len() > 1 {
            w.push(format!("comes from {} different folders", folders.len()));
        }
    }
    if kind == Kind::Move && params.create_dest {
        w.push(format!("creates the new folder \"{dest_label}\""));
    }
    if kind == Kind::Trash {
        w.push("goes to Trash, not deleted for good; you can restore it from there".to_string());
    }
    if items.len() > EXEC_CHUNK {
        w.push(format!("{} messages: runs in batches and may take a moment", items.len()));
    }
    if already_there > 0 {
        w.push(format!("{already_there} already there, left out"));
    }
    if left_out > 0 {
        w.push(format!("{left_out} reference{} left out (not found, outside the allowed folders, or the folder was reset)", if left_out == 1 { "" } else { "s" }));
    }
    w
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub kind: Kind,
    pub params: Params,
    pub title: String,
    pub warnings: Vec<String>,
    pub items: Vec<NewItem>,
}

/// Compose the card from RESOLVED messages. Nothing here reads model-provided text.
pub fn build_plan(kind: Kind, params: Params, items: Vec<NewItem>, left_out: usize, already_there: usize) -> Result<Planned, String> {
    if kind != Kind::CreateFolder && items.is_empty() {
        return Err("None of those messages can be changed (not found, outside the allowed folders, or already in place).".into());
    }
    if items.len() > MAX_ITEMS {
        return Err(format!("A suggestion can hold at most {MAX_ITEMS} messages."));
    }
    let dest_label = clean_line(params.dest_folder.as_deref().unwrap_or(""), 80);
    let title = title_for(kind, &params, items.len(), &dest_label);
    let warnings = warnings_for(kind, &params, &items, left_out, already_there, &dest_label);
    Ok(Planned { kind, params, title, warnings, items })
}

// ------------------------------------------------------------------------------------------
// Store (ai.db)
// ------------------------------------------------------------------------------------------

fn row_proposal(r: &rusqlite::Row<'_>) -> rusqlite::Result<Proposal> {
    let kind: String = r.get(2)?;
    let state: String = r.get(3)?;
    let params: String = r.get(4)?;
    let warnings: String = r.get(7)?;
    Ok(Proposal {
        id: r.get(0)?,
        conv_id: r.get(1)?,
        kind: Kind::parse(&kind).unwrap_or(Kind::Move),
        state: State::parse(&state).unwrap_or(State::Failed),
        params: serde_json::from_str(&params).unwrap_or_default(),
        title: r.get(5)?,
        reason: r.get(6)?,
        warnings: serde_json::from_str(&warnings).unwrap_or_default(),
        error: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        expires_at: r.get(11)?,
    })
}
const PROPOSAL_COLS: &str = "id, conv_id, kind, state, params_json, title, reason, warnings_json, error, created_at, updated_at, expires_at";

fn row_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    let status: String = r.get(11)?;
    Ok(Item {
        idx: r.get(0)?,
        account: r.get(1)?,
        folder: r.get(2)?,
        uid: r.get(3)?,
        uidvalidity: r.get(4)?,
        sender: r.get(5)?,
        subject: r.get(6)?,
        seen: r.get::<_, i64>(7)? != 0,
        flagged: r.get::<_, i64>(8)? != 0,
        to_folder: r.get(9)?,
        selected: r.get::<_, i64>(10)? != 0,
        status: ItemStatus::parse(&status),
        error: r.get(12)?,
    })
}
const ITEM_COLS: &str = "idx, account, folder, uid, uidvalidity, sender, subject, seen, flagged, to_folder, selected, status, error";

fn row_log(r: &rusqlite::Row<'_>) -> rusqlite::Result<LogRow> {
    let opt = |v: Option<i64>| v.and_then(|x| u32::try_from(x).ok());
    Ok(LogRow {
        id: r.get(0)?,
        proposal_id: r.get(1)?,
        item_idx: r.get(2)?,
        ts: r.get(3)?,
        op: r.get(4)?,
        account: r.get(5)?,
        src_folder: r.get(6)?,
        src_uid: opt(r.get(7)?),
        src_validity: opt(r.get(8)?),
        dest_folder: r.get(9)?,
        dest_uid: opt(r.get(10)?),
        dest_validity: opt(r.get(11)?),
        message_id: r.get(12)?,
        flag: r.get(13)?,
        flag_old: r.get::<_, Option<i64>>(14)?.map(|v| v != 0),
        flag_new: r.get::<_, Option<i64>>(15)?.map(|v| v != 0),
        outcome: r.get(16)?,
        error: r.get(17)?,
        undone: r.get(18)?,
        undo_error: r.get(19)?,
    })
}
const LOG_COLS: &str = "id, proposal_id, item_idx, ts, op, account, src_folder, src_uid, src_validity, dest_folder, dest_uid, dest_validity, message_id, flag, flag_old, flag_new, outcome, error, undone, undo_error";

impl Store {
    /// Insert a `Proposed` proposal with its rows. Returns the id.
    pub fn create_proposal(&mut self, p: &NewProposal, now: i64) -> StoreResult<i64> {
        let tx = self.conn_mut().transaction()?;
        tx.execute(
            "INSERT INTO proposals (conv_id, kind, state, params_json, title, reason, warnings_json, created_at, updated_at, expires_at)
             VALUES (?1,?2,'proposed',?3,?4,?5,?6,?7,?7,?8)",
            params![p.conv_id, p.kind.as_str(), serde_json::to_string(&p.params)?, p.title, p.reason, serde_json::to_string(&p.warnings)?, now, now + TTL_SECS],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut st = tx.prepare(
                "INSERT INTO proposal_items (proposal_id, idx, account, folder, uid, uidvalidity, sender, subject, seen, flagged, to_folder)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?;
            for (i, it) in p.items.iter().enumerate() {
                st.execute(params![id, i as i64, it.account, it.folder, it.uid, it.uidvalidity, it.sender, it.subject, it.seen, it.flagged, it.to_folder])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn get_proposal(&mut self, id: i64) -> StoreResult<Option<Proposal>> {
        Ok(self.conn_mut().query_row(&format!("SELECT {PROPOSAL_COLS} FROM proposals WHERE id=?1"), params![id], row_proposal).optional()?)
    }

    /// Newest first. `conv_id: None` = every conversation.
    pub fn list_proposals(&mut self, conv_id: Option<i64>, limit: u32) -> StoreResult<Vec<Proposal>> {
        let c = self.conn_mut();
        let mut st = c.prepare(&format!("SELECT {PROPOSAL_COLS} FROM proposals WHERE (?1 IS NULL AND conv_id > 0) OR conv_id=?1 ORDER BY created_at DESC, id DESC LIMIT ?2"))?;
        let rows = st.query_map(params![conv_id, limit], row_proposal)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn proposal_items(&mut self, id: i64) -> StoreResult<Vec<Item>> {
        let c = self.conn_mut();
        let mut st = c.prepare(&format!("SELECT {ITEM_COLS} FROM proposal_items WHERE proposal_id=?1 ORDER BY idx"))?;
        let rows = st.query_map(params![id], row_item)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn proposal_counts(&mut self, id: i64) -> StoreResult<Counts> {
        let c = self.conn_mut();
        let mut st = c.prepare("SELECT status, selected, COUNT(*) FROM proposal_items WHERE proposal_id=?1 GROUP BY status, selected")?;
        let rows = st.query_map(params![id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, u32>(2)?)))?;
        let mut c = Counts::default();
        for r in rows {
            let (status, sel, n) = r?;
            c.total += n;
            if sel != 0 {
                c.selected += n;
            }
            match ItemStatus::parse(&status) {
                ItemStatus::Done => c.done += n,
                ItemStatus::Failed => c.failed += n,
                ItemStatus::Skipped => c.skipped += n,
                ItemStatus::Noop => c.noop += n,
                ItemStatus::Undone => c.undone += n,
                ItemStatus::UndoFailed => c.undo_failed += n,
                ItemStatus::Unknown => c.unknown += n,
                ItemStatus::Pending => {}
            }
        }
        Ok(c)
    }

    /// Compare-and-set state change. Illegal moves and a changed current state are refused, so two
    /// concurrent approvals cannot both win.
    pub fn transition_proposal(&mut self, id: i64, from: State, to: State, now: i64, error: &str) -> Result<(), ProposalError> {
        if !from.can_become(to) {
            return Err(ProposalError::Illegal { from, to });
        }
        let n = self
            .conn_mut()
            .execute("UPDATE proposals SET state=?3, error=?4, updated_at=?5 WHERE id=?1 AND state=?2", params![id, from.as_str(), to.as_str(), error, now])
            .map_err(db)?;
        if n == 1 {
            return Ok(());
        }
        match self.get_proposal(id).map_err(db)? {
            None => Err(ProposalError::NotFound),
            Some(p) => Err(ProposalError::Illegal { from: p.state, to }),
        }
    }

    /// Select exactly the rows in `selected` (and deselect every other row). Fails closed: a row the
    /// caller did not name is never executed.
    pub fn set_selection(&mut self, id: i64, selected: &[i64]) -> StoreResult<()> {
        let tx = self.conn_mut().transaction()?;
        tx.execute("UPDATE proposal_items SET selected=0 WHERE proposal_id=?1", params![id])?;
        for idx in selected {
            tx.execute("UPDATE proposal_items SET selected=1 WHERE proposal_id=?1 AND idx=?2", params![id, idx])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_item_status(&mut self, id: i64, idx: i64, status: ItemStatus, error: &str) -> StoreResult<()> {
        self.conn_mut().execute("UPDATE proposal_items SET status=?3, error=?4 WHERE proposal_id=?1 AND idx=?2", params![id, idx, status.as_str(), error])?;
        Ok(())
    }

    pub fn add_log(&mut self, r: &LogRow, now: i64) -> StoreResult<i64> {
        let c = self.conn_mut();
        c.execute(
            "INSERT INTO action_log (proposal_id, item_idx, ts, op, account, src_folder, src_uid, src_validity, dest_folder, dest_uid, dest_validity, message_id, flag, flag_old, flag_new, outcome, error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![r.proposal_id, r.item_idx, now, r.op, r.account, r.src_folder, r.src_uid, r.src_validity, r.dest_folder, r.dest_uid, r.dest_validity, r.message_id, r.flag, r.flag_old, r.flag_new, r.outcome, r.error],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn list_log(&mut self, proposal_id: i64) -> StoreResult<Vec<LogRow>> {
        let c = self.conn_mut();
        let mut st = c.prepare(&format!("SELECT {LOG_COLS} FROM action_log WHERE proposal_id=?1 ORDER BY id"))?;
        let rows = st.query_map(params![proposal_id], row_log)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_log_undone(&mut self, log_id: i64, undone: i64, error: &str) -> StoreResult<()> {
        self.conn_mut().execute("UPDATE action_log SET undone=?2, undo_error=?3 WHERE id=?1", params![log_id, undone, error])?;
        Ok(())
    }

    /// Proposals nobody decided in time become `Expired` (so they can no longer be approved).
    pub fn expire_stale(&mut self, now: i64) -> StoreResult<usize> {
        Ok(self.conn_mut().execute("UPDATE proposals SET state='expired', updated_at=?1 WHERE state='proposed' AND expires_at < ?1", params![now])?)
    }

    /// A crash or quit while a proposal was `Executing` leaves it in that state forever. Settle such
    /// rows from the log: some ok step -> `PartiallyDone`, else `Failed`. Returns how many were settled.
    pub fn recover_interrupted(&mut self, now: i64) -> StoreResult<usize> {
        let ids: Vec<i64> = {
            let c = self.conn_mut();
            let mut st = c.prepare("SELECT id FROM proposals WHERE state='executing'")?;
            let rows = st.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for id in &ids {
            let ok: i64 = self.conn_mut().query_row("SELECT COUNT(*) FROM action_log WHERE proposal_id=?1 AND outcome IN ('ok','ok-unverified') AND op<>'create_folder'", params![id], |r| r.get(0))?;
            let cal_ok = self.cal_log_has_ok(*id)?;
            let to = if ok > 0 || cal_ok { State::PartiallyDone } else { State::Failed };
            let _ = self.transition_proposal(*id, State::Executing, to, now, "MiraMail was closed while this was running. Check the activity log for what was done.");
        }
        Ok(ids.len())
    }
}

// ------------------------------------------------------------------------------------------
// The mail side of execution (implemented by the app; faked in tests)
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleFolder {
    Trash,
    Archive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveReqItem {
    pub uid: Option<u32>,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveReq {
    pub account: String,
    pub from_folder: String,
    /// UIDVALIDITY the UIDs were observed under; the server must still agree.
    pub from_validity: Option<u32>,
    pub to_folder: String,
    pub items: Vec<MoveReqItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveItemRes {
    pub uid: Option<u32>,
    pub ok: bool,
    pub error: Option<String>,
    pub new_uid: Option<u32>,
    pub message_id: Option<String>,
    /// The command may have run but the result could not be confirmed (timeout / dropped connection
    /// and the follow-up check could not tell). `ok` is false; the message may be in the destination.
    pub unknown: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRes {
    pub dest_validity: Option<u32>,
    /// Parallel to the request's items.
    pub items: Vec<MoveItemRes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagReq {
    pub account: String,
    pub folder: String,
    pub validity: Option<u32>,
    pub uids: Vec<u32>,
    /// `seen` | `flagged`
    pub flag: String,
    pub value: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagRes {
    /// `(uid, ok, error)` parallel to the request's uids.
    pub items: Vec<(u32, bool, Option<String>)>,
    /// The value the SERVER had for the flag before the call, parallel to `items` (`None` = unknown).
    /// A message that already had the wanted value is reported `ok` and left untouched.
    pub old: Vec<Option<bool>>,
}

/// Write access to the user's mail, used ONLY by [`Executor`] after an explicit approval. The
/// implementation must keep the local cache consistent with what it did on the server. The trait has
/// no send and no permanent-delete operation, and the model never sees it.
pub trait MailActionHost: Send + Sync {
    /// UIDVALIDITY of a folder as known locally (`None` = never synced).
    fn folder_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String>;
    /// The locally cached row of a message, `None` when it is not there any more.
    fn lookup(&self, account: &str, folder: &str, uid: u32) -> Result<Option<MsgMeta>, String>;
    fn role_folder(&self, account: &str, role: RoleFolder) -> Result<Option<String>, String>;
    fn folder_exists(&self, account: &str, folder: &str) -> Result<bool, String>;
    /// The existing folder whose name equals `name` ignoring case, spelled as the server has it.
    fn find_folder(&self, account: &str, name: &str) -> Result<Option<String>, String>;
    fn move_batch(&self, req: MoveReq) -> BoxFut<'_, Result<MoveRes, String>>;
    fn flag_batch(&self, req: FlagReq) -> BoxFut<'_, Result<FlagRes, String>>;
    /// `Ok(true)` created, `Ok(false)` already existed.
    fn create_folder(&self, account: &str, name: &str) -> BoxFut<'_, Result<bool, String>>;
}

// ------------------------------------------------------------------------------------------
// Executor
// ------------------------------------------------------------------------------------------

fn lock(s: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecReport {
    pub state: State,
    pub ok: usize,
    pub failed: usize,
    pub skipped: usize,
    pub noop: usize,
    /// Moves whose outcome could not be confirmed after a connection failure.
    pub unverified: usize,
    /// `(account, folder)` pairs whose contents changed (source and destination folders).
    pub affected: Vec<(String, String)>,
    /// A folder was created: the folder list changed.
    pub folders_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoReport {
    pub state: State,
    pub undone: usize,
    pub failed: usize,
    /// Distinct short reasons, at most 5.
    pub errors: Vec<String>,
    pub affected: Vec<(String, String)>,
}

#[derive(Default)]
struct Run {
    ok: usize,
    failed: usize,
    skipped: usize,
    noop: usize,
    /// Moves whose result could not be confirmed (counted as partial success: Undo can still try).
    unverified: usize,
    fatal: Option<String>,
    affected: HashSet<(String, String)>,
    folders_changed: bool,
}

/// Shared "being worked on" set so one proposal is never executed and undone at the same time.
pub type Busy = Arc<Mutex<HashSet<i64>>>;

/// Proof that the proposal `id` is claimed (no other approve / undo can run on it) until dropped.
pub struct BusyGuard {
    set: Busy,
    id: i64,
}
impl BusyGuard {
    pub(crate) fn new(set: Busy, id: i64) -> BusyGuard {
        BusyGuard { set, id }
    }
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.id);
    }
}

/// Reject a proposed suggestion. Touches no mail, so it needs no mail host.
pub fn reject(store: &Mutex<Store>, id: i64, now: i64) -> Result<(), ProposalError> {
    lock(store).transition_proposal(id, State::Proposed, State::Rejected, now, "")
}

pub struct Executor {
    store: Arc<Mutex<Store>>,
    host: Arc<dyn MailActionHost>,
    clock: Clock,
    busy: Busy,
}

const NOT_ATTEMPTED: &str = "Not attempted: an earlier step failed.";

impl Executor {
    pub fn new(store: Arc<Mutex<Store>>, host: Arc<dyn MailActionHost>, clock: Clock, busy: Busy) -> Executor {
        Executor { store, host, clock, busy }
    }

    /// Claim the proposal. A caller that must read the conversation's consent for [`Executor::approve_claimed`]
    /// claims FIRST and reads second, so a permission change cannot slip in between (it is refused while claimed).
    pub fn claim(&self, id: i64) -> Result<BusyGuard, ProposalError> {
        if !self.busy.lock().unwrap_or_else(|e| e.into_inner()).insert(id) {
            return Err(ProposalError::Busy);
        }
        Ok(BusyGuard { set: self.busy.clone(), id })
    }

    /// Mark a proposed suggestion as rejected. Nothing is executed.
    pub fn reject(&self, id: i64) -> Result<(), ProposalError> {
        reject(&self.store, id, (self.clock)())
    }

    /// Approve and run exactly the rows in `selected` (fail closed: unnamed rows are not executed;
    /// at most [`MAX_ITEMS`] indexes). `scopes` = the folders the
    /// conversation's consent allows. `progress` is called with the proposal id after each state or
    /// batch change.
    pub async fn approve(&self, id: i64, selected: &[i64], scopes: &[Scope], progress: &(dyn Fn(i64) + Send + Sync)) -> Result<ExecReport, ProposalError> {
        if selected.len() > MAX_ITEMS {
            return Err(ProposalError::TooMany);
        }
        let guard = self.claim(id)?;
        self.approve_claimed(guard, id, selected, scopes, progress).await
    }

    /// Same as [`Executor::approve`] for a proposal the caller already claimed (`guard` is held until done).
    pub async fn approve_claimed(&self, guard: BusyGuard, id: i64, selected: &[i64], scopes: &[Scope], progress: &(dyn Fn(i64) + Send + Sync)) -> Result<ExecReport, ProposalError> {
        let _guard = guard;
        if _guard.id != id || selected.len() > MAX_ITEMS {
            return Err(if selected.len() > MAX_ITEMS { ProposalError::TooMany } else { ProposalError::NotFound });
        }
        let now = (self.clock)();
        let p = {
            let mut st = lock(&self.store);
            let p = st.get_proposal(id).map_err(db)?.ok_or(ProposalError::NotFound)?;
            if p.state != State::Proposed {
                return Err(ProposalError::Illegal { from: p.state, to: State::Approved });
            }
            if p.kind.is_calendar() {
                return Err(ProposalError::Illegal { from: p.state, to: State::Approved });
            }
            if now > p.expires_at {
                st.transition_proposal(id, State::Proposed, State::Expired, now, "")?;
                return Err(ProposalError::Expired);
            }
            if p.kind != Kind::CreateFolder {
                st.set_selection(id, selected).map_err(db)?;
                if st.proposal_counts(id).map_err(db)?.selected == 0 {
                    return Err(ProposalError::NothingSelected);
                }
            }
            st.transition_proposal(id, State::Proposed, State::Approved, now, "")?;
            st.transition_proposal(id, State::Approved, State::Executing, now, "")?;
            p
        };
        progress(id);
        let run = if p.kind == Kind::CreateFolder { self.run_create_folder(&p, scopes).await } else { self.run_items(&p, scopes, progress).await };
        let state = if run.fatal.is_some() && run.ok + run.unverified == 0 {
            State::Failed
        } else if run.ok > 0 && run.failed + run.skipped + run.unverified == 0 && run.fatal.is_none() {
            State::Done
        } else if run.ok + run.unverified > 0 {
            State::PartiallyDone
        } else if run.failed + run.skipped + run.unverified == 0 {
            State::Done // nothing needed to change
        } else {
            State::Failed
        };
        let note = run.fatal.clone().unwrap_or_default();
        lock(&self.store).transition_proposal(id, State::Executing, state, (self.clock)(), &note)?;
        progress(id);
        let mut affected: Vec<(String, String)> = run.affected.into_iter().collect();
        affected.sort();
        Ok(ExecReport { state, ok: run.ok, failed: run.failed, skipped: run.skipped, noop: run.noop, unverified: run.unverified, affected, folders_changed: run.folders_changed })
    }

    async fn run_create_folder(&self, p: &Proposal, scopes: &[Scope]) -> Run {
        let mut run = Run::default();
        let (Some(account), Some(name)) = (p.params.account.clone(), p.params.new_folder.clone()) else {
            run.fatal = Some("The suggestion is incomplete.".into());
            return run;
        };
        if !scopes.iter().any(|s| s.account == account) {
            run.fatal = Some("That account is outside the folders allowed for this conversation.".into());
            run.skipped = 1;
            return run;
        }
        let name = match validate_folder_name(&name) {
            Ok(n) => n,
            Err(e) => {
                run.fatal = Some(e);
                run.failed = 1;
                return run;
            }
        };
        let name_for_scope = name.clone();
        match self.host.create_folder(&account, &name).await {
            Ok(created) => {
                let mut row = LogRow::new(p.id, -1, "create_folder", &account);
                row.dest_folder = name;
                row.outcome = if created { "ok" } else { "exists" }.into();
                if !created {
                    row.error = "The folder already existed.".into();
                }
                let _ = lock(&self.store).add_log(&row, (self.clock)());
                if created {
                    // Approved create-folder card: the new folder joins THIS conversation's allowed set.
                    let _ = lock(&self.store).add_conv_folder(p.conv_id, &account, &name_for_scope);
                    run.ok = 1;
                    run.folders_changed = true;
                } else {
                    run.noop = 1;
                }
            }
            Err(e) => {
                let mut row = LogRow::new(p.id, -1, "create_folder", &account);
                row.dest_folder = name;
                row.outcome = "failed".into();
                row.error = e.clone();
                let _ = lock(&self.store).add_log(&row, (self.clock)());
                run.failed = 1;
                run.fatal = Some(e);
            }
        }
        run
    }

    /// Record the result for one row: item status + log row, in the order the user can later read.
    fn finish(&self, p: &Proposal, it: &Item, status: ItemStatus, error: &str, row: Option<LogRow>) {
        let mut st = lock(&self.store);
        let _ = st.set_item_status(p.id, it.idx, status, error);
        if let Some(mut r) = row {
            r.error = error.to_string();
            if r.outcome.is_empty() {
                r.outcome = "failed".into();
            }
            let _ = st.add_log(&r, (self.clock)());
        }
    }

    fn skip_row(&self, p: &Proposal, it: &Item, outcome: &str) -> LogRow {
        let op = if p.kind == Kind::Flag { "flag" } else { "move" };
        let mut r = LogRow::new(p.id, it.idx, op, &it.account);
        r.src_folder = it.folder.clone();
        r.src_uid = Some(it.uid);
        r.src_validity = Some(it.uidvalidity);
        r.outcome = outcome.into();
        r
    }

    async fn run_items(&self, p: &Proposal, scopes: &[Scope], progress: &(dyn Fn(i64) + Send + Sync)) -> Run {
        let mut run = Run::default();
        let items: Vec<Item> = match lock(&self.store).proposal_items(p.id) {
            Ok(v) => v.into_iter().filter(|i| i.selected && i.status == ItemStatus::Pending).collect(),
            Err(_) => {
                run.fatal = Some("MiraMail couldn't read the suggestion.".into());
                return run;
            }
        };
        let mut groups: BTreeMap<(String, String), Vec<Item>> = BTreeMap::new();
        for it in items {
            groups.entry((it.account.clone(), it.folder.clone())).or_default().push(it);
        }
        let mut aborted: Option<String> = None;

        // A move into a new folder: create it first (once), but only when at least one selected row
        // would really be moved there: the destination account must be one of the conversation's
        // accounts and some selected row of that account must sit in an allowed folder. Otherwise
        // nothing is created and the rows are skipped by the checks below.
        if p.kind == Kind::Move && p.params.create_dest {
            if let (Some(acc), Some(name)) = (p.params.dest_account.clone(), p.params.dest_folder.clone()) {
                let mut row = LogRow::new(p.id, -1, "create_folder", &acc);
                row.dest_folder = name.clone();
                let allowed = scopes.iter().any(|s| s.account == acc)
                    && groups.keys().any(|(a, f)| *a == acc && scopes.iter().any(|s| s.account == *a && s.folder == *f));
                if !allowed {
                    row.outcome = "skipped".into();
                    row.error = "The new folder was not created: no selected message can be moved there.".into();
                    let _ = lock(&self.store).add_log(&row, (self.clock)());
                } else if self.host.folder_exists(&acc, &name).unwrap_or(false) {
                    row.outcome = "exists".into();
                    row.error = "The folder already existed.".into();
                    let _ = lock(&self.store).add_log(&row, (self.clock)());
                } else {
                    match self.host.create_folder(&acc, &name).await {
                        Ok(created) => {
                            row.outcome = if created { "ok" } else { "exists" }.into();
                            if !created {
                                row.error = "The folder already existed.".into();
                            }
                            let _ = lock(&self.store).add_log(&row, (self.clock)());
                            run.folders_changed |= created;
                            if created {
                                // Created by THIS approved move: the folder joins the conversation's allowed set, like an approved create-folder card.
                                let _ = lock(&self.store).add_conv_folder(p.conv_id, &acc, &name);
                            }
                        }
                        Err(e) => {
                            row.outcome = "failed".into();
                            row.error = e.clone();
                            let _ = lock(&self.store).add_log(&row, (self.clock)());
                            aborted = Some(format!("The new folder could not be created: {e}"));
                            run.fatal = Some(e);
                        }
                    }
                }
            }
        }

        for ((account, folder), group) in groups {
            if let Some(msg) = &aborted {
                for it in &group {
                    self.finish(p, it, ItemStatus::Skipped, msg, Some(self.skip_row(p, it, "skipped")));
                    run.skipped += 1;
                }
                continue;
            }
            // Folder-level checks.
            let group_err: Option<String> = if !scopes.iter().any(|s| s.account == account && s.folder == folder) {
                Some("This folder is outside the folders allowed for this conversation.".into())
            } else {
                None
            };
            if let Some(e) = group_err {
                for it in &group {
                    self.finish(p, it, ItemStatus::Skipped, &e, Some(self.skip_row(p, it, "skipped")));
                    run.skipped += 1;
                }
                continue;
            }
            let current_validity = match self.host.folder_validity(&account, &folder) {
                Ok(v) => v,
                Err(e) => {
                    for it in &group {
                        self.finish(p, it, ItemStatus::Failed, &e, Some(self.skip_row(p, it, "failed")));
                        run.failed += 1;
                    }
                    continue;
                }
            };
            // Destination.
            let dest: Result<Option<String>, String> = match p.kind {
                Kind::Move => {
                    if p.params.dest_account.as_deref() == Some(account.as_str()) {
                        p.params.dest_folder.clone().map(Some).ok_or_else(|| "No destination folder.".to_string())
                    } else {
                        Err("The destination folder belongs to a different account.".to_string())
                    }
                }
                Kind::Archive | Kind::Trash => {
                    let role = if p.kind == Kind::Trash { RoleFolder::Trash } else { RoleFolder::Archive };
                    match self.host.role_folder(&account, role) {
                        Ok(Some(f)) => Ok(Some(f)),
                        Ok(None) => Err(format!("This account has no {} folder.", if p.kind == Kind::Trash { "Trash" } else { "Archive" })),
                        Err(e) => Err(e),
                    }
                }
                _ => Ok(None),
            };
            let dest = match dest {
                Ok(d) => d,
                Err(e) => {
                    for it in &group {
                        self.finish(p, it, ItemStatus::Skipped, &e, Some(self.skip_row(p, it, "skipped")));
                        run.skipped += 1;
                    }
                    continue;
                }
            };
            if dest.as_deref() == Some(folder.as_str()) {
                for it in &group {
                    self.finish(p, it, ItemStatus::Noop, "Already in that folder.", None);
                    run.noop += 1;
                }
                continue;
            }
            // Row-level revalidation.
            let mut valid: Vec<(Item, MsgMeta)> = Vec::new();
            for it in group {
                if current_validity != Some(it.uidvalidity) {
                    self.finish(p, &it, ItemStatus::Skipped, "The folder was reset on the server, so this message can't be identified safely.", Some(self.skip_row(p, &it, "skipped")));
                    run.skipped += 1;
                    continue;
                }
                match self.host.lookup(&account, &folder, it.uid) {
                    Ok(Some(m)) if m.uidvalidity == it.uidvalidity => valid.push((it, m)),
                    Ok(_) => {
                        self.finish(p, &it, ItemStatus::Skipped, "The message is no longer in this folder (moved or deleted).", Some(self.skip_row(p, &it, "skipped")));
                        run.skipped += 1;
                    }
                    Err(e) => {
                        self.finish(p, &it, ItemStatus::Failed, &e, Some(self.skip_row(p, &it, "failed")));
                        run.failed += 1;
                    }
                }
            }

            if p.kind == Kind::Flag {
                self.run_flags(p, &account, &folder, current_validity, valid, &mut run, &mut aborted, progress).await;
                continue;
            }
            let dest = dest.unwrap_or_default();
            for chunk in valid.chunks(EXEC_CHUNK) {
                if let Some(msg) = &aborted {
                    for (it, _) in chunk {
                        self.finish(p, it, ItemStatus::Skipped, msg, Some(self.skip_row(p, it, "skipped")));
                        run.skipped += 1;
                    }
                    continue;
                }
                let req = MoveReq {
                    account: account.clone(),
                    from_folder: folder.clone(),
                    from_validity: current_validity,
                    to_folder: dest.clone(),
                    items: chunk.iter().map(|(it, _)| MoveReqItem { uid: Some(it.uid), message_id: None }).collect(),
                };
                match self.host.move_batch(req).await {
                    Ok(res) if res.items.len() == chunk.len() => {
                        for ((it, _), r) in chunk.iter().zip(res.items) {
                            if r.ok {
                                let mut row = LogRow::new(p.id, it.idx, "move", &account);
                                row.src_folder = folder.clone();
                                row.src_uid = Some(it.uid);
                                row.src_validity = current_validity;
                                row.dest_folder = dest.clone();
                                row.dest_uid = r.new_uid;
                                row.dest_validity = res.dest_validity;
                                row.message_id = r.message_id;
                                self.finish(p, it, ItemStatus::Done, "", Some(row));
                                run.ok += 1;
                                run.affected.insert((account.clone(), folder.clone()));
                                run.affected.insert((account.clone(), dest.clone()));
                            } else if r.unknown {
                                // The command may have run. Record what an Undo needs (Message-ID) so it can still try.
                                let mut row = LogRow::new(p.id, it.idx, "move", &account);
                                row.src_folder = folder.clone();
                                row.src_uid = Some(it.uid);
                                row.src_validity = current_validity;
                                row.dest_folder = dest.clone();
                                row.dest_validity = res.dest_validity;
                                row.message_id = r.message_id;
                                row.outcome = "ok-unverified".into();
                                self.finish(p, it, ItemStatus::Unknown, "Not confirmed: the connection failed while moving. The message may be in the destination; Undo will look for it.", Some(row));
                                run.unverified += 1;
                                run.affected.insert((account.clone(), folder.clone()));
                                run.affected.insert((account.clone(), dest.clone()));
                            } else {
                                let e = r.error.unwrap_or_else(|| "The server did not move the message.".into());
                                self.finish(p, it, ItemStatus::Failed, &e, Some(self.skip_row(p, it, "failed")));
                                run.failed += 1;
                            }
                        }
                    }
                    Ok(_) => {
                        let e = "The mail store returned an inconsistent answer.".to_string();
                        for (it, _) in chunk {
                            self.finish(p, it, ItemStatus::Failed, &e, Some(self.skip_row(p, it, "failed")));
                            run.failed += 1;
                        }
                        aborted = Some(NOT_ATTEMPTED.into());
                    }
                    Err(e) => {
                        for (it, _) in chunk {
                            self.finish(p, it, ItemStatus::Failed, &e, Some(self.skip_row(p, it, "failed")));
                            run.failed += 1;
                        }
                        aborted = Some(NOT_ATTEMPTED.into());
                    }
                }
                progress(p.id);
            }
        }
        run
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_flags(
        &self,
        p: &Proposal,
        account: &str,
        folder: &str,
        validity: Option<u32>,
        valid: Vec<(Item, MsgMeta)>,
        run: &mut Run,
        aborted: &mut Option<String>,
        progress: &(dyn Fn(i64) + Send + Sync),
    ) {
        let Some(op) = p.params.flag else {
            for (it, _) in &valid {
                self.finish(p, it, ItemStatus::Failed, "The suggestion is incomplete.", Some(self.skip_row(p, it, "failed")));
                run.failed += 1;
            }
            return;
        };
        let want = op.value();
        // No local noop pre-check: the cache may be stale, so the server reports each message's
        // current value and decides (it leaves messages that already match untouched).
        let todo: Vec<(Item, bool)> = valid.into_iter().map(|(it, m)| {
            let local = if op.flag() == "seen" { m.seen } else { m.flagged };
            (it, local)
        }).collect();
        for chunk in todo.chunks(EXEC_CHUNK) {
            if let Some(msg) = aborted.as_ref() {
                for (it, _) in chunk {
                    self.finish(p, it, ItemStatus::Skipped, msg, Some(self.skip_row(p, it, "skipped")));
                    run.skipped += 1;
                }
                continue;
            }
            let req = FlagReq { account: account.into(), folder: folder.into(), validity, uids: chunk.iter().map(|(i, _)| i.uid).collect(), flag: op.flag().into(), value: want };
            match self.host.flag_batch(req).await {
                Ok(res) if res.items.len() == chunk.len() && res.old.len() == chunk.len() => {
                    for (((it, local), (_, ok, err)), server_old) in chunk.iter().zip(res.items).zip(res.old) {
                        if ok {
                            // The server's value is the truth for the log; fall back to the cache only if it did not say.
                            let old = server_old.unwrap_or(*local);
                            if old == want {
                                self.finish(p, it, ItemStatus::Noop, "Already set.", None);
                                run.noop += 1;
                                continue;
                            }
                            let mut row = LogRow::new(p.id, it.idx, "flag", account);
                            row.src_folder = folder.into();
                            row.src_uid = Some(it.uid);
                            row.src_validity = validity;
                            row.flag = op.flag().into();
                            row.flag_old = Some(old);
                            row.flag_new = Some(want);
                            self.finish(p, it, ItemStatus::Done, "", Some(row));
                            run.ok += 1;
                            run.affected.insert((account.to_string(), folder.to_string()));
                        } else {
                            let e = err.unwrap_or_else(|| "The server did not change the flag.".into());
                            self.finish(p, it, ItemStatus::Failed, &e, Some(self.skip_row(p, it, "failed")));
                            run.failed += 1;
                        }
                    }
                }
                Ok(_) | Err(_) => {
                    let e = "The server could not change the flags.".to_string();
                    for (it, _) in chunk {
                        self.finish(p, it, ItemStatus::Failed, &e, Some(self.skip_row(p, it, "failed")));
                        run.failed += 1;
                    }
                    *aborted = Some(NOT_ATTEMPTED.into());
                }
            }
            progress(p.id);
        }
    }

    /// Reverse what the log says was done. Rows that cannot be reversed are reported, not hidden; a
    /// second call retries them. The proposal becomes `Undone` only when every executed row is undone.
    pub async fn undo(&self, id: i64, progress: &(dyn Fn(i64) + Send + Sync)) -> Result<UndoReport, ProposalError> {
        let _guard = self.claim(id)?;
        let (p, rows) = {
            let mut st = lock(&self.store);
            let p = st.get_proposal(id).map_err(db)?.ok_or(ProposalError::NotFound)?;
            if !p.state.can_become(State::Undone) || p.kind.is_calendar() {
                return Err(ProposalError::Illegal { from: p.state, to: State::Undone });
            }
            let rows: Vec<LogRow> = st.list_log(id).map_err(db)?.into_iter().filter(reversible).collect();
            if rows.is_empty() {
                return Err(ProposalError::NothingToUndo);
            }
            (p, rows)
        };
        let mut undone = 0usize;
        let mut failed = 0usize;
        let mut errors: Vec<String> = Vec::new();
        let mut affected: HashSet<(String, String)> = HashSet::new();
        let note = |e: &str, errors: &mut Vec<String>| {
            if errors.len() < 5 && !errors.iter().any(|x| x == e) {
                errors.push(e.to_string());
            }
        };

        // Moves: group by (account, where it is now, where it came from).
        type MoveKey = (String, String, Option<u32>, String);
        let mut moves: BTreeMap<MoveKey, Vec<&LogRow>> = BTreeMap::new();
        type FlagKey = (String, String, Option<u32>, String, bool);
        let mut flags: BTreeMap<FlagKey, Vec<&LogRow>> = BTreeMap::new();
        for r in &rows {
            if r.op == "move" {
                moves.entry((r.account.clone(), r.dest_folder.clone(), r.dest_validity, r.src_folder.clone())).or_default().push(r);
            } else if r.op == "flag" {
                match (r.flag_old, r.flag_new) {
                    (Some(o), Some(n)) if o == n => {
                        // Nothing was changed; trivially undone.
                        let mut st = lock(&self.store);
                        let _ = st.set_log_undone(r.id, 1, "");
                        let _ = st.set_item_status(id, r.item_idx, ItemStatus::Undone, "");
                        undone += 1;
                    }
                    (Some(o), _) => flags.entry((r.account.clone(), r.src_folder.clone(), r.src_validity, r.flag.clone(), o)).or_default().push(r),
                    _ => {}
                }
            }
        }
        let mut aborted = false;
        for ((account, from_folder, from_validity, to_folder), group) in moves {
            for chunk in group.chunks(EXEC_CHUNK) {
                if aborted {
                    for r in chunk {
                        self.mark_undo(id, r, Err(NOT_ATTEMPTED.into()));
                        failed += 1;
                    }
                    note(NOT_ATTEMPTED, &mut errors);
                    continue;
                }
                let req = MoveReq {
                    account: account.clone(),
                    from_folder: from_folder.clone(),
                    from_validity,
                    to_folder: to_folder.clone(),
                    items: chunk.iter().map(|r| MoveReqItem { uid: r.dest_uid, message_id: r.message_id.clone() }).collect(),
                };
                match self.host.move_batch(req).await {
                    Ok(res) if res.items.len() == chunk.len() => {
                        for (r, x) in chunk.iter().zip(res.items) {
                            if x.ok {
                                self.mark_undo(id, r, Ok(()));
                                undone += 1;
                                affected.insert((account.clone(), from_folder.clone()));
                                affected.insert((account.clone(), to_folder.clone()));
                            } else if x.unknown {
                                let e = "Not confirmed: the connection failed while moving it back. Check the folder, then try Undo again.".to_string();
                                note(&e, &mut errors);
                                self.mark_undo(id, r, Err(e));
                                failed += 1;
                            } else {
                                let e = x.error.unwrap_or_else(|| "Could not move it back.".into());
                                note(&e, &mut errors);
                                self.mark_undo(id, r, Err(e));
                                failed += 1;
                            }
                        }
                    }
                    other => {
                        let e = match other {
                            Err(e) => e,
                            _ => "The mail store returned an inconsistent answer.".to_string(),
                        };
                        note(&e, &mut errors);
                        for r in chunk {
                            self.mark_undo(id, r, Err(e.clone()));
                            failed += 1;
                        }
                        aborted = true;
                    }
                }
                progress(id);
            }
        }
        for ((account, folder, validity, flag, old), group) in flags {
            for chunk in group.chunks(EXEC_CHUNK) {
                if aborted {
                    for r in chunk {
                        self.mark_undo(id, r, Err(NOT_ATTEMPTED.into()));
                        failed += 1;
                    }
                    continue;
                }
                // A row without a source UID cannot be restored; report it by itself and carry on.
                let (usable, unusable): (Vec<&&LogRow>, Vec<&&LogRow>) = chunk.iter().partition(|r| r.src_uid.is_some());
                for r in unusable {
                    let e = "This message's id was not recorded, so its flag can't be restored.".to_string();
                    note(&e, &mut errors);
                    self.mark_undo(id, r, Err(e));
                    failed += 1;
                }
                if usable.is_empty() {
                    continue;
                }
                let req = FlagReq { account: account.clone(), folder: folder.clone(), validity, uids: usable.iter().filter_map(|r| r.src_uid).collect(), flag: flag.clone(), value: old };
                match self.host.flag_batch(req).await {
                    Ok(res) if res.items.len() == usable.len() => {
                        for (r, (_, ok, err)) in usable.iter().zip(res.items) {
                            if ok {
                                self.mark_undo(id, r, Ok(()));
                                undone += 1;
                                affected.insert((account.clone(), folder.clone()));
                            } else {
                                let e = err.unwrap_or_else(|| "Could not restore the flag.".into());
                                note(&e, &mut errors);
                                self.mark_undo(id, r, Err(e));
                                failed += 1;
                            }
                        }
                    }
                    _ => {
                        let e = "The server could not restore the flags.".to_string();
                        note(&e, &mut errors);
                        for r in usable {
                            self.mark_undo(id, r, Err(e.clone()));
                            failed += 1;
                        }
                        aborted = true;
                    }
                }
                progress(id);
            }
        }
        // Final state: Undone only if nothing executed remains un-reversed.
        let state = {
            let mut st = lock(&self.store);
            let remaining = st.list_log(id).map_err(db)?.iter().filter(|r| reversible(r)).count();
            if remaining == 0 {
                st.transition_proposal(id, p.state, State::Undone, (self.clock)(), "")?;
                State::Undone
            } else {
                p.state
            }
        };
        progress(id);
        let mut affected: Vec<(String, String)> = affected.into_iter().collect();
        affected.sort();
        Ok(UndoReport { state, undone, failed, errors, affected })
    }

    fn mark_undo(&self, id: i64, r: &LogRow, res: Result<(), String>) {
        let mut st = lock(&self.store);
        match res {
            Ok(()) => {
                let _ = st.set_log_undone(r.id, 1, "");
                let _ = st.set_item_status(id, r.item_idx, ItemStatus::Undone, "");
            }
            Err(e) => {
                let _ = st.set_log_undone(r.id, 2, &e);
                let _ = st.set_item_status(id, r.item_idx, ItemStatus::UndoFailed, &e);
            }
        }
    }
}

/// A log row an undo should try to reverse.
fn reversible(r: &LogRow) -> bool {
    (r.outcome == "ok" || r.outcome == "ok-unverified") && r.op != "create_folder" && r.undone != 1
}

/// True when [`Executor::undo`] could reverse something for this proposal.
pub fn undoable(p: &Proposal, log: &[LogRow]) -> bool {
    p.state.can_become(State::Undone) && log.iter().any(reversible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_table_is_exact() {
        let legal = [
            (State::Proposed, State::Approved),
            (State::Proposed, State::Rejected),
            (State::Proposed, State::Expired),
            (State::Approved, State::Executing),
            (State::Executing, State::Done),
            (State::Executing, State::Failed),
            (State::Executing, State::PartiallyDone),
            (State::Done, State::Undone),
            (State::PartiallyDone, State::Undone),
        ];
        for a in State::ALL {
            for b in State::ALL {
                assert_eq!(a.can_become(b), legal.contains(&(a, b)), "{a:?} -> {b:?}");
            }
        }
        for s in State::ALL {
            assert_eq!(State::parse(s.as_str()), Some(s));
        }
        assert!(State::parse("nonsense").is_none());
    }

    #[test]
    fn titles_come_from_counts_not_text() {
        let p = Params { dest_folder: Some("Receipts".into()), ..Params::default() };
        assert_eq!(title_for(Kind::Move, &p, 42, "Receipts"), "Move 42 messages to Receipts");
        assert_eq!(title_for(Kind::Move, &p, 1, "Receipts"), "Move 1 message to Receipts");
        assert_eq!(title_for(Kind::Archive, &p, 3, ""), "Archive 3 messages");
        assert_eq!(title_for(Kind::Trash, &p, 2, ""), "Move 2 messages to Trash");
        let f = Params { flag: Some(FlagOp::Unread), ..Params::default() };
        assert_eq!(title_for(Kind::Flag, &f, 5, ""), "Mark 5 messages as unread");
        let c = Params { new_folder: Some("Tax\n2026".into()), ..Params::default() };
        assert_eq!(title_for(Kind::CreateFolder, &c, 0, ""), "Create folder \"Tax 2026\"");
    }

    fn item(seen: bool, flagged: bool, folder: &str) -> NewItem {
        NewItem { account: "a".into(), folder: folder.into(), uid: 1, uidvalidity: 1, sender: "s".into(), subject: "x".into(), seen, flagged, to_folder: "T".into() }
    }

    #[test]
    fn warnings_describe_the_resolved_items() {
        let items = vec![item(false, true, "INBOX"), item(false, false, "INBOX"), item(true, false, "Other")];
        let p = Params { create_dest: true, dest_folder: Some("New".into()), ..Params::default() };
        let w = warnings_for(Kind::Move, &p, &items, 2, 1, "New");
        assert!(w.contains(&"includes 2 unread".to_string()));
        assert!(w.contains(&"includes 1 flagged".to_string()));
        assert!(w.iter().any(|x| x.contains("2 different folders")));
        assert!(w.iter().any(|x| x.contains("creates the new folder \"New\"")));
        assert!(w.iter().any(|x| x.starts_with("2 references left out")));
        assert!(w.iter().any(|x| x.starts_with("1 already there")));
        let t = warnings_for(Kind::Trash, &Params::default(), &items, 0, 0, "");
        assert!(t.iter().any(|x| x.contains("not deleted for good")));
        assert!(warnings_for(Kind::Flag, &Params::default(), &items, 0, 0, "").is_empty());
    }

    #[test]
    fn plan_is_computed_from_items_and_rejects_empty_or_oversized() {
        let items: Vec<NewItem> = (0..3).map(|_| item(false, false, "INBOX")).collect();
        let p = build_plan(Kind::Archive, Params::default(), items, 0, 0).unwrap();
        assert_eq!(p.title, "Archive 3 messages");
        assert!(build_plan(Kind::Archive, Params::default(), vec![], 0, 0).is_err());
        let many: Vec<NewItem> = (0..MAX_ITEMS + 1).map(|_| item(true, false, "INBOX")).collect();
        assert!(build_plan(Kind::Archive, Params::default(), many, 0, 0).is_err());
        let ok: Vec<NewItem> = (0..MAX_ITEMS).map(|_| item(true, false, "INBOX")).collect();
        let big = build_plan(Kind::Archive, Params::default(), ok, 0, 0).unwrap();
        assert!(big.warnings.iter().any(|w| w.contains("batches")));
        assert!(build_plan(Kind::CreateFolder, Params { new_folder: Some("X".into()), ..Params::default() }, vec![], 0, 0).is_ok());
    }

    #[test]
    fn folder_names_are_restricted() {
        assert_eq!(validate_folder_name("  Receipts 2026 ").unwrap(), "Receipts 2026");
        for bad in ["", "a/b", "a.b", "INBOX", "inbox", "x\ny", "\u{202e}x", "caf\u{e9}", "a  b", &"x".repeat(101)] {
            assert!(validate_folder_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn flag_ops() {
        assert_eq!(FlagOp::parse("read").unwrap().flag(), "seen");
        assert!(FlagOp::parse("read").unwrap().value());
        assert!(!FlagOp::parse("unflagged").unwrap().value());
        assert!(FlagOp::parse("delete").is_none());
    }
}
