//! Batch mailbox operations (AI chat P2): move, flag, trash, create folder.
//!
//! Safety properties (tested against a fake server below):
//! * every command names an explicit UID set; there is NO plain `EXPUNGE` anywhere in this module
//!   (the [`BatchServer`] trait has no such method). Without `UID MOVE` the fallback is
//!   `UID COPY` + `UID STORE +FLAGS \Deleted` + `UID EXPUNGE` of exactly the UIDs being moved, which
//!   needs UIDPLUS; a server with neither is refused BEFORE anything is copied.
//! * the source folder's UIDVALIDITY is re-checked on the server (`SELECT`) before any change;
//! * large batches are split into chunks of [`BATCH_CHUNK`] UIDs; a failing chunk stops the run and
//!   the remaining items are reported as not attempted;
//! * nothing here permanently deletes: "trash" is a move into the server's Trash folder.
//!
//! Every call returns one outcome per input item plus what an undo needs (the new UID in the
//! destination when it could be determined, the destination's UIDVALIDITY).

use std::collections::{HashMap, HashSet};

use crate::imap::{uid_ranges, ImapClient};
use crate::types::FlagChange;
use crate::{Error, Result};

/// UIDs per IMAP command.
pub const BATCH_CHUNK: usize = 500;
/// Longest folder name accepted for a new folder.
pub const MAX_FOLDER_NAME: usize = 100;

/// One message of a batch. `uid: None` means "find it by Message-ID in the source folder".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchItem {
    pub uid: Option<u32>,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemOutcome {
    /// The UID the item had in the source folder (after Message-ID resolution).
    pub uid: Option<u32>,
    pub ok: bool,
    pub error: Option<String>,
    /// UID of the message in the destination folder, when it could be determined.
    pub new_uid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveReport {
    pub src_validity: u32,
    pub dest_validity: Option<u32>,
    /// Parallel to the input items.
    pub outcomes: Vec<ItemOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagReport {
    pub validity: u32,
    /// `(uid, ok, error)` parallel to the input UIDs.
    pub outcomes: Vec<(u32, bool, Option<String>)>,
    /// The value the SERVER reported for the flag before this call (`None` = message not found),
    /// parallel to `outcomes`. A message that already had the wanted value is left untouched and
    /// reported `ok`.
    pub old: Vec<Option<bool>>,
}

/// What a check after a failed/interrupted move found for one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveVerdict {
    /// Still in the source folder: the move did not happen.
    NotMoved,
    /// Gone from the source and found (exactly once) in the destination, with its new UID.
    Moved(Option<u32>),
    /// Cannot tell.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub dest_validity: Option<u32>,
    /// Parallel to the input items.
    pub verdicts: Vec<MoveVerdict>,
}

/// The server operations the batch logic needs. There is deliberately no plain `EXPUNGE`.
pub(crate) trait BatchServer {
    fn supports_move(&self) -> bool;
    fn supports_uidplus(&self) -> bool;
    /// SELECT; returns `(uidvalidity, exists, uidnext)`.
    async fn select(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)>;
    /// EXAMINE (read-only select).
    async fn examine(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)>;
    /// STATUS `(UIDVALIDITY UIDNEXT)` of a folder without selecting it.
    async fn status(&mut self, folder: &str) -> Result<(Option<u32>, Option<u32>)>;
    /// Which of `set` still exist in the selected folder.
    async fn present(&mut self, set: &str) -> Result<Vec<u32>>;
    async fn uid_move(&mut self, set: &str, dest: &str) -> Result<()>;
    async fn uid_copy(&mut self, set: &str, dest: &str) -> Result<()>;
    async fn uid_store(&mut self, set: &str, query: &str) -> Result<()>;
    /// `UID EXPUNGE set` (UIDPLUS). Removes only the listed UIDs that carry `\Deleted`.
    async fn uid_expunge(&mut self, set: &str) -> Result<()>;
    /// `(uid, message-id)` of every message in the selected folder with UID >= `lo`.
    async fn message_ids_from(&mut self, lo: u32) -> Result<Vec<(u32, Option<String>)>>;
    /// UIDs in the selected folder whose Message-ID header CONTAINS `id` (IMAP HEADER search is a
    /// substring match; callers must confirm with [`BatchServer::message_ids_of`]).
    async fn search_message_id(&mut self, id: &str) -> Result<Vec<u32>>;
    /// `(uid, message-id)` of the listed UIDs of the selected folder (ids without `<>`).
    async fn message_ids_of(&mut self, set: &str) -> Result<Vec<(u32, Option<String>)>>;
    /// `(uid, seen, flagged)` of the listed UIDs of the selected folder, as the server has them now.
    async fn flags_of(&mut self, set: &str) -> Result<Vec<(u32, bool, bool)>>;
    async fn create(&mut self, name: &str) -> Result<()>;
    async fn list_names(&mut self) -> Result<Vec<String>>;
}

/// A new folder name the AI (or anyone) may ask for: printable ASCII letters, digits, space, `-`, `_`.
/// No hierarchy characters, so the name cannot escape into another namespace or collide with a
/// server's delimiter rules; no `INBOX`.
pub fn validate_new_folder_name(name: &str) -> std::result::Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        return Err("The folder name is empty.".into());
    }
    if n.chars().count() > MAX_FOLDER_NAME {
        return Err(format!("The folder name is longer than {MAX_FOLDER_NAME} characters."));
    }
    if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_') {
        return Err("Use only letters, digits, spaces, '-' and '_' in a new folder name.".into());
    }
    if n.contains("  ") || n.eq_ignore_ascii_case("inbox") {
        return Err("That folder name is not allowed.".into());
    }
    Ok(n.to_string())
}

fn check_validity(found: u32, expected: Option<u32>) -> Result<()> {
    if expected.is_some_and(|v| v != found) {
        return Err(Error::Imap("UIDVALIDITY changed; resync the folder".into()));
    }
    Ok(())
}

fn norm_id(s: &str) -> &str {
    s.trim().trim_start_matches('<').trim_end_matches('>')
}

/// UIDs in the selected folder whose Message-ID EQUALS `id` (angle brackets ignored). The IMAP
/// search only narrows the candidates (it is a substring match); each hit's header is fetched and
/// compared exactly, so `a@x` never matches `aa@x`.
pub(crate) async fn find_by_message_id<S: BatchServer>(srv: &mut S, id: &str) -> Result<Vec<u32>> {
    let want = norm_id(id);
    let mut hits = srv.search_message_id(want).await?;
    hits.sort_unstable();
    hits.dedup();
    let mut out = Vec::new();
    for chunk in hits.chunks(BATCH_CHUNK) {
        for (u, mid) in srv.message_ids_of(&uid_ranges(chunk)).await? {
            if chunk.contains(&u) && mid.as_deref().map(norm_id) == Some(want) {
                out.push(u);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// After a move failed or timed out (so the command may or may not have run): look at the server
/// and say, per item, whether it moved. Read-only (EXAMINE + SEARCH + FETCH).
pub(crate) async fn verify_move_with<S: BatchServer>(
    srv: &mut S,
    folder: &str,
    expected_validity: Option<u32>,
    dest: &str,
    items: &[BatchItem],
) -> Result<VerifyReport> {
    let mut verdicts = vec![MoveVerdict::Unknown; items.len()];
    let (v, _, _) = srv.examine(folder).await?;
    if expected_validity.is_some_and(|e| e != v) {
        return Ok(VerifyReport { dest_validity: None, verdicts });
    }
    let mut wanted: Vec<u32> = items.iter().filter_map(|i| i.uid).collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut present: HashSet<u32> = HashSet::new();
    for chunk in wanted.chunks(BATCH_CHUNK) {
        present.extend(srv.present(&uid_ranges(chunk)).await?);
    }
    let mut check_dest: Vec<usize> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        match (it.uid, &it.message_id) {
            (Some(u), _) if present.contains(&u) => verdicts[i] = MoveVerdict::NotMoved,
            (Some(_), _) => check_dest.push(i),
            (None, Some(id)) => match find_by_message_id(srv, id).await {
                Ok(f) if !f.is_empty() => verdicts[i] = MoveVerdict::NotMoved,
                Ok(_) => check_dest.push(i),
                Err(_) => {}
            },
            (None, None) => {}
        }
    }
    let mut dest_validity = None;
    if !check_dest.is_empty() {
        if let Ok((dv, _, _)) = srv.examine(dest).await {
            dest_validity = Some(dv);
            for i in check_dest {
                if let Some(id) = &items[i].message_id {
                    if let Ok(f) = find_by_message_id(srv, id).await {
                        if f.len() == 1 {
                            verdicts[i] = MoveVerdict::Moved(Some(f[0]));
                        }
                    }
                }
            }
        }
    }
    Ok(VerifyReport { dest_validity, verdicts })
}

/// Move `items` from `folder` to `dest`. See the module docs for the guarantees.
pub(crate) async fn batch_move_with<S: BatchServer>(
    srv: &mut S,
    folder: &str,
    expected_validity: Option<u32>,
    dest: &str,
    items: &[BatchItem],
) -> Result<MoveReport> {
    if dest == folder {
        return Err(Error::Invalid("the destination is the folder the messages are already in".into()));
    }
    if dest.trim().is_empty() || dest.chars().any(char::is_control) {
        return Err(Error::Invalid("invalid destination folder".into()));
    }
    if !srv.supports_move() && !srv.supports_uidplus() {
        // Without UID MOVE we need UID EXPUNGE; a plain EXPUNGE could remove other \Deleted messages.
        return Err(Error::Invalid("this mail server cannot move messages in a way that is safe for a batch (needs UID MOVE or UIDPLUS)".into()));
    }
    let (src_validity, _, _) = srv.select(folder).await?;
    check_validity(src_validity, expected_validity)?;

    // Resolve items given only by Message-ID.
    let mut uids: Vec<Option<u32>> = Vec::with_capacity(items.len());
    let mut resolve_err: Vec<Option<String>> = vec![None; items.len()];
    for (i, it) in items.iter().enumerate() {
        match (it.uid, &it.message_id) {
            (Some(u), _) => uids.push(Some(u)),
            (None, Some(id)) => match find_by_message_id(srv, id).await {
                Ok(v) if v.len() == 1 => uids.push(Some(v[0])),
                Ok(v) if v.is_empty() => {
                    uids.push(None);
                    resolve_err[i] = Some("The message was not found in the folder.".into());
                }
                Ok(_) => {
                    uids.push(None);
                    resolve_err[i] = Some("More than one message has that Message-ID; not touching any of them.".into());
                }
                Err(_) => {
                    uids.push(None);
                    resolve_err[i] = Some("The message could not be looked up.".into());
                }
            },
            (None, None) => {
                uids.push(None);
                resolve_err[i] = Some("The message cannot be located (it has no Message-ID).".into());
            }
        }
    }

    // Which of the UIDs still exist?
    let mut wanted: Vec<u32> = uids.iter().flatten().copied().collect();
    wanted.sort_unstable();
    wanted.dedup();
    let mut present: HashSet<u32> = HashSet::new();
    for chunk in wanted.chunks(BATCH_CHUNK) {
        present.extend(srv.present(&uid_ranges(chunk)).await?);
    }

    let (dest_validity, uidnext_before) = srv.status(dest).await?;

    // Move chunk by chunk. `status[uid]` = Ok(()) | Err(message).
    let mut result: HashMap<u32, std::result::Result<(), String>> = HashMap::new();
    let todo: Vec<u32> = wanted.iter().copied().filter(|u| present.contains(u)).collect();
    let mut stopped: Option<String> = None;
    for chunk in todo.chunks(BATCH_CHUNK) {
        if let Some(msg) = &stopped {
            for u in chunk {
                result.insert(*u, Err(msg.clone()));
            }
            continue;
        }
        let set = uid_ranges(chunk);
        let step: std::result::Result<(), (String, bool)> = if srv.supports_move() {
            srv.uid_move(&set, dest).await.map_err(|_| ("The server refused the move.".to_string(), false))
        } else {
            match srv.uid_copy(&set, dest).await {
                Err(_) => Err(("The server refused to copy the messages.".to_string(), false)),
                Ok(()) => match srv.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await {
                    Err(_) => Err(("Copied, but the original could not be marked for removal.".to_string(), true)),
                    Ok(()) => srv.uid_expunge(&set).await.map_err(|_| ("Copied, but the original could not be removed.".to_string(), true)),
                },
            }
        };
        match step {
            Ok(()) => {
                for u in chunk {
                    result.insert(*u, Ok(()));
                }
            }
            Err((msg, _copied)) => {
                for u in chunk {
                    result.insert(*u, Err(msg.clone()));
                }
                stopped = Some("Not attempted: an earlier batch failed.".to_string());
            }
        }
    }

    // New UIDs in the destination: everything that arrived since UIDNEXT-before, matched by Message-ID.
    let mut new_uids: HashMap<String, u32> = HashMap::new();
    let moved_any = result.values().any(|r| r.is_ok());
    if moved_any && dest_validity.is_some() {
        if let Some(lo) = uidnext_before {
            if items.iter().any(|i| i.message_id.is_some()) {
                if let Ok((v, _, _)) = srv.examine(dest).await {
                    if Some(v) == dest_validity {
                        if let Ok(rows) = srv.message_ids_from(lo).await {
                            let mut seen: HashMap<String, Vec<u32>> = HashMap::new();
                            for (u, id) in rows.into_iter().filter(|(u, _)| *u >= lo) {
                                if let Some(id) = id {
                                    seen.entry(id).or_default().push(u);
                                }
                            }
                            // Only unambiguous ids: one new message with it, and one moved item with it.
                            let mut moved_count: HashMap<&str, usize> = HashMap::new();
                            for (i, it) in items.iter().enumerate() {
                                if let (Some(id), Some(u)) = (&it.message_id, uids[i]) {
                                    if matches!(result.get(&u), Some(Ok(()))) {
                                        *moved_count.entry(id.as_str()).or_default() += 1;
                                    }
                                }
                            }
                            for (id, us) in seen {
                                if us.len() == 1 && moved_count.get(id.as_str()) == Some(&1) {
                                    new_uids.insert(id, us[0]);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let outcomes = items
        .iter()
        .enumerate()
        .map(|(i, it)| match uids[i] {
            None => ItemOutcome { uid: None, ok: false, error: resolve_err[i].clone(), new_uid: None },
            Some(u) => match result.get(&u) {
                Some(Ok(())) => ItemOutcome { uid: Some(u), ok: true, error: None, new_uid: it.message_id.as_ref().and_then(|id| new_uids.get(id).copied()) },
                Some(Err(m)) => ItemOutcome { uid: Some(u), ok: false, error: Some(m.clone()), new_uid: None },
                None => ItemOutcome { uid: Some(u), ok: false, error: Some("The message is no longer in the folder.".into()), new_uid: None },
            },
        })
        .collect();
    Ok(MoveReport { src_validity, dest_validity, outcomes })
}

/// Add or remove \Seen / \Flagged on `uids` of `folder`.
pub(crate) async fn batch_flag_with<S: BatchServer>(
    srv: &mut S,
    folder: &str,
    expected_validity: Option<u32>,
    uids: &[u32],
    change: FlagChange,
) -> Result<FlagReport> {
    let (validity, _, _) = srv.select(folder).await?;
    check_validity(validity, expected_validity)?;
    let mut wanted: Vec<u32> = uids.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    // The server's own current flags decide what needs doing (the local cache may be stale).
    let (is_seen, want) = match change {
        FlagChange::Seen(v) => (true, v),
        FlagChange::Flagged(v) => (false, v),
    };
    let mut state: HashMap<u32, bool> = HashMap::new();
    for chunk in wanted.chunks(BATCH_CHUNK) {
        for (u, seen, flagged) in srv.flags_of(&uid_ranges(chunk)).await? {
            if chunk.contains(&u) {
                state.insert(u, if is_seen { seen } else { flagged });
            }
        }
    }
    let cmd = crate::imap::flag_cmd(change);
    let mut res: HashMap<u32, std::result::Result<(), String>> = HashMap::new();
    let todo: Vec<u32> = wanted.iter().copied().filter(|u| state.get(u).is_some_and(|o| *o != want)).collect();
    for u in wanted.iter().filter(|u| state.get(u) == Some(&want)) {
        res.insert(*u, Ok(())); // already as wanted: nothing to send
    }
    let mut stopped = false;
    for chunk in todo.chunks(BATCH_CHUNK) {
        let r = if stopped { Err("Not attempted: an earlier batch failed.".to_string()) } else { srv.uid_store(&uid_ranges(chunk), &cmd).await.map_err(|_| "The server refused the change.".to_string()) };
        if r.is_err() {
            stopped = true;
        }
        for u in chunk {
            res.insert(*u, r.clone());
        }
    }
    let outcomes = uids
        .iter()
        .map(|u| match res.get(u) {
            Some(Ok(())) => (*u, true, None),
            Some(Err(m)) => (*u, false, Some(m.clone())),
            None => (*u, false, Some("The message is no longer in the folder.".to_string())),
        })
        .collect();
    let old = uids.iter().map(|u| state.get(u).copied()).collect();
    Ok(FlagReport { validity, outcomes, old })
}

/// Create a top-level folder. `Ok(true)` = created, `Ok(false)` = it already existed.
pub(crate) async fn create_folder_with<S: BatchServer>(srv: &mut S, name: &str) -> Result<bool> {
    let name = validate_new_folder_name(name).map_err(Error::Invalid)?;
    if srv.list_names().await?.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
        return Ok(false);
    }
    srv.create(&name).await?;
    Ok(true)
}

impl ImapClient {
    /// Move messages of `folder` to `dest` (UID MOVE, else COPY + UID STORE + UID EXPUNGE of exactly
    /// those UIDs). Aborts before changing anything if the folder's UIDVALIDITY differs from
    /// `expected_validity`. Never permanently deletes: the messages end up in `dest`.
    pub async fn batch_move(&mut self, folder: &str, expected_validity: Option<u32>, dest: &str, items: &[BatchItem]) -> Result<MoveReport> {
        batch_move_with(self, folder, expected_validity, dest, items).await
    }

    /// Move messages to the server's Trash folder (`trash_folder` comes from the folder roles).
    /// This is a move, never an expunge of the messages.
    pub async fn batch_trash(&mut self, folder: &str, expected_validity: Option<u32>, trash_folder: &str, items: &[BatchItem]) -> Result<MoveReport> {
        batch_move_with(self, folder, expected_validity, trash_folder, items).await
    }

    /// After a failed or timed-out move: where did each message end up? (read-only)
    pub async fn verify_move(&mut self, folder: &str, expected_validity: Option<u32>, dest: &str, items: &[BatchItem]) -> Result<VerifyReport> {
        verify_move_with(self, folder, expected_validity, dest, items).await
    }

    pub async fn batch_flag(&mut self, folder: &str, expected_validity: Option<u32>, uids: &[u32], change: FlagChange) -> Result<FlagReport> {
        batch_flag_with(self, folder, expected_validity, uids, change).await
    }

    /// Create a folder (see [`validate_new_folder_name`]). `Ok(false)` = it already existed.
    pub async fn create_folder(&mut self, name: &str) -> Result<bool> {
        create_folder_with(self, name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct Msg {
        uid: u32,
        id: Option<String>,
        deleted: bool,
        seen: bool,
    }

    struct Fake {
        mv: bool,
        uidplus: bool,
        validity: HashMap<String, u32>,
        boxes: HashMap<String, Vec<Msg>>,
        next: HashMap<String, u32>,
        selected: Option<String>,
        log: Vec<String>,
        fail_cmd_n: Option<(String, usize)>,
        count: HashMap<String, usize>,
    }

    impl Fake {
        fn new(mv: bool, uidplus: bool) -> Fake {
            let mut f = Fake { mv, uidplus, validity: HashMap::new(), boxes: HashMap::new(), next: HashMap::new(), selected: None, log: vec![], fail_cmd_n: None, count: HashMap::new() };
            for n in ["INBOX", "Trash", "Receipts"] {
                f.validity.insert(n.into(), 7);
                f.boxes.insert(n.into(), vec![]);
                f.next.insert(n.into(), 1);
            }
            f
        }
        fn add(&mut self, folder: &str, id: &str) -> u32 {
            let uid = self.next[folder];
            self.next.insert(folder.into(), uid + 1);
            self.boxes.get_mut(folder).unwrap().push(Msg { uid, id: Some(id.into()), deleted: false, seen: false });
            uid
        }
        fn uids(&self, folder: &str) -> Vec<u32> {
            self.boxes[folder].iter().map(|m| m.uid).collect()
        }
        fn sel(&self) -> String {
            self.selected.clone().expect("a folder is selected")
        }
        fn parse(set: &str) -> Vec<u32> {
            let mut v = vec![];
            for part in set.split(',') {
                if let Some((a, b)) = part.split_once(':') {
                    v.extend(a.parse::<u32>().unwrap()..=b.parse::<u32>().unwrap());
                } else {
                    v.push(part.parse().unwrap());
                }
            }
            v
        }
        fn tick(&mut self, kind: &str) -> Result<()> {
            let n = self.count.entry(kind.into()).or_default();
            *n += 1;
            if let Some((k, at)) = &self.fail_cmd_n {
                if k == kind && *n == *at {
                    return Err(Error::Imap("boom".into()));
                }
            }
            Ok(())
        }
        fn deliver(&mut self, src: &str, dest: &str, set: &[u32], remove: bool) {
            let moved: Vec<Msg> = self.boxes[src].iter().filter(|m| set.contains(&m.uid)).cloned().collect();
            for m in moved {
                let uid = self.next[dest];
                self.next.insert(dest.into(), uid + 1);
                self.boxes.get_mut(dest).unwrap().push(Msg { uid, id: m.id.clone(), deleted: false, seen: m.seen });
            }
            if remove {
                self.boxes.get_mut(src).unwrap().retain(|m| !set.contains(&m.uid));
            }
        }
    }

    impl BatchServer for Fake {
        fn supports_move(&self) -> bool {
            self.mv
        }
        fn supports_uidplus(&self) -> bool {
            self.uidplus
        }
        async fn select(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)> {
            self.log.push(format!("SELECT {folder}"));
            self.selected = Some(folder.into());
            Ok((self.validity[folder], self.boxes[folder].len() as u32, Some(self.next[folder])))
        }
        async fn examine(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)> {
            self.log.push(format!("EXAMINE {folder}"));
            self.selected = Some(folder.into());
            Ok((self.validity[folder], self.boxes[folder].len() as u32, Some(self.next[folder])))
        }
        async fn status(&mut self, folder: &str) -> Result<(Option<u32>, Option<u32>)> {
            self.log.push(format!("STATUS {folder}"));
            match self.validity.get(folder) {
                Some(v) => Ok((Some(*v), Some(self.next[folder]))),
                None => Err(Error::Imap("no such folder".into())),
            }
        }
        async fn present(&mut self, set: &str) -> Result<Vec<u32>> {
            let want = Fake::parse(set);
            let sel = self.sel();
            Ok(self.boxes[&sel].iter().map(|m| m.uid).filter(|u| want.contains(u)).collect())
        }
        async fn uid_move(&mut self, set: &str, dest: &str) -> Result<()> {
            self.log.push(format!("UID MOVE {set} {dest}"));
            self.tick("move")?;
            let (sel, uids) = (self.sel(), Fake::parse(set));
            self.deliver(&sel, dest, &uids, true);
            Ok(())
        }
        async fn uid_copy(&mut self, set: &str, dest: &str) -> Result<()> {
            self.log.push(format!("UID COPY {set} {dest}"));
            self.tick("copy")?;
            let (sel, uids) = (self.sel(), Fake::parse(set));
            self.deliver(&sel, dest, &uids, false);
            Ok(())
        }
        async fn uid_store(&mut self, set: &str, query: &str) -> Result<()> {
            self.log.push(format!("UID STORE {set} {query}"));
            self.tick("store")?;
            let (sel, uids) = (self.sel(), Fake::parse(set));
            for m in self.boxes.get_mut(&sel).unwrap().iter_mut().filter(|m| uids.contains(&m.uid)) {
                match query {
                    "+FLAGS.SILENT (\\Deleted)" => m.deleted = true,
                    "+FLAGS.SILENT (\\Seen)" => m.seen = true,
                    "-FLAGS.SILENT (\\Seen)" => m.seen = false,
                    _ => {}
                }
            }
            Ok(())
        }
        async fn uid_expunge(&mut self, set: &str) -> Result<()> {
            self.log.push(format!("UID EXPUNGE {set}"));
            self.tick("expunge")?;
            let (sel, uids) = (self.sel(), Fake::parse(set));
            // RFC 4315: only messages in the set that carry \Deleted go away.
            self.boxes.get_mut(&sel).unwrap().retain(|m| !(uids.contains(&m.uid) && m.deleted));
            Ok(())
        }
        async fn message_ids_from(&mut self, lo: u32) -> Result<Vec<(u32, Option<String>)>> {
            self.log.push(format!("FETCH {lo}:*"));
            let sel = self.sel();
            Ok(self.boxes[&sel].iter().filter(|m| m.uid >= lo).map(|m| (m.uid, m.id.clone())).collect())
        }
        async fn search_message_id(&mut self, id: &str) -> Result<Vec<u32>> {
            let sel = self.sel();
            // Like a real IMAP HEADER search: substring match.
            Ok(self.boxes[&sel].iter().filter(|m| m.id.as_deref().is_some_and(|x| x.contains(id))).map(|m| m.uid).collect())
        }
        async fn message_ids_of(&mut self, set: &str) -> Result<Vec<(u32, Option<String>)>> {
            let (sel, want) = (self.sel(), Fake::parse(set));
            Ok(self.boxes[&sel].iter().filter(|m| want.contains(&m.uid)).map(|m| (m.uid, m.id.clone())).collect())
        }
        async fn flags_of(&mut self, set: &str) -> Result<Vec<(u32, bool, bool)>> {
            let (sel, want) = (self.sel(), Fake::parse(set));
            Ok(self.boxes[&sel].iter().filter(|m| want.contains(&m.uid)).map(|m| (m.uid, m.seen, false)).collect())
        }
        async fn create(&mut self, name: &str) -> Result<()> {
            self.log.push(format!("CREATE {name}"));
            self.validity.insert(name.into(), 9);
            self.boxes.insert(name.into(), vec![]);
            self.next.insert(name.into(), 1);
            Ok(())
        }
        async fn list_names(&mut self) -> Result<Vec<String>> {
            Ok(self.boxes.keys().cloned().collect())
        }
    }

    fn items(f: &Fake, folder: &str, uids: &[u32]) -> Vec<BatchItem> {
        uids.iter().map(|u| BatchItem { uid: Some(*u), message_id: f.boxes[folder].iter().find(|m| m.uid == *u).and_then(|m| m.id.clone()) }).collect()
    }

    #[tokio::test]
    async fn uid_move_moves_exactly_the_listed_uids_and_reports_new_uids() {
        let mut f = Fake::new(true, true);
        for i in 0..6 {
            f.add("INBOX", &format!("m{i}@x"));
        }
        let it = items(&f, "INBOX", &[2, 3, 5]);
        let r = batch_move_with(&mut f, "INBOX", Some(7), "Receipts", &it).await.unwrap();
        assert_eq!(f.uids("INBOX"), vec![1, 4, 6]);
        assert_eq!(f.uids("Receipts"), vec![1, 2, 3]);
        assert!(r.outcomes.iter().all(|o| o.ok));
        assert_eq!(r.outcomes.iter().map(|o| o.new_uid).collect::<Vec<_>>(), vec![Some(1), Some(2), Some(3)]);
        assert_eq!(r.dest_validity, Some(7));
        assert!(f.log.iter().any(|l| l == "UID MOVE 2:3,5 Receipts"));
    }

    #[tokio::test]
    async fn copy_path_expunges_only_the_listed_uids_even_if_others_are_deleted() {
        let mut f = Fake::new(false, true);
        for i in 0..5 {
            f.add("INBOX", &format!("m{i}@x"));
        }
        // Another client flagged uid 4 \Deleted but has not expunged it.
        f.boxes.get_mut("INBOX").unwrap()[3].deleted = true;
        let it = items(&f, "INBOX", &[1, 2]);
        let r = batch_move_with(&mut f, "INBOX", Some(7), "Trash", &it).await.unwrap();
        assert!(r.outcomes.iter().all(|o| o.ok));
        assert_eq!(f.uids("INBOX"), vec![3, 4, 5], "uid 4 (deleted by someone else) must survive");
        assert_eq!(f.uids("Trash"), vec![1, 2]);
        let cmds: Vec<&String> = f.log.iter().filter(|l| l.starts_with("UID ")).collect();
        assert_eq!(cmds, vec!["UID COPY 1:2 Trash", "UID STORE 1:2 +FLAGS.SILENT (\\Deleted)", "UID EXPUNGE 1:2"]);
        assert!(!f.log.iter().any(|l| l == "EXPUNGE"), "no plain EXPUNGE ever");
    }

    #[tokio::test]
    async fn server_with_neither_move_nor_uidplus_is_refused_before_any_change() {
        let mut f = Fake::new(false, false);
        f.add("INBOX", "a@x");
        let it = items(&f, "INBOX", &[1]);
        let e = batch_move_with(&mut f, "INBOX", Some(7), "Trash", &it).await.unwrap_err();
        assert!(e.to_string().contains("UID MOVE or UIDPLUS"));
        assert_eq!(f.uids("INBOX"), vec![1]);
        assert!(f.log.is_empty(), "nothing was even selected: {:?}", f.log);
    }

    #[tokio::test]
    async fn stale_validity_aborts_without_changes() {
        let mut f = Fake::new(true, true);
        f.add("INBOX", "a@x");
        let it = items(&f, "INBOX", &[1]);
        let e = batch_move_with(&mut f, "INBOX", Some(6), "Trash", &it).await.unwrap_err();
        assert!(e.to_string().contains("UIDVALIDITY changed"));
        assert_eq!(f.uids("INBOX"), vec![1]);
        assert!(!f.log.iter().any(|l| l.starts_with("UID ")));
    }

    #[tokio::test]
    async fn missing_uids_fail_individually_and_same_folder_is_rejected() {
        let mut f = Fake::new(true, true);
        f.add("INBOX", "a@x");
        let it = vec![BatchItem { uid: Some(1), message_id: Some("a@x".into()) }, BatchItem { uid: Some(99), message_id: None }];
        let r = batch_move_with(&mut f, "INBOX", Some(7), "Receipts", &it).await.unwrap();
        assert!(r.outcomes[0].ok);
        assert!(!r.outcomes[1].ok && r.outcomes[1].error.as_deref().unwrap().contains("no longer"));
        assert!(batch_move_with(&mut f, "INBOX", Some(7), "INBOX", &it).await.is_err());
    }

    #[tokio::test]
    async fn large_batches_are_chunked_and_a_failing_chunk_stops_the_run() {
        let mut f = Fake::new(true, true);
        for i in 0..1200 {
            f.add("INBOX", &format!("m{i}@x"));
        }
        let all: Vec<u32> = (1..=1200).collect();
        let it = items(&f, "INBOX", &all);
        f.fail_cmd_n = Some(("move".into(), 2)); // second chunk fails
        let r = batch_move_with(&mut f, "INBOX", Some(7), "Receipts", &it).await.unwrap();
        let ok = r.outcomes.iter().filter(|o| o.ok).count();
        assert_eq!(ok, 500, "only the first chunk of {BATCH_CHUNK} landed");
        assert_eq!(f.uids("Receipts").len(), 500);
        assert_eq!(f.uids("INBOX").len(), 700);
        assert!(r.outcomes[500].error.as_deref().unwrap().contains("refused"));
        assert!(r.outcomes[1100].error.as_deref().unwrap().contains("Not attempted"));
        assert_eq!(f.log.iter().filter(|l| l.starts_with("UID MOVE")).count(), 2);
    }

    #[tokio::test]
    async fn copied_but_not_removed_is_reported_honestly() {
        let mut f = Fake::new(false, true);
        f.add("INBOX", "a@x");
        let it = items(&f, "INBOX", &[1]);
        f.fail_cmd_n = Some(("expunge".into(), 1));
        let r = batch_move_with(&mut f, "INBOX", Some(7), "Trash", &it).await.unwrap();
        assert!(!r.outcomes[0].ok);
        assert!(r.outcomes[0].error.as_deref().unwrap().starts_with("Copied, but"));
    }

    #[tokio::test]
    async fn undo_by_message_id_when_the_uid_is_unknown() {
        let mut f = Fake::new(true, true);
        f.add("Trash", "a@x");
        f.add("Trash", "b@x");
        let it = vec![BatchItem { uid: None, message_id: Some("b@x".into()) }, BatchItem { uid: None, message_id: None }];
        let r = batch_move_with(&mut f, "Trash", Some(7), "INBOX", &it).await.unwrap();
        assert!(r.outcomes[0].ok && r.outcomes[0].uid == Some(2));
        assert!(!r.outcomes[1].ok);
        assert_eq!(f.uids("Trash"), vec![1]);
    }

    #[tokio::test]
    async fn ambiguous_message_ids_leave_new_uid_unresolved() {
        let mut f = Fake::new(true, true);
        f.add("INBOX", "dup@x");
        f.add("INBOX", "dup@x");
        let it = items(&f, "INBOX", &[1, 2]);
        let r = batch_move_with(&mut f, "INBOX", Some(7), "Receipts", &it).await.unwrap();
        assert!(r.outcomes.iter().all(|o| o.ok && o.new_uid.is_none()));
    }

    #[tokio::test]
    async fn batch_flag_touches_only_listed_present_uids() {
        let mut f = Fake::new(true, true);
        for i in 0..4 {
            f.add("INBOX", &format!("m{i}@x"));
        }
        let r = batch_flag_with(&mut f, "INBOX", Some(7), &[2, 3, 42], FlagChange::Seen(true)).await.unwrap();
        assert_eq!(r.outcomes.iter().map(|o| o.1).collect::<Vec<_>>(), vec![true, true, false]);
        let seen: Vec<u32> = f.boxes["INBOX"].iter().filter(|m| m.seen).map(|m| m.uid).collect();
        assert_eq!(seen, vec![2, 3]);
        assert!(f.log.iter().any(|l| l == "UID STORE 2:3 +FLAGS.SILENT (\\Seen)"));
        assert!(batch_flag_with(&mut f, "INBOX", Some(1), &[2], FlagChange::Seen(true)).await.is_err());
    }

    #[tokio::test]
    async fn message_id_lookup_is_exact_not_substring() {
        let mut f = Fake::new(true, true);
        f.add("Trash", "aa@x");
        f.add("Trash", "a@x");
        f.add("Trash", "ba@x.org");
        f.selected = Some("Trash".into());
        assert_eq!(f.search_message_id("a@x").await.unwrap().len(), 3, "the raw search is a substring match");
        assert_eq!(find_by_message_id(&mut f, "a@x").await.unwrap(), vec![2]);
        assert_eq!(find_by_message_id(&mut f, "<a@x>").await.unwrap(), vec![2]);
        // a move by Message-ID takes only the exact message
        let it = vec![BatchItem { uid: None, message_id: Some("a@x".into()) }];
        let r = batch_move_with(&mut f, "Trash", Some(7), "INBOX", &it).await.unwrap();
        assert!(r.outcomes[0].ok && r.outcomes[0].uid == Some(2));
        assert_eq!(f.uids("Trash"), vec![1, 3]);
        // only a longer id exists -> not found rather than "more than one"
        let it = vec![BatchItem { uid: None, message_id: Some("b@x".into()) }];
        let r = batch_move_with(&mut f, "Trash", Some(7), "INBOX", &it).await.unwrap();
        assert!(!r.outcomes[0].ok && r.outcomes[0].error.as_deref().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn flag_uses_server_state_and_reports_the_old_value() {
        let mut f = Fake::new(true, true);
        for i in 0..4 {
            f.add("INBOX", &format!("m{i}@x"));
        }
        f.boxes.get_mut("INBOX").unwrap()[1].seen = true; // uid 2 already read on the server
        let r = batch_flag_with(&mut f, "INBOX", Some(7), &[1, 2, 9], FlagChange::Seen(true)).await.unwrap();
        assert_eq!(r.old, vec![Some(false), Some(true), None]);
        assert_eq!(r.outcomes.iter().map(|o| o.1).collect::<Vec<_>>(), vec![true, true, false]);
        assert!(f.log.iter().any(|l| l == "UID STORE 1 +FLAGS.SILENT (\\Seen)"), "already-read uid 2 is not sent: {:?}", f.log);
    }

    #[tokio::test]
    async fn verify_move_tells_moved_from_not_moved_from_unknown() {
        let mut f = Fake::new(true, true);
        for i in 0..4 {
            f.add("INBOX", &format!("m{i}@x"));
        }
        let it = items(&f, "INBOX", &[1, 2, 3]);
        // the server executed the move of 2 and 3 (the reply was lost); 1 stayed
        f.select("INBOX").await.unwrap();
        f.uid_move("2:3", "Receipts").await.unwrap();
        // 3's copy vanished from the destination afterwards (deleted by someone) -> cannot tell
        f.boxes.get_mut("Receipts").unwrap().retain(|m| m.id.as_deref() != Some("m2@x"));
        let r = verify_move_with(&mut f, "INBOX", Some(7), "Receipts", &it).await.unwrap();
        assert_eq!(r.verdicts, vec![MoveVerdict::NotMoved, MoveVerdict::Moved(Some(1)), MoveVerdict::Unknown]);
        assert_eq!(r.dest_validity, Some(7));
        // changed UIDVALIDITY: everything unknown
        let r = verify_move_with(&mut f, "INBOX", Some(5), "Receipts", &it).await.unwrap();
        assert!(r.verdicts.iter().all(|v| *v == MoveVerdict::Unknown));
    }

    #[tokio::test]
    async fn create_folder_validates_and_is_idempotent() {
        let mut f = Fake::new(true, true);
        assert!(create_folder_with(&mut f, "Tax 2026").await.unwrap());
        assert!(!create_folder_with(&mut f, "tax 2026").await.unwrap());
        for bad in ["", "  ", "a/b", "a.b", "INBOX", "x\r\nA1 DELETE INBOX", "caf\u{e9}", "a*", "a  b", &"x".repeat(101)] {
            assert!(create_folder_with(&mut f, bad).await.is_err(), "{bad:?}");
        }
        assert_eq!(f.log.iter().filter(|l| l.starts_with("CREATE")).count(), 1);
    }
}
