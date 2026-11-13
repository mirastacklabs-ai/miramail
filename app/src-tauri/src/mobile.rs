//! iOS stand-ins for the Mac-only modules (`menu`, `tray`, `widget`). They exist so the shared code and the frontend's
//! invokes keep working on a phone: every function is a harmless no-op or returns what the UI treats as "not available".
//! Compiled only for mobile (`cfg(mobile)`); the Mac build uses the real modules and never sees this file.

pub mod menu {
    use tauri::AppHandle;

    /// There is one window on a phone and it is always showing; only the notification-click bookkeeping applies.
    pub fn show_main(app: &AppHandle) {
        use tauri::Manager;
        app.state::<crate::badge::BadgeState>().clear_notified();
    }

    #[tauri::command]
    pub fn get_launch_at_login() -> Result<bool, String> {
        Ok(false)
    }

    #[tauri::command]
    pub fn set_launch_at_login(_on: bool) -> Result<bool, String> {
        Err("Launch at login isn't available on iPhone.".into())
    }
}

pub mod tray {
    use crate::badge::InboxSummary;
    use crate::plat_prefs::PlatformPrefs;
    use tauri::AppHandle;

    pub fn sync(_app: &AppHandle, _s: &InboxSummary, _p: &PlatformPrefs) {}
}

pub mod widget {
    use tauri::{AppHandle, Runtime};

    pub const LABEL: &str = "widget";

    /// No widget window on iOS. What is restricted instead is time: while the user is not in front of the app (iOS
    /// may load the page during a background launch), only local commands run; the rest is refused with
    /// `presence::DEFERRED` and the page asks again once the user is back (see presence.rs).
    pub fn guard<R: Runtime>(
        inner: impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static,
    ) -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
        move |invoke| {
            if !crate::presence::command_allowed(crate::presence::user_present(), invoke.message.command()) {
                invoke.resolver.reject(crate::presence::DEFERRED);
                return true;
            }
            inner(invoke)
        }
    }

    pub fn sync(_app: &AppHandle) {}

    #[tauri::command]
    pub fn focus_main(_app: AppHandle, _account: Option<String>, _folder: Option<String>, _uid: Option<u32>) {}

    #[tauri::command]
    pub fn widget_menu() -> Result<(), String> {
        Err("There is no widget on iPhone.".into())
    }
}
