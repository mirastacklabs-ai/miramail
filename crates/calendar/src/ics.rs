//! iCalendar (RFC 5545) parsing and building. Never panics on hostile input; input capped at 1 MB.

use crate::error::{Error, Result};
use crate::rrule::event_zone;
pub use crate::rrule::MAX_RRULE_LEN;
use crate::tm::{self, Zone};
use crate::types::*;
use crate::tz::{Resolved, Tzs};
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, Offset, TimeZone};
use chrono_tz::{OffsetComponents, Tz};
use std::fmt::Write as _;

pub const MAX_ICS_BYTES: usize = 1_048_576;
/// Size cap for rows this crate wrote itself (re-parsed from SQLite); they must never silently vanish.
pub(crate) const TRUSTED_MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 16;
const MAX_EVENTS: usize = 5_000;
/// Hard caps: exceeding one is an error, never a silent truncation.
pub const MAX_ATTENDEES: usize = 5_000;
pub const MAX_EXDATES: usize = 10_000;
/// Largest absolute DURATION / TRIGGER honoured (~10 years); larger values are clamped.
const MAX_DURATION_SECS: i64 = 315_360_000;

/// Parser options. The default is SAFE: inbound `X-MIRAMAIL-ZOOM-*` properties are ignored, because
/// anyone can put them in an invite. Opt in only for resources of a CalDAV calendar the user owns
/// and for rows this crate stored itself. `X-MIRAMAIL-ZOOM-STARTURL` is never read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseOptions {
    pub trust_zoom_props: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCalendar {
    pub method: Option<String>,
    /// Master / standalone events (no RECURRENCE-ID). `calendar_id` is empty.
    pub events: Vec<CalEvent>,
    /// RECURRENCE-ID overrides, keyed by (`event.uid`, `recurrence_id`).
    pub overrides: Vec<EventOverride>,
}

// ---------------------------------------------------------------------------------------------
// Low-level content-line parsing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct Prop {
    pub(crate) name: String,
    pub(crate) params: Vec<(String, String)>,
    pub(crate) value: String,
}

impl Prop {
    pub(crate) fn param(&self, k: &str) -> Option<&str> {
        self.params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Default)]
pub(crate) struct Comp {
    pub(crate) name: String,
    pub(crate) props: Vec<Prop>,
    pub(crate) children: Vec<Comp>,
}

impl Comp {
    pub(crate) fn prop(&self, n: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == n)
    }
    pub(crate) fn text(&self, n: &str) -> Option<String> {
        self.prop(n).map(|p| unescape(&p.value))
    }
}

pub(crate) fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let l = raw.strip_suffix('\r').unwrap_or(raw);
        if (l.starts_with(' ') || l.starts_with('\t')) && !lines.is_empty() {
            if let Some(last) = lines.last_mut() {
                last.push_str(&l[1..]);
            }
        } else if !l.is_empty() {
            lines.push(l.to_string());
        }
    }
    lines
}

pub(crate) fn parse_line(line: &str) -> Option<Prop> {
    let mut name_end = None;
    for (i, c) in line.char_indices() {
        if c == ';' || c == ':' {
            name_end = Some((i, c));
            break;
        }
    }
    let (ne, sep) = name_end?;
    let name = line[..ne].trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let name = name.rsplit('.').next().unwrap_or(&name).to_string(); // drop group prefix
    let mut params = Vec::new();
    let mut rest = &line[ne + 1..];
    if sep == ';' {
        // parse params until an unquoted ':'
        let bytes = rest.as_bytes();
        let mut in_q = false;
        let mut seg_start = 0;
        let mut value_at = None;
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => in_q = !in_q,
                b';' if !in_q => {
                    push_param(&mut params, &rest[seg_start..i]);
                    seg_start = i + 1;
                }
                b':' if !in_q => {
                    push_param(&mut params, &rest[seg_start..i]);
                    value_at = Some(i + 1);
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        rest = &rest[value_at?..];
    }
    Some(Prop { name, params, value: rest.to_string() })
}

fn push_param(params: &mut Vec<(String, String)>, seg: &str) {
    if let Some((k, v)) = seg.split_once('=') {
        let v = v.trim();
        let v = v.strip_prefix('"').unwrap_or(v);
        let v = v.strip_suffix('"').unwrap_or(v);
        params.push((k.trim().to_ascii_uppercase(), v.to_string()));
    }
}

pub(crate) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push_str("\\n");
            }
            c if c.is_control() && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn build_tree(text: &str) -> (Comp, bool) {
    let mut stack: Vec<Comp> = vec![Comp { name: "ROOT".into(), ..Default::default() }];
    let mut skip = 0usize;
    let mut saw_cal = false;
    let mut events = 0usize;
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else { continue };
        match p.name.as_str() {
            "BEGIN" => {
                let n = p.value.trim().to_ascii_uppercase();
                if skip > 0 || stack.len() >= MAX_DEPTH {
                    skip += 1;
                    continue;
                }
                if n == "VCALENDAR" || n == "VEVENT" {
                    saw_cal = true;
                }
                if n == "VEVENT" {
                    events += 1;
                    if events > MAX_EVENTS {
                        skip += 1;
                        continue;
                    }
                }
                stack.push(Comp { name: n, ..Default::default() });
            }
            "END" => {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let n = p.value.trim().to_ascii_uppercase();
                if stack.len() > 1 && stack.iter().skip(1).any(|c| c.name == n) {
                    while stack.len() > 1 {
                        let Some(done) = stack.pop() else { break };
                        let matched = done.name == n;
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(done);
                        }
                        if matched {
                            break;
                        }
                    }
                }
            }
            _ => {
                if skip == 0 {
                    if let Some(top) = stack.last_mut() {
                        top.props.push(p);
                    }
                }
            }
        }
    }
    // tolerate truncation: close everything
    while stack.len() > 1 {
        let Some(done) = stack.pop() else { break };
        if let Some(parent) = stack.last_mut() {
            parent.children.push(done);
        }
    }
    (stack.pop().unwrap_or_default(), saw_cal)
}

pub(crate) fn collect<'a>(c: &'a Comp, name: &str, out: &mut Vec<&'a Comp>) {
    for ch in &c.children {
        if ch.name == name {
            out.push(ch);
        }
        collect(ch, name, out);
    }
}

// ---------------------------------------------------------------------------------------------
// Value parsing
// ---------------------------------------------------------------------------------------------

enum Dv {
    Date(NaiveDate),
    /// Instant, the IANA tzid it is expressed in (`None` = floating, `UTC` for Z / unresolvable
    /// zones / evaluated VTIMEZONEs), and the wall-clock value as written.
    Dt(DateTime<FixedOffset>, Option<String>, NaiveDateTime),
}

fn parse_dt_value(p: &Prop, v: &str, tzs: &Tzs, default_tzid: Option<&str>) -> Option<Dv> {
    let v = v.trim();
    let is_date = p.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE")) || (v.len() == 8 && !v.contains('T'));
    if is_date {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(Dv::Date);
    }
    let (body, z) = match v.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (v, false),
    };
    let naive = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").ok()?;
    let as_utc = |tz: Option<&str>| Dv::Dt(naive.and_utc().fixed_offset(), tz.map(String::from), naive);
    if z {
        return Some(as_utc(Some("UTC")));
    }
    match p.param("TZID").or(default_tzid) {
        None => Some(as_utc(None)), // floating
        Some(t) => match tzs.resolve(t) {
            Some(Resolved::Iana(tz)) => {
                let inst = Zone::Tz(tz).local_to_instant(naive)?;
                Some(Dv::Dt(inst, Some(tz.name().to_string()), naive))
            }
            Some(Resolved::Vtz(v)) => {
                let inst = v.local_to_instant(naive)?;
                Some(Dv::Dt(inst.with_timezone(&tm::utc()), Some("UTC".to_string()), naive))
            }
            None => Some(as_utc(Some("UTC"))),
        },
    }
}

/// Remove every `name` property from a stored ICS (used by migrations).
pub(crate) fn strip_prop(ics: &str, name: &str) -> String {
    let mut out = String::with_capacity(ics.len());
    let pre = format!("{name}:");
    let pre_p = format!("{name};");
    for l in unfold(ics) {
        let u = l.to_ascii_uppercase();
        if u.starts_with(&pre) || u.starts_with(&pre_p) {
            continue;
        }
        fold_into(&mut out, &l);
    }
    out
}

/// TRIGGER / DURATION helper for the raw patcher.
pub(crate) fn trigger_secs(v: &str) -> Option<i64> {
    parse_duration(v)
}

/// `[+-]P[nW][nD][T[nH][nM][nS]]` -> seconds, clamped to +-10 years. `None` when malformed or when
/// the arithmetic overflows (hostile input).
fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let s = s.strip_prefix(['P', 'p'])?;
    let mut total: i64 = 0;
    let mut num: Option<i64> = None;
    let mut in_time = false;
    let mut any = false;
    for c in s.chars() {
        match c.to_ascii_uppercase() {
            'T' => in_time = true,
            d if d.is_ascii_digit() => {
                num = Some(num.unwrap_or(0).checked_mul(10)?.checked_add(d.to_digit(10)? as i64)?);
            }
            u @ ('W' | 'D' | 'H' | 'M' | 'S') => {
                let n = num.take()?;
                let mult = match (u, in_time) {
                    ('W', false) => 604_800,
                    ('D', false) => 86_400,
                    ('H', true) => 3_600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                total = total.checked_add(n.checked_mul(mult)?)?;
                any = true;
            }
            _ => return None,
        }
    }
    if num.is_some() || !any {
        return None;
    }
    let total = total.min(MAX_DURATION_SECS);
    Some(if neg { -total } else { total })
}

fn fmt_in_zone(zone: &Zone, dv: &Dv, all_day: bool) -> String {
    match dv {
        Dv::Date(d) => tm::fmt_date(*d),
        // an all-day EXDATE/RECURRENCE-ID given as DATE-TIME means the local date in ITS OWN zone
        Dv::Dt(_, _, n) if all_day => tm::fmt_date(n.date()),
        Dv::Dt(dt, _, _) => tm::fmt_dt(&zone.at(dt)),
    }
}

pub(crate) fn mailto(v: &str) -> String {
    let v = v.trim();
    let l = v.get(..7).unwrap_or("");
    let e = if l.eq_ignore_ascii_case("mailto:") { &v[7..] } else { v };
    e.trim().to_string()
}

fn parse_attendee(p: &Prop, organizer: bool) -> Option<Attendee> {
    let email = mailto(&p.value);
    if email.is_empty() {
        return None;
    }
    let name = p.param("CN").map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    if organizer {
        return Some(Attendee { email, name, role: AttendeeRole::Chair, status: AttendeeStatus::Accepted });
    }
    let role = match p.param("ROLE").map(|r| r.to_ascii_uppercase()).as_deref() {
        Some("CHAIR") => AttendeeRole::Chair,
        Some("OPT-PARTICIPANT") | Some("NON-PARTICIPANT") => AttendeeRole::Optional,
        _ => AttendeeRole::Required,
    };
    let status = match p.param("PARTSTAT").map(|r| r.to_ascii_uppercase()).as_deref() {
        Some("ACCEPTED") => AttendeeStatus::Accepted,
        Some("DECLINED") => AttendeeStatus::Declined,
        Some("TENTATIVE") => AttendeeStatus::Tentative,
        _ => AttendeeStatus::NeedsAction,
    };
    Some(Attendee { email, name, role, status })
}

/// Stable across runs and toolchains: FNV-1a over DTSTART + SUMMARY + RRULE values only.
fn synthetic_uid(c: &Comp) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for name in ["DTSTART", "SUMMARY", "RRULE"] {
        feed(name.as_bytes());
        feed(&[0xff]);
        if let Some(p) = c.prop(name) {
            feed(p.value.trim().as_bytes());
        }
        feed(&[0xfe]);
    }
    format!("noid-{h:016x}@miramail")
}

type Parsed = (CalEvent, Option<String>);

fn event_from(c: &Comp, tzs: &Tzs, trust_zoom: bool) -> Result<Option<Parsed>> {
    let Some(ds) = c.prop("DTSTART") else { return Ok(None) };
    let Some(start) = parse_dt_value(ds, &ds.value, tzs, None) else { return Ok(None) };
    let mut ev = CalEvent {
        uid: c.text("UID").map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).unwrap_or_else(|| synthetic_uid(c)),
        summary: c.text("SUMMARY").unwrap_or_default(),
        description: c.text("DESCRIPTION").unwrap_or_default(),
        location: c.text("LOCATION").unwrap_or_default(),
        ..Default::default()
    };

    let end_prop = c.prop("DTEND").or_else(|| c.prop("DUE"));
    let dur = c.prop("DURATION").and_then(|p| parse_duration(&p.value));
    match &start {
        Dv::Date(sd) => {
            ev.all_day = true;
            let from_dur = dur.and_then(|s| {
                let days = (s.saturating_add(86_399) / 86_400).max(1);
                Duration::try_days(days).and_then(|d| sd.checked_add_signed(d))
            });
            let ed = end_prop
                .and_then(|p| parse_dt_value(p, &p.value, tzs, None))
                .map(|d| match d {
                    Dv::Date(x) => x,
                    Dv::Dt(_, _, n) => n.date(),
                })
                .or(from_dur)
                .unwrap_or_else(|| sd.succ_opt().unwrap_or(*sd));
            let ed = if ed > *sd { ed } else { sd.succ_opt().unwrap_or(*sd) };
            ev.start = tm::fmt_date(*sd);
            ev.end = tm::fmt_date(ed);
        }
        Dv::Dt(sdt, tzid, _) => {
            let zone = tzid.as_deref().and_then(tm::parse_tz).map(Zone::Tz).unwrap_or(Zone::Fixed(tm::utc()));
            let edt = end_prop
                .and_then(|p| parse_dt_value(p, &p.value, tzs, tzid.as_deref()))
                .and_then(|d| match d {
                    Dv::Dt(dt, _, _) => Some(dt),
                    Dv::Date(x) => zone.local_to_instant(x.and_time(tm::midnight())),
                })
                .or_else(|| dur.and_then(|s| Duration::try_seconds(s).and_then(|d| sdt.checked_add_signed(d))))
                .unwrap_or(*sdt);
            let edt = if edt >= *sdt { edt } else { *sdt };
            ev.tzid = tzid.clone();
            ev.start = tm::fmt_dt(&zone.at(sdt));
            ev.end = tm::fmt_dt(&zone.at(&edt));
        }
    }
    let zone = event_zone(&ev);

    if let Some(p) = c.prop("RRULE") {
        let r = p.value.trim();
        if r.len() > MAX_RRULE_LEN {
            return Err(Error::Invalid("RRULE too long".into()));
        }
        if !r.is_empty() {
            ev.rrule = Some(r.to_string());
        }
    }

    for p in c.props.iter().filter(|p| p.name == "EXDATE") {
        for v in p.value.split(',') {
            let Some(d) = parse_dt_value(p, v, tzs, ev.tzid.as_deref()) else { continue };
            if ev.exdates.len() >= MAX_EXDATES {
                return Err(Error::Invalid("too many EXDATE values".into()));
            }
            ev.exdates.push(fmt_in_zone(&zone, &d, ev.all_day));
        }
    }

    ev.organizer = c.prop("ORGANIZER").and_then(|p| parse_attendee(p, true));
    for p in c.props.iter().filter(|p| p.name == "ATTENDEE") {
        if let Some(a) = parse_attendee(p, false) {
            if ev.attendees.len() >= MAX_ATTENDEES {
                return Err(Error::Invalid("too many ATTENDEE properties".into()));
            }
            ev.attendees.push(a);
        }
    }

    ev.status = match c.text("STATUS").map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("TENTATIVE") => EventStatus::Tentative,
        Some("CANCELLED") | Some("CANCELED") => EventStatus::Cancelled,
        _ => EventStatus::Confirmed,
    };
    ev.sequence = c.text("SEQUENCE").and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0).max(0);
    ev.dtstamp = c
        .prop("DTSTAMP")
        .and_then(|p| parse_dt_value(p, &p.value, tzs, Some("UTC")))
        .and_then(|d| match d {
            Dv::Dt(dt, _, _) => Some(tm::fmt_dt(&dt.with_timezone(&tm::utc()))),
            Dv::Date(_) => None,
        });

    ev.reminder_minutes = c
        .children
        .iter()
        .filter(|a| a.name == "VALARM")
        .filter_map(|a| a.prop("TRIGGER"))
        .filter(|t| !t.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME")))
        .filter_map(|t| parse_duration(&t.value))
        .find(|&s| s <= 0)
        .map(|s| (-s) / 60);

    if trust_zoom {
        let zid = c.text("X-MIRAMAIL-ZOOM-ID").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let zurl = c.text("X-MIRAMAIL-ZOOM-URL").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if let (Some(meeting_id), Some(join_url)) = (zid, zurl) {
            ev.zoom = Some(ZoomMeeting {
                meeting_id,
                join_url,
                password: c.text("X-MIRAMAIL-ZOOM-PASSWORD").filter(|s| !s.is_empty()),
                start_url: None, // the host token is never read back from any resource
            });
        }
    }

    let rec = match c.prop("RECURRENCE-ID") {
        Some(p) => {
            let Some(d) = parse_dt_value(p, &p.value, tzs, ev.tzid.as_deref()) else { return Ok(None) };
            Some(fmt_in_zone(&zone, &d, ev.all_day))
        }
        None => None,
    };
    Ok(Some((ev, rec)))
}

/// Parse an iCalendar resource with the SAFE defaults (see [`ParseOptions`]).
/// Err on oversize input, when no calendar/event component exists, or when a hard cap is exceeded
/// (attendees, EXDATEs, RRULE length) -- nothing is truncated silently.
pub fn parse_ics(data: &[u8]) -> Result<ParsedCalendar> {
    parse_ics_opts(data, ParseOptions::default())
}

/// [`parse_ics`] with explicit [`ParseOptions`].
pub fn parse_ics_opts(data: &[u8], opts: ParseOptions) -> Result<ParsedCalendar> {
    parse_inner(data, opts, MAX_ICS_BYTES)
}

pub(crate) fn parse_inner(data: &[u8], opts: ParseOptions, max_bytes: usize) -> Result<ParsedCalendar> {
    if data.len() > max_bytes {
        return Err(Error::TooLarge { max: max_bytes });
    }
    let text = String::from_utf8_lossy(data);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let (root, saw) = build_tree(text);
    if !saw {
        return Err(Error::Invalid("no VCALENDAR or VEVENT found".into()));
    }
    let tzs = Tzs::from_root(&root);
    let mut out = ParsedCalendar::default();
    let mut cals = Vec::new();
    collect(&root, "VCALENDAR", &mut cals);
    out.method = cals
        .iter()
        .find_map(|c| c.text("METHOD"))
        .map(|m| m.trim().to_ascii_uppercase())
        .filter(|m| !m.is_empty());
    let mut vevents = Vec::new();
    collect(&root, "VEVENT", &mut vevents);
    for c in vevents {
        match event_from(c, &tzs, opts.trust_zoom_props)? {
            Some((ev, None)) => out.events.push(ev),
            Some((ev, Some(rid))) => out.overrides.push(EventOverride { recurrence_id: rid, event: ev }),
            None => {}
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------------------------

pub(crate) fn fold_into(out: &mut String, line: &str) {
    let mut cur = 0usize;
    let mut limit = 75usize;
    for c in line.chars() {
        let l = c.len_utf8();
        if cur + l > limit {
            out.push_str("\r\n ");
            cur = 0;
            limit = 74;
        }
        out.push(c);
        cur += l;
    }
    out.push_str("\r\n");
}

pub(crate) fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

pub(crate) fn param_val(s: &str) -> String {
    let v: String = s.chars().filter(|c| !c.is_control() && *c != '"').collect();
    if v.contains([';', ':', ',']) {
        format!("\"{v}\"")
    } else {
        v
    }
}

fn fmt_offset(secs: i32) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    format!("{sign}{:02}{:02}", a / 3600, (a % 3600) / 60)
}

fn offset_secs(tz: &Tz, utc: NaiveDateTime) -> (i32, bool) {
    let o = tz.offset_from_utc_datetime(&utc);
    (o.fix().local_minus_utc(), o.dst_offset() != chrono::TimeDelta::zero())
}

/// Minimal VTIMEZONE for `tz` describing the transitions of `year`.
pub(crate) fn vtimezone(tz: Tz, year: i32) -> Vec<String> {
    let mut lines = vec!["BEGIN:VTIMEZONE".to_string(), format!("TZID:{}", tz.name()), format!("X-LIC-LOCATION:{}", tz.name())];
    let mut trans: Vec<(NaiveDateTime, i32, i32, bool)> = Vec::new(); // (local time before, from, to, is_dst_after)
    if let Some(jan1) = NaiveDate::from_ymd_opt(year, 1, 1) {
        let mut prev = offset_secs(&tz, jan1.and_hms_opt(0, 0, 0).unwrap_or_default());
        'outer: for day in 0..366i64 {
            let Some(d) = jan1.checked_add_signed(Duration::days(day)) else { break };
            for h in 0..24 {
                let Some(t) = d.and_hms_opt(h, 0, 0) else { continue };
                let cur = offset_secs(&tz, t);
                if cur.0 != prev.0 {
                    let local_before = t.checked_add_signed(Duration::seconds(prev.0 as i64));
                    if let Some(lb) = local_before {
                        trans.push((lb, prev.0, cur.0, cur.1));
                    }
                    prev = cur;
                    if trans.len() >= 4 {
                        break 'outer;
                    }
                }
            }
        }
    }
    if trans.is_empty() {
        let (o, _) = offset_secs(&tz, NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or_default().and_hms_opt(0, 0, 0).unwrap_or_default());
        lines.extend([
            "BEGIN:STANDARD".into(),
            "DTSTART:19700101T000000".into(),
            format!("TZOFFSETFROM:{}", fmt_offset(o)),
            format!("TZOFFSETTO:{}", fmt_offset(o)),
            "END:STANDARD".into(),
        ]);
    }
    for (lb, from, to, dst) in trans {
        let kind = if dst { "DAYLIGHT" } else { "STANDARD" };
        let d = lb.date();
        let dim = {
            let (ny, nm) = if d.month() == 12 { (d.year() + 1, 1) } else { (d.year(), d.month() + 1) };
            NaiveDate::from_ymd_opt(ny, nm, 1).and_then(|n| n.pred_opt()).map(|n| n.day()).unwrap_or(31)
        };
        let ord = if d.day() + 7 > dim { -1 } else { ((d.day() - 1) / 7 + 1) as i32 };
        let wd = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"][d.weekday().num_days_from_monday() as usize];
        lines.extend([
            format!("BEGIN:{kind}"),
            format!("DTSTART:{}", lb.format("%Y%m%dT%H%M%S")),
            format!("TZOFFSETFROM:{}", fmt_offset(from)),
            format!("TZOFFSETTO:{}", fmt_offset(to)),
            format!("RRULE:FREQ=YEARLY;BYMONTH={};BYDAY={ord}{wd}", d.month()),
            format!("END:{kind}"),
        ]);
    }
    lines.push("END:VTIMEZONE".into());
    lines
}

pub(crate) fn named_tz(tzid: &Option<String>) -> Option<Tz> {
    let tz = tzid.as_deref().and_then(tm::parse_tz)?;
    // UTC is written with a Z suffix, not a TZID.
    (tz.name() != "UTC" && tz.name() != "Etc/UTC").then_some(tz)
}

/// Returns (";PARAMS", value) for a wire start/end/exdate/recurrence-id string.
/// `tzid: None` with a `+00:00` wire offset is a FLOATING time (no `Z`, no TZID).
pub(crate) fn ics_time(ev: &CalEvent, wire: &str) -> Option<(String, String)> {
    if ev.all_day || wire.len() == 10 {
        let d = tm::parse_wire_date(wire.get(..10)?)?;
        return Some((";VALUE=DATE".into(), d.format("%Y%m%d").to_string()));
    }
    let dt = tm::parse_wire_dt(wire)?;
    match named_tz(&ev.tzid) {
        Some(tz) => Some((format!(";TZID={}", tz.name()), dt.with_timezone(&tz).format("%Y%m%dT%H%M%S").to_string())),
        None if ev.tzid.is_none() && dt.offset().local_minus_utc() == 0 => {
            Some((String::new(), dt.naive_local().format("%Y%m%dT%H%M%S").to_string()))
        }
        None => Some((String::new(), dt.naive_utc().format("%Y%m%dT%H%M%SZ").to_string())),
    }
}

pub(crate) fn dur_string(minutes: i64) -> String {
    if minutes == 0 {
        "PT0S".into()
    } else if minutes % 1440 == 0 {
        format!("-P{}D", minutes / 1440)
    } else if minutes % 60 == 0 {
        format!("-PT{}H", minutes / 60)
    } else {
        format!("-PT{minutes}M")
    }
}

pub(crate) fn now_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    DateTime::from_timestamp(secs, 0).unwrap_or_default().format("%Y%m%dT%H%M%SZ").to_string()
}

fn stamp_of(ev: &CalEvent) -> String {
    ev.dtstamp
        .as_deref()
        .and_then(tm::parse_wire_dt)
        .map(|d| d.naive_utc().format("%Y%m%dT%H%M%SZ").to_string())
        .unwrap_or_else(now_stamp)
}

pub(crate) fn person(prefix: &str, a: &Attendee, with_status: bool, schedule_client: bool) -> String {
    let mut s = String::from(prefix);
    if let Some(n) = a.name.as_deref().filter(|n| !n.is_empty()) {
        let _ = write!(s, ";CN={}", param_val(n));
    }
    if with_status {
        let role = match a.role {
            AttendeeRole::Chair => "CHAIR",
            AttendeeRole::Required => "REQ-PARTICIPANT",
            AttendeeRole::Optional => "OPT-PARTICIPANT",
        };
        let _ = write!(s, ";ROLE={role};PARTSTAT={}", a.status.as_partstat());
        if a.status == AttendeeStatus::NeedsAction {
            s.push_str(";RSVP=TRUE");
        }
        if schedule_client {
            s.push_str(";SCHEDULE-AGENT=CLIENT");
        }
    }
    let _ = write!(s, ":mailto:{}", clean(&a.email));
    s
}

#[derive(Clone, Copy, Default)]
pub(crate) struct BuildCfg {
    /// Report instead of silently skipping: unparsable times, over-cap RRULE/attendees/EXDATEs, size.
    pub strict: bool,
    /// Add `SCHEDULE-AGENT=CLIENT` to ATTENDEEs (we send the iMIP mail ourselves).
    pub schedule_client: bool,
}

pub(crate) fn check_caps(ev: &CalEvent) -> Result<()> {
    if ev.rrule.as_deref().is_some_and(|r| r.len() > MAX_RRULE_LEN) {
        return Err(Error::Invalid(format!("RRULE longer than {MAX_RRULE_LEN} bytes")));
    }
    if ev.attendees.len() > MAX_ATTENDEES {
        return Err(Error::Invalid(format!("more than {MAX_ATTENDEES} attendees")));
    }
    if ev.exdates.len() > MAX_EXDATES {
        return Err(Error::Invalid(format!("more than {MAX_EXDATES} EXDATEs")));
    }
    Ok(())
}

pub(crate) fn vevent(out: &mut String, ev: &CalEvent, rec: Option<&str>, cfg: BuildCfg) -> Result<()> {
    if cfg.strict {
        check_caps(ev)?;
    }
    let mut l = |s: String| fold_into(out, &s);
    l("BEGIN:VEVENT".into());
    l(format!("UID:{}", escape(&ev.uid)));
    l(format!("DTSTAMP:{}", stamp_of(ev)));
    if let Some(r) = rec {
        match ics_time(ev, r) {
            Some((p, v)) => l(format!("RECURRENCE-ID{p}:{v}")),
            None if cfg.strict => return Err(Error::Invalid("unparsable recurrence id".into())),
            None => {}
        }
    }
    match ics_time(ev, &ev.start) {
        Some((p, v)) => l(format!("DTSTART{p}:{v}")),
        None if cfg.strict => return Err(Error::Invalid("unparsable start time".into())),
        None => {}
    }
    if let Some((p, v)) = ics_time(ev, &ev.end) {
        l(format!("DTEND{p}:{v}"));
    }
    if let Some(r) = ev.rrule.as_deref().filter(|r| !r.trim().is_empty()) {
        l(format!("RRULE:{}", clean(r.trim().trim_start_matches("RRULE:"))));
    }
    for x in &ev.exdates {
        match ics_time(ev, x) {
            Some((p, v)) => l(format!("EXDATE{p}:{v}")),
            None if cfg.strict => return Err(Error::Invalid("unparsable EXDATE".into())),
            None => {}
        }
    }
    l(format!("SUMMARY:{}", escape(&ev.summary)));
    if !ev.description.is_empty() {
        l(format!("DESCRIPTION:{}", escape(&ev.description)));
    }
    if !ev.location.is_empty() {
        l(format!("LOCATION:{}", escape(&ev.location)));
    }
    l(format!("STATUS:{}", status_str(ev.status)));
    l(format!("SEQUENCE:{}", ev.sequence.max(0)));
    if let Some(o) = &ev.organizer {
        let mut org = person("ORGANIZER", o, false, false);
        if cfg.schedule_client {
            if let Some((pre, val)) = org.split_once(":mailto:") {
                org = format!("{pre};SCHEDULE-AGENT=CLIENT:mailto:{val}");
            }
        }
        l(org);
    }
    for a in &ev.attendees {
        l(person("ATTENDEE", a, true, cfg.schedule_client));
    }
    if let Some(z) = &ev.zoom {
        // The host start_url (token) is deliberately never serialised.
        for line in zoom_lines(z) {
            l(line);
        }
    }
    if let Some(m) = ev.reminder_minutes.filter(|m| *m >= 0) {
        for line in valarm_lines(m) {
            l(line);
        }
    }
    l("END:VEVENT".into());
    Ok(())
}

pub(crate) fn status_str(s: EventStatus) -> &'static str {
    match s {
        EventStatus::Confirmed => "CONFIRMED",
        EventStatus::Tentative => "TENTATIVE",
        EventStatus::Cancelled => "CANCELLED",
    }
}

pub(crate) fn zoom_lines(z: &ZoomMeeting) -> Vec<String> {
    let mut v = vec![format!("X-MIRAMAIL-ZOOM-ID:{}", escape(&z.meeting_id)), format!("X-MIRAMAIL-ZOOM-URL:{}", escape(&z.join_url))];
    if let Some(p) = &z.password {
        v.push(format!("X-MIRAMAIL-ZOOM-PASSWORD:{}", escape(p)));
    }
    v
}

pub(crate) fn valarm_lines(minutes: i64) -> Vec<String> {
    vec![
        "BEGIN:VALARM".into(),
        "ACTION:DISPLAY".into(),
        "DESCRIPTION:Reminder".into(),
        format!("TRIGGER:{}", dur_string(minutes)),
        "END:VALARM".into(),
    ]
}

/// Build a VCALENDAR with one VEVENT. `method` e.g. `Some("REQUEST")`.
/// Lenient: never fails, silently skips unparsable times. Prefer [`try_build_ics`].
pub fn build_ics(ev: &CalEvent, method: Option<&str>) -> String {
    build_ics_series(Some(ev), &[], method)
}

/// Build a VCALENDAR containing an optional master event and RECURRENCE-ID overrides.
/// Lenient: never fails, silently skips unparsable times. Prefer [`try_build_ics_series`].
pub fn build_ics_series(master: Option<&CalEvent>, overrides: &[EventOverride], method: Option<&str>) -> String {
    build_series_cfg(master, overrides, method, BuildCfg::default()).unwrap_or_default()
}

/// Strict [`build_ics`]: errors on an unparsable start, over-cap RRULE/attendees/EXDATEs, or output
/// above [`MAX_ICS_BYTES`] instead of emitting an invalid or truncated ICS.
pub fn try_build_ics(ev: &CalEvent, method: Option<&str>) -> Result<String> {
    try_build_ics_series(Some(ev), &[], method)
}

/// Strict [`build_ics_series`].
pub fn try_build_ics_series(master: Option<&CalEvent>, overrides: &[EventOverride], method: Option<&str>) -> Result<String> {
    build_series_cfg(master, overrides, method, BuildCfg { strict: true, schedule_client: false })
}

/// Strict build with `SCHEDULE-AGENT=CLIENT` on every ATTENDEE when `schedule_agent_client` (so a
/// CalDAV server does not send its own scheduling mail on top of ours).
pub fn try_build_ics_series_opts(master: Option<&CalEvent>, overrides: &[EventOverride], method: Option<&str>, schedule_agent_client: bool) -> Result<String> {
    build_series_cfg(master, overrides, method, BuildCfg { strict: true, schedule_client: schedule_agent_client })
}

pub(crate) fn build_series_cfg(master: Option<&CalEvent>, overrides: &[EventOverride], method: Option<&str>, cfg: BuildCfg) -> Result<String> {
    let mut out = String::new();
    fold_into(&mut out, "BEGIN:VCALENDAR");
    fold_into(&mut out, "VERSION:2.0");
    fold_into(&mut out, "PRODID:-//MiraMail//Calendar 0.1//EN");
    fold_into(&mut out, "CALSCALE:GREGORIAN");
    if let Some(m) = method.map(clean).filter(|m| !m.is_empty()) {
        fold_into(&mut out, &format!("METHOD:{m}"));
    }
    let mut seen: Vec<Tz> = Vec::new();
    for ev in master.into_iter().chain(overrides.iter().map(|o| &o.event)) {
        if ev.all_day {
            continue;
        }
        if let Some(tz) = named_tz(&ev.tzid) {
            if !seen.contains(&tz) {
                seen.push(tz);
                let year = tm::parse_wire_dt(&ev.start).map(|d| d.year()).unwrap_or(2026);
                for l in vtimezone(tz, year) {
                    fold_into(&mut out, &l);
                }
            }
        }
    }
    if let Some(m) = master {
        vevent(&mut out, m, None, cfg)?;
    }
    for o in overrides {
        vevent(&mut out, &o.event, Some(&o.recurrence_id), cfg)?;
    }
    fold_into(&mut out, "END:VCALENDAR");
    if cfg.strict && out.len() > MAX_ICS_BYTES {
        return Err(Error::TooLarge { max: MAX_ICS_BYTES });
    }
    Ok(out)
}
