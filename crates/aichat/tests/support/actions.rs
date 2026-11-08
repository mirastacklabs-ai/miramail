//! In-memory `MailActionHost` for proposal tests. Every mutating call is recorded in `mutations`; a
//! test that must prove "nothing happened" asserts that list is empty. There is no way to send mail
//! through this type (the trait has no such method).
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use aichat::proposals::*;
use aichat::tools::{BoxFut, MsgMeta};

#[derive(Clone, Debug)]
pub struct FMsg {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub from: String,
    pub subject: String,
    pub seen: bool,
    pub flagged: bool,
    pub message_id: Option<String>,
}

#[derive(Default)]
pub struct FakeState {
    pub msgs: Vec<FMsg>,
    /// (account, folder) -> UIDVALIDITY; also the set of existing folders.
    pub validity: HashMap<(String, String), u32>,
    pub trash: Option<String>,
    pub archive: Option<String>,
    pub mutations: Vec<String>,
    pub move_calls: Vec<usize>,
    /// 1-based index of the move_batch call that fails as a whole.
    pub fail_move_call: Option<usize>,
    /// UIDs the server refuses to move / flag (per item).
    pub refuse_uids: HashSet<u32>,
    pub next_uid: HashMap<(String, String), u32>,
    /// Pretend COPYUID is unavailable and the new UID cannot be determined.
    pub hide_new_uid: bool,
    pub fail_create: bool,
    /// move_batch executes the move on the "server" and then reports that the connection died
    /// (the result is unknown to the caller). 1-based call index.
    pub lose_reply_on_move_call: Option<usize>,
    /// What the follow-up check says about a lost-reply move: `true` = it can tell (reports Moved with
    /// the new UID), `false` = unknown.
    pub verify_can_tell: bool,
    /// What the LOCAL CACHE (`lookup`) claims for `seen`, when it differs from the server's truth.
    pub stale_seen: HashMap<(String, u32), bool>,
}

pub struct FakeActions {
    pub st: Mutex<FakeState>,
}

pub const ACC: &str = "me@example.org";

impl FakeActions {
    pub fn new() -> FakeActions {
        let mut s = FakeState::default();
        for f in ["INBOX", "Trash", "Archive", "Receipts", "Secret"] {
            s.validity.insert((ACC.into(), f.into()), 7);
        }
        s.trash = Some("Trash".into());
        s.archive = Some("Archive".into());
        FakeActions { st: Mutex::new(s) }
    }

    pub fn add(&self, folder: &str, uid: u32, subject: &str, seen: bool) {
        let mut s = self.st.lock().unwrap();
        s.msgs.push(FMsg { account: ACC.into(), folder: folder.into(), uid, from: "Bob <bob@shop.test>".into(), subject: subject.into(), seen, flagged: false, message_id: Some(format!("{folder}-{uid}@x")) });
        let k = (ACC.to_string(), folder.to_string());
        let n = s.next_uid.entry(k).or_insert(1);
        *n = (*n).max(uid + 1);
    }

    pub fn mutations(&self) -> Vec<String> {
        self.st.lock().unwrap().mutations.clone()
    }

    pub fn uids(&self, folder: &str) -> Vec<u32> {
        let mut v: Vec<u32> = self.st.lock().unwrap().msgs.iter().filter(|m| m.folder == folder).map(|m| m.uid).collect();
        v.sort_unstable();
        v
    }

    pub fn set_validity(&self, folder: &str, v: u32) {
        self.st.lock().unwrap().validity.insert((ACC.into(), folder.into()), v);
    }

    pub fn remove(&self, folder: &str, uid: u32) {
        self.st.lock().unwrap().msgs.retain(|m| !(m.folder == folder && m.uid == uid));
    }

    fn meta(&self, m: &FMsg, v: u32) -> MsgMeta {
        MsgMeta {
            account: m.account.clone(),
            folder: m.folder.clone(),
            uid: m.uid,
            uidvalidity: v,
            subject: m.subject.clone(),
            from: m.from.clone(),
            to: "me@example.org".into(),
            date: 1_793_000_000,
            seen: m.seen,
            flagged: m.flagged,
            answered: false,
            snippet: String::new(),
            bulk: false,
        }
    }
}

impl MailActionHost for FakeActions {
    fn folder_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String> {
        Ok(self.st.lock().unwrap().validity.get(&(account.into(), folder.into())).copied())
    }

    fn lookup(&self, account: &str, folder: &str, uid: u32) -> Result<Option<MsgMeta>, String> {
        let s = self.st.lock().unwrap();
        let v = s.validity.get(&(account.to_string(), folder.to_string())).copied().unwrap_or(0);
        Ok(s.msgs.iter().find(|m| m.account == account && m.folder == folder && m.uid == uid).map(|m| {
            let mut meta = self.meta(m, v);
            if let Some(x) = s.stale_seen.get(&(folder.to_string(), uid)) {
                meta.seen = *x;
            }
            meta
        }))
    }

    fn role_folder(&self, _account: &str, role: RoleFolder) -> Result<Option<String>, String> {
        let s = self.st.lock().unwrap();
        Ok(match role {
            RoleFolder::Trash => s.trash.clone(),
            RoleFolder::Archive => s.archive.clone(),
        })
    }

    fn folder_exists(&self, account: &str, folder: &str) -> Result<bool, String> {
        Ok(self.st.lock().unwrap().validity.contains_key(&(account.into(), folder.into())))
    }

    fn find_folder(&self, account: &str, name: &str) -> Result<Option<String>, String> {
        Ok(self.st.lock().unwrap().validity.keys().find(|(a, f)| a == account && f.eq_ignore_ascii_case(name)).map(|(_, f)| f.clone()))
    }

    fn move_batch(&self, req: MoveReq) -> BoxFut<'_, Result<MoveRes, String>> {
        Box::pin(async move {
            let mut s = self.st.lock().unwrap();
            s.move_calls.push(req.items.len());
            let n = s.move_calls.len();
            if s.fail_move_call == Some(n) {
                return Err("The mail server did not answer.".into());
            }
            let key = (req.account.clone(), req.from_folder.clone());
            let cur = s.validity.get(&key).copied();
            if req.from_validity.is_some() && cur != req.from_validity {
                return Err("The folder changed on the server; search again.".into());
            }
            let dkey = (req.account.clone(), req.to_folder.clone());
            let Some(dest_validity) = s.validity.get(&dkey).copied() else { return Err("No such destination folder.".into()) };
            let mut out = Vec::new();
            let mut moved: Vec<u32> = Vec::new();
            for it in &req.items {
                let pos = s.msgs.iter().position(|m| m.account == req.account && m.folder == req.from_folder && (it.uid == Some(m.uid) || (it.uid.is_none() && it.message_id.is_some() && m.message_id == it.message_id)));
                match pos {
                    None => out.push(MoveItemRes { uid: it.uid, ok: false, error: Some("The message is no longer in the folder.".into()), new_uid: None, message_id: None, unknown: false }),
                    Some(p) if s.refuse_uids.contains(&s.msgs[p].uid) => out.push(MoveItemRes { uid: Some(s.msgs[p].uid), ok: false, error: Some("The server refused the move.".into()), new_uid: None, message_id: None, unknown: false }),
                    Some(p) => {
                        let mut m = s.msgs.remove(p);
                        let old = m.uid;
                        let next = s.next_uid.entry(dkey.clone()).or_insert(1);
                        m.uid = *next;
                        *next += 1;
                        let (new_uid, mid) = (m.uid, m.message_id.clone());
                        m.folder = req.to_folder.clone();
                        s.msgs.push(m);
                        moved.push(old);
                        let lost = s.lose_reply_on_move_call == Some(n);
                        if lost && !s.verify_can_tell {
                            out.push(MoveItemRes { uid: Some(old), ok: false, error: Some("The mail server did not answer in time.".into()), new_uid: None, message_id: mid, unknown: true });
                        } else {
                            out.push(MoveItemRes { uid: Some(old), ok: true, error: None, new_uid: if s.hide_new_uid { None } else { Some(new_uid) }, message_id: mid, unknown: false });
                        }
                    }
                }
            }
            s.mutations.push(format!("move {}/{} {:?} -> {}", req.account, req.from_folder, moved, req.to_folder));
            Ok(MoveRes { dest_validity: Some(dest_validity), items: out })
        })
    }

    fn flag_batch(&self, req: FlagReq) -> BoxFut<'_, Result<FlagRes, String>> {
        Box::pin(async move {
            let mut s = self.st.lock().unwrap();
            let cur = s.validity.get(&(req.account.clone(), req.folder.clone())).copied();
            if req.validity.is_some() && cur != req.validity {
                return Err("The folder changed on the server; search again.".into());
            }
            let mut items = Vec::new();
            let mut old = Vec::new();
            let mut changed = Vec::new();
            for uid in &req.uids {
                let refuse = s.refuse_uids.contains(uid);
                match s.msgs.iter_mut().find(|m| m.folder == req.folder && m.uid == *uid) {
                    Some(m) => {
                        let cur = if req.flag == "seen" { m.seen } else { m.flagged };
                        old.push(Some(cur));
                        if cur == req.value {
                            items.push((*uid, true, None)); // already as wanted: the server leaves it alone
                        } else if refuse {
                            items.push((*uid, false, Some("The server refused the change.".to_string())));
                        } else {
                            if req.flag == "seen" {
                                m.seen = req.value;
                            } else {
                                m.flagged = req.value;
                            }
                            changed.push(*uid);
                            items.push((*uid, true, None));
                        }
                    }
                    None => {
                        old.push(None);
                        items.push((*uid, false, Some("The message is no longer in the folder.".to_string())));
                    }
                }
            }
            s.mutations.push(format!("flag {} {}={} {:?}", req.folder, req.flag, req.value, changed));
            Ok(FlagRes { items, old })
        })
    }

    fn create_folder(&self, account: &str, name: &str) -> BoxFut<'_, Result<bool, String>> {
        let (account, name) = (account.to_string(), name.to_string());
        Box::pin(async move {
            let mut s = self.st.lock().unwrap();
            if s.fail_create {
                return Err("The server refused to create the folder.".into());
            }
            if s.validity.contains_key(&(account.clone(), name.clone())) {
                return Ok(false);
            }
            s.validity.insert((account.clone(), name.clone()), 11);
            s.mutations.push(format!("create {name}"));
            Ok(true)
        })
    }
}
