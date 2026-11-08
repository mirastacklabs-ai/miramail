//! `List-Unsubscribe` (RFC 2369) / `List-Unsubscribe-Post` (RFC 8058) parsing.
//!
//! The header value is attacker-controlled (it is whatever the sender wrote), and we act on it only after
//! the user confirms, so the parser is strict about what it will ever turn into an action:
//! * `mailto:` -> exactly one plain address; the whole query (subject, body, cc, bcc, ...) is ignored and
//!   the mail MiraMail sends carries a fixed subject and body, so the sender's header can't put words in
//!   the user's mouth;
//! * `https:` -> https only, no userinfo, default port only, a real multi-label DNS host (no IP literals,
//!   no `localhost`/`.local`/`.internal` and friends, no numeric-looking TLD), printable ASCII, <= 2000 chars;
//! * the one-click POST is offered only with an https target AND a `List-Unsubscribe-Post` value that is
//!   exactly `List-Unsubscribe=One-Click`.
//!
//! `http:` and every other scheme are ignored.

const MAX_URI: usize = 2000;
const MAX_ITEMS: usize = 8;
/// Fixed subject and body of every unsubscribe mail.
pub const UNSUB_SUBJECT: &str = "unsubscribe";
pub const UNSUB_BODY: &str = "unsubscribe";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailtoTarget {
    pub address: String,
}

impl MailtoTarget {
    pub fn subject(&self) -> &'static str {
        UNSUB_SUBJECT
    }
    pub fn body(&self) -> &'static str {
        UNSUB_BODY
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpsTarget {
    pub url: String,
    pub host: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnsubOptions {
    pub mailto: Option<MailtoTarget>,
    pub https: Option<HttpsTarget>,
    /// RFC 8058 one-click POST is possible (`https` is set and the Post header says One-Click).
    pub one_click: bool,
}

/// Parse the two header values (either may be empty).
pub fn parse_list_unsubscribe(value: &str, post: &str) -> UnsubOptions {
    let mut out = UnsubOptions::default();
    let mut rest = value;
    let mut items = 0;
    while let Some(a) = rest.find('<') {
        let Some(len) = rest[a + 1..].find('>') else { break };
        // A folded header may break a line inside the brackets (RFC 2369): drop the fold (line break plus its
        // indentation). A space within one line stays, and makes the URI invalid.
        let item_owned: String = rest[a + 1..a + 1 + len].split(['\r', '\n']).map(str::trim).collect();
        let item = item_owned.as_str();
        rest = &rest[a + 1 + len + 1..];
        items += 1;
        if items > MAX_ITEMS {
            break;
        }
        let lower = item.get(..8).map(|p| p.to_ascii_lowercase()).unwrap_or_default();
        if out.mailto.is_none() && lower.starts_with("mailto:") {
            out.mailto = parse_mailto(item);
        } else if out.https.is_none() && lower.starts_with("https://") {
            out.https = parse_https(item);
        }
    }
    out.one_click = out.https.is_some() && post.trim().eq_ignore_ascii_case("list-unsubscribe=one-click");
    out
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16)) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]); // RFC 6068: '+' is a literal plus in mailto, not a space
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A single plain mailbox: ASCII, one `@`, dotted domain. No display names, lists or quoted locals.
pub fn plain_address_ok(a: &str) -> bool {
    if a.is_empty() || a.len() > 254 {
        return false;
    }
    let Some((local, domain)) = a.split_once('@') else { return false };
    if local.is_empty() || local.len() > 64 || domain.contains('@') {
        return false;
    }
    let local_ok = local.chars().all(|c| c.is_ascii_alphanumeric() || "._%+-!#$&'*/=^`{|}~".contains(c)) && !local.starts_with('.') && !local.ends_with('.') && !local.contains("..");
    local_ok && host_ok(domain)
}

fn parse_mailto(item: &str) -> Option<MailtoTarget> {
    let body = &item[7..];
    let addr_raw = body.split_once('?').map_or(body, |x| x.0);
    let address = pct_decode(addr_raw);
    if !plain_address_ok(&address) {
        return None;
    }
    // Only the domain is case-insensitive; keep the local part exactly as the sender wrote it.
    let (local, domain) = address.split_once('@')?;
    Some(MailtoTarget { address: format!("{local}@{}", domain.to_ascii_lowercase()) })
}

/// A public-looking DNS name: >= 2 labels of `[a-z0-9-]`, TLD starts with a letter (so no IPv4, decimal or
/// hex number tricks), none of the usual private-network suffixes.
fn host_ok(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    if h.is_empty() || h.len() > 253 || h.ends_with('.') {
        return false;
    }
    let labels: Vec<&str> = h.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    for l in &labels {
        if l.is_empty() || l.len() > 63 || l.starts_with('-') || l.ends_with('-') || !l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            return false;
        }
    }
    let tld = labels[labels.len() - 1];
    if !tld.as_bytes()[0].is_ascii_lowercase() || tld.len() < 2 {
        return false;
    }
    const PRIVATE: [&str; 11] = ["localhost", "local", "internal", "lan", "home", "corp", "intranet", "private", "arpa", "test", "invalid"];
    !PRIVATE.contains(&tld) && h != "localhost"
}

fn parse_https(item: &str) -> Option<HttpsTarget> {
    if item.len() > MAX_URI || !item.bytes().all(|b| (0x21..0x7f).contains(&b)) || item.contains('\\') {
        return None;
    }
    let rest = &item[8..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() || authority.contains('@') || authority.starts_with('[') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, "443")) => h,
        Some(_) => return None,
        None => authority,
    };
    if !host_ok(host) {
        return None;
    }
    Some(HttpsTarget { url: item.to_string(), host: host.to_ascii_lowercase() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_header() {
        let o = parse_list_unsubscribe("<mailto:unsub@list.example.org?subject=unsubscribe%20me>, <https://list.example.org/u/abc?x=1>", "List-Unsubscribe=One-Click");
        let m = o.mailto.unwrap();
        assert_eq!((m.address.as_str(), m.subject(), m.body()), ("unsub@list.example.org", "unsubscribe", "unsubscribe"));
        let h = o.https.unwrap();
        assert_eq!((h.host.as_str(), h.url.as_str()), ("list.example.org", "https://list.example.org/u/abc?x=1"));
        assert!(o.one_click);
    }

    #[test]
    fn one_click_needs_post_header_and_https() {
        let v = "<https://a.example.com/u>";
        assert!(!parse_list_unsubscribe(v, "").one_click);
        assert!(!parse_list_unsubscribe(v, "List-Unsubscribe=One-Click-Not").one_click);
        assert!(!parse_list_unsubscribe(v, "x=List-Unsubscribe=One-Click").one_click);
        assert!(parse_list_unsubscribe(v, " list-unsubscribe=one-click ").one_click);
        assert!(!parse_list_unsubscribe("<mailto:a@b.example>", "List-Unsubscribe=One-Click").one_click, "no https target");
        assert!(parse_list_unsubscribe(v, "").https.is_some(), "https can still be opened in the browser");
    }

    #[test]
    fn mailto_ignores_whole_query() {
        let o = parse_list_unsubscribe("<mailto:u@x.example?cc=victim@y.example&bcc=v2@y.example&subject=Send%20me%20money&body=a%0Ab>", "");
        let m = o.mailto.unwrap();
        assert_eq!(m.address, "u@x.example");
        assert_eq!((m.subject(), m.body()), ("unsubscribe", "unsubscribe"), "sender-chosen text is never used");
    }

    #[test]
    fn plus_addressing_and_case() {
        let m = |v: &str| parse_list_unsubscribe(v, "").mailto.map(|m| m.address);
        assert_eq!(m("<mailto:googlegroups-manage+123+unsubscribe@googlegroups.com>").as_deref(), Some("googlegroups-manage+123+unsubscribe@googlegroups.com"));
        assert_eq!(m("<mailto:Bounce+AbC@Lists.Example.COM>").as_deref(), Some("Bounce+AbC@lists.example.com"), "only the domain is lowercased");
        assert_eq!(m("<mailto:a%2Bb@x.example>").as_deref(), Some("a+b@x.example"));
    }

    #[test]
    fn whitespace_inside_brackets_from_folding() {
        let o = parse_list_unsubscribe("<https://list.example.org/\r\n u/abc>, <mailto:u@\n x.example>", "");
        assert_eq!(o.https.unwrap().url, "https://list.example.org/u/abc");
        assert_eq!(o.mailto.unwrap().address, "u@x.example");
    }

    #[test]
    fn mailto_rejects_bad_addresses() {
        for bad in [
            "<mailto:a@b.example,c@d.example>", "<mailto:a@b.example;c@d.example>", "<mailto:>", "<mailto:nodomain>", "<mailto:a b@c.example>",
            "<mailto:a%0D%0ABcc:x@y.example@c.example>", "<mailto:\"a\"@c.example>", "<mailto:a@localhost>", "<mailto:a@c>", "<mailto:a@@c.example>",
            "<mailto:%3Cx@y.example%3E>", "<mailto:a@127.0.0.1>",
        ] {
            assert!(parse_list_unsubscribe(bad, "").mailto.is_none(), "{bad}");
        }
        let long = format!("<mailto:{}@x.example>", "a".repeat(100));
        assert!(parse_list_unsubscribe(&long, "").mailto.is_none());
    }

    #[test]
    fn https_rejects_dangerous_targets() {
        for bad in [
            "<http://a.example.com/u>", "<https://localhost/u>", "<https://127.0.0.1/u>", "<https://10.0.0.5/u>", "<https://[::1]/u>",
            "<https://0x7f.0.0.1/u>", "<https://2130706433/u>", "<https://user:pw@a.example.com/u>", "<https://a.example.com@evil.example/u>",
            "<https://printer.local/u>", "<https://nas.lan/u>", "<https://intranet/u>", "<https://a.example.com:8443/u>", "<https://a.example.com:80/u>",
            "<https://a..example.com/u>", "<https://-a.example.com/u>", "<https://a.example.com./u>", "<ftp://a.example.com/u>", "<javascript:alert(1)>",
            "<file:///etc/passwd>", "<https://a.example.com/u v>", "<https://a.example.com/\u{e9}>", "<https://a.example.com\\@evil.example/>", "<https://>",
            "<https://1.2.3.4.5/u>", "<https://exa_mple.com/u>",
        ] {
            let o = parse_list_unsubscribe(bad, "List-Unsubscribe=One-Click");
            assert!(o.https.is_none() && !o.one_click, "{bad}");
        }
        let long = format!("<https://a.example.com/{}>", "x".repeat(2100));
        assert!(parse_list_unsubscribe(&long, "").https.is_none());
        assert_eq!(parse_list_unsubscribe("<HTTPS://News.Example.COM:443/u>", "").https.unwrap().host, "news.example.com");
    }

    #[test]
    fn garbage_and_limits() {
        for v in ["", "<", ">", "<>", "no brackets https://a.example.com", "<<<<>>>>", "\u{0}<\u{1}>", "<mailto:", "<https://a.example.com"] {
            assert_eq!(parse_list_unsubscribe(v, ""), UnsubOptions::default(), "{v:?}");
        }
        let many = "<x>".repeat(100) + "<https://late.example.com/u>";
        assert!(parse_list_unsubscribe(&many, "").https.is_none(), "only the first items are considered");
        // first valid of each kind wins; an invalid first item does not hide a valid later one
        let o = parse_list_unsubscribe("<https://127.0.0.1/x>, <https://ok.example.com/a>, <https://other.example.com/b>", "");
        assert_eq!(o.https.unwrap().host, "ok.example.com");
    }

    #[test]
    fn percent_decoding_edges() {
        assert_eq!(pct_decode("a%20b+c%zz%4"), "a b+c%zz%4");
        assert_eq!(pct_decode("%"), "%");
        assert_eq!(pct_decode("%41"), "A");
    }
}
