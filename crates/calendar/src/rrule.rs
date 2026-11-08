//! RRULE parsing and expansion (RFC 5545 section 3.3.10), implemented on chrono / chrono-tz.
//!
//! Supported: FREQ=DAILY|WEEKLY|MONTHLY|YEARLY, INTERVAL, COUNT, UNTIL, BYDAY (with ordinals),
//! BYMONTHDAY, BYMONTH, BYSETPOS, WKST. Unsupported parts (BYHOUR, BYMINUTE, BYSECOND, BYWEEKNO,
//! BYYEARDAY, sub-daily FREQ) are rejected so callers can fall back to the single base occurrence.

use crate::error::{Error, Result};
use crate::tm::{self, Zone};
use crate::types::{CalEvent, EventOverride, EventStatus, Occurrence};
use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Weekday,
};
use std::collections::HashSet;

/// Maximum occurrences produced by one expansion.
pub const MAX_OCCURRENCES: usize = 10_000;
const MAX_PERIODS: i64 = 200_000;
/// Work budget (days scanned) of one standalone expansion; `Calendar::list_occurrences` shares one
/// budget across all events of a call.
pub(crate) const DEFAULT_WORK: u64 = 20_000_000;
/// Longest accepted RRULE text.
pub const MAX_RRULE_LEN: usize = 4000;
/// Longest summary / location copied into an [`Occurrence`] (chars).
pub const MAX_OCC_TEXT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Until {
    Date(NaiveDate),
    Utc(NaiveDateTime),
    Floating(NaiveDateTime),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub freq: Freq,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<Until>,
    pub by_day: Vec<(Option<i32>, Weekday)>,
    pub by_month_day: Vec<i32>,
    pub by_month: Vec<u32>,
    pub by_set_pos: Vec<i32>,
    pub wkst: Weekday,
}

fn bad(msg: &str) -> Error {
    Error::Invalid(format!("rrule: {msg}"))
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

pub(crate) fn parse_until(v: &str) -> Option<Until> {
    let v = v.trim();
    if v.len() == 8 {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(Until::Date);
    }
    let (body, z) = match v.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (v, false),
    };
    let n = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").ok()?;
    Some(if z { Until::Utc(n) } else { Until::Floating(n) })
}

pub fn parse_rule(s: &str) -> Result<Rule> {
    let s = s.trim();
    let s = s.strip_prefix("RRULE:").unwrap_or(s);
    if s.len() > MAX_RRULE_LEN {
        return Err(bad("too long"));
    }
    let mut freq = None;
    let mut r = Rule {
        freq: Freq::Daily,
        interval: 1,
        count: None,
        until: None,
        by_day: vec![],
        by_month_day: vec![],
        by_month: vec![],
        by_set_pos: vec![],
        wkst: Weekday::Mon,
    };
    for part in s.split(';').filter(|p| !p.trim().is_empty()) {
        let (k, v) = part.split_once('=').ok_or_else(|| bad("missing '='"))?;
        let v = v.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match v.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return Err(bad("unsupported FREQ")),
                })
            }
            "INTERVAL" => {
                r.interval = v.parse().map_err(|_| bad("INTERVAL"))?;
                if r.interval == 0 || r.interval > 1_000_000 {
                    return Err(bad("INTERVAL out of range"));
                }
            }
            "COUNT" => r.count = Some(v.parse().map_err(|_| bad("COUNT"))?),
            "UNTIL" => r.until = Some(parse_until(v).ok_or_else(|| bad("UNTIL"))?),
            "WKST" => r.wkst = weekday(&v.to_ascii_uppercase()).ok_or_else(|| bad("WKST"))?,
            "BYDAY" => {
                for item in v.split(',') {
                    let item = item.trim().to_ascii_uppercase();
                    if item.len() < 2 || !item.is_char_boundary(item.len() - 2) {
                        return Err(bad("BYDAY"));
                    }
                    let (num, wd) = item.split_at(item.len() - 2);
                    let wd = weekday(wd).ok_or_else(|| bad("BYDAY"))?;
                    let ord = if num.is_empty() {
                        None
                    } else {
                        let n: i32 = num.parse().map_err(|_| bad("BYDAY ordinal"))?;
                        if n == 0 || n.abs() > 53 {
                            return Err(bad("BYDAY ordinal"));
                        }
                        Some(n)
                    };
                    r.by_day.push((ord, wd));
                }
            }
            "BYMONTHDAY" => {
                for x in v.split(',') {
                    let n: i32 = x.trim().parse().map_err(|_| bad("BYMONTHDAY"))?;
                    if n == 0 || n.abs() > 31 {
                        return Err(bad("BYMONTHDAY"));
                    }
                    r.by_month_day.push(n);
                }
            }
            "BYMONTH" => {
                for x in v.split(',') {
                    let n: u32 = x.trim().parse().map_err(|_| bad("BYMONTH"))?;
                    if !(1..=12).contains(&n) {
                        return Err(bad("BYMONTH"));
                    }
                    r.by_month.push(n);
                }
            }
            "BYSETPOS" => {
                for x in v.split(',') {
                    let n: i32 = x.trim().parse().map_err(|_| bad("BYSETPOS"))?;
                    if n == 0 || n.abs() > 366 {
                        return Err(bad("BYSETPOS"));
                    }
                    r.by_set_pos.push(n);
                }
            }
            "BYHOUR" | "BYMINUTE" | "BYSECOND" | "BYWEEKNO" | "BYYEARDAY" => {
                return Err(bad("unsupported BY* part"))
            }
            _ => {} // RSCALE, SKIP, X- parts: ignored
        }
    }
    r.freq = freq.ok_or_else(|| bad("missing FREQ"))?;
    Ok(r)
}

fn dim(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    match (
        NaiveDate::from_ymd_opt(y, m, 1),
        NaiveDate::from_ymd_opt(ny, nm, 1),
    ) {
        (Some(a), Some(b)) => (b - a).num_days() as u32,
        (Some(_), None) => 31,
        _ => 31,
    }
}

fn week_start(d: NaiveDate, wkst: Weekday) -> Option<NaiveDate> {
    let back = (d.weekday().num_days_from_monday() + 7 - wkst.num_days_from_monday()) % 7;
    d.checked_sub_signed(Duration::days(back as i64))
}

/// First day (inclusive) and last day (exclusive) of period number `n` (already multiplied by interval).
fn period(rule: &Rule, start: NaiveDate, n: i64) -> Option<(NaiveDate, NaiveDate)> {
    match rule.freq {
        Freq::Daily => {
            let d = start.checked_add_signed(Duration::days(n))?;
            Some((d, d.checked_add_signed(Duration::days(1))?))
        }
        Freq::Weekly => {
            let ws = week_start(start, rule.wkst)?.checked_add_signed(Duration::days(n.checked_mul(7)?))?;
            Some((ws, ws.checked_add_signed(Duration::days(7))?))
        }
        Freq::Monthly => {
            let idx = start.year() as i64 * 12 + start.month0() as i64 + n;
            let (y, m) = (idx.div_euclid(12), idx.rem_euclid(12) as u32 + 1);
            let a = NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, m, 1)?;
            let b = if m == 12 {
                NaiveDate::from_ymd_opt(i32::try_from(y + 1).ok()?, 1, 1)?
            } else {
                NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, m + 1, 1)?
            };
            Some((a, b))
        }
        Freq::Yearly => {
            let y = i32::try_from(start.year() as i64 + n).ok()?;
            Some((NaiveDate::from_ymd_opt(y, 1, 1)?, NaiveDate::from_ymd_opt(y.checked_add(1)?, 1, 1)?))
        }
    }
}

fn day_matches(rule: &Rule, start: NaiveDate, d: NaiveDate) -> bool {
    let (y, m, day) = (d.year(), d.month(), d.day());
    // month
    if !rule.by_month.is_empty() {
        if !rule.by_month.contains(&m) {
            return false;
        }
    } else if rule.freq == Freq::Yearly && rule.by_day.is_empty() && rule.by_month_day.is_empty() && m != start.month() {
        return false;
    }
    // month day
    let dm = dim(y, m) as i32;
    if !rule.by_month_day.is_empty() {
        if !rule.by_month_day.iter().any(|&x| if x > 0 { x == day as i32 } else { dm + x + 1 == day as i32 }) {
            return false;
        }
    } else if rule.by_day.is_empty() && matches!(rule.freq, Freq::Monthly | Freq::Yearly) && day != start.day() {
        return false;
    }
    // weekday
    if !rule.by_day.is_empty() {
        let month_scope = rule.freq == Freq::Monthly || (rule.freq == Freq::Yearly && !rule.by_month.is_empty());
        let year_scope = rule.freq == Freq::Yearly && rule.by_month.is_empty();
        let ok = rule.by_day.iter().any(|&(ord, wd)| {
            if wd != d.weekday() {
                return false;
            }
            match ord {
                None => true,
                Some(n) if month_scope || year_scope => {
                    let (pos, total) = if month_scope {
                        (day as i32, dm)
                    } else {
                        (d.ordinal() as i32, if NaiveDate::from_ymd_opt(y, 12, 31).map(|x| x.ordinal()) == Some(366) { 366 } else { 365 })
                    };
                    if n > 0 {
                        (pos - 1) / 7 + 1 == n
                    } else {
                        (total - pos) / 7 + 1 == -n
                    }
                }
                Some(_) => true, // ordinals are meaningless for other FREQs; treat as plain weekday
            }
        });
        if !ok {
            return false;
        }
    } else if rule.freq == Freq::Weekly && start.weekday() != d.weekday() {
        return false;
    }
    true
}

/// Index of the period containing `d` relative to the period containing `start`, in base units.
fn period_index(rule: &Rule, start: NaiveDate, d: NaiveDate) -> i64 {
    match rule.freq {
        Freq::Daily => (d - start).num_days(),
        Freq::Weekly => match (week_start(start, rule.wkst), week_start(d, rule.wkst)) {
            (Some(a), Some(b)) => (b - a).num_days() / 7,
            _ => 0,
        },
        Freq::Monthly => (d.year() as i64 * 12 + d.month0() as i64) - (start.year() as i64 * 12 + start.month0() as i64),
        Freq::Yearly => d.year() as i64 - start.year() as i64,
    }
}

/// Expand to local (wall-clock) start times in `[from, to]`. `dtstart` is ALWAYS the first
/// occurrence (RFC 5545 3.8.5.3) and counts toward COUNT, even when it does not match the BY* parts.
/// `until` is the already-localised UNTIL bound (inclusive). At most `cap` results.
pub fn expand_local(
    rule: &Rule,
    dtstart: NaiveDateTime,
    until: Option<NaiveDateTime>,
    from: NaiveDateTime,
    to: NaiveDateTime,
    cap: usize,
) -> Vec<NaiveDateTime> {
    let mut work = DEFAULT_WORK;
    expand_local_budget(rule, dtstart, until, from, to, cap, &mut work)
}

pub(crate) fn expand_local_budget(
    rule: &Rule,
    dtstart: NaiveDateTime,
    until: Option<NaiveDateTime>,
    from: NaiveDateTime,
    to: NaiveDateTime,
    cap: usize,
    work: &mut u64,
) -> Vec<NaiveDateTime> {
    let cap = cap.min(MAX_OCCURRENCES);
    let mut out = Vec::new();
    if cap == 0 {
        return out;
    }
    let start = dtstart.date();
    let time = dtstart.time();
    let step = rule.interval.max(1) as i64;
    let mut k: i64 = 0;
    if rule.count.is_none() {
        let idx = period_index(rule, start, from.date());
        k = (idx.div_euclid(step) - 1).max(0);
    }
    // DTSTART is the first instance.
    let mut emitted: u64 = 1;
    if dtstart >= from && dtstart <= to {
        out.push(dtstart);
        if out.len() >= cap {
            return out;
        }
    }
    for _ in 0..MAX_PERIODS {
        let Some(n) = k.checked_mul(step) else { break };
        let Some((first, last)) = period(rule, start, n) else { break };
        if first.and_time(NaiveTime::MIN) > to {
            break;
        }
        let span = (last - first).num_days().max(1) as u64;
        if *work < span {
            break;
        }
        *work -= span;
        let mut cands = Vec::new();
        let mut d = first;
        while d < last {
            if day_matches(rule, start, d) {
                cands.push(d);
            }
            match d.succ_opt() {
                Some(x) => d = x,
                None => break,
            }
        }
        if !rule.by_set_pos.is_empty() {
            let len = cands.len() as i32;
            let mut picked: Vec<NaiveDate> = rule
                .by_set_pos
                .iter()
                .filter_map(|&p| {
                    let i = if p > 0 { p - 1 } else { len + p };
                    (i >= 0 && i < len).then(|| cands[i as usize])
                })
                .collect();
            picked.sort();
            picked.dedup();
            cands = picked;
        }
        for c in cands {
            let dt = c.and_time(time);
            if dt <= dtstart {
                continue;
            }
            if until.is_some_and(|u| dt > u) {
                return out;
            }
            emitted += 1;
            if rule.count.is_some_and(|c| emitted > c as u64) {
                return out;
            }
            if dt >= from && dt <= to {
                out.push(dt);
                if out.len() >= cap {
                    return out;
                }
            }
        }
        k += 1;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Event-level expansion
// ---------------------------------------------------------------------------------------------

/// The zone used for recurrence arithmetic of an event.
pub(crate) fn event_zone(ev: &CalEvent) -> Zone {
    if let Some(tz) = ev.tzid.as_deref().and_then(tm::parse_tz) {
        return Zone::Tz(tz);
    }
    let off = tm::parse_wire_dt(&ev.start).map(|d| *d.offset()).unwrap_or_else(tm::utc);
    Zone::Fixed(off)
}

enum Base {
    AllDay { start: NaiveDate, days: i64 },
    Timed { start: DateTime<FixedOffset>, dur: Duration },
}

fn base_of(ev: &CalEvent) -> Option<Base> {
    if ev.all_day {
        let s = tm::parse_wire_date(&ev.start)?;
        let e = tm::parse_wire_date(&ev.end).unwrap_or(s);
        Some(Base::AllDay { start: s, days: (e - s).num_days().max(1) })
    } else {
        let s = tm::parse_wire_dt(&ev.start)?;
        let e = tm::parse_wire_dt(&ev.end).unwrap_or(s);
        Some(Base::Timed { start: s, dur: (e - s).max(Duration::zero()) })
    }
}

fn has_zoom(ev: &CalEvent) -> bool {
    ev.zoom.is_some() || ev.location.contains("zoom.us/j/") || ev.description.contains("zoom.us/j/")
}

fn trunc(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn make_occ(ev: &CalEvent, rec: String, start: String, end: String, recurring: bool) -> Occurrence {
    Occurrence {
        uid: ev.uid.clone(),
        calendar_id: ev.calendar_id.clone(),
        recurrence_start: rec,
        start,
        end,
        all_day: ev.all_day,
        summary: trunc(&ev.summary, MAX_OCC_TEXT),
        location: trunc(&ev.location, MAX_OCC_TEXT),
        color: String::new(),
        has_zoom: has_zoom(ev),
        is_recurring: recurring,
    }
}

/// Occurrence of a single (non-expanded) event, if it overlaps `[from, to)`.
fn single(ev: &CalEvent, rec: Option<&str>, from: &DateTime<FixedOffset>, to: &DateTime<FixedOffset>, recurring: bool) -> Option<Occurrence> {
    if ev.status == EventStatus::Cancelled {
        return None;
    }
    let (s_inst, e_inst) = match base_of(ev)? {
        Base::AllDay { start, days } => (
            start.and_time(NaiveTime::MIN),
            start.checked_add_signed(Duration::days(days))?.and_time(NaiveTime::MIN),
        ),
        Base::Timed { start, dur } => {
            let e = start.checked_add_signed(dur)?;
            (start.naive_utc(), e.naive_utc())
        }
    };
    let (f, t) = if ev.all_day { (from.naive_local(), to.naive_local()) } else { (from.naive_utc(), to.naive_utc()) };
    let overlaps = s_inst < t && (e_inst > f || (e_inst == s_inst && s_inst >= f));
    overlaps.then(|| make_occ(ev, rec.unwrap_or(&ev.start).to_string(), ev.start.clone(), ev.end.clone(), recurring))
}

/// Occurrences of RECURRENCE-ID overrides whose master is not stored (single-instance invites).
pub(crate) fn override_occurrences(overrides: &[EventOverride], from: &DateTime<FixedOffset>, to: &DateTime<FixedOffset>) -> Vec<Occurrence> {
    overrides.iter().filter_map(|o| single(&o.event, Some(&o.recurrence_id), from, to, true)).collect()
}

/// Expand an event (plus RECURRENCE-ID overrides) into occurrences overlapping `[from, to)`.
/// `color` is left empty. Capped at [`MAX_OCCURRENCES`]. Cancelled events/overrides yield nothing.
pub fn expand_event(ev: &CalEvent, overrides: &[EventOverride], from: DateTime<FixedOffset>, to: DateTime<FixedOffset>) -> Vec<Occurrence> {
    let mut work = DEFAULT_WORK;
    expand_event_budget(ev, overrides, from, to, &mut work)
}

/// As [`expand_event`] but charging scanned days to a shared `work` budget (hostile-input guard for
/// calls that expand many events).
pub(crate) fn expand_event_budget(ev: &CalEvent, overrides: &[EventOverride], from: DateTime<FixedOffset>, to: DateTime<FixedOffset>, work: &mut u64) -> Vec<Occurrence> {
    let mut out = Vec::new();
    let rule = ev.rrule.as_deref().and_then(|r| parse_rule(r).ok());
    let Some(rule) = rule else {
        out.extend(single(ev, None, &from, &to, false));
        return out;
    };
    if ev.status != EventStatus::Cancelled {
        if let Some(base) = base_of(ev) {
            expand_master(ev, &rule, &base, overrides, &from, &to, &mut out, work);
        }
    }
    // Overrides are standalone occurrences (they may have moved).
    for o in overrides {
        if out.len() >= MAX_OCCURRENCES {
            break;
        }
        out.extend(single(&o.event, Some(&o.recurrence_id), &from, &to, true));
    }
    out
}

/// EXDATE / RECURRENCE-ID lookups, parsed once per expansion.
struct Skip {
    all_day: bool,
    /// timed: bare-date exdates; all-day: date prefix of every exdate
    ex_days: HashSet<String>,
    ex_inst: HashSet<i64>,
    ov_days: HashSet<String>,
    ov_inst: HashSet<i64>,
}

impl Skip {
    fn new(ev: &CalEvent, overrides: &[EventOverride]) -> Skip {
        let mut s = Skip { all_day: ev.all_day, ex_days: HashSet::new(), ex_inst: HashSet::new(), ov_days: HashSet::new(), ov_inst: HashSet::new() };
        for x in &ev.exdates {
            if ev.all_day {
                if let Some(p) = x.get(..10) {
                    s.ex_days.insert(p.to_string());
                }
            } else if x.len() == 10 {
                s.ex_days.insert(x.clone());
            } else if let Some(d) = tm::parse_wire_dt(x) {
                s.ex_inst.insert(d.timestamp());
            }
        }
        for o in overrides {
            if ev.all_day {
                if let Some(p) = o.recurrence_id.get(..10) {
                    s.ov_days.insert(p.to_string());
                }
            } else if let Some(d) = tm::parse_wire_dt(&o.recurrence_id) {
                s.ov_inst.insert(d.timestamp());
            }
        }
        s
    }

    fn skipped(&self, day: &str, ts: i64) -> bool {
        if self.all_day {
            self.ex_days.contains(day) || self.ov_days.contains(day)
        } else {
            self.ex_days.contains(day) || self.ex_inst.contains(&ts) || self.ov_inst.contains(&ts)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_master(ev: &CalEvent, rule: &Rule, base: &Base, overrides: &[EventOverride], from: &DateTime<FixedOffset>, to: &DateTime<FixedOffset>, out: &mut Vec<Occurrence>, work: &mut u64) {
    let zone = event_zone(ev);
    let (dtstart, dur_pad) = match base {
        Base::AllDay { start, days } => (start.and_time(NaiveTime::MIN), Duration::days(*days)),
        Base::Timed { start, dur } => (zone.to_local(start), *dur),
    };
    let until = rule.until.and_then(|u| match u {
        Until::Date(d) => d.and_hms_opt(23, 59, 59),
        Until::Floating(n) => Some(n),
        Until::Utc(n) => {
            if ev.all_day {
                Some(n)
            } else {
                Some(zone.to_local(&n.and_utc().fixed_offset()))
            }
        }
    });
    let pad = dur_pad + Duration::days(2);
    let (lf, lt) = if ev.all_day {
        (from.naive_local().checked_sub_signed(pad), from.naive_local().max(to.naive_local()).checked_add_signed(Duration::days(1)))
    } else {
        (
            from.checked_sub_signed(pad).map(|d| zone.to_local(&d)),
            to.checked_add_signed(Duration::days(2)).map(|d| zone.to_local(&d)),
        )
    };
    let (Some(lf), Some(lt)) = (lf, lt) else { return };
    let lt = lt.max(lf);
    // The series ended before the window: nothing to expand (DTSTART itself aside).
    if until.is_some_and(|u| u < lf && u >= dtstart) {
        return;
    }
    let starts = expand_local_budget(rule, dtstart, until, lf, lt, MAX_OCCURRENCES, work);
    let skip = Skip::new(ev, overrides);
    let (f, t) = if ev.all_day { (from.naive_local(), to.naive_local()) } else { (from.naive_utc(), to.naive_utc()) };

    for l in starts {
        if out.len() >= MAX_OCCURRENCES {
            return;
        }
        let (rec, s_str, e_str, s_cmp, e_cmp, ts) = match base {
            Base::AllDay { days, .. } => {
                let d = l.date();
                let Some(e) = d.checked_add_signed(Duration::days(*days)) else { continue };
                (tm::fmt_date(d), tm::fmt_date(d), tm::fmt_date(e), l, e.and_time(NaiveTime::MIN), 0)
            }
            Base::Timed { dur, .. } => {
                let Some(inst) = zone.local_to_instant(l) else { continue };
                let Some(end) = inst.checked_add_signed(*dur) else { continue };
                let end = zone.at(&end);
                (tm::fmt_dt(&inst), tm::fmt_dt(&inst), tm::fmt_dt(&end), inst.naive_utc(), end.naive_utc(), inst.timestamp())
            }
        };
        if skip.skipped(&tm::fmt_date(l.date()), ts) {
            continue;
        }
        if s_cmp < t && (e_cmp > f || (e_cmp == s_cmp && s_cmp >= f)) {
            out.push(make_occ(ev, rec, s_str, e_str, true));
        }
    }
}
