//! Preferences > Privacy > Storage: how big the offline cache is, the per-account "keep bodies for N days / max MB"
//! limits, and the explicit "Clear cached bodies" button. Nothing here runs by itself: the limits only decide whether a
//! body that was just fetched is stored (`mailcore::cachectl`), and clearing happens only when the user confirms it
//! (rows only: no file is deleted, nothing on the server changes).

use crate::state::{err, AppState};
use mailcore::{CachePolicy, CacheUsage, Storage};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::State;

pub const FILE: &str = "cache_policy.json";

pub struct PolicyFile {
    path: PathBuf,
    lock: Mutex<()>,
}

impl PolicyFile {
    pub fn new(path: PathBuf) -> Self {
        PolicyFile { path, lock: Mutex::new(()) }
    }
}

/// Read the saved limits (missing or corrupt file: none) and install them in the storage layer.
pub fn load_into(storage: &Storage, path: &Path) {
    if let Some(m) = read(path) {
        for (account, p) in m {
            if p.validate().is_ok() {
                storage.set_cache_policy(&account, p);
            }
        }
    }
}

fn read(path: &Path) -> Option<BTreeMap<String, CachePolicy>> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn write(path: &Path, m: &BTreeMap<String, CachePolicy>) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(m).map_err(|e| e.to_string())?;
    // Temp file in the same directory, then rename over the target: a crash never leaves a half-written file.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| format!("Couldn't save the setting: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("Couldn't save the setting: {e}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStorage {
    pub account: String,
    pub usage: CacheUsage,
    pub policy: CachePolicy,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageReport {
    /// Size of the cache database (all accounts share one file).
    pub db_bytes: u64,
    pub accounts: Vec<AccountStorage>,
}

#[tauri::command]
pub fn storage_usage(state: State<'_, AppState>) -> Result<StorageReport, String> {
    let mut accounts = Vec::new();
    for a in state.storage.list_accounts().map_err(err)? {
        accounts.push(AccountStorage { usage: state.storage.cache_usage(&a.email).map_err(err)?, policy: state.storage.cache_policy(&a.email), account: a.email });
    }
    Ok(StorageReport { db_bytes: state.storage.db_size_bytes().map_err(err)?, accounts })
}

/// Drop one account's cached bodies (the UI confirms first). Returns how many messages lost their cached body.
#[tauri::command]
pub fn storage_clear_bodies(state: State<'_, AppState>, account: String) -> Result<u64, String> {
    state.account(&account)?;
    state.storage.clear_cached_bodies(&account).map_err(err)
}

/// `keep_days` / `max_mb` None = no limit. Stops caching beyond the limit; deletes nothing.
#[tauri::command]
pub fn storage_set_policy(state: State<'_, AppState>, file: State<'_, PolicyFile>, account: String, policy: CachePolicy) -> Result<CachePolicy, String> {
    state.account(&account)?;
    policy.validate()?;
    let _g = file.lock.lock().unwrap_or_else(|e| e.into_inner());
    state.storage.set_cache_policy(&account, policy);
    let mut m = read(&file.path).unwrap_or_default();
    if policy == CachePolicy::default() {
        m.remove(&account);
    } else {
        m.insert(account, policy);
    }
    write(&file.path, &m)?;
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_round_trip_through_the_file_and_bad_ones_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE);
        let mut m = BTreeMap::new();
        m.insert("a@x.org".to_string(), CachePolicy { keep_days: Some(90), max_mb: Some(200) });
        m.insert("b@x.org".to_string(), CachePolicy { keep_days: Some(0), max_mb: None });
        write(&p, &m).unwrap();
        let s = Storage::open_in_memory().unwrap();
        load_into(&s, &p);
        assert_eq!(s.cache_policy("a@x.org"), CachePolicy { keep_days: Some(90), max_mb: Some(200) });
        assert_eq!(s.cache_policy("b@x.org"), CachePolicy::default(), "an invalid saved limit is ignored");
        // Missing / corrupt file: no limits, no error.
        load_into(&s, &dir.path().join("nope.json"));
        std::fs::write(&p, b"not json").unwrap();
        load_into(&s, &p);
    }

    #[test]
    fn write_is_atomic_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(FILE);
        let mut m = BTreeMap::new();
        m.insert("a@x.org".to_string(), CachePolicy { keep_days: Some(30), max_mb: None });
        write(&p, &m).unwrap();
        write(&p, &m).unwrap();
        assert_eq!(read(&p).unwrap(), m);
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }
}
