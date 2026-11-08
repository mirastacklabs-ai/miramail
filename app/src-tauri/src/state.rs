//! Shared app state: storage, secrets, per-account IMAP locks and INBOX watchers.

use crate::bgsync::{self, Persisted, SyncSettings};
use crate::live::{LiveEntry, LiveStatus};
use crate::wire::{NewMailEvent, StatusEvent};
use mailcore::{AccountConfig, ImapClient, SecretStore, Storage, WatchEvent};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, watch, Mutex as AsyncMutex, OwnedMutexGuard};

pub const INBOX: &str = "INBOX";

pub struct AppState {
    pub storage: Arc<Storage>,
    pub secrets: Arc<dyn SecretStore>,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    watchers: Mutex<HashMap<String, watch::Sender<bool>>>,
    /// Last INBOX watcher status per account (the sync loop only runs while "connected").
    inbox_status: Mutex<HashMap<String, &'static str>>,
    /// Live (user-chosen) folder watchers: (account, folder) -> entry.
    live: Mutex<HashMap<(String, String), LiveEntry>>,
    /// Persisted live folders per account.
    live_cfg: Mutex<BTreeMap<String, Vec<String>>>,
    pub sync_cfg: watch::Sender<SyncSettings>,
    settings_path: Mutex<Option<PathBuf>>,
}

/// An authenticated IMAP connection that holds the account's lock until closed/dropped.
pub struct Conn {
    pub client: ImapClient,
    _guard: OwnedMutexGuard<()>,
}

impl Conn {
    pub async fn close(self) {
        let _ = self.client.logout().await;
    }
}

pub fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl AppState {
    pub fn new(storage: Arc<Storage>, secrets: Arc<dyn SecretStore>) -> Self {
        AppState {
            storage,
            secrets,
            locks: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            inbox_status: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            live_cfg: Mutex::new(BTreeMap::new()),
            sync_cfg: watch::channel(SyncSettings::default()).0,
            settings_path: Mutex::new(None),
        }
    }

    /// Load `sync_settings.json` (defaults if missing/corrupt) and remember where to save it.
    pub fn init_config(&self, path: PathBuf) {
        let p = bgsync::load_config(&path);
        self.sync_cfg.send_replace(p.settings);
        *self.live_cfg.lock().unwrap_or_else(|e| e.into_inner()) = p.live_folders;
        *self.settings_path.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    }

    /// Write the current settings + live folders (no-op when no path was set, e.g. in tests).
    pub fn persist_config(&self) -> Result<(), String> {
        let Some(path) = self.settings_path.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return Ok(()) };
        let p = Persisted {
            settings: *self.sync_cfg.borrow(),
            live_folders: self.live_cfg.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        };
        bgsync::save_config(&path, &p).map_err(|e| format!("Couldn't save settings: {e}"))
    }

    pub fn live_folders(&self, account: &str) -> Vec<String> {
        self.live_cfg.lock().unwrap_or_else(|e| e.into_inner()).get(account).cloned().unwrap_or_default()
    }

    pub fn set_live_cfg(&self, account: &str, folder: &str, on: bool) {
        let mut m = self.live_cfg.lock().unwrap_or_else(|e| e.into_inner());
        let v = m.entry(account.to_string()).or_default();
        v.retain(|f| f != folder);
        if on {
            v.push(folder.to_string());
        }
        if v.is_empty() {
            m.remove(account);
        }
    }

    /// Current "initial messages per folder" setting (newest N fetched by a folder's first sync).
    pub fn initial_sync_limit(&self) -> u32 {
        self.sync_cfg.borrow().initial_sync_messages
    }

    pub fn inbox_status(&self, account: &str) -> Option<&'static str> {
        self.inbox_status.lock().unwrap_or_else(|e| e.into_inner()).get(account).copied()
    }

    pub fn register_live(&self, account: &str, folder: &str, entry: LiveEntry) {
        let old = self.live.lock().unwrap_or_else(|e| e.into_inner()).insert((account.to_string(), folder.to_string()), entry);
        if let Some(o) = old {
            let _ = o.stop.send(true);
        }
    }

    pub fn stop_live(&self, account: &str, folder: &str) {
        let e = self.live.lock().unwrap_or_else(|e| e.into_inner()).remove(&(account.to_string(), folder.to_string()));
        if let Some(e) = e {
            let _ = e.stop.send(true);
        }
    }

    pub fn live_states(&self, account: &str) -> HashMap<String, LiveStatus> {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|((a, _), _)| a == account)
            .map(|((_, f), e)| (f.clone(), e.status.borrow().clone()))
            .collect()
    }

    /// Account removed: stop its live watchers and forget its live folders.
    pub fn forget_account(&self, account: &str) {
        let keys: Vec<String> =
            self.live.lock().unwrap_or_else(|e| e.into_inner()).keys().filter(|(a, _)| a == account).map(|(_, f)| f.clone()).collect();
        for f in keys {
            self.stop_live(account, &f);
        }
        self.inbox_status.lock().unwrap_or_else(|e| e.into_inner()).remove(account);
        let had = self.live_cfg.lock().unwrap_or_else(|e| e.into_inner()).remove(account).is_some();
        if had {
            let _ = self.persist_config();
        }
    }

    fn lock_for(&self, account: &str) -> Arc<AsyncMutex<()>> {
        let mut m = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        m.entry(account.to_string()).or_default().clone()
    }

    pub fn account(&self, email: &str) -> Result<AccountConfig, String> {
        self.storage
            .get_account(email)
            .map_err(err)?
            .ok_or_else(|| "That account no longer exists.".to_string())
    }

    pub fn password(&self, email: &str) -> Result<String, String> {
        self.secrets
            .get(email)
            .map_err(err)?
            .ok_or_else(|| "No saved password for this account. Remove it and add it again.".to_string())
    }

    /// Connect + LOGIN, serialised per account so we never log in twice at once.
    pub async fn connect(&self, email: &str) -> Result<Conn, String> {
        let cfg = self.account(email)?;
        let password = self.password(email)?;
        let guard = self.lock_for(email).lock_owned().await;
        let client = ImapClient::connect(&cfg.imap, &cfg.username, &password).await.map_err(err)?;
        Ok(Conn { client, _guard: guard })
    }

    pub fn stop_watcher(&self, email: &str) {
        let tx = self.watchers.lock().unwrap_or_else(|p| p.into_inner()).remove(email);
        if let Some(tx) = tx {
            let _ = tx.send(true);
        }
        self.forget_account(email);
    }

    fn register_watcher(&self, email: &str, tx: watch::Sender<bool>) {
        let old = self.watchers.lock().unwrap_or_else(|p| p.into_inner()).insert(email.to_string(), tx);
        if let Some(old) = old {
            let _ = old.send(true);
        }
    }
}

/// Map a watcher event to the `mail://status` state, if it changes the status.
pub fn status_of(ev: &WatchEvent) -> Option<&'static str> {
    match ev {
        WatchEvent::Connected { .. } => Some("connected"),
        WatchEvent::Disconnected { .. } => Some("offline"),
        WatchEvent::Reconnecting { .. } => Some("reconnecting"),
        WatchEvent::NewMail => None,
    }
}

/// Start (or restart) the INBOX watcher for an account.
pub fn start_watcher(app: &AppHandle, cfg: AccountConfig, password: String) {
    let state = app.state::<AppState>();
    let (stop_tx, stop_rx) = watch::channel(false);
    state.register_watcher(&cfg.email, stop_tx);

    let (tx, mut rx) = mpsc::channel::<WatchEvent>(16);
    let email = cfg.email.clone();
    let stop_for_sync = stop_rx.clone();
    let stop_for_live = stop_rx.clone();
    let (pw_live, cfg_live) = (password.clone(), cfg.clone());
    tauri::async_runtime::spawn(mailcore::watch::watch_folder(
        cfg.imap.clone(),
        cfg.username.clone(),
        password,
        INBOX.to_string(),
        tx,
        stop_rx,
    ));

    // Round-robin sync of non-INBOX folders (stops with the watcher).
    tauri::async_runtime::spawn(bgsync::run_loop(
        bgsync::real::AppOps { app: app.clone(), email: cfg.email.clone() },
        state.sync_cfg.subscribe(),
        stop_for_sync,
    ));
    crate::live::restore_live(app, cfg_live, pw_live, stop_for_live);

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let Some(s) = status_of(&ev) {
                app.state::<AppState>().inbox_status.lock().unwrap_or_else(|e| e.into_inner()).insert(email.clone(), s);
                let _ = app.emit("mail://status", StatusEvent { account: email.clone(), state: s });
                continue;
            }
            // NewMail: pull headers into the cache first so the UI finds them.
            let st = app.state::<AppState>();
            if let Ok(mut conn) = st.connect(&email).await {
                let _ = conn.client.sync_folder(&st.storage, &email, INBOX, st.initial_sync_limit()).await;
                conn.close().await;
            }
            let _ = app.emit("mail://new", NewMailEvent { account: email.clone(), folder: INBOX.to_string() });
            // Local rules / template auto-reply (no-ops unless the user turned something on).
            crate::automation::notify(&app, &email, INBOX);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::{InMemorySecretStore, ServerConfig, Tls};

    fn state() -> AppState {
        AppState::new(Arc::new(Storage::open_in_memory().unwrap()), Arc::new(InMemorySecretStore::new()))
    }

    #[test]
    fn missing_account_and_password_are_user_errors() {
        let s = state();
        assert!(s.account("nobody@x.org").unwrap_err().contains("no longer exists"));
        assert!(s.password("nobody@x.org").unwrap_err().contains("No saved password"));
        s.secrets.set("a@x.org", "pw").unwrap();
        assert_eq!(s.password("a@x.org").unwrap(), "pw");
    }

    #[test]
    fn account_roundtrip_in_memory() {
        let s = state();
        let c = AccountConfig {
            email: "a@x.org".into(),
            imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "s".into(), port: 465, tls: Tls::Implicit },
            username: "a@x.org".into(),
        };
        s.storage.upsert_account(&c).unwrap();
        assert_eq!(s.account("a@x.org").unwrap(), c);
    }

    #[test]
    fn same_account_shares_a_lock() {
        let s = state();
        assert!(Arc::ptr_eq(&s.lock_for("a"), &s.lock_for("a")));
        assert!(!Arc::ptr_eq(&s.lock_for("a"), &s.lock_for("b")));
    }

    #[test]
    fn status_mapping() {
        assert_eq!(status_of(&WatchEvent::Connected { idle: true }), Some("connected"));
        assert_eq!(status_of(&WatchEvent::Disconnected { reason: "x".into() }), Some("offline"));
        assert_eq!(status_of(&WatchEvent::Reconnecting { attempt: 1, delay_secs: 1 }), Some("reconnecting"));
        assert_eq!(status_of(&WatchEvent::NewMail), None);
    }
}
