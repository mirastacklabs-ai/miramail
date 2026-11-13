//! Native macOS notifications for new mail. Local only (the system notification center; no network, no push
//! service). Decisions are pure (`plan`) and unit-tested; `on_arrivals` is the thin glue.
//!
//! Delivery is `native::notifications` (UNUserNotificationCenter, public API only).
//! Click behaviour: when MiraMail becomes the focused app within `CLICK_WINDOW` of a notification we open the
//! message it announced (see `winstate`).

use crate::badge::{BadgeState, Recent};
use crate::plat_prefs::PlatformPrefs;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const CLICK_WINDOW: Duration = Duration::from_secs(8);
/// More new messages than this in one poll collapse into a single "N new messages" notification.
const MAX_INDIVIDUAL: usize = 3;
/// The iPhone background check collapses above this (it can find more at once than a live poll).
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub const MAX_INDIVIDUAL_BG: usize = 5;
const MAX_TEXT: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub title: String,
    pub body: String,
    /// The message to open when the user follows the notification (None for a summary of several).
    pub target: Option<Recent>,
}

fn clip(s: &str) -> String {
    let t = crate::textsafe::clean_display(s);
    if t.chars().count() > MAX_TEXT {
        let mut c: String = t.chars().take(MAX_TEXT).collect();
        c.push('…');
        c
    } else {
        t
    }
}

/// What to show for `arrivals`, honouring per-account switches and the preview setting. Previews off: no sender,
/// no subject (only "New mail" + the account), so nothing readable sits on the lock screen.
pub fn plan(arrivals: &[Recent], prefs: &PlatformPrefs, account_count: usize) -> Vec<Spec> {
    plan_with(arrivals, prefs, account_count, MAX_INDIVIDUAL)
}

/// [`plan`] with a chosen limit for how many individual notifications are shown before they collapse into a summary.
/// One decision for the Mac live poll and the iPhone background check; only the limit differs.
pub fn plan_with(arrivals: &[Recent], prefs: &PlatformPrefs, account_count: usize, max_individual: usize) -> Vec<Spec> {
    if !prefs.notify {
        return Vec::new();
    }
    let wanted: Vec<&Recent> = arrivals.iter().filter(|r| !prefs.notify_off.contains(&r.account)).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    if wanted.len() > max_individual {
        let mut accts: Vec<&str> = wanted.iter().map(|r| r.account.as_str()).collect();
        accts.sort_unstable();
        accts.dedup();
        return vec![Spec {
            title: format!("{} new messages", wanted.len()),
            body: if account_count > 1 && (prefs.notify_preview || !cfg!(target_os = "ios")) { clip(&accts.join(", ")) } else { String::new() },
            target: None,
        }];
    }
    wanted
        .into_iter()
        .map(|r| {
            let acct = if account_count > 1 { clip(&r.account) } else { String::new() };
            // iPhone, previews off: no account address on the lock screen (the title already says "New mail").
            let acct_neutral = if cfg!(target_os = "ios") { String::new() } else { acct.clone() };
            if prefs.notify_preview {
                let subject = r.subject.as_deref().map(clip).filter(|s| !s.is_empty()).unwrap_or_else(|| "(no subject)".to_string());
                Spec {
                    title: clip(&r.from),
                    body: if acct.is_empty() { subject } else { format!("{subject}\n{acct}") },
                    target: Some(r.clone()),
                }
            } else {
                Spec { title: "New mail".to_string(), body: acct_neutral, target: Some(r.clone()) }
            }
        })
        .collect()
}

fn app_is_focused(app: &AppHandle) -> bool {
    app.webview_windows().values().any(|w| w.is_focused().unwrap_or(false))
}

/// Request identifier of a notification that announces one message ("mm:<uid>:<account>"): the iPhone tap handler
/// (plugins/mailcheck, `TapDelegate.parse`) reads it back to open that message. Only INBOX mail is announced.
pub fn target_id(r: &Recent) -> String {
    format!("mm:{}:{}", r.uid, r.account)
}

/// Post one local notification. Errors (no bundle, permission denied) are ignored by callers: the system drops it.
pub fn deliver(title: &str, body: &str, sound: bool) -> Result<(), String> {
    deliver_id(title, body, sound, None)
}

fn deliver_id(title: &str, body: &str, sound: bool, id: Option<&str>) -> Result<(), String> {
    crate::native::notifications::deliver(title, body, sound, id)
}

pub fn show(spec: &Spec, sound: bool) -> Result<(), String> {
    // The identifier only matters on iPhone (tap -> open that message); macOS keeps its anonymous ids.
    let id = if cfg!(target_os = "ios") { spec.target.as_ref().filter(|t| t.folder.eq_ignore_ascii_case("INBOX")).map(target_id) } else { None };
    deliver_id(&spec.title, &spec.body, sound, id.as_deref())
}

/// [`show`], waiting until the system has accepted the notification (iPhone background check).
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub async fn show_confirmed(spec: &Spec, sound: bool) -> Result<(), String> {
    let id = spec.target.as_ref().filter(|t| t.folder.eq_ignore_ascii_case("INBOX")).map(target_id);
    crate::native::notifications::deliver_confirmed(&spec.title, &spec.body, sound, id.as_deref()).await
}

/// New unread INBOX mail was detected. Suppressed while a MiraMail window is focused (the user is looking at it).
/// iPhone: never posts. The live poller runs only while the user is in front of the app (where iOS would not show it
/// either, see `TapDelegate.willPresent`), and mail that arrives while they are away is announced by the background
/// check (bgcheck.rs); this also keeps the permission prompt out of the poller.
pub fn on_arrivals(app: &AppHandle, prefs: &PlatformPrefs, accounts: &[String], arrivals: Vec<Recent>) {
    if cfg!(target_os = "ios") || !prefs.notify || app_is_focused(app) {
        return;
    }
    let specs = plan(&arrivals, prefs, accounts.len());
    if specs.is_empty() {
        return;
    }
    let (app, sound) = (app.clone(), prefs.notify_sound);
    // Ask for authorization first (a no-op once decided; the system prompt shows only when undecided, e.g. after a
    // backup restore turned notifications on), off the calling thread so the UI is never blocked.
    tauri::async_runtime::spawn(async move {
        if !matches!(crate::native::notifications::ensure_permission().await, Ok(true)) {
            return;
        }
        for spec in specs {
            if show(&spec, sound).is_ok() {
                // macOS only: "the app was focused soon after a notification" means the user clicked it. On iPhone a tap
                // reaches the app through the notification delegate instead (plugins/mailcheck).
                #[cfg(not(target_os = "ios"))]
                if let Some(t) = spec.target {
                    app.state::<BadgeState>().note_notified(t);
                }
            }
        }
    });
}

/// MiraMail just became the focused app: if it follows a notification, open that message.
pub fn on_focus_gained(app: &AppHandle) {
    if let Some(r) = app.state::<BadgeState>().take_recent_notified(CLICK_WINDOW) {
        // Previews off: only select the message; opening it would mark it read without the user having seen it.
        let select_only = !app.state::<crate::plat_prefs::PlatformState>().get().notify_preview;
        let _ = app.emit_to("main", "app://open-message", OpenMessage { account: r.account, folder: r.folder, uid: r.uid, select_only });
    }
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OpenMessage {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    /// Select the message without opening it (which would mark it read).
    pub select_only: bool,
}

/// Preferences "Send a test notification". Triggers macOS's one-time permission prompt; a refusal is reported.
#[tauri::command]
pub async fn notify_test(app: AppHandle) -> Result<(), String> {
    let prefs = app.state::<crate::plat_prefs::PlatformState>().get();
    if !crate::native::notifications::ensure_permission().await? {
        return Err(if cfg!(target_os = "ios") {
            "Notifications are turned off for MiraMail. Allow them in the Settings app > Notifications > MiraMail.".into()
        } else {
            "Notifications are turned off for MiraMail. Allow them in System Settings > Notifications.".into()
        });
    }
    // iPhone: the id lets the notification delegate show this one as a banner while the app is open.
    let id = if cfg!(target_os = "ios") { Some("miramail-test") } else { None };
    deliver_id("MiraMail", "Notifications are working.", prefs.notify_sound, id)
}

/// iPhone: the message a tapped notification announced (taken: returned once). The page asks for it at start-up and
/// whenever the native side signals a tap. Previews off: only select the message, so it is not marked read unseen.
/// Always `None` on a Mac (a click is detected by `on_focus_gained` there).
#[tauri::command]
pub async fn notification_tap(app: AppHandle) -> Option<OpenMessage> {
    #[cfg(target_os = "ios")]
    {
        let t = tauri_plugin_mailcheck::take_tap(&app)?;
        let st = app.state::<crate::state::AppState>();
        // Only INBOX mail of an account that still exists is announced; anything else is ignored.
        if !t.folder.eq_ignore_ascii_case("INBOX") || !st.storage.list_accounts().ok()?.iter().any(|a| a.email == t.account) {
            return None;
        }
        let select_only = !app.state::<crate::plat_prefs::PlatformState>().get().notify_preview;
        Some(OpenMessage { account: t.account, folder: t.folder, uid: t.uid, select_only })
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = app;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(account: &str, uid: u32, from: &str, subject: &str) -> Recent {
        Recent { account: account.into(), folder: "INBOX".into(), uid, from: from.into(), subject: Some(subject.into()), date: uid as i64 }
    }
    fn on() -> PlatformPrefs {
        PlatformPrefs { notify: true, ..Default::default() }
    }

    #[test]
    fn off_means_nothing() {
        assert!(plan(&[r("a", 1, "Ann", "Hi")], &PlatformPrefs::default(), 1).is_empty());
    }

    #[test]
    fn preview_on_shows_sender_and_subject_and_targets_the_message() {
        let s = plan(&[r("a@x.org", 7, "Ann", "Lunch?")], &on(), 1);
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].title.as_str(), s[0].body.as_str()), ("Ann", "Lunch?"));
        assert_eq!(s[0].target.as_ref().unwrap().uid, 7);
        let s = plan(&[r("a@x.org", 7, "Ann", "Lunch?")], &on(), 2);
        assert_eq!(s[0].body, "Lunch?\na@x.org", "account named when there are several");
    }

    #[test]
    fn preview_off_hides_sender_and_subject() {
        let p = PlatformPrefs { notify_preview: false, ..on() };
        let s = plan(&[r("a@x.org", 7, "Ann Secret", "Salary")], &p, 2);
        assert_eq!(s[0].title, "New mail");
        assert_eq!(s[0].body, "a@x.org");
        assert!(!s[0].title.contains("Ann") && !s[0].body.contains("Salary"));
    }

    #[test]
    fn per_account_switch() {
        let p = PlatformPrefs { notify_off: vec!["b@x.org".into()], ..on() };
        let s = plan(&[r("a@x.org", 1, "A", "1"), r("b@x.org", 2, "B", "2")], &p, 2);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].target.as_ref().unwrap().account, "a@x.org");
        assert!(plan(&[r("b@x.org", 2, "B", "2")], &p, 2).is_empty());
    }

    #[test]
    fn a_burst_collapses_into_one_summary() {
        let rows: Vec<Recent> = (1..=5).map(|i| r("a@x.org", i, "S", "x")).collect();
        let s = plan(&rows, &on(), 1);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].title, "5 new messages");
        assert!(s[0].target.is_none() && s[0].body.is_empty());
        let s = plan(&rows[..3], &on(), 1);
        assert_eq!(s.len(), 3, "three still show individually");
    }

    #[test]
    fn the_limit_before_a_summary_is_a_parameter() {
        let rows: Vec<Recent> = (1..=5).map(|i| r("a@x.org", i, "S", "x")).collect();
        assert_eq!(plan_with(&rows, &on(), 1, MAX_INDIVIDUAL_BG).len(), 5, "five show individually on the phone");
        assert_eq!(plan_with(&rows, &on(), 1, MAX_INDIVIDUAL).len(), 1, "the Mac default still collapses them");
        let six: Vec<Recent> = (1..=6).map(|i| r("a@x.org", i, "S", "x")).collect();
        let s = plan_with(&six, &on(), 1, MAX_INDIVIDUAL_BG);
        assert_eq!((s.len(), s[0].title.as_str()), (1, "6 new messages"));
        assert_eq!(plan(&rows[..3], &on(), 1), plan_with(&rows[..3], &on(), 1, 3), "plan() is plan_with(3)");
    }

    #[test]
    fn target_id_keeps_colons_of_the_account_and_never_carries_text() {
        let id = target_id(&r("a:b@x.org", 42, "Ann Secret", "Salary"));
        assert_eq!(id, "mm:42:a:b@x.org");
        assert!(!id.contains("Ann") && !id.contains("Salary"));
    }

    #[test]
    fn text_is_clipped_and_control_characters_removed() {
        let s = plan(&[r("a", 1, "An\u{7}n", &"x".repeat(500))], &on(), 1);
        assert_eq!(s[0].title, "Ann");
        assert_eq!(s[0].body.chars().count(), MAX_TEXT + 1);
        let s = plan(&[r("a", 1, "Bob\u{202E}evil", "s\u{200B}ubj")], &on(), 1);
        assert_eq!((s[0].title.as_str(), s[0].body.as_str()), ("Bobevil", "subj"));
    }
}
