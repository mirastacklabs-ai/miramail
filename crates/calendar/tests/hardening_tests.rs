//! Regression tests for the security/logic review findings (Q01..Q20).
#[allow(dead_code)]
mod fixtures;
use calendar::itip;
use calendar::*;
use chrono::{DateTime, FixedOffset};
use fixtures::crlf;
use std::time::{Duration, Instant};

fn dt(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn parse(s: &str) -> ParsedCalendar {
    parse_ics(&crlf(s)).unwrap()
}

fn one_event(props: &str) -> String {
    format!("BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:h1\n{props}\nEND:VEVENT\nEND:VCALENDAR\n")
}

fn unfolded(s: &str) -> String {
    s.replace("\r\n ", "")
}

// ------------------------------------------------------------------ Q01 durations

#[test]
fn q01_hostile_durations_never_panic() {
    let big = i64::MAX.to_string();
    let cases = [
        format!("DTSTART;VALUE=DATE:20261110\nDURATION:P{big}D"),
        format!("DTSTART;VALUE=DATE:20261110\nDURATION:P{big}W"),
        format!("DTSTART;VALUE=DATE:20261110\nDURATION:PT{big}S"),
        "DTSTART;VALUE=DATE:20261110\nDURATION:P99999999999999999999D".to_string(),
        "DTSTART;VALUE=DATE:20261110\nDURATION:-P5D".to_string(),
        "DTSTART;VALUE=DATE:20261110\nDURATION:garbage".to_string(),
        format!("DTSTART:20261110T090000Z\nDURATION:PT{big}S"),
        format!("DTSTART:20261110T090000Z\nDURATION:-PT{big}S"),
        format!("DTSTART:20261110T090000Z\nDURATION:P{big}D"),
        "DTSTART:20261110T090000Z\nDURATION:PT\n".to_string(),
        "DTSTART:20261110T090000Z\nDURATION:P1X".to_string(),
    ];
    for c in cases {
        let src = one_event(&c);
        let p = parse_ics(&crlf(&src)).unwrap();
        for e in &p.events {
            let _ = build_ics(e, None);
            let _ = expand_event(e, &[], dt("2026-01-01T00:00:00+00:00"), dt("2030-01-01T00:00:00+00:00"));
        }
    }
    // clamped to ~10 years, not wrapped
    let p = parse(&one_event(&format!("DTSTART:20261110T090000Z\nDURATION:PT{big}S")));
    let e = &p.events[0];
    let span = dt(&e.end) - dt(&e.start);
    assert!(span.num_days() >= 3649 && span.num_days() <= 3651, "{}", span.num_days());
    let p = parse(&one_event(&format!("DTSTART;VALUE=DATE:20261110\nDURATION:P{big}D")));
    assert!(p.events[0].end > p.events[0].start);
    // hostile TRIGGERs
    for t in [format!("-PT{big}S"), format!("-P{big}W"), "-P99999999999999999999D".to_string(), format!("PT{big}S"), "-PT".into(), "garbage".into()] {
        let src = one_event(&format!("DTSTART:20261110T090000Z\nBEGIN:VALARM\nTRIGGER:{t}\nEND:VALARM"));
        let p = parse_ics(&crlf(&src)).unwrap();
        let _ = p.events[0].reminder_minutes;
    }
    let p = parse(&one_event(&format!("DTSTART:20261110T090000Z\nBEGIN:VALARM\nTRIGGER:-PT{big}S\nEND:VALARM")));
    assert_eq!(p.events[0].reminder_minutes, Some(315_360_000 / 60));
}

// ------------------------------------------------------------------ Q02 tz cost

#[test]
fn q02_huge_tzid_and_many_exdates_are_fast() {
    let t0 = Instant::now();
    let tzid = "a/".repeat(200_000); // 400 KB, twice in the file
    let exdates: Vec<String> = (0..9000).map(|i| format!("2027{:02}{:02}T090000", i % 12 + 1, i % 28 + 1)).collect();
    let src = format!(
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART;TZID={tzid}:20261110T090000\r\nEXDATE;TZID={tzid}:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        exdates.join(",")
    );
    assert!(src.len() < MAX_ICS_BYTES);
    let p = parse_ics(src.as_bytes()).unwrap();
    assert_eq!(p.events[0].exdates.len(), 9000);
    // 10k separate EXDATE lines with a short unknown TZID and a mozilla-style path TZID
    let mut s = String::from("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART;TZID=/mozilla.org/20050126_1/Europe/London:20261110T090000\r\n");
    for i in 0..10_000 {
        s.push_str(&format!("EXDATE;TZID=/mozilla.org/20050126_1/Europe/London:2027{:02}{:02}T090000\r\n", i % 12 + 1, i % 28 + 1));
    }
    s.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    let p = parse_ics(s.as_bytes()).unwrap();
    assert_eq!(p.events[0].tzid.as_deref(), Some("Europe/London"));
    assert_eq!(p.events[0].exdates.len(), 10_000);
    assert!(t0.elapsed() < Duration::from_secs(5), "took {:?}", t0.elapsed());
}

// ------------------------------------------------------------------ Q03 / Q04 zoom

const ZOOM_PROPS: &str = "X-MIRAMAIL-ZOOM-ID:81234567890\nX-MIRAMAIL-ZOOM-URL:https://evil.example/j/1\nX-MIRAMAIL-ZOOM-PASSWORD:pw\nX-MIRAMAIL-ZOOM-STARTURL:https://zoom.us/s/1?zak=SECRET";

#[test]
fn q03_start_url_never_serialised_or_stored() {
    let mut e = CalEvent {
        uid: "z1".into(),
        calendar_id: "personal".into(),
        summary: "Z".into(),
        start: "2026-11-10T09:00:00+00:00".into(),
        end: "2026-11-10T10:00:00+00:00".into(),
        tzid: Some("UTC".into()),
        zoom: Some(ZoomMeeting { meeting_id: "1".into(), join_url: "https://zoom.us/j/1".into(), password: None, start_url: Some("https://zoom.us/s/1?zak=SECRET".into()) }),
        ..Default::default()
    };
    for ics in [build_ics(&e, None), build_ics(&e, Some("REQUEST")), itip::build_request(&e), itip::build_cancel(&e)] {
        assert!(!ics.contains("STARTURL") && !ics.contains("SECRET"));
        assert!(ics.contains("X-MIRAMAIL-ZOOM-ID:1"));
    }
    let db = Calendar::open_in_memory().unwrap();
    db.upsert_event(&e).unwrap();
    let got = db.get_event("z1", "personal").unwrap().unwrap();
    assert_eq!(got.zoom.as_ref().unwrap().meeting_id, "1");
    assert_eq!(got.zoom.unwrap().start_url, None);
    let put = db.resource_ics_for_put("z1", "personal").unwrap();
    assert!(!put.contains("SECRET") && !put.contains("STARTURL"));
    assert!(!itip::request_body(&e).contains("SECRET"));
    e.zoom.as_mut().unwrap().start_url = None;
}

#[test]
fn q04_inbound_zoom_props_ignored_unless_trusted() {
    let src = one_event(&format!("DTSTART:20261110T090000Z\n{ZOOM_PROPS}"));
    assert!(parse_ics(&crlf(&src)).unwrap().events[0].zoom.is_none());
    let inv = itip::invite_from_ics(&crlf(&src.replace("VERSION:2.0", "VERSION:2.0\nMETHOD:REQUEST")), &[], "a", false).unwrap().unwrap();
    assert!(inv.event.zoom.is_none());
    let z = parse_ics_opts(&crlf(&src), ParseOptions { trust_zoom_props: true }).unwrap().events[0].zoom.clone().unwrap();
    assert_eq!(z.meeting_id, "81234567890");
    assert_eq!(z.start_url, None); // never read, even when trusted
    // storage default is safe, opt-in works
    let db = Calendar::open_in_memory().unwrap();
    db.upsert_ics("personal", &crlf(&src), None, None).unwrap();
    assert!(db.get_event("h1", "personal").unwrap().unwrap().zoom.is_none());
    db.upsert_ics_opts("personal", &crlf(&src), None, None, ParseOptions { trust_zoom_props: true }).unwrap();
    assert!(db.get_event("h1", "personal").unwrap().unwrap().zoom.is_some());
}

// ------------------------------------------------------------------ Q05 raw-preserving save

const SERVER_ICS: &str = "BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Acme//Server//EN
X-WR-CALNAME:Work
BEGIN:VTIMEZONE
TZID:Eastern Standard Time
BEGIN:STANDARD
DTSTART:16010101T020000
TZOFFSETFROM:-0400
TZOFFSETTO:-0500
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11
END:STANDARD
BEGIN:DAYLIGHT
DTSTART:16010101T020000
TZOFFSETFROM:-0500
TZOFFSETTO:-0400
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3
END:DAYLIGHT
END:VTIMEZONE
BEGIN:VEVENT
UID:srv-1
DTSTAMP:20261101T120000Z
DTSTART;TZID=Eastern Standard Time:20261110T090000
DTEND;TZID=Eastern Standard Time:20261110T100000
RRULE:FREQ=WEEKLY;BYDAY=TU
RDATE;TZID=Eastern Standard Time:20261112T090000
SUMMARY:Weekly sync
DESCRIPTION:Old
CLASS:PRIVATE
TRANSP:TRANSPARENT
CATEGORIES:WORK,TEAM
URL:https://example.com/x
ATTACH;FMTTYPE=text/plain:https://example.com/a.txt
X-CUSTOM-THING;X-PARAM=1:keep me
ORGANIZER;CN=Dana;PARTSTAT=ACCEPTED:mailto:dana@example.com
ATTENDEE;CUTYPE=INDIVIDUAL;PARTSTAT=DELEGATED;DELEGATED-TO=\"mailto:z@example.com\";CN=Bob:mailto:bob@example.org
ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;X-SEQ=3:mailto:me@example.com
ATTENDEE;PARTSTAT=ACCEPTED:mailto:gone@example.com
SEQUENCE:2
BEGIN:VALARM
TRIGGER:-PT15M
ACTION:DISPLAY
DESCRIPTION:r
END:VALARM
BEGIN:VALARM
TRIGGER;VALUE=DATE-TIME:20261110T000000Z
ACTION:AUDIO
END:VALARM
BEGIN:VALARM
TRIGGER:PT5M
ACTION:EMAIL
END:VALARM
X-MIRAMAIL-ZOOM-STARTURL:https://zoom.us/s/1?zak=SECRET
END:VEVENT
BEGIN:VEVENT
UID:srv-1
RECURRENCE-ID;TZID=Eastern Standard Time:20261117T090000
DTSTART;TZID=Eastern Standard Time:20261117T110000
DTEND;TZID=Eastern Standard Time:20261117T120000
SUMMARY:Weekly sync (moved)
X-OVR-PROP:keep override prop
SEQUENCE:3
END:VEVENT
END:VCALENDAR
";

const KEEP: &[&str] = &[
    "X-WR-CALNAME:Work",
    "BEGIN:VTIMEZONE\r\nTZID:Eastern Standard Time",
    "RDATE;TZID=Eastern Standard Time:20261112T090000",
    "CLASS:PRIVATE",
    "TRANSP:TRANSPARENT",
    "CATEGORIES:WORK,TEAM",
    "URL:https://example.com/x",
    "ATTACH;FMTTYPE=text/plain:https://example.com/a.txt",
    "X-CUSTOM-THING;X-PARAM=1:keep me",
    "ORGANIZER;CN=Dana;PARTSTAT=ACCEPTED:mailto:dana@example.com",
    "ATTENDEE;CUTYPE=INDIVIDUAL;PARTSTAT=DELEGATED;DELEGATED-TO=\"mailto:z@example.com\";CN=Bob:mailto:bob@example.org",
    "TRIGGER;VALUE=DATE-TIME:20261110T000000Z",
    "TRIGGER:PT5M",
    "X-OVR-PROP:keep override prop",
    "SUMMARY:Weekly sync (moved)",
];

fn server_db() -> Calendar {
    let db = Calendar::open_in_memory().unwrap();
    assert_eq!(db.upsert_ics("personal", &crlf(SERVER_ICS), Some("/srv-1.ics"), Some("e1")).unwrap(), vec!["srv-1"]);
    db
}

#[test]
fn q05_untouched_save_is_verbatim_and_drops_host_token() {
    let db = server_db();
    let put = unfolded(&db.resource_ics_for_put("srv-1", "personal").unwrap());
    for k in KEEP {
        assert!(put.contains(k), "lost {k}\n{put}");
    }
    assert!(!put.contains("SECRET") && !put.contains("STARTURL"));
    assert!(put.contains("SEQUENCE:2\r\n") && !put.contains("SEQUENCE:3\r\nBEGIN:VALARM"));
    assert!(!put.contains("METHOD"));
    let p = parse_ics(put.as_bytes()).unwrap();
    assert_eq!((p.events.len(), p.overrides.len()), (1, 1));
}

#[test]
fn q05_edit_patches_only_edited_properties() {
    let db = server_db();
    let mut ev = db.get_event("srv-1", "personal").unwrap().unwrap();
    assert_eq!(ev.tzid.as_deref(), Some("America/New_York"));
    ev.summary = "Renamed".into();
    ev.location = "HQ".into();
    ev.start = "2026-11-10T10:00:00-05:00".into();
    ev.end = "2026-11-10T11:00:00-05:00".into();
    ev.attendees.retain(|a| a.email != "gone@example.com");
    ev.attendees.push(Attendee { email: "new@example.com".into(), name: Some("New".into()), role: AttendeeRole::Required, status: AttendeeStatus::NeedsAction });
    ev.attendees.iter_mut().find(|a| a.email == "me@example.com").unwrap().status = AttendeeStatus::Accepted;
    ev.reminder_minutes = Some(30);
    ev.exdates.push("2026-11-24T09:00:00-05:00".into());
    ev.zoom = Some(ZoomMeeting { meeting_id: "9".into(), join_url: "https://zoom.us/j/9".into(), password: None, start_url: Some("https://zoom.us/s/9?zak=T".into()) });
    db.upsert_event(&ev).unwrap();
    let put_raw = db.resource_ics_for_put("srv-1", "personal").unwrap();
    let put = unfolded(&put_raw);
    for k in KEEP {
        assert!(put.contains(k), "lost {k}\n{put}");
    }
    for k in [
        "SUMMARY:Renamed",
        "LOCATION:HQ",
        "DTSTART;TZID=America/New_York:20261110T100000",
        "DTEND;TZID=America/New_York:20261110T110000",
        "EXDATE;TZID=America/New_York:20261124T090000",
        "mailto:new@example.com",
        "PARTSTAT=ACCEPTED;RSVP=TRUE;X-SEQ=3:mailto:me@example.com",
        "TRIGGER:-PT30M",
        "SEQUENCE:3",
        "X-MIRAMAIL-ZOOM-ID:9",
        "BEGIN:VTIMEZONE\r\nTZID:America/New_York",
    ] {
        assert!(put.contains(k), "missing {k}\n{put}");
    }
    for k in ["gone@example.com", "-PT15M", "SUMMARY:Weekly sync\r\n", "zak=", "STARTURL"] {
        assert!(!put.contains(k), "should be gone: {k}");
    }
    // result is valid and round-trips through the parser
    let p = parse_ics_opts(put_raw.as_bytes(), ParseOptions { trust_zoom_props: true }).unwrap();
    assert_eq!(p.events[0].summary, "Renamed");
    assert_eq!(p.events[0].start, "2026-11-10T10:00:00-05:00");
    assert_eq!(p.events[0].reminder_minutes, Some(30));
    assert_eq!(p.overrides.len(), 1);
}

#[test]
fn q05_overrides_edit_add_delete() {
    let db = server_db();
    let mut o = db.get_overrides("srv-1", "personal").unwrap().remove(0);
    o.event.summary = "Edited instance".into();
    db.upsert_override(&o).unwrap();
    let put = unfolded(&db.resource_ics_for_put("srv-1", "personal").unwrap());
    assert!(put.contains("SUMMARY:Edited instance") && put.contains("X-OVR-PROP:keep override prop"));
    assert!(!put.contains("Weekly sync (moved)"));
    // a new local override is appended; the deleted one disappears
    let mut n = o.clone();
    n.recurrence_id = "2026-11-24T09:00:00-05:00".into();
    n.event.start = "2026-11-24T13:00:00-05:00".into();
    n.event.end = "2026-11-24T14:00:00-05:00".into();
    n.event.summary = "Extra".into();
    db.upsert_override(&n).unwrap();
    assert!(db.delete_override("srv-1", "personal", &o.recurrence_id).unwrap());
    let put = unfolded(&db.resource_ics_for_put("srv-1", "personal").unwrap());
    let p = parse_ics(put.as_bytes()).unwrap();
    assert_eq!(p.overrides.len(), 1);
    assert_eq!(p.overrides[0].event.summary, "Extra");
    assert!(put.contains("CLASS:PRIVATE"));
}

#[test]
fn q05_no_raw_falls_back_to_strict_rebuild() {
    let db = Calendar::open_in_memory().unwrap();
    let e = CalEvent { uid: "l1".into(), calendar_id: "personal".into(), summary: "Local".into(), start: "2026-11-10T09:00:00+00:00".into(), end: "2026-11-10T10:00:00+00:00".into(), tzid: Some("UTC".into()), ..Default::default() };
    db.upsert_event(&e).unwrap();
    let put = db.resource_ics_for_put("l1", "personal").unwrap();
    assert_eq!(parse_ics(put.as_bytes()).unwrap().events[0].summary, "Local");
    assert!(matches!(db.resource_ics_for_put("nope", "personal"), Err(Error::NotFound(_))));
}

#[test]
fn q05_caps_error_instead_of_dropping() {
    // 3000-char RRULE survives parse + save
    let long_rule = format!("FREQ=YEARLY;BYMONTH=1;BYMONTHDAY={}", (1..=28).map(|d| d.to_string()).collect::<Vec<_>>().join(","));
    assert!(long_rule.len() < 1000);
    let rule = format!("{long_rule};X-PAD={}", "a".repeat(2500));
    let p = parse(&one_event(&format!("DTSTART:20261110T090000Z\nRRULE:{rule}")));
    assert_eq!(p.events[0].rrule.as_deref(), Some(rule.as_str()));
    assert!(build_ics(&p.events[0], None).contains(&rule[..60]));
    // over the cap: error, not silent loss
    let too_long = format!("FREQ=DAILY;X-PAD={}", "a".repeat(4100));
    assert!(parse_ics(&crlf(&one_event(&format!("DTSTART:20261110T090000Z\nRRULE:{too_long}")))).is_err());
    let mut e = p.events[0].clone();
    e.rrule = Some(too_long);
    assert!(try_build_ics(&e, None).is_err());
    // attendees / exdates caps
    let mut e = p.events[0].clone();
    e.attendees = (0..5001).map(|i| Attendee { email: format!("u{i}@e.com"), name: None, role: AttendeeRole::Required, status: AttendeeStatus::NeedsAction }).collect();
    assert!(try_build_ics(&e, None).is_err());
    e.attendees.truncate(5000);
    e.exdates = (0..10_001).map(|i| format!("2030-01-01T00:00:{:02}+00:00", i % 60)).collect();
    assert!(try_build_ics(&e, None).is_err());
    let db = Calendar::open_in_memory().unwrap();
    e.calendar_id = "personal".into();
    assert!(db.upsert_event(&e).is_err());
    // unparsable start is an error, not an invalid ICS
    let mut bad = p.events[0].clone();
    bad.start = "garbage".into();
    assert!(try_build_ics(&bad, None).is_err());
}

#[test]
fn q05_delegated_and_organizer_status_roundtrip() {
    let p = parse(SERVER_ICS);
    let bob = p.events[0].attendees.iter().find(|a| a.email == "bob@example.org").unwrap();
    assert_eq!(bob.status, AttendeeStatus::NeedsAction); // DELEGATED has no wire enum; preserved verbatim by the raw patcher
    let db = server_db();
    let mut ev = db.get_event("srv-1", "personal").unwrap().unwrap();
    ev.summary = "x".into();
    db.upsert_event(&ev).unwrap();
    let put = unfolded(&db.resource_ics_for_put("srv-1", "personal").unwrap());
    assert!(put.contains("PARTSTAT=DELEGATED") && put.contains("ORGANIZER;CN=Dana;PARTSTAT=ACCEPTED:"));
}

// ------------------------------------------------------------------ Q06 VTIMEZONE

fn custom_tz_event(tz_def: &str, local: &str) -> CalEvent {
    let src = format!(
        "BEGIN:VCALENDAR\nVERSION:2.0\n{tz_def}BEGIN:VEVENT\nUID:tz1\nDTSTART;TZID=Customized Time Zone:{local}\nDTEND;TZID=Customized Time Zone:{local}\nSUMMARY:x\nEND:VEVENT\nEND:VCALENDAR\n"
    );
    parse(&src).events.remove(0)
}

const EU_LIKE: &str = "BEGIN:VTIMEZONE
TZID:Customized Time Zone
BEGIN:STANDARD
DTSTART:16011028T030000
TZOFFSETFROM:+0200
TZOFFSETTO:+0100
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10
END:STANDARD
BEGIN:DAYLIGHT
DTSTART:16010325T020000
TZOFFSETFROM:+0100
TZOFFSETTO:+0200
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3
END:DAYLIGHT
END:VTIMEZONE
";

const HALF_HOUR_DST: &str = "BEGIN:VTIMEZONE
TZID:Customized Time Zone
BEGIN:STANDARD
DTSTART:19701025T030000
TZOFFSETFROM:+0330
TZOFFSETTO:+0300
RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10
END:STANDARD
BEGIN:DAYLIGHT
DTSTART:19700329T020000
TZOFFSETFROM:+0300
TZOFFSETTO:+0330
RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3
END:DAYLIGHT
END:VTIMEZONE
";

#[test]
fn q06_custom_vtimezone_matching_a_real_zone_uses_its_rules() {
    let summer = custom_tz_event(EU_LIKE, "20260715T090000");
    let winter = custom_tz_event(EU_LIKE, "20260115T090000");
    assert_eq!(summer.start, "2026-07-15T09:00:00+02:00");
    assert_eq!(winter.start, "2026-01-15T09:00:00+01:00");
    assert!(summer.tzid.is_some() && summer.tzid.as_deref() != Some("UTC"));
    // across the DST change (Sun 2026-10-25 03:00 -> 02:00)
    assert_eq!(custom_tz_event(EU_LIKE, "20261024T120000").start, "2026-10-24T12:00:00+02:00");
    assert_eq!(custom_tz_event(EU_LIKE, "20261026T120000").start, "2026-10-26T12:00:00+01:00");
    // recurrences keep following the zone
    let mut w = summer.clone();
    w.rrule = Some("FREQ=WEEKLY;COUNT=20".into());
    let occ = expand_event(&w, &[], dt("2026-07-01T00:00:00+00:00"), dt("2027-01-01T00:00:00+00:00"));
    assert!(occ.iter().any(|o| o.start.ends_with("+02:00")) && occ.iter().any(|o| o.start.ends_with("+01:00")));
    assert!(occ.iter().all(|o| o.start[11..19] == *"09:00:00"));
}

#[test]
fn q06_unmatchable_vtimezone_is_evaluated_per_date() {
    let jul = custom_tz_event(HALF_HOUR_DST, "20260715T090000"); // +03:30
    let jan = custom_tz_event(HALF_HOUR_DST, "20260115T090000"); // +03:00
    assert_eq!(jul.start, "2026-07-15T05:30:00+00:00");
    assert_eq!(jan.start, "2026-01-15T06:00:00+00:00");
    // never "standard offset all year": both sides of the 2026-03-29 transition
    assert_eq!(custom_tz_event(HALF_HOUR_DST, "20260328T090000").start, "2026-03-28T06:00:00+00:00");
    assert_eq!(custom_tz_event(HALF_HOUR_DST, "20260330T090000").start, "2026-03-30T05:30:00+00:00");
    // and the October change back
    assert_eq!(custom_tz_event(HALF_HOUR_DST, "20261024T090000").start, "2026-10-24T05:30:00+00:00");
    assert_eq!(custom_tz_event(HALF_HOUR_DST, "20261026T090000").start, "2026-10-26T06:00:00+00:00");
}

#[test]
fn q06_windows_names() {
    for (win, iana) in [
        ("E. Europe Standard Time", "Europe/Chisinau"),
        ("Cen. Australia Standard Time", "Australia/Adelaide"),
        ("Central Standard Time (Mexico)", "America/Mexico_City"),
        ("Tokyo Standard Time", "Asia/Tokyo"),
        ("Pacific SA Standard Time", "America/Santiago"),
        ("W. Europe Standard Time", "Europe/Berlin"),
    ] {
        let src = format!("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:w\nDTSTART;TZID={win}:20261110T090000\nEND:VEVENT\nEND:VCALENDAR\n");
        assert_eq!(parse(&src).events[0].tzid.as_deref(), Some(iana), "{win}");
    }
    // "Customized Time Zone" without a VTIMEZONE is not guessable -> UTC
    let src = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:w\nDTSTART;TZID=Customized Time Zone:20261110T090000\nEND:VEVENT\nEND:VCALENDAR\n";
    assert_eq!(parse(src).events[0].tzid.as_deref(), Some("UTC"));
}

// ------------------------------------------------------------------ Q07 / Q08 / Q16 iTIP

fn series_master() -> CalEvent {
    CalEvent {
        uid: "ser@x".into(),
        calendar_id: "personal".into(),
        summary: "Series".into(),
        start: "2026-11-02T10:00:00+00:00".into(),
        end: "2026-11-02T11:00:00+00:00".into(),
        tzid: Some("UTC".into()),
        rrule: Some("FREQ=WEEKLY".into()),
        organizer: Some(Attendee { email: "alice@example.com".into(), name: None, role: AttendeeRole::Chair, status: AttendeeStatus::Accepted }),
        attendees: vec![
            Attendee { email: "alice@example.com".into(), name: None, role: AttendeeRole::Chair, status: AttendeeStatus::Accepted },
            Attendee { email: "bob@example.org".into(), name: None, role: AttendeeRole::Required, status: AttendeeStatus::NeedsAction },
        ],
        dtstamp: Some("2026-11-01T12:00:00+00:00".into()),
        ..Default::default()
    }
}

#[test]
fn q07_reply_request_cancel_authorisation() {
    let stored = series_master();
    let reply_ics = itip::build_reply(&stored, "bob@example.org", AttendeeStatus::Accepted).unwrap();
    let reply = parse_ics(reply_ics.as_bytes()).unwrap().events.remove(0);
    let out = itip::apply_reply(&stored, &reply, "BOB@example.org").unwrap();
    assert_eq!(out.attendees[1].status, AttendeeStatus::Accepted);
    assert_eq!(out.attendees[0], stored.attendees[0]);
    assert_eq!(out.summary, stored.summary);
    // spoofed sender, non-attendee, other uid, stale
    assert!(itip::apply_reply(&stored, &reply, "mallory@evil.example").is_err());
    let mut dave = reply.clone();
    dave.attendees[0].email = "dave@example.org".into();
    assert!(itip::apply_reply(&stored, &dave, "dave@example.org").is_err());
    let mut other = reply.clone();
    other.uid = "other".into();
    assert!(itip::apply_reply(&stored, &other, "bob@example.org").is_err());
    // the reply cannot smuggle content changes
    let mut sneaky = reply.clone();
    sneaky.summary = "pwned".into();
    sneaky.start = "2030-01-01T00:00:00+00:00".into();
    assert_eq!(itip::apply_reply(&stored, &sneaky, "bob@example.org").unwrap().summary, "Series");
    assert!(!InviteMethod::Reply.is_importable() && InviteMethod::Request.is_importable());

    // cancel
    let c = itip::apply_cancel(&stored, &stored, "alice@example.com").unwrap();
    assert_eq!(c.status, EventStatus::Cancelled);
    assert!(itip::apply_cancel(&stored, &stored, "bob@example.org").is_err());

    // request
    let mut upd = stored.clone();
    upd.sequence = 1;
    upd.summary = "Series v2".into();
    let r = itip::apply_request(Some(&stored), &upd, "alice@example.com").unwrap();
    assert_eq!((r.summary.as_str(), r.calendar_id.as_str()), ("Series v2", "personal"));
    assert!(itip::apply_request(Some(&stored), &upd, "bob@example.org").is_err());
    let mut old = stored.clone();
    old.sequence = 5;
    assert!(itip::apply_request(Some(&old), &upd, "alice@example.com").is_err()); // stale SEQUENCE
    let mut same_seq_old_stamp = stored.clone();
    same_seq_old_stamp.dtstamp = Some("2026-10-01T00:00:00+00:00".into());
    assert!(itip::apply_request(Some(&stored), &same_seq_old_stamp, "alice@example.com").is_err());
    let mut same_seq_new_stamp = stored.clone();
    same_seq_new_stamp.dtstamp = Some("2026-11-02T00:00:00+00:00".into());
    assert!(itip::apply_request(Some(&stored), &same_seq_new_stamp, "alice@example.com").is_ok());
    let mut hijack = upd.clone();
    hijack.organizer.as_mut().unwrap().email = "mallory@evil.example".into();
    assert!(itip::apply_request(Some(&stored), &hijack, "mallory@evil.example").is_err());
    // invites never bring zoom data
    let mut z = upd.clone();
    z.zoom = Some(ZoomMeeting { meeting_id: "1".into(), join_url: "u".into(), password: None, start_url: None });
    assert!(itip::apply_request(None, &z, "alice@example.com").unwrap().zoom.is_none());
}

#[test]
fn q08_single_instance_routes_to_override_or_exdate() {
    let master = series_master();
    let mut inst = master.clone();
    inst.rrule = None;
    inst.start = "2026-11-09T14:00:00+00:00".into();
    inst.end = "2026-11-09T15:00:00+00:00".into();
    inst.summary = "Moved".into();
    inst.sequence = 1;
    let ics = build_ics_series(None, &[EventOverride { recurrence_id: "2026-11-09T10:00:00+00:00".into(), event: inst }], Some("REQUEST"));
    let inv = itip::invite_from_ics(ics.as_bytes(), &["bob@example.org".into()], "a", false).unwrap().unwrap();
    assert_eq!(inv.recurrence_id.as_deref(), Some("2026-11-09T10:00:00+00:00"));
    assert_eq!(inv.event.summary, "Moved");

    let up = itip::apply_request_series(Some(&master), &[], &inv, "alice@example.com").unwrap();
    assert!(up.master.is_none(), "the master must not be replaced");
    assert_eq!(up.overrides.len(), 1);
    assert_eq!(up.overrides[0].recurrence_id, "2026-11-09T10:00:00+00:00");
    assert_eq!(up.overrides[0].event.uid, master.uid);
    assert!(itip::apply_request_series(Some(&master), &[], &inv, "mallory@evil.example").is_err());
    // no master stored: override-only
    let up2 = itip::apply_request_series(None, &[], &inv, "alice@example.com").unwrap();
    assert!(up2.master.is_none() && up2.overrides.len() == 1);

    // single-instance CANCEL -> EXDATE + override removal, master stays confirmed
    let mut cinst = inv.clone();
    cinst.method = InviteMethod::Cancel;
    let stored_ovs = up.overrides.clone();
    let cu = itip::apply_cancel_series(Some(&master), &stored_ovs, &cinst, "alice@example.com").unwrap();
    let m = cu.master.unwrap();
    assert_eq!(m.status, EventStatus::Confirmed);
    assert_eq!(m.exdates, vec!["2026-11-09T10:00:00+00:00"]);
    assert_eq!(cu.removed_overrides, vec!["2026-11-09T10:00:00+00:00"]);
    assert!(itip::apply_cancel_series(Some(&master), &stored_ovs, &cinst, "bob@example.org").is_err());

    // master + overrides in one part
    let all = build_ics_series(Some(&master), &stored_ovs, Some("REQUEST"));
    let inv2 = itip::invite_from_ics(all.as_bytes(), &[], "a", false).unwrap().unwrap();
    assert!(inv2.recurrence_id.is_none() && inv2.overrides.len() == 1);
    let up3 = itip::apply_request_series(None, &[], &inv2, "alice@example.com").unwrap();
    assert!(up3.master.is_some() && up3.overrides.len() == 1);
    // wire format has the new fields with defaults
    let v = serde_json::to_value(&inv2).unwrap();
    assert!(v.get("recurrenceId").is_some() && v.get("overrides").is_some());
    let old: Invite = serde_json::from_value(serde_json::json!({
        "method": "REQUEST", "event": serde_json::to_value(&master).unwrap(), "myStatus": null, "alreadyInCalendar": false, "attachmentId": "x"
    }))
    .unwrap();
    assert!(old.recurrence_id.is_none() && old.overrides.is_empty());
}

#[test]
fn q16_only_the_organizer_sends() {
    let e = series_master();
    let me = vec!["alice@example.com".to_string()];
    let notme = vec!["bob@example.org".to_string()];
    assert!(itip::try_build_request(&e, &notme, false).is_err());
    assert!(itip::try_build_cancel(&e, &notme).is_err());
    let r = itip::try_build_request(&e, &me, true).unwrap();
    assert!(r.contains("METHOD:REQUEST") && r.contains("SCHEDULE-AGENT=CLIENT"));
    assert!(!itip::try_build_request(&e, &me, false).unwrap().contains("SCHEDULE-AGENT"));
    assert!(itip::try_build_cancel(&e, &me).unwrap().contains("METHOD:CANCEL"));
}

// ------------------------------------------------------------------ Q09 floating

#[test]
fn q09_floating_stays_floating() {
    let p = parse(&one_event("DTSTART:20261110T090000\nDTEND:20261110T100000\nEXDATE:20261117T090000\nRRULE:FREQ=WEEKLY"));
    let e = &p.events[0];
    assert_eq!(e.tzid, None);
    assert_eq!(e.start, "2026-11-10T09:00:00+00:00"); // wall clock with a placeholder offset
    let ics = build_ics(e, None);
    assert!(ics.contains("DTSTART:20261110T090000\r\n"), "{ics}");
    assert!(ics.contains("DTEND:20261110T100000\r\n") && ics.contains("EXDATE:20261117T090000\r\n"));
    let mut b = parse_ics(ics.as_bytes()).unwrap().events.remove(0);
    b.dtstamp = None;
    let mut a = e.clone();
    a.dtstamp = None;
    assert_eq!(a, b);
    // a Z time is a real UTC instant, not floating
    let z = parse(&one_event("DTSTART:20261110T090000Z"));
    assert_eq!(z.events[0].tzid.as_deref(), Some("UTC"));
    assert!(build_ics(&z.events[0], None).contains("DTSTART:20261110T090000Z"));
}

// ------------------------------------------------------------------ Q10 text hygiene

#[test]
fn q10_itip_text_is_sanitised() {
    let mut e = series_master();
    e.summary = format!("Hi\r\nBcc: evil@x.example\u{202e}\tTab{}", "A".repeat(10_000));
    e.location = format!("Room\nX-Header: 1{}", "L".repeat(5000));
    e.organizer.as_mut().unwrap().name = Some("Eve\r\nSubject: spoof".into());
    e.tzid = Some("UTC\r\nX: y".into());
    e.zoom = Some(ZoomMeeting { meeting_id: "1".into(), join_url: format!("https://zoom.us/j/1\r\nX: {}", "u".repeat(5000)), password: Some("pw\r\nX".into()), start_url: None });
    let who = "Mal\r\nTo: x@y".to_string();
    let texts = [
        itip::request_subject(&e),
        itip::cancel_subject(&e),
        itip::reply_subject(&e, AttendeeStatus::Accepted),
        itip::request_body(&e),
        itip::cancel_body(&e),
        itip::reply_body(&e, &who, AttendeeStatus::Declined),
    ];
    for t in &texts {
        assert!(!t.contains('\r') && !t.contains('\u{202e}') && !t.contains('\t'), "{t:?}");
        assert!(t.len() < 3000, "{}", t.len());
    }
    for t in &texts[..3] {
        assert!(!t.contains('\n') && t.len() < 700);
    }
    // body lines are fixed labels, injected text cannot start a new header-like line
    for line in texts[3].lines() {
        assert!(line.is_empty() || line.starts_with("What: ") || line.starts_with("When: ") || line.starts_with("Where: ") || line.starts_with("Zoom: ") || line.starts_with("Passcode: ") || line.starts_with("Organizer: ") || line.starts_with("Repeats: ") || line.starts_with("This invitation") || line.starts_with("You have"), "{line:?}");
    }
}

// ------------------------------------------------------------------ Q11 settings

#[test]
fn q11_secret_like_setting_keys_are_rejected() {
    let db = Calendar::open_in_memory().unwrap();
    for k in ["Zoom-Client_Secret", "zoomZAK", "oauth.Bearer", "AUTH", "api.key", "pwd", "x.Pass_Word", "caldav.PASSWD", "refresh-token", "client_credentials", "ApiKey", "p a s s"] {
        assert!(db.set_setting(k, "v").is_err(), "{k}");
    }
    for k in ["zoom.client_id", "ui.theme", "caldav.sync.dav1", "week.start"] {
        db.set_setting(k, "v").unwrap();
    }
}

// ------------------------------------------------------------------ Q12 / Q13 / Q14 / Q15 storage + expansion

#[test]
fn q12_large_rows_never_vanish() {
    let db = Calendar::open_in_memory().unwrap();
    let mut e = CalEvent { uid: "big".into(), calendar_id: "personal".into(), summary: "Big".into(), start: "2026-11-10T09:00:00+00:00".into(), end: "2026-11-10T10:00:00+00:00".into(), tzid: Some("UTC".into()), ..Default::default() };
    e.description = "d".repeat(1_000_000);
    db.upsert_event(&e).unwrap();
    assert_eq!(db.get_event("big", "personal").unwrap().unwrap().description.len(), 1_000_000);
    assert_eq!(db.list_events("personal").unwrap().len(), 1);
    e.uid = "huge".into();
    e.description = "d".repeat(1_200_000);
    assert!(matches!(db.upsert_event(&e), Err(Error::TooLarge { .. })));
}

#[test]
fn q13_override_only_and_stale_override_handling() {
    let db = Calendar::open_in_memory().unwrap();
    let inst = "BEGIN:VCALENDAR\nMETHOD:REQUEST\nBEGIN:VEVENT\nUID:solo\nRECURRENCE-ID:20261109T100000Z\nDTSTART:20261109T140000Z\nDTEND:20261109T150000Z\nSUMMARY:Just this one\nEND:VEVENT\nEND:VCALENDAR\n";
    assert_eq!(db.upsert_ics("personal", &crlf(inst), None, None).unwrap(), vec!["solo"]);
    assert!(db.get_event("solo", "personal").unwrap().is_none());
    let ovs = db.get_overrides("solo", "personal").unwrap();
    assert_eq!(ovs.len(), 1);
    assert_eq!(ovs[0].recurrence_id, "2026-11-09T10:00:00+00:00");
    let occ = db.list_occurrences("2026-11-01T00:00:00+00:00", "2026-12-01T00:00:00+00:00", true).unwrap();
    assert_eq!(occ.len(), 1);
    assert_eq!(occ[0].recurrence_start, "2026-11-09T10:00:00+00:00");
    assert_eq!(occ[0].start, "2026-11-09T14:00:00+00:00");
    assert!(db.resource_ics_for_put("solo", "personal").unwrap().contains("RECURRENCE-ID"));

    // re-syncing a resource with fewer overrides removes the stale ones
    let two = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:m\nDTSTART:20261102T100000Z\nRRULE:FREQ=WEEKLY\nSUMMARY:M\nEND:VEVENT\nBEGIN:VEVENT\nUID:m\nRECURRENCE-ID:20261109T100000Z\nDTSTART:20261109T120000Z\nSUMMARY:O1\nEND:VEVENT\nBEGIN:VEVENT\nUID:m\nRECURRENCE-ID:20261116T100000Z\nDTSTART:20261116T120000Z\nSUMMARY:O2\nEND:VEVENT\nEND:VCALENDAR\n";
    db.upsert_ics("personal", &crlf(two), Some("/m.ics"), None).unwrap();
    assert_eq!(db.get_overrides("m", "personal").unwrap().len(), 2);
    let one = two.replace("BEGIN:VEVENT\nUID:m\nRECURRENCE-ID:20261116T100000Z\nDTSTART:20261116T120000Z\nSUMMARY:O2\nEND:VEVENT\n", "");
    db.upsert_ics("personal", &crlf(&one), Some("/m.ics"), None).unwrap();
    let ovs = db.get_overrides("m", "personal").unwrap();
    assert_eq!(ovs.len(), 1);
    assert_eq!(ovs[0].event.summary, "O1");
}

#[test]
fn q14_occurrence_text_is_truncated_and_until_skips() {
    let mut e = CalEvent { uid: "t".into(), summary: "S".repeat(1000), location: "L".repeat(1000), start: "2026-11-10T09:00:00+00:00".into(), end: "2026-11-10T10:00:00+00:00".into(), tzid: Some("UTC".into()), ..Default::default() };
    let o = &expand_event(&e, &[], dt("2026-11-01T00:00:00+00:00"), dt("2026-12-01T00:00:00+00:00"))[0];
    assert_eq!((o.summary.chars().count(), o.location.chars().count()), (256, 256));
    // a series that ended long before the window yields nothing (and is cheap)
    e.rrule = Some("FREQ=DAILY;UNTIL=20200101".into());
    e.start = "2019-01-01T09:00:00+00:00".into();
    e.end = "2019-01-01T10:00:00+00:00".into();
    assert!(expand_event(&e, &[], dt("2026-11-01T00:00:00+00:00"), dt("2026-12-01T00:00:00+00:00")).is_empty());
}

#[test]
fn q17_synthetic_uid_is_stable() {
    let src = one_event("DTSTART:20261110T090000Z\nSUMMARY:Same\nRRULE:FREQ=DAILY").replace("UID:h1\n", "");
    let a = parse(&src).events[0].uid.clone();
    let b = parse(&format!("{src}X-NOISE:1\n")).events[0].uid.clone();
    assert_eq!(a, b);
    assert_eq!(a, parse(&src.replace("VERSION:2.0", "VERSION:2.0\nPRODID:other")).events[0].uid);
    assert!(a.starts_with("noid-") && a.ends_with("@miramail"));
    assert_ne!(a, parse(&src.replace("Same", "Different")).events[0].uid);
    assert_eq!(a, "noid-1219f847218ce63e@miramail"); // pinned: FNV-1a, never changes across toolchains
}

#[test]
fn q18_dtstart_is_always_the_first_instance() {
    // 2026-11-02 is a Monday; the rule only matches Tuesdays
    let e = CalEvent { uid: "d".into(), summary: "x".into(), start: "2026-11-02T10:00:00+00:00".into(), end: "2026-11-02T11:00:00+00:00".into(), tzid: Some("UTC".into()), rrule: Some("FREQ=WEEKLY;BYDAY=TU;COUNT=3".into()), ..Default::default() };
    let s: Vec<_> = expand_event(&e, &[], dt("2026-11-01T00:00:00+00:00"), dt("2026-12-31T00:00:00+00:00")).into_iter().map(|o| o.start[..10].to_string()).collect();
    assert_eq!(s, vec!["2026-11-02", "2026-11-03", "2026-11-10"]); // DTSTART counts toward COUNT=3
}

#[test]
fn q19_all_day_exdate_uses_its_own_local_date() {
    let src = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:ad\nDTSTART;VALUE=DATE:20261101\nRRULE:FREQ=DAILY;COUNT=10\nEXDATE;TZID=America/New_York:20261104T230000\nEND:VEVENT\nEND:VCALENDAR\n";
    let e = &parse(src).events[0];
    assert_eq!(e.exdates, vec!["2026-11-04"]); // not 2026-11-05 (the UTC date)
    let s: Vec<_> = expand_event(e, &[], dt("2026-11-01T00:00:00+00:00"), dt("2026-11-12T00:00:00+00:00")).into_iter().map(|o| o.start).collect();
    assert!(!s.contains(&"2026-11-04".to_string()) && s.contains(&"2026-11-05".to_string()));
}

// ------------------------------------------------------------------ Q20 read-only + migrations

#[test]
fn q20_read_only_calendars_refuse_user_writes_but_sync_works() {
    let db = Calendar::open_in_memory().unwrap();
    db.create_calendar(&NewCalendar { id: Some("ro".into()), name: "RO".into(), color: "#112233".into(), kind: CalendarKind::Caldav, url: None, username: None, read_only: true }).unwrap();
    let e = CalEvent { uid: "r".into(), calendar_id: "ro".into(), summary: "x".into(), start: "2026-11-10T09:00:00+00:00".into(), end: "2026-11-10T10:00:00+00:00".into(), tzid: Some("UTC".into()), ..Default::default() };
    assert!(db.upsert_event(&e).is_err());
    assert!(db.upsert_override(&EventOverride { recurrence_id: e.start.clone(), event: e.clone() }).is_err());
    db.sync_upsert_event(&e).unwrap();
    db.upsert_ics("ro", &crlf(SERVER_ICS), Some("/a.ics"), None).unwrap(); // sync path
    assert!(db.delete_event("r", "ro").is_err());
    assert!(db.delete_override("srv-1", "ro", "x").is_err());
    assert!(db.sync_delete_event("r", "ro").unwrap());
}

#[test]
fn q20_schema_migration_from_v1() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE calendars(id TEXT PRIMARY KEY, name TEXT NOT NULL, color TEXT NOT NULL, kind TEXT NOT NULL,
               url TEXT, username TEXT, visible INTEGER NOT NULL DEFAULT 1, read_only INTEGER NOT NULL DEFAULT 0,
               last_sync INTEGER, error TEXT, ctag TEXT, sync_marker TEXT, position INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE events(uid TEXT NOT NULL, calendar_id TEXT NOT NULL REFERENCES calendars(id) ON DELETE CASCADE,
               ics TEXT NOT NULL, summary TEXT NOT NULL, start_text TEXT NOT NULL, end_text TEXT NOT NULL,
               start_utc INTEGER NOT NULL, end_utc INTEGER NOT NULL, rrule TEXT, href TEXT, etag TEXT, PRIMARY KEY(uid, calendar_id));
             CREATE TABLE overrides(uid TEXT NOT NULL, calendar_id TEXT NOT NULL, recurrence_id TEXT NOT NULL, ics TEXT NOT NULL,
               PRIMARY KEY(uid, calendar_id, recurrence_id));
             CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO calendars(id,name,color,kind) VALUES('personal','Personal','#4f7cff','local');",
        )
        .unwrap();
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:old\r\nDTSTART:20261110T090000Z\r\nDTEND:20261110T100000Z\r\nSUMMARY:Old\r\nX-MIRAMAIL-ZOOM-ID:1\r\nX-MIRAMAIL-ZOOM-URL:https://zoom.us/j/1\r\nX-MIRAMAIL-ZOOM-STARTURL:https://zoom.us/s/1?zak=SECRET\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        c.execute("INSERT INTO events VALUES('old','personal',?1,'Old','a','b',1,2,NULL,NULL,NULL)", [ics]).unwrap();
    }
    let db = Calendar::open(&path).unwrap();
    let got = db.get_event("old", "personal").unwrap().unwrap();
    assert_eq!(got.zoom.unwrap().start_url, None);
    drop(db);
    let c = rusqlite::Connection::open(&path).unwrap();
    let v: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(v, 2);
    let ics: String = c.query_row("SELECT ics FROM events", [], |r| r.get(0)).unwrap();
    assert!(!ics.contains("SECRET") && !ics.contains("STARTURL") && ics.contains("ZOOM-ID"));
    // a database from the future is refused, not silently mangled
    c.pragma_update(None, "user_version", 99).unwrap();
    drop(c);
    assert!(Calendar::open(&path).is_err());
}

#[test]
fn fuzz_patcher_never_panics() {
    let mut x: u64 = 0x1234_5678_9ABC_DEF1;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let base = crlf(SERVER_ICS);
    for _ in 0..300 {
        let mut v = base.clone();
        for _ in 0..(next() % 6 + 1) {
            let i = (next() as usize) % v.len();
            match next() % 3 {
                0 => v[i] = next() as u8,
                1 => {
                    v.remove(i);
                }
                _ => v.insert(i, b":;,\\\"\r\n =TZ"[(next() % 11) as usize]),
            }
        }
        let db = Calendar::open_in_memory().unwrap();
        let Ok(uids) = db.upsert_ics("personal", &v, Some("/f.ics"), None) else { continue };
        for u in uids {
            if let Some(mut e) = db.get_event(&u, "personal").unwrap() {
                e.summary = "edited".into();
                e.attendees.pop();
                e.reminder_minutes = Some(5);
                let _ = db.upsert_event(&e);
            }
            if let Ok(put) = db.resource_ics_for_put(&u, "personal") {
                let _ = parse_ics(put.as_bytes());
            }
        }
    }
}

// ------------------------------------------------------------------ review round 2 (Q01 / Q04 / Q14 of the glue review)

#[test]
fn r2_q01_schedule_agent_client_on_existing_attendees_and_organizer() {
    // The server resource already has ORGANIZER + ATTENDEEs; an untouched save must still tell an RFC 6638
    // server not to send its own invitations.
    let db = server_db();
    let put = unfolded(&db.resource_ics_for_put_opts("srv-1", "personal", true).unwrap());
    let people: Vec<&str> = put.lines().filter(|l| l.starts_with("ATTENDEE") || l.starts_with("ORGANIZER")).collect();
    assert!(people.len() >= 2, "{put}");
    for l in &people {
        assert!(l.contains("SCHEDULE-AGENT=CLIENT"), "missing on {l}");
    }
    // not requested -> not added
    let plain = unfolded(&db.resource_ics_for_put("srv-1", "personal").unwrap());
    assert!(!plain.contains("SCHEDULE-AGENT"), "{plain}");
    // strict rebuild path (no raw bytes) tags ORGANIZER as well
    let l = Calendar::open_in_memory().unwrap();
    let mut e = series_master();
    e.rrule = None;
    l.upsert_event(&e).unwrap();
    let built = unfolded(&l.resource_ics_for_put_opts("ser@x", "personal", true).unwrap());
    assert!(built.contains("ORGANIZER;SCHEDULE-AGENT=CLIENT:mailto:alice@example.com") || built.contains("SCHEDULE-AGENT=CLIENT:mailto:alice@example.com"), "{built}");
    // idempotent: patching the patched resource again does not duplicate the parameter
    let l2 = Calendar::open_in_memory().unwrap();
    l2.upsert_ics("personal", unfolded(&db.resource_ics_for_put_opts("srv-1", "personal", true).unwrap()).as_bytes(), Some("/srv-1.ics"), Some("e2")).unwrap();
    let again = unfolded(&l2.resource_ics_for_put_opts("srv-1", "personal", true).unwrap());
    assert!(!again.contains("SCHEDULE-AGENT=CLIENT;SCHEDULE-AGENT=CLIENT"), "{again}");
}

#[test]
fn r2_q04_copy_raw_ics_between_calendars() {
    let db = server_db();
    db.create_calendar(&NewCalendar { id: Some("other".into()), name: "O".into(), color: "#112233".into(), kind: CalendarKind::Caldav, url: Some("https://x.example/o/".into()), username: Some("u".into()), read_only: false })
        .unwrap();
    let mut ev = db.get_event("srv-1", "personal").unwrap().unwrap();
    ev.calendar_id = "other".into();
    ev.href = None;
    ev.etag = None;
    db.upsert_event(&ev).unwrap();
    assert!(db.copy_raw_ics("srv-1", "personal", "other").unwrap());
    for mut o in db.get_overrides("srv-1", "personal").unwrap() {
        o.event.calendar_id = "other".into();
        db.upsert_override(&o).unwrap();
    }
    let put = unfolded(&db.resource_ics_for_put("srv-1", "other").unwrap());
    for k in KEEP {
        assert!(put.contains(k), "lost {k} in the copy\n{put}");
    }
    assert!(!db.copy_raw_ics("srv-1", "personal", "nope").unwrap_or(false));
}

#[test]
fn r2_q14_single_instance_cancel_checks_sequence() {
    let mut master = series_master();
    master.sequence = 3;
    let mut inst = master.clone();
    inst.rrule = None;
    inst.start = "2026-11-09T10:00:00+00:00".into();
    inst.end = "2026-11-09T11:00:00+00:00".into();
    inst.sequence = 1; // older than the master's SEQUENCE
    let ics = build_ics_series(None, &[EventOverride { recurrence_id: "2026-11-09T10:00:00+00:00".into(), event: inst.clone() }], Some("CANCEL"));
    let inv = itip::invite_from_ics(ics.as_bytes(), &[], "a", false).unwrap().unwrap();
    assert!(itip::apply_cancel_series(Some(&master), &[], &inv, "alice@example.com").is_err(), "replayed old instance cancel");
    inst.sequence = 3;
    let ics = build_ics_series(None, &[EventOverride { recurrence_id: "2026-11-09T10:00:00+00:00".into(), event: inst }], Some("CANCEL"));
    let inv = itip::invite_from_ics(ics.as_bytes(), &[], "a", false).unwrap().unwrap();
    assert!(itip::apply_cancel_series(Some(&master), &[], &inv, "alice@example.com").is_ok());
    // rid_eq: same instant in a different offset, and date vs date-time
    assert!(itip::rid_eq("2026-11-09T10:00:00+00:00", "2026-11-09T15:30:00+05:30"));
    assert!(!itip::rid_eq("2026-11-09T10:00:00+00:00", "2026-11-10T10:00:00+00:00"));
}
