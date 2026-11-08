//! Local address book (docs/ESSENTIALS_SPEC.md, Tier 2). Own tables in `workspace.db`; no network, no server.
//! Contacts are user content: they are never logged. The autocomplete source stays the cached mail; this book
//! adds names, several addresses, phone, notes and groups on top, and expands a group into its members.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::state::{err, AppState};
use crate::vcard::{self, Card};

pub const MAX_CONTACTS: i64 = 20_000;
const MAX_GROUP_MEMBERS_IN_SUGGEST: usize = 200;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub id: i64,
    pub name: String,
    pub org: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub notes: String,
    pub groups: Vec<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactInput {
    pub id: Option<i64>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub org: String,
    #[serde(default)]
    pub emails: Vec<String>,
    #[serde(default)]
    pub phones: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: i64,
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Addr {
    pub name: Option<String>,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GroupHit {
    pub name: String,
    pub members: Vec<Addr>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Suggest {
    pub contacts: Vec<Addr>,
    pub groups: Vec<GroupHit>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub added: u32,
    pub merged: u32,
    pub skipped: u32,
    pub truncated: bool,
}

/// A validated, normalised contact ready to be written.
struct Norm {
    name: String,
    org: String,
    emails: Vec<String>,
    phones: Vec<String>,
    notes: String,
    groups: Vec<String>,
}

pub struct ContactStore {
    conn: Mutex<Connection>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn db(_: rusqlite::Error) -> String {
    "The address book couldn't be read or saved.".to_string()
}

fn like(s: &str) -> String {
    let mut o = String::from("%");
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            o.push('\\');
        }
        o.push(c);
    }
    o.push('%');
    o
}

fn clean_line(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(max).collect()
}

fn clean_group(s: &str) -> String {
    clean_line(s, 60)
}

impl ContactStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        Self::from_conn(Connection::open(path).map_err(|e| e.to_string())?)
    }
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        Self::from_conn(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }
    fn from_conn(conn: Connection) -> Result<Self, String> {
        conn.pragma_update(None, "foreign_keys", true).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS contacts(
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL DEFAULT '', org TEXT NOT NULL DEFAULT '',
                notes TEXT NOT NULL DEFAULT '', created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS contact_emails(
                email TEXT PRIMARY KEY, contact_id INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE, pos INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS contact_emails_c ON contact_emails(contact_id);
             CREATE TABLE IF NOT EXISTS contact_phones(
                contact_id INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE, phone TEXT NOT NULL, pos INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS contact_phones_c ON contact_phones(contact_id);
             CREATE TABLE IF NOT EXISTS contact_groups(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE COLLATE NOCASE);
             CREATE TABLE IF NOT EXISTS contact_group_members(
                group_id INTEGER NOT NULL REFERENCES contact_groups(id) ON DELETE CASCADE,
                contact_id INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
                PRIMARY KEY (group_id, contact_id));
             CREATE INDEX IF NOT EXISTS contact_members_c ON contact_group_members(contact_id);",
        )
        .map_err(|e| e.to_string())?;
        Ok(ContactStore { conn: Mutex::new(conn) })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, String> {
        self.conn.lock().map_err(|_| "The address book is unavailable.".to_string())
    }

    fn load(c: &Connection, id: i64) -> Result<Option<Contact>, String> {
        let Some((name, org, notes, updated_at)) = c
            .query_row("SELECT name, org, notes, updated_at FROM contacts WHERE id=?1", [id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))
            .optional()
            .map_err(db)?
        else {
            return Ok(None);
        };
        let col = |sql: &str| -> Result<Vec<String>, String> {
            let mut st = c.prepare(sql).map_err(db)?;
            let v = st.query_map([id], |r| r.get::<_, String>(0)).map_err(db)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db)?;
            Ok(v)
        };
        Ok(Some(Contact {
            id,
            name,
            org,
            notes,
            updated_at,
            emails: col("SELECT email FROM contact_emails WHERE contact_id=?1 ORDER BY pos")?,
            phones: col("SELECT phone FROM contact_phones WHERE contact_id=?1 ORDER BY pos")?,
            groups: col("SELECT g.name FROM contact_groups g JOIN contact_group_members m ON m.group_id=g.id WHERE m.contact_id=?1 ORDER BY g.name")?,
        }))
    }

    #[cfg(test)]
    pub fn get(&self, id: i64) -> Result<Option<Contact>, String> {
        let c = self.lock()?;
        Self::load(&c, id)
    }

    pub fn find_by_email(&self, address: &str) -> Result<Option<Contact>, String> {
        let c = self.lock()?;
        let id: Option<i64> = c.query_row("SELECT contact_id FROM contact_emails WHERE email=?1", [address.trim().to_lowercase()], |r| r.get(0)).optional().map_err(db)?;
        match id {
            Some(id) => Self::load(&c, id),
            None => Ok(None),
        }
    }

    /// Sorted by name (case-insensitive; nameless contacts by first address). `query` matches name, org, address,
    /// phone and notes; `group` restricts to one group (by name).
    pub fn list(&self, query: &str, group: Option<&str>) -> Result<Vec<Contact>, String> {
        let c = self.lock()?;
        let q = like(query.trim());
        let mut st = c
            .prepare(
                "SELECT c.id FROM contacts c
                 WHERE (?1 = '%%' OR c.name LIKE ?1 ESCAPE '\\' OR c.org LIKE ?1 ESCAPE '\\' OR c.notes LIKE ?1 ESCAPE '\\'
                        OR EXISTS (SELECT 1 FROM contact_emails e WHERE e.contact_id=c.id AND e.email LIKE ?1 ESCAPE '\\')
                        OR EXISTS (SELECT 1 FROM contact_phones p WHERE p.contact_id=c.id AND p.phone LIKE ?1 ESCAPE '\\'))
                   AND (?2 IS NULL OR EXISTS (SELECT 1 FROM contact_group_members m JOIN contact_groups g ON g.id=m.group_id WHERE m.contact_id=c.id AND g.name=?2))
                 ORDER BY lower(CASE WHEN c.name='' THEN COALESCE((SELECT email FROM contact_emails WHERE contact_id=c.id ORDER BY pos LIMIT 1),'') ELSE c.name END), c.id
                 LIMIT 5000",
            )
            .map_err(db)?;
        let ids = st.query_map(params![q, group], |r| r.get::<_, i64>(0)).map_err(db)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(ct) = Self::load(&c, id)? {
                out.push(ct);
            }
        }
        Ok(out)
    }

    fn group_id(c: &Connection, name: &str) -> Result<i64, String> {
        c.execute("INSERT OR IGNORE INTO contact_groups(name) VALUES (?1)", [name]).map_err(db)?;
        c.query_row("SELECT id FROM contact_groups WHERE name=?1", [name], |r| r.get(0)).map_err(db)
    }

    /// Validate and normalise an input. Does not touch the database.
    fn normalise(i: &ContactInput) -> Result<Norm, String> {
        let name = clean_line(&i.name, vcard::MAX_NAME);
        let mut emails: Vec<String> = Vec::new();
        for e in &i.emails {
            let e = e.trim().to_lowercase();
            if e.is_empty() {
                continue;
            }
            if !vcard::valid_email(&e) {
                return Err(format!("\"{}\" is not a valid e-mail address.", clean_line(&e, 60)));
            }
            if !emails.contains(&e) {
                emails.push(e);
            }
        }
        if emails.len() > vcard::MAX_EMAILS {
            return Err(format!("A contact can have at most {} addresses.", vcard::MAX_EMAILS));
        }
        let mut phones: Vec<String> = Vec::new();
        for p in &i.phones {
            let p = clean_line(p, 40);
            if !p.is_empty() && !phones.contains(&p) {
                phones.push(p);
            }
        }
        if phones.len() > vcard::MAX_PHONES {
            return Err(format!("A contact can have at most {} phone numbers.", vcard::MAX_PHONES));
        }
        let mut groups: Vec<String> = Vec::new();
        for g in &i.groups {
            let g = clean_group(g);
            if !g.is_empty() && !groups.iter().any(|x| x.eq_ignore_ascii_case(&g)) {
                groups.push(g);
            }
        }
        if groups.len() > vcard::MAX_GROUPS {
            return Err(format!("A contact can be in at most {} groups.", vcard::MAX_GROUPS));
        }
        if name.is_empty() && emails.is_empty() {
            return Err("Give the contact a name or an e-mail address.".into());
        }
        let notes: String = i.notes.chars().filter(|c| !c.is_control() || *c == '\n').take(vcard::MAX_NOTES).collect();
        Ok(Norm { name, org: clean_line(&i.org, vcard::MAX_FIELD), emails, phones, notes: notes.trim().to_string(), groups })
    }

    fn write(c: &Connection, id: i64, n: &Norm) -> Result<(), String> {
        let Norm { name, org, emails, phones, notes, groups } = n;
        // An address can belong to one contact only: say which one.
        for e in emails.iter() {
            let other: Option<(i64, String)> = c
                .query_row("SELECT c.id, c.name FROM contact_emails e JOIN contacts c ON c.id=e.contact_id WHERE e.email=?1", [e], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .map_err(db)?;
            if let Some((oid, oname)) = other {
                if oid != id {
                    let who = if oname.is_empty() { "another contact".to_string() } else { format!("\"{}\"", clean_line(&oname, 60)) };
                    return Err(format!("{e} already belongs to {who}."));
                }
            }
        }
        c.execute("UPDATE contacts SET name=?2, org=?3, notes=?4, updated_at=?5 WHERE id=?1", params![id, name, org, notes, now()]).map_err(db)?;
        c.execute("DELETE FROM contact_emails WHERE contact_id=?1", [id]).map_err(db)?;
        c.execute("DELETE FROM contact_phones WHERE contact_id=?1", [id]).map_err(db)?;
        c.execute("DELETE FROM contact_group_members WHERE contact_id=?1", [id]).map_err(db)?;
        for (i, e) in emails.iter().enumerate() {
            c.execute("INSERT INTO contact_emails(email, contact_id, pos) VALUES (?1,?2,?3)", params![e, id, i as i64]).map_err(db)?;
        }
        for (i, p) in phones.iter().enumerate() {
            c.execute("INSERT INTO contact_phones(contact_id, phone, pos) VALUES (?1,?2,?3)", params![id, p, i as i64]).map_err(db)?;
        }
        for g in groups {
            let gid = Self::group_id(c, g)?;
            c.execute("INSERT OR IGNORE INTO contact_group_members(group_id, contact_id) VALUES (?1,?2)", params![gid, id]).map_err(db)?;
        }
        // Groups left empty by this edit stay (the user may fill them again); they are removed explicitly.
        Ok(())
    }

    pub fn save(&self, input: &ContactInput) -> Result<Contact, String> {
        let norm = Self::normalise(input)?;
        let mut c = self.lock()?;
        let tx = c.transaction().map_err(db)?;
        let id = match input.id {
            Some(id) => {
                if tx.query_row("SELECT 1 FROM contacts WHERE id=?1", [id], |_| Ok(())).optional().map_err(db)?.is_none() {
                    return Err("That contact no longer exists.".into());
                }
                id
            }
            None => {
                let n: i64 = tx.query_row("SELECT COUNT(*) FROM contacts", [], |r| r.get(0)).map_err(db)?;
                if n >= MAX_CONTACTS {
                    return Err(format!("The address book is full ({MAX_CONTACTS} contacts)."));
                }
                tx.execute("INSERT INTO contacts(name, created_at, updated_at) VALUES ('', ?1, ?1)", [now()]).map_err(db)?;
                tx.last_insert_rowid()
            }
        };
        Self::write(&tx, id, &norm)?;
        tx.commit().map_err(db)?;
        Self::load(&c, id)?.ok_or_else(|| "That contact no longer exists.".to_string())
    }

    pub fn delete(&self, id: i64) -> Result<bool, String> {
        Ok(self.lock()?.execute("DELETE FROM contacts WHERE id=?1", [id]).map_err(db)? > 0)
    }

    pub fn groups(&self) -> Result<Vec<Group>, String> {
        let c = self.lock()?;
        let mut st = c
            .prepare("SELECT g.id, g.name, (SELECT COUNT(*) FROM contact_group_members m WHERE m.group_id=g.id) FROM contact_groups g ORDER BY g.name COLLATE NOCASE")
            .map_err(db)?;
        let v = st.query_map([], |r| Ok(Group { id: r.get(0)?, name: r.get(1)?, count: r.get(2)? })).map_err(db)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db)?;
        Ok(v)
    }

    pub fn rename_group(&self, id: i64, name: &str) -> Result<(), String> {
        let n = clean_group(name);
        if n.is_empty() {
            return Err("Give the group a name.".into());
        }
        let c = self.lock()?;
        match c.execute("UPDATE contact_groups SET name=?2 WHERE id=?1", params![id, n]) {
            Ok(0) => Err("That group no longer exists.".into()),
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => Err("A group with that name already exists.".into()),
            Err(e) => Err(db(e)),
        }
    }

    /// Removes the group only; its members stay in the address book.
    pub fn delete_group(&self, id: i64) -> Result<(), String> {
        self.lock()?.execute("DELETE FROM contact_groups WHERE id=?1", [id]).map_err(db)?;
        Ok(())
    }

    /// Members of a group by name as addressable entries (first address of each member; members without one are
    /// left out). This is what Compose expands a group into.
    pub fn expand_group(&self, name: &str) -> Result<Vec<Addr>, String> {
        let c = self.lock()?;
        Self::members(&c, "g.name = ?1", name.trim())
    }

    fn members(c: &Connection, filter: &str, arg: &str) -> Result<Vec<Addr>, String> {
        let sql = format!(
            "SELECT c.name, (SELECT email FROM contact_emails WHERE contact_id=c.id ORDER BY pos LIMIT 1)
             FROM contact_groups g JOIN contact_group_members m ON m.group_id=g.id JOIN contacts c ON c.id=m.contact_id
             WHERE {filter} ORDER BY lower(c.name), c.id LIMIT {MAX_GROUP_MEMBERS_IN_SUGGEST}"
        );
        let mut st = c.prepare(&sql).map_err(db)?;
        let rows = st.query_map([arg], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))).map_err(db)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db)?;
        Ok(rows.into_iter().filter_map(|(n, e)| e.map(|address| Addr { name: (!n.is_empty()).then_some(n), address })).collect())
    }

    /// Autocomplete for the recipient fields: contacts whose name or address contains `prefix`, and groups whose
    /// name does (with their members, so picking one fills in the whole group).
    pub fn suggest(&self, prefix: &str, limit: usize) -> Result<Suggest, String> {
        let p = prefix.trim();
        if p.is_empty() || p.len() > 200 {
            return Ok(Suggest::default());
        }
        let q = like(p);
        let c = self.lock()?;
        let mut out = Suggest::default();
        {
            let mut st = c
                .prepare(
                    "SELECT c.name, e.email FROM contacts c JOIN contact_emails e ON e.contact_id=c.id
                     WHERE (c.name LIKE ?1 ESCAPE '\\' AND e.pos = 0) OR e.email LIKE ?1 ESCAPE '\\'
                     ORDER BY (c.name LIKE ?2 ESCAPE '\\' OR e.email LIKE ?2 ESCAPE '\\') DESC, lower(c.name), e.pos LIMIT ?3",
                )
                .map_err(db)?;
            let starts = format!("{}%", &q[1..q.len() - 1]);
            let rows = st.query_map(params![q, starts, limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(db)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db)?;
            out.contacts = rows.into_iter().map(|(n, address)| Addr { name: (!n.is_empty()).then_some(n), address }).collect();
        }
        let mut gs = c.prepare("SELECT name FROM contact_groups WHERE name LIKE ?1 ESCAPE '\\' ORDER BY name COLLATE NOCASE LIMIT 5").map_err(db)?;
        let names = gs.query_map([&q], |r| r.get::<_, String>(0)).map_err(db)?.collect::<rusqlite::Result<Vec<_>>>().map_err(db)?;
        for name in names {
            let members = Self::members(&c, "g.name = ?1", &name)?;
            if !members.is_empty() {
                out.groups.push(GroupHit { name, members });
            }
        }
        Ok(out)
    }

    /// Add plain `name <address>` entries (e.g. picked from the mail history). Existing addresses are left alone.
    pub fn add_many(&self, entries: &[Addr]) -> Result<u32, String> {
        let mut added = 0;
        for e in entries.iter().take(500) {
            let address = e.address.trim().to_lowercase();
            if !vcard::valid_email(&address) || self.find_by_email(&address)?.is_some() {
                continue;
            }
            self.save(&ContactInput { id: None, name: e.name.clone().unwrap_or_default(), org: String::new(), emails: vec![address], phones: vec![], notes: String::new(), groups: vec![] })?;
            added += 1;
        }
        Ok(added)
    }

    /// Merge parsed vCards: a card with an address we already know is merged into that contact (only filling what is
    /// missing, never overwriting); anything else is added.
    pub fn import(&self, p: vcard::Parsed) -> Result<ImportResult, String> {
        let mut r = ImportResult { skipped: p.skipped as u32, truncated: p.truncated, ..Default::default() };
        for card in p.cards {
            let existing = match card.emails.iter().find_map(|e| self.find_by_email(e).transpose()) {
                Some(x) => Some(x?),
                None => None,
            };
            match existing {
                Some(ex) => {
                    let mut m = ContactInput { id: Some(ex.id), name: ex.name.clone(), org: ex.org.clone(), emails: ex.emails.clone(), phones: ex.phones.clone(), notes: ex.notes.clone(), groups: ex.groups.clone() };
                    if m.name.is_empty() {
                        m.name = card.name.clone();
                    }
                    if m.org.is_empty() {
                        m.org = card.org.clone();
                    }
                    if m.notes.is_empty() {
                        m.notes = card.notes.clone();
                    }
                    for e in &card.emails {
                        if !m.emails.contains(e) && m.emails.len() < vcard::MAX_EMAILS && self.find_by_email(e)?.is_none() {
                            m.emails.push(e.clone());
                        }
                    }
                    for ph in &card.phones {
                        if !m.phones.contains(ph) && m.phones.len() < vcard::MAX_PHONES {
                            m.phones.push(ph.clone());
                        }
                    }
                    for g in &card.groups {
                        if !m.groups.iter().any(|x| x.eq_ignore_ascii_case(g)) && m.groups.len() < vcard::MAX_GROUPS {
                            m.groups.push(g.clone());
                        }
                    }
                    if m.name != ex.name || m.org != ex.org || m.notes != ex.notes || m.emails != ex.emails || m.phones != ex.phones || m.groups != ex.groups {
                        self.save(&m)?;
                        r.merged += 1;
                    } else {
                        r.skipped += 1;
                    }
                }
                None => {
                    let input = ContactInput { id: None, name: card.name, org: card.org, emails: card.emails, phones: card.phones, notes: card.notes, groups: card.groups };
                    match self.save(&input) {
                        Ok(_) => r.added += 1,
                        Err(e) if e.contains("full") => return Err(e),
                        Err(_) => r.skipped += 1,
                    }
                }
            }
        }
        Ok(r)
    }

    pub fn cards(&self, group: Option<&str>) -> Result<Vec<Card>, String> {
        Ok(self
            .list("", group)?
            .into_iter()
            .map(|c| Card { name: c.name, org: c.org, emails: c.emails, phones: c.phones, notes: c.notes, groups: c.groups })
            .collect())
    }
}

// ---------------------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardInfo {
    pub contact: Option<Contact>,
    /// Cached messages received from the address (junk and trash excluded).
    pub messages: u32,
    /// Unix seconds of the newest of them, 0 when none.
    pub newest: i64,
}

#[tauri::command]
pub async fn contacts_list(store: State<'_, ContactStore>, query: Option<String>, group: Option<String>) -> Result<Vec<Contact>, String> {
    store.list(query.as_deref().unwrap_or(""), group.as_deref().filter(|g| !g.is_empty()))
}

#[tauri::command]
pub async fn contacts_save(store: State<'_, ContactStore>, contact: ContactInput) -> Result<Contact, String> {
    store.save(&contact)
}

#[tauri::command]
pub async fn contacts_delete(store: State<'_, ContactStore>, id: i64) -> Result<(), String> {
    store.delete(id)?;
    Ok(())
}

#[tauri::command]
pub async fn contacts_groups(store: State<'_, ContactStore>) -> Result<Vec<Group>, String> {
    store.groups()
}

#[tauri::command]
pub async fn contacts_group_rename(store: State<'_, ContactStore>, id: i64, name: String) -> Result<(), String> {
    store.rename_group(id, &name)
}

#[tauri::command]
pub async fn contacts_group_delete(store: State<'_, ContactStore>, id: i64) -> Result<(), String> {
    store.delete_group(id)
}

/// The members of a group by name (first address of each), as Compose inserts them.
#[tauri::command]
pub async fn contacts_expand_group(store: State<'_, ContactStore>, name: String) -> Result<Vec<Addr>, String> {
    store.expand_group(&name)
}

#[tauri::command]
pub async fn contacts_suggest(store: State<'_, ContactStore>, prefix: String, limit: Option<u32>) -> Result<Suggest, String> {
    store.suggest(&prefix, limit.unwrap_or(5).clamp(1, 25) as usize)
}

/// Contact card for a sender: the saved contact (if any) and how many cached messages came from the address.
#[tauri::command]
pub async fn contacts_card(state: State<'_, AppState>, store: State<'_, ContactStore>, address: String) -> Result<CardInfo, String> {
    let (messages, newest) = state.storage.count_from(&address).map_err(err)?;
    Ok(CardInfo { contact: store.find_by_email(&address)?, messages, newest })
}

/// Frequent correspondents from the cached mail that are not in the address book yet (the lazy seed source).
#[tauri::command]
pub async fn contacts_from_mail(state: State<'_, AppState>, store: State<'_, ContactStore>, limit: Option<u32>) -> Result<Vec<Addr>, String> {
    let rows = state.storage.address_rows_for(None, "", 4000).map_err(err)?;
    let own: Vec<String> = state.storage.list_accounts().map_err(err)?.into_iter().map(|a| a.email).collect();
    let all = crate::composer::build_suggestions(&rows, &own, "@", 300);
    let mut out = Vec::new();
    for s in all {
        if out.len() >= limit.unwrap_or(40).clamp(1, 200) as usize {
            break;
        }
        if store.find_by_email(&s.address)?.is_none() {
            out.push(Addr { name: s.name, address: s.address });
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn contacts_add_many(store: State<'_, ContactStore>, entries: Vec<Addr>) -> Result<u32, String> {
    store.add_many(&entries)
}

/// Import the text of a `.vcf` file (read by the frontend through the file picker). Network-free and bounded.
#[tauri::command]
pub async fn contacts_import(store: State<'_, ContactStore>, text: String) -> Result<ImportResult, String> {
    if text.len() > vcard::MAX_INPUT_BYTES * 2 {
        return Err("That file is too large to import (limit 8 MB).".into());
    }
    let parsed = tauri::async_runtime::spawn_blocking(move || vcard::parse(&text)).await.map_err(|e| e.to_string())?;
    store.import(parsed)
}

/// Write the address book (or one group) to a new `.vcf` file in Downloads. Never overwrites: `contacts-1.vcf`...
#[tauri::command]
pub async fn contacts_export(app: AppHandle, store: State<'_, ContactStore>, group: Option<String>) -> Result<String, String> {
    let group = group.filter(|g| !g.is_empty());
    let cards = store.cards(group.as_deref())?;
    if cards.is_empty() {
        return Err("There are no contacts to export.".into());
    }
    let dir = crate::files::downloads_dir(&app)?;
    let name = match &group {
        Some(g) => format!("contacts-{}.vcf", crate::files::sanitize_filename(g)),
        None => "contacts.vcf".to_string(),
    };
    let path = crate::files::save_unique(&dir, &name, vcard::write(&cards).as_bytes()).map_err(|e| format!("Couldn't save the file: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str, emails: &[&str]) -> ContactInput {
        ContactInput { id: None, name: name.into(), org: String::new(), emails: emails.iter().map(|s| s.to_string()).collect(), phones: vec![], notes: String::new(), groups: vec![] }
    }

    #[test]
    fn crud_round_trip_with_a_real_file_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.db");
        let id = {
            let s = ContactStore::open(&path).unwrap();
            let mut i = input("Ada Lovelace", &["Ada@Example.org", "ada@home.test", "ada@example.org"]);
            i.phones = vec!["+44 1".into()];
            i.notes = "met at\nPyCon".into();
            i.groups = vec!["Friends".into(), "friends".into(), "Work".into()];
            let c = s.save(&i).unwrap();
            assert_eq!(c.emails, vec!["ada@example.org", "ada@home.test"], "lowercased and deduped");
            assert_eq!(c.groups, vec!["Friends", "Work"]);
            c.id
        };
        // reopened: data persisted
        let s = ContactStore::open(&path).unwrap();
        let c = s.get(id).unwrap().unwrap();
        assert_eq!((c.name.as_str(), c.phones.clone(), c.notes.as_str()), ("Ada Lovelace", vec!["+44 1".to_string()], "met at\nPyCon"));
        // edit
        let mut e = input("Ada L.", &["ada@example.org"]);
        e.id = Some(id);
        let c = s.save(&e).unwrap();
        assert_eq!((c.name.as_str(), c.emails.len(), c.groups.len(), c.phones.len()), ("Ada L.", 1, 0, 0), "fields are replaced as sent");
        assert!(s.find_by_email("ADA@example.org").unwrap().is_some());
        assert!(s.find_by_email("ada@home.test").unwrap().is_none(), "removed address is free again");
        // delete cascades
        assert!(s.delete(id).unwrap());
        assert!(s.get(id).unwrap().is_none() && s.find_by_email("ada@example.org").unwrap().is_none());
        assert!(!s.delete(id).unwrap());
        assert!(s.save(&ContactInput { id: Some(id), ..input("x", &[]) }).is_err(), "editing a deleted contact fails");
    }

    #[test]
    fn validation_and_address_ownership() {
        let s = ContactStore::open_in_memory().unwrap();
        assert!(s.save(&input("", &[])).is_err());
        assert!(s.save(&input("X", &["not an address"])).is_err());
        assert!(s.save(&input("X", &["a@b"])).is_err());
        assert!(s.save(&input("Only Name", &[])).is_ok(), "a name alone is fine");
        s.save(&input("Bob", &["bob@x.test"])).unwrap();
        let e = s.save(&input("Other Bob", &["bob@x.test"])).unwrap_err();
        assert!(e.contains("already belongs to \"Bob\""), "{e}");
        let many: Vec<String> = (0..11).map(|i| format!("e{i}@x.test")).collect();
        assert!(s.save(&ContactInput { emails: many, ..input("Many", &[]) }).is_err());
        // control characters never reach the book
        let c = s.save(&input("Ev\u{7}il\u{0}", &["ev@x.test"])).unwrap();
        assert_eq!(c.name, "Evil");
    }

    #[test]
    fn list_search_and_group_filter() {
        let s = ContactStore::open_in_memory().unwrap();
        let mut a = input("zed", &["z@x.test"]);
        a.groups = vec!["Team".into()];
        s.save(&a).unwrap();
        let mut b = input("Alice", &["alice@corp.test"]);
        b.groups = vec!["Team".into(), "Friends".into()];
        b.phones = vec!["555-0100".into()];
        s.save(&b).unwrap();
        s.save(&input("", &["nameless@x.test"])).unwrap();
        let names = |v: Vec<Contact>| v.into_iter().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(names(s.list("", None).unwrap()), vec!["Alice", "", "zed"], "case-insensitive; nameless sorted by address");
        assert_eq!(names(s.list("corp", None).unwrap()), vec!["Alice"]);
        assert_eq!(names(s.list("0100", None).unwrap()), vec!["Alice"]);
        assert_eq!(names(s.list("", Some("Team")).unwrap()), vec!["Alice", "zed"]);
        assert_eq!(names(s.list("", Some("Friends")).unwrap()), vec!["Alice"]);
        assert!(s.list("%", None).unwrap().is_empty(), "wildcards are literal");
        let g = s.groups().unwrap();
        assert_eq!(g.iter().map(|g| (g.name.as_str(), g.count)).collect::<Vec<_>>(), vec![("Friends", 1), ("Team", 2)]);
    }

    #[test]
    fn group_expansion_and_suggest() {
        let s = ContactStore::open_in_memory().unwrap();
        for (n, e) in [("Alice", "alice@x.test"), ("Bob", "bob@x.test"), ("Carol", "carol@x.test")] {
            let mut i = input(n, &[e, &format!("{}@second.test", n.to_lowercase())]);
            i.groups = if n == "Carol" { vec![] } else { vec!["Team".into()] };
            s.save(&i).unwrap();
        }
        s.save(&ContactInput { groups: vec!["Team".into()], ..input("No Address", &[]) }).unwrap();
        let m = s.expand_group("team").unwrap();
        assert_eq!(m.iter().map(|a| a.address.as_str()).collect::<Vec<_>>(), vec!["alice@x.test", "bob@x.test"], "first address of each member; no-address members left out");
        assert_eq!(m[0].name.as_deref(), Some("Alice"));
        let sg = s.suggest("tea", 5).unwrap();
        assert_eq!(sg.groups.len(), 1);
        assert_eq!(sg.groups[0].name, "Team");
        assert_eq!(sg.groups[0].members.len(), 2);
        // contact suggestions: by name (first address) and by address fragment
        let sg = s.suggest("car", 5).unwrap();
        assert_eq!(sg.contacts.iter().map(|a| a.address.as_str()).collect::<Vec<_>>(), vec!["carol@x.test", "carol@second.test"], "name match gives the first address; the second matches as an address");
        let sg = s.suggest("second", 5).unwrap();
        assert_eq!(sg.contacts.len(), 3, "an address fragment matches the secondary addresses");
        let sg = s.suggest("no add", 5).unwrap();
        assert!(sg.contacts.is_empty(), "a contact without an address is never offered");
        assert!(s.suggest("", 5).unwrap().contacts.is_empty());
        assert!(s.suggest("%", 5).unwrap().contacts.is_empty());
        // deleting a group keeps the contacts
        let gid = s.groups().unwrap()[0].id;
        s.delete_group(gid).unwrap();
        assert!(s.groups().unwrap().is_empty() && s.list("", None).unwrap().len() == 4);
        assert!(s.expand_group("Team").unwrap().is_empty());
    }

    #[test]
    fn group_rename_conflicts() {
        let s = ContactStore::open_in_memory().unwrap();
        s.save(&ContactInput { groups: vec!["A".into(), "B".into()], ..input("x", &["x@x.test"]) }).unwrap();
        let g = s.groups().unwrap();
        assert!(s.rename_group(g[0].id, "b").unwrap_err().contains("already exists"));
        assert!(s.rename_group(g[0].id, "  ").is_err());
        s.rename_group(g[0].id, "Alpha").unwrap();
        assert_eq!(s.groups().unwrap()[0].name, "Alpha");
    }

    #[test]
    fn import_merges_without_overwriting_and_export_round_trips() {
        let s = ContactStore::open_in_memory().unwrap();
        s.save(&ContactInput { phones: vec!["111".into()], ..input("Ada", &["ada@x.test"]) }).unwrap();
        let text = "BEGIN:VCARD\nVERSION:3.0\nFN:Ada Other Name\nEMAIL:ada@x.test\nEMAIL:ada2@x.test\nTEL:222\nCATEGORIES:Club\nEND:VCARD\n\
                    BEGIN:VCARD\nVERSION:3.0\nFN:Grace\nEMAIL:grace@navy.test\nORG:Navy\nEND:VCARD\n\
                    BEGIN:VCARD\nVERSION:3.0\nEMAIL:bad address\nEND:VCARD\n";
        let r = s.import(vcard::parse(text)).unwrap();
        assert_eq!((r.added, r.merged, r.skipped), (1, 1, 1));
        let ada = s.find_by_email("ada@x.test").unwrap().unwrap();
        assert_eq!(ada.name, "Ada", "the existing name is kept");
        assert_eq!(ada.emails, vec!["ada@x.test", "ada2@x.test"]);
        assert_eq!(ada.phones, vec!["111", "222"]);
        assert_eq!(ada.groups, vec!["Club"]);
        // importing the same file again changes nothing
        let r = s.import(vcard::parse(text)).unwrap();
        assert_eq!((r.added, r.merged), (0, 0));
        assert_eq!(s.list("", None).unwrap().len(), 2);
        // export -> import into a fresh book gives the same contacts
        let out = vcard::write(&s.cards(None).unwrap());
        let s2 = ContactStore::open_in_memory().unwrap();
        s2.import(vcard::parse(&out)).unwrap();
        let strip = |v: Vec<Contact>| v.into_iter().map(|c| (c.name, c.org, c.emails, c.phones, c.groups)).collect::<Vec<_>>();
        assert_eq!(strip(s.list("", None).unwrap()), strip(s2.list("", None).unwrap()));
        // a group export contains only that group
        assert_eq!(s.cards(Some("Club")).unwrap().len(), 1);
    }

    #[test]
    fn add_many_skips_known_and_invalid() {
        let s = ContactStore::open_in_memory().unwrap();
        s.save(&input("Known", &["known@x.test"])).unwrap();
        let n = s
            .add_many(&[
                Addr { name: Some("New".into()), address: "New@X.test".into() },
                Addr { name: None, address: "known@x.test".into() },
                Addr { name: None, address: "junk".into() },
            ])
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(s.find_by_email("new@x.test").unwrap().unwrap().name, "New");
    }
}
