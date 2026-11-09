//! Mail safety report for the open message (docs/SAFETY_CHECKS.md, Tier 1).
//!
//! Read-only and local: it parses the cached raw message with `mailcore::safety` and counts earlier mail from the
//! sender in the local cache. No network, no AI, no write. The report is plain data (codes, severities, plain-text
//! sentences); the Reader renders it as text, never as HTML.

use crate::composer::ComposeStore;
use crate::state::{err, AppState};
use mailcore::safety::{analyze, build_input, SafetyReport};
use tauri::State;

type CmdResult<T> = Result<T, String>;

/// Safety report for one cached message. Fails (quietly, the Reader just shows no banner) when the body is not cached.
#[tauri::command]
pub fn safety_report(state: State<'_, AppState>, compose: State<'_, ComposeStore>, account: String, folder: String, uid: u32) -> CmdResult<SafetyReport> {
    let raw = state.storage.get_body(&account, &folder, uid).map_err(err)?.ok_or("This message isn't downloaded yet.")?;
    let parsed = mailcore::parse_message(&raw).map_err(err)?;
    let extra = mailcore::mime::parse_extra_headers(&raw);
    let hosts = crate::organise::auth_hosts(&state.storage, &account);
    let mut own: Vec<String> = state.storage.list_accounts().map_err(err)?.into_iter().map(|a| a.email).collect();
    own.extend(compose.list_identities().unwrap_or_default().into_iter().filter(|i| i.account == account).map(|i| i.address));
    let earlier = parsed.headers.from.first().and_then(|f| state.storage.count_from(&f.address).ok()).map(|(n, _)| n);
    Ok(analyze(&build_input(&parsed, &extra, &own, &hosts, earlier)))
}
