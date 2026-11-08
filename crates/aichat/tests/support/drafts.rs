//! In-memory `DraftHost` for draft tests, and a `FakeSmtp` whose counter proves "nothing was sent".
//! `DraftHost` has no send method; `FakeSmtp` is not reachable from any `aichat` API, so its counter can
//! only move if someone wires a sender into the model's path, which the source scan test also forbids.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use aichat::drafts::{Addr, DraftHost, ReplySource};
use aichat::tools::{BoxFut, Scope};

/// Counts SMTP sends. Any real sender would increment this.
pub static SMTP_SENDS: AtomicUsize = AtomicUsize::new(0);

pub struct FakeSmtp;
impl FakeSmtp {
    pub fn sends() -> usize {
        SMTP_SENDS.load(Ordering::SeqCst)
    }
}

pub struct FakeDrafts {
    pub sources: Mutex<HashMap<(String, u32), ReplySource>>,
    pub known: Mutex<HashSet<String>>,
    pub own: Vec<String>,
    pub calls: Mutex<Vec<String>>,
}

pub fn addr(name: &str, a: &str) -> Addr {
    Addr { name: name.into(), address: a.into() }
}

impl FakeDrafts {
    pub fn new() -> FakeDrafts {
        FakeDrafts { sources: Mutex::new(HashMap::new()), known: Mutex::new(HashSet::new()), own: vec!["me@example.org".into()], calls: Mutex::new(vec![]) }
    }
    pub fn add_source(&self, folder: &str, uid: u32, src: ReplySource) {
        self.sources.lock().unwrap().insert((folder.into(), uid), src);
    }
    pub fn know(&self, a: &str) {
        self.known.lock().unwrap().insert(a.to_lowercase());
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl DraftHost for FakeDrafts {
    fn reply_source(&self, scope: &Scope, uid: u32, _uidvalidity: u32) -> BoxFut<'_, Result<ReplySource, String>> {
        self.calls.lock().unwrap().push("reply_source".into());
        let r = self.sources.lock().unwrap().get(&(scope.folder.clone(), uid)).cloned().ok_or_else(|| "That message is no longer in the local copy.".to_string());
        Box::pin(async move { r })
    }
    fn address_known(&self, address: &str) -> Result<bool, String> {
        self.calls.lock().unwrap().push("address_known".into());
        Ok(self.known.lock().unwrap().contains(&address.to_lowercase()))
    }
    fn own_addresses(&self, _account: &str) -> Result<Vec<String>, String> {
        Ok(self.own.clone())
    }
}
