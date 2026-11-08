//! Calendar state: `calendar.db`, Keychain-backed secrets (CalDAV passwords, Zoom tokens) and the
//! network seams (`CalDavRemote`, `ZoomRemote`) that let the command logic be tested without a network.

use calendar::{Calendar, CalendarInfo};
use calsync::caldav::{PushOutcome, SyncSummary};
use calsync::{CalDavClient, CalDavError, Config, DiscoveredCalendar, TokenStore, ZoomClient, ZoomConfig, ZoomError};
use mailcore::SecretStore;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};

pub const CALDAV_PREFIX: &str = "caldav:";
pub const ZOOM_PREFIX: &str = "zoom:";
pub const ZOOM_SECRET_KEY: &str = "zoom:client_secret";
/// Non-secret Zoom client id, kept in the calendar settings table.
pub const ZOOM_CLIENT_ID_SETTING: &str = "zoom.client_id";
/// Non-secret list (JSON array) of the Zoom meeting ids this app created; only these are ever updated or deleted.
pub const ZOOM_CREATED_SETTING: &str = "zoom.created";
const ZOOM_CREATED_MAX: usize = 500;

/// Non-secret marker (value "1") that the server rejected the saved CalDAV password. While it is set the
/// calendar is not contacted any more; adding the calendar again (new password) clears it.
pub fn signin_failed_key(calendar_id: &str) -> String {
    format!("cal.signin_failed.{calendar_id}")
}

/// Keychain key of a CalDAV calendar password (mail accounts always contain '@', so no collision).
pub fn caldav_key(calendar_id: &str) -> String {
    format!("{CALDAV_PREFIX}{calendar_id}")
}

/// `TokenStore` over the app's secret store: keys are stored as `zoom:<key>`.
pub struct SecretTokenStore {
    secrets: Arc<dyn SecretStore>,
}

impl SecretTokenStore {
    pub fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self { secrets }
    }
}

fn store_err<E>(_: E) -> ZoomError {
    // The underlying text is never forwarded: it must not be able to carry a secret.
    ZoomError::Store("could not access the Keychain".into())
}

impl TokenStore for SecretTokenStore {
    fn get(&self, key: &str) -> calsync::zoom::Result<Option<String>> {
        self.secrets.get(&format!("{ZOOM_PREFIX}{key}")).map_err(store_err)
    }
    fn set(&self, key: &str, val: &str) -> calsync::zoom::Result<()> {
        self.secrets.set(&format!("{ZOOM_PREFIX}{key}"), val).map_err(store_err)
    }
    fn delete(&self, key: &str) -> calsync::zoom::Result<()> {
        self.secrets.delete(&format!("{ZOOM_PREFIX}{key}")).map_err(store_err)
    }
}

/// Friendly text for a calendar storage error (never includes event content).
pub fn cerr(e: calendar::Error) -> String {
    match e {
        calendar::Error::TooLarge { .. } => "That event is too large to save.".into(),
        calendar::Error::Invalid(m) => m,
        calendar::Error::NotFound(w) => format!("Not found: {w}."),
        calendar::Error::Db(_) => "The calendar database reported an error.".into(),
    }
}

pub struct CalState {
    pub store: Arc<Mutex<Calendar>>,
    pub secrets: Arc<dyn SecretStore>,
    /// Serialises sync runs (background loop + `cal_sync`).
    pub sync_gate: tokio::sync::Mutex<()>,
    zoom: Mutex<Option<Arc<ZoomClient>>>,
    /// Shared by every `ZoomClient` this state builds, so a client replaced by `reset_zoom` cannot refresh
    /// (and rotate the refresh token) concurrently with its successor.
    zoom_refresh: Arc<tokio::sync::Mutex<()>>,
    pub zoom_connecting: std::sync::atomic::AtomicBool,
    /// Per-calendar write locks: a local save / delete / removal and that calendar's sync never interleave.
    cal_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Replies already sent this session (see `cal_logic::reply_key`).
    replied: Mutex<HashSet<String>>,
}

impl CalState {
    pub fn new(cal: Calendar, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            store: Arc::new(Mutex::new(cal)),
            secrets,
            sync_gate: tokio::sync::Mutex::new(()),
            zoom: Mutex::new(None),
            zoom_refresh: Arc::new(tokio::sync::Mutex::new(())),
            zoom_connecting: std::sync::atomic::AtomicBool::new(false),
            cal_locks: Mutex::new(HashMap::new()),
            replied: Mutex::new(HashSet::new()),
        }
    }

    /// The write lock of one calendar. Hold it (`lock_owned().await`) across a local write + its push.
    /// When two are needed take them in sorted id order.
    pub fn cal_lock(&self, calendar_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.cal_locks.lock().unwrap_or_else(|e| e.into_inner()).entry(calendar_id.to_string()).or_default().clone()
    }

    /// Records a reply; `false` when the same reply was already recorded.
    pub fn note_reply(&self, key: &str) -> bool {
        self.replied.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_string())
    }

    pub fn forget_reply(&self, key: &str) {
        self.replied.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }

    pub fn signin_failed(&self, calendar_id: &str) -> bool {
        self.db(|c| c.get_setting(&signin_failed_key(calendar_id))).ok().flatten().is_some()
    }

    pub fn set_signin_failed(&self, calendar_id: &str) {
        let _ = self.db(|c| c.set_setting(&signin_failed_key(calendar_id), "1"));
    }

    pub fn clear_signin_failed(&self, calendar_id: &str) {
        let _ = self.db(|c| c.delete_setting(&signin_failed_key(calendar_id)));
    }

    fn zoom_created(&self) -> Vec<String> {
        self.db(|c| c.get_setting(ZOOM_CREATED_SETTING)).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Remember a meeting this app created (called right after `create_meeting`).
    pub fn zoom_record(&self, meeting_id: &str) -> Result<(), String> {
        let mut v = self.zoom_created();
        if !v.iter().any(|m| m == meeting_id) {
            v.push(meeting_id.to_string());
            if v.len() > ZOOM_CREATED_MAX {
                v.remove(0);
            }
            let json = serde_json::to_string(&v).unwrap_or_else(|_| "[]".into());
            self.db(|c| c.set_setting(ZOOM_CREATED_SETTING, &json))?;
        }
        Ok(())
    }

    /// Was this meeting created by this app on this Mac? Everything else is somebody else's and is left alone.
    pub fn zoom_is_ours(&self, meeting_id: &str) -> bool {
        self.zoom_created().iter().any(|m| m == meeting_id)
    }

    pub fn zoom_forget(&self, meeting_id: &str) {
        let mut v = self.zoom_created();
        v.retain(|m| m != meeting_id);
        let json = serde_json::to_string(&v).unwrap_or_else(|_| "[]".into());
        let _ = self.db(|c| c.set_setting(ZOOM_CREATED_SETTING, &json));
    }

    /// Run `f` on the calendar store. Never call this around an `.await`.
    pub fn db<T>(&self, f: impl FnOnce(&Calendar) -> calendar::Result<T>) -> Result<T, String> {
        let g = self.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&g).map_err(cerr)
    }

    pub fn calendar(&self, id: &str) -> Result<CalendarInfo, String> {
        self.db(|c| c.get_calendar(id))?.ok_or_else(|| "That calendar no longer exists.".to_string())
    }

    pub fn caldav_password(&self, calendar_id: &str) -> Option<String> {
        self.secrets.get(&caldav_key(calendar_id)).ok().flatten().filter(|p| !p.is_empty())
    }

    /// The shared Zoom client (so the refresh single-flight works). Rebuilt after `reset_zoom`.
    pub fn zoom_client(&self) -> Result<Arc<ZoomClient>, String> {
        let mut g = self.zoom.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = g.as_ref() {
            return Ok(c.clone());
        }
        let client_id = self.db(|c| c.get_setting(ZOOM_CLIENT_ID_SETTING))?.unwrap_or_default();
        let client_secret = self.secrets.get(ZOOM_SECRET_KEY).ok().flatten().filter(|s| !s.is_empty());
        let cfg = ZoomConfig { client_id, client_secret, ..ZoomConfig::default() };
        let c = Arc::new(ZoomClient::with_refresh_lock(cfg, Arc::new(SecretTokenStore::new(self.secrets.clone())), self.zoom_refresh.clone()));
        *g = Some(c.clone());
        Ok(c)
    }

    pub fn reset_zoom(&self) {
        *self.zoom.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

// ---------------------------------------------------------------------------------------------
// Network seams
// ---------------------------------------------------------------------------------------------

pub trait CalDavRemote: Send + Sync {
    fn discover(&self, base_url: &str, username: &str, password: &str, diag: &calsync::Diag) -> impl Future<Output = Result<Vec<DiscoveredCalendar>, CalDavError>> + Send;
    fn sync(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, password: &str) -> impl Future<Output = Result<SyncSummary, CalDavError>> + Send;
    fn push(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, password: &str, uid: &str) -> impl Future<Output = Result<PushOutcome, CalDavError>> + Send;
    fn delete(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, password: &str, href: &str, etag: Option<&str>) -> impl Future<Output = Result<(), CalDavError>> + Send;
}

pub trait ZoomRemote: Send + Sync {
    fn update(&self, meeting_id: &str, topic: &str, start_rfc3339: &str, minutes: u32, tz: &str) -> impl Future<Output = Result<(), ZoomError>> + Send;
    fn delete(&self, meeting_id: &str) -> impl Future<Output = Result<(), ZoomError>> + Send;
}

/// Real CalDAV: a client is built per call from the stored url/username + Keychain password.
pub struct RealCalDav;

fn client(base_url: &str, username: &str, password: &str) -> Result<CalDavClient, CalDavError> {
    CalDavClient::new(Config { base_url: base_url.to_string(), username: username.to_string(), password: password.to_string(), allow_insecure_loopback: false })
}

fn cal_client(cal: &CalendarInfo, password: &str) -> Result<CalDavClient, CalDavError> {
    let url = cal.url.as_deref().ok_or_else(|| CalDavError::Config("calendar has no URL".into()))?;
    client(url, cal.username.as_deref().unwrap_or_default(), password)
}

impl CalDavRemote for RealCalDav {
    async fn discover(&self, base_url: &str, username: &str, password: &str, diag: &calsync::Diag) -> Result<Vec<DiscoveredCalendar>, CalDavError> {
        client(base_url, username, password)?.with_diag(diag.clone()).discover().await
    }
    async fn sync(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, password: &str) -> Result<SyncSummary, CalDavError> {
        cal_client(cal, password)?.sync_calendar(store, cal).await
    }
    async fn push(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, password: &str, uid: &str) -> Result<PushOutcome, CalDavError> {
        // MiraMail sends every iMIP mail itself: an RFC 6638 server must not schedule on its own (duplicate invites).
        cal_client(cal, password)?.push_event_opts(store, cal, uid, true).await
    }
    async fn delete(&self, store: &Mutex<Calendar>, cal: &CalendarInfo, password: &str, href: &str, etag: Option<&str>) -> Result<(), CalDavError> {
        cal_client(cal, password)?.delete_remote(store, cal, href, etag).await
    }
}

/// Real Zoom meetings API (only reachable with stored tokens).
pub struct RealZoom(pub Arc<ZoomClient>);

impl ZoomRemote for RealZoom {
    async fn update(&self, meeting_id: &str, topic: &str, start_rfc3339: &str, minutes: u32, tz: &str) -> Result<(), ZoomError> {
        self.0.update_meeting(meeting_id, topic, start_rfc3339, minutes, tz, None).await
    }
    async fn delete(&self, meeting_id: &str) -> Result<(), ZoomError> {
        self.0.delete_meeting(meeting_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::InMemorySecretStore;

    #[test]
    fn token_store_prefixes_keys_and_keeps_secrets_out_of_errors() {
        let secrets = Arc::new(InMemorySecretStore::new());
        let ts = SecretTokenStore::new(secrets.clone());
        ts.set(calsync::zoom::KEY_ACCESS, "tok").unwrap();
        ts.set(calsync::zoom::KEY_REFRESH, "ref").unwrap();
        assert_eq!(secrets.get("zoom:zoom.access").unwrap().as_deref(), Some("tok"));
        assert_eq!(ts.get("zoom.refresh").unwrap().as_deref(), Some("ref"));
        assert_eq!(ts.get("zoom.expires").unwrap(), None);
        ts.delete("zoom.access").unwrap();
        assert_eq!(secrets.get("zoom:zoom.access").unwrap(), None);
        // no collision with mail accounts or caldav keys
        assert_eq!(caldav_key("abc"), "caldav:abc");
        assert!(!caldav_key("abc").contains('@'));
    }

    #[test]
    fn zoom_client_uses_setting_and_secret_and_resets() {
        let st = CalState::new(Calendar::open_in_memory().unwrap(), Arc::new(InMemorySecretStore::new()));
        let c = st.zoom_client().unwrap();
        assert!(Arc::ptr_eq(&c, &st.zoom_client().unwrap()));
        st.db(|c| c.set_setting(ZOOM_CLIENT_ID_SETTING, "cid")).unwrap();
        st.reset_zoom();
        let c2 = st.zoom_client().unwrap();
        assert!(!Arc::ptr_eq(&c, &c2));
    }

    #[test]
    fn calendar_errors_are_friendly() {
        assert!(cerr(calendar::Error::TooLarge { max: 1 }).contains("too large"));
        assert_eq!(cerr(calendar::Error::Invalid("uid is empty".into())), "uid is empty");
    }

    #[test]
    fn q11_rebuilt_zoom_clients_share_one_refresh_lock() {
        let st = CalState::new(Calendar::open_in_memory().unwrap(), Arc::new(InMemorySecretStore::new()));
        let a = st.zoom_client().unwrap();
        st.reset_zoom();
        let b = st.zoom_client().unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(Arc::strong_count(&st.zoom_refresh), 3, "state + the old client + the new one");
    }

    #[test]
    fn zoom_created_list_is_capped_and_non_secret() {
        let st = CalState::new(Calendar::open_in_memory().unwrap(), Arc::new(InMemorySecretStore::new()));
        assert!(!st.zoom_is_ours("1"));
        for i in 0..(ZOOM_CREATED_MAX + 5) {
            st.zoom_record(&i.to_string()).unwrap();
        }
        assert!(!st.zoom_is_ours("0") && st.zoom_is_ours(&(ZOOM_CREATED_MAX + 4).to_string()));
        st.zoom_forget(&(ZOOM_CREATED_MAX + 4).to_string());
        assert!(!st.zoom_is_ours(&(ZOOM_CREATED_MAX + 4).to_string()));
    }
}
