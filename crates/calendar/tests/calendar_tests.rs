mod fixtures;
use calendar::itip;
use calendar::*;
use chrono::{DateTime, FixedOffset};
use fixtures::*;

fn dt(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn timed(start: &str, end: &str, tzid: &str, rrule: Option<&str>) -> CalEvent {
    CalEvent {
        uid: "t@x".into(),
        summary: "T".into(),
        start: start.into(),
        end: end.into(),
        tzid: Some(tzid.into()),
        rrule: rrule.map(String::from),
        ..Default::default()
    }
}

fn starts(ev: &CalEvent, from: &str, to: &str) -> Vec<String> {
    expand_event(ev, &[], dt(from), dt(to)).into_iter().map(|o| o.start).collect()
}

fn parse(s: &str) -> ParsedCalendar {
    parse_ics(&crlf(s)).unwrap()
}

// ---------------------------------------------------------------- parsing real-world style

#[test]
fn google_invite() {
    let p = parse(GOOGLE_INVITE);
    assert_eq!(p.method.as_deref(), Some("REQUEST"));
    assert_eq!(p.events.len(), 1);
    let e = &p.events[0];
    assert_eq!(e.uid, "abc123@google.com");
    assert_eq!(e.start, "2026-11-06T09:30:00+00:00");
    assert_eq!(e.end, "2026-11-06T10:30:00+00:00");
    assert_eq!(e.tzid.as_deref(), Some("UTC"));
    assert_eq!(e.location, "Room 4, HQ");
    assert_eq!(e.description, "Quarterly planning\nAgenda: budget, hiring\n\nJoin: https://meet.google.com/abc-defg-hij-klm");
    assert_eq!(e.reminder_minutes, Some(30));
    assert_eq!(e.attendees.len(), 3);
    assert_eq!(e.attendees[2].name.as_deref(), Some("Carol, Jr."));
    assert_eq!(e.attendees[2].role, AttendeeRole::Optional);
    assert_eq!(e.attendees[2].status, AttendeeStatus::Tentative);
    assert_eq!(e.organizer.as_ref().unwrap().email, "alice@example.com");
    assert_eq!(itip::my_status(e, &["BOB@example.org".into()]), Some(AttendeeStatus::NeedsAction));
    assert_eq!(itip::my_status(e, &["nobody@example.org".into()]), None);
}

#[test]
fn outlook_windows_tzid_maps_to_iana() {
    let e = &parse(OUTLOOK_INVITE).events[0];
    assert_eq!(e.tzid.as_deref(), Some("America/New_York"));
    assert_eq!(e.start, "2026-11-10T09:00:00-05:00");
    assert_eq!(e.end, "2026-11-10T10:00:00-05:00");
    assert_eq!(e.organizer.as_ref().unwrap().email, "dana@contoso.example");
    assert_eq!(e.sequence, 1);
    assert_eq!(e.reminder_minutes, Some(15));
    assert_eq!(e.description, "Please review the deck.\n");
}

#[test]
fn unknown_vtimezone_falls_back_to_utc_via_offset() {
    let e = &parse(CUSTOM_TZ).events[0];
    assert_eq!(e.start, "2026-11-10T07:00:00+00:00");
    assert_eq!(e.end, "2026-11-10T08:30:00+00:00"); // DURATION
    assert_eq!(e.tzid.as_deref(), Some("UTC"));
}

#[test]
fn apple_recurring_with_exdate_and_override() {
    let p = parse(APPLE_RECURRING);
    assert_eq!(p.events.len(), 1);
    assert_eq!(p.overrides.len(), 1);
    let m = &p.events[0];
    assert_eq!(m.tzid.as_deref(), Some("Asia/Kolkata"));
    assert_eq!(m.start, "2026-11-02T10:00:00+05:30");
    assert_eq!(m.exdates, vec!["2026-11-04T10:00:00+05:30"]);
    assert_eq!(m.reminder_minutes, Some(10)); // DATE-TIME trigger ignored
    let o = &p.overrides[0];
    assert_eq!(o.recurrence_id, "2026-11-09T10:00:00+05:30");
    assert_eq!(o.event.start, "2026-11-09T14:00:00+05:30");

    let occ = expand_event(m, &p.overrides, dt("2026-11-01T00:00:00+00:00"), dt("2026-12-01T00:00:00+00:00"));
    let s: Vec<_> = occ.iter().map(|o| o.start.as_str()).collect();
    assert_eq!(
        s,
        vec![
            "2026-11-02T10:00:00+05:30",
            "2026-11-11T10:00:00+05:30",
            "2026-11-16T10:00:00+05:30",
            "2026-11-18T10:00:00+05:30",
            "2026-11-09T14:00:00+05:30",
        ]
    );
    assert_eq!(occ[4].summary, "Standup (moved)");
    assert_eq!(occ[4].recurrence_start, "2026-11-09T10:00:00+05:30");
    assert!(occ.iter().all(|o| o.is_recurring));
}

#[test]
fn zoom_invite_with_link_in_description() {
    let p = parse(ZOOM_INVITE);
    assert_eq!(p.method.as_deref(), Some("PUBLISH"));
    let e = &p.events[0];
    assert!(e.description.contains("https://us02web.zoom.us/j/81234567890?pwd=AbCdEf"));
    assert!(e.description.contains("Passcode: 123456"));
    assert!(e.zoom.is_none());
    let occ = expand_event(e, &[], dt("2026-11-10T00:00:00+00:00"), dt("2026-11-11T00:00:00+00:00"));
    assert_eq!(occ.len(), 1);
    assert!(occ[0].has_zoom);
}

#[test]
fn all_day_exclusive_end_and_defaults() {
    let p = parse(ALL_DAY);
    let (a, b) = (&p.events[0], &p.events[1]);
    assert!(a.all_day && a.tzid.is_none());
    assert_eq!((a.start.as_str(), a.end.as_str()), ("2026-12-25", "2026-12-27"));
    assert_eq!((b.start.as_str(), b.end.as_str()), ("2026-12-31", "2027-01-01"));
    let out = build_ics(a, None);
    assert!(out.contains("DTSTART;VALUE=DATE:20261225\r\n"));
    assert!(out.contains("DTEND;VALUE=DATE:20261227\r\n"));
    // overlap: Dec 26 is inside, Dec 27 is not (exclusive end)
    assert_eq!(expand_event(a, &[], dt("2026-12-26T00:00:00+00:00"), dt("2026-12-27T00:00:00+00:00")).len(), 1);
    assert_eq!(expand_event(a, &[], dt("2026-12-27T00:00:00+00:00"), dt("2026-12-28T00:00:00+00:00")).len(), 0);
    // yearly all-day
    let s: Vec<_> = expand_event(b, &[], dt("2026-01-01T00:00:00+00:00"), dt("2029-01-01T00:00:00+00:00")).into_iter().map(|o| o.start).collect();
    assert_eq!(s, vec!["2026-12-31", "2027-12-31", "2028-12-31"]);
}

// ---------------------------------------------------------------- round trips

fn roundtrip(src: &str) {
    let a = parse(src);
    let master = a.events.first();
    let out = build_ics_series(master, &a.overrides, a.method.as_deref());
    let mut b = parse_ics(out.as_bytes()).unwrap();
    let mut a = a;
    for e in a.events.iter_mut().chain(b.events.iter_mut()) {
        e.dtstamp = None;
    }
    for o in a.overrides.iter_mut().chain(b.overrides.iter_mut()) {
        o.event.dtstamp = None;
    }
    assert_eq!(a.method, b.method);
    assert_eq!(a.overrides, b.overrides);
    if a.events.len() <= 1 {
        assert_eq!(a.events, b.events);
    }
    for l in out.split("\r\n") {
        assert!(l.len() <= 75, "unfolded line: {}", l.len());
    }
    assert!(out.ends_with("\r\n") && !out.replace("\r\n", "").contains('\n'));
}

#[test]
fn fixtures_roundtrip() {
    for f in [GOOGLE_INVITE, OUTLOOK_INVITE, CUSTOM_TZ, APPLE_RECURRING, ZOOM_INVITE] {
        roundtrip(f);
    }
    // two all-day events: one by one
    for e in parse(ALL_DAY).events {
        let mut b = parse_ics(build_ics(&e, None).as_bytes()).unwrap();
        b.events[0].dtstamp = None;
        assert_eq!(b.events, vec![e]);
    }
}

#[test]
fn rich_event_roundtrip() {
    let mut e = timed("2026-11-06T15:00:00+05:30", "2026-11-06T16:30:00+05:30", "Asia/Kolkata", Some("FREQ=WEEKLY;BYDAY=MO,FR;COUNT=8"));
    e.uid = "rich-1@miramail".into();
    e.summary = "Plan; review, \\ ünïcödé 日本語 ".repeat(6);
    e.description = "line1\nline2, with; special \\ chars\n\n".to_string() + &"x".repeat(300);
    e.location = "https://us02web.zoom.us/j/1".into();
    e.exdates = vec!["2026-11-09T15:00:00+05:30".into(), "2026-11-13T15:00:00+05:30".into()];
    e.organizer = Some(Attendee { email: "me@example.com".into(), name: Some("Me".into()), role: AttendeeRole::Chair, status: AttendeeStatus::Accepted });
    e.attendees = vec![
        Attendee { email: "a@example.com".into(), name: Some("A \"Q\", B".replace('"', "")), role: AttendeeRole::Required, status: AttendeeStatus::Accepted },
        Attendee { email: "b@example.com".into(), name: None, role: AttendeeRole::Optional, status: AttendeeStatus::Declined },
        Attendee { email: "c@example.com".into(), name: None, role: AttendeeRole::Required, status: AttendeeStatus::NeedsAction },
    ];
    e.status = EventStatus::Tentative;
    e.reminder_minutes = Some(90);
    e.sequence = 4;
    e.zoom = Some(ZoomMeeting { meeting_id: "81234567890".into(), join_url: "https://us02web.zoom.us/j/81234567890?pwd=x".into(), password: Some("p;w,d".into()), start_url: Some("https://zoom.us/s/1?zak=abc".into()) });
    let ics = build_ics(&e, Some("REQUEST"));
    assert!(ics.contains("X-MIRAMAIL-ZOOM-ID:81234567890\r\n"));
    assert!(ics.contains("TZID:Asia/Kolkata"));
    assert!(!ics.contains("STARTURL") && !ics.contains("zak=abc"), "host start_url must never be serialised");
    // inbound zoom props are ignored by default; trusted parsing reads them (without the start_url)
    assert!(parse_ics(ics.as_bytes()).unwrap().events[0].zoom.is_none());
    let mut p = parse_ics_opts(ics.as_bytes(), ParseOptions { trust_zoom_props: true }).unwrap();
    assert_eq!(p.method.as_deref(), Some("REQUEST"));
    p.events[0].dtstamp = None;
    e.zoom.as_mut().unwrap().start_url = None;
    assert_eq!(p.events, vec![e.clone()]);

    // other shapes
    let mut ny = timed("2026-11-10T09:00:00-05:00", "2026-11-10T10:00:00-05:00", "America/New_York", None);
    ny.reminder_minutes = Some(1440);
    let mut utc = timed("2026-11-10T09:00:00+00:00", "2026-11-10T10:00:00+00:00", "UTC", None);
    utc.reminder_minutes = Some(0);
    let mut ad = CalEvent { uid: "ad".into(), all_day: true, start: "2026-02-01".into(), end: "2026-02-02".into(), rrule: Some("FREQ=YEARLY".into()), exdates: vec!["2027-02-01".into()], ..Default::default() };
    ad.summary = "Birthday".into();
    for ev in [ny, utc, ad] {
        let mut got = parse_ics(build_ics(&ev, None).as_bytes()).unwrap().events;
        got[0].dtstamp = None;
        assert_eq!(got, vec![ev]);
    }
}

#[test]
fn build_emits_vtimezone_with_dst_rules() {
    let e = timed("2026-11-10T09:00:00-05:00", "2026-11-10T10:00:00-05:00", "America/New_York", None);
    let out = build_ics(&e, None);
    assert!(out.contains("BEGIN:VTIMEZONE\r\nTZID:America/New_York"));
    assert!(out.contains("BEGIN:DAYLIGHT") && out.contains("BEGIN:STANDARD"));
    assert!(out.contains("BYMONTH=3;BYDAY=2SU") && out.contains("BYMONTH=11;BYDAY=1SU"), "{out}");
    // text injection is neutralised
    let mut bad = e.clone();
    bad.summary = "x\r\nEND:VEVENT\r\nBEGIN:VEVENT".into();
    bad.attendees = vec![Attendee { email: "a@b.c\r\nX-EVIL:1".into(), name: Some("n\r\nX-EVIL:2".into()), role: AttendeeRole::Required, status: AttendeeStatus::NeedsAction }];
    let out = build_ics(&bad, None);
    assert!(!out.lines().any(|l| l.starts_with("X-EVIL")));
    assert_eq!(out.split("\r\n").filter(|l| *l == "BEGIN:VEVENT").count(), 1);
    assert_eq!(parse_ics(out.as_bytes()).unwrap().events.len(), 1);
}

// ---------------------------------------------------------------- recurrence

#[test]
fn kolkata_weekly_no_dst() {
    let e = timed("2026-11-02T10:00:00+05:30", "2026-11-02T11:00:00+05:30", "Asia/Kolkata", Some("FREQ=WEEKLY;BYDAY=MO,WE"));
    assert_eq!(
        starts(&e, "2026-11-01T00:00:00+05:30", "2026-11-15T00:00:00+05:30"),
        vec!["2026-11-02T10:00:00+05:30", "2026-11-04T10:00:00+05:30", "2026-11-09T10:00:00+05:30", "2026-11-11T10:00:00+05:30"]
    );
}

#[test]
fn new_york_dst_end_keeps_wall_clock() {
    let e = timed("2026-10-28T09:00:00-04:00", "2026-10-28T10:00:00-04:00", "America/New_York", Some("FREQ=WEEKLY;BYDAY=WE"));
    let occ = expand_event(&e, &[], dt("2026-10-25T00:00:00+00:00"), dt("2026-11-10T00:00:00+00:00"));
    assert_eq!(occ[0].start, "2026-10-28T09:00:00-04:00");
    assert_eq!(occ[1].start, "2026-11-04T09:00:00-05:00");
    assert_eq!(occ[1].end, "2026-11-04T10:00:00-05:00");
}

#[test]
fn new_york_dst_start_and_gap() {
    let e = timed("2026-03-07T02:30:00-05:00", "2026-03-07T03:00:00-05:00", "America/New_York", Some("FREQ=DAILY;COUNT=3"));
    assert_eq!(
        starts(&e, "2026-03-01T00:00:00+00:00", "2026-03-20T00:00:00+00:00"),
        vec!["2026-03-07T02:30:00-05:00", "2026-03-08T03:30:00-04:00", "2026-03-09T02:30:00-04:00"]
    );
    // duration is real elapsed time across the change
    let w = timed("2026-03-07T23:00:00-05:00", "2026-03-08T04:00:00-04:00", "America/New_York", Some("FREQ=DAILY;COUNT=1"));
    let o = &expand_event(&w, &[], dt("2026-03-01T00:00:00+00:00"), dt("2026-03-20T00:00:00+00:00"))[0];
    assert_eq!(o.end, "2026-03-08T04:00:00-04:00");
}

#[test]
fn london_dst() {
    let e = timed("2026-03-27T08:00:00+00:00", "2026-03-27T09:00:00+00:00", "Europe/London", Some("FREQ=DAILY"));
    assert_eq!(
        starts(&e, "2026-03-27T00:00:00+00:00", "2026-03-31T00:00:00+00:00"),
        vec!["2026-03-27T08:00:00+00:00", "2026-03-28T08:00:00+00:00", "2026-03-29T08:00:00+01:00", "2026-03-30T08:00:00+01:00"]
    );
}

#[test]
fn exdate_count_until_interval() {
    let mut e = timed("2026-11-02T10:00:00+05:30", "2026-11-02T11:00:00+05:30", "Asia/Kolkata", Some("FREQ=DAILY;COUNT=5"));
    e.exdates = vec!["2026-11-04T10:00:00+05:30".into()];
    assert_eq!(starts(&e, "2026-11-01T00:00:00+00:00", "2026-12-01T00:00:00+00:00").len(), 4);
    // bare-date exdate excludes a timed occurrence
    e.exdates = vec!["2026-11-03".into()];
    let s = starts(&e, "2026-11-01T00:00:00+00:00", "2026-12-01T00:00:00+00:00");
    assert_eq!(s.len(), 4);
    assert!(!s.iter().any(|x| x.starts_with("2026-11-03")));

    e.exdates.clear();
    e.rrule = Some("FREQ=DAILY;UNTIL=20261104".into());
    assert_eq!(starts(&e, "2026-11-01T00:00:00+00:00", "2026-12-01T00:00:00+00:00").len(), 3);
    e.rrule = Some("FREQ=DAILY;UNTIL=20261104T043000Z".into()); // = 10:00 IST on the 4th, inclusive
    assert_eq!(starts(&e, "2026-11-01T00:00:00+00:00", "2026-12-01T00:00:00+00:00").len(), 3);
    e.rrule = Some("FREQ=DAILY;INTERVAL=3;COUNT=3".into());
    assert_eq!(
        starts(&e, "2026-11-01T00:00:00+00:00", "2026-12-01T00:00:00+00:00"),
        vec!["2026-11-02T10:00:00+05:30", "2026-11-05T10:00:00+05:30", "2026-11-08T10:00:00+05:30"]
    );
    // COUNT counts occurrences before the query window too
    e.rrule = Some("FREQ=DAILY;COUNT=3".into());
    assert_eq!(starts(&e, "2026-11-04T00:00:00+05:30", "2026-12-01T00:00:00+00:00").len(), 1);
}

#[test]
fn bysetpos_and_ordinals() {
    let day = |rule: &str, start: &str, to: &str| {
        let e = timed(start, &start.replace("10:00", "11:00"), "UTC", Some(rule));
        starts(&e, "2026-01-01T00:00:00+00:00", to).into_iter().map(|s| s[..10].to_string()).collect::<Vec<_>>()
    };
    let last_fri = vec!["2026-01-30", "2026-02-27", "2026-03-27", "2026-04-24"];
    assert_eq!(day("FREQ=MONTHLY;BYDAY=FR;BYSETPOS=-1", "2026-01-30T10:00:00+00:00", "2026-05-01T00:00:00+00:00"), last_fri);
    assert_eq!(day("FREQ=MONTHLY;BYDAY=-1FR", "2026-01-30T10:00:00+00:00", "2026-05-01T00:00:00+00:00"), last_fri);
    assert_eq!(day("FREQ=MONTHLY;BYDAY=1MO", "2026-11-02T10:00:00+00:00", "2027-02-01T00:00:00+00:00"), vec!["2026-11-02", "2026-12-07", "2027-01-04"]);
    assert_eq!(day("FREQ=MONTHLY;BYMONTHDAY=-1", "2026-01-31T10:00:00+00:00", "2026-04-02T00:00:00+00:00"), vec!["2026-01-31", "2026-02-28", "2026-03-31"]);
    // plain monthly on the 31st skips short months
    assert_eq!(day("FREQ=MONTHLY", "2026-01-31T10:00:00+00:00", "2026-06-01T00:00:00+00:00"), vec!["2026-01-31", "2026-03-31", "2026-05-31"]);
    // first and last weekday of month
    assert_eq!(
        day("FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=1,-1", "2026-01-01T10:00:00+00:00", "2026-03-01T00:00:00+00:00"),
        vec!["2026-01-01", "2026-01-30", "2026-02-02", "2026-02-27"]
    );
    // yearly: leap day, thanksgiving
    assert_eq!(
        day("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29", "2024-02-29T10:00:00+00:00", "2033-01-01T00:00:00+00:00"),
        vec!["2028-02-29", "2032-02-29"] // window starts 2026-01-01
    );
    assert_eq!(
        day("FREQ=YEARLY;BYMONTH=11;BYDAY=4TH", "2026-11-26T10:00:00+00:00", "2028-12-31T00:00:00+00:00"),
        vec!["2026-11-26", "2027-11-25", "2028-11-23"]
    );
    // every other week with WKST
    assert_eq!(
        day("FREQ=WEEKLY;INTERVAL=2;BYDAY=SU,MO;WKST=SU", "2026-11-01T10:00:00+00:00", "2026-12-15T00:00:00+00:00"),
        vec!["2026-11-01", "2026-11-02", "2026-11-15", "2026-11-16", "2026-11-29", "2026-11-30", "2026-12-13", "2026-12-14"]
    );
    // BYMONTH limits a daily rule
    assert_eq!(day("FREQ=DAILY;BYMONTH=2;COUNT=2", "2026-01-31T10:00:00+00:00", "2026-12-31T00:00:00+00:00"), vec!["2026-01-31", "2026-02-01"]); // DTSTART is always the first instance and counts
}

#[test]
fn rule_parse_errors_and_fallbacks() {
    for bad in ["", "garbage", "FREQ=SECONDLY", "FREQ=DAILY;INTERVAL=0", "FREQ=DAILY;BYHOUR=3", "FREQ=WEEKLY;BYDAY=XX", "FREQ=MONTHLY;BYMONTHDAY=0", "FREQ=DAILY;COUNT=abc", "FREQ=YEARLY;BYMONTH=13", "FREQ=DAILY;UNTIL=nope", "BYDAY=MO", "FREQ=DAILY;INTERVAL=99999999999"] {
        assert!(parse_rule(bad).is_err(), "{bad}");
    }
    assert!(parse_rule("RRULE:FREQ=WEEKLY;BYDAY=MO,-1FR;X-FOO=1").is_ok());
    // unparsable RRULE -> single base occurrence
    let e = timed("2026-11-02T10:00:00+00:00", "2026-11-02T11:00:00+00:00", "UTC", Some("FREQ=SECONDLY"));
    assert_eq!(starts(&e, "2026-11-01T00:00:00+00:00", "2026-12-01T00:00:00+00:00").len(), 1);
}

#[test]
fn expansion_is_capped_and_terminates() {
    let e = timed("2000-01-01T10:00:00+00:00", "2000-01-01T11:00:00+00:00", "UTC", Some("FREQ=DAILY"));
    assert_eq!(starts(&e, "2000-01-01T00:00:00+00:00", "2100-01-01T00:00:00+00:00").len(), MAX_OCCURRENCES);
    // never matches: must terminate quickly
    let never = timed("2026-01-01T10:00:00+00:00", "2026-01-01T11:00:00+00:00", "UTC", Some("FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=30"));
    // only DTSTART itself (always the first instance) can occur
    assert_eq!(starts(&never, "2026-01-01T00:00:00+00:00", "9998-01-01T00:00:00+00:00").len(), 1);
    let huge = timed("2026-01-01T10:00:00+00:00", "2026-01-01T11:00:00+00:00", "UTC", Some("FREQ=DAILY;INTERVAL=1000000;COUNT=4000000000"));
    assert!(starts(&huge, "0001-01-01T00:00:00+00:00", "9999-01-01T00:00:00+00:00").len() <= 10);
    // cancelled events yield nothing
    let mut c = timed("2026-11-02T10:00:00+00:00", "2026-11-02T11:00:00+00:00", "UTC", Some("FREQ=DAILY"));
    c.status = EventStatus::Cancelled;
    assert!(starts(&c, "2026-11-01T00:00:00+00:00", "2026-12-01T00:00:00+00:00").is_empty());
}

// ---------------------------------------------------------------- hostile input

#[test]
fn invalid_and_hostile_input_never_panics() {
    assert!(parse_ics(b"").is_err());
    assert!(parse_ics(b"hello world").is_err());
    assert!(parse_ics(&[0xff, 0xfe, 0x00, 0x01]).is_err());
    assert!(parse_ics(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap().events.is_empty());
    // missing DTSTART / bad dates are skipped, not fatal
    let p = parse_ics(b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nSUMMARY:x\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:b\r\nDTSTART:20269999T999999\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").unwrap();
    assert!(p.events.is_empty());
    // truncation at every byte of every fixture
    for f in [GOOGLE_INVITE, OUTLOOK_INVITE, APPLE_RECURRING, ZOOM_INVITE, ALL_DAY] {
        let b = crlf(f);
        for i in 0..b.len() {
            let _ = parse_ics(&b[..i]);
        }
    }
    // truncated (unterminated) event is still recovered
    let b = crlf(ZOOM_INVITE);
    let cut = String::from_utf8_lossy(&b).find("UID:zoom").unwrap();
    assert_eq!(parse_ics(&b[..cut]).unwrap().events.len(), 1);
}

#[test]
fn fuzz_garbage_and_mutations() {
    let mut x: u64 = 0x9E3779B97F4A7C15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..300 {
        let n = (next() % 3000) as usize;
        let v: Vec<u8> = (0..n).map(|_| next() as u8).collect();
        let _ = parse_ics(&v);
    }
    let base = crlf(APPLE_RECURRING);
    for _ in 0..500 {
        let mut v = base.clone();
        for _ in 0..(next() % 8 + 1) {
            let i = (next() as usize) % v.len();
            match next() % 3 {
                0 => v[i] = next() as u8,
                1 => {
                    v.remove(i);
                }
                _ => v.insert(i, b":;,\\\"\r\n =TZ"[(next() % 11) as usize]),
            }
        }
        if let Ok(p) = parse_ics(&v) {
            for e in &p.events {
                let _ = expand_event(e, &p.overrides, dt("2026-11-01T00:00:00+00:00"), dt("2027-11-01T00:00:00+00:00"));
                let _ = build_ics(e, None);
            }
        }
    }
}

#[test]
fn huge_input() {
    let big = vec![b'a'; MAX_ICS_BYTES + 1];
    assert!(matches!(parse_ics(&big), Err(Error::TooLarge { .. })));
    // exactly at the cap is accepted for parsing (and may simply contain nothing useful)
    let at_cap = vec![b'a'; MAX_ICS_BYTES];
    assert!(parse_ics(&at_cap).is_err()); // no calendar
    let spam = "BEGIN:VEVENT\r\n".repeat(MAX_ICS_BYTES / 14);
    let _ = parse_ics(spam.as_bytes());
    let nested = "BEGIN:VALARM\r\n".repeat(MAX_ICS_BYTES / 14);
    let _ = parse_ics(nested.as_bytes());
    let ends = "END:VEVENT\r\n".repeat(MAX_ICS_BYTES / 12);
    let _ = parse_ics(ends.as_bytes());
    let long = format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261110T090000Z\r\nSUMMARY:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n", "y".repeat(900_000));
    assert_eq!(parse_ics(long.as_bytes()).unwrap().events[0].summary.len(), 900_000);
    let folds = format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261110T090000Z\r\nSUMMARY:{}\r\nEND:VEVENT\r\n", "\r\n a".repeat(300_000));
    let _ = parse_ics(folds.as_bytes());
    // thousands of attendees / exdates are capped, not fatal
    let mut s = String::from("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261110T090000Z\r\nRRULE:FREQ=DAILY\r\n");
    for i in 0..4000 {
        s.push_str(&format!("ATTENDEE:mailto:u{i}@e.com\r\nEXDATE:20270101T000000Z\r\n"));
    }
    s.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    let p = parse_ics(s.as_bytes()).unwrap();
    assert_eq!(p.events[0].attendees.len(), 4000); // below the caps: nothing dropped
    assert_eq!(p.events[0].exdates.len(), 4000);
    // beyond a cap is an ERROR, never a silent truncation
    let over = s.replace("END:VEVENT", &"ATTENDEE:mailto:x@e.com\r\n".repeat(1001));
    assert!(parse_ics(over.as_bytes()).is_err());
    let _ = expand_event(&p.events[0], &[], dt("2026-11-01T00:00:00+00:00"), dt("2027-11-01T00:00:00+00:00"));
}

// ---------------------------------------------------------------- iTIP

#[test]
fn itip_reply_request_cancel() {
    let e = parse(GOOGLE_INVITE).events.remove(0);
    let reply = itip::build_reply(&e, "Bob@Example.org", AttendeeStatus::Accepted).unwrap();
    assert!(reply.contains("METHOD:REPLY\r\n"));
    let p = parse_ics(reply.as_bytes()).unwrap();
    assert_eq!(p.method.as_deref(), Some("REPLY"));
    let r = &p.events[0];
    assert_eq!(r.uid, e.uid);
    assert_eq!(r.attendees.len(), 1);
    assert_eq!(r.attendees[0].email, "bob@example.org");
    assert_eq!(r.attendees[0].status, AttendeeStatus::Accepted);
    assert_eq!(r.organizer.as_ref().unwrap().email, "alice@example.com");
    assert!(r.description.is_empty() && r.location.is_empty());
    assert_eq!(r.start, e.start);
    assert!(reply.contains("PARTSTAT=ACCEPTED"));

    // unlisted address (e.g. forwarded invite) is added; no organizer is an error
    let r2 = itip::build_reply(&e, "alias@mine.example", AttendeeStatus::Declined).unwrap();
    assert!(r2.contains("mailto:alias@mine.example") && r2.contains("PARTSTAT=DECLINED"));
    let mut no_org = e.clone();
    no_org.organizer = None;
    assert!(itip::build_reply(&no_org, "bob@example.org", AttendeeStatus::Accepted).is_err());

    let req = itip::build_request(&e);
    assert!(req.contains("METHOD:REQUEST\r\n"));
    let c = parse_ics(itip::build_cancel(&e).as_bytes()).unwrap();
    assert_eq!(c.method.as_deref(), Some("CANCEL"));
    assert_eq!(c.events[0].status, EventStatus::Cancelled);

    let mine = vec!["bob@example.org".to_string()];
    assert_eq!(itip::recipients(&e, &mine), vec!["alice@example.com", "carol@example.net"]);
    assert!(itip::request_subject(&e).starts_with("Invitation: Quarterly planning @ "));
    assert_eq!(itip::reply_subject(&e, AttendeeStatus::Tentative), "Tentative: Quarterly planning");
    assert!(itip::request_body(&e).contains("When: Fri Nov 6, 2026 09:30-10:30 (UTC)"));
    let mut m = e.clone();
    itip::set_attendee_status(&mut m, "BOB@example.org", None, AttendeeStatus::Declined);
    assert_eq!(itip::my_status(&m, &mine), Some(AttendeeStatus::Declined));

    let inv = itip::invite_from_ics(&crlf(GOOGLE_INVITE), &mine, "att-1", false).unwrap().unwrap();
    assert_eq!(inv.method, InviteMethod::Request);
    assert_eq!(inv.my_status, Some(AttendeeStatus::NeedsAction));
    assert!(itip::invite_from_ics(b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", &mine, "a", false).unwrap().is_none());
}

// ---------------------------------------------------------------- wire format

#[test]
fn wire_json_shape() {
    let e = CalEvent {
        attendees: vec![Attendee { email: "a@b.c".into(), name: None, role: AttendeeRole::Chair, status: AttendeeStatus::NeedsAction }],
        zoom: Some(ZoomMeeting { meeting_id: "1".into(), join_url: "u".into(), password: None, start_url: None }),
        ..Default::default()
    };
    let v: serde_json::Value = serde_json::to_value(&e).unwrap();
    for k in ["uid", "calendarId", "allDay", "tzid", "rrule", "exdates", "reminderMinutes", "sequence", "href", "etag", "organizer", "zoom"] {
        assert!(v.get(k).is_some(), "missing {k}");
    }
    assert_eq!(v["status"], "confirmed");
    assert_eq!(v["attendees"][0]["status"], "needs-action");
    assert_eq!(v["attendees"][0]["role"], "chair");
    assert_eq!(v["zoom"]["joinUrl"], "u");
    let back: CalEvent = serde_json::from_value(v).unwrap();
    assert_eq!(back, e);
    let inv = Invite { method: InviteMethod::Request, event: e, my_status: Some(AttendeeStatus::Tentative), already_in_calendar: false, attachment_id: "x".into(), recurrence_id: None, overrides: vec![], sender_unverified: false };
    let v = serde_json::to_value(&inv).unwrap();
    assert_eq!(v["method"], "REQUEST");
    assert_eq!(v["myStatus"], "tentative");
    assert_eq!(v["alreadyInCalendar"], false);
    let ci = CalendarInfo { id: "i".into(), name: "n".into(), color: "#000000".into(), kind: CalendarKind::Caldav, url: None, username: None, visible: true, read_only: false, last_sync: None, error: None };
    let v = serde_json::to_value(ci).unwrap();
    assert_eq!(v["kind"], "caldav");
    assert!(v.get("readOnly").is_some() && v.get("lastSync").is_some());
}

// ---------------------------------------------------------------- storage

fn ev_in(cal: &str, uid: &str, start: &str, end: &str) -> CalEvent {
    CalEvent { uid: uid.into(), calendar_id: cal.into(), summary: format!("S-{uid}"), start: start.into(), end: end.into(), tzid: Some("UTC".into()), ..Default::default() }
}

#[test]
fn storage_calendars_and_defaults() {
    let db = Calendar::open_in_memory().unwrap();
    let l = db.list_calendars().unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!((l[0].id.as_str(), l[0].name.as_str(), l[0].kind), (DEFAULT_CALENDAR_ID, "Personal", CalendarKind::Local));
    assert!(l[0].visible && !l[0].read_only);

    let w = db.create_local("Work", "#112233").unwrap();
    assert!(db.create_local("", "#112233").is_err());
    assert!(db.create_local("Bad", "red").is_err());
    db.update_calendar(&w.id, &CalendarPatch { name: Some("Work 2".into()), visible: Some(false), color: Some("#abcdef".into()), ..Default::default() }).unwrap();
    let w2 = db.get_calendar(&w.id).unwrap().unwrap();
    assert_eq!((w2.name.as_str(), w2.color.as_str(), w2.visible), ("Work 2", "#abcdef", false));
    assert!(matches!(db.update_calendar("nope", &CalendarPatch::default()), Err(Error::NotFound(_))));

    let c = db
        .create_calendar(&NewCalendar { id: Some("dav1".into()), name: "Fastmail".into(), color: "#00ff00".into(), kind: CalendarKind::Caldav, url: Some("https://dav.example/cal/".into()), username: Some("me@example.com".into()), read_only: false })
        .unwrap();
    assert_eq!(c.kind, CalendarKind::Caldav);
    db.set_calendar_sync_state("dav1", Some("ctag-1"), Some("sync-9"), None, Some(1_700_000_000)).unwrap();
    assert_eq!(db.get_calendar_sync_state("dav1").unwrap(), SyncState { ctag: Some("ctag-1".into()), sync_marker: Some("sync-9".into()), error: None, last_sync: Some(1_700_000_000) });
    db.set_calendar_sync_state("dav1", None, None, Some("401"), None).unwrap();
    assert_eq!(db.get_calendar("dav1").unwrap().unwrap().error.as_deref(), Some("401"));

    assert!(db.delete_calendar(DEFAULT_CALENDAR_ID).is_err());
    db.upsert_event(&ev_in("dav1", "e1", "2026-11-06T09:00:00+00:00", "2026-11-06T10:00:00+00:00")).unwrap();
    db.delete_calendar("dav1").unwrap();
    assert!(db.get_event("e1", "dav1").unwrap().is_none());
    assert_eq!(db.list_calendars().unwrap().len(), 2);
}

#[test]
fn storage_events_occurrences_and_overrides() {
    let db = Calendar::open_in_memory().unwrap();
    let hidden = db.create_local("Hidden", "#222222").unwrap();
    db.update_calendar(&hidden.id, &CalendarPatch { visible: Some(false), ..Default::default() }).unwrap();

    let mut single = ev_in("personal", "one", "2026-11-06T09:00:00+00:00", "2026-11-06T10:00:00+00:00");
    single.href = Some("/cal/one.ics".into());
    single.etag = Some("\"abc\"".into());
    db.upsert_event(&single).unwrap();
    let mut got = db.get_event("one", "personal").unwrap().unwrap();
    assert!(got.dtstamp.is_some());
    got.dtstamp = None;
    assert_eq!(got, single);
    assert_eq!(db.find_event("one").unwrap().unwrap().uid, "one");
    single.summary = "renamed".into();
    db.upsert_event(&single).unwrap();
    assert_eq!(db.list_events("personal").unwrap().len(), 1);
    assert_eq!(db.list_event_refs("personal").unwrap()[0].href.as_deref(), Some("/cal/one.ics"));

    db.upsert_event(&ev_in(&hidden.id, "h", "2026-11-06T09:00:00+00:00", "2026-11-06T10:00:00+00:00")).unwrap();
    db.upsert_event(&ev_in("personal", "far", "2030-01-01T09:00:00+00:00", "2030-01-01T10:00:00+00:00")).unwrap();
    assert!(db.upsert_event(&ev_in("missing", "z", "2026-11-06T09:00:00+00:00", "2026-11-06T10:00:00+00:00")).is_err());
    assert!(db.upsert_event(&ev_in("personal", "bad", "garbage", "garbage")).is_err());

    let (a, b) = ("2026-11-01T00:00:00+00:00", "2026-12-01T00:00:00+00:00");
    let vis = db.list_occurrences(a, b, true).unwrap();
    assert_eq!(vis.len(), 1);
    assert_eq!(vis[0].summary, "renamed");
    assert_eq!(vis[0].color, "#4f7cff");
    assert_eq!(db.list_occurrences(a, b, false).unwrap().len(), 2);
    assert!(db.list_occurrences("x", b, true).is_err());

    // recurring + overrides via a CalDAV-style resource
    let uids = db.upsert_ics("personal", &crlf(APPLE_RECURRING), Some("/cal/standup.ics"), Some("\"e1\"")).unwrap();
    assert_eq!(uids, vec!["APPLE-STANDUP-1"]);
    let occ = db.list_occurrences(a, b, true).unwrap();
    let standups: Vec<_> = occ.iter().filter(|o| o.uid == "APPLE-STANDUP-1").collect();
    assert_eq!(standups.len(), 5);
    assert!(standups.iter().any(|o| o.summary == "Standup (moved)" && o.start == "2026-11-09T14:00:00+05:30"));
    assert!(standups.iter().all(|o| !(o.start == "2026-11-09T10:00:00+05:30" || o.start == "2026-11-04T10:00:00+05:30")));
    // sorted by instant
    let ts: Vec<_> = occ.iter().map(|o| DateTime::parse_from_rfc3339(&o.start).unwrap().timestamp()).collect();
    assert!(ts.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(db.get_overrides("APPLE-STANDUP-1", "personal").unwrap().len(), 1);
    assert_eq!(db.get_event("APPLE-STANDUP-1", "personal").unwrap().unwrap().href.as_deref(), Some("/cal/standup.ics"));

    assert!(db.delete_event("APPLE-STANDUP-1", "personal").unwrap());
    assert!(!db.delete_event("APPLE-STANDUP-1", "personal").unwrap());
    assert!(db.get_overrides("APPLE-STANDUP-1", "personal").unwrap().is_empty());
    assert!(db.upsert_ics("personal", b"junk", None, None).is_err());
}

#[test]
fn storage_all_day_in_negative_offset_range() {
    let db = Calendar::open_in_memory().unwrap();
    let ad = CalEvent { uid: "ad".into(), calendar_id: "personal".into(), all_day: true, start: "2026-12-25".into(), end: "2026-12-26".into(), summary: "X".into(), ..Default::default() };
    db.upsert_event(&ad).unwrap();
    let r = db.list_occurrences("2026-12-25T00:00:00-08:00", "2026-12-26T00:00:00-08:00", true).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].start, "2026-12-25");
    assert!(db.list_occurrences("2026-12-26T00:00:00-08:00", "2026-12-27T00:00:00-08:00", true).unwrap().is_empty());
}

#[test]
fn storage_settings_and_no_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("calendar.db");
    {
        let db = Calendar::open(&path).unwrap();
        db.set_setting("zoom.client_id", "abc").unwrap();
        db.set_setting("zoom.client_id", "abc2").unwrap();
        db.set_setting("caldav.sync.dav1", "sync-9").unwrap();
        db.upsert_event(&ev_in("personal", "p", "2026-11-06T09:00:00+00:00", "2026-11-06T10:00:00+00:00")).unwrap();
        for k in ["zoom.client_secret", "zoom.refresh_token", "caldav.password", "x.access_token"] {
            assert!(db.set_setting(k, "v").is_err(), "{k}");
        }
        let cols = db.column_names().unwrap();
        assert!(!cols.is_empty());
        for c in &cols {
            let l = c.to_lowercase();
            assert!(!l.contains("password") && !l.contains("secret") && !l.contains("token") && !l.contains("credential"), "suspicious column {c}");
        }
    }
    let db = Calendar::open(&path).unwrap(); // persists, default calendar not duplicated
    assert_eq!(db.get_setting("zoom.client_id").unwrap().as_deref(), Some("abc2"));
    assert_eq!(db.get_setting("nope").unwrap(), None);
    db.delete_setting("zoom.client_id").unwrap();
    assert_eq!(db.get_setting("zoom.client_id").unwrap(), None);
    assert_eq!(db.list_calendars().unwrap().len(), 1);
    assert_eq!(db.list_events("personal").unwrap().len(), 1);
    assert_ne!(new_id(), new_id());
    assert!(new_event_uid().ends_with("@miramail"));
}
