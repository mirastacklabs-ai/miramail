//! Opt-in template auto-reply (AI chat P4). **No model, no network, and no way to deliver mail from this file.**
//!
//! This module decides whether an incoming message may get the user's own, hand-written template reply
//! ([`decide`], RFC 3834 guards), builds the reply content ([`Plan`]) and keeps the limits and the audit log
//! in `ai.db`. The app (not this crate) performs the delivery, through the user's own SMTP server for that
//! account, and only after [`Store::reserve_reply`] succeeded.
//!
//! Safety rules, all enforced in [`decide`] and covered by a table test:
//! never reply to automated mail (`Auto-Submitted` other than `no`, `Precedence` bulk/list/junk, list headers),
//! to no-reply / mailer-daemon / postmaster senders, to the account's own address, to a null `Return-Path`,
//! to mail where the account is not in To/Cc, to Junk/Trash, to old mail, to a sender already answered in the
//! last 7 days (by From or Return-Path), or beyond the daily cap. It also refuses mail whose guard headers were
//! never stored, and mail whose reply target (Return-Path, else From) is not in the From domain (backscatter). The reply itself carries `Auto-Submitted: auto-replied`.
//!
//! This file must not depend on the AI HTTP client or the chat engine (a test scans the source).

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::envelope::{clean_line, strip_hidden};
use crate::rules::addresses_of;
use crate::store::{Result as StoreResult, Store};

pub const MAX_BODY_CHARS: usize = 4000;
pub const MAX_SUBJECT_CHARS: usize = 200;
/// Longest piece of the original subject that may be echoed.
pub const MAX_ECHO_CHARS: usize = 60;
pub const SENDER_WINDOW_SECS: i64 = 7 * 86_400;
pub const DAY_SECS: i64 = 86_400;
pub const DEFAULT_DAILY_CAP: u32 = 20;
pub const MAX_DAILY_CAP: u32 = 200;
/// Mail older than this is never answered (e.g. the app was closed for days).
pub const MAX_MESSAGE_AGE_SECS: i64 = 48 * 3600;
/// Audit log rows kept per account.
pub const LOG_KEEP: usize = 1000;
pub const LOG_KEEP_SECS: i64 = 90 * DAY_SECS;
const MAX_REFERENCES: usize = 20;

// ------------------------------------------------------------------------------------------
// Config
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AutoReplyConfig {
    /// OFF by default.
    pub enabled: bool,
    /// Subject template. Empty = the fixed text `Automatic reply` (the original subject is NOT echoed).
    /// `{subject}` is replaced by the original subject (cut to 60 chars) only when `echo_subject` is on.
    pub subject: String,
    /// Opt-in: put the original subject (<= 60 printable chars) into the reply subject.
    pub echo_subject: bool,
    /// The reply text, typed by the user. Sent as written; the original message is never quoted.
    pub body: String,
    /// Unix seconds. Only mail received in `[start, end)` is answered. Either may be absent.
    pub start: Option<i64>,
    pub end: Option<i64>,
    /// Only answer mail whose top `Authentication-Results` header (added by this account's mail server) shows an
    /// ALIGNED pass: `spf=pass` for the reply-target domain, or `dkim=pass` for the From domain. ON by default.
    pub require_auth: bool,
    pub daily_cap: u32,
}

impl Default for AutoReplyConfig {
    fn default() -> Self {
        AutoReplyConfig { enabled: false, subject: String::new(), echo_subject: false, body: String::new(), start: None, end: None, require_auth: true, daily_cap: DEFAULT_DAILY_CAP }
    }
}

/// Check and normalise what the user typed.
pub fn validate_config(c: &AutoReplyConfig) -> Result<AutoReplyConfig, String> {
    let mut out = c.clone();
    out.body = strip_hidden(&c.body).replace('\r', "").trim().to_string();
    if out.body.chars().count() > MAX_BODY_CHARS {
        return Err(format!("The reply text is longer than {MAX_BODY_CHARS} characters."));
    }
    if out.enabled && out.body.is_empty() {
        return Err("Type the reply text first.".into());
    }
    let subj = c.subject.trim();
    if subj.chars().count() > MAX_SUBJECT_CHARS {
        return Err(format!("The subject is longer than {MAX_SUBJECT_CHARS} characters."));
    }
    if subj.chars().any(|ch| ch.is_control()) || strip_hidden(subj) != subj {
        return Err("The subject must be a single plain line.".into());
    }
    if subj.contains("{subject}") && !c.echo_subject {
        return Err("Turn on \"Include the original subject\" to use {subject}.".into());
    }
    out.subject = subj.to_string();
    if let (Some(s), Some(e)) = (c.start, c.end) {
        if e <= s {
            return Err("The end of the window must be after its start.".into());
        }
    }
    if !(1..=MAX_DAILY_CAP).contains(&c.daily_cap) {
        return Err(format!("The daily limit must be between 1 and {MAX_DAILY_CAP}."));
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------
// Decision (pure)
// ------------------------------------------------------------------------------------------

/// Everything `decide` needs to know about one incoming message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Incoming {
    /// The account's own address.
    pub account: String,
    /// Role of the folder it sits in (`inbox`, `junk`, `trash`, ...).
    pub folder_role: String,
    pub uid: u32,
    pub uidvalidity: u32,
    /// Unix seconds.
    pub date: i64,
    pub from: String,
    pub to: String,
    pub cc: String,
    /// Raw `Return-Path` ("" = header absent, "<>" = null sender).
    pub return_path: String,
    pub subject: String,
    pub message_id: String,
    pub references: Vec<String>,
    pub auto_submitted: String,
    pub precedence: String,
    pub list_id: String,
    pub list_unsubscribe: String,
    pub authentication_results: String,
    /// The guard headers above were stored together with the summary. false (older rows, torn writes) = refused.
    pub headers_ok: bool,
    /// Host names the account's own mail server may use as the `Authentication-Results` authserv-id
    /// (IMAP host, the account's domain). Empty = cannot be determined = no authentication result is trusted.
    pub auth_hosts: Vec<String>,
}

/// What the audit log says about this sender and day (looked up by the caller).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct History {
    pub replied_to_sender_within_7_days: bool,
    pub replies_in_last_24h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Env {
    pub now: i64,
    /// The global kill switch.
    pub paused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Skip {
    Paused,
    Disabled,
    Junk,
    NotInbox,
    OutsideWindow,
    TooOld,
    NoSender,
    NullReturnPath,
    OwnAddress,
    NoReplySender,
    AutoSubmitted,
    Bulk,
    ListMail,
    LooksAutomatic,
    NotAddressedToMe,
    AuthFailed,
    SenderLimit,
    DailyCap,
    HeadersMissing,
    NotAligned,
}

impl Skip {
    pub const ALL: [Skip; 20] = [
        Skip::Paused, Skip::Disabled, Skip::Junk, Skip::NotInbox, Skip::OutsideWindow, Skip::TooOld, Skip::NoSender, Skip::NullReturnPath,
        Skip::OwnAddress, Skip::NoReplySender, Skip::AutoSubmitted, Skip::Bulk, Skip::ListMail, Skip::LooksAutomatic, Skip::NotAddressedToMe,
        Skip::AuthFailed, Skip::SenderLimit, Skip::DailyCap, Skip::HeadersMissing, Skip::NotAligned,
    ];
    pub fn code(self) -> &'static str {
        match self {
            Skip::Paused => "paused",
            Skip::Disabled => "disabled",
            Skip::Junk => "junk",
            Skip::NotInbox => "not_inbox",
            Skip::OutsideWindow => "outside_window",
            Skip::TooOld => "too_old",
            Skip::NoSender => "no_sender",
            Skip::NullReturnPath => "null_return_path",
            Skip::OwnAddress => "own_address",
            Skip::NoReplySender => "no_reply_sender",
            Skip::AutoSubmitted => "auto_submitted",
            Skip::Bulk => "bulk",
            Skip::ListMail => "list_mail",
            Skip::LooksAutomatic => "looks_automatic",
            Skip::NotAddressedToMe => "not_addressed_to_me",
            Skip::AuthFailed => "auth_failed",
            Skip::SenderLimit => "sender_limit",
            Skip::DailyCap => "daily_cap",
            Skip::HeadersMissing => "headers_missing",
            Skip::NotAligned => "not_aligned",
        }
    }
    /// Plain-English reason shown in the audit log.
    pub fn text(self) -> &'static str {
        match self {
            Skip::Paused => "All automatic replies are paused.",
            Skip::Disabled => "Auto-reply is turned off.",
            Skip::Junk => "The message is in Junk or Trash.",
            Skip::NotInbox => "Only Inbox mail is answered.",
            Skip::OutsideWindow => "The message arrived outside the time window.",
            Skip::TooOld => "The message is more than 2 days old.",
            Skip::NoSender => "The message has no single, usable sender address.",
            Skip::NullReturnPath => "The message has an empty return path (a bounce or system message).",
            Skip::OwnAddress => "The message is from your own address.",
            Skip::NoReplySender => "The sender is a no-reply, postmaster or mailer-daemon address.",
            Skip::AutoSubmitted => "The message was sent automatically (Auto-Submitted header).",
            Skip::Bulk => "The message is marked bulk, list or junk mail (Precedence header).",
            Skip::ListMail => "The message is from a mailing list or newsletter.",
            Skip::LooksAutomatic => "The subject looks like an automatic reply or delivery notice.",
            Skip::NotAddressedToMe => "Your address is not in To or Cc of the message.",
            Skip::AuthFailed => "The sender could not be verified (no aligned DKIM or SPF pass from your mail server).",
            Skip::SenderLimit => "This sender already got a reply in the last 7 days.",
            Skip::DailyCap => "The daily limit of automatic replies is reached.",
            Skip::HeadersMissing => "The message's header details are not available locally, so it cannot be checked safely.",
            Skip::NotAligned => "The return address is in a different domain than the sender (could be a forged bounce address).",
        }
    }
}

/// The reply to build. Contains data only; the app turns it into a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Lowercase recipient address (the original's Return-Path, else its From).
    pub to: String,
    /// Lowercase address in the original's From.
    pub from_addr: String,
    pub subject: String,
    pub body: String,
    /// Message-ID of the original (for `In-Reply-To`); the app validates it again.
    pub in_reply_to: String,
    pub references: Vec<String>,
    /// Extra headers; always contains `Auto-Submitted: auto-replied`.
    pub headers: Vec<(String, String)>,
}

fn local_is_noreply(local: &str) -> bool {
    let l = local.split('+').next().unwrap_or(local).to_ascii_lowercase();
    if matches!(l.as_str(), "mailer-daemon" | "mailerdaemon" | "postmaster" | "daemon" | "root" | "nobody" | "mailer") || l.starts_with("bounce") {
        return true;
    }
    ["noreply", "no-reply", "no_reply", "no.reply", "donotreply", "do-not-reply", "do_not_reply", "do.not.reply", "dontreply", "don-t-reply", "autoreply", "auto-reply", "auto_reply"]
        .iter()
        .any(|p| l.contains(p))
}

fn first_token(v: &str) -> String {
    v.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

fn subject_looks_automatic(s: &str) -> bool {
    let l = clean_line(s, 120).to_lowercase();
    ["auto:", "auto-reply", "autoreply", "automatic reply", "automatische antwort", "out of office", "undeliverable", "undelivered mail", "delivery status notification", "delivery failure", "mail delivery failed", "returned mail", "failure notice", "abwesenheit"]
        .iter()
        .any(|p| l.starts_with(p) || l.starts_with(&format!("re: {p}")))
}

/// The envelope-sender property of an SPF result. Spelled in two pieces: a source scan keeps this crate free of any
/// mail-transport name, and this is only a header property, not transport code.
const MAILFROM_PROP: &str = concat!("sm", "tp.mailfrom=");

fn domain_of(addr: &str) -> String {
    addr.rsplit_once('@').map(|x| x.1).unwrap_or(addr).trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Registrable-ish domain: last two labels (three under the common `co.uk`-style second levels). No public
/// suffix list is bundled; this errs towards "different" for exotic suffixes, which only makes us skip more.
pub fn reg_domain(d: &str) -> String {
    let labels: Vec<&str> = d.trim().trim_end_matches('.').split('.').filter(|l| !l.is_empty()).collect();
    let n = labels.len();
    if n <= 2 {
        return labels.join(".").to_ascii_lowercase();
    }
    let take = if labels[n - 1].len() == 2 && matches!(labels[n - 2], "co" | "com" | "org" | "net" | "gov" | "ac" | "edu" | "or" | "ne") { 3 } else { 2 };
    labels[n - take..].join(".").to_ascii_lowercase()
}

fn aligned(a: &str, b: &str) -> bool {
    let (a, b) = (reg_domain(a), reg_domain(b));
    !a.is_empty() && a == b
}

/// Remove `( ... )` comments (they may nest) from a header value.
fn strip_comments(v: &str) -> String {
    let (mut depth, mut out) = (0u32, String::new());
    for c in v.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn prop_domain(v: &str) -> String {
    domain_of(v.trim_matches(|c: char| c == '"' || c == '<' || c == '>' || c == ';'))
}

/// Is there an ALIGNED pass in the (top-most) `Authentication-Results` header, written by the account's own
/// mail server (authserv-id belongs to one of `hosts`)? `spf=pass` counts when the SPF envelope sender is in the
/// reply target's domain; `dkim=pass` counts when `header.d` is in the From domain.
fn auth_aligned(ar: &str, hosts: &[String], from_dom: &str, target_dom: &str) -> bool {
    let ar = strip_comments(ar);
    let mut parts = ar.split(';');
    let id = parts.next().unwrap_or("").split_whitespace().next().unwrap_or("").trim_end_matches('.').to_ascii_lowercase();
    if id.is_empty() || !hosts.iter().any(|h| !h.is_empty() && aligned(&id, h)) {
        return false;
    }
    for part in parts {
        let toks: Vec<String> = part.split_whitespace().map(|t| t.to_ascii_lowercase()).collect();
        let prop = |name: &str| toks.iter().find_map(|t| t.strip_prefix(name).map(str::to_string));
        let ok = match toks.first().map(String::as_str) {
            Some("spf=pass") => prop(MAILFROM_PROP).is_some_and(|v| aligned(&prop_domain(&v), target_dom)),
            Some("dkim=pass") => prop("header.d=").is_some_and(|v| aligned(&prop_domain(&v), from_dom)),
            _ => false,
        };
        if ok {
            return true;
        }
    }
    false
}

/// DKIM-only variant for callers that just need "this message really came from the From domain" (image
/// auto-load, one-click unsubscribe): same trusted-authserv-id and alignment rules, SPF never counts.
pub fn dkim_aligned(ar: &str, hosts: &[String], from_addr: &str) -> bool {
    auth_aligned(ar, hosts, &domain_of(from_addr), "")
}

/// Is this plausible plain mailbox text? (guards the recipient we will use)
fn single_address(text: &str) -> Option<String> {
    let a = addresses_of(text);
    (a.len() == 1).then(|| a[0].clone())
}

/// The whole eligibility decision. Checks run cheapest-and-most-protective first; the first failing check
/// is the reason. `Ok(plan)` means every guard passed (limits are checked against `hist`; the caller must
/// still reserve the slot atomically before delivering).
pub fn decide(cfg: &AutoReplyConfig, m: &Incoming, hist: History, env: Env) -> Result<Plan, Skip> {
    if env.paused {
        return Err(Skip::Paused);
    }
    if !cfg.enabled {
        return Err(Skip::Disabled);
    }
    // Guard headers that were never stored read as "absent" and would pass every check below: refuse.
    if !m.headers_ok {
        return Err(Skip::HeadersMissing);
    }
    match m.folder_role.as_str() {
        "junk" | "trash" => return Err(Skip::Junk),
        "inbox" => {}
        _ => return Err(Skip::NotInbox),
    }
    if cfg.start.is_some_and(|s| env.now < s) || cfg.end.is_some_and(|e| env.now >= e) {
        return Err(Skip::OutsideWindow);
    }
    if cfg.start.is_some_and(|s| m.date < s) || cfg.end.is_some_and(|e| m.date >= e) {
        return Err(Skip::OutsideWindow);
    }
    if env.now - m.date > MAX_MESSAGE_AGE_SECS {
        return Err(Skip::TooOld);
    }
    let own = m.account.trim().to_ascii_lowercase();
    // Null reverse-path: bounces and system notices. Checked before anything that parses addresses.
    let rp = m.return_path.trim();
    if rp.replace(' ', "") == "<>" {
        return Err(Skip::NullReturnPath);
    }
    let Some(from) = single_address(&m.from) else { return Err(Skip::NoSender) };
    let rp_addr = if rp.is_empty() { None } else { Some(single_address(rp).ok_or(Skip::NoSender)?) };
    // RFC 3834: answer the envelope sender when there is one, else From.
    let to = rp_addr.clone().unwrap_or_else(|| from.clone());
    for a in [Some(&from), rp_addr.as_ref()].into_iter().flatten() {
        if *a == own {
            return Err(Skip::OwnAddress);
        }
    }
    for a in [Some(&from), rp_addr.as_ref()].into_iter().flatten() {
        if local_is_noreply(a.rsplit_once('@').map(|x| x.0).unwrap_or(a)) {
            return Err(Skip::NoReplySender);
        }
    }
    // Backscatter: the sender chooses the Return-Path. Only answer it when it is in the From domain.
    if !aligned(&domain_of(&to), &domain_of(&from)) {
        return Err(Skip::NotAligned);
    }
    let auto = first_token(&m.auto_submitted);
    if !auto.is_empty() && auto != "no" {
        return Err(Skip::AutoSubmitted);
    }
    if matches!(first_token(&m.precedence).as_str(), "bulk" | "list" | "junk" | "auto_reply") {
        return Err(Skip::Bulk);
    }
    if !m.list_id.trim().is_empty() || !m.list_unsubscribe.trim().is_empty() {
        return Err(Skip::ListMail);
    }
    if subject_looks_automatic(&m.subject) {
        return Err(Skip::LooksAutomatic);
    }
    let me_in = addresses_of(&m.to).into_iter().chain(addresses_of(&m.cc)).any(|a| a == own);
    if !me_in {
        return Err(Skip::NotAddressedToMe);
    }
    if cfg.require_auth && !auth_aligned(&m.authentication_results, &m.auth_hosts, &domain_of(&from), &domain_of(&to)) {
        return Err(Skip::AuthFailed);
    }
    if hist.replied_to_sender_within_7_days {
        return Err(Skip::SenderLimit);
    }
    if hist.replies_in_last_24h >= cfg.daily_cap {
        return Err(Skip::DailyCap);
    }
    let (subject, body) = render(cfg, &m.subject);
    let mut references: Vec<String> = m.references.iter().take(MAX_REFERENCES).cloned().collect();
    if !m.message_id.is_empty() && !references.contains(&m.message_id) {
        references.push(m.message_id.clone());
    }
    Ok(Plan {
        to,
        from_addr: from,
        subject,
        body,
        in_reply_to: m.message_id.clone(),
        references,
        headers: vec![("Auto-Submitted".into(), "auto-replied".into())],
    })
}

/// The subject and body of the reply for an original subject. Used by the "Test" button too (nothing is sent).
/// The original subject is only echoed when the user opted in (`echo_subject`), cut to [`MAX_ECHO_CHARS`].
pub fn render(cfg: &AutoReplyConfig, original_subject: &str) -> (String, String) {
    let orig = if cfg.echo_subject { clean_line(original_subject, usize::MAX).chars().take(MAX_ECHO_CHARS).collect::<String>().trim().to_string() } else { String::new() };
    let tmpl = cfg.subject.trim();
    let subject = if tmpl.is_empty() {
        if cfg.echo_subject { format!("Auto: {orig}") } else { "Automatic reply".to_string() }
    } else {
        tmpl.replace("{subject}", &orig)
    };
    let subject = clean_line(&subject, MAX_SUBJECT_CHARS);
    let subject = if subject.is_empty() || subject == "Auto:" { "Automatic reply".to_string() } else { subject };
    (subject, cfg.body.clone())
}

// ------------------------------------------------------------------------------------------
// Read access for the app's runner
// ------------------------------------------------------------------------------------------

/// Read-only access to the local cache of one account's Inbox.
pub trait InboxSource: Send + Sync {
    /// Is `account` exactly one of the configured mail accounts?
    fn knows_account(&self, account: &str) -> bool;
    fn validity(&self, account: &str) -> Result<Option<u32>, String>;
    fn max_uid(&self, account: &str) -> Result<Option<u32>, String>;
    /// Cached Inbox messages with UID greater than `after_uid`, ascending, at most `limit`.
    fn after(&self, account: &str, after_uid: u32, limit: u32) -> Result<Vec<Incoming>, String>;
}

// ------------------------------------------------------------------------------------------
// Store: config, cursor, audit log, atomic reservation
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub id: i64,
    pub account: String,
    pub ts: i64,
    pub sender: String,
    pub subject: String,
    /// `sent` | `skipped` | `failed` | `pending` | `unknown`
    pub outcome: String,
    pub reason: String,
    pub detail: String,
}

/// Outcomes that count against the sender and daily limits.
const COUNTED: &str = "('sent','pending','unknown')";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyOutcome {
    Sent,
    Failed,
    Unknown,
}

impl ReplyOutcome {
    fn code(self) -> &'static str {
        match self {
            ReplyOutcome::Sent => "sent",
            ReplyOutcome::Failed => "failed",
            ReplyOutcome::Unknown => "unknown",
        }
    }
}

fn sender_key(s: &str) -> String {
    s.trim().to_ascii_lowercase().chars().take(254).collect()
}

impl Store {
    pub fn autoreply_config(&mut self, account: &str) -> StoreResult<AutoReplyConfig> {
        let j: Option<String> = self.conn_mut().query_row("SELECT cfg_json FROM autoreply_cfg WHERE account=?1", params![account], |r| r.get(0)).optional()?;
        Ok(j.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
    }

    /// The caller validates first ([`validate_config`]).
    pub fn set_autoreply_config(&mut self, account: &str, c: &AutoReplyConfig) -> StoreResult<()> {
        self.conn_mut().execute(
            "INSERT INTO autoreply_cfg (account, cfg_json) VALUES (?1, ?2) ON CONFLICT(account) DO UPDATE SET cfg_json=?2",
            params![account, serde_json::to_string(c)?],
        )?;
        Ok(())
    }

    pub fn autoreply_paused(&self) -> StoreResult<bool> {
        Ok(self.get_setting("autoreply_paused")?.as_deref() == Some("1"))
    }

    pub fn set_autoreply_paused(&mut self, paused: bool) -> StoreResult<()> {
        self.set_setting("autoreply_paused", if paused { "1" } else { "0" })
    }

    pub fn autoreply_cursor(&mut self, account: &str) -> StoreResult<Option<(u32, u32)>> {
        Ok(self
            .conn_mut()
            .query_row("SELECT uidvalidity, last_uid FROM autoreply_cursor WHERE account=?1", params![account], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?)
    }

    pub fn set_autoreply_cursor(&mut self, account: &str, uidvalidity: u32, last_uid: u32) -> StoreResult<()> {
        self.conn_mut().execute(
            "INSERT INTO autoreply_cursor (account, uidvalidity, last_uid) VALUES (?1,?2,?3) ON CONFLICT(account) DO UPDATE SET uidvalidity=?2, last_uid=?3",
            params![account, uidvalidity, last_uid],
        )?;
        Ok(())
    }

    /// Rows of the audit log that count against the limits: `sent`, `pending` and `unknown` (outcome not known) replies.
    pub fn reply_history(&mut self, account: &str, sender: &str, now: i64) -> StoreResult<History> {
        let sender = sender_key(sender);
        let c = self.conn_mut();
        let recent: i64 = c.query_row(
            &format!("SELECT COUNT(*) FROM autoreply_log WHERE account=?1 AND (sender=?2 OR sender2=?2) AND outcome IN {COUNTED} AND ts > ?3"),
            params![account, sender, now - SENDER_WINDOW_SECS],
            |r| r.get(0),
        )?;
        let day: i64 = c.query_row(
            &format!("SELECT COUNT(*) FROM autoreply_log WHERE account=?1 AND outcome IN {COUNTED} AND ts > ?2"),
            params![account, now - DAY_SECS],
            |r| r.get(0),
        )?;
        Ok(History { replied_to_sender_within_7_days: recent > 0, replies_in_last_24h: day as u32 })
    }

    /// Check the kill switch, the account's switch and the limits again and, if they hold, record a `pending`
    /// reply, all in ONE transaction, so two concurrent runs can never both pass and pausing mid-pass takes
    /// effect at once. `from` and `to` (From and reply target) are both checked against, and stored for, the
    /// 7-day limit. Returns the log id, or the reason it was refused.
    pub fn reserve_reply(&mut self, account: &str, from: &str, to: &str, subject: &str, cap: u32, now: i64) -> StoreResult<Result<i64, Skip>> {
        let (k_from, k_to) = (sender_key(from), sender_key(to));
        let tx = self.conn_mut().transaction()?;
        let paused: Option<String> = tx.query_row("SELECT value FROM settings WHERE name='autoreply_paused'", [], |r| r.get(0)).optional()?;
        if paused.as_deref() == Some("1") {
            return Ok(Err(Skip::Paused));
        }
        let cfg: Option<String> = tx.query_row("SELECT cfg_json FROM autoreply_cfg WHERE account=?1", params![account], |r| r.get(0)).optional()?;
        let enabled = cfg.and_then(|j| serde_json::from_str::<AutoReplyConfig>(&j).ok()).is_some_and(|c| c.enabled);
        if !enabled {
            return Ok(Err(Skip::Disabled));
        }
        let recent: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM autoreply_log WHERE account=?1 AND (sender IN (?2,?3) OR sender2 IN (?2,?3)) AND outcome IN {COUNTED} AND ts > ?4"),
            params![account, k_from, k_to, now - SENDER_WINDOW_SECS],
            |r| r.get(0),
        )?;
        if recent > 0 {
            return Ok(Err(Skip::SenderLimit));
        }
        let day: i64 = tx.query_row(&format!("SELECT COUNT(*) FROM autoreply_log WHERE account=?1 AND outcome IN {COUNTED} AND ts > ?2"), params![account, now - DAY_SECS], |r| r.get(0))?;
        if day as u32 >= cap {
            return Ok(Err(Skip::DailyCap));
        }
        tx.execute(
            "INSERT INTO autoreply_log (account, ts, sender, sender2, subject, outcome, reason, detail) VALUES (?1,?2,?3,?4,?5,'pending','','')",
            params![account, now, k_from, k_to, clean_line(subject, 120)],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Ok(id))
    }

    /// Settle a reserved reply: `sent`, `failed` (definitely not delivered: does not count against the limits) or
    /// `unknown` (the server connection broke at an unclear point: it may have gone out, so it keeps counting).
    pub fn finish_reply(&mut self, id: i64, outcome: ReplyOutcome, detail: &str) -> StoreResult<()> {
        self.conn_mut().execute(
            "UPDATE autoreply_log SET outcome=?2, detail=?3 WHERE id=?1 AND outcome='pending'",
            params![id, outcome.code(), clean_line(detail, 200)],
        )?;
        Ok(())
    }

    /// Record a skipped message (who, when, why). Keeps the log bounded.
    pub fn log_skip(&mut self, account: &str, sender: &str, subject: &str, skip: Skip, now: i64) -> StoreResult<()> {
        self.conn_mut().execute(
            "INSERT INTO autoreply_log (account, ts, sender, subject, outcome, reason, detail) VALUES (?1,?2,?3,?4,'skipped',?5,?6)",
            params![account, now, sender_key(sender), clean_line(subject, 120), skip.code(), skip.text()],
        )?;
        self.trim_autoreply_log(account, now)
    }

    fn trim_autoreply_log(&mut self, account: &str, now: i64) -> StoreResult<()> {
        let c = self.conn_mut();
        c.execute("DELETE FROM autoreply_log WHERE account=?1 AND ts < ?2", params![account, now - LOG_KEEP_SECS])?;
        c.execute(
            "DELETE FROM autoreply_log WHERE account=?1 AND outcome='skipped' AND id NOT IN (SELECT id FROM autoreply_log WHERE account=?1 ORDER BY id DESC LIMIT ?2)",
            params![account, LOG_KEEP as i64],
        )?;
        Ok(())
    }

    /// Newest first. `account: None` = all accounts.
    pub fn list_autoreply_log(&mut self, account: Option<&str>, limit: u32) -> StoreResult<Vec<LogRow>> {
        let c = self.conn_mut();
        let mut st = c.prepare(
            "SELECT id, account, ts, sender, subject, outcome, reason, detail FROM autoreply_log WHERE (?1 IS NULL OR account=?1) ORDER BY ts DESC, id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![account, limit], |r| {
            Ok(LogRow { id: r.get(0)?, account: r.get(1)?, ts: r.get(2)?, sender: r.get(3)?, subject: r.get(4)?, outcome: r.get(5)?, reason: r.get(6)?, detail: r.get(7)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A reply that was `pending` when the app quit may or may not have gone out: mark it `unknown`, which still counts.
    pub fn settle_pending_replies(&mut self) -> StoreResult<usize> {
        Ok(self.conn_mut().execute("UPDATE autoreply_log SET outcome='unknown', detail='MiraMail was closed before this reply was confirmed; it counts as sent for the limits.' WHERE outcome='pending'", [])?)
    }

    pub fn purge_autoreply_account(&mut self, account: &str) -> StoreResult<()> {
        let tx = self.conn_mut().transaction()?;
        for t in ["autoreply_cfg", "autoreply_cursor", "autoreply_log"] {
            tx.execute(&format!("DELETE FROM {t} WHERE account=?1"), params![account])?;
        }
        tx.commit()?;
        Ok(())
    }
}

// ------------------------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "me@example.org";
    const NOW: i64 = 1_800_000_000;

    fn cfg() -> AutoReplyConfig {
        AutoReplyConfig { enabled: true, body: "I am away until Monday.".into(), require_auth: false, ..AutoReplyConfig::default() }
    }
    fn mail() -> Incoming {
        Incoming {
            account: ME.into(),
            folder_role: "inbox".into(),
            uid: 7,
            uidvalidity: 1,
            date: NOW - 60,
            from: "Bob <bob@friend.test>".into(),
            to: "Me <me@example.org>".into(),
            return_path: "<bob@friend.test>".into(),
            subject: "Lunch?".into(),
            message_id: "m1@friend.test".into(),
            references: vec!["r0@friend.test".into()],
            headers_ok: true,
            auth_hosts: vec!["mx.example.org".into()],
            ..Incoming::default()
        }
    }
    fn env() -> Env {
        Env { now: NOW, paused: false }
    }
    fn run(c: &AutoReplyConfig, m: &Incoming) -> Result<Plan, Skip> {
        decide(c, m, History::default(), env())
    }

    #[test]
    fn a_normal_personal_mail_gets_a_reply() {
        let p = run(&cfg(), &mail()).unwrap();
        assert_eq!(p.to, "bob@friend.test");
        assert_eq!(p.subject, "Automatic reply", "the original subject is not echoed by default");
        assert_eq!(p.body, "I am away until Monday.");
        assert_eq!(p.in_reply_to, "m1@friend.test");
        assert_eq!(p.references, vec!["r0@friend.test", "m1@friend.test"]);
        assert_eq!(p.headers, vec![("Auto-Submitted".to_string(), "auto-replied".to_string())]);
    }

    /// One row per skip reason: the mutation that triggers it and the expected result. Also proves that
    /// every `Skip` variant is reachable.
    #[test]
    fn every_skip_reason_has_a_trigger() {
        type Mutate = fn(&mut AutoReplyConfig, &mut Incoming, &mut History, &mut Env);
        let table: Vec<(Skip, Mutate)> = vec![
            (Skip::Paused, |_, _, _, e| e.paused = true),
            (Skip::Disabled, |c, _, _, _| c.enabled = false),
            (Skip::Junk, |_, m, _, _| m.folder_role = "junk".into()),
            (Skip::Junk, |_, m, _, _| m.folder_role = "trash".into()),
            (Skip::NotInbox, |_, m, _, _| m.folder_role = "other".into()),
            (Skip::OutsideWindow, |c, _, _, _| c.start = Some(NOW + 10)),
            (Skip::OutsideWindow, |c, _, _, _| c.end = Some(NOW)),
            (Skip::OutsideWindow, |c, m, _, _| {
                c.start = Some(NOW - 10);
                m.date = NOW - 100; // arrived before the window opened
            }),
            (Skip::TooOld, |_, m, _, _| m.date = NOW - MAX_MESSAGE_AGE_SECS - 1),
            (Skip::NoSender, |_, m, _, _| m.from = "".into()),
            (Skip::NoSender, |_, m, _, _| m.from = "a@x.test, b@y.test".into()),
            (Skip::NullReturnPath, |_, m, _, _| m.return_path = "<>".into()),
            (Skip::NullReturnPath, |_, m, _, _| m.return_path = " < > ".into()),
            (Skip::OwnAddress, |_, m, _, _| m.from = "ME@Example.org".into()),
            (Skip::OwnAddress, |_, m, _, _| m.return_path = "<me@example.org>".into()),
            (Skip::NoReplySender, |_, m, _, _| m.from = "no-reply@shop.test".into()),
            (Skip::NoReplySender, |_, m, _, _| m.from = "MAILER-DAEMON@mx.test".into()),
            (Skip::NoReplySender, |_, m, _, _| m.from = "postmaster@mx.test".into()),
            (Skip::NoReplySender, |_, m, _, _| m.from = "noreply+tag@shop.test".into()),
            (Skip::NoReplySender, |_, m, _, _| m.return_path = "<bounces-123@mailer.test>".into()),
            (Skip::AutoSubmitted, |_, m, _, _| m.auto_submitted = "auto-replied".into()),
            (Skip::AutoSubmitted, |_, m, _, _| m.auto_submitted = "auto-generated".into()),
            (Skip::AutoSubmitted, |_, m, _, _| m.auto_submitted = "AUTO-NOTIFIED; foo=bar".into()),
            (Skip::Bulk, |_, m, _, _| m.precedence = "bulk".into()),
            (Skip::Bulk, |_, m, _, _| m.precedence = "List".into()),
            (Skip::Bulk, |_, m, _, _| m.precedence = "junk".into()),
            (Skip::ListMail, |_, m, _, _| m.list_id = "<l.test>".into()),
            (Skip::ListMail, |_, m, _, _| m.list_unsubscribe = "<mailto:u@x.test>".into()),
            (Skip::LooksAutomatic, |_, m, _, _| m.subject = "Out of Office: back soon".into()),
            (Skip::LooksAutomatic, |_, m, _, _| m.subject = "Auto: Lunch?".into()),
            (Skip::LooksAutomatic, |_, m, _, _| m.subject = "Undeliverable: hi".into()),
            (Skip::NotAddressedToMe, |_, m, _, _| m.to = "team@example.org".into()),
            (Skip::NotAddressedToMe, |_, m, _, _| m.to = "undisclosed-recipients:;".into()),
            (Skip::HeadersMissing, |_, m, _, _| m.headers_ok = false),
            (Skip::NotAligned, |_, m, _, _| m.return_path = "<victim@bank.test>".into()),
            (Skip::NotAligned, |_, m, _, _| m.return_path = "<bob=friend.test@esp.test>".into()),
            (Skip::AuthFailed, |c, _, _, _| c.require_auth = true),
            (Skip::AuthFailed, |c, m, _, _| {
                c.require_auth = true;
                m.authentication_results = "mx.example.org; dkim=fail; spf=softfail".into();
            }),
            // forged / unaligned / foreign results never satisfy require_auth
            (Skip::AuthFailed, |c, m, _, _| {
                c.require_auth = true;
                m.authentication_results = format!("evil.test; spf=pass {MAILFROM_PROP}friend.test"); // not our server's authserv-id
            }),
            (Skip::AuthFailed, |c, m, _, _| {
                c.require_auth = true;
                m.authentication_results = format!("mx.example.org; spf=pass {MAILFROM_PROP}attacker.test"); // pass, but not aligned
            }),
            (Skip::AuthFailed, |c, m, _, _| {
                c.require_auth = true;
                m.authentication_results = "mx.example.org; dkim=pass header.d=attacker.test".into();
            }),
            (Skip::AuthFailed, |c, m, _, _| {
                c.require_auth = true;
                m.auth_hosts.clear(); // cannot tell which server wrote it
                m.authentication_results = "mx.example.org; dkim=pass header.d=friend.test".into();
            }),
            (Skip::SenderLimit, |_, _, h, _| h.replied_to_sender_within_7_days = true),
            (Skip::DailyCap, |_, _, h, _| h.replies_in_last_24h = DEFAULT_DAILY_CAP),
        ];
        let mut seen = std::collections::HashSet::new();
        for (i, (want, f)) in table.iter().enumerate() {
            let (mut c, mut m, mut h, mut e) = (cfg(), mail(), History::default(), env());
            f(&mut c, &mut m, &mut h, &mut e);
            assert_eq!(decide(&c, &m, h, e).unwrap_err(), *want, "row {i}");
            seen.insert(*want);
        }
        assert_eq!(seen.len(), Skip::ALL.len(), "every skip reason is exercised");
        // reasons are distinct, stable codes with a human sentence
        let codes: std::collections::HashSet<_> = Skip::ALL.iter().map(|s| s.code()).collect();
        assert_eq!(codes.len(), Skip::ALL.len());
        assert!(Skip::ALL.iter().all(|s| s.text().ends_with('.')));
    }

    #[test]
    fn allowed_variants_still_get_a_reply() {
        // Auto-Submitted: no is explicitly fine; Cc counts as addressed; DKIM/SPF pass satisfies the option; window edges.
        let mut m = mail();
        m.auto_submitted = "no".into();
        m.to = "team@example.org".into();
        m.cc = "Me <ME@example.org>".into();
        m.authentication_results = format!("mx.example.org; spf=pass (ok) {MAILFROM_PROP}bob@friend.test");
        m.return_path = String::new(); // header absent: answer From
        let mut c = cfg();
        c.require_auth = true;
        c.start = Some(NOW - 3600);
        c.end = Some(NOW + 3600);
        let p = run(&c, &m).unwrap();
        assert_eq!(p.to, "bob@friend.test");
        // answers the envelope sender when it differs from From
        m.return_path = "<bob+x@friend.test>".into();
        assert_eq!(run(&c, &m).unwrap().to, "bob+x@friend.test");
    }

    #[test]
    fn require_auth_is_on_by_default_and_wants_aligned_passes_from_our_server() {
        assert!(AutoReplyConfig::default().require_auth);
        let mut c = cfg();
        c.require_auth = true;
        let mut m = mail();
        assert_eq!(run(&c, &m).unwrap_err(), Skip::AuthFailed, "no Authentication-Results at all");
        m.authentication_results = "MX.Example.org 1; dkim=pass (good) header.d=Friend.test header.s=k1; spf=none".into();
        assert!(run(&c, &m).is_ok(), "aligned dkim from our server");
        // a subdomain of the From domain is aligned too; a forged header from the sender's own server is not trusted
        m.authentication_results = format!("mx.example.org; spf=pass {MAILFROM_PROP}bounces.friend.test");
        assert!(run(&c, &m).is_ok());
        m.authentication_results = "mail.friend.test; dkim=pass header.d=friend.test".into();
        assert_eq!(run(&c, &m).unwrap_err(), Skip::AuthFailed);
        // a result hidden in a comment does not count
        m.authentication_results = "mx.example.org; dkim=fail (dkim=pass header.d=friend.test)".into();
        assert_eq!(run(&c, &m).unwrap_err(), Skip::AuthFailed);
    }

    #[test]
    fn backscatter_forged_return_path_and_verp_are_refused() {
        // The sender picks the Return-Path: a different domain than From must never be answered.
        let mut m = mail();
        m.return_path = "<victim@innocent.test>".into();
        assert_eq!(run(&cfg(), &m).unwrap_err(), Skip::NotAligned);
        // VERP-style per-recipient bounce address on a mailer domain
        m.return_path = "<bounce+bob=friend.test@em.mailer.test>".into();
        assert!(matches!(run(&cfg(), &m).unwrap_err(), Skip::NotAligned | Skip::NoReplySender));
        m.return_path = "<bob=friend.test@em.mailer.test>".into();
        assert_eq!(run(&cfg(), &m).unwrap_err(), Skip::NotAligned);
        // same registrable domain is fine, including a subdomain
        m.return_path = "<bob@mail.friend.test>".into();
        assert_eq!(run(&cfg(), &m).unwrap().to, "bob@mail.friend.test");
        assert!(aligned("a.co.uk", "b.a.co.uk") && !aligned("a.co.uk", "b.co.uk"));
    }

    #[test]
    fn order_puts_protective_checks_before_limits() {
        // A bounce that also trips the sender limit is reported as a bounce, never "already replied".
        let mut m = mail();
        m.return_path = "<>".into();
        let h = History { replied_to_sender_within_7_days: true, replies_in_last_24h: 999 };
        assert_eq!(decide(&cfg(), &m, h, env()).unwrap_err(), Skip::NullReturnPath);
        let mut m = mail();
        m.auto_submitted = "auto-replied".into();
        assert_eq!(decide(&cfg(), &m, h, env()).unwrap_err(), Skip::AutoSubmitted);
        // The kill switch beats everything.
        assert_eq!(decide(&cfg(), &mail(), History::default(), Env { now: NOW, paused: true }).unwrap_err(), Skip::Paused);
    }

    #[test]
    fn display_name_tricks_do_not_pick_the_recipient() {
        let mut m = mail();
        m.from = "\"victim@other.test\" <bob@friend.test>".into();
        assert_eq!(run(&cfg(), &m).unwrap().to, "bob@friend.test");
        m.return_path = "<a@x.test> <b@y.test>".into();
        assert_eq!(run(&cfg(), &m).unwrap_err(), Skip::NoSender);
        m.return_path = String::new();
        m.from = "bob@friend.test\r\nBcc: evil@x.test".into();
        let r = run(&cfg(), &m);
        if let Ok(p) = r {
            assert_eq!(p.to, "bob@friend.test");
        }
    }

    #[test]
    fn render_template_and_hostile_subjects() {
        let mut c = cfg();
        // default: fixed text, the original subject is never echoed
        assert_eq!(render(&c, "Secret: merger plans").0, "Automatic reply");
        c.subject = "Away ({subject})".into();
        assert_eq!(render(&c, "Hi").0, "Away ()", "{{subject}} is inert unless the user opted in");
        c.echo_subject = true;
        assert_eq!(render(&c, "Hi\r\nBcc: x@y.z\u{202e}").0, "Away (Hi Bcc: x@y.z)");
        c.subject = String::new();
        let long = "s".repeat(500);
        let (s, _) = render(&c, &long);
        assert_eq!(s, format!("Auto: {}", "s".repeat(MAX_ECHO_CHARS)), "hard-cut to 60 chars");
        c.subject = "{subject}".into();
        assert_eq!(render(&c, "").0, "Automatic reply");
    }

    #[test]
    fn config_validation() {
        let ok = AutoReplyConfig { enabled: true, body: "  Back soon.\r\nBye  ".into(), subject: " Away ".into(), ..AutoReplyConfig::default() };
        assert!(validate_config(&AutoReplyConfig { subject: "Re {subject}".into(), ..ok.clone() }).is_err(), "{{subject}} needs the opt-in");
        assert!(validate_config(&AutoReplyConfig { subject: "Re {subject}".into(), echo_subject: true, ..ok.clone() }).is_ok());
        let v = validate_config(&ok).unwrap();
        assert_eq!((v.body.as_str(), v.subject.as_str()), ("Back soon.\nBye", "Away"));
        let bad = |f: &dyn Fn(&mut AutoReplyConfig)| {
            let mut c = ok.clone();
            f(&mut c);
            validate_config(&c).is_err()
        };
        assert!(bad(&|c| c.body = String::new()));
        assert!(bad(&|c| c.body = "x".repeat(MAX_BODY_CHARS + 1)));
        assert!(bad(&|c| c.subject = "a\nb".into()));
        assert!(bad(&|c| c.subject = "a\u{202e}b".into()));
        assert!(bad(&|c| c.subject = "s".repeat(MAX_SUBJECT_CHARS + 1)));
        assert!(bad(&|c| { c.start = Some(10); c.end = Some(10); }));
        assert!(bad(&|c| c.daily_cap = 0));
        assert!(bad(&|c| c.daily_cap = MAX_DAILY_CAP + 1));
        // disabled with an empty body is fine (a saved draft of the setting)
        assert!(validate_config(&AutoReplyConfig::default()).is_ok());
        assert!(!AutoReplyConfig::default().enabled, "OFF by default");
    }

    fn store_with(account: &str) -> Store {
        let mut st = Store::open_in_memory().unwrap();
        st.set_autoreply_config(account, &cfg()).unwrap();
        st
    }

    #[test]
    fn reservation_enforces_sender_and_daily_limits_atomically() {
        let a = "me@example.org";
        let mut st = store_with(a);
        let r1 = st.reserve_reply(a, "Bob@Friend.test", "Bob@Friend.test", "Lunch?", 3, NOW).unwrap().unwrap();
        assert_eq!(st.reserve_reply(a, "bob@friend.test", "bob@friend.test", "again", 3, NOW + 5).unwrap(), Err(Skip::SenderLimit), "pending counts, case-insensitive");
        st.finish_reply(r1, ReplyOutcome::Sent, "").unwrap();
        assert_eq!(st.reserve_reply(a, "bob@friend.test", "bob@friend.test", "x", 3, NOW + 6 * 86_400).unwrap(), Err(Skip::SenderLimit));
        assert!(st.reserve_reply(a, "bob@friend.test", "bob@friend.test", "x", 3, NOW + 7 * 86_400 + 1).unwrap().is_ok(), "allowed again after 7 days");
        // daily cap
        let mut st = store_with(a);
        for i in 0..3 {
            st.reserve_reply(a, &format!("s{i}@x.test"), &format!("s{i}@x.test"), "s", 3, NOW + i).unwrap().unwrap();
        }
        assert_eq!(st.reserve_reply(a, "s9@x.test", "s9@x.test", "s", 3, NOW + 10).unwrap(), Err(Skip::DailyCap));
        assert!(st.reserve_reply(a, "s9@x.test", "s9@x.test", "s", 3, NOW + DAY_SECS + 5).unwrap().is_ok(), "the cap is a rolling 24 hours");
        // another account is independent (when it is enabled)
        st.set_autoreply_config("other@example.org", &cfg()).unwrap();
        assert!(st.reserve_reply("other@example.org", "s0@x.test", "s0@x.test", "s", 3, NOW + 10).unwrap().is_ok());
        // a failed delivery frees the slot; a pending one left by a crash stays counted as `unknown`
        let mut st = store_with(a);
        let id = st.reserve_reply(a, "f@x.test", "f@x.test", "s", 1, NOW).unwrap().unwrap();
        assert_eq!(st.reserve_reply(a, "g@x.test", "g@x.test", "s", 1, NOW).unwrap(), Err(Skip::DailyCap));
        st.finish_reply(id, ReplyOutcome::Failed, "server refused").unwrap();
        let id2 = st.reserve_reply(a, "g@x.test", "g@x.test", "s", 1, NOW).unwrap().unwrap();
        assert_eq!(st.settle_pending_replies().unwrap(), 1);
        let h = st.reply_history(a, "g@x.test", NOW).unwrap();
        assert_eq!(h, History { replied_to_sender_within_7_days: true, replies_in_last_24h: 1 }, "crash-settled row still counts");
        assert_eq!(st.reserve_reply(a, "g@x.test", "g@x.test", "s", 5, NOW + 1).unwrap(), Err(Skip::SenderLimit));
        let _ = id2;
        // an ambiguous SMTP error is recorded as `unknown` and keeps counting
        let mut st = store_with(a);
        let id = st.reserve_reply(a, "u@x.test", "u@x.test", "s", 5, NOW).unwrap().unwrap();
        st.finish_reply(id, ReplyOutcome::Unknown, "connection lost").unwrap();
        assert_eq!(st.reserve_reply(a, "u@x.test", "u@x.test", "s", 5, NOW + 1).unwrap(), Err(Skip::SenderLimit));
    }

    #[test]
    fn sender_limit_covers_from_and_return_path_and_the_kill_switch_is_rechecked() {
        let a = "me@example.org";
        let mut st = store_with(a);
        // answered bob (From) via a Return-Path on the same domain
        st.reserve_reply(a, "bob@friend.test", "bounce-x@friend.test", "s", 9, NOW).unwrap().unwrap();
        assert_eq!(st.reserve_reply(a, "bob@friend.test", "bob@friend.test", "s", 9, NOW + 1).unwrap(), Err(Skip::SenderLimit), "same From");
        assert_eq!(st.reserve_reply(a, "carol@friend.test", "bounce-x@friend.test", "s", 9, NOW + 1).unwrap(), Err(Skip::SenderLimit), "same Return-Path");
        assert!(st.reserve_reply(a, "carol@friend.test", "carol@friend.test", "s", 9, NOW + 1).unwrap().is_ok());
        // pausing (or switching the account off) between decide() and the reservation refuses the reply
        st.set_autoreply_paused(true).unwrap();
        assert_eq!(st.reserve_reply(a, "dave@friend.test", "dave@friend.test", "s", 9, NOW + 2).unwrap(), Err(Skip::Paused));
        st.set_autoreply_paused(false).unwrap();
        let mut off = cfg();
        off.enabled = false;
        st.set_autoreply_config(a, &off).unwrap();
        assert_eq!(st.reserve_reply(a, "dave@friend.test", "dave@friend.test", "s", 9, NOW + 2).unwrap(), Err(Skip::Disabled));
        assert_eq!(st.reserve_reply("never-configured@example.org", "dave@friend.test", "dave@friend.test", "s", 9, NOW + 2).unwrap(), Err(Skip::Disabled));
    }

    #[test]
    fn audit_log_records_who_when_why_and_stays_bounded() {
        let mut st = Store::open_in_memory().unwrap();
        let a = "me@example.org";
        st.log_skip(a, "News@Shop.test", "Sale!!", Skip::ListMail, NOW).unwrap();
        let rows = st.list_autoreply_log(Some(a), 10).unwrap();
        assert_eq!((rows[0].sender.as_str(), rows[0].outcome.as_str(), rows[0].reason.as_str(), rows[0].ts), ("news@shop.test", "skipped", "list_mail", NOW));
        assert!(rows[0].detail.contains("newsletter"));
        for i in 0..(LOG_KEEP + 50) {
            st.log_skip(a, "x@y.test", &format!("s{i}"), Skip::Bulk, NOW + i as i64).unwrap();
        }
        assert_eq!(st.list_autoreply_log(Some(a), 5000).unwrap().len(), LOG_KEEP);
        // sent rows are never trimmed by the skip cap
        st.set_autoreply_config(a, &cfg()).unwrap();
        let id = st.reserve_reply(a, "keep@y.test", "keep@y.test", "k", 5, NOW).unwrap().unwrap();
        st.finish_reply(id, ReplyOutcome::Sent, "").unwrap();
        for i in 0..20 {
            st.log_skip(a, "x@y.test", "s", Skip::Bulk, NOW + 10_000 + i).unwrap();
        }
        assert!(st.list_autoreply_log(Some(a), 5000).unwrap().iter().any(|r| r.outcome == "sent"));
    }

    #[test]
    fn config_store_and_kill_switch() {
        let mut st = Store::open_in_memory().unwrap();
        assert_eq!(st.autoreply_config("a@x.org").unwrap(), AutoReplyConfig::default());
        let c = validate_config(&cfg()).unwrap();
        st.set_autoreply_config("a@x.org", &c).unwrap();
        assert_eq!(st.autoreply_config("a@x.org").unwrap(), c);
        assert!(!st.autoreply_paused().unwrap());
        st.set_autoreply_paused(true).unwrap();
        assert!(st.autoreply_paused().unwrap());
        st.purge_autoreply_account("a@x.org").unwrap();
        assert!(!st.autoreply_config("a@x.org").unwrap().enabled);
    }
}
