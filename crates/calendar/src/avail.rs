//! Availability arithmetic for the AI assistant (and anything else that needs "when am I free?").
//!
//! Pure functions over epoch seconds and an IANA zone; no storage, no I/O. Busy blocks are half-open
//! `[start, end)`. Working hours are wall-clock windows in the user's zone, so a day with a DST change
//! has a working window of the real elapsed length (e.g. 01:00-05:00 is 3 h on a spring-forward day and
//! 5 h on a fall-back day). A wall time that does not exist (the spring-forward gap) moves forward by
//! the gap; an ambiguous one (fall-back) takes the earlier instant.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone};
pub use chrono_tz::Tz;

/// Longest range the assistant may ask about in one call.
pub const MAX_RANGE_DAYS: i64 = 62;

pub fn parse_tz(name: &str) -> Option<Tz> {
    name.trim().parse::<Tz>().ok()
}

/// A busy interval in epoch seconds, `end` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    pub start: i64,
    pub end: i64,
    /// Every event merged into this block is tentative.
    pub tentative: bool,
}

/// Sort and merge overlapping or touching blocks. A merged block is tentative only when all its parts are.
pub fn merge_busy(blocks: &[Busy]) -> Vec<Busy> {
    let mut v: Vec<Busy> = blocks.iter().copied().filter(|b| b.end > b.start).collect();
    v.sort_by_key(|b| (b.start, b.end));
    let mut out: Vec<Busy> = Vec::with_capacity(v.len());
    for b in v {
        match out.last_mut() {
            Some(last) if b.start <= last.end => {
                last.end = last.end.max(b.end);
                last.tentative &= b.tentative;
            }
            _ => out.push(b),
        }
    }
    out
}

/// Working hours: minutes after local midnight (`end_min` may be 1440) on the ticked weekdays (Monday = 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkHours {
    pub start_min: u32,
    pub end_min: u32,
    pub days: [bool; 7],
}

impl Default for WorkHours {
    fn default() -> Self {
        WorkHours { start_min: 9 * 60, end_min: 18 * 60, days: [true, true, true, true, true, false, false] }
    }
}

impl WorkHours {
    /// `HH:MM-HH:MM Mon-Fri` style description for the model.
    pub fn describe(&self) -> String {
        const N: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let days: Vec<&str> = (0..7).filter(|i| self.days[*i]).map(|i| N[i]).collect();
        let days = if days.len() == 5 && self.days[..5].iter().all(|d| *d) { "Mon-Fri".to_string() } else { days.join(",") };
        format!("{:02}:{:02}-{:02}:{:02} {}", self.start_min / 60, self.start_min % 60, self.end_min / 60,self.end_min % 60, days)
    }
}

/// Wall time -> instant. Ambiguous -> earlier; in a gap -> shifted forward by up to 3 h.
pub fn local_instant<Z: TimeZone>(tz: &Z, n: NaiveDateTime) -> Option<i64> {
    let mut cur = n;
    for _ in 0..4 {
        if let Some(dt) = tz.from_local_datetime(&cur).earliest() {
            return Some(dt.timestamp());
        }
        cur = cur.checked_add_signed(Duration::hours(1))?;
    }
    None
}

/// Local calendar date of an instant.
pub fn local_date<Z: TimeZone>(tz: &Z, ts: i64) -> Option<NaiveDate> {
    Some(tz.timestamp_opt(ts, 0).single()?.date_naive())
}

fn at_minute(date: NaiveDate, min: u32) -> Option<NaiveDateTime> {
    date.and_hms_opt(0, 0, 0)?.checked_add_signed(Duration::minutes(i64::from(min)))
}

/// The working windows `[start, end)` that intersect `[from, to)`, clipped to it, in order.
pub fn working_windows<Z: TimeZone>(tz: &Z, from: i64, to: i64, wh: &WorkHours) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    if to <= from || wh.end_min <= wh.start_min {
        return out;
    }
    let (Some(first), Some(last)) = (local_date(tz, from), local_date(tz, to)) else { return out };
    let mut d = first - Duration::days(1);
    let stop = last + Duration::days(1);
    while d <= stop {
        let wd = d.weekday().num_days_from_monday() as usize;
        if wh.days[wd] {
            if let (Some(s), Some(e)) = (at_minute(d, wh.start_min).and_then(|n| local_instant(tz, n)), at_minute(d, wh.end_min).and_then(|n| local_instant(tz, n))) {
                let (s, e) = (s.max(from), e.min(to));
                if e > s {
                    out.push((s, e));
                }
            }
        }
        d += Duration::days(1);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotQuery {
    pub from: i64,
    pub to: i64,
    pub duration: i64,
    /// Free gap required before and after each busy block.
    pub buffer: i64,
    /// `None` = any time of day.
    pub work: Option<WorkHours>,
    pub max_results: usize,
    /// At most this many slots start on one local day.
    pub per_day: usize,
    /// Nothing starts before this instant (now).
    pub not_before: i64,
    /// Slot starts are rounded up to a multiple of this many seconds.
    pub grid: i64,
}

fn subtract(window: (i64, i64), busy: &[Busy]) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut cur = window.0;
    for b in busy {
        if b.end <= cur {
            continue;
        }
        if b.start >= window.1 {
            break;
        }
        if b.start > cur {
            out.push((cur, b.start.min(window.1)));
        }
        cur = cur.max(b.end);
        if cur >= window.1 {
            break;
        }
    }
    if cur < window.1 {
        out.push((cur, window.1));
    }
    out
}

fn ceil_to(x: i64, grid: i64) -> i64 {
    if grid <= 1 {
        return x;
    }
    x.div_euclid(grid) * grid + if x.rem_euclid(grid) == 0 { 0 } else { grid }
}

/// Free slots of exactly `duration` seconds, earliest first, back to back inside each free interval,
/// at most `per_day` per local day and `max_results` overall.
pub fn find_slots<Z: TimeZone>(tz: &Z, busy: &[Busy], q: &SlotQuery) -> Vec<(i64, i64)> {
    let lo = q.from.max(q.not_before);
    let hi = q.to;
    if q.duration <= 0 || lo >= hi || q.max_results == 0 || q.per_day == 0 {
        return Vec::new();
    }
    let expanded: Vec<Busy> = busy.iter().map(|b| Busy { start: b.start - q.buffer.max(0), end: b.end + q.buffer.max(0), tentative: b.tentative }).collect();
    let busy = merge_busy(&expanded);
    let windows = match &q.work {
        Some(wh) => working_windows(tz, lo, hi, wh),
        None => vec![(lo, hi)],
    };
    let mut per_day: std::collections::HashMap<NaiveDate, usize> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for w in windows {
        for (a, b) in subtract(w, &busy) {
            let mut s = ceil_to(a, q.grid);
            while s + q.duration <= b {
                let day = local_date(tz, s);
                let n = day.map_or(0, |d| *per_day.get(&d).unwrap_or(&0));
                if n < q.per_day {
                    out.push((s, s + q.duration));
                    if let Some(d) = day {
                        *per_day.entry(d).or_insert(0) += 1;
                    }
                    if out.len() >= q.max_results {
                        return out;
                    }
                }
                s = ceil_to(s + q.duration, q.grid);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn ts(tz: &Tz, y: i32, m: u32, d: u32, h: u32, mi: u32) -> i64 {
        local_instant(tz, NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, 0).unwrap()).unwrap()
    }

    fn q(from: i64, to: i64, dur_min: i64) -> SlotQuery {
        SlotQuery { from, to, duration: dur_min * 60, buffer: 0, work: Some(WorkHours::default()), max_results: 50, per_day: 50, not_before: 0, grid: 900 }
    }

    #[test]
    fn merge_overlapping_touching_and_tentative() {
        let m = merge_busy(&[
            Busy { start: 100, end: 200, tentative: true },
            Busy { start: 150, end: 300, tentative: true },
            Busy { start: 300, end: 400, tentative: false },
            Busy { start: 500, end: 600, tentative: true },
            Busy { start: 10, end: 10, tentative: false },
        ]);
        assert_eq!(m, vec![Busy { start: 100, end: 400, tentative: false }, Busy { start: 500, end: 600, tentative: true }]);
    }

    #[test]
    fn plain_day_with_two_meetings_in_utc() {
        let tz = Tz::UTC;
        let day = |h, m| ts(&tz, 2026, 11, 10, h, m); // Tuesday
        let busy = [Busy { start: day(10, 0), end: day(11, 0), tentative: false }, Busy { start: day(14, 0), end: day(15, 30), tentative: false }];
        let slots = find_slots(&tz, &busy, &q(day(0, 0), ts(&tz, 2026, 11, 11, 0, 0), 60));
        let got: Vec<(i64, i64)> = slots.iter().map(|(a, b)| ((a - day(0, 0)) / 60, (b - day(0, 0)) / 60)).collect();
        // 09:00-10:00 | 11:00-12:00, 12:00-13:00, 13:00-14:00 | 15:30-16:30, 16:30-17:30
        assert_eq!(got, vec![(540, 600), (660, 720), (720, 780), (780, 840), (930, 990), (990, 1050)]);
    }

    #[test]
    fn buffers_widen_busy_blocks_but_not_the_working_window() {
        let tz = Tz::UTC;
        let day = |h, m| ts(&tz, 2026, 11, 10, h, m);
        let busy = [Busy { start: day(10, 0), end: day(11, 0), tentative: false }];
        let mut query = q(day(0, 0), ts(&tz, 2026, 11, 11, 0, 0), 60);
        query.buffer = 30 * 60;
        let got: Vec<i64> = find_slots(&tz, &busy, &query).iter().map(|(a, _)| (a - day(0, 0)) / 60).collect();
        // 09:00-09:30 is too short; the first slot starts at 11:30 (busy ends 11:00 + 30 min buffer).
        assert_eq!(got, vec![690, 750, 810, 870, 930, 990]);
    }

    #[test]
    fn weekend_and_outside_hours_are_skipped_and_cap_per_day_applies() {
        let tz = Tz::UTC;
        // Fri 2026-11-06 .. Mon 2026-11-09 (Sat/Sun are not working days)
        let from = ts(&tz, 2026, 11, 6, 0, 0);
        let to = ts(&tz, 2026, 11, 10, 0, 0);
        let mut query = q(from, to, 120);
        query.per_day = 2;
        let slots = find_slots(&tz, &[], &query);
        let days: Vec<u32> = slots.iter().map(|(a, _)| local_date(&tz, *a).unwrap().day()).collect();
        assert_eq!(days, vec![6, 6, 9, 9]);
        assert_eq!(slots[0], (ts(&tz, 2026, 11, 6, 9, 0), ts(&tz, 2026, 11, 6, 11, 0)));
        assert_eq!(slots[1], (ts(&tz, 2026, 11, 6, 11, 0), ts(&tz, 2026, 11, 6, 13, 0)));
        // any-time search is not limited to working hours
        let mut any = q(from, ts(&tz, 2026, 11, 6, 6, 0), 120);
        any.work = None;
        assert_eq!(find_slots(&tz, &[], &any).len(), 3);
    }

    #[test]
    fn not_before_and_max_results_and_grid() {
        let tz = Tz::UTC;
        let day = |h, m| ts(&tz, 2026, 11, 10, h, m);
        let mut query = q(day(0, 0), ts(&tz, 2026, 11, 11, 0, 0), 30);
        query.not_before = day(9, 7); // rounds up to 09:15
        query.max_results = 2;
        let got = find_slots(&tz, &[], &query);
        assert_eq!(got, vec![(day(9, 15), day(9, 45)), (day(9, 45), day(10, 15))]);
        assert!(find_slots(&tz, &[], &SlotQuery { duration: 0, ..query }).is_empty());
        assert!(find_slots(&tz, &[], &SlotQuery { not_before: day(23, 59), to: day(23, 59), ..query }).is_empty());
    }

    // ---- DST ---------------------------------------------------------------------------------

    fn night_hours() -> WorkHours {
        WorkHours { start_min: 60, end_min: 300, days: [true; 7] } // 01:00-05:00 local, every day
    }

    fn window_len(tz: &Tz, y: i32, m: u32, d: u32) -> i64 {
        let from = ts(tz, y, m, d, 0, 0) - 86_400;
        let w = working_windows(tz, from, from + 4 * 86_400, &night_hours());
        let day_start = ts(tz, y, m, d, 0, 0);
        let hit = w.iter().find(|(s, e)| *s >= day_start && *e <= day_start + 86_400 + 3600).expect("window on that day");
        (hit.1 - hit.0) / 60
    }

    #[test]
    fn working_window_lengths_follow_real_elapsed_time_in_three_zones() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let berlin: Tz = "Europe/Berlin".parse().unwrap();
        let sydney: Tz = "Australia/Sydney".parse().unwrap();
        // (zone, spring-forward day, fall-back day): 01:00-05:00 is 3 h / 5 h on those days, 4 h otherwise
        assert_eq!(window_len(&ny, 2026, 3, 8), 180);
        assert_eq!(window_len(&ny, 2026, 11, 1), 300);
        assert_eq!(window_len(&ny, 2026, 11, 3), 240);
        assert_eq!(window_len(&berlin, 2026, 3, 29), 180);
        assert_eq!(window_len(&berlin, 2026, 10, 25), 300);
        assert_eq!(window_len(&sydney, 2026, 10, 4), 180); // southern hemisphere: spring forward in October
        assert_eq!(window_len(&sydney, 2026, 4, 5), 300); // fall back in April
    }

    #[test]
    fn slot_fit_depends_on_real_hours_across_a_transition() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let query = |y: i32, m: u32, d: u32, dur: i64| {
            let from = ts(&ny, y, m, d, 0, 0);
            SlotQuery { from, to: from + 86_400, duration: dur * 60, buffer: 0, work: Some(night_hours()), max_results: 10, per_day: 10, not_before: 0, grid: 900 }
        };
        // spring forward 2026-03-08: 01:00-05:00 local is only 3 elapsed hours
        assert_eq!(find_slots(&ny, &[], &query(2026, 3, 8, 180)).len(), 1);
        assert!(find_slots(&ny, &[], &query(2026, 3, 8, 240)).is_empty());
        // fall back 2026-11-01: 5 elapsed hours, so a 4 h slot fits
        assert_eq!(find_slots(&ny, &[], &query(2026, 11, 1, 240)).len(), 1);
        assert_eq!(find_slots(&ny, &[], &query(2026, 11, 1, 300)).len(), 1);
        // slot boundaries on the spring-forward day: 01:00 EST .. 03:00+ EDT with the gap removed
        let s = find_slots(&ny, &[], &query(2026, 3, 8, 60));
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].0, ts(&ny, 2026, 3, 8, 1, 0));
        assert_eq!(s[1].0, s[0].0 + 3600);
        assert_eq!(ny.timestamp_opt(s[1].0, 0).unwrap().format("%H:%M").to_string(), "03:00");
    }

    #[test]
    fn busy_block_across_the_gap_is_respected_in_berlin() {
        let berlin: Tz = "Europe/Berlin".parse().unwrap();
        let day = ts(&berlin, 2026, 3, 29, 0, 0); // 02:00 -> 03:00
        let busy = [Busy { start: ts(&berlin, 2026, 3, 29, 1, 30), end: ts(&berlin, 2026, 3, 29, 3, 30), tentative: false }];
        let query = SlotQuery { from: day, to: day + 86_400, duration: 3600, buffer: 0, work: Some(night_hours()), max_results: 10, per_day: 10, not_before: 0, grid: 900 };
        let got = find_slots(&berlin, &busy, &query);
        // 01:00-01:30 too short; after 03:30 CEST there is 03:30-04:30 and 04:30-05:30>05:00 (no)
        assert_eq!(got.len(), 1);
        assert_eq!(berlin.timestamp_opt(got[0].0, 0).unwrap().format("%H:%M").to_string(), "03:30");
    }

    #[test]
    fn nonexistent_wall_time_moves_forward_and_ambiguous_takes_earlier() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let gap = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap().and_hms_opt(2, 30, 0).unwrap();
        let t = local_instant(&ny, gap).unwrap();
        assert_eq!(ny.timestamp_opt(t, 0).unwrap().format("%H:%M").to_string(), "03:30");
        let amb = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap().and_hms_opt(1, 30, 0).unwrap();
        let t = local_instant(&ny, amb).unwrap();
        assert_eq!(ny.timestamp_opt(t, 0).unwrap().format("%z").to_string(), "-0400"); // the first 01:30 (EDT)
    }

    #[test]
    fn describe_working_hours() {
        assert_eq!(WorkHours::default().describe(), "09:00-18:00 Mon-Fri");
        assert_eq!(WorkHours { start_min: 480, end_min: 1020, days: [true, false, true, false, false, false, true] }.describe(), "08:00-17:00 Mon,Wed,Sun");
    }
}
