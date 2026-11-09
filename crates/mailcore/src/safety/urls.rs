//! Hand-rolled URL host parsing for the safety checks (no `url` crate: it is not a dependency of this crate).
//!
//! Implements just what the checks need: scheme, userinfo, host (WHATWG-style IPv4 parsing so `2130706433`,
//! `0x7f.1` and `0177.0.0.1` are recognised as numeric addresses), IPv6 (via `std`), port, a Punycode decoder
//! and a small script classifier. Nothing here touches the network or DNS.

use std::net::{Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKind {
    Domain,
    /// A numeric IPv4 host. `canonical` = plain `a.b.c.d` with no leading zeros / hex / short forms.
    Ipv4 { addr: Ipv4Addr, canonical: bool },
    /// IPv6; `embedded_v4` is set for `::ffff:a.b.c.d` (IPv4-mapped) and NAT64 `64:ff9b::/96`.
    Ipv6 { addr: Ipv6Addr, embedded_v4: Option<Ipv4Addr> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub userinfo: Option<String>,
    /// Lower-cased host without brackets / trailing dot; percent-escapes decoded.
    pub host: String,
    pub port: Option<u16>,
    /// Everything after the authority (`/path?query#frag`), unchanged.
    pub rest: String,
    pub kind: HostKind,
    pub trailing_dot: bool,
    pub pct_host: bool,
}

fn is_special(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ftp" | "ws" | "wss")
}

/// Parse an absolute http(s)-style URL. `None` when it has no authority or the host is unusable.
pub fn parse_url(raw: &str) -> Option<Url> {
    // WHATWG: tabs / CR / LF are removed anywhere; leading/trailing C0 controls and spaces are trimmed.
    let cleaned: String = raw.chars().filter(|c| !matches!(c, '\t' | '\r' | '\n')).collect();
    let s = cleaned.trim_matches(|c: char| c <= ' ');
    let colon = s.find(':')?;
    let scheme = &s[..colon];
    if scheme.is_empty() || !scheme.chars().next()?.is_ascii_alphabetic() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    let scheme = scheme.to_ascii_lowercase();
    let after = &s[colon + 1..];
    let after = if is_special(&scheme) {
        after.trim_start_matches(['/', '\\'])
    } else {
        after.strip_prefix("//")?
    };
    let end = after.find(['/', '\\', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    let rest = after[end..].to_string();
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(authority[..i].to_string()), &authority[i + 1..]),
        None => (None, authority),
    };
    let (host_raw, port) = if let Some(inner) = hostport.strip_prefix('[') {
        let close = inner.find(']')?;
        let tail = &inner[close + 1..];
        let port = if tail.is_empty() {
            None
        } else {
            let p = tail.strip_prefix(':')?;
            if p.is_empty() { None } else { Some(p.parse::<u16>().ok()?) }
        };
        (format!("[{}]", &inner[..close]), port)
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h.to_string(), if p.is_empty() { None } else { Some(p.parse::<u16>().ok()?) }),
            Some(_) => return None,
            None => (hostport.to_string(), None),
        }
    };
    if host_raw.is_empty() || host_raw.chars().any(|c| c <= ' ' || c == '\u{7f}') {
        return None;
    }
    if let Some(inner) = host_raw.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        let addr: Ipv6Addr = inner.parse().ok()?;
        let seg = addr.segments();
        let embedded_v4 = addr.to_ipv4_mapped().or_else(|| {
            // NAT64 well-known prefix 64:ff9b::/96 embeds an IPv4 address in the last 32 bits.
            (seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0]).then(|| Ipv4Addr::new((seg[6] >> 8) as u8, seg[6] as u8, (seg[7] >> 8) as u8, seg[7] as u8))
        });
        return Some(Url { scheme, userinfo, host: inner.to_ascii_lowercase(), port, rest, kind: HostKind::Ipv6 { addr, embedded_v4 }, trailing_dot: false, pct_host: false });
    }
    let pct_host = host_raw.contains('%');
    let decoded = if pct_host { pct_decode(&host_raw) } else { host_raw.clone() };
    let mut host = decoded.to_lowercase();
    let trailing_dot = host.ends_with('.');
    if trailing_dot {
        host.pop();
    }
    if host.is_empty() || host.contains(['/', '\\', '?', '#', '@', ' ']) {
        return None;
    }
    let kind = match parse_ipv4(&host) {
        Ipv4Parse::Ip(addr, canonical) => HostKind::Ipv4 { addr, canonical },
        Ipv4Parse::Invalid => return None,
        Ipv4Parse::NotIp => HostKind::Domain,
    };
    Some(Url { scheme, userinfo, host, port, rest, kind, trailing_dot, pct_host })
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
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, PartialEq, Eq)]
enum Ipv4Parse {
    NotIp,
    Invalid,
    Ip(Ipv4Addr, bool),
}

fn parse_part(p: &str) -> Option<u64> {
    if p.is_empty() {
        return None;
    }
    let (digits, radix) = if let Some(h) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
        (h, 16)
    } else if p.len() > 1 && p.starts_with('0') {
        (&p[1..], 8)
    } else {
        (p, 10)
    };
    if digits.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(digits, radix).ok()
}

fn ends_in_number(p: &str) -> bool {
    !p.is_empty() && (p.chars().all(|c| c.is_ascii_digit()) || ((p.starts_with("0x") || p.starts_with("0X")) && p[2..].chars().all(|c| c.is_ascii_hexdigit())))
}

/// WHATWG "IPv4 parser": the host is a number when its LAST label is numeric (decimal, `0x` hex or leading-0 octal).
fn parse_ipv4(host: &str) -> Ipv4Parse {
    let mut parts: Vec<&str> = host.split('.').collect();
    if parts.len() > 1 && parts.last() == Some(&"") {
        parts.pop();
    }
    if !ends_in_number(parts.last().copied().unwrap_or("")) {
        return Ipv4Parse::NotIp;
    }
    if parts.len() > 4 {
        return Ipv4Parse::Invalid;
    }
    let mut nums = Vec::new();
    for p in &parts {
        match parse_part(p) {
            Some(n) => nums.push(n),
            None => return Ipv4Parse::Invalid,
        }
    }
    let n = nums.len();
    if nums[..n - 1].iter().any(|&x| x > 255) || nums[n - 1] >= 256u64.pow((5 - n) as u32) {
        return Ipv4Parse::Invalid;
    }
    let mut v = nums[n - 1];
    for (i, x) in nums[..n - 1].iter().enumerate() {
        v += x * 256u64.pow((3 - i) as u32);
    }
    let canonical = n == 4 && parts.iter().all(|p| p.parse::<u32>().is_ok_and(|x| x.to_string() == **p));
    Ipv4Parse::Ip(Ipv4Addr::from(v as u32), canonical)
}

// ------------------------------------------------------------------------------------------ domains

/// Last two labels (three under `co.uk`-style second levels). Same heuristic as `aichat::autoreply::reg_domain`;
/// no public-suffix list is bundled.
pub fn reg_domain(host: &str) -> String {
    let labels: Vec<&str> = host.trim().trim_end_matches('.').split('.').filter(|l| !l.is_empty()).collect();
    let n = labels.len();
    if n <= 2 {
        return labels.join(".").to_lowercase();
    }
    let take = if labels[n - 1].len() == 2 && matches!(labels[n - 2], "co" | "com" | "org" | "net" | "gov" | "ac" | "edu" | "or" | "ne") { 3 } else { 2 };
    labels[n - take..].join(".").to_lowercase()
}

/// Left-most label of the registrable domain (`paypal` for `www.paypal.co.uk`).
pub fn reg_label(host: &str) -> String {
    reg_domain(host).split('.').next().unwrap_or("").to_string()
}

pub fn domain_of_address(addr: &str) -> String {
    addr.trim().rsplit_once('@').map(|x| x.1).unwrap_or("").trim().trim_end_matches('.').to_lowercase()
}

// ------------------------------------------------------------------------------------------ punycode / scripts

fn adapt(mut delta: u32, numpoints: u32, first: bool) -> u32 {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    delta = if first { delta / 700 } else { delta / 2 };
    delta += delta / numpoints;
    let mut k = 0;
    while delta > ((BASE - TMIN) * TMAX) / 2 {
        delta /= BASE - TMIN;
        k += BASE;
    }
    k + (BASE - TMIN + 1) * delta / (delta + 38)
}

/// RFC 3492 decoder for one label body (the part after `xn--`). `None` on malformed input.
pub fn punycode_decode(input: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    if input.len() > 63 || !input.is_ascii() {
        return None;
    }
    let (mut out, rest): (Vec<char>, &str) = match input.rfind('-') {
        Some(i) => (input[..i].chars().collect(), &input[i + 1..]),
        None => (Vec::new(), input),
    };
    let digits = rest.as_bytes();
    let (mut n, mut i, mut bias) = (128u32, 0u32, 72u32);
    let mut p = 0;
    while p < digits.len() {
        let oldi = i;
        let (mut w, mut k) = (1u32, BASE);
        loop {
            let d = *digits.get(p)?;
            p += 1;
            let digit = match d {
                b'a'..=b'z' => d - b'a',
                b'A'..=b'Z' => d - b'A',
                b'0'..=b'9' => d - b'0' + 26,
                _ => return None,
            } as u32;
            i = i.checked_add(digit.checked_mul(w)?)?;
            let t = if k <= bias { TMIN } else if k >= bias + TMAX { TMAX } else { k - bias };
            if digit < t {
                break;
            }
            w = w.checked_mul(BASE - t)?;
            k += BASE;
        }
        let len = out.len() as u32 + 1;
        bias = adapt(i - oldi, len, oldi == 0);
        n = n.checked_add(i / len)?;
        i %= len;
        out.insert(i as usize, char::from_u32(n)?);
        i += 1;
    }
    Some(out.into_iter().collect())
}

/// The host with every `xn--` label decoded (undecodable labels are kept as written).
pub fn unicode_host(host: &str) -> String {
    host.split('.')
        .map(|l| match l.strip_prefix("xn--") {
            Some(body) => punycode_decode(body).unwrap_or_else(|| l.to_string()),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Script {
    Latin,
    Cyrillic,
    Greek,
    Armenian,
    Other,
    Common,
}

pub fn script_of(c: char) -> Script {
    match c {
        'a'..='z' | 'A'..='Z' | '\u{C0}'..='\u{24F}' | '\u{1E00}'..='\u{1EFF}' => Script::Latin,
        '\u{400}'..='\u{52F}' => Script::Cyrillic,
        '\u{370}'..='\u{3FF}' => Script::Greek,
        '\u{530}'..='\u{58F}' => Script::Armenian,
        '0'..='9' | '-' | '_' => Script::Common,
        _ => Script::Other,
    }
}

/// Does any single label mix Latin with Cyrillic / Greek / Armenian letters?
pub fn has_mixed_script(unicode_host: &str) -> bool {
    unicode_host.split('.').any(|label| {
        let mut seen: Vec<Script> = Vec::new();
        for c in label.chars() {
            let s = script_of(c);
            if matches!(s, Script::Common) {
                continue;
            }
            if !seen.contains(&s) {
                seen.push(s);
            }
        }
        seen.len() > 1
    })
}

/// Map look-alike non-Latin letters to the ASCII letter they imitate (a small subset of Unicode confusables;
/// not the full TR39 table). Non-mapped non-ASCII chars stay as they are.
fn fold_char(c: char) -> char {
    match c {
        'а' => 'a', 'е' => 'e', 'о' => 'o', 'р' => 'p', 'с' => 'c', 'х' => 'x', 'у' => 'y', 'і' => 'i', 'ј' => 'j',
        'ӏ' => 'l', 'ѕ' => 's', 'һ' => 'h', 'ԁ' => 'd', 'ԛ' => 'q', 'ԝ' => 'w', 'ɡ' => 'g', 'ı' => 'i', 'ɑ' => 'a',
        'ο' => 'o', 'ν' => 'v', 'α' => 'a', 'ι' => 'i', 'ρ' => 'p', 'τ' => 't', 'υ' => 'u', 'κ' => 'k', 'ℓ' => 'l',
        'А' => 'a', 'В' => 'b', 'Е' => 'e', 'К' => 'k', 'М' => 'm', 'Н' => 'h', 'О' => 'o', 'Р' => 'p', 'С' => 'c', 'Т' => 't', 'Х' => 'x',
        'Ο' => 'o', 'Α' => 'a', 'Ε' => 'e', 'Ι' => 'i', 'Κ' => 'k', 'Μ' => 'm', 'Ν' => 'n', 'Ρ' => 'p', 'Τ' => 't',
        c => c.to_lowercase().next().unwrap_or(c),
    }
}

/// Skeletons of a label: unicode look-alikes folded to ASCII, then the usual ASCII swaps (`rn`->`m`, `vv`->`w`,
/// `0`->`o`, `1`->`l` or `i`). Several variants, because `1` can stand for either `l` or `i`.
pub fn skeletons(label: &str) -> Vec<String> {
    let folded: String = label.chars().map(fold_char).collect();
    let base = folded.replace("rn", "m").replace("vv", "w").replace('0', "o");
    vec![base.replace('1', "l"), base.replace('1', "i")]
}

/// Levenshtein distance, bounded (returns `max + 1` when it exceeds `max`).
pub fn edit_distance(a: &str, b: &str, max: usize) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()].min(max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(u: &str) -> String {
        parse_url(u).unwrap().host
    }

    #[test]
    fn ipv4_obfuscations_are_recognised() {
        for (u, want, canon) in [
            ("http://2130706433/", "127.0.0.1", false),
            ("http://0x7f.1/", "127.0.0.1", false),
            ("http://0177.0.0.1/", "127.0.0.1", false),
            ("http://127.1/", "127.0.0.1", false),
            ("http://192.168.0.1/x", "192.168.0.1", true),
            ("http://0xA9FEA9FE/", "169.254.169.254", false),
            ("http://127.0.0.1./", "127.0.0.1", true),
        ] {
            match parse_url(u).unwrap().kind {
                HostKind::Ipv4 { addr, canonical } => {
                    assert_eq!(addr.to_string(), want, "{u}");
                    assert_eq!(canonical, canon, "{u}");
                }
                k => panic!("{u}: {k:?}"),
            }
        }
    }

    #[test]
    fn numeric_looking_domains_are_not_ips() {
        assert_eq!(parse_url("https://1password.com/").unwrap().kind, HostKind::Domain);
        assert_eq!(parse_url("https://123abc.example.com/").unwrap().kind, HostKind::Domain);
        assert!(parse_url("http://1.2.3.4.5/").is_none());
        assert!(parse_url("http://999.1.1.1/").is_none());
    }

    #[test]
    fn ipv6_and_embedded_ipv4() {
        let u = parse_url("http://[::ffff:127.0.0.1]/").unwrap();
        assert!(matches!(u.kind, HostKind::Ipv6 { embedded_v4: Some(a), .. } if a == Ipv4Addr::new(127, 0, 0, 1)));
        let u = parse_url("http://[::ffff:7f00:1]:8080/x").unwrap();
        assert!(matches!(u.kind, HostKind::Ipv6 { embedded_v4: Some(a), .. } if a == Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(u.port, Some(8080));
        let u = parse_url("http://[64:ff9b::a9fe:a9fe]/").unwrap();
        assert!(matches!(u.kind, HostKind::Ipv6 { embedded_v4: Some(a), .. } if a == Ipv4Addr::new(169, 254, 169, 254)));
        let u = parse_url("http://[2001:db8::1]/").unwrap();
        assert!(matches!(u.kind, HostKind::Ipv6 { embedded_v4: None, .. }));
        assert!(parse_url("http://[zzzz]/").is_none());
    }

    #[test]
    fn userinfo_trailing_dot_port_and_backslashes() {
        let u = parse_url("https://paypal.com@evil.tld/login").unwrap();
        assert_eq!(u.userinfo.as_deref(), Some("paypal.com"));
        assert_eq!(u.host, "evil.tld");
        let u = parse_url("https://evil.tld./x").unwrap();
        assert!(u.trailing_dot);
        assert_eq!(u.host, "evil.tld");
        assert_eq!(parse_url("https://a.test:8443/").unwrap().port, Some(8443));
        assert_eq!(host("https:\\\\a.test\\x"), "a.test");
        assert_eq!(host("HtTp://A.Test/"), "a.test");
        assert_eq!(host("ht\ttps://a.test/"), "a.test");
        assert!(parse_url("https://a.test:99999/").is_none());
    }

    #[test]
    fn non_urls_are_rejected() {
        for s in ["", "no scheme", "mailto:a@b.c", "javascript:alert(1)", "data:text/html;base64,AAAA", "file:///etc/passwd", "https://", "x-apple.systempreferences:"] {
            assert!(parse_url(s).is_none(), "{s}");
        }
    }

    #[test]
    fn percent_encoded_host_is_flagged() {
        let u = parse_url("http://ev%69l.test/").unwrap();
        assert!(u.pct_host);
        assert_eq!(u.host, "evil.test");
    }

    #[test]
    fn punycode_roundtrip_known_vectors() {
        assert_eq!(punycode_decode("bcher-kva").unwrap(), "bücher");
        assert_eq!(unicode_host("xn--bcher-kva.example"), "bücher.example");
        // xn--80ak6aa92e is the well-known all-Cyrillic "apple" look-alike.
        let u = unicode_host("xn--80ak6aa92e.com");
        assert!(u.starts_with("а"), "{u}");
        assert!(!has_mixed_script(&u));
        assert!(skeletons(u.split('.').next().unwrap()).contains(&"apple".to_string()), "{u}");
        assert!(punycode_decode("!!!").is_none());
    }

    #[test]
    fn mixed_script_and_skeletons() {
        assert!(has_mixed_script("pаypal.com")); // Cyrillic а inside Latin
        assert!(!has_mixed_script("paypal.com"));
        assert!(!has_mixed_script("bücher.de"));
        assert!(skeletons("pаypal").contains(&"paypal".to_string()));
        assert!(skeletons("paypa1").contains(&"paypal".to_string()));
        assert!(skeletons("rnicrosoft").contains(&"microsoft".to_string()));
        assert!(skeletons("g00gle").contains(&"google".to_string()));
    }

    #[test]
    fn reg_domain_and_distance() {
        assert_eq!(reg_domain("www.paypal.co.uk"), "paypal.co.uk");
        assert_eq!(reg_domain("a.b.example.com"), "example.com");
        assert_eq!(reg_label("login.paypal.com"), "paypal");
        assert_eq!(edit_distance("paypal", "paypai", 2), 1);
        assert_eq!(edit_distance("paypal", "google", 2), 3);
        assert_eq!(domain_of_address("A@Ex.COM."), "ex.com");
    }
}
