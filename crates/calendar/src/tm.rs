//! Internal time helpers: wire-format parsing/formatting and zone handling.

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone};
use chrono_tz::Tz;

pub(crate) fn fmt_dt(dt: &DateTime<FixedOffset>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

pub(crate) fn fmt_date(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

pub(crate) fn parse_wire_dt(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s.trim()).ok()
}

pub(crate) fn parse_wire_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

pub(crate) fn midnight() -> NaiveTime {
    NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default()
}

pub(crate) fn utc() -> FixedOffset {
    chrono::Utc.fix()
}

/// Timestamp (seconds) of a wire value; dates are taken as UTC midnight.
pub(crate) fn wire_ts(s: &str) -> Option<i64> {
    if let Some(dt) = parse_wire_dt(s) {
        return Some(dt.timestamp());
    }
    parse_wire_date(s).map(|d| d.and_time(midnight()).and_utc().timestamp())
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Zone {
    Tz(Tz),
    Fixed(FixedOffset),
}

impl Zone {
    /// Resolve a local wall time to an instant. Ambiguous -> earlier; gap -> shifted forward.
    pub(crate) fn local_to_instant(&self, n: NaiveDateTime) -> Option<DateTime<FixedOffset>> {
        match self {
            Zone::Fixed(o) => o.from_local_datetime(&n).single(),
            Zone::Tz(tz) => {
                let mut cur = n;
                for _ in 0..4 {
                    if let Some(dt) = tz.from_local_datetime(&cur).earliest() {
                        return Some(dt.fixed_offset());
                    }
                    cur = cur.checked_add_signed(Duration::hours(1))?;
                }
                None
            }
        }
    }

    pub(crate) fn at(&self, dt: &DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        match self {
            Zone::Fixed(o) => dt.with_timezone(o),
            Zone::Tz(tz) => dt.with_timezone(tz).fixed_offset(),
        }
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn to_local(&self, dt: &DateTime<FixedOffset>) -> NaiveDateTime {
        self.at(dt).naive_local()
    }
}

pub(crate) fn parse_tz(name: &str) -> Option<Tz> {
    name.trim().parse::<Tz>().ok()
}
