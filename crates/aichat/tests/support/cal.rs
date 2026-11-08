//! In-memory `CalendarHost` + `CalActionHost` for the calendar tool tests. Every mutating call of the action
//! side is recorded in `log`; a test that must prove "nothing happened" asserts it is empty. The read side
//! does naive string matching on dates (the real time arithmetic is tested in the calendar crate and in the
//! app host). There is no way to send mail through these types: `sent` counts what the real app would mail
//! and only the action side can raise it.
#![allow(dead_code)]

use std::sync::Mutex;

use aichat::cal::*;
use aichat::tools::{BoxFut, Scope};

pub const CAL_WORK: &str = "work-id";
pub const CAL_HOME: &str = "home-id";
pub const CAL_SECRET: &str = "secret-id";
pub const CAL_RO: &str = "holidays-id";

pub fn meta(id: &str, name: &str, ro: bool, default: bool) -> CalMeta {
    CalMeta { id: id.into(), name: name.into(), read_only: ro, kind: "local".into(), is_default: default }
}

pub fn event(cal: &str, uid: &str, title: &str, start: &str) -> EventRow {
    EventRow {
        calendar_id: cal.into(),
        uid: uid.into(),
        title: title.into(),
        when: format!("Thu 12 Nov {}", &start[11..16]),
        start: start.into(),
        end: start.into(),
        start_ts: 1_800_000_000,
        end_ts: 1_800_003_600,
        my_status: "organizer".into(),
        busy: true,
        version: format!("v1-{uid}"),
        ..EventRow::default()
    }
}

pub struct FakeCal {
    pub cals: Vec<CalMeta>,
    pub events: Mutex<Vec<EventRow>>,
    pub now: i64,
    pub known: Vec<String>,
    pub mine: Mutex<Vec<String>>,
    pub zoom: bool,
    pub conflicts: Mutex<Vec<String>>,
    pub invite: Mutex<Option<InviteInfo>>,
    pub validity: u32,
    /// Return events of every calendar whatever the request says (a buggy host).
    pub leaky: bool,
    pub log: Mutex<Vec<String>>,
    /// Calls to `CalendarHost` methods that could have changed something (must stay empty).
    pub sent: Mutex<usize>,
    pub fail_create: Mutex<Option<String>>,
    /// Title of the all-day event `free_busy` reports.
    pub all_day_title: Mutex<String>,
    /// `respond_invite` saves the answer but cannot mail it.
    pub respond_unsent: Mutex<bool>,
}

impl FakeCal {
    pub fn new() -> FakeCal {
        FakeCal {
            cals: vec![meta(CAL_WORK, "Work", false, true), meta(CAL_HOME, "Home", false, false), meta(CAL_SECRET, "Secret", false, false), meta(CAL_RO, "Holidays", true, false)],
            events: Mutex::new(vec![]),
            now: 1_799_000_000,
            known: vec!["ravi@known.test".into()],
            mine: Mutex::new(vec!["me@example.org".into()]),
            zoom: false,
            conflicts: Mutex::new(vec![]),
            invite: Mutex::new(None),
            validity: 7,
            leaky: false,
            log: Mutex::new(vec![]),
            sent: Mutex::new(0),
            fail_create: Mutex::new(None),
            all_day_title: Mutex::new("Company holiday".into()),
            respond_unsent: Mutex::new(false),
        }
    }
    pub fn with_events(self, ev: Vec<EventRow>) -> FakeCal {
        *self.events.lock().unwrap() = ev;
        self
    }
    pub fn mutations(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
    fn note(&self, s: impl Into<String>) {
        self.log.lock().unwrap().push(s.into());
    }
    pub fn set_events(&self, ev: Vec<EventRow>) {
        *self.events.lock().unwrap() = ev;
    }
}

fn when_for(start: &str, end: Option<&str>) -> When {
    When { start: start.into(), end: end.unwrap_or(start).into(), tzid: Some("Europe/Berlin".into()), all_day: start.len() == 10, start_ts: 1_800_000_000, end_ts: 1_800_003_600, display: format!("Fri 13 Nov {}", start.get(11..16).unwrap_or("all day")) }
}

impl CalendarHost for FakeCal {
    fn now(&self) -> i64 {
        self.now
    }
    fn calendars(&self) -> Result<Vec<CalMeta>, String> {
        Ok(self.cals.clone())
    }
    fn list_events(&self, calendars: &[String], from: &str, to: &str) -> Result<EventList, String> {
        if from.is_empty() || to.is_empty() {
            return Err("bad range".into());
        }
        let all = self.events.lock().unwrap().clone();
        let v: Vec<EventRow> = all.into_iter().filter(|e| self.leaky || calendars.contains(&e.calendar_id)).collect();
        Ok(EventList { total: v.len(), events: v, tz_name: "Europe/Berlin".into() })
    }
    fn get_event(&self, calendar_id: &str, uid: &str, _rid: &str) -> Result<Option<EventRow>, String> {
        Ok(self.events.lock().unwrap().iter().find(|e| e.calendar_id == calendar_id && e.uid == uid).cloned())
    }
    fn free_busy(&self, _c: &[String], _f: &str, _t: &str, _i: bool) -> Result<BusyOut, String> {
        Ok(BusyOut { blocks: vec![BusyBlock { start: "2026-11-12 13:00".into(), end: "2026-11-12 14:00".into(), tentative: false }], all_day: vec![("2026-11-13".into(), self.all_day_title.lock().unwrap().clone())], tz_name: "Europe/Berlin".into() })
    }
    fn find_free_slots(&self, _c: &[String], req: &SlotReq) -> Result<SlotsOut, String> {
        Ok(SlotsOut {
            slots: vec![SlotOut { start: "2026-11-12T15:00".into(), end: "2026-11-12T15:30".into(), when: format!("Thu 12 Nov 15:00 ({} min)", req.duration_minutes) }],
            tz_name: "Europe/Berlin".into(),
            working_hours: "09:00-18:00 Mon-Fri".into(),
            note: String::new(),
        })
    }
    fn resolve_when(&self, start: &str, end: Option<&str>, _tz: Option<&str>) -> Result<When, String> {
        if start.starts_with("bad") {
            return Err("Use YYYY-MM-DD or YYYY-MM-DDTHH:MM.".into());
        }
        let mut w = when_for(start, end);
        if start.starts_with("1999") {
            w.start_ts = 1_000;
            w.end_ts = 2_000;
        }
        Ok(w)
    }
    fn resolve_change(&self, _cur: &EventRow, start: Option<&str>, end: Option<&str>, _tz: Option<&str>) -> Result<When, String> {
        Ok(when_for(start.or(end).unwrap_or("2026-11-13T10:00"), end))
    }
    fn conflicts(&self, _c: &[String], _w: &When, _x: Option<&str>) -> Result<Vec<String>, String> {
        Ok(self.conflicts.lock().unwrap().clone())
    }
    fn address_known(&self, a: &str) -> Result<bool, String> {
        Ok(self.known.iter().any(|k| k.eq_ignore_ascii_case(a)))
    }
    fn my_addresses(&self) -> Vec<String> {
        self.mine.lock().unwrap().clone()
    }
    fn zoom_connected(&self) -> bool {
        self.zoom
    }
    fn invite_in_message(&self, _scope: &Scope, _uid: u32, _v: u32) -> BoxFut<'_, Result<Option<InviteInfo>, String>> {
        let i = self.invite.lock().unwrap().clone();
        Box::pin(async move { Ok(i) })
    }
}

impl CalActionHost for FakeCal {
    fn calendar(&self, id: &str) -> Result<Option<CalMeta>, String> {
        Ok(self.cals.iter().find(|c| c.id == id).cloned())
    }
    fn lookup(&self, calendar_id: &str, uid: &str, _rid: &str) -> Result<Option<EventRow>, String> {
        Ok(self.events.lock().unwrap().iter().find(|e| e.calendar_id == calendar_id && e.uid == uid).cloned())
    }
    fn mail_validity(&self, _account: &str, _folder: &str) -> Result<Option<u32>, String> {
        Ok(Some(self.validity))
    }
    fn can_send(&self) -> bool {
        !self.mine.lock().unwrap().is_empty()
    }
    fn create_event(&self, calendar_id: &str, spec: &EventSpec, notify: bool) -> BoxFut<'_, Result<Created, String>> {
        if let Some(e) = self.fail_create.lock().unwrap().clone() {
            return Box::pin(async move { Err(e) });
        }
        let uid = format!("new-{}", self.events.lock().unwrap().len() + 1);
        self.note(format!("create {calendar_id} {uid} {}", spec.title));
        let mut ev = event(calendar_id, &uid, &spec.title, &format!("{}:00", spec.when.start.get(..16).unwrap_or("2026-11-13T10")));
        ev.version = "v1-created".into();
        self.events.lock().unwrap().push(ev);
        let invited = notify && !spec.attendees.is_empty() && self.can_send();
        if invited {
            *self.sent.lock().unwrap() += 1;
        }
        let cal = calendar_id.to_string();
        let zoom = spec.add_zoom.then(|| "zm1".to_string());
        Box::pin(async move { Ok(Created { uid, calendar_id: cal, version: "v1-created".into(), invited, zoom_meeting_id: zoom, warning: None }) })
    }
    fn change_event(&self, calendar_id: &str, uid: &str, changes: &EventChanges, notify: bool, expected_version: &str) -> BoxFut<'_, Result<Changed, String>> {
        let can_send = self.can_send();
        let mut evs = self.events.lock().unwrap();
        let ev = evs.iter_mut().find(|e| e.calendar_id == calendar_id && e.uid == uid);
        let r = match ev {
            Some(e) if !expected_version.is_empty() && e.version != expected_version => Err("The event was changed after this suggestion was made, so nothing was done.".to_string()),
            Some(e) => {
                self.note(format!("change {calendar_id} {uid} {:?} notify={notify}", changes.title));
                let snapshot = format!("{{\"title\":\"{}\",\"version\":\"{}\"}}", e.title, e.version);
                let notified = notify && can_send && e.attendee_count > 0;
                if let Some(t) = &changes.title {
                    e.title = t.clone();
                }
                e.version = "v2-changed".into();
                if notified {
                    *self.sent.lock().unwrap() += 1;
                }
                Ok(Changed { snapshot, version: e.version.clone(), notified, warning: None })
            }
            None => Err("gone".to_string()),
        };
        Box::pin(async move { r })
    }
    fn cancel_event(&self, calendar_id: &str, uid: &str, notify: bool, expected_version: &str) -> BoxFut<'_, Result<Cancelled, String>> {
        let mut evs = self.events.lock().unwrap();
        let pos = evs.iter().position(|e| e.calendar_id == calendar_id && e.uid == uid);
        let r = match pos {
            Some(i) if !expected_version.is_empty() && evs[i].version != expected_version => Err("The event was changed after this suggestion was made, so nothing was done.".to_string()),
            Some(i) => {
                self.note(format!("cancel {calendar_id} {uid} notify={notify}"));
                let e = evs.remove(i);
                if notify {
                    *self.sent.lock().unwrap() += 1;
                }
                Ok(Cancelled { snapshot: format!("{{\"cal\":\"{}\",\"uid\":\"{}\",\"title\":\"{}\",\"version\":\"{}\"}}", e.calendar_id, e.uid, e.title, e.version), notified: notify, warning: None })
            }
            None => Err("gone".to_string()),
        };
        Box::pin(async move { r })
    }
    fn restore_event(&self, snapshot: &str, notify: bool) -> BoxFut<'_, Result<String, String>> {
        self.note(format!("restore {snapshot} notify={notify}"));
        let v: serde_json::Value = serde_json::from_str(snapshot).unwrap_or_default();
        let mut evs = self.events.lock().unwrap();
        if let Some(uid) = v.get("uid").and_then(|u| u.as_str()) {
            // a cancelled event comes back
            let mut e = event(v["cal"].as_str().unwrap_or(CAL_WORK), uid, v["title"].as_str().unwrap_or(""), "2026-11-12T13:00:00");
            e.version = v["version"].as_str().unwrap_or("v1").to_string();
            evs.push(e);
        } else if let Some(e) = evs.iter_mut().find(|e| e.title != v["title"].as_str().unwrap_or("")) {
            e.title = v["title"].as_str().unwrap_or("").to_string();
            e.version = v["version"].as_str().unwrap_or("v1").to_string();
        }
        Box::pin(async move { Ok("v-restored".to_string()) })
    }
    fn remove_created(&self, calendar_id: &str, uid: &str, zoom: Option<&str>, notify: bool) -> BoxFut<'_, Result<(), String>> {
        self.note(format!("remove {calendar_id} {uid} zoom={zoom:?} notify={notify}"));
        self.events.lock().unwrap().retain(|e| !(e.calendar_id == calendar_id && e.uid == uid));
        Box::pin(async move { Ok(()) })
    }
    fn respond_invite(&self, target: &RespondTarget, status: &str) -> BoxFut<'_, Result<Responded, String>> {
        self.note(format!("respond {target:?} {status}"));
        let sent = !*self.respond_unsent.lock().unwrap();
        if sent {
            *self.sent.lock().unwrap() += 1;
        }
        Box::pin(async move { Ok(Responded { sent, warning: None }) })
    }
}
