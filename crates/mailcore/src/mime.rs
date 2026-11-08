//! MIME parsing (mail-parser) into frontend-friendly types.

use crate::sanitize::{sanitize_html_with, SanitizedHtml};
use crate::types::EmailAddress;
use crate::{Error, Result};
use base64::Engine as _;
use mail_parser::{AddressList, MessageParser, PartKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageHeaders {
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Vec<EmailAddress>,
    pub to: Vec<EmailAddress>,
    pub cc: Vec<EmailAddress>,
    /// Unix seconds (UTC), if the Date header parsed.
    pub date: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentInfo {
    /// Opaque id for `attachment_bytes` (index within the attachment list).
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: usize,
    pub content_id: Option<String>,
    pub inline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedMessage {
    pub headers: MessageHeaders,
    /// Plain-text body (derived from HTML if the message has no text part).
    pub text: Option<String>,
    /// Raw (UNSANITISED) HTML body. Never render without `sanitize_html`.
    pub html: Option<String>,
    pub attachments: Vec<AttachmentInfo>,
}

impl ParsedMessage {
    /// Text for the AI: derived from the HTML with hidden content removed when there is an HTML part, else the
    /// plain part. See [`crate::ai_text::ai_text`].
    pub fn ai_text(&self) -> String {
        crate::ai_text::ai_text(self.html.as_deref().unwrap_or(""), self.text.as_deref())
    }
}

fn addrs(l: Option<AddressList<'_>>) -> Vec<EmailAddress> {
    l.map(|l| {
        l.mailboxes()
            .filter_map(|m| {
                m.address().map(|a| EmailAddress {
                    name: m.name().filter(|n| !n.is_empty()).map(str::to_owned),
                    address: a.to_owned(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

fn strip_angle(s: &str) -> String {
    s.trim().trim_start_matches('<').trim_end_matches('>').to_owned()
}

fn headers_of(m: &mail_parser::Message<'_>) -> MessageHeaders {
    MessageHeaders {
        message_id: m.message_id().map(strip_angle),
        in_reply_to: m
            .in_reply_to()
            .map(|l| l.iter().map(strip_angle).collect())
            .unwrap_or_default(),
        references: m
            .references()
            .map(|l| l.iter().map(strip_angle).collect())
            .unwrap_or_default(),
        subject: m.subject().unwrap_or_default().to_owned(),
        from: addrs(m.from()),
        to: addrs(m.to()),
        cc: addrs(m.cc()),
        date: m.date().filter(|d| d.is_valid()).map(|d| d.to_timestamp()),
    }
}

/// Parse only a header block (as returned by `BODY.PEEK[HEADER]`).
pub fn parse_headers(raw: &[u8]) -> Result<MessageHeaders> {
    let mut buf;
    let data: &[u8] = if raw.ends_with(b"\r\n\r\n") || raw.ends_with(b"\n\n") {
        raw
    } else {
        buf = raw.to_vec();
        buf.extend_from_slice(b"\r\n\r\n");
        &buf
    };
    let m = MessageParser::default()
        .parse_headers(data)
        .ok_or_else(|| Error::Parse("unparseable headers".into()))?;
    Ok(headers_of(&m))
}

/// Parse a full RFC 5322 message.
pub fn parse_message(raw: &[u8]) -> Result<ParsedMessage> {
    let m = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Parse("unparseable message".into()))?;
    Ok(parsed_from(&m))
}

fn parsed_from(m: &mail_parser::Message<'_>) -> ParsedMessage {
    let headers = headers_of(m);

    let mut html_parts = Vec::new();
    for p in m.html_body() {
        if matches!(p.kind(), PartKind::Html) {
            if let Some(t) = p.text() {
                html_parts.push(t.into_owned());
            }
        }
    }
    let html = (!html_parts.is_empty()).then(|| html_parts.join("\n"));

    let mut text_parts = Vec::new();
    for p in m.text_body() {
        if matches!(p.kind(), PartKind::Text) {
            if let Some(t) = p.text() {
                text_parts.push(t.into_owned());
            }
        }
    }
    let text = if text_parts.is_empty() {
        m.body_text(0).map(|t| t.into_owned())
    } else {
        Some(text_parts.join("\n"))
    };

    let attachments = m
        .attachments()
        .enumerate()
        .map(|(i, p)| {
            let ct = p.content_type();
            let mime = ct
                .as_ref()
                .map(|c| match c.subtype() {
                    Some(s) => format!("{}/{}", c.ctype(), s),
                    None => c.ctype().to_owned(),
                })
                .unwrap_or_else(|| "application/octet-stream".into())
                .to_ascii_lowercase();
            AttachmentInfo {
                id: i.to_string(),
                name: p
                    .attachment_name()
                    .map(sanitize_filename)
                    .unwrap_or_else(|| format!("attachment-{i}")),
                mime,
                size: p.decoded_len(),
                content_id: p.content_id().map(strip_angle),
                inline: p.is_inline(),
            }
        })
        .collect();

    ParsedMessage { headers, text, html, attachments }
}

const MAX_NAME_BYTES: usize = 200;

/// Invisible format characters (Unicode Cf and friends) incl. bidi overrides,
/// which can disguise an extension (`evil\u{202e}fdp.exe`).
fn is_format_char(c: char) -> bool {
    matches!(c,
        '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}'
        | '\u{e0000}'..='\u{e007f}')
}

fn truncate_bytes(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Strip path separators / control / bidi chars and cap the length (keeping
/// the extension) so a name is safe to save under.
fn sanitize_filename(n: &str) -> String {
    let s: String = n
        .chars()
        .filter(|c| !is_format_char(*c))
        .map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c })
        .collect();
    let mut s = s.trim().trim_start_matches('.').to_owned();
    if s.len() > MAX_NAME_BYTES {
        match s.rfind('.').filter(|i| s.len() - i <= 16) {
            Some(i) => {
                let ext = s[i..].to_owned();
                let stem = truncate_bytes(&s[..i], MAX_NAME_BYTES - ext.len()).to_owned();
                s = stem + &ext;
            }
            None => s = truncate_bytes(&s, MAX_NAME_BYTES).to_owned(),
        }
    }
    if s.is_empty() { "attachment".into() } else { s }
}

/// Decoded bytes of attachment `id` (as listed in `ParsedMessage::attachments`).
pub fn attachment_bytes(raw: &[u8], id: &str) -> Result<Vec<u8>> {
    let idx: usize = id.parse().map_err(|_| Error::Invalid("attachment id".into()))?;
    let m = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Parse("unparseable message".into()))?;
    let p = m
        .attachments()
        .nth(idx)
        .ok_or_else(|| Error::NotFound(format!("attachment {id}")))?;
    Ok(p.decoded().into_owned())
}

/// Short single-line preview of a text body.
pub fn make_snippet(text: &str) -> String {
    let mut out = String::new();
    for w in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
        if out.chars().count() >= 200 {
            break;
        }
    }
    out.chars().take(200).collect()
}

const MAX_INLINE_BYTES: usize = 5 * 1024 * 1024;
const MAX_INLINE_TOTAL: usize = 20 * 1024 * 1024;
const MAX_INLINE_COUNT: usize = 100;

/// Image types we will inline. The `data:` MIME is taken from this table,
/// never from the attacker's Content-Type text.
fn inline_image_mime(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/bmp" => "image/bmp",
        _ => return None,
    })
}

/// Sanitise the HTML body of `raw` and inline `cid:` images as `data:` URIs.
/// Returns `None` when the message has no HTML part.
pub fn render_html(raw: &[u8], allow_remote_images: bool) -> Result<Option<SanitizedHtml>> {
    render_html_with(raw, allow_remote_images, &std::collections::HashSet::new())
}

/// [`render_html`] that also drops remote images whose URL is in `deny_urls`
/// (see `tracking::tracker_url_set`).
pub fn render_html_with(raw: &[u8], allow_remote_images: bool, deny_urls: &std::collections::HashSet<String>) -> Result<Option<SanitizedHtml>> {
    let m = MessageParser::default()
        .parse(raw)
        .ok_or_else(|| Error::Parse("unparseable message".into()))?;
    let parsed = parsed_from(&m);
    let Some(html) = parsed.html else { return Ok(None) };
    let mut out = sanitize_html_with(&html, allow_remote_images, deny_urls);
    let parts: Vec<_> = m.attachments().collect();
    let (mut total, mut count) = (0usize, 0usize);
    for (a, part) in parsed.attachments.iter().zip(parts) {
        let Some(cid) = &a.content_id else { continue };
        let Some(mime) = inline_image_mime(&a.mime) else { continue };
        if a.size > MAX_INLINE_BYTES {
            continue;
        }
        // Matched ASCII-case-insensitively: senders are not consistent about the case of the reference vs the
        // Content-ID header (the sanitiser keeps the reference exactly as written).
        let needle = format!("src=\"cid:{cid}\"").to_ascii_lowercase();
        if !out.html.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        if count >= MAX_INLINE_COUNT || total + a.size > MAX_INLINE_TOTAL {
            break;
        }
        count += 1;
        total += a.size;
        let uri = format!(
            "src=\"data:{mime};base64,{}\"",
            base64::engine::general_purpose::STANDARD.encode(part.decoded())
        );
        out.html = replace_ascii_ci(&out.html, &needle, &uri);
    }
    Ok(Some(out))
}

/// Replace every ASCII-case-insensitive occurrence of `needle_lower` (already lower-case) in `hay`.
/// `to_ascii_lowercase` keeps byte offsets, so positions found in the lowered copy are valid in `hay`.
fn replace_ascii_ci(hay: &str, needle_lower: &str, with: &str) -> String {
    let low = hay.to_ascii_lowercase();
    let mut out = String::with_capacity(hay.len());
    let mut last = 0;
    for (i, _) in low.match_indices(needle_lower) {
        out.push_str(&hay[last..i]);
        out.push_str(with);
        last = i + needle_lower.len();
    }
    out.push_str(&hay[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALT: &str = "From: \"Ann Example\" <ann@example.org>\r\nTo: bob@example.net, Carl <carl@example.net>\r\nSubject: =?UTF-8?B?SGVsbG8g8J+MjQ==?=\r\nDate: Tue, 03 Jun 2025 10:00:00 +0000\r\nMessage-ID: <abc@example.org>\r\nIn-Reply-To: <prev@example.org>\r\nReferences: <root@example.org> <prev@example.org>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=XX\r\n\r\n--XX\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nplain body\r\n--XX\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>html <b>body</b></p>\r\n--XX--\r\n";

    #[test]
    fn multipart_alternative() {
        let p = parse_message(ALT.as_bytes()).unwrap();
        assert_eq!(p.headers.subject, "Hello \u{1F30D}");
        assert_eq!(p.headers.message_id.as_deref(), Some("abc@example.org"));
        assert_eq!(p.headers.in_reply_to, vec!["prev@example.org"]);
        assert_eq!(p.headers.references, vec!["root@example.org", "prev@example.org"]);
        assert_eq!(p.headers.from[0].name.as_deref(), Some("Ann Example"));
        assert_eq!(p.headers.from[0].address, "ann@example.org");
        assert_eq!(p.headers.to.len(), 2);
        assert_eq!(p.headers.date, Some(1748944800));
        assert_eq!(p.text.as_deref().unwrap().trim(), "plain body");
        assert!(p.html.as_deref().unwrap().contains("<b>body</b>"));
        assert!(p.attachments.is_empty());
    }

    #[test]
    fn text_only_has_no_html() {
        let p = parse_message(b"Subject: x\r\n\r\njust text\r\n").unwrap();
        assert!(p.html.is_none());
        assert_eq!(p.text.as_deref().unwrap().trim(), "just text");
    }

    #[test]
    fn html_only_derives_text() {
        let p = parse_message(b"Subject: x\r\nContent-Type: text/html\r\n\r\n<p>Hi <b>there</b></p>\r\n").unwrap();
        assert!(p.html.is_some());
        assert!(p.text.unwrap().contains("Hi"));
    }

    #[test]
    fn attachment_roundtrip() {
        let raw = "Subject: att\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=B\r\n\r\n--B\r\nContent-Type: text/plain\r\n\r\nsee attached\r\n--B\r\nContent-Type: application/pdf; name=\"../r\u{e9}sum\u{e9}.pdf\"\r\nContent-Disposition: attachment; filename=\"../report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nSGVsbG8gUERG\r\n--B--\r\n";
        let p = parse_message(raw.as_bytes()).unwrap();
        assert_eq!(p.attachments.len(), 1);
        let a = &p.attachments[0];
        assert_eq!(a.mime, "application/pdf");
        assert_eq!(a.size, 9);
        assert!(!a.name.contains('/'));
        assert!(!a.name.starts_with('.'));
        assert_eq!(attachment_bytes(raw.as_bytes(), &a.id).unwrap(), b"Hello PDF");
        assert!(attachment_bytes(raw.as_bytes(), "7").is_err());
        assert!(attachment_bytes(raw.as_bytes(), "x").is_err());
    }

    #[test]
    fn latin1_body_qp() {
        let raw = b"Subject: caf\r\nContent-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\ncaf=E9 cr=E8me\r\n";
        let p = parse_message(raw).unwrap();
        assert_eq!(p.text.unwrap().trim(), "caf\u{e9} cr\u{e8}me");
    }

    #[test]
    fn koi8r_raw_8bit_body() {
        let mut raw = b"Subject: x\r\nContent-Type: text/plain; charset=koi8-r\r\nContent-Transfer-Encoding: 8bit\r\n\r\n".to_vec();
        raw.extend_from_slice(&[0xF0, 0xD2, 0xC9, 0xD7, 0xC5, 0xD4]); // "Привет"
        let p = parse_message(&raw).unwrap();
        assert_eq!(p.text.unwrap().trim(), "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}");
    }

    #[test]
    fn encoded_word_subjects() {
        let h = parse_headers(b"Subject: =?ISO-8859-1?Q?Caf=E9_Cr=E8me?=\r\nFrom: =?UTF-8?Q?J=C3=BCrgen?= <j@example.de>").unwrap();
        assert_eq!(h.subject, "Caf\u{e9} Cr\u{e8}me");
        assert_eq!(h.from[0].name.as_deref(), Some("J\u{fc}rgen"));
    }

    #[test]
    fn headers_missing_fields_and_garbage_date() {
        let h = parse_headers(b"Subject: s\r\nDate: not a date\r\n\r\n").unwrap();
        assert_eq!(h.date, None);
        assert!(h.from.is_empty());
        assert!(h.message_id.is_none());
    }

    #[test]
    fn garbage_input_does_not_panic() {
        let _ = parse_message(b"");
        let _ = parse_message(&[0xff; 64]);
        let _ = parse_message(b"Content-Type: multipart/mixed; boundary=\r\n\r\n--\r\n");
        let _ = parse_headers(b"\x00\x01\x02");
    }

    #[test]
    fn snippet() {
        assert_eq!(make_snippet("  a \n\n b\t c "), "a b c");
        assert_eq!(make_snippet(&"word ".repeat(100)).chars().count(), 200);
    }

    #[test]
    fn cid_images_inlined_and_remote_blocked() {
        let raw = "Subject: i\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=R\r\n\r\n--R\r\nContent-Type: text/html\r\n\r\n<img src=\"cid:logo@x\"><img src=\"https://t.example/p.gif\">\r\n--R\r\nContent-Type: image/png\r\nContent-ID: <logo@x>\r\nContent-Disposition: inline\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--R--\r\n";
        let r = render_html(raw.as_bytes(), false).unwrap().unwrap();
        assert!(r.html.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""), "{}", r.html);
        assert!(!r.html.contains("t.example"));
        assert_eq!(r.blocked_remote_count, 1);
    }

    /// Outlook / Exchange Content-IDs carry upper-case hex (`image001.png@01DA2B3C.4D5E6F70`). The reference in the
    /// HTML uses the same case, so the image must still be inlined.
    #[test]
    fn tp_mixed_case_content_id_is_inlined() {
        let raw = "Subject: i\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=R\r\n\r\n--R\r\nContent-Type: text/html\r\n\r\n<p>Hi</p><img src=\"cid:image001.png@01DA2B3C.4D5E6F70\">\r\n--R\r\nContent-Type: image/png\r\nContent-ID: <image001.png@01DA2B3C.4D5E6F70>\r\nContent-Disposition: inline\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--R--\r\n";
        let r = render_html(raw.as_bytes(), false).unwrap().unwrap();
        assert!(r.html.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""), "{}", r.html);
        assert!(!r.html.contains("cid:"), "{}", r.html);
        // Reference and header disagree on case: still the same image.
        let raw = raw.replace("Content-ID: <image001.png@01DA2B3C.4D5E6F70>", "Content-ID: <IMAGE001.PNG@01da2b3c.4d5e6f70>");
        let r = render_html(raw.as_bytes(), false).unwrap().unwrap();
        assert!(r.html.contains("src=\"data:image/png;base64,iVBORw0KGgo=\"") && !r.html.contains("cid:"), "{}", r.html);
    }

    /// Several parts, one referenced twice, one unreferenced, one non-image: only referenced images are inlined,
    /// and nothing that is not an image ever becomes a `data:` URI.
    #[test]
    fn tp_cid_reuse_unreferenced_and_non_image_parts() {
        let raw = "Subject: i\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=R\r\n\r\n--R\r\nContent-Type: text/html\r\n\r\n<img src=\"cid:a@x\"><img src=\"cid:a@x\"><img src=\"cid:doc@x\"><img src=\"cid:missing@x\">\r\n--R\r\nContent-Type: image/gif\r\nContent-ID: <a@x>\r\nContent-Transfer-Encoding: base64\r\n\r\nR0lGODlh\r\n--R\r\nContent-Type: image/png\r\nContent-ID: <unused@x>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--R\r\nContent-Type: text/html\r\nContent-ID: <doc@x>\r\nContent-Disposition: attachment; filename=x.html\r\n\r\n<script>alert(1)</script>\r\n--R--\r\n";
        let r = render_html(raw.as_bytes(), false).unwrap().unwrap();
        assert_eq!(r.html.matches("src=\"data:image/gif;base64,R0lGODlh\"").count(), 2, "{}", r.html);
        assert!(!r.html.contains("image/png") && !r.html.contains("text/html") && !r.html.contains("<script"), "{}", r.html);
    }

    #[test]
    fn q03_hostile_content_type_cannot_inject_attributes() {
        let raw = "Subject: i\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=R\r\n\r\n--R\r\nContent-Type: text/html\r\n\r\n<img src=\"cid:logo@x\">\r\n--R\r\nContent-Type: image/png\" style=\"background:url(https://evil)\r\nContent-ID: <logo@x>\r\nContent-Disposition: inline\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--R\r\nContent-Type: image/svg+xml\r\nContent-ID: <svg@x>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--R--\r\n";
        let r = render_html(raw.as_bytes(), false).unwrap().unwrap();
        assert!(!r.html.contains("evil") && !r.html.contains("style"), "{}", r.html);
        assert!(!r.html.contains("data:"), "{}", r.html);
    }

    #[test]
    fn q10_inline_total_is_capped() {
        let big = "A".repeat(4 * 1024 * 1024 / 3 * 4); // ~4 MB decoded each
        let mut raw = String::from("Subject: i\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=R\r\n\r\n--R\r\nContent-Type: text/html\r\n\r\n");
        for i in 0..8 {
            raw.push_str(&format!("<img src=\"cid:c{i}\">"));
        }
        raw.push_str("\r\n");
        for i in 0..8 {
            raw.push_str(&format!("--R\r\nContent-Type: image/png\r\nContent-ID: <c{i}>\r\nContent-Transfer-Encoding: base64\r\n\r\n{big}\r\n"));
        }
        raw.push_str("--R--\r\n");
        let r = render_html(raw.as_bytes(), false).unwrap().unwrap();
        let inlined = r.html.matches("src=\"data:image/png").count();
        assert!((1..=5).contains(&inlined), "inlined {inlined}");
    }

    #[test]
    fn q11_filename_bidi_and_length() {
        let n = sanitize_filename("invoice\u{202e}fdp.exe");
        assert_eq!(n, "invoicefdp.exe");
        let long = format!("{}.pdf", "\u{e9}".repeat(500));
        let n = sanitize_filename(&long);
        assert!(n.len() <= 200 && n.ends_with(".pdf"), "{}", n.len());
        let n = sanitize_filename(&"x".repeat(1000));
        assert_eq!(n.len(), 200);
    }

    #[test]
    fn render_html_none_for_text() {
        assert!(render_html(b"Subject: x\r\n\r\nhi\r\n", false).unwrap().is_none());
    }

    #[test]
    fn render_with_deny_list_skips_trackers_keeps_clean() {
        let html = "<p>hi</p><img src=\"https://us1.list-manage.com/track/open.php?u=1&amp;id=2\" width=\"1\" height=\"1\"><img src=\"https://cdn.example/banner.png\" width=\"600\" height=\"200\">";
        let raw = format!("From: a@example.org\r\nTo: me@example.net\r\nSubject: x\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n");
        let deny = crate::tracking::tracker_url_set(html, &["me@example.net".to_string()]);
        assert_eq!(deny.len(), 1);
        let r = render_html_with(raw.as_bytes(), true, &deny).unwrap().unwrap();
        assert!(!r.html.contains("list-manage"), "{}", r.html);
        assert!(r.html.contains("https://cdn.example/banner.png"), "{}", r.html);
        assert_eq!((r.blocked_remote_count, r.skipped_tracker_count), (0, 1));
        let all = render_html(raw.as_bytes(), true).unwrap().unwrap();
        assert!(all.html.contains("list-manage"));
    }
}

/// Header fields the list view does not need but newsletter detection, auto-reply loop prevention
/// and threading do. All values are unfolded, whitespace-collapsed and capped at
/// [`EXTRA_HEADER_MAX`] chars. Empty string = header absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraHeaders {
    pub list_id: String,
    pub list_unsubscribe: String,
    pub auto_submitted: String,
    pub precedence: String,
    pub reply_to: String,
    pub authentication_results: String,
    pub in_reply_to: String,
    /// Space-separated message ids (without angle brackets).
    pub references: String,
    /// Raw `Cc` header (display form, unfolded). Rules and auto-reply need it; the list view does not.
    pub cc: String,
    /// Raw `Return-Path` header (the envelope sender the delivering server recorded); `<>` = null sender.
    pub return_path: String,
    /// Raw `List-Unsubscribe-Post` header (RFC 8058); `List-Unsubscribe=One-Click` enables the https one-click POST.
    pub list_unsubscribe_post: String,
    /// Header-only guess that the message has an attachment (Content-Type `multipart/mixed` or `application/*`).
    /// Refined from the full body once it is cached. Never stored back from the database.
    pub attachment_hint: bool,
    /// Set by storage only: the guard headers above were stored together with the summary (false = never
    /// stored, e.g. a row cached before schema v4 or a torn write). Never set by the parser.
    #[serde(default)]
    pub headers_ok: bool,
}

pub const EXTRA_HEADER_MAX: usize = 1000;

fn collapse(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(EXTRA_HEADER_MAX).collect()
}

fn ids_of(s: &str) -> String {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(a) = rest.find('<') {
        let Some(b) = rest[a..].find('>') else { break };
        let id = rest[a + 1..a + b].trim();
        if !id.is_empty() && !id.contains(char::is_whitespace) {
            out.push(id.to_string());
        }
        rest = &rest[a + b + 1..];
    }
    let joined = out.join(" ");
    joined.chars().take(EXTRA_HEADER_MAX).collect()
}

/// Header-only guess: does this Content-Type usually mean "has an attachment"?
fn attachment_hint(content_type: &str) -> bool {
    let ct = content_type.trim().to_ascii_lowercase();
    ct.starts_with("multipart/mixed") || (ct.starts_with("application/") && !ct.starts_with("application/pkcs7-mime") && !ct.starts_with("application/x-pkcs7-mime"))
}

/// Extract [`ExtraHeaders`] from a header block or a whole message (stops at the first blank line).
/// Never fails: unparseable input yields empty fields.
pub fn parse_extra_headers(raw: &[u8]) -> ExtraHeaders {
    let text = String::from_utf8_lossy(&raw[..raw.len().min(256 * 1024)]);
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some((_, v)) = fields.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
        } else if let Some((n, v)) = line.split_once(':') {
            fields.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let get = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()).unwrap_or("");
    ExtraHeaders {
        list_id: collapse(get("list-id")),
        list_unsubscribe: collapse(get("list-unsubscribe")),
        auto_submitted: collapse(get("auto-submitted")),
        precedence: collapse(get("precedence")),
        reply_to: collapse(get("reply-to")),
        authentication_results: collapse(get("authentication-results")),
        in_reply_to: ids_of(get("in-reply-to")),
        references: ids_of(get("references")),
        cc: collapse(get("cc")),
        return_path: collapse(get("return-path")),
        list_unsubscribe_post: collapse(get("list-unsubscribe-post")),
        attachment_hint: attachment_hint(get("content-type")),
        headers_ok: false,
    }
}

#[cfg(test)]
mod extra_header_tests {
    use super::*;

    #[test]
    fn extracts_and_unfolds() {
        let raw = b"From: a@x\r\nList-Id: <news.example.org>\r\nList-Unsubscribe: <mailto:u@x>,\r\n <https://x/u>\r\nAuto-Submitted: auto-generated\r\nPrecedence: bulk\r\nReply-To: Bob <b@x>\r\nAuthentication-Results: mx; dkim=pass\r\nIn-Reply-To: <p@x>\r\nReferences: <r1@x>\r\n <r2@x>\r\n\r\nList-Id: <body-must-be-ignored>\r\n";
        let h = parse_extra_headers(raw);
        assert_eq!(h.list_id, "<news.example.org>");
        assert_eq!(h.list_unsubscribe, "<mailto:u@x>, <https://x/u>");
        assert_eq!(h.auto_submitted, "auto-generated");
        assert_eq!(h.precedence, "bulk");
        assert_eq!(h.reply_to, "Bob <b@x>");
        assert_eq!(h.authentication_results, "mx; dkim=pass");
        assert_eq!(h.in_reply_to, "p@x");
        assert_eq!(h.references, "r1@x r2@x");
    }

    #[test]
    fn cc_and_return_path_are_kept() {
        let raw = b"Return-Path: <>\r\nCc: Al <al@x>,\r\n bo@y\r\nFrom: a@x\r\n\r\nCc: body@ignored\r\n";
        let h = parse_extra_headers(raw);
        assert_eq!(h.return_path, "<>");
        assert_eq!(h.cc, "Al <al@x>, bo@y");
        assert_eq!(parse_extra_headers(b"From: a@x\r\n\r\n").return_path, "");
    }

    #[test]
    fn unsubscribe_post_and_attachment_hint() {
        let raw = b"List-Unsubscribe: <https://x/u>\r\nList-Unsubscribe-Post: List-Unsubscribe=One-Click\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n";
        let h = parse_extra_headers(raw);
        assert_eq!(h.list_unsubscribe_post, "List-Unsubscribe=One-Click");
        assert!(h.attachment_hint);
        assert!(!parse_extra_headers(b"Content-Type: multipart/alternative; boundary=b\r\n\r\n").attachment_hint);
        assert!(parse_extra_headers(b"Content-Type: application/pdf\r\n\r\n").attachment_hint);
        assert!(!parse_extra_headers(b"Content-Type: application/pkcs7-mime\r\n\r\n").attachment_hint);
    }

    #[test]
    fn garbage_yields_empty() {
        assert_eq!(parse_extra_headers(b"\x00\xff\xfe nonsense"), ExtraHeaders::default());
        assert_eq!(parse_extra_headers(b""), ExtraHeaders::default());
    }
}
