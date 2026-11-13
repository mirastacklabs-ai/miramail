use crate::{Error, Result};
use std::collections::HashMap;
use std::sync::Mutex;

pub const SERVICE: &str = "miramail";

/// Password storage. Passwords never go to SQLite.
pub trait SecretStore: Send + Sync {
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    fn get(&self, account: &str) -> Result<Option<String>>;
    /// Deleting a missing entry is not an error.
    fn delete(&self, account: &str) -> Result<()>;
}

#[cfg(not(target_os = "ios"))]
use keyring::{Entry, Error as KrError};
#[cfg(target_os = "ios")]
use keyring_core::{Entry, Error as KrError};

/// Error text for a Keychain failure. `BadEncoding` carries the raw secret bytes, so it never reaches a message or log.
fn kr_msg(e: &KrError) -> String {
    match e {
        KrError::BadEncoding(_) => "keychain item is not valid UTF-8".to_string(),
        other => other.to_string(),
    }
}

/// iOS has no `keyring` v1 front-end: install the Protected Data (data-protection Keychain) store once.
#[cfg(target_os = "ios")]
fn init_ios_store() -> Result<()> {
    use std::sync::OnceLock;
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        apple_native_keyring_store::protected::Store::new()
            .map(|s| keyring_core::set_default_store(s))
            .map_err(|e| e.to_string())
    })
    .clone()
    .map_err(Error::Secret)
}

/// Keychain services. Real secrets live under `SERVICE`; migration helper items (temp copy, marker) live under their
/// own service so they can never collide with a real key (emails, `caldav:<id>`, `zoom:<key>`).
#[cfg(any(target_os = "ios", test))]
const HELPER_SERVICE: &str = "miramail.migration";

/// Raw item operations by (service, item name). The iOS Keychain store implements this; tests use a fake. It exists
/// so the access-policy migration below is platform-independent code with tests.
#[cfg(any(target_os = "ios", test))]
trait RawItems {
    fn raw_get(&self, svc: &str, name: &str) -> Result<Option<String>>;
    /// Create or replace with the CURRENT access policy (delete + add, because an update does not reliably change it).
    fn raw_set(&self, svc: &str, name: &str, value: &str) -> Result<()>;
    fn raw_delete(&self, svc: &str, name: &str) -> Result<()>;
}

/// iOS items created before 2026-11 used the store default (WhenUnlocked: unreadable while the phone is locked, so
/// background refresh fails; and it may sync into backups restored elsewhere). New items use
/// AfterFirstUnlockThisDeviceOnly. Apple hides an existing item's policy, so a marker item ("<account>#pol1", stored
/// with the new policy, in the helper service) records that the migration finished.
///
/// Rules:
/// * One process-wide lock is held across each whole get/set/delete, so a migrating get() can never re-save an old
///   password over a newer one or resurrect a deleted account.
/// * set() writes the NEW value to a temp item first, then the account, reads it back, drops the temp, then writes the
///   marker. A failure after the account was deleted leaves the new value in the temp item.
/// * get(): an existing account value always wins; a temp item is only used to restore a MISSING account, so a stale
///   temp can never overwrite a newer account value.
/// * The first version kept its helpers under the main service (`<key>#pol1` marker, `<key>#mig` temp). An old marker
///   counts as "migrated"; old helpers are removed once the new marker is written.
#[cfg(any(target_os = "ios", test))]
mod migrate {
    use super::*;
    use std::sync::Mutex;
    pub(super) const MARKER: &str = "#pol1";
    pub(super) const TEMP: &str = "#mig";
    static LOCK: Mutex<()> = Mutex::new(());
    const MAIN: &str = SERVICE;
    const HELP: &str = HELPER_SERVICE;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(super) fn get<B: RawItems>(b: &B, account: &str) -> Result<Option<String>> {
        let _g = lock();
        let tmp = format!("{account}{TEMP}");
        let marker = format!("{account}{MARKER}");
        match b.raw_get(MAIN, account)? {
            Some(v) => {
                // The password is already read. If either marker lookup errored we cannot tell whether the item was
                // migrated: write nothing (a failed Keychain read must never trigger a rewrite) and retry on a later get.
                let (new_m, old_m) = (b.raw_get(HELP, &marker), b.raw_get(MAIN, &marker));
                match (new_m, old_m) {
                    (Ok(Some(_)), _) => {}
                    (Ok(None), Ok(Some(_))) => {
                        let _ = finish(b, account); // migrated by the first version: adopt the marker, clean old helpers
                    }
                    (Ok(None), Ok(None)) => {
                        let _ = migrate_item(b, account, &v); // best effort; a failure leaves the original in place
                    }
                    _ => {}
                }
                Ok(Some(v))
            }
            None => {
                let t = match b.raw_get(HELP, &tmp)? {
                    Some(t) => Some(t),
                    None => b.raw_get(MAIN, &tmp)?,
                };
                match t {
                    // An earlier write was interrupted after the account was deleted: restore from the temp copy.
                    Some(t) => {
                        // We hold the token: a failed restore must not hide it (the temp copy stays for a retry).
                        if b.raw_set(MAIN, account, &t).is_ok()
                            && b.raw_get(MAIN, account).ok().flatten().as_deref() == Some(t.as_str())
                        {
                            let _ = finish(b, account);
                        }
                        Ok(Some(t))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    /// Drop the temp copies first, then record "done" (a crash in between just re-runs the migration).
    fn finish<B: RawItems>(b: &B, account: &str) -> Result<()> {
        let tmp = format!("{account}{TEMP}");
        let marker = format!("{account}{MARKER}");
        b.raw_delete(HELP, &tmp)?;
        b.raw_delete(MAIN, &tmp)?;
        b.raw_set(HELP, &marker, "1")?;
        b.raw_delete(MAIN, &marker)
    }

    /// temp (new policy) -> account (replace) -> read back -> finish. The temp copy stays if anything fails.
    fn write_account<B: RawItems>(b: &B, account: &str, v: &str) -> Result<()> {
        let tmp = format!("{account}{TEMP}");
        if let Err(e) = b.raw_set(HELP, &tmp, v) {
            let _ = b.raw_delete(HELP, &tmp); // a half-written temp must not linger
            return Err(e);
        }
        if b.raw_get(HELP, &tmp)?.as_deref() != Some(v) {
            let _ = b.raw_delete(HELP, &tmp);
            return Err(Error::Secret("keychain write: temp copy unreadable".into()));
        }
        b.raw_set(MAIN, account, v)?;
        match b.raw_get(MAIN, account)? {
            Some(ref got) if got == v => {}
            other => {
                // The account holds something else: the temp copy is stale. If the account is missing it is the
                // only copy of the new value and stays so the next get() can restore it.
                if other.is_some() {
                    let _ = b.raw_delete(HELP, &tmp);
                }
                return Err(Error::Secret("keychain write: saved item unreadable".into()));
            }
        }
        finish(b, account)
    }

    fn migrate_item<B: RawItems>(b: &B, account: &str, v: &str) -> Result<()> {
        write_account(b, account, v)
    }

    pub(super) fn set<B: RawItems>(b: &B, account: &str, secret: &str) -> Result<()> {
        let _g = lock();
        write_account(b, account, secret)
    }

    pub(super) fn delete<B: RawItems>(b: &B, account: &str) -> Result<()> {
        let _g = lock();
        b.raw_delete(HELP, &format!("{account}{TEMP}"))?;
        b.raw_delete(MAIN, &format!("{account}{TEMP}"))?;
        b.raw_delete(HELP, &format!("{account}{MARKER}"))?;
        b.raw_delete(MAIN, &format!("{account}{MARKER}"))?;
        b.raw_delete(MAIN, account)
    }
}

/// Keychain (macOS `keyring` crate; iOS Protected Data store), service "miramail".
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainSecretStore;

#[cfg(not(target_os = "ios"))]
impl KeychainSecretStore {
    fn entry(account: &str) -> Result<Entry> {
        Entry::new(SERVICE, account).map_err(|e| Error::Secret(kr_msg(&e)))
    }
}

#[cfg(not(target_os = "ios"))]
impl SecretStore for KeychainSecretStore {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(|e| Error::Secret(kr_msg(&e)))
    }
    fn get(&self, account: &str) -> Result<Option<String>> {
        match Self::entry(account)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(KrError::NoEntry) => Ok(None),
            Err(e) => Err(Error::Secret(kr_msg(&e))),
        }
    }
    fn delete(&self, account: &str) -> Result<()> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(KrError::NoEntry) => Ok(()),
            Err(e) => Err(Error::Secret(kr_msg(&e))),
        }
    }
}

#[cfg(target_os = "ios")]
impl KeychainSecretStore {
    /// Entry with the new access policy ("after first unlock, this device only").
    fn entry(svc: &str, name: &str) -> Result<Entry> {
        init_ios_store()?;
        let mods = std::collections::HashMap::from([("access-policy", "after-first-unlock-this-device-only")]);
        Entry::new_with_modifiers(svc, name, &mods).map_err(|e| Error::Secret(kr_msg(&e)))
    }
}

#[cfg(target_os = "ios")]
impl RawItems for KeychainSecretStore {
    fn raw_get(&self, svc: &str, name: &str) -> Result<Option<String>> {
        match Self::entry(svc, name)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(KrError::NoEntry) => Ok(None),
            Err(e) => Err(Error::Secret(kr_msg(&e))),
        }
    }
    fn raw_set(&self, svc: &str, name: &str, value: &str) -> Result<()> {
        let e = Self::entry(svc, name)?;
        match e.delete_credential() {
            Ok(()) | Err(KrError::NoEntry) => {}
            Err(err) => return Err(Error::Secret(kr_msg(&err))),
        }
        e.set_password(value).map_err(|e| Error::Secret(kr_msg(&e)))
    }
    fn raw_delete(&self, svc: &str, name: &str) -> Result<()> {
        match Self::entry(svc, name)?.delete_credential() {
            Ok(()) | Err(KrError::NoEntry) => Ok(()),
            Err(e) => Err(Error::Secret(kr_msg(&e))),
        }
    }
}

#[cfg(target_os = "ios")]
impl SecretStore for KeychainSecretStore {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        migrate::set(self, account, secret)
    }
    fn get(&self, account: &str) -> Result<Option<String>> {
        migrate::get(self, account)
    }
    fn delete(&self, account: &str) -> Result<()> {
        migrate::delete(self, account)
    }
}

/// In-memory store for tests.
#[derive(Default)]
pub struct InMemorySecretStore {
    map: Mutex<HashMap<String, String>>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, String>>> {
        self.map.lock().map_err(|_| Error::Secret("poisoned".into()))
    }
}

impl SecretStore for InMemorySecretStore {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        self.lock()?.insert(account.into(), secret.into());
        Ok(())
    }
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.lock()?.get(account).cloned())
    }
    fn delete(&self, account: &str) -> Result<()> {
        self.lock()?.remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = SERVICE;
    const H: &str = HELPER_SERVICE;

    /// Fake keychain: remembers which policy generation each item was written with and can inject a failure.
    #[derive(Default)]
    struct Fake {
        items: Mutex<HashMap<(String, String), (String, bool)>>, // (service, name) -> (value, new_policy)
        fail_set_on: Mutex<Option<(String, String)>>,
        /// raw_get of this (service, name) returns an error
        fail_get_on: Mutex<Option<(String, String)>>,
        /// sleep this long once after the first read of the main account item (widens the race window)
        slow_main_get: Mutex<Option<std::time::Duration>>,
        /// sleep this long once inside the migration's temp-copy write (`#mig`), after the old value was read
        slow_mig_set: Mutex<Option<std::time::Duration>>,
    }
    impl Fake {
        fn legacy(&self, svc: &str, name: &str, v: &str) {
            self.items.lock().unwrap().insert((svc.into(), name.into()), (v.into(), false));
        }
        fn put(&self, svc: &str, name: &str, v: &str) {
            self.items.lock().unwrap().insert((svc.into(), name.into()), (v.into(), true));
        }
        fn val(&self, svc: &str, name: &str) -> Option<String> {
            self.items.lock().unwrap().get(&(svc.to_string(), name.to_string())).map(|x| x.0.clone())
        }
        fn policy_new(&self, svc: &str, name: &str) -> Option<bool> {
            self.items.lock().unwrap().get(&(svc.to_string(), name.to_string())).map(|x| x.1)
        }
        fn fail_on(&self, svc: &str, name: &str) {
            *self.fail_set_on.lock().unwrap() = Some((svc.into(), name.into()));
        }
    }
    impl RawItems for Fake {
        fn raw_get(&self, s: &str, n: &str) -> Result<Option<String>> {
            if self.fail_get_on.lock().unwrap().as_ref() == Some(&(s.to_string(), n.to_string())) {
                return Err(Error::Secret("injected get".into()));
            }
            let v = self.val(s, n);
            if s == M && !n.contains('#') {
                let d = self.slow_main_get.lock().unwrap().take();
                if let Some(d) = d {
                    std::thread::sleep(d);
                }
            }
            Ok(v)
        }
        fn raw_set(&self, s: &str, n: &str, v: &str) -> Result<()> {
            let key = (s.to_string(), n.to_string());
            if n.ends_with("#mig") {
                let d = self.slow_mig_set.lock().unwrap().take();
                if let Some(d) = d {
                    std::thread::sleep(d);
                }
            }
            if self.fail_set_on.lock().unwrap().as_ref() == Some(&key) {
                // simulates a crash after the old item was deleted and before the new one was added
                self.items.lock().unwrap().remove(&key);
                return Err(Error::Secret("injected".into()));
            }
            self.items.lock().unwrap().insert(key, (v.into(), true));
            Ok(())
        }
        fn raw_delete(&self, s: &str, n: &str) -> Result<()> {
            self.items.lock().unwrap().remove(&(s.to_string(), n.to_string()));
            Ok(())
        }
    }

    #[test]
    fn marker_read_error_does_not_fail_get() {
        for (svc, name) in [(H, "a@x.org#pol1"), (M, "a@x.org#pol1")] {
            let f = Fake::default();
            f.legacy(M, "a@x.org", "pw");
            *f.fail_get_on.lock().unwrap() = Some((svc.into(), name.into()));
            assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("pw"));
            // No write happened: the item keeps its legacy policy and no helper item was created.
            assert_eq!(f.policy_new(M, "a@x.org"), Some(false), "{svc} marker error: item rewritten");
            assert_eq!(f.items.lock().unwrap().len(), 1, "{svc} marker error: helper items created");
        }
    }

    #[test]
    fn legacy_item_is_migrated_on_read() {
        let f = Fake::default();
        f.legacy(M, "a@x.org", "pw");
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("pw"));
        assert_eq!(f.policy_new(M, "a@x.org"), Some(true));
        assert!(f.val(H, "a@x.org#pol1").is_some(), "marker lives in the helper service");
        assert!(f.val(M, "a@x.org#pol1").is_none());
        assert!(f.val(H, "a@x.org#mig").is_none() && f.val(M, "a@x.org#mig").is_none());
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("pw"));
    }

    #[test]
    fn interrupted_migration_never_loses_the_secret() {
        let f = Fake::default();
        f.legacy(M, "a@x.org", "pw");
        f.fail_on(M, "a@x.org");
        // read still returns the password; the original was deleted but the temp copy survives
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("pw"));
        assert!(f.val(M, "a@x.org").is_none());
        assert_eq!(f.val(H, "a@x.org#mig").as_deref(), Some("pw"));
        *f.fail_set_on.lock().unwrap() = None;
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("pw"));
        assert_eq!(f.policy_new(M, "a@x.org"), Some(true));
        assert!(f.val(H, "a@x.org#mig").is_none());
    }

    #[test]
    fn failed_set_keeps_the_new_value_and_next_get_restores_it() {
        let f = Fake::default();
        migrate::set(&f, "a@x.org", "old").unwrap();
        f.fail_on(M, "a@x.org");
        assert!(migrate::set(&f, "a@x.org", "new").is_err());
        assert!(f.val(M, "a@x.org").is_none());
        *f.fail_set_on.lock().unwrap() = None;
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("new"), "never the old one");
        assert_eq!(f.val(M, "a@x.org").as_deref(), Some("new"));
        assert!(f.val(H, "a@x.org#mig").is_none());
    }

    #[test]
    fn stale_temp_never_overwrites_a_newer_account_value() {
        let f = Fake::default();
        f.put(M, "a@x.org", "newer");
        f.put(H, "a@x.org#pol1", "1");
        f.put(H, "a@x.org#mig", "older");
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("newer"));
        assert_eq!(f.val(M, "a@x.org").as_deref(), Some("newer"));
        // also without a marker (migration re-run): the account value wins
        let g = Fake::default();
        g.put(M, "a@x.org", "newer");
        g.put(H, "a@x.org#mig", "older");
        assert_eq!(migrate::get(&g, "a@x.org").unwrap().as_deref(), Some("newer"));
        assert_eq!(g.val(M, "a@x.org").as_deref(), Some("newer"));
        assert!(g.val(H, "a@x.org#mig").is_none());
    }

    #[test]
    fn temp_is_deleted_before_the_marker_is_written() {
        // marker write fails: temp must already be gone, password intact, migration simply re-runs
        let f = Fake::default();
        f.legacy(M, "a@x.org", "pw");
        f.fail_on(H, "a@x.org#pol1");
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("pw"));
        assert!(f.val(H, "a@x.org#mig").is_none());
        assert!(f.val(H, "a@x.org#pol1").is_none());
        *f.fail_set_on.lock().unwrap() = None;
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("pw"));
        assert!(f.val(H, "a@x.org#pol1").is_some());
    }

    #[test]
    fn helper_items_live_only_in_the_helper_service() {
        let f = Fake::default();
        migrate::set(&f, "a@x.org", "pw").unwrap();
        migrate::set(&f, "caldav:7", "cal").unwrap();
        let items = f.items.lock().unwrap();
        let mut main: Vec<_> = items.keys().filter(|k| k.0 == M).map(|k| k.1.clone()).collect();
        main.sort();
        assert_eq!(main, vec!["a@x.org".to_string(), "caldav:7".to_string()], "only real keys under the main service");
        assert!(items.contains_key(&(H.to_string(), "a@x.org#pol1".to_string())));
    }

    #[test]
    fn first_version_marker_counts_as_migrated_and_old_helpers_are_cleaned() {
        let f = Fake::default();
        f.put(M, "a@x.org", "pw");
        f.put(M, "a@x.org#pol1", "1"); // what the phone already has
        f.put(M, "a@x.org#mig", "stale");
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("pw"));
        assert_eq!(f.val(M, "a@x.org").as_deref(), Some("pw"));
        assert!(f.val(M, "a@x.org#pol1").is_none() && f.val(M, "a@x.org#mig").is_none());
        assert!(f.val(H, "a@x.org#pol1").is_some());
        // an interrupted first-version migration (account gone, old temp left) restores from the old temp
        let g = Fake::default();
        g.put(M, "b@x.org#mig", "pw2");
        assert_eq!(migrate::get(&g, "b@x.org").unwrap().as_deref(), Some("pw2"));
        assert_eq!(g.val(M, "b@x.org").as_deref(), Some("pw2"));
        assert!(g.val(M, "b@x.org#mig").is_none());
    }

    #[test]
    fn set_and_delete_clean_up_helper_items() {
        let f = Fake::default();
        f.legacy(M, "a@x.org", "old");
        migrate::set(&f, "a@x.org", "new").unwrap();
        assert_eq!(migrate::get(&f, "a@x.org").unwrap().as_deref(), Some("new"));
        migrate::delete(&f, "a@x.org").unwrap();
        assert!(f.items.lock().unwrap().is_empty());
        assert_eq!(migrate::get(&f, "a@x.org").unwrap(), None);
    }

    #[test]
    fn migration_interleaved_with_set_keeps_the_new_password() {
        use std::sync::Arc;
        let f = Arc::new(Fake::default());
        f.legacy(M, "a@x.org", "old");
        // the delay sits inside the migration's write (after it read "old"), so without the lock set() would
        // finish first and the migration would then write "old" over "new"
        *f.slow_mig_set.lock().unwrap() = Some(std::time::Duration::from_millis(150));
        let g = f.clone();
        let t = std::thread::spawn(move || migrate::get(&*g, "a@x.org").unwrap());
        std::thread::sleep(std::time::Duration::from_millis(40));
        migrate::set(&*f, "a@x.org", "new").unwrap();
        t.join().unwrap();
        assert_eq!(migrate::get(&*f, "a@x.org").unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn migration_interleaved_with_delete_does_not_resurrect() {
        use std::sync::Arc;
        let f = Arc::new(Fake::default());
        f.legacy(M, "a@x.org", "old");
        *f.slow_main_get.lock().unwrap() = Some(std::time::Duration::from_millis(150));
        let g = f.clone();
        let t = std::thread::spawn(move || migrate::get(&*g, "a@x.org").unwrap());
        std::thread::sleep(std::time::Duration::from_millis(40));
        migrate::delete(&*f, "a@x.org").unwrap();
        t.join().unwrap();
        assert_eq!(migrate::get(&*f, "a@x.org").unwrap(), None);
        assert!(f.items.lock().unwrap().is_empty());
    }

    #[test]
    fn roundtrip() {
        let s = InMemorySecretStore::new();
        assert_eq!(s.get("a@x.org").unwrap(), None);
        s.set("a@x.org", "pw1").unwrap();
        assert_eq!(s.get("a@x.org").unwrap().as_deref(), Some("pw1"));
        s.set("a@x.org", "pw2").unwrap();
        assert_eq!(s.get("a@x.org").unwrap().as_deref(), Some("pw2"));
        s.delete("a@x.org").unwrap();
        s.delete("a@x.org").unwrap();
        assert_eq!(s.get("a@x.org").unwrap(), None);
    }

    #[test]
    fn usable_as_dyn() {
        let s: Box<dyn SecretStore> = Box::new(InMemorySecretStore::new());
        s.set("k", "v").unwrap();
        assert_eq!(s.get("k").unwrap().unwrap(), "v");
    }
}

#[cfg(test)]
mod q_tests {
    use super::*;

    #[test]
    fn bad_encoding_message_never_contains_the_bytes() {
        let m = kr_msg(&KrError::BadEncoding(b"hunter2".to_vec()));
        assert!(!m.contains("hunter2") && !m.contains("104"));
    }
}
