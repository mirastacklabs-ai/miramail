//! Link clicks inside the sandboxed message frame.
//!
//! WebKit never runs event listeners whose target document sits in a frame that may not run scripts (measured with a
//! WKWebView harness: 0 hits in the sandboxed frame, 1 in an unsandboxed control), even when the parent added them. So
//! the Reader marks links `target="_blank"` and the iframe gets `allow-popups`; a click then reaches the navigation
//! policy here as a new-window action. We cancel it (no popup is ever created) and, for web / mail links, tell THAT
//! webview's frontend to run its "Open link?" flow. Everything else the webview tries to load is refused.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::webview::{DownloadEvent, NewWindowResponse};
use tauri::{AppHandle, Emitter, Manager, Runtime, Url, WebviewWindowBuilder};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Nav {
    /// The app's own content (and its srcdoc/data/blob frames): let the webview load it.
    Allow,
    /// An http(s)/mailto link the user clicked: cancel the load, hand it to the frontend.
    Forward,
    /// Anything else: cancel.
    Deny,
}

pub const EVENT: &str = "mail://link";

pub fn classify(url: &Url, dev: bool) -> Nav {
    match url.scheme() {
        "tauri" | "about" | "data" | "blob" => Nav::Allow,
        "http" if dev && url.host_str() == Some("localhost") && url.port() == Some(1420) => Nav::Allow,
        "http" | "https" | "mailto" => {
            if !url.username().is_empty() || url.password().is_some() {
                return Nav::Deny;
            }
            if crate::native::link_allowed(url.as_str()) { Nav::Forward } else { Nav::Deny }
        }
        _ => Nav::Deny,
    }
}

pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("linknav")
        .on_navigation(|webview, url| match classify(url, cfg!(debug_assertions)) {
            Nav::Allow => true,
            Nav::Forward => {
                // Only to the webview that navigated; the payload is the URL itself, never logged.
                let _ = webview.emit_to(webview.label(), EVENT, url.as_str().to_string());
                false
            }
            Nav::Deny => false,
        })
        .build()
}

/// What a "new window" request (WebKit's context menu "Open Link" / "Open Link in New Window" skip the navigation policy and
/// only call the UI delegate) should do: forward web / mail links to the frontend, never open a window for anything.
pub fn new_window_forwards(url: &Url) -> bool {
    classify(url, false) == Nav::Forward
}

/// Apply to EVERY window builder (the plugin hook above has no new-window counterpart): link requests from the context
/// menu reach the same "Open link?" flow, no window is ever created, and WebKit-initiated downloads are refused
/// (attachments are saved by Rust commands, never by the webview).
pub fn harden<'a, R: Runtime, M: Manager<R>>(b: WebviewWindowBuilder<'a, R, M>, app: &AppHandle<R>, label: &str) -> WebviewWindowBuilder<'a, R, M> {
    let app = app.clone();
    let label = label.to_string();
    b.on_new_window(move |url, _features| {
        if new_window_forwards(&url) {
            let _ = app.emit_to(label.as_str(), EVENT, url.as_str().to_string());
        }
        NewWindowResponse::Deny
    })
    .on_download(|_, e: DownloadEvent<'_>| matches!(e, DownloadEvent::Finished { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str, dev: bool) -> Nav {
        classify(&Url::parse(s).unwrap(), dev)
    }

    #[test]
    fn web_and_mail_are_forwarded() {
        assert_eq!(c("https://example.org/a?b=1#c", false), Nav::Forward);
        assert_eq!(c("http://example.org/", false), Nav::Forward);
        assert_eq!(c("mailto:a@b.com?subject=hi", false), Nav::Forward);
        assert_eq!(c("http://localhost:1420/", false), Nav::Forward);
    }

    #[test]
    fn app_content_is_allowed() {
        assert_eq!(c("tauri://localhost/index.html#pane=x", false), Nav::Allow);
        assert_eq!(c("tauri://localhost/widget.html", false), Nav::Allow);
        assert_eq!(c("about:srcdoc", false), Nav::Allow);
        assert_eq!(c("about:blank", false), Nav::Allow);
        assert_eq!(c("data:text/html,hi", false), Nav::Allow);
        assert_eq!(c("blob:tauri://localhost/1234", false), Nav::Allow);
    }

    #[test]
    fn dev_url_only_in_dev() {
        assert_eq!(c("http://localhost:1420/index.html#pane=x", true), Nav::Allow);
        assert_eq!(c("http://localhost:1421/", true), Nav::Forward);
        assert_eq!(c("https://localhost:1420/", true), Nav::Forward);
    }

    #[test]
    fn everything_else_is_denied() {
        assert_eq!(c("javascript:alert(1)", false), Nav::Deny);
        assert_eq!(c("file:///etc/passwd", false), Nav::Deny);
        assert_eq!(c("myapp://x/y", false), Nav::Deny);
        assert_eq!(c("ftp://example.org/", false), Nav::Deny);
        assert_eq!(c("https://user:pw@example.org/", false), Nav::Deny);
        assert_eq!(c("https://user@example.org/", false), Nav::Deny);
    }

    #[test]
    fn new_window_forwards_only_web_and_mail() {
        let f = |s: &str| new_window_forwards(&Url::parse(s).unwrap());
        assert!(f("https://example.org/") && f("http://example.org/") && f("mailto:a@b.com"));
        assert!(!f("about:srcdoc") && !f("about:blank") && !f("tauri://localhost/") && !f("javascript:alert(1)") && !f("file:///x") && !f("https://u:p@example.org/"));
    }
}
