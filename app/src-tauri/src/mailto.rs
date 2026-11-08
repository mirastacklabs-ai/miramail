//! `mailto:` links (RFC 6068) -> a prefilled Compose. Strict on purpose: the URL comes from another app, so
//! anything that could smuggle a header (CR/LF in a header field), or that is absurdly large, is rejected.
//! Compose is only OPENED prefilled; nothing is ever sent without the user pressing Send.

use serde::Serialize;
use std::sync::Mutex;

pub const MAX_URL: usize = 100_000;
pub const MAX_BODY: usize = 64 * 1024;
pub const MAX_SUBJECT: usize = 998;
pub const MAX_RECIPIENTS: usize = 50;
const MAX_ADDR: usize = 254;

/// Same shape as the frontend's compose init (`workspace.openCompose`).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailtoDraft {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: String,
    pub in_reply_to: Option<()>,
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode (strict: a bad escape or invalid UTF-8 is an error; `+` stays a plus in mailto).
pub fn pct_decode(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let (h, l) = (b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex));
            match (h, l) {
                (Some(h), Some(l)) => out.push(h << 4 | l),
                _ => return Err("The link has a broken %-escape.".into()),
            }
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "The link isn't valid text.".to_string())
}

fn has_control(s: &str, allow_newlines: bool) -> bool {
    s.chars().any(|c| c.is_control() && !(allow_newlines && matches!(c, '\n' | '\r' | '\t')))
}

/// One bare addr-spec: no whitespace/control/angle brackets/quotes/commas, exactly one `@` with text both sides.
fn valid_addr(a: &str) -> bool {
    if a.is_empty() || a.len() > MAX_ADDR || a.chars().any(|c| c.is_whitespace() || c.is_control() || crate::textsafe::is_format_char(c) || "<>\"(),;:\\[]".contains(c)) {
        return false;
    }
    let mut parts = a.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    !local.is_empty() && domain.contains(|c: char| c.is_alphanumeric()) && !domain.starts_with('.') && !domain.ends_with('.') && !domain.contains("..")
}

fn addrs(decoded: &str, into: &mut Vec<String>) -> Result<(), String> {
    if has_control(decoded, false) {
        return Err("The link has a line break in an address.".into());
    }
    for a in decoded.split(',').map(str::trim).filter(|a| !a.is_empty()) {
        if !valid_addr(a) {
            return Err("The link has an address that doesn't look right.".into());
        }
        if into.len() >= MAX_RECIPIENTS {
            return Err("The link has too many recipients.".into());
        }
        into.push(a.to_string());
    }
    Ok(())
}

pub fn parse(url: &str) -> Result<MailtoDraft, String> {
    if url.len() > MAX_URL {
        return Err("The mailto link is too large.".into());
    }
    let url = url.trim();
    let rest = match url.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("mailto:") => &url[7..],
        _ => return Err("Not a mailto link.".into()),
    };
    // A fragment is never part of a mailto.
    let rest = rest.split('#').next().unwrap_or("");
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, q),
        None => (rest, ""),
    };
    let mut d = MailtoDraft::default();
    addrs(&pct_decode(path)?, &mut d.to)?;
    let (mut got_subject, mut got_body) = (false, false);
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = pct_decode(k)?.to_ascii_lowercase();
        let val = pct_decode(v)?;
        match key.as_str() {
            "to" => addrs(&val, &mut d.to)?,
            "cc" => addrs(&val, &mut d.cc)?,
            "bcc" => addrs(&val, &mut d.bcc)?,
            "subject" if !got_subject => {
                if has_control(&val, false) {
                    return Err("The link has a line break in the subject.".into());
                }
                if val.len() > MAX_SUBJECT {
                    return Err("The subject in the link is too long.".into());
                }
                d.subject = crate::textsafe::strip_format(&val);
                got_subject = true;
            }
            "body" if !got_body => {
                if val.len() > MAX_BODY {
                    return Err("The message text in the link is too large.".into());
                }
                if has_control(&val, true) {
                    return Err("The message text in the link has unsupported characters.".into());
                }
                d.text = crate::textsafe::strip_format(&val).replace("\r\n", "\n").replace('\r', "\n");
                got_body = true;
            }
            // Any other header (in-reply-to, references, from, x-*, ...) is ignored, never forwarded.
            _ => {}
        }
    }
    Ok(d)
}

/// Links that arrived before (or while) the window could take them; the frontend drains it.
#[derive(Default)]
pub struct Pending(Mutex<Vec<MailtoDraft>>);

impl Pending {
    /// Keeps at most 10 so a link-flooding app can't pile up unbounded state.
    pub fn push(&self, d: MailtoDraft) {
        let mut v = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if v.len() < 10 {
            v.push(d);
        }
    }
    pub fn take(&self) -> Vec<MailtoDraft> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Handle URLs the OS gave us (`RunEvent::Opened`): only valid mailto links are queued; the rest are ignored.
pub fn on_opened(app: &tauri::AppHandle, urls: &[tauri::Url]) {
    use tauri::{Emitter, Manager};
    let mut any = false;
    for u in urls.iter().filter(|u| u.scheme().eq_ignore_ascii_case("mailto")) {
        match parse(u.as_str()) {
            Ok(d) => {
                app.state::<Pending>().push(d);
                any = true;
            }
            Err(e) => {
                let _ = app.emit_to("main", "app://notice", e);
            }
        }
    }
    if any {
        crate::menu::show_main(app);
        let _ = app.emit_to("main", "app://mailto", ());
    }
}

/// The frontend drains queued links (also on startup, for a cold launch from a mailto link).
#[tauri::command]
pub fn take_pending_mailto(state: tauri::State<'_, Pending>) -> Vec<MailtoDraft> {
    state.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_address_and_fields() {
        let d = parse("mailto:Alice@Example.org,bob@x.io?cc=c@x.io&bcc=d@x.io&subject=Hello%20there&body=Line1%0D%0ALine2").unwrap();
        assert_eq!(d.to, vec!["Alice@Example.org", "bob@x.io"]);
        assert_eq!((d.cc, d.bcc), (vec!["c@x.io".into()], vec!["d@x.io".into()]));
        assert_eq!(d.subject, "Hello there");
        assert_eq!(d.text, "Line1\nLine2", "CRLF in the body becomes LF");
    }

    #[test]
    fn scheme_case_to_header_and_no_address() {
        let d = parse("MAILTO:?to=a@b.co&TO=c@d.co&Subject=x").unwrap();
        assert_eq!(d.to, vec!["a@b.co", "c@d.co"]);
        assert_eq!(d.subject, "x");
        assert!(parse("mailto:").unwrap().to.is_empty());
        assert!(parse("http://x.org").is_err());
        assert!(parse("mailtox:a@b.co").is_err());
    }

    #[test]
    fn plus_is_literal_and_unicode_decodes() {
        let d = parse("mailto:a@b.co?subject=a+b%20%C3%A9").unwrap();
        assert_eq!(d.subject, "a+b é");
    }

    #[test]
    fn bidi_and_format_characters_are_stripped() {
        let d = parse("mailto:a@b.co?subject=inv%E2%80%AEfdp&body=x%E2%80%8By%EF%BB%BF").unwrap();
        assert_eq!((d.subject.as_str(), d.text.as_str()), ("invfdp", "xy"));
        assert!(parse("mailto:a%E2%80%AE@b.co").is_err());
    }

    #[test]
    fn header_injection_is_rejected() {
        for u in [
            "mailto:a@b.co?subject=hi%0D%0ABcc:evil@x.io",
            "mailto:a@b.co?subject=hi%0ABcc:evil@x.io",
            "mailto:a@b.co?subject=hi%0Dx",
            "mailto:a@b.co%0D%0ABcc:evil@x.io",
            "mailto:a@b.co?cc=c@d.co%0D%0ABcc:evil@x.io",
            "mailto:a@b.co?to=x@y.co%0Az@y.co",
            "mailto:a@b.co?subject=nul%00byte",
            "mailto:a@b.co?body=nul%00byte",
            "mailto:a@b.co?subject=esc%1B%5B31m",
        ] {
            assert!(parse(u).is_err(), "must reject {u}");
        }
        // Raw (unencoded) CR/LF in the URL string are rejected too.
        assert!(parse("mailto:a@b.co?subject=hi\r\nBcc: evil@x.io").is_err());
        assert!(parse("mailto:a@b.co\n?subject=x").is_err());
    }

    #[test]
    fn dangerous_headers_are_ignored_not_forwarded() {
        let d = parse("mailto:a@b.co?from=ceo@x.io&in-reply-to=%3Cx%3E&x-evil=1&references=r").unwrap();
        assert_eq!(d, MailtoDraft { to: vec!["a@b.co".into()], ..Default::default() });
    }

    #[test]
    fn bad_addresses_rejected() {
        for u in [
            "mailto:not-an-address",
            "mailto:a@b@c.co",
            "mailto:%3Cx@y.co%3E",
            "mailto:a%20b@y.co",
            "mailto:a@y..co",
            "mailto:@y.co",
            "mailto:a@",
            "mailto:\"a\"@y.co",
            "mailto:a@y.co?cc=bad",
        ] {
            assert!(parse(u).is_err(), "must reject {u}");
        }
    }

    #[test]
    fn bad_escapes_and_invalid_utf8() {
        assert!(parse("mailto:a@b.co?subject=%zz").is_err());
        assert!(parse("mailto:a@b.co?subject=%4").is_err());
        assert!(parse("mailto:a@b.co?subject=%FF%FE").is_err());
    }

    #[test]
    fn size_limits() {
        let big = "x".repeat(MAX_BODY + 1);
        assert!(parse(&format!("mailto:a@b.co?body={big}")).is_err());
        assert!(parse(&format!("mailto:a@b.co?body={}", "y".repeat(MAX_BODY))).is_ok());
        assert!(parse(&format!("mailto:a@b.co?subject={}", "s".repeat(MAX_SUBJECT + 1))).is_err());
        assert!(parse(&format!("mailto:a@b.co?body={}", "z".repeat(MAX_URL))).is_err(), "whole URL capped");
        let many = (0..=MAX_RECIPIENTS).map(|i| format!("u{i}@x.io")).collect::<Vec<_>>().join(",");
        assert!(parse(&format!("mailto:{many}")).is_err());
    }

    #[test]
    fn first_subject_and_body_win_and_fragment_is_dropped() {
        let d = parse("mailto:a@b.co?subject=one&subject=two&body=b1&body=b2#frag").unwrap();
        assert_eq!((d.subject.as_str(), d.text.as_str()), ("one", "b1"));
    }

    #[test]
    fn wire_shape_matches_the_compose_init() {
        let v = serde_json::to_value(parse("mailto:a@b.co?subject=s").unwrap()).unwrap();
        assert_eq!(v["to"][0], "a@b.co");
        assert!(v["inReplyTo"].is_null());
        for k in ["cc", "bcc", "subject", "text"] {
            assert!(v.get(k).is_some(), "{k}");
        }
    }

    #[test]
    fn pending_queue_is_bounded_and_drains() {
        let p = Pending::default();
        for _ in 0..30 {
            p.push(MailtoDraft::default());
        }
        assert_eq!(p.take().len(), 10);
        assert!(p.take().is_empty());
    }
}
