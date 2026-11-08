//! `CalendarHost` / `CalActionHost` over the calendar database (AI chat, calendar tools).
//!
//! Reading is local: events come from `calendar.db` (recurrences expanded by the calendar crate), busy
//! blocks and free slots are computed by `calendar::avail` in the user's IANA zone, so DST, working hours,
//! buffers and all-day events are handled by tested app code and never by the model. The only network
//! paths are the ones the calendar already has: the CalDAV push of a write the user approved, the iTIP
//! mail of an approved invitation / update / cancellation / answer (SMTP through the user's own account),
//! and Zoom, only when the user connected it and asked for a meeting on an approved card.
//!
//! The write half ([`CalActionHost`]) is called ONLY by `aichat::calexec::CalExecutor`; every write goes
//! through `cal_logic::{save_event, delete_event, respond_invite}`, the same functions the Calendar UI
//! uses, and keeps their rules (organizer-only invitations, read-only calendars, per-calendar locks).
//!
//! Never logs; errors are generic strings with no event content.

use std::collections::HashMap;
use std::sync::Arc;

use aichat::cal::*;
use aichat::envelope::clean_line;
use aichat::tools::{BoxFut, Scope};
use calendar::avail::{self, Busy, SlotQuery, Tz, WorkHours};
use calendar::itip;
use calendar::{Attendee, AttendeeRole, AttendeeStatus, CalEvent, EventStatus, Invite, InviteMethod, Occurrence, ZoomMeeting, DEFAULT_CALENDAR_ID};
use chrono::{DateTime, Datelike, Duration, Offset, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Timelike};
use mailcore::OutgoingMessage;

use crate::cal_logic::{self, SaveOutcome};
use crate::cal_state::{CalDavRemote, CalState, ZoomRemote};

/// Slots offered per local day.
const SLOTS_PER_DAY: usize = 3;
const SLOT_GRID_SECS: i64 = 15 * 60;
/// Longest event the assistant may propose.
const MAX_EVENT_DAYS: i64 = 14;
const DEFAULT_DURATION_MIN: i64 = 60;
const MAX_CONFLICTS: usize = 10;
/// Occurrences scanned per read (the calendar crate already caps its own work).
const MAX_ROWS: usize = 2000;

// ------------------------------------------------------------------------------------------
// Environment seam
// ------------------------------------------------------------------------------------------

/// Everything the host needs from the running app. Production: [`AppCalEnv`]; tests: an in-memory fake.
pub trait CalEnv: Send + Sync + 'static {
    fn cal(&self) -> &CalState;
    /// The user's own mail addresses.
    fn mine(&self) -> Vec<String>;
    fn now(&self) -> i64;
    fn user_tz(&self) -> Tz;
    fn working_hours(&self) -> WorkHours {
        WorkHours::default()
    }
    fn address_known(&self, address: &str) -> bool;
    /// Send one iTIP mail from the account `from` (an own address) over SMTP. Only reached after an approval.
    fn send_itip(&self, from: Option<String>, msg: OutgoingMessage) -> BoxFut<'_, Result<(), String>>;
    fn zoom_connected(&self) -> bool;
    fn zoom_create(&self, topic: &str, start_rfc3339: &str, minutes: u32, tz: &str) -> BoxFut<'_, Result<ZoomMeeting, String>>;
    fn read_raw_message(&self, account: &str, folder: &str, uid: u32) -> BoxFut<'_, Result<Vec<u8>, String>>;
    fn mail_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String>;
    /// Tell the UI the calendar changed.
    fn changed(&self);
}

pub struct CalAppHost<E: CalEnv, R: CalDavRemote + 'static, Z: ZoomRemote + 'static> {
    env: Arc<E>,
    dav: R,
    zoom: Z,
}

impl<E: CalEnv, R: CalDavRemote + 'static, Z: ZoomRemote + 'static> CalAppHost<E, R, Z> {
    pub fn new(env: Arc<E>, dav: R, zoom: Z) -> Self {
        CalAppHost { env, dav, zoom }
    }
}

// ------------------------------------------------------------------------------------------
// Time helpers (pure)
// ------------------------------------------------------------------------------------------

fn fnv(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0xff)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// Changes whenever anything the assistant could have based a suggestion on changes.
pub fn version_of(ev: &CalEvent) -> String {
    let att: String = ev.attendees.iter().map(|a| format!("{}={:?};", a.email.to_ascii_lowercase(), a.status)).collect();
    let seq = ev.sequence.to_string();
    let st = format!("{:?}", ev.status);
    format!(
        "{}:{:016x}",
        ev.etag.as_deref().unwrap_or("-"),
        fnv(&[&ev.summary, &ev.start, &ev.end, ev.tzid.as_deref().unwrap_or(""), &ev.location, &ev.description, ev.rrule.as_deref().unwrap_or(""), &att, &seq, &st])
    )
}

fn local_dt(ts: i64, tz: &Tz) -> DateTime<Tz> {
    tz.timestamp_opt(ts, 0).single().unwrap_or_else(|| tz.timestamp_opt(0, 0).unwrap())
}

fn midnight_ts(d: NaiveDate, tz: &Tz) -> Option<i64> {
    avail::local_instant(tz, d.and_hms_opt(0, 0, 0)?)
}

/// `Thu 12 Nov 13:00-14:00` (en dash), or both ends when the event crosses midnight; all-day events show dates.
pub fn describe_span(s_ts: i64, e_ts: i64, all_day: bool, tz: &Tz) -> String {
    let (s, e) = (local_dt(s_ts, tz), local_dt(e_ts, tz));
    if all_day {
        let last = (e - Duration::days(1)).date_naive().max(s.date_naive());
        return if last > s.date_naive() { format!("{} \u{2013} {} (all day)", s.format("%a %-d %b"), last.format("%a %-d %b")) } else { format!("{} (all day)", s.format("%a %-d %b")) };
    }
    if s.date_naive() == e.date_naive() {
        format!("{} {}\u{2013}{}", s.format("%a %-d %b"), s.format("%H:%M"), e.format("%H:%M"))
    } else {
        format!("{} {} \u{2013} {} {}", s.format("%a %-d %b"), s.format("%H:%M"), e.format("%a %-d %b"), e.format("%H:%M"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Point {
    Date(NaiveDate),
    Instant(i64),
}

fn parse_naive(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    for f in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, f) {
            return Some(n);
        }
    }
    None
}

const TIME_FORMAT_HELP: &str = "Use YYYY-MM-DD, YYYY-MM-DDTHH:MM (the user's time zone) or RFC 3339 with an offset.";

/// A time the model wrote, read in `tz` unless it carries its own offset.
fn parse_point(s: &str, tz: &Tz) -> Result<Point, String> {
    let s = s.trim();
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Point::Date(d));
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(Point::Instant(dt.timestamp()));
    }
    if let Some(n) = parse_naive(s) {
        return avail::local_instant(tz, n).map(Point::Instant).ok_or_else(|| "That local time does not exist in this time zone.".to_string());
    }
    Err(format!("\"{}\" is not a valid time. {TIME_FORMAT_HELP}", clean_line(s, 40)))
}

/// A range: a date-only `to` includes that whole day. At most 62 days.
fn parse_range(from: &str, to: &str, tz: &Tz) -> Result<(i64, i64), String> {
    let f = match parse_point(from, tz)? {
        Point::Date(d) => midnight_ts(d, tz).ok_or("Invalid start.")?,
        Point::Instant(t) => t,
    };
    let t = match parse_point(to, tz)? {
        Point::Date(d) => midnight_ts(d + Duration::days(1), tz).ok_or("Invalid end.")?,
        Point::Instant(t) => t,
    };
    if t <= f {
        return Err("The end of the range must be after its start.".into());
    }
    if t - f > avail::MAX_RANGE_DAYS * 86_400 + 3_600 {
        return Err(format!("The range is longer than {} days. Ask for a shorter range or several calls.", avail::MAX_RANGE_DAYS));
    }
    Ok((f, t))
}

fn iso_local(ts: i64, tz: &Tz) -> String {
    local_dt(ts, tz).format("%Y-%m-%dT%H:%M").to_string()
}

fn rfc3339_in(ts: i64, tz: &Tz) -> String {
    local_dt(ts, tz).to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn timed_when(s_ts: i64, e_ts: i64, tz: &Tz, tz_name: &str, display_tz: &Tz) -> Result<When, String> {
    if e_ts <= s_ts {
        return Err("The event must end after it starts.".into());
    }
    if e_ts - s_ts > MAX_EVENT_DAYS * 86_400 {
        return Err(format!("An event can be at most {MAX_EVENT_DAYS} days long."));
    }
    Ok(When {
        start: rfc3339_in(s_ts, tz),
        end: rfc3339_in(e_ts, tz),
        tzid: Some(tz_name.to_string()),
        all_day: false,
        start_ts: s_ts,
        end_ts: e_ts,
        display: describe_span(s_ts, e_ts, false, display_tz),
    })
}

fn allday_when(first: NaiveDate, last: NaiveDate, tz: &Tz) -> Result<When, String> {
    if last < first {
        return Err("The last day is before the first day.".into());
    }
    if (last - first).num_days() >= MAX_EVENT_DAYS {
        return Err(format!("An event can be at most {MAX_EVENT_DAYS} days long."));
    }
    let end_excl = last + Duration::days(1);
    let (s_ts, e_ts) = (midnight_ts(first, tz).ok_or("Invalid date.")?, midnight_ts(end_excl, tz).ok_or("Invalid date.")?);
    Ok(When {
        start: first.format("%Y-%m-%d").to_string(),
        end: end_excl.format("%Y-%m-%d").to_string(),
        tzid: None,
        all_day: true,
        start_ts: s_ts,
        end_ts: e_ts,
        display: describe_span(s_ts, e_ts, true, tz),
    })
}

fn zone_named(name: Option<&str>, fallback: &Tz) -> Result<(Tz, String), String> {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => avail::parse_tz(n).map(|t| (t, n.to_string())).ok_or_else(|| format!("\"{}\" is not a known IANA time zone name (for example Europe/Berlin).", clean_line(n, 40))),
        None => Ok((*fallback, fallback.name().to_string())),
    }
}

// ------------------------------------------------------------------------------------------
// Rows
// ------------------------------------------------------------------------------------------

fn status_str(s: AttendeeStatus) -> &'static str {
    match s {
        AttendeeStatus::Accepted => "accepted",
        AttendeeStatus::Declined => "declined",
        AttendeeStatus::Tentative => "tentative",
        AttendeeStatus::NeedsAction => "needs-action",
    }
}

fn person(a: &Attendee) -> String {
    match a.name.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(n) => format!("{n} <{}>", a.email),
        None => a.email.clone(),
    }
}

/// The instant range of an occurrence of `ev` (floating times are read in the user's zone).
fn occurrence_span(occ_start: &str, occ_end: &str, ev: &CalEvent, tz: &Tz) -> Option<(i64, i64)> {
    if ev.all_day {
        let d = |s: &str| NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok();
        let (s, e) = (d(occ_start)?, d(occ_end).or_else(|| d(occ_start).map(|x| x + Duration::days(1)))?);
        return Some((midnight_ts(s, tz)?, midnight_ts(e.max(s + Duration::days(1)), tz)?));
    }
    let inst = |s: &str| -> Option<i64> {
        if ev.tzid.is_none() {
            let n = NaiveDateTime::parse_from_str(s.get(..19)?, "%Y-%m-%dT%H:%M:%S").ok()?;
            avail::local_instant(tz, n)
        } else {
            DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp())
        }
    };
    let (s, e) = (inst(occ_start)?, inst(occ_end).unwrap_or_else(|| inst(occ_start).unwrap_or(0)));
    Some((s, e.max(s)))
}

struct Ctx<'a> {
    tz: Tz,
    mine: &'a [String],
}

fn build_row(ev: &CalEvent, occ: Option<&Occurrence>, cx: &Ctx<'_>, ignore_tentative: bool) -> Option<EventRow> {
    let (os, oe) = match occ {
        Some(o) => (o.start.as_str(), o.end.as_str()),
        None => (ev.start.as_str(), ev.end.as_str()),
    };
    let (s_ts, e_ts) = occurrence_span(os, oe, ev, &cx.tz)?;
    let organizer = itip::is_organizer(ev, cx.mine);
    let mine = itip::my_status(ev, cx.mine);
    let my_status = if organizer {
        "organizer"
    } else {
        match mine {
            Some(s) => status_str(s),
            None => "none",
        }
    };
    let declined = mine == Some(AttendeeStatus::Declined);
    let tentative = ev.status == EventStatus::Tentative || mine == Some(AttendeeStatus::Tentative);
    let busy = !declined && !(tentative && ignore_tentative) && ev.status != EventStatus::Cancelled;
    let recurring = ev.rrule.is_some() || occ.is_some_and(|o| o.is_recurring);
    Some(EventRow {
        calendar_id: ev.calendar_id.clone(),
        uid: ev.uid.clone(),
        rid: if recurring { occ.map(|o| o.recurrence_start.clone()).unwrap_or_default() } else { String::new() },
        title: ev.summary.clone(),
        location: ev.location.clone(),
        description: ev.description.clone(),
        when: describe_span(s_ts, e_ts, ev.all_day, &cx.tz),
        start: if ev.all_day { local_dt(s_ts, &cx.tz).format("%Y-%m-%d").to_string() } else { iso_local(s_ts, &cx.tz) },
        end: if ev.all_day { local_dt(e_ts, &cx.tz).format("%Y-%m-%d").to_string() } else { iso_local(e_ts, &cx.tz) },
        start_ts: s_ts,
        end_ts: e_ts,
        all_day: ev.all_day,
        recurring,
        organizer: ev.organizer.as_ref().map(person).unwrap_or_default(),
        attendees: ev.attendees.iter().take(MAX_ATTENDEES_SHOWN + 1).map(|a| AttendeeRow { name: a.name.clone().unwrap_or_default(), email: a.email.clone(), status: status_str(a.status).into() }).collect(),
        attendee_count: ev.attendees.len(),
        my_status: my_status.into(),
        busy,
        tentative,
        has_zoom: ev.zoom.is_some() || ev.location.contains("zoom.us/j/") || ev.description.contains("zoom.us/j/"),
        tzid: ev.tzid.clone(),
        version: version_of(ev),
    })
}

impl<E: CalEnv, R: CalDavRemote + 'static, Z: ZoomRemote + 'static> CalAppHost<E, R, Z> {
    fn rows_between(&self, calendars: &[String], from_ts: i64, to_ts: i64, ignore_tentative: bool) -> Result<(Vec<EventRow>, usize), String> {
        let tz = self.env.user_tz();
        let mine = self.env.mine();
        let cx = Ctx { tz, mine: &mine };
        // A day of margin on both sides: floating events are matched by wall-clock digits.
        let (a, b) = (rfc3339_in(from_ts - 86_400, &Tz::UTC), rfc3339_in(to_ts + 86_400, &Tz::UTC));
        let occs = self.env.cal().db(|c| c.list_occurrences(&a, &b, false))?;
        let mut cache: HashMap<(String, String), Option<CalEvent>> = HashMap::new();
        let mut out = Vec::new();
        let mut total = 0usize;
        for o in occs.iter().filter(|o| calendars.contains(&o.calendar_id)) {
            let ev = cache.entry((o.uid.clone(), o.calendar_id.clone())).or_insert_with(|| self.env.cal().db(|c| c.get_event(&o.uid, &o.calendar_id)).ok().flatten()).clone();
            // An override-only series has no master: build the row from the occurrence itself.
            let ev = ev.unwrap_or_else(|| CalEvent { uid: o.uid.clone(), calendar_id: o.calendar_id.clone(), summary: o.summary.clone(), location: o.location.clone(), start: o.start.clone(), end: o.end.clone(), all_day: o.all_day, tzid: Some("UTC".into()), ..CalEvent::default() });
            let Some(row) = build_row(&ev, Some(o), &cx, ignore_tentative) else { continue };
            let ends_before = row.end_ts <= from_ts && !(row.end_ts == row.start_ts && row.start_ts >= from_ts);
            if ends_before || row.start_ts >= to_ts {
                continue;
            }
            total += 1;
            if out.len() < MAX_ROWS {
                out.push(row);
            }
        }
        out.sort_by(|x, y| (x.start_ts, &x.title, &x.uid).cmp(&(y.start_ts, &y.title, &y.uid)));
        Ok((out, total))
    }
}

fn tz_label(tz: &Tz) -> String {
    tz.name().to_string()
}

// ------------------------------------------------------------------------------------------
// Read side
// ------------------------------------------------------------------------------------------

impl<E: CalEnv, R: CalDavRemote + 'static, Z: ZoomRemote + 'static> CalendarHost for CalAppHost<E, R, Z> {
    fn now(&self) -> i64 {
        self.env.now()
    }

    fn calendars(&self) -> Result<Vec<CalMeta>, String> {
        Ok(self
            .env
            .cal()
            .db(|c| c.list_calendars())?
            .into_iter()
            .map(|c| CalMeta { is_default: c.id == DEFAULT_CALENDAR_ID, kind: format!("{:?}", c.kind).to_ascii_lowercase(), id: c.id, name: c.name, read_only: c.read_only })
            .collect())
    }

    fn list_events(&self, calendars: &[String], from: &str, to: &str) -> Result<EventList, String> {
        let tz = self.env.user_tz();
        let (f, t) = parse_range(from, to, &tz)?;
        let (mut events, total) = self.rows_between(calendars, f, t, false)?;
        events.truncate(MAX_EVENTS_PER_RESULT);
        Ok(EventList { events, total, tz_name: tz_label(&tz) })
    }

    fn get_event(&self, calendar_id: &str, uid: &str, rid: &str) -> Result<Option<EventRow>, String> {
        let tz = self.env.user_tz();
        let Some(ev) = self.env.cal().db(|c| c.get_event(uid, calendar_id))? else { return Ok(None) };
        let mine = self.env.mine();
        let cx = Ctx { tz, mine: &mine };
        if rid.is_empty() {
            return Ok(build_row(&ev, None, &cx, false));
        }
        // One occurrence of a series: find it around its original start.
        let center = match parse_point(rid, &tz) {
            Ok(Point::Instant(t)) => t,
            Ok(Point::Date(d)) => midnight_ts(d, &tz).unwrap_or(0),
            Err(e) => return Err(e),
        };
        let (rows, _) = self.rows_between(&[calendar_id.to_string()], center - 86_400, center + 2 * 86_400, false)?;
        Ok(rows.into_iter().find(|r| r.uid == uid && (r.rid == rid || itip::rid_eq(&r.rid, rid))).map(|mut r| {
            r.description = ev.description.clone();
            r
        }))
    }

    fn free_busy(&self, calendars: &[String], from: &str, to: &str, ignore_tentative: bool) -> Result<BusyOut, String> {
        let tz = self.env.user_tz();
        let (f, t) = parse_range(from, to, &tz)?;
        let (rows, _) = self.rows_between(calendars, f, t, ignore_tentative)?;
        let blocks: Vec<Busy> = rows.iter().filter(|r| r.busy && !r.all_day).map(|r| Busy { start: r.start_ts.max(f), end: r.end_ts.min(t), tentative: r.tentative }).collect();
        let merged = avail::merge_busy(&blocks);
        let fmt = |ts: i64| local_dt(ts, &tz).format("%Y-%m-%d %H:%M").to_string();
        Ok(BusyOut {
            blocks: merged.iter().map(|b| BusyBlock { start: fmt(b.start), end: fmt(b.end), tentative: b.tentative }).collect(),
            all_day: rows.iter().filter(|r| r.all_day && r.my_status != "declined").map(|r| (r.when.to_string(), r.title.clone())).take(60).collect(),
            tz_name: tz_label(&tz),
        })
    }

    fn find_free_slots(&self, calendars: &[String], req: &SlotReq) -> Result<SlotsOut, String> {
        let tz = self.env.user_tz();
        let (f, t) = parse_range(&req.from, &req.to, &tz)?;
        if !(5..=480).contains(&req.duration_minutes) {
            return Err("duration_minutes must be between 5 and 480.".into());
        }
        // Events just outside the window still block slots near its edges once the buffer is applied.
        let margin = i64::from(MAX_BUFFER_MINUTES) * 60;
        let (rows, _) = self.rows_between(calendars, f - margin, t + margin, req.ignore_tentative)?;
        let blocks: Vec<Busy> = rows.iter().filter(|r| r.busy && !r.all_day).map(|r| Busy { start: r.start_ts, end: r.end_ts, tentative: r.tentative }).collect();
        let wh = self.env.working_hours();
        let now = self.env.now();
        let q = SlotQuery {
            from: f,
            to: t,
            duration: i64::from(req.duration_minutes) * 60,
            buffer: i64::from(req.buffer_minutes.min(MAX_BUFFER_MINUTES)) * 60,
            work: req.working_hours_only.then_some(wh),
            max_results: (req.max_results as usize).clamp(1, MAX_SLOTS),
            per_day: SLOTS_PER_DAY,
            not_before: now,
            grid: SLOT_GRID_SECS,
        };
        let slots = avail::find_slots(&tz, &blocks, &q);
        let mut note = String::new();
        if t <= now {
            note = "That whole range is in the past.".into();
        } else if rows.iter().any(|r| r.all_day && r.busy) {
            note = "All-day events were not treated as busy.".into();
        }
        Ok(SlotsOut {
            slots: slots.iter().map(|(a, b)| SlotOut { start: iso_local(*a, &tz), end: iso_local(*b, &tz), when: describe_span(*a, *b, false, &tz) }).collect(),
            tz_name: tz_label(&tz),
            working_hours: wh.describe(),
            note,
        })
    }

    fn resolve_when(&self, start: &str, end: Option<&str>, tz: Option<&str>) -> Result<When, String> {
        let user = self.env.user_tz();
        let (ztz, zname) = zone_named(tz, &user)?;
        match parse_point(start, &ztz)? {
            Point::Date(first) => {
                let last = match end.map(str::trim).filter(|e| !e.is_empty()) {
                    None => first,
                    Some(e) => match parse_point(e, &ztz)? {
                        Point::Date(d) => d,
                        Point::Instant(_) => return Err("An all-day event needs its last day as a date (YYYY-MM-DD), or give start and end as times.".into()),
                    },
                };
                allday_when(first, last, &user)
            }
            Point::Instant(s) => {
                let e = match end.map(str::trim).filter(|e| !e.is_empty()) {
                    None => s + DEFAULT_DURATION_MIN * 60,
                    Some(e) => match parse_point(e, &ztz)? {
                        Point::Instant(t) => t,
                        Point::Date(_) => return Err("The end must be a time (YYYY-MM-DDTHH:MM) when the start is a time.".into()),
                    },
                };
                timed_when(s, e, &ztz, &zname, &user)
            }
        }
    }

    fn resolve_change(&self, cur: &EventRow, start: Option<&str>, end: Option<&str>, tz: Option<&str>) -> Result<When, String> {
        let user = self.env.user_tz();
        let cur_tz = cur.tzid.as_deref().and_then(avail::parse_tz).unwrap_or(user);
        let (ztz, zname) = match tz {
            Some(_) => zone_named(tz, &user)?,
            None => (cur_tz, cur.tzid.clone().filter(|t| avail::parse_tz(t).is_some()).unwrap_or_else(|| user.name().to_string())),
        };
        if cur.all_day {
            let day = |ts: i64| local_dt(ts, &user).date_naive();
            let (cs, ce) = (day(cur.start_ts), day(cur.end_ts) - Duration::days(1));
            let first = match start {
                Some(s) => match parse_point(s, &ztz)? {
                    Point::Date(d) => d,
                    Point::Instant(_) => return Err("This is an all-day event: give dates (YYYY-MM-DD).".into()),
                },
                None => cs,
            };
            let last = match end {
                Some(e) => match parse_point(e, &ztz)? {
                    Point::Date(d) => d,
                    Point::Instant(_) => return Err("This is an all-day event: give dates (YYYY-MM-DD).".into()),
                },
                None if start.is_some() => first + (ce - cs),
                None => ce,
            };
            return allday_when(first, last, &user);
        }
        let dur = cur.end_ts - cur.start_ts;
        let inst = |s: &str| match parse_point(s, &ztz)? {
            Point::Instant(t) => Ok(t),
            Point::Date(_) => Err("Give a time (YYYY-MM-DDTHH:MM) for a timed event.".to_string()),
        };
        let (s_ts, e_ts) = match (start, end) {
            (Some(s), Some(e)) => (inst(s)?, inst(e)?),
            (Some(s), None) => {
                let s = inst(s)?;
                (s, s + dur)
            }
            (None, Some(e)) => (cur.start_ts, inst(e)?),
            (None, None) => (cur.start_ts, cur.end_ts),
        };
        timed_when(s_ts, e_ts, &ztz, &zname, &user)
    }

    fn conflicts(&self, calendars: &[String], when: &When, exclude_uid: Option<&str>) -> Result<Vec<String>, String> {
        if when.all_day {
            return Ok(vec![]); // all-day events neither block nor are blocked
        }
        let tz = self.env.user_tz();
        let (rows, _) = self.rows_between(calendars, when.start_ts, when.end_ts, false)?;
        Ok(rows
            .iter()
            .filter(|r| r.busy && !r.all_day && Some(r.uid.as_str()) != exclude_uid && r.start_ts < when.end_ts && r.end_ts > when.start_ts)
            .take(MAX_CONFLICTS)
            .map(|r| format!("'{}' {}\u{2013}{}", clean_line(&r.title, 60), local_dt(r.start_ts, &tz).format("%H:%M"), local_dt(r.end_ts, &tz).format("%H:%M")))
            .collect())
    }

    fn address_known(&self, address: &str) -> Result<bool, String> {
        Ok(self.env.address_known(address))
    }

    fn my_addresses(&self) -> Vec<String> {
        self.env.mine()
    }

    fn zoom_connected(&self) -> bool {
        self.env.zoom_connected()
    }

    fn invite_in_message(&self, scope: &Scope, uid: u32, _uidvalidity: u32) -> BoxFut<'_, Result<Option<InviteInfo>, String>> {
        let scope = scope.clone();
        Box::pin(async move {
            let raw = self.env.read_raw_message(&scope.account, &scope.folder, uid).await?;
            let parsed = mailcore::parse_message(&raw).map_err(|_| "The message could not be read.".to_string())?;
            let Some(inv) = crate::cal_invite::invite_for_message(self.env.cal(), &raw, &parsed.attachments, &scope.account) else { return Ok(None) };
            let tz = self.env.user_tz();
            Ok(Some(invite_info(&inv, &tz)))
        })
    }
}

fn invite_info(inv: &Invite, tz: &Tz) -> InviteInfo {
    let ev = &inv.event;
    let when = if ev.all_day {
        let d = |s: &str| NaiveDate::parse_from_str(s.get(..10).unwrap_or(""), "%Y-%m-%d").ok();
        match (d(&ev.start), d(&ev.end)) {
            (Some(s), Some(e)) => allday_when(s, (e - Duration::days(1)).max(s), tz).unwrap_or_default(),
            _ => When::default(),
        }
    } else {
        let p = |s: &str| DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp());
        match (p(&ev.start), p(&ev.end)) {
            (Some(s), Some(e)) => When { start: ev.start.clone(), end: ev.end.clone(), tzid: ev.tzid.clone(), all_day: false, start_ts: s, end_ts: e, display: describe_span(s, e, false, tz) },
            _ => When::default(),
        }
    };
    InviteInfo {
        uid: ev.uid.clone(),
        title: ev.summary.clone(),
        when,
        organizer: ev.organizer.as_ref().map(person).unwrap_or_default(),
        method: inv.method.as_str().to_string(),
        my_status: inv.my_status.map(|s| status_str(s).to_string()).unwrap_or_default(),
        attachment_id: inv.attachment_id.clone(),
        already_in_calendar: inv.already_in_calendar,
        recurring_instance: inv.recurrence_id.is_some(),
        sender_unverified: inv.sender_unverified,
    }
}

// ------------------------------------------------------------------------------------------
// Write side (approved cards only)
// ------------------------------------------------------------------------------------------

fn minutes_between(when: &When) -> u32 {
    ((when.end_ts - when.start_ts) / 60).clamp(1, 24 * 60) as u32
}

fn apply_when(ev: &mut CalEvent, w: &When) {
    ev.start = w.start.clone();
    ev.end = w.end.clone();
    ev.all_day = w.all_day;
    ev.tzid = w.tzid.clone();
}

impl<E: CalEnv, R: CalDavRemote + 'static, Z: ZoomRemote + 'static> CalAppHost<E, R, Z> {
    /// Version of the event as stored NOW (sending an update bumps SEQUENCE after the save).
    fn current_version(&self, saved: &CalEvent) -> String {
        self.env.cal().db(|c| c.get_event(&saved.uid, &saved.calendar_id)).ok().flatten().map_or_else(|| version_of(saved), |e| version_of(&e))
    }

    /// Send the mail a save / delete produced (after the approval the user gave), then settle the SEQUENCE.
    async fn finish_save(&self, out: SaveOutcome) -> (CalEvent, bool, Option<String>) {
        let mut warning = None;
        let mut sent = false;
        if let Some(mail) = out.mail {
            let from = out.event.organizer.as_ref().map(|o| o.email.clone());
            match self.env.send_itip(from, mail).await {
                Ok(()) => {
                    sent = true;
                    if let Some(seq) = out.pending_sequence {
                        cal_logic::commit_sequence(self.env.cal(), &self.dav, &out.event.uid, &out.event.calendar_id, seq).await;
                    }
                    if let Some(e) = out.push_error {
                        warning = Some(format!("The invitations were sent. {e}"));
                    }
                }
                Err(e) => warning = Some(format!("The event was saved, but the invitations were NOT sent: {e}")),
            }
        } else if let Some(e) = out.push_error {
            warning = Some(e);
        }
        (out.event, sent, warning)
    }
}

impl<E: CalEnv, R: CalDavRemote + 'static, Z: ZoomRemote + 'static> CalActionHost for CalAppHost<E, R, Z> {
    fn calendar(&self, id: &str) -> Result<Option<CalMeta>, String> {
        Ok(self.calendars()?.into_iter().find(|c| c.id == id))
    }

    fn lookup(&self, calendar_id: &str, uid: &str, rid: &str) -> Result<Option<EventRow>, String> {
        self.get_event(calendar_id, uid, rid)
    }

    fn mail_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String> {
        self.env.mail_validity(account, folder)
    }

    fn can_send(&self) -> bool {
        !self.env.mine().is_empty()
    }

    fn create_event(&self, calendar_id: &str, spec: &EventSpec, notify: bool) -> BoxFut<'_, Result<Created, String>> {
        let (calendar_id, spec) = (calendar_id.to_string(), spec.clone());
        Box::pin(async move {
            let mine = self.env.mine();
            let mut ev = CalEvent {
                uid: calendar::new_event_uid(),
                calendar_id: calendar_id.clone(),
                summary: spec.title.clone(),
                description: spec.description.clone(),
                location: spec.location.clone(),
                reminder_minutes: spec.reminder_minutes,
                ..CalEvent::default()
            };
            apply_when(&mut ev, &spec.when);
            ev.attendees = spec.attendees.iter().map(|p| Attendee { email: p.email.clone(), name: Some(p.name.clone()).filter(|n| !n.is_empty()), role: AttendeeRole::Required, status: AttendeeStatus::NeedsAction }).collect();
            let mut warning: Option<String> = None;
            let mut zoom_id = None;
            if spec.add_zoom && !spec.when.all_day {
                match self.env.zoom_create(&spec.title, &spec.when.start, minutes_between(&spec.when), spec.when.tzid.as_deref().unwrap_or("UTC")).await {
                    Ok(z) => {
                        zoom_id = Some(z.meeting_id.clone());
                        ev.zoom = Some(z);
                    }
                    Err(e) => warning = Some(format!("The Zoom meeting could not be created, so the event has no Zoom link: {e}")),
                }
            }
            let inviting = notify && !ev.attendees.is_empty() && !mine.is_empty();
            let out = cal_logic::save_event(self.env.cal(), &self.dav, &self.zoom, ev, inviting, &mine).await;
            self.env.changed();
            let (stored, sent, w2) = self.finish_save(out?).await;
            Ok(Created { uid: stored.uid.clone(), calendar_id: stored.calendar_id.clone(), version: self.current_version(&stored), invited: sent, zoom_meeting_id: zoom_id, warning: w2.or(warning) })
        })
    }

    fn change_event(&self, calendar_id: &str, uid: &str, changes: &EventChanges, notify: bool, expected_version: &str) -> BoxFut<'_, Result<Changed, String>> {
        let (calendar_id, uid, changes, expected) = (calendar_id.to_string(), uid.to_string(), changes.clone(), expected_version.to_string());
        Box::pin(async move {
            let mine = self.env.mine();
            let before = self.env.cal().db(|c| c.get_event(&uid, &calendar_id))?.ok_or("The event no longer exists.")?;
            let snapshot = serde_json::to_string(&before).map_err(|_| "The event could not be saved for Undo.".to_string())?;
            let mut ev = before.clone();
            if let Some(t) = &changes.title {
                ev.summary = t.clone();
            }
            if let Some(l) = &changes.location {
                ev.location = l.clone();
            }
            if let Some(d) = &changes.description {
                ev.description = d.clone();
            }
            if let Some(w) = &changes.when {
                apply_when(&mut ev, w);
            }
            let inviting = notify && !ev.attendees.is_empty() && itip::is_organizer(&ev, &mine) && !mine.is_empty();
            // The version the card was written for is checked again under the calendar's lock.
            let same = |e: &CalEvent| expected.is_empty() || version_of(e) == expected;
            let out = cal_logic::save_event_if(self.env.cal(), &self.dav, &self.zoom, ev, inviting, &mine, Some(&same)).await;
            self.env.changed();
            let (stored, sent, warning) = self.finish_save(out?).await;
            Ok(Changed { snapshot, version: self.current_version(&stored), notified: sent, warning })
        })
    }

    fn cancel_event(&self, calendar_id: &str, uid: &str, notify: bool, expected_version: &str) -> BoxFut<'_, Result<Cancelled, String>> {
        let (calendar_id, uid, expected) = (calendar_id.to_string(), uid.to_string(), expected_version.to_string());
        Box::pin(async move {
            let mine = self.env.mine();
            let before = self.env.cal().db(|c| c.get_event(&uid, &calendar_id))?.ok_or("The event no longer exists.")?;
            let snapshot = serde_json::to_string(&before).map_err(|_| "The event could not be saved for Undo.".to_string())?;
            let same = |e: &CalEvent| expected.is_empty() || version_of(e) == expected;
            let res = cal_logic::delete_event_if(self.env.cal(), &self.dav, &self.zoom, &uid, &calendar_id, notify, &mine, Some(&same)).await;
            self.env.changed();
            let out = res?;
            let mut warning = out.warning;
            let mut notified = false;
            if let Some(mail) = out.mail {
                match self.env.send_itip(out.from, mail).await {
                    Ok(()) => notified = true,
                    Err(e) => {
                        warning.get_or_insert(format!("The event was deleted, but the cancellation could not be sent: {e}"));
                    }
                }
            }
            Ok(Cancelled { snapshot, notified, warning })
        })
    }

    fn restore_event(&self, snapshot: &str, notify: bool) -> BoxFut<'_, Result<String, String>> {
        let snapshot = snapshot.to_string();
        Box::pin(async move {
            let mine = self.env.mine();
            let mut ev: CalEvent = serde_json::from_str(&snapshot).map_err(|_| "The saved copy of the event could not be read.".to_string())?;
            // Undo of a CHANGE: the event still exists, so it keeps its server resource (href / etag, the If-Match
            // of the push) and its SEQUENCE moves past the current one, which is what attendees' clients need to
            // accept the restore. Undo of a CANCEL: the resource is gone, so a fresh one is made.
            match self.env.cal().db(|c| c.get_event(&ev.uid, &ev.calendar_id))? {
                Some(cur) => {
                    ev.href = cur.href.clone();
                    ev.etag = cur.etag.clone();
                    ev.sequence = cur.sequence.max(ev.sequence) + 1;
                }
                None => {
                    ev.href = None;
                    ev.etag = None;
                }
            }
            let inviting = notify && !ev.attendees.is_empty() && itip::is_organizer(&ev, &mine) && !mine.is_empty();
            let out = cal_logic::save_event(self.env.cal(), &self.dav, &self.zoom, ev, inviting, &mine).await;
            self.env.changed();
            let (stored, _sent, warning) = self.finish_save(out?).await;
            match warning {
                Some(w) if w.contains("NOT sent") => Err(w),
                _ => Ok(self.current_version(&stored)),
            }
        })
    }

    fn remove_created(&self, calendar_id: &str, uid: &str, _zoom_meeting_id: Option<&str>, notify: bool) -> BoxFut<'_, Result<(), String>> {
        let (calendar_id, uid) = (calendar_id.to_string(), uid.to_string());
        Box::pin(async move {
            let c = self.cancel_event(&calendar_id, &uid, notify, "").await?;
            match c.warning {
                Some(w) if w.contains("could not be sent") => Err(w),
                _ => Ok(()),
            }
        })
    }

    fn respond_invite(&self, target: &RespondTarget, status: &str) -> BoxFut<'_, Result<Responded, String>> {
        let (target, status) = (target.clone(), status.to_string());
        Box::pin(async move {
            let st = match status.as_str() {
                "accepted" => AttendeeStatus::Accepted,
                "tentative" => AttendeeStatus::Tentative,
                "declined" => AttendeeStatus::Declined,
                _ => return Err("Choose accept, tentative or decline.".into()),
            };
            let mine = self.env.mine();
            let (inv, sender, me) = match &target {
                RespondTarget::Message(m) => {
                    let raw = self.env.read_raw_message(&m.account, &m.folder, m.uid).await?;
                    let (inv, sender) = crate::cal_invite::load_invite(self.env.cal(), &raw, &m.account, &m.attachment_id)?;
                    (inv, sender, m.account.clone())
                }
                RespondTarget::Stored { calendar_id, uid } => {
                    let ev = self.env.cal().db(|c| c.get_event(uid, calendar_id))?.ok_or("The event no longer exists.")?;
                    let org = ev.organizer.as_ref().map(|o| o.email.clone()).ok_or("This event has no organizer to answer.")?;
                    let me = ev.attendees.iter().find(|a| mine.iter().any(|m| m.eq_ignore_ascii_case(&a.email))).map(|a| a.email.clone()).ok_or("You are not an invitee of this event.")?;
                    let inv = Invite { method: InviteMethod::Request, event: ev, my_status: None, already_in_calendar: true, attachment_id: String::new(), recurrence_id: None, overrides: vec![], sender_unverified: false };
                    (inv, org, me)
                }
            };
            let key = cal_logic::reply_key(&inv, &me, st);
            let res = cal_logic::respond_invite(self.env.cal(), &self.dav, inv, &sender, &me, st, &mine, false).await;
            self.env.changed();
            let (_ev, mail) = res?;
            match self.env.send_itip(Some(me), mail).await {
                Ok(()) => Ok(Responded { sent: true, warning: None }),
                Err(e) => {
                    self.env.cal().forget_reply(&key); // not sent: the user may approve again
                    // The answer is stored locally: report "saved, not sent" instead of a failure that would say
                    // nothing happened.
                    Ok(Responded { sent: false, warning: Some(format!("Your answer was saved in your calendar, but the reply was NOT sent: {e}")) })
                }
            }
        })
    }
}

// ------------------------------------------------------------------------------------------
// Production environment
// ------------------------------------------------------------------------------------------

/// The running app: `CalState` + `AppState` behind the `AppHandle`.
pub struct AppCalEnv {
    pub app: tauri::AppHandle,
}

impl AppCalEnv {
    fn app_state(&self) -> tauri::State<'_, crate::state::AppState> {
        use tauri::Manager;
        self.app.state::<crate::state::AppState>()
    }
}

impl CalEnv for AppCalEnv {
    fn cal(&self) -> &CalState {
        use tauri::Manager;
        self.app.state::<CalState>().inner()
    }
    fn mine(&self) -> Vec<String> {
        crate::cal_cmds::my_emails(&self.app_state())
    }
    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
    fn user_tz(&self) -> Tz {
        system_tz()
    }
    fn address_known(&self, address: &str) -> bool {
        self.app_state().storage.address_seen(address).unwrap_or(false)
    }
    fn send_itip(&self, from: Option<String>, msg: OutgoingMessage) -> BoxFut<'_, Result<(), String>> {
        Box::pin(async move { crate::cal_cmds::send_itip(&self.app_state(), from.as_deref(), msg).await })
    }
    fn zoom_connected(&self) -> bool {
        let st = self.cal();
        [calsync::zoom::KEY_REFRESH, calsync::zoom::KEY_ACCESS].iter().any(|k| st.secrets.get(&format!("{}{k}", crate::cal_state::ZOOM_PREFIX)).ok().flatten().is_some_and(|v| !v.is_empty()))
    }
    fn zoom_create(&self, topic: &str, start: &str, minutes: u32, tz: &str) -> BoxFut<'_, Result<ZoomMeeting, String>> {
        let (topic, start, tz) = (topic.to_string(), start.to_string(), tz.to_string());
        Box::pin(async move {
            let st = self.cal();
            let m = st.zoom_client()?.create_meeting(&topic, &start, minutes, &tz, None).await.map_err(|e| e.to_string())?;
            st.zoom_record(&m.meeting_id)?; // only meetings recorded here can later be updated or deleted by this app
            Ok(m)
        })
    }
    fn read_raw_message(&self, account: &str, folder: &str, uid: u32) -> BoxFut<'_, Result<Vec<u8>, String>> {
        let (account, folder) = (account.to_string(), folder.to_string());
        Box::pin(async move { crate::cal_cmds::read_message(&self.app_state(), &account, &folder, uid).await })
    }
    fn mail_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String> {
        self.app_state().storage.folder_sync_state(account, folder).map(|s| s.0).map_err(|_| "The local mail cache could not be read.".to_string())
    }
    fn changed(&self) {
        crate::cal_sched::changed(&self.app);
    }
}

/// The Mac's IANA time zone (falls back to UTC if it cannot be determined).
pub fn system_tz() -> Tz {
    iana_time_zone::get_timezone().ok().and_then(|n| avail::parse_tz(&n)).unwrap_or(Tz::UTC)
}

/// Production zoom seam: the shared client of the calendar state (rebuilt after a reset).
pub struct EnvZoom(pub Arc<AppCalEnv>);

impl ZoomRemote for EnvZoom {
    async fn update(&self, meeting_id: &str, topic: &str, start: &str, minutes: u32, tz: &str) -> Result<(), calsync::ZoomError> {
        crate::cal_state::RealZoom(self.0.cal().zoom_client().map_err(calsync::ZoomError::Store)?).update(meeting_id, topic, start, minutes, tz).await
    }
    async fn delete(&self, meeting_id: &str) -> Result<(), calsync::ZoomError> {
        crate::cal_state::RealZoom(self.0.cal().zoom_client().map_err(calsync::ZoomError::Store)?).delete(meeting_id).await
    }
}

/// The note put in front of every user message: current local time, zone and working hours.
pub fn environment_info(now_ts: i64, tz: &Tz, wh: &WorkHours) -> aichat::context::EnvInfo {
    let n = local_dt(now_ts, tz);
    let off = n.offset().fix().local_minus_utc();
    let (sign, a) = if off < 0 { ('-', -off) } else { ('+', off) };
    aichat::context::EnvInfo {
        now_local: format!("{:04}-{:02}-{:02} {:02}:{:02}", n.year(), n.month(), n.day(), n.hour(), n.minute()),
        weekday: n.format("%A").to_string(),
        tz_name: tz.name().to_string(),
        utc_offset: format!("UTC{sign}{:02}:{:02}", a / 3600, a % 3600 / 60),
        work_hours: wh.describe(),
    }
}


#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use calsync::{CalDavError, DiscoveredCalendar};
    use calsync::caldav::{PushOutcome, SyncSummary};
    use calendar::{Calendar, CalendarInfo};
    use mailcore::InMemorySecretStore;
    use std::sync::Mutex;

    pub(crate) struct NoDav;
    impl CalDavRemote for NoDav {
        async fn discover(&self, _b: &str, _u: &str, _p: &str, _d: &calsync::Diag) -> Result<Vec<DiscoveredCalendar>, CalDavError> {
            Err(CalDavError::Config("test".into()))
        }
        async fn sync(&self, _s: &Mutex<Calendar>, _c: &CalendarInfo, _p: &str) -> Result<SyncSummary, CalDavError> {
            Err(CalDavError::Config("test".into()))
        }
        async fn push(&self, _s: &Mutex<Calendar>, _c: &CalendarInfo, _p: &str, _u: &str) -> Result<PushOutcome, CalDavError> {
            Err(CalDavError::Config("test".into()))
        }
        async fn delete(&self, _s: &Mutex<Calendar>, _c: &CalendarInfo, _h: &str, _p: &str, _e: Option<&str>) -> Result<(), CalDavError> {
            Err(CalDavError::Config("test".into()))
        }
    }
    pub(crate) struct NoZoom;
    impl ZoomRemote for NoZoom {
        async fn update(&self, _i: &str, _t: &str, _s: &str, _m: u32, _z: &str) -> Result<(), calsync::ZoomError> {
            Ok(())
        }
        async fn delete(&self, _i: &str) -> Result<(), calsync::ZoomError> {
            Ok(())
        }
    }

    const ME: &str = "me@example.org";

    pub(crate) struct FakeEnv {
        pub cal: CalState,
        pub now: i64,
        pub tz: Tz,
        pub sent: Mutex<Vec<OutgoingMessage>>,
        pub fail_send: bool,
    }
    impl CalEnv for FakeEnv {
        fn cal(&self) -> &CalState {
            &self.cal
        }
        fn mine(&self) -> Vec<String> {
            vec![ME.into()]
        }
        fn now(&self) -> i64 {
            self.now
        }
        fn user_tz(&self) -> Tz {
            self.tz
        }
        fn address_known(&self, a: &str) -> bool {
            a == "ravi@known.test"
        }
        fn send_itip(&self, _from: Option<String>, msg: OutgoingMessage) -> BoxFut<'_, Result<(), String>> {
            let fail = self.fail_send;
            if !fail {
                self.sent.lock().unwrap().push(msg);
            }
            Box::pin(async move { if fail { Err("smtp down".to_string()) } else { Ok(()) } })
        }
        fn zoom_connected(&self) -> bool {
            false
        }
        fn zoom_create(&self, _t: &str, _s: &str, _m: u32, _z: &str) -> BoxFut<'_, Result<ZoomMeeting, String>> {
            Box::pin(async { Err("no zoom".to_string()) })
        }
        fn read_raw_message(&self, _a: &str, _f: &str, _u: u32) -> BoxFut<'_, Result<Vec<u8>, String>> {
            Box::pin(async { Err("no mail".to_string()) })
        }
        fn mail_validity(&self, _a: &str, _f: &str) -> Result<Option<u32>, String> {
            Ok(Some(1))
        }
        fn changed(&self) {}
    }

    pub(crate) type Host = CalAppHost<FakeEnv, NoDav, NoZoom>;

    pub(crate) fn ts(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp()
    }

    pub(crate) fn host_at(now: &str, zone: &str) -> (Arc<FakeEnv>, Host) {
        let cal = CalState::new(Calendar::open_in_memory().unwrap(), Arc::new(InMemorySecretStore::new()));
        let env = Arc::new(FakeEnv { cal, now: ts(now), tz: zone.parse().unwrap(), sent: Mutex::new(vec![]), fail_send: false });
        (env.clone(), CalAppHost::new(env, NoDav, NoZoom))
    }

    fn host() -> (Arc<FakeEnv>, Host) {
        host_at("2026-11-02T08:00:00+01:00", "Europe/Berlin") // Monday morning in Berlin
    }

    fn put(env: &FakeEnv, uid: &str, title: &str, start: &str, end: &str) -> CalEvent {
        let ev = CalEvent { uid: uid.into(), calendar_id: DEFAULT_CALENDAR_ID.into(), summary: title.into(), start: start.into(), end: end.into(), tzid: Some("Europe/Berlin".into()), ..CalEvent::default() };
        env.cal.db(|c| c.upsert_event(&ev)).unwrap();
        ev
    }

    fn me_as(status: AttendeeStatus) -> Attendee {
        Attendee { email: ME.into(), name: None, role: AttendeeRole::Required, status }
    }

    #[allow(clippy::too_many_arguments)]
    fn day(h: &Host, from: &str, to: &str, mins: u32, buffer: u32, wh: bool, ignore: bool, max: u32) -> Vec<String> {
        let req = SlotReq { from: from.into(), to: to.into(), duration_minutes: mins, working_hours_only: wh, buffer_minutes: buffer, max_results: max, ignore_tentative: ignore };
        h.find_free_slots(&[DEFAULT_CALENDAR_ID.to_string()], &req).unwrap().slots.iter().map(|s| s.start.clone()).collect()
    }

    /// Tuesday 2026-11-03 in Berlin with every kind of event the free-slot search must treat correctly.
    fn busy_tuesday(env: &FakeEnv) {
        let mut a = put(env, "a", "Accepted", "2026-11-03T10:00:00+01:00", "2026-11-03T11:00:00+01:00");
        a.attendees = vec![me_as(AttendeeStatus::Accepted)];
        env.cal.db(|c| c.upsert_event(&a)).unwrap();
        let mut d = put(env, "d", "Declined", "2026-11-03T14:00:00+01:00", "2026-11-03T15:00:00+01:00");
        d.attendees = vec![me_as(AttendeeStatus::Declined)];
        env.cal.db(|c| c.upsert_event(&d)).unwrap();
        let mut t = put(env, "t", "Tentative", "2026-11-03T12:00:00+01:00", "2026-11-03T13:00:00+01:00");
        t.attendees = vec![me_as(AttendeeStatus::Tentative)];
        env.cal.db(|c| c.upsert_event(&t)).unwrap();
        let ad = CalEvent { uid: "ad".into(), calendar_id: DEFAULT_CALENDAR_ID.into(), summary: "Company holiday".into(), start: "2026-11-03".into(), end: "2026-11-04".into(), all_day: true, ..CalEvent::default() };
        env.cal.db(|c| c.upsert_event(&ad)).unwrap();
        // weekly series started the week before: its Nov 3 occurrence is 16:00-17:00
        let mut r = put(env, "r", "Weekly", "2026-10-27T16:00:00+01:00", "2026-10-27T17:00:00+01:00");
        r.rrule = Some("FREQ=WEEKLY;BYDAY=TU".into());
        env.cal.db(|c| c.upsert_event(&r)).unwrap();
    }

    #[test]
    fn free_slots_handle_declined_tentative_all_day_recurring_and_overlap() {
        let (env, h) = host();
        busy_tuesday(&env);
        // an overlapping duplicate of the accepted meeting must not create a second block
        put(&env, "o", "Overlap", "2026-11-03T10:30:00+01:00", "2026-11-03T11:30:00+01:00");
        let fb = h.free_busy(&[DEFAULT_CALENDAR_ID.to_string()], "2026-11-03", "2026-11-03", false).unwrap();
        let blocks: Vec<String> = fb.blocks.iter().map(|b| format!("{}-{}{}", &b.start[11..], &b.end[11..], if b.tentative { "t" } else { "" })).collect();
        assert_eq!(blocks, vec!["10:00-11:30", "12:00-13:00t", "16:00-17:00"], "declined ignored, overlap merged, recurrence expanded");
        assert_eq!(fb.all_day.len(), 1, "all-day events are listed, not blocking");
        let fb2 = h.free_busy(&[DEFAULT_CALENDAR_ID.to_string()], "2026-11-03", "2026-11-03", true).unwrap();
        assert_eq!(fb2.blocks.len(), 2, "tentative ignored on request");
        let t = |ign, buf, mins| day(&h, "2026-11-03", "2026-11-03", mins, buf, true, ign, 3);
        assert_eq!(t(false, 0, 60), vec!["2026-11-03T09:00", "2026-11-03T13:00", "2026-11-03T14:00"], "11:30-12:00 is too short and the tentative block starts at 12:00");
        assert_eq!(t(true, 0, 60), vec!["2026-11-03T09:00", "2026-11-03T11:30", "2026-11-03T12:30"]);
        assert_eq!(t(false, 30, 60), vec!["2026-11-03T13:30", "2026-11-03T14:30"], "buffers shrink the gaps");
        // outside working hours / weekend
        assert!(day(&h, "2026-11-07", "2026-11-08", 60, 0, true, false, 5).is_empty(), "weekend");
        assert_eq!(day(&h, "2026-11-07", "2026-11-07", 60, 0, false, false, 2), vec!["2026-11-07T00:00", "2026-11-07T01:00"]);
    }

    #[test]
    fn slots_are_in_the_past_free_and_dst_correct_across_the_berlin_fall_back_weekend() {
        let (_env, h) = host_at("2026-10-23T12:00:00+02:00", "Europe/Berlin");
        // Sunday 25 Oct 2026 is the 25-hour day; Monday starts at 09:00 CET (+01:00)
        let s = h.find_free_slots(&[DEFAULT_CALENDAR_ID.to_string()], &SlotReq { from: "2026-10-24".into(), to: "2026-10-26".into(), duration_minutes: 30, working_hours_only: true, buffer_minutes: 0, max_results: 5, ignore_tentative: false }).unwrap();
        assert_eq!(s.slots[0].start, "2026-10-26T09:00");
        // a range entirely before "now" yields nothing and says why
        let past = h.find_free_slots(&[DEFAULT_CALENDAR_ID.to_string()], &SlotReq { from: "2026-10-01".into(), to: "2026-10-02".into(), duration_minutes: 30, working_hours_only: true, buffer_minutes: 0, max_results: 5, ignore_tentative: false }).unwrap();
        assert!(past.slots.is_empty() && past.note.contains("past"));
        // "today" does not offer times that already passed (now = 12:00)
        let (_e2, h2) = host_at("2026-11-03T12:10:00+01:00", "Europe/Berlin");
        assert_eq!(day(&h2, "2026-11-03", "2026-11-03", 30, 0, true, false, 1), vec!["2026-11-03T12:15"]);
    }

    #[test]
    fn ranges_are_capped_at_62_days() {
        let (_e, h) = host();
        let c = [DEFAULT_CALENDAR_ID.to_string()];
        assert!(h.list_events(&c, "2026-11-01", "2026-12-31").is_ok(), "61 days");
        assert!(h.list_events(&c, "2026-11-01", "2027-01-01").is_ok(), "62 days (the last date is included)");
        let e = h.list_events(&c, "2026-11-01", "2027-01-02").unwrap_err();
        assert!(e.contains("62 days"), "{e}");
        assert!(h.free_busy(&c, "2026-01-01", "2026-12-31", false).is_err());
        assert!(h.find_free_slots(&c, &SlotReq { from: "2026-01-01".into(), to: "2026-12-31".into(), duration_minutes: 30, working_hours_only: true, buffer_minutes: 0, max_results: 3, ignore_tentative: false }).is_err());
        assert!(h.list_events(&c, "2026-11-05", "2026-11-04").is_err(), "end before start");
        assert!(h.list_events(&c, "soon", "2026-11-04").unwrap_err().contains("not a valid time"));
    }

    #[test]
    fn out_of_scope_calendars_never_come_back_from_the_host() {
        let (env, h) = host();
        env.cal.db(|c| c.create_local("Secret", "#112233")).unwrap();
        let secret = env.cal.db(|c| c.list_calendars()).unwrap().into_iter().find(|c| c.name == "Secret").unwrap();
        put(&env, "mine", "Dentist", "2026-11-03T09:00:00+01:00", "2026-11-03T10:00:00+01:00");
        let s = CalEvent { uid: "hidden".into(), calendar_id: secret.id.clone(), summary: "Therapy".into(), start: "2026-11-03T11:00:00+01:00".into(), end: "2026-11-03T12:00:00+01:00".into(), tzid: Some("Europe/Berlin".into()), ..CalEvent::default() };
        env.cal.db(|c| c.upsert_event(&s)).unwrap();
        let only = [DEFAULT_CALENDAR_ID.to_string()];
        let l = h.list_events(&only, "2026-11-03", "2026-11-03").unwrap();
        assert_eq!(l.events.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(), vec!["Dentist"]);
        assert_eq!(l.total, 1, "not even the count leaks");
        let fb = h.free_busy(&only, "2026-11-03", "2026-11-03", false).unwrap();
        assert_eq!(fb.blocks.len(), 1);
        assert!(h.conflicts(&only, &h.resolve_when("2026-11-03T11:30", None, None).unwrap(), None).unwrap().is_empty());
        let both = h.list_events(&[DEFAULT_CALENDAR_ID.to_string(), secret.id], "2026-11-03", "2026-11-03").unwrap();
        assert_eq!(both.events.len(), 2);
    }

    #[test]
    fn resolve_when_reads_times_in_the_right_zone_and_handles_all_day_and_dst() {
        let (_e, h) = host();
        let w = h.resolve_when("2026-11-13T13:00", None, None).unwrap();
        assert_eq!((w.start.as_str(), w.end.as_str(), w.tzid.as_deref()), ("2026-11-13T13:00:00+01:00", "2026-11-13T14:00:00+01:00", Some("Europe/Berlin")));
        assert_eq!(w.display, "Fri 13 Nov 13:00\u{2013}14:00");
        let ny = h.resolve_when("2026-11-13T13:00", Some("2026-11-13T14:30"), Some("America/New_York")).unwrap();
        assert_eq!((ny.start.as_str(), ny.tzid.as_deref(), ny.end_ts - ny.start_ts), ("2026-11-13T13:00:00-05:00", Some("America/New_York"), 5400));
        assert_eq!(ny.display, "Fri 13 Nov 19:00\u{2013}20:30", "displayed in the user's zone");
        let off = h.resolve_when("2026-11-13T12:00:00Z", None, None).unwrap();
        assert_eq!(off.start, "2026-11-13T13:00:00+01:00");
        // DST gap: 02:30 on 29 Mar 2026 does not exist in Berlin and moves forward
        assert_eq!(h.resolve_when("2026-03-29T02:30", None, None).unwrap().start, "2026-03-29T03:30:00+02:00");
        // all-day: the model's last day is inclusive, storage is exclusive
        let ad = h.resolve_when("2026-11-14", Some("2026-11-15"), None).unwrap();
        assert!(ad.all_day && ad.start == "2026-11-14" && ad.end == "2026-11-16" && ad.tzid.is_none());
        assert_eq!(ad.display, "Sat 14 Nov \u{2013} Sun 15 Nov (all day)");
        assert_eq!(h.resolve_when("2026-11-14", None, None).unwrap().end, "2026-11-15");
        for (s, e, z) in [("2026-11-14", Some("2026-11-14T10:00"), None), ("2026-11-14T10:00", Some("2026-11-15"), None), ("2026-11-14T10:00", Some("2026-11-14T09:00"), None), ("2026-11-14T10:00", None, Some("Mars/Base")), ("2026-11-14T10:00", Some("2026-12-30T10:00"), None)] {
            assert!(h.resolve_when(s, e, z).is_err(), "{s} {e:?} {z:?}");
        }
    }

    #[test]
    fn resolve_change_keeps_length_or_start() {
        let (env, h) = host();
        put(&env, "x", "Review", "2026-11-12T13:00:00+01:00", "2026-11-12T14:30:00+01:00");
        let cur = h.get_event(DEFAULT_CALENDAR_ID, "x", "").unwrap().unwrap();
        let moved = h.resolve_change(&cur, Some("2026-11-13T10:00"), None, None).unwrap();
        assert_eq!((moved.start.as_str(), moved.end_ts - moved.start_ts), ("2026-11-13T10:00:00+01:00", 5400));
        let shorter = h.resolve_change(&cur, None, Some("2026-11-12T14:00"), None).unwrap();
        assert_eq!((shorter.start_ts, shorter.end_ts - shorter.start_ts), (cur.start_ts, 3600));
        assert!(h.resolve_change(&cur, None, Some("2026-11-12T12:00"), None).is_err());
    }

    #[test]
    fn conflicts_count_overlapping_busy_events_only() {
        let (env, h) = host();
        busy_tuesday(&env);
        let c = [DEFAULT_CALENDAR_ID.to_string()];
        let w = |s: &str, e: &str| h.resolve_when(s, Some(e), None).unwrap();
        assert_eq!(h.conflicts(&c, &w("2026-11-03T10:30", "2026-11-03T11:30"), None).unwrap(), vec!["'Accepted' 10:00\u{2013}11:00"]);
        assert!(h.conflicts(&c, &w("2026-11-03T10:30", "2026-11-03T11:30"), Some("a")).unwrap().is_empty(), "the event being moved is excluded");
        assert!(h.conflicts(&c, &w("2026-11-03T14:00", "2026-11-03T15:00"), None).unwrap().is_empty(), "declined does not conflict");
        assert!(h.conflicts(&c, &w("2026-11-03T11:00", "2026-11-03T12:00"), None).unwrap().is_empty(), "touching is not overlapping");
        assert_eq!(h.conflicts(&c, &w("2026-11-03T12:30", "2026-11-03T13:30"), None).unwrap().len(), 1, "tentative conflicts");
        assert_eq!(h.conflicts(&c, &w("2026-11-03T16:30", "2026-11-03T17:30"), None).unwrap().len(), 1, "recurring occurrence conflicts");
        assert!(h.conflicts(&c, &h.resolve_when("2026-11-03", None, None).unwrap(), None).unwrap().is_empty(), "all-day events never conflict");
    }

    #[test]
    fn rows_carry_my_status_version_and_no_raw_ids_in_titles() {
        let (env, h) = host();
        let mut e = put(&env, "inv", "Offsite", "2026-11-04T09:00:00+01:00", "2026-11-04T10:00:00+01:00");
        e.organizer = Some(Attendee { email: "olga@corp.test".into(), name: Some("Olga".into()), role: AttendeeRole::Chair, status: AttendeeStatus::Accepted });
        e.attendees = vec![me_as(AttendeeStatus::NeedsAction), Attendee { email: "x@y.test".into(), name: None, role: AttendeeRole::Optional, status: AttendeeStatus::Accepted }];
        env.cal.db(|c| c.upsert_event(&e)).unwrap();
        let r = h.get_event(DEFAULT_CALENDAR_ID, "inv", "").unwrap().unwrap();
        assert_eq!((r.my_status.as_str(), r.attendee_count, r.busy), ("needs-action", 2, true));
        assert_eq!(r.organizer, "Olga <olga@corp.test>");
        let v1 = r.version.clone();
        e.summary = "Offsite (moved)".into();
        env.cal.db(|c| c.upsert_event(&e)).unwrap();
        assert_ne!(h.get_event(DEFAULT_CALENDAR_ID, "inv", "").unwrap().unwrap().version, v1);
        assert!(h.get_event(DEFAULT_CALENDAR_ID, "missing", "").unwrap().is_none());
    }

    fn run<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    #[test]
    fn approved_writes_go_through_the_calendar_paths_send_mail_only_for_organised_events_and_undo() {
        let (env, h) = host();
        let spec = EventSpec {
            title: "Lunch with Ravi".into(),
            when: h.resolve_when("2026-11-13T13:00", Some("2026-11-13T14:00"), None).unwrap(),
            attendees: vec![Person { email: "ravi@known.test".into(), name: String::new() }],
            ..EventSpec::default()
        };
        assert!(env.sent.lock().unwrap().is_empty(), "nothing is sent before an approved create");
        let c = run(h.create_event(DEFAULT_CALENDAR_ID, &spec, true)).unwrap();
        assert!(c.invited && c.warning.is_none());
        let sent = env.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, vec!["ravi@known.test".to_string()]);
        assert_eq!(sent[0].calendar.as_ref().map(|p| p.method.as_str()), Some("REQUEST"));
        let stored = env.cal.db(|cc| cc.get_event(&c.uid, DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        assert_eq!(stored.organizer.as_ref().map(|o| o.email.as_str()), Some(ME));
        assert_eq!(version_of(&stored), c.version);

        // change: attendees get an update, the snapshot restores the old title
        let ch = run(h.change_event(DEFAULT_CALENDAR_ID, &c.uid, &EventChanges { title: Some("Lunch (moved)".into()), when: Some(h.resolve_when("2026-11-13T14:00", None, None).unwrap()), ..EventChanges::default() }, true, &c.version)).unwrap();
        assert!(ch.notified);
        assert_eq!(env.sent.lock().unwrap().len(), 2);
        let now = env.cal.db(|cc| cc.get_event(&c.uid, DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        assert_eq!((now.summary.as_str(), now.start.as_str()), ("Lunch (moved)", "2026-11-13T14:00:00+01:00"));
        assert_eq!(version_of(&now), ch.version);
        run(h.restore_event(&ch.snapshot, true)).unwrap();
        let back = env.cal.db(|cc| cc.get_event(&c.uid, DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        assert_eq!((back.summary.as_str(), back.start.as_str()), ("Lunch with Ravi", "2026-11-13T13:00:00+01:00"));

        // cancel: attendees get a CANCEL; restore brings the event back
        let can = run(h.cancel_event(DEFAULT_CALENDAR_ID, &c.uid, true, "")).unwrap();
        assert!(can.notified);
        assert!(env.cal.db(|cc| cc.get_event(&c.uid, DEFAULT_CALENDAR_ID)).unwrap().is_none());
        assert_eq!(env.sent.lock().unwrap().last().unwrap().calendar.as_ref().unwrap().method, "CANCEL");
        run(h.restore_event(&can.snapshot, false)).unwrap();
        assert!(env.cal.db(|cc| cc.get_event(&c.uid, DEFAULT_CALENDAR_ID)).unwrap().is_some());
        run(h.remove_created(DEFAULT_CALENDAR_ID, &c.uid, None, false)).unwrap();
        assert!(env.cal.db(|cc| cc.get_event(&c.uid, DEFAULT_CALENDAR_ID)).unwrap().is_none());
    }

    #[test]
    fn a_failed_send_is_reported_and_an_event_without_attendees_sends_nothing() {
        let cal = CalState::new(Calendar::open_in_memory().unwrap(), Arc::new(InMemorySecretStore::new()));
        let env = Arc::new(FakeEnv { cal, now: ts("2026-11-02T08:00:00+01:00"), tz: Tz::Europe__Berlin, sent: Mutex::new(vec![]), fail_send: true });
        let h: Host = CalAppHost::new(env.clone(), NoDav, NoZoom);
        let when = h.resolve_when("2026-11-13T13:00", None, None).unwrap();
        let solo = run(h.create_event(DEFAULT_CALENDAR_ID, &EventSpec { title: "Focus".into(), when: when.clone(), ..EventSpec::default() }, true)).unwrap();
        assert!(!solo.invited && solo.warning.is_none());
        let spec = EventSpec { title: "Sync".into(), when, attendees: vec![Person { email: "ravi@known.test".into(), name: String::new() }], ..EventSpec::default() };
        let c = run(h.create_event(DEFAULT_CALENDAR_ID, &spec, true)).unwrap();
        assert!(!c.invited && c.warning.unwrap().contains("NOT sent"), "the user is told the invitations did not go out");
    }

    #[test]
    fn answering_a_stored_invitation_stores_the_answer_and_mails_the_organizer() {
        let (env, h) = host();
        let mut e = put(&env, "inv", "Offsite", "2026-11-04T09:00:00+01:00", "2026-11-04T10:00:00+01:00");
        e.organizer = Some(Attendee { email: "olga@corp.test".into(), name: None, role: AttendeeRole::Chair, status: AttendeeStatus::Accepted });
        e.attendees = vec![me_as(AttendeeStatus::NeedsAction)];
        env.cal.db(|c| c.upsert_event(&e)).unwrap();
        let r = run(h.respond_invite(&RespondTarget::Stored { calendar_id: DEFAULT_CALENDAR_ID.into(), uid: "inv".into() }, "accepted")).unwrap();
        assert!(r.sent);
        let m = env.sent.lock().unwrap().clone();
        assert_eq!((m.len(), m[0].to.clone(), m[0].calendar.as_ref().map(|p| p.method.clone())), (1, vec!["olga@corp.test".to_string()], Some("REPLY".to_string())));
        let after = env.cal.db(|c| c.get_event("inv", DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        assert_eq!(after.attendees[0].status, AttendeeStatus::Accepted);
        // the same answer twice is refused (existing double-click guard)
        assert!(run(h.respond_invite(&RespondTarget::Stored { calendar_id: DEFAULT_CALENDAR_ID.into(), uid: "inv".into() }, "accepted")).is_err());
        assert!(run(h.respond_invite(&RespondTarget::Stored { calendar_id: DEFAULT_CALENDAR_ID.into(), uid: "nope".into() }, "declined")).is_err());
        assert!(run(h.respond_invite(&RespondTarget::Stored { calendar_id: DEFAULT_CALENDAR_ID.into(), uid: "inv".into() }, "forward")).is_err());
    }

    // ---- review fixes (Q01, Q04, Q05, Q08) -------------------------------------------------------

    /// CalDAV that records every push / delete with the resource identity it was handed.
    #[derive(Clone, Default)]
    struct RecDav {
        log: Arc<Mutex<Vec<String>>>,
    }
    impl CalDavRemote for RecDav {
        async fn discover(&self, _b: &str, _u: &str, _p: &str, _d: &calsync::Diag) -> Result<Vec<DiscoveredCalendar>, CalDavError> {
            Err(CalDavError::Config("test".into()))
        }
        async fn sync(&self, _s: &Mutex<Calendar>, _c: &CalendarInfo, _p: &str) -> Result<SyncSummary, CalDavError> {
            Err(CalDavError::Config("test".into()))
        }
        async fn push(&self, s: &Mutex<Calendar>, cal: &CalendarInfo, _pw: &str, uid: &str) -> Result<PushOutcome, CalDavError> {
            let c = s.lock().unwrap();
            let mut e = c.get_event(uid, &cal.id).unwrap().unwrap();
            let n = self.log.lock().unwrap().len();
            self.log.lock().unwrap().push(format!("push {uid} href={:?} etag={:?} seq={}", e.href, e.etag, e.sequence));
            let href = e.href.clone().unwrap_or_else(|| format!("/c/{uid}.ics"));
            let etag = format!("\"e{}\"", n + 1);
            e.href = Some(href.clone());
            e.etag = Some(etag.clone());
            c.upsert_event(&e).unwrap();
            Ok(PushOutcome::Pushed(calsync::caldav::PutOutcome { href, etag: Some(etag) }))
        }
        async fn delete(&self, _s: &Mutex<Calendar>, _c: &CalendarInfo, href: &str, _p: &str, _e: Option<&str>) -> Result<(), CalDavError> {
            self.log.lock().unwrap().push(format!("delete {href}"));
            Ok(())
        }
    }

    fn caldav_host() -> (Arc<FakeEnv>, CalAppHost<FakeEnv, RecDav, NoZoom>, RecDav) {
        use mailcore::SecretStore;
        let secrets = Arc::new(InMemorySecretStore::new());
        let cal = CalState::new(Calendar::open_in_memory().unwrap(), secrets.clone());
        cal.db(|c| {
            c.create_calendar(&calendar::NewCalendar { id: Some("dv".into()), name: "Dav".into(), color: "#112233".into(), kind: calendar::CalendarKind::Caldav, url: Some("https://dav.example/dv/".into()), username: Some("u".into()), read_only: false })
        })
        .unwrap();
        secrets.set(&crate::cal_state::caldav_key("dv"), "pw").unwrap();
        let env = Arc::new(FakeEnv { cal, now: ts("2026-11-02T08:00:00+01:00"), tz: Tz::Europe__Berlin, sent: Mutex::new(vec![]), fail_send: false });
        let dav = RecDav::default();
        (env.clone(), CalAppHost::new(env, dav.clone(), NoZoom), dav)
    }

    #[test]
    fn q04_undo_of_a_change_keeps_the_resource_and_bumps_the_sequence() {
        let (env, h, dav) = caldav_host();
        let spec = EventSpec { title: "Lunch".into(), when: h.resolve_when("2026-11-13T13:00", Some("2026-11-13T14:00"), None).unwrap(), attendees: vec![Person { email: "ravi@known.test".into(), name: String::new() }], ..EventSpec::default() };
        let c = run(h.create_event("dv", &spec, true)).unwrap();
        let ch = run(h.change_event("dv", &c.uid, &EventChanges { title: Some("Lunch (moved)".into()), ..EventChanges::default() }, true, &c.version)).unwrap();
        let after_change = env.cal.db(|cc| cc.get_event(&c.uid, "dv")).unwrap().unwrap();
        run(h.restore_event(&ch.snapshot, true)).unwrap();
        let back = env.cal.db(|cc| cc.get_event(&c.uid, "dv")).unwrap().unwrap();
        assert_eq!(back.summary, "Lunch");
        assert_eq!(back.href, after_change.href, "same server resource, no new href");
        assert!(back.sequence > after_change.sequence, "SEQUENCE moved past the current one: {} vs {}", back.sequence, after_change.sequence);
        let log = dav.log.lock().unwrap().clone();
        let (cur_href, cur_etag, seq) = (format!("href={:?}", after_change.href), format!("etag={:?}", after_change.etag), format!("seq={}", after_change.sequence + 1));
        assert!(log.iter().any(|l| l.contains(&cur_href) && l.contains(&cur_etag) && l.contains(&seq)), "the restore pushes with the CURRENT href/etag (If-Match) and SEQUENCE+1: {log:?}");
        assert_eq!(log.iter().filter(|l| l.starts_with("push") && l.contains("href=None")).count(), 1, "only the create had no resource yet: {log:?}");
        // undo of a cancel still makes a fresh resource
        let can = run(h.cancel_event("dv", &c.uid, false, "")).unwrap();
        run(h.restore_event(&can.snapshot, false)).unwrap();
        let log = dav.log.lock().unwrap().clone();
        assert!(log.last().unwrap().contains("href=None"), "{log:?}");
    }

    #[test]
    fn q01_a_no_notify_card_sends_nothing_even_for_an_invited_event() {
        let (env, h) = host();
        let spec = EventSpec { title: "Sync".into(), when: h.resolve_when("2026-11-13T13:00", None, None).unwrap(), attendees: vec![Person { email: "ravi@known.test".into(), name: String::new() }], ..EventSpec::default() };
        let c = run(h.create_event(DEFAULT_CALENDAR_ID, &spec, false)).unwrap();
        assert!(!c.invited && c.warning.is_none());
        let ch = run(h.change_event(DEFAULT_CALENDAR_ID, &c.uid, &EventChanges { title: Some("Sync 2".into()), ..EventChanges::default() }, false, &c.version)).unwrap();
        assert!(!ch.notified);
        let can = run(h.cancel_event(DEFAULT_CALENDAR_ID, &c.uid, false, &ch.version)).unwrap();
        assert!(!can.notified);
        assert!(env.sent.lock().unwrap().is_empty(), "nothing was mailed");
    }

    #[test]
    fn q05_a_stale_version_is_refused_under_the_lock_and_writes_nothing() {
        let (env, h) = host();
        let spec = EventSpec { title: "Plan".into(), when: h.resolve_when("2026-11-13T13:00", None, None).unwrap(), ..EventSpec::default() };
        let c = run(h.create_event(DEFAULT_CALENDAR_ID, &spec, false)).unwrap();
        // somebody edits it after the card was written
        let mut e = env.cal.db(|cc| cc.get_event(&c.uid, DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        e.location = "Room 9".into();
        env.cal.db(|cc| cc.upsert_event(&e)).unwrap();
        let err = run(h.change_event(DEFAULT_CALENDAR_ID, &c.uid, &EventChanges { title: Some("Overwrite".into()), ..EventChanges::default() }, false, &c.version)).unwrap_err();
        assert!(err.contains("changed after this suggestion"), "{err}");
        let err = run(h.cancel_event(DEFAULT_CALENDAR_ID, &c.uid, false, &c.version)).unwrap_err();
        assert!(err.contains("changed after this suggestion"), "{err}");
        let now = env.cal.db(|cc| cc.get_event(&c.uid, DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        assert_eq!((now.summary.as_str(), now.location.as_str()), ("Plan", "Room 9"), "neither the edit nor the event was lost");
    }

    #[test]
    fn q08_an_answer_saved_but_not_mailed_is_ok_with_a_saved_not_sent_warning() {
        let cal = CalState::new(Calendar::open_in_memory().unwrap(), Arc::new(InMemorySecretStore::new()));
        let env = Arc::new(FakeEnv { cal, now: ts("2026-11-02T08:00:00+01:00"), tz: Tz::Europe__Berlin, sent: Mutex::new(vec![]), fail_send: true });
        let h: Host = CalAppHost::new(env.clone(), NoDav, NoZoom);
        let mut e = put(&env, "inv", "Offsite", "2026-11-04T09:00:00+01:00", "2026-11-04T10:00:00+01:00");
        e.organizer = Some(Attendee { email: "olga@corp.test".into(), name: None, role: AttendeeRole::Chair, status: AttendeeStatus::Accepted });
        e.attendees = vec![me_as(AttendeeStatus::NeedsAction)];
        env.cal.db(|c| c.upsert_event(&e)).unwrap();
        let t = RespondTarget::Stored { calendar_id: DEFAULT_CALENDAR_ID.into(), uid: "inv".into() };
        let r = run(h.respond_invite(&t, "accepted")).unwrap();
        assert!(!r.sent && r.warning.as_deref().unwrap_or("").contains("NOT sent"), "{r:?}");
        let stored = env.cal.db(|c| c.get_event("inv", DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        assert_eq!(stored.attendees[0].status, AttendeeStatus::Accepted, "the answer really is saved");
    }

    #[test]
    fn environment_note_facts() {
        let tz: Tz = "Europe/Berlin".parse().unwrap();
        let e = environment_info(ts("2026-11-05T14:32:10+01:00"), &tz, &WorkHours::default());
        assert_eq!((e.now_local.as_str(), e.weekday.as_str(), e.tz_name.as_str(), e.utc_offset.as_str(), e.work_hours.as_str()), ("2026-11-05 14:32", "Thursday", "Europe/Berlin", "UTC+01:00", "09:00-18:00 Mon-Fri"));
        let ny: Tz = "America/New_York".parse().unwrap();
        assert_eq!(environment_info(ts("2026-07-01T12:00:00Z"), &ny, &WorkHours::default()).utc_offset, "UTC-04:00");
        let k: Tz = "Asia/Kolkata".parse().unwrap();
        assert_eq!(environment_info(ts("2026-07-01T12:00:00Z"), &k, &WorkHours::default()).utc_offset, "UTC+05:30");
    }
}
