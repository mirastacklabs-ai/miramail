//! AI-written drafts (AI chat P3).
//!
//! The model can only ask for a DRAFT. A draft is text plus recipients that the user may open in
//! Compose; the user then reads it and presses Send themselves. This module has no way to send
//! anything: there is no transport here, no dependency on the mail crate, and the [`DraftHost`] trait
//! is read-only. (`tests/draft_no_send.rs` checks this by scanning the source and the manifest.)
//!
//! What the app, not the model, decides:
//! * recipients of a reply: computed from the resolved message's Reply-To / From (and To / Cc for
//!   reply-all, minus the user's own addresses). The model supplies no address for a reply;
//! * the reply subject (`Re:`) and the quoted original;
//! * warnings: addresses the user's mailbox has never seen, a Reply-To that differs from the sender,
//!   links in the body, a very large recipient list.
//!
//! The model's words are the body (cleaned: control, bidi and zero-width characters removed, length
//! capped, plain text only) and the "AI's reason".

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::envelope::{clean_line, strip_hidden};
use crate::store::{Result as StoreResult, Store};
use crate::tools::{fmt_ts, BoxFut, Scope};

/// Longest body the model may write (characters).
pub const MAX_BODY_CHARS: usize = 8000;
pub const MAX_SUBJECT_CHARS: usize = 200;
/// Most recipients per field in `draft_new`.
pub const MAX_NEW_RECIPIENTS: usize = 10;
/// Most recipients a reply-all draft keeps.
pub const MAX_REPLY_RECIPIENTS: usize = 50;
/// Characters of the original message quoted under a reply.
pub const MAX_QUOTE_CHARS: usize = 4000;
/// Most drafts the model can queue in one user turn.
pub const MAX_PER_TURN: usize = 5;
pub const MAX_REASON_CHARS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    Reply,
    New,
}

impl DraftKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DraftKind::Reply => "reply",
            DraftKind::New => "new",
        }
    }
    pub fn parse(s: &str) -> Option<DraftKind> {
        match s {
            "reply" => Some(DraftKind::Reply),
            "new" => Some(DraftKind::New),
            _ => None,
        }
    }
}

/// The only states a draft has. There is no "sent": sending happens in Compose, outside this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftState {
    Proposed,
    Opened,
    Dismissed,
}

impl DraftState {
    pub fn as_str(self) -> &'static str {
        match self {
            DraftState::Proposed => "proposed",
            DraftState::Opened => "opened",
            DraftState::Dismissed => "dismissed",
        }
    }
    pub fn parse(s: &str) -> Option<DraftState> {
        match s {
            "proposed" => Some(DraftState::Proposed),
            "opened" => Some(DraftState::Opened),
            "dismissed" => Some(DraftState::Dismissed),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------------------------------
// Read-only host (implemented by the app)
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Addr {
    pub name: String,
    pub address: String,
}

/// What a reply is computed from. Read from the user's own mail store.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReplySource {
    pub subject: String,
    pub from: Vec<Addr>,
    pub reply_to: Vec<Addr>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    /// Message-ID without angle brackets.
    pub message_id: Option<String>,
    pub references: Vec<String>,
    /// Unix seconds, 0 = unknown.
    pub date: i64,
    pub body: String,
    /// The message sits in the account's Sent-role folder (so a From of the user's own address can be believed).
    pub in_sent: bool,
}

/// Read access needed to build a draft. There is no method that changes mail or sends anything.
pub trait DraftHost: Send + Sync {
    /// Headers and text of a cached message (the engine already checked scope and UIDVALIDITY).
    fn reply_source(&self, scope: &Scope, uid: u32, uidvalidity: u32) -> BoxFut<'_, Result<ReplySource, String>>;
    /// True when `address` appears as a sender or recipient anywhere in the user's cached mail (all folders, Sent included).
    fn address_known(&self, address: &str) -> Result<bool, String>;
    /// The addresses that are the user's own for `account` (never put into a reply's recipients).
    fn own_addresses(&self, account: &str) -> Result<Vec<String>, String>;
}

// ------------------------------------------------------------------------------------------
// Cleaning and addresses
// ------------------------------------------------------------------------------------------

/// Plain text only: control, bidi, zero-width and tag characters removed, CRLF normalised, trailing
/// blank space trimmed, at most [`MAX_BODY_CHARS`]. Returns the text and whether it was cut.
pub fn clean_body(s: &str) -> (String, bool) {
    let text = strip_hidden(s);
    let text = text.trim_matches(|c| c == '\n' || c == '\r').trim_end().to_string();
    let lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    let text = lines.join("\n");
    if text.chars().count() > MAX_BODY_CHARS {
        (text.chars().take(MAX_BODY_CHARS).collect(), true)
    } else {
        (text, false)
    }
}

/// A plain `local@domain.tld`: no display name, no angle brackets, no whitespace / control / hidden
/// characters, no list or quoting syntax.
pub fn valid_address(a: &str) -> bool {
    if a.len() < 3 || a.len() > 254 || strip_hidden(a) != a {
        return false;
    }
    if a.chars().any(|c| c.is_whitespace() || c.is_control() || "<>,;:\"()[]\\".contains(c)) {
        return false;
    }
    let mut it = a.split('@');
    let (Some(local), Some(domain), None) = (it.next(), it.next(), it.next()) else { return false };
    if local.is_empty() || local.len() > 64 || local.starts_with('.') || local.ends_with('.') {
        return false;
    }
    // Labels are ASCII letters, digits and '-' (IDNs arrive as punycode, xn--...).
    domain.contains('.') && domain.len() <= 253 && domain.split('.').all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// Parse a header-style list (`Name <a@b>, c@d`) into valid addresses; anything malformed is dropped.
pub fn parse_addr_list(s: &str) -> Vec<Addr> {
    let mut parts: Vec<String> = Vec::new();
    let (mut cur, mut quoted, mut angle) = (String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            '<' if !quoted => {
                angle = true;
                cur.push(c);
            }
            '>' if !quoted => {
                angle = false;
                cur.push(c);
            }
            ',' | ';' if !quoted && !angle => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .filter_map(|p| {
            let p = p.trim();
            let (name, addr) = match (p.rfind('<'), p.rfind('>')) {
                (Some(l), Some(r)) if l < r => (p[..l].trim().trim_matches('"').trim(), p[l + 1..r].trim()),
                _ => ("", p),
            };
            valid_address(addr).then(|| Addr { name: clean_line(name, 60), address: addr.to_string() })
        })
        .collect()
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn push_unique(out: &mut Vec<String>, a: &str) {
    if !out.iter().any(|x| same(x, a)) {
        out.push(a.to_string());
    }
}

/// `Re: subject` (kept as is when it already starts with Re:).
pub fn re_subject(subject: &str) -> String {
    let s = clean_line(subject, MAX_SUBJECT_CHARS);
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("re:") || lower.starts_with("re :") {
        s
    } else {
        format!("Re: {s}").trim().to_string()
    }
}

fn who(a: &Addr) -> String {
    if a.name.is_empty() {
        a.address.clone()
    } else {
        format!("{} <{}>", a.name, a.address)
    }
}

/// The quoted original appended under a reply (computed from the stored message, never by the model).
pub fn quote_block(src: &ReplySource) -> String {
    let original = strip_hidden(&src.body);
    let original = original.trim_end();
    let cut = original.chars().count() > MAX_QUOTE_CHARS;
    let shown: String = original.chars().take(MAX_QUOTE_CHARS).collect();
    let mut quoted: Vec<String> = shown.lines().map(|l| if l.starts_with('>') { format!(">{l}") } else { format!("> {l}") }).collect();
    if cut {
        quoted.push("> […]".into());
    }
    let sender = src.from.first().map(who).unwrap_or_else(|| "someone".into());
    let when = if src.date > 0 { fmt_ts(src.date) } else { "an earlier date".into() };
    format!("On {when}, {sender} wrote:\n{}", quoted.join("\n"))
}

// ------------------------------------------------------------------------------------------
// Planning (pure)
// ------------------------------------------------------------------------------------------

/// A draft ready to store. Everything except `body` and `reason` was computed by the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub kind: DraftKind,
    pub title: String,
    pub account: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub quote: String,
    pub in_reply_to: Option<String>,
    pub references: String,
    pub warnings: Vec<String>,
    pub new_addresses: Vec<String>,
}

fn link_warning(body: &str, warnings: &mut Vec<String>) {
    let n = body.matches("http://").count() + body.matches("https://").count();
    if n > 0 {
        warnings.push(format!("The text contains {n} link{}. Check where {} lead before sending.", if n == 1 { "" } else { "s" }, if n == 1 { "it" } else { "they" }));
    }
}

/// Compute a reply. `own` = the user's addresses. The model contributes only `body`.
pub fn plan_reply(account: &str, src: &ReplySource, own: &[String], reply_all: bool, body: &str, known: &dyn Fn(&str) -> bool) -> Result<Planned, String> {
    let is_own = |a: &str| own.iter().any(|o| same(o, a)) || same(account, a);
    let (body, cut) = clean_body(body);
    if body.is_empty() {
        return Err("The body is empty after removing unsupported characters.".into());
    }
    let mut warnings = Vec::new();
    if cut {
        warnings.push(format!("The text was cut at {MAX_BODY_CHARS} characters."));
    }

    // A reply to a message the user wrote goes to the people they wrote to. Only believed for mail in the Sent
    // folder: anything else can carry the user's own address in From (forged) and a To list of the sender's choosing.
    let from_me = src.in_sent && !src.from.is_empty() && src.from.iter().all(|a| is_own(&a.address));
    if !src.in_sent && !src.from.is_empty() && src.from.iter().all(|a| is_own(&a.address)) {
        warnings.push("This message claims to be from your own address but is not in your Sent folder. Check the recipient carefully.".into());
    }
    let primary: &[Addr] = if from_me {
        &src.to
    } else if !src.reply_to.is_empty() {
        &src.reply_to
    } else {
        &src.from
    };
    let mut to: Vec<String> = Vec::new();
    for a in primary {
        if !is_own(&a.address) {
            push_unique(&mut to, &a.address);
        }
    }
    if to.is_empty() {
        return Err("There is nobody to reply to in that message. Use draft_new with explicit recipients if the user asked for it.".into());
    }
    if !from_me && !src.reply_to.is_empty() {
        let differs = src.reply_to.iter().any(|r| !src.from.iter().any(|f| same(&f.address, &r.address)));
        if differs {
            let from = src.from.iter().map(|a| a.address.clone()).collect::<Vec<_>>().join(", ");
            warnings.push(format!("The message asks for replies to {} instead of the sender ({}).", to.join(", "), if from.is_empty() { "unknown".into() } else { from }));
        }
    }
    let mut cc: Vec<String> = Vec::new();
    if reply_all {
        let extra: Vec<&Addr> = if from_me { src.cc.iter().collect() } else { src.to.iter().chain(src.cc.iter()).collect() };
        for a in extra {
            if !is_own(&a.address) && !to.iter().any(|t| same(t, &a.address)) {
                push_unique(&mut cc, &a.address);
            }
        }
    }
    if to.len() + cc.len() > MAX_REPLY_RECIPIENTS {
        warnings.push(format!("The reply had more than {MAX_REPLY_RECIPIENTS} recipients; the rest were left out."));
        to.truncate(MAX_REPLY_RECIPIENTS);
        cc.truncate(MAX_REPLY_RECIPIENTS.saturating_sub(to.len()));
    }
    if to.len() + cc.len() >= 6 {
        warnings.push(format!("This goes to {} people.", to.len() + cc.len()));
    }
    // Recipients nobody in the mailbox has seen (a Reply-To pointing somewhere new, a forged From) are flagged.
    let mut new_addresses = Vec::new();
    for a in to.iter().chain(cc.iter()) {
        if !known(a) {
            new_addresses.push(a.clone());
        }
    }
    for a in new_addresses.iter().take(5) {
        warnings.push(format!("New address: {a} does not appear anywhere in your mail. Check it carefully."));
    }
    if new_addresses.len() > 5 {
        warnings.push(format!("{} more addresses are new to you.", new_addresses.len() - 5));
    }
    link_warning(&body, &mut warnings);

    let subject = re_subject(&src.subject);
    let title = format!("Reply to {}: {}", clean_line(&to[0], 80), clean_line(&subject, 80));
    Ok(Planned {
        kind: DraftKind::Reply,
        title,
        account: account.to_string(),
        to,
        cc,
        subject,
        body,
        quote: quote_block(src),
        in_reply_to: src.message_id.clone().filter(|m| !m.trim().is_empty()),
        references: src.references.join(" "),
        warnings,
        new_addresses,
    })
}

/// Validate one address list from the model. Errors name the offending value (cleaned).
pub fn check_new_recipients(field: &str, list: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for a in list {
        let t = a.trim();
        if !valid_address(t) {
            return Err(format!("\"{}\" in {field} is not a plain email address (name@example.com, no display name).", clean_line(t, 60)));
        }
        push_unique(&mut out, t);
    }
    if out.len() > MAX_NEW_RECIPIENTS {
        return Err(format!("{field} has too many addresses (maximum {MAX_NEW_RECIPIENTS})."));
    }
    Ok(out)
}

/// Compute a new message. The model supplies the addresses, so each one the mailbox has never seen is
/// flagged. `known` answers "has this address appeared in the user's mail before".
pub fn plan_new(account: &str, to: &[String], cc: &[String], subject: &str, body: &str, known: &dyn Fn(&str) -> bool) -> Result<Planned, String> {
    let to = check_new_recipients("to", to)?;
    let mut cc = check_new_recipients("cc", cc)?;
    if to.is_empty() {
        return Err("A new message needs at least one recipient in \"to\".".into());
    }
    cc.retain(|c| !to.iter().any(|t| same(t, c)));
    let subject = clean_line(subject, MAX_SUBJECT_CHARS);
    if subject.is_empty() {
        return Err("A new message needs a subject.".into());
    }
    let (body, cut) = clean_body(body);
    if body.is_empty() {
        return Err("The body is empty after removing unsupported characters.".into());
    }
    let mut warnings = Vec::new();
    let mut new_addresses = Vec::new();
    for a in to.iter().chain(cc.iter()) {
        if !known(a) {
            new_addresses.push(a.clone());
        }
    }
    for a in new_addresses.iter().take(5) {
        warnings.push(format!("New address: {a} does not appear anywhere in your mail. Check it carefully."));
    }
    if new_addresses.len() > 5 {
        warnings.push(format!("{} more addresses are new to you.", new_addresses.len() - 5));
    }
    if cut {
        warnings.push(format!("The text was cut at {MAX_BODY_CHARS} characters."));
    }
    link_warning(&body, &mut warnings);
    let more = if to.len() > 1 { format!(" +{}", to.len() - 1) } else { String::new() };
    let title = format!("New message to {}{more}: {}", clean_line(&to[0], 80), clean_line(&subject, 80));
    Ok(Planned {
        kind: DraftKind::New,
        title,
        account: account.to_string(),
        to,
        cc,
        subject,
        body,
        quote: String::new(),
        in_reply_to: None,
        references: String::new(),
        warnings,
        new_addresses,
    })
}

/// What Compose is opened with: the model's body followed by the app-built quote.
pub fn compose_text(body: &str, quote: &str) -> String {
    if quote.is_empty() {
        body.to_string()
    } else {
        format!("{body}\n\n{quote}\n")
    }
}

// ------------------------------------------------------------------------------------------
// Persistence
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub id: i64,
    pub conv_id: i64,
    pub kind: DraftKind,
    pub state: DraftState,
    pub title: String,
    pub account: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub quote: String,
    pub in_reply_to: Option<String>,
    pub references: String,
    pub reason: String,
    pub warnings: Vec<String>,
    pub new_addresses: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLS: &str = "id, conv_id, kind, state, title, account, to_json, cc_json, subject, body, quote, in_reply_to, refs, reason, warnings_json, new_json, created_at, updated_at";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Draft> {
    let list = |s: String| serde_json::from_str::<Vec<String>>(&s).unwrap_or_default();
    Ok(Draft {
        id: r.get(0)?,
        conv_id: r.get(1)?,
        kind: DraftKind::parse(&r.get::<_, String>(2)?).unwrap_or(DraftKind::New),
        state: DraftState::parse(&r.get::<_, String>(3)?).unwrap_or(DraftState::Dismissed),
        title: r.get(4)?,
        account: r.get(5)?,
        to: list(r.get(6)?),
        cc: list(r.get(7)?),
        subject: r.get(8)?,
        body: r.get(9)?,
        quote: r.get(10)?,
        in_reply_to: r.get(11)?,
        references: r.get(12)?,
        reason: r.get(13)?,
        warnings: list(r.get(14)?),
        new_addresses: list(r.get(15)?),
        created_at: r.get(16)?,
        updated_at: r.get(17)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftError {
    NotFound,
    /// The draft was dismissed (or is otherwise not in a state that allows the action).
    Illegal(DraftState),
    Db,
}

impl std::fmt::Display for DraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DraftError::NotFound => f.write_str("That draft no longer exists."),
            DraftError::Illegal(DraftState::Dismissed) => f.write_str("That draft was dismissed."),
            DraftError::Illegal(DraftState::Opened) => f.write_str("That draft was already opened in Compose."),
            DraftError::Illegal(DraftState::Proposed) => f.write_str("That draft cannot be changed right now."),
            DraftError::Db => f.write_str("The AI chat database could not be read."),
        }
    }
}

impl std::error::Error for DraftError {}

impl Store {
    /// Insert a `Proposed` draft. Returns the id.
    pub fn create_draft(&mut self, conv_id: i64, p: &Planned, reason: &str, now: i64) -> StoreResult<i64> {
        let c = self.conn_mut();
        c.execute(
            "INSERT INTO ai_drafts (conv_id, kind, state, title, account, to_json, cc_json, subject, body, quote, in_reply_to, refs, reason, warnings_json, new_json, created_at, updated_at)
             VALUES (?1,?2,'proposed',?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?15)",
            params![
                conv_id,
                p.kind.as_str(),
                p.title,
                p.account,
                serde_json::to_string(&p.to)?,
                serde_json::to_string(&p.cc)?,
                p.subject,
                p.body,
                p.quote,
                p.in_reply_to,
                p.references,
                reason,
                serde_json::to_string(&p.warnings)?,
                serde_json::to_string(&p.new_addresses)?,
                now
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn get_draft(&mut self, id: i64) -> StoreResult<Option<Draft>> {
        Ok(self.conn_mut().query_row(&format!("SELECT {COLS} FROM ai_drafts WHERE id=?1"), params![id], row).optional()?)
    }

    /// Newest first. `conv_id: None` = every conversation.
    pub fn list_drafts(&mut self, conv_id: Option<i64>, limit: u32) -> StoreResult<Vec<Draft>> {
        let c = self.conn_mut();
        let mut st = c.prepare(&format!("SELECT {COLS} FROM ai_drafts WHERE (?1 IS NULL OR conv_id=?1) ORDER BY created_at DESC, id DESC LIMIT ?2"))?;
        let rows = st.query_map(params![conv_id, limit], row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `Proposed | Opened -> Opened` (opening again is allowed: the user may have closed the Compose pane).
    /// A dismissed draft cannot be opened.
    pub fn mark_draft_opened(&mut self, id: i64, now: i64) -> std::result::Result<Draft, DraftError> {
        let n = self
            .conn_mut()
            .execute("UPDATE ai_drafts SET state='opened', updated_at=?2 WHERE id=?1 AND state IN ('proposed','opened')", params![id, now])
            .map_err(|_| DraftError::Db)?;
        let d = self.get_draft(id).map_err(|_| DraftError::Db)?.ok_or(DraftError::NotFound)?;
        if n == 1 {
            Ok(d)
        } else {
            Err(DraftError::Illegal(d.state))
        }
    }

    /// `Proposed -> Dismissed`.
    pub fn mark_draft_dismissed(&mut self, id: i64, now: i64) -> std::result::Result<Draft, DraftError> {
        let n = self
            .conn_mut()
            .execute("UPDATE ai_drafts SET state='dismissed', updated_at=?2 WHERE id=?1 AND state='proposed'", params![id, now])
            .map_err(|_| DraftError::Db)?;
        let d = self.get_draft(id).map_err(|_| DraftError::Db)?.ok_or(DraftError::NotFound)?;
        if n == 1 {
            Ok(d)
        } else {
            Err(DraftError::Illegal(d.state))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(name: &str, addr: &str) -> Addr {
        Addr { name: name.into(), address: addr.into() }
    }

    fn all_known(_: &str) -> bool {
        true
    }

    fn src() -> ReplySource {
        ReplySource {
            subject: "Invoice".into(),
            from: vec![a("Bob", "bob@shop.test")],
            reply_to: vec![],
            to: vec![a("", "me@example.org"), a("", "carol@other.test")],
            cc: vec![a("", "dave@other.test"), a("", "ME@example.org")],
            message_id: Some("abc@shop.test".into()),
            references: vec!["r1@x".into()],
            date: 1_793_000_000,
            body: "Please pay\n> older\nthanks".into(),
            in_sent: false,
        }
    }

    #[test]
    fn address_validation_is_strict() {
        for ok in ["a@b.co", "first.last+tag@sub.example.org"] {
            assert!(valid_address(ok), "{ok}");
        }
        for bad in ["", "a", "a@b", "a@@b.co", "a b@c.de", "Bob <a@b.co>", "a@b.co,c@d.ef", "a@b.co\n", ".a@b.co", "a@b..co", "a\u{202e}@b.co", "a@b.co;x", "\"a\"@b.co"] {
            assert!(!valid_address(bad), "{bad:?}");
        }
        assert!(!valid_address(&format!("{}@b.co", "x".repeat(65))));
    }

    #[test]
    fn address_lists_are_parsed_and_junk_dropped() {
        let l = parse_addr_list("\"Smith, Bob\" <bob@x.org>, carol@y.org; not-an-address, <dave@z.org>");
        assert_eq!(l.iter().map(|x| x.address.as_str()).collect::<Vec<_>>(), vec!["bob@x.org", "carol@y.org", "dave@z.org"]);
        assert_eq!(l[0].name, "Smith, Bob");
        assert!(parse_addr_list("").is_empty());
    }

    #[test]
    fn body_is_plain_capped_and_free_of_hidden_characters() {
        let (b, cut) = clean_body("\n\nHi\u{202e}there\u{200b}\r\nline\u{0007}2   \n\n\n");
        assert_eq!(b, "Hithere\nline2");
        assert!(!cut);
        let (b, cut) = clean_body(&"x".repeat(MAX_BODY_CHARS + 50));
        assert_eq!(b.chars().count(), MAX_BODY_CHARS);
        assert!(cut);
    }

    #[test]
    fn reply_goes_to_from_and_never_to_own_addresses() {
        let own = vec!["me@example.org".to_string()];
        let p = plan_reply("me@example.org", &src(), &own, false, "Paid, thanks.", &all_known).unwrap();
        assert_eq!(p.to, vec!["bob@shop.test"]);
        assert!(p.cc.is_empty());
        assert_eq!(p.subject, "Re: Invoice");
        assert_eq!(p.in_reply_to.as_deref(), Some("abc@shop.test"));
        assert_eq!(p.references, "r1@x");
        assert!(p.quote.starts_with("On 2026-10-26 07:33 UTC, Bob <bob@shop.test> wrote:\n> Please pay\n>> older\n> thanks"), "{}", p.quote);
        let all = plan_reply("me@example.org", &src(), &own, true, "Paid.", &all_known).unwrap();
        assert_eq!(all.to, vec!["bob@shop.test"]);
        assert_eq!(all.cc, vec!["carol@other.test", "dave@other.test"], "own address removed in any case, no duplicates");
    }

    #[test]
    fn reply_to_header_wins_and_a_different_address_is_flagged() {
        let mut s = src();
        s.reply_to = vec![a("", "attacker@evil.test")];
        let p = plan_reply("me@example.org", &s, &["me@example.org".into()], false, "ok", &all_known).unwrap();
        assert_eq!(p.to, vec!["attacker@evil.test"]);
        assert!(p.warnings.iter().any(|w| w.contains("instead of the sender") && w.contains("bob@shop.test")), "{:?}", p.warnings);
        // Reply-To equal to From is not suspicious.
        s.reply_to = vec![a("", "BOB@shop.test")];
        let p = plan_reply("me@example.org", &s, &["me@example.org".into()], false, "ok", &all_known).unwrap();
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn replying_to_my_own_message_goes_to_the_people_i_wrote_to() {
        let mut s = src();
        s.from = vec![a("Me", "me@example.org")];
        s.to = vec![a("", "bob@shop.test")];
        s.cc = vec![a("", "carol@other.test")];
        s.in_sent = true;
        let p = plan_reply("me@example.org", &s, &["me@example.org".into()], true, "ping", &all_known).unwrap();
        assert_eq!(p.to, vec!["bob@shop.test"]);
        assert_eq!(p.cc, vec!["carol@other.test"]);
        s.to = vec![];
        assert!(plan_reply("me@example.org", &s, &["me@example.org".into()], false, "x", &all_known).is_err(), "nobody to reply to");
    }

    #[test]
    fn a_forged_from_me_outside_sent_cannot_redirect_the_reply() {
        // An Inbox message "from" my own address, To = the attacker's chosen list, Reply-To = attacker.
        let mut s = src();
        s.from = vec![a("Me", "me@example.org")];
        s.to = vec![a("", "victim@elsewhere.test")];
        s.reply_to = vec![a("", "attacker@evil.test")];
        s.in_sent = false;
        let known = |x: &str| x == "victim@elsewhere.test";
        let p = plan_reply("me@example.org", &s, &["me@example.org".into()], false, "ok", &known).unwrap();
        assert_eq!(p.to, vec!["attacker@evil.test"], "not treated as my own sent mail: follows Reply-To like any incoming mail, but flagged");
        assert!(p.warnings.iter().any(|w| w.contains("not in your Sent folder")), "{:?}", p.warnings);
        assert!(p.warnings.iter().any(|w| w.contains("instead of the sender")), "{:?}", p.warnings);
        assert!(p.warnings.iter().any(|w| w.starts_with("New address: attacker@evil.test")), "{:?}", p.warnings);
        assert_eq!(p.new_addresses, vec!["attacker@evil.test"]);
        // without Reply-To nobody is left to answer (the "sender" is me): refused rather than guessing from To
        s.reply_to.clear();
        assert!(plan_reply("me@example.org", &s, &["me@example.org".into()], false, "ok", &known).is_err());
        // the same message in the Sent folder goes to its To list, which is the user's own choice
        s.in_sent = true;
        let p = plan_reply("me@example.org", &s, &["me@example.org".into()], false, "ok", &known).unwrap();
        assert_eq!(p.to, vec!["victim@elsewhere.test"]);
        assert!(p.new_addresses.is_empty());
        // ordinary replies flag recipients the mailbox has never seen
        let p = plan_reply("me@example.org", &src(), &["me@example.org".into()], false, "ok", &|_: &str| false).unwrap();
        assert_eq!(p.new_addresses, vec!["bob@shop.test"]);
    }

    #[test]
    fn domain_labels_are_ascii_or_punycode() {
        assert!(valid_address("a@xn--bcher-kva.example"));
        assert!(!valid_address("a@b\u{fc}cher.example"), "raw unicode labels are refused");
        assert!(!valid_address("a@ex_ample.com"));
        assert!(!valid_address(&format!("a@{}.com", "x".repeat(64))));
    }

    #[test]
    fn existing_re_prefix_is_kept_and_links_are_flagged() {
        let mut s = src();
        s.subject = "RE: Invoice".into();
        let p = plan_reply("me@example.org", &s, &[], false, "see https://evil.test/x and http://a.test", &all_known).unwrap();
        assert_eq!(p.subject, "RE: Invoice");
        assert!(p.warnings.iter().any(|w| w.contains("2 links")), "{:?}", p.warnings);
    }

    #[test]
    fn new_draft_flags_addresses_the_mailbox_has_never_seen() {
        let known = |a: &str| a == "bob@shop.test";
        let p = plan_new("me@example.org", &["bob@shop.test".into(), "new@person.test".into()], &["cc@new.test".into(), "bob@shop.test".into()], "Hello  there", "Hi", &known).unwrap();
        assert_eq!(p.new_addresses, vec!["new@person.test", "cc@new.test"]);
        assert_eq!(p.cc, vec!["cc@new.test"], "a cc already in to is dropped");
        assert_eq!(p.warnings.iter().filter(|w| w.starts_with("New address:")).count(), 2);
        assert_eq!(p.subject, "Hello there");
        assert!(p.quote.is_empty() && p.in_reply_to.is_none());
        assert!(p.title.starts_with("New message to bob@shop.test +1"), "{}", p.title);
    }

    #[test]
    fn new_draft_rejects_bad_input() {
        let k = |_: &str| true;
        assert!(plan_new("a@b.co", &[], &[], "s", "b", &k).is_err(), "no recipient");
        assert!(plan_new("a@b.co", &["Bob <b@c.de>".into()], &[], "s", "b", &k).is_err(), "display names are refused");
        assert!(plan_new("a@b.co", &["b@c.de".into()], &[], "  ", "b", &k).is_err(), "subject");
        assert!(plan_new("a@b.co", &["b@c.de".into()], &[], "s", "\u{200b}\u{202e}", &k).is_err(), "body empty after cleaning");
        let many: Vec<String> = (0..11).map(|i| format!("u{i}@x.test")).collect();
        assert!(plan_new("a@b.co", &many, &[], "s", "b", &k).is_err());
    }

    #[test]
    fn compose_text_appends_the_quote_only_for_replies() {
        assert_eq!(compose_text("Hi", ""), "Hi");
        assert_eq!(compose_text("Hi", "On x, y wrote:\n> z"), "Hi\n\nOn x, y wrote:\n> z\n");
    }
}
