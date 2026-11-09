//! Native macOS menu bar + every menu-event dispatch (menu bar, tray item, widget context menu).
//!
//! Menu items that map to an in-app action emit `app://menu` {action} to the main window; the frontend
//! (`lib/platform.svelte.ts`) calls the same functions the keyboard shortcuts use. Native accelerators are only
//! used with a modifier (a bare-letter accelerator would swallow typing in the composer).

use crate::{badge, panes, tray, widget};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager};

/// Actions the frontend understands. Anything else coming from a menu is ignored.
pub const FRONTEND_ACTIONS: &[&str] = &[
    "preferences", "about", "new-message", "check-mail", "view-mail", "view-calendar", "toggle-view", "toggle-ai", "cycle-theme", "shortcuts",
    "go-inbox", "go-drafts", "go-sent", "go-archive", "go-trash", "go-junk", "reply", "reply-all", "forward", "archive", "trash",
    "mark-read", "mark-unread", "toggle-flag",
];
/// Actions that act on the main window's selected message. They are ignored unless the main window is focused,
/// so a focused detached pane never triggers them against whatever main has selected.
const MESSAGE_ACTIONS: &[&str] = &["reply", "reply-all", "forward", "archive", "trash", "mark-read", "mark-unread", "toggle-flag"];
/// Actions that should bring the main window forward first.
const FOCUS_FIRST: &[&str] = &["new-message", "preferences", "about", "shortcuts"];

fn item(app: &AppHandle, id: &str, text: &str, accel: Option<&str>) -> tauri::Result<MenuItem<tauri::Wry>> {
    MenuItem::with_id(app, id, text, true, accel)
}

pub fn build(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let sep = || PredefinedMenuItem::separator(app);
    let app_menu = SubmenuBuilder::new(app, "MiraMail")
        .item(&item(app, "about", "About MiraMail", None)?)
        .separator()
        .item(&item(app, "preferences", "Preferences…", Some("CmdOrCtrl+,"))?)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    let file = SubmenuBuilder::new(app, "File")
        .item(&item(app, "new-message", "New Message", Some("CmdOrCtrl+N"))?)
        .item(&item(app, "check-mail", "Get New Mail", Some("CmdOrCtrl+Shift+M"))?)
        .item(&sep()?)
        // Cmd+W closes the focused pane (the window closes via the red button / Cmd+Q).
        .item(&item(app, "close-pane", "Close Pane", Some("CmdOrCtrl+W"))?)
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit").undo().redo().separator().cut().copy().paste().select_all().build()?;
    let view = SubmenuBuilder::new(app, "View")
        .item(&item(app, "view-mail", "Mail", Some("CmdOrCtrl+1"))?)
        .item(&item(app, "view-calendar", "Calendar", Some("CmdOrCtrl+2"))?)
        .item(&item(app, "toggle-view", "Switch Between Tabs and Tiles", None)?)
        .separator()
        .item(&item(app, "toggle-ai", "Show or Hide MIRA", Some("CmdOrCtrl+J"))?)
        .item(&item(app, "cycle-theme", "Switch Theme", None)?)
        .separator()
        .fullscreen()
        .build()?;
    let mailbox = SubmenuBuilder::new(app, "Mailbox")
        .item(&item(app, "go-inbox", "Inbox", Some("CmdOrCtrl+Alt+1"))?)
        .item(&item(app, "go-drafts", "Drafts", Some("CmdOrCtrl+Alt+2"))?)
        .item(&item(app, "go-sent", "Sent", Some("CmdOrCtrl+Alt+3"))?)
        .item(&item(app, "go-archive", "Archive", Some("CmdOrCtrl+Alt+4"))?)
        .item(&item(app, "go-trash", "Trash", Some("CmdOrCtrl+Alt+5"))?)
        .item(&item(app, "go-junk", "Junk", Some("CmdOrCtrl+Alt+6"))?)
        .build()?;
    let message = SubmenuBuilder::new(app, "Message")
        .item(&item(app, "reply", "Reply", Some("CmdOrCtrl+R"))?)
        .item(&item(app, "reply-all", "Reply All", Some("CmdOrCtrl+Shift+R"))?)
        .item(&item(app, "forward", "Forward", Some("CmdOrCtrl+Shift+F"))?)
        .separator()
        .item(&item(app, "archive", "Archive", Some("Ctrl+CmdOrCtrl+A"))?)
        .item(&item(app, "trash", "Move to Trash", None)?)
        .separator()
        .item(&item(app, "mark-read", "Mark as Read", Some("CmdOrCtrl+Shift+U"))?)
        .item(&item(app, "mark-unread", "Mark as Unread", Some("CmdOrCtrl+Alt+U"))?)
        .item(&item(app, "toggle-flag", "Flag / Unflag", Some("CmdOrCtrl+Shift+L"))?)
        .build()?;
    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .item(&item(app, "detach-pane", "Move Pane to New Window", Some("CmdOrCtrl+Shift+O"))?)
        .item(&item(app, "attach-pane", "Move Pane to Main Window", Some("CmdOrCtrl+Shift+A"))?)
        .build()?;
    let help = SubmenuBuilder::new(app, "Help").item(&item(app, "shortcuts", "Keyboard Shortcuts", Some("CmdOrCtrl+/"))?).build()?;
    #[cfg(target_os = "macos")]
    {
        let _ = window.set_as_windows_menu_for_nsapp();
        let _ = help.set_as_help_menu_for_nsapp();
    }
    Menu::with_items(app, &[&app_menu as &Submenu<_>, &file, &edit, &view, &mailbox, &message, &window, &help])
}

pub fn install(app: &tauri::App) -> tauri::Result<()> {
    let handle = app.handle();
    app.set_menu(build(handle)?)?;
    app.on_menu_event(|app, ev| dispatch(app, ev.id().as_ref()));
    Ok(())
}

/// Bring the main window to the front (also un-minimises it).
pub fn show_main(app: &AppHandle) {
    // Focus that comes from the tray / widget / mailto must not be mistaken for a notification click.
    app.state::<badge::BadgeState>().clear_notified();
    if let Some(w) = app.get_webview_window(panes::MAIN) {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// `Some(email)` for a tray account row id.
pub fn tray_account(id: &str) -> Option<&str> {
    id.strip_prefix(tray::ACCOUNT_PREFIX).filter(|a| !a.is_empty())
}

pub fn emit_action(app: &AppHandle, action: &str) {
    if is_frontend_action(action) {
        if is_message_action(action) && !app.get_webview_window(panes::MAIN).is_some_and(|w| w.is_focused().unwrap_or(false)) {
            return;
        }
        if FOCUS_FIRST.contains(&action) {
            show_main(app);
        }
        let _ = app.emit_to(panes::MAIN, "app://menu", action);
    }
}

pub fn dispatch(app: &AppHandle, id: &str) {
    match id {
        // Existing pane actions apply to the focused window only.
        "close-pane" | "detach-pane" | "attach-pane" => panes::emit_to_focused(app, &format!("ws://{id}")),
        "tray-open" => show_main(app),
        "tray-compose" => {
            show_main(app);
            emit_action(app, "new-message");
        }
        "tray-check" | "widget-refresh" => {
            emit_action(app, "check-mail");
            badge::refresh_now(app);
        }
        "tray-quit" => panes::begin_shutdown(app),
        "widget-hide" => widget::set_shown(app, false),
        _ => {
            if let Some(a) = tray_account(id) {
                show_main(app);
                let _ = app.emit_to(panes::MAIN, "app://open-inbox", a.to_string());
            } else if let Some(a) = id.strip_prefix(widget::ACCOUNT_PREFIX) {
                widget::choose_account(app, if a.is_empty() { None } else { Some(a.to_string()) });
            } else {
                emit_action(app, id);
            }
        }
    }
}

/// Launch at login via SMAppService.mainApp (see `native::login`). Default off; only the user's toggle changes it.
#[tauri::command]
pub fn get_launch_at_login() -> Result<bool, String> {
    crate::native::login::enabled()
}

#[tauri::command]
pub fn set_launch_at_login(on: bool) -> Result<bool, String> {
    crate::native::login::set(on)?;
    crate::native::login::enabled()
}

pub fn is_message_action(id: &str) -> bool {
    MESSAGE_ACTIONS.contains(&id)
}

/// Used by tests and the menu: whether the id is one the frontend may be asked to run.
pub fn is_frontend_action(id: &str) -> bool {
    FRONTEND_ACTIONS.contains(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_actions_reach_the_frontend() {
        assert!(is_frontend_action("reply"));
        assert!(!is_frontend_action("delete-everything"));
        assert!(!is_frontend_action(""));
        let mut seen = std::collections::HashSet::new();
        assert!(FRONTEND_ACTIONS.iter().all(|a| seen.insert(*a)), "no duplicates");
        assert!(FOCUS_FIRST.iter().all(|a| FRONTEND_ACTIONS.contains(a)));
        assert!(MESSAGE_ACTIONS.iter().all(|a| FRONTEND_ACTIONS.contains(a)));
        assert!(is_message_action("trash") && !is_message_action("check-mail") && !is_message_action("new-message"));
    }

    #[test]
    fn tray_account_ids() {
        assert_eq!(tray_account("tray-acct:a@x.org"), Some("a@x.org"));
        assert_eq!(tray_account("tray-acct:"), None);
        assert_eq!(tray_account("tray-open"), None);
    }
}
