//! Inbox count: ONE source of truth for the Dock badge, the menu-bar item, the mini widget and notifications.
//!
//! Counts come from the local message cache (the INBOX watcher + background sync already keep it fresh), so this
//! opens NO extra IMAP connections. The exact server unseen count, when the UI fetches it (`unread_counts`),
//! only contributes an "older unread beyond the cached window" offset. A cheap poll of the cache (every
//! `POLL`) notices read / move / delete / new mail wherever it happens; changes are emitted as
//! `mail://badge` and pushed to the Dock, tray and notification code.
//! Subjects never leave this process, and are stripped from the summary unless "show previews" is on.

use crate::notify;
use crate::plat_prefs::{PlatformPrefs, PlatformState, TrayStyle};
use mailcore::{MessageSummary, Storage};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const INBOX: &str = "INBOX";
pub const POLL: Duration = Duration::from_millis(1500);
/// Newest unread rows read per account for the "recent senders" list and new-mail detection.
const RECENT_PER_ACCOUNT: u32 = 20;
const RECENT_SHOWN: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recent {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    /// Display name of the sender (or the address when there is no name).
    pub from: String,
    /// Present only when the user enabled previews (the UI-facing summary strips it otherwise).
    pub subject: Option<String>,
    pub date: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InboxSummary {
    pub total: u32,
    pub per_account: BTreeMap<String, u32>,
    /// Account order as stored (for stable menus).
    pub accounts: Vec<String>,
    /// Cached messages in the INBOXes (for the "total" menu-bar style).
    pub inbox_total: u32,
    pub recent: Vec<Recent>,
    /// The widget's account filter (`None` = all), so the widget needs no second call.
    pub widget_account: Option<String>,
    pub previews: bool,
}

// ---------------------------------------------------------------- pure logic

/// Display part of a From header: `"Ann Lee" <a@x>` -> `Ann Lee`; bare address stays.
pub fn display_name(from: &str) -> String {
    let f = from.trim();
    let name = match f.find('<') {
        Some(i) => f[..i].trim().trim_matches('"').trim(),
        None => "",
    };
    if !name.is_empty() {
        return crate::textsafe::clean_display(name);
    }
    let addr = f.trim_start_matches(['<', '"']).trim_end_matches(['>', '"']);
    crate::textsafe::clean_display(addr)
}

/// Cache count + the "older unread" offset from the last exact server count.
pub fn unread_with_offset(cache: u32, offset: u32) -> u32 {
    cache.saturating_add(offset)
}

/// Unread messages the server has beyond what the cache shows (never negative).
pub fn offset_from_exact(exact: u32, cache: u32) -> u32 {
    exact.saturating_sub(cache)
}

/// Total and per-account map from the per-account unread list.
pub fn combine(per: &[(String, u32)]) -> (u32, BTreeMap<String, u32>) {
    let mut total = 0u32;
    let mut m = BTreeMap::new();
    for (a, n) in per {
        total = total.saturating_add(*n);
        m.insert(a.clone(), *n);
    }
    (total, m)
}

/// The text next to the menu-bar glyph. `None` = no title.
pub fn tray_title(style: TrayStyle, unread: u32, inbox_total: u32) -> Option<String> {
    let n = match style {
        TrayStyle::Unread => unread,
        TrayStyle::HideZero if unread == 0 => return None,
        TrayStyle::HideZero => unread,
        TrayStyle::Total => inbox_total,
    };
    Some(if n > 999 { "999+".to_string() } else { n.to_string() })
}

/// Dock badge value: `None` clears it.
pub fn dock_count(enabled: bool, unread: u32) -> Option<i64> {
    (enabled && unread > 0).then_some(i64::from(unread))
}

/// Unread messages newer than the last-seen UID. First sight of an account (`baseline == None`) only records the
/// baseline, so a launch never announces mail that was already there. Returns (new, new baseline).
pub fn new_arrivals(baseline: Option<u32>, unseen: &[Recent]) -> (Vec<Recent>, Option<u32>) {
    let max = unseen.iter().map(|r| r.uid).max();
    match baseline {
        None => (Vec::new(), max.or(Some(0))),
        Some(b) => {
            let mut new: Vec<Recent> = unseen.iter().filter(|r| r.uid > b).cloned().collect();
            new.sort_by(|a, b| b.date.cmp(&a.date).then(b.uid.cmp(&a.uid)));
            (new, Some(max.map_or(b, |m| m.max(b))))
        }
    }
}

/// True when the cached INBOX no longer reaches the baseline (UIDVALIDITY reset: UIDs restarted lower, or the
/// cache was wiped). The baseline is then meaningless and must be re-recorded silently.
pub fn baseline_stale(baseline: u32, cache_max_uid: Option<u32>) -> bool {
    cache_max_uid.is_none_or(|m| m < baseline)
}

/// Remove subjects unless previews are on.
pub fn strip_subjects(recent: &[Recent], previews: bool) -> Vec<Recent> {
    recent.iter().cloned().map(|mut r| {
        if !previews {
            r.subject = None;
        }
        r
    }).collect()
}

/// Pure assembly of the summary from per-account cache numbers.
pub fn build_summary(
    accounts: &[String],
    cache_unread: &HashMap<String, u32>,
    offsets: &HashMap<String, u32>,
    inbox_total: u32,
    recent_all: &[Recent],
    prefs: &PlatformPrefs,
) -> InboxSummary {
    let per: Vec<(String, u32)> = accounts
        .iter()
        .map(|a| (a.clone(), unread_with_offset(cache_unread.get(a).copied().unwrap_or(0), offsets.get(a).copied().unwrap_or(0))))
        .collect();
    let (total, per_account) = combine(&per);
    // The widget's account filter applies before the cut to three, so a single-account widget still shows three.
    let mut recent: Vec<Recent> = recent_all.iter().filter(|r| prefs.widget_account.as_ref().is_none_or(|a| a == &r.account)).cloned().collect();
    recent.sort_by(|a, b| b.date.cmp(&a.date).then(b.uid.cmp(&a.uid)));
    recent.truncate(RECENT_SHOWN);
    InboxSummary {
        total,
        per_account,
        accounts: accounts.to_vec(),
        inbox_total,
        recent: strip_subjects(&recent, prefs.widget_previews),
        widget_account: prefs.widget_account.clone(),
        previews: prefs.widget_previews,
    }
}

// ---------------------------------------------------------------- state + poller

#[derive(Default)]
struct Inner {
    offsets: HashMap<String, u32>,
    baseline: HashMap<String, u32>,
    last: Option<InboxSummary>,
    /// The most recent notification (when, which message), for "click focuses the message".
    last_notified: Option<(std::time::Instant, Recent)>,
}

#[derive(Default)]
pub struct BadgeState(Mutex<Inner>);

impl BadgeState {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn summary(&self) -> InboxSummary {
        self.lock().last.clone().unwrap_or_default()
    }
    pub fn note_notified(&self, r: Recent) {
        self.lock().last_notified = Some((std::time::Instant::now(), r));
    }
    pub fn clear_notified(&self) {
        self.lock().last_notified = None;
    }
    /// The message of the last notification if it was shown within `within` (taken: used once).
    pub fn take_recent_notified(&self, within: Duration) -> Option<Recent> {
        let mut g = self.lock();
        match g.last_notified.take() {
            Some((at, r)) if at.elapsed() <= within => Some(r),
            _ => None,
        }
    }
}

/// Server-exact INBOX unseen count from the UI's `unread_counts`: keeps the "older unread" offset current.
pub fn record_exact(app: &AppHandle, storage: &Storage, account: &str, counts: &HashMap<String, u32>) {
    let exact = counts.iter().find(|(f, _)| f.eq_ignore_ascii_case(INBOX)).map(|(_, n)| *n);
    let (Some(exact), Ok(cache)) = (exact, storage.unseen_count(account, INBOX)) else { return };
    app.state::<BadgeState>().lock().offsets.insert(account.to_string(), offset_from_exact(exact, cache));
}

struct Snapshot {
    accounts: Vec<String>,
    cache_unread: HashMap<String, u32>,
    inbox_total: u32,
    recent: Vec<Recent>,
    unseen_by_account: HashMap<String, Vec<Recent>>,
    cache_max_uid: HashMap<String, Option<u32>>,
}

fn to_recent(m: &MessageSummary) -> Recent {
    Recent {
        account: m.account.clone(),
        folder: m.folder.clone(),
        uid: m.uid,
        from: display_name(&m.from),
        subject: Some(crate::textsafe::clean_display(&m.subject).chars().take(200).collect()),
        date: m.date,
    }
}

fn snapshot(storage: &Storage, lo: Option<&crate::localorg::LocalOrg>) -> Snapshot {
    let accounts: Vec<String> = storage.list_accounts().unwrap_or_default().into_iter().map(|a| a.email).collect();
    let (mut cache_unread, mut recent, mut unseen_by_account, mut inbox_total) = (HashMap::new(), Vec::new(), HashMap::new(), 0u32);
    let mut cache_max_uid = HashMap::new();
    for a in &accounts {
        // Muted conversations do not count as unread (local mute list; docs/ESSENTIALS_SPEC.md).
        let muted = lo.map_or(0, |l| l.muted_unseen(storage, a));
        cache_unread.insert(a.clone(), storage.unseen_count(a, INBOX).unwrap_or(0).saturating_sub(muted));
        inbox_total = inbox_total.saturating_add(storage.message_count(a, INBOX).unwrap_or(0));
        let rows: Vec<Recent> = storage.recent_unseen(a, INBOX, RECENT_PER_ACCOUNT).unwrap_or_default().iter().map(to_recent).collect();
        recent.extend(rows.iter().take(RECENT_SHOWN).cloned());
        unseen_by_account.insert(a.clone(), rows);
        cache_max_uid.insert(a.clone(), storage.max_uid(a, INBOX).ok().flatten());
    }
    Snapshot { accounts, cache_unread, inbox_total, recent, unseen_by_account, cache_max_uid }
}

/// One poll step. `force` re-emits and re-applies even if nothing changed (preferences changed).
pub async fn tick(app: &AppHandle, force: bool) {
    let storage = app.state::<crate::state::AppState>().storage.clone();
    let lo = app.try_state::<crate::localorg::LocalOrg>().map(|s| s.inner().clone());
    let Ok(snap) = tauri::async_runtime::spawn_blocking(move || snapshot(&storage, lo.as_ref())).await else { return };
    let prefs = app.state::<PlatformState>().get();
    let st = app.state::<BadgeState>();

    // New mail -> notifications (per account), decided under the lock, sent after it.
    let mut arrivals: Vec<Recent> = Vec::new();
    let summary = {
        let mut g = st.lock();
        g.baseline.retain(|a, _| snap.accounts.contains(a));
        g.offsets.retain(|a, _| snap.accounts.contains(a));
        for a in &snap.accounts {
            if let Some(b) = g.baseline.get(a).copied() {
                if baseline_stale(b, snap.cache_max_uid.get(a).copied().flatten()) {
                    g.baseline.remove(a);
                }
            }
            let rows = snap.unseen_by_account.get(a).map(Vec::as_slice).unwrap_or(&[]);
            let (new, base) = new_arrivals(g.baseline.get(a).copied(), rows);
            if let Some(b) = base {
                g.baseline.insert(a.clone(), b);
            }
            arrivals.extend(new);
        }
        let s = build_summary(&snap.accounts, &snap.cache_unread, &g.offsets, snap.inbox_total, &snap.recent, &prefs);
        if !force && g.last.as_ref() == Some(&s) {
            None
        } else {
            g.last = Some(s.clone());
            Some(s)
        }
    };
    if let Some(s) = summary {
        let _ = app.emit("mail://badge", &s);
        apply(app, &s, &prefs);
    }
    if !arrivals.is_empty() {
        // Blocked / muted / VIP-only filtering first (blocked mail is moved to Junk in the background).
        let arrivals = crate::localorg::on_arrivals(app, &prefs, arrivals);
        notify::on_arrivals(app, &prefs, &snap.accounts, arrivals);
    }
}

/// Push a summary to the Dock and the menu-bar item (main thread; AppKit is not thread safe).
pub fn apply(app: &AppHandle, s: &InboxSummary, prefs: &PlatformPrefs) {
    let (app2, s2, p2) = (app.clone(), s.clone(), prefs.clone());
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = app2.get_webview_window("main") {
            let _ = w.set_badge_count(dock_count(p2.dock_badge, s2.total));
        }
        crate::tray::sync(&app2, &s2, &p2);
    });
}

/// Re-apply everything now (after a preferences change).
pub fn refresh_now(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { tick(&app, true).await });
}

pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tick(&app, false).await;
            tokio::time::sleep(POLL).await;
        }
    });
}

// ---------------------------------------------------------------- commands

/// Read-only. The mini widget window is allowed to call exactly this (plus `focus_main`, `widget_menu`).
#[tauri::command]
pub fn get_inbox_summary(state: tauri::State<'_, BadgeState>) -> InboxSummary {
    state.summary()
}

#[tauri::command]
pub fn get_platform_prefs(state: tauri::State<'_, PlatformState>) -> PlatformPrefs {
    state.get()
}

#[tauri::command]
pub fn set_platform_prefs(app: AppHandle, state: tauri::State<'_, PlatformState>, prefs: PlatformPrefs) -> Result<PlatformPrefs, String> {
    let turned_on = prefs.notify && !state.get().notify;
    let p = state.set(prefs)?;
    if turned_on {
        // The user just opted in: this is the one place macOS's permission prompt is triggered (besides the test button).
        crate::native::notifications::request_permission();
    }
    crate::widget::sync(&app);
    refresh_now(&app);
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(account: &str, uid: u32, date: i64, subject: &str) -> Recent {
        Recent { account: account.into(), folder: INBOX.into(), uid, from: "A".into(), subject: Some(subject.into()), date }
    }

    #[test]
    fn baseline_resets_when_uids_restart_lower() {
        assert!(baseline_stale(500, Some(3)), "UIDVALIDITY reset");
        assert!(baseline_stale(500, None), "cache wiped");
        assert!(!baseline_stale(500, Some(500)));
        assert!(!baseline_stale(500, Some(900)));
        // After the reset the next tick records silently, then real arrivals notify.
        let (new, base) = new_arrivals(None, &[r("a", 2, 1, "x")]);
        assert!(new.is_empty() && base == Some(2));
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name("\"Ann Lee\" <ann@x.org>"), "Ann Lee");
        assert_eq!(display_name("Bob <bob@x.org>"), "Bob");
        assert_eq!(display_name("<c@x.org>"), "c@x.org");
        assert_eq!(display_name("d@x.org"), "d@x.org");
        assert_eq!(display_name("E\u{7}vil <e@x.org>"), "Evil");
    }

    #[test]
    fn exact_count_only_adds_the_older_unread() {
        assert_eq!(offset_from_exact(120, 30), 90);
        assert_eq!(offset_from_exact(5, 30), 0, "never negative");
        assert_eq!(unread_with_offset(30, 90), 120);
        // reading a cached message lowers the total by one, the older ones stay
        assert_eq!(unread_with_offset(29, 90), 119);
        assert_eq!(unread_with_offset(u32::MAX, 5), u32::MAX);
    }

    #[test]
    fn aggregation_per_account_and_total() {
        let prefs = PlatformPrefs::default();
        let accounts = vec!["a@x.org".to_string(), "b@x.org".to_string(), "c@x.org".to_string()];
        let cache = HashMap::from([("a@x.org".to_string(), 3), ("b@x.org".to_string(), 0)]);
        let offsets = HashMap::from([("a@x.org".to_string(), 2), ("gone@x.org".to_string(), 99)]);
        let s = build_summary(&accounts, &cache, &offsets, 42, &[], &prefs);
        assert_eq!(s.total, 5);
        assert_eq!(s.per_account.get("a@x.org"), Some(&5));
        assert_eq!(s.per_account.get("b@x.org"), Some(&0));
        assert_eq!(s.per_account.get("c@x.org"), Some(&0), "account with no cache yet counts 0");
        assert!(!s.per_account.contains_key("gone@x.org"), "removed account's offset is ignored");
        assert_eq!(s.accounts, accounts);
        assert_eq!(s.inbox_total, 42);
        assert_eq!(build_summary(&[], &HashMap::new(), &HashMap::new(), 0, &[], &prefs), InboxSummary { previews: false, ..Default::default() });
    }

    #[test]
    fn widget_account_filters_recent_before_the_cut() {
        let rows = vec![r("a", 1, 10, "s1"), r("b", 2, 40, "s2"), r("b", 3, 30, "s3"), r("b", 4, 20, "s4"), r("b", 5, 5, "s5")];
        let prefs = PlatformPrefs { widget_account: Some("a".into()), ..Default::default() };
        let s = build_summary(&["a".into(), "b".into()], &HashMap::new(), &HashMap::new(), 0, &rows, &prefs);
        assert_eq!(s.recent.iter().map(|x| x.uid).collect::<Vec<_>>(), vec![1]);
        assert_eq!(s.widget_account.as_deref(), Some("a"));
    }

    #[test]
    fn recent_is_newest_three_and_subjects_need_previews() {
        let rows = vec![r("a", 1, 10, "s1"), r("a", 2, 40, "s2"), r("b", 3, 30, "s3"), r("b", 4, 20, "s4")];
        let mut prefs = PlatformPrefs::default();
        let s = build_summary(&["a".into(), "b".into()], &HashMap::new(), &HashMap::new(), 0, &rows, &prefs);
        assert_eq!(s.recent.iter().map(|x| x.uid).collect::<Vec<_>>(), vec![2, 3, 4]);
        assert!(s.recent.iter().all(|x| x.subject.is_none()), "subjects stripped by default");
        prefs.widget_previews = true;
        let s = build_summary(&["a".into(), "b".into()], &HashMap::new(), &HashMap::new(), 0, &rows, &prefs);
        assert_eq!(s.recent[0].subject.as_deref(), Some("s2"));
        let json = serde_json::to_value(&s).unwrap();
        assert!(json.get("perAccount").is_some() && json.get("total").is_some() && json.get("widgetAccount").is_some());
    }

    #[test]
    fn tray_titles() {
        assert_eq!(tray_title(TrayStyle::Unread, 0, 9), Some("0".into()));
        assert_eq!(tray_title(TrayStyle::Unread, 7, 9), Some("7".into()));
        assert_eq!(tray_title(TrayStyle::HideZero, 0, 9), None);
        assert_eq!(tray_title(TrayStyle::HideZero, 3, 9), Some("3".into()));
        assert_eq!(tray_title(TrayStyle::Total, 3, 9), Some("9".into()));
        assert_eq!(tray_title(TrayStyle::Unread, 12_345, 0), Some("999+".into()));
    }

    #[test]
    fn dock_count_clears_at_zero_or_when_off() {
        assert_eq!(dock_count(true, 4), Some(4));
        assert_eq!(dock_count(true, 0), None);
        assert_eq!(dock_count(false, 4), None);
    }

    #[test]
    fn first_sight_records_a_baseline_without_announcing() {
        let rows = vec![r("a", 5, 50, "x"), r("a", 9, 90, "y")];
        let (new, base) = new_arrivals(None, &rows);
        assert!(new.is_empty());
        assert_eq!(base, Some(9));
        assert_eq!(new_arrivals(None, &[]).1, Some(0));
    }

    #[test]
    fn only_unread_above_the_baseline_are_new_and_the_baseline_never_goes_back() {
        let rows = vec![r("a", 5, 50, "x"), r("a", 9, 90, "y"), r("a", 10, 95, "z")];
        let (new, base) = new_arrivals(Some(9), &rows);
        assert_eq!(new.iter().map(|x| x.uid).collect::<Vec<_>>(), vec![10]);
        assert_eq!(base, Some(10));
        // the newest message was read/deleted: nothing new, baseline kept so it is not re-announced later
        let (new, base) = new_arrivals(Some(10), &rows[..1]);
        assert!(new.is_empty());
        assert_eq!(base, Some(10));
        let (new, base) = new_arrivals(Some(10), &[]);
        assert!(new.is_empty() && base == Some(10));
    }

    #[test]
    fn notified_message_is_taken_once_and_expires() {
        let s = BadgeState::default();
        assert!(s.take_recent_notified(Duration::from_secs(5)).is_none());
        s.note_notified(r("a", 1, 1, "x"));
        assert!(s.take_recent_notified(Duration::from_secs(5)).is_some());
        assert!(s.take_recent_notified(Duration::from_secs(5)).is_none(), "used once");
        s.note_notified(r("a", 1, 1, "x"));
        std::thread::sleep(Duration::from_millis(5));
        assert!(s.take_recent_notified(Duration::ZERO).is_none(), "expired");
    }
}
