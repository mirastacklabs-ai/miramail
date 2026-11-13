//! iPhone home-screen widgets (WidgetKit extension "MiraMailWidgets", app/src-tauri/gen/apple/MiraMailWidgets/).
//!
//! The widgets never connect to a mail server, never read the Keychain and make no network call. This module writes a
//! small JSON snapshot into the App Group container (`<group>/widget.json`; the group is `group.<app bundle id>`, the
//! path comes from Swift via the mailcheck plugin) and asks WidgetKit to reload; the extension only renders the file.
//!
//! Snapshot schema, version 1 (mirrored in MiraMailWidgets/Snapshot.swift; change both together and bump `version`):
//! ```text
//! {
//!   "version": 1,
//!   "updated_at": <unix seconds when written>,
//!   "details": <bool: the user opted in to names / subjects / event titles>,
//!   "accounts": [ { "label": "Account 1", "unread": <n> }, ... ],       // generic labels, never an address
//!   "latest":   [ { "from_name": <string|null>, "subject": <string|null>, "received_at": <unix s> }, ... ],  // <= 5, newest first
//!   "calendar": [ { "title": <string|null>, "start": <unix s>, "end": <unix s>, "all_day": <bool>, "colour": "#rrggbb" }, ... ]  // today + tomorrow
//! }
//! ```
//! Privacy: `from_name`, `subject` and real event titles are written only when "Show sender names and subjects in
//! widgets" is on (default off); otherwise `latest` carries only the time, and event titles are "Busy". No address is
//! written as a sender name or account label. With the setting ON, subjects and event titles are free text and can
//! contain anything, including addresses. Turning the setting off rewrites the file without them at once (or deletes
//! it if that fails); removing the last account deletes it. The setting lives in `widget_prefs.json` (own file, own commands).
//!
//! When it runs (iOS only; elsewhere every entry point is a no-op): after the INBOX summary changes (badge.rs), after a
//! background check (lib.rs), after calendar changes (cal_sched.rs), at start, and when the setting changes.
//! WidgetKit is told to reload at most once per 5 minutes, or once per minute when the content changed. Nothing here is logged except counts.
//!
//! Also: `miramail://mail` / `miramail://calendar` links from a widget tap select that tab (`on_opened`).

// The driver runs on iOS only; the pure parts are tested everywhere.
#![cfg_attr(not(target_os = "ios"), allow(dead_code))]

use calendar::Occurrence;
use chrono::{Duration as ChronoDuration, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const FILE: &str = "widget.json";
pub const PREFS_FILE: &str = "widget_prefs.json";
pub const VERSION: u32 = 1;
pub const MAX_LATEST: usize = 5;
pub const MAX_EVENTS: usize = 30;
const MAX_TEXT: usize = 120;
const MAX_NAME: usize = 60;
/// Title shown for every event while details are off.
pub const BUSY: &str = "Busy";
const DEFAULT_COLOUR: &str = "#00c9b1";
/// WidgetKit reloads: at most this often unless the content changed.
pub const RELOAD_EVERY_SECS: i64 = 300;
/// ... but a changed snapshot is not reloaded more often than this.
pub const RELOAD_CHANGED_SECS: i64 = 60;

// ---------------------------------------------------------------- wire types

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCount {
    pub label: String,
    pub unread: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Latest {
    pub from_name: Option<String>,
    pub subject: Option<String>,
    pub received_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub title: Option<String>,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub colour: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub updated_at: i64,
    pub details: bool,
    pub accounts: Vec<AccountCount>,
    pub latest: Vec<Latest>,
    pub calendar: Vec<Event>,
}

// ---------------------------------------------------------------- pure building

/// One unread INBOX message as the cache knows it (`from` may be a bare address when the sender has no name).
#[derive(Debug, Clone)]
pub struct RecentIn {
    pub from: String,
    pub subject: String,
    pub date: i64,
}

/// One calendar occurrence with its times already resolved to unix seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEvent {
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub colour: String,
}

pub struct Input {
    /// Unread per account, in account order.
    pub unread: Vec<u32>,
    pub recent: Vec<RecentIn>,
    pub events: Vec<RawEvent>,
    pub now: i64,
    /// Window the calendar part covers: [start, end) (local today 00:00 .. day after tomorrow 00:00).
    pub window: (i64, i64),
    pub details: bool,
}

fn clip(s: &str, max: usize) -> String {
    crate::textsafe::clean_display(s).trim().chars().take(max).collect()
}

fn valid_colour(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit())
}

/// A sender name that is safe to show: never an address (a From without a name arrives as the address itself).
fn sender_name(from: &str) -> Option<String> {
    let n = clip(from, MAX_NAME);
    (!n.is_empty() && !n.contains('@')).then_some(n)
}

pub fn build(i: &Input) -> Snapshot {
    let accounts = i
        .unread
        .iter()
        .enumerate()
        .map(|(n, u)| AccountCount { label: format!("Account {}", n + 1), unread: *u })
        .collect();
    let mut recent: Vec<&RecentIn> = i.recent.iter().collect();
    recent.sort_by_key(|r| std::cmp::Reverse(r.date));
    let latest = recent
        .into_iter()
        .take(MAX_LATEST)
        .map(|r| Latest {
            from_name: if i.details { sender_name(&r.from) } else { None },
            subject: if i.details { Some(clip(&r.subject, MAX_TEXT)).filter(|s| !s.is_empty()) } else { None },
            received_at: r.date,
        })
        .collect();
    let (ws, we) = i.window;
    let mut ev: Vec<&RawEvent> = i.events.iter().filter(|e| e.end > ws && e.start < we && e.end >= e.start).collect();
    ev.sort_by(|a, b| a.start.cmp(&b.start).then(b.all_day.cmp(&a.all_day)).then(a.end.cmp(&b.end)));
    let calendar = ev
        .into_iter()
        .take(MAX_EVENTS)
        .map(|e| Event {
            title: Some(if i.details { clip(&e.title, MAX_TEXT) } else { BUSY.to_string() }).map(|t| if t.is_empty() { BUSY.to_string() } else { t }),
            start: e.start,
            end: e.end,
            all_day: e.all_day,
            colour: if valid_colour(&e.colour) { e.colour.clone() } else { DEFAULT_COLOUR.to_string() },
        })
        .collect();
    Snapshot { version: VERSION, updated_at: i.now, details: i.details, accounts, latest, calendar }
}

/// Resolve an occurrence to unix seconds in `tz`. Timed: RFC 3339 (a floating time, `tzid == None`, carries a placeholder
/// offset and is read as wall-clock digits in `tz`, like the reminders do). All-day: `YYYY-MM-DD`, the end exclusive.
pub fn raw_event<Tz: TimeZone>(o: &Occurrence, tzid: Option<&str>, tz: &Tz) -> Option<RawEvent> {
    let (start, end) = if o.all_day {
        let day = |s: &str| NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok();
        let s = day(&o.start)?;
        let mut e = day(&o.end).unwrap_or(s);
        if e <= s {
            e = s + ChronoDuration::days(1);
        }
        let ts = |d: NaiveDate| tz.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest().map(|t| t.timestamp());
        (ts(s)?, ts(e)?)
    } else {
        let parsed = chrono::DateTime::parse_from_rfc3339(o.start.trim()).ok()?;
        let start = if tzid.is_none() {
            let naive = chrono::NaiveDateTime::parse_from_str(o.start.get(..19)?, "%Y-%m-%dT%H:%M:%S").ok()?;
            tz.from_local_datetime(&naive).earliest()?.timestamp()
        } else {
            parsed.timestamp()
        };
        let dur = chrono::DateTime::parse_from_rfc3339(o.end.trim()).ok().map(|e| e.timestamp() - parsed.timestamp()).filter(|d| *d > 0).unwrap_or(30 * 60);
        (start, start + dur)
    };
    Some(RawEvent { title: o.summary.clone(), start, end, all_day: o.all_day, colour: o.color.clone() })
}

static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Atomic write (unique temp file in the same directory, then rename): the widget never reads half a file, and two
/// writers never share a temp file. A failed write leaves no temp file behind.
pub fn write_atomic(dir: &Path, s: &Snapshot) -> std::io::Result<PathBuf> {
    let dest = dir.join(FILE);
    let n = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{FILE}.{}.{n}.tmp", std::process::id()));
    let r = std::fs::write(&tmp, serde_json::to_vec(s).map_err(std::io::Error::other)?).and_then(|_| std::fs::rename(&tmp, &dest));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map(|_| dest)
}

/// Hash of everything except `updated_at`: "did the content change".
pub fn signature(s: &Snapshot) -> u64 {
    let mut c = s.clone();
    c.updated_at = 0;
    let mut h = DefaultHasher::new();
    serde_json::to_string(&c).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// Throttle for `WidgetCenter.reloadAllTimelines()`: once per [`RELOAD_EVERY_SECS`] unless the content changed.
#[derive(Debug, Default)]
pub struct Gate {
    last_at: Option<i64>,
    last_sig: u64,
}

impl Gate {
    pub fn due(&mut self, now: i64, sig: u64) -> bool {
        let due = match self.last_at {
            None => true,
            Some(t) => (sig != self.last_sig && now - t >= RELOAD_CHANGED_SECS) || now - t >= RELOAD_EVERY_SECS || now < t,
        };
        if due {
            self.last_at = Some(now);
            self.last_sig = sig;
        }
        due
    }
}

// ---------------------------------------------------------------- deep links

/// `miramail://mail` / `miramail://calendar` -> the tab to select.
pub fn parse_deep_link(u: &tauri::Url) -> Option<&'static str> {
    if !u.scheme().eq_ignore_ascii_case("miramail") {
        return None;
    }
    let target = u.host_str().map(str::to_string).unwrap_or_else(|| u.path().trim_matches('/').to_string());
    match target.to_ascii_lowercase().as_str() {
        "mail" => Some("mail"),
        "calendar" => Some("calendar"),
        _ => None,
    }
}

static PENDING_TAB: Mutex<Option<&'static str>> = Mutex::new(None);

/// A URL opened the app (RunEvent::Opened). Only the two widget links are acted on; the page asks for the tab.
pub fn on_opened(app: &tauri::AppHandle, urls: &[tauri::Url]) {
    use tauri::Emitter;
    if let Some(tab) = urls.iter().find_map(parse_deep_link) {
        *PENDING_TAB.lock().unwrap_or_else(|e| e.into_inner()) = Some(tab);
        let _ = app.emit_to("main", "app://open-tab", tab);
    }
}

/// The tab a widget tap asked for (returned once). The page calls this at start and on `app://open-tab`.
#[tauri::command]
pub fn take_pending_tab() -> Option<String> {
    PENDING_TAB.lock().unwrap_or_else(|e| e.into_inner()).take().map(str::to_string)
}

// ---------------------------------------------------------------- preference

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct WidgetPrefs {
    /// "Show sender names and subjects in widgets" (default off).
    pub show_details: bool,
}

pub fn load_prefs(path: &Path) -> WidgetPrefs {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_prefs(path: &Path, p: &WidgetPrefs) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(p).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, path)
}

#[derive(Default)]
pub struct WidgetState {
    prefs: Mutex<WidgetPrefs>,
    path: Mutex<Option<PathBuf>>,
}

impl WidgetState {
    pub fn init(&self, path: PathBuf) {
        *self.prefs.lock().unwrap_or_else(|e| e.into_inner()) = load_prefs(&path);
        *self.path.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    }
    pub fn get(&self) -> WidgetPrefs {
        *self.prefs.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn set(&self, p: WidgetPrefs) -> Result<(), String> {
        *self.prefs.lock().unwrap_or_else(|e| e.into_inner()) = p;
        let Some(path) = self.path.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return Ok(()) };
        save_prefs(&path, &p).map_err(|e| format!("Couldn't save the widget setting: {e}"))
    }
}

#[tauri::command]
pub fn get_widget_prefs(state: tauri::State<'_, WidgetState>) -> WidgetPrefs {
    state.get()
}

#[tauri::command]
pub fn set_widget_prefs(app: tauri::AppHandle, state: tauri::State<'_, WidgetState>, show_details: bool) -> Result<WidgetPrefs, String> {
    let p = WidgetPrefs { show_details };
    state.set(p)?;
    if !show_details {
        // Privacy first: the details-free snapshot is on disk before this returns (or the file is gone).
        refresh_sync(&app, true);
    } else {
        request(&app, true);
    }
    Ok(p)
}

// ---------------------------------------------------------------- iOS driver

#[cfg(target_os = "ios")]
mod ios {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use tauri::{AppHandle, Manager};

    static PENDING: AtomicBool = AtomicBool::new(false);
    static FORCE: AtomicBool = AtomicBool::new(false);
    static GATE: Mutex<Gate> = Mutex::new(Gate { last_at: None, last_sig: 0 });
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    /// One refresh at a time (background check, coalesced request and the settings command may overlap).
    static REFRESH: Mutex<()> = Mutex::new(());

    fn group_dir(app: &AppHandle) -> Option<PathBuf> {
        if let Some(d) = DIR.get() {
            return Some(d.clone());
        }
        let d = tauri_plugin_mailcheck::widget_container(app)?;
        let _ = DIR.set(d.clone());
        Some(d)
    }

    fn gather(app: &AppHandle, storage: &std::sync::Arc<mailcore::Storage>, badge: &crate::badge::BadgeState, details: bool, now: i64) -> Input {
        use chrono::Local;
        let storage = storage.clone();
        let lo = app.try_state::<crate::localorg::LocalOrg>().map(|s| s.inner().clone());
        let mut unread = Vec::new();
        let mut recent = Vec::new();
        for a in storage.list_accounts().unwrap_or_default() {
            let muted = lo.as_ref().map_or(0, |l| l.muted_unseen(&storage, &a.email));
            let cache = storage.unseen_count(&a.email, crate::badge::INBOX).unwrap_or(0).saturating_sub(muted);
            unread.push(crate::badge::unread_with_offset(cache, badge.offset(&a.email)));
            let muted_uids = lo.as_ref().map(|l| l.muted_unseen_uids(&storage, &a.email)).unwrap_or_default();
            let rows = storage.recent_unseen(&a.email, crate::badge::INBOX, 20).unwrap_or_default();
            for m in rows.iter().filter(|m| !muted_uids.contains(&m.uid)).take(MAX_LATEST) {
                recent.push(RecentIn { from: crate::badge::display_name(&m.from), subject: m.subject.clone(), date: m.date });
            }
        }
        let today = Local::now().date_naive();
        let mid = |d: NaiveDate| Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default()).earliest().map_or(now, |t| t.timestamp());
        let window = (mid(today), mid(today + ChronoDuration::days(2)));
        let mut events = Vec::new();
        if let Some(cal) = app.try_state::<crate::cal_state::CalState>() {
            let wire = |ts: i64| chrono::DateTime::from_timestamp(ts - 86_400, 0).unwrap_or_default().to_rfc3339();
            let to = chrono::DateTime::from_timestamp(window.1 + 86_400, 0).unwrap_or_default().to_rfc3339();
            if let Ok(occs) = cal.db(|c| c.list_occurrences(&wire(window.0), &to, true)) {
                let mut tzids: std::collections::HashMap<(String, String), Option<String>> = std::collections::HashMap::new();
                for o in occs.iter().filter(|o| !o.all_day) {
                    tzids.entry((o.uid.clone(), o.calendar_id.clone())).or_insert_with(|| {
                        cal.db(|c| c.get_event(&o.uid, &o.calendar_id)).ok().flatten().and_then(|e| e.tzid)
                    });
                }
                for o in &occs {
                    let tzid = tzids.get(&(o.uid.clone(), o.calendar_id.clone())).cloned().flatten();
                    if let Some(r) = raw_event(o, tzid.as_deref(), &Local) {
                        events.push(r);
                    }
                }
            }
        }
        Input { unread, recent, events, now, window, details }
    }

    /// Build, write and (throttled) reload. Blocking; call from a worker thread.
    pub fn refresh_blocking(app: &AppHandle, force: bool) {
        // Not managed yet (very early start, background launch): nothing to write.
        let (Some(ws), Some(badge), Some(st)) = (app.try_state::<WidgetState>(), app.try_state::<crate::badge::BadgeState>(), app.try_state::<crate::state::AppState>()) else { return };
        let _one_at_a_time = REFRESH.lock().unwrap_or_else(|e| e.into_inner());
        let Some(dir) = group_dir(app) else { return }; // App Group not provisioned: widgets simply show "Open MiraMail to sync"
        let details = ws.get().show_details;
        let now = chrono::Utc::now().timestamp();
        let input = gather(app, &st.storage, &badge, details, now);
        if input.unread.is_empty() {
            // No account (last one removed): nothing of the user's may stay in the shared container.
            let _ = std::fs::remove_file(dir.join(FILE));
            tauri_plugin_mailcheck::reload_widgets(app);
            return;
        }
        let snap = build(&input);
        if write_atomic(&dir, &snap).is_err() {
            // An older file may hold details the user just switched off: better none than stale.
            let _ = std::fs::remove_file(dir.join(FILE));
            return;
        }
        let due = GATE.lock().unwrap_or_else(|e| e.into_inner()).due(now, signature(&snap));
        if due || force {
            tauri_plugin_mailcheck::reload_widgets(app);
        }
    }

    /// Coalesced: many triggers within 2 s make one refresh.
    pub fn request(app: &AppHandle, force: bool) {
        if force {
            FORCE.store(true, Ordering::SeqCst);
        }
        if PENDING.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            PENDING.store(false, Ordering::SeqCst);
            let force = FORCE.swap(false, Ordering::SeqCst);
            let _ = tauri::async_runtime::spawn_blocking(move || refresh_blocking(&app, force)).await;
        });
    }
}

/// Ask for a snapshot refresh soon (coalesced). No-op off iOS.
pub fn request(app: &tauri::AppHandle, force: bool) {
    #[cfg(target_os = "ios")]
    ios::request(app, force);
    #[cfg(not(target_os = "ios"))]
    let _ = (app, force);
}

/// Refresh now and wait for it (always reloads when `force`). No-op off iOS.
pub fn refresh_sync(app: &tauri::AppHandle, force: bool) {
    #[cfg(target_os = "ios")]
    ios::refresh_blocking(app, force);
    #[cfg(not(target_os = "ios"))]
    let _ = (app, force);
}

/// Refresh now, on the calling (worker) thread: for the background check, which must finish its work before iOS
/// suspends the process. No-op off iOS.
pub fn refresh_blocking(app: &tauri::AppHandle) {
    #[cfg(target_os = "ios")]
    ios::refresh_blocking(app, false);
    #[cfg(not(target_os = "ios"))]
    let _ = app;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn rin(from: &str, subject: &str, date: i64) -> RecentIn {
        RecentIn { from: from.into(), subject: subject.into(), date }
    }
    fn ev(title: &str, start: i64, end: i64) -> RawEvent {
        RawEvent { title: title.into(), start, end, all_day: false, colour: "#112233".into() }
    }
    fn input(details: bool) -> Input {
        Input {
            unread: vec![3, 0],
            recent: vec![rin("Ann Lee", "Lunch?", 100), rin("bob@example.org", "Invoice", 300), rin("Cy", "Hi", 200)],
            events: vec![ev("Dentist", 1_000, 2_000), ev("Standup", 500, 800)],
            now: 400,
            window: (0, 10_000),
            details,
        }
    }

    #[test]
    fn privacy_off_carries_no_names_subjects_titles_or_addresses() {
        let s = build(&input(false));
        assert!(!s.details);
        assert_eq!(s.accounts.iter().map(|a| a.unread).collect::<Vec<_>>(), vec![3, 0]);
        assert_eq!(s.accounts[0].label, "Account 1");
        assert_eq!(s.latest.len(), 3);
        assert!(s.latest.iter().all(|l| l.from_name.is_none() && l.subject.is_none()));
        assert_eq!(s.latest.iter().map(|l| l.received_at).collect::<Vec<_>>(), vec![300, 200, 100], "newest first");
        assert!(s.calendar.iter().all(|e| e.title.as_deref() == Some(BUSY)));
        let json = serde_json::to_string(&s).unwrap();
        for secret in ["Ann", "Lunch", "bob", "example.org", "Invoice", "Dentist", "Standup", "@"] {
            assert!(!json.contains(secret), "{secret} leaked");
        }
    }

    #[test]
    fn privacy_on_adds_display_names_subjects_and_titles_but_never_an_address() {
        let s = build(&input(true));
        assert!(s.details);
        assert_eq!(s.latest[0].from_name, None, "a bare address is not a name");
        assert_eq!(s.latest[0].subject.as_deref(), Some("Invoice"));
        assert_eq!(s.latest[1].from_name.as_deref(), Some("Cy"));
        assert_eq!(s.calendar[0].title.as_deref(), Some("Standup"), "sorted by start");
        assert!(!serde_json::to_string(&s).unwrap().contains('@'));
    }

    #[test]
    fn limits_and_window_apply() {
        let mut i = input(true);
        i.recent = (0..20).map(|n| rin("A", "s", n)).collect();
        i.events = (0..50).map(|n| ev("e", n * 10 + 1, n * 10 + 5)).collect();
        i.events.push(ev("past", -100, -50));
        i.events.push(ev("far", 20_000, 21_000));
        i.events.push(ev("spans", -10, 5));
        let s = build(&i);
        assert_eq!(s.latest.len(), MAX_LATEST);
        assert_eq!(s.latest[0].received_at, 19);
        assert_eq!(s.calendar.len(), MAX_EVENTS);
        assert!(s.calendar.iter().all(|e| e.title.as_deref() != Some("past") && e.title.as_deref() != Some("far")));
        assert_eq!(s.calendar[0].start, -10, "an event already running at the window start is kept");
    }

    #[test]
    fn text_is_cleaned_clipped_and_bad_colours_replaced() {
        let mut i = input(true);
        i.recent = vec![rin("Ann\u{202e}Lee", &"x".repeat(500), 1)];
        i.events = vec![RawEvent { colour: "red; x".into(), ..ev(&"y".repeat(500), 1, 2) }];
        let s = build(&i);
        assert_eq!(s.latest[0].from_name.as_deref(), Some("AnnLee"));
        assert_eq!(s.latest[0].subject.as_ref().unwrap().chars().count(), MAX_TEXT);
        assert_eq!(s.calendar[0].title.as_ref().unwrap().chars().count(), MAX_TEXT);
        assert_eq!(s.calendar[0].colour, DEFAULT_COLOUR);
    }

    #[test]
    fn atomic_write_leaves_only_the_final_file_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let s = build(&input(false));
        let p = write_atomic(dir.path(), &s).unwrap();
        assert_eq!(p, dir.path().join(FILE));
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "no temp file left");
        // unique temp names: repeated and failing writes never collide or leave a temp file
        write_atomic(dir.path(), &s).unwrap();
        assert!(write_atomic(&dir.path().join("missing"), &s).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let back: Snapshot = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(back, s);
        // an older file is replaced whole
        let mut s2 = s.clone();
        s2.accounts.clear();
        write_atomic(dir.path(), &s2).unwrap();
        assert!(serde_json::from_slice::<Snapshot>(&std::fs::read(&p).unwrap()).unwrap().accounts.is_empty());
    }

    #[test]
    fn json_keys_match_the_documented_schema() {
        let v: serde_json::Value = serde_json::to_value(build(&input(true))).unwrap();
        for k in ["version", "updated_at", "details", "accounts", "latest", "calendar"] {
            assert!(v.get(k).is_some(), "{k}");
        }
        assert!(v["latest"][0].get("from_name").is_some() && v["latest"][0].get("received_at").is_some());
        assert!(v["calendar"][0].get("all_day").is_some() && v["calendar"][0].get("colour").is_some());
        assert_eq!(v["version"], 1);
    }

    #[test]
    fn reload_gate_throttles_unless_content_changed() {
        let mut g = Gate::default();
        assert!(g.due(1_000, 1), "first time");
        assert!(!g.due(1_100, 1), "same content within 5 min");
        assert!(!g.due(1_030, 2), "changed, but under a minute since the last reload");
        assert!(g.due(1_060, 2), "content changed and a minute passed");
        assert!(!g.due(1_200, 2));
        assert!(g.due(1_060 + RELOAD_EVERY_SECS, 2), "5 minutes passed");
        assert!(g.due(10, 2), "clock went backwards");
    }

    #[test]
    fn signature_ignores_updated_at_only() {
        let a = build(&input(false));
        let mut b = a.clone();
        b.updated_at += 99;
        assert_eq!(signature(&a), signature(&b));
        b.accounts[0].unread += 1;
        assert_ne!(signature(&a), signature(&b));
    }

    fn occ(start: &str, end: &str, all_day: bool) -> Occurrence {
        Occurrence {
            uid: "u".into(),
            calendar_id: "c".into(),
            recurrence_start: start.into(),
            start: start.into(),
            end: end.into(),
            all_day,
            summary: "S".into(),
            location: String::new(),
            color: "#abcdef".into(),
            has_zoom: false,
            is_recurring: false,
        }
    }

    #[test]
    fn occurrences_resolve_to_epochs() {
        let tz = FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
        let e = raw_event(&occ("2026-11-06T15:00:00+05:30", "2026-11-06T16:00:00+05:30", false), Some("Asia/Kolkata"), &tz).unwrap();
        assert_eq!((e.start, e.end - e.start), (1_793_957_400, 3600));
        // floating: the digits are wall-clock in the given zone, whatever offset the wire carries
        let f = raw_event(&occ("2026-11-06T15:00:00+00:00", "2026-11-06T15:45:00+00:00", false), None, &tz).unwrap();
        assert_eq!((f.start, f.end - f.start), (1_793_957_400, 2700));
        // no usable end: 30 minutes
        let n = raw_event(&occ("2026-11-06T15:00:00+05:30", "2026-11-06T15:00:00+05:30", false), Some("x"), &tz).unwrap();
        assert_eq!(n.end - n.start, 1800);
        // all-day: local midnight, exclusive end, a missing end is one day
        let d = raw_event(&occ("2026-11-06", "2026-11-08", true), None, &tz).unwrap();
        assert_eq!(d.end - d.start, 2 * 86_400);
        assert_eq!(d.start, 1_793_957_400 - 15 * 3600);
        let one = raw_event(&occ("2026-11-06", "2026-11-06", true), None, &tz).unwrap();
        assert_eq!(one.end - one.start, 86_400);
        assert!(raw_event(&occ("garbage", "garbage", false), None, &tz).is_none());
    }

    #[test]
    fn deep_links_select_a_tab() {
        let p = |s: &str| parse_deep_link(&tauri::Url::parse(s).unwrap());
        assert_eq!(p("miramail://mail"), Some("mail"));
        assert_eq!(p("miramail://calendar"), Some("calendar"));
        assert_eq!(p("MiraMail://Calendar/"), Some("calendar"));
        assert_eq!(p("miramail://evil"), None);
        assert_eq!(p("https://mail"), None);
        assert_eq!(p("mailto:a@b.org"), None);
    }

    #[test]
    fn prefs_default_off_and_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PREFS_FILE);
        assert!(!load_prefs(&path).show_details, "default off");
        std::fs::write(&path, b"{ nope").unwrap();
        assert!(!load_prefs(&path).show_details, "corrupt -> off");
        let s = WidgetState::default();
        s.init(path.clone());
        s.set(WidgetPrefs { show_details: true }).unwrap();
        let s2 = WidgetState::default();
        s2.init(path);
        assert!(s2.get().show_details);
    }
}
