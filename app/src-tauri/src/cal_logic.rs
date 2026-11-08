//! Calendar command logic, independent of Tauri: everything here takes the calendar state plus the
//! network seams (`CalDavRemote`, `ZoomRemote`) so it can be tested with an in-memory store and fakes.
//! Outgoing iTIP mails are returned as `OutgoingMessage`s; the command layer sends them over SMTP.

use crate::cal_state::{caldav_key, CalDavRemote, CalState, ZoomRemote};
use calendar::itip;
use calendar::{
    new_event_uid, new_id, Attendee, AttendeeRole, AttendeeStatus, CalEvent, CalendarInfo, CalendarKind, CalendarPatch, EventOverride, Invite,
    InviteMethod, NewCalendar, DEFAULT_CALENDAR_ID,
};
use calsync::caldav::PushOutcome;
use mailcore::{CalendarPart, OutgoingMessage};

pub const PALETTE: [&str; 8] = ["#4f8cff", "#e5484d", "#30a46c", "#f5a524", "#8e4ec6", "#12a594", "#e93d82", "#6e56cf"];

const MSG_NO_PASSWORD: &str = "No saved password for this calendar. Remove it and add it again.";
const MSG_READ_ONLY: &str = "This calendar is read-only.";
const MSG_SIGNIN_FAILED: &str = "The server rejected the saved password. Remove this calendar and add it again with the current password.";
const MSG_CONFLICT: &str =
    "This event was changed on the server at the same time, so the server's version was kept. Open it again to re-apply your edits.";

pub fn valid_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

// ---------------------------------------------------------------------------------------------
// Calendars
// ---------------------------------------------------------------------------------------------

pub fn create_local(st: &CalState, name: &str, color: &str) -> Result<CalendarInfo, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the calendar a name.".into());
    }
    if !valid_color(color) {
        return Err("Pick a valid colour.".into());
    }
    st.db(|c| c.create_local(name, color))
}

pub fn update_calendar(st: &CalState, id: &str, name: Option<String>, color: Option<String>, visible: Option<bool>) -> Result<(), String> {
    if name.as_deref().is_some_and(|n| n.trim().is_empty()) {
        return Err("Give the calendar a name.".into());
    }
    if color.as_deref().is_some_and(|c| !valid_color(c)) {
        return Err("Pick a valid colour.".into());
    }
    let patch = CalendarPatch { name: name.map(|n| n.trim().to_string()), color, visible, ..Default::default() };
    st.db(|c| c.update_calendar(id, &patch))
}

pub async fn remove_calendar(st: &CalState, id: &str) -> Result<(), String> {
    // Not while this calendar is being synced or written to.
    let _lock = st.cal_lock(id).lock_owned().await;
    let cal = st.calendar(id)?;
    st.db(|c| {
        c.delete_calendar(id)?;
        // calsync's pending-change / owned bookkeeping and the sign-in marker for this calendar
        calsync::caldav::forget_calendar(c, id)?;
        c.delete_setting(&crate::cal_state::signin_failed_key(id))
    })?;
    if cal.kind == CalendarKind::Caldav {
        let _ = st.secrets.delete(&caldav_key(id));
    }
    Ok(())
}

fn server_url(preset: &str, url: Option<&str>, username: &str, region: Option<&str>) -> Result<String, String> {
    if preset == "custom" {
        let u = url.map(str::trim).unwrap_or_default();
        if u.is_empty() {
            return Err("Enter the calendar server address.".into());
        }
        return Ok(if u.contains("://") { u.to_string() } else { format!("https://{u}") });
    }
    if preset == "zoho" && region.is_some_and(|r| !r.trim().is_empty() && !calsync::caldav::valid_zoho_region(&r.trim().to_ascii_lowercase())) {
        return Err("Unknown Zoho data centre. Pick the region your Zoho account lives in.".into());
    }
    calsync::caldav::preset_url_region(preset, username, region).ok_or_else(|| "Unknown calendar provider.".to_string())
}

/// What the Add-calendar form is prefilled with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CaldavSuggestion {
    pub preset: String,
    pub region: Option<String>,
    pub url: Option<String>,
    pub username: String,
}

/// Guess the calendar provider from a mail account: Zoho hosts (any data centre) -> `zoho` + region;
/// iCloud / Fastmail hosts likewise; otherwise `custom` with no URL. Pure; `hosts` are the IMAP/SMTP hosts.
pub fn suggest_caldav(email: &str, hosts: &[&str]) -> CaldavSuggestion {
    let username = email.trim().to_string();
    for h in hosts {
        if let Some(r) = calsync::caldav::zoho_region_from_host(h) {
            return CaldavSuggestion { preset: "zoho".into(), region: Some(r.into()), url: calsync::caldav::preset_url_region("zoho", &username, Some(r)), username };
        }
    }
    let dom = |h: &str, d: &str| {
        let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
        h == d || h.ends_with(&format!(".{d}"))
    };
    for (preset, domains) in [("icloud", &["icloud.com", "me.com", "mac.com"][..]), ("fastmail", &["fastmail.com", "messagingengine.com"][..])] {
        if hosts.iter().any(|h| domains.iter().any(|d| dom(h, d))) {
            return CaldavSuggestion { preset: preset.into(), region: None, url: calsync::caldav::preset_url(preset, &username), username };
        }
    }
    // Zoho custom-domain mailboxes still use Zoho's servers; the email's own domain tells nothing, so fall back to custom.
    CaldavSuggestion { preset: "custom".into(), region: None, url: None, username }
}

/// A failed "add CalDAV calendar": a plain-words message plus a non-secret trail for "Copy details".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AddError {
    pub message: String,
    pub diagnostics: Vec<String>,
}

fn zoho_hint(preset: &str) -> &'static str {
    if preset == "zoho" {
        " In Zoho Calendar, open Settings > Synchronize > CalDAV and tick the calendars you want to sync (your primary calendar is on by default), then try again."
    } else {
        ""
    }
}

/// Discover first; only after success store the password (Keychain) and create the calendar rows.
#[cfg(test)]
pub async fn add_caldav<R: CalDavRemote>(
    st: &CalState,
    r: &R,
    preset: &str,
    url: Option<&str>,
    username: &str,
    password: &str,
) -> Result<Vec<CalendarInfo>, String> {
    add_caldav_ex(st, r, preset, url, None, username, password, &calsync::Diag::new()).await.map_err(|e| e.message)
}

/// [`add_caldav`] with a Zoho data-centre `region` and a diagnostics sink (step progress + non-secret trail).
#[allow(clippy::too_many_arguments)]
pub async fn add_caldav_ex<R: CalDavRemote>(
    st: &CalState,
    r: &R,
    preset: &str,
    url: Option<&str>,
    region: Option<&str>,
    username: &str,
    password: &str,
    diag: &calsync::Diag,
) -> Result<Vec<CalendarInfo>, AddError> {
    add_caldav_with(st, r, preset, url, region, username, password, diag, false).await
}

/// Shown instead of the generic 401 text when the password came from the user's mail account.
pub const MSG_REUSED_401: &str = "The server rejected your mail password for the calendar. Your mail password may be an app-specific password that only works for mail. Enter a different login below (for example a password that allows calendars/CalDAV) and try again.";

/// Provider (`zoho` / `icloud` / `fastmail`) a mail account belongs to, from its IMAP/SMTP hosts.
pub fn provider_for_hosts(hosts: &[&str]) -> Option<&'static str> {
    if hosts.iter().any(|h| calsync::caldav::zoho_region_from_host(h).is_some()) {
        return Some("zoho");
    }
    let dom = |h: &str, d: &str| {
        let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
        h == d || h.ends_with(&format!(".{d}"))
    };
    [("icloud", &["icloud.com", "me.com", "mac.com"][..]), ("fastmail", &["fastmail.com", "messagingengine.com"][..])]
        .into_iter()
        .find(|(_, domains)| hosts.iter().any(|h| domains.iter().any(|d| dom(h, d))))
        .map(|(p, _)| p)
}

fn account_provider(a: &mailcore::AccountConfig) -> Option<&'static str> {
    provider_for_hosts(&[a.imap.host.as_str(), a.smtp.host.as_str()])
}

/// A saved mail account whose login can be offered for the calendar (wire shape `{email, provider}`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReusableAccount {
    pub email: String,
    pub provider: String,
}

/// Saved mail accounts at the same provider as `preset` (never `custom`). No secrets involved.
pub fn reusable_accounts(accounts: &[mailcore::AccountConfig], preset: &str) -> Vec<ReusableAccount> {
    accounts
        .iter()
        .filter(|a| account_provider(a).is_some_and(|p| p == preset))
        .map(|a| ReusableAccount { email: a.email.clone(), provider: preset.to_string() })
        .collect()
}

/// Connect a CalDAV server with the password of a saved mail account at the same provider.
/// The password is read here (never sent to the webview, never put in an error) and, as for any
/// calendar, stored under `caldav:<id>` only after discovery succeeded.
#[allow(clippy::too_many_arguments)]
pub async fn add_caldav_reuse<R: CalDavRemote>(
    st: &CalState,
    mail_secrets: &dyn mailcore::SecretStore,
    accounts: &[mailcore::AccountConfig],
    r: &R,
    preset: &str,
    region: Option<&str>,
    reuse: &str,
    diag: &calsync::Diag,
) -> Result<Vec<CalendarInfo>, AddError> {
    let plain = |m: &str| AddError { message: m.to_string(), diagnostics: diag.lines() };
    let acc = accounts
        .iter()
        .find(|a| a.email.eq_ignore_ascii_case(reuse.trim()))
        .ok_or_else(|| plain("That mail account is not one of your saved accounts."))?;
    if preset == "custom" || account_provider(acc) != Some(preset) {
        return Err(plain("That mail account is at a different provider than this calendar server. Enter the login for the calendar instead."));
    }
    let password = match mail_secrets.get(&acc.email) {
        Ok(Some(p)) if !p.is_empty() => p,
        _ => return Err(plain("No saved password for that mail account. Enter the login for the calendar instead.")),
    };
    let username = if acc.username.contains('@') { acc.username.clone() } else { acc.email.clone() };
    let inferred = if preset == "zoho" && region.is_none_or(|x| x.trim().is_empty()) {
        [acc.imap.host.as_str(), acc.smtp.host.as_str()].into_iter().find_map(calsync::caldav::zoho_region_from_host)
    } else {
        None
    };
    add_caldav_with(st, r, preset, None, region.or(inferred), &username, &password, diag, true).await
}

#[allow(clippy::too_many_arguments)]
async fn add_caldav_with<R: CalDavRemote>(
    st: &CalState,
    r: &R,
    preset: &str,
    url: Option<&str>,
    region: Option<&str>,
    username: &str,
    password: &str,
    diag: &calsync::Diag,
    reused: bool,
) -> Result<Vec<CalendarInfo>, AddError> {
    let plain = |m: &str| AddError { message: m.to_string(), diagnostics: diag.lines() };
    let username = username.trim();
    if username.is_empty() {
        return Err(plain("Enter your username."));
    }
    if password.is_empty() {
        return Err(plain("Enter your password."));
    }
    let base = server_url(preset, url, username, region).map_err(|m| plain(&m))?;
    let fail = |e: &calsync::CalDavError| {
        let message = if reused && *e == calsync::CalDavError::Unauthorized {
            MSG_REUSED_401.to_string()
        } else if *e == calsync::CalDavError::NoCalendars {
            format!("{}{}", e, zoho_hint(preset))
        } else {
            format!("{}{}", diag.explain(e), if *e == calsync::CalDavError::NotFound { zoho_hint_404(preset) } else { "" })
        };
        AddError { message, diagnostics: diag.lines() }
    };
    let found = r.discover(&base, username, password, diag).await.map_err(|e| fail(&e))?;
    if found.is_empty() {
        return Err(fail(&calsync::CalDavError::NoCalendars));
    }
    add_found(st, found, username, password).map_err(|m| plain(&m))
}

fn zoho_hint_404(preset: &str) -> &'static str {
    if preset == "zoho" {
        " Zoho accounts live in one data centre (.com, .in, .eu, .com.au ...); choose the one you sign in to."
    } else {
        ""
    }
}

fn add_found(st: &CalState, found: Vec<calsync::DiscoveredCalendar>, username: &str, password: &str) -> Result<Vec<CalendarInfo>, String> {
    let existing = st.db(|c| c.list_calendars())?;
    let mut out: Vec<CalendarInfo> = Vec::new();
    let mut created: Vec<String> = Vec::new();
    for (i, d) in found.iter().enumerate() {
        if let Some(e) = existing
            .iter()
            .find(|e| e.kind == CalendarKind::Caldav && e.url.as_deref().is_some_and(|u| same(u, &d.url)) && e.username.as_deref() == Some(username))
        {
            st.secrets.set(&caldav_key(&e.id), password).map_err(|_| "Couldn't save the password to the Keychain.".to_string())?;
            st.clear_signin_failed(&e.id);
            let _ = calsync::caldav::set_owned(&st.store, &e.id, d.owned);
            out.push(e.clone());
            continue;
        }
        let id = new_id();
        let name = Some(d.name.trim()).filter(|n| !n.is_empty()).unwrap_or("Calendar").to_string();
        let color = d.color.clone().filter(|c| valid_color(c)).unwrap_or_else(|| PALETTE[(existing.len() + i) % PALETTE.len()].to_string());
        let made = st
            .secrets
            .set(&caldav_key(&id), password)
            .map_err(|_| "Couldn't save the password to the Keychain.".to_string())
            .and_then(|_| {
                created.push(id.clone());
                st.db(|c| {
                    calsync::caldav::forget_calendar(c, &id)?; // stale bookkeeping of a recycled id, if any
                    c.set_setting(&format!("calsync.owned.{id}"), if d.owned { "1" } else { "0" })?;
                    c.create_calendar(&NewCalendar {
                        id: Some(id.clone()),
                        name,
                        color,
                        kind: CalendarKind::Caldav,
                        url: Some(d.url.clone()),
                        username: Some(username.to_string()),
                        read_only: d.read_only,
                    })
                })
            });
        match made {
            Ok(info) => out.push(info),
            Err(e) => {
                for id in &created {
                    let _ = st.db(|c| c.delete_calendar(id));
                    let _ = st.secrets.delete(&caldav_key(id));
                }
                return Err(e);
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Calendars that synced successfully.
    pub calendars: usize,
    /// Events pulled + pushed + deleted.
    pub events: usize,
    /// Calendars a sync was attempted for.
    pub attempted: usize,
    pub errors: Vec<String>,
}

pub fn has_caldav(st: &CalState) -> bool {
    st.db(|c| c.list_calendars()).map(|v| v.iter().any(|c| c.kind == CalendarKind::Caldav)).unwrap_or(false)
}

/// Sync every CalDAV calendar (or just `only`). A calendar without a stored password is never contacted;
/// its error is recorded on the calendar. One failing calendar does not stop the others.
pub async fn sync_calendars<R: CalDavRemote>(st: &CalState, r: &R, only: Option<&str>) -> Result<SyncOutcome, String> {
    let _gate = st.sync_gate.lock().await;
    let mut cals = st.db(|c| c.list_calendars())?;
    if let Some(id) = only {
        cals.retain(|c| c.id == id);
        if cals.is_empty() {
            return Err("That calendar no longer exists.".into());
        }
    }
    let mut out = SyncOutcome::default();
    for cal in cals.iter().filter(|c| c.kind == CalendarKind::Caldav) {
        out.attempted += 1;
        // never touch a calendar while a save / delete / removal of it is in flight
        let _lock = st.cal_lock(&cal.id).lock_owned().await;
        if st.signin_failed(&cal.id) {
            // The server already rejected this password: do not keep sending it until the user re-enters it.
            let _ = st.db(|c| {
                let s = c.get_calendar_sync_state(&cal.id)?;
                c.set_calendar_sync_state(&cal.id, s.ctag.as_deref(), s.sync_marker.as_deref(), Some(MSG_SIGNIN_FAILED), s.last_sync)
            });
            out.errors.push(format!("{}: {MSG_SIGNIN_FAILED}", cal.name));
            continue;
        }
        let Some(pw) = st.caldav_password(&cal.id) else {
            let _ = st.db(|c| {
                let s = c.get_calendar_sync_state(&cal.id)?;
                c.set_calendar_sync_state(&cal.id, s.ctag.as_deref(), s.sync_marker.as_deref(), Some(MSG_NO_PASSWORD), s.last_sync)
            });
            out.errors.push(format!("{}: {MSG_NO_PASSWORD}", cal.name));
            continue;
        };
        match r.sync(&st.store, cal, &pw).await {
            Ok(s) => {
                out.calendars += 1;
                out.events += s.pulled + s.pushed + s.deleted;
            }
            // `sync_calendar` already recorded the error text on the calendar.
            Err(e) => {
                if e == calsync::CalDavError::Unauthorized {
                    st.set_signin_failed(&cal.id);
                }
                out.errors.push(format!("{}: {e}", cal.name));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Remote push helper
// ---------------------------------------------------------------------------------------------

/// What happened to the CalDAV push that follows a local write.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pushed {
    /// Pushed, or nothing to push (local calendar).
    Done,
    /// The server had a newer version; it replaced the local one.
    Conflict,
    /// Saved locally and queued for the next sync; the text says why.
    Queued(String),
}

async fn push_if_caldav<R: CalDavRemote>(st: &CalState, r: &R, cal: &CalendarInfo, uid: &str) -> Pushed {
    if cal.kind != CalendarKind::Caldav {
        return Pushed::Done;
    }
    if st.signin_failed(&cal.id) {
        let _ = calsync::caldav::mark_dirty(&st.store, &cal.id, uid);
        return Pushed::Queued(format!("Saved on this Mac, but not on the server. {MSG_SIGNIN_FAILED}"));
    }
    let Some(pw) = st.caldav_password(&cal.id) else {
        let _ = calsync::caldav::mark_dirty(&st.store, &cal.id, uid);
        return Pushed::Queued(format!("Saved on this Mac, but not on the server. {MSG_NO_PASSWORD}"));
    };
    match r.push(&st.store, cal, &pw, uid).await {
        Ok(PushOutcome::Pushed(_)) => Pushed::Done,
        Ok(PushOutcome::Conflict) => Pushed::Conflict,
        Err(e) => {
            if e == calsync::CalDavError::Unauthorized {
                st.set_signin_failed(&cal.id);
            }
            Pushed::Queued(format!("Saved on this Mac, but not on the calendar server: {e} It will sync later."))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// iTIP mail building
// ---------------------------------------------------------------------------------------------

fn itip_mail(to: Vec<String>, subject: String, text: String, method: &str, ics: String) -> OutgoingMessage {
    OutgoingMessage {
        to,
        cc: vec![],
        bcc: vec![],
        subject,
        text,
        html: None,
        attachments: vec![],
        in_reply_to: None,
        calendar: Some(CalendarPart { method: method.into(), ics }),
        extra_headers: vec![],
    }
}

/// REQUEST mail to every attendee that is not the user. `None` when there is nobody to invite.
pub fn build_request_mail(ev: &CalEvent, mine: &[String], schedule_agent_client: bool) -> Result<Option<OutgoingMessage>, String> {
    let to = itip::recipients(ev, mine);
    if to.is_empty() {
        return Ok(None);
    }
    let ics = itip::try_build_request(ev, mine, schedule_agent_client).map_err(crate::cal_state::cerr)?;
    Ok(Some(itip_mail(to, itip::request_subject(ev), itip::request_body(ev), "REQUEST", ics)))
}

pub fn build_cancel_mail(ev: &CalEvent, mine: &[String]) -> Result<Option<OutgoingMessage>, String> {
    let to = itip::recipients(ev, mine);
    if to.is_empty() {
        return Ok(None);
    }
    let ics = itip::try_build_cancel(ev, mine).map_err(crate::cal_state::cerr)?;
    Ok(Some(itip_mail(to, itip::cancel_subject(ev), itip::cancel_body(ev), "CANCEL", ics)))
}

pub fn build_reply_mail(ev: &CalEvent, me: &str, status: AttendeeStatus) -> Result<OutgoingMessage, String> {
    let org = ev.organizer.as_ref().ok_or("This invitation has no organizer to reply to.")?;
    let ics = itip::build_reply(ev, me, status).map_err(crate::cal_state::cerr)?;
    Ok(itip_mail(vec![org.email.clone()], itip::reply_subject(ev, status), itip::reply_body(ev, me, status), "REPLY", ics))
}

// ---------------------------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOutcome {
    pub event: CalEvent,
    pub mail: Option<OutgoingMessage>,
    /// The CalDAV push did not complete (event saved locally, queued). Reported to the user AFTER the invitation
    /// mail went out, so a server problem never swallows the invitations.
    pub push_error: Option<String>,
    /// SEQUENCE the stored event gets once `mail` was actually sent (`commit_sequence`). The mail already carries it;
    /// a failed send leaves the stored SEQUENCE alone so a retry re-uses the same number.
    pub pending_sequence: Option<i64>,
}

/// (start RFC 3339, minutes, IANA zone) for a Zoom meeting update; `None` for all-day / floating events.
fn zoom_args(ev: &CalEvent) -> Option<(String, u32, String)> {
    if ev.all_day {
        return None;
    }
    let tz = ev.tzid.clone()?;
    let s = chrono::DateTime::parse_from_rfc3339(&ev.start).ok()?;
    let e = chrono::DateTime::parse_from_rfc3339(&ev.end).ok()?;
    let minutes = (e - s).num_minutes().clamp(1, 24 * 60) as u32;
    Some((ev.start.clone(), minutes, tz))
}

fn may_touch_zoom(ev: &CalEvent, mine: &[String]) -> bool {
    ev.organizer.is_none() || itip::is_organizer(ev, mine)
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let auth = rest.split(['/', '?', '#']).next()?;
    let host = auth.rsplit('@').next()?.split(':').next()?;
    Some(host.to_ascii_lowercase()).filter(|h| !h.is_empty())
}

/// Both calendars live on the same CalDAV server account (a move between them keeps server-side data).
fn same_server(a: &CalendarInfo, b: &CalendarInfo) -> bool {
    a.kind == CalendarKind::Caldav
        && b.kind == CalendarKind::Caldav
        && a.username == b.username
        && matches!((a.url.as_deref().and_then(host_of), b.url.as_deref().and_then(host_of)), (Some(x), Some(y)) if x == y)
}

/// Write an event locally (strict builder), push it to CalDAV, update its Zoom meeting when the time or
/// title changed, and build the iTIP REQUEST when asked. Errors before the local write leave nothing behind.
/// The invitation mail does not depend on the CalDAV push: it is built from the local copy and a push problem is
/// reported in `push_error` (or as the error when there is nothing to send).
pub async fn save_event<R: CalDavRemote, Z: ZoomRemote>(st: &CalState, r: &R, z: &Z, ev: CalEvent, send_invites: bool, mine: &[String]) -> Result<SaveOutcome, String> {
    save_event_if(st, r, z, ev, send_invites, mine, None).await
}

/// A check run on the stored event while the calendar's lock is held, right before a write.
pub type Precondition<'a> = &'a (dyn Fn(&CalEvent) -> bool + Send + Sync);

pub const MSG_CHANGED_SINCE: &str = "The event was changed after this suggestion was made, so nothing was done. Ask again to get a fresh suggestion.";

/// [`save_event`] with a precondition on the STORED event, checked under the per-calendar lock (the assistant's
/// approved edits use it so a sync or an edit between the card and the click cannot be overwritten).
pub async fn save_event_if<R: CalDavRemote, Z: ZoomRemote>(
    st: &CalState,
    r: &R,
    z: &Z,
    mut ev: CalEvent,
    send_invites: bool,
    mine: &[String],
    precond: Option<Precondition<'_>>,
) -> Result<SaveOutcome, String> {
    let cal = st.calendar(&ev.calendar_id)?;
    if cal.read_only {
        return Err(MSG_READ_ONLY.into());
    }
    if ev.uid.trim().is_empty() {
        ev.uid = new_event_uid();
    }
    // Serialise with this calendar's sync (and with the calendar the event moves out of), sorted to avoid deadlocks.
    let mut ids = vec![cal.id.clone()];
    if st.db(|c| c.get_event(&ev.uid, &cal.id))?.is_none() {
        if let Some(o) = st.db(|c| c.find_event(&ev.uid))?.filter(|o| o.calendar_id != cal.id) {
            ids.push(o.calendar_id);
        }
    }
    ids.sort();
    ids.dedup();
    let mut _locks = Vec::new();
    for id in &ids {
        _locks.push(st.cal_lock(id).lock_owned().await);
    }

    let here = st.db(|c| c.get_event(&ev.uid, &cal.id))?;
    if let Some(ok) = precond {
        if !here.as_ref().is_some_and(ok) {
            return Err(MSG_CHANGED_SINCE.into());
        }
    }
    let moved_from = if here.is_none() { st.db(|c| c.find_event(&ev.uid))?.filter(|o| o.calendar_id != cal.id) } else { None };
    if moved_from.as_ref().is_some_and(|o| !ids.contains(&o.calendar_id)) {
        return Err("This event was just changed somewhere else. Open it again and retry.".into());
    }
    let old_cal = match &moved_from {
        Some(o) => {
            let c = st.calendar(&o.calendar_id)?;
            if c.read_only {
                return Err("This event can't be moved out of a read-only calendar.".into());
            }
            Some(c)
        }
        None => None,
    };

    // Server-controlled fields come from the store, never from the frontend.
    ev.href = here.as_ref().and_then(|p| p.href.clone());
    ev.etag = here.as_ref().and_then(|p| p.etag.clone());
    if let Some(p) = &here {
        ev.sequence = ev.sequence.max(p.sequence);
    }
    if !ev.attendees.is_empty() && ev.organizer.is_none() {
        if let Some(me) = mine.first() {
            ev.organizer = Some(Attendee { email: me.clone(), name: None, role: AttendeeRole::Chair, status: AttendeeStatus::Accepted });
        }
    }
    let inviting = send_invites && !ev.attendees.is_empty();
    // An UPDATE of an already-invited event carries SEQUENCE + 1; that number is only stored once the mail is out.
    let mut pending_sequence = None;
    if inviting {
        if mine.is_empty() {
            return Err("Add a mail account first to send invitations.".into());
        }
        if !itip::is_organizer(&ev, mine) {
            return Err("You can only send invitations for events you organise.".into());
        }
        if let Some(p) = &here {
            if !p.attendees.is_empty() {
                pending_sequence = Some(p.sequence.max(ev.sequence) + 1);
            }
        }
    }
    let start_url = ev.zoom.as_ref().and_then(|zm| zm.start_url.clone());

    st.db(|c| c.upsert_event(&ev))?;

    // Moving between calendars: bring the series overrides (and, on the same server, the server's original
    // resource) along. The old copy is only removed once the new one is stored AND pushed (below).
    if let (Some(old), Some(oc)) = (&moved_from, &old_cal) {
        let carried = st.db(|c| {
            for mut o in c.get_overrides(&old.uid, &oc.id)? {
                o.event.calendar_id = cal.id.clone();
                o.event.href = None;
                o.event.etag = None;
                c.upsert_override(&o)?;
            }
            if same_server(oc, &cal) && calsync::caldav::is_owned(c, &oc.id) && calsync::caldav::is_owned(c, &cal.id) {
                c.copy_raw_ics(&old.uid, &oc.id, &cal.id)?;
            }
            Ok(())
        });
        if let Err(e) = carried {
            // Nothing was removed from the old calendar: drop the half-made copy so there is no duplicate.
            let _ = st.db(|c| c.delete_event(&ev.uid, &cal.id));
            return Err(e);
        }
    }

    // Zoom meeting follows time / title changes (best effort; never blocks the save). Only meetings this app created.
    if let (Some(zm), Some(p)) = (&ev.zoom, &here) {
        let prev_id = p.zoom.as_ref().map(|pz| pz.meeting_id.as_str());
        let changed = p.summary != ev.summary || p.start != ev.start || p.end != ev.end || p.tzid != ev.tzid;
        if changed && prev_id == Some(zm.meeting_id.as_str()) && may_touch_zoom(&ev, mine) && st.zoom_is_ours(&zm.meeting_id) {
            if let Some((start, minutes, tz)) = zoom_args(&ev) {
                let _ = z.update(&zm.meeting_id, &ev.summary, &start, minutes, &tz).await;
            }
        }
    }

    let pushed = push_if_caldav(st, r, &cal, &ev.uid).await;

    if let (Some(old), Some(oc)) = (&moved_from, &old_cal) {
        if pushed == Pushed::Conflict {
            // The server's version replaced ours: keep the old copy rather than risk losing the event.
            return Err(format!("{MSG_CONFLICT} The event was not removed from its previous calendar."));
        }
        let _ = delete_stored(st, r, oc, old, true).await;
    }

    let mut stored = st.db(|c| c.get_event(&ev.uid, &cal.id))?.ok_or("The event could not be read back after saving.")?;
    if let (Some(zm), Some(u)) = (stored.zoom.as_mut(), start_url) {
        zm.start_url = Some(u);
    }
    let push_error = match pushed {
        Pushed::Done => None,
        Pushed::Conflict => return Err(MSG_CONFLICT.into()), // server wins; inviting from its version would be wrong
        Pushed::Queued(m) => Some(m),
    };
    let mail = if inviting {
        let mut for_mail = stored.clone();
        if let Some(n) = pending_sequence {
            for_mail.sequence = n;
        }
        // The server must not also invite: MiraMail's own mail is the only one (the PUT carries SCHEDULE-AGENT=CLIENT).
        match build_request_mail(&for_mail, mine, false) {
            Ok(m) => m,
            Err(e) => return Err(format!("The event was saved, but the invitations were NOT sent: {e}")),
        }
    } else {
        None
    };
    if mail.is_none() {
        if let Some(e) = push_error {
            return Err(e);
        }
        pending_sequence = None;
    }
    Ok(SaveOutcome { event: stored, mail, push_error, pending_sequence })
}

/// After the REQUEST mail was sent: store the bumped SEQUENCE (and push it, best effort).
pub async fn commit_sequence<R: CalDavRemote>(st: &CalState, r: &R, uid: &str, calendar_id: &str, sequence: i64) {
    let _lock = st.cal_lock(calendar_id).lock_owned().await;
    let Ok(Some(mut ev)) = st.db(|c| c.get_event(uid, calendar_id)) else { return };
    if ev.sequence >= sequence {
        return;
    }
    ev.sequence = sequence;
    if st.db(|c| c.upsert_event(&ev)).is_err() {
        return;
    }
    if let Ok(cal) = st.calendar(calendar_id) {
        let _ = push_if_caldav(st, r, &cal, uid).await;
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeleteOutcome {
    pub mail: Option<OutgoingMessage>,
    /// Account the CANCEL must be sent from: the event's organizer (one of the user's own addresses).
    pub from: Option<String>,
    /// The event is gone locally, but the server could not be updated; shown to the user after the mail went out.
    pub warning: Option<String>,
}

/// Delete locally and on the server (queued for the next sync when the server is unreachable).
/// `force`: the user moved the event elsewhere, so on a 412 (changed on the server) the deletion is queued
/// without a precondition instead of leaving a second copy behind.
async fn delete_stored<R: CalDavRemote>(st: &CalState, r: &R, cal: &CalendarInfo, ev: &CalEvent, force: bool) -> Option<String> {
    if let Err(e) = st.db(|c| c.delete_event(&ev.uid, &cal.id)) {
        return Some(e);
    }
    if cal.kind != CalendarKind::Caldav {
        return None;
    }
    let href = ev.href.as_deref()?;
    if st.signin_failed(&cal.id) {
        let _ = calsync::caldav::mark_deleted(&st.store, &cal.id, href, ev.etag.as_deref());
        return Some(format!("Deleted on this Mac, but not on the server. {MSG_SIGNIN_FAILED}"));
    }
    let Some(pw) = st.caldav_password(&cal.id) else {
        let _ = calsync::caldav::mark_deleted(&st.store, &cal.id, href, ev.etag.as_deref());
        return Some(format!("Deleted on this Mac, but not on the server. {MSG_NO_PASSWORD}"));
    };
    match r.delete(&st.store, cal, &pw, href, ev.etag.as_deref()).await {
        Ok(()) => None,
        Err(calsync::CalDavError::PreconditionFailed) if force => {
            let _ = calsync::caldav::mark_deleted(&st.store, &cal.id, href, None);
            None
        }
        Err(calsync::CalDavError::PreconditionFailed) => {
            Some("The event changed on the server, so it was not deleted there. Sync to see the latest version.".into())
        }
        Err(e) => {
            if e == calsync::CalDavError::Unauthorized {
                st.set_signin_failed(&cal.id);
            }
            Some(format!("Deleted on this Mac, but not on the calendar server: {e} It will be retried at the next sync."))
        }
    }
}

pub async fn delete_event<R: CalDavRemote, Z: ZoomRemote>(st: &CalState, r: &R, z: &Z, uid: &str, calendar_id: &str, notify: bool, mine: &[String]) -> Result<DeleteOutcome, String> {
    delete_event_if(st, r, z, uid, calendar_id, notify, mine, None).await
}

/// [`delete_event`] with a precondition on the stored event, checked under the calendar's lock.
#[allow(clippy::too_many_arguments)]
pub async fn delete_event_if<R: CalDavRemote, Z: ZoomRemote>(
    st: &CalState,
    r: &R,
    z: &Z,
    uid: &str,
    calendar_id: &str,
    notify: bool,
    mine: &[String],
    precond: Option<Precondition<'_>>,
) -> Result<DeleteOutcome, String> {
    let cal = st.calendar(calendar_id)?;
    if cal.read_only {
        return Err(MSG_READ_ONLY.into());
    }
    let _lock = st.cal_lock(calendar_id).lock_owned().await;
    let Some(ev) = st.db(|c| c.get_event(uid, calendar_id))? else { return Ok(DeleteOutcome::default()) };
    if precond.is_some_and(|ok| !ok(&ev)) {
        return Err(MSG_CHANGED_SINCE.into());
    }
    let mail = if notify && !ev.attendees.is_empty() && itip::is_organizer(&ev, mine) { build_cancel_mail(&ev, mine)? } else { None };
    // The CANCEL must come from the organizer address: that is the only sender attendees will accept.
    let from = mail.as_ref().and_then(|_| ev.organizer.as_ref().map(|o| o.email.clone()));
    let warning = delete_stored(st, r, &cal, &ev, false).await;
    // Only a meeting this app created and attached to an event the user owns (writable calendar).
    if let Some(zm) = &ev.zoom {
        if may_touch_zoom(&ev, mine) && st.zoom_is_ours(&zm.meeting_id) && z.delete(&zm.meeting_id).await.is_ok() {
            st.zoom_forget(&zm.meeting_id);
        }
    }
    Ok(DeleteOutcome { mail, from, warning })
}

// ---------------------------------------------------------------------------------------------
// Invites (mail -> calendar)
// ---------------------------------------------------------------------------------------------

fn itip_err(e: calendar::Error) -> String {
    match e {
        calendar::Error::Invalid(m) if m.contains("not the organizer") => "This message wasn't sent by the event's organizer, so it was ignored.".into(),
        calendar::Error::Invalid(m) if m.contains("stale") => "A newer version of this event is already in your calendar.".into(),
        calendar::Error::Invalid(m) if m.contains("sender") || m.contains("organizer changed") => {
            "This message doesn't come from a participant of the event, so it was ignored.".into()
        }
        other => crate::cal_state::cerr(other),
    }
}

const MSG_UNVERIFIED: &str = "This message could not be verified as really coming from its sender (no passing DMARC/DKIM result), so it was not applied \
     to an event that is already in your calendar. If you trust it, apply it again and confirm.";

/// The calendar an inbound invite for `uid` lands in: the one that already has the event, else the default.
fn target_calendar(st: &CalState, uid: &str) -> Result<String, String> {
    Ok(st.db(|c| c.find_event(uid))?.map(|s| s.calendar_id).unwrap_or_else(|| DEFAULT_CALENDAR_ID.to_string()))
}

/// The master of a series with an override for the instance `rid` that was never overridden before: the
/// instance keeps the master's content, its times come from `rid` + the master's duration (never from the
/// untrusted message).
fn instance_from_master(master: &CalEvent, rid: &str) -> Result<CalEvent, String> {
    let bad = || "That reply is for an occurrence that isn't in your calendar.".to_string();
    if master.rrule.is_none() || master.exdates.iter().any(|x| itip::rid_eq(x, rid)) {
        return Err(bad());
    }
    let mut e = master.clone();
    e.rrule = None;
    e.exdates.clear();
    if master.all_day {
        let d = |s: &str| chrono::NaiveDate::parse_from_str(s.get(..10).unwrap_or(""), "%Y-%m-%d").map_err(|_| bad());
        let (ms, me, rs) = (d(&master.start)?, d(&master.end)?, d(rid)?);
        e.start = rs.format("%Y-%m-%d").to_string();
        e.end = (rs + (me - ms)).format("%Y-%m-%d").to_string();
    } else {
        let p = |s: &str| chrono::DateTime::parse_from_rfc3339(s).map_err(|_| bad());
        let (ms, me, rs) = (p(&master.start)?, p(&master.end)?, p(rid)?);
        e.start = rid.to_string();
        e.end = (rs + (me - ms)).to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    }
    Ok(e)
}

/// Apply a REQUEST/PUBLISH/CANCEL/REPLY already parsed from a mail whose From address is `sender`
/// (the DMARC/DKIM state of that mail is `inv.sender_unverified`). `mine` are all of the user's own addresses.
/// Returns the resulting master (or instance) event and its calendar.
fn write_invite(st: &CalState, inv: &Invite, sender: &str, mine: &[String], allow_unverified: bool) -> Result<(CalEvent, CalendarInfo), String> {
    let uid = inv.event.uid.clone();
    let stored = st.db(|c| c.find_event(&uid))?;
    let cid = stored.as_ref().map(|s| s.calendar_id.clone()).unwrap_or_else(|| DEFAULT_CALENDAR_ID.to_string());
    let cal = st.calendar(&cid)?;
    let ovs = st.db(|c| c.get_overrides(&uid, &cid))?;
    let fix = |mut e: CalEvent| {
        e.calendar_id = cid.clone();
        e
    };
    let inbound_event = matches!(inv.method, InviteMethod::Request | InviteMethod::Cancel | InviteMethod::Publish);
    if inbound_event {
        // A REQUEST/CANCEL always comes from the organizer; one that names one of our own addresses as organizer
        // cannot have come from us, so it is a forgery (or an echo) and never touches the calendar.
        let me_org = |e: &CalEvent| e.organizer.as_ref().is_some_and(|o| mine.iter().any(|m| same(m, &o.email)));
        if me_org(&inv.event) || inv.overrides.iter().any(|o| me_org(&o.event)) {
            return Err("This invitation names you as its organizer but did not come from you, so it was ignored.".into());
        }
        // An invite may not take over an event we hold without an organizer (e.g. one the user made themselves).
        if inv.method != InviteMethod::Cancel
            && stored.as_ref().is_some_and(|s| s.organizer.is_none())
            && (inv.event.organizer.is_some() || inv.overrides.iter().any(|o| o.event.organizer.is_some()))
        {
            return Err("This event is already in your calendar without an organizer, so this invitation can't take it over.".into());
        }
    }
    let updating = matches!(inv.method, InviteMethod::Cancel | InviteMethod::Reply) || stored.is_some() || !ovs.is_empty();
    if inv.sender_unverified && updating && !allow_unverified {
        return Err(MSG_UNVERIFIED.into());
    }
    let primary: CalEvent = match inv.method {
        InviteMethod::Reply => {
            let found = inv.recurrence_id.as_ref().and_then(|rid| ovs.iter().find(|o| itip::rid_eq(&o.recurrence_id, rid)));
            let target = match (&inv.recurrence_id, found) {
                (Some(_), Some(o)) => o.event.clone(),
                // never-overridden instance of a stored series: derive the override from the master
                (Some(rid), None) => instance_from_master(stored.as_ref().ok_or("That reply is for an event that isn't in your calendar.")?, rid)?,
                (None, _) => stored.clone().ok_or("That reply is for an event that isn't in your calendar.")?,
            };
            let upd = fix(itip::apply_reply(&target, &inv.event, sender).map_err(itip_err)?);
            match &inv.recurrence_id {
                Some(rid) => {
                    let key = found.map(|o| o.recurrence_id.clone()).unwrap_or_else(|| rid.clone());
                    st.db(|c| c.upsert_override(&EventOverride { recurrence_id: key, event: upd.clone() }))?
                }
                None => st.db(|c| c.upsert_event(&upd))?,
            }
            upd
        }
        InviteMethod::Cancel => {
            if stored.is_none() && ovs.is_empty() {
                return Err("That event isn't in your calendar, so there is nothing to cancel.".into());
            }
            let up = itip::apply_cancel_series(stored.as_ref(), &ovs, inv, sender).map_err(itip_err)?;
            apply_update(st, &cid, &uid, &up)?
        }
        InviteMethod::Request | InviteMethod::Publish => {
            let up = itip::apply_request_series(stored.as_ref(), &ovs, inv, sender).map_err(itip_err)?;
            apply_update(st, &cid, &uid, &up)?
        }
    };
    Ok((primary, cal))
}

fn apply_update(st: &CalState, cid: &str, uid: &str, up: &itip::SeriesUpdate) -> Result<CalEvent, String> {
    let mut primary: Option<CalEvent> = None;
    if let Some(m) = &up.master {
        let mut m = m.clone();
        m.calendar_id = cid.to_string();
        st.db(|c| c.upsert_event(&m))?;
        primary = Some(m);
    }
    for o in &up.overrides {
        let mut o = o.clone();
        o.event.calendar_id = cid.to_string();
        st.db(|c| c.upsert_override(&o))?;
        primary.get_or_insert(o.event);
    }
    for rid in &up.removed_overrides {
        st.db(|c| c.delete_override(uid, cid, rid))?;
    }
    primary.ok_or_else(|| "Nothing to apply.".to_string())
}

/// "Add to calendar": store an invite (REQUEST / PUBLISH / CANCEL, or apply a REPLY) from a mail whose
/// `From` address is `sender`. An invite whose DMARC/DKIM result did not pass (`sender_unverified`) is only
/// applied to an existing event / as a CANCEL when the user confirmed (`allow_unverified`).
/// Push failures are not fatal: the change is queued for the next sync.
pub async fn import_invite<R: CalDavRemote>(
    st: &CalState,
    r: &R,
    inv: &Invite,
    sender: &str,
    mine: &[String],
    allow_unverified: bool,
) -> Result<CalEvent, String> {
    let _lock = st.cal_lock(&target_calendar(st, &inv.event.uid)?).lock_owned().await;
    let (ev, cal) = write_invite(st, inv, sender, mine, allow_unverified)?;
    let _ = push_if_caldav(st, r, &cal, &ev.uid).await;
    Ok(st.db(|c| c.get_event(&ev.uid, &cal.id))?.unwrap_or(ev))
}

/// Session-wide identity of one answer to one invitation (used to reject a double click / double submit).
pub fn reply_key(inv: &Invite, me: &str, status: AttendeeStatus) -> String {
    format!("{}\u{1}{}\u{1}{}\u{1}{:?}", me.trim().to_ascii_lowercase(), inv.event.uid, inv.event.sequence, status)
}

/// Accept / tentative / decline: store the event with the user's answer and build the REPLY for the organizer.
/// The invitation is validated against the sender first, so a spoofed invite never produces a reply. The same
/// answer to the same invitation is accepted once per session (a second REPLY mail is refused); a failed
/// attempt does not count.
#[allow(clippy::too_many_arguments)]
pub async fn respond_invite<R: CalDavRemote>(
    st: &CalState,
    r: &R,
    mut inv: Invite,
    sender: &str,
    me: &str,
    status: AttendeeStatus,
    mine: &[String],
    allow_unverified: bool,
) -> Result<(CalEvent, OutgoingMessage), String> {
    if inv.method != InviteMethod::Request {
        return Err("This isn't an invitation you can respond to.".into());
    }
    if inv.recurrence_id.is_some() {
        return Err("Replying to a single occurrence isn't supported yet. Add it to your calendar instead.".into());
    }
    if inv.event.organizer.is_none() {
        return Err("This invitation has no organizer to reply to.".into());
    }
    let key = reply_key(&inv, me, status);
    if !st.note_reply(&key) {
        return Err("You already sent this reply.".into());
    }
    let res = respond_inner(st, r, &mut inv, sender, me, status, mine, allow_unverified).await;
    if res.is_err() {
        st.forget_reply(&key);
    }
    res
}

#[allow(clippy::too_many_arguments)]
async fn respond_inner<R: CalDavRemote>(
    st: &CalState,
    r: &R,
    inv: &mut Invite,
    sender: &str,
    me: &str,
    status: AttendeeStatus,
    mine: &[String],
    allow_unverified: bool,
) -> Result<(CalEvent, OutgoingMessage), String> {
    itip::set_attendee_status(&mut inv.event, me, None, status);
    for o in inv.overrides.iter_mut() {
        itip::set_attendee_status(&mut o.event, me, None, status);
    }
    let _lock = st.cal_lock(&target_calendar(st, &inv.event.uid)?).lock_owned().await;
    let (ev, cal) = write_invite(st, inv, sender, mine, allow_unverified)?;
    let _ = push_if_caldav(st, r, &cal, &ev.uid).await;
    let stored = st.db(|c| c.get_event(&ev.uid, &cal.id))?.unwrap_or(ev);
    let mail = build_reply_mail(&stored, me, status)?;
    Ok((stored, mail))
}

// ---------------------------------------------------------------------------------------------
// Zoom settings
// ---------------------------------------------------------------------------------------------

/// Store the Zoom client id (settings) and optional secret (Keychain). `client_secret`: `None` keeps the stored
/// secret, `Some("")` removes it. Changing the client id signs Zoom out locally (old tokens belong to the old app).
pub fn zoom_set_client(st: &CalState, client_id: &str, client_secret: Option<&str>) -> Result<(), String> {
    use crate::cal_state::{ZOOM_CLIENT_ID_SETTING, ZOOM_PREFIX, ZOOM_SECRET_KEY};
    if st.zoom_connecting.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("A Zoom sign-in is in progress. Finish it (or wait for it to time out) before changing the Zoom app.".into());
    }
    let id = client_id.trim();
    if id.is_empty() || id.len() > 200 || id.chars().any(char::is_control) {
        return Err("Enter your Zoom Client ID.".into());
    }
    let prev = st.db(|c| c.get_setting(ZOOM_CLIENT_ID_SETTING))?;
    if let Some(s) = client_secret {
        let s = s.trim();
        let r = if s.is_empty() { st.secrets.delete(ZOOM_SECRET_KEY) } else { st.secrets.set(ZOOM_SECRET_KEY, s) };
        r.map_err(|_| "Couldn't save the Zoom client secret to the Keychain.".to_string())?;
    }
    if prev.as_deref().is_some_and(|p| p != id) {
        for k in [calsync::zoom::KEY_ACCESS, calsync::zoom::KEY_REFRESH, calsync::zoom::KEY_EXPIRES] {
            let _ = st.secrets.delete(&format!("{ZOOM_PREFIX}{k}"));
        }
    }
    st.db(|c| c.set_setting(ZOOM_CLIENT_ID_SETTING, id))?;
    st.reset_zoom();
    Ok(())
}

/// Delete a Zoom meeting by id, but only one this app created (recorded at creation). Anything else is refused,
/// whatever the frontend asks for.
pub async fn zoom_delete_recorded<Z: ZoomRemote>(st: &CalState, z: &Z, meeting_id: &str) -> Result<(), String> {
    if !st.zoom_is_ours(meeting_id) {
        return Err("MiraMail can only delete Zoom meetings it created on this Mac.".into());
    }
    z.delete(meeting_id).await.map_err(|e| e.to_string())?;
    st.zoom_forget(meeting_id);
    Ok(())
}

/// Friendly text for a Zoom failure during sign-in, with the redirect-URI hint where it helps.
pub fn zoom_connect_error(e: &calsync::ZoomError, redirect_uri: &str) -> String {
    use calsync::ZoomError as Z;
    match e {
        Z::PortInUse(_) | Z::NotConfigured | Z::Offline | Z::Timeout | Z::Tls | Z::Insecure => e.to_string(),
        _ => format!(
            "{e} Check that your Zoom app's Redirect URL is exactly {redirect_uri} and that its scopes include meeting:write:meeting and user:read:user."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cal_invite::{load_invite, testmail};
    use calendar::{Calendar, ZoomMeeting};
    use calsync::caldav::{PutOutcome, SyncSummary};
    use calsync::{CalDavError, DiscoveredCalendar, ZoomError};
    use mailcore::{InMemorySecretStore, SecretStore};
    use std::sync::{Arc, Mutex};

    // ---- fakes ----------------------------------------------------------------------------

    #[derive(Default)]
    struct FakeDav {
        log: Mutex<Vec<String>>,
        discovered: Mutex<Vec<DiscoveredCalendar>>,
        discover_err: Mutex<Option<CalDavError>>,
        push_result: Mutex<Option<Result<PushOutcome, CalDavError>>>,
        delete_result: Mutex<Option<Result<(), CalDavError>>>,
        sync_err: Mutex<Option<CalDavError>>,
        discover_pw: Mutex<Vec<String>>,
    }
    impl FakeDav {
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }
    impl CalDavRemote for FakeDav {
        async fn discover(&self, base_url: &str, username: &str, pw: &str, _d: &calsync::Diag) -> Result<Vec<DiscoveredCalendar>, CalDavError> {
            self.discover_pw.lock().unwrap().push(pw.to_string());
            self.log.lock().unwrap().push(format!("discover {base_url} {username}"));
            match self.discover_err.lock().unwrap().clone() {
                Some(e) => Err(e),
                None => Ok(self.discovered.lock().unwrap().clone()),
            }
        }
        async fn sync(&self, _s: &Mutex<Calendar>, cal: &CalendarInfo, pw: &str) -> Result<SyncSummary, CalDavError> {
            self.log.lock().unwrap().push(format!("sync {} pw={}", cal.name, pw));
            if let Some(e) = self.sync_err.lock().unwrap().clone() {
                return Err(e);
            }
            Ok(SyncSummary { pulled: 2, pushed: 1, deleted: 0, conflicts: 0 })
        }
        async fn push(&self, s: &Mutex<Calendar>, cal: &CalendarInfo, _pw: &str, uid: &str) -> Result<PushOutcome, CalDavError> {
            self.log.lock().unwrap().push(format!("push {} {uid}", cal.id));
            let r = self.push_result.lock().unwrap().clone().unwrap_or(Ok(PushOutcome::Pushed(PutOutcome { href: "/c/x.ics".into(), etag: Some("\"e\"".into()) })));
            if let Ok(PushOutcome::Pushed(o)) = &r {
                // what the real client's record_put does
                let c = s.lock().unwrap();
                let mut e = c.get_event(uid, &cal.id).unwrap().unwrap();
                e.href = Some(o.href.clone());
                e.etag = o.etag.clone();
                c.upsert_event(&e).unwrap();
            }
            r
        }
        async fn delete(&self, _s: &Mutex<Calendar>, cal: &CalendarInfo, _pw: &str, href: &str, etag: Option<&str>) -> Result<(), CalDavError> {
            self.log.lock().unwrap().push(format!("delete {} {href} {}", cal.id, etag.unwrap_or("-")));
            self.delete_result.lock().unwrap().clone().unwrap_or(Ok(()))
        }
    }

    #[derive(Default)]
    struct FakeZoom {
        log: Mutex<Vec<String>>,
        delete_ok: Mutex<bool>,
    }
    impl ZoomRemote for FakeZoom {
        async fn update(&self, id: &str, topic: &str, start: &str, minutes: u32, tz: &str) -> Result<(), ZoomError> {
            self.log.lock().unwrap().push(format!("update {id} {topic} {start} {minutes} {tz}"));
            Ok(())
        }
        async fn delete(&self, id: &str) -> Result<(), ZoomError> {
            self.log.lock().unwrap().push(format!("delete {id}"));
            if *self.delete_ok.lock().unwrap() {
                return Ok(());
            }
            Err(ZoomError::NotConnected) // errors are best effort
        }
    }

    // ---- helpers --------------------------------------------------------------------------

    const ME: &str = "me@x.org";

    fn setup() -> (CalState, Arc<InMemorySecretStore>) {
        let secrets = Arc::new(InMemorySecretStore::new());
        (CalState::new(Calendar::open_in_memory().unwrap(), secrets.clone()), secrets)
    }

    fn mine() -> Vec<String> {
        vec![ME.to_string()]
    }

    fn disc(url: &str, name: &str, color: Option<&str>, ro: bool) -> DiscoveredCalendar {
        DiscoveredCalendar { url: url.into(), name: name.into(), color: color.map(String::from), read_only: ro, ctag: None, owned: true }
    }

    /// A CalDAV calendar row + its Keychain password.
    fn caldav_cal(st: &CalState, secrets: &InMemorySecretStore, id: &str, name: &str, read_only: bool, with_pw: bool) -> CalendarInfo {
        let info = st
            .db(|c| {
                c.create_calendar(&NewCalendar {
                    id: Some(id.into()),
                    name: name.into(),
                    color: "#112233".into(),
                    kind: CalendarKind::Caldav,
                    url: Some(format!("https://dav.example/{id}/")),
                    username: Some("u".into()),
                    read_only,
                })
            })
            .unwrap();
        if with_pw {
            secrets.set(&caldav_key(id), "pw").unwrap();
        }
        info
    }

    fn ev(uid: &str, cal: &str) -> CalEvent {
        CalEvent {
            uid: uid.into(),
            calendar_id: cal.into(),
            summary: "Lunch".into(),
            start: "2026-11-10T12:00:00+00:00".into(),
            end: "2026-11-10T13:00:00+00:00".into(),
            tzid: Some("UTC".into()),
            ..Default::default()
        }
    }

    fn att(email: &str) -> Attendee {
        Attendee { email: email.into(), name: None, role: AttendeeRole::Required, status: AttendeeStatus::NeedsAction }
    }

    // ---- calendars ------------------------------------------------------------------------

    #[tokio::test]
    async fn add_caldav_discovers_then_stores_password_and_rows() {
        let (st, secrets) = setup();
        let dav = FakeDav::default();
        *dav.discovered.lock().unwrap() = vec![disc("https://d.example/a/", "Work", Some("#aa00bb"), false), disc("https://d.example/b/", "Birthdays", None, true)];
        let out = add_caldav(&st, &dav, "fastmail", None, " me@x.org ", "secret").await.unwrap();
        assert_eq!(dav.log(), vec!["discover https://caldav.fastmail.com me@x.org"]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].color, "#aa00bb");
        assert!(valid_color(&out[1].color) && out[1].read_only);
        assert_eq!(out[0].kind, CalendarKind::Caldav);
        assert_eq!(out[0].url.as_deref(), Some("https://d.example/a/"));
        assert_eq!(secrets.get(&caldav_key(&out[0].id)).unwrap().as_deref(), Some("secret"));
        assert_eq!(secrets.get(&caldav_key(&out[1].id)).unwrap().as_deref(), Some("secret"));
        // the password is not in the database
        let all = serde_json::to_string(&st.db(|c| c.list_calendars()).unwrap()).unwrap();
        assert!(!all.contains("secret"));
        // adding the same server again reuses the rows
        let again = add_caldav(&st, &dav, "fastmail", None, "me@x.org", "new").await.unwrap();
        assert_eq!(again.iter().map(|c| &c.id).collect::<Vec<_>>(), out.iter().map(|c| &c.id).collect::<Vec<_>>());
        assert_eq!(st.db(|c| c.list_calendars()).unwrap().len(), 3); // + Personal
        assert_eq!(secrets.get(&caldav_key(&out[0].id)).unwrap().as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn add_caldav_failure_stores_nothing_and_is_friendly() {
        let (st, secrets) = setup();
        let dav = FakeDav::default();
        *dav.discover_err.lock().unwrap() = Some(CalDavError::Unauthorized);
        let e = add_caldav(&st, &dav, "icloud", None, "me@icloud.com", "pw").await.unwrap_err();
        assert!(e.contains("app-specific password"), "{e}");
        assert_eq!(st.db(|c| c.list_calendars()).unwrap().len(), 1);
        assert_eq!(secrets.get("caldav:anything").unwrap(), None);
        // input validation
        assert!(add_caldav(&st, &dav, "custom", None, "u", "pw").await.unwrap_err().contains("server address"));
        assert!(add_caldav(&st, &dav, "nope", None, "u", "pw").await.unwrap_err().contains("Unknown"));
        assert!(add_caldav(&st, &dav, "custom", Some("x.example/dav"), "", "pw").await.is_err());
        *dav.discover_err.lock().unwrap() = None;
        assert!(add_caldav(&st, &dav, "custom", Some("x.example/dav"), "u", "pw").await.unwrap_err().contains("No calendars"));
        assert!(dav.log().last().unwrap().contains("https://x.example/dav"));
    }

    #[tokio::test]
    async fn sync_skips_calendars_without_password_and_sums_counts() {
        let (st, secrets) = setup();
        caldav_cal(&st, &secrets, "a", "A", false, true);
        caldav_cal(&st, &secrets, "b", "B", false, false);
        let dav = FakeDav::default();
        let out = sync_calendars(&st, &dav, None).await.unwrap();
        assert_eq!((out.attempted, out.calendars, out.events), (2, 1, 3));
        assert_eq!(dav.log(), vec!["sync A pw=pw"], "B must never be contacted");
        assert!(out.errors[0].starts_with("B: No saved password"));
        let b = st.calendar("b").unwrap();
        assert!(b.error.unwrap().contains("No saved password"));
        // single-calendar sync; local calendar syncs nothing; unknown id is an error
        assert_eq!(sync_calendars(&st, &dav, Some(DEFAULT_CALENDAR_ID)).await.unwrap().attempted, 0);
        assert!(sync_calendars(&st, &dav, Some("zzz")).await.is_err());
        assert_eq!(sync_calendars(&st, &dav, Some("a")).await.unwrap().calendars, 1);
    }

    #[tokio::test]
    async fn calendar_management() {
        let (st, secrets) = setup();
        assert!(create_local(&st, " ", "#112233").is_err());
        assert!(create_local(&st, "x", "red").is_err());
        let c = create_local(&st, "Home", "#112233").unwrap();
        update_calendar(&st, &c.id, Some("Home2".into()), None, Some(false)).unwrap();
        assert_eq!(st.calendar(&c.id).unwrap().name, "Home2");
        assert!(!st.calendar(&c.id).unwrap().visible);
        assert!(update_calendar(&st, &c.id, None, Some("nope".into()), None).is_err());
        caldav_cal(&st, &secrets, "dv", "D", false, true);
        remove_calendar(&st, "dv").await.unwrap();
        assert_eq!(secrets.get(&caldav_key("dv")).unwrap(), None, "keychain entry removed");
        assert!(remove_calendar(&st, DEFAULT_CALENDAR_ID).await.is_err());
    }


    // ---- Zoho / diagnostics / suggestion ---------------------------------------------------

    #[test]
    fn suggest_from_account_hosts() {
        let s = suggest_caldav(" me@acme.in ", &["imappro.zoho.in", "smtppro.zoho.in"]);
        assert_eq!((s.preset.as_str(), s.region.as_deref(), s.username.as_str()), ("zoho", Some("in"), "me@acme.in"));
        assert_eq!(s.url.as_deref(), Some("https://calendar.zoho.in/"));
        assert_eq!(suggest_caldav("a@b.com", &["imap.zoho.com.au"]).region.as_deref(), Some("com.au"));
        assert_eq!(suggest_caldav("a@b.com", &["imap.zoho.eu"]).region.as_deref(), Some("eu"));
        assert_eq!(suggest_caldav("a@me.com", &["imap.mail.me.com"]).preset, "icloud");
        assert_eq!(suggest_caldav("a@x.org", &["imap.fastmail.com"]).preset, "fastmail");
        let c = suggest_caldav("a@x.org", &["mail.example.org"]);
        assert_eq!((c.preset.as_str(), c.url), ("custom", None));
    }

    #[tokio::test]
    async fn zoho_region_is_used_and_errors_are_step_explained() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        *dav.discovered.lock().unwrap() = vec![disc("https://calendar.zoho.in/caldav/z/a/", "Mine", None, false)];
        let d = calsync::Diag::new();
        add_caldav_ex(&st, &dav, "zoho", None, Some("in"), "me@x.in", "secret-pw", &d).await.unwrap();
        assert_eq!(dav.log(), vec!["discover https://calendar.zoho.in/ me@x.in"]);
        assert!(add_caldav_ex(&st, &dav, "zoho", None, Some("mars"), "me@x.in", "pw", &d).await.unwrap_err().message.contains("Unknown Zoho"));
        // empty discovery -> Zoho enabling hint; 401 -> password hint; neither leaks the password
        *dav.discovered.lock().unwrap() = vec![];
        let e = add_caldav_ex(&st, &dav, "zoho", None, None, "me@x.in", "secret-pw", &d).await.unwrap_err();
        assert!(e.message.contains("Synchronize > CalDAV") && !e.message.contains("secret-pw"), "{}", e.message);
        *dav.discover_err.lock().unwrap() = Some(CalDavError::Unauthorized);
        let e = add_caldav_ex(&st, &dav, "zoho", None, None, "me@x.in", "secret-pw", &d).await.unwrap_err();
        assert!(e.message.contains("two-factor") && !format!("{e:?}").contains("secret-pw"));
    }

    // ---- reuse the mail login -------------------------------------------------------------

    fn mail_acc(email: &str, imap: &str, smtp: &str) -> mailcore::AccountConfig {
        use mailcore::{ServerConfig, Tls};
        mailcore::AccountConfig {
            email: email.into(),
            imap: ServerConfig { host: imap.into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: smtp.into(), port: 465, tls: Tls::Implicit },
            username: email.into(),
        }
    }

    #[test]
    fn provider_matching_from_imap_hosts() {
        assert_eq!(provider_for_hosts(&["imappro.zoho.in"]), Some("zoho"));
        assert_eq!(provider_for_hosts(&["imap.zoho.com"]), Some("zoho"));
        assert_eq!(provider_for_hosts(&["imap.mail.me.com"]), Some("icloud"));
        assert_eq!(provider_for_hosts(&["imap.mail.icloud.com"]), Some("icloud"));
        assert_eq!(provider_for_hosts(&["imap.fastmail.com"]), Some("fastmail"));
        assert_eq!(provider_for_hosts(&["mail.example.org"]), None);
        assert_eq!(provider_for_hosts(&["imap.notzoho.com"]), None);
        let accs = vec![mail_acc("a@z.in", "imappro.zoho.in", "smtppro.zoho.in"), mail_acc("b@f.com", "imap.fastmail.com", "smtp.fastmail.com"), mail_acc("c@o.org", "mail.o.org", "mail.o.org")];
        let z = reusable_accounts(&accs, "zoho");
        assert_eq!(z, vec![ReusableAccount { email: "a@z.in".into(), provider: "zoho".into() }]);
        assert_eq!(reusable_accounts(&accs, "fastmail").len(), 1);
        assert!(reusable_accounts(&accs, "icloud").is_empty());
        assert!(reusable_accounts(&accs, "custom").is_empty());
        let json = serde_json::to_string(&z).unwrap();
        assert!(!json.contains("pass"), "{json}");
    }

    #[tokio::test]
    async fn reuse_reads_mail_secret_stores_per_calendar_only_after_discovery() {
        let (st, cal_secrets) = setup();
        let mail = InMemorySecretStore::new();
        mail.set("a@z.in", "mail-pw-123").unwrap();
        let accs = vec![mail_acc("a@z.in", "imappro.zoho.in", "smtppro.zoho.in")];
        let dav = FakeDav::default();
        let d = calsync::Diag::new();
        // failed discovery (401): nothing stored, message explains, password not echoed anywhere
        *dav.discover_err.lock().unwrap() = Some(CalDavError::Unauthorized);
        let e = add_caldav_reuse(&st, &mail, &accs, &dav, "zoho", None, "a@z.in", &d).await.unwrap_err();
        assert!(e.message.contains("app-specific") && e.message.contains("different login"), "{}", e.message);
        assert!(!format!("{e:?}").contains("mail-pw-123"));
        assert!(st.db(|c| c.list_calendars()).unwrap().iter().all(|c| c.kind != CalendarKind::Caldav));
        assert_eq!(cal_secrets.get("caldav:anything").unwrap(), None);
        // success: region inferred from the host, username = account, per-calendar secret stored
        *dav.discover_err.lock().unwrap() = None;
        *dav.discovered.lock().unwrap() = vec![disc("https://calendar.zoho.in/caldav/z/a/", "Mine", None, false)];
        let out = add_caldav_reuse(&st, &mail, &accs, &dav, "zoho", None, "A@Z.IN", &d).await.unwrap();
        assert_eq!(out.len(), 1);
        assert!(dav.log().last().unwrap().starts_with("discover https://calendar.zoho.in/ a@z.in"), "{:?}", dav.log());
        assert_eq!(dav.discover_pw.lock().unwrap().last().unwrap(), "mail-pw-123");
        assert_eq!(cal_secrets.get(&caldav_key(&out[0].id)).unwrap().as_deref(), Some("mail-pw-123"));
        assert!(!serde_json::to_string(&out).unwrap().contains("mail-pw-123"));
        // removing the mail account's secret does not break the calendar's own copy, and vice versa
        mail.delete("a@z.in").unwrap();
        assert_eq!(st.caldav_password(&out[0].id).as_deref(), Some("mail-pw-123"));
    }

    #[tokio::test]
    async fn reuse_rejects_unknown_wrong_provider_and_missing_password() {
        let (st, _s) = setup();
        let mail = InMemorySecretStore::new();
        mail.set("a@z.in", "pw").unwrap();
        mail.set("c@o.org", "pw").unwrap();
        let accs = vec![mail_acc("a@z.in", "imappro.zoho.in", "smtppro.zoho.in"), mail_acc("c@o.org", "mail.o.org", "mail.o.org"), mail_acc("n@z.com", "imap.zoho.com", "smtp.zoho.com")];
        let dav = FakeDav::default();
        let d = calsync::Diag::new();
        let e = add_caldav_reuse(&st, &mail, &accs, &dav, "zoho", None, "evil@x.org", &d).await.unwrap_err();
        assert!(e.message.contains("not one of your saved"));
        let e = add_caldav_reuse(&st, &mail, &accs, &dav, "zoho", None, "c@o.org", &d).await.unwrap_err();
        assert!(e.message.contains("different provider"));
        let e = add_caldav_reuse(&st, &mail, &accs, &dav, "icloud", None, "a@z.in", &d).await.unwrap_err();
        assert!(e.message.contains("different provider"));
        let e = add_caldav_reuse(&st, &mail, &accs, &dav, "custom", None, "a@z.in", &d).await.unwrap_err();
        assert!(e.message.contains("different provider"));
        let e = add_caldav_reuse(&st, &mail, &accs, &dav, "zoho", None, "n@z.com", &d).await.unwrap_err();
        assert!(e.message.contains("No saved password"));
        assert!(dav.log().is_empty(), "no server contact on rejected requests: {:?}", dav.log());
    }

    #[tokio::test]
    async fn remove_calendar_never_contacts_the_server_or_deletes_a_collection() {
        let (st, secrets) = setup();
        caldav_cal(&st, &secrets, "dv", "D", false, true);
        // remove_calendar has no network seam at all; the fake proves nothing was called around it too
        let dav = FakeDav::default();
        remove_calendar(&st, "dv").await.unwrap();
        assert!(dav.log().is_empty(), "{:?}", dav.log());
        assert!(st.calendar("dv").is_err());
    }

    // ---- events ---------------------------------------------------------------------------

    #[tokio::test]
    async fn save_local_event_roundtrips_and_surfaces_builder_errors() {
        let (st, _s) = setup();
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        let mut e = ev("", DEFAULT_CALENDAR_ID);
        let out = save_event(&st, &dav, &z, e.clone(), false, &mine()).await.unwrap();
        assert!(out.event.uid.ends_with("@miramail"), "uid generated");
        assert!(out.mail.is_none());
        assert!(dav.log().is_empty(), "local calendar never touches CalDAV");
        // strict builder: unparsable start is an error and writes nothing
        e.uid = "bad".into();
        e.start = "not a time".into();
        assert!(save_event(&st, &dav, &z, e, false, &mine()).await.is_err());
        assert!(st.db(|c| c.get_event("bad", DEFAULT_CALENDAR_ID)).unwrap().is_none());
        // over-cap attendees
        let mut big = ev("big", DEFAULT_CALENDAR_ID);
        big.attendees = (0..5_001).map(|i| att(&format!("a{i}@x.org"))).collect();
        assert!(save_event(&st, &dav, &z, big, false, &mine()).await.is_err());
        assert!(save_event(&st, &dav, &z, ev("u", "nope"), false, &mine()).await.unwrap_err().contains("no longer exists"));
    }

    #[tokio::test]
    async fn read_only_calendar_refuses_writes() {
        let (st, s) = setup();
        caldav_cal(&st, &s, "ro", "RO", true, true);
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        assert!(save_event(&st, &dav, &z, ev("u", "ro"), false, &mine()).await.unwrap_err().contains("read-only"));
        assert!(delete_event(&st, &dav, &z, "u", "ro", false, &mine()).await.unwrap_err().contains("read-only"));
        assert!(dav.log().is_empty());
    }

    #[tokio::test]
    async fn caldav_save_pushes_and_conflict_is_friendly_server_wins() {
        let (st, s) = setup();
        caldav_cal(&st, &s, "dv", "D", false, true);
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        save_event(&st, &dav, &z, ev("u1", "dv"), false, &mine()).await.unwrap();
        assert_eq!(dav.log(), vec!["push dv u1"]);
        *dav.push_result.lock().unwrap() = Some(Ok(PushOutcome::Conflict));
        let e = save_event(&st, &dav, &z, ev("u1", "dv"), false, &mine()).await.unwrap_err();
        assert!(e.contains("server's version was kept"), "{e}");
        *dav.push_result.lock().unwrap() = Some(Err(CalDavError::Offline));
        let e = save_event(&st, &dav, &z, ev("u1", "dv"), false, &mine()).await.unwrap_err();
        assert!(e.starts_with("Saved on this Mac") && e.contains("sync later"), "{e}");
        // saved locally despite the push error
        assert!(st.db(|c| c.get_event("u1", "dv")).unwrap().is_some());
    }

    #[tokio::test]
    async fn caldav_save_without_password_marks_dirty_and_reports() {
        let (st, s) = setup();
        caldav_cal(&st, &s, "dv", "D", false, false);
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        let e = save_event(&st, &dav, &z, ev("u1", "dv"), false, &mine()).await.unwrap_err();
        assert!(e.contains("No saved password"), "{e}");
        assert!(dav.log().is_empty());
        assert_eq!(calsync::caldav::pending_changes(&st.store, "dv").unwrap(), 1);
    }

    #[tokio::test]
    async fn invites_need_the_organizer_and_build_a_request_mail() {
        let (st, _s) = setup();
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        let mut e = ev("inv1", DEFAULT_CALENDAR_ID);
        e.attendees = vec![att("bob@y.org"), att(ME)];
        let out = save_event(&st, &dav, &z, e.clone(), true, &mine()).await.unwrap();
        assert_eq!(out.event.organizer.as_ref().unwrap().email, ME, "organizer defaults to the user");
        let mail = out.mail.expect("request mail");
        assert_eq!(mail.to, vec!["bob@y.org"], "never mails the user");
        assert!(mail.subject.starts_with("Invitation: Lunch"), "{}", mail.subject);
        let part = mail.calendar.unwrap();
        assert_eq!(part.method, "REQUEST");
        let flat = part.ics.replace("\r\n ", "");
        assert!(flat.contains("METHOD:REQUEST") && flat.contains("mailto:bob@y.org"), "{flat}");
        // the mail itself builds (no SMTP)
        let built = mailcore::smtp::build_message(ME, &OutgoingMessage { calendar: Some(part), to: mail.to, subject: mail.subject, text: mail.text, ..empty_msg() }).unwrap();
        assert!(String::from_utf8_lossy(&built.formatted()).to_ascii_lowercase().contains("method=request"));

        // updating bumps SEQUENCE in the mail now, in the store only once the mail is out
        let upd = save_event(&st, &dav, &z, out.event.clone(), true, &mine()).await.unwrap();
        assert_eq!(upd.event.sequence, 0, "not stored before the mail was sent");
        assert_eq!(upd.pending_sequence, Some(1));
        let um = upd.mail.unwrap();
        assert!(um.subject.starts_with("Updated invitation"));
        assert!(um.calendar.unwrap().ics.contains("SEQUENCE:1"));
        commit_sequence(&st, &dav, &upd.event.uid, DEFAULT_CALENDAR_ID, 1).await;
        assert_eq!(st.db(|c| c.get_event(&upd.event.uid, DEFAULT_CALENDAR_ID)).unwrap().unwrap().sequence, 1);

        // not the organizer: clear error, nothing written
        let mut other = ev("inv2", DEFAULT_CALENDAR_ID);
        other.attendees = vec![att("bob@y.org")];
        other.organizer = Some(att("boss@corp.example"));
        let e = save_event(&st, &dav, &z, other.clone(), true, &mine()).await.unwrap_err();
        assert!(e.contains("organise"), "{e}");
        assert!(st.db(|c| c.get_event("inv2", DEFAULT_CALENDAR_ID)).unwrap().is_none());
        // ...but saving without sending is fine
        assert!(save_event(&st, &dav, &z, other, false, &mine()).await.unwrap().mail.is_none());
        // no mail account
        let mut e3 = ev("inv3", DEFAULT_CALENDAR_ID);
        e3.attendees = vec![att("bob@y.org")];
        assert!(save_event(&st, &dav, &z, e3, true, &[]).await.unwrap_err().contains("mail account"));
    }

    fn empty_msg() -> OutgoingMessage {
        OutgoingMessage { to: vec![], cc: vec![], bcc: vec![], subject: String::new(), text: String::new(), html: None, attachments: vec![], in_reply_to: None, calendar: None, extra_headers: vec![] }
    }

    #[tokio::test]
    async fn zoom_meeting_updates_only_when_time_or_title_changed() {
        let (st, _s) = setup();
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        st.zoom_record("123").unwrap(); // created by this app
        let mut e = ev("z1", DEFAULT_CALENDAR_ID);
        e.zoom = Some(ZoomMeeting { meeting_id: "123".into(), join_url: "https://zoom.us/j/123".into(), password: None, start_url: Some("https://zoom.us/s/secret".into()) });
        let first = save_event(&st, &dav, &z, e.clone(), false, &mine()).await.unwrap();
        assert!(z.log.lock().unwrap().is_empty(), "newly attached meeting was created by the frontend");
        assert_eq!(first.event.zoom.as_ref().unwrap().start_url.as_deref(), Some("https://zoom.us/s/secret"), "start url only echoed back from memory");
        let stored = st.db(|c| c.get_event("z1", DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        assert!(stored.zoom.unwrap().start_url.is_none(), "start url never persisted");

        let mut same = first.event.clone();
        same.description = "agenda".into();
        save_event(&st, &dav, &z, same, false, &mine()).await.unwrap();
        assert!(z.log.lock().unwrap().is_empty());

        let mut moved = first.event.clone();
        moved.start = "2026-11-10T14:00:00+00:00".into();
        moved.end = "2026-11-10T14:45:00+00:00".into();
        save_event(&st, &dav, &z, moved, false, &mine()).await.unwrap();
        assert_eq!(*z.log.lock().unwrap(), vec!["update 123 Lunch 2026-11-10T14:00:00+00:00 45 UTC"]);
    }

    #[tokio::test]
    async fn delete_local_remote_zoom_and_cancel_rules() {
        let (st, s) = setup();
        caldav_cal(&st, &s, "dv", "D", false, true);
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        st.zoom_record("9").unwrap();
        let zoom = ZoomMeeting { meeting_id: "9".into(), join_url: "https://zoom.us/j/9".into(), password: None, start_url: None };
        // an organised event with attendees + zoom on a CalDAV calendar
        let mut e = ev("d1", "dv");
        e.attendees = vec![att("bob@y.org")];
        e.zoom = Some(zoom.clone());
        save_event(&st, &dav, &z, e, false, &mine()).await.unwrap();
        let out = delete_event(&st, &dav, &z, "d1", "dv", true, &mine()).await.unwrap();
        assert!(st.db(|c| c.get_event("d1", "dv")).unwrap().is_none());
        let log = dav.log();
        assert_eq!(log[1], "delete dv /c/x.ics \"e\"", "{log:?}");
        assert_eq!(*z.log.lock().unwrap(), vec!["delete 9"]);
        let m = out.mail.expect("cancel");
        assert_eq!(m.calendar.as_ref().unwrap().method, "CANCEL");
        assert_eq!(m.to, vec!["bob@y.org"]);
        assert!(out.warning.is_none());

        // someone else's event: no cancel mail, no zoom delete
        let z2 = FakeZoom::default();
        let mut theirs = ev("d2", DEFAULT_CALENDAR_ID);
        theirs.organizer = Some(att("boss@corp.example"));
        theirs.attendees = vec![att(ME), att("bob@y.org")];
        theirs.zoom = Some(zoom);
        save_event(&st, &dav, &z2, theirs, false, &mine()).await.unwrap();
        let out = delete_event(&st, &dav, &z2, "d2", DEFAULT_CALENDAR_ID, true, &mine()).await.unwrap();
        assert!(out.mail.is_none());
        assert!(z2.log.lock().unwrap().is_empty());

        // notify=false never mails; unknown event is a no-op
        let mut e3 = ev("d3", DEFAULT_CALENDAR_ID);
        e3.attendees = vec![att("bob@y.org")];
        save_event(&st, &dav, &z2, e3, false, &mine()).await.unwrap();
        assert!(delete_event(&st, &dav, &z2, "d3", DEFAULT_CALENDAR_ID, false, &mine()).await.unwrap().mail.is_none());
        assert_eq!(delete_event(&st, &dav, &z2, "zz", DEFAULT_CALENDAR_ID, true, &mine()).await.unwrap(), DeleteOutcome::default());
    }

    #[tokio::test]
    async fn delete_reports_server_trouble_but_removes_locally() {
        let (st, s) = setup();
        caldav_cal(&st, &s, "dv", "D", false, true);
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        save_event(&st, &dav, &z, ev("d1", "dv"), false, &mine()).await.unwrap();
        *dav.delete_result.lock().unwrap() = Some(Err(CalDavError::PreconditionFailed));
        let out = delete_event(&st, &dav, &z, "d1", "dv", false, &mine()).await.unwrap();
        assert!(out.warning.unwrap().contains("changed on the server"));
        assert!(st.db(|c| c.get_event("d1", "dv")).unwrap().is_none());
    }

    #[tokio::test]
    async fn moving_an_event_to_another_calendar_removes_the_old_copy() {
        let (st, s) = setup();
        caldav_cal(&st, &s, "dv", "D", false, true);
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        save_event(&st, &dav, &z, ev("m1", "dv"), false, &mine()).await.unwrap();
        let moved = save_event(&st, &dav, &z, ev("m1", DEFAULT_CALENDAR_ID), false, &mine()).await.unwrap();
        assert_eq!(moved.event.calendar_id, DEFAULT_CALENDAR_ID);
        assert!(st.db(|c| c.get_event("m1", "dv")).unwrap().is_none());
        assert!(dav.log().iter().any(|l| l.starts_with("delete dv")));
    }

    // ---- invites from mail ----------------------------------------------------------------

    fn invite_from(from: &str, ics_from: &str, method: &str, st: &CalState) -> (calendar::Invite, String) {
        invite_from_auth(from, ics_from, method, st, true)
    }

    /// `verified`: the mail carries a passing DMARC result for the From domain (else no Authentication-Results at all).
    fn invite_from_auth(from: &str, ics_from: &str, method: &str, st: &CalState, verified: bool) -> (calendar::Invite, String) {
        let mut raw = testmail::zoom_style(from, &testmail::ics(method, ics_from, ME, ""));
        if verified {
            let dom = from.rsplit('@').next().unwrap();
            raw = testmail::with_auth_results(raw, &format!("dmarc=pass header.from={dom}"));
        }
        let p = mailcore::parse_message(&raw).unwrap();
        let id = p.attachments.iter().find(|a| crate::cal_invite::is_ics_part(a)).unwrap().id.clone();
        load_invite(st, &raw, ME, &id).unwrap()
    }

    #[tokio::test]
    async fn import_request_adds_event_and_spoofed_sender_is_rejected() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        // spoof: ORGANIZER says boss@, From says mallory@
        let (inv, sender) = invite_from("mallory@evil.example", "boss@corp.example", "REQUEST", &st);
        let e = import_invite(&st, &dav, &inv, &sender, &mine(), false).await.unwrap_err();
        assert!(e.contains("organizer"), "{e}");
        assert!(st.db(|c| c.find_event("evt1@corp.example")).unwrap().is_none());

        let (inv, sender) = invite_from("boss@corp.example", "boss@corp.example", "REQUEST", &st);
        let ev = import_invite(&st, &dav, &inv, &sender, &mine(), false).await.unwrap();
        assert_eq!(ev.calendar_id, DEFAULT_CALENDAR_ID);
        assert!(ev.zoom.is_none());
        assert!(st.db(|c| c.find_event("evt1@corp.example")).unwrap().is_some());
    }

    #[tokio::test]
    async fn respond_stores_my_status_and_replies_to_the_organizer() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        // spoofed invite: no reply, nothing stored
        let (inv, sender) = invite_from("mallory@evil.example", "boss@corp.example", "REQUEST", &st);
        assert!(respond_invite(&st, &dav, inv, &sender, ME, AttendeeStatus::Accepted, &mine(), false).await.is_err());
        assert!(st.db(|c| c.find_event("evt1@corp.example")).unwrap().is_none());

        let (inv, sender) = invite_from("boss@corp.example", "boss@corp.example", "REQUEST", &st);
        let (stored, mail) = respond_invite(&st, &dav, inv, &sender, ME, AttendeeStatus::Tentative, &mine(), false).await.unwrap();
        assert_eq!(itip::my_status(&stored, &mine()), Some(AttendeeStatus::Tentative));
        assert_eq!(mail.to, vec!["boss@corp.example"]);
        let part = mail.calendar.unwrap();
        assert_eq!(part.method, "REPLY");
        let flat = part.ics.replace("\r\n ", "");
        assert!(flat.contains("METHOD:REPLY") && flat.contains("PARTSTAT=TENTATIVE"), "{flat}");
        assert!(mail.subject.starts_with("Tentative:"));
        let in_db = st.db(|c| c.find_event("evt1@corp.example")).unwrap().unwrap();
        assert_eq!(itip::my_status(&in_db, &mine()), Some(AttendeeStatus::Tentative));

        // a CANCEL / REPLY is not something to respond to
        let (cancel, s2) = invite_from("boss@corp.example", "boss@corp.example", "CANCEL", &st);
        assert!(respond_invite(&st, &dav, cancel, &s2, ME, AttendeeStatus::Accepted, &mine(), false).await.unwrap_err().contains("can respond"));
    }

    #[tokio::test]
    async fn cancel_import_and_reply_application_are_sender_verified() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        let (inv, sender) = invite_from("boss@corp.example", "boss@corp.example", "REQUEST", &st);
        import_invite(&st, &dav, &inv, &sender, &mine(), false).await.unwrap();

        let (cancel, _) = invite_from("boss@corp.example", "boss@corp.example", "CANCEL", &st);
        assert!(import_invite(&st, &dav, &cancel, "mallory@evil.example", &mine(), false).await.is_err(), "spoofed cancel");
        let ev = st.db(|c| c.find_event("evt1@corp.example")).unwrap().unwrap();
        assert_eq!(ev.status, calendar::EventStatus::Confirmed);
        let done = import_invite(&st, &dav, &cancel, "boss@corp.example", &mine(), false).await.unwrap();
        assert_eq!(done.status, calendar::EventStatus::Cancelled);

        // REPLY for an event we organise: only the replier's own PARTSTAT changes; never imported as an event
        let mut mine_ev = ev.clone();
        mine_ev.uid = "own1".into();
        mine_ev.status = calendar::EventStatus::Confirmed;
        mine_ev.organizer = Some(att(ME));
        mine_ev.attendees = vec![att("bob@y.org")];
        st.db(|c| c.upsert_event(&mine_ev)).unwrap();
        let mut reply = inv.clone();
        reply.method = InviteMethod::Reply;
        reply.event = CalEvent { uid: "own1".into(), attendees: vec![Attendee { status: AttendeeStatus::Accepted, ..att("bob@y.org") }], organizer: Some(att(ME)), ..mine_ev.clone() };
        assert!(import_invite(&st, &dav, &reply, "mallory@evil.example", &mine(), false).await.is_err(), "spoofed reply");
        let upd = import_invite(&st, &dav, &reply, "bob@y.org", &mine(), false).await.unwrap();
        assert_eq!(upd.attendees[0].status, AttendeeStatus::Accepted);
        let mut orphan = reply.clone();
        orphan.event.uid = "ghost".into();
        assert!(import_invite(&st, &dav, &orphan, "bob@y.org", &mine(), false).await.is_err(), "reply to unknown event is never imported");
        assert!(st.db(|c| c.find_event("ghost")).unwrap().is_none());
    }

    // ---- zoom settings --------------------------------------------------------------------

    #[test]
    fn zoom_client_settings_split_between_db_and_keychain() {
        use crate::cal_state::{SecretTokenStore, ZOOM_CLIENT_ID_SETTING, ZOOM_SECRET_KEY};
        use calsync::TokenStore;
        let (st, s) = setup();
        assert!(zoom_set_client(&st, "  ", None).is_err());
        zoom_set_client(&st, "cid1", Some("shh")).unwrap();
        assert_eq!(st.db(|c| c.get_setting(ZOOM_CLIENT_ID_SETTING)).unwrap().as_deref(), Some("cid1"));
        assert_eq!(s.get(ZOOM_SECRET_KEY).unwrap().as_deref(), Some("shh"));
        let db_dump = serde_json::to_string(&st.db(|c| c.list_calendars()).unwrap()).unwrap();
        assert!(!db_dump.contains("shh"));
        // tokens survive a re-save of the same client id, and are dropped when the id changes
        let ts = SecretTokenStore::new(s.clone());
        ts.set("zoom.access", "tok").unwrap();
        zoom_set_client(&st, "cid1", None).unwrap();
        assert_eq!(ts.get("zoom.access").unwrap().as_deref(), Some("tok"));
        assert_eq!(s.get(ZOOM_SECRET_KEY).unwrap().as_deref(), Some("shh"), "None keeps the secret");
        zoom_set_client(&st, "cid2", Some("")).unwrap();
        assert_eq!(ts.get("zoom.access").unwrap(), None);
        assert_eq!(s.get(ZOOM_SECRET_KEY).unwrap(), None);
        assert_eq!(st.zoom_client().unwrap().redirect_uri(), "http://127.0.0.1:53682/callback");
    }

    #[test]
    fn zoom_connect_errors_carry_hints_only_where_useful() {
        let r = "http://127.0.0.1:53682/callback";
        let t = zoom_connect_error(&ZoomError::CallbackTimeout, r);
        assert!(t.contains(r) && t.contains("timed out"));
        assert!(zoom_connect_error(&ZoomError::Api("Invalid redirect".into()), r).contains(r));
        let p = zoom_connect_error(&ZoomError::PortInUse(53682), r);
        assert!(p.contains("53682") && !p.contains("Redirect URL"));
    }

    // ---- review round 2 regression tests -------------------------------------------------

    fn with_dav_cal(id: &str) -> (CalState, Arc<InMemorySecretStore>, FakeDav, FakeZoom) {
        let (st, s) = setup();
        caldav_cal(&st, &s, id, id, false, true);
        (st, s, FakeDav::default(), FakeZoom::default())
    }

    fn organised(uid: &str, cal: &str) -> CalEvent {
        let mut e = ev(uid, cal);
        e.organizer = Some(Attendee { role: AttendeeRole::Chair, status: AttendeeStatus::Accepted, ..att(ME) });
        e.attendees = vec![att("bob@y.org")];
        e
    }

    #[tokio::test]
    async fn q01_request_mail_does_not_carry_the_ineffective_flag() {
        let (st, _s, dav, z) = with_dav_cal("dv");
        let out = save_event(&st, &dav, &z, organised("q1", "dv"), true, &mine()).await.unwrap();
        let ics = out.mail.unwrap().calendar.unwrap().ics;
        assert!(!ics.contains("SCHEDULE-AGENT"), "the mail ICS stays plain: {ics}");
    }

    #[tokio::test]
    async fn q02_inbound_invites_naming_me_as_organizer_are_rejected() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        let mine2 = vec![ME.to_string(), "boss@corp.example".to_string()];
        for method in ["REQUEST", "CANCEL"] {
            let (inv, sender) = invite_from("boss@corp.example", "boss@corp.example", method, &st);
            let e = import_invite(&st, &dav, &inv, &sender, &mine2, false).await.unwrap_err();
            assert!(e.contains("names you as its organizer"), "{method}: {e}");
        }
        assert!(st.db(|c| c.find_event("evt1@corp.example")).unwrap().is_none());
    }

    #[tokio::test]
    async fn q02_request_cannot_give_an_organizer_to_an_organizer_less_event() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        let mut own = ev("evt1@corp.example", DEFAULT_CALENDAR_ID);
        own.organizer = None;
        st.db(|c| c.upsert_event(&own)).unwrap();
        let (inv, sender) = invite_from("boss@corp.example", "boss@corp.example", "REQUEST", &st);
        let e = import_invite(&st, &dav, &inv, &sender, &mine(), false).await.unwrap_err();
        assert!(e.contains("without an organizer"), "{e}");
        let stored = st.db(|c| c.get_event("evt1@corp.example", DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        assert!(stored.organizer.is_none(), "unchanged");
    }

    #[tokio::test]
    async fn q02_unverified_sender_may_add_but_not_cancel_or_update_without_confirmation() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        let (inv, sender) = invite_from_auth("boss@corp.example", "boss@corp.example", "REQUEST", &st, false);
        assert!(inv.sender_unverified);
        // a brand-new event can still be added...
        import_invite(&st, &dav, &inv, &sender, &mine(), false).await.unwrap();
        // ...but an update to it, or a cancellation, needs the user's explicit confirmation
        let e = import_invite(&st, &dav, &inv, &sender, &mine(), false).await.unwrap_err();
        assert!(e.contains("could not be verified") && e.contains("confirm"), "{e}");
        let (cancel, _) = invite_from_auth("boss@corp.example", "boss@corp.example", "CANCEL", &st, false);
        assert!(import_invite(&st, &dav, &cancel, &sender, &mine(), false).await.unwrap_err().contains("could not be verified"));
        assert_eq!(st.db(|c| c.find_event("evt1@corp.example")).unwrap().unwrap().status, calendar::EventStatus::Confirmed);
        let done = import_invite(&st, &dav, &cancel, &sender, &mine(), true).await.unwrap();
        assert_eq!(done.status, calendar::EventStatus::Cancelled);
        // a verified sender needs no confirmation
        let (st2, _s2) = setup();
        let (inv, sender) = invite_from("boss@corp.example", "boss@corp.example", "REQUEST", &st2);
        assert!(!inv.sender_unverified);
        import_invite(&st2, &dav, &inv, &sender, &mine(), false).await.unwrap();
        let (cancel, _) = invite_from("boss@corp.example", "boss@corp.example", "CANCEL", &st2);
        import_invite(&st2, &dav, &cancel, &sender, &mine(), false).await.unwrap();
    }

    #[tokio::test]
    async fn q03_discovery_owned_flag_is_persisted_and_cleared() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        *dav.discovered.lock().unwrap() = vec![
            DiscoveredCalendar { owned: false, ..disc("https://d.example/shared/", "Shared", None, false) },
            disc("https://d.example/mine/", "Mine", None, false),
        ];
        let out = add_caldav(&st, &dav, "custom", Some("https://d.example/"), "u", "pw").await.unwrap();
        let owned = |id: &str| st.db(|c| Ok(calsync::caldav::is_owned(c, id))).unwrap();
        assert!(!owned(&out[0].id) && owned(&out[1].id));
        // re-adding refreshes the flag
        *dav.discovered.lock().unwrap() = vec![disc("https://d.example/shared/", "Shared", None, false)];
        add_caldav(&st, &dav, "custom", Some("https://d.example/"), "u", "pw").await.unwrap();
        assert!(owned(&out[0].id));
        remove_calendar(&st, &out[0].id).await.unwrap();
        assert!(!owned(&out[0].id), "removed with the calendar");
        assert!(st.db(|c| c.get_setting(&format!("calsync.owned.{}", out[0].id))).unwrap().is_none());
    }

    #[tokio::test]
    async fn q03_zoom_meetings_not_created_here_are_never_touched() {
        let (st, _s) = setup();
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        let mut e = organised("zz1", DEFAULT_CALENDAR_ID);
        e.zoom = Some(ZoomMeeting { meeting_id: "777".into(), join_url: "https://zoom.us/j/777".into(), password: None, start_url: None });
        let first = save_event(&st, &dav, &z, e, false, &mine()).await.unwrap();
        let mut moved = first.event.clone();
        moved.start = "2026-11-10T15:00:00+00:00".into();
        moved.end = "2026-11-10T16:00:00+00:00".into();
        save_event(&st, &dav, &z, moved, false, &mine()).await.unwrap();
        delete_event(&st, &dav, &z, "zz1", DEFAULT_CALENDAR_ID, false, &mine()).await.unwrap();
        assert!(z.log.lock().unwrap().is_empty(), "foreign meeting id: no update, no delete: {:?}", z.log.lock().unwrap());

        // zoom_delete_meeting: only recorded ids
        assert!(zoom_delete_recorded(&st, &z, "777").await.unwrap_err().contains("created"));
        assert!(z.log.lock().unwrap().is_empty());
        st.zoom_record("888").unwrap();
        *z.delete_ok.lock().unwrap() = true;
        zoom_delete_recorded(&st, &z, "888").await.unwrap();
        assert_eq!(*z.log.lock().unwrap(), vec!["delete 888"]);
        assert!(!st.zoom_is_ours("888"), "forgotten after the delete");
        // the list is a plain, non-secret setting
        assert_eq!(st.db(|c| c.get_setting(crate::cal_state::ZOOM_CREATED_SETTING)).unwrap().as_deref(), Some("[]"));
    }

    #[tokio::test]
    async fn q04_move_carries_overrides_and_deletes_the_old_copy_only_after_the_new_push() {
        let (st, s) = setup();
        caldav_cal(&st, &s, "dv", "D", false, true);
        caldav_cal(&st, &s, "dw", "W", false, true);
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        let mut series = ev("mv1", "dv");
        series.rrule = Some("FREQ=WEEKLY".into());
        save_event(&st, &dav, &z, series.clone(), false, &mine()).await.unwrap();
        let mut inst = series.clone();
        inst.rrule = None;
        inst.summary = "Moved instance".into();
        st.db(|c| c.upsert_override(&EventOverride { recurrence_id: "2026-11-17T12:00:00+00:00".into(), event: inst })).unwrap();

        series.calendar_id = "dw".into();
        save_event(&st, &dav, &z, series, false, &mine()).await.unwrap();
        assert_eq!(st.db(|c| c.get_overrides("mv1", "dw")).unwrap().len(), 1, "overrides came along");
        assert!(st.db(|c| c.get_event("mv1", "dv")).unwrap().is_none());
        let log = dav.log();
        let push_new = log.iter().position(|l| l == "push dw mv1").expect("pushed to the new calendar");
        let del_old = log.iter().position(|l| l.starts_with("delete dv")).expect("deleted from the old calendar");
        assert!(push_new < del_old, "{log:?}");
    }

    #[tokio::test]
    async fn q04_move_with_precondition_failure_queues_a_tombstone_and_conflict_keeps_the_old_copy() {
        let (st, s) = setup();
        caldav_cal(&st, &s, "dv", "D", false, true);
        caldav_cal(&st, &s, "dw", "W", false, true);
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        save_event(&st, &dav, &z, ev("mv2", "dv"), false, &mine()).await.unwrap();
        *dav.delete_result.lock().unwrap() = Some(Err(CalDavError::PreconditionFailed));
        save_event(&st, &dav, &z, ev("mv2", "dw"), false, &mine()).await.unwrap();
        assert!(st.db(|c| c.get_event("mv2", "dv")).unwrap().is_none());
        assert_eq!(calsync::caldav::pending_changes(&st.store, "dv").unwrap(), 1, "forced deletion queued: no duplicate left behind");

        // the new copy collides on the server: server wins, the old copy is NOT removed
        *dav.delete_result.lock().unwrap() = None;
        save_event(&st, &dav, &z, ev("mv3", "dv"), false, &mine()).await.unwrap();
        *dav.push_result.lock().unwrap() = Some(Ok(PushOutcome::Conflict));
        let e = save_event(&st, &dav, &z, ev("mv3", "dw"), false, &mine()).await.unwrap_err();
        assert!(e.contains("not removed from its previous calendar"), "{e}");
        assert!(st.db(|c| c.get_event("mv3", "dv")).unwrap().is_some());
    }

    #[tokio::test]
    async fn q05_invitations_go_out_even_if_the_push_fails_and_sequence_waits_for_the_mail() {
        let (st, _s, dav, z) = with_dav_cal("dv");
        let first = save_event(&st, &dav, &z, organised("q5", "dv"), true, &mine()).await.unwrap();
        assert!(first.mail.is_some() && first.pending_sequence.is_none() && first.push_error.is_none());
        *dav.push_result.lock().unwrap() = Some(Err(CalDavError::Offline));
        let upd = save_event(&st, &dav, &z, first.event.clone(), true, &mine()).await.unwrap();
        let mail = upd.mail.expect("mail is built from the local copy");
        assert!(mail.calendar.unwrap().ics.contains("SEQUENCE:1"));
        assert!(upd.push_error.unwrap().contains("sync later"));
        assert_eq!(upd.pending_sequence, Some(1));
        let stored = || st.db(|c| c.get_event("q5", "dv")).unwrap().unwrap().sequence;
        assert_eq!(stored(), 0, "SEQUENCE is not bumped before the mail was sent");
        // the mail fails -> nothing committed, a retry builds the same number again
        let retry = save_event(&st, &dav, &z, upd.event.clone(), true, &mine()).await.unwrap();
        assert_eq!(retry.pending_sequence, Some(1));
        *dav.push_result.lock().unwrap() = None;
        commit_sequence(&st, &dav, "q5", "dv", 1).await;
        assert_eq!(stored(), 1);
        commit_sequence(&st, &dav, "q5", "dv", 1).await;
        assert_eq!(stored(), 1, "idempotent");
        // without a mail to send a push problem is still an error, as before
        *dav.push_result.lock().unwrap() = Some(Err(CalDavError::Offline));
        assert!(save_event(&st, &dav, &z, ev("q5b", "dv"), false, &mine()).await.is_err());
        // a conflict never invites from the server's version
        *dav.push_result.lock().unwrap() = Some(Ok(PushOutcome::Conflict));
        assert!(save_event(&st, &dav, &z, organised("q5", "dv"), true, &mine()).await.unwrap_err().contains("server's version was kept"));
    }

    #[tokio::test]
    async fn q06_cancel_is_sent_from_the_organizers_account() {
        let (st, _s) = setup();
        let (dav, z) = (FakeDav::default(), FakeZoom::default());
        let both = vec![ME.to_string(), "alt@x.org".to_string()];
        let mut e = organised("q6", DEFAULT_CALENDAR_ID);
        e.organizer = Some(att("alt@x.org"));
        save_event(&st, &dav, &z, e, false, &both).await.unwrap();
        let out = delete_event(&st, &dav, &z, "q6", DEFAULT_CALENDAR_ID, true, &both).await.unwrap();
        assert!(out.mail.is_some());
        assert_eq!(out.from.as_deref(), Some("alt@x.org"), "not the first account");
        assert_eq!(delete_event(&st, &dav, &z, "q6", DEFAULT_CALENDAR_ID, true, &both).await.unwrap().from, None);
    }

    #[tokio::test]
    async fn q08_writes_wait_for_the_calendar_lock() {
        use std::time::Duration;
        let (st, _s, dav, z) = with_dav_cal("dv");
        save_event(&st, &dav, &z, ev("lk", "dv"), false, &mine()).await.unwrap();
        let g = st.cal_lock("dv").lock_owned().await; // a sync of "dv" is running
        let wait = Duration::from_millis(120);
        assert!(tokio::time::timeout(wait, save_event(&st, &dav, &z, ev("lk2", "dv"), false, &mine())).await.is_err(), "save waits");
        assert!(tokio::time::timeout(wait, delete_event(&st, &dav, &z, "lk", "dv", false, &mine())).await.is_err(), "delete waits");
        assert!(tokio::time::timeout(wait, remove_calendar(&st, "dv")).await.is_err(), "remove waits");
        assert!(tokio::time::timeout(wait, sync_calendars(&st, &dav, Some("dv"))).await.is_err(), "sync waits for a write too");
        assert!(st.db(|c| c.get_event("lk", "dv")).unwrap().is_some() && st.db(|c| c.get_event("lk2", "dv")).unwrap().is_none());
        drop(g);
        save_event(&st, &dav, &z, ev("lk2", "dv"), false, &mine()).await.unwrap();
        // another calendar is never blocked
        let g = st.cal_lock("dv").lock_owned().await;
        save_event(&st, &dav, &z, ev("lk3", DEFAULT_CALENDAR_ID), false, &mine()).await.unwrap();
        drop(g);
    }

    #[test]
    fn q11_zoom_client_cannot_be_changed_while_a_sign_in_is_in_flight() {
        let (st, _s) = setup();
        st.zoom_connecting.store(true, std::sync::atomic::Ordering::SeqCst);
        let e = zoom_set_client(&st, "cid", Some("sec")).unwrap_err();
        assert!(e.contains("in progress"), "{e}");
        assert!(st.db(|c| c.get_setting(crate::cal_state::ZOOM_CLIENT_ID_SETTING)).unwrap().is_none(), "nothing written");
        st.zoom_connecting.store(false, std::sync::atomic::Ordering::SeqCst);
        zoom_set_client(&st, "cid", None).unwrap();
    }

    fn instance_reply(master: &CalEvent, rid: &str, who: &str) -> Invite {
        let mut inst = master.clone();
        inst.rrule = None;
        inst.start = rid.into();
        inst.attendees = vec![Attendee { status: AttendeeStatus::Accepted, ..att(who) }];
        let ics = calendar::build_ics_series(None, &[EventOverride { recurrence_id: rid.into(), event: inst }], Some("REPLY"));
        itip::invite_from_ics(ics.as_bytes(), &[], "a", false).unwrap().unwrap()
    }

    #[tokio::test]
    async fn q14_reply_to_a_never_overridden_instance_creates_the_override_from_the_master() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        let mut master = organised("ser1", DEFAULT_CALENDAR_ID);
        master.rrule = Some("FREQ=WEEKLY".into());
        st.db(|c| c.upsert_event(&master)).unwrap();
        let rid = "2026-11-17T12:00:00+00:00";
        let inv = instance_reply(&master, rid, "bob@y.org");
        assert_eq!(inv.method, InviteMethod::Reply);
        import_invite(&st, &dav, &inv, "bob@y.org", &mine(), false).await.unwrap();
        let ovs = st.db(|c| c.get_overrides("ser1", DEFAULT_CALENDAR_ID)).unwrap();
        assert_eq!(ovs.len(), 1);
        assert_eq!(ovs[0].event.attendees.iter().find(|a| a.email == "bob@y.org").unwrap().status, AttendeeStatus::Accepted);
        assert_eq!((ovs[0].event.start.as_str(), ovs[0].event.end.as_str()), (rid, "2026-11-17T13:00:00+00:00"), "times from the master, not the mail");
        // the master itself is untouched
        let m = st.db(|c| c.get_event("ser1", DEFAULT_CALENDAR_ID)).unwrap().unwrap();
        assert_eq!(m.attendees[0].status, AttendeeStatus::NeedsAction);
        // the same instance in another offset notation finds the existing override (rid_eq) instead of adding a second one
        let inv2 = instance_reply(&master, "2026-11-17T17:30:00+05:30", "bob@y.org");
        import_invite(&st, &dav, &inv2, "bob@y.org", &mine(), false).await.unwrap();
        assert_eq!(st.db(|c| c.get_overrides("ser1", DEFAULT_CALENDAR_ID)).unwrap().len(), 1);
        // an instance of a non-recurring event / a stranger are still refused
        let mut plain = organised("one1", DEFAULT_CALENDAR_ID);
        plain.rrule = None;
        st.db(|c| c.upsert_event(&plain)).unwrap();
        assert!(import_invite(&st, &dav, &instance_reply(&plain, rid, "bob@y.org"), "bob@y.org", &mine(), false).await.is_err());
        assert!(import_invite(&st, &dav, &instance_reply(&master, "2026-11-24T12:00:00+00:00", "bob@y.org"), "mallory@evil.example", &mine(), false).await.is_err());
    }

    #[tokio::test]
    async fn q15_a_rejected_password_stops_syncing_until_the_calendar_is_added_again() {
        let (st, s) = setup();
        caldav_cal(&st, &s, "a", "A", false, true);
        let dav = FakeDav::default();
        *dav.sync_err.lock().unwrap() = Some(CalDavError::Unauthorized);
        let out = sync_calendars(&st, &dav, None).await.unwrap();
        assert_eq!(out.errors.len(), 1);
        assert!(st.signin_failed("a"));
        *dav.sync_err.lock().unwrap() = None;
        let out = sync_calendars(&st, &dav, None).await.unwrap();
        assert_eq!(dav.log().iter().filter(|l| l.starts_with("sync")).count(), 1, "the server is not contacted again");
        assert!(out.errors[0].contains("rejected the saved password"), "{:?}", out.errors);
        assert!(st.calendar("a").unwrap().error.unwrap().contains("rejected"));
        // saves are queued locally instead of hitting the server
        let z = FakeZoom::default();
        let e = save_event(&st, &dav, &z, ev("u", "a"), false, &mine()).await.unwrap_err();
        assert!(e.contains("rejected the saved password"), "{e}");
        assert!(dav.log().iter().all(|l| !l.starts_with("push")));
        assert_eq!(calsync::caldav::pending_changes(&st.store, "a").unwrap(), 1);
        // re-entering the password (adding the same calendar again) clears the marker
        *dav.discovered.lock().unwrap() = vec![disc("https://dav.example/a/", "A", None, false)];
        add_caldav(&st, &dav, "custom", Some("https://dav.example/a/"), "u", "new-pw").await.unwrap();
        assert!(!st.signin_failed("a"));
        sync_calendars(&st, &dav, None).await.unwrap();
        assert_eq!(dav.log().iter().filter(|l| l.starts_with("sync")).count(), 2);
        // marker is a plain setting (no secrets), gone with the calendar
        st.set_signin_failed("a");
        remove_calendar(&st, "a").await.unwrap();
        assert!(!st.signin_failed("a"));
    }

    #[tokio::test]
    async fn q16_the_same_reply_is_sent_once_per_session() {
        let (st, _s) = setup();
        let dav = FakeDav::default();
        let (inv, sender) = invite_from("boss@corp.example", "boss@corp.example", "REQUEST", &st);
        respond_invite(&st, &dav, inv.clone(), &sender, ME, AttendeeStatus::Accepted, &mine(), false).await.unwrap();
        let e = respond_invite(&st, &dav, inv.clone(), &sender, ME, AttendeeStatus::Accepted, &mine(), false).await.unwrap_err();
        assert!(e.contains("already sent"), "{e}");
        // changing your mind is a different reply
        respond_invite(&st, &dav, inv.clone(), &sender, ME, AttendeeStatus::Declined, &mine(), false).await.unwrap();
        // a reply whose mail could not be sent can be retried (the command forgets the key)
        st.forget_reply(&reply_key(&inv, ME, AttendeeStatus::Tentative));
        respond_invite(&st, &dav, inv.clone(), &sender, ME, AttendeeStatus::Tentative, &mine(), false).await.unwrap();
        st.forget_reply(&reply_key(&inv, ME, AttendeeStatus::Tentative));
        respond_invite(&st, &dav, inv, &sender, ME, AttendeeStatus::Tentative, &mine(), false).await.unwrap();
    }
}
