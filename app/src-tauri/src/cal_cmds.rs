//! Tauri commands for the calendar and Zoom (contract: docs/CALENDAR_SPEC.md, docs/APP_API.md).
//! Thin wrappers: the logic lives in `cal_logic` (testable) and `cal_invite`.

use crate::cal_invite;
use crate::cal_logic::{self, SyncOutcome};
use crate::cal_sched::changed;
use crate::cal_state::{CalState, RealCalDav, RealZoom};
use crate::commands::raw_message;
use crate::state::{err, AppState};
use calendar::{AttendeeStatus, CalEvent, CalendarInfo, Occurrence, ZoomMeeting};
use calsync::zoom::CALLBACK_TIMEOUT;
use calsync::ZoomStatus;
use mailcore::OutgoingMessage;
use serde::Serialize;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, State};

type CmdResult<T> = Result<T, String>;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct SyncCounts {
    pub calendars: usize,
    pub events: usize,
}

/// Reduce a sync pass to the wire counts; an error only when every attempted calendar failed.
pub fn sync_result(o: SyncOutcome) -> CmdResult<SyncCounts> {
    if o.attempted > 0 && o.calendars == 0 && !o.errors.is_empty() {
        return Err(o.errors.join("\n"));
    }
    Ok(SyncCounts { calendars: o.calendars, events: o.events })
}

pub(crate) fn my_emails(state: &AppState) -> Vec<String> {
    state.storage.list_accounts().unwrap_or_default().into_iter().map(|a| a.email).collect()
}

/// Send an iTIP mail by SMTP from the account `from_hint` (if it is one of ours), else the first account.
pub(crate) async fn send_itip(state: &AppState, from_hint: Option<&str>, msg: OutgoingMessage) -> CmdResult<()> {
    let accounts = state.storage.list_accounts().map_err(err)?;
    let cfg = from_hint
        .and_then(|h| accounts.iter().find(|a| a.email.eq_ignore_ascii_case(h)))
        .or_else(|| accounts.first())
        .ok_or("Add a mail account first to send invitations.")?;
    let password = state.password(&cfg.email)?;
    mailcore::smtp::send(cfg, &password, &msg).await.map(|_| ()).map_err(err)
}

#[tauri::command]
pub fn cal_list_calendars(cal: State<'_, CalState>) -> CmdResult<Vec<CalendarInfo>> {
    cal.db(|c| c.list_calendars())
}

#[tauri::command]
pub fn cal_create_local(app: AppHandle, cal: State<'_, CalState>, name: String, color: String) -> CmdResult<CalendarInfo> {
    let r = cal_logic::create_local(&cal, &name, &color)?;
    changed(&app);
    Ok(r)
}

/// Prefill for the Add-calendar form, inferred from the user's mail account (its IMAP/SMTP hosts).
/// `account` = the mail address; default the first account.
#[tauri::command]
pub fn cal_suggest_caldav(state: State<'_, AppState>, account: Option<String>) -> CmdResult<cal_logic::CaldavSuggestion> {
    let accounts = state.storage.list_accounts().map_err(err)?;
    let acc = account.as_deref().and_then(|a| accounts.iter().find(|x| x.email.eq_ignore_ascii_case(a))).or_else(|| accounts.first());
    Ok(match acc {
        Some(a) => cal_logic::suggest_caldav(&a.email, &[a.imap.host.as_str(), a.smtp.host.as_str()]),
        None => cal_logic::suggest_caldav("", &[]),
    })
}

/// Saved mail accounts at the same provider as `preset` (`zoho` / `icloud` / `fastmail`): the Add-calendar
/// form offers "Use my mail login" only for these. Carries no secret.
#[tauri::command]
pub fn cal_reusable_accounts(state: State<'_, AppState>, preset: String) -> CmdResult<Vec<cal_logic::ReusableAccount>> {
    let accounts = state.storage.list_accounts().map_err(err)?;
    Ok(cal_logic::reusable_accounts(&accounts, &preset))
}

/// Connect a CalDAV server. Progress steps are emitted as `cal://add-progress` (a string).
/// On failure the error is `{ message, diagnostics }` (plain words + a non-secret trail).
/// With `reuse_mail_account` (a saved mail address) the mail password is read here from the secret store and
/// `username`/`password`/`url` are ignored; the password never reaches the webview.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cal_add_caldav(
    app: AppHandle,
    cal: State<'_, CalState>,
    state: State<'_, AppState>,
    preset: String,
    url: Option<String>,
    region: Option<String>,
    username: Option<String>,
    password: Option<String>,
    reuse_mail_account: Option<String>,
) -> Result<Vec<CalendarInfo>, cal_logic::AddError> {
    use tauri::Emitter;
    let a2 = app.clone();
    let diag = calsync::Diag::new().with_progress(move |s| {
        let _ = a2.emit("cal://add-progress", s.to_string());
    });
    let r = match reuse_mail_account.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(mail) => {
            let accounts = state.storage.list_accounts().map_err(|e| cal_logic::AddError { message: err(e), diagnostics: vec![] })?;
            cal_logic::add_caldav_reuse(&cal, state.secrets.as_ref(), &accounts, &RealCalDav, &preset, region.as_deref(), mail, &diag).await?
        }
        None => {
            cal_logic::add_caldav_ex(&cal, &RealCalDav, &preset, url.as_deref(), region.as_deref(), username.as_deref().unwrap_or_default(), password.as_deref().unwrap_or_default(), &diag)
                .await?
        }
    };
    changed(&app);
    // Pull the new calendars' events in the background; the UI refreshes on `cal://changed`.
    let a = app.clone();
    tauri::async_runtime::spawn(async move { crate::cal_sched::sync_tick(&a).await });
    Ok(r)
}

#[tauri::command]
pub fn cal_update_calendar(
    app: AppHandle,
    cal: State<'_, CalState>,
    id: String,
    name: Option<String>,
    color: Option<String>,
    visible: Option<bool>,
) -> CmdResult<()> {
    cal_logic::update_calendar(&cal, &id, name, color, visible)?;
    changed(&app);
    Ok(())
}

#[tauri::command]
pub async fn cal_remove_calendar(app: AppHandle, cal: State<'_, CalState>, id: String) -> CmdResult<()> {
    cal_logic::remove_calendar(&cal, &id).await?;
    changed(&app);
    Ok(())
}

#[tauri::command]
pub async fn cal_sync(app: AppHandle, cal: State<'_, CalState>, calendar_id: Option<String>) -> CmdResult<SyncCounts> {
    let out = cal_logic::sync_calendars(&cal, &RealCalDav, calendar_id.as_deref()).await?;
    if out.attempted > 0 {
        changed(&app);
    }
    sync_result(out)
}

#[tauri::command]
pub fn cal_list_events(cal: State<'_, CalState>, start: String, end: String) -> CmdResult<Vec<Occurrence>> {
    cal.db(|c| c.list_occurrences(&start, &end, true))
}

#[tauri::command]
pub fn cal_get_event(cal: State<'_, CalState>, uid: String, calendar_id: String) -> CmdResult<CalEvent> {
    cal.db(|c| c.get_event(&uid, &calendar_id))?.ok_or_else(|| "That event no longer exists.".to_string())
}

#[tauri::command]
pub async fn cal_save_event(
    app: AppHandle,
    cal: State<'_, CalState>,
    state: State<'_, AppState>,
    event: CalEvent,
    send_invites: bool,
) -> CmdResult<CalEvent> {
    let mine = my_emails(&state);
    let zoom = RealZoom(cal.zoom_client()?);
    let res = cal_logic::save_event(&cal, &RealCalDav, &zoom, event, send_invites, &mine).await;
    changed(&app); // also after a conflict: the server's version replaced the local one
    let out = res?;
    if let Some(mail) = out.mail {
        // The invitation goes out from the organizer's own account, whatever happened with the CalDAV push.
        let from = out.event.organizer.as_ref().map(|o| o.email.clone());
        if let Err(e) = send_itip(&state, from.as_deref(), mail).await {
            return Err(format!("The event was saved, but the invitations were NOT sent: {e}"));
        }
        if let Some(seq) = out.pending_sequence {
            cal_logic::commit_sequence(&cal, &RealCalDav, &out.event.uid, &out.event.calendar_id, seq).await;
            changed(&app);
        }
        if let Some(e) = out.push_error {
            return Err(format!("The invitations were sent. {e}"));
        }
    }
    Ok(out.event)
}

#[tauri::command]
pub async fn cal_delete_event(
    app: AppHandle,
    cal: State<'_, CalState>,
    state: State<'_, AppState>,
    uid: String,
    calendar_id: String,
    notify_attendees: bool,
) -> CmdResult<()> {
    let mine = my_emails(&state);
    let zoom = RealZoom(cal.zoom_client()?);
    let res = cal_logic::delete_event(&cal, &RealCalDav, &zoom, &uid, &calendar_id, notify_attendees, &mine).await;
    changed(&app);
    let out = res?;
    let mut warning = out.warning;
    if let Some(mail) = out.mail {
        if let Err(e) = send_itip(&state, out.from.as_deref(), mail).await {
            warning.get_or_insert(format!("The event was deleted, but the cancellation could not be sent: {e}"));
        }
    }
    warning.map_or(Ok(()), Err)
}

pub(crate) async fn read_message(state: &AppState, account: &str, folder: &str, uid: u32) -> CmdResult<Vec<u8>> {
    let mut conn = None;
    let raw = raw_message(state, &mut conn, account, folder, uid).await;
    if let Some(c) = conn {
        c.close().await;
    }
    raw
}

// The argument list is the frontend contract (docs/CALENDAR_SPEC.md).
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cal_import_invite(
    app: AppHandle,
    cal: State<'_, CalState>,
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: u32,
    attachment_id: String,
    confirm_unverified: Option<bool>,
) -> CmdResult<CalEvent> {
    let raw = read_message(&state, &account, &folder, uid).await?;
    let (inv, sender) = cal_invite::load_invite(&cal, &raw, &account, &attachment_id)?;
    let mine = my_emails(&state);
    let r = cal_logic::import_invite(&cal, &RealCalDav, &inv, &sender, &mine, confirm_unverified.unwrap_or(false)).await;
    changed(&app);
    r
}

// The argument list is the frontend contract (docs/CALENDAR_SPEC.md).
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn cal_respond_invite(
    app: AppHandle,
    cal: State<'_, CalState>,
    state: State<'_, AppState>,
    account: String,
    folder: String,
    uid: u32,
    attachment_id: String,
    status: AttendeeStatus,
    confirm_unverified: Option<bool>,
) -> CmdResult<()> {
    if status == AttendeeStatus::NeedsAction {
        return Err("Choose accept, tentative or decline.".into());
    }
    let raw = read_message(&state, &account, &folder, uid).await?;
    let (inv, sender) = cal_invite::load_invite(&cal, &raw, &account, &attachment_id)?;
    let key = cal_logic::reply_key(&inv, &account, status);
    let mine = my_emails(&state);
    let r = cal_logic::respond_invite(&cal, &RealCalDav, inv, &sender, &account, status, &mine, confirm_unverified.unwrap_or(false)).await;
    changed(&app);
    let (_ev, mail) = r?;
    send_itip(&state, Some(&account), mail).await.map_err(|e| {
        cal.forget_reply(&key); // not sent: the user may try again
        format!("Your answer was saved, but the reply could not be sent: {e}")
    })
}

// ---- Zoom ------------------------------------------------------------------------------------

fn zerr(e: calsync::ZoomError) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn zoom_status(cal: State<'_, CalState>) -> CmdResult<ZoomStatus> {
    cal.zoom_client()?.status().await.map_err(zerr)
}

#[tauri::command]
pub fn zoom_set_client(cal: State<'_, CalState>, client_id: String, client_secret: Option<String>) -> CmdResult<()> {
    cal_logic::zoom_set_client(&cal, &client_id, client_secret.as_deref())
}

struct ConnectGuard<'a>(&'a std::sync::atomic::AtomicBool);
impl Drop for ConnectGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Open the system browser for the Zoom sign-in and wait (<= 3 minutes) for the loopback callback.
/// Async, so the UI thread is never blocked.
#[tauri::command]
pub async fn zoom_connect(app: AppHandle, cal: State<'_, CalState>) -> CmdResult<ZoomStatus> {
    if cal.zoom_connecting.swap(true, Ordering::SeqCst) {
        return Err("A Zoom sign-in is already in progress. Finish it in your browser, or wait for it to time out.".into());
    }
    let _guard = ConnectGuard(&cal.zoom_connecting);
    let client = cal.zoom_client()?;
    let redirect = client.redirect_uri();
    let session = client.begin_authorize().map_err(|e| cal_logic::zoom_connect_error(&e, &redirect))?;
    crate::native::open_url(&app, &session.url).await.map_err(|_| "Couldn't open your browser for the Zoom sign-in.".to_string())?;
    client.wait_for_callback(session, CALLBACK_TIMEOUT).await.map_err(|e| cal_logic::zoom_connect_error(&e, &redirect))?;
    client.status().await.map_err(zerr)
}

#[tauri::command]
pub async fn zoom_disconnect(app: AppHandle, cal: State<'_, CalState>) -> CmdResult<()> {
    cal.zoom_client()?.disconnect().await.map_err(zerr)?;
    changed(&app);
    Ok(())
}

#[tauri::command]
pub async fn zoom_create_meeting(
    cal: State<'_, CalState>,
    topic: String,
    start: String,
    duration_minutes: u32,
    timezone: String,
    agenda: Option<String>,
) -> CmdResult<ZoomMeeting> {
    let m = cal.zoom_client()?.create_meeting(&topic, &start, duration_minutes, &timezone, agenda.as_deref()).await.map_err(zerr)?;
    // Only meetings recorded here can later be updated or deleted by this app.
    cal.zoom_record(&m.meeting_id)?;
    Ok(m)
}

#[tauri::command]
pub async fn zoom_delete_meeting(cal: State<'_, CalState>, meeting_id: String) -> CmdResult<()> {
    let zoom = RealZoom(cal.zoom_client()?);
    cal_logic::zoom_delete_recorded(&cal, &zoom, &meeting_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_result_counts_and_errors() {
        let ok = SyncOutcome { calendars: 2, events: 5, attempted: 3, errors: vec!["x".into()] };
        assert_eq!(sync_result(ok).unwrap(), SyncCounts { calendars: 2, events: 5 });
        let none = SyncOutcome::default();
        assert_eq!(sync_result(none).unwrap(), SyncCounts { calendars: 0, events: 0 });
        let all_failed = SyncOutcome { calendars: 0, events: 0, attempted: 1, errors: vec!["Work: Wrong password".into()] };
        assert!(sync_result(all_failed).unwrap_err().contains("Wrong password"));
    }

    #[test]
    fn sync_counts_wire_shape() {
        let v = serde_json::to_value(SyncCounts { calendars: 1, events: 2 }).unwrap();
        assert_eq!(v["calendars"], 1);
        assert_eq!(v["events"], 2);
    }
}
