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
    if !prefs.notify {
        return Vec::new();
    }
    let wanted: Vec<&Recent> = arrivals.iter().filter(|r| !prefs.notify_off.contains(&r.account)).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    if wanted.len() > MAX_INDIVIDUAL {
        let mut accts: Vec<&str> = wanted.iter().map(|r| r.account.as_str()).collect();
        accts.sort_unstable();
        accts.dedup();
        return vec![Spec {
            title: format!("{} new messages", wanted.len()),
            body: if account_count > 1 { clip(&accts.join(", ")) } else { String::new() },
            target: None,
        }];
    }
    wanted
        .into_iter()
        .map(|r| {
            let acct = if account_count > 1 { clip(&r.account) } else { String::new() };
            if prefs.notify_preview {
                let subject = r.subject.as_deref().map(clip).filter(|s| !s.is_empty()).unwrap_or_else(|| "(no subject)".to_string());
                Spec {
                    title: clip(&r.from),
                    body: if acct.is_empty() { subject } else { format!("{subject}\n{acct}") },
                    target: Some(r.clone()),
                }
            } else {
                Spec { title: "New mail".to_string(), body: acct, target: Some(r.clone()) }
            }
        })
        .collect()
}

fn app_is_focused(app: &AppHandle) -> bool {
    app.webview_windows().values().any(|w| w.is_focused().unwrap_or(false))
}

/// Post one local notification. Errors (no bundle, permission denied) are ignored by callers: the system drops it.
pub fn deliver(title: &str, body: &str, sound: bool) -> Result<(), String> {
    crate::native::notifications::deliver(title, body, sound)
}

fn show(spec: &Spec, sound: bool) -> Result<(), String> {
    deliver(&spec.title, &spec.body, sound)
}

/// New unread INBOX mail was detected. Suppressed while a MiraMail window is focused (the user is looking at it).
pub fn on_arrivals(app: &AppHandle, prefs: &PlatformPrefs, accounts: &[String], arrivals: Vec<Recent>) {
    if !prefs.notify || app_is_focused(app) {
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
        return Err("Notifications are turned off for MiraMail. Allow them in System Settings > Notifications.".into());
    }
    let spec = Spec { title: "MiraMail".into(), body: "Notifications are working.".into(), target: None };
    show(&spec, prefs.notify_sound)
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
    fn text_is_clipped_and_control_characters_removed() {
        let s = plan(&[r("a", 1, "An\u{7}n", &"x".repeat(500))], &on(), 1);
        assert_eq!(s[0].title, "Ann");
        assert_eq!(s[0].body.chars().count(), MAX_TEXT + 1);
        let s = plan(&[r("a", 1, "Bob\u{202E}evil", "s\u{200B}ubj")], &on(), 1);
        assert_eq!((s[0].title.as_str(), s[0].body.as_str()), ("Bobevil", "subj"));
    }
}
