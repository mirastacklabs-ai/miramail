//! User-chosen "live" folders: each gets its own `mailcore::watch::watch_folder` (IDLE, polling
//! fallback) = one extra IMAP connection. Pure helpers are unit-tested; the glue is at the bottom.

use mailcore::{Folder, WatchEvent};
use serde::Serialize;
use std::time::Duration;

/// Gap between starting restored live watchers, so providers' connection limits are not tripped.
pub const STAGGER_STEP: Duration = Duration::from_millis(500);
/// How long enabling a live folder waits for the server's first answer.
pub const FIRST_OUTCOME_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TEXT: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveState {
    /// Started, no server answer yet.
    Connecting,
    Live,
    Polling,
    Error,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveStatus {
    pub state: LiveState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl LiveStatus {
    pub fn new(state: LiveState) -> Self {
        LiveStatus { state, message: None }
    }
    pub fn error(raw: &str) -> Self {
        LiveStatus { state: LiveState::Error, message: Some(friendly_error(raw)) }
    }
}

/// Plain text only: control characters stripped, whitespace collapsed, truncated to ~300 chars.
pub fn sanitize_server_text(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_TEXT {
        let mut t: String = collapsed.chars().take(MAX_TEXT).collect();
        t.push('…');
        t
    } else {
        collapsed
    }
}

/// One friendly line, followed by the server's own text (sanitised, verbatim otherwise).
pub fn friendly_error(raw: &str) -> String {
    let server = sanitize_server_text(raw);
    let l = server.to_lowercase();
    let hint = if l.contains("too many") || l.contains("simultaneous") || l.contains("connection limit") || l.contains("max connections") {
        "The server limits how many connections you can have open. Turn off another live folder and try again."
    } else if l.contains("authentication") || l.contains("login") || l.contains("authenticationfailed") {
        "The server rejected the login for this folder."
    } else if l.contains("certificate") || l.contains("tls") {
        "The secure connection to the server failed."
    } else if l.contains("timed out") || l.contains("timeout") {
        "The server didn't answer in time."
    } else if l.contains("nonexistent") || l.contains("doesn't exist") || l.contains("does not exist") || l.contains("no such") {
        "The server says this folder doesn't exist."
    } else {
        "The server wouldn't keep this folder live."
    };
    if server.is_empty() { hint.to_string() } else { format!("{hint} Server said: {server}") }
}

/// Map a watcher event to a new status, if it changes it. Reconnecting/NewMail keep the last status.
pub fn status_of_event(ev: &WatchEvent) -> Option<LiveStatus> {
    match ev {
        WatchEvent::Connected { idle: true } => Some(LiveStatus::new(LiveState::Live)),
        WatchEvent::Connected { idle: false } => Some(LiveStatus {
            state: LiveState::Polling,
            message: Some("Server doesn't support push; checking every 60 s.".into()),
        }),
        WatchEvent::Disconnected { reason } => Some(LiveStatus::error(reason)),
        WatchEvent::Reconnecting { .. } | WatchEvent::NewMail => None,
    }
}

/// Start offsets for `n` restored watchers: step, 2*step, ...
pub fn stagger_offsets(n: usize, step: Duration) -> Vec<Duration> {
    (1..=n as u32).map(|i| step * i).collect()
}

/// Pre-flight checks for enabling `folder` as live. `live` = the account's current live folders.
pub fn check_enable(live: &[String], folder: &str, max: u32, folders: &[Folder]) -> Result<(), String> {
    if folder.eq_ignore_ascii_case("INBOX") {
        return Err("INBOX is always live.".into());
    }
    match folders.iter().find(|f| f.name == folder) {
        Some(f) if f.selectable => {}
        Some(_) => return Err("This folder can't be opened, so it can't be kept live.".into()),
        None => return Err("That folder no longer exists. Refresh the folder list.".into()),
    }
    if !live.iter().any(|f| f == folder) && live.len() as u32 >= max {
        return Err(format!(
            "You can keep at most {max} folders live (each one holds an extra connection to your mail server, and providers limit those). Turn one off first, or raise the limit in Settings."
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- glue

use crate::bgsync::stopped;
use crate::state::AppState;
use crate::wire::{LiveEvent, NewMailEvent};
use mailcore::AccountConfig;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, watch};

pub struct LiveEntry {
    pub stop: watch::Sender<bool>,
    pub status: Arc<watch::Sender<LiveStatus>>,
}

/// Start (or restart) a live watcher; returns a receiver for its status.
pub fn start_live(app: &AppHandle, cfg: &AccountConfig, password: String, folder: String) -> watch::Receiver<LiveStatus> {
    let state = app.state::<AppState>();
    let (stop_tx, stop_rx) = watch::channel(false);
    let status = Arc::new(watch::channel(LiveStatus::new(LiveState::Connecting)).0);
    let rx = status.subscribe();
    state.register_live(&cfg.email, &folder, LiveEntry { stop: stop_tx, status: status.clone() });

    let (tx, mut events) = mpsc::channel::<WatchEvent>(16);
    tauri::async_runtime::spawn(mailcore::watch::watch_folder(
        cfg.imap.clone(),
        cfg.username.clone(),
        password,
        folder.clone(),
        tx,
        stop_rx,
    ));

    let (app, email) = (app.clone(), cfg.email.clone());
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = events.recv().await {
            if let Some(s) = status_of_event(&ev) {
                if *status.borrow() != s {
                    status.send_replace(s.clone());
                    let _ = app.emit(
                        "mail://live",
                        LiveEvent { account: email.clone(), folder: folder.clone(), state: s.state, message: s.message },
                    );
                }
                continue;
            }
            // iPhone: nothing while the user is away (the watcher is being stopped then; presence.rs).
            if ev == WatchEvent::NewMail && crate::presence::user_present() {
                let st = app.state::<AppState>();
                if let Ok(mut conn) = st.connect(&email).await {
                    if crate::presence::user_present() {
                        let _ = conn.client.sync_folder(&st.storage, &email, &folder, st.initial_sync_limit()).await;
                    }
                    conn.close().await;
                }
                let _ = app.emit("mail://new", NewMailEvent { account: email.clone(), folder: folder.clone() });
                crate::prefetch::kick(&app, &email, &folder, crate::prefetch::RULES_GRACE);
                crate::automation::notify(&app, &email, &folder);
            }
        }
    });
    rx
}

/// Restore the account's persisted live folders, one every `STAGGER_STEP`, until `stop` fires.
pub fn restore_live(app: &AppHandle, cfg: AccountConfig, password: String, stop: watch::Receiver<bool>) {
    let folders = app.state::<AppState>().live_folders(&cfg.email);
    if folders.is_empty() {
        return;
    }
    let offsets = stagger_offsets(folders.len(), STAGGER_STEP);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut stop = stop;
        let mut prev = Duration::ZERO;
        for (folder, at) in folders.into_iter().zip(offsets) {
            tokio::select! {
                _ = tokio::time::sleep(at - prev) => {}
                _ = stopped(&mut stop) => return,
            }
            prev = at;
            start_live(&app, &cfg, password.clone(), folder);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::FolderRole;

    fn folder(name: &str, selectable: bool) -> Folder {
        Folder { name: name.into(), delimiter: None, role: FolderRole::Other, selectable }
    }

    #[test]
    fn sanitises_server_text() {
        let s = sanitize_server_text("NO [LIMIT]\r\n too   many\x1b[31m connections\x00");
        assert_eq!(s, "NO [LIMIT] too many [31m connections");
        assert!(!s.chars().any(|c| c.is_control()));
        let long = "x".repeat(1000);
        let t = sanitize_server_text(&long);
        assert_eq!(t.chars().count(), 301);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn friendly_lines() {
        let m = friendly_error("imap: NO Too many simultaneous connections");
        assert!(m.starts_with("The server limits how many connections"));
        assert!(m.contains("Server said: imap: NO Too many simultaneous connections"));
        assert!(friendly_error("authentication failed").starts_with("The server rejected the login"));
        assert!(friendly_error("tls: bad certificate").starts_with("The secure connection"));
        assert!(friendly_error("timed out: select").starts_with("The server didn't answer"));
        assert_eq!(friendly_error("   "), "The server wouldn't keep this folder live.");
    }

    #[test]
    fn maps_watcher_outcomes() {
        assert_eq!(status_of_event(&WatchEvent::Connected { idle: true }), Some(LiveStatus::new(LiveState::Live)));
        let p = status_of_event(&WatchEvent::Connected { idle: false }).unwrap();
        assert_eq!(p.state, LiveState::Polling);
        assert!(p.message.unwrap().contains("doesn't support push"));
        let e = status_of_event(&WatchEvent::Disconnected { reason: "imap: BYE too many connections".into() }).unwrap();
        assert_eq!(e.state, LiveState::Error);
        assert!(e.message.unwrap().contains("BYE too many connections"));
        assert_eq!(status_of_event(&WatchEvent::Reconnecting { attempt: 1, delay_secs: 1 }), None);
        assert_eq!(status_of_event(&WatchEvent::NewMail), None);
    }

    #[test]
    fn stagger_schedule() {
        assert!(stagger_offsets(0, STAGGER_STEP).is_empty());
        let v = stagger_offsets(3, STAGGER_STEP);
        assert_eq!(v, vec![Duration::from_millis(500), Duration::from_millis(1000), Duration::from_millis(1500)]);
    }

    #[test]
    fn enable_checks_and_max() {
        let all = vec![folder("A", true), folder("B", true), folder("C", true), folder("Parent", false)];
        let live: Vec<String> = vec!["A".into(), "B".into()];
        assert!(check_enable(&live, "C", 3, &all).is_ok());
        assert!(check_enable(&live, "C", 2, &all).unwrap_err().contains("at most 2"));
        assert!(check_enable(&live, "A", 2, &all).is_ok(), "already live is idempotent");
        assert!(check_enable(&[], "inbox", 5, &all).unwrap_err().contains("always live"));
        assert!(check_enable(&[], "Parent", 5, &all).unwrap_err().contains("can't be opened"));
        assert!(check_enable(&[], "Gone", 5, &all).unwrap_err().contains("no longer exists"));
    }
}
