//! iPhone owner rule: while the user is not in front of MiraMail (iOS launched it in the background for a refresh, or
//! woke the suspended app for one), the only work allowed is the read-and-store INBOX check in `bgcheck.rs` plus local
//! notifications and the badge. Nothing that changes mail or talks to the outside world runs then: rules / auto-reply,
//! Junk moves and auto-read, outbox sends, snooze returns, CalDAV, the Drafts mirror sweep, the cache backfill, IDLE
//! watchers and the folder sync loop, extra LIST / STATUS, MIRA / AI, unsubscribe. All of it runs, or catches up, when
//! the user opens the app.
//!
//! The state itself lives in `tauri-plugin-mailcheck` (Swift reports `didBecomeActive` / `didEnterBackground`). Off iOS
//! the user is always present, so none of the gates below ever changes macOS behaviour.

use serde::Serialize;

/// Error text of a command refused while the user is away. The page recognises the prefix and stays silent.
pub const DEFERRED: &str = "deferred: MiraMail is in the background";

/// True while the user is in front of the app. Always true off iOS.
pub fn user_present() -> bool {
    #[cfg(target_os = "ios")]
    {
        tauri_plugin_mailcheck::user_present()
    }
    #[cfg(not(target_os = "ios"))]
    {
        true
    }
}


/// `Err(DEFERRED)` unless the user is present.
pub fn check(present: bool) -> Result<(), String> {
    if present {
        Ok(())
    } else {
        Err(DEFERRED.to_string())
    }
}

/// [`check`] against the live state.
pub fn require() -> Result<(), String> {
    check(user_present())
}

/// Commands the page may call while the user is away (iPhone): reads of local data and local-only saves, so a page that
/// iOS loaded during a background launch can render. Everything else is refused with [`DEFERRED`] (an allow-list:
/// a command added later is held back until it is reviewed). `list_folders` is listed because it serves only the
/// cached list while the user is away (see `commands::list_folders`).
#[cfg_attr(not(mobile), allow(dead_code))] // used by the iPhone invoke guard (mobile.rs)
pub const ALLOWED_WHILE_AWAY: &[&str] = &[
    "launch_state",
    "list_accounts",
    "list_folders",
    "list_messages",
    "list_live_folders",
    "live_states",
    "get_sync_settings",
    "message_analysis",
    "safety_report",
    "search_mail",
    "list_colours",
    "sender_images_verified",
    "composer_data",
    "sanitize_compose",
    "suggest_addresses",
    "contacts_list",
    "contacts_groups",
    "contacts_suggest",
    "contacts_card",
    "contacts_expand_group",
    "save_draft",
    "list_drafts",
    "delete_draft",
    "save_layout",
    "load_layout",
    "outbox_list",
    "snooze_info",
    "snooze_list",
    "snooze_count",
    "get_pane",
    "save_pane",
    "list_windows",
    "window_at",
    "storage_usage",
    "cal_list_calendars",
    "cal_list_events",
    "cal_get_event",
    "ai_list_profiles",
    "ai_get_settings",
    "ai_list_conversations",
    "ai_get_conversation",
    "ai_egress",
    "ai_list_proposals",
    "ai_get_proposal",
    "ai_list_drafts",
    "ai_get_draft",
    "ai_rules_list",
    "ai_rule_card",
    "ai_automation_state",
    "ai_rule_runs",
    "ai_autoreply_get",
    "ai_autoreply_log",
    "get_inbox_summary",
    "get_platform_prefs",
    "take_pending_mailto",
    "notification_tap",
    "thread_map",
    "thread_members",
    "local_marks",
    "sender_lists",
    "get_launch_at_login",
    "platform_info",
];

/// May the page run `command` now?
#[cfg_attr(not(mobile), allow(dead_code))]
pub fn command_allowed(present: bool, command: &str) -> bool {
    present || ALLOWED_WHILE_AWAY.contains(&command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LaunchState {
    pub present: bool,
    /// Changes so far; the page ignores an answer older than an `app://presence` event it already saw.
    pub seq: u64,
}

pub fn launch_state_now() -> LaunchState {
    #[cfg(target_os = "ios")]
    {
        let (present, seq) = tauri_plugin_mailcheck::presence_state();
        LaunchState { present, seq }
    }
    #[cfg(not(target_os = "ios"))]
    {
        LaunchState { present: true, seq: 0 }
    }
}

/// The page asks this once at start (after it listens for `app://presence`) before anything that talks to a server.
#[tauri::command]
pub fn launch_state() -> LaunchState {
    launch_state_now()
}

/// What a presence change starts or stops. Pure; `lib.rs::on_presence` carries it out.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The once-per-process services (outbox, snooze, CalDAV, Drafts sweep, backfill, badge poller).
    pub start_services: bool,
    /// INBOX watchers (+ folder sync loop + live folders) for every account.
    pub start_watchers: bool,
    /// The launch pass: sync the Inbox once, then rules / auto-reply (first time the user is present in this process).
    pub launch_pass: bool,
    /// Rules / auto-reply over what arrived while the user was away (cursor based, so nothing runs twice).
    pub catch_up: bool,
    pub stop_watchers: bool,
}

pub fn plan(present: bool, services_started: bool, watchers_running: bool) -> Plan {
    match (present, services_started, watchers_running) {
        (true, false, _) => Plan { start_services: true, start_watchers: true, launch_pass: true, ..Plan::default() },
        (true, true, false) => Plan { start_watchers: true, catch_up: true, ..Plan::default() },
        (false, _, true) => Plan { stop_watchers: true, ..Plan::default() },
        _ => Plan::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_arrival_starts_everything_once() {
        assert_eq!(plan(true, false, false), Plan { start_services: true, start_watchers: true, launch_pass: true, ..Plan::default() });
    }

    #[test]
    fn coming_back_restarts_watchers_and_catches_up_without_a_second_service_start() {
        let p = plan(true, true, false);
        assert!(p.start_watchers && p.catch_up);
        assert!(!p.start_services && !p.launch_pass);
        assert_eq!(plan(true, true, true), Plan::default(), "already running: nothing (idempotent)");
    }

    #[test]
    fn leaving_stops_watchers_and_starts_nothing() {
        assert_eq!(plan(false, true, true), Plan { stop_watchers: true, ..Plan::default() });
        assert_eq!(plan(false, false, false), Plan::default(), "background launch: nothing starts");
        assert_eq!(plan(false, true, false), Plan::default());
    }

    #[test]
    fn away_means_deferred() {
        assert_eq!(check(false).unwrap_err(), DEFERRED);
        assert!(check(true).is_ok());
        assert!(DEFERRED.starts_with("deferred:"), "the page matches this prefix (app/src/lib/foreground-model.ts)");
    }

    #[test]
    fn while_away_only_local_commands_pass() {
        for c in ["sync_folder", "unread_counts", "get_message", "set_flag", "move_message", "outbox_submit", "outbox_retry", "ai_send", "cal_sync", "zoom_status", "unsubscribe_one_click", "set_folder_live", "draft_mirror_save", "add_account", "something_new"] {
            assert!(!command_allowed(false, c), "{c} must wait for the user");
            assert!(command_allowed(true, c));
        }
        for c in ["launch_state", "list_accounts", "list_messages", "load_layout", "list_drafts", "outbox_list", "snooze_list", "live_states", "list_folders"] {
            assert!(command_allowed(false, c), "{c} is local");
        }
    }

    #[test]
    fn every_allowed_command_is_registered() {
        let src = include_str!("lib.rs");
        let handler = &src[src.find("generate_handler![").expect("handler list")..];
        for c in ALLOWED_WHILE_AWAY {
            assert!(handler.contains(&format!("::{c},")), "{c} is not a registered command (typo?)");
        }
    }

    #[test]
    fn desktop_is_always_present() {
        if !cfg!(target_os = "ios") {
            assert!(user_present());
            assert_eq!(launch_state_now(), LaunchState { present: true, seq: 0 });
        }
    }
}
