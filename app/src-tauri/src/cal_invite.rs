//! Invite detection in received mail: the first `text/calendar` part or `.ics` attachment.
//! The content is untrusted: size-capped, parse failures are ignored, inbound Zoom props are dropped
//! (default `ParseOptions` inside `invite_from_ics`).

use crate::cal_state::CalState;
use calendar::{Invite, MAX_ICS_BYTES};
use mailcore::mime::AttachmentInfo;

/// Is this attachment entry an iCalendar part?
pub fn is_ics_part(a: &AttachmentInfo) -> bool {
    let m = a.mime.to_ascii_lowercase();
    m.starts_with("text/calendar") || m.starts_with("application/ics") || a.name.to_ascii_lowercase().ends_with(".ics")
}

/// `(attachment id, bytes)` of the first iCalendar part within the size cap. Oversized parts are skipped.
pub fn find_invite_part(raw: &[u8], atts: &[AttachmentInfo]) -> Option<(String, Vec<u8>)> {
    for a in atts.iter().filter(|a| is_ics_part(a)) {
        if a.size > MAX_ICS_BYTES {
            continue;
        }
        if let Ok(bytes) = mailcore::attachment_bytes(raw, &a.id) {
            if bytes.len() <= MAX_ICS_BYTES {
                return Some((a.id.clone(), bytes));
            }
        }
    }
    None
}

/// The header section of a message as unfolded `(name, value)` pairs (stops at the first blank line).
fn header_pairs(raw: &[u8]) -> Vec<(String, String)> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").or_else(|| raw.windows(2).position(|w| w == b"\n\n")).unwrap_or(raw.len());
    let text = String::from_utf8_lossy(&raw[..end.min(64 * 1024)]).into_owned();
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            out.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    out
}

fn domains_aligned(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().trim_matches('"').to_ascii_lowercase(), b.trim().trim_matches('"').to_ascii_lowercase());
    !a.is_empty() && (a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}")))
}

/// Did the receiving server vouch for the From domain? True only when the TOP-MOST `Authentication-Results`
/// header (the one the user's own mail server prepended) shows `dmarc=pass`, or `dkim=pass` with a signing
/// domain aligned with the From domain. A missing header, `none`, `fail`, `softfail`, `neutral`... all count as
/// NOT verified, so a provider that does not add the header simply gets the cautious behaviour.
pub fn sender_verified(raw: &[u8], from_addr: &str) -> bool {
    let Some(from_domain) = from_addr.rsplit('@').next().filter(|d| !d.is_empty() && from_addr.contains('@')) else { return false };
    let Some((_, ar)) = header_pairs(raw).into_iter().find(|(k, _)| k == "authentication-results") else { return false };
    // first segment is the authserv-id; the rest are `method=result prop.name=value ...`
    for seg in ar.split(';').skip(1) {
        let seg = seg.trim().to_ascii_lowercase();
        let prop = |name: &str| seg.split_whitespace().find_map(|t| t.strip_prefix(name).map(|v| v.trim_matches(|c| c == '"' || c == ';').to_string()));
        if seg.starts_with("dmarc=pass") && prop("header.from=").is_none_or(|d| domains_aligned(&d, from_domain)) {
            return true;
        }
        if seg.starts_with("dkim=pass") {
            let signer = prop("header.d=").or_else(|| prop("header.i=").map(|i| i.rsplit('@').next().unwrap_or("").to_string()));
            if signer.is_some_and(|d| domains_aligned(&d, from_domain)) {
                return true;
            }
        }
    }
    false
}

/// The invite carried by a message, for `get_message`. Never fails: anything unusable is `None`.
pub fn invite_for_message(st: &CalState, raw: &[u8], atts: &[AttachmentInfo], account: &str) -> Option<Invite> {
    let (id, bytes) = find_invite_part(raw, atts)?;
    let mut inv = calendar::itip::invite_from_ics(&bytes, &[account.to_string()], &id, false).ok().flatten()?;
    inv.already_in_calendar = st.db(|c| c.find_event(&inv.event.uid)).ok().flatten().is_some();
    inv.sender_unverified = mailcore::parse_message(raw)
        .ok()
        .and_then(|p| p.headers.from.first().map(|a| a.address.clone()))
        .is_none_or(|from| !sender_verified(raw, &from));
    Some(inv)
}

/// Re-parse the invite of `attachment_id` for `cal_import_invite` / `cal_respond_invite`, together with the
/// message's authenticated sender (its `From` address): that address, not anything inside the ICS, is what
/// `apply_*` checks against the organizer / attendee.
pub fn load_invite(st: &CalState, raw: &[u8], account: &str, attachment_id: &str) -> Result<(Invite, String), String> {
    let parsed = mailcore::parse_message(raw).map_err(|e| e.to_string())?;
    let sender = parsed.headers.from.first().map(|a| a.address.clone()).filter(|a| a.contains('@')).ok_or("This message has no sender address.")?;
    let part = parsed.attachments.iter().find(|a| a.id == attachment_id && is_ics_part(a)).ok_or("This message no longer contains the invitation.")?;
    if part.size > MAX_ICS_BYTES {
        return Err("That invitation is too large to open.".into());
    }
    let bytes = mailcore::attachment_bytes(raw, attachment_id).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_ICS_BYTES {
        return Err("That invitation is too large to open.".into());
    }
    let mut inv = calendar::itip::invite_from_ics(&bytes, &[account.to_string()], attachment_id, false)
        .ok()
        .flatten()
        .ok_or("This message doesn't contain a valid calendar invitation.")?;
    inv.already_in_calendar = st.db(|c| c.find_event(&inv.event.uid)).ok().flatten().is_some();
    inv.sender_unverified = !sender_verified(raw, &sender);
    Ok((inv, sender))
}

#[cfg(test)]
pub(crate) mod testmail {
    //! Mail fixtures shared by the invite tests (no real mailbox).
    pub fn crlf(s: &str) -> String {
        s.replace("\r\n", "\n").replace('\n', "\r\n")
    }

    pub fn ics(method: &str, organizer: &str, attendee: &str, extra: &str) -> String {
        crlf(&format!(
            "BEGIN:VCALENDAR\nVERSION:2.0\nPRODID:-//t//EN\nMETHOD:{method}\nBEGIN:VEVENT\nUID:evt1@corp.example\nDTSTAMP:20261101T100000Z\n\
             DTSTART:20261110T090000Z\nDTEND:20261110T093000Z\nSUMMARY:Sync\nSEQUENCE:0\nORGANIZER;CN=Boss:mailto:{organizer}\n\
             ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{attendee}\nATTENDEE;PARTSTAT=ACCEPTED:mailto:{organizer}\n{extra}END:VEVENT\nEND:VCALENDAR\n"
        ))
    }

    /// Google-style: multipart/alternative (text + text/calendar) inside multipart/mixed with an invite.ics attachment.
    pub fn google_style(from: &str, ics_body: &str) -> Vec<u8> {
        crlf(&format!(
            "From: Boss <{from}>\nTo: me@x.org\nSubject: Invitation: Sync\nMIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=B\n\n\
             --B\nContent-Type: multipart/alternative; boundary=A\n\n--A\nContent-Type: text/plain; charset=utf-8\n\nYou are invited.\n\
             --A\nContent-Type: text/calendar; charset=UTF-8; method=REQUEST\n\n{ics_body}--A--\n\
             --B\nContent-Type: application/ics; name=\"invite.ics\"\nContent-Disposition: attachment; filename=\"invite.ics\"\n\n{ics_body}--B--\n"
        ))
        .into_bytes()
    }

    /// Zoom-style: ics only as a named attachment of a generic type.
    /// `zoom_style` with an `Authentication-Results` header the user's own server would have prepended.
    pub fn with_auth_results(raw: Vec<u8>, ar: &str) -> Vec<u8> {
        let mut out = format!("Authentication-Results: mx.me.example; {ar}\r\n").into_bytes();
        out.extend(raw);
        out
    }

    pub fn zoom_style(from: &str, ics_body: &str) -> Vec<u8> {
        crlf(&format!(
            "From: {from}\nTo: me@x.org\nSubject: Zoom\nMIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=B\n\n\
             --B\nContent-Type: text/plain\n\nJoin.\n--B\nContent-Type: application/octet-stream; name=\"meeting.ics\"\n\
             Content-Disposition: attachment; filename=\"meeting.ics\"\n\n{ics_body}--B--\n"
        ))
        .into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::testmail::*;
    use super::*;
    use calendar::{Calendar, InviteMethod};
    use mailcore::InMemorySecretStore;
    use std::sync::Arc;

    fn st() -> CalState {
        CalState::new(Calendar::open_in_memory().unwrap(), Arc::new(InMemorySecretStore::new()))
    }

    fn detect(st: &CalState, raw: &[u8]) -> Option<Invite> {
        let p = mailcore::parse_message(raw).unwrap();
        invite_for_message(st, raw, &p.attachments, "me@x.org")
    }

    #[test]
    fn google_style_invite_is_found_with_my_status() {
        let raw = google_style("boss@corp.example", &ics("REQUEST", "boss@corp.example", "me@x.org", ""));
        let inv = detect(&st(), &raw).expect("invite");
        assert_eq!(inv.method, InviteMethod::Request);
        assert_eq!(inv.event.summary, "Sync");
        assert_eq!(inv.my_status, Some(calendar::AttendeeStatus::NeedsAction));
        assert!(!inv.already_in_calendar);
        assert!(!inv.attachment_id.is_empty());
    }

    #[test]
    fn zoom_style_ics_attachment_is_found_and_inbound_zoom_props_are_ignored() {
        let extra = "X-MIRAMAIL-ZOOM-ID:1\nX-MIRAMAIL-ZOOM-URL:https://evil.example/j/1\n";
        let raw = zoom_style("boss@corp.example", &ics("REQUEST", "boss@corp.example", "me@x.org", extra));
        let inv = detect(&st(), &raw).expect("invite");
        assert!(inv.event.zoom.is_none(), "inbound zoom props must never be trusted");
    }

    #[test]
    fn already_in_calendar_is_reported() {
        let s = st();
        let raw = google_style("boss@corp.example", &ics("REQUEST", "boss@corp.example", "me@x.org", ""));
        let inv = detect(&s, &raw).unwrap();
        let ev = calendar::CalEvent { calendar_id: DEFAULT.into(), ..inv.event.clone() };
        s.db(|c| c.upsert_event(&ev)).unwrap();
        assert!(detect(&s, &raw).unwrap().already_in_calendar);
    }

    #[test]
    fn oversized_ics_is_ignored() {
        let big = "X-BIG:".to_string() + &"a".repeat(MAX_ICS_BYTES + 10) + "\n";
        let raw = zoom_style("boss@corp.example", &ics("REQUEST", "boss@corp.example", "me@x.org", &big));
        assert!(detect(&st(), &raw).is_none());
    }

    #[test]
    fn unparsable_or_missing_ics_is_ignored() {
        let raw = zoom_style("boss@corp.example", "this is not a calendar\n");
        assert!(detect(&st(), &raw).is_none());
        let plain = b"From: a@b.c\r\nSubject: hi\r\n\r\nhello\r\n";
        assert!(detect(&st(), plain).is_none());
    }

    #[test]
    fn text_calendar_alternative_without_attachment_is_found() {
        let body = ics("REQUEST", "boss@corp.example", "me@x.org", "");
        let raw = testmail::crlf(&format!(
            "From: boss@corp.example\nTo: me@x.org\nSubject: s\nMIME-Version: 1.0\nContent-Type: multipart/alternative; boundary=A\n\n--A\nContent-Type: text/plain\n\nhi\n--A\nContent-Type: text/calendar; method=REQUEST\n\n{body}--A--\n"
        ))
        .into_bytes();
        assert!(detect(&st(), &raw).is_some());
    }

    const DEFAULT: &str = calendar::DEFAULT_CALENDAR_ID;

    #[test]
    fn load_invite_uses_the_from_header_as_sender_and_validates_the_part() {
        let s = st();
        let raw = zoom_style("boss@corp.example", &ics("REQUEST", "boss@corp.example", "me@x.org", ""));
        let p = mailcore::parse_message(&raw).unwrap();
        let id = p.attachments.iter().find(|a| is_ics_part(a)).unwrap().id.clone();
        let (inv, sender) = load_invite(&s, &raw, "me@x.org", &id).unwrap();
        assert_eq!(sender, "boss@corp.example");
        assert_eq!(inv.event.uid, "evt1@corp.example");
        // a non-ics attachment id / bogus id is refused
        assert!(load_invite(&s, &raw, "me@x.org", "99").is_err());
    }

    #[test]
    fn authentication_results_decide_sender_unverified() {
        let ics_body = ics("REQUEST", "boss@corp.example", "me@x.org", "");
        let plain = zoom_style("boss@corp.example", &ics_body);
        let id = mailcore::parse_message(&plain).unwrap().attachments.iter().find(|a| is_ics_part(a)).unwrap().id.clone();
        let flag = |raw: &[u8]| load_invite(&st(), raw, "me@x.org", &id).unwrap().0.sender_unverified;
        assert!(flag(&plain), "no Authentication-Results at all -> unverified");
        for (ar, verified) in [
            ("dmarc=pass header.from=corp.example", true),
            ("spf=pass smtp.mailfrom=x@corp.example; dkim=pass header.d=mail.corp.example; dmarc=none", true),
            ("dkim=pass header.d=evil.example", false), // signed by somebody else
            ("dkim=fail header.d=corp.example; dmarc=fail header.from=corp.example", false),
            ("spf=pass smtp.mailfrom=boss@corp.example", false), // SPF alone does not authenticate the From header
            ("dmarc=pass header.from=evil.example", false),
        ] {
            let raw = with_auth_results(plain.clone(), ar);
            assert_eq!(flag(&raw), !verified, "{ar}");
        }
        // only the top-most header counts: an injected one further down cannot vouch for the message
        let mut forged = with_auth_results(plain.clone(), "dmarc=fail header.from=corp.example");
        forged = String::from_utf8(forged).unwrap().replacen("\r\nSubject:", "\r\nAuthentication-Results: x; dmarc=pass header.from=corp.example\r\nSubject:", 1).into_bytes();
        assert!(flag(&forged));
        // get_message path sets the same flag
        let verified = with_auth_results(plain.clone(), "dmarc=pass header.from=corp.example");
        assert!(!detect(&st(), &verified).unwrap().sender_unverified);
        assert!(detect(&st(), &plain).unwrap().sender_unverified);
        // wire shape
        assert_eq!(serde_json::to_value(detect(&st(), &plain).unwrap()).unwrap()["senderUnverified"], true);
    }
}
