//! Background round-robin sync of NON-INBOX folders (INBOX stays on IDLE).
//!
//! Every `interval_seconds` a tick syncs the next `folders_per_cycle` folders over ONE short-lived
//! connection (taken through the per-account lock, so it never overlaps a user command).
//! The scheduling logic is generic over [`SyncOps`] so it is tested offline with a fake and paused time.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::sleep;

// ---------------------------------------------------------------- settings

pub const FOLDERS_RANGE: std::ops::RangeInclusive<u32> = 1..=20;
pub const INTERVAL_RANGE: std::ops::RangeInclusive<u32> = 15..=3600;
pub const LIVE_RANGE: std::ops::RangeInclusive<u32> = 1..=10;
/// Newest messages fetched per folder on its first sync.
pub const INITIAL_RANGE: std::ops::RangeInclusive<u32> = 50..=5000;
pub const DEFAULT_INITIAL: u32 = 200;
pub const SETTINGS_FILE: &str = "sync_settings.json";
/// Upper bound for tick-error backoff.
pub const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// Delay before the first tick after start (lets the INBOX watcher connect first).
pub const FIRST_TICK: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncSettings {
    pub folders_per_cycle: u32,
    pub interval_seconds: u32,
    /// Max extra live (own-connection) folders per account, INBOX not counted.
    pub max_live_folders: u32,
    /// How many of the newest messages a folder's FIRST sync fetches (applies to the next first sync).
    pub initial_sync_messages: u32,
}

impl Default for SyncSettings {
    fn default() -> Self {
        SyncSettings { folders_per_cycle: 2, interval_seconds: 60, max_live_folders: 5, initial_sync_messages: DEFAULT_INITIAL }
    }
}

impl SyncSettings {
    /// Strict validation for user input (`set_sync_settings`).
    pub fn validate(&self) -> Result<(), String> {
        if !FOLDERS_RANGE.contains(&self.folders_per_cycle) {
            return Err(format!("Folders per cycle must be between {} and {}.", FOLDERS_RANGE.start(), FOLDERS_RANGE.end()));
        }
        if !INTERVAL_RANGE.contains(&self.interval_seconds) {
            return Err(format!("Cycle interval must be between {} and {} seconds.", INTERVAL_RANGE.start(), INTERVAL_RANGE.end()));
        }
        if !LIVE_RANGE.contains(&self.max_live_folders) {
            return Err(format!("Live folders must be between {} and {}.", LIVE_RANGE.start(), LIVE_RANGE.end()));
        }
        if !INITIAL_RANGE.contains(&self.initial_sync_messages) {
            return Err(format!(
                "Initial messages per folder must be between {} and {}.",
                INITIAL_RANGE.start(),
                INITIAL_RANGE.end()
            ));
        }
        Ok(())
    }

    /// Lenient: pull out-of-range values (hand-edited file) into range.
    pub fn clamped(self) -> Self {
        SyncSettings {
            folders_per_cycle: self.folders_per_cycle.clamp(*FOLDERS_RANGE.start(), *FOLDERS_RANGE.end()),
            interval_seconds: self.interval_seconds.clamp(*INTERVAL_RANGE.start(), *INTERVAL_RANGE.end()),
            max_live_folders: self.max_live_folders.clamp(*LIVE_RANGE.start(), *LIVE_RANGE.end()),
            initial_sync_messages: self.initial_sync_messages.clamp(*INITIAL_RANGE.start(), *INITIAL_RANGE.end()),
        }
    }

    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_seconds as u64)
    }
}

/// Everything in `sync_settings.json`: the wire settings plus per-account live folders.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Persisted {
    #[serde(flatten)]
    pub settings: SyncSettings,
    #[serde(default)]
    pub live_folders: BTreeMap<String, Vec<String>>,
}

impl Persisted {
    /// Clamp settings; drop INBOX (always live), blanks and duplicates from live lists.
    fn normalized(mut self) -> Self {
        self.settings = self.settings.clamped();
        for v in self.live_folders.values_mut() {
            let mut seen = std::collections::HashSet::new();
            v.retain(|f| !f.trim().is_empty() && !f.eq_ignore_ascii_case("INBOX") && seen.insert(f.clone()));
        }
        self.live_folders.retain(|_, v| !v.is_empty());
        self
    }
}

/// Missing, unreadable or corrupt file -> defaults. Never fails.
pub fn load_config(path: &Path) -> Persisted {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Persisted>(&b).ok())
        .map(Persisted::normalized)
        .unwrap_or_default()
}

/// Atomic write: temp file in the same directory, then rename over the target.
pub fn save_config(path: &Path, p: &Persisted) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(p).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

// ---------------------------------------------------------------- round robin

/// Next `n` folders starting at `cursor`, wrapping; returns the batch and the new cursor.
/// A cursor past the end (list shrank) wraps. `n` larger than the list returns each folder once.
pub fn next_batch(folders: &[String], cursor: usize, n: usize) -> (Vec<String>, usize) {
    if folders.is_empty() || n == 0 {
        return (Vec::new(), 0);
    }
    let len = folders.len();
    let start = cursor % len;
    let k = n.min(len);
    let batch = (0..k).map(|i| folders[(start + i) % len].clone()).collect();
    (batch, (start + k) % len)
}

/// Keep the cursor pointing at the same folder when the list changes; otherwise wrap into range.
pub fn remap_cursor(old: &[String], new: &[String], cursor: usize) -> usize {
    if new.is_empty() {
        return 0;
    }
    if old == new {
        return cursor % new.len();
    }
    old.get(cursor).and_then(|name| new.iter().position(|f| f == name)).unwrap_or(cursor % new.len())
}

/// Backoff after `failures` consecutive failed ticks: interval * 2^failures, capped at 10 min
/// (never below the interval itself).
pub fn tick_delay(interval: Duration, failures: u32) -> Duration {
    if failures == 0 {
        return interval;
    }
    let scaled = interval.saturating_mul(1u32 << failures.min(10));
    scaled.min(MAX_BACKOFF).max(interval)
}

/// Max jitter added to a wait: 10% of the interval, at most 5 s.
pub fn jitter_cap(interval: Duration) -> Duration {
    (interval / 10).min(Duration::from_secs(5))
}

#[derive(Debug, Default)]
pub struct Sched {
    cursor: usize,
    last: Vec<String>,
    failures: u32,
}

impl Sched {
    pub fn plan(&mut self, folders: Vec<String>, n: usize) -> Vec<String> {
        self.cursor = remap_cursor(&self.last, &folders, self.cursor);
        let (batch, cur) = next_batch(&folders, self.cursor, n);
        self.cursor = cur;
        self.last = folders;
        batch
    }
    pub fn record(&mut self, ok: bool) {
        self.failures = if ok { 0 } else { self.failures.saturating_add(1) };
    }
    pub fn failures(&self) -> u32 {
        self.failures
    }
}

// ---------------------------------------------------------------- loop

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickOutcome {
    /// (folder, new message count) for folders that synced.
    pub synced: Vec<(String, u32)>,
    /// Folders that failed individually.
    pub failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickError {
    /// Credentials rejected: stop the loop for good (retrying risks a lockout).
    Auth,
    Other(String),
}

/// Everything the loop needs from the outside world (faked in tests).
#[allow(async_fn_in_trait)]
pub trait SyncOps: Send + Sync + 'static {
    /// True only while the INBOX watcher reports the account as connected.
    fn online(&self) -> bool;
    /// Selectable folders excluding INBOX.
    fn folders(&self) -> Vec<String>;
    /// Sync `batch` over one connection, then close it.
    fn sync_batch(&self, batch: &[String]) -> impl Future<Output = Result<TickOutcome, TickError>> + Send;
    fn emit_new(&self, folder: &str);
    fn emit_synced(&self, folders: &[String]);
    /// Random value in `0..=max_ms`.
    fn jitter_ms(&self, max_ms: u64) -> u64;
}

pub async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        if stop.changed().await.is_err() {
            return; // sender dropped
        }
    }
}

/// Run until `stop` flips (or its sender drops) or credentials are rejected.
pub async fn run_loop<O: SyncOps>(ops: O, mut settings: watch::Receiver<SyncSettings>, mut stop: watch::Receiver<bool>) {
    let mut sched = Sched::default();
    let mut first = true;
    let mut settings_open = true;
    'outer: loop {
        let cfg = *settings.borrow();
        let base = if first { FIRST_TICK.min(cfg.interval()) } else { tick_delay(cfg.interval(), sched.failures()) };
        let wait = base + Duration::from_millis(ops.jitter_ms(jitter_cap(cfg.interval()).as_millis() as u64));
        tokio::select! {
            _ = sleep(wait) => {}
            _ = stopped(&mut stop) => return,
            r = settings.changed(), if settings_open => {
                if r.is_err() { settings_open = false; }
                continue 'outer; // apply the new interval immediately
            }
        }
        first = false;

        if !ops.online() {
            continue; // offline / reconnecting / auth-failed: don't touch the server
        }
        let cfg = *settings.borrow();
        let batch = sched.plan(ops.folders(), cfg.folders_per_cycle as usize);
        if batch.is_empty() {
            continue;
        }
        let res = tokio::select! {
            r = ops.sync_batch(&batch) => r,
            _ = stopped(&mut stop) => return,
        };
        match res {
            Ok(out) => {
                sched.record(!(out.failed >= batch.len()));
                for (f, n) in &out.synced {
                    if *n > 0 {
                        ops.emit_new(f);
                    }
                }
                ops.emit_synced(&batch);
            }
            Err(TickError::Auth) => return,
            Err(TickError::Other(_)) => sched.record(false),
        }
    }
}

// ---------------------------------------------------------------- real ops

pub mod real {
    use super::*;
    use crate::state::{AppState, INBOX};
    use crate::wire::{NewMailEvent, SyncedEvent};
    use mailcore::FolderRole;
    use tauri::{AppHandle, Emitter, Manager};

    pub struct AppOps {
        pub app: AppHandle,
        pub email: String,
    }

    impl SyncOps for AppOps {
        fn online(&self) -> bool {
            self.app.state::<AppState>().inbox_status(&self.email) == Some("connected")
        }

        fn folders(&self) -> Vec<String> {
            let st = self.app.state::<AppState>();
            let live = st.live_folders(&self.email);
            st.storage
                .list_folders(&self.email)
                .unwrap_or_default()
                .into_iter()
                .filter(|f| f.selectable && f.role != FolderRole::Inbox && !f.name.eq_ignore_ascii_case(INBOX))
                .filter(|f| !live.contains(&f.name)) // live folders have their own watcher
                .map(|f| f.name)
                .collect()
        }

        async fn sync_batch(&self, batch: &[String]) -> Result<TickOutcome, TickError> {
            let st = self.app.state::<AppState>();
            let mut conn = st.connect(&self.email).await.map_err(|e| {
                if e == mailcore::Error::AuthFailed.to_string() { TickError::Auth } else { TickError::Other(e) }
            })?;
            let mut out = TickOutcome::default();
            let limit = st.initial_sync_limit();
            for f in batch {
                match conn.client.sync_folder(&st.storage, &self.email, f, limit).await {
                    Ok(r) => out.synced.push((f.clone(), r.new)),
                    Err(_) => out.failed += 1,
                }
            }
            conn.close().await;
            Ok(out)
        }

        fn emit_new(&self, folder: &str) {
            let _ = self.app.emit("mail://new", NewMailEvent { account: self.email.clone(), folder: folder.to_string() });
            crate::automation::notify(&self.app, &self.email, folder);
        }

        fn emit_synced(&self, folders: &[String]) {
            let _ = self.app.emit("mail://synced", SyncedEvent { account: self.email.clone(), folders: folders.to_vec() });
        }

        fn jitter_ms(&self, max_ms: u64) -> u64 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(0);
            if max_ms == 0 { 0 } else { nanos % (max_ms + 1) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn batch_basic_and_wrap() {
        let f = names(&["a", "b", "c", "d", "e"]);
        assert_eq!(next_batch(&f, 0, 2), (names(&["a", "b"]), 2));
        assert_eq!(next_batch(&f, 2, 2), (names(&["c", "d"]), 4));
        assert_eq!(next_batch(&f, 4, 2), (names(&["e", "a"]), 1));
    }

    #[test]
    fn batch_n_larger_than_list() {
        let f = names(&["a", "b"]);
        assert_eq!(next_batch(&f, 1, 20), (names(&["b", "a"]), 1));
    }

    #[test]
    fn batch_empty_and_zero() {
        assert_eq!(next_batch(&[], 5, 2), (vec![], 0));
        assert_eq!(next_batch(&names(&["a"]), 0, 0), (vec![], 0));
    }

    #[test]
    fn batch_list_shrinks() {
        let f = names(&["a", "b", "c"]);
        // cursor 7 from a former longer list wraps into range
        assert_eq!(next_batch(&f, 7, 2), (names(&["b", "c"]), 0));
    }

    #[test]
    fn batch_n_one_cycles_everything() {
        let f = names(&["a", "b", "c"]);
        let mut cur = 0;
        let mut seen = vec![];
        for _ in 0..4 {
            let (b, c) = next_batch(&f, cur, 1);
            seen.extend(b);
            cur = c;
        }
        assert_eq!(seen, names(&["a", "b", "c", "a"]));
    }

    #[test]
    fn cursor_follows_folder_when_list_changes() {
        let old = names(&["a", "b", "c", "d"]);
        assert_eq!(remap_cursor(&old, &names(&["x", "a", "b", "c", "d"]), 2), 3); // still "c"
        assert_eq!(remap_cursor(&old, &names(&["a", "d"]), 2), 0); // "c" gone: wraps
        assert_eq!(remap_cursor(&old, &[], 2), 0);
        assert_eq!(remap_cursor(&[], &names(&["a", "b"]), 0), 0);
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let i = Duration::from_secs(60);
        let v: Vec<u64> = (0..6).map(|n| tick_delay(i, n).as_secs()).collect();
        assert_eq!(v, vec![60, 120, 240, 480, 600, 600]);
        assert_eq!(tick_delay(i, u32::MAX).as_secs(), 600);
        assert_eq!(tick_delay(Duration::from_secs(3600), 3).as_secs(), 3600); // never below the interval
        assert_eq!(jitter_cap(Duration::from_secs(60)).as_secs(), 5);
        assert_eq!(jitter_cap(Duration::from_secs(15)).as_millis(), 1500);
    }

    fn st(f: u32, i: u32, l: u32) -> SyncSettings {
        SyncSettings { folders_per_cycle: f, interval_seconds: i, max_live_folders: l, initial_sync_messages: 200 }
    }

    #[test]
    fn validation_ranges() {
        assert!(SyncSettings::default().validate().is_ok());
        assert!(st(0, 60, 5).validate().unwrap_err().contains("Folders per cycle"));
        assert!(st(21, 60, 5).validate().is_err());
        assert!(st(2, 14, 5).validate().unwrap_err().contains("Cycle interval"));
        assert!(st(2, 3601, 5).validate().is_err());
        assert!(st(2, 60, 0).validate().unwrap_err().contains("Live folders"));
        assert!(st(2, 60, 11).validate().is_err());
        assert!(st(1, 15, 1).validate().is_ok() && st(20, 3600, 10).validate().is_ok());
        assert_eq!(st(0, 99999, 99).clamped(), st(1, 3600, 10));
    }

    fn init(n: u32) -> SyncSettings {
        SyncSettings { initial_sync_messages: n, ..SyncSettings::default() }
    }

    #[test]
    fn initial_sync_messages_validation() {
        assert_eq!(SyncSettings::default().initial_sync_messages, 200);
        assert!(init(50).validate().is_ok() && init(5000).validate().is_ok() && init(200).validate().is_ok());
        assert!(init(49).validate().unwrap_err().contains("Initial messages per folder"));
        assert!(init(0).validate().is_err() && init(5001).validate().is_err() && init(u32::MAX).validate().is_err());
        assert_eq!(init(0).clamped().initial_sync_messages, 50);
        assert_eq!(init(999_999).clamped().initial_sync_messages, 5000);
        assert_eq!(init(1234).clamped().initial_sync_messages, 1234);
    }

    #[test]
    fn initial_sync_messages_persist_and_default_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(SETTINGS_FILE);
        let c = Persisted { settings: init(2000), live_folders: BTreeMap::new() };
        save_config(&p, &c).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("initialSyncMessages"));
        assert_eq!(load_config(&p).settings.initial_sync_messages, 2000);
        // a file written before this setting existed keeps working
        std::fs::write(&p, br#"{"foldersPerCycle": 3, "intervalSeconds": 90, "maxLiveFolders": 4}"#).unwrap();
        let s = load_config(&p).settings;
        assert_eq!((s.folders_per_cycle, s.initial_sync_messages), (3, 200));
        // hand-edited out-of-range value is clamped on load
        std::fs::write(&p, br#"{"initialSyncMessages": 10}"#).unwrap();
        assert_eq!(load_config(&p).settings.initial_sync_messages, 50);
        std::fs::write(&p, br#"{"initialSyncMessages": 100000}"#).unwrap();
        assert_eq!(load_config(&p).settings.initial_sync_messages, 5000);
        // wrong type -> whole file falls back to defaults (never fails)
        std::fs::write(&p, br#"{"initialSyncMessages": "lots"}"#).unwrap();
        assert_eq!(load_config(&p), Persisted::default());
    }

    #[test]
    fn config_persist_roundtrip_and_tolerate_bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(SETTINGS_FILE);
        assert_eq!(load_config(&p), Persisted::default()); // missing
        let mut live = BTreeMap::new();
        live.insert("a@x.org".to_string(), vec!["Work".to_string(), "Lists/rust".to_string()]);
        let c = Persisted { settings: st(5, 120, 3), live_folders: live };
        save_config(&p, &c).unwrap();
        assert_eq!(load_config(&p), c);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("foldersPerCycle") && text.contains("liveFolders") && text.contains("maxLiveFolders"));
        assert!(!p.with_extension("json.tmp").exists(), "temp file renamed away");
        std::fs::write(&p, b"{not json").unwrap();
        assert_eq!(load_config(&p), Persisted::default()); // corrupt
        std::fs::write(&p, br#"{"foldersPerCycle": 99, "intervalSeconds": 1, "maxLiveFolders": 0}"#).unwrap();
        assert_eq!(load_config(&p).settings, st(20, 15, 1)); // clamped
        std::fs::write(&p, br#"{"foldersPerCycle": 4}"#).unwrap();
        assert_eq!(load_config(&p).settings, st(4, 60, 5)); // partial
        std::fs::write(&p, br#"{"liveFolders": {"a": ["INBOX", "X", "X", " "], "b": []}}"#).unwrap();
        let l = load_config(&p).live_folders;
        assert_eq!(l.get("a"), Some(&vec!["X".to_string()]));
        assert!(!l.contains_key("b"));
    }

    // ---- loop with a fake and paused time ----

    #[derive(Default)]
    struct Log {
        batches: Vec<Vec<String>>,
        new: Vec<String>,
        synced: Vec<Vec<String>>,
    }
    struct Fake {
        log: Arc<Mutex<Log>>,
        online: Arc<Mutex<bool>>,
        folders: Vec<String>,
        results: Arc<Mutex<Vec<Result<TickOutcome, TickError>>>>,
    }
    impl SyncOps for Fake {
        fn online(&self) -> bool {
            *self.online.lock().unwrap()
        }
        fn folders(&self) -> Vec<String> {
            self.folders.clone()
        }
        async fn sync_batch(&self, batch: &[String]) -> Result<TickOutcome, TickError> {
            self.log.lock().unwrap().batches.push(batch.to_vec());
            let mut r = self.results.lock().unwrap();
            if r.is_empty() {
                Ok(TickOutcome { synced: batch.iter().map(|f| (f.clone(), 0)).collect(), failed: 0 })
            } else {
                r.remove(0)
            }
        }
        fn emit_new(&self, folder: &str) {
            self.log.lock().unwrap().new.push(folder.to_string());
        }
        fn emit_synced(&self, folders: &[String]) {
            self.log.lock().unwrap().synced.push(folders.to_vec());
        }
        fn jitter_ms(&self, _max: u64) -> u64 {
            0
        }
    }

    struct Rig {
        log: Arc<Mutex<Log>>,
        online: Arc<Mutex<bool>>,
        settings: watch::Sender<SyncSettings>,
        stop: watch::Sender<bool>,
        task: tokio::task::JoinHandle<()>,
    }
    fn rig(folders: &[&str], results: Vec<Result<TickOutcome, TickError>>, cfg: SyncSettings) -> Rig {
        let log = Arc::new(Mutex::new(Log::default()));
        let online = Arc::new(Mutex::new(true));
        let fake = Fake { log: log.clone(), online: online.clone(), folders: names(folders), results: Arc::new(Mutex::new(results)) };
        let (settings, srx) = watch::channel(cfg);
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(run_loop(fake, srx, stop_rx));
        Rig { log, online, settings, stop, task }
    }
    async fn advance(secs: u64) {
        for _ in 0..10 {
            tokio::task::yield_now().await; // let the loop register its sleep first
        }
        tokio::time::advance(Duration::from_secs(secs)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }
    fn cfg(n: u32, i: u32) -> SyncSettings {
        st(n, i, 5)
    }

    #[tokio::test(start_paused = true)]
    async fn loop_round_robins_and_emits() {
        let ok = |f: &str, n| Ok(TickOutcome { synced: vec![(f.to_string(), n)], failed: 0 });
        let r = rig(&["a", "b", "c"], vec![ok("a", 3)], cfg(1, 60));
        advance(10).await; // first tick
        advance(60).await;
        advance(60).await;
        advance(60).await;
        {
            let l = r.log.lock().unwrap();
            assert_eq!(l.batches, vec![names(&["a"]), names(&["b"]), names(&["c"]), names(&["a"])]);
            assert_eq!(l.new, names(&["a"])); // only folders with new mail
            assert_eq!(l.synced.len(), 4);
        }
        r.stop.send(true).unwrap();
        r.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn loop_skips_when_offline_without_advancing() {
        let r = rig(&["a", "b"], vec![], cfg(1, 60));
        *r.online.lock().unwrap() = false;
        advance(10).await;
        advance(60).await;
        assert!(r.log.lock().unwrap().batches.is_empty());
        *r.online.lock().unwrap() = true;
        advance(60).await;
        assert_eq!(r.log.lock().unwrap().batches, vec![names(&["a"])]);
        r.stop.send(true).unwrap();
        r.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn loop_backs_off_on_errors_and_stops_on_auth() {
        let e = || Err(TickError::Other("x".into()));
        let r = rig(&["a"], vec![e(), e()], cfg(1, 60));
        advance(10).await; // tick 1 fails -> next wait 120
        assert_eq!(r.log.lock().unwrap().batches.len(), 1);
        advance(60).await;
        assert_eq!(r.log.lock().unwrap().batches.len(), 1, "backoff: not yet");
        advance(60).await; // tick 2 fails -> next wait 240
        assert_eq!(r.log.lock().unwrap().batches.len(), 2);
        advance(200).await;
        assert_eq!(r.log.lock().unwrap().batches.len(), 2);
        advance(40).await; // tick 3 succeeds -> back to 60
        assert_eq!(r.log.lock().unwrap().batches.len(), 3);
        advance(60).await;
        assert_eq!(r.log.lock().unwrap().batches.len(), 4);
        r.stop.send(true).unwrap();
        r.task.await.unwrap();

        let r = rig(&["a"], vec![Err(TickError::Auth)], cfg(1, 60));
        advance(10).await;
        r.task.await.unwrap(); // loop ended by itself
        assert_eq!(r.log.lock().unwrap().batches.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn loop_applies_settings_live_and_stops_promptly() {
        let r = rig(&["a", "b", "c", "d"], vec![], cfg(1, 3600));
        advance(10).await;
        assert_eq!(r.log.lock().unwrap().batches.len(), 1);
        r.settings.send(cfg(2, 30)).unwrap(); // new interval + batch size, no restart
        advance(30).await;
        {
            let l = r.log.lock().unwrap();
            assert_eq!(l.batches.len(), 2);
            assert_eq!(l.batches[1], names(&["b", "c"]));
        }
        r.stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), r.task).await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn loop_with_no_folders_never_syncs() {
        let r = rig(&[], vec![], cfg(2, 15));
        advance(10).await;
        advance(15).await;
        assert!(r.log.lock().unwrap().batches.is_empty());
        drop(r.stop); // sender dropped also ends the loop
        r.task.await.unwrap();
    }
}
