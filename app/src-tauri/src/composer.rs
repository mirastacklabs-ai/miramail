//! Composer backend: signatures, From identities, address suggestions and raw-message fetch for "Forward as attachment".
//! Signatures and identities live in `workspace.db` (own tables, same pattern as drafts). Signature text and
//! HTML are user content: never logged. The frontend never chooses the From header freely: `resolve_from`
//! only accepts the account's own address or an identity the user saved for that same account.

use crate::state::{err, AppState};
use crate::wire::{parse_addresses, AttachmentData, MAX_ATTACH_BYTES};
use mailcore::addrbook::AddrRow;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use tauri::State;

const MAX_SIGNATURES: i64 = 200;
const MAX_IDENTITIES: i64 = 100;
const MAX_SIG_TEXT: usize = 20_000;
const MAX_SIG_HTML: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// Rich signatures carry `html` (sanitised on save) plus the plain-text rendering in `text`.
    #[serde(default)]
    pub rich: bool,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub html: String,
    /// "above" or "below" the quoted text in replies / forwards.
    #[serde(default = "below")]
    pub placement: String,
    /// Accounts this signature is offered for; empty means every account.
    #[serde(default)]
    pub accounts: Vec<String>,
}

fn below() -> String {
    "below".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SigDefaults {
    pub account: String,
    #[serde(default)]
    pub new_id: Option<String>,
    #[serde(default)]
    pub reply_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    #[serde(default)]
    pub id: String,
    pub account: String,
    #[serde(default)]
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub reply_to: String,
    #[serde(default)]
    pub signature_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposerData {
    pub signatures: Vec<Signature>,
    pub defaults: Vec<SigDefaults>,
    pub identities: Vec<Identity>,
}

pub struct ComposeStore {
    conn: Mutex<Connection>,
}

static SEQ: AtomicU64 = AtomicU64::new(1);

fn new_id(prefix: &str) -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("{prefix}{ms:x}{:x}", SEQ.fetch_add(1, Ordering::Relaxed))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A plain `local@domain.tld` address: no whitespace, angle brackets, quotes, commas or control characters.
pub fn plain_address(a: &str) -> Option<String> {
    let a = a.trim();
    let ok = a.len() <= 254
        && a.matches('@').count() == 1
        && !a.starts_with('@')
        && !a.ends_with('@')
        && a.rsplit('@').next().is_some_and(|d| d.contains('.') && !d.starts_with('.') && !d.ends_with('.'))
        && a.chars().all(|c| c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | ',' | ';' | '(' | ')' | '[' | ']' | '\\'));
    ok.then(|| a.to_string())
}

fn clean_display_name(n: &str) -> String {
    n.chars().filter(|c| !c.is_control() && !matches!(c, '<' | '>' | '"' | '\\')).collect::<String>().trim().chars().take(100).collect()
}

impl ComposeStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let c = Connection::open(path).map_err(|e| e.to_string())?;
        c.busy_timeout(std::time::Duration::from_secs(3)).map_err(|e| e.to_string())?;
        Self::from_conn(c)
    }
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        Self::from_conn(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }
    fn from_conn(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS signatures(
                id TEXT PRIMARY KEY, name TEXT NOT NULL, rich INTEGER NOT NULL, text TEXT NOT NULL, html TEXT NOT NULL,
                placement TEXT NOT NULL, accounts TEXT NOT NULL, position INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS sig_defaults(account TEXT PRIMARY KEY, new_id TEXT, reply_id TEXT);
             CREATE TABLE IF NOT EXISTS identities(
                id TEXT PRIMARY KEY, account TEXT NOT NULL, name TEXT NOT NULL, address TEXT NOT NULL,
                reply_to TEXT NOT NULL, signature_id TEXT, position INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS identities_account ON identities(account);",
        )
        .map_err(|e| e.to_string())?;
        Ok(ComposeStore { conn: Mutex::new(conn) })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "Signature storage is unavailable.".to_string())
    }

    pub fn list_signatures(&self) -> Result<Vec<Signature>, String> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT id,name,rich,text,html,placement,accounts FROM signatures ORDER BY position, id").map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| {
                Ok(Signature {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    rich: r.get::<_, i64>(2)? != 0,
                    text: r.get(3)?,
                    html: r.get(4)?,
                    placement: r.get(5)?,
                    accounts: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Validate and upsert; returns the stored signature (new id when `id` was empty).
    pub fn save_signature(&self, s: &Signature) -> Result<Signature, String> {
        let name: String = s.name.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(80).collect();
        if name.is_empty() {
            return Err("Give the signature a name.".into());
        }
        if s.text.chars().count() > MAX_SIG_TEXT {
            return Err("This signature is too long.".into());
        }
        if s.html.len() > MAX_SIG_HTML * 2 {
            return Err("This signature is too large.".into());
        }
        if !matches!(s.placement.as_str(), "above" | "below") {
            return Err("Placement must be above or below the quoted text.".into());
        }
        if s.accounts.len() > 50 || s.accounts.iter().any(|a| a.is_empty() || a.len() > 320) {
            return Err("Invalid account list.".into());
        }
        let html = if s.rich { mailcore::sanitize_compose_html(&s.html) } else { String::new() };
        if html.len() > MAX_SIG_HTML {
            return Err("This signature is too large (limit 1 MB, including pictures).".into());
        }
        let accounts = serde_json::to_string(&s.accounts).map_err(|e| e.to_string())?;
        let c = self.lock()?;
        let id = if s.id.is_empty() { new_id("s") } else { s.id.clone() };
        if !valid_id(&id) {
            return Err("Invalid signature id.".into());
        }
        let exists = c.query_row("SELECT 1 FROM signatures WHERE id=?1", [&id], |_| Ok(())).optional().map_err(|e| e.to_string())?.is_some();
        if exists {
            c.execute(
                "UPDATE signatures SET name=?2, rich=?3, text=?4, html=?5, placement=?6, accounts=?7 WHERE id=?1",
                params![id, name, s.rich as i64, s.text, html, s.placement, accounts],
            )
            .map_err(|e| e.to_string())?;
        } else {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM signatures", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            if n >= MAX_SIGNATURES {
                return Err("Too many signatures. Delete some first.".into());
            }
            let pos: i64 = c.query_row("SELECT COALESCE(MAX(position),0)+1 FROM signatures", [], |r| r.get(0)).map_err(|e| e.to_string())?;
            c.execute(
                "INSERT INTO signatures(id,name,rich,text,html,placement,accounts,position) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![id, name, s.rich as i64, s.text, html, s.placement, accounts, pos],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(Signature { id, name, rich: s.rich, text: s.text.clone(), html, placement: s.placement.clone(), accounts: s.accounts.clone() })
    }

    /// Delete a signature and every default / identity reference to it.
    pub fn delete_signature(&self, id: &str) -> Result<(), String> {
        let c = self.lock()?;
        c.execute("DELETE FROM signatures WHERE id=?1", [id]).map_err(|e| e.to_string())?;
        c.execute("UPDATE sig_defaults SET new_id=NULL WHERE new_id=?1", [id]).map_err(|e| e.to_string())?;
        c.execute("UPDATE sig_defaults SET reply_id=NULL WHERE reply_id=?1", [id]).map_err(|e| e.to_string())?;
        c.execute("UPDATE identities SET signature_id=NULL WHERE signature_id=?1", [id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_defaults(&self) -> Result<Vec<SigDefaults>, String> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT account,new_id,reply_id FROM sig_defaults ORDER BY account").map_err(|e| e.to_string())?;
        let rows = st.query_map([], |r| Ok(SigDefaults { account: r.get(0)?, new_id: r.get(1)?, reply_id: r.get(2)? })).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    pub fn set_defaults(&self, d: &SigDefaults) -> Result<(), String> {
        if d.account.is_empty() || d.account.len() > 320 {
            return Err("Invalid account.".into());
        }
        let c = self.lock()?;
        for id in [&d.new_id, &d.reply_id].into_iter().flatten() {
            let ok = c.query_row("SELECT 1 FROM signatures WHERE id=?1", [id], |_| Ok(())).optional().map_err(|e| e.to_string())?.is_some();
            if !ok {
                return Err("That signature no longer exists.".into());
            }
        }
        c.execute(
            "INSERT INTO sig_defaults(account,new_id,reply_id) VALUES (?1,?2,?3)
             ON CONFLICT(account) DO UPDATE SET new_id=excluded.new_id, reply_id=excluded.reply_id",
            params![d.account, d.new_id, d.reply_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_identities(&self) -> Result<Vec<Identity>, String> {
        let c = self.lock()?;
        let mut st = c.prepare("SELECT id,account,name,address,reply_to,signature_id FROM identities ORDER BY account, position, id").map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| Ok(Identity { id: r.get(0)?, account: r.get(1)?, name: r.get(2)?, address: r.get(3)?, reply_to: r.get(4)?, signature_id: r.get(5)? }))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }

    /// Validate and upsert an identity of `account` (the caller has checked the account exists).
    pub fn save_identity(&self, i: &Identity) -> Result<Identity, String> {
        let address = plain_address(&i.address).ok_or("That doesn't look like an email address.")?;
        let reply_to = if i.reply_to.trim().is_empty() { String::new() } else { plain_address(&i.reply_to).ok_or("The reply-to address doesn't look like an email address.")? };
        let name = clean_display_name(&i.name);
        let c = self.lock()?;
        if let Some(sid) = &i.signature_id {
            let ok = c.query_row("SELECT 1 FROM signatures WHERE id=?1", [sid], |_| Ok(())).optional().map_err(|e| e.to_string())?.is_some();
            if !ok {
                return Err("That signature no longer exists.".into());
            }
        }
        let id = if i.id.is_empty() { new_id("i") } else { i.id.clone() };
        if !valid_id(&id) {
            return Err("Invalid identity id.".into());
        }
        // The same address may not be saved twice for one account.
        let dup = c
            .query_row("SELECT 1 FROM identities WHERE account=?1 AND lower(address)=lower(?2) AND id<>?3", params![i.account, address, id], |_| Ok(()))
            .optional()
            .map_err(|e| e.to_string())?
            .is_some();
        if dup {
            return Err("You already have an identity with that address.".into());
        }
        let exists = c.query_row("SELECT account FROM identities WHERE id=?1", [&id], |r| r.get::<_, String>(0)).optional().map_err(|e| e.to_string())?;
        match exists {
            Some(acc) if acc != i.account => return Err("An identity cannot move to another account.".into()),
            Some(_) => {
                c.execute("UPDATE identities SET name=?2, address=?3, reply_to=?4, signature_id=?5 WHERE id=?1", params![id, name, address, reply_to, i.signature_id])
                    .map_err(|e| e.to_string())?;
            }
            None => {
                let n: i64 = c.query_row("SELECT COUNT(*) FROM identities", [], |r| r.get(0)).map_err(|e| e.to_string())?;
                if n >= MAX_IDENTITIES {
                    return Err("Too many identities. Delete some first.".into());
                }
                let pos: i64 = c.query_row("SELECT COALESCE(MAX(position),0)+1 FROM identities", [], |r| r.get(0)).map_err(|e| e.to_string())?;
                c.execute(
                    "INSERT INTO identities(id,account,name,address,reply_to,signature_id,position) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![id, i.account, name, address, reply_to, i.signature_id, pos],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        Ok(Identity { id, account: i.account.clone(), name, address, reply_to, signature_id: i.signature_id.clone() })
    }

    pub fn delete_identity(&self, id: &str) -> Result<(), String> {
        self.lock()?.execute("DELETE FROM identities WHERE id=?1", [id]).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Account removal: drop its identities and signature defaults.
    pub fn delete_for_account(&self, account: &str) -> Result<(), String> {
        let c = self.lock()?;
        c.execute("DELETE FROM identities WHERE account=?1", [account]).map_err(|e| e.to_string())?;
        c.execute("DELETE FROM sig_defaults WHERE account=?1", [account]).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// The From header (and optional Reply-To) a message will be sent with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFrom {
    /// `"Name" <addr>` or a bare address.
    pub mailbox: String,
    pub reply_to: Option<String>,
}

/// Choose the sender. `requested` is what the frontend asked for; only the account's own address or a saved
/// identity of that account is honoured, and display name / reply-to always come from the stored identity.
pub fn resolve_from(account_email: &str, identities: &[Identity], requested: Option<&str>) -> Result<ResolvedFrom, String> {
    let want = requested.map(str::trim).filter(|s| !s.is_empty()).unwrap_or(account_email);
    let mine: Vec<&Identity> = identities.iter().filter(|i| i.account == account_email).collect();
    let found = mine.iter().find(|i| i.address.eq_ignore_ascii_case(want)).copied();
    let is_primary = want.eq_ignore_ascii_case(account_email);
    if !is_primary && found.is_none() {
        return Err("That sender address isn't set up for this account. Add it under Preferences > Signatures > Identities.".into());
    }
    let address = if is_primary { account_email.to_string() } else { found.map(|i| i.address.clone()).unwrap_or_default() };
    let name = found.map(|i| i.name.as_str()).unwrap_or("");
    let mailbox = if name.is_empty() { address } else { format!("\"{name}\" <{address}>") };
    let reply_to = found.map(|i| i.reply_to.clone()).filter(|r| !r.is_empty());
    Ok(ResolvedFrom { mailbox, reply_to })
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    pub name: Option<String>,
    pub address: String,
}

/// Rank address-book candidates from cached message rows. Addresses you wrote to count most, then senders;
/// own addresses are excluded; prefix matches come before substring matches.
pub fn build_suggestions(rows: &[AddrRow], own: &[String], needle: &str, limit: usize) -> Vec<Suggestion> {
    let n = needle.trim().to_lowercase();
    if n.is_empty() {
        return vec![];
    }
    let own: Vec<String> = own.iter().map(|a| a.to_lowercase()).collect();
    struct Acc {
        name: Option<String>,
        score: f64,
        newest: i64,
    }
    let mut map: HashMap<String, Acc> = HashMap::new();
    for r in rows {
        let sent = r.role == "sent";
        let mut add = |list: &str, w: f64| {
            for a in parse_addresses(list) {
                let key = a.address.to_lowercase();
                if own.contains(&key) || !key.contains('@') {
                    continue;
                }
                let e = map.entry(key).or_insert(Acc { name: None, score: 0.0, newest: 0 });
                e.score += w;
                if r.date >= e.newest {
                    e.newest = r.date;
                    if a.name.is_some() {
                        e.name = a.name.clone();
                    }
                } else if e.name.is_none() {
                    e.name = a.name.clone();
                }
            }
        };
        if sent {
            add(&r.to, 3.0);
            add(&r.cc, 2.0);
        } else {
            add(&r.from, 1.0);
            add(&r.to, 0.2);
            add(&r.cc, 0.2);
        }
    }
    let mut hits: Vec<(String, Acc, bool)> = map
        .into_iter()
        .filter_map(|(addr, a)| {
            let nm = a.name.as_deref().unwrap_or("").to_lowercase();
            let matches = addr.contains(&n) || nm.contains(&n);
            let prefix = addr.starts_with(&n) || nm.split_whitespace().any(|w| w.starts_with(&n)) || nm.starts_with(&n);
            matches.then_some((addr, a, prefix))
        })
        .collect();
    hits.sort_by(|x, y| y.2.cmp(&x.2).then(y.1.score.partial_cmp(&x.1.score).unwrap_or(std::cmp::Ordering::Equal)).then(y.1.newest.cmp(&x.1.newest)).then(x.0.cmp(&y.0)));
    hits.into_iter().take(limit).map(|(address, a, _)| Suggestion { name: a.name, address }).collect()
}

// ---- commands ----

#[tauri::command]
pub async fn composer_data(store: State<'_, ComposeStore>) -> Result<ComposerData, String> {
    Ok(ComposerData { signatures: store.list_signatures()?, defaults: store.list_defaults()?, identities: store.list_identities()? })
}
#[tauri::command]
pub async fn save_signature(store: State<'_, ComposeStore>, signature: Signature) -> Result<Signature, String> {
    store.save_signature(&signature)
}
#[tauri::command]
pub async fn delete_signature(store: State<'_, ComposeStore>, id: String) -> Result<(), String> {
    store.delete_signature(&id)
}
#[tauri::command]
pub async fn set_signature_defaults(store: State<'_, ComposeStore>, defaults: SigDefaults) -> Result<(), String> {
    store.set_defaults(&defaults)
}
#[tauri::command]
pub async fn save_identity(state: State<'_, AppState>, store: State<'_, ComposeStore>, identity: Identity) -> Result<Identity, String> {
    state.account(&identity.account)?;
    store.save_identity(&identity)
}
#[tauri::command]
pub async fn delete_identity(store: State<'_, ComposeStore>, id: String) -> Result<(), String> {
    store.delete_identity(&id)
}

/// Sanitise HTML pasted into the editor (same policy the send path applies).
#[tauri::command]
pub async fn sanitize_compose(html: String) -> Result<String, String> {
    if html.len() > mailcore::MAX_COMPOSE_HTML {
        return Err("That's too large to paste.".into());
    }
    Ok(mailcore::sanitize_compose_html(&html))
}

/// Address-book suggestions from the local cache (no network). With `account` (the account being composed from)
/// only that account's cached mail is used.
#[tauri::command]
pub async fn suggest_addresses(state: State<'_, AppState>, store: State<'_, ComposeStore>, prefix: String, limit: Option<u32>, account: Option<String>) -> Result<Vec<Suggestion>, String> {
    if prefix.trim().chars().count() < 1 || prefix.len() > 200 {
        return Ok(vec![]);
    }
    let rows = state.storage.address_rows_for(account.as_deref().filter(|a| !a.is_empty()), &prefix, 4000).map_err(err)?;
    let mut own: Vec<String> = state.storage.list_accounts().map_err(err)?.into_iter().map(|a| a.email).collect();
    own.extend(store.list_identities()?.into_iter().map(|i| i.address));
    Ok(build_suggestions(&rows, &own, &prefix, limit.unwrap_or(8).clamp(1, 25) as usize))
}

/// The raw RFC 822 bytes of a cached/fetched message as a `message/rfc822` attachment (Forward as attachment).
#[tauri::command]
pub async fn get_raw_message(state: State<'_, AppState>, account: String, folder: String, uid: u32, uidvalidity: Option<u32>) -> Result<AttachmentData, String> {
    use base64::Engine as _;
    crate::commands::check_original(&state, &account, &folder, uid, uidvalidity)?;
    // Refuse oversize from the cached summary (RFC822.SIZE) before fetching or allocating the body.
    let summary = state.storage.get_message(&account, &folder, uid).map_err(err)?;
    if summary.as_ref().is_some_and(|m| m.size as usize > MAX_ATTACH_BYTES) {
        return Err(RAW_TOO_BIG.into());
    }
    let mut conn = None;
    let raw = crate::commands::raw_message(&state, &mut conn, &account, &folder, uid).await;
    if let Some(c) = conn {
        c.close().await;
    }
    let raw = raw?;
    if raw.len() > MAX_ATTACH_BYTES {
        return Err(RAW_TOO_BIG.into());
    }
    let subject = summary.map(|m| m.subject).unwrap_or_default();
    Ok(AttachmentData { name: eml_name(&subject), mime: "message/rfc822".into(), data_base64: base64::engine::general_purpose::STANDARD.encode(raw) })
}

const RAW_TOO_BIG: &str = "This message is over 20 MB, too large to forward as an attachment.";

/// File name for a forwarded message: the cleaned subject (no path parts, control or bidi characters) plus `.eml`.
fn eml_name(subject: &str) -> String {
    let clean = crate::textsafe::clean_display(subject);
    let base = if clean.trim().is_empty() { "message".to_string() } else { crate::wire::clean_attachment_name(&clean) };
    format!("{base}.eml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(name: &str) -> Signature {
        Signature { id: String::new(), name: name.into(), rich: false, text: "Ada\nExample Ltd".into(), html: String::new(), placement: "below".into(), accounts: vec![] }
    }
    fn ident(account: &str, address: &str) -> Identity {
        Identity { id: String::new(), account: account.into(), name: "Ada L".into(), address: address.into(), reply_to: String::new(), signature_id: None }
    }

    #[test]
    fn forward_eml_name_is_clean() {
        assert_eq!(eml_name("Re: plan"), "Re: plan.eml");
        assert_eq!(eml_name("  "), "message.eml");
        assert_eq!(eml_name("\u{202E}\u{200B}"), "message.eml");
        assert_eq!(eml_name("a/../b\u{202E}fdp\u{FEFF}"), "bfdp.eml");
    }

    #[test]
    fn signature_crud_roundtrip() {
        let s = ComposeStore::open_in_memory().unwrap();
        let a = s.save_signature(&sig("Work")).unwrap();
        assert!(!a.id.is_empty());
        let mut rich = sig("Fancy");
        rich.rich = true;
        rich.html = "<p><b>Ada</b><script>alert(1)</script><img src=\"https://t.example/p.gif\"></p>".into();
        rich.accounts = vec!["me@x.org".into()];
        let b = s.save_signature(&rich).unwrap();
        assert!(!b.html.contains("script") && !b.html.contains("t.example") && b.html.contains("<b>Ada</b>"), "{}", b.html);
        let all = s.list_signatures().unwrap();
        assert_eq!(all.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["Work", "Fancy"]);
        assert_eq!(all[1].accounts, vec!["me@x.org"]);
        // update in place keeps order and id
        let mut a2 = a.clone();
        a2.name = "Work 2".into();
        a2.placement = "above".into();
        s.save_signature(&a2).unwrap();
        let all = s.list_signatures().unwrap();
        assert_eq!((all[0].id.as_str(), all[0].name.as_str(), all[0].placement.as_str()), (a.id.as_str(), "Work 2", "above"));
        // validation
        assert!(s.save_signature(&sig("  ")).is_err());
        let mut bad = sig("x");
        bad.placement = "left".into();
        assert!(s.save_signature(&bad).is_err());
        let mut bad = sig("x");
        bad.id = "a b".into();
        assert!(s.save_signature(&bad).is_err());
    }

    #[test]
    fn delete_signature_clears_defaults_and_identity_links() {
        let s = ComposeStore::open_in_memory().unwrap();
        let a = s.save_signature(&sig("A")).unwrap();
        let b = s.save_signature(&sig("B")).unwrap();
        s.set_defaults(&SigDefaults { account: "me@x.org".into(), new_id: Some(a.id.clone()), reply_id: Some(b.id.clone()) }).unwrap();
        let mut i = ident("me@x.org", "alias@x.org");
        i.signature_id = Some(a.id.clone());
        s.save_identity(&i).unwrap();
        s.delete_signature(&a.id).unwrap();
        let d = s.list_defaults().unwrap();
        assert_eq!((d[0].new_id.clone(), d[0].reply_id.clone()), (None, Some(b.id.clone())));
        assert_eq!(s.list_identities().unwrap()[0].signature_id, None);
        assert_eq!(s.list_signatures().unwrap().len(), 1);
        assert!(s.set_defaults(&SigDefaults { account: "me@x.org".into(), new_id: Some(a.id), reply_id: None }).is_err());
    }

    #[test]
    fn identities_validate_and_stay_with_their_account() {
        let s = ComposeStore::open_in_memory().unwrap();
        let mut i = ident("me@x.org", "sales@x.org");
        i.name = "Sales \"Team\" <evil@x.org>".into();
        i.reply_to = "reply@x.org".into();
        let saved = s.save_identity(&i).unwrap();
        assert_eq!(saved.name, "Sales Team evil@x.org");
        assert!(s.save_identity(&ident("me@x.org", "SALES@x.org")).is_err(), "duplicate address");
        for bad in ["nope", "a@b", "a b@c.d", "a@b.c, d@e.f", "<a@b.c>", "a@@b.c", "Bob <a@b.c>"] {
            assert!(s.save_identity(&ident("me@x.org", bad)).is_err(), "{bad}");
        }
        let mut bad = ident("me@x.org", "ok@x.org");
        bad.reply_to = "not an address".into();
        assert!(s.save_identity(&bad).is_err());
        // cannot move an identity to another account
        let mut mv = saved.clone();
        mv.account = "other@y.org".into();
        assert!(s.save_identity(&mv).is_err());
        assert_eq!(s.list_identities().unwrap().len(), 1);
        s.delete_for_account("me@x.org").unwrap();
        assert!(s.list_identities().unwrap().is_empty());
    }

    #[test]
    fn from_can_only_be_own_address_or_a_saved_alias_of_the_same_account() {
        let s = ComposeStore::open_in_memory().unwrap();
        let mut al = ident("me@x.org", "alias@x.org");
        al.reply_to = "team@x.org".into();
        s.save_identity(&al).unwrap();
        s.save_identity(&ident("other@y.org", "theirs@y.org")).unwrap();
        let ids = s.list_identities().unwrap();
        let p = resolve_from("me@x.org", &ids, None).unwrap();
        assert_eq!(p, ResolvedFrom { mailbox: "me@x.org".into(), reply_to: None });
        assert_eq!(resolve_from("me@x.org", &ids, Some("ME@x.org")).unwrap().mailbox, "me@x.org");
        let a = resolve_from("me@x.org", &ids, Some("alias@x.org")).unwrap();
        assert_eq!(a, ResolvedFrom { mailbox: "\"Ada L\" <alias@x.org>".into(), reply_to: Some("team@x.org".into()) });
        // another account's alias, an arbitrary address and a header-injection attempt are refused
        for bad in ["theirs@y.org", "ceo@bank.example", "alias@x.org>\r\nBcc: e@x.y", "Evil <alias@x.org>"] {
            assert!(resolve_from("me@x.org", &ids, Some(bad)).is_err(), "{bad}");
        }
        // the built mail carries exactly that From and Reply-To
        let msg = mailcore::OutgoingMessage {
            to: vec!["bob@z.org".into()], cc: vec![], bcc: vec![], subject: "s".into(), text: "t".into(), html: None, attachments: vec![], in_reply_to: None, calendar: None,
            extra_headers: vec![("Reply-To".into(), a.reply_to.clone().unwrap())],
        };
        let raw = String::from_utf8(mailcore::smtp::build_message(&a.mailbox, &msg).unwrap().formatted()).unwrap();
        assert!(raw.contains("From: \"Ada L\" <alias@x.org>") && raw.contains("Reply-To: team@x.org"), "{raw}");
    }

    fn row(from: &str, to: &str, cc: &str, date: i64, role: &str) -> AddrRow {
        AddrRow { from: from.into(), to: to.into(), cc: cc.into(), date, role: role.into() }
    }

    #[test]
    fn suggestions_rank_sent_recipients_first_and_hide_own_addresses() {
        let rows = vec![
            row("Grace Hopper <grace@navy.example>", "me@x.org", "", 10, "inbox"),
            row("me@x.org", "Grace H <grace@navy.example>, gary@g.example", "", 20, "sent"),
            row("Gabe <gabe@g.example>", "me@x.org, alias@x.org", "", 30, "inbox"),
            row("Spammy <me@x.org>", "x@y.example", "", 5, "inbox"),
        ];
        let own = vec!["me@x.org".to_string(), "alias@x.org".to_string()];
        let s = build_suggestions(&rows, &own, "g", 10);
        let a: Vec<&str> = s.iter().map(|x| x.address.as_str()).collect();
        assert_eq!(a, vec!["grace@navy.example", "gary@g.example", "gabe@g.example"]);
        assert_eq!(s[0].name.as_deref(), Some("Grace H"), "newest display name wins");
        assert!(build_suggestions(&rows, &own, "alias", 10).is_empty(), "own addresses are not suggested");
        assert!(build_suggestions(&rows, &own, "", 10).is_empty());
        assert_eq!(build_suggestions(&rows, &own, "hopper", 10).len(), 0, "name was replaced by the newer one");
        assert_eq!(build_suggestions(&rows, &own, "navy", 10)[0].address, "grace@navy.example");
        assert_eq!(build_suggestions(&rows, &own, "g", 1).len(), 1);
    }
}
