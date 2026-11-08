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

/// macOS Keychain (via the `keyring` crate), service "miramail".
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainSecretStore;

impl KeychainSecretStore {
    fn entry(account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, account).map_err(|e| Error::Secret(e.to_string()))
    }
}

impl SecretStore for KeychainSecretStore {
    fn set(&self, account: &str, secret: &str) -> Result<()> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(|e| Error::Secret(e.to_string()))
    }
    fn get(&self, account: &str) -> Result<Option<String>> {
        match Self::entry(account)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Secret(e.to_string())),
        }
    }
    fn delete(&self, account: &str) -> Result<()> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Secret(e.to_string())),
        }
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
