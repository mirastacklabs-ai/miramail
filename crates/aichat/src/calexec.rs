//! Executor for calendar proposals (create / change / cancel an event, answer an invitation).
//!
//! The only code path that changes a calendar for the assistant, and it runs ONLY from the user's click
//! on Approve (or Undo). Like the mail [`crate::proposals::Executor`] it
//!
//! * moves the proposal through the shared state machine with compare-and-set (one approval at most);
//! * re-validates everything at execution time: the calendar is still in the conversation's scope and not
//!   read-only, the event still exists and its version is the one the card was written for, an invitation's
//!   folder is still in the mail scope with the same UIDVALIDITY;
//! * writes a `cal_log` row with what an Undo needs (a snapshot of the event before a change or a
//!   cancellation, the id of a created event) and reports a partial outcome honestly.
//!
//! Sending (invitations, updates, cancellations, answers) happens inside the app's own calendar paths via
//! [`CalActionHost`], after this approval, and the card said so before the click.

use std::sync::{Arc, Mutex};

use rusqlite::params;

use crate::cal::{CalActionHost, CalParams, RespondTarget};
use crate::engine::Clock;
use crate::proposals::{Busy, BusyGuard, Kind, Proposal, ProposalError, State};
use crate::store::{Result as StoreResult, Store};
use crate::tools::Scope;

fn lock(s: &Mutex<Store>) -> std::sync::MutexGuard<'_, Store> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

const SETTINGS_CHANGED: &str = "Settings changed since this suggestion; ask again.";

fn db<E>(_: E) -> ProposalError {
    ProposalError::Storage
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalLogRow {
    pub id: i64,
    pub proposal_id: i64,
    pub ts: i64,
    /// `create` | `change` | `cancel` | `respond`
    pub op: String,
    pub calendar_id: String,
    pub uid: String,
    pub version_after: String,
    pub snapshot: String,
    pub zoom_id: String,
    pub notified: bool,
    /// `ok` | `failed`
    pub outcome: String,
    pub error: String,
    /// 0 = not undone, 1 = undone, 2 = undo failed
    pub undone: i64,
    pub undo_error: String,
}

impl CalLogRow {
    fn new(proposal_id: i64, op: &str, calendar_id: &str) -> CalLogRow {
        CalLogRow {
            id: 0,
            proposal_id,
            ts: 0,
            op: op.into(),
            calendar_id: calendar_id.into(),
            uid: String::new(),
            version_after: String::new(),
            snapshot: String::new(),
            zoom_id: String::new(),
            notified: false,
            outcome: "ok".into(),
            error: String::new(),
            undone: 0,
            undo_error: String::new(),
        }
    }
}

impl Store {
    pub fn add_cal_log(&mut self, r: &CalLogRow, now: i64) -> StoreResult<i64> {
        let c = self.conn_mut();
        c.execute(
            "INSERT INTO cal_log (proposal_id, ts, op, calendar_id, uid, version_after, snapshot, zoom_id, notified, outcome, error)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![r.proposal_id, now, r.op, r.calendar_id, r.uid, r.version_after, r.snapshot, r.zoom_id, r.notified, r.outcome, r.error],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn list_cal_log(&mut self, proposal_id: i64) -> StoreResult<Vec<CalLogRow>> {
        let c = self.conn_mut();
        let mut st = c.prepare(
            "SELECT id, proposal_id, ts, op, calendar_id, uid, version_after, snapshot, zoom_id, notified, outcome, error, undone, undo_error
             FROM cal_log WHERE proposal_id=?1 ORDER BY id",
        )?;
        let rows = st.query_map(params![proposal_id], |r| {
            Ok(CalLogRow {
                id: r.get(0)?,
                proposal_id: r.get(1)?,
                ts: r.get(2)?,
                op: r.get(3)?,
                calendar_id: r.get(4)?,
                uid: r.get(5)?,
                version_after: r.get(6)?,
                snapshot: r.get(7)?,
                zoom_id: r.get(8)?,
                notified: r.get::<_, i64>(9)? != 0,
                outcome: r.get(10)?,
                error: r.get(11)?,
                undone: r.get(12)?,
                undo_error: r.get(13)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_cal_log_undone(&mut self, id: i64, undone: i64, error: &str) -> StoreResult<()> {
        self.conn_mut().execute("UPDATE cal_log SET undone=?2, undo_error=?3 WHERE id=?1", params![id, undone, error])?;
        Ok(())
    }

    /// Did an interrupted calendar proposal get as far as a successful step?
    pub fn cal_log_has_ok(&mut self, proposal_id: i64) -> StoreResult<bool> {
        let n: i64 = self.conn_mut().query_row("SELECT COUNT(*) FROM cal_log WHERE proposal_id=?1 AND outcome='ok'", params![proposal_id], |r| r.get(0))?;
        Ok(n > 0)
    }
}

fn undoable_row(r: &CalLogRow) -> bool {
    r.outcome == "ok" && r.undone != 1 && matches!(r.op.as_str(), "create" | "change" | "cancel")
}

/// True when [`CalExecutor::undo`] could reverse something for this proposal.
pub fn cal_undoable(st: &mut Store, p: &Proposal) -> bool {
    p.kind.is_calendar() && p.state.can_become(State::Undone) && st.list_cal_log(p.id).map(|l| l.iter().any(undoable_row)).unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalReport {
    pub state: State,
    /// Short app-written result for the card ("Event created", "Answer sent").
    pub summary: String,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalUndoReport {
    pub state: State,
    pub undone: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

pub struct CalExecutor {
    store: Arc<Mutex<Store>>,
    host: Arc<dyn CalActionHost>,
    clock: Clock,
    busy: Busy,
}

struct Done {
    summary: String,
    warning: Option<String>,
}

impl CalExecutor {
    pub fn new(store: Arc<Mutex<Store>>, host: Arc<dyn CalActionHost>, clock: Clock, busy: Busy) -> CalExecutor {
        CalExecutor { store, host, clock, busy }
    }

    /// Claim the proposal. A caller that must read the conversation's consent for [`CalExecutor::approve_claimed`]
    /// claims FIRST and reads second, so a permission change cannot slip in between (it is refused while claimed).
    pub fn claim(&self, id: i64) -> Result<BusyGuard, ProposalError> {
        if !self.busy.lock().unwrap_or_else(|e| e.into_inner()).insert(id) {
            return Err(ProposalError::Busy);
        }
        Ok(BusyGuard::new(self.busy.clone(), id))
    }

    pub fn reject(&self, id: i64) -> Result<(), ProposalError> {
        crate::proposals::reject(&self.store, id, (self.clock)())
    }

    /// Approve and run. `allowed` = calendar ids the conversation's consent allows; `mail_scopes` = the
    /// folders it allows (for an invitation in a message).
    pub async fn approve(&self, id: i64, allowed: &[String], mail_scopes: &[Scope], progress: &(dyn Fn(i64) + Send + Sync)) -> Result<CalReport, ProposalError> {
        let guard = self.claim(id)?;
        self.approve_claimed(guard, id, allowed, mail_scopes, progress).await
    }

    /// Same as [`CalExecutor::approve`] for a proposal the caller already claimed (`guard` is held until done).
    pub async fn approve_claimed(&self, guard: BusyGuard, id: i64, allowed: &[String], mail_scopes: &[Scope], progress: &(dyn Fn(i64) + Send + Sync)) -> Result<CalReport, ProposalError> {
        let _guard = guard;
        let now = (self.clock)();
        let p = {
            let mut st = lock(&self.store);
            let p = st.get_proposal(id).map_err(db)?.ok_or(ProposalError::NotFound)?;
            if !p.kind.is_calendar() {
                return Err(ProposalError::Illegal { from: p.state, to: State::Approved });
            }
            if p.state != State::Proposed {
                return Err(ProposalError::Illegal { from: p.state, to: State::Approved });
            }
            if now > p.expires_at {
                st.transition_proposal(id, State::Proposed, State::Expired, now, "")?;
                return Err(ProposalError::Expired);
            }
            st.transition_proposal(id, State::Proposed, State::Approved, now, "")?;
            st.transition_proposal(id, State::Approved, State::Executing, now, "")?;
            p
        };
        progress(id);
        let res = self.run(&p, allowed, mail_scopes).await;
        let (state, note, report) = match res {
            Ok(d) => (State::Done, d.warning.clone().unwrap_or_default(), CalReport { state: State::Done, summary: d.summary, warning: d.warning }),
            Err(e) => (State::Failed, e.clone(), CalReport { state: State::Failed, summary: String::new(), warning: Some(e) }),
        };
        lock(&self.store).transition_proposal(id, State::Executing, state, (self.clock)(), &note)?;
        progress(id);
        Ok(report)
    }

    fn log(&self, row: CalLogRow) {
        let _ = lock(&self.store).add_cal_log(&row, (self.clock)());
    }

    async fn run(&self, p: &Proposal, allowed: &[String], mail_scopes: &[Scope]) -> Result<Done, String> {
        let cp: CalParams = p.params.cal.clone().ok_or("The suggestion is incomplete.")?;
        let in_scope = |cal: &str| allowed.iter().any(|a| a == cal);
        let mut row = CalLogRow::new(p.id, "", &cp.calendar_id);
        let fail = |this: &CalExecutor, mut row: CalLogRow, op: &str, e: String| -> String {
            row.op = op.into();
            row.outcome = "failed".into();
            row.error = e.clone();
            this.log(row);
            e
        };
        match p.kind {
            Kind::EventCreate => {
                let op = "create";
                let spec = cp.new.clone().ok_or("The suggestion is incomplete.")?;
                if !in_scope(&cp.calendar_id) {
                    return Err(fail(self, row, op, "That calendar is outside the calendars allowed for this conversation.".into()));
                }
                match self.host.calendar(&cp.calendar_id) {
                    Ok(Some(c)) if !c.read_only => {}
                    Ok(Some(_)) => return Err(fail(self, row, op, "That calendar is read-only.".into())),
                    Ok(None) => return Err(fail(self, row, op, "That calendar no longer exists.".into())),
                    Err(e) => return Err(fail(self, row, op, e)),
                }
                // Fail closed: the card said whether invitations would go out. If they would now go out although
                // the card said they would not (a mail account was added since), do nothing.
                if !cp.notify && !spec.attendees.is_empty() && self.host.can_send() {
                    return Err(fail(self, row, op, SETTINGS_CHANGED.into()));
                }
                match self.host.create_event(&cp.calendar_id, &spec, cp.notify).await {
                    Ok(c) => {
                        row.op = op.into();
                        row.uid = c.uid.clone();
                        row.calendar_id = c.calendar_id.clone();
                        row.version_after = c.version.clone();
                        row.zoom_id = c.zoom_meeting_id.clone().unwrap_or_default();
                        row.notified = c.invited;
                        self.log(row);
                        let summary = if c.invited { "Event created and invitations sent.".to_string() } else { "Event created.".to_string() };
                        Ok(Done { summary, warning: c.warning })
                    }
                    Err(e) => Err(fail(self, row, op, e)),
                }
            }
            Kind::EventChange | Kind::EventCancel => {
                let op = if p.kind == Kind::EventChange { "change" } else { "cancel" };
                if !in_scope(&cp.calendar_id) {
                    return Err(fail(self, row, op, "That calendar is outside the calendars allowed for this conversation.".into()));
                }
                match self.host.calendar(&cp.calendar_id) {
                    Ok(Some(c)) if !c.read_only => {}
                    Ok(Some(_)) => return Err(fail(self, row, op, "That calendar is read-only.".into())),
                    Ok(None) => return Err(fail(self, row, op, "That calendar no longer exists.".into())),
                    Err(e) => return Err(fail(self, row, op, e)),
                }
                let cur = match self.host.lookup(&cp.calendar_id, &cp.uid, "") {
                    Ok(Some(c)) => c,
                    Ok(None) => return Err(fail(self, row, op, "The event no longer exists, so nothing was changed.".into())),
                    Err(e) => return Err(fail(self, row, op, e)),
                };
                if cur.version != cp.expected_version {
                    return Err(fail(self, row, op, "The event was changed after this suggestion was made, so nothing was done. Ask again to get a fresh suggestion.".into()));
                }
                row.uid = cp.uid.clone();
                if !cp.notify && cur.attendee_count > 0 && cur.my_status == "organizer" && self.host.can_send() {
                    return Err(fail(self, row, op, SETTINGS_CHANGED.into()));
                }
                if p.kind == Kind::EventChange {
                    let ch = cp.changes.clone().ok_or("The suggestion is incomplete.")?;
                    match self.host.change_event(&cp.calendar_id, &cp.uid, &ch, cp.notify, &cp.expected_version).await {
                        Ok(c) => {
                            row.op = op.into();
                            row.snapshot = c.snapshot;
                            row.version_after = c.version;
                            row.notified = c.notified;
                            self.log(row);
                            let summary = if c.notified { "Event updated and attendees notified.".to_string() } else { "Event updated.".to_string() };
                            Ok(Done { summary, warning: c.warning })
                        }
                        Err(e) => Err(fail(self, row, op, e)),
                    }
                } else {
                    match self.host.cancel_event(&cp.calendar_id, &cp.uid, cp.notify, &cp.expected_version).await {
                        Ok(c) => {
                            row.op = op.into();
                            row.snapshot = c.snapshot;
                            row.notified = c.notified;
                            self.log(row);
                            let summary = if c.notified { "Event cancelled and attendees notified.".to_string() } else { "Event cancelled.".to_string() };
                            Ok(Done { summary, warning: c.warning })
                        }
                        Err(e) => Err(fail(self, row, op, e)),
                    }
                }
            }
            Kind::InviteResponse => {
                let op = "respond";
                let status = cp.response.clone().ok_or("The suggestion is incomplete.")?;
                let target = match &cp.msg {
                    Some(m) => {
                        if !mail_scopes.iter().any(|s| s.account == m.account && s.folder == m.folder) {
                            return Err(fail(self, row, op, "That message is outside the folders allowed for this conversation.".into()));
                        }
                        match self.host.mail_validity(&m.account, &m.folder) {
                            Ok(Some(v)) if v == m.uidvalidity => {}
                            Ok(_) => return Err(fail(self, row, op, "The folder was reset on the server, so the invitation can't be identified safely.".into())),
                            Err(e) => return Err(fail(self, row, op, e)),
                        }
                        RespondTarget::Message(m.clone())
                    }
                    None => {
                        if !in_scope(&cp.calendar_id) {
                            return Err(fail(self, row, op, "That calendar is outside the calendars allowed for this conversation.".into()));
                        }
                        match self.host.calendar(&cp.calendar_id) {
                            Ok(Some(c)) if !c.read_only => {}
                            Ok(Some(_)) => return Err(fail(self, row, op, "That calendar is read-only.".into())),
                            Ok(None) => return Err(fail(self, row, op, "That calendar no longer exists.".into())),
                            Err(e) => return Err(fail(self, row, op, e)),
                        }
                        match self.host.lookup(&cp.calendar_id, &cp.uid, "") {
                            Ok(Some(c)) if c.version == cp.expected_version => {}
                            Ok(Some(_)) => return Err(fail(self, row, op, "The event was changed after this suggestion was made, so no answer was sent.".into())),
                            Ok(None) => return Err(fail(self, row, op, "The event no longer exists, so no answer was sent.".into())),
                            Err(e) => return Err(fail(self, row, op, e)),
                        }
                        RespondTarget::Stored { calendar_id: cp.calendar_id.clone(), uid: cp.uid.clone() }
                    }
                };
                match self.host.respond_invite(&target, &status).await {
                    Ok(r) => {
                        row.op = op.into();
                        row.uid = cp.uid.clone();
                        row.notified = r.sent;
                        self.log(row);
                        // Saved but not mailed is reported as such (summary, warning and log row all say "not sent").
                        let warning = r.warning.or_else(|| (!r.sent).then(|| "Your answer was saved in your calendar but was NOT sent to the organizer.".to_string()));
                        Ok(Done { summary: if r.sent { "Answer sent to the organizer.".into() } else { "Answer saved, but not sent.".into() }, warning })
                    }
                    Err(e) => Err(fail(self, row, op, e)),
                }
            }
            _ => Err("This is not a calendar suggestion.".into()),
        }
    }

    /// Reverse what a calendar proposal did. A second call retries rows that failed.
    pub async fn undo(&self, id: i64, allowed: &[String], progress: &(dyn Fn(i64) + Send + Sync)) -> Result<CalUndoReport, ProposalError> {
        let _guard = self.claim(id)?;
        let rows = {
            let mut st = lock(&self.store);
            let p = st.get_proposal(id).map_err(db)?.ok_or(ProposalError::NotFound)?;
            if !p.kind.is_calendar() || !p.state.can_become(State::Undone) {
                return Err(ProposalError::Illegal { from: p.state, to: State::Undone });
            }
            let rows: Vec<CalLogRow> = st.list_cal_log(id).map_err(db)?.into_iter().filter(undoable_row).collect();
            if rows.is_empty() {
                return Err(ProposalError::NothingToUndo);
            }
            rows
        };
        let (mut undone, mut failed) = (0usize, 0usize);
        let mut errors: Vec<String> = Vec::new();
        for r in rows {
            let res = self.undo_row(&r, allowed).await;
            let mut st = lock(&self.store);
            match res {
                Ok(()) => {
                    undone += 1;
                    let _ = st.set_cal_log_undone(r.id, 1, "");
                }
                Err(e) => {
                    failed += 1;
                    let _ = st.set_cal_log_undone(r.id, 2, &e);
                    if errors.len() < 5 && !errors.contains(&e) {
                        errors.push(e);
                    }
                }
            }
        }
        let state = if failed == 0 {
            let mut st = lock(&self.store);
            let now = (self.clock)();
            if st.transition_proposal(id, State::Done, State::Undone, now, "").is_err() {
                st.transition_proposal(id, State::PartiallyDone, State::Undone, now, "")?;
            }
            State::Undone
        } else {
            State::Done
        };
        progress(id);
        Ok(CalUndoReport { state, undone, failed, errors })
    }

    async fn undo_row(&self, r: &CalLogRow, allowed: &[String]) -> Result<(), String> {
        if !allowed.iter().any(|a| a == &r.calendar_id) {
            return Err("That calendar is outside the calendars allowed for this conversation.".into());
        }
        let current = self.host.lookup(&r.calendar_id, &r.uid, "")?;
        match r.op.as_str() {
            "create" => match current {
                None => Ok(()), // already gone: nothing to undo
                Some(c) if c.version != r.version_after => Err("The event was changed after it was created, so it was left alone.".into()),
                Some(_) => {
                    let zoom = (!r.zoom_id.is_empty()).then_some(r.zoom_id.as_str());
                    self.host.remove_created(&r.calendar_id, &r.uid, zoom, r.notified).await
                }
            },
            "change" => match current {
                None => Err("The event no longer exists, so the change can't be reverted.".into()),
                Some(c) if c.version != r.version_after => Err("The event was changed again after this change, so it was left alone.".into()),
                Some(_) => self.host.restore_event(&r.snapshot, r.notified).await.map(|_| ()),
            },
            "cancel" => match current {
                Some(_) => Err("An event with this identity exists again, so nothing was restored.".into()),
                None => self.host.restore_event(&r.snapshot, r.notified).await.map(|_| ()),
            },
            _ => Err("This step can't be undone.".into()),
        }
    }
}
