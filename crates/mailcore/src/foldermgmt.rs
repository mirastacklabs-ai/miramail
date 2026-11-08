//! Folder management: create / rename / delete / subscribe / empty, plus the pure guards around them.
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

/// Full name of a new folder named `leaf` under `parent` (a folder name, or `None` for the top level).
pub fn child_name(folders: &[Folder], parent: Option<&str>, leaf: &str) -> std::result::Result<String, String> {
    let leaf = validate_new_folder_name(leaf)?;
    let name = match parent {
        None => leaf,
        Some(p) => {
            let pf = find(folders, p).ok_or("The parent folder no longer exists.")?;
            format!("{}{}{}", pf.name, delim_of(pf), leaf)
        }
    };
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
