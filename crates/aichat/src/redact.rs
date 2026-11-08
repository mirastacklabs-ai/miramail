//! Redaction applied to mail text before it is sent to the model: quoted replies and signatures
//! removed, links shortened to their host, optionally addresses masked. Pure functions; the consent
//! dialog's toggles choose which run.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedactOpts {
    pub strip_quotes: bool,
    pub shorten_links: bool,
    pub mask_addresses: bool,
}

impl Default for RedactOpts {
    /// Owner decision 6: strip quotes + signatures, shorten links, do not mask addresses.
    fn default() -> Self {
        RedactOpts { strip_quotes: true, shorten_links: true, mask_addresses: false }
    }
}

/// Stable placeholders for addresses within one turn: the same address always maps to the same `person-N`.
#[derive(Debug, Default)]
pub struct AddrMasker {
    map: HashMap<String, usize>,
}

impl AddrMasker {
    fn name(&mut self, addr: &str) -> String {
        let key = addr.to_ascii_lowercase();
        let n = self.map.len() + 1;
        let id = *self.map.entry(key).or_insert(n);
        format!("person-{id}")
    }
}

/// Apply the enabled redactions to a message body.
pub fn redact_body(text: &str, o: &RedactOpts, m: &mut AddrMasker) -> String {
    let mut t = text.to_string();
    if o.strip_quotes {
        t = strip_signature(&strip_quoted(&t));
    }
    if o.shorten_links {
        t = shorten_links(&t);
    }
    if o.mask_addresses {
        t = mask_addresses(&t, m);
    }
    t
}

/// Header-ish single-line fields (from / to / subject): links and addresses only.
pub fn redact_field(text: &str, o: &RedactOpts, m: &mut AddrMasker) -> String {
    let mut t = text.to_string();
    if o.shorten_links {
        t = shorten_links(&t);
    }
    if o.mask_addresses {
        t = mask_addresses(&t, m);
    }
    t
}

fn is_attribution(line: &str) -> bool {
    let l = line.trim();
    if l.len() > 300 {
        return false;
    }
    let lower = l.to_lowercase();
    (lower.starts_with("on ") && (lower.ends_with("wrote:") || lower.ends_with("wrote :")))
        || (lower.starts_with("le ") && (lower.ends_with("a écrit :") || lower.ends_with("a écrit:")))
        || (lower.starts_with("am ") && lower.ends_with("schrieb:"))
        || (lower.starts_with("el ") && lower.ends_with("escribió:"))
}

fn is_original_marker(line: &str) -> bool {
    let l = line.trim();
    let lower = l.to_lowercase();
    (l.starts_with("-----") && (lower.contains("original message") || lower.contains("forwarded message") || lower.contains("ursprüngliche nachricht")))
        || (l.len() >= 20 && l.chars().all(|c| c == '_'))
}

/// Drop `>` quoted lines and attribution lines; cut Outlook-style "Original Message" tails and
/// "From:/Sent:" header blocks.
pub fn strip_quoted(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        if t.starts_with('>') {
            i += 1;
            continue;
        }
        if is_original_marker(line) {
            break;
        }
        // "From: x\nSent: y" header block of an inlined original (Outlook)
        if t.to_lowercase().starts_with("from:")
            && lines.get(i + 1).is_some_and(|n| {
                let n = n.trim_start().to_lowercase();
                n.starts_with("sent:") || n.starts_with("date:") || n.starts_with("to:")
            })
            && lines.get(i + 2).is_some_and(|n| {
                let n = n.trim_start().to_lowercase();
                ["to:", "cc:", "subject:", "date:", "sent:"].iter().any(|p| n.starts_with(p))
            })
            && out.iter().any(|l| !l.trim().is_empty())
        {
            break;
        }
        if is_attribution(line) {
            i += 1;
            continue;
        }
        // attribution wrapped over two lines
        if line.trim().to_lowercase().starts_with("on ") && lines.get(i + 1).is_some_and(|n| is_attribution(&format!("On {}", n.trim()))) && !line.trim().ends_with(':') {
            i += 2;
            continue;
        }
        out.push(line);
        i += 1;
    }
    collapse_blank(&out.join("\n"))
}

fn collapse_blank(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blanks = 0;
    for l in s.lines() {
        if l.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(l.trim_end());
        out.push('\n');
    }
    out.trim().to_string()
}

/// Cut a trailing signature: the `-- ` delimiter (RFC 3676) near the end, or a "Sent from my ..."
/// footer.
pub fn strip_signature(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(40);
    let mut cut: Option<usize> = None;
    for (i, l) in lines.iter().enumerate().skip(start) {
        let t = l.trim_end();
        let lower = t.trim().to_lowercase();
        if t == "--" || lower.starts_with("sent from my ") || lower.starts_with("get outlook for ") || lower.starts_with("envoyé de mon ") {
            cut = Some(i);
            break;
        }
    }
    match cut {
        Some(i) => collapse_blank(&lines[..i].join("\n")),
        None => text.to_string(),
    }
}

fn url_end(s: &str) -> usize {
    s.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | ')' | ']' | '}' | '|')).unwrap_or(s.len())
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = auth.rsplit('@').next().unwrap_or("");
    let host = if host_port.starts_with('[') {
        host_port.split(']').next().map(|h| format!("{h}]"))?
    } else {
        host_port.split(':').next()?.to_string()
    };
    let host = host.trim_end_matches(['.', ',', ';', '!']).to_lowercase();
    (!host.is_empty() && host.len() <= 255).then_some(host)
}

/// Replace every `http(s)://...` link with `[link: host]`. Userinfo and paths/queries (where
/// tracking tokens live) are dropped; the host is what the user would judge a link by.
pub fn shorten_links(text: &str) -> String {
    replace_links(text, |url| match host_of(url) {
        Some(h) => format!("[link: {h}]"),
        None => "[link]".to_string(),
    })
}

/// Replace every `http(s)://...` link with whatever `f` returns for the link's text.
pub fn replace_links(text: &str, mut f: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let lower = rest.to_ascii_lowercase();
        let pos = match (lower.find("http://"), lower.find("https://")) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let Some(p) = pos else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..p]);
        let tail = &rest[p..];
        let mut end = url_end(tail);
        while end > 0 && matches!(tail.as_bytes()[end - 1], b'.' | b',' | b';' | b':' | b'!' | b'?') {
            end -= 1; // sentence punctuation after a link is not part of it
        }
        out.push_str(&f(&tail[..end]));
        rest = &tail[end..];
    }
    out
}

fn addr_char(c: char, local: bool) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '-' | '_') || (local && matches!(c, '+' | '%' | '\'' | '!' | '#' | '$' | '&' | '*' | '/' | '=' | '?' | '^' | '`' | '{' | '|' | '}' | '~'))
}

/// Replace email addresses with stable `person-N` placeholders.
pub fn mask_addresses(text: &str, m: &mut AddrMasker) -> String {
    replace_addresses(text, |a| m.name(a))
}

/// Replace every email address with whatever `f` returns for the address text.
pub fn replace_addresses(text: &str, mut f: impl FnMut(&str) -> String) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            // local part already emitted at the tail of `out`; domain follows
            let mut j = i + 1;
            while j < chars.len() && addr_char(chars[j], false) {
                j += 1;
            }
            let domain: String = chars[i + 1..j].iter().collect::<String>().trim_end_matches(['.', '-']).to_string();
            let local_len = out.chars().rev().take_while(|c| addr_char(*c, true)).count();
            if local_len > 0 && domain.contains('.') && !domain.starts_with('.') {
                let local: String = out.chars().rev().take(local_len).collect::<Vec<_>>().into_iter().rev().collect();
                let keep = out.chars().count() - local_len;
                out = out.chars().take(keep).collect();
                out.push_str(&f(&format!("{local}@{domain}")));
                i += 1 + domain.chars().count();
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_quoted_replies() {
        let t = "Thanks, see below.\n\nOn Mon, 2 Nov 2026 at 10:00, Bob <bob@x.org> wrote:\n> old text\n> more old\n\nBye";
        assert_eq!(strip_quoted(t), "Thanks, see below.\n\nBye");
        let outlook = "Sure.\n\n-----Original Message-----\nFrom: A\nSent: Monday\nOld stuff";
        assert_eq!(strip_quoted(outlook), "Sure.");
        let hdr = "Sure.\n\nFrom: A <a@x>\nSent: Monday\nTo: me\nSubject: hi\n\nold";
        assert_eq!(strip_quoted(hdr), "Sure.");
        let wrapped = "Yes\nOn Mon, 2 Nov 2026 at 10:00 AM, Somebody With A Long Name\n<s@x.org> wrote:\n> q";
        assert_eq!(strip_quoted(wrapped), "Yes");
        assert_eq!(strip_quoted("a\nFrom: me\nb"), "a\nFrom: me\nb", "a lone From: line stays");
        assert_eq!(strip_quoted("> only quote"), "");
    }

    #[test]
    fn strips_signatures() {
        assert_eq!(strip_signature("Hello\n\n-- \nAlice\nCEO"), "Hello");
        assert_eq!(strip_signature("Hello\nSent from my iPhone"), "Hello");
        assert_eq!(strip_signature("a -- b\nno signature"), "a -- b\nno signature");
        // a delimiter far from the end (older than 40 lines) is not a signature
        let long = format!("--\n{}", "x\n".repeat(60));
        assert!(strip_signature(&long).starts_with("--"));
    }

    #[test]
    fn shortens_links_to_host() {
        let t = "See https://user:pw@Track.Example.org:8443/a/b?utm=1&id=SECRET#frag and (http://plain.test/x). Also https://x.test.";
        let s = shorten_links(t);
        assert_eq!(s, "See [link: track.example.org] and ([link: plain.test]). Also [link: x.test].");
        assert!(!s.contains("SECRET") && !s.contains("pw"));
        assert_eq!(shorten_links("no links http:// here"), "no links [link] here");
        assert_eq!(shorten_links("HTTPS://UP.TEST/p"), "[link: up.test]");
    }

    #[test]
    fn masks_addresses_stably() {
        let mut m = AddrMasker::default();
        let t = "From Bob <bob@shop.test>, cc: alice+x@example.org; again bob@shop.test, not-an-address@ or @home, a@b";
        let s = mask_addresses(t, &mut m);
        assert_eq!(s, "From Bob <person-1>, cc: person-2; again person-1, not-an-address@ or @home, a@b");
        let mut m2 = AddrMasker::default();
        assert_eq!(mask_addresses("Bob@Shop.test", &mut m2), "person-1");
        assert_eq!(mask_addresses("BOB@shop.TEST", &mut m2), "person-1");
    }

    #[test]
    fn options_combine() {
        let t = "Hi bob@x.org https://x.org/a?t=1\n> quoted\n-- \nsig";
        let mut m = AddrMasker::default();
        let all = redact_body(t, &RedactOpts { strip_quotes: true, shorten_links: true, mask_addresses: true }, &mut m);
        assert_eq!(all, "Hi person-1 [link: x.org]");
        let none = redact_body(t, &RedactOpts { strip_quotes: false, shorten_links: false, mask_addresses: false }, &mut AddrMasker::default());
        assert_eq!(none, t);
        assert!(RedactOpts::default().strip_quotes && !RedactOpts::default().mask_addresses);
    }
}
