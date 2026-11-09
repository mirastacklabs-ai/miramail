//! Local search over the SQLite cache (FTS5) and thread lookup. Used by the AI chat tools.
//!
//! The query language mirrors `app/src/lib/query.ts` (the message-list filter): whitespace-separated
//! terms, `"quoted phrases"`, `-term` negation and the operators `from: to: subject: contains: body:
//! is: before: after: older_than: newer_than: cc: has:attachment folder:`. `cc:` matches the stored Cc header
//! (messages cached before schema v3 have none), `has:attachment` the stored `has_attachment` flag (a header
//! guess until the body is cached), `folder:` a folder name, its last path part or its role.
//!
//! Safety: user text never reaches SQL or the FTS5 query grammar unquoted. Every text term becomes one
//! double-quoted FTS5 phrase (inner quotes doubled, so operators like AND / NEAR / `*` / `col:` are
//! literal text), the MATCH expression is a bound parameter, and column names come from a fixed enum.

use crate::storage::Storage;
use crate::types::MessageSummary;
use crate::Result;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, OptionalExtension};

/// Max characters of a query string and max number of terms we act on.
pub const MAX_QUERY_CHARS: usize = 500;
pub const MAX_TERMS: usize = 16;
pub const MAX_RESULTS: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeKey {
    pub account: String,
    pub folder: String,
}

impl ScopeKey {
    pub fn new(account: impl Into<String>, folder: impl Into<String>) -> Self {
        ScopeKey { account: account.into(), folder: folder.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    From,
    To,
    Subject,
    Any,
}

impl TextField {
    fn column(self) -> Option<&'static str> {
        match self {
            TextField::From => Some("from_addr"),
            TextField::To => Some("to_addr"),
            TextField::Subject => Some("subject"),
            TextField::Any => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsValue {
    Unread,
    Read,
    Flagged,
    Unflagged,
    Answered,
    Unanswered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    Text { field: TextField, value: String, neg: bool },
    Is { value: IsValue, neg: bool },
    /// `before` = strictly before `ts`; otherwise on or after `ts`.
    Date { before: bool, ts: i64, neg: bool },
    /// Substring of the stored Cc header (case-insensitive).
    Cc { value: String, neg: bool },
    HasAttachment { neg: bool },
    /// Folder name (full, or last path part) or role name, case-insensitive.
    Folder { value: String, neg: bool },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub terms: Vec<Term>,
    pub hints: Vec<String>,
}

fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for ch in input.chars() {
        if ch == '"' {
            quoted = !quoted;
            cur.push(ch);
        } else if ch.is_whitespace() && !quoted {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const DAY: i64 = 86_400;

/// `YYYY-MM-DD`, `YYYY/MM/DD`, `today` or `yesterday` to the start of that day (seconds, shifted by
/// `tz_offset` so a local day boundary can be honoured). `None` if invalid.
pub fn parse_day(v: &str, now: i64, tz_offset: i64) -> Option<i64> {
    let s = v.to_ascii_lowercase();
    let today = (now + tz_offset).div_euclid(DAY) * DAY - tz_offset;
    match s.as_str() {
        "today" => return Some(today),
        "yesterday" => return Some(today - DAY),
        _ => {}
    }
    let b = s.as_bytes();
    if b.len() != 10 || !(b[4] == b'-' || b[4] == b'/') || b[7] != b[4] {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).filter(|t| t.bytes().all(|c| c.is_ascii_digit())).and_then(|t| t.parse::<i64>().ok());
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let days = days_from_civil(y, m, d);
    if civil_from_days(days) != (y, m, d) {
        return None;
    }
    Some(days * DAY - tz_offset)
}

/// `7d`, `2w`, `3m`, `1y`, `12h` to seconds.
pub fn parse_span(v: &str) -> Option<i64> {
    let v = v.to_ascii_lowercase();
    if v.len() < 2 || v.len() > 5 {
        return None;
    }
    let (n, unit) = v.split_at(v.len() - 1);
    if !n.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = n.parse().ok()?;
    let mul = match unit {
        "h" => 3600,
        "d" => DAY,
        "w" => 7 * DAY,
        "m" => 30 * DAY,
        "y" => 365 * DAY,
        _ => return None,
    };
    Some(n * mul)
}

fn clean(v: &str) -> String {
    v.chars().filter(|c| !c.is_control() && *c != '"').collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse a query string. Never fails; problems become `hints`.
pub fn parse_query(input: &str, now: i64, tz_offset: i64) -> ParsedQuery {
    let mut out = ParsedQuery::default();
    let input: String = input.chars().take(MAX_QUERY_CHARS).collect();
    for tok in tokenize(&input) {
        if out.terms.len() >= MAX_TERMS {
            out.hints.push(format!("Only the first {MAX_TERMS} search terms were used."));
            break;
        }
        let mut body = tok.as_str();
        let mut neg = false;
        if body.len() > 1 && body.starts_with('-') {
            neg = true;
            body = &body[1..];
        }
        let split = body.split_once(':').filter(|(op, _)| !op.is_empty() && op.chars().all(|c| c.is_ascii_alphabetic() || c == '_'));
        let known = ["from", "to", "cc", "subject", "contains", "body", "is", "before", "after", "older_than", "newer_than", "has", "folder"];
        let (op, val) = match split {
            Some((op, val)) if known.contains(&op.to_ascii_lowercase().as_str()) => (op.to_ascii_lowercase(), clean(val)),
            _ => {
                let v = clean(body);
                push_text(&mut out, TextField::Any, v, neg);
                continue;
            }
        };
        if val.is_empty() {
            continue;
        }
        match op.as_str() {
            "from" => push_text(&mut out, TextField::From, val, neg),
            "to" => push_text(&mut out, TextField::To, val, neg),
            "subject" => push_text(&mut out, TextField::Subject, val, neg),
            "contains" | "body" => push_text(&mut out, TextField::Any, val, neg),
            // ASCII-only on purpose: SQLite's lower() only folds ASCII, so a Unicode-aware lowercase here would
            // compare "ÉCOLE" (query) against "École" (stored, untouched by lower()) and never match.
            "cc" => out.terms.push(Term::Cc { value: val.to_ascii_lowercase(), neg }),
            "folder" => out.terms.push(Term::Folder { value: val.to_ascii_lowercase(), neg }),
            "is" => {
                let v = match val.to_ascii_lowercase().as_str() {
                    "unread" => Some(IsValue::Unread),
                    "read" => Some(IsValue::Read),
                    "flagged" => Some(IsValue::Flagged),
                    "unflagged" => Some(IsValue::Unflagged),
                    "answered" => Some(IsValue::Answered),
                    "unanswered" => Some(IsValue::Unanswered),
                    _ => None,
                };
                match v {
                    Some(value) => out.terms.push(Term::Is { value, neg }),
                    None => out.hints.push(format!("Unknown is:{val}; use unread, read, flagged, unflagged, answered")),
                }
            }
            "before" | "after" => match parse_day(&val, now, tz_offset) {
                Some(ts) => out.terms.push(Term::Date { before: op == "before", ts, neg }),
                None => out.hints.push(format!("Invalid date \"{val}\"; use YYYY-MM-DD, today or yesterday")),
            },
            "older_than" | "newer_than" => match parse_span(&val) {
                Some(span) => out.terms.push(Term::Date { before: op == "older_than", ts: now - span, neg }),
                None => out.hints.push(format!("Invalid span \"{val}\"; use e.g. 7d, 2w, 3m")),
            },
            "has" => match val.to_ascii_lowercase().as_str() {
                "attachment" | "attachments" => out.terms.push(Term::HasAttachment { neg }),
                _ => out.hints.push(format!("Unknown has:{val}; use has:attachment")),
            },
            _ => {}
        }
    }
    out
}

fn push_text(out: &mut ParsedQuery, field: TextField, value: String, neg: bool) {
    if value.is_empty() {
        return;
    }
    // A phrase with no letter or digit has no FTS tokens; matching it would be meaningless.
    if !value.chars().any(char::is_alphanumeric) {
        out.hints.push(format!("Ignored \"{value}\": it has no searchable words"));
        return;
    }
    out.terms.push(Term::Text { field, value, neg });
}

/// One FTS5 phrase with optional column filter and prefix match on its last token.
/// Everything the user typed sits inside the double quotes (inner quotes doubled).
fn fts_phrase(field: TextField, value: &str) -> String {
    let q = format!("\"{}\"*", value.replace('"', "\"\""));
    match field.column() {
        Some(c) => format!("{c} : {q}"),
        None => q,
    }
}

#[derive(Debug, Clone)]
pub struct SearchParams {
    pub query: String,
    /// Only messages in these folders can be returned. Empty = nothing.
    pub scopes: Vec<ScopeKey>,
    pub limit: u32,
    pub offset: u32,
    pub now: i64,
    /// Seconds east of UTC used for `today`/`before:`/`after:` day boundaries.
    pub tz_offset: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub summary: MessageSummary,
    /// Looks like a newsletter / automated mail (List-Id, Precedence: bulk|list|junk, Auto-Submitted).
    pub bulk: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    /// Total matches (not limited).
    pub total: u32,
    pub hints: Vec<String>,
}

const SUMMARY_SELECT: &str = "account, folder, uid, uidvalidity, message_id, subject, from_addr, to_addr, date, seen, flagged, answered, draft, deleted, snippet, (body_raw IS NOT NULL), size, \
    (list_id != '' OR lower(precedence) IN ('bulk','list','junk') OR (auto_submitted != '' AND lower(auto_submitted) != 'no'))";

fn hit_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        summary: MessageSummary {
            account: r.get(0)?,
            folder: r.get(1)?,
            uid: r.get(2)?,
            uidvalidity: r.get(3)?,
            message_id: r.get(4)?,
            subject: r.get(5)?,
            from: r.get(6)?,
            to: r.get(7)?,
            date: r.get(8)?,
            flags: crate::types::Flags {
                seen: r.get(9)?,
                flagged: r.get(10)?,
                answered: r.get(11)?,
                draft: r.get(12)?,
                deleted: r.get(13)?,
            },
            snippet: r.get(14)?,
            has_body: r.get(15)?,
            size: r.get(16)?,
        },
        bulk: r.get(17)?,
    })
}

/// WHERE clause + params for the scopes. Returns `None` when there is no scope (no results).
fn scope_clause(scopes: &[ScopeKey], params: &mut Vec<Value>) -> Option<String> {
    if scopes.is_empty() {
        return None;
    }
    // One `account = ? AND folder IN (...)` per account: the expression tree stays as shallow as the number
    // of accounts, whereas one OR-term per folder would hit SQLite's expression depth limit (1000) on a
    // large all-folders search.
    let mut by_account: Vec<(&str, Vec<&str>)> = Vec::new();
    for s in scopes {
        match by_account.iter_mut().find(|(a, _)| *a == s.account) {
            Some((_, v)) => v.push(&s.folder),
            None => by_account.push((&s.account, vec![&s.folder])),
        }
    }
    let mut parts = Vec::new();
    for (account, folders) in by_account {
        params.push(Value::Text(account.to_string()));
        for f in &folders {
            params.push(Value::Text((*f).to_string()));
        }
        parts.push(format!("(account = ? AND folder IN ({}))", vec!["?"; folders.len()].join(",")));
    }
    Some(format!("({})", parts.join(" OR ")))
}

/// Most message-ids `thread_of` will follow (bounds SQL size against hostile References headers).
const MAX_THREAD_IDS: usize = 200;

impl Storage {
    /// Search the local cache. See the module docs for the query language and safety notes.
    pub fn search_messages(&self, p: &SearchParams) -> Result<SearchResult> {
        let parsed = parse_query(&p.query, p.now, p.tz_offset);
        let mut res = SearchResult { hints: parsed.hints.clone(), ..Default::default() };
        let mut params: Vec<Value> = Vec::new();
        let Some(scope) = scope_clause(&p.scopes, &mut params) else { return Ok(res) };
        let mut wheres = vec![scope];

        let mut positive: Vec<String> = Vec::new();
        for t in &parsed.terms {
            match t {
                Term::Text { field, value, neg: false } => positive.push(fts_phrase(*field, value)),
                Term::Text { field, value, neg: true } => {
                    wheres.push("messages.rowid NOT IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)".into());
                    params.push(Value::Text(fts_phrase(*field, value)));
                }
                Term::Is { value, neg } => {
                    let cond = match value {
                        IsValue::Unread => "seen = 0",
                        IsValue::Read => "seen = 1",
                        IsValue::Flagged => "flagged = 1",
                        IsValue::Unflagged => "flagged = 0",
                        IsValue::Answered => "answered = 1",
                        IsValue::Unanswered => "answered = 0",
                    };
                    wheres.push(if *neg { format!("NOT ({cond})") } else { cond.into() });
                }
                Term::Date { before, ts, neg } => {
                    let cond = if *before { "date < ?" } else { "date >= ?" };
                    wheres.push(if *neg { format!("NOT ({cond})") } else { cond.into() });
                    params.push(Value::Integer(*ts));
                }
                Term::Cc { value, neg } => {
                    let cond = "instr(lower(cc), ?) > 0";
                    wheres.push(if *neg { format!("NOT ({cond})") } else { cond.into() });
                    params.push(Value::Text(value.clone()));
                }
                Term::HasAttachment { neg } => wheres.push(if *neg { "has_attachment = 0".into() } else { "has_attachment = 1".into() }),
                Term::Folder { value, neg } => {
                    // Exact folder name, last path part (after / or .), or role; no LIKE, so no wildcard injection.
                    let cond = "EXISTS (SELECT 1 FROM folders f WHERE f.account = messages.account AND f.name = messages.folder AND \
                        (lower(f.name) = ? OR lower(f.role) = ? OR substr(lower(f.name), -length(?) - 1) IN ('/' || ?, '.' || ?)))";
                    wheres.push(if *neg { format!("NOT ({cond})") } else { cond.into() });
                    for _ in 0..5 {
                        params.push(Value::Text(value.clone()));
                    }
                }
            }
        }
        if !positive.is_empty() {
            // One MATCH with all positive phrases ANDed (each phrase is quoted, so AND here is ours).
            wheres.push("messages.rowid IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)".into());
            params.push(Value::Text(positive.join(" AND ")));
        }
        let where_sql = wheres.join(" AND ");
        let c = self.lock()?;
        res.total = c.query_row(&format!("SELECT COUNT(*) FROM messages WHERE {where_sql}"), params_from_iter(params.iter()), |r| r.get(0))?;
        let mut q_params = params.clone();
        q_params.push(Value::Integer(p.limit.clamp(1, MAX_RESULTS) as i64));
        q_params.push(Value::Integer(p.offset.min(10_000) as i64));
        let mut st = c.prepare(&format!(
            "SELECT {SUMMARY_SELECT} FROM messages WHERE {where_sql} ORDER BY date DESC, uid DESC LIMIT ? OFFSET ?"
        ))?;
        let rows = st.query_map(params_from_iter(q_params.iter()), hit_from_row)?;
        res.hits = rows.collect::<std::result::Result<_, _>>()?;
        Ok(res)
    }

    /// Per-folder counts for the AI `mail_stats` / `list_folders` tools.
    pub fn folder_stats(&self, account: &str, folder: &str) -> Result<FolderStats> {
        Ok(self.lock()?.query_row(
            "SELECT COUNT(*), COALESCE(SUM(seen = 0), 0), COALESCE(SUM(flagged), 0), MIN(date), MAX(date)
             FROM messages WHERE account=?1 AND folder=?2",
            [account, folder],
            |r| {
                Ok(FolderStats {
                    total: r.get(0)?,
                    unread: r.get(1)?,
                    flagged: r.get(2)?,
                    oldest: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    newest: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                })
            },
        )?)
    }

    /// True when `address` appears in the From or To field of any cached message of any account and
    /// folder (so Sent mail counts). Case-insensitive; LIKE wildcards in the input are escaped.
    pub fn address_seen(&self, address: &str) -> Result<bool> {
        let a = address.trim().to_ascii_lowercase(); // SQL lower() is ASCII-only
        if a.is_empty() {
            return Ok(false);
        }
        let pat = format!("%{}%", a.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let c = self.lock()?;
        let mut st = c.prepare(
            "SELECT lower(from_addr), lower(to_addr) FROM messages WHERE lower(from_addr) LIKE ?1 ESCAPE '\\' OR lower(to_addr) LIKE ?1 ESCAPE '\\' LIMIT 200",
        )?;
        let rows = st.query_map(rusqlite::params![pat], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        // The LIKE is a substring match ("abob@x.org" contains "bob@x.org"): confirm a whole-address match.
        let whole = |field: &str| field.split(|ch: char| !(ch.is_alphanumeric() || "._%+-@".contains(ch))).any(|t| t == a);
        for row in rows {
            let (f, t) = row?;
            if whole(&f) || whole(&t) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Most frequent senders (display strings) across the scopes.
    pub fn top_senders(&self, scopes: &[ScopeKey], limit: u32) -> Result<Vec<(String, u32)>> {
        let mut params = Vec::new();
        let Some(scope) = scope_clause(scopes, &mut params) else { return Ok(vec![]) };
        params.push(Value::Integer(limit.clamp(1, 20) as i64));
        let c = self.lock()?;
        let mut st = c.prepare(&format!(
            "SELECT from_addr, COUNT(*) AS n FROM messages WHERE {scope} AND from_addr != '' GROUP BY from_addr ORDER BY n DESC, from_addr LIMIT ?"
        ))?;
        let rows = st.query_map(params_from_iter(params.iter()), |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Messages of the same conversation as `(account, folder, uid)`, oldest first, restricted to
    /// `scopes`. Links by Message-ID / In-Reply-To / References; when the message has no such links
    /// (or nothing else matches) falls back to an identical normalised subject. Always includes the
    /// message itself when it is in scope.
    pub fn thread_of(&self, account: &str, folder: &str, uid: u32, uidvalidity: u32, scopes: &[ScopeKey], limit: u32) -> Result<Vec<MessageSummary>> {
        let mut sp = Vec::new();
        let Some(scope) = scope_clause(scopes, &mut sp) else { return Ok(vec![]) };
        let limit = limit.clamp(1, 50) as usize;
        let c = self.lock()?;
        let me: Option<(Option<String>, String, String, String)> = c
            .query_row(
                "SELECT message_id, in_reply_to, refs, subject FROM messages WHERE account=?1 AND folder=?2 AND uid=?3 AND uidvalidity=?4",
                rusqlite::params![account, folder, uid, uidvalidity],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((mid, irt, refs, subject)) = me else { return Ok(vec![]) };

        // Grow the id set twice: ancestors named by the message, then everything that names any of them.
        let mut ids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        ids.extend(mid.iter().filter(|m| !m.is_empty()).cloned());
        ids.extend(irt.split_whitespace().take(MAX_THREAD_IDS).map(str::to_string));
        ids.extend(refs.split_whitespace().take(MAX_THREAD_IDS).map(str::to_string));
        let mut found: Vec<(MessageSummary, String)> = Vec::new();
        for _ in 0..2 {
            if ids.is_empty() {
                break;
            }
            // A hostile header can name thousands of ids; keep the query and the walk bounded.
            let id_list: Vec<String> = ids.iter().take(MAX_THREAD_IDS).cloned().collect();
            let mut conds = Vec::new();
            let mut params: Vec<Value> = vec![Value::Text(account.to_string())];
            for id in &id_list {
                conds.push("(message_id = ? OR instr(' ' || in_reply_to || ' ', ' ' || ? || ' ') > 0 OR instr(' ' || refs || ' ', ' ' || ? || ' ') > 0)");
                for _ in 0..3 {
                    params.push(Value::Text(id.clone()));
                }
            }
            params.extend(sp.iter().cloned());
            let sql = format!(
                "SELECT {SUMMARY_SELECT}, message_id, in_reply_to, refs FROM messages WHERE account = ? AND ({}) AND {scope} ORDER BY date, uid LIMIT 200",
                conds.join(" OR ")
            );
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map(params_from_iter(params.iter()), |r| {
                let h = hit_from_row(r)?;
                let m: Option<String> = r.get(18)?;
                let i: String = r.get(19)?;
                let f: String = r.get(20)?;
                Ok((h.summary, format!("{} {} {}", m.unwrap_or_default(), i, f)))
            })?;
            found = rows.collect::<std::result::Result<_, _>>()?;
            let before = ids.len();
            'grow: for (_, linked) in &found {
                for t in linked.split_whitespace() {
                    if ids.len() >= MAX_THREAD_IDS {
                        break 'grow;
                    }
                    ids.insert(t.to_string());
                }
            }
            if ids.len() == before || ids.len() >= MAX_THREAD_IDS {
                break;
            }
        }
        let mut out: Vec<MessageSummary> = found.into_iter().map(|(s, _)| s).collect();

        let core = normalize_subject(&subject);
        if out.len() <= 1 && !core.is_empty() {
            // Subject fallback: narrow in SQL (escaped LIKE), confirm in Rust.
            let esc = core.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let mut params: Vec<Value> = vec![Value::Text(account.to_string()), Value::Text(format!("%{esc}%"))];
            params.extend(sp.iter().cloned());
            let sql = format!(
                "SELECT {SUMMARY_SELECT} FROM messages WHERE account = ? AND subject LIKE ? ESCAPE '\\' AND {scope} ORDER BY date, uid LIMIT 200"
            );
            let mut st = c.prepare(&sql)?;
            let rows = st.query_map(params_from_iter(params.iter()), hit_from_row)?;
            for h in rows {
                let s = h?.summary;
                if normalize_subject(&s.subject) == core && !out.iter().any(|o| o.folder == s.folder && o.uid == s.uid) {
                    out.push(s);
                }
            }
            out.sort_by(|a, b| a.date.cmp(&b.date).then(a.uid.cmp(&b.uid)));
        }
        // Keep the requested message in view if the list has to be cut: take the newest `limit`.
        if out.len() > limit {
            let cut = out.len() - limit;
            out.drain(..cut);
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FolderStats {
    pub total: u32,
    pub unread: u32,
    pub flagged: u32,
    pub oldest: i64,
    pub newest: i64,
}

/// `subject` without ONE leading reply/forward marker, or None. Markers (ASCII case-insensitive): `Re`, `Fwd`, `Fw`,
/// `Aw`, `Sv`, `Wg`, optionally followed by a counter `[2]` / `(2)`, with optional spaces before the counter and the
/// colon (`RE :` as French Outlook writes it, `Re[2]:`, `Re(2):`).
pub fn strip_reply_marker(subject: &str) -> Option<&str> {
    let b = subject.as_bytes();
    let word = ["fwd", "re", "fw", "aw", "sv", "wg"].iter().find(|w| b.len() >= w.len() && b[..w.len()].eq_ignore_ascii_case(w.as_bytes()))?;
    let skip_ws = |mut i: usize| {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    let mut i = skip_ws(word.len());
    if i < b.len() && (b[i] == b'[' || b[i] == b'(') {
        let close = if b[i] == b'[' { b']' } else { b')' };
        let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || b.get(i + 1 + digits) != Some(&close) {
            return None;
        }
        i = skip_ws(i + digits + 2);
    }
    if b.get(i) != Some(&b':') {
        return None;
    }
    Some(subject[i + 1..].trim_start())
}

/// Lower-cased subject without leading `Re:` / `Fwd:` / `Fw:` / `Aw:` / `Sv:` / `Wg:` markers (see [`strip_reply_marker`]).
pub fn normalize_subject(s: &str) -> String {
    let mut t = s.trim();
    while let Some(rest) = strip_reply_marker(t) {
        t = rest;
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    const ACC: &str = "me@example.org";
    const NOW: i64 = 1_800_000_000;

    fn setup() -> Storage {
        let s = Storage::open_in_memory().unwrap();
        s.upsert_account(&AccountConfig {
            email: ACC.into(),
            username: "me".into(),
            imap: ServerConfig { host: "i".into(), port: 993, tls: Tls::Implicit },
            smtp: ServerConfig { host: "s".into(), port: 587, tls: Tls::StartTls },
        })
        .unwrap();
        let f = |n: &str, r| Folder { name: n.into(), delimiter: Some("/".into()), role: r, selectable: true };
        s.replace_folders(ACC, &[f("INBOX", FolderRole::Inbox), f("Archive", FolderRole::Archive), f("Secret", FolderRole::Other)]).unwrap();
        s
    }

    #[allow(clippy::too_many_arguments)]
    fn put(s: &Storage, folder: &str, uid: u32, date: i64, from: &str, subject: &str, seen: bool, mid: &str) {
        s.upsert_message(&MessageSummary {
            account: ACC.into(),
            folder: folder.into(),
            uid,
            uidvalidity: 7,
            message_id: Some(mid.into()),
            subject: subject.into(),
            from: from.into(),
            to: ACC.into(),
            date,
            flags: Flags { seen, ..Default::default() },
            snippet: String::new(),
            has_body: false,
            size: 1,
        })
        .unwrap();
    }

    fn q(s: &Storage, query: &str, scopes: &[(&str, &str)]) -> SearchResult {
        s.search_messages(&SearchParams {
            query: query.into(),
            scopes: scopes.iter().map(|(a, f)| ScopeKey::new(*a, *f)).collect(),
            limit: 50,
            offset: 0,
            now: NOW,
            tz_offset: 0,
        })
        .unwrap()
    }
    fn uids(r: &SearchResult) -> Vec<u32> {
        r.hits.iter().map(|h| h.summary.uid).collect()
    }
    const INBOX: &[(&str, &str)] = &[(ACC, "INBOX")];

    fn fixture() -> Storage {
        let s = setup();
        put(&s, "INBOX", 1, NOW - 100, "Alice Example <alice@example.com>", "Quarterly invoice", false, "a1@x");
        put(&s, "INBOX", 2, NOW - 200, "Bob <bob@shop.test>", "Lunch on Friday?", true, "b1@x");
        put(&s, "INBOX", 3, NOW - 90 * DAY, "News <news@letters.test>", "Weekly digest: café", true, "n1@x");
        put(&s, "Secret", 9, NOW - 50, "Alice Example <alice@example.com>", "Secret invoice plans", false, "s1@x");
        s
    }

    #[test]
    fn operators_and_scope() {
        let s = fixture();
        assert_eq!(uids(&q(&s, "invoice", INBOX)), vec![1]);
        assert_eq!(uids(&q(&s, "from:alice", INBOX)), vec![1]);
        assert_eq!(uids(&q(&s, "from:alice@example.com", INBOX)), vec![1]);
        assert_eq!(uids(&q(&s, "subject:lunch", INBOX)), vec![2]);
        assert_eq!(uids(&q(&s, "is:unread", INBOX)), vec![1]);
        assert_eq!(uids(&q(&s, "-is:unread", INBOX)), vec![2, 3]);
        assert_eq!(uids(&q(&s, "-from:alice", INBOX)), vec![2, 3]);
        assert_eq!(uids(&q(&s, "older_than:30d", INBOX)), vec![3]);
        assert_eq!(uids(&q(&s, "newer_than:30d", INBOX)), vec![1, 2]);
        assert_eq!(uids(&q(&s, "cafe", INBOX)), vec![3], "diacritics folded");
        assert_eq!(uids(&q(&s, "\"lunch on\"", INBOX)), vec![2]);
        assert_eq!(uids(&q(&s, "from:alice invoice", INBOX)), vec![1]);
        // scope is enforced in SQL: the Secret folder never appears unless it is in scope
        assert_eq!(uids(&q(&s, "invoice", INBOX)), vec![1]);
        assert_eq!(uids(&q(&s, "invoice", &[(ACC, "INBOX"), (ACC, "Secret")])), vec![9, 1]);
        assert!(q(&s, "invoice", &[]).hits.is_empty());
        assert!(q(&s, "invoice", &[("other@x", "INBOX")]).hits.is_empty());
        let r = q(&s, "is:bogus has:nothing", INBOX);
        assert_eq!(r.hints.len(), 2);
        assert_eq!(r.total, 3);
    }

    #[test]
    fn huge_scope_list_does_not_hit_the_expression_depth_limit() {
        let s = fixture();
        let mut scopes: Vec<(String, String)> = (0..3000).map(|i| (ACC.to_string(), format!("F{i}"))).collect();
        scopes.push((ACC.to_string(), "INBOX".to_string()));
        scopes.push(("other@x".to_string(), "INBOX".to_string()));
        let r = s
            .search_messages(&SearchParams {
                query: "invoice".into(),
                scopes: scopes.iter().map(|(a, f)| ScopeKey::new(a.as_str(), f.as_str())).collect(),
                limit: 50,
                offset: 0,
                now: NOW,
                tz_offset: 0,
            })
            .unwrap();
        assert_eq!(uids(&r), vec![1]);
    }

    #[test]
    fn non_ascii_folder_and_cc_values_compare_consistently() {
        let s = fixture();
        let f = |n: &str, r| Folder { name: n.into(), delimiter: Some("/".into()), role: r, selectable: true };
        s.replace_folders(ACC, &[f("INBOX", FolderRole::Inbox), f("Archive", FolderRole::Archive), f("Secret", FolderRole::Other), f("École", FolderRole::Other)]).unwrap();
        put(&s, "École", 1, NOW - 10, "x@y.test", "school", false, "e1@x");
        let all = &[(ACC, "INBOX"), (ACC, "École")];
        assert_eq!(uids(&q(&s, "folder:École", all)), vec![1], "same spelling matches");
        assert_eq!(uids(&q(&s, "folder:école", all)), Vec::<u32>::new(), "no half-way Unicode folding on one side only");
        assert_eq!(q(&s, "folder:INBOX", all).hits.len(), 3);
        assert_eq!(q(&s, "folder:inbox", all).hits.len(), 3, "ASCII stays case-insensitive");
    }

    #[test]
    fn cc_attachment_and_folder_operators() {
        let s = fixture();
        let h = crate::mime::ExtraHeaders { cc: "Carol <carol@x.test>".into(), attachment_hint: true, ..Default::default() };
        s.set_extra_headers(ACC, "INBOX", 1, &h).unwrap();
        s.set_extra_headers(ACC, "Secret", 9, &crate::mime::ExtraHeaders { cc: "carol@x.test".into(), ..Default::default() }).unwrap();
        let all: &[(&str, &str)] = &[(ACC, "INBOX"), (ACC, "Secret"), (ACC, "Archive")];
        assert_eq!(uids(&q(&s, "cc:carol", all)), vec![9, 1]);
        assert_eq!(uids(&q(&s, "cc:CAROL@x.test", INBOX)), vec![1]);
        assert_eq!(uids(&q(&s, "-cc:carol", INBOX)), vec![2, 3]);
        assert_eq!(uids(&q(&s, "has:attachment", all)), vec![1]);
        assert_eq!(uids(&q(&s, "-has:attachment", INBOX)), vec![2, 3]);
        assert!(q(&s, "has:nothing", INBOX).hints[0].contains("has:attachment"));
        // folder: exact name, role, and negation; scope still wins
        assert_eq!(uids(&q(&s, "folder:secret", all)), vec![9]);
        assert_eq!(uids(&q(&s, "folder:inbox invoice", all)), vec![1]);
        assert_eq!(uids(&q(&s, "-folder:inbox", all)), vec![9]);
        assert!(q(&s, "folder:secret", INBOX).hits.is_empty());
        // the body parse refines the header guess: a plain text body has no attachment
        let raw = b"From: a@x\r\nSubject: x\r\n\r\nhello";
        s.set_body(ACC, "INBOX", 1, 7, raw, "hello").unwrap();
        assert!(q(&s, "has:attachment", all).hits.is_empty());
        let mime = b"From: a@x\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nhi\r\n--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=a.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nQUJD\r\n--b--\r\n";
        s.set_body(ACC, "INBOX", 2, 7, mime, "hi").unwrap();
        assert_eq!(uids(&q(&s, "has:attachment", all)), vec![2]);
        // header sync after the body is known must not undo the refinement
        s.set_extra_headers(ACC, "INBOX", 1, &h).unwrap();
        assert!(!uids(&q(&s, "has:attachment", all)).contains(&1));
    }

    #[test]
    fn dates() {
        let s = fixture();
        let day = |ts: i64| {
            let (y, m, d) = civil_from_days(ts.div_euclid(DAY));
            format!("{y:04}-{m:02}-{d:02}")
        };
        let d = day(NOW - 90 * DAY);
        assert_eq!(uids(&q(&s, &format!("before:{d}"), INBOX)), Vec::<u32>::new());
        assert_eq!(uids(&q(&s, &format!("after:{d}"), INBOX)), vec![1, 2, 3]);
        assert!(!q(&s, "before:2026-02-30", INBOX).hints.is_empty(), "invalid date hinted");
        assert_eq!(parse_day("2026-01-31", NOW, 0), Some(days_from_civil(2026, 1, 31) * DAY));
        assert_eq!(parse_day("2026-13-01", NOW, 0), None);
        assert_eq!(parse_span("2w"), Some(14 * DAY));
        assert_eq!(parse_span("w"), None);
    }

    #[test]
    fn hostile_queries_are_inert() {
        let s = fixture();
        let long = "a ".repeat(2000);
        let hostile = [
            "\"; DROP TABLE messages; --",
            "' OR 1=1 --",
            "invoice\"",
            "\"",
            "\"\"",
            "***",
            "*",
            "NEAR(invoice lunch, 1)",
            "invoice OR lunch",
            "invoice AND NOT lunch",
            "body_text:secret",
            "subject:\"x\" OR \"y",
            "from:",
            "-",
            "--",
            "(((",
            "a:b:c",
            "^invoice",
            "invoice*",
            "{subject}: invoice",
            "messages_fts:invoice",
            "\u{0}\u{1}\u{2}",
            "from:\"\" to:\"\"",
            "-\"\"",
            long.as_str(),
            "rank MATCH 1",
            "\\",
        ];
        for h in hostile {
            // must not error, panic, or change the schema
            let r = s.search_messages(&SearchParams {
                query: h.into(),
                scopes: vec![ScopeKey::new(ACC, "INBOX")],
                limit: 10,
                offset: 0,
                now: NOW,
                tz_offset: 0,
            });
            assert!(r.is_ok(), "query {h:?} failed: {:?}", r.err());
        }
        // operators in a query are literal words, not FTS syntax
        assert!(q(&s, "invoice OR lunch", INBOX).hits.is_empty(), "OR is a word, not an operator");
        assert!(q(&s, "body_text:secret", INBOX).hits.is_empty());
        assert!(q(&s, "invoice\"", INBOX).hits.len() <= 1);
        assert_eq!(s.message_count(ACC, "INBOX").unwrap(), 3, "table intact");
        assert_eq!(uids(&q(&s, "invoice", INBOX)), vec![1]);
    }

    #[test]
    fn fts_follows_updates_deletes_and_cascade() {
        let s = fixture();
        put(&s, "INBOX", 1, NOW - 100, "Alice <alice@example.com>", "Renamed subject", false, "a1@x");
        assert!(q(&s, "invoice", INBOX).hits.is_empty());
        assert_eq!(uids(&q(&s, "renamed", INBOX)), vec![1]);
        s.delete_message(ACC, "INBOX", 1).unwrap();
        assert!(q(&s, "renamed", INBOX).hits.is_empty());
        s.clear_folder_messages(ACC, "INBOX").unwrap();
        assert!(q(&s, "lunch", INBOX).hits.is_empty());
        // cascade from deleting the account
        assert_eq!(uids(&q(&s, "invoice", &[(ACC, "Secret")])), vec![9]);
        s.delete_account(ACC).unwrap();
        assert!(q(&s, "invoice", &[(ACC, "Secret")]).hits.is_empty());
    }

    #[test]
    fn address_seen_matches_whole_addresses_in_from_and_to() {
        let s = fixture();
        s.lock().unwrap().execute("UPDATE messages SET from_addr = 'Bob <Bob@Shop.test>', to_addr = 'carol@other.test, \"Dan\" <dan_x@other.test>' WHERE uid = 1", []).unwrap();
        assert!(s.address_seen("bob@shop.test").unwrap(), "case-insensitive, From");
        assert!(s.address_seen("CAROL@other.test").unwrap(), "To");
        assert!(s.address_seen("dan_x@other.test").unwrap(), "underscore is literal");
        assert!(!s.address_seen("ob@shop.test").unwrap(), "a suffix of a known address is not that address");
        assert!(!s.address_seen("bob@shop.te").unwrap());
        assert!(!s.address_seen("dan%x@other.test").unwrap() && !s.address_seen("%").unwrap(), "wildcards are escaped");
        assert!(!s.address_seen("").unwrap());
    }

    #[test]
    fn body_text_is_indexed_on_set_body_and_backfilled() {
        let s = fixture();
        let raw = b"From: a@x\r\nSubject: hi\r\nMessage-ID: <a1@x>\r\nList-Id: <l.example>\r\nIn-Reply-To: <zzz@x>\r\nContent-Type: text/plain\r\n\r\nThe zebra crossing is closed.\r\n";
        assert!(q(&s, "zebra", INBOX).hits.is_empty());
        s.set_body(ACC, "INBOX", 1, 7, raw, "snip").unwrap();
        assert_eq!(uids(&q(&s, "zebra", INBOX)), vec![1]);
        assert_eq!(s.get_body_text(ACC, "INBOX", 1).unwrap().unwrap().trim(), "The zebra crossing is closed.");
        assert!(s.get_body_text(ACC, "INBOX", 2).unwrap().is_none(), "not indexed yet");
        let h = s.get_extra_headers(ACC, "INBOX", 1).unwrap().unwrap();
        assert_eq!((h.list_id.as_str(), h.in_reply_to.as_str()), ("<l.example>", "zzz@x"));
        assert!(q(&s, "invoice", INBOX).hits[0].bulk, "list-id marks bulk");
        // backfill: simulate a pre-migration row (body cached, not indexed)
        s.lock().unwrap().execute("UPDATE messages SET body_indexed = 0, body_text = '' WHERE uid = 1", []).unwrap();
        assert!(q(&s, "zebra", INBOX).hits.is_empty());
        assert_eq!(s.backfill_body_text(10).unwrap(), 1);
        assert_eq!(s.backfill_body_text(10).unwrap(), 0);
        assert_eq!(uids(&q(&s, "zebra", INBOX)), vec![1]);
        s.rebuild_fts().unwrap();
        assert_eq!(uids(&q(&s, "zebra", INBOX)), vec![1]);
    }

    #[test]
    fn migration_from_v1_backfills_fts_for_existing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("old.db");
        {
            // Build a v1 database by hand: apply only the first migration.
            let c = rusqlite::Connection::open(&p).unwrap();
            c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
            c.execute_batch(crate::storage::V1_FOR_TESTS).unwrap();
            c.execute_batch("PRAGMA user_version = 1;").unwrap();
            c.execute_batch("INSERT INTO accounts VALUES ('me@example.org','me','{}'); INSERT INTO folders(account,name,role) VALUES ('me@example.org','INBOX','inbox'); INSERT INTO messages(account,folder,uid,uidvalidity,subject,from_addr,to_addr,date) VALUES ('me@example.org','INBOX',1,7,'Old budget memo','x@y','me@example.org',5);").unwrap();
        }
        let s = Storage::open(&p).unwrap();
        assert!(s.schema_version().unwrap() >= 4);
        assert_eq!(uids(&q(&s, "budget", INBOX)), vec![1]);
    }

    #[test]
    fn threads_by_references_and_subject() {
        let s = setup();
        put(&s, "INBOX", 1, 100, "a@x", "Project plan", true, "p1@x");
        put(&s, "INBOX", 2, 200, "b@x", "Re: Project plan", true, "p2@x");
        put(&s, "Archive", 3, 300, "a@x", "Re: Re: Project plan", true, "p3@x");
        put(&s, "INBOX", 4, 400, "z@x", "Unrelated", true, "u1@x");
        put(&s, "Secret", 5, 500, "z@x", "Re: Project plan", true, "p5@x");
        let h = |refs: &str, irt: &str| crate::mime::ExtraHeaders { references: refs.into(), in_reply_to: irt.into(), ..Default::default() };
        s.set_extra_headers(ACC, "INBOX", 2, &h("p1@x", "p1@x")).unwrap();
        s.set_extra_headers(ACC, "Archive", 3, &h("p1@x p2@x", "p2@x")).unwrap();
        let scopes = [ScopeKey::new(ACC, "INBOX"), ScopeKey::new(ACC, "Archive")];
        let t = s.thread_of(ACC, "INBOX", 2, 7, &scopes, 10).unwrap();
        assert_eq!(t.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![1, 2, 3], "linked via references; Secret out of scope");
        // subject fallback: no links at all
        let t = s.thread_of(ACC, "INBOX", 1, 7, &[ScopeKey::new(ACC, "INBOX")], 10).unwrap();
        assert_eq!(t.iter().map(|m| m.uid).collect::<Vec<_>>(), vec![1, 2]);
        assert!(s.thread_of(ACC, "INBOX", 99, 7, &scopes, 10).unwrap().is_empty());
        assert!(s.thread_of(ACC, "INBOX", 1, 7, &[], 10).unwrap().is_empty());
        assert!(s.thread_of(ACC, "INBOX", 2, 8, &scopes, 10).unwrap().is_empty(), "wrong UIDVALIDITY finds nothing");
        assert_eq!(normalize_subject("Re: RE: Fwd:  Hello   World"), "hello world");
        for v in ["RE : Hello", "Re[2]: Hello", "Re(2): Hello", "AW: Hello", "AW : Hello", "SV: Hello", "Fwd: Hello", "FW: Hello", "Fwd[3] : Hello", "Re (2) : Re[10]: Hello", "WG: Hello", "re:Hello"] {
            assert_eq!(normalize_subject(v), "hello", "{v}");
        }
        // Not markers: words that merely start with the letters, or a counter without digits / without a close.
        for v in ["Report: Q3", "Reply later", "Re", "Re[x]: Hello", "Re[2: Hello", "Re(2]: Hello", "Resume: Hello", "Sverige: nyheter"] {
            assert_eq!(normalize_subject(v), v.to_lowercase(), "{v}");
        }
        assert_eq!(normalize_subject("Re: ünï"), "ünï");
    }

    #[test]
    fn thread_walk_is_bounded_against_hostile_references() {
        let s = setup();
        put(&s, "INBOX", 1, 100, "a@x", "Hostile", true, "h1@x");
        let refs: Vec<String> = (0..5000).map(|i| format!("r{i}@x")).collect();
        let h = crate::mime::ExtraHeaders { references: refs.join(" "), ..Default::default() };
        s.set_extra_headers(ACC, "INBOX", 1, &h).unwrap();
        for i in 0..300u32 {
            put(&s, "INBOX", 10 + i, 200 + i64::from(i), "b@x", "Other", true, &format!("o{i}@x"));
            let h = crate::mime::ExtraHeaders { references: format!("h1@x r{i}@x"), ..Default::default() };
            s.set_extra_headers(ACC, "INBOX", 10 + i, &h).unwrap();
        }
        let t = s.thread_of(ACC, "INBOX", 1, 7, &[ScopeKey::new(ACC, "INBOX")], 50).unwrap();
        assert!(!t.is_empty() && t.len() <= 50);
    }

    #[test]
    fn stats() {
        let s = fixture();
        let st = s.folder_stats(ACC, "INBOX").unwrap();
        assert_eq!((st.total, st.unread), (3, 1));
        let top = s.top_senders(&[ScopeKey::new(ACC, "INBOX"), ScopeKey::new(ACC, "Secret")], 3).unwrap();
        assert_eq!(top[0].1, 2);
    }
}
