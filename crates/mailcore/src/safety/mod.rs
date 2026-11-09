//! Mail safety checks, Tier 1 (docs/LINK_SANDBOX_SPEC.md, docs/SAFETY_CHECKS.md).
//!
//! Deterministic, on this Mac, no AI, no network, no DNS. Pure functions take facts that are already parsed out of
//! a message ([`SafetyInput`]) and return a [`SafetyReport`]: a score, a level and the reasons behind it.
//!
//! There is deliberately no "safe" level. The best result is [`Level::NoneFound`] ("no red flags found"), which
//! says only that these heuristics found nothing. Every reason carries plain-English text; attacker-controlled
//! strings (display names, hosts, file names) are cleaned and clipped before they reach `detail`.

pub mod attach;
pub mod auth;
pub mod content;
pub mod data;
pub mod html;
pub mod links;
pub mod sender;
pub mod urls;

use crate::mime::{ExtraHeaders, ParsedMessage};
use crate::types::EmailAddress;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    fn weight(self) -> f64 {
        match self {
            Severity::Info => 0.0,
            Severity::Low => 0.08,
            Severity::Medium => 0.2,
            Severity::High => 0.4,
            Severity::Critical => 0.7,
        }
    }
    fn bump(self) -> Severity {
        match self {
            Severity::Info => Severity::Low,
            Severity::Low => Severity::Medium,
            Severity::Medium => Severity::High,
            s => s,
        }
    }
}

/// `NoneFound` is "no red flags found", never "safe".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    NoneFound,
    Caution,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    pub code: String,
    pub severity: Severity,
    pub title: String,
    /// Plain text (never HTML). May quote cleaned, clipped text from the message.
    pub detail: String,
    /// How many times this code fired (for example links); the detail describes the first.
    pub count: u32,
    #[serde(skip)]
    pub floor: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyReport {
    pub score: u8,
    pub level: Level,
    /// Most severe first.
    pub reasons: Vec<Reason>,
}

#[derive(Debug, Clone, Default)]
pub struct LinkFact {
    pub href: String,
    /// Visible text of the anchor (tags stripped).
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct AttachFact {
    pub name: String,
    pub mime: String,
}

/// Already-parsed facts about one message. Everything is optional / empty-safe.
#[derive(Debug, Clone, Default)]
pub struct SafetyInput {
    pub from: Option<EmailAddress>,
    pub reply_to: Vec<EmailAddress>,
    /// Raw `Return-Path` header value.
    pub return_path: String,
    /// The stored (first) `Authentication-Results` header, whitespace-collapsed and capped by the parser.
    pub authentication_results: String,
    /// Server names allowed to sign `Authentication-Results` (the account's IMAP host and mail domain).
    pub trusted_auth_hosts: Vec<String>,
    /// The account's own address plus saved identities / aliases.
    pub own_addresses: Vec<String>,
    /// `List-Id` present or `Precedence: list|bulk`.
    pub is_list_mail: bool,
    pub subject: String,
    /// Readable text (hidden content removed when possible).
    pub text: String,
    /// Anchors of the SANITISED html (what the reader can click).
    pub links: Vec<LinkFact>,
    /// Anchors in the RAW html whose scheme is dangerous (`javascript:`, `data:`, ...), counted before sanitising.
    pub dangerous_scheme_links: u32,
    pub attachments: Vec<AttachFact>,
    pub has_reply_headers: bool,
    /// Messages from this sender address already cached on this Mac (including this one). `None` = unknown.
    pub cached_from_count: Option<u32>,
}

// ------------------------------------------------------------------------------------------ collector

/// Collects reasons, merging repeats of the same code.
#[derive(Default)]
pub struct Collector {
    pub items: Vec<Reason>,
}

pub fn title_of(code: &str) -> &'static str {
    match code {
        "AUTH_DMARC_FAIL" => "Sender failed the DMARC check",
        "AUTH_DKIM_FAIL" => "Signature check failed (DKIM)",
        "AUTH_SPF_FAIL" => "Sending server not authorised (SPF)",
        "AUTH_NONE" => "No sender authentication passed",
        "AUTH_UNKNOWN" => "Sender authentication could not be checked",
        "SENDER_REPLYTO_MISMATCH" => "Replies go to a different domain",
        "SENDER_RETURNPATH_MISMATCH" => "Bounce address differs from the sender",
        "SENDER_DISPLAY_NAME_ADDRESS" => "Display name shows another address",
        "SENDER_DISPLAY_NAME_BRAND" => "Name claims a well-known brand",
        "SENDER_SPOOFS_OWN_DOMAIN" => "Claims to come from your own domain",
        "SENDER_FIRST_TIME" => "First message from this sender",
        "DOMAIN_CONFUSABLE" => "Look-alike domain",
        "DOMAIN_MIXED_SCRIPT" => "Domain mixes alphabets",
        "DOMAIN_PUNYCODE" => "Domain uses non-ASCII letters",
        "DOMAIN_EDIT_DISTANCE" => "Domain is almost a well-known name",
        "DOMAIN_BRAND_IN_SUBDOMAIN" => "Well-known name used inside another domain",
        "DOMAIN_SUSPICIOUS_TLD" => "Domain ending often abused",
        "LINK_TEXT_MISMATCH" => "Link text shows a different website",
        "LINK_TRACKING_REDIRECT" => "Link goes through a mailing or click-tracking service",
        "LINK_IP_LITERAL" => "Link goes to a numeric address",
        "LINK_USERINFO" => "Link hides its real destination after an @",
        "LINK_SHORTENER" => "Link uses a URL shortener",
        "LINK_OPEN_REDIRECT" => "Link redirects to another site",
        "LINK_DANGEROUS_SCHEME" => "Link runs code or opens local data",
        "LINK_NONSTANDARD_PORT" => "Link uses an unusual port",
        "LINK_VERY_LONG" => "Very long link",
        "LINK_TRAILING_DOT" => "Link address ends in a dot",
        "LINK_OBFUSCATED_HOST" => "Link address is disguised",
        "ATT_EXECUTABLE" => "Attachment is a program or script",
        "ATT_DOUBLE_EXT" => "Attachment has a disguised file type",
        "ATT_MACRO" => "Attachment can contain macros",
        "ATT_ARCHIVE" => "Attachment is an archive",
        "ATT_ENCRYPTED_ARCHIVE_PASSWORD" => "Archive with a password in the message",
        "ATT_DISK_IMAGE" => "Attachment is a disk image",
        "ATT_HTML" => "Attachment is a web page",
        "ATT_MIME_MISMATCH" => "Attachment type does not match its name",
        "ATT_BIDI_NAME" => "Attachment name hides its real ending",
        "CONTENT_URGENCY" => "Pressure and urgency wording",
        "CONTENT_CREDENTIAL_REQUEST" => "Asks you to confirm credentials or card details",
        "CONTENT_PAYMENT_CHANGE" => "Mentions changed payment details",
        "CONTENT_GIFT_CARD" => "Asks for gift cards",
        "CONTENT_SECRECY" => "Asks you to keep it secret",
        "THREAD_FAKE_REPLY" => "Looks like a reply but is not part of a conversation",
        _ => "Warning sign",
    }
}

/// Plain text safe to show: control and bidi characters become spaces, whitespace is collapsed, length clipped.
pub fn clean(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() || is_bidi(c) || matches!(c, '\u{200b}' | '\u{feff}' | '\u{2028}' | '\u{2029}') { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut t: String = flat.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

pub fn is_bidi(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
}

impl Collector {
    pub fn add(&mut self, code: &str, severity: Severity, detail: impl Into<String>) {
        self.add_floor(code, severity, 0, detail);
    }

    pub fn add_floor(&mut self, code: &str, severity: Severity, floor: u8, detail: impl Into<String>) {
        if let Some(r) = self.items.iter_mut().find(|r| r.code == code) {
            r.count += 1;
            r.severity = r.severity.max(severity);
            r.floor = r.floor.max(floor);
            return;
        }
        self.items.push(Reason { code: code.to_string(), severity, title: title_of(code).to_string(), detail: detail.into(), count: 1, floor });
    }

    pub fn has(&self, code: &str) -> bool {
        self.items.iter().any(|r| r.code == code)
    }
}

// ------------------------------------------------------------------------------------------ scoring

/// Noisy-OR of the reason weights: `round(100 * (1 - prod(1 - w)))`, so many weak signals cannot add up linearly.
pub fn score_of(reasons: &[Reason]) -> u8 {
    let keep: f64 = reasons.iter().map(|r| 1.0 - r.severity.weight()).product();
    (100.0 * (1.0 - keep)).round().clamp(0.0, 100.0) as u8
}

pub fn level_of(score: u8) -> Level {
    match score {
        0..=19 => Level::NoneFound,
        20..=49 => Level::Caution,
        _ => Level::Warning,
    }
}

/// Wording that raises sensitive content codes when the sender or links already look off.
fn bump_content(c: &mut Collector) {
    let risky = c.items.iter().any(|r| {
        (r.code.starts_with("SENDER_") && r.severity >= Severity::Medium && r.code != "SENDER_FIRST_TIME")
            || (r.code.starts_with("AUTH_") && r.code.ends_with("_FAIL") && r.severity >= Severity::Medium)
            || (r.code.starts_with("LINK_") && r.severity >= Severity::Medium)
            || r.code.starts_with("DOMAIN_CONFUSABLE")
    });
    if !risky {
        return;
    }
    for r in &mut c.items {
        if matches!(r.code.as_str(), "CONTENT_CREDENTIAL_REQUEST" | "CONTENT_PAYMENT_CHANGE" | "CONTENT_GIFT_CARD" | "CONTENT_SECRECY") {
            r.severity = r.severity.bump();
        }
    }
}

pub fn analyze(input: &SafetyInput) -> SafetyReport {
    let mut c = Collector::default();
    auth::check(input, &mut c);
    sender::check(input, &mut c);
    links::check(input, &mut c);
    attach::check(input, &mut c);
    content::check(input, &mut c);
    bump_content(&mut c);
    let mut reasons = c.items;
    reasons.sort_by_key(|a| std::cmp::Reverse(a.severity));
    let floor = reasons.iter().map(|r| r.floor).max().unwrap_or(0);
    let score = score_of(&reasons).max(floor);
    SafetyReport { score, level: level_of(score), reasons }
}

// ------------------------------------------------------------------------------------------ building the input

/// Facts from a parsed message. `own` = the account's address and saved identities; `hosts` = server names allowed
/// to sign `Authentication-Results`; `cached_from_count` = from `Storage::count_from` (local cache only).
pub fn build_input(parsed: &ParsedMessage, extra: &ExtraHeaders, own: &[String], hosts: &[String], cached_from_count: Option<u32>) -> SafetyInput {
    let raw_html = parsed.html.as_deref().unwrap_or("");
    let sanitised = if raw_html.is_empty() { String::new() } else { crate::sanitize::sanitize_html(raw_html, false).html };
    let list = !extra.list_id.trim().is_empty() || matches!(extra.precedence.trim().to_ascii_lowercase().as_str(), "list" | "bulk");
    SafetyInput {
        from: parsed.headers.from.first().cloned(),
        reply_to: parsed.headers.reply_to.clone(),
        return_path: extra.return_path.clone(),
        authentication_results: extra.authentication_results.clone(),
        trusted_auth_hosts: hosts.to_vec(),
        own_addresses: own.to_vec(),
        is_list_mail: list,
        subject: parsed.headers.subject.clone(),
        text: parsed.ai_text().chars().take(content::MAX_TEXT).collect(),
        links: if raw_html.is_empty() { html::text_urls(parsed.text.as_deref().unwrap_or(""), html::MAX_ANCHORS) } else { html::anchors(&sanitised, html::MAX_ANCHORS) },
        dangerous_scheme_links: html::dangerous_scheme_count(raw_html),
        attachments: parsed.attachments.iter().map(|a| AttachFact { name: a.name.clone(), mime: a.mime.clone() }).collect(),
        has_reply_headers: !parsed.headers.in_reply_to.is_empty() || !parsed.headers.references.is_empty(),
        cached_from_count,
    }
}

#[cfg(test)]
mod tests;
