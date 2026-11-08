//! The optional floating mini widget (Stage 1 of docs/WIDGET_SPEC.md): a small frameless, opaque window kept at
//! desktop level (below normal windows), draggable, position remembered. Its own tiny Svelte entry is `widget.html`.
//!
//! Security: the widget window may call ONLY `WIDGET_COMMANDS` (all read-only or navigation). `guard` wraps the
//! app's invoke handler and rejects everything else from this window; its capability file also grants it nothing
//! beyond listening to events and dragging.

use crate::badge::BadgeState;
use crate::plat_prefs::{Geo, PlatformState};
use crate::winstate;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const LABEL: &str = "widget";
pub const ACCOUNT_PREFIX: &str = "widget-acct:";
/// Everything the widget window may invoke.
pub const WIDGET_COMMANDS: &[&str] = &["get_inbox_summary", "focus_main", "widget_menu"];
const DEFAULT_SIZE: (f64, f64) = (230.0, 280.0);

/// Default-deny by window label: `main` and valid `pane-<id>` windows get everything, the widget only
/// `WIDGET_COMMANDS`, any other label (unknown / future windows) nothing.
pub fn command_allowed(window_label: &str, command: &str) -> bool {
    if window_label == LABEL {
        WIDGET_COMMANDS.contains(&command)
    } else {
        crate::panes::valid_label(window_label)
    }
}

/// Wrap the app's invoke handler: calls from the widget window outside the allow-list are rejected.
pub fn guard<R: Runtime>(
    inner: impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke| {
        if !command_allowed(invoke.message.webview().label(), invoke.message.command()) {
            invoke.resolver.reject("The widget window isn't allowed to do that.");
            return true;
        }
        inner(invoke)
    }
}

fn default_geo(app: &AppHandle) -> Geo {
    let s = winstate::screens(app);
    match s.first() {
        Some(m) => Geo { x: m.x + m.w - DEFAULT_SIZE.0 - 24.0, y: m.y + 56.0, w: DEFAULT_SIZE.0, h: DEFAULT_SIZE.1 },
        None => Geo { x: 80.0, y: 80.0, w: DEFAULT_SIZE.0, h: DEFAULT_SIZE.1 },
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn desktop_behaviour(w: &WebviewWindow) {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior as B};
    if let Ok(ptr) = w.ns_window() {
        // SAFETY: `ns_window` returns the live NSWindow of this webview window; we only call a setter, on the main thread.
        let ns: &NSWindow = unsafe { &*(ptr as *const NSWindow) };
        ns.setCollectionBehavior(B::CanJoinAllSpaces | B::Stationary | B::IgnoresCycle | B::Transient);
    }
}
#[cfg(not(target_os = "macos"))]
fn desktop_behaviour(_: &WebviewWindow) {}

fn open(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.show();
        return;
    }
    let saved = app.state::<PlatformState>().get().widget_geo;
    let g = saved.and_then(|g| winstate::clamp_visible(g, &winstate::screens(app))).unwrap_or_else(|| default_geo(app));
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("widget.html".into()))
        .title("MiraMail")
        .inner_size(g.w, g.h)
        .position(g.x, g.y)
        .min_inner_size(160.0, 160.0)
        .decorations(false)
        // Opaque on purpose: window transparency on macOS needs the private-API flag, which the Mac App Store refuses.
        .background_color(tauri::window::Color(10, 10, 10, 255))
        .shadow(false)
        .resizable(true)
        .always_on_bottom(true)
        .skip_taskbar(true)
        .visible_on_all_workspaces(true)
        .accept_first_mouse(true)
        .focused(false)
        .build();
    if let Ok(w) = built {
        desktop_behaviour(&w);
    }
}

/// Make the widget window match the preference (open it, or close it). Safe to call any time.
pub fn sync(app: &AppHandle) {
    let want = app.state::<PlatformState>().get().widget_show;
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if want {
            open(&app2);
        } else if let Some(w) = app2.get_webview_window(LABEL) {
            let _ = w.close();
        }
    });
}

pub fn set_shown(app: &AppHandle, on: bool) {
    let st = app.state::<PlatformState>();
    st.update_mem(|p| p.widget_show = on);
    let _ = st.persist();
    sync(app);
    let _ = app.emit_to(crate::panes::MAIN, "app://prefs-changed", ());
}

pub fn choose_account(app: &AppHandle, account: Option<String>) {
    let st = app.state::<PlatformState>();
    st.update_mem(|p| p.widget_account = account);
    let _ = st.persist();
    crate::badge::refresh_now(app);
    let _ = app.emit_to(crate::panes::MAIN, "app://prefs-changed", ());
}

// ---------------------------------------------------------------- commands (widget-callable)

/// Bring the main window forward, optionally to a message (`account`+`folder`+`uid`) or an inbox (`account`).
#[tauri::command]
pub fn focus_main(app: AppHandle, account: Option<String>, folder: Option<String>, uid: Option<u32>) {
    crate::menu::show_main(&app);
    let ok = |s: &str| !s.is_empty() && s.len() <= 320 && !s.chars().any(char::is_control);
    match (account, folder, uid) {
        (Some(a), Some(f), Some(u)) if ok(&a) && ok(&f) => {
            let _ = app.emit_to(crate::panes::MAIN, "app://open-message", crate::notify::OpenMessage { account: a, folder: f, uid: u, select_only: false });
        }
        (Some(a), _, _) if ok(&a) => {
            let _ = app.emit_to(crate::panes::MAIN, "app://open-inbox", a);
        }
        _ => {}
    }
}

/// Right-click menu of the widget: choose account, refresh, hide. Only the widget window may ask for it.
#[tauri::command]
pub fn widget_menu(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    if window.label() != LABEL {
        return Err("Only the widget window can open this menu.".into());
    }
    let s = app.state::<BadgeState>().summary();
    let chosen = app.state::<PlatformState>().get().widget_account;
    let menu = Menu::new(&app).map_err(|e| e.to_string())?;
    let e = |e: tauri::Error| e.to_string();
    menu.append(&CheckMenuItem::with_id(&app, ACCOUNT_PREFIX.to_string(), "All accounts", true, chosen.is_none(), None::<&str>).map_err(e)?).map_err(e)?;
    for a in &s.accounts {
        let on = chosen.as_deref() == Some(a.as_str());
        menu.append(&CheckMenuItem::with_id(&app, format!("{ACCOUNT_PREFIX}{a}"), a, true, on, None::<&str>).map_err(e)?).map_err(e)?;
    }
    menu.append(&PredefinedMenuItem::separator(&app).map_err(e)?).map_err(e)?;
    menu.append(&MenuItem::with_id(&app, "widget-refresh", "Refresh", true, None::<&str>).map_err(e)?).map_err(e)?;
    menu.append(&MenuItem::with_id(&app, "widget-hide", "Hide widget", true, None::<&str>).map_err(e)?).map_err(e)?;
    window.popup_menu(&menu).map_err(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_window_is_limited_to_the_allow_list() {
        for c in WIDGET_COMMANDS {
            assert!(command_allowed(LABEL, c));
        }
        for bad in [
            "set_flag", "move_message", "move_message_to", "delete_message", "send_message", "add_account", "remove_account",
            "save_draft", "ai_send", "ai_approve_proposal", "cal_save_event", "cal_delete_event", "set_platform_prefs",
            "get_message", "list_messages", "get_attachment", "take_pending_mailto", "save_attachment_to_downloads", "",
        ] {
            assert!(!command_allowed(LABEL, bad), "widget must not call {bad}");
        }
    }

    #[test]
    fn other_windows_are_unaffected() {
        assert!(command_allowed("main", "set_flag"));
        assert!(command_allowed("pane-abc", "outbox_submit"));
    }

    #[test]
    fn unknown_or_malformed_labels_are_denied() {
        for l in ["", "other", "future-window", "pane-", "pane-a b", "pane-../x", "Main", "widget2", "pane-é"] {
            assert!(!command_allowed(l, "get_inbox_summary"), "{l:?}");
            assert!(!command_allowed(l, "set_flag"), "{l:?}");
        }
    }

    #[test]
    fn allow_list_is_read_only_or_navigation() {
        // Anything that mutates mail or settings must never be added here.
        for c in WIDGET_COMMANDS {
            assert!(!c.contains("set_") && !c.contains("delete") && !c.contains("send") && !c.contains("move") && !c.contains("save"), "{c}");
        }
    }
}
