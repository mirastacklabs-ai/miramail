//! Tauri commands (contract: docs/APP_API.md). Errors are plain user-facing strings.

use crate::bgsync::SyncSettings;
use crate::files::save_unique;
use crate::live::{self, LiveState, LiveStatus};
use crate::state::{err, start_watcher, AppState};
use crate::wire::*;
use autodetect::{Candidate, DetectionResult, Source, UsernameHint};
use base64::Engine as _;
use mailcore::{
    attachment_bytes, imap::find_role, parse_message, render_html_with, AccountConfig, FlagChange, Folder, FolderRole,
};
use tauri::{AppHandle, State};

type CmdResult<T> = Result<T, String>;

fn candidate(s: &WireServer) -> Candidate {
    Candidate {
        host: s.host.trim().to_string(),
        port: s.port,
        tls: s.tls.into(),
        source: Source::Heuristic,
        username_hint: UsernameHint::Email,
    }
}

fn config_from_verified(v: &autodetect::VerifiedSettings) -> AccountConfig {
    let srv = |s: &autodetect::ServerSettings| mailcore::ServerConfig {
        host: s.host.clone(),
        port: s.port,
        tls: WireTls::from(s.tls).into(),
    };
    AccountConfig { email: v.email.clone(), imap: srv(&v.imap), smtp: srv(&v.smtp), username: v.imap.username.clone() }
}

/// Verify user-entered (Advanced) settings. If the username is the address or its local part,
/// autodetect's verifier checks IMAP and SMTP; a custom username can only be checked over IMAP
/// (SMTP is then confirmed on first send).
async fn verify_advanced(email: &str, password: &str, adv: &Advanced) -> CmdResult<AccountConfig> {
    let username = adv.username.trim().to_string();
    let local = email.rsplit_once('@').map(|(l, _)| l).unwrap_or(email);
    if username.is_empty() || username == email || username == local {
        let det = DetectionResult {
            email: email.to_string(),
            domain: email.rsplit_once('@').map(|(_, d)| d.to_string()).unwrap_or_default(),
            provider: None,
            imap: vec![candidate(&adv.imap)],
            smtp: vec![candidate(&adv.smtp)],
            sources: vec![],
            elapsed_ms: 0,
        };
        let v = autodetect::verify(&det, email, password).await.map_err(|e| e.user_message())?;
        Ok(config_from_verified(&v))
    } else {
        let imap: mailcore::ServerConfig = (&adv.imap).into();
        let client = mailcore::ImapClient::connect(&imap, &username, password).await.map_err(err)?;
        let _ = client.logout().await;
        Ok(AccountConfig { email: email.to_string(), imap, smtp: (&adv.smtp).into(), username })
    }
}

#[tauri::command]
pub async fn add_account(
    app: AppHandle,
    state: State<'_, AppState>,
    email: String,
    password: String,
    advanced: Option<Advanced>,
) -> CmdResult<AccountInfo> {
    let email = email.trim().to_lowercase();
    if !email.contains('@') || email.starts_with('@') || email.ends_with('@') || email.contains(char::is_whitespace) {
        return Err("That doesn't look like an email address.".into());
    }
    if password.is_empty() {
        return Err("Enter your password.".into());
    }
    if state.storage.get_account(&email).map_err(err)?.is_some() {
        return Err("This account is already added.".into());
    }

    let (cfg, provider) = match &advanced {
        Some(adv) => (verify_advanced(&email, &password, adv).await?, None),
        None => {
            let det = autodetect::detect(&email).await.map_err(|e| e.user_message())?;
            let v = autodetect::verify(&det, &email, &password).await.map_err(|e| e.user_message())?;
            let provider = v.provider.clone();
            (config_from_verified(&v), provider)
        }
    };

    // Verification succeeded: only now persist anything.
    state.storage.upsert_account(&cfg).map_err(err)?;
    if let Err(e) = state.secrets.set(&email, &password) {
        let _ = state.storage.delete_account(&email);
        return Err(err(e));
    }
    let info = AccountInfo::from_config(&cfg, provider);
    start_watcher(&app, cfg, password);
    Ok(info)
}

#[tauri::command]
pub async fn list_accounts(state: State<'_, AppState>) -> CmdResult<Vec<AccountInfo>> {
    // The provider id is not persisted by mailcore, so it is only known right after add_account.
    Ok(state.storage.list_accounts().map_err(err)?.iter().map(|c| AccountInfo::from_config(c, None)).collect())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri-injected State params + flat wire contract
pub async fn remove_account(
    state: State<'_, AppState>,
    drafts: State<'_, crate::drafts::DraftStore>,
    composer: State<'_, crate::composer::ComposeStore>,
    ai: State<'_, crate::ai::AiState>,
    outbox: State<'_, crate::outbox::Outbox>,
    mirror: State<'_, crate::draft_mirror::DraftMirror>,
    snooze: State<'_, crate::snooze::SnoozeState>,
    email: String,
) -> CmdResult<()> {
    state.stop_watcher(&email);
    let _ = snooze.store.delete_for_account(&email);
    state.storage.set_cache_policy(&email, Default::default());
    ai.purge_account(&email);
    ai.forget_account_automation(&email);
    drafts.delete_for_account(&email)?;
    let _ = composer.delete_for_account(&email);
    let _ = outbox.store.delete_for_account(&email);
    let _ = mirror.delete_for_account(&email);
    state.secrets.delete(&email).map_err(err)?;
    state.storage.delete_account(&email).map_err(err)
}

#[tauri::command]
pub async fn list_folders(state: State<'_, AppState>, account: String) -> CmdResult<Vec<WireFolder>> {
    let cached = state.storage.list_folders(&account).map_err(err)?;
    let fresh = match state.connect(&account).await {
        Ok(mut conn) => {
            let r = conn.client.list_folders().await;
            conn.close().await;
            r.ok()
        }
        Err(e) if cached.is_empty() => return Err(e),
        Err(_) => None,
    };
    let folders = match fresh {
        Some(f) => {
            state.storage.replace_folders(&account, &f).map_err(err)?;
            f
        }
        None if cached.is_empty() => return Err("Couldn't load folders. Check your connection.".into()),
        None => cached,
    };
    Ok(folders.iter().map(WireFolder::from).collect())
}

/// Unread count per folder (IMAP STATUS UNSEEN). Used for the sidebar new-mail dots.
#[tauri::command]
pub async fn unread_counts(app: AppHandle, state: State<'_, AppState>, account: String) -> CmdResult<std::collections::HashMap<String, u32>> {
    let folders = state.storage.list_folders(&account).map_err(err)?;
    let mut conn = state.connect(&account).await?;
    let r = conn.client.unseen_counts(&folders).await;
    conn.close().await;
    let counts: std::collections::HashMap<String, u32> = r.map_err(err)?.into_iter().collect();
    // The exact INBOX number keeps the Dock badge / menu-bar count right when more is unread than is cached.
    crate::badge::record_exact(&app, &state.storage, &account, &counts);
    Ok(counts)
}

#[tauri::command]
pub async fn sync_folder(app: AppHandle, state: State<'_, AppState>, account: String, folder: String) -> CmdResult<WireSyncReport> {
    let mut conn = state.connect(&account).await?;
    let r = conn.client.sync_folder(&state.storage, &account, &folder, state.initial_sync_limit()).await;
    conn.close().await;
    let report = r.map_err(err)?;
    if report.new > 0 {
        crate::automation::notify(&app, &account, &folder);
    }
    Ok(report.into())
}

/// Most rows one `list_messages` call returns.
pub const LIST_MAX: u32 = 10_000;
pub const LOAD_OLDER_DEFAULT: u32 = 200;
pub const LOAD_OLDER_MAX: u32 = 1000;

/// `None` -> 200; otherwise clamped to 1..=1000.
pub fn load_older_count(count: Option<u32>) -> u32 {
    count.unwrap_or(LOAD_OLDER_DEFAULT).clamp(1, LOAD_OLDER_MAX)
}

/// Fetch the next `count` (default 200, max 1000) headers older than the oldest cached message.
#[tauri::command]
pub async fn load_older(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    count: Option<u32>,
) -> CmdResult<WireLoadOlder> {
    let mut conn = state.connect(&account).await?;
    let r = conn.client.load_older(&state.storage, &account, &folder, load_older_count(count)).await;
    conn.close().await;
    Ok(r.map_err(err)?.into())
}

#[tauri::command]
pub async fn list_messages(
    state: State<'_, AppState>,
    snooze: State<'_, crate::snooze::SnoozeState>,
    account: String,
    folder: String,
    limit: u32,
    offset: u32,
) -> CmdResult<Vec<WireSummary>> {
    let mut rows = state.storage.list_messages(&account, &folder, limit.min(LIST_MAX), offset).map_err(err)?;
    // Messages snoozed without a server folder stay where they are but are hidden here until they come back.
    let hidden = snooze.store.hidden(&account, &folder);
    if !hidden.is_empty() {
        rows.retain(|m| !hidden.contains(&(m.uid, m.uidvalidity)));
    }
    Ok(rows.iter().map(WireSummary::from).collect())
}

/// Shown when a stored (account, folder, uid) no longer names the message it was taken from.
pub(crate) const ORIGINAL_GONE: &str = "The original message is no longer available.";

/// When `expected` is given (a forward restored from a saved draft), the cached summary must exist and carry that
/// UIDVALIDITY: a bare UID can name a different message after the server renumbers the folder. The IMAP fetch
/// then re-checks the same validity against the server (`fetch_body`).
pub(crate) fn check_original(state: &AppState, account: &str, folder: &str, uid: u32, expected: Option<u32>) -> CmdResult<()> {
    let Some(want) = expected else { return Ok(()) };
    match state.storage.get_message(account, folder, uid).map_err(err)? {
        Some(m) if m.uidvalidity == want => Ok(()),
        _ => Err(ORIGINAL_GONE.into()),
    }
}

/// Raw message from cache, else fetched (and cached) over `conn`.
pub(crate) async fn raw_message(
    state: &AppState,
    conn: &mut Option<crate::state::Conn>,
    account: &str,
    folder: &str,
    uid: u32,
) -> CmdResult<Vec<u8>> {
    if let Some(raw) = state.storage.get_body(account, folder, uid).map_err(err)? {
        return Ok(raw);
    }
    if conn.is_none() {
        *conn = Some(state.connect(account).await?);
    }
    let c = conn.as_mut().expect("connected above");
    c.client.get_body(&state.storage, account, folder, uid).await.map_err(err)
}

async fn close_opt(conn: Option<crate::state::Conn>) {
    if let Some(c) = conn {
        c.close().await;
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri-injected State params + flat wire contract
pub async fn get_message(
    state: State<'_, AppState>,
    cal: State<'_, crate::cal_state::CalState>,
    account: String,
    folder: String,
    uid: u32,
    allow_remote_images: bool,
    skip_trackers: Option<bool>,
    mark_seen: Option<bool>,
) -> CmdResult<MessageBody> {
    let mut conn = None;
    let raw = raw_message(&state, &mut conn, &account, &folder, uid).await;
    let raw = match raw {
        Ok(r) => r,
        Err(e) => {
            close_opt(conn).await;
            return Err(e);
        }
    };

    // Mark seen (best effort: offline reading must still work). `mark_seen = Some(false)` is the Preferences "never / after N s" case.
    if let Some(sum) = state.storage.get_message(&account, &folder, uid).map_err(err)? {
        if !sum.flags.seen && mark_seen != Some(false) {
            if conn.is_none() {
                conn = state.connect(&account).await.ok();
            }
            if let Some(c) = conn.as_mut() {
                if c.client.set_flag(&folder, uid, Some(sum.uidvalidity), FlagChange::Seen(true)).await.is_ok() {
                    let mut f = sum.flags;
                    f.seen = true;
                    let _ = state.storage.update_flags(&account, &folder, uid, f);
                }
            }
        }
    }
    close_opt(conn).await;

    let summary = state
        .storage
        .get_message(&account, &folder, uid)
        .map_err(err)?
        .ok_or_else(|| "Message not found. Refresh the folder.".to_string())?;
    let parsed = parse_message(&raw).map_err(err)?;
    // Static, advisory tracker analysis of the RAW html (independent of what gets loaded).
    let mut recipients: Vec<String> = vec![account.clone()];
    recipients.extend(parsed.headers.to.iter().chain(parsed.headers.cc.iter()).map(|a| a.address.clone()));
    let sender_domain = parsed.headers.from.first().and_then(|a| a.address.rsplit_once('@').map(|(_, d)| d.to_string()));
    let images: Vec<WireImage> = parsed
        .html
        .as_deref()
        .map(|h| mailcore::analyze_remote_images_from(h, &recipients, sender_domain.as_deref()))
        .unwrap_or_default();
    let deny = match (&parsed.html, allow_remote_images && skip_trackers.unwrap_or(true)) {
        (Some(h), true) => mailcore::tracker_url_set(h, &recipients),
        _ => Default::default(),
    };
    let rendered = render_html_with(&raw, allow_remote_images, &deny).map_err(err)?;
    let (html, blocked) = match rendered {
        Some(s) => (Some(s.html), s.blocked_remote_count),
        None => (None, 0),
    };
    let references = (!parsed.headers.references.is_empty()).then(|| parsed.headers.references.join(" "));
    Ok(MessageBody {
        summary: WireSummary::from(&summary),
        text: parsed.text,
        html,
        blocked_remote_count: blocked,
        images,
        attachments: parsed
            .attachments
            .iter()
            .filter(|a| !a.inline)
            .map(|a| WireAttachment { id: a.id.clone(), name: a.name.clone(), mime: a.mime.clone(), size: a.size })
            .collect(),
        cc: parsed.headers.cc.iter().map(Address::from).collect(),
        reply_to: parsed.headers.reply_to.iter().map(Address::from).collect(),
        references,
        invite: crate::cal_invite::invite_for_message(&cal, &raw, &parsed.attachments, &account),
    })
}

/// (name, mime, bytes) of one attachment.
async fn load_attachment(
    state: &AppState,
    account: &str,
    folder: &str,
    uid: u32,
    id: &str,
    expected_validity: Option<u32>,
) -> CmdResult<(String, String, Vec<u8>)> {
    check_original(state, account, folder, uid, expected_validity)?;
    let mut conn = None;
    let raw = raw_message(state, &mut conn, account, folder, uid).await;
    close_opt(conn).await;
    let raw = raw?;
    let parsed = parse_message(&raw).map_err(err)?;
    let info = parsed
        .attachments
        .iter()
        .find(|a| a.id == id)
        .ok_or_else(|| "Attachment not found.".to_string())?;
    let bytes = attachment_bytes(&raw, id).map_err(err)?;
    Ok((info.name.clone(), info.mime.clone(), bytes))
}

#[tauri::command]
pub async fn get_attachment(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: u32,
    id: String,
    uidvalidity: Option<u32>,
) -> CmdResult<AttachmentData> {
    let (name, mime, bytes) = load_attachment(&state, &account, &folder, uid, &id, uidvalidity).await?;
    Ok(AttachmentData { name, mime, data_base64: base64::engine::general_purpose::STANDARD.encode(bytes) })
}

#[tauri::command]
pub async fn save_attachment_to_downloads(
    app: AppHandle,
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: u32,
    id: String,
) -> CmdResult<String> {
    let (name, _mime, bytes) = load_attachment(&state, &account, &folder, uid, &id, None).await?;
    let dir = crate::files::downloads_dir(&app)?;
    let path = save_unique(&dir, &name, &bytes).map_err(|e| format!("Couldn't save the file: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn set_flag(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: u32,
    flag: String,
    value: bool,
) -> CmdResult<()> {
    let change = match flag.as_str() {
        "seen" => FlagChange::Seen(value),
        "flagged" => FlagChange::Flagged(value),
        _ => return Err("Unsupported flag.".into()),
    };
    let validity = state.storage.get_message(&account, &folder, uid).map_err(err)?.map(|m| m.uidvalidity);
    let mut conn = state.connect(&account).await?;
    let r = conn.client.set_flag(&folder, uid, validity, change).await;
    conn.close().await;
    r.map_err(err)?;
    if let Some(m) = state.storage.get_message(&account, &folder, uid).map_err(err)? {
        let mut f = m.flags;
        match change {
            FlagChange::Seen(v) => f.seen = v,
            FlagChange::Flagged(v) => f.flagged = v,
        }
        state.storage.update_flags(&account, &folder, uid, f).map_err(err)?;
    }
    Ok(())
}

fn folders_for(state: &AppState, account: &str) -> CmdResult<Vec<Folder>> {
    state.storage.list_folders(account).map_err(err)
}

#[tauri::command]
pub async fn move_message(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: u32,
    dest_role: FolderRole,
) -> CmdResult<()> {
    if dest_role == FolderRole::Other {
        return Err("Unsupported destination.".into());
    }
    let folders = folders_for(&state, &account)?;
    let dest = find_role(&folders, dest_role)
        .map(|f| f.name.clone())
        .ok_or_else(|| format!("No {} folder found on this account.", dest_role.as_str()))?;
    let validity = state.storage.get_message(&account, &folder, uid).map_err(err)?.map(|m| m.uidvalidity);
    let mut conn = state.connect(&account).await?;
    let r = conn.client.move_message(&folder, uid, validity, &dest).await;
    conn.close().await;
    r.map_err(err)?;
    state.storage.delete_message(&account, &folder, uid).map_err(err)
}

/// Move one message to any selectable folder (drag and drop in the UI).
#[tauri::command]
pub async fn move_message_to(
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: u32,
    dest_folder: String,
) -> CmdResult<()> {
    if dest_folder == folder {
        return Ok(());
    }
    let folders = folders_for(&state, &account)?;
    let dest = folders
        .iter()
        .find(|f| f.name == dest_folder)
        .ok_or_else(|| "That folder no longer exists.".to_string())?;
    if !dest.selectable {
        return Err("Messages can't be moved into that folder.".into());
    }
    let dest = dest.name.clone();
    let validity = state.storage.get_message(&account, &folder, uid).map_err(err)?.map(|m| m.uidvalidity);
    let mut conn = state.connect(&account).await?;
    let r = conn.client.move_message(&folder, uid, validity, &dest).await;
    conn.close().await;
    r.map_err(err)?;
    state.storage.delete_message(&account, &folder, uid).map_err(err)
}

#[tauri::command]
pub async fn delete_message(state: State<'_, AppState>, account: String, folder: String, uid: u32) -> CmdResult<()> {
    let validity = state.storage.get_message(&account, &folder, uid).map_err(err)?.map(|m| m.uidvalidity);
    let mut conn = state.connect(&account).await?;
    let r = conn.client.delete_message(&folder, uid, validity).await;
    conn.close().await;
    r.map_err(err)?;
    state.storage.delete_message(&account, &folder, uid).map_err(err)
}

#[tauri::command]
pub fn get_sync_settings(state: State<'_, AppState>) -> SyncSettings {
    *state.sync_cfg.borrow()
}

/// Validate, persist, then apply live to running loops.
#[tauri::command]
pub fn set_sync_settings(state: State<'_, AppState>, settings: SyncSettings) -> CmdResult<SyncSettings> {
    settings.validate()?;
    let prev = state.sync_cfg.send_replace(settings);
    if let Err(e) = state.persist_config() {
        state.sync_cfg.send_replace(prev);
        return Err(e);
    }
    Ok(settings)
}

#[tauri::command]
pub fn list_live_folders(state: State<'_, AppState>, account: String) -> Vec<String> {
    state.live_folders(&account)
}

/// Current state of this session's live watchers for the account (folder -> status).
#[tauri::command]
pub fn live_states(state: State<'_, AppState>, account: String) -> std::collections::HashMap<String, LiveStatus> {
    state.live_states(&account)
}

/// Enable/disable a live folder. Enabling waits (<= 10 s) for the server's first answer; on `error`
/// the watcher is stopped and nothing is persisted.
#[tauri::command]
pub async fn set_folder_live(
    app: AppHandle,
    state: State<'_, AppState>,
    account: String,
    folder: String,
    live: bool,
) -> CmdResult<LiveStatus> {
    if !live {
        state.stop_live(&account, &folder);
        state.set_live_cfg(&account, &folder, false);
        state.persist_config()?;
        return Ok(LiveStatus::new(LiveState::Off));
    }
    let cfg = state.account(&account)?;
    let folders = state.storage.list_folders(&account).map_err(err)?;
    let max = state.sync_cfg.borrow().max_live_folders;
    live::check_enable(&state.live_folders(&account), &folder, max, &folders)?;
    let password = state.password(&account)?;

    let mut rx = live::start_live(&app, &cfg, password, folder.clone());
    let first = match tokio::time::timeout(live::FIRST_OUTCOME_TIMEOUT, rx.wait_for(|s| s.state != LiveState::Connecting)).await {
        Ok(Ok(s)) => s.clone(),
        _ => LiveStatus::error("timed out: no answer from the server"),
    };
    if first.state == LiveState::Error {
        state.stop_live(&account, &folder);
        return Ok(first);
    }
    state.set_live_cfg(&account, &folder, true);
    if let Err(e) = state.persist_config() {
        state.stop_live(&account, &folder);
        state.set_live_cfg(&account, &folder, false);
        return Err(e);
    }
    Ok(first)
}

#[cfg(test)]
mod load_older_tests {
    use super::*;

    #[test]
    fn count_defaults_and_clamps() {
        assert_eq!(load_older_count(None), 200);
        assert_eq!(load_older_count(Some(0)), 1);
        assert_eq!(load_older_count(Some(500)), 500);
        assert_eq!(load_older_count(Some(1000)), 1000);
        assert_eq!(load_older_count(Some(1001)), 1000);
        assert_eq!(load_older_count(Some(u32::MAX)), 1000);
    }
}
