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

impl<S: SecretStore> SecretStore for CachedSecretStore<S> {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        self.inner.set(account, secret)?;
        self.map().insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        if let Some(v) = self.map().get(account) {
            return Ok(Some(v.clone()));
        }
        let v = self.inner.get(account)?;
        if let Some(s) = &v {
            self.map().insert(account.to_string(), s.clone());
        }
        Ok(v)
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.map().remove(account);
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
}
