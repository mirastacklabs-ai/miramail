//! Time-zone resolution for ICS parsing: IANA names, Windows names, and evaluation of embedded
//! VTIMEZONE observances (never "standard offset all year").

use crate::ics::{collect, Comp};
use crate::rrule::{expand_local, parse_rule, Rule, Until};
use crate::tm;
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveDateTime, Offset, TimeZone};
use chrono_tz::Tz;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// TZIDs longer than this are rejected outright (hostile input).
pub(crate) const MAX_TZID_LEN: usize = 128;
const MAX_VTIMEZONES: usize = 64;
const MAX_OBSERVANCES: usize = 16;
const MAX_CACHE: usize = 256;

/// CLDR windowsZones primary (territory 001) mappings. "Customized Time Zone" is deliberately absent:
/// it must be evaluated from the VTIMEZONE in the file.
pub(crate) const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Eastern Standard Time", "America/New_York"),
    ("Central Standard Time", "America/Chicago"),
    ("Mountain Standard Time", "America/Denver"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("GMT Standard Time", "Europe/London"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("FLE Standard Time", "Europe/Kiev"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("Afghanistan Standard Time", "Asia/Kabul"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("Aleutian Standard Time", "America/Adak"),
    ("Altai Standard Time", "Asia/Barnaul"),
    ("Arab Standard Time", "Asia/Riyadh"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Arabic Standard Time", "Asia/Baghdad"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Astrakhan Standard Time", "Europe/Astrakhan"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("Aus Central W. Standard Time", "Australia/Eucla"),
    ("Azerbaijan Standard Time", "Asia/Baku"),
    ("Azores Standard Time", "Atlantic/Azores"),
    ("Bahia Standard Time", "America/Bahia"),
    ("Bangladesh Standard Time", "Asia/Dhaka"),
    ("Belarus Standard Time", "Europe/Minsk"),
    ("Bougainville Standard Time", "Pacific/Bougainville"),
    ("Canada Central Standard Time", "America/Regina"),
    ("Cape Verde Standard Time", "Atlantic/Cape_Verde"),
    ("Caucasus Standard Time", "Asia/Yerevan"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Asia Standard Time", "Asia/Almaty"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Chatham Islands Standard Time", "Pacific/Chatham"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Cuba Standard Time", "America/Havana"),
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Easter Island Standard Time", "Pacific/Easter"),
    ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Fiji Standard Time", "Pacific/Fiji"),
    ("Georgian Standard Time", "Asia/Tbilisi"),
    ("Greenland Standard Time", "America/Nuuk"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("Jordan Standard Time", "Asia/Amman"),
    ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Libya Standard Time", "Africa/Tripoli"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"),
    ("Lord Howe Standard Time", "Australia/Lord_Howe"),
    ("Magadan Standard Time", "Asia/Magadan"),
    ("Magallanes Standard Time", "America/Punta_Arenas"),
    ("Marquesas Standard Time", "Pacific/Marquesas"),
    ("Mauritius Standard Time", "Indian/Mauritius"),
    ("Mid-Atlantic Standard Time", "Etc/GMT+2"),
    ("Middle East Standard Time", "Asia/Beirut"),
    ("Montevideo Standard Time", "America/Montevideo"),
    ("Morocco Standard Time", "Africa/Casablanca"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"),
    ("Myanmar Standard Time", "Asia/Yangon"),
    ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Namibia Standard Time", "Africa/Windhoek"),
    ("Nepal Standard Time", "Asia/Kathmandu"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("Norfolk Standard Time", "Pacific/Norfolk"),
    ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"),
    ("North Korea Standard Time", "Asia/Pyongyang"),
    ("Omsk Standard Time", "Asia/Omsk"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("Paraguay Standard Time", "America/Asuncion"),
    ("Qyzylorda Standard Time", "Asia/Qyzylorda"),
    ("Russia Time Zone 10", "Asia/Srednekolymsk"),
    ("Russia Time Zone 11", "Asia/Kamchatka"),
    ("Russia Time Zone 3", "Europe/Samara"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("SA Western Standard Time", "America/La_Paz"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Saint Pierre Standard Time", "America/Miquelon"),
    ("Sakhalin Standard Time", "Asia/Sakhalin"),
    ("Samoa Standard Time", "Pacific/Apia"),
    ("Sao Tome Standard Time", "Africa/Sao_Tome"),
    ("Saratov Standard Time", "Europe/Saratov"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("South Sudan Standard Time", "Africa/Juba"),
    ("Sri Lanka Standard Time", "Asia/Colombo"),
    ("Sudan Standard Time", "Africa/Khartoum"),
    ("Syria Standard Time", "Asia/Damascus"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Tasmania Standard Time", "Australia/Hobart"),
    ("Tocantins Standard Time", "America/Araguaina"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Tomsk Standard Time", "Asia/Tomsk"),
    ("Tonga Standard Time", "Pacific/Tongatapu"),
    ("Transbaikal Standard Time", "Asia/Chita"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Turks And Caicos Standard Time", "America/Grand_Turk"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("UTC", "UTC"),
    ("UTC-11", "Etc/GMT+11"),
    ("UTC-09", "Etc/GMT+9"),
    ("UTC-08", "Etc/GMT+8"),
    ("UTC-02", "Etc/GMT+2"),
    ("UTC+12", "Etc/GMT-12"),
    ("UTC+13", "Etc/GMT-13"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"),
    ("Venezuela Standard Time", "America/Caracas"),
    ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("Volgograd Standard Time", "Europe/Volgograd"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("West Asia Standard Time", "Asia/Tashkent"),
    ("West Bank Standard Time", "Asia/Hebron"),
    ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Yakutsk Standard Time", "Asia/Yakutsk"),
    ("Yukon Standard Time", "America/Whitehorse"),
];

fn windows_zone(n: &str) -> Option<Tz> {
    WINDOWS_ZONES.iter().find(|(w, _)| w.eq_ignore_ascii_case(n)).and_then(|(_, i)| tm::parse_tz(i))
}

/// IANA / Windows name, or an IANA name behind a path prefix (`/mozilla.org/20050126_1/Europe/London`).
/// Only the last three path segments are tried; names over [`MAX_TZID_LEN`] are rejected.
pub(crate) fn tz_by_suffix(n: &str) -> Option<Tz> {
    if n.len() > MAX_TZID_LEN {
        return None;
    }
    if let Some(t) = tm::parse_tz(n).or_else(|| windows_zone(n)) {
        return Some(t);
    }
    let slashes: Vec<usize> = n.match_indices('/').map(|(i, _)| i).collect();
    slashes.iter().rev().take(3).find_map(|&i| tm::parse_tz(&n[i + 1..]))
}

pub(crate) fn parse_utc_offset(s: &str) -> Option<i32> {
    let s = s.trim();
    let (sign, d) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    if !(d.len() == 4 || d.len() == 6) || !d.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i32 = d[0..2].parse().ok()?;
    let m: i32 = d[2..4].parse().ok()?;
    let sec: i32 = if d.len() == 6 { d[4..6].parse().ok()? } else { 0 };
    Some(sign * (h * 3600 + m * 60 + sec))
}

fn parse_naive(v: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(v.trim().trim_end_matches(['Z', 'z']), "%Y%m%dT%H%M%S").ok()
}

// ---------------------------------------------------------------------------------------------
// VTIMEZONE evaluation
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Obs {
    dtstart: NaiveDateTime,
    from: i32,
    to: i32,
    rule: Option<Rule>,
    rdates: Vec<NaiveDateTime>,
}

fn parse_obs(c: &Comp) -> Option<Obs> {
    let dtstart = parse_naive(&c.prop("DTSTART")?.value)?;
    let from = parse_utc_offset(&c.prop("TZOFFSETFROM")?.value)?;
    let to = parse_utc_offset(&c.prop("TZOFFSETTO")?.value)?;
    let rule = c.prop("RRULE").and_then(|p| parse_rule(&p.value).ok());
    let rdates = c
        .props
        .iter()
        .filter(|p| p.name == "RDATE")
        .flat_map(|p| p.value.split(',').take(256).filter_map(parse_naive).collect::<Vec<_>>())
        .collect();
    Some(Obs { dtstart, from, to, rule, rdates })
}

/// Offset timeline of a VTIMEZONE over 1970..=2100 (flat before/after).
#[derive(Debug)]
pub(crate) struct Vtz {
    base: i32,
    /// (utc timestamp, offset before, offset after), sorted.
    trans: Vec<(i64, i32, i32)>,
}

impl Vtz {
    fn build(obs: &[Obs]) -> Option<Vtz> {
        let from_n = NaiveDate::from_ymd_opt(1970, 1, 1)?.and_hms_opt(0, 0, 0)?;
        let to_n = NaiveDate::from_ymd_opt(2100, 12, 31)?.and_hms_opt(23, 59, 59)?;
        let mut trans: Vec<(i64, i32, i32)> = Vec::new();
        for o in obs {
            let locals: Vec<NaiveDateTime> = match &o.rule {
                Some(r) => {
                    let until = r.until.and_then(|u| match u {
                        Until::Date(d) => d.and_hms_opt(23, 59, 59),
                        Until::Floating(n) => Some(n),
                        Until::Utc(n) => n.checked_add_signed(Duration::seconds(o.from as i64)),
                    });
                    let mut v = expand_local(r, o.dtstart, until, from_n, to_n, 500);
                    v.extend(o.rdates.iter().copied());
                    v
                }
                None => std::iter::once(o.dtstart).chain(o.rdates.iter().copied()).collect(),
            };
            for l in locals {
                trans.push((l.and_utc().timestamp() - o.from as i64, o.from, o.to));
            }
        }
        trans.sort();
        trans.dedup_by_key(|t| t.0);
        let base = match trans.first() {
            Some(t) => t.1,
            None => obs.first()?.to,
        };
        Some(Vtz { base, trans })
    }

    pub(crate) fn offset_at(&self, ts: i64) -> i32 {
        let i = self.trans.partition_point(|t| t.0 <= ts);
        if i == 0 {
            self.base
        } else {
            self.trans[i - 1].2
        }
    }

    /// Wall time -> instant. Ambiguous -> earlier; gap -> interpreted with the pre-gap offset.
    pub(crate) fn local_to_instant(&self, n: NaiveDateTime) -> Option<DateTime<FixedOffset>> {
        let l = n.and_utc().timestamp();
        let before = self.offset_at(l.saturating_sub(86_400));
        let mut cands = [before, self.offset_at(l), self.offset_at(l.saturating_add(86_400))];
        cands.sort_unstable();
        let mut best: Option<i64> = None;
        for o in cands {
            let ts = l - o as i64;
            if self.offset_at(ts) == o {
                best = Some(best.map_or(ts, |b| b.min(ts)));
            }
        }
        let ts = best.unwrap_or(l - before as i64);
        FixedOffset::east_opt(self.offset_at(ts))?.timestamp_opt(ts, 0).single()
    }

    /// An IANA zone whose offsets agree with this timeline across 2015..2035 (incl. at every
    /// transition of this timeline). `None` when the timeline has no transition in that window.
    fn match_iana(&self) -> Option<Tz> {
        let lo = NaiveDate::from_ymd_opt(2015, 1, 1)?.and_hms_opt(0, 0, 0)?.and_utc().timestamp();
        let hi = NaiveDate::from_ymd_opt(2035, 12, 31)?.and_hms_opt(0, 0, 0)?.and_utc().timestamp();
        let mut samples: Vec<(i64, i32)> = Vec::new();
        let in_win: Vec<&(i64, i32, i32)> = self.trans.iter().filter(|t| t.0 > lo && t.0 < hi).collect();
        if in_win.is_empty() {
            return None;
        }
        for t in in_win {
            samples.push((t.0 - 1, t.1));
            samples.push((t.0 + 1, t.2));
        }
        let mut ts = lo;
        while ts < hi {
            samples.push((ts + 43_200, self.offset_at(ts + 43_200)));
            ts += 5 * 86_400;
        }
        let agrees = |tz: Tz| {
            samples.iter().all(|&(ts, off)| {
                DateTime::from_timestamp(ts, 0).is_some_and(|d| tz.offset_from_utc_datetime(&d.naive_utc()).fix().local_minus_utc() == off)
            })
        };
        let preferred = WINDOWS_ZONES.iter().filter_map(|(_, i)| tm::parse_tz(i));
        let all = chrono_tz::TZ_VARIANTS.iter().copied().filter(|t| t.name().contains('/') && !t.name().starts_with("Etc/"));
        preferred.chain(all).find(|&tz| agrees(tz))
    }
}

pub(crate) enum Resolved {
    Iana(Tz),
    Vtz(Rc<Vtz>),
}

impl Clone for Resolved {
    fn clone(&self) -> Self {
        match self {
            Resolved::Iana(t) => Resolved::Iana(*t),
            Resolved::Vtz(v) => Resolved::Vtz(Rc::clone(v)),
        }
    }
}

struct VtzDef {
    name_tz: Option<Tz>,
    obs: Vec<Obs>,
}

/// TZID resolver for one parse: each distinct TZID is resolved at most once.
pub(crate) struct Tzs {
    defs: HashMap<String, VtzDef>,
    cache: RefCell<HashMap<String, Option<Resolved>>>,
}

impl Tzs {
    pub(crate) fn from_root(root: &Comp) -> Tzs {
        let mut v = Vec::new();
        collect(root, "VTIMEZONE", &mut v);
        let mut defs = HashMap::new();
        for z in v.into_iter().take(MAX_VTIMEZONES) {
            let Some(id) = z.text("TZID") else { continue };
            let id = id.trim().to_string();
            if id.len() > MAX_TZID_LEN {
                continue;
            }
            let name_tz = z
                .text("X-LIC-LOCATION")
                .and_then(|l| tz_by_suffix(l.trim()))
                .or_else(|| tz_by_suffix(&id));
            let obs: Vec<Obs> = z
                .children
                .iter()
                .filter(|c| c.name == "STANDARD" || c.name == "DAYLIGHT")
                .take(MAX_OBSERVANCES)
                .filter_map(parse_obs)
                .collect();
            defs.insert(id, VtzDef { name_tz, obs });
        }
        Tzs { defs, cache: RefCell::new(HashMap::new()) }
    }

    pub(crate) fn resolve(&self, name: &str) -> Option<Resolved> {
        let n = name.trim().trim_matches('"');
        if n.len() > MAX_TZID_LEN {
            return None;
        }
        if let Some(r) = self.cache.borrow().get(n) {
            return r.clone();
        }
        let r = self.resolve_uncached(n);
        let mut c = self.cache.borrow_mut();
        if c.len() < MAX_CACHE {
            c.insert(n.to_string(), r.clone());
        }
        r
    }

    fn resolve_uncached(&self, n: &str) -> Option<Resolved> {
        if let Some(t) = tm::parse_tz(n) {
            return Some(Resolved::Iana(t));
        }
        if let Some(d) = self.defs.get(n) {
            if let Some(t) = d.name_tz {
                return Some(Resolved::Iana(t));
            }
            if let Some(v) = Vtz::build(&d.obs) {
                return Some(match v.match_iana() {
                    Some(t) => Resolved::Iana(t),
                    None => Resolved::Vtz(Rc::new(v)),
                });
            }
        }
        tz_by_suffix(n).map(Resolved::Iana)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_table_is_valid_and_large() {
        assert!(WINDOWS_ZONES.len() >= 130);
        for (w, i) in WINDOWS_ZONES {
            assert!(tm::parse_tz(i).is_some(), "{w} -> {i}");
        }
    }

    #[test]
    fn suffix_is_bounded() {
        assert!(tz_by_suffix(&"a/".repeat(300_000)).is_none());
        assert_eq!(tz_by_suffix("/mozilla.org/20050126_1/Europe/London").map(|t| t.name()), Some("Europe/London"));
        assert!(tz_by_suffix("x/y/z/w/v/Nope").is_none());
    }
}
