//! Mirror of compose drafts into the account's IMAP Drafts folder.
//!
//! A mirror write is "APPEND the new copy (flags `\Seen \Draft`), find its UID by its unique Message-ID, then
//! delete the previous copy": append first, so a failure never loses the only server copy. Deleting is
//! `UID STORE +FLAGS (\Deleted)` + `UID EXPUNGE` of exactly the UID we appended earlier, after checking that
//! the UID still carries our Message-ID and that UIDVALIDITY is unchanged. There is deliberately no plain
//! `EXPUNGE`, so other messages flagged `\Deleted` are never touched. Servers without UIDPLUS are skipped
//! silently (`Ok(None)`): we can neither learn nor safely remove the UID there.

use crate::batch::BatchServer;
use crate::{Error, ImapClient, Result};

/// Error text when an appended draft could not be found again: a copy exists on the server that we do not track.
pub const UNLOCATED: &str = "could not locate the appended draft";

/// Where the server copy of one mirrored draft lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRef {
    pub folder: String,
    pub validity: u32,
    pub uid: u32,
    /// Message-ID without `<>`.
    pub message_id: String,
}

pub(crate) trait DraftServer: BatchServer {
    async fn append_draft(&mut self, folder: &str, raw: &[u8]) -> Result<()>;
}

/// The `Message-ID` header value of a raw message, without `<>`.
pub fn extract_message_id(raw: &[u8]) -> Option<String> {
    let head_end = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(raw.len().min(16 * 1024));
    let head = String::from_utf8_lossy(&raw[..head_end]);
    let mut lines = head.split("\r\n").peekable();
    while let Some(l) = lines.next() {
        if l.len() >= 11 && l.is_char_boundary(11) && l[..11].eq_ignore_ascii_case("message-id:") {
            let mut v = l[11..].trim().to_string();
            // Folded value on the next line.
            if v.is_empty() {
                if let Some(n) = lines.peek().filter(|n| n.starts_with(' ') || n.starts_with('\t')) {
                    v = n.trim().to_string();
                }
            }
            let v = v.trim_start_matches('<').trim_end_matches('>').trim().to_string();
            return (!v.is_empty()).then_some(v);
        }
    }
    None
}

async fn confirm_uid<S: BatchServer>(srv: &mut S, uid: u32, message_id: &str) -> Result<bool> {
    Ok(srv.message_ids_of(&uid.to_string()).await?.iter().any(|(u, id)| *u == uid && id.as_deref() == Some(message_id)))
}

/// Delete exactly `prev` if it is provably still the draft we appended. Returns whether something was removed.
pub(crate) async fn remove_with<S: BatchServer>(srv: &mut S, prev: &DraftRef) -> Result<bool> {
    if !srv.supports_uidplus() {
        return Ok(false);
    }
    let (validity, _, _) = srv.select(&prev.folder).await?;
    if validity != prev.validity || !confirm_uid(srv, prev.uid, &prev.message_id).await? {
        return Ok(false);
    }
    let set = prev.uid.to_string();
    srv.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await?;
    srv.uid_expunge(&set).await?;
    Ok(true)
}

/// Append `raw` as a draft and retire `prev`. `Ok(None)`: the server lacks UIDPLUS, nothing was done.
pub(crate) async fn mirror_with<S: DraftServer>(srv: &mut S, folder: &str, raw: &[u8], prev: Option<&DraftRef>) -> Result<Option<DraftRef>> {
    if !srv.supports_uidplus() {
        return Ok(None);
    }
    let mid = extract_message_id(raw).ok_or_else(|| Error::Invalid("draft has no Message-ID".into()))?;
    // UIDNEXT before the APPEND: everything the server stores from now on has a UID >= this, which lets us find our
    // copy by header scan even when the server's SEARCH index lags behind (async indexing).
    let uid_next_before = srv.select(folder).await.ok().and_then(|(_, _, n)| n);
    srv.append_draft(folder, raw).await?;
    let (validity, _, _) = srv.select(folder).await?;
    let hits = srv.search_message_id(&mid).await?;
    let mut found: Vec<u32> = Vec::new();
    if !hits.is_empty() {
        let set = hits.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        found = srv.message_ids_of(&set).await?.into_iter().filter(|(_, id)| id.as_deref() == Some(mid.as_str())).map(|(u, _)| u).collect();
    }
    if found.is_empty() {
        if let Some(lo) = uid_next_before {
            found = srv.message_ids_from(lo).await?.into_iter().filter(|(_, id)| id.as_deref() == Some(mid.as_str())).map(|(u, _)| u).collect();
        }
    }
    let [uid] = found[..] else {
        return Err(Error::Imap(UNLOCATED.into()));
    };
    let new = DraftRef { folder: folder.to_string(), validity, uid, message_id: mid };
    if let Some(p) = prev.filter(|p| p.folder == folder && p.validity == validity) {
        // The new copy is safe on the server; failing to retire the old one is not an error.
        let _ = remove_with(srv, p).await;
    }
    Ok(Some(new))
}

impl DraftServer for ImapClient {
    async fn append_draft(&mut self, folder: &str, raw: &[u8]) -> Result<()> {
        self.append_flagged(folder, raw, "(\\Seen \\Draft)").await
    }
}

/// Mirror a draft over a live IMAP session (see module docs).
pub async fn mirror_draft(client: &mut ImapClient, folder: &str, raw: &[u8], prev: Option<&DraftRef>) -> Result<Option<DraftRef>> {
    mirror_with(client, folder, raw, prev).await
}

/// Remove a mirrored draft's server copy (exact UID, verified).
pub async fn remove_draft(client: &mut ImapClient, prev: &DraftRef) -> Result<bool> {
    remove_with(client, prev).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Clone)]
    struct Msg {
        uid: u32,
        id: Option<String>,
        deleted: bool,
        flags: String,
    }

    /// In-memory server: one validity per folder, UIDs ascending, RFC 4315 `UID EXPUNGE`.
    struct Fake {
        uidplus: bool,
        validity: u32,
        next: u32,
        boxes: HashMap<String, Vec<Msg>>,
        sel: String,
        log: Vec<String>,
        fail_append: bool,
        hide_appended: bool,
        /// Search index lags (SEARCH finds nothing) but FETCH sees the message.
        lagging_search: bool,
    }

    impl Fake {
        fn new(uidplus: bool) -> Self {
            let mut boxes = HashMap::new();
            boxes.insert("Drafts".to_string(), vec![]);
            Fake { uidplus, validity: 5, next: 1, boxes, sel: String::new(), log: vec![], fail_append: false, hide_appended: false, lagging_search: false }
        }
        fn add(&mut self, folder: &str, id: &str, deleted: bool) -> u32 {
            let uid = self.next;
            self.next += 1;
            self.boxes.get_mut(folder).unwrap().push(Msg { uid, id: Some(id.into()), deleted, flags: String::new() });
            uid
        }
        fn uids(&self, folder: &str) -> Vec<u32> {
            self.boxes[folder].iter().map(|m| m.uid).collect()
        }
        fn parse(set: &str) -> Vec<u32> {
            set.split(',').filter_map(|x| x.parse().ok()).collect()
        }
    }

    impl BatchServer for Fake {
        fn supports_move(&self) -> bool {
            false
        }
        fn supports_uidplus(&self) -> bool {
            self.uidplus
        }
        async fn select(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)> {
            self.log.push(format!("SELECT {folder}"));
            self.sel = folder.into();
            Ok((self.validity, self.boxes[folder].len() as u32, Some(self.next)))
        }
        async fn examine(&mut self, folder: &str) -> Result<(u32, u32, Option<u32>)> {
            self.select(folder).await
        }
        async fn status(&mut self, _: &str) -> Result<(Option<u32>, Option<u32>)> {
            Ok((Some(self.validity), Some(self.next)))
        }
        async fn present(&mut self, _: &str) -> Result<Vec<u32>> {
            Ok(vec![])
        }
        async fn uid_move(&mut self, _: &str, _: &str) -> Result<()> {
            panic!("a draft mirror never moves mail")
        }
        async fn uid_copy(&mut self, _: &str, _: &str) -> Result<()> {
            panic!("a draft mirror never copies mail")
        }
        async fn uid_store(&mut self, set: &str, query: &str) -> Result<()> {
            self.log.push(format!("UID STORE {set} {query}"));
            let want = Fake::parse(set);
            let sel = self.sel.clone();
            for m in self.boxes.get_mut(&sel).unwrap().iter_mut().filter(|m| want.contains(&m.uid)) {
                if query == "+FLAGS.SILENT (\\Deleted)" {
                    m.deleted = true;
                }
            }
            Ok(())
        }
        async fn uid_expunge(&mut self, set: &str) -> Result<()> {
            self.log.push(format!("UID EXPUNGE {set}"));
            let want = Fake::parse(set);
            let sel = self.sel.clone();
            self.boxes.get_mut(&sel).unwrap().retain(|m| !(want.contains(&m.uid) && m.deleted));
            Ok(())
        }
        async fn message_ids_from(&mut self, lo: u32) -> Result<Vec<(u32, Option<String>)>> {
            let sel = self.sel.clone();
            Ok(self.boxes[&sel].iter().filter(|m| !self.hide_appended && m.uid >= lo).map(|m| (m.uid, m.id.clone())).collect())
        }
        async fn search_message_id(&mut self, id: &str) -> Result<Vec<u32>> {
            let sel = self.sel.clone();
            Ok(self.boxes[&sel].iter().filter(|m| !self.hide_appended && !self.lagging_search && m.id.as_deref().is_some_and(|x| x.contains(id))).map(|m| m.uid).collect())
        }
        async fn message_ids_of(&mut self, set: &str) -> Result<Vec<(u32, Option<String>)>> {
            let (sel, want) = (self.sel.clone(), Fake::parse(set));
            Ok(self.boxes[&sel].iter().filter(|m| want.contains(&m.uid)).map(|m| (m.uid, m.id.clone())).collect())
        }
        async fn flags_of(&mut self, _: &str) -> Result<Vec<(u32, bool, bool)>> {
            Ok(vec![])
        }
        async fn create(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn list_names(&mut self) -> Result<Vec<String>> {
            Ok(self.boxes.keys().cloned().collect())
        }
    }

    impl DraftServer for Fake {
        async fn append_draft(&mut self, folder: &str, raw: &[u8]) -> Result<()> {
            self.log.push(format!("APPEND {folder}"));
            if self.fail_append {
                return Err(Error::Imap("no".into()));
            }
            let id = extract_message_id(raw).unwrap();
            let uid = self.add(folder, &id, false);
            self.boxes.get_mut(folder).unwrap().iter_mut().find(|m| m.uid == uid).unwrap().flags = "(\\Seen \\Draft)".into();
            Ok(())
        }
    }

    fn raw(id: &str) -> Vec<u8> {
        format!("From: a@x.org\r\nMessage-ID: <{id}>\r\nSubject: s\r\n\r\nbody").into_bytes()
    }

    #[test]
    fn extracts_message_ids() {
        assert_eq!(extract_message_id(&raw("abc@x")).as_deref(), Some("abc@x"));
        assert_eq!(extract_message_id(b"message-id:\r\n <fold@x>\r\n\r\nbody").as_deref(), Some("fold@x"));
        assert_eq!(extract_message_id(b"Subject: x\r\n\r\nMessage-ID: <in-body@x>"), None);
        assert_eq!(extract_message_id(b"Subject: x\r\n\r\n"), None);
    }

    #[tokio::test]
    async fn first_mirror_appends_with_draft_flag_and_returns_the_uid() {
        let mut f = Fake::new(true);
        let r = mirror_with(&mut f, "Drafts", &raw("one@x"), None).await.unwrap().unwrap();
        assert_eq!((r.folder.as_str(), r.uid, r.validity, r.message_id.as_str()), ("Drafts", 1, 5, "one@x"));
        assert_eq!(f.boxes["Drafts"][0].flags, "(\\Seen \\Draft)");
    }

    #[tokio::test]
    async fn replacing_deletes_only_the_exact_previous_uid() {
        let mut f = Fake::new(true);
        // Someone else's draft, and another message that is flagged \Deleted but not expunged.
        let other = f.add("Drafts", "other@x", false);
        let flagged = f.add("Drafts", "flagged@x", true);
        let first = mirror_with(&mut f, "Drafts", &raw("one@x"), None).await.unwrap().unwrap();
        let second = mirror_with(&mut f, "Drafts", &raw("two@x"), Some(&first)).await.unwrap().unwrap();
        assert_eq!(f.uids("Drafts"), vec![other, flagged, second.uid], "previous copy gone, bystanders untouched");
        assert!(f.log.iter().any(|l| l == &format!("UID EXPUNGE {}", first.uid)));
        assert!(f.log.iter().all(|l| !l.starts_with("EXPUNGE")), "never a plain EXPUNGE");
        // The new copy is appended before the old one is removed.
        let a = f.log.iter().position(|l| l == "APPEND Drafts").unwrap();
        let e = f.log.iter().position(|l| l.starts_with("UID EXPUNGE")).unwrap();
        assert!(a < e);
    }

    #[tokio::test]
    async fn without_uidplus_nothing_happens() {
        let mut f = Fake::new(false);
        assert_eq!(mirror_with(&mut f, "Drafts", &raw("one@x"), None).await.unwrap(), None);
        assert!(f.log.is_empty(), "no command was sent: {:?}", f.log);
        let stale = DraftRef { folder: "Drafts".into(), validity: 5, uid: 1, message_id: "one@x".into() };
        assert!(!remove_with(&mut f, &stale).await.unwrap());
        assert!(f.log.is_empty());
    }

    #[tokio::test]
    async fn append_failure_keeps_the_previous_copy() {
        let mut f = Fake::new(true);
        let first = mirror_with(&mut f, "Drafts", &raw("one@x"), None).await.unwrap().unwrap();
        f.fail_append = true;
        assert!(mirror_with(&mut f, "Drafts", &raw("two@x"), Some(&first)).await.is_err());
        assert_eq!(f.uids("Drafts"), vec![first.uid]);
    }

    #[tokio::test]
    async fn unlocatable_append_is_an_error_and_deletes_nothing() {
        let mut f = Fake::new(true);
        let first = mirror_with(&mut f, "Drafts", &raw("one@x"), None).await.unwrap().unwrap();
        f.hide_appended = true;
        assert!(mirror_with(&mut f, "Drafts", &raw("two@x"), Some(&first)).await.is_err());
        assert!(f.boxes["Drafts"].iter().any(|m| m.uid == first.uid));
    }

    #[tokio::test]
    async fn lagging_search_index_falls_back_to_uidnext_scan() {
        let mut f = Fake::new(true);
        let other = f.add("Drafts", "other@x", false);
        f.lagging_search = true;
        let r = mirror_with(&mut f, "Drafts", &raw("one@x"), None).await.unwrap().unwrap();
        assert!(r.uid > other);
        let r2 = mirror_with(&mut f, "Drafts", &raw("two@x"), Some(&r)).await.unwrap().unwrap();
        assert_eq!(f.uids("Drafts"), vec![other, r2.uid], "no untracked duplicate");
    }

    #[tokio::test]
    async fn changed_uidvalidity_or_foreign_message_is_never_deleted() {
        let mut f = Fake::new(true);
        let first = mirror_with(&mut f, "Drafts", &raw("one@x"), None).await.unwrap().unwrap();
        // UIDVALIDITY changed: the stored UID may now be somebody else's message.
        f.validity = 6;
        assert!(!remove_with(&mut f, &first).await.unwrap());
        assert_eq!(f.uids("Drafts"), vec![first.uid]);
        // Same validity, but the UID now holds a different message (Message-ID differs).
        f.validity = 5;
        f.boxes.get_mut("Drafts").unwrap()[0].id = Some("someone-else@x".into());
        assert!(!remove_with(&mut f, &first).await.unwrap());
        assert_eq!(f.uids("Drafts"), vec![first.uid]);
        // After a validity change a replace still appends, but leaves the unprovable old copy alone.
        f.validity = 7;
        let stale = DraftRef { validity: 5, ..first.clone() };
        let n = mirror_with(&mut f, "Drafts", &raw("three@x"), Some(&stale)).await.unwrap().unwrap();
        assert_eq!(n.validity, 7);
        assert_eq!(f.uids("Drafts").len(), 2);
    }

    #[tokio::test]
    async fn remove_deletes_the_verified_copy_only() {
        let mut f = Fake::new(true);
        let keep = f.add("Drafts", "keep@x", false);
        let r = mirror_with(&mut f, "Drafts", &raw("one@x"), None).await.unwrap().unwrap();
        assert!(remove_with(&mut f, &r).await.unwrap());
        assert_eq!(f.uids("Drafts"), vec![keep]);
    }

    #[test]
    fn build_draft_allows_missing_and_half_typed_recipients() {
        use crate::types::OutgoingMessage;
        let m = OutgoingMessage {
            to: vec!["bo".into(), "bob@example.net".into()],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
            text: "hello".into(),
            html: None,
            attachments: vec![],
            in_reply_to: None,
            calendar: None,
            extra_headers: vec![],
        };
        let raw = crate::smtp::build_draft("me@example.org", &m).unwrap().formatted();
        let s = String::from_utf8_lossy(&raw).to_string();
        assert!(s.contains("bob@example.net") && !s.contains("To: bo\r\n"), "{s}");
        assert!(extract_message_id(&raw).is_some());
        let none = OutgoingMessage { to: vec![], ..m };
        let r = crate::smtp::build_draft("me@example.org", &none);
        assert!(r.is_ok(), "{:?}", r.err());
        assert!(crate::smtp::build_message("me@example.org", &none).is_err(), "a real send still needs a recipient");
    }
}
