//! SMTP sending via lettre (tokio + rustls).

use crate::imap::ImapClient;
use crate::types::*;
use crate::{Error, Result};
use lettre::message::header::{ContentDisposition, ContentType, HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::time::Duration;

fn mbox(s: &str) -> Result<Mailbox> {
    s.trim().parse::<Mailbox>().map_err(|e| Error::Invalid(format!("address {s:?}: {e}")))
}

/// `<id>` if `id` is a plausible msg-id (printable ASCII, no spaces, brackets
/// or quotes), else None. Prevents header injection / malformed threading.
fn angle(id: &str) -> Option<String> {
    let core = id.trim().trim_start_matches('<').trim_end_matches('>');
    let ok = !core.is_empty()
        && core.len() <= 255
        && core.chars().all(|c| c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '\\' | ','));
    ok.then(|| format!("<{core}>"))
}

const MAX_REFERENCES: usize = 20;

/// (In-Reply-To, References) header values for a reply, or None if the
/// parent's Message-ID is unusable. References keep only the last 20.
pub fn reply_headers(parent: &ReplyTo) -> Option<(String, String)> {
    let in_reply_to = angle(&parent.message_id)?;
    let mut refs: Vec<String> = parent.references.iter().filter_map(|r| angle(r)).collect();
    if !refs.contains(&in_reply_to) {
        refs.push(in_reply_to.clone());
    }
    let skip = refs.len().saturating_sub(MAX_REFERENCES);
    Some((in_reply_to, refs[skip..].join(" ")))
}

/// "Re: x" unless already prefixed.
pub fn reply_subject(subject: &str) -> String {
    let t = subject.trim();
    if t.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("re:")) { t.to_owned() } else { format!("Re: {t}") }
}

/// `text/calendar; charset=utf-8; method=X` part. Only the three iTIP methods we send are accepted
/// (also keeps the header free of injected parameters).
fn calendar_part(c: &CalendarPart) -> Result<SinglePart> {
    let method = c.method.trim().to_ascii_uppercase();
    if !matches!(method.as_str(), "REQUEST" | "REPLY" | "CANCEL") {
        return Err(Error::Invalid("unsupported calendar method".into()));
    }
    let ct = ContentType::parse(&format!("text/calendar; charset=utf-8; method={method}")).map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(SinglePart::builder().header(ct).body(c.ics.clone()))
}

/// Header fields callers may add through `OutgoingMessage::extra_headers` (compared case-insensitively).
/// Deliberately small: nothing that changes who gets the mail or how it is parsed. `Reply-To` is set only from a
/// user-defined identity (the app resolves it from its own store, never from the frontend).
pub const EXTRA_HEADER_NAMES: [&str; 4] = ["Auto-Submitted", "Precedence", "X-Auto-Response-Suppress", "Reply-To"];

/// Validate one extra header: allow-listed name, value printable ASCII (no CR/LF), at most 200 chars.
pub fn check_extra_header(name: &str, value: &str) -> Result<(String, String)> {
    let canon = EXTRA_HEADER_NAMES
        .iter()
        .find(|n| n.eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| Error::Invalid(format!("header {name:?} is not allowed")))?;
    let v = value.trim();
    if v.is_empty() || v.len() > 200 || !v.chars().all(|c| c == ' ' || c.is_ascii_graphic()) {
        return Err(Error::Invalid(format!("invalid value for header {canon}")));
    }
    Ok(((*canon).to_string(), v.to_string()))
}

/// Build an RFC 5322 message (text, or text+html alternative, plus attachments).
pub fn build_message(from: &str, msg: &OutgoingMessage) -> Result<Message> {
    let extra: Vec<(String, String)> = msg.extra_headers.iter().map(|(n, v)| check_extra_header(n, v)).collect::<Result<_>>()?;
    let mut m = build_message_inner(from, msg)?;
    for (n, v) in extra {
        let name = HeaderName::new_from_ascii(n).map_err(|e| Error::Invalid(e.to_string()))?;
        m.headers_mut().insert_raw(HeaderValue::new(name, v));
    }
    Ok(m)
}

/// Build a message for the Drafts folder: like [`build_message`] but recipients may be missing, and
/// addresses that do not parse yet (half-typed) are left out instead of failing.
pub fn build_draft(from: &str, msg: &OutgoingMessage) -> Result<Message> {
    let keep = |l: &[String]| l.iter().filter(|a| mbox(a).is_ok()).cloned().collect::<Vec<_>>();
    let mut m = msg.clone();
    m.to = keep(&msg.to);
    m.cc = keep(&msg.cc);
    m.bcc = keep(&msg.bcc);
    m.extra_headers.clear();
    // lettre cannot build a message without a recipient: borrow the sender and drop the header again.
    let none = m.to.is_empty() && m.cc.is_empty() && m.bcc.is_empty();
    if none {
        m.to = vec![from.to_string()];
    }
    let mut built = build_message_inner_with(from, &m, false)?;
    if none {
        built.headers_mut().remove::<lettre::message::header::To>();
    }
    Ok(built)
}

fn build_message_inner(from: &str, msg: &OutgoingMessage) -> Result<Message> {
    build_message_inner_with(from, msg, true)
}

fn build_message_inner_with(from: &str, msg: &OutgoingMessage, need_rcpt: bool) -> Result<Message> {
    if need_rcpt && msg.to.is_empty() && msg.cc.is_empty() && msg.bcc.is_empty() {
        return Err(Error::Invalid("no recipients".into()));
    }
    let mut b = Message::builder().from(mbox(from)?).subject(msg.subject.clone());
    for a in &msg.to {
        b = b.to(mbox(a)?);
    }
    for a in &msg.cc {
        b = b.cc(mbox(a)?);
    }
    for a in &msg.bcc {
        b = b.bcc(mbox(a)?);
    }
    if let Some((irt, refs)) = msg.in_reply_to.as_ref().and_then(reply_headers) {
        b = b.in_reply_to(irt).references(refs);
    }
    b = b.message_id(None);

    let plain = || SinglePart::plain(msg.text.clone());
    // multipart/alternative: text [+ html] + optional text/calendar; None when the body is plain text only.
    let alt: Option<MultiPart> = match (&msg.html, &msg.calendar) {
        (None, None) => None,
        (Some(h), None) => Some(MultiPart::alternative_plain_html(msg.text.clone(), h.clone())),
        (html, Some(c)) => {
            let mut a = MultiPart::alternative().singlepart(plain());
            if let Some(h) = html {
                a = a.singlepart(SinglePart::html(h.clone()));
            }
            Some(a.singlepart(calendar_part(c)?))
        }
    };
    if msg.attachments.is_empty() {
        // No multipart/mixed wrapper unless something is actually attached.
        return match alt {
            Some(a) => b.multipart(a),
            None => b.singlepart(plain()),
        }
        .map_err(|e| Error::Smtp(e.to_string()));
    }
    let mut root = match alt {
        Some(a) => MultiPart::mixed().multipart(a),
        None => MultiPart::mixed().singlepart(plain()),
    };
    for a in &msg.attachments {
        let ct = ContentType::parse(&a.mime)
            .or_else(|_| ContentType::parse("application/octet-stream"))
            .map_err(|e| Error::Invalid(e.to_string()))?;
        root = root.singlepart(
            SinglePart::builder()
                .header(ct)
                .header(ContentDisposition::attachment(&a.name))
                .body(a.data.clone()),
        );
    }
    b.multipart(root).map_err(|e| Error::Smtp(e.to_string()))
}

fn transport(cfg: &ServerConfig, username: &str, password: &str) -> Result<AsyncSmtpTransport<Tokio1Executor>> {
    let builder = match cfg.tls {
        Tls::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host),
        Tls::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host),
    }
    .map_err(|e| Error::Smtp(e.to_string()))?;
    Ok(builder
        .port(cfg.port)
        .credentials(Credentials::new(username.to_owned(), password.to_owned()))
        .timeout(Some(Duration::from_secs(30)))
        .build())
}

/// Send `msg`; returns the exact bytes sent (for a Sent-folder copy).
pub async fn send(account: &AccountConfig, password: &str, msg: &OutgoingMessage) -> Result<Vec<u8>> {
    let m = build_message(&account.email, msg)?;
    let raw = m.formatted();
    let t = transport(&account.smtp, &account.username, password)?;
    t.send(m).await.map_err(classify_send_error)?;
    Ok(raw)
}

/// A server reply (4xx / 5xx) proves the message was not accepted. So does a failure before any message data was
/// offered: connect / DNS failure (lettre "Connection error") and TLS handshake failure. Those are safe to retry
/// (`Smtp`). Anything else (timeout, dropped connection mid-session) may have happened during/after DATA, so the
/// message may have been delivered (`SmtpUnknown`).
fn classify_send_error(e: lettre::transport::smtp::Error) -> Error {
    let pre_data = e.is_permanent() || e.is_transient() || e.is_tls() || e.to_string().starts_with("Connection error");
    if pre_data {
        Error::Smtp(e.to_string())
    } else {
        Error::SmtpUnknown(e.to_string())
    }
}

/// Send, then APPEND a copy to `sent_folder` over IMAP. Pass `append_copy =
/// false` for servers that save sent mail themselves (check with your
/// provider list) to avoid duplicates. A failed append is returned as
/// `Ok(Some(err))` because the mail HAS been sent.
pub async fn send_and_save(
    account: &AccountConfig,
    password: &str,
    msg: &OutgoingMessage,
    imap: &mut ImapClient,
    sent_folder: &str,
    append_copy: bool,
) -> Result<Option<Error>> {
    let raw = send(account, password, msg).await?;
    if append_copy {
        return Ok(imap.append(sent_folder, &raw, true).await.err());
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mime::parse_message;

    fn out() -> OutgoingMessage {
        OutgoingMessage {
            to: vec!["Bob <bob@example.net>".into()],
            cc: vec![],
            bcc: vec!["hidden@example.net".into()],
            subject: "Re: caf\u{e9}".into(),
            text: "hello".into(),
            html: None,
            attachments: vec![],
            in_reply_to: None,
            calendar: None,
            extra_headers: vec![],
        }
    }

    #[tokio::test]
    async fn unreachable_host_is_a_retryable_failure_not_unknown() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let acct = AccountConfig {
            email: "me@example.org".into(),
            imap: ServerConfig { host: "127.0.0.1".into(), port: 1, tls: Tls::Implicit },
            smtp: ServerConfig { host: "127.0.0.1".into(), port, tls: Tls::StartTls },
            username: "me".into(),
        };
        let err = send(&acct, "pw", &out()).await.unwrap_err();
        assert!(matches!(err, Error::Smtp(_)), "{err:?}");
    }

    #[test]
    fn extra_headers_are_added_and_validated() {
        let mut o = out();
        o.extra_headers = vec![("auto-submitted".into(), "auto-replied".into())];
        let raw = build_message("me@example.org", &o).unwrap().formatted();
        let s = String::from_utf8_lossy(&raw);
        assert!(s.contains("Auto-Submitted: auto-replied\r\n"), "{s}");
        for (n, v) in [("To", "x@y.z"), ("Bcc", "x@y.z"), ("Auto-Submitted", "a\r\nBcc: e@x"), ("Auto-Submitted", ""), ("Auto-Submitted", "caf\u{e9}"), ("X-Evil", "1")] {
            o.extra_headers = vec![(n.into(), v.into())];
            assert!(build_message("me@example.org", &o).is_err(), "{n}: {v:?}");
        }
    }

    #[test]
    fn reply_to_header_is_allowed_for_identities() {
        let mut o = out();
        o.extra_headers = vec![("reply-to".into(), "team@example.org".into())];
        let raw = build_message("me@example.org", &o).unwrap().formatted();
        assert!(String::from_utf8_lossy(&raw).contains("Reply-To: team@example.org\r\n"));
        o.extra_headers = vec![("Reply-To".into(), "a@b.c\r\nBcc: e@x.y".into())];
        assert!(build_message("me@example.org", &o).is_err());
    }

    #[test]
    fn calendar_part_is_an_alternative_with_method() {
        let mut o = out();
        o.calendar = Some(CalendarPart { method: "reply".into(), ics: "BEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n".into() });
        let raw = build_message("me@example.org", &o).unwrap().formatted();
        let s = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        assert!(s.contains("multipart/alternative"), "{s}");
        assert!(s.contains("text/calendar") && s.contains("method=reply"), "{s}");
        let p = parse_message(&raw).unwrap();
        assert_eq!(p.text.unwrap().trim(), "hello");
        assert!(p.attachments.iter().any(|a| a.mime.starts_with("text/calendar")));
        // html + calendar + attachment still builds
        o.html = Some("<p>x</p>".into());
        o.attachments = vec![OutgoingAttachment { name: "a.txt".into(), mime: "text/plain".into(), data: b"D".to_vec() }];
        assert!(build_message("me@example.org", &o).is_ok());
    }

    #[test]
    fn calendar_method_is_validated() {
        let mut o = out();
        o.calendar = Some(CalendarPart { method: "REPLY\r\nBcc: x@y.z".into(), ics: String::new() });
        assert!(build_message("me@example.org", &o).is_err());
        o.calendar = Some(CalendarPart { method: "PUBLISH".into(), ics: String::new() });
        assert!(build_message("me@example.org", &o).is_err());
    }

    #[test]
    fn plain_message_roundtrips_through_parser() {
        let m = build_message("Me <me@example.org>", &out()).unwrap();
        let raw = m.formatted();
        let p = parse_message(&raw).unwrap();
        assert_eq!(p.headers.subject, "Re: caf\u{e9}");
        assert_eq!(p.headers.from[0].address, "me@example.org");
        assert_eq!(p.headers.to[0].address, "bob@example.net");
        assert!(p.headers.message_id.is_some());
        assert!(p.headers.date.is_some());
        assert_eq!(p.text.unwrap().trim(), "hello");
        let s = String::from_utf8_lossy(&raw).to_ascii_lowercase();
        assert!(!s.contains("hidden@example.net"), "Bcc must not appear in headers");
        assert_eq!(m.envelope().to().len(), 2, "Bcc still in envelope");
    }

    #[test]
    fn reply_sets_threading_headers() {
        let mut o = out();
        o.in_reply_to = Some(ReplyTo { message_id: "p3@x".into(), references: vec!["p1@x".into(), "<p2@x>".into()] });
        let raw = build_message("me@example.org", &o).unwrap().formatted();
        let p = parse_message(&raw).unwrap();
        assert_eq!(p.headers.in_reply_to, vec!["p3@x"]);
        assert_eq!(p.headers.references, vec!["p1@x", "p2@x", "p3@x"]);
    }

    #[test]
    fn reply_headers_no_duplicate_parent() {
        let (irt, refs) = reply_headers(&ReplyTo { message_id: "<b@x>".into(), references: vec!["a@x".into(), "b@x".into()] }).unwrap();
        assert_eq!(irt, "<b@x>");
        assert_eq!(refs, "<a@x> <b@x>");
        let (_, refs) = reply_headers(&ReplyTo { message_id: "a@x".into(), references: vec![] }).unwrap();
        assert_eq!(refs, "<a@x>");
    }

    #[test]
    fn q13_msgid_validation_and_reference_cap() {
        assert!(reply_headers(&ReplyTo { message_id: "a b@x".into(), references: vec![] }).is_none());
        assert!(reply_headers(&ReplyTo { message_id: "a@x>\r\nBcc: e@x".into(), references: vec![] }).is_none());
        let refs: Vec<String> = (0..50).map(|i| format!("r{i}@x")).collect();
        let (_, h) = reply_headers(&ReplyTo { message_id: "p@x".into(), references: refs.clone() }).unwrap();
        let v: Vec<&str> = h.split(' ').collect();
        assert_eq!(v.len(), 20);
        assert_eq!(v[19], "<p@x>");
        assert_eq!(v[0], "<r31@x>");
        let (_, h) = reply_headers(&ReplyTo { message_id: "p@x".into(), references: vec!["bad id".into(), "ok@x".into()] }).unwrap();
        assert_eq!(h, "<ok@x> <p@x>");
    }

    #[test]
    fn q14_plain_mail_is_single_part() {
        let raw = String::from_utf8_lossy(&build_message("me@example.org", &out()).unwrap().formatted()).to_ascii_lowercase();
        assert!(!raw.contains("multipart"), "{raw}");
        assert!(raw.contains("text/plain"));
    }

    #[test]
    fn subject_prefix() {
        assert_eq!(reply_subject("Hi"), "Re: Hi");
        assert_eq!(reply_subject("RE: Hi"), "RE: Hi");
        assert_eq!(reply_subject(" re: Hi "), "re: Hi");
        // Q06: non-ASCII near the 3-byte boundary must not panic.
        assert_eq!(reply_subject("\u{1F600}\u{1F600} party"), "Re: \u{1F600}\u{1F600} party");
        assert_eq!(reply_subject("a\u{e9}"), "Re: a\u{e9}");
        assert_eq!(reply_subject("\u{e9}"), "Re: \u{e9}");
    }

    #[test]
    fn html_and_attachments() {
        let mut o = out();
        o.html = Some("<p>hi</p>".into());
        o.attachments = vec![OutgoingAttachment { name: "a.txt".into(), mime: "text/plain".into(), data: b"DATA".to_vec() }];
        let raw = build_message("me@example.org", &o).unwrap().formatted();
        let p = parse_message(&raw).unwrap();
        assert!(p.html.unwrap().contains("<p>hi</p>"));
        assert_eq!(p.text.unwrap().trim(), "hello");
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].name, "a.txt");
        assert_eq!(crate::mime::attachment_bytes(&raw, &p.attachments[0].id).unwrap(), b"DATA");
    }

    #[test]
    fn rejects_bad_input() {
        let mut o = out();
        o.to = vec![];
        o.bcc = vec![];
        assert!(build_message("me@example.org", &o).is_err());
        let mut o = out();
        o.to = vec!["not an address".into()];
        assert!(build_message("me@example.org", &o).is_err());
        assert!(build_message("garbage", &out()).is_err());
    }

    #[test]
    fn header_injection_is_not_possible() {
        let mut o = out();
        o.subject = "x\r\nBcc: evil@example.com".into();
        let raw = build_message("me@example.org", &o).unwrap().formatted();
        let s = String::from_utf8_lossy(&raw);
        assert!(!s.contains("\r\nBcc: evil"), "{s}");
    }
}
