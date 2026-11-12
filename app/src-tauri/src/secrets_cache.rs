//! Process-lifetime cache in front of the Keychain.
//!
//! Every IMAP command used to read the password from the macOS Keychain, so each
//! sync could raise a Keychain prompt. The cache reads each account's secret at most
//! once per launch (one prompt at most) and drops it on delete. Secrets live only in
//! process memory, exactly as they already do while a connection is open.

use mailcore::{Result, SecretStore};
use std::collections::HashMap;
use std::sync::Mutex;

pub struct CachedSecretStore<S: SecretStore> {
    inner: S,
    cache: Mutex<HashMap<String, String>>,
}

impl<S: SecretStore> CachedSecretStore<S> {
    pub fn new(inner: S) -> Self {
        Self { inner, cache: Mutex::new(HashMap::new()) }
    }
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// One lock is held across each whole operation (inner call + cache update), so a slow read-through can never
/// insert a stale value after a concurrent set/delete finished.
impl<S: SecretStore> SecretStore for CachedSecretStore<S> {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        let mut m = self.map();
        match self.inner.set(account, secret) {
            Ok(()) => {
                m.insert(account.to_string(), secret.to_string());
                Ok(())
            }
            Err(e) => {
                // the stored value is unknown now: never serve the old cached one
                m.remove(account);
                Err(e)
            }
        }
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        let mut m = self.map();
        if let Some(v) = m.get(account) {
            return Ok(Some(v.clone()));
        }
        let v = self.inner.get(account)?;
        if let Some(s) = &v {
            m.insert(account.to_string(), s.clone());
        }
        Ok(v)
    }

    fn delete(&self, account: &str) -> Result<()> {
        let mut m = self.map();
        m.remove(account);
        self.inner.delete(account)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct Counting {
        gets: Arc<AtomicUsize>,
        data: Mutex<HashMap<String, String>>,
    }
    impl SecretStore for Counting {
        fn set(&self, a: &str, s: &str) -> Result<()> {
            self.data.lock().unwrap().insert(a.into(), s.into());
            Ok(())
        }
        fn get(&self, a: &str) -> Result<Option<String>> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            Ok(self.data.lock().unwrap().get(a).cloned())
        }
        fn delete(&self, a: &str) -> Result<()> {
            self.data.lock().unwrap().remove(a);
            Ok(())
        }
    }

    #[test]
    fn reads_the_backing_store_once_per_account() {
        let gets = Arc::new(AtomicUsize::new(0));
        let inner = Counting { gets: gets.clone(), data: Mutex::new(HashMap::new()) };
        inner.set("a@x.org", "pw").unwrap();
        let c = CachedSecretStore::new(inner);
        for _ in 0..10 {
            assert_eq!(c.get("a@x.org").unwrap().as_deref(), Some("pw"));
        }
        assert_eq!(gets.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn delete_and_missing_are_not_cached() {
        let gets = Arc::new(AtomicUsize::new(0));
        let c = CachedSecretStore::new(Counting { gets: gets.clone(), data: Mutex::new(HashMap::new()) });
        assert_eq!(c.get("none@x.org").unwrap(), None);
        assert_eq!(c.get("none@x.org").unwrap(), None);
        assert_eq!(gets.load(Ordering::SeqCst), 2);
        c.set("a@x.org", "pw").unwrap();
        c.delete("a@x.org").unwrap();
        assert_eq!(c.get("a@x.org").unwrap(), None);
    }

    #[test]
    fn set_updates_the_cache() {
        let c = CachedSecretStore::new(Counting { gets: Arc::new(AtomicUsize::new(0)), data: Mutex::new(HashMap::new()) });
        c.set("a@x.org", "old").unwrap();
        c.set("a@x.org", "new").unwrap();
        assert_eq!(c.get("a@x.org").unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn failed_set_evicts_the_cached_value() {
        struct FailSet(Mutex<HashMap<String, String>>, std::sync::atomic::AtomicBool);
        impl SecretStore for FailSet {
            fn set(&self, a: &str, s: &str) -> Result<()> {
                if self.1.load(Ordering::SeqCst) {
                    return Err(mailcore::Error::Secret("injected".into()));
                }
                self.0.lock().unwrap().insert(a.into(), s.into());
                Ok(())
            }
            fn get(&self, a: &str) -> Result<Option<String>> {
                Ok(self.0.lock().unwrap().get(a).cloned())
            }
            fn delete(&self, a: &str) -> Result<()> {
                self.0.lock().unwrap().remove(a);
                Ok(())
            }
        }
        let c = CachedSecretStore::new(FailSet(Mutex::new(HashMap::new()), std::sync::atomic::AtomicBool::new(false)));
        c.set("a@x.org", "old").unwrap();
        c.inner.1.store(true, Ordering::SeqCst);
        assert!(c.set("a@x.org", "new").is_err());
        assert!(!c.map().contains_key("a@x.org"));
        // reads go to the backing store, not a stale cache entry
        assert_eq!(c.get("a@x.org").unwrap().as_deref(), Some("old"));
    }

    #[test]
    fn slow_read_cannot_overwrite_a_concurrent_set() {
        struct Slow {
            data: Mutex<HashMap<String, String>>,
            slow: std::sync::atomic::AtomicBool,
        }
        impl SecretStore for Slow {
            fn set(&self, a: &str, s: &str) -> Result<()> {
                self.data.lock().unwrap().insert(a.into(), s.into());
                Ok(())
            }
            fn get(&self, a: &str) -> Result<Option<String>> {
                let v = self.data.lock().unwrap().get(a).cloned();
                if self.slow.swap(false, Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
                Ok(v)
            }
            fn delete(&self, a: &str) -> Result<()> {
                self.data.lock().unwrap().remove(a);
                Ok(())
            }
        }
        let inner = Slow { data: Mutex::new(HashMap::new()), slow: std::sync::atomic::AtomicBool::new(false) };
        inner.set("a@x.org", "old").unwrap();
        inner.slow.store(true, Ordering::SeqCst);
        let c = Arc::new(CachedSecretStore::new(inner));
        let c2 = c.clone();
        let t = std::thread::spawn(move || c2.get("a@x.org").unwrap());
        std::thread::sleep(std::time::Duration::from_millis(40));
        c.set("a@x.org", "new").unwrap();
        t.join().unwrap();
        assert_eq!(c.get("a@x.org").unwrap().as_deref(), Some("new"));
    }
}
