//! Organising + reader actions (docs/ESSENTIALS_SPEC.md): whole-mailbox search, folder management, bulk
//! operations, one-click unsubscribe, view source / save .eml / open attachment, colour labels.
//!
//! Nothing here deletes mail permanently except `folder_empty` (Trash/Junk only), `folder_delete`
//! (guarded, see `mailcore::foldermgmt`) and `bulk_delete_forever`, each of which the UI reaches only
//! through an explicit confirm. All bulk moves/flags go through the set-safe `mailcore::batch` functions
//! (explicit UID sets, no plain EXPUNGE).

use crate::files::save_unique;
use crate::state::{err, AppState};
use crate::wire::*;
use mailcore::search::{parse_query, IsValue, Term, TextField};
use mailcore::unsub::{parse_list_unsubscribe, UnsubOptions};
use mailcore::{foldermgmt, imap::find_role, BatchItem, FlagChange, Folder, FolderRole, ScopeKey, SearchParams, ServerQuery, Storage};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

type CmdResult<T> = Result<T, String>;

/// Most UIDs one bulk command accepts (the UI sends chunks of 200 and reports progress between them).
pub const BULK_MAX: usize = 1000;
/// Most folders one server-side search visits.
pub const SERVER_SEARCH_FOLDERS: usize = 12;

// ------------------------------------------------------------------------------------------ search

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSearch {
    pub hits: Vec<WireSummary>,
    pub total: u32,
    pub hints: Vec<String>,
}

/// Folders a search scope covers. `folder`: the one folder; `account`: every selectable folder of the
/// account; `all`: every selectable folder of every account.
pub fn scopes_for(storage: &Storage, scope: &str, account: Option<&str>, folder: Option<&str>) -> CmdResult<Vec<ScopeKey>> {
    let selectable = |acc: &str| -> CmdResult<Vec<ScopeKey>> {
        Ok(storage.list_folders(acc).map_err(err)?.into_iter().filter(|f| f.selectable).map(|f| ScopeKey::new(acc, f.name)).collect())
    };
    match scope {
        "folder" => match (account, folder) {
            (Some(a), Some(f)) => Ok(vec![ScopeKey::new(a, f)]),
            _ => Err("Pick a folder to search in.".into()),
        },
        "account" => selectable(account.ok_or("Pick an account to search in.")?),
        "all" => {
            let mut v = Vec::new();
            for a in storage.list_accounts().map_err(err)? {
                v.extend(selectable(&a.email)?);
            }
            Ok(v)
        }
        _ => Err("Unknown search scope.".into()),
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // flat wire contract
pub async fn search_mail(
    state: State<'_, AppState>,
    query: String,
    scope: String,
    account: Option<String>,
    folder: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
    tz_offset: Option<i64>,
) -> CmdResult<WireSearch> {
    let scopes = scopes_for(&state.storage, &scope, account.as_deref(), folder.as_deref())?;
    let r = state
        .storage
        .search_messages(&SearchParams {
            query,
            scopes,
            limit: limit.unwrap_or(50),
            offset: offset.unwrap_or(0),
            now: chrono::Utc::now().timestamp(),
            tz_offset: tz_offset.unwrap_or(0).clamp(-50_400, 50_400),
        })
        .map_err(err)?;
    Ok(WireSearch { hits: r.hits.iter().map(|h| WireSummary::from(&h.summary)).collect(), total: r.total, hints: r.hints })
}

/// Translate what the server can search for. Returns the query, the folder filter (`folder:` terms) and
/// notes for everything that can only be applied locally (the local search re-runs afterwards).
pub fn server_query_from(query: &str, now: i64, tz: i64) -> CmdResult<(ServerQuery, Option<String>, Vec<String>)> {
    let parsed = parse_query(query, now, tz);
    let mut q = ServerQuery::default();
    let mut folder = None;
    let mut notes = Vec::new();
    let day = |ts: i64| chrono::DateTime::from_timestamp(ts + tz, 0).map(|d| d.format("%Y-%m-%d").to_string());
    for t in &parsed.terms {
        match t {
            Term::Text { field, value, neg: false } => {
                let slot = match field {
                    TextField::From => &mut q.from,
                    TextField::To => &mut q.to,
                    TextField::Subject => &mut q.subject,
                    TextField::Any => &mut q.text,
                };
                if slot.is_none() {
                    *slot = Some(value.clone());
                } else {
                    notes.push(format!("The server can't combine two \"{value}\"-style terms; the extra one is applied to cached messages only."));
                }
            }
            Term::Is { value: IsValue::Unread, neg } => q.unseen = Some(!neg),
            Term::Is { value: IsValue::Read, neg } => q.unseen = Some(*neg),
            Term::Is { value: IsValue::Flagged, neg } => q.flagged = Some(!neg),
            Term::Is { value: IsValue::Unflagged, neg } => q.flagged = Some(*neg),
            Term::Date { before, ts, neg: false } => {
                if let Some(d) = day(*ts) {
                    if *before { q.before.get_or_insert(d); } else { q.since.get_or_insert(d); }
                }
            }
            Term::Folder { value, neg: false } => folder = Some(value.clone()),
            _ => notes.push("Part of the query (exclusions, answered, cc, attachments) is applied to cached messages only.".into()),
        }
    }
    notes.dedup();
    if q == ServerQuery::default() {
        return Err("Nothing in this query can be searched on the server (try words, from:, to:, subject:, dates, is:unread or is:flagged).".into());
    }
    Ok((q, folder, notes))
}

/// Does `folder:value` name this folder? (name, last path part, or role; case-insensitive)
pub fn folder_matches(f: &Folder, value: &str) -> bool {
    let n = f.name.to_lowercase();
    n == value || f.role.as_str() == value || n.ends_with(&format!("/{value}")) || n.ends_with(&format!(".{value}"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireServerSearch {
    /// Messages the server reported (now cached, so a local search finds them).
    pub found: u32,
    pub searched: u32,
    pub skipped: u32,
    pub notes: Vec<String>,
}

#[tauri::command]
pub async fn search_server(
    state: State<'_, AppState>,
    query: String,
    scope: String,
    account: Option<String>,
    folder: Option<String>,
    tz_offset: Option<i64>,
) -> CmdResult<WireServerSearch> {
    let tz = tz_offset.unwrap_or(0).clamp(-50_400, 50_400);
    let (q, folder_filter, mut notes) = server_query_from(&query, chrono::Utc::now().timestamp(), tz)?;
    let scopes = scopes_for(&state.storage, &scope, account.as_deref(), folder.as_deref())?;
    let mut by_account: Vec<(String, Vec<String>)> = Vec::new();
    let mut n_folders = 0;
    for s in scopes {
        if n_folders >= SERVER_SEARCH_FOLDERS {
            notes.push(format!("Only the first {SERVER_SEARCH_FOLDERS} folders were searched on the server."));
            break;
        }
        if let Some(v) = &folder_filter {
            let known = state.storage.list_folders(&s.account).map_err(err)?;
            if !known.iter().any(|f| f.name == s.folder && folder_matches(f, v)) {
                continue;
            }
        }
        n_folders += 1;
        match by_account.iter_mut().find(|(a, _)| *a == s.account) {
            Some((_, v)) => v.push(s.folder),
            None => by_account.push((s.account, vec![s.folder])),
        }
    }
    let (mut found, mut searched, mut skipped) = (0u32, 0u32, 0u32);
    for (acc, folders) in by_account {
        let mut conn = match state.connect(&acc).await {
            Ok(c) => c,
            Err(e) => {
                skipped += folders.len() as u32;
                notes.push(format!("{acc}: {e}"));
                continue;
            }
        };
        for f in folders {
            match conn.client.server_search(&state.storage, &acc, &f, None, &q, 100).await {
                Ok((_, uids)) => {
                    searched += 1;
                    found += uids.len() as u32;
                }
                Err(e) => {
                    skipped += 1;
                    let m = e.to_string();
                    if !m.contains("not been synced") {
                        notes.push(format!("{f}: {m}"));
                    }
                }
            }
        }
        conn.close().await;
    }
    notes.truncate(5);
    Ok(WireServerSearch { found, searched, skipped, notes })
}

// ------------------------------------------------------------------------------------------ folders

async fn refresh_folders(state: &AppState, account: &str, conn: &mut crate::state::Conn) -> CmdResult<Vec<WireFolder>> {
    let list = conn.client.list_folders().await.map_err(err)?;
    state.storage.replace_folders(account, &list).map_err(err)?;
    Ok(list.iter().map(WireFolder::from).collect())
}

#[tauri::command]
pub async fn folder_create(state: State<'_, AppState>, account: String, parent: Option<String>, name: String) -> CmdResult<Vec<WireFolder>> {
    let known = state.storage.list_folders(&account).map_err(err)?;
    let full = foldermgmt::child_name(&known, parent.as_deref(), &name)?;
    let mut conn = state.connect(&account).await?;
    let r = async {
        conn.client.create_folder_named(&full).await.map_err(err)?;
        refresh_folders(&state, &account, &mut conn).await
    }
    .await;
    conn.close().await;
    r
}

/// Result of a folder rename/move: the fresh folder list plus notes the UI shows (revoked AI access, a
/// refresh that has to wait for the next sync).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderOpResult {
    pub folders: Vec<WireFolder>,
    pub notes: Vec<String>,
}

#[tauri::command]
pub async fn folder_rename(
    app: AppHandle,
    state: State<'_, AppState>,
    snooze: State<'_, crate::snooze::SnoozeState>,
    ai: State<'_, crate::ai::AiState>,
    account: String,
    folder: String,
    new_name: String,
) -> CmdResult<FolderOpResult> {
    let from = folder.clone();
    rename_and_refresh(&app, &state, &snooze.store, &ai, &account, &from, |known| foldermgmt::renamed(known, &folder, &new_name)).await
}

/// Move `folder` (with its subfolders) under `new_parent`, or to the top level when `None`. Same IMAP RENAME,
/// watcher handling, config persist and LIST refresh as a rename.
#[tauri::command]
pub async fn folder_move(
    app: AppHandle,
    state: State<'_, AppState>,
    snooze: State<'_, crate::snooze::SnoozeState>,
    ai: State<'_, crate::ai::AiState>,
    account: String,
    folder: String,
    new_parent: Option<String>,
) -> CmdResult<FolderOpResult> {
    let from = folder.clone();
    rename_and_refresh(&app, &state, &snooze.store, &ai, &account, &from, |known| foldermgmt::moved(known, &folder, new_parent.as_deref())).await
}

/// Name of `old` (a folder at or below `from`) after `from` was renamed to `to`.
pub fn remapped(old: &str, from: &str, to: &str) -> String {
    if old == from {
        to.to_string()
    } else {
        format!("{to}{}", &old[from.len()..])
    }
}

/// The cached folder list as it will look once `from` (and everything below it) is called `to`.
pub fn remap_folders(known: &[Folder], from: &str, to: &str) -> Vec<Folder> {
    let affected = folder_and_children(known, from);
    known.iter().map(|f| if affected.contains(&f.name) { Folder { name: remapped(&f.name, from, to), ..f.clone() } } else { f.clone() }).collect()
}

/// Shared by rename and move. Order: refresh LIST (so subfolders created elsewhere are included), compute the
/// new name and the affected set, refuse while a snooze depends on one of them, RENAME, stop the old live
/// watchers, revoke AI grants for the old names (never migrated), refresh LIST again (one retry; a failure
/// after a successful RENAME is a note, not an error), restart live sync under the new names.
async fn rename_and_refresh(
    app: &AppHandle,
    state: &AppState,
    snooze: &crate::snooze::SnoozeStore,
    ai: &crate::ai::AiState,
    account: &str,
    folder: &str,
    name_for: impl FnOnce(&[Folder]) -> Result<String, String>,
) -> CmdResult<FolderOpResult> {
    let mut conn = state.connect(account).await?;
    let r = async {
        refresh_folders(state, account, &mut conn).await?;
        let known = state.storage.list_folders(account).map_err(err)?;
        let to = name_for(&known)?;
        let affected = folder_and_children(&known, folder);
        if let Some(used) = snooze.folder_in_use(account, &affected)? {
            return Err(format!("Wake or forget the snoozed messages in “{used}” first."));
        }
        let was_live: Vec<String> = state.live_folders(account).into_iter().filter(|f| affected.contains(f)).collect();
        conn.client.rename_folder(folder, &to).await.map_err(err)?;
        // From here the server has the new names; nothing below may report the operation as failed.
        let mut notes = Vec::new();
        // Live watchers hold the old names: stop them only now that the rename really happened.
        for f in &affected {
            state.stop_live(account, f);
            state.set_live_cfg(account, f, false);
        }
        // Fail closed: the old names lose every AI grant; nothing is copied to the new names.
        match ai.revoke_folder_grants(account, &affected) {
            Ok(0) => {}
            Ok(_) => notes.push(format!("MIRA's access to “{folder}” was removed because the folder changed place. Allow it again in the conversation if you want.")),
            Err(_) => notes.push("MIRA's permissions for this folder could not be updated. Check them in the conversation.".to_string()),
        }
        let list = match conn.client.list_folders().await {
            Ok(l) => Some(l),
            Err(_) => conn.client.list_folders().await.ok(),
        };
        let folders = match list {
            Some(l) => {
                state.storage.replace_folders(account, &l).map_err(err)?;
                l.iter().map(WireFolder::from).collect()
            }
            None => {
                notes.push("Renamed on the server; the folder list will update on the next sync.".to_string());
                let local = remap_folders(&known, folder, &to);
                let _ = state.storage.replace_folders(account, &local);
                local.iter().map(WireFolder::from).collect()
            }
        };
        // Live folders come back under their new names.
        for old in &was_live {
            let new = remapped(old, folder, &to);
            if let (Ok(cfg), Ok(pw)) = (state.account(account), state.password(account)) {
                let _ = crate::live::start_live(app, &cfg, pw, new.clone());
                state.set_live_cfg(account, &new, true);
            }
        }
        let _ = state.persist_config();
        Ok(FolderOpResult { folders, notes })
    }
    .await;
    conn.close().await;
    r
}

/// `folder` and every folder below it (a rename or delete of the parent moves them all).
pub fn folder_and_children(known: &[Folder], folder: &str) -> Vec<String> {
    let delim = known.iter().find(|f| f.name == folder).and_then(|f| f.delimiter.clone());
    let mut v = vec![folder.to_string()];
    if let Some(d) = delim.filter(|d| !d.is_empty()) {
        let prefix = format!("{folder}{d}");
        v.extend(known.iter().filter(|f| f.name.starts_with(&prefix)).map(|f| f.name.clone()));
    }
    v
}

/// Messages the server holds in a folder (STATUS; does not select it).
#[tauri::command]
pub async fn folder_message_count(state: State<'_, AppState>, account: String, folder: String) -> CmdResult<u32> {
    let mut conn = state.connect(&account).await?;
    let r = conn.client.folder_status_total(&folder).await;
    conn.close().await;
    r.map_err(err)
}

#[tauri::command]
pub async fn folder_delete(state: State<'_, AppState>, account: String, folder: String, confirm_count: Option<u32>) -> CmdResult<Vec<WireFolder>> {
    let mut conn = state.connect(&account).await?;
    let r = async {
        // Refresh LIST first so the has-subfolders check sees folders created elsewhere since the last sync.
        // (A subfolder or message added by another client between this STATUS and the DELETE below is an
        // inherent IMAP race: DELETE itself refuses a mailbox with inferior names on most servers, and the
        // confirmed count is what the user agreed to.)
        refresh_folders(&state, &account, &mut conn).await?;
        let known = state.storage.list_folders(&account).map_err(err)?;
        // Cheap structural checks first, then the server's own count decides whether the confirm is enough.
        foldermgmt::check_deletable(&known, &folder, 0, Some(0))?;
        let count = conn.client.folder_status_total(&folder).await.map_err(err)?;
        foldermgmt::check_deletable(&known, &folder, count, confirm_count)?;
        conn.client.delete_folder_named(&folder).await.map_err(err)?;
        refresh_folders(&state, &account, &mut conn).await
    }
    .await;
    conn.close().await;
    if r.is_ok() {
        state.stop_live(&account, &folder);
        state.set_live_cfg(&account, &folder, false);
        let _ = state.persist_config();
    }
    r
}

#[tauri::command]
pub async fn folder_subscribe(state: State<'_, AppState>, account: String, folder: String, subscribed: bool) -> CmdResult<()> {
    let known = state.storage.list_folders(&account).map_err(err)?;
    if !known.iter().any(|f| f.name == folder) {
        return Err("That folder no longer exists.".into());
    }
    let mut conn = state.connect(&account).await?;
    let r = conn.client.set_subscribed(&folder, subscribed).await;
    conn.close().await;
    r.map_err(err)
}

#[tauri::command]
pub async fn folder_subscriptions(state: State<'_, AppState>, account: String) -> CmdResult<Vec<String>> {
    let mut conn = state.connect(&account).await?;
    let r = conn.client.subscribed_names().await;
    conn.close().await;
    r.map_err(err)
}

/// Permanently remove every message in Trash or Junk. The UI asks first, naming the count.
#[tauri::command]
pub async fn folder_empty(state: State<'_, AppState>, account: String, folder: String) -> CmdResult<u32> {
    let known = state.storage.list_folders(&account).map_err(err)?;
    foldermgmt::check_emptiable(&known, &folder)?;
    let mut conn = state.connect(&account).await?;
    let rep = conn.client.empty_folder(&folder).await;
    conn.close().await;
    apply_empty_report(&state.storage, &account, &folder, rep)
}

/// Cache upkeep after emptying: only UIDs the server confirmed expunged leave the cache. On a partial
/// failure the error says how many went, and the rest stay visible (and get reconciled at the next sync).
pub fn apply_empty_report(storage: &Storage, account: &str, folder: &str, rep: foldermgmt::EmptyReport) -> CmdResult<u32> {
    for u in &rep.removed {
        let _ = storage.delete_message(account, folder, *u);
    }
    match rep.error {
        None => Ok(rep.removed.len() as u32),
        Some(e) => Err(format!("Deleted {} of {} messages before it stopped: {}", rep.removed.len(), rep.total, err(e))),
    }
}

// ------------------------------------------------------------------------------------------ bulk

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveItem {
    pub uid: Option<u32>,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireOutcome {
    pub uid: Option<u32>,
    pub ok: bool,
    pub error: Option<String>,
    pub new_uid: Option<u32>,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireBulk {
    pub outcomes: Vec<WireOutcome>,
    pub dest_folder: Option<String>,
    /// UIDVALIDITY of the destination (needed to undo by new UID).
    pub dest_validity: Option<u32>,
}

/// Fill in Message-IDs from the cache so a moved message can be found again in its destination (undo).
fn batch_items(storage: &Storage, account: &str, folder: &str, items: &[MoveItem]) -> Vec<BatchItem> {
    items
        .iter()
        .map(|i| {
            let mid = i.message_id.clone().or_else(|| {
                i.uid.and_then(|u| storage.get_message(account, folder, u).ok().flatten()).and_then(|m| m.message_id)
            });
            BatchItem { uid: i.uid, message_id: mid }
        })
        .collect()
}

/// Cache upkeep after a move: the messages that really left the folder are dropped from the cache (they
/// show up in the destination at its next sync). Failed items stay. Returns how many rows were removed.
pub fn apply_move_outcomes(storage: &Storage, account: &str, folder: &str, outcomes: &[mailcore::ItemOutcome]) -> u32 {
    let mut n = 0;
    for o in outcomes.iter().filter(|o| o.ok) {
        if let Some(u) = o.uid {
            if storage.delete_message(account, folder, u).is_ok() {
                n += 1;
            }
        }
    }
    n
}

pub fn wire_outcomes(items: &[BatchItem], outcomes: &[mailcore::ItemOutcome]) -> Vec<WireOutcome> {
    outcomes
        .iter()
        .zip(items)
        .map(|(o, i)| WireOutcome { uid: o.uid.or(i.uid), ok: o.ok, error: o.error.clone(), new_uid: o.new_uid, message_id: i.message_id.clone() })
        .collect()
}

/// Cache upkeep after a flag change: only messages the server confirmed are updated locally.
pub fn apply_flag_outcomes(storage: &Storage, account: &str, folder: &str, outcomes: &[(u32, bool, Option<String>)], change: FlagChange) {
    for (uid, _, _) in outcomes.iter().filter(|o| o.1) {
        if let Ok(Some(m)) = storage.get_message(account, folder, *uid) {
            let mut f = m.flags;
            match change {
                FlagChange::Seen(v) => f.seen = v,
                FlagChange::Flagged(v) => f.flagged = v,
            }
            let _ = storage.update_flags(account, folder, *uid, f);
        }
    }
}

fn check_bulk_len(n: usize) -> CmdResult<()> {
    if n == 0 {
        return Err("No messages selected.".into());
    }
    if n > BULK_MAX {
        return Err(format!("Too many messages at once (limit {BULK_MAX})."));
    }
    Ok(())
}

/// Move messages to a folder (`dest_folder`) or to the folder with a role (`dest_role`: archive, trash, junk, inbox).
/// Used for archive, trash, move, mark-as-junk and undo (then `expected_validity` is the destination's validity
/// returned by the first call and `items` carry the new UIDs and Message-IDs).
#[tauri::command]
#[allow(clippy::too_many_arguments)] // flat wire contract
pub async fn bulk_move(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    items: Vec<MoveItem>,
    dest_folder: Option<String>,
    dest_role: Option<FolderRole>,
    expected_validity: Option<u32>,
) -> CmdResult<WireBulk> {
    check_bulk_len(items.len())?;
    let known = state.storage.list_folders(&account).map_err(err)?;
    let dest = match (dest_folder, dest_role) {
        (Some(d), _) => known.iter().find(|f| f.name == d && f.selectable).map(|f| f.name.clone()).ok_or("That folder no longer exists.")?,
        (None, Some(r)) if r != FolderRole::Other => {
            find_role(&known, r).map(|f| f.name.clone()).ok_or_else(|| format!("No {} folder found on this account.", r.as_str()))?
        }
        _ => return Err("Pick a destination folder.".into()),
    };
    if dest == folder {
        return Err("The messages are already in that folder.".into());
    }
    let batch = batch_items(&state.storage, &account, &folder, &items);
    let validity = expected_validity.or_else(|| {
        batch.iter().find_map(|b| b.uid).and_then(|u| state.storage.get_message(&account, &folder, u).ok().flatten()).map(|m| m.uidvalidity)
    });
    let mut conn = state.connect(&account).await?;
    let r = conn.client.batch_move(&folder, validity, &dest, &batch).await;
    conn.close().await;
    let report = r.map_err(err)?;
    apply_move_outcomes(&state.storage, &account, &folder, &report.outcomes);
    Ok(WireBulk { outcomes: wire_outcomes(&batch, &report.outcomes), dest_folder: Some(dest), dest_validity: report.dest_validity })
}

#[tauri::command]
pub async fn bulk_flag(state: State<'_, AppState>, account: String, folder: String, uids: Vec<u32>, flag: String, value: bool) -> CmdResult<WireBulk> {
    check_bulk_len(uids.len())?;
    let change = match flag.as_str() {
        "seen" => FlagChange::Seen(value),
        "flagged" => FlagChange::Flagged(value),
        _ => return Err("Unsupported flag.".into()),
    };
    let validity = uids.first().and_then(|u| state.storage.get_message(&account, &folder, *u).ok().flatten()).map(|m| m.uidvalidity);
    let mut conn = state.connect(&account).await?;
    let r = conn.client.batch_flag(&folder, validity, &uids, change).await;
    conn.close().await;
    let report = r.map_err(err)?;
    apply_flag_outcomes(&state.storage, &account, &folder, &report.outcomes, change);
    let outcomes = report
        .outcomes
        .iter()
        .map(|(u, ok, e)| WireOutcome { uid: Some(*u), ok: *ok, error: e.clone(), new_uid: None, message_id: None })
        .collect();
    Ok(WireBulk { outcomes, dest_folder: None, dest_validity: None })
}

/// Which of `uids` may be deleted, each with the UIDVALIDITY it was cached under. A UID with no cached
/// validity is refused: without it the server-side check is skipped and the UID could name another message
/// on a reset mailbox.
pub fn delete_plan(storage: &Storage, account: &str, folder: &str, uids: &[u32]) -> Vec<(u32, Option<u32>)> {
    uids.iter().map(|u| (*u, storage.get_message(account, folder, *u).ok().flatten().map(|m| m.uidvalidity))).collect()
}

/// Permanently delete the listed messages. Only Trash and Junk (same rule as emptying a folder); the UI
/// confirms with the count first.
#[tauri::command]
pub async fn bulk_delete_forever(state: State<'_, AppState>, account: String, folder: String, uids: Vec<u32>) -> CmdResult<WireBulk> {
    check_bulk_len(uids.len())?;
    let known = state.storage.list_folders(&account).map_err(err)?;
    foldermgmt::check_emptiable(&known, &folder)?;
    let plan = delete_plan(&state.storage, &account, &folder, &uids);
    let mut conn = if plan.iter().any(|p| p.1.is_some()) { Some(state.connect(&account).await?) } else { None };
    let mut outcomes = Vec::with_capacity(plan.len());
    for (u, validity) in plan {
        let res = match (&mut conn, validity) {
            (Some(c), Some(v)) => c.client.delete_message(&folder, u, Some(v)).await.map_err(err),
            _ => Err("This message isn't in the local cache any more, so it can't be safely deleted. Refresh the folder and try again.".to_string()),
        };
        match res {
            Ok(()) => {
                let _ = state.storage.delete_message(&account, &folder, u);
                outcomes.push(WireOutcome { uid: Some(u), ok: true, error: None, new_uid: None, message_id: None });
            }
            Err(e) => outcomes.push(WireOutcome { uid: Some(u), ok: false, error: Some(e), new_uid: None, message_id: None }),
        }
    }
    if let Some(c) = conn {
        c.close().await;
    }
    Ok(WireBulk { outcomes, dest_folder: None, dest_validity: None })
}

// ------------------------------------------------------------------------------------------ colours

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireColour {
    pub uid: u32,
    pub colour: u8,
}

/// Colour labels are local to this Mac (not synced via IMAP keywords). 0 clears, 1..=7 sets.
#[tauri::command]
pub fn set_colour(state: State<'_, AppState>, account: String, folder: String, uid: u32, colour: u8) -> CmdResult<()> {
    if colour > 7 {
        return Err("Unknown colour.".into());
    }
    let mid = state
        .storage
        .get_message(&account, &folder, uid)
        .map_err(err)?
        .and_then(|m| m.message_id)
        .ok_or("This message has no Message-ID, so a colour can't be attached to it.")?;
    state.storage.set_colour(&account, &mid, colour).map_err(err)
}

#[tauri::command]
pub fn list_colours(state: State<'_, AppState>, account: String, folder: String) -> CmdResult<Vec<WireColour>> {
    Ok(state.storage.colours_in_folder(&account, &folder).map_err(err)?.into_iter().map(|(uid, colour)| WireColour { uid, colour }).collect())
}

// ------------------------------------------------------------------------------------------ unsubscribe

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireUnsub {
    pub mailto: Option<String>,
    pub https_host: Option<String>,
    pub https_url: Option<String>,
    pub one_click: bool,
}

/// Stored headers of a cached message, preferring the cached raw body when there is one.
fn load_headers(state: &AppState, account: &str, folder: &str, uid: u32) -> CmdResult<mailcore::mime::ExtraHeaders> {
    let h = state.storage.get_extra_headers(account, folder, uid).map_err(err)?.ok_or("Message not found. Refresh the folder.")?;
    Ok(match state.storage.get_body(account, folder, uid).map_err(err)? {
        Some(raw) => mailcore::mime::parse_extra_headers(&raw),
        None => h,
    })
}

/// Server names allowed to sign `Authentication-Results` for this account (same rule as auto-reply).
pub(crate) fn auth_hosts(storage: &Storage, account: &str) -> Vec<String> {
    let Ok(Some(cfg)) = storage.get_account(account) else { return vec![] };
    let mut v = vec![cfg.imap.host.to_ascii_lowercase()];
    if let Some((_, d)) = cfg.email.rsplit_once('@') {
        v.push(d.to_ascii_lowercase());
    }
    v.retain(|h| !h.is_empty());
    v
}

/// The bare address inside a `From` display string.
pub fn bare_address(from: &str) -> &str {
    match (from.rfind('<'), from.rfind('>')) {
        (Some(a), Some(b)) if a < b => from[a + 1..b].trim(),
        _ => from.trim(),
    }
}

/// Did the user's own mail server record an aligned `dkim=pass` for the From domain? Forgeable `From` text
/// alone proves nothing, so image auto-loading and one-click POST both require this. No data = no.
pub fn dkim_verified(from: &str, h: &mailcore::mime::ExtraHeaders, hosts: &[String]) -> bool {
    aichat::autoreply::dkim_aligned(&h.authentication_results, hosts, bare_address(from))
}

fn message_dkim_verified(state: &AppState, account: &str, folder: &str, uid: u32, h: &mailcore::mime::ExtraHeaders) -> bool {
    let from = state.storage.get_message(account, folder, uid).ok().flatten().map(|m| m.from).unwrap_or_default();
    dkim_verified(&from, h, &auth_hosts(&state.storage, account))
}

/// May "always load images" take effect for this very message? (The UI keeps images blocked when not.)
#[tauri::command]
pub fn sender_images_verified(state: State<'_, AppState>, account: String, folder: String, uid: u32) -> bool {
    load_headers(&state, &account, &folder, uid).map(|h| message_dkim_verified(&state, &account, &folder, uid, &h)).unwrap_or(false)
}

/// Options for a cached message, re-derived from its stored headers every time (the UI never supplies a target).
/// `one_click` is offered only when the message is DKIM-verified.
fn unsub_options(state: &AppState, account: &str, folder: &str, uid: u32) -> CmdResult<UnsubOptions> {
    let h = load_headers(state, account, folder, uid)?;
    let mut o = parse_list_unsubscribe(&h.list_unsubscribe, &h.list_unsubscribe_post);
    o.one_click = o.one_click && message_dkim_verified(state, account, folder, uid, &h);
    Ok(o)
}

#[tauri::command]
pub fn unsubscribe_info(state: State<'_, AppState>, account: String, folder: String, uid: u32) -> CmdResult<Option<WireUnsub>> {
    let o = unsub_options(&state, &account, &folder, uid)?;
    if o.mailto.is_none() && o.https.is_none() {
        return Ok(None);
    }
    Ok(Some(WireUnsub {
        mailto: o.mailto.map(|m| m.address),
        https_host: o.https.as_ref().map(|h| h.host.clone()),
        https_url: o.https.map(|h| h.url),
        one_click: o.one_click,
    }))
}

/// The unsubscribe mail, built only from the parsed target (never from UI input). Subject and body are fixed.
pub fn unsubscribe_message(t: &mailcore::unsub::MailtoTarget) -> CmdResult<mailcore::OutgoingMessage> {
    serde_json::from_value(serde_json::json!({
        "to": [t.address], "cc": [], "bcc": [], "subject": t.subject(), "text": t.body(), "html": null,
        "attachments": [], "in_reply_to": null,
    }))
    .map_err(err)
}

#[tauri::command]
pub async fn unsubscribe_mailto(state: State<'_, AppState>, account: String, folder: String, uid: u32) -> CmdResult<()> {
    let target = unsub_options(&state, &account, &folder, uid)?.mailto.ok_or("This message has no unsubscribe address.")?;
    let msg = unsubscribe_message(&target)?;
    let cfg = state.account(&account)?;
    let password = state.password(&account)?;
    mailcore::smtp::send(&cfg, &password, &msg).await.map_err(err)?;
    Ok(())
}

/// Body of the RFC 8058 request and nothing else.
pub const ONE_CLICK_BODY: &str = "List-Unsubscribe=One-Click";
const POST_TIMEOUT: Duration = Duration::from_secs(10);

/// Only addresses on the public internet may be contacted (the host name passed the syntax checks, but DNS
/// could still point it at this Mac or the local network).
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() || v.is_broadcast() || v.is_documentation()
                || v.is_multicast() || o[0] == 0 || (o[0] == 100 && (o[1] & 0xc0) == 64) || (o[0] == 198 && (o[1] & 0xfe) == 18) || o[0] >= 240)
        }
        IpAddr::V6(v) => {
            if let Some(m) = v.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(m));
            }
            let s = v.segments();
            let v4 = |hi: u16, lo: u16| std::net::Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
            // Forms that carry an IPv4 address: vet the embedded address as if it were contacted directly.
            // NAT64 64:ff9b::/96 (and local-use 64:ff9b:1::/48), IPv4-compatible ::a.b.c.d, 6to4 2002:AABB:CCDD::/48.
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1) {
                return is_public_ip(IpAddr::V4(v4(s[6], s[7])));
            }
            if s[..6] == [0, 0, 0, 0, 0, 0] {
                return is_public_ip(IpAddr::V4(v4(s[6], s[7])));
            }
            if s[0] == 0x2002 {
                return is_public_ip(IpAddr::V4(v4(s[1], s[2])));
            }
            !(v.is_loopback() || v.is_unspecified() || v.is_multicast() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xffc0) == 0xfec0 || (s[0] == 0x2001 && s[1] == 0x0db8))
        }
    }
}

/// Keep only vetted addresses; refuse when any resolved address is not public.
pub fn vet_addrs(addrs: &[SocketAddr]) -> CmdResult<Vec<SocketAddr>> {
    if addrs.is_empty() {
        return Err("The sender's address could not be found.".into());
    }
    if addrs.iter().any(|a| !is_public_ip(a.ip())) {
        return Err("That address points to a private or local network, so MiraMail won't contact it.".into());
    }
    Ok(addrs.iter().map(|a| SocketAddr::new(a.ip(), 0)).collect())
}

async fn one_click_post(host: &str, url: &str) -> CmdResult<()> {
    let resolved = tokio::time::timeout(POST_TIMEOUT, tokio::net::lookup_host((host, 443u16)))
        .await
        .map_err(|_| "Looking up the sender's address timed out.".to_string())?
        .map_err(|_| "The sender's address could not be found.".to_string())?
        .collect::<Vec<_>>();
    let pinned = vet_addrs(&resolved)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(POST_TIMEOUT)
        .connect_timeout(POST_TIMEOUT)
        .https_only(true)
        .no_proxy()
        .resolve_to_addrs(host, &pinned)
        .build()
        .map_err(|_| "Couldn't prepare the request.".to_string())?;
    let resp = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(ONE_CLICK_BODY)
        .send()
        .await
        .map_err(|e| if e.is_timeout() { "The sender did not answer within 10 seconds.".to_string() } else { "Couldn't reach the sender.".to_string() })?;
    let s = resp.status();
    if s.is_success() {
        Ok(())
    } else if s.is_redirection() {
        Err("The sender answered with a redirect. MiraMail doesn't follow redirects; use the link in your browser instead.".into())
    } else {
        Err(format!("The sender's server refused the request (HTTP {}).", s.as_u16()))
    }
}

#[tauri::command]
pub async fn unsubscribe_one_click(state: State<'_, AppState>, account: String, folder: String, uid: u32) -> CmdResult<()> {
    let o = unsub_options(&state, &account, &folder, uid)?;
    let (true, Some(t)) = (o.one_click, o.https) else { return Err("This message doesn't support one-click unsubscribe.".into()) };
    one_click_post(&t.host, &t.url).await
}

// ------------------------------------------------------------------------------------------ reader

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSource {
    pub headers: String,
    pub raw: String,
    pub size: usize,
    pub truncated: bool,
}

const SOURCE_MAX: usize = 4 * 1024 * 1024;

/// Header block (up to the first blank line) and the raw source, lossily decoded, capped at 4 MB.
pub fn source_view(raw: &[u8]) -> WireSource {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 2).or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|i| i + 1)).unwrap_or(raw.len());
    let cut = raw.len().min(SOURCE_MAX);
    WireSource {
        headers: String::from_utf8_lossy(&raw[..end.min(cut)]).into_owned(),
        raw: String::from_utf8_lossy(&raw[..cut]).into_owned(),
        size: raw.len(),
        truncated: raw.len() > SOURCE_MAX,
    }
}

#[tauri::command]
pub async fn get_source(state: State<'_, AppState>, account: String, folder: String, uid: u32) -> CmdResult<WireSource> {
    let mut conn = None;
    let raw = crate::commands::raw_message(&state, &mut conn, &account, &folder, uid).await;
    if let Some(c) = conn {
        c.close().await;
    }
    Ok(source_view(&raw?))
}

/// File name for a saved message: subject, made safe by `save_unique`'s sanitiser, plus `.eml`.
pub fn eml_name(subject: &str) -> String {
    let s: String = subject.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s: String = s.chars().take(80).collect();
    if s.trim().is_empty() { "message.eml".into() } else { format!("{s}.eml") }
}

#[tauri::command]
pub async fn save_eml(app: AppHandle, state: State<'_, AppState>, account: String, folder: String, uid: u32) -> CmdResult<String> {
    let mut conn = None;
    let raw = crate::commands::raw_message(&state, &mut conn, &account, &folder, uid).await;
    if let Some(c) = conn {
        c.close().await;
    }
    let raw = raw?;
    let subject = state.storage.get_message(&account, &folder, uid).map_err(err)?.map(|m| m.subject).unwrap_or_default();
    let dir = crate::files::downloads_dir(&app)?;
    let path = save_unique(&dir, &eml_name(&subject), &raw).map_err(|e| format!("Couldn't save the file: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Types that are safe to hand to the system's default app. Anything that can run code (apps, scripts,
/// installers, archives with executables, html, svg, office macros files) is NOT here: save those instead.
pub fn openable(name: &str) -> bool {
    const OK: [&str; 21] = [
        "pdf", "png", "jpg", "jpeg", "gif", "webp", "heic", "tiff", "bmp", "txt", "csv", "rtf", "md", "docx", "xlsx", "pptx", "pages", "numbers", "key", "ics", "vcf",
    ];
    name.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && OK.contains(&ext.to_ascii_lowercase().as_str()))
}

#[tauri::command]
pub async fn open_attachment(app: AppHandle, state: State<'_, AppState>, account: String, folder: String, uid: u32, id: String) -> CmdResult<()> {
    use std::hash::{Hash, Hasher};
    let mut conn = None;
    let raw = crate::commands::raw_message(&state, &mut conn, &account, &folder, uid).await;
    if let Some(c) = conn {
        c.close().await;
    }
    let raw = raw?;
    let parsed = mailcore::parse_message(&raw).map_err(err)?;
    let info = parsed.attachments.iter().find(|a| a.id == id).ok_or("Attachment not found.")?;
    let name = crate::files::sanitize_filename(&info.name);
    if !openable(&name) {
        return Err("For safety MiraMail only opens documents, pictures and similar files directly. Save this one and open it yourself.".into());
    }
    let bytes = mailcore::attachment_bytes(&raw, &id).map_err(err)?;
    // The cache key includes UIDVALIDITY: after a reset the same UID is another message. With no known
    // validity nothing is ever reused (the key is unique per call).
    let validity = state.storage.get_message(&account, &folder, uid).map_err(err)?.map(|m| m.uidvalidity);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match validity {
        Some(v) => (&account, &folder, v, uid, &id).hash(&mut h),
        None => (&account, &folder, uid, &id, std::time::SystemTime::now()).hash(&mut h),
    }
    let root = app.path().app_cache_dir().map_err(|_| "Couldn't find the cache folder.".to_string())?.join(crate::files::OPENED_DIR);
    // Owner rule: the app never deletes files on the Mac unasked, so opened copies are not auto-cleaned (see files::cleanup_opened, unused until the owner opts in).
    let dir = root.join(format!("{:016x}", h.finish()));
    let sandboxed = crate::files::is_sandboxed();
    let dl = if sandboxed { Some(crate::files::downloads_dir(&app)?) } else { None };
    // Sandboxed: the copy goes to the real ~/Downloads so the other app can read it. Otherwise the cache folder.
    let path = crate::files::stage_for_open(sandboxed, dl.as_deref(), &dir, &name, &bytes, validity.is_some()).map_err(|e| format!("Couldn't prepare the file: {e}"))?;
    crate::native::open_file(&app, path).await.map_err(|_| "Couldn't open the file.".to_string())
}

/// Native print of the current webview (the UI first mounts a print-only copy of the sanitised message; the
/// `@media print` rules hide everything else).
#[tauri::command]
pub fn print_page(window: tauri::WebviewWindow) -> CmdResult<()> {
    window.print().map_err(err)
}

/// Fill the v5 columns (attachment flag, unsubscribe headers) for mail cached before they existed, in the
/// background, a few rows at a time.
pub fn start_backfill(storage: Arc<Storage>) {
    std::thread::spawn(move || {
        while matches!(storage.backfill_attachment_meta(100), Ok(n) if n > 0) {
            std::thread::sleep(Duration::from_millis(40));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::{AccountConfig, Flags, ItemOutcome, MessageSummary, ServerConfig, Tls};

    const ACC: &str = "me@example.org";

    fn store() -> Storage {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_account(&AccountConfig {
            email: ACC.into(),
            username: "me".into(),
            imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "s".into(), port: 587, tls: Tls::StartTls },
        })
        .unwrap();
        let f = |n: &str, r| Folder { name: n.into(), delimiter: Some("/".into()), role: r, selectable: n != "Parent" };
        s.replace_folders(
            ACC,
            &[f("INBOX", FolderRole::Inbox), f("Archive", FolderRole::Archive), f("Trash", FolderRole::Trash), f("Parent", FolderRole::Other)],
        )
        .unwrap();
        for uid in 1..=5u32 {
            s.upsert_message(&MessageSummary {
                account: ACC.into(),
                folder: "INBOX".into(),
                uid,
                uidvalidity: 7,
                message_id: Some(format!("m{uid}@x")),
                subject: format!("s{uid}"),
                from: "a@x".into(),
                to: ACC.into(),
                date: 100 + uid as i64,
                flags: Flags::default(),
                snippet: String::new(),
                has_body: false,
                size: 1,
            })
            .unwrap();
        }
        s
    }

    fn oc(uid: u32, ok: bool) -> ItemOutcome {
        ItemOutcome { uid: Some(uid), ok, error: (!ok).then(|| "The server refused the move.".to_string()), new_uid: ok.then_some(uid + 100) }
    }

    #[test]
    fn partial_move_failure_only_drops_the_moved_rows() {
        let s = store();
        let items: Vec<MoveItem> = (1..=5).map(|u| MoveItem { uid: Some(u), message_id: None }).collect();
        let batch = batch_items(&s, ACC, "INBOX", &items);
        assert_eq!(batch[2].message_id.as_deref(), Some("m3@x"), "Message-ID filled from the cache for undo");
        let outcomes = vec![oc(1, true), oc(2, false), oc(3, true), oc(4, false), oc(5, false)];
        assert_eq!(apply_move_outcomes(&s, ACC, "INBOX", &outcomes), 2);
        assert_eq!(s.uids(ACC, "INBOX").unwrap(), vec![2, 4, 5], "failed ones stay in the cache");
        let w = wire_outcomes(&batch, &outcomes);
        assert_eq!(w.len(), 5);
        assert!(w[0].ok && w[0].new_uid == Some(101) && w[0].message_id.as_deref() == Some("m1@x"));
        assert!(!w[1].ok && w[1].error.as_deref().unwrap().contains("refused") && w[1].new_uid.is_none());
    }

    #[test]
    fn flag_outcomes_update_only_confirmed_rows() {
        let s = store();
        let outcomes = vec![(1, true, None), (2, false, Some("no".to_string())), (3, true, None), (99, true, None)];
        apply_flag_outcomes(&s, ACC, "INBOX", &outcomes, FlagChange::Seen(true));
        let seen = |u| s.get_message(ACC, "INBOX", u).unwrap().unwrap().flags.seen;
        assert!(seen(1) && !seen(2) && seen(3) && !seen(4));
        apply_flag_outcomes(&s, ACC, "INBOX", &[(1, true, None)], FlagChange::Flagged(true));
        assert!(s.get_message(ACC, "INBOX", 1).unwrap().unwrap().flags.flagged);
    }

    #[test]
    fn bulk_size_limits() {
        assert!(check_bulk_len(0).is_err());
        assert!(check_bulk_len(1).is_ok());
        assert!(check_bulk_len(BULK_MAX).is_ok());
        assert!(check_bulk_len(BULK_MAX + 1).is_err());
    }

    #[test]
    fn search_scopes() {
        let s = store();
        assert_eq!(scopes_for(&s, "folder", Some(ACC), Some("INBOX")).unwrap(), vec![ScopeKey::new(ACC, "INBOX")]);
        assert!(scopes_for(&s, "folder", Some(ACC), None).is_err());
        let acc = scopes_for(&s, "account", Some(ACC), None).unwrap();
        assert_eq!(acc.len(), 3, "the non-selectable parent is skipped");
        assert_eq!(scopes_for(&s, "all", None, None).unwrap(), acc);
        assert!(scopes_for(&s, "everything", None, None).is_err());
        assert!(scopes_for(&s, "account", None, None).is_err());
    }

    #[test]
    fn server_query_translation() {
        let now = 1_800_000_000;
        let (q, folder, notes) = server_query_from("from:alice subject:invoice is:unread invoices after:2026-01-31 before:2026-03-01 folder:Archive", now, 0).unwrap();
        assert_eq!((q.from.as_deref(), q.subject.as_deref(), q.text.as_deref()), (Some("alice"), Some("invoice"), Some("invoices")));
        assert_eq!((q.unseen, q.since.as_deref(), q.before.as_deref()), (Some(true), Some("2026-01-31"), Some("2026-03-01")));
        assert_eq!(folder.as_deref(), Some("archive"));
        assert!(notes.is_empty());
        let (q, _, notes) = server_query_from("-is:unread -from:bob budget has:attachment", now, 0).unwrap();
        assert_eq!((q.unseen, q.text.as_deref(), q.from), (Some(false), Some("budget"), None));
        assert_eq!(notes.len(), 1, "exclusions/attachments are local-only, noted once");
        assert!(server_query_from("has:attachment cc:bob", now, 0).is_err(), "nothing the server can do");
        assert!(server_query_from("", now, 0).is_err());
        // a hostile value never reaches the criteria unquoted: build_search_criteria rejects or quotes it
        let (q, _, _) = server_query_from("subject:\"x\" OR (ALL)", now, 0).unwrap();
        assert!(mailcore::build_search_criteria(&q).unwrap().starts_with("SUBJECT \"x\""));
    }

    #[test]
    fn folder_filter_matching() {
        let f = |n: &str, r| Folder { name: n.into(), delimiter: Some("/".into()), role: r, selectable: true };
        assert!(folder_matches(&f("Projects/Alpha", FolderRole::Other), "alpha"));
        assert!(folder_matches(&f("Projects/Alpha", FolderRole::Other), "projects/alpha"));
        assert!(folder_matches(&f("Gesendet", FolderRole::Sent), "sent"));
        assert!(!folder_matches(&f("Projects/Alpha", FolderRole::Other), "lpha"));
        assert!(folder_matches(&f("INBOX.Work", FolderRole::Other), "work"));
    }

    #[test]
    fn unsubscribe_mail_is_built_from_the_parsed_target_only() {
        let o = parse_list_unsubscribe("<mailto:u@x.example?cc=v@y.example&subject=bye>", "");
        let m = unsubscribe_message(&o.mailto.unwrap()).unwrap();
        assert_eq!(m.to, vec!["u@x.example".to_string()]);
        assert!(m.cc.is_empty() && m.bcc.is_empty() && m.attachments.is_empty() && m.html.is_none());
        assert_eq!((m.subject.as_str(), m.text.as_str()), ("unsubscribe", "unsubscribe"), "header-supplied subject is ignored");
    }

    #[test]
    fn unsub_options_come_from_stored_headers() {
        let s = store();
        let h = mailcore::mime::ExtraHeaders {
            list_unsubscribe: "<https://news.example.com/u?id=1>, <mailto:u@news.example.com>".into(),
            list_unsubscribe_post: "List-Unsubscribe=One-Click".into(),
            ..Default::default()
        };
        s.set_extra_headers(ACC, "INBOX", 1, &h).unwrap();
        let got = s.get_extra_headers(ACC, "INBOX", 1).unwrap().unwrap();
        let o = parse_list_unsubscribe(&got.list_unsubscribe, &got.list_unsubscribe_post);
        assert!(o.one_click && o.mailto.is_some());
        assert_eq!(o.https.unwrap().host, "news.example.com");
    }

    #[test]
    fn only_public_addresses_are_contacted() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for bad in [
            "127.0.0.1", "10.1.2.3", "192.168.0.9", "172.16.5.5", "169.254.1.1", "0.0.0.0", "100.64.0.1", "198.18.0.1", "224.0.0.1", "255.255.255.255", "240.0.0.1",
            "::1", "::", "fe80::1", "fec0::1", "64:ff9b::7f00:1", "64:ff9b::a00:1", "64:ff9b::c0a8:1", "2002:7f00:1::1", "2002:0a00:1::", "::7f00:1", "::10.0.0.1", "64:ff9b:1::a9fe:1", "fc00::1", "fd12::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "2001:db8::1", "ff02::1",
        ] {
            assert!(!is_public_ip(ip(bad)), "{bad}");
        }
        for good in ["93.184.216.34", "8.8.8.8", "2606:4700:4700::1111", "::ffff:8.8.8.8", "64:ff9b::808:808", "2002:808:808::1", "::8.8.8.8"] {
            assert!(is_public_ip(ip(good)), "{good}");
        }
        let sa = |s: &str| SocketAddr::new(ip(s), 443);
        assert!(vet_addrs(&[]).is_err());
        assert!(vet_addrs(&[sa("8.8.8.8"), sa("10.0.0.1")]).is_err(), "one private answer poisons the set");
        assert_eq!(vet_addrs(&[sa("8.8.8.8")]).unwrap()[0].port(), 0, "port 0: the URL's own port is used");
    }

    #[test]
    fn delete_plan_refuses_uids_without_cached_validity() {
        let s = store();
        let plan = delete_plan(&s, ACC, "INBOX", &[1, 99]);
        assert_eq!(plan, vec![(1, Some(7)), (99, None)], "an uncached UID has no validity, so the command refuses it");
        // the role gate every permanent delete goes through
        let known = s.list_folders(ACC).unwrap();
        assert!(foldermgmt::check_emptiable(&known, "Trash").is_ok());
        assert!(foldermgmt::check_emptiable(&known, "INBOX").is_err());
        assert!(foldermgmt::check_emptiable(&known, "Archive").is_err());
        assert!(foldermgmt::check_emptiable(&known, "Gone").is_err());
    }

    #[test]
    fn empty_report_drops_only_expunged_uids() {
        let s = store();
        let rep = foldermgmt::EmptyReport { total: 5, removed: vec![1, 2], error: Some(mailcore::Error::Timeout("expunge")) };
        let e = apply_empty_report(&s, ACC, "INBOX", rep).unwrap_err();
        assert!(e.contains("Deleted 2 of 5"), "{e}");
        assert_eq!(s.uids(ACC, "INBOX").unwrap(), vec![3, 4, 5], "the unconfirmed ones stay cached");
        let ok = apply_empty_report(&s, ACC, "INBOX", foldermgmt::EmptyReport { total: 3, removed: vec![3, 4, 5], error: None });
        assert_eq!(ok.unwrap(), 3);
        assert!(s.uids(ACC, "INBOX").unwrap().is_empty());
    }

    #[test]
    fn local_list_follows_a_rename_when_the_refresh_fails() {
        let f = |n: &str| Folder { name: n.into(), delimiter: Some("/".into()), role: FolderRole::Other, selectable: true };
        let known = vec![f("Projects"), f("Projects/Alpha"), f("ProjectsX"), f("Misc")];
        let names: Vec<String> = remap_folders(&known, "Projects", "Misc/Projects").into_iter().map(|x| x.name).collect();
        assert_eq!(names, vec!["Misc/Projects", "Misc/Projects/Alpha", "ProjectsX", "Misc"]);
        assert_eq!(remapped("Projects/Alpha/Deep", "Projects", "P2"), "P2/Alpha/Deep");
        assert_eq!(remapped("Projects", "Projects", "P2"), "P2");
    }

    #[test]
    fn children_follow_a_renamed_parent() {
        let f = |n: &str| Folder { name: n.into(), delimiter: Some("/".into()), role: FolderRole::Other, selectable: true };
        let known = vec![f("Projects"), f("Projects/Alpha"), f("Projects/Alpha/Deep"), f("ProjectsX"), f("Misc")];
        assert_eq!(folder_and_children(&known, "Projects"), vec!["Projects", "Projects/Alpha", "Projects/Alpha/Deep"]);
        assert_eq!(folder_and_children(&known, "Misc"), vec!["Misc"]);
    }

    #[test]
    fn images_and_one_click_need_an_aligned_dkim_pass_from_our_server() {
        let hosts = vec!["imap.example.org".to_string(), "example.org".to_string()];
        let h = |ar: &str| mailcore::mime::ExtraHeaders { authentication_results: ar.into(), ..Default::default() };
        let from = "Shop <news@mail.shop.test>";
        assert_eq!(bare_address(from), "news@mail.shop.test");
        assert!(dkim_verified(from, &h("imap.example.org; dkim=pass header.d=shop.test"), &hosts));
        assert!(!dkim_verified(from, &h(""), &hosts), "no data: stay blocked");
        assert!(!dkim_verified(from, &h("imap.example.org; dkim=fail header.d=shop.test"), &hosts));
        assert!(!dkim_verified(from, &h("imap.example.org; dkim=pass header.d=attacker.test"), &hosts), "not aligned");
        assert!(!dkim_verified(from, &h("evil.test; dkim=pass header.d=shop.test"), &hosts), "forged by the sender's side");
        assert!(!dkim_verified(from, &h("imap.example.org; spf=pass smtp.mailfrom=news@shop.test"), &hosts), "SPF alone does not count");
        assert!(!dkim_verified(from, &h("imap.example.org; dkim=pass header.d=shop.test"), &[]), "unknown server: nothing trusted");
    }

    #[test]
    fn one_click_body_is_exactly_the_standard_one() {
        assert_eq!(ONE_CLICK_BODY, "List-Unsubscribe=One-Click");
    }

    #[test]
    fn source_view_splits_headers_and_caps() {
        let v = source_view(b"From: a@x\r\nSubject: hi\r\n\r\nbody text");
        assert_eq!(v.headers, "From: a@x\r\nSubject: hi\r\n");
        assert!(v.raw.ends_with("body text") && !v.truncated && v.size == 35);
        let big = vec![b'a'; SOURCE_MAX + 10];
        let v = source_view(&big);
        assert!(v.truncated && v.raw.len() == SOURCE_MAX && v.size == SOURCE_MAX + 10);
        assert_eq!(source_view(b"\xff\xfe").raw.chars().count(), 2, "lossy, no panic");
    }

    #[test]
    fn eml_names_and_openable_types() {
        assert_eq!(eml_name("Re: Q3 plan\r\n"), "Re: Q3 plan.eml");
        assert_eq!(eml_name("  \n "), "message.eml");
        assert_eq!(eml_name(&"x".repeat(300)).len(), 84);
        assert_eq!(crate::files::sanitize_filename(&eml_name("../../etc/passwd")), "passwd.eml");
        for ok in ["a.pdf", "Photo.JPG", "sheet.xlsx", "n.txt"] {
            assert!(openable(ok), "{ok}");
        }
        for bad in ["run.command", "x.app", "a.sh", "i.dmg", "m.pkg", "p.html", "v.svg", "x.exe", "x.jar", "x.docm", "noext", ".pdf", "evil.pdf.app", "z.zip", "a.webloc", "a.terminal"] {
            assert!(!openable(bad), "{bad}");
        }
    }
}
