//! iOS background refresh bridge (feasibility spike).
//!
//! Swift (`ios/Sources/BgRefreshPlugin.swift`) registers a `BGAppRefreshTask`. When iOS fires it,
//! Swift calls the C symbol [`bgrefresh_run_check`] in-process; that runs the handler the app
//! installed with [`set_check_handler`] and returns a short, non-sensitive message. Swift then posts
//! a local notification with it. Mail logic stays in the app crate; this crate knows nothing of it.

use std::sync::OnceLock;
use tauri::{
    plugin::{Builder, TauriPlugin},
    Runtime,
};

/// Returns a one-line, non-sensitive summary (e.g. "3 unread") or an error text.
pub type CheckHandler = Box<dyn Fn() -> Result<String, String> + Send + Sync>;
static HANDLER: OnceLock<CheckHandler> = OnceLock::new();

pub fn set_check_handler(h: CheckHandler) {
    let _ = HANDLER.set(h);
}

/// Called from Swift when the background task fires (or from the debug command).
/// Writes a NUL-terminated UTF-8 message into `buf` (capacity `cap`); returns 0 = ok, 1 = handler error, 2 = no handler.
///
/// # Safety
/// `buf` must be valid for `cap` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn bgrefresh_run_check(buf: *mut u8, cap: usize) -> i32 {
    let (code, msg) = match HANDLER.get() {
        None => (2, "no handler installed".to_string()),
        Some(h) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(h)) {
            Ok(Ok(m)) => (0, m),
            Ok(Err(e)) => (1, e),
            Err(_) => (1, "handler panicked".to_string()),
        },
    };
    if !buf.is_null() && cap > 0 {
        let bytes = msg.as_bytes();
        let n = bytes.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    code
}

#[cfg(mobile)]
mod mobile {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tauri::plugin::PluginHandle;

    #[cfg(target_os = "ios")]
    tauri::ios_plugin_binding!(init_plugin_bgrefresh);

    pub struct Handle<R: Runtime>(pub PluginHandle<R>);

    #[derive(Serialize, Deserialize, Debug)]
    pub struct Empty {}

    #[derive(Serialize, Deserialize, Debug)]
    pub struct Status {
        pub scheduled: bool,
        pub notifications: String,
        pub fires: i64,
        pub last_fire: String,
        pub last_result: String,
    }

    pub fn init<R: Runtime>(
        api: tauri::plugin::PluginApi<R, ()>,
    ) -> Result<Handle<R>, Box<dyn std::error::Error>> {
        #[cfg(target_os = "ios")]
        let h = api.register_ios_plugin(init_plugin_bgrefresh)?;
        #[cfg(not(target_os = "ios"))]
        let h = {
            let _ = api;
            return Err("bgrefresh is iOS only".into());
        };
        #[cfg(target_os = "ios")]
        Ok(Handle(h))
    }

    macro_rules! cmd {
        ($name:ident, $swift:literal, $ret:ty) => {
            #[tauri::command]
            pub async fn $name<R: Runtime>(app: tauri::AppHandle<R>) -> Result<$ret, String> {
                use tauri::Manager;
                app.state::<Handle<R>>().0.run_mobile_plugin($swift, Empty {}).map_err(|e| e.to_string())
            }
        };
    }
    cmd!(schedule, "schedule", Status);
    cmd!(request_notifications, "requestNotifications", Status);
    cmd!(status, "status", Status);
    cmd!(simulate_fire, "simulateFire", Status);
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    let b = Builder::new("bgrefresh");
    #[cfg(mobile)]
    let b = b
        .invoke_handler(tauri::generate_handler![
            mobile::schedule,
            mobile::request_notifications,
            mobile::status,
            mobile::simulate_fire
        ])
        .setup(|app, api| {
            use tauri::Manager;
            app.manage(mobile::init(api)?);
            Ok(())
        });
    b.build()
}
