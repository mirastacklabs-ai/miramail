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
use tauri::{AppHandle, Manager, State};

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
    app: tauri::AppHandle,
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
    let r = state.storage.delete_account(&email).map_err(err);
    // iPhone widgets: drop the removed account's counts / names now (the last account removed deletes widget.json).
    crate::widget_snapshot::request(&app, true);
    r
}

#[tauri::command]
pub async fn list_folders(state: State<'_, AppState>, account: String) -> CmdResult<Vec<WireFolder>> {
    let cached = state.storage.list_folders(&account).map_err(err)?;
    // iPhone, the user is away: the cached list only (no LIST, nothing written); the page asks again when they are back.
    if !crate::presence::user_present() {
        if cached.is_empty() {
            return Err(crate::presence::DEFERRED.into());
        }
        return Ok(cached.iter().map(WireFolder::from).collect());
    }
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
    crate::presence::require()?; // iPhone: no STATUS round while the user is away
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
    crate::presence::require()?; // iPhone: the background check alone reads mail while the user is away
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

/// Raw message for the Reader (body, attachments): the offline cache, a recent Reader fetch, else the account's
/// interactive lane, so a user-initiated open never queues behind background syncs.
pub(crate) async fn reader_raw(state: &AppState, account: &str, folder: &str, uid: u32) -> CmdResult<std::sync::Arc<[u8]>> {
    Ok(reader_raw_src(state, account, folder, uid).await?.0)
}

/// [`reader_raw`] plus whether it was available locally (`true`) or fetched from the server.
async fn reader_raw_src(state: &AppState, account: &str, folder: &str, uid: u32) -> CmdResult<(std::sync::Arc<[u8]>, bool)> {
    if let Some(raw) = state.body_for(account, folder, uid)? {
        return Ok((raw, true));
    }
    let mut ic = state.interactive(account).await?;
    let r = ic.client.get_body(&state.storage, account, folder, uid).await;
    let k = ic.client.timings;
    crate::safety_cmds::perf::log(&format!(
        "perf: get_message fetch: lane=interactive reused={} select={}ms fetch={}ms bytes={} cache_write={}ms",
        ic.reused, k.select_ms, k.fetch_ms, k.fetched_bytes, k.store_ms
    ));
    // The session stays warm after a clean answer ("no such message" is one); any other error closes it.
    let keep = matches!(&r, Ok(_) | Err(mailcore::Error::NotFound(_)));
    let raw: std::sync::Arc<[u8]> = match r {
        Ok(raw) => raw.into(),
        Err(e) => {
            ic.finish(false);
            return Err(err(e));
        }
    };
    // Kept in memory only when the offline cache did not store it (cache policy); remembered while the lane is still
    // held, so an account removal or the user leaving in between is seen (state.rs `keep_body`).
    if let Ok(Some(m)) = state.storage.get_message(account, folder, uid) {
        if !m.has_body {
            state.remember_body(&ic, account, folder, uid, m.uidvalidity, raw.clone());
        }
    }
    ic.finish(keep);
    Ok((raw, false))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri-injected State params + flat wire contract
pub async fn get_message(
    app: AppHandle,
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: u32,
    allow_remote_images: bool,
    skip_trackers: Option<bool>,
    mark_seen: Option<bool>,
) -> CmdResult<MessageBody> {
    let t0 = std::time::Instant::now();
    let (raw, cached) = reader_raw_src(&state, &account, &folder, uid).await?;
    let t_raw = t0.elapsed();

    let summary = state.storage.get_message(&account, &folder, uid).map_err(err)?.ok_or_else(|| "Message not found. Refresh the folder.".to_string())?;

    // Body first: parse + render only. Remote images stay blocked unless the user/sender rule allowed them AND trackers
    // are not being skipped; when trackers must be skipped, `message_analysis` computes the deny-set and re-renders, so
    // a tracker is never requested before it is known. CPU work: off the async worker.
    let render_allow = allow_remote_images && !skip_trackers.unwrap_or(true);
    let (parsed, rendered, t_parse, t_render) = tauri::async_runtime::spawn_blocking(move || -> CmdResult<_> {
        let tp = std::time::Instant::now();
        let parsed = parse_message(&raw).map_err(err)?;
        let t_parse = tp.elapsed();
        let tr = std::time::Instant::now();
        let rendered = render_html_with(&raw, render_allow, &Default::default()).map_err(err)?;
        Ok((parsed, rendered, t_parse, tr.elapsed()))
    })
    .await
    .map_err(|e| format!("reader stopped: {e}"))??;

    // Mark seen only now that the message parsed and rendered, and AFTER the body is returned (best effort, offline
    // reading must still work): the reader never waits on a network round trip. `mark_seen = Some(false)` is the
    // Preferences "never / after N s" case. The UI already shows the message as read when it asked for this; if the
    // mark is skipped or fails, `mail://synced` makes it reload that row and the unread counts from the cache (still
    // unread), so no false "read" state is left behind.
    let defer_seen = !summary.flags.seen && mark_seen != Some(false);
    if defer_seen {
        spawn_mark_seen(app, account.clone(), folder.clone(), uid, summary.uidvalidity);
    }

    let (html, blocked) = match rendered {
        Some(s) => (Some(s.html), s.blocked_remote_count),
        None => (None, 0),
    };
    let references = (!parsed.headers.references.is_empty()).then(|| parsed.headers.references.join(" "));
    let out = MessageBody {
        summary: WireSummary::from(&summary),
        text: parsed.text,
        html,
        blocked_remote_count: blocked,
        attachments: parsed
            .attachments
            .iter()
            .filter(|a| !a.inline)
            .map(|a| WireAttachment { id: a.id.clone(), name: a.name.clone(), mime: a.mime.clone(), size: a.size })
            .collect(),
        cc: parsed.headers.cc.iter().map(Address::from).collect(),
        reply_to: parsed.headers.reply_to.iter().map(Address::from).collect(),
        references,
    };
    crate::safety_cmds::perf::log(&format!(
        "perf: get_message: cached={cached} raw={}ms parse={}ms render={}ms total={}ms seen={}",
        t_raw.as_millis(),
        t_parse.as_millis(),
        t_render.as_millis(),
        t0.elapsed().as_millis(),
        if defer_seen { "deferred" } else { "none" }
    ));
    Ok(out)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SeenOutcome {
    /// The server took \Seen.
    Stored,
    /// The server refused the STORE (or its SELECT).
    Failed,
    /// The user left (iPhone), or a user flag change / sync made it moot: nothing changed.
    Skipped,
    /// Nothing was sent: the background turn or the connection did not come in time. Retried once.
    NotSent,
    /// The budget ran out with the command in flight; the STORE may or may not have reached the server.
    TimedOut,
}

impl SeenOutcome {
    fn as_str(self) -> &'static str {
        match self {
            SeenOutcome::Stored => "ok",
            SeenOutcome::Failed => "failed",
            SeenOutcome::Skipped => "skipped",
            SeenOutcome::NotSent => "not_sent",
            SeenOutcome::TimedOut => "timeout",
        }
    }
}

/// One mark-seen attempt: wait for the background turn ([`MARK_SEEN_TURN_BUDGET`]), then connect + SELECT + STORE on
/// the interactive lane within [`MARK_SEEN_BUDGET`].
async fn mark_seen_attempt(state: &AppState, acc: &str, fol: &str, uid: u32, validity: u32, k: &mut Option<mailcore::ImapTimings>, turn_ms: &mut u128) -> SeenOutcome {
    if !crate::presence::user_present() {
        return SeenOutcome::Skipped;
    }
    let tw = std::time::Instant::now();
    let turn = tokio::time::timeout(MARK_SEEN_TURN_BUDGET, state.background_turn(acc)).await;
    *turn_ms = tw.elapsed().as_millis();
    let Ok(_turn) = turn else { return SeenOutcome::NotSent };
    // The user may have left meanwhile: change nothing then.
    if !crate::presence::user_present() {
        return SeenOutcome::Skipped;
    }
    let mut sent = false;
    let r = tokio::time::timeout(MARK_SEEN_BUDGET, async {
        let Ok(mut ic) = state.interactive(acc).await else { return SeenOutcome::NotSent };
        if !crate::presence::user_present() {
            ic.finish(false);
            return SeenOutcome::Skipped;
        }
        sent = true;
        let r = ic.client.set_flag(fol, uid, Some(validity), FlagChange::Seen(true)).await;
        *k = Some(ic.client.timings);
        let ok = r.is_ok();
        ic.finish(ok);
        if ok { SeenOutcome::Stored } else { SeenOutcome::Failed }
    })
    .await;
    match r {
        Ok(o) => o,
        Err(_) if sent => SeenOutcome::TimedOut,
        Err(_) => SeenOutcome::NotSent,
    }
}

/// Deferred \Seen for a message just shown. Runs on the interactive session (warm, usually the same folder) while
/// holding the background lane's turn, so a folder sync that read the flags before this STORE cannot write "unseen"
/// back after it. The turn is taken first and the lane second (nothing takes them the other way round), so the
/// next open is not held up while this waits for a sync. When nothing could be sent (busy background lane, no
/// connection) it tries once more after [`MARK_SEEN_RETRY_AFTER`], unless the user changed a flag meanwhile.
fn spawn_mark_seen(app: AppHandle, acc: String, fol: String, uid: u32, validity: u32) {
    tauri::async_runtime::spawn(async move {
        let t = std::time::Instant::now();
        let state = app.state::<AppState>();
        let epoch = state.flag_epoch();
        let mut k = None;
        let mut turn_ms = 0u128;
        let mut outcome = mark_seen_attempt(&state, &acc, &fol, uid, validity, &mut k, &mut turn_ms).await;
        let mut retried = false;
        if outcome == SeenOutcome::NotSent {
            tokio::time::sleep(MARK_SEEN_RETRY_AFTER).await;
            let still_unread = matches!(state.storage.get_message(&acc, &fol, uid), Ok(Some(m)) if m.uidvalidity == validity && !m.flags.seen);
            outcome = if state.flag_epoch() == epoch && still_unread {
                retried = true;
                mark_seen_attempt(&state, &acc, &fol, uid, validity, &mut k, &mut turn_ms).await
            } else {
                SeenOutcome::Skipped
            };
        }
        // Outside every budget: once the server has taken \Seen the cache follows, and no timeout can cut in between.
        // Re-read and set only `seen`, so a flag change made meanwhile (e.g. flagged) is not overwritten.
        if outcome == SeenOutcome::Stored {
            if let Ok(Some(cur)) = state.storage.get_message(&acc, &fol, uid) {
                if cur.uidvalidity == validity && !cur.flags.seen {
                    let mut f = cur.flags;
                    f.seen = true;
                    let _ = state.storage.update_flags(&acc, &fol, uid, f);
                }
            }
        } else {
            // Not marked (or unknown after a timeout): the UI reloads the row from the cache, which still says unread;
            // the next sync of the folder brings the server's flags.
            use tauri::Emitter;
            let _ = app.emit("mail://synced", SyncedEvent { account: acc.clone(), folders: vec![fol.clone()] });
        }
        crate::safety_cmds::perf::log(&format!(
            "perf: mark_seen: outcome={} retried={retried} lane=interactive turn_wait={turn_ms}ms select={}ms store={}ms took={}ms",
            outcome.as_str(),
            k.map_or(0, |k| k.select_ms),
            k.map_or(0, |k| k.store_ms),
            t.elapsed().as_millis()
        ));
    });
}

/// Budget of one mark-seen exchange (connect + select + store), after the background turn was obtained.
const MARK_SEEN_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
/// How long a mark-seen waits for the background lane's turn (a long folder sync or export may hold it).
const MARK_SEEN_TURN_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);
/// Pause before the single retry of a mark-seen that sent nothing.
const MARK_SEEN_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// Second step of opening a message: tracker analysis of the remote images, the calendar invite and, when images are
/// allowed with trackers skipped, the HTML re-rendered with the tracker deny-set. Local only: it reads the body
/// `get_message` just cached (never the network), so it may also run while the iPhone user is away.
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri-injected State params + flat wire contract
pub async fn message_analysis(
    state: State<'_, AppState>,
    cal: State<'_, crate::cal_state::CalState>,
    account: String,
    folder: String,
    uid: u32,
    allow_remote_images: bool,
    skip_trackers: Option<bool>,
    uidvalidity: Option<u32>,
) -> CmdResult<MessageAnalysis> {
    let t0 = std::time::Instant::now();
    // The page passes the shown message's UIDVALIDITY: a renumbered folder never lets this check another message.
    check_original(&state, &account, &folder, uid, uidvalidity)?;
    // Local first; a body that is neither cached nor in memory any more is fetched again over the interactive lane
    // (only while the user is present), so its links never stay off for good.
    let raw = reader_raw(&state, &account, &folder, uid).await?;
    let skip = allow_remote_images && skip_trackers.unwrap_or(true);
    let acc = account.clone();
    let (raw, attachments, images, rendered) = tauri::async_runtime::spawn_blocking(move || -> CmdResult<_> {
        let parsed = parse_message(&raw).map_err(err)?;
        let mut recipients: Vec<String> = vec![acc];
        recipients.extend(parsed.headers.to.iter().chain(parsed.headers.cc.iter()).map(|a| a.address.clone()));
        let sender_domain = parsed.headers.from.first().and_then(|a| a.address.rsplit_once('@').map(|(_, d)| d.to_string()));
        // Static, advisory analysis of the RAW html (independent of what gets loaded).
        let images: Vec<WireImage> = parsed
            .html
            .as_deref()
            .map(|h| mailcore::analyze_remote_images_from(h, &recipients, sender_domain.as_deref()))
            .unwrap_or_default();
        let rendered = match (&parsed.html, skip) {
            (Some(h), true) => {
                let deny = mailcore::tracker_url_set(h, &recipients);
                render_html_with(&raw, true, &deny).map_err(err)?
            }
            _ => None,
        };
        Ok((raw, parsed.attachments, images, rendered))
    })
    .await
    .map_err(|e| format!("analysis stopped: {e}"))??;
    let (html, blocked) = match rendered {
        Some(s) => (Some(s.html), s.blocked_remote_count),
        None => (None, 0),
    };
    let invite = crate::cal_invite::invite_for_message(&cal, &raw, &attachments, &account);
    let out = MessageAnalysis { images, invite, html, blocked_remote_count: blocked };
    crate::safety_cmds::perf::log(&format!(
        "perf: message_analysis took={}ms images={} bytes={} rerender={}",
        t0.elapsed().as_millis(),
        out.images.len(),
        raw.len(),
        out.html.is_some()
    ));
    Ok(out)
}

/// (name, mime, bytes) of one attachment.
pub(crate) async fn load_attachment(
    state: &AppState,
    account: &str,
    folder: &str,
    uid: u32,
    id: &str,
    expected_validity: Option<u32>,
) -> CmdResult<(String, String, Vec<u8>)> {
    check_original(state, account, folder, uid, expected_validity)?;
    // User-initiated: the interactive lane (never behind background syncs).
    let raw = reader_raw(state, account, folder, uid).await?;
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
    state.note_flag_change(); // a pending mark-seen retry must not undo this
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
pub fn set_sync_settings(app: tauri::AppHandle, state: State<'_, AppState>, settings: SyncSettings) -> CmdResult<SyncSettings> {
    settings.validate()?;
    let prev = state.sync_cfg.send_replace(settings);
    if let Err(e) = state.persist_config() {
        state.sync_cfg.send_replace(prev);
        return Err(e);
    }
    // "Download new mail in advance" was just turned on: catch up now instead of at the next new mail.
    if settings.prefetch && !prev.prefetch {
        crate::prefetch::kick_all(&app);
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

    /// The Reader's checks pass the shown message's UIDVALIDITY: another one (folder renumbered) is refused, so a
    /// check can never validate a different message; a body kept in memory needs the same UIDVALIDITY as well.
    #[test]
    fn checks_refuse_another_uidvalidity() {
        use mailcore::{Flags, MessageSummary};
        let st = AppState::new(std::sync::Arc::new(mailcore::Storage::open_in_memory().unwrap()), std::sync::Arc::new(mailcore::InMemorySecretStore::new()));
        let acc = AccountConfig {
            email: "a@x.org".into(),
            imap: mailcore::ServerConfig { host: "i".into(), port: 993, tls: mailcore::Tls::Implicit },
            smtp: mailcore::ServerConfig { host: "s".into(), port: 465, tls: mailcore::Tls::Implicit },
            username: "a@x.org".into(),
        };
        st.storage.upsert_account(&acc).unwrap();
        st.storage
            .replace_folders("a@x.org", &[Folder { name: "INBOX".into(), delimiter: Some("/".into()), role: FolderRole::Inbox, selectable: true }])
            .unwrap();
        st.storage
            .upsert_message(&MessageSummary {
                account: "a@x.org".into(),
                folder: "INBOX".into(),
                uid: 5,
                uidvalidity: 9,
                message_id: None,
                subject: String::new(),
                from: String::new(),
                to: String::new(),
                date: 1,
                flags: Flags::default(),
                snippet: String::new(),
                has_body: false,
                size: 1,
            })
            .unwrap();
        assert!(check_original(&st, "a@x.org", "INBOX", 5, Some(9)).is_ok());
        assert_eq!(check_original(&st, "a@x.org", "INBOX", 5, Some(10)).unwrap_err(), ORIGINAL_GONE);
        assert_eq!(check_original(&st, "a@x.org", "Archive", 5, Some(9)).unwrap_err(), ORIGINAL_GONE, "another folder's uid 5");
        assert!(check_original(&st, "a@x.org", "INBOX", 5, None).is_ok(), "older callers without it");
    }

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
