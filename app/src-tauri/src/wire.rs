//! Wire (camelCase) types for the frontend and mapping from mailcore/autodetect types.

use autodetect::TlsMode;
use mailcore::{AccountConfig, EmailAddress, Flags, Folder, FolderRole, MessageSummary, OutgoingMessage, ReplyTo};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireTls {
    Implicit,
    Starttls,
}

impl From<mailcore::Tls> for WireTls {
    fn from(t: mailcore::Tls) -> Self {
        match t {
            mailcore::Tls::Implicit => WireTls::Implicit,
            mailcore::Tls::StartTls => WireTls::Starttls,
        }
    }
}
impl From<WireTls> for mailcore::Tls {
    fn from(t: WireTls) -> Self {
        match t {
            WireTls::Implicit => mailcore::Tls::Implicit,
            WireTls::Starttls => mailcore::Tls::StartTls,
        }
    }
}
impl From<TlsMode> for WireTls {
    fn from(t: TlsMode) -> Self {
        match t {
            TlsMode::Implicit => WireTls::Implicit,
            TlsMode::StartTls => WireTls::Starttls,
        }
    }
}
impl From<WireTls> for TlsMode {
    fn from(t: WireTls) -> Self {
        match t {
            WireTls::Implicit => TlsMode::Implicit,
            WireTls::Starttls => TlsMode::StartTls,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireServer {
    pub host: String,
    pub port: u16,
    pub tls: WireTls,
}

impl From<&mailcore::ServerConfig> for WireServer {
    fn from(s: &mailcore::ServerConfig) -> Self {
        WireServer { host: s.host.clone(), port: s.port, tls: s.tls.into() }
    }
}
impl From<&WireServer> for mailcore::ServerConfig {
    fn from(s: &WireServer) -> Self {
        mailcore::ServerConfig { host: s.host.trim().to_string(), port: s.port, tls: s.tls.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    pub email: String,
    pub username: String,
    pub imap: WireServer,
    pub smtp: WireServer,
    pub provider: Option<String>,
}

impl AccountInfo {
    pub fn from_config(c: &AccountConfig, provider: Option<String>) -> Self {
        AccountInfo {
            email: c.email.clone(),
            username: c.username.clone(),
            imap: (&c.imap).into(),
            smtp: (&c.smtp).into(),
            provider,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Advanced {
    pub imap: WireServer,
    pub smtp: WireServer,
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireFolder {
    pub name: String,
    pub role: FolderRole,
    pub selectable: bool,
    pub delimiter: Option<String>,
}

impl From<&Folder> for WireFolder {
    fn from(f: &Folder) -> Self {
        WireFolder { name: f.name.clone(), role: f.role, selectable: f.selectable, delimiter: f.delimiter.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    pub name: Option<String>,
    pub address: String,
}

impl From<&EmailAddress> for Address {
    fn from(a: &EmailAddress) -> Self {
        Address { name: a.name.clone(), address: a.address.clone() }
    }
}

/// Parse the `"Name <addr>, addr2"` display form that mailcore stores in SQLite.
/// A comma inside a display name ("Doe, John <j@x>") is handled: a segment is a
/// bare address only if it contains `@` and no `<`.
pub fn parse_addresses(s: &str) -> Vec<Address> {
    let mut out = Vec::new();
    let mut pending = String::new();
    for seg in s.split(',') {
        if !pending.is_empty() {
            pending.push(',');
        }
        pending.push_str(seg);
        let p = pending.trim();
        if p.is_empty() {
            pending.clear();
            continue;
        }
        if let (Some(l), true) = (p.rfind('<'), p.ends_with('>')) {
            let addr = p[l + 1..p.len() - 1].trim().to_string();
            let name = p[..l].trim().trim_matches('"').trim().to_string();
            out.push(Address { name: (!name.is_empty()).then_some(name), address: addr });
            pending.clear();
        } else if p.contains('@') && !p.contains('<') && !p.contains(' ') {
            out.push(Address { name: None, address: p.to_string() });
            pending.clear();
        }
    }
    let p = pending.trim();
    if !p.is_empty() && p.contains('@') {
        out.push(Address { name: None, address: p.to_string() });
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireFlags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub deleted: bool,
}

impl From<Flags> for WireFlags {
    fn from(f: Flags) -> Self {
        WireFlags { seen: f.seen, flagged: f.flagged, answered: f.answered, draft: f.draft, deleted: f.deleted }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSummary {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub message_id: Option<String>,
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub date: Option<i64>,
    pub flags: WireFlags,
    pub snippet: String,
    pub has_body: bool,
    /// UIDVALIDITY the UID was observed under (a forward stores it so a restored draft cannot fetch another message).
    pub uidvalidity: u32,
}

impl From<&MessageSummary> for WireSummary {
    fn from(m: &MessageSummary) -> Self {
        WireSummary {
            account: m.account.clone(),
            folder: m.folder.clone(),
            uid: m.uid,
            message_id: m.message_id.clone(),
            subject: m.subject.clone(),
            from: parse_addresses(&m.from).into_iter().next(),
            to: parse_addresses(&m.to),
            date: (m.date != 0).then_some(m.date),
            flags: m.flags.into(),
            snippet: m.snippet.clone(),
            has_body: m.has_body,
            uidvalidity: m.uidvalidity,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireAttachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: usize,
}

pub type WireImage = mailcore::ImageInfo;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageBody {
    pub summary: WireSummary,
    pub text: Option<String>,
    pub html: Option<String>,
    pub blocked_remote_count: usize,
    /// Advisory analysis of every remote image in the raw HTML (loaded or not).
    pub images: Vec<WireImage>,
    pub attachments: Vec<WireAttachment>,
    pub cc: Vec<Address>,
    pub references: Option<String>,
    /// First `text/calendar` part / `.ics` attachment, parsed (null when absent or unusable).
    pub invite: Option<calendar::Invite>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentData {
    pub name: String,
    pub mime: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireSyncReport {
    pub new: u32,
    pub updated: u32,
    pub removed: u32,
    pub uidvalidity_reset: bool,
    pub server_total: u32,
}

impl From<mailcore::SyncReport> for WireSyncReport {
    fn from(r: mailcore::SyncReport) -> Self {
        WireSyncReport {
            new: r.new,
            updated: r.updated,
            removed: r.removed,
            uidvalidity_reset: r.uidvalidity_reset,
            server_total: r.server_total,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireLoadOlder {
    pub loaded: u32,
    pub remaining_on_server: u32,
    pub server_total: u32,
    pub reached_start: bool,
}

impl From<mailcore::LoadOlderReport> for WireLoadOlder {
    fn from(r: mailcore::LoadOlderReport) -> Self {
        WireLoadOlder {
            loaded: r.loaded,
            remaining_on_server: r.remaining_on_server,
            server_total: r.server_total,
            reached_start: r.reached_start,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireReplyTo {
    pub message_id: String,
    pub references: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outgoing {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: String,
    pub in_reply_to: Option<WireReplyTo>,
    #[serde(default)]
    pub attachments: Vec<WireOutAttachment>,
    /// Composer HTML (rich mode). Sanitised on conversion; `text` is the plain alternative.
    #[serde(default)]
    pub html: Option<String>,
    /// Requested sender address; honoured only if it is the account's own address or a saved identity of it.
    #[serde(default)]
    pub from_address: Option<String>,
}

/// A file attached in Compose (dropped or picked), base64 over the IPC bridge.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireOutAttachment {
    pub name: String,
    pub mime: String,
    pub data_base64: String,
}

/// Most a message may carry in attachments (before base64 expansion). Most providers cap near 25 MB.
pub const MAX_ATTACH_BYTES: usize = 20 * 1024 * 1024;

/// A safe display/file name: no path parts or control characters, never empty.
pub fn clean_attachment_name(n: &str) -> String {
    let base = n.rsplit(['/', '\\']).next().unwrap_or("");
    let s: String = base.chars().filter(|c| !c.is_control() && !crate::textsafe::is_format_char(*c)).collect::<String>().trim().chars().take(180).collect();
    if s.is_empty() { "attachment".into() } else { s }
}

/// Decode and size-check the attachments of an outgoing message.
pub fn decode_attachments(list: &[WireOutAttachment]) -> Result<Vec<mailcore::OutgoingAttachment>, String> {
    use base64::Engine as _;
    let mut total = 0usize;
    let mut out = Vec::with_capacity(list.len());
    for a in list {
        // Refuse before allocating: base64 of N bytes is about 4N/3 characters.
        if total.saturating_add(a.data_base64.len() / 4 * 3) > MAX_ATTACH_BYTES + 3 {
            return Err("Attachments are over 20 MB in total. Remove some or share a link instead.".into());
        }
        let data = base64::engine::general_purpose::STANDARD.decode(a.data_base64.as_bytes()).map_err(|_| format!("Could not read the attachment \u{201c}{}\u{201d}.", clean_attachment_name(&a.name)))?;
        total += data.len();
        if total > MAX_ATTACH_BYTES {
            return Err("Attachments are over 20 MB in total. Remove some or share a link instead.".into());
        }
        let mime = if a.mime.trim().is_empty() { "application/octet-stream".to_string() } else { a.mime.trim().to_string() };
        out.push(mailcore::OutgoingAttachment { name: clean_attachment_name(&a.name), mime, data });
    }
    Ok(out)
}

impl From<Outgoing> for OutgoingMessage {
    fn from(o: Outgoing) -> Self {
        OutgoingMessage {
            to: o.to,
            cc: o.cc,
            bcc: o.bcc,
            subject: o.subject,
            text: o.text,
            html: o.html.as_deref().filter(|h| !h.trim().is_empty()).map(mailcore::sanitize_compose_html),
            attachments: vec![],
            in_reply_to: o.in_reply_to.map(|r| ReplyTo {
                message_id: r.message_id,
                references: r.references.map(|s| s.split_whitespace().map(str::to_owned).collect()).unwrap_or_default(),
            }),
            calendar: None,
            extra_headers: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NewMailEvent {
    pub account: String,
    pub folder: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncedEvent {
    pub account: String,
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveEvent {
    pub account: String,
    pub folder: String,
    pub state: crate::live::LiveState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusEvent {
    pub account: String,
    pub state: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_info_wire_shape() {
        let v = mailcore::analyze_remote_images("<img src=\"https://t.example/open.gif\" width=\"1\">", &[]);
        let j = serde_json::to_value(&v[0]).unwrap();
        assert_eq!(j["verdict"], "tracker");
        assert_eq!(j["host"], "t.example");
        assert!(j["reasons"].is_array() && j.get("tracker").is_some() && j["width"] == 1);
    }

    #[test]
    fn tls_wire_names() {
        assert_eq!(serde_json::to_string(&WireTls::Starttls).unwrap(), "\"starttls\"");
        assert_eq!(serde_json::to_string(&WireTls::Implicit).unwrap(), "\"implicit\"");
        assert_eq!(mailcore::Tls::from(WireTls::Starttls), mailcore::Tls::StartTls);
    }

    #[test]
    fn address_parsing() {
        let a = parse_addresses("Bob <bob@x.org>, carol@y.org, Doe, John <j@z.org>");
        assert_eq!(a.len(), 3);
        assert_eq!(a[0], Address { name: Some("Bob".into()), address: "bob@x.org".into() });
        assert_eq!(a[1], Address { name: None, address: "carol@y.org".into() });
        assert_eq!(a[2], Address { name: Some("Doe, John".into()), address: "j@z.org".into() });
        assert!(parse_addresses("").is_empty());
    }

    #[test]
    fn summary_maps_to_camel_case() {
        let m = MessageSummary {
            account: "a@x.org".into(),
            folder: "INBOX".into(),
            uid: 7,
            uidvalidity: 1,
            message_id: Some("id@x".into()),
            subject: "Hi".into(),
            from: "Bob <bob@x.org>".into(),
            to: "a@x.org".into(),
            date: 0,
            flags: Flags { seen: true, ..Default::default() },
            snippet: String::new(),
            has_body: false,
            size: 10,
        };
        let w = WireSummary::from(&m);
        assert_eq!(w.date, None);
        let v = serde_json::to_value(&w).unwrap();
        assert_eq!(v["messageId"], "id@x");
        assert_eq!(v["hasBody"], false);
        assert_eq!(v["uidvalidity"], 1);
        assert_eq!(v["from"]["address"], "bob@x.org");
        assert_eq!(v["flags"]["seen"], true);
        assert_eq!(v["to"][0]["name"], serde_json::Value::Null);
    }

    #[test]
    fn outgoing_deserialises_and_splits_references() {
        let o: Outgoing = serde_json::from_str(
            r#"{"to":["a@b.c"],"cc":[],"bcc":[],"subject":"s","text":"t","inReplyTo":{"messageId":"m","references":"r1 r2"}}"#,
        )
        .unwrap();
        let m: OutgoingMessage = o.into();
        assert_eq!(m.in_reply_to.unwrap().references, vec!["r1", "r2"]);
    }

    #[test]
    fn outgoing_html_is_sanitised_and_optional() {
        let o: Outgoing = serde_json::from_str(r#"{"to":["a@b.c"],"cc":[],"bcc":[],"subject":"s","text":"t","inReplyTo":null,"html":"<p onclick=x()>hi</p><script>alert(1)</script><img src=\"https://t.example/p.gif\">","fromAddress":"alias@x.org"}"#).unwrap();
        assert_eq!(o.from_address.as_deref(), Some("alias@x.org"));
        let m: OutgoingMessage = o.into();
        let h = m.html.unwrap();
        assert!(h.contains("<p>hi</p>") && !h.contains("script") && !h.contains("onclick") && !h.contains("t.example"), "{h}");
        let plain: Outgoing = serde_json::from_str(r#"{"to":["a@b.c"],"cc":[],"bcc":[],"subject":"s","text":"t","inReplyTo":null,"html":"  "}"#).unwrap();
        assert!(OutgoingMessage::from(plain).html.is_none());
    }

    #[test]
    fn account_info_shape() {
        let c = AccountConfig {
            email: "a@x.org".into(),
            imap: mailcore::ServerConfig { host: "i".into(), port: 993, tls: mailcore::Tls::Implicit },
            smtp: mailcore::ServerConfig { host: "s".into(), port: 587, tls: mailcore::Tls::StartTls },
            username: "a@x.org".into(),
        };
        let v = serde_json::to_value(AccountInfo::from_config(&c, None)).unwrap();
        assert_eq!(v["smtp"]["tls"], "starttls");
        assert_eq!(v["provider"], serde_json::Value::Null);
    }

    #[test]
    fn message_body_carries_an_optional_camel_case_invite() {
        let summary = WireSummary::from(&MessageSummary {
            account: "a@x.org".into(),
            folder: "INBOX".into(),
            uid: 1,
            uidvalidity: 1,
            message_id: None,
            subject: "s".into(),
            from: "b@x.org".into(),
            to: "a@x.org".into(),
            date: 0,
            flags: Flags::default(),
            snippet: String::new(),
            has_body: true,
            size: 1,
        });
        let mut body = MessageBody {
            summary,
            text: None,
            html: None,
            blocked_remote_count: 0,
            images: vec![],
            attachments: vec![],
            cc: vec![],
            references: None,
            invite: None,
        };
        let v = serde_json::to_value(&body).unwrap();
        assert!(v.get("invite").is_some() && v["invite"].is_null(), "invite must be present as null");
        assert!(v.get("blockedRemoteCount").is_some() && v.get("references").is_some(), "existing fields kept");

        let ics = b"BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:u@x\r\nDTSTAMP:20261101T100000Z\r\nDTSTART:20261110T090000Z\r\nDTEND:20261110T100000Z\r\nSUMMARY:S\r\nORGANIZER:mailto:o@x.org\r\nATTENDEE;PARTSTAT=TENTATIVE:mailto:a@x.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        body.invite = calendar::itip::invite_from_ics(ics, &["a@x.org".to_string()], "3", true).unwrap();
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["invite"]["method"], "REQUEST");
        assert_eq!(v["invite"]["myStatus"], "tentative");
        assert_eq!(v["invite"]["alreadyInCalendar"], true);
        assert_eq!(v["invite"]["attachmentId"], "3");
        assert_eq!(v["invite"]["event"]["calendarId"], "");
        assert_eq!(v["invite"]["event"]["allDay"], false);
        assert_eq!(v["invite"]["event"]["organizer"]["email"], "o@x.org");
    }
}

#[cfg(test)]
mod attach_tests {
    use super::*;
    fn w(name: &str, b64: &str) -> WireOutAttachment { WireOutAttachment { name: name.into(), mime: "".into(), data_base64: b64.into() } }
    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_attachment_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_attachment_name("C:\\x\\a.pdf"), "a.pdf");
        assert_eq!(clean_attachment_name("  \n"), "attachment");
        assert_eq!(clean_attachment_name("invoice\u{202E}fdp.exe\u{200B}\u{FEFF}\u{2066}x\u{2029}"), "invoicefdp.exex");
        assert_eq!(clean_attachment_name("\u{202E}\u{200F}"), "attachment");
    }
    #[test]
    fn decodes_and_defaults_mime() {
        let v = decode_attachments(&[w("a.txt", "aGVsbG8=")]).unwrap();
        assert_eq!((v[0].data.as_slice(), v[0].mime.as_str()), (&b"hello"[..], "application/octet-stream"));
        assert!(decode_attachments(&[w("a", "!!!")]).is_err());
    }
    #[test]
    fn oversize_is_refused_before_decoding() {
        // Not valid base64 at all: only the length check can produce the size error.
        let junk = "!".repeat(MAX_ATTACH_BYTES / 3 * 4 + 64);
        let e = decode_attachments(&[w("a.bin", &junk)]).unwrap_err();
        assert!(e.contains("over 20 MB"), "{e}");
        let e = decode_attachments(&[w("a.bin", "!!!!")]).unwrap_err();
        assert!(e.contains("Could not read"), "{e}");
    }
    #[test]
    fn total_size_is_capped() {
        use base64::Engine as _;
        let big = base64::engine::general_purpose::STANDARD.encode(vec![0u8; MAX_ATTACH_BYTES / 2 + 1]);
        assert!(decode_attachments(&[w("a", &big), w("b", &big)]).is_err());
        assert!(decode_attachments(&[w("a", &big)]).is_ok());
    }
}
