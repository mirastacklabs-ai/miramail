//! Shared app state: storage, secrets, per-account IMAP locks and INBOX watchers.
//!
//! IMAP connections per account (the server budget):
//! - INBOX watcher: 1 (IDLE / poll, its own connection);
//! - live folders: one each, at most `max_live_folders` (default 5, max 10);
//! - background lane: 1, through [`AppState::connect`] (serialised per account: folder sync loop, new-mail sync, user
//!   commands that change mail, outbox, snooze, Drafts mirror, MIRA tools, ...);
//! - interactive lane: 1, through [`AppState::interactive`]: the Reader's body fetch (uncached message), attachment
//!   fetch and the deferred mark-seen. Kept warm for [`LANE_IDLE`] after its last use so the next open skips
//!   TCP / TLS / LOGIN (and SELECT when it is the same folder); closed when idle, on error or timeout, on account
//!   removal and when the iPhone user leaves the app.
//!
//! So at most 3 + live folders at once (default 8, worst 13; before the interactive lane: 2 + live).

use crate::bgsync::{self, Persisted, SyncSettings};
use crate::live::{LiveEntry, LiveStatus};
use crate::wire::{NewMailEvent, StatusEvent};
use mailcore::{AccountConfig, ImapClient, SecretStore, Storage, WatchEvent};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, watch, Mutex as AsyncMutex, OwnedMutexGuard};

pub const INBOX: &str = "INBOX";

pub struct AppState {
    pub storage: Arc<Storage>,
    pub secrets: Arc<dyn SecretStore>,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// Interactive lane per account (see the module doc).
    lanes: Mutex<HashMap<String, Arc<Lane>>>,
    /// Bumped when the iPhone user leaves: a warm session from before is never reused.
    lane_gen: Arc<AtomicU64>,
    /// Bumped by every user flag change (set_flag, bulk_flag): a deferred mark-seen retry never undoes one.
    flag_epoch: AtomicU64,
    /// Bodies the Reader fetched that the offline cache did NOT keep (cache policy), in memory only, so the local checks
    /// (message_analysis, safety_report) and an attachment save find them without another fetch. Cleared when the
    /// iPhone user leaves and on account removal.
    recent_bodies: Mutex<Vec<RecentBody>>,
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

/// A warm interactive session is reused only within this time of its last use.
pub const LANE_IDLE: Duration = Duration::from_secs(60);
/// NOOP before reusing a warm session: a dead one (server timeout, network change) is found within this and replaced.
const LANE_PROBE: Duration = Duration::from_secs(3);
/// Polite LOGOUT of a session we are done with (never awaited by a user action).
const LOGOUT_BUDGET: Duration = Duration::from_secs(5);
const RECENT_BODIES: usize = 2;
/// Largest body kept in memory (iPhone: small, so the app stays far from the memory limit).
#[cfg(target_os = "ios")]
const RECENT_BODY_MAX: usize = 8 * 1024 * 1024;
#[cfg(not(target_os = "ios"))]
const RECENT_BODY_MAX: usize = 32 * 1024 * 1024;

struct RecentBody {
    account: String,
    folder: String,
    uid: u32,
    uidvalidity: u32,
    raw: Arc<[u8]>,
}

struct Warm {
    client: ImapClient,
    used: Instant,
    /// Wall clock as well: the monotonic clock may not advance while the Mac sleeps.
    used_wall: SystemTime,
    gen: u64,
    token: u64,
}

/// One account's interactive lane: at most one session, used by one caller at a time.
#[derive(Default)]
pub struct Lane {
    slot: Arc<AsyncMutex<Option<Warm>>>,
    tokens: AtomicU64,
    closed: AtomicBool,
}

/// May a warm session be reused? Same presence generation, and used less than [`LANE_IDLE`] ago by both clocks.
pub fn warm_usable(idle: Duration, wall_idle: Option<Duration>, same_gen: bool) -> bool {
    same_gen && idle < LANE_IDLE && wall_idle.is_some_and(|w| w < LANE_IDLE)
}

fn spawn_logout(client: ImapClient) {
    tauri::async_runtime::spawn(async move {
        let _ = tokio::time::timeout(LOGOUT_BUDGET, client.logout()).await;
    });
}

fn close_lane(lane: Arc<Lane>, why: &'static str) {
    tauri::async_runtime::spawn(async move {
        let w = lane.slot.lock().await.take();
        if let Some(w) = w {
            let _ = tokio::time::timeout(LOGOUT_BUDGET, w.client.logout()).await;
            crate::safety_cmds::perf::log(&format!("perf: imap_lane: lane=interactive closed={why}"));
        }
    });
}

/// The interactive lane's session, held by one caller. Call [`Interactive::finish`]: `keep = true` after a clean
/// exchange parks it warm for the next caller; dropping it without `finish` (a cancelled future) closes the socket.
pub struct Interactive {
    pub client: ImapClient,
    pub reused: bool,
    gen: u64,
    gen_now: Arc<AtomicU64>,
    lane: Arc<Lane>,
    guard: OwnedMutexGuard<Option<Warm>>,
}

impl Interactive {
    pub fn finish(self, keep: bool) {
        let Interactive { client, gen, gen_now, lane, mut guard, .. } = self;
        let keep = keep && !lane.closed.load(Ordering::SeqCst) && gen == gen_now.load(Ordering::SeqCst) && crate::presence::user_present();
        if !keep {
            drop(guard);
            spawn_logout(client);
            return;
        }
        let token = lane.tokens.fetch_add(1, Ordering::SeqCst) + 1;
        *guard = Some(Warm { client, used: Instant::now(), used_wall: SystemTime::now(), gen, token });
        drop(guard);
        let slot = lane.slot.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(LANE_IDLE).await;
            let idle = {
                let mut g = slot.lock().await;
                if g.as_ref().is_some_and(|w| w.token == token) { g.take() } else { None }
            };
            if let Some(w) = idle {
                let _ = tokio::time::timeout(LOGOUT_BUDGET, w.client.logout()).await;
                crate::safety_cmds::perf::log("perf: imap_lane: lane=interactive closed=idle");
            }
        });
    }
}

impl AppState {
    pub fn new(storage: Arc<Storage>, secrets: Arc<dyn SecretStore>) -> Self {
        AppState {
            storage,
            secrets,
            locks: Mutex::new(HashMap::new()),
            lanes: Mutex::new(HashMap::new()),
            lane_gen: Arc::new(AtomicU64::new(0)),
            recent_bodies: Mutex::new(Vec::new()),
            flag_epoch: AtomicU64::new(0),
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

    /// Account removed: stop its live watchers, close its interactive session, forget its live folders.
    pub fn forget_account(&self, account: &str) {
        let lane = self.lanes.lock().unwrap_or_else(|e| e.into_inner()).remove(account);
        {
            // Under the bodies lock: a fetch finishing right now either sees the closed lane or is purged here.
            let mut v = self.recent_bodies.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(l) = &lane {
                l.closed.store(true, Ordering::SeqCst);
            }
            v.retain(|b| b.account != account);
        }
        if let Some(lane) = lane {
            close_lane(lane, "account_removed");
        }
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

    /// Background lane: Connect + LOGIN, serialised per account so background work and mail-changing commands never
    /// overlap each other (one such connection per account; a folder sync never interleaves with a flag change).
    pub async fn connect(&self, email: &str) -> Result<Conn, String> {
        let cfg = self.account(email)?;
        let password = self.password(email)?;
        let t = std::time::Instant::now();
        let guard = self.lock_for(email).lock_owned().await;
        let lock_ms = t.elapsed().as_millis();
        let t = std::time::Instant::now();
        let client = ImapClient::connect(&cfg.imap, &cfg.username, &password).await.map_err(err)?;
        let k = client.timings;
        crate::safety_cmds::perf::log(&format!(
            "perf: imap_connect: lane=background lock_wait={lock_ms}ms tcp={}ms tls={}ms login={}ms total={}ms",
            k.tcp_ms, k.tls_ms, k.login_ms, t.elapsed().as_millis()
        ));
        Ok(Conn { client, _guard: guard })
    }

    /// The background lane's turn without a connection: waits until no background connection of this account is in
    /// use and holds that off until dropped (the deferred mark-seen, so a folder sync that read the flags before its
    /// STORE cannot write "unseen" back after it).
    pub async fn background_turn(&self, email: &str) -> OwnedMutexGuard<()> {
        self.lock_for(email).lock_owned().await
    }

    fn lane_for(&self, email: &str) -> Arc<Lane> {
        let mut m = self.lanes.lock().unwrap_or_else(|p| p.into_inner());
        m.entry(email.to_string()).or_default().clone()
    }

    /// Interactive lane: the account's user-initiated reader session, never queued behind background work (only
    /// behind another interactive call). Reuses the warm session when it is fresh and answers a NOOP, else logs in.
    pub async fn interactive(&self, email: &str) -> Result<Interactive, String> {
        // iPhone: no server contact while the user is away (get_message / attachments are refused by the command guard
        // already; this also covers the local checks' fallback fetch).
        crate::presence::require()?;
        let lane = self.lane_for(email);
        let t = Instant::now();
        let mut guard = lane.slot.clone().lock_owned().await;
        let lock_ms = t.elapsed().as_millis();
        let gen = self.lane_gen.load(Ordering::SeqCst);
        let mut stale = "none";
        if let Some(w) = guard.take() {
            if warm_usable(w.used.elapsed(), w.used_wall.elapsed().ok(), w.gen == gen) {
                let mut client = w.client;
                let tp = Instant::now();
                if matches!(tokio::time::timeout(LANE_PROBE, client.noop()).await, Ok(Ok(()))) {
                    client.timings = Default::default();
                    crate::safety_cmds::perf::log(&format!(
                        "perf: imap_connect: lane=interactive reused=true lock_wait={lock_ms}ms probe={}ms",
                        tp.elapsed().as_millis()
                    ));
                    return Ok(Interactive { client, reused: true, gen, gen_now: self.lane_gen.clone(), lane, guard });
                }
                stale = "probe_failed"; // dropped: the socket closes with it
            } else {
                stale = "expired";
                spawn_logout(w.client);
            }
        }
        let cfg = self.account(email)?;
        let password = self.password(email)?;
        let t = Instant::now();
        let mut client = ImapClient::connect(&cfg.imap, &cfg.username, &password).await.map_err(err)?;
        client.reuse_selection(true);
        let k = client.timings;
        crate::safety_cmds::perf::log(&format!(
            "perf: imap_connect: lane=interactive reused=false stale={stale} lock_wait={lock_ms}ms tcp={}ms tls={}ms login={}ms total={}ms",
            k.tcp_ms, k.tls_ms, k.login_ms, t.elapsed().as_millis()
        ));
        Ok(Interactive { client, reused: false, gen, gen_now: self.lane_gen.clone(), lane, guard })
    }

    /// iPhone, the user left: close every interactive session (and never reuse one from before), drop the bodies kept
    /// in memory.
    pub fn close_lanes(&self) {
        {
            let mut v = self.recent_bodies.lock().unwrap_or_else(|e| e.into_inner());
            self.lane_gen.fetch_add(1, Ordering::SeqCst);
            v.clear();
        }
        let lanes: Vec<Arc<Lane>> = self.lanes.lock().unwrap_or_else(|p| p.into_inner()).values().cloned().collect();
        for lane in lanes {
            close_lane(lane, "user_left");
        }
    }

    /// Remember a body `ic` fetched that the offline cache did not keep (see `recent_bodies`).
    pub fn remember_body(&self, ic: &Interactive, account: &str, folder: &str, uid: u32, uidvalidity: u32, raw: Arc<[u8]>) {
        self.keep_body(&ic.lane, ic.gen, account, folder, uid, uidvalidity, raw);
    }

    #[allow(clippy::too_many_arguments)]
    fn keep_body(&self, lane: &Lane, gen: u64, account: &str, folder: &str, uid: u32, uidvalidity: u32, raw: Arc<[u8]>) {
        if raw.len() > RECENT_BODY_MAX {
            return;
        }
        let mut v = self.recent_bodies.lock().unwrap_or_else(|e| e.into_inner());
        // Checked under the same lock `forget_account` / `close_lanes` take: a removed account or a user who left
        // in the meantime never gets a body back in memory.
        if lane.closed.load(Ordering::SeqCst) || gen != self.lane_gen.load(Ordering::SeqCst) {
            return;
        }
        v.retain(|b| !(b.account == account && b.folder == folder && b.uid == uid));
        v.push(RecentBody { account: account.into(), folder: folder.into(), uid, uidvalidity, raw });
        let n = v.len();
        if n > RECENT_BODIES {
            v.drain(..n - RECENT_BODIES);
        }
    }

    /// The raw body: the offline cache, else a recent Reader fetch of the same message (same UIDVALIDITY).
    pub fn body_for(&self, account: &str, folder: &str, uid: u32) -> Result<Option<Arc<[u8]>>, String> {
        if let Some(raw) = self.storage.get_body(account, folder, uid).map_err(err)? {
            return Ok(Some(raw.into()));
        }
        let Some(validity) = self.storage.get_message(account, folder, uid).map_err(err)?.map(|m| m.uidvalidity) else { return Ok(None) };
        let v = self.recent_bodies.lock().unwrap_or_else(|e| e.into_inner());
        Ok(v.iter()
            .find(|b| b.account == account && b.folder == folder && b.uid == uid && b.uidvalidity == validity)
            .map(|b| b.raw.clone()))
    }

    /// A user flag change (set_flag / bulk_flag) happened.
    pub fn note_flag_change(&self) {
        self.flag_epoch.fetch_add(1, Ordering::SeqCst);
    }

    pub fn flag_epoch(&self) -> u64 {
        self.flag_epoch.load(Ordering::SeqCst)
    }

    pub fn stop_watcher(&self, email: &str) {
        let tx = self.watchers.lock().unwrap_or_else(|p| p.into_inner()).remove(email);
        if let Some(tx) = tx {
            let _ = tx.send(true);
        }
        self.forget_account(email);
    }

    /// iPhone, the user left: stop every INBOX watcher (with its folder sync loop) and every live-folder watcher, keeping
    /// the settings, so nothing holds a connection or the account lock while a background check runs. They start again
    /// when the user is back (`lib.rs::on_presence`).
    pub fn pause_watchers(&self) {
        let inbox: Vec<watch::Sender<bool>> = self.watchers.lock().unwrap_or_else(|p| p.into_inner()).drain().map(|(_, tx)| tx).collect();
        for tx in inbox {
            let _ = tx.send(true);
        }
        // A stopped watcher reports nothing more: drop the stale "connected" so nothing trusts it (bgsync) until a new one reports.
        self.inbox_status.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let live: Vec<LiveEntry> = self.live.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, e)| e).collect();
        for e in live {
            let _ = e.stop.send(true);
        }
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
            // NewMail: pull headers into the cache first so the UI finds them. (iPhone: never while the user is away; the
            // watcher is being stopped then and the background check does the reading.)
            if !crate::presence::user_present() {
                continue;
            }
            let st = app.state::<AppState>();
            if let Ok(mut conn) = st.connect(&email).await {
                let _ = conn.client.sync_folder(&st.storage, &email, INBOX, st.initial_sync_limit()).await;
                conn.close().await;
            }
            let _ = app.emit("mail://new", NewMailEvent { account: email.clone(), folder: INBOX.to_string() });
            crate::prefetch::kick(&app, &email, INBOX, crate::prefetch::RULES_GRACE);
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
    fn same_account_shares_a_lane_and_lanes_are_separate_from_the_background_lock() {
        let s = state();
        assert!(Arc::ptr_eq(&s.lane_for("a"), &s.lane_for("a")));
        assert!(!Arc::ptr_eq(&s.lane_for("a"), &s.lane_for("b")));
        // Holding the background lock never blocks the interactive lane (the iPhone's 2 s lock_wait).
        let bg = s.lock_for("a").try_lock_owned().expect("background lock free");
        assert!(s.lane_for("a").slot.clone().try_lock_owned().is_ok(), "interactive lane is not behind the background lock");
        drop(bg);
    }

    #[test]
    fn warm_session_reuse_rules() {
        let s = Duration::from_secs;
        assert!(warm_usable(s(5), Some(s(5)), true));
        assert!(!warm_usable(s(5), Some(s(5)), false), "the user left in between");
        assert!(!warm_usable(LANE_IDLE, Some(s(1)), true), "idle too long");
        assert!(!warm_usable(s(1), Some(LANE_IDLE), true), "wall clock says idle too long (Mac slept)");
        assert!(!warm_usable(s(1), None, true), "wall clock went backwards");
    }

    #[test]
    fn recent_bodies_are_bounded_and_need_the_same_uidvalidity() {
        let s = state();
        let c = AccountConfig {
            email: "a@x.org".into(),
            imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "s".into(), port: 465, tls: Tls::Implicit },
            username: "a@x.org".into(),
        };
        s.storage.upsert_account(&c).unwrap();
        assert!(s.body_for("a@x.org", "INBOX", 1).unwrap().is_none(), "no summary: nothing");
        let lane = s.lane_for("a@x.org");
        let raw: Arc<[u8]> = Arc::from(&b"raw"[..]);
        for uid in 1..=3 {
            s.keep_body(&lane, 0, "a@x.org", "INBOX", uid, 9, raw.clone());
        }
        assert_eq!(s.recent_bodies.lock().unwrap().len(), RECENT_BODIES);
        assert!(s.recent_bodies.lock().unwrap().iter().all(|b| b.uid != 1), "oldest dropped");
        s.keep_body(&lane, 0, "a@x.org", "INBOX", 4, 9, vec![0u8; RECENT_BODY_MAX + 1].into());
        assert!(s.recent_bodies.lock().unwrap().iter().all(|b| b.uid != 4), "too large: not kept");
        s.forget_account("a@x.org");
        assert!(s.recent_bodies.lock().unwrap().is_empty(), "account removed: bodies dropped");
        // A fetch that finishes after the removal (its lane is closed) cannot put the body back.
        s.keep_body(&lane, 0, "a@x.org", "INBOX", 5, 9, raw.clone());
        assert!(s.recent_bodies.lock().unwrap().is_empty(), "removed account: not re-inserted");
        // The user left (iPhone): bodies dropped, and one fetched under the old generation is not kept.
        let lane_b = s.lane_for("b@x.org");
        s.keep_body(&lane_b, 0, "b@x.org", "INBOX", 1, 9, raw.clone());
        assert_eq!(s.recent_bodies.lock().unwrap().len(), 1);
        s.close_lanes();
        assert!(s.recent_bodies.lock().unwrap().is_empty(), "user left: bodies dropped");
        s.keep_body(&lane_b, 0, "b@x.org", "INBOX", 2, 9, raw);
        assert!(s.recent_bodies.lock().unwrap().is_empty(), "old generation: not kept");
    }

    #[test]
    fn pausing_stops_every_watcher_but_keeps_the_live_settings() {
        let s = state();
        let (tx, rx) = watch::channel(false);
        s.register_watcher("a@x.org", tx);
        let (ltx, lrx) = watch::channel(false);
        let status = Arc::new(watch::channel(LiveStatus::new(crate::live::LiveState::Live)).0);
        s.register_live("a@x.org", "Lists", LiveEntry { stop: ltx, status });
        s.set_live_cfg("a@x.org", "Lists", true);
        s.pause_watchers();
        assert!(*rx.borrow() && *lrx.borrow(), "both told to stop");
        assert!(s.live_states("a@x.org").is_empty());
        assert_eq!(s.live_folders("a@x.org"), vec!["Lists".to_string()], "restored when the user is back");
    }

    #[test]
    fn status_mapping() {
        assert_eq!(status_of(&WatchEvent::Connected { idle: true }), Some("connected"));
        assert_eq!(status_of(&WatchEvent::Disconnected { reason: "x".into() }), Some("offline"));
        assert_eq!(status_of(&WatchEvent::Reconnecting { attempt: 1, delay_secs: 1 }), Some("reconnecting"));
        assert_eq!(status_of(&WatchEvent::NewMail), None);
    }
}
