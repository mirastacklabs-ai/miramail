//! iOS background new-mail check bridge. No push service, no server: iOS wakes the app now and then
//! (`BGAppRefreshTask`), the app checks the user's own IMAP servers and posts local notifications.
//!
//! Swift (`ios/Sources/MailCheckPlugin.swift`) registers the task at launch and schedules it when the app goes to the
//! background and after every run. When iOS fires it, Swift calls [`mailcheck_task_begin`] and then, on a worker
//! thread, [`mailcheck_run_check`]; that runs the handler the app installed with [`set_check_handler`]. When iOS says
//! time is up, Swift calls [`mailcheck_cancel`], which flips the [`CancelToken`] the handler watches. Mail logic stays in
//! the app crate; this crate knows nothing of it and carries only counts back (never subjects, senders or bodies).
//!
//! It also owns the process-wide "is the user in front of the app" state ([`user_present`]): Swift reports every
//! `didBecomeActive` / `didEnterBackground` (and the state at launch) through [`mailcheck_set_present`], and the app
//! reacts through the handler it installs with [`set_presence_handler`]. Off iOS the user is always present.
//!
//! Nothing here runs from launch arguments and there is no way to trigger a check from the web view.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{
    plugin::{Builder, TauriPlugin},
    Runtime,
};

/// Set by [`mailcheck_cancel`] even when no check is running yet (iOS may expire the task before the worker thread
/// reached Rust). Cleared by [`mailcheck_task_begin`], i.e. when iOS hands over a new task. Every token sees it.
static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Set by [`mailcheck_cancel`] (iOS expiration handler); the handler must stop cleanly soon after.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst) || CANCEL_REQUESTED.load(Ordering::SeqCst)
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// How a check ended. Counts only: this text reaches the device log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Finished normally; the string is a short, non-sensitive summary such as "accounts=2 new=3 notified=3".
    Done(String),
    /// Stopped because the token was cancelled or the time budget ran out (the task is reported as not successful).
    Cut(String),
}

pub type CheckHandler = Box<dyn Fn(CancelToken) -> Result<Outcome, String> + Send + Sync>;
static HANDLER: OnceLock<CheckHandler> = OnceLock::new();
static CURRENT: Mutex<Option<CancelToken>> = Mutex::new(None);
/// One check at a time (a late worker of an expired task must not overlap the next task's check).
static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn set_check_handler(h: CheckHandler) {
    let _ = HANDLER.set(h);
}

/// Called from Swift (main thread) when iOS hands over a refresh task, before its expiration handler is set: a
/// cancel left over from an earlier task must not stop this one.
#[no_mangle]
pub extern "C" fn mailcheck_task_begin() {
    CANCEL_REQUESTED.store(false, Ordering::SeqCst);
}

/// Called from Swift when the background task fires. Writes a NUL-terminated UTF-8 summary into `buf` (capacity
/// `cap`). Returns 0 = done, 1 = handler error, 2 = no handler installed, 3 = cut short (cancelled / out of time),
/// 4 = another check is still running.
///
/// # Safety
/// `buf` must be valid for `cap` writable bytes (or null).
#[no_mangle]
pub unsafe extern "C" fn mailcheck_run_check(buf: *mut u8, cap: usize) -> i32 {
    let (code, msg) = if RUNNING.swap(true, Ordering::SeqCst) {
        (4, "another check is running".to_string())
    } else {
        let token = CancelToken::default();
        *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.clone());
        let r = match HANDLER.get() {
            None => (2, "no handler installed".to_string()),
            Some(h) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h(token))) {
                Ok(Ok(Outcome::Done(m))) => (0, m),
                Ok(Ok(Outcome::Cut(m))) => (3, m),
                Ok(Err(e)) => (1, e),
                Err(_) => (1, "handler panicked".to_string()),
            },
        };
        *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
        RUNNING.store(false, Ordering::SeqCst);
        r
    };
    if !buf.is_null() && cap > 0 {
        let bytes = msg.as_bytes();
        let n = bytes.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n);
        *buf.add(n) = 0;
    }
    code
}

/// Called from Swift's expiration handler: ask the running check (or the one about to start) to stop. Never blocks.
#[no_mangle]
pub extern "C" fn mailcheck_cancel() {
    CANCEL_REQUESTED.store(true, Ordering::SeqCst);
    if let Some(t) = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        t.cancel();
    }
}

// ---------------------------------------------------------------- user presence

/// What a presence report leads to. Pure; the statics below wrap it.
#[derive(Debug, Default)]
struct PresenceHub {
    present: bool,
    /// Bumped on every change, so the page can order a `launch_state` answer against `app://presence` events.
    seq: u64,
    installed: bool,
}

impl PresenceHub {
    fn new(present: bool) -> Self {
        PresenceHub { present, seq: 0, installed: false }
    }
    /// Swift reported `present`. `Some(p)`: call the handler with `p` now. A report before the handler is installed is
    /// not lost: [`PresenceHub::install`] delivers the state it left behind.
    fn report(&mut self, present: bool) -> Option<bool> {
        if present == self.present {
            return None;
        }
        self.present = present;
        self.seq += 1;
        self.installed.then_some(present)
    }
    /// The handler was installed: it is called once with the current state (whatever was reported before), then on
    /// every change.
    fn install(&mut self) -> bool {
        self.installed = true;
        self.present
    }
}

pub type PresenceHandler = Arc<dyn Fn(bool) + Send + Sync>;
/// Off iOS the user is always in front of the app. On iOS: not present until Swift says so (safe default).
static PRESENCE: Mutex<Option<PresenceHub>> = Mutex::new(None);
static PRESENCE_HANDLER: Mutex<Option<PresenceHandler>> = Mutex::new(None);
/// `seq << 1 | present`, one word so a reader never sees a new `present` with an old `seq` (or the reverse).
static PRESENCE_WORD: AtomicU64 = AtomicU64::new(!cfg!(target_os = "ios") as u64);

fn with_hub<T>(f: impl FnOnce(&mut PresenceHub) -> T) -> T {
    let mut g = PRESENCE.lock().unwrap_or_else(|e| e.into_inner());
    let hub = g.get_or_insert_with(|| PresenceHub::new(!cfg!(target_os = "ios")));
    let r = f(hub);
    PRESENCE_WORD.store(hub.seq << 1 | hub.present as u64, Ordering::SeqCst);
    r
}

fn call_presence(h: &PresenceHandler, present: bool) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h(present)));
}

/// True while the user is in front of the app: on iOS between `didBecomeActive` and `didEnterBackground` (a launch
/// that iOS started in the background, or a suspended app woken for a refresh, is not). Always true off iOS.
pub fn user_present() -> bool {
    PRESENCE_WORD.load(Ordering::SeqCst) & 1 == 1
}

/// `(present, seq)` read in one atomic load.
pub fn presence_state() -> (bool, u64) {
    let w = PRESENCE_WORD.load(Ordering::SeqCst);
    (w & 1 == 1, w >> 1)
}

/// Changes of [`user_present`] so far in this process (0 = none).
pub fn presence_seq() -> u64 {
    PRESENCE_WORD.load(Ordering::SeqCst) >> 1
}

/// Install what runs when the user arrives / leaves. Called once at once with the current state (also covering a
/// report that came before this call), then on every change, on the thread Swift reports from (main). Idempotent
/// handlers only. A second install replaces the first.
pub fn set_presence_handler(h: PresenceHandler) {
    *PRESENCE_HANDLER.lock().unwrap_or_else(|e| e.into_inner()) = Some(h.clone());
    let now = with_hub(PresenceHub::install);
    call_presence(&h, now);
}

/// Called from Swift: 1 = the user is in front of the app (`didBecomeActive`, or not `.background` at launch), 0 = not
/// (`didEnterBackground`, or `.background` at launch).
#[no_mangle]
pub extern "C" fn mailcheck_set_present(present: i32) {
    if let Some(p) = with_hub(|hub| hub.report(present != 0)) {
        let h = PRESENCE_HANDLER.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(h) = h {
            call_presence(&h, p);
        }
    }
}

// ---------------------------------------------------------------- Low Data Mode / expensive path

/// `NWPath.isConstrained` (Low Data Mode, or Low Data on this Wi-Fi) and `NWPath.isExpensive` (cellular, hotspot) for the
/// current network path, reported by Swift's `NWPathMonitor` (public API, read-only). On iOS nothing is known until the
/// first report, and an unknown path counts as constrained. Off iOS: always known, never constrained or expensive.
static PATH_KNOWN: AtomicBool = AtomicBool::new(!cfg!(target_os = "ios"));
static CONSTRAINED: AtomicBool = AtomicBool::new(false);
static EXPENSIVE: AtomicBool = AtomicBool::new(false);

/// True while optional downloads must not run: Low Data Mode, or the path is not known yet (iOS, before the first report).
pub fn low_data_mode() -> bool {
    !PATH_KNOWN.load(Ordering::SeqCst) || CONSTRAINED.load(Ordering::SeqCst)
}

/// True on a metered path (cellular, personal hotspot).
pub fn path_expensive() -> bool {
    EXPENSIVE.load(Ordering::SeqCst)
}

/// Called from Swift whenever the path changes: 1 = yes, 0 = no for each flag.
#[no_mangle]
pub extern "C" fn mailcheck_set_path(constrained: i32, expensive: i32) {
    CONSTRAINED.store(constrained != 0, Ordering::SeqCst);
    EXPENSIVE.store(expensive != 0, Ordering::SeqCst);
    PATH_KNOWN.store(true, Ordering::SeqCst);
}

/// The message a tapped notification announced.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Tap {
    pub account: String,
    pub folder: String,
    pub uid: u32,
}

#[cfg(mobile)]
mod mobile {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tauri::plugin::PluginHandle;

    #[cfg(target_os = "ios")]
    tauri::ios_plugin_binding!(init_plugin_mailcheck);

    pub struct Handle<R: Runtime>(pub PluginHandle<R>);

    #[derive(Serialize)]
    pub struct Badge {
        pub count: i64,
    }
    #[derive(Serialize)]
    pub struct Empty {}
    // share / Quick Look / print arguments (ShareSheet.swift)
    #[derive(Serialize)]
    pub struct PathArgs<'a> {
        pub path: &'a str,
    }
    #[derive(Serialize)]
    pub struct PrintHtmlArgs<'a> {
        pub html: &'a str,
        pub title: &'a str,
    }
    #[derive(Deserialize)]
    pub struct AllowedReply {
        pub allowed: bool,
    }
    #[derive(Deserialize)]
    pub struct TapReply {
        pub tap: Option<Tap>,
    }

    pub fn init<R: Runtime>(
        api: tauri::plugin::PluginApi<R, ()>,
    ) -> Result<Handle<R>, Box<dyn std::error::Error>> {
        #[cfg(target_os = "ios")]
        {
            let h = api.register_ios_plugin(init_plugin_mailcheck)?;
            Ok(Handle(h))
        }
        #[cfg(not(target_os = "ios"))]
        {
            let _ = api;
            Err("mailcheck is iOS only".into())
        }
    }
}

/// Set the app icon badge (0 clears it). No-op off iOS. Call from a worker thread, not the main thread.
pub fn set_badge<R: Runtime>(app: &tauri::AppHandle<R>, count: i64) {
    #[cfg(mobile)]
    {
        use tauri::Manager;
        if let Some(h) = app.try_state::<mobile::Handle<R>>() {
            let _ = h.0.run_mobile_plugin::<serde_json::Value>("setBadge", mobile::Badge { count: count.max(0) });
        }
    }
    #[cfg(not(mobile))]
    let _ = (app, count);
}

/// Read-only check (`getNotificationSettings`): are notifications authorized or provisional? Never shows the permission
/// prompt. Blocking; call from a worker thread. False off iOS or when the answer cannot be had.
pub fn notifications_allowed<R: Runtime>(app: &tauri::AppHandle<R>) -> bool {
    #[cfg(mobile)]
    {
        use tauri::Manager;
        app.try_state::<mobile::Handle<R>>()
            .and_then(|h| h.0.run_mobile_plugin::<mobile::AllowedReply>("notifAllowed", mobile::Empty {}).ok())
            .is_some_and(|r| r.allowed)
    }
    #[cfg(not(mobile))]
    {
        let _ = app;
        false
    }
}

/// The notification the user tapped since the last call (taken: returned once). `None` off iOS.
pub fn take_tap<R: Runtime>(app: &tauri::AppHandle<R>) -> Option<Tap> {
    #[cfg(mobile)]
    {
        use tauri::Manager;
        let h = app.try_state::<mobile::Handle<R>>()?;
        h.0.run_mobile_plugin::<mobile::TapReply>("takeTap", mobile::Empty {}).ok()?.tap
    }
    #[cfg(not(mobile))]
    {
        let _ = app;
        None
    }
}

// ---------------------------------------------------------------- share / Quick Look / print (ShareSheet.swift)

/// System share sheet (Save to Files, AirDrop, Mail, ...) for a file the app staged in its own cache. Swift refuses a
/// path outside `<container>/.../share/<dir>/` and deletes the folder when the sheet is dismissed. Blocking until the
/// sheet is presented; call from a worker thread. Errors off iOS.
pub fn share_file<R: Runtime>(app: &tauri::AppHandle<R>, path: &std::path::Path) -> Result<(), String> {
    file_cmd(app, "shareFile", path)
}

/// Quick Look preview (inside the app) of a staged file; same path rules and cleanup as [`share_file`].
pub fn preview_file<R: Runtime>(app: &tauri::AppHandle<R>, path: &std::path::Path) -> Result<(), String> {
    file_cmd(app, "previewFile", path)
}

/// System print sheet for a staged file (PDF or image); same path rules and cleanup as [`share_file`].
pub fn print_file<R: Runtime>(app: &tauri::AppHandle<R>, path: &std::path::Path) -> Result<(), String> {
    file_cmd(app, "printFile", path)
}

/// System print sheet for an HTML document, rendered with JavaScript off and navigation blocked. `title` names the job.
pub fn print_html<R: Runtime>(app: &tauri::AppHandle<R>, html: &str, title: &str) -> Result<(), String> {
    #[cfg(mobile)]
    {
        use tauri::Manager;
        let h = app.try_state::<mobile::Handle<R>>().ok_or("The print sheet isn't available.")?;
        h.0.run_mobile_plugin::<serde_json::Value>("printHtml", mobile::PrintHtmlArgs { html, title })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(mobile))]
    {
        let _ = (app, html, title);
        Err("Printing is only available on iPhone.".into())
    }
}

fn file_cmd<R: Runtime>(app: &tauri::AppHandle<R>, cmd: &str, path: &std::path::Path) -> Result<(), String> {
    #[cfg(mobile)]
    {
        use tauri::Manager;
        let h = app.try_state::<mobile::Handle<R>>().ok_or("The share sheet isn't available.")?;
        let path = path.to_str().ok_or("Bad file path.")?;
        h.0.run_mobile_plugin::<serde_json::Value>(cmd, mobile::PathArgs { path }).map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(not(mobile))]
    {
        let _ = (app, cmd, path);
        Err("The share sheet is only available on iPhone.".into())
    }
}

// ---------------------------------------------------------------- home-screen widgets (WidgetBridge.swift)

#[cfg(mobile)]
#[derive(serde::Deserialize)]
struct WidgetDirReply {
    path: Option<String>,
}

/// The App Group container (`group.<app bundle id>`) the widget extension reads, or `None` when the group is not
/// provisioned / off iOS. Blocking; call from a worker thread. Nothing else is shared with the extension.
pub fn widget_container<R: Runtime>(app: &tauri::AppHandle<R>) -> Result<Option<std::path::PathBuf>, String> {
    #[cfg(mobile)]
    {
        use tauri::Manager;
        let h = app.try_state::<mobile::Handle<R>>().ok_or("plugin handle not managed")?;
        let r = h.0.run_mobile_plugin::<WidgetDirReply>("widgetContainer", mobile::Empty {}).map_err(|e| e.to_string())?;
        Ok(r.path.filter(|p| !p.is_empty()).map(std::path::PathBuf::from))
    }
    #[cfg(not(mobile))]
    {
        let _ = app;
        Ok(None)
    }
}

/// `WidgetCenter.shared.reloadAllTimelines()`. No-op off iOS.
pub fn reload_widgets<R: Runtime>(app: &tauri::AppHandle<R>) {
    #[cfg(mobile)]
    {
        use tauri::Manager;
        if let Some(h) = app.try_state::<mobile::Handle<R>>() {
            let _ = h.0.run_mobile_plugin::<serde_json::Value>("widgetReload", mobile::Empty {});
        }
    }
    #[cfg(not(mobile))]
    let _ = app;
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    let b = Builder::new("mailcheck");
    #[cfg(mobile)]
    let b = b.setup(|app, api| {
        use tauri::Manager;
        app.manage(mobile::init(api)?);
        Ok(())
    });
    b.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_before_the_handler_is_installed_is_not_lost() {
        let mut h = PresenceHub::new(false);
        assert_eq!(h.report(true), None, "nobody to tell yet");
        assert_eq!(h.seq, 1);
        assert!(h.install(), "install delivers the state the early report left behind");
        assert_eq!(h.report(true), None, "no change, no call");
        assert_eq!(h.report(false), Some(false));
        assert_eq!(h.report(true), Some(true));
        assert_eq!(h.seq, 3);
    }

    #[test]
    fn install_without_reports_delivers_the_initial_state() {
        assert!(!PresenceHub::new(false).install(), "iOS default: not present until Swift says so");
        assert!(PresenceHub::new(true).install());
    }

    #[test]
    fn a_cancel_before_the_check_starts_is_seen_and_a_new_task_clears_it() {
        mailcheck_task_begin();
        let t = CancelToken::default();
        assert!(!t.is_cancelled());
        mailcheck_cancel(); // no CURRENT yet: the worker thread has not reached Rust
        assert!(t.is_cancelled(), "a token made after the cancel still sees it");
        mailcheck_task_begin();
        assert!(!CancelToken::default().is_cancelled(), "the next task starts clean");
        let own = CancelToken::default();
        own.cancel();
        assert!(own.is_cancelled());
    }

    #[test]
    fn path_flags_follow_the_report_and_unknown_counts_as_constrained_on_ios() {
        // The statics are process-wide: one test exercises them in order.
        if cfg!(target_os = "ios") {
            PATH_KNOWN.store(false, Ordering::SeqCst);
            assert!(low_data_mode(), "unknown path = constrained");
        }
        mailcheck_set_path(1, 0);
        assert!(low_data_mode() && !path_expensive());
        mailcheck_set_path(0, 1);
        assert!(!low_data_mode() && path_expensive());
        mailcheck_set_path(0, 0);
        assert!(!low_data_mode() && !path_expensive());
    }

    #[test]
    fn desktop_is_always_present() {
        if !cfg!(target_os = "ios") {
            assert!(user_present());
        }
    }
}
