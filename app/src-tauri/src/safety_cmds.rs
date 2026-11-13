//! Mail safety report for the open message (docs/SAFETY_CHECKS.md, Tier 1).
//!
//! Read-only and local (the body is re-fetched over the interactive lane only if it is no longer available): it parses the cached raw message with `mailcore::safety` and counts earlier mail from the
//! sender in the local cache. No network, no AI, no write. The report is plain data (codes, severities, plain-text
//! sentences); the Reader renders it as text, never as HTML.

use crate::composer::ComposeStore;
use crate::state::{err, AppState};
use mailcore::safety::{analyze, build_input, SafetyReport};
use tauri::State;

type CmdResult<T> = Result<T, String>;

/// Safety report for one message. Fails only when its body cannot be had (the Reader then keeps the message's links
/// inert and offers a retry).
/// `async` + `spawn_blocking`: a plain command runs on the main thread, and parsing and sanitising a link-heavy mail
/// there froze the UI (WKWebView could not paint) until the analysis finished.
#[tauri::command]
pub async fn safety_report(
    state: State<'_, AppState>,
    compose: State<'_, ComposeStore>,
    account: String,
    folder: String,
    uid: u32,
    uidvalidity: Option<u32>,
) -> CmdResult<SafetyReport> {
    // The page passes the shown message's UIDVALIDITY: a renumbered folder never lets this check another message.
    crate::commands::check_original(&state, &account, &folder, uid, uidvalidity)?;
    let storage = state.storage.clone();
    let identities: Vec<String> = compose.list_identities().unwrap_or_default().into_iter().filter(|i| i.account == account).map(|i| i.address).collect();
    let t0 = std::time::Instant::now();
    // The offline cache, else the Reader's recent fetch (a body the cache policy keeps off disk), else fetched again over
    // the interactive lane (user's own mail server, read-only, only while the user is present).
    let raw = crate::commands::reader_raw(&state, &account, &folder, uid).await?;
    tauri::async_runtime::spawn_blocking(move || {
        let parsed = mailcore::parse_message(&raw).map_err(err)?;
        let extra = mailcore::mime::parse_extra_headers(&raw);
        let hosts = crate::organise::auth_hosts(&storage, &account);
        let mut own: Vec<String> = storage.list_accounts().map_err(err)?.into_iter().map(|a| a.email).collect();
        own.extend(identities);
        let earlier = parsed.headers.from.first().and_then(|f| storage.count_from(&f.address).ok()).map(|(n, _)| n);
        let report = analyze(&build_input(&parsed, &extra, &own, &hosts, earlier));
        let links = parsed.html.as_deref().map(|h| h.matches("href").count()).unwrap_or(0);
        perf::log(&format!("perf: safety_report: links~{links} bytes={} took={}ms", raw.len(), t0.elapsed().as_millis()));
        Ok(report)
    })
    .await
    .map_err(|e| format!("safety check stopped: {e}"))?
}

/// Durations and counts only, sent by the UI (tap to body painted, tap to safety result). Never any content.
#[tauri::command]
pub fn perf_log(event: String, ms: u32) {
    let ev: String = event.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(40).collect();
    perf::log(&format!("perf: ui {ev} {ms}ms"));
}

/// Timing lines. iOS: appended (UTC timestamp first) to `<app data>/perf.log` (256 KB cap, rolled to `perf.log.1`),
/// because the unified log does not reach `idevicesyslog` on iOS 26; NSLog is kept as well. Desktop: stderr in debug
/// builds only. Callers pass only durations, counts and byte sizes, never identifiers.
pub mod perf {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    #[cfg(target_os = "ios")]
    const CAP: u64 = 256 * 1024;
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    static LOCK: Mutex<()> = Mutex::new(());

    /// Where the log goes (the app data dir). Call once at setup.
    #[cfg_attr(not(target_os = "ios"), allow(dead_code))]
    pub fn init(dir: &Path) {
        let _ = FILE.set(dir.join("perf.log"));
    }

    /// Append one line to `path`; past `cap` bytes the file is renamed to `<path>.1` (replacing it) and a new one starts.
    /// IO errors are ignored.
    #[cfg_attr(not(any(test, target_os = "ios")), allow(dead_code))]
    pub(super) fn append_capped(path: &Path, line: &str, cap: u64) {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if std::fs::metadata(path).map(|m| m.len() >= cap).unwrap_or(false) {
            let mut old = path.as_os_str().to_owned();
            old.push(".1");
            let _ = std::fs::rename(path, PathBuf::from(old));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }

    #[cfg(target_os = "ios")]
    pub fn log(msg: &str) {
        use objc2_foundation::NSString;
        extern "C" {
            fn NSLog(format: *const NSString, ...);
        }
        if let Some(p) = FILE.get() {
            let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
            append_capped(p, &format!("{ts} {msg}"), CAP);
        }
        let fmt = NSString::from_str(&msg.replace('%', "%%"));
        // SAFETY: NSLog is variadic; the format string has no conversions (all '%' escaped), so no arguments are read.
        unsafe { NSLog(&*fmt) };
    }
    #[cfg(not(target_os = "ios"))]
    pub fn log(msg: &str) {
        #[cfg(debug_assertions)]
        eprintln!("{msg}");
        #[cfg(not(debug_assertions))]
        let _ = msg;
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rolls_over_at_cap() {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/perf-test").join(std::process::id().to_string());
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::remove_file(dir.join("perf.log"));
            let p = dir.join("perf.log");
            for i in 0..20 {
                append_capped(&p, &format!("line {i:04}"), 100);
            }
            assert!(p.exists() && dir.join("perf.log.1").exists());
            assert!(std::fs::metadata(&p).unwrap().len() < 200);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
