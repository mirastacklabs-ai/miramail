//! iTIP (RFC 5546) helpers: REQUEST / REPLY / CANCEL ICS and human-readable subject/body text.

use crate::error::{Error, Result};
use crate::ics::{build_ics, parse_ics, try_build_ics, try_build_ics_series_opts};
use crate::rrule::event_zone;
use crate::tm;
use crate::types::*;

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The user's own participation status in `ev` (None if not an attendee).
pub fn my_status(ev: &CalEvent, my_emails: &[String]) -> Option<AttendeeStatus> {
    ev.attendees
        .iter()
        .find(|a| my_emails.iter().any(|m| same(m, &a.email)))
        .map(|a| a.status)
}

/// True if the user is the organizer.
pub fn is_organizer(ev: &CalEvent, my_emails: &[String]) -> bool {
    ev.organizer.as_ref().is_some_and(|o| my_emails.iter().any(|m| same(m, &o.email)))
}

/// Set (or add) `email`'s PARTSTAT on the event; used to store the event with the user's answer.
pub fn set_attendee_status(ev: &mut CalEvent, email: &str, name: Option<&str>, status: AttendeeStatus) {
    if let Some(a) = ev.attendees.iter_mut().find(|a| same(&a.email, email)) {
        a.status = status;
    } else {
        ev.attendees.push(Attendee {
            email: email.trim().to_string(),
            name: name.map(String::from),
            role: AttendeeRole::Required,
            status,
        });
    }
}

/// Attendee addresses to notify: every attendee that is not one of the user's own addresses, de-duplicated.
pub fn recipients(ev: &CalEvent, my_emails: &[String]) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for a in &ev.attendees {
        if !my_emails.iter().any(|m| same(m, &a.email)) && !v.iter().any(|x| same(x, &a.email)) {
            v.push(a.email.clone());
        }
    }
    v
}

/// METHOD:REQUEST ICS (new or updated invitation; bump `sequence` before calling for updates).
/// Unchecked: does not verify the user is the organizer. For sending prefer [`try_build_request`].
pub fn build_request(ev: &CalEvent) -> String {
    build_ics(ev, Some("REQUEST"))
}

/// Checked REQUEST for SENDING: only the organizer may send one (`Err` otherwise), the DTSTAMP is
/// refreshed, and the build is strict (no silently dropped data). `schedule_agent_client` adds
/// `SCHEDULE-AGENT=CLIENT` to every ATTENDEE so a CalDAV server does not mail them a second time.
pub fn try_build_request(ev: &CalEvent, my_emails: &[String], schedule_agent_client: bool) -> Result<String> {
    if !is_organizer(ev, my_emails) {
        return Err(Error::Invalid("only the organizer can send an invitation".into()));
    }
    let mut r = ev.clone();
    r.dtstamp = None;
    try_build_ics_series_opts(Some(&r), &[], Some("REQUEST"), schedule_agent_client)
}

/// METHOD:CANCEL ICS (the event is emitted with STATUS:CANCELLED).
/// Unchecked: prefer [`try_build_cancel`] for sending.
pub fn build_cancel(ev: &CalEvent) -> String {
    let mut c = ev.clone();
    c.status = EventStatus::Cancelled;
    c.reminder_minutes = None;
    build_ics(&c, Some("CANCEL"))
}

/// Checked CANCEL for SENDING (organizer only, fresh DTSTAMP, strict build).
pub fn try_build_cancel(ev: &CalEvent, my_emails: &[String]) -> Result<String> {
    if !is_organizer(ev, my_emails) {
        return Err(Error::Invalid("only the organizer can cancel an event for others".into()));
    }
    let mut c = ev.clone();
    c.status = EventStatus::Cancelled;
    c.reminder_minutes = None;
    c.dtstamp = None;
    try_build_ics(&c, Some("CANCEL"))
}

/// METHOD:REPLY ICS from `my_email` with the given PARTSTAT. Needs an organizer to reply to.
pub fn build_reply(ev: &CalEvent, my_email: &str, status: AttendeeStatus) -> Result<String> {
    if ev.organizer.is_none() {
        return Err(Error::Invalid("event has no organizer to reply to".into()));
    }
    let me = ev
        .attendees
        .iter()
        .find(|a| same(&a.email, my_email))
        .cloned()
        .unwrap_or(Attendee { email: my_email.trim().to_string(), name: None, role: AttendeeRole::Required, status });
    let mut r = ev.clone();
    r.attendees = vec![Attendee { status, ..me }];
    // A REPLY carries only what identifies the instance: drop content the organizer owns.
    r.description.clear();
    r.location.clear();
    r.zoom = None;
    r.reminder_minutes = None;
    r.exdates.clear();
    r.status = EventStatus::Confirmed;
    r.dtstamp = None;
    try_build_ics(&r, Some("REPLY"))
}

// ---------------------------------------------------------------------------------------------
// Applying inbound iTIP messages. `sender_email` is the authenticated From address of the mail
// carrying the part; every function refuses to act for anyone else.
// ---------------------------------------------------------------------------------------------

/// Apply a REPLY to the stored event: only `sender_email`'s own PARTSTAT (and display name) change.
/// A REPLY must never be imported as an event. For a reply to ONE instance of a series, pass the
/// stored override (not the master) as `stored`.
pub fn apply_reply(stored: &CalEvent, reply: &CalEvent, sender_email: &str) -> Result<CalEvent> {
    if stored.uid != reply.uid {
        return Err(Error::Invalid("reply is for a different event".into()));
    }
    if reply.sequence < stored.sequence {
        return Err(Error::Invalid("stale reply".into()));
    }
    let ra = reply
        .attendees
        .iter()
        .find(|a| same(&a.email, sender_email))
        .ok_or_else(|| Error::Invalid("reply does not list its sender".into()))?;
    let mut out = stored.clone();
    let a = out
        .attendees
        .iter_mut()
        .find(|a| same(&a.email, sender_email))
        .ok_or_else(|| Error::Invalid("sender is not an attendee of this event".into()))?;
    a.status = ra.status;
    if let Some(n) = ra.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        a.name = Some(n.to_string());
    }
    Ok(out)
}

/// Apply a whole-event CANCEL: only the stored organizer may cancel.
pub fn apply_cancel(stored: &CalEvent, cancel: &CalEvent, sender_email: &str) -> Result<CalEvent> {
    if stored.uid != cancel.uid {
        return Err(Error::Invalid("cancellation is for a different event".into()));
    }
    let org = stored.organizer.as_ref().ok_or_else(|| Error::Invalid("stored event has no organizer".into()))?;
    if !same(&org.email, sender_email) {
        return Err(Error::Invalid("sender is not the organizer".into()));
    }
    if cancel.sequence < stored.sequence {
        return Err(Error::Invalid("stale cancellation".into()));
    }
    let mut out = stored.clone();
    out.status = EventStatus::Cancelled;
    out.sequence = cancel.sequence.max(stored.sequence);
    Ok(out)
}

/// Apply a REQUEST: the sender must be the organizer (and the stored organizer when updating); the
/// update is accepted only if `SEQUENCE >= stored` and, on equal sequence, DTSTAMP is not older.
/// Local-only data (calendar, href/etag, Zoom link) is carried over; invites never bring Zoom data.
pub fn apply_request(stored: Option<&CalEvent>, incoming: &CalEvent, sender_email: &str) -> Result<CalEvent> {
    let org = incoming.organizer.as_ref().ok_or_else(|| Error::Invalid("request has no organizer".into()))?;
    if !same(&org.email, sender_email) {
        return Err(Error::Invalid("sender is not the organizer".into()));
    }
    let mut out = incoming.clone();
    out.zoom = None;
    if let Some(st) = stored {
        if st.uid != incoming.uid {
            return Err(Error::Invalid("request is for a different event".into()));
        }
        if let Some(so) = &st.organizer {
            if !same(&so.email, &org.email) {
                return Err(Error::Invalid("organizer changed".into()));
            }
        }
        if incoming.sequence < st.sequence {
            return Err(Error::Invalid("stale request (older SEQUENCE)".into()));
        }
        if incoming.sequence == st.sequence {
            let newer = |a: &Option<String>, b: &Option<String>| match (a.as_deref().and_then(tm::parse_wire_dt), b.as_deref().and_then(tm::parse_wire_dt)) {
                (Some(x), Some(y)) => x >= y,
                _ => true,
            };
            if !newer(&incoming.dtstamp, &st.dtstamp) {
                return Err(Error::Invalid("stale request (older DTSTAMP)".into()));
            }
        }
        out.calendar_id = st.calendar_id.clone();
        out.href = st.href.clone();
        out.etag = st.etag.clone();
        out.zoom = st.zoom.clone();
    }
    Ok(out)
}

/// What to write after applying an invite to a series.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesUpdate {
    /// Master / standalone event to upsert, if it changed.
    pub master: Option<CalEvent>,
    /// RECURRENCE-ID overrides to upsert (`Calendar::upsert_override`).
    pub overrides: Vec<EventOverride>,
    /// `recurrence_id`s of stored overrides to delete (`Calendar::delete_override`).
    pub removed_overrides: Vec<String>,
}

/// Do two RECURRENCE-ID / EXDATE wire strings name the same instance (same instant, or same day for dates)?
pub fn rid_eq(a: &str, b: &str) -> bool {
    a == b
        || match (tm::parse_wire_dt(a), tm::parse_wire_dt(b)) {
            (Some(x), Some(y)) => x == y,
            _ => (a.len() == 10 && b.get(..10) == Some(a)) || (b.len() == 10 && a.get(..10) == Some(b)),
        }
}

fn instance_override(stored_master: Option<&CalEvent>, stored_overrides: &[EventOverride], rid: &str, ev: &CalEvent, sender: &str) -> Result<EventOverride> {
    let prev = stored_overrides.iter().find(|o| rid_eq(&o.recurrence_id, rid));
    // the master (if any) fixes the organizer for the whole series
    if let (Some(m), Some(o)) = (stored_master, ev.organizer.as_ref()) {
        if m.organizer.as_ref().is_some_and(|mo| !same(&mo.email, &o.email)) {
            return Err(Error::Invalid("organizer changed".into()));
        }
    }
    let mut e = apply_request(prev.map(|o| &o.event), ev, sender)?;
    if let Some(m) = stored_master {
        e.uid = m.uid.clone();
        e.calendar_id = m.calendar_id.clone();
        e.href = m.href.clone();
        e.etag = m.etag.clone();
    }
    Ok(EventOverride { recurrence_id: prev.map(|o| o.recurrence_id.clone()).unwrap_or_else(|| rid.to_string()), event: e })
}

/// Apply a REQUEST/PUBLISH [`Invite`] to a series. A whole-series invite updates the master (and any
/// overrides it carries); a single-instance invite (`invite.recurrence_id`) becomes an OVERRIDE and
/// never replaces the master -- without a stored master it is kept as an override-only resource.
pub fn apply_request_series(stored_master: Option<&CalEvent>, stored_overrides: &[EventOverride], invite: &Invite, sender_email: &str) -> Result<SeriesUpdate> {
    let mut up = SeriesUpdate::default();
    let master_for_ids;
    match &invite.recurrence_id {
        None => {
            let m = apply_request(stored_master, &invite.event, sender_email)?;
            master_for_ids = Some(m.clone());
            up.master = Some(m);
        }
        Some(rid) => {
            up.overrides.push(instance_override(stored_master, stored_overrides, rid, &invite.event, sender_email)?);
            master_for_ids = stored_master.cloned();
        }
    }
    for o in &invite.overrides {
        up.overrides.push(instance_override(master_for_ids.as_ref(), stored_overrides, &o.recurrence_id, &o.event, sender_email)?);
    }
    Ok(up)
}

/// Apply a CANCEL [`Invite`] to a series. Whole-series: the master becomes CANCELLED. Single
/// instance: the instance is added to the master's EXDATEs and its stored override (if any) removed;
/// the master is NOT cancelled. Only the stored organizer may cancel.
pub fn apply_cancel_series(stored_master: Option<&CalEvent>, stored_overrides: &[EventOverride], invite: &Invite, sender_email: &str) -> Result<SeriesUpdate> {
    let mut up = SeriesUpdate::default();
    // (recurrence id, SEQUENCE of the cancelling component)
    let mut rids: Vec<(&str, i64)> = Vec::new();
    match &invite.recurrence_id {
        None => {
            let st = stored_master.ok_or_else(|| Error::NotFound("event".into()))?;
            up.master = Some(apply_cancel(st, &invite.event, sender_email)?);
        }
        Some(r) => rids.push((r, invite.event.sequence)),
    }
    rids.extend(invite.overrides.iter().map(|o| (o.recurrence_id.as_str(), o.event.sequence)));
    if rids.is_empty() {
        return Ok(up);
    }
    // authority: the stored master's organizer, else the stored override's
    let org = stored_master
        .and_then(|m| m.organizer.as_ref())
        .or_else(|| stored_overrides.iter().find_map(|o| o.event.organizer.as_ref()));
    let Some(org) = org else {
        return if stored_master.is_none() && stored_overrides.is_empty() { Ok(up) } else { Err(Error::Invalid("stored event has no organizer".into())) };
    };
    if !same(&org.email, sender_email) {
        return Err(Error::Invalid("sender is not the organizer".into()));
    }
    let mut master = up.master.take().or_else(|| stored_master.cloned());
    // A replayed (older) instance cancellation must not delete a newer version of the instance.
    for (rid, seq) in &rids {
        let floor = stored_overrides.iter().find(|o| rid_eq(&o.recurrence_id, rid)).map(|o| o.event.sequence).or_else(|| stored_master.map(|m| m.sequence));
        if floor.is_some_and(|f| *seq < f) {
            return Err(Error::Invalid("stale cancellation".into()));
        }
    }
    for (rid, _) in rids {
        if let Some(o) = stored_overrides.iter().find(|o| rid_eq(&o.recurrence_id, rid)) {
            up.removed_overrides.push(o.recurrence_id.clone());
        }
        if let Some(m) = master.as_mut() {
            if !m.exdates.iter().any(|x| rid_eq(x, rid)) {
                m.exdates.push(rid.to_string());
            }
        }
    }
    up.master = master;
    Ok(up)
}

/// Parse an ICS part (an email attachment: UNTRUSTED) into an [`Invite`]. `Ok(None)` if it contains
/// no event. Inbound `X-MIRAMAIL-ZOOM-*` properties are ignored. A part that only carries
/// RECURRENCE-ID components is a single-instance invite: `event` is the first instance and
/// `recurrence_id` is set; route it with [`apply_request_series`] / [`apply_cancel_series`].
pub fn invite_from_ics(data: &[u8], my_emails: &[String], attachment_id: &str, already_in_calendar: bool) -> Result<Option<Invite>> {
    let p = parse_ics(data)?;
    let method = p.method.as_deref().and_then(InviteMethod::parse).unwrap_or(InviteMethod::Publish);
    let mut overrides = p.overrides;
    let (event, recurrence_id) = match p.events.into_iter().next() {
        Some(m) => (m, None),
        None if !overrides.is_empty() => {
            let o = overrides.remove(0);
            (o.event, Some(o.recurrence_id))
        }
        None => return Ok(None),
    };
    Ok(Some(Invite {
        method,
        my_status: my_status(&event, my_emails),
        event,
        already_in_calendar,
        attachment_id: attachment_id.to_string(),
        recurrence_id,
        overrides,
        sender_unverified: false,
    }))
}

/// Strip control characters (CR/LF/tab included) and bidi overrides, and cap the length, for any
/// event text that ends up in a mail subject or body.
pub(crate) fn safe_text(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    for c in s.chars() {
        if matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}') {
            continue;
        }
        if n >= max {
            out.push('\u{2026}');
            break;
        }
        out.push(if c.is_control() { ' ' } else { c });
        n += 1;
    }
    out.trim().to_string()
}

const MAX_SUMMARY: usize = 200;
const MAX_LOCATION: usize = 300;
const MAX_NAME: usize = 100;
const MAX_URL: usize = 500;

/// Human-readable time range in the event's zone, e.g. `Fri Nov 6, 2026 15:00-16:00 (Asia/Kolkata)`.
pub fn describe_when(ev: &CalEvent) -> String {
    if ev.all_day {
        let s = tm::parse_wire_date(&ev.start);
        let e = tm::parse_wire_date(&ev.end).and_then(|d| d.pred_opt());
        return match (s, e) {
            (Some(s), Some(e)) if e > s => format!("{} - {} (all day)", s.format("%a %b %-d, %Y"), e.format("%a %b %-d, %Y")),
            (Some(s), _) => format!("{} (all day)", s.format("%a %b %-d, %Y")),
            _ => String::new(),
        };
    }
    let z = event_zone(ev);
    let (Some(s), Some(e)) = (tm::parse_wire_dt(&ev.start), tm::parse_wire_dt(&ev.end)) else { return String::new() };
    let (s, e) = (z.at(&s), z.at(&e));
    let tzn = ev.tzid.as_deref().map(|t| safe_text(t, 64)).unwrap_or_else(|| "floating".to_string());
    if s.date_naive() == e.date_naive() {
        format!("{} {}-{} ({tzn})", s.format("%a %b %-d, %Y"), s.format("%H:%M"), e.format("%H:%M"))
    } else {
        format!("{} {} - {} {} ({tzn})", s.format("%a %b %-d, %Y"), s.format("%H:%M"), e.format("%a %b %-d, %Y"), e.format("%H:%M"))
    }
}

pub fn request_subject(ev: &CalEvent) -> String {
    let when = describe_when(ev);
    let prefix = if ev.sequence > 0 { "Updated invitation" } else { "Invitation" };
    if when.is_empty() {
        format!("{prefix}: {}", safe_text(&ev.summary, MAX_SUMMARY))
    } else {
        format!("{prefix}: {} @ {when}", safe_text(&ev.summary, MAX_SUMMARY))
    }
}

pub fn cancel_subject(ev: &CalEvent) -> String {
    format!("Canceled event: {}", safe_text(&ev.summary, MAX_SUMMARY))
}

pub fn reply_subject(ev: &CalEvent, status: AttendeeStatus) -> String {
    let p = match status {
        AttendeeStatus::Accepted => "Accepted",
        AttendeeStatus::Declined => "Declined",
        AttendeeStatus::Tentative => "Tentative",
        AttendeeStatus::NeedsAction => "Response pending",
    };
    format!("{p}: {}", safe_text(&ev.summary, MAX_SUMMARY))
}

fn details(ev: &CalEvent) -> String {
    let mut s = format!("What: {}\nWhen: {}\n", safe_text(&ev.summary, MAX_SUMMARY), describe_when(ev));
    if !ev.location.is_empty() {
        s.push_str(&format!("Where: {}\n", safe_text(&ev.location, MAX_LOCATION)));
    }
    if let Some(z) = &ev.zoom {
        s.push_str(&format!("Zoom: {}\n", safe_text(&z.join_url, MAX_URL)));
        if let Some(p) = &z.password {
            s.push_str(&format!("Passcode: {}\n", safe_text(p, 64)));
        }
    }
    if let Some(o) = &ev.organizer {
        s.push_str(&format!("Organizer: {}\n", safe_text(o.name.as_deref().unwrap_or(&o.email), MAX_NAME)));
    }
    if ev.rrule.is_some() {
        s.push_str("Repeats: yes\n");
    }
    s
}

pub fn request_body(ev: &CalEvent) -> String {
    let head = if ev.sequence > 0 { "This invitation has been updated." } else { "You have been invited to an event." };
    format!("{head}\n\n{}", details(ev))
}

pub fn cancel_body(ev: &CalEvent) -> String {
    format!("This event has been canceled.\n\n{}", details(ev))
}

pub fn reply_body(ev: &CalEvent, who: &str, status: AttendeeStatus) -> String {
    let v = match status {
        AttendeeStatus::Accepted => "accepted",
        AttendeeStatus::Declined => "declined",
        AttendeeStatus::Tentative => "tentatively accepted",
        AttendeeStatus::NeedsAction => "not yet responded to",
    };
    format!("{} has {v} this invitation.\n\n{}", safe_text(who, MAX_NAME), details(ev))
}
