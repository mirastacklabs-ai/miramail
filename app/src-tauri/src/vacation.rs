//! Server-side vacation auto-responder (ManageSieve). Thin Tauri layer over `mailcore::sieve`.
//! Connects only to the account's own mail server (same host as IMAP unless overridden in the UI's Advanced box).

use crate::state::{err, AppState};
use mailcore::sieve::{self, DisableOutcome, EnableOutcome, SieveTarget, SieveTls, VacationSettings, VacationStatus};
use mailcore::AccountConfig;
use serde::Deserialize;
use tauri::State;

type CmdResult<T> = Result<T, String>;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SieveOverride {
    pub host: Option<String>,
    pub port: Option<u16>,
    /// "implicit" | "starttls" (default starttls).
    pub tls: Option<String>,
}

/// An override may only name a host that already belongs to this account: its IMAP or SMTP host, or its mail
/// domain (or a subdomain of it). Anything else is refused so the Keychain password cannot be sent elsewhere.
fn host_allowed(cfg: &AccountConfig, host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    let same = |x: &str| x.trim_end_matches('.').eq_ignore_ascii_case(&h);
    if same(&cfg.imap.host) || same(&cfg.smtp.host) {
        return true;
    }
    let Some((_, domain)) = cfg.email.rsplit_once('@') else { return false };
    let d = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    !d.is_empty() && (h == d || h.ends_with(&format!(".{d}")))
}

pub fn target_for(cfg: &AccountConfig, o: Option<&SieveOverride>) -> CmdResult<SieveTarget> {
    let o = o.cloned().unwrap_or_default();
    let host = match o.host.map(|h| h.trim().to_owned()).filter(|h| !h.is_empty()) {
        Some(h) if host_allowed(cfg, &h) => h,
        Some(_) => return Err("For safety, the automatic-reply server must be your mail server or a host under your email domain. Your password is never sent anywhere else.".into()),
        None => cfg.imap.host.clone(),
    };
    Ok(SieveTarget {
        host,
        port: o.port.filter(|p| *p != 0).unwrap_or(sieve::DEFAULT_PORT),
        tls: if o.tls.as_deref() == Some("implicit") { SieveTls::Implicit } else { SieveTls::StartTls },
    })
}

fn setup(state: &AppState, account: &str, o: Option<&SieveOverride>) -> CmdResult<(SieveTarget, String, String)> {
    let cfg = state.account(account)?;
    // Validate the target before the password is even read from the Keychain.
    let t = target_for(&cfg, o)?;
    let pw = state.password(account)?;
    Ok((t, cfg.username, pw))
}

#[tauri::command]
pub async fn vacation_status(state: State<'_, AppState>, account: String, server: Option<SieveOverride>) -> CmdResult<VacationStatus> {
    let (t, u, p) = setup(&state, &account, server.as_ref())?;
    sieve::vacation_status(&t, &u, &p).await.map_err(err)
}

#[tauri::command]
pub async fn vacation_enable(
    state: State<'_, AppState>,
    account: String,
    settings: VacationSettings,
    confirm_replace: bool,
    server: Option<SieveOverride>,
) -> CmdResult<EnableOutcome> {
    let (t, u, p) = setup(&state, &account, server.as_ref())?;
    sieve::vacation_enable(&t, &u, &p, &settings, confirm_replace).await.map_err(err)
}

#[tauri::command]
pub async fn vacation_disable(
    state: State<'_, AppState>,
    account: String,
    restore_previous: bool,
    server: Option<SieveOverride>,
) -> CmdResult<DisableOutcome> {
    let (t, u, p) = setup(&state, &account, server.as_ref())?;
    sieve::vacation_disable(&t, &u, &p, restore_previous).await.map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::{ServerConfig, Tls};

    fn cfg() -> AccountConfig {
        let s = |h: &str, p| ServerConfig { host: h.into(), port: p, tls: Tls::Implicit };
        AccountConfig { email: "a@x.org".into(), imap: s("imap.x.org", 993), smtp: s("smtp.x.org", 465), username: "a@x.org".into() }
    }

    #[test]
    fn default_target_is_imap_host_on_4190_starttls() {
        let t = target_for(&cfg(), None).unwrap();
        assert_eq!((t.host.as_str(), t.port, t.tls), ("imap.x.org", 4190, SieveTls::StartTls));
    }

    #[test]
    fn override_applies_and_blank_host_falls_back() {
        let o = SieveOverride { host: Some(" sieve.x.org ".into()), port: Some(5190), tls: Some("implicit".into()) };
        let t = target_for(&cfg(), Some(&o)).unwrap();
        assert_eq!((t.host.as_str(), t.port, t.tls), ("sieve.x.org", 5190, SieveTls::Implicit));
        let o = SieveOverride { host: Some("  ".into()), port: Some(0), tls: None };
        let t = target_for(&cfg(), Some(&o)).unwrap();
        assert_eq!((t.host.as_str(), t.port), ("imap.x.org", 4190));
    }

    #[test]
    fn foreign_hosts_are_refused() {
        for h in ["evil.example", "x.org.evil.example", "notx.org", "evilx.org", "127.0.0.1", "sieve.x.org.evil.com"] {
            let o = SieveOverride { host: Some(h.into()), ..Default::default() };
            assert!(target_for(&cfg(), Some(&o)).is_err(), "{h}");
        }
        for h in ["x.org", "IMAP.X.ORG", "smtp.x.org", "mail.sub.x.org", "x.org."] {
            let o = SieveOverride { host: Some(h.into()), ..Default::default() };
            assert!(target_for(&cfg(), Some(&o)).is_ok(), "{h}");
        }
    }
}
