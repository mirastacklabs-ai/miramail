//! Folder management: create / rename / move / delete / subscribe / empty, plus the pure guards around them.
//!
//! Safety properties:
//! * special folders (INBOX and every folder with a role) can be neither renamed nor deleted;
//! * a folder with subfolders is never deleted (the server would delete the children too);
//! * a non-empty folder is only deleted when the caller passes the exact message count it showed the
//!   user (`confirm_count`), re-checked against the server's own count at that moment;
//! * emptying is limited to Trash and Junk, enumerates the UIDs first and removes exactly those
//!   (`UID EXPUNGE`); without UIDPLUS it flags exactly those UIDs `\Deleted` and expunges, which is the
//!   same set because the whole folder is being emptied.
//!   Permanent deletion happens ONLY here, behind an explicit user confirm in the UI.
//! * new names use the same character rules as `batch::validate_new_folder_name`.

use crate::batch::{validate_new_folder_name, BATCH_CHUNK};
use crate::imap::{uid_ranges, ImapClient};
use crate::types::{Folder, FolderRole};
use crate::{Error, Result};
use futures::TryStreamExt;
use std::time::Duration;
use tokio::time::timeout;

const CMD_TIMEOUT: Duration = Duration::from_secs(30);

fn delim_of(f: &Folder) -> String {
    f.delimiter.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| "/".into())
}

fn find<'a>(folders: &'a [Folder], name: &str) -> Option<&'a Folder> {
    folders.iter().find(|f| f.name == name)
}

pub fn is_protected(f: &Folder) -> bool {
    f.role != FolderRole::Other || f.name.eq_ignore_ascii_case("INBOX")
}

pub fn has_children(folders: &[Folder], f: &Folder) -> bool {
    let d = delim_of(f);
    let prefix = format!("{}{}", f.name, d);
    folders.iter().any(|o| o.name.starts_with(&prefix))
}

/// A new or renamed leaf must not read as a special folder (Sent, Drafts, Trash, Junk/Spam, Archive, Inbox):
/// the app and the server would then treat the folder as that role.
fn reject_role_name(leaf: &str) -> std::result::Result<(), String> {
    if crate::imap::classify_folder(leaf, None, &[]) != FolderRole::Other {
        return Err(format!("“{leaf}” is a name reserved for a special folder. Choose another name."));
    }
    Ok(())
}

/// Full name of a new folder named `leaf` under `parent` (a folder name, or `None` for the top level),
/// WITHOUT the collision check (so a caller can reuse an existing folder of that name).
pub fn child_full_name(folders: &[Folder], parent: Option<&str>, leaf: &str) -> std::result::Result<String, String> {
    let leaf = validate_new_folder_name(leaf)?;
    reject_role_name(&leaf)?;
    Ok(match parent {
        None => leaf,
        Some(p) => {
            let pf = find(folders, p).ok_or("The parent folder no longer exists.")?;
            check_parent_role(pf)?;
            let d = pf.delimiter.clone().filter(|d| !d.is_empty()).ok_or("This server does not support folders inside folders.")?;
            format!("{}{}{}", pf.name, d, leaf)
        }
    })
}

/// Special folders (Trash, Junk, Sent, Drafts) can't hold new subfolders made on the assistant's or the user's behalf.
/// INBOX stays allowed: servers that keep every folder under `INBOX.` need it.
fn check_parent_role(pf: &Folder) -> std::result::Result<(), String> {
    match pf.role {
        FolderRole::Trash | FolderRole::Junk | FolderRole::Sent | FolderRole::Drafts => Err("A new folder can't be created inside a special folder such as Trash, Junk, Sent or Drafts.".into()),
        _ => Ok(()),
    }
}

/// `Ok` when `parent` exists and may hold a new subfolder (see [`child_full_name`]).
pub fn parent_allows_children(folders: &[Folder], parent: &str) -> std::result::Result<(), String> {
    let pf = find(folders, parent).ok_or("The parent folder no longer exists.")?;
    check_parent_role(pf)?;
    if pf.delimiter.as_deref().is_none_or(str::is_empty) {
        return Err("This server does not support folders inside folders.".into());
    }
    Ok(())
}

/// Full name of a new folder named `leaf` under `parent` (a folder name, or `None` for the top level).
pub fn child_name(folders: &[Folder], parent: Option<&str>, leaf: &str) -> std::result::Result<String, String> {
    let name = child_full_name(folders, parent, leaf)?;
    if folders.iter().any(|f| f.name.eq_ignore_ascii_case(&name)) {
        return Err("A folder with that name already exists.".into());
    }
    Ok(name)
}

/// New full name when renaming `old` to `new_leaf` (same parent).
pub fn renamed(folders: &[Folder], old: &str, new_leaf: &str) -> std::result::Result<String, String> {
    let f = find(folders, old).ok_or("That folder no longer exists.")?;
    if is_protected(f) {
        return Err("This is a special folder and can't be renamed.".into());
    }
    let leaf = validate_new_folder_name(new_leaf)?;
    reject_role_name(&leaf)?;
    let d = delim_of(f);
    let name = match old.rsplit_once(d.as_str()) {
        Some((parent, _)) => format!("{parent}{d}{leaf}"),
        None => leaf,
    };
    if name == old {
        return Err("That is already the folder's name.".into());
    }
    if folders.iter().any(|x| x.name.eq_ignore_ascii_case(&name)) {
        return Err("A folder with that name already exists.".into());
    }
    Ok(name)
}

/// Personal-namespace prefix of servers that keep every folder under INBOX (e.g. Dovecot `INBOX.` / Courier):
/// `Some("INBOX.")` when INBOX has a delimiter and every other folder starts with `INBOX<delim>`. On those
/// servers a "top level" folder has to carry the prefix or the server refuses the name.
pub fn personal_prefix(folders: &[Folder]) -> Option<String> {
    let inbox = folders.iter().find(|f| f.name.eq_ignore_ascii_case("INBOX"))?;
    let d = inbox.delimiter.clone().filter(|d| !d.is_empty())?;
    let prefix = format!("{}{}", inbox.name, d);
    let others: Vec<_> = folders.iter().filter(|f| !f.name.eq_ignore_ascii_case("INBOX")).collect();
    (!others.is_empty() && others.iter().all(|f| f.name.starts_with(&prefix))).then_some(prefix)
}

/// New full name when moving `folder` (with its subfolders) under `new_parent` (`None` = top level of the
/// personal namespace). Keeps the leaf name and uses the delimiter of the destination.
pub fn moved(folders: &[Folder], folder: &str, new_parent: Option<&str>) -> std::result::Result<String, String> {
    let f = find(folders, folder).ok_or("That folder no longer exists.")?;
    if is_protected(f) {
        return Err("This is a special folder and can't be moved.".into());
    }
    let d = f.delimiter.clone().filter(|d| !d.is_empty()).ok_or("This server does not support folder hierarchy, so folders can't be moved.")?;
    let leaf = folder.rsplit_once(d.as_str()).map_or(folder, |(_, l)| l);
    let sub = format!("{folder}{d}");
    if folders.iter().any(|o| o.name.starts_with(&sub) && is_protected(o)) {
        return Err("This folder contains a special folder and can't be moved.".into());
    }
    let name = match new_parent {
        Some(p) => {
            let pf = find(folders, p).ok_or("The destination folder no longer exists.")?;
            if p == folder || p.starts_with(&sub) {
                return Err("A folder can't be moved into itself or one of its subfolders.".into());
            }
            let pd = pf.delimiter.clone().filter(|d| !d.is_empty()).ok_or("The destination does not support subfolders.")?;
            format!("{p}{pd}{leaf}")
        }
        None => match personal_prefix(folders) {
            Some(prefix) => format!("{prefix}{leaf}"),
            None => leaf.to_string(),
        },
    };
    if name == folder {
        return Err("The folder is already there.".into());
    }
    // The destination delimiter decides what counts as "below the new name". The folder itself and its own
    // subfolders are about to move, so they never collide (a case-only change is a legal move).
    let pd = new_parent.and_then(|p| find(folders, p)).map_or_else(|| d.clone(), delim_of);
    let sub_new = format!("{name}{pd}").to_lowercase();
    if folders.iter().filter(|x| x.name != folder && !x.name.starts_with(&sub)).any(|x| x.name.eq_ignore_ascii_case(&name) || x.name.to_lowercase().starts_with(&sub_new)) {
        return Err("A folder with that name already exists at the destination.".into());
    }
    Ok(name)
}

/// May `name` be deleted now? `count` = what the server says it holds, `confirm_count` = what the user confirmed.
pub fn check_deletable(folders: &[Folder], name: &str, count: u32, confirm_count: Option<u32>) -> std::result::Result<(), String> {
    let f = find(folders, name).ok_or("That folder no longer exists.")?;
    if is_protected(f) {
        return Err("This is a special folder and can't be deleted.".into());
    }
    if has_children(folders, f) {
        return Err("This folder has subfolders. Delete or move them first.".into());
    }
    if count > 0 && confirm_count != Some(count) {
        return Err(format!(
            "This folder contains {count} message{}. Deleting it would delete them permanently; confirm the exact count to proceed.",
            if count == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

/// Only Trash and Junk may be emptied.
pub fn check_emptiable(folders: &[Folder], name: &str) -> std::result::Result<(), String> {
    match find(folders, name) {
        Some(f) if matches!(f.role, FolderRole::Trash | FolderRole::Junk) => Ok(()),
        Some(_) => Err("Only Trash and Junk can be emptied.".into()),
        None => Err("That folder no longer exists.".into()),
    }
}

impl ImapClient {
    pub async fn create_folder_named(&mut self, full_name: &str) -> Result<()> {
        timeout(CMD_TIMEOUT, self.session_mut().create(full_name)).await.map_err(|_| Error::Timeout("create"))??;
        // Best effort: show the new folder in other clients too.
        let _ = timeout(CMD_TIMEOUT, self.session_mut().subscribe(full_name)).await;
        Ok(())
    }

    pub async fn rename_folder(&mut self, from: &str, to: &str) -> Result<()> {
        timeout(CMD_TIMEOUT, self.session_mut().rename(from, to)).await.map_err(|_| Error::Timeout("rename"))??;
        Ok(())
    }

    /// DELETE the mailbox itself. Callers must have run [`check_deletable`] first.
    pub async fn delete_folder_named(&mut self, name: &str) -> Result<()> {
        let _ = timeout(CMD_TIMEOUT, self.session_mut().unsubscribe(name)).await;
        timeout(CMD_TIMEOUT, self.session_mut().delete(name)).await.map_err(|_| Error::Timeout("delete"))??;
        Ok(())
    }

    pub async fn set_subscribed(&mut self, name: &str, subscribed: bool) -> Result<()> {
        let s = self.session_mut();
        if subscribed {
            timeout(CMD_TIMEOUT, s.subscribe(name)).await.map_err(|_| Error::Timeout("subscribe"))??;
        } else {
            timeout(CMD_TIMEOUT, s.unsubscribe(name)).await.map_err(|_| Error::Timeout("unsubscribe"))??;
        }
        Ok(())
    }

    /// Names the user is subscribed to (LSUB).
    pub async fn subscribed_names(&mut self) -> Result<Vec<String>> {
        let names: Vec<_> = timeout(CMD_TIMEOUT, async { self.session_mut().lsub(Some(""), Some("*")).await?.try_collect().await })
            .await
            .map_err(|_| Error::Timeout("lsub"))??;
        Ok(names.iter().map(|n| n.name().to_owned()).collect())
    }

    /// Permanently remove every message of `folder`. Callers must have run [`check_emptiable`] and obtained
    /// the user's confirmation. Never loses track of progress: `removed` lists exactly the UIDs the server
    /// confirmed expunged, and `error` is set when it stopped early (then the caller must drop only those UIDs
    /// from the cache).
    pub async fn empty_folder(&mut self, folder: &str) -> EmptyReport {
        let mut rep = EmptyReport::default();
        if let Err(e) = self.empty_folder_inner(folder, &mut rep).await {
            rep.error = Some(e);
        }
        rep
    }

    async fn empty_folder_inner(&mut self, folder: &str, rep: &mut EmptyReport) -> Result<()> {
        let plus = self.has_uidplus();
        self.select(folder).await?;
        let found = timeout(CMD_TIMEOUT, self.session_mut().uid_search("ALL")).await.map_err(|_| Error::Timeout("search"))??;
        let mut uids: Vec<u32> = found.into_iter().collect();
        uids.sort_unstable();
        rep.total = uids.len() as u32;
        for chunk in uids.chunks(BATCH_CHUNK) {
            let set = uid_ranges(chunk);
            let _: Vec<async_imap::types::Fetch> = timeout(CMD_TIMEOUT * 2, async { self.session_mut().uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await?.try_collect().await })
                .await
                .map_err(|_| Error::Timeout("store"))??;
            if plus {
                let _: Vec<u32> = timeout(CMD_TIMEOUT * 2, async { self.session_mut().uid_expunge(&set).await?.try_collect().await })
                    .await
                    .map_err(|_| Error::Timeout("expunge"))??;
                rep.removed.extend_from_slice(chunk);
            }
        }
        if !plus && !uids.is_empty() {
            // Everything in the folder carries \Deleted now (it is being emptied), so a plain EXPUNGE removes the same set.
            let _: Vec<u32> = timeout(CMD_TIMEOUT * 2, async { self.session_mut().expunge().await?.try_collect().await })
                .await
                .map_err(|_| Error::Timeout("expunge"))??;
            rep.removed = uids;
        }
        Ok(())
    }
}

/// Outcome of [`ImapClient::empty_folder`].
#[derive(Debug, Default)]
pub struct EmptyReport {
    /// Messages the folder held when the run started.
    pub total: u32,
    /// UIDs confirmed expunged on the server.
    pub removed: Vec<u32>,
    pub error: Option<Error>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, role: FolderRole, d: &str) -> Folder {
        Folder { name: name.into(), delimiter: Some(d.into()), role, selectable: true }
    }
    fn tree() -> Vec<Folder> {
        vec![
            f("INBOX", FolderRole::Inbox, "/"),
            f("Trash", FolderRole::Trash, "/"),
            f("Junk", FolderRole::Junk, "/"),
            f("Projects", FolderRole::Other, "/"),
            f("Projects/Alpha", FolderRole::Other, "/"),
            f("Misc", FolderRole::Other, "/"),
        ]
    }

    #[test]
    fn parent_roles_and_missing_delimiter() {
        let t = tree();
        for p in ["Trash", "Junk"] {
            assert!(child_name(&t, Some(p), "X").unwrap_err().contains("special folder"), "{p}");
            assert!(parent_allows_children(&t, p).is_err());
        }
        assert_eq!(child_name(&t, Some("INBOX"), "X").unwrap(), "INBOX/X", "INBOX may hold folders");
        assert!(parent_allows_children(&t, "Projects").is_ok());
        let flat = vec![Folder { name: "INBOX".into(), delimiter: None, role: FolderRole::Inbox, selectable: true }, Folder { name: "Misc".into(), delimiter: None, role: FolderRole::Other, selectable: true }];
        assert_eq!(child_name(&flat, Some("Misc"), "X").unwrap_err(), "This server does not support folders inside folders.");
        assert!(parent_allows_children(&flat, "Misc").is_err());
        assert_eq!(child_name(&flat, None, "Top").unwrap(), "Top", "top level still works");
    }

    #[test]
    fn child_and_rename_names() {
        let t = tree();
        assert_eq!(child_name(&t, None, "Taxes").unwrap(), "Taxes");
        assert_eq!(child_name(&t, Some("Projects"), "Beta").unwrap(), "Projects/Beta");
        assert!(child_name(&t, Some("Projects"), "Alpha").is_err(), "exists");
        assert!(child_name(&t, None, "projects").is_err(), "case-insensitive duplicate");
        assert!(child_name(&t, Some("Gone"), "X").is_err());
        assert!(child_name(&t, None, "a/b").is_err());
        assert!(child_name(&t, None, "..").is_err());
        assert!(child_name(&t, None, "INBOX").is_err());
        assert_eq!(renamed(&t, "Projects/Alpha", "Gamma").unwrap(), "Projects/Gamma");
        assert_eq!(renamed(&t, "Misc", "Other").unwrap(), "Other");
        assert!(renamed(&t, "Misc", "Projects").is_err());
        assert!(renamed(&t, "Misc", "Misc").is_err());
        assert!(renamed(&t, "INBOX", "X").is_err());
        assert!(renamed(&t, "Trash", "X").is_err());
        assert!(renamed(&t, "Nope", "X").is_err());
        let dotted = vec![f("INBOX", FolderRole::Inbox, "."), f("INBOX.Work", FolderRole::Other, ".")];
        assert_eq!(renamed(&dotted, "INBOX.Work", "Jobs").unwrap(), "INBOX.Jobs");
        assert_eq!(child_name(&dotted, Some("INBOX"), "New").unwrap(), "INBOX.New");
    }

    #[test]
    fn role_names_are_rejected_for_new_and_renamed_leaves() {
        let t = tree();
        for n in ["Sent", "sent items", "Drafts", "TRASH", "Junk", "Spam", "Archive", "Archives", "Bin", "All Mail"] {
            assert!(child_name(&t, None, n).unwrap_err().contains("special folder"), "create {n}");
            assert!(child_name(&t, Some("Projects"), n).is_err(), "create child {n}");
            assert!(renamed(&t, "Misc", n).is_err(), "rename {n}");
        }
        assert!(child_name(&t, None, "Archive2").is_ok());
        assert!(renamed(&t, "Misc", "Sentinel").is_ok());
    }

    #[test]
    fn case_only_move_and_destination_delimiter() {
        // A folder never collides with itself: moving "a/Misc" under "A" keeps its name but changes the parent.
        let t = vec![f("INBOX", FolderRole::Inbox, "/"), f("a", FolderRole::Other, "/"), f("A", FolderRole::Other, "/"), f("a/Misc", FolderRole::Other, "/")];
        assert_eq!(moved(&t, "a/Misc", Some("A")).unwrap(), "A/Misc");
        // sub-prefix check uses the destination delimiter: "B.X.y" sits below the new name "B.X".
        let m = vec![f("INBOX", FolderRole::Inbox, "."), f("B", FolderRole::Other, "."), f("B.X.y", FolderRole::Other, "."), f("X", FolderRole::Other, ".")];
        assert!(moved(&m, "X", Some("B")).unwrap_err().contains("exists"));
    }

    #[test]
    fn move_names_and_refusals() {
        let mut t = tree();
        t.push(f("Projects/Alpha/Deep", FolderRole::Other, "/"));
        assert_eq!(moved(&t, "Misc", Some("Projects")).unwrap(), "Projects/Misc");
        assert_eq!(moved(&t, "Projects/Alpha", None).unwrap(), "Alpha");
        assert_eq!(moved(&t, "Projects/Alpha", Some("Misc")).unwrap(), "Misc/Alpha");
        assert_eq!(moved(&t, "Projects/Alpha/Deep", Some("Misc")).unwrap(), "Misc/Deep");
        assert!(moved(&t, "Misc", Some("Misc")).is_err(), "into itself");
        assert!(moved(&t, "Projects", Some("Projects/Alpha")).is_err(), "into child");
        assert!(moved(&t, "Projects", Some("Projects/Alpha/Deep")).is_err(), "into grandchild");
        assert!(moved(&t, "Misc", None).unwrap_err().contains("already"), "top level no-op");
        assert!(moved(&t, "Projects/Alpha", Some("Projects")).unwrap_err().contains("already"), "same parent");
        assert!(moved(&t, "INBOX", Some("Misc")).is_err());
        assert!(moved(&t, "Trash", Some("Misc")).is_err());
        assert!(moved(&t, "Nope", Some("Misc")).is_err());
        assert!(moved(&t, "Misc", Some("Nope")).is_err());
        let mut sib = tree();
        sib.push(f("Projects2", FolderRole::Other, "/"));
        assert_eq!(moved(&sib, "Projects2", Some("Projects")).unwrap(), "Projects/Projects2", "sibling with a shared prefix is not a descendant");
        assert_eq!(moved(&sib, "Projects", Some("Projects2")).unwrap(), "Projects2/Projects");
        let mut c = tree();
        c.push(f("Misc/Alpha", FolderRole::Other, "/"));
        assert!(moved(&c, "Projects/Alpha", Some("Misc")).unwrap_err().contains("exists"));
        c.push(f("alpha", FolderRole::Other, "/"));
        assert!(moved(&c, "Projects/Alpha", None).unwrap_err().contains("exists"), "case-insensitive");
        let mut r = tree();
        r.push(f("Misc/Sent", FolderRole::Sent, "/"));
        assert!(moved(&r, "Misc", Some("Projects")).is_err(), "holds a special folder");
        let nd = vec![f("INBOX", FolderRole::Inbox, "/"), Folder { name: "A".into(), delimiter: None, role: FolderRole::Other, selectable: true }, f("B", FolderRole::Other, "/")];
        assert!(moved(&nd, "A", Some("B")).unwrap_err().contains("hierarchy"));
    }

    #[test]
    fn move_respects_inbox_prefix_namespace_and_delimiter() {
        let d = vec![
            f("INBOX", FolderRole::Inbox, "."),
            f("INBOX.Sent", FolderRole::Sent, "."),
            f("INBOX.Work", FolderRole::Other, "."),
            f("INBOX.Work.Q1", FolderRole::Other, "."),
            f("INBOX.Misc", FolderRole::Other, "."),
        ];
        assert_eq!(personal_prefix(&d).as_deref(), Some("INBOX."));
        assert_eq!(moved(&d, "INBOX.Work.Q1", None).unwrap(), "INBOX.Q1", "top level keeps the prefix");
        assert_eq!(moved(&d, "INBOX.Misc", Some("INBOX.Work")).unwrap(), "INBOX.Work.Misc");
        assert!(moved(&d, "INBOX.Misc", None).is_err(), "already top level");
        assert!(personal_prefix(&tree()).is_none());
    }

    #[test]
    fn delete_guards() {
        let t = tree();
        assert!(check_deletable(&t, "Misc", 0, None).is_ok());
        let e = check_deletable(&t, "Misc", 3, None).unwrap_err();
        assert!(e.contains("3 messages"), "{e}");
        assert!(check_deletable(&t, "Misc", 3, Some(2)).is_err(), "stale confirm count");
        assert!(check_deletable(&t, "Misc", 3, Some(4)).is_err());
        assert!(check_deletable(&t, "Misc", 3, Some(3)).is_ok());
        assert!(check_deletable(&t, "Misc", 1, None).unwrap_err().contains("1 message."));
        assert!(check_deletable(&t, "Projects", 0, None).unwrap_err().contains("subfolders"));
        for special in ["INBOX", "Trash", "Junk"] {
            assert!(check_deletable(&t, special, 0, Some(0)).is_err(), "{special}");
        }
        assert!(check_deletable(&t, "Nope", 0, None).is_err());
        assert!(check_deletable(&[f("Inbox", FolderRole::Other, "/")], "Inbox", 0, None).is_err(), "INBOX by name even without role");
    }

    #[test]
    fn only_trash_and_junk_can_be_emptied() {
        let t = tree();
        assert!(check_emptiable(&t, "Trash").is_ok());
        assert!(check_emptiable(&t, "Junk").is_ok());
        for n in ["INBOX", "Misc", "Projects"] {
            assert!(check_emptiable(&t, n).is_err(), "{n}");
        }
        assert!(check_emptiable(&t, "Nope").is_err());
    }
}
