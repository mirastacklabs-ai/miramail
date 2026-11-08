//! Thin, public-API-only wrappers over AppKit / UserNotifications / ServiceManagement (objc2). Everything the Mac App
//! Store review cares about lives here: opening URLs and files (NSWorkspace, no `open` subprocess), local notifications
//! (UNUserNotificationCenter, no swizzling or AppleScript) and launch at login (SMAppService, no LaunchAgent plist).
//! The decisions (`link_allowed`) are pure and unit-tested; the OS calls are not exercised by tests.

use serde::Serialize;
use tauri::AppHandle;

/// Only these schemes may be handed to the system by a link click or by the app itself.
pub fn link_allowed(url: &str) -> bool {
    let u = url.trim();
    if u.is_empty() || u.len() > 8192 || u.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Some((scheme, rest)) = u.split_once(':') else { return false };
    match scheme.to_ascii_lowercase().as_str() {
        "https" | "http" => rest.strip_prefix("//").is_some_and(|r| r.split(['/', '?', '#']).next().is_some_and(|h| !h.is_empty() && !h.starts_with('@'))),
        "mailto" => !rest.is_empty(),
        _ => false,
    }
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct PlatformInfo {
    /// Running inside the Mac App Store App Sandbox.
    pub sandboxed: bool,
    /// Launch at login can be offered (SMAppService, macOS 13+).
    pub launch_at_login: bool,
}

#[tauri::command]
pub fn platform_info() -> PlatformInfo {
    PlatformInfo { sandboxed: crate::files::is_sandboxed(), launch_at_login: login::supported() }
}

/// Open an http(s) or mailto URL in the default app via NSWorkspace. Rejects every other scheme.
pub async fn open_url(app: &AppHandle, url: &str) -> Result<(), String> {
    if !link_allowed(url) {
        return Err("MiraMail only opens web and email links.".into());
    }
    run_on_main(app, url.trim().to_string(), |u| sys::open_url(&u)).await
}

/// Open a file MiraMail itself staged (an attachment copy) with its default app. The caller vets the file type.
pub async fn open_file(app: &AppHandle, path: std::path::PathBuf) -> Result<(), String> {
    run_on_main(app, path, |p| sys::open_file(&p)).await
}

async fn run_on_main<T: Send + 'static>(app: &AppHandle, arg: T, f: impl FnOnce(T) -> bool + Send + 'static) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f(arg));
    })
    .map_err(|_| "Couldn't reach the system.".to_string())?;
    match rx.await {
        Ok(true) => Ok(()),
        _ => Err("The system couldn't open that.".into()),
    }
}

/// Open a web or email link (used by the UI for every link click).
#[tauri::command]
pub async fn open_link(app: AppHandle, url: String) -> Result<(), String> {
    open_url(&app, &url).await
}

#[cfg(target_os = "macos")]
mod sys {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    pub fn open_url(url: &str) -> bool {
        let Some(u) = NSURL::URLWithString(&NSString::from_str(url)) else { return false };
        NSWorkspace::sharedWorkspace().openURL(&u)
    }
    pub fn open_file(path: &std::path::Path) -> bool {
        let u = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        NSWorkspace::sharedWorkspace().openURL(&u)
    }
}
#[cfg(not(target_os = "macos"))]
mod sys {
    pub fn open_url(_: &str) -> bool { false }
    pub fn open_file(_: &std::path::Path) -> bool { false }
}

/// Launch at login through SMAppService.mainApp (works sandboxed and not; no plist, no helper).
pub mod login {
    #[cfg(target_os = "macos")]
    mod imp {
        use objc2::runtime::AnyClass;
        use objc2_service_management::{SMAppService, SMAppServiceStatus};

        pub fn supported() -> bool {
            AnyClass::get(c"SMAppService").is_some()
        }
        pub fn enabled() -> Result<bool, String> {
            if !supported() {
                return Ok(false);
            }
            Ok(unsafe { SMAppService::mainAppService().status() } == SMAppServiceStatus::Enabled)
        }
        pub fn set(on: bool) -> Result<(), String> {
            if !supported() {
                return Err("Launch at login needs macOS 13 or later.".into());
            }
            let svc = unsafe { SMAppService::mainAppService() };
            let r = unsafe { if on { svc.registerAndReturnError() } else { svc.unregisterAndReturnError() } };
            match r {
                Ok(()) => {
                    if on && unsafe { svc.status() } == SMAppServiceStatus::RequiresApproval {
                        return Err("Allow MiraMail under System Settings > General > Login Items to finish turning this on.".into());
                    }
                    Ok(())
                }
                Err(e) => Err(format!("Couldn't change the launch-at-login setting: {}", e.localizedDescription())),
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    mod imp {
        pub fn supported() -> bool { false }
        pub fn enabled() -> Result<bool, String> { Ok(false) }
        pub fn set(_: bool) -> Result<(), String> { Err("Launch at login isn't available on this system.".into()) }
    }
    pub use imp::{enabled, set, supported};
}

/// Local notifications through UNUserNotificationCenter. Needs a real app bundle: unbundled dev runs are a no-op error.
pub mod notifications {
    #[cfg(target_os = "macos")]
    mod imp {
        use block2::RcBlock;
        use objc2::rc::Retained;
        use objc2::runtime::ProtocolObject;
        use objc2::{define_class, msg_send, ClassType};
        use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
        use objc2_user_notifications::*;
        use std::sync::Once;

        define_class!(
            #[unsafe(super(NSObject))]
            #[name = "MiraMailNotificationDelegate"]
            struct Delegate;

            unsafe impl NSObjectProtocol for Delegate {}

            unsafe impl UNUserNotificationCenterDelegate for Delegate {
                // Show banners even while MiraMail is frontmost (the "send a test" button); real arrivals are
                // already suppressed in code while a window is focused.
                #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
                fn will_present(&self, _c: &UNUserNotificationCenter, _n: &UNNotification, done: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>) {
                    done.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List | UNNotificationPresentationOptions::Sound,));
                }
            }
        );

        fn center() -> Result<Retained<UNUserNotificationCenter>, String> {
            // The centre throws an Objective-C exception for a process without a bundle identifier (cargo run).
            if NSBundle::mainBundle().bundleIdentifier().is_none() {
                return Err("Notifications need the installed app.".into());
            }
            static ONCE: Once = Once::new();
            let c = UNUserNotificationCenter::currentNotificationCenter();
            ONCE.call_once(|| {
                let d: Retained<Delegate> = unsafe { msg_send![Delegate::class(), new] };
                c.setDelegate(Some(ProtocolObject::from_ref(&*d)));
                std::mem::forget(d); // the centre keeps only a weak reference
            });
            Ok(c)
        }

        /// Ask macOS for permission (the system shows its prompt only the first time). `done(granted)`.
        pub fn request_authorization(done: impl Fn(bool) + 'static) -> Result<(), String> {
            let c = center()?;
            let block = RcBlock::new(move |granted: objc2::runtime::Bool, _e: *mut NSError| done(granted.as_bool()));
            c.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &block);
            Ok(())
        }

        pub fn deliver(title: &str, body: &str, sound: bool) -> Result<(), String> {
            let c = center()?;
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(title));
            content.setBody(&NSString::from_str(body));
            if sound {
                content.setSound(Some(&UNNotificationSound::defaultSound()));
            }
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let id = format!("miramail-{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
            let req = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&id), &content, None);
            c.addNotificationRequest_withCompletionHandler(&req, None);
            Ok(())
        }
    }
    #[cfg(not(target_os = "macos"))]
    mod imp {
        pub fn request_authorization(_: impl Fn(bool) + 'static) -> Result<(), String> { Err("Unsupported.".into()) }
        pub fn deliver(_: &str, _: &str, _: bool) -> Result<(), String> { Err("Unsupported.".into()) }
    }
    pub use imp::deliver;

    /// Fire-and-forget permission request (called when the user turns notifications on).
    pub fn request_permission() {
        let _ = imp::request_authorization(|_| {});
    }

    /// Ask (prompting once if undecided) and report whether notifications are allowed.
    pub async fn ensure_permission() -> Result<bool, String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Mutex::new(Some(tx));
        imp::request_authorization(move |g| {
            if let Some(t) = tx.lock().ok().and_then(|mut t| t.take()) {
                let _ = t.send(g);
            }
        })?;
        tokio::time::timeout(std::time::Duration::from_secs(120), rx).await.map_err(|_| "No answer from the system.".to_string())?.map_err(|_| "No answer from the system.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::link_allowed;

    #[test]
    fn only_web_and_mail_links() {
        for ok in ["https://example.com/a?b=c#d", "http://localhost:8080", "HTTPS://Example.com", "mailto:a@b.org?subject=x", " https://zoom.us/oauth/authorize?x=1 "] {
            assert!(link_allowed(ok), "{ok}");
        }
        for bad in ["", "file:///etc/passwd", "javascript:alert(1)", "data:text/html,x", "tel:123", "ssh://h", "x-apple.systempreferences:", "https://", "https:///x", "https:example.com", "https://a b.com", "https://a.com/\nx", "mailto:", "/Applications/Foo.app", "https://@evil"] {
            assert!(!link_allowed(bad), "{bad:?}");
        }
    }
}
