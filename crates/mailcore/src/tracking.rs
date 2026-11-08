//! Remote-image tracker analysis (advisory).
//!
//! IMPORTANT: this is static heuristics over the RAW HTML. It cannot observe what a
//! server does with a request, so a "clean" verdict is not a guarantee and a "tracker"
//! verdict is a strong hint, not proof. Nothing here touches the network.
//!
//! Hash-of-address detection (md5/sha1/sha256 of the recipient) is intentionally NOT
//! implemented: none of those crates is a mailcore dependency and we do not add one for it.

use serde::Serialize;
use std::collections::HashSet;
use std::sync::OnceLock;

const MAX_IMAGES: usize = 500;
const MAX_URL_ANALYSED: usize = 8 * 1024;
/// Max bytes scanned for the end of one tag (bounds work on unterminated tags).
const MAX_TAG_BYTES: usize = 32 * 1024;
/// Max `<img` candidates examined, remote or not (bounds total work on hostile input).
const MAX_TAG_ATTEMPTS: usize = 5000;
const DISPLAY_URL_CHARS: usize = 300;
const TRACKER_DATA: &str = include_str!("../data/trackers.toml");

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    pub url: String,
    pub host: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub hidden: bool,
    pub verdict: &'static str,
    pub reasons: Vec<String>,
    pub tracker: Option<String>,
}

// ---------------------------------------------------------------- data

struct Rule {
    name: String,
    host_suffix: Option<String>,
    host_contains: Option<String>,
    path: Option<String>,
}
struct Generic {
    contains: String,
    reason: String,
}
struct Db {
    rules: Vec<Rule>,
    generic: Vec<Generic>,
}

fn parse_db(src: &str) -> Db {
    let mut db = Db { rules: vec![], generic: vec![] };
    let mut cur: Option<(bool, Vec<(String, String)>)> = None; // (is_rule, kv)
    fn flush(db: &mut Db, cur: &mut Option<(bool, Vec<(String, String)>)>) {
        let Some((is_rule, kv)) = cur.take() else { return };
        let get = |k: &str| kv.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        if is_rule {
            if let Some(name) = get("name") {
                let r = Rule { name, host_suffix: get("host_suffix"), host_contains: get("host_contains"), path: get("path") };
                if r.host_suffix.is_some() || r.host_contains.is_some() || r.path.is_some() {
                    db.rules.push(r);
                }
            }
        } else if let (Some(contains), Some(reason)) = (get("contains"), get("reason")) {
            db.generic.push(Generic { contains, reason });
        }
    }
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[rule]]" || line == "[[generic]]" {
            flush(&mut db, &mut cur);
            cur = Some((line == "[[rule]]", vec![]));
        } else if let (Some((k, v)), Some((_, kv))) = (line.split_once('='), cur.as_mut()) {
            let v = v.trim();
            if let Some(v) = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                kv.push((k.trim().to_string(), v.to_string()));
            }
        }
    }
    flush(&mut db, &mut cur);
    db
}

fn db() -> &'static Db {
    static DB: OnceLock<Db> = OnceLock::new();
    DB.get_or_init(|| parse_db(TRACKER_DATA))
}

// ---------------------------------------------------------------- scanner

/// One `<img>` found in raw HTML: decoded `src` and the attributes we use.
struct RawImg {
    src: String,
    width: Option<String>,
    height: Option<String>,
    style: Option<String>,
    hidden_attr: bool,
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'&' {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let rest = &s[i + 1..];
        let mut done = false;
        if let Some(num) = rest.strip_prefix('#') {
            let (digits, radix, skip) = match num.strip_prefix(['x', 'X']) {
                Some(h) => (h, 16, 2),
                None => (num, 10, 1),
            };
            let n: String = digits.chars().take_while(|c| c.is_digit(radix)).take(8).collect();
            if !n.is_empty() {
                if let Some(c) = u32::from_str_radix(&n, radix).ok().and_then(char::from_u32) {
                    out.push(c);
                    let mut used = skip + n.len();
                    if digits[n.len()..].starts_with(';') {
                        used += 1;
                    }
                    i += 1 + used;
                    done = true;
                }
            }
        } else {
            for (name, c) in [("amp", '&'), ("lt", '<'), ("gt", '>'), ("quot", '"'), ("apos", '\'')] {
                if let Some(after) = rest.strip_prefix(name) {
                    // With ';' always; without it only when not followed by alnum or '=' (HTML attribute rule).
                    let semi = after.starts_with(';');
                    let ok = semi || !after.chars().next().is_some_and(|n| n.is_ascii_alphanumeric() || n == '=');
                    if ok && (semi || name == "amp" || name == "lt" || name == "gt" || name == "quot") {
                        out.push(c);
                        i += 1 + name.len() + usize::from(semi);
                        done = true;
                        break;
                    }
                }
            }
        }
        if !done {
            out.push('&');
            i += 1;
        }
    }
    out
}

/// Scan one tag starting just after `<img`. Returns (attrs, index after the tag, terminated).
fn scan_tag(html: &str, start: usize) -> (RawImg, usize, bool) {
    let b = html.as_bytes();
    let end_limit = b.len().min(start + MAX_TAG_BYTES);
    let mut img = RawImg { src: String::new(), width: None, height: None, style: None, hidden_attr: false };
    let mut i = start;
    let is_ws = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | b'/');
    while i < end_limit {
        while i < end_limit && is_ws(b[i]) {
            i += 1;
        }
        if i >= end_limit {
            break;
        }
        if b[i] == b'>' {
            return (img, i + 1, true);
        }
        // attribute name
        let ns = i;
        while i < end_limit && !is_ws(b[i]) && b[i] != b'=' && b[i] != b'>' {
            i += 1;
        }
        let name = html[ns..i].to_ascii_lowercase();
        while i < end_limit && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        let mut value = String::new();
        if i < end_limit && b[i] == b'=' {
            i += 1;
            while i < end_limit && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
                i += 1;
            }
            if i < end_limit && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vs = i;
                while i < end_limit && b[i] != q {
                    i += 1;
                }
                if i >= end_limit {
                    return (img, i, false); // unterminated quote
                }
                value = html[vs..i].to_string();
                i += 1;
            } else {
                let vs = i;
                while i < end_limit && !matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | b'>') {
                    i += 1;
                }
                value = html[vs..i].to_string();
            }
        }
        match name.as_str() {
            "src" if img.src.is_empty() => img.src = decode_entities(&value),
            "width" => img.width = Some(value),
            "height" => img.height = Some(value),
            "style" => img.style = Some(decode_entities(&value)),
            "hidden" => img.hidden_attr = true,
            _ => {}
        }
        // Non-ASCII byte boundaries: ensure `i` is a char boundary before slicing again.
        while i < b.len() && !html.is_char_boundary(i) {
            i += 1;
        }
    }
    (img, i.min(b.len()), false)
}

fn find_imgs(html: &str) -> Vec<RawImg> {
    let b = html.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut attempts) = (0usize, 0usize);
    while i + 4 <= b.len() && attempts < MAX_TAG_ATTEMPTS && out.len() < MAX_IMAGES {
        let Some(off) = b[i..].iter().position(|&c| c == b'<') else { break };
        let p = i + off;
        i = p + 1;
        if p + 4 > b.len() || !b[p + 1..p + 4].eq_ignore_ascii_case(b"img") {
            continue;
        }
        // Must be followed by whitespace, '/', '>' (not `<imgfoo`).
        if !matches!(b.get(p + 4), Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0c | b'/' | b'>')) {
            continue;
        }
        attempts += 1;
        let (img, next, terminated) = scan_tag(html, p + 4);
        if terminated {
            i = next;
        }
        if is_remote_src(&img.src) {
            out.push(img);
        }
    }
    out
}

/// Same normalisation the sanitiser applies before scheme detection (case kept).
fn clean_url(v: &str) -> String {
    v.chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect::<String>()
        .trim_matches(|c: char| c <= ' ')
        .chars()
        .map(|c| if c == '\\' { '/' } else { c })
        .collect()
}

fn is_remote_src(src: &str) -> bool {
    let l = clean_url(src).to_ascii_lowercase();
    l.starts_with("http:") || l.starts_with("https:") || l.starts_with("//")
}

// ---------------------------------------------------------------- analysis helpers

struct UrlParts {
    host: String,
    /// path + query, original case, without fragment
    rest: String,
}

fn split_url(url: &str) -> UrlParts {
    let after_scheme = match url.find("//") {
        Some(p) if p <= 6 => &url[p + 2..],
        _ => url,
    };
    let end = after_scheme.find(['/', '?', '#']).unwrap_or(after_scheme.len());
    let auth = &after_scheme[..end];
    let hostport = auth.rsplit('@').next().unwrap_or(auth);
    let host = if hostport.starts_with('[') {
        hostport.split(']').next().unwrap_or("").trim_start_matches('[').to_string()
    } else {
        hostport.split(':').next().unwrap_or("").to_string()
    };
    let rest = &after_scheme[end..];
    let rest = rest.split('#').next().unwrap_or("");
    UrlParts { host: host.to_ascii_lowercase(), rest: rest.to_string() }
}

fn parse_dim(v: &str) -> Option<u32> {
    let v = v.trim().trim_end_matches("!important").trim().to_ascii_lowercase();
    let v = v.strip_suffix("px").unwrap_or(&v).trim();
    if v.is_empty() || !v.bytes().all(|c| c.is_ascii_digit() || c == b'.') {
        return None;
    }
    v.parse::<f32>().ok().map(|f| f.max(0.0).round().min(100_000.0) as u32)
}

struct StyleInfo {
    width: Option<u32>,
    height: Option<u32>,
    hidden: bool,
}

fn parse_style(style: &str) -> StyleInfo {
    let mut s = StyleInfo { width: None, height: None, hidden: false };
    for d in style.split(';') {
        let Some((p, v)) = d.split_once(':') else { continue };
        let (p, vl) = (p.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase());
        let vv = vl.trim_end_matches("!important").trim();
        match p.as_str() {
            "width" => s.width = parse_dim(vv),
            "height" => s.height = parse_dim(vv),
            "display" if vv == "none" => s.hidden = true,
            "visibility" if vv == "hidden" => s.hidden = true,
            "opacity" if vv.parse::<f32>().is_ok_and(|f| f == 0.0) => s.hidden = true,
            _ => {}
        }
    }
    s
}

fn pct_decode(s: &str) -> String {
    fn hex(c: u8) -> Option<u8> {
        (c as char).to_digit(16).map(|d| d as u8)
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Base64 needles for `email` that are stable wherever the address sits in a longer string.
fn b64_needles(email: &str) -> Vec<String> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let mut v = Vec::new();
    let n = email.len();
    for k in 0..3usize {
        let mut bytes = vec![b'x'; k];
        bytes.extend_from_slice(email.as_bytes());
        let enc = STANDARD.encode(&bytes);
        let enc = enc.trim_end_matches('=');
        let lead = (8 * k).div_ceil(6);
        let trail = usize::from(!(k + n).is_multiple_of(3));
        if enc.len() > lead + trail + 6 {
            let core = &enc[lead..enc.len() - trail];
            v.push(core.to_string());
            v.push(core.replace('+', "-").replace('/', "_"));
        }
    }
    v
}

fn embeds_recipient(url: &str, recipients: &[String]) -> Option<&'static str> {
    let lower = url.to_ascii_lowercase();
    let decoded = pct_decode(&lower);
    let decoded2 = pct_decode(&decoded);
    let (o1, o2) = (pct_decode(url), pct_decode(&pct_decode(url)));
    for r in recipients {
        let r = r.trim().to_ascii_lowercase();
        if r.len() < 5 || !r.contains('@') {
            continue;
        }
        if lower.contains(&r) {
            return Some("URL contains the recipient address");
        }
        if lower.contains(&r.replace('@', "%40")) || decoded.contains(&r) || decoded2.contains(&r) {
            return Some("recipient address is percent-encoded in the URL");
        }
        // base64 is case-sensitive: search the original-case URL and its decoded forms.
        for needle in b64_needles(&r) {
            if url.contains(&needle) || o1.contains(&needle) || o2.contains(&needle) {
                return Some("recipient address is base64-encoded in the URL");
            }
        }
    }
    None
}

fn is_uuid(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    p.len() == 5 && [8, 4, 4, 4, 12].iter().zip(&p).all(|(n, x)| x.len() == *n && x.bytes().all(|c| c.is_ascii_hexdigit()))
}

/// Looks like a random id (hex/base64-ish), not a descriptive filename.
fn is_opaque(tok: &str) -> bool {
    let t = tok.rsplit_once('.').filter(|(_, e)| e.len() <= 4 && e.bytes().all(|c| c.is_ascii_alphanumeric())).map_or(tok, |(a, _)| a);
    if t.len() < 24 {
        return false;
    }
    if is_uuid(t) {
        return true;
    }
    if !t.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'+' | b'/' | b'=')) {
        return false;
    }
    let seps = t.bytes().filter(|c| matches!(c, b'-' | b'_')).count();
    let digit = t.bytes().any(|c| c.is_ascii_digit());
    let alpha = t.bytes().any(|c| c.is_ascii_alphabetic());
    seps <= t.len() / 10 && digit && alpha
}

fn query_pairs(rest: &str) -> Vec<(String, String)> {
    rest.split_once('?')
        .map(|(_, q)| q.split('&').map(|kv| kv.split_once('=').map_or((kv.to_string(), String::new()), |(k, v)| (k.to_string(), v.to_string()))).collect())
        .unwrap_or_default()
}

fn has_path_token(rest: &str) -> bool {
    let path = rest.split('?').next().unwrap_or("");
    path.split('/').any(is_opaque)
}

fn registrable(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return host.to_string();
    }
    let sld_generic = matches!(labels[n - 2], "co" | "com" | "org" | "net" | "ac" | "gov" | "edu") && labels[n - 1].len() == 2;
    labels[n - if sld_generic { 3 } else { 2 }..].join(".")
}

fn host_matches_suffix(host: &str, suf: &str) -> bool {
    host == suf || host.strip_suffix(suf).is_some_and(|p| p.ends_with('.'))
}

fn known_tracker(host: &str, rest_lower: &str) -> Option<&'static str> {
    db().rules.iter().find(|r| {
        r.host_suffix.as_deref().is_none_or(|s| host_matches_suffix(host, s))
            && r.host_contains.as_deref().is_none_or(|s| host.contains(s))
            && r.path.as_deref().is_none_or(|p| rest_lower.contains(p))
    }).map(|r| r.name.as_str())
}

fn truncate_display(url: &str) -> String {
    if url.chars().count() <= DISPLAY_URL_CHARS {
        url.to_string()
    } else {
        let mut s: String = url.chars().take(DISPLAY_URL_CHARS - 1).collect();
        s.push('…');
        s
    }
}

// ---------------------------------------------------------------- public API

/// Analyse every remote `<img>` in `raw_html`. Returns `(info, normalised_full_url)` pairs;
/// the second item is the key the sanitiser matches against.
fn analyze_full(raw_html: &str, recipients: &[String], sender_domain: Option<&str>) -> Vec<(ImageInfo, String)> {
    find_imgs(raw_html)
        .into_iter()
        .map(|img| {
            let full = clean_url(&img.src);
            let key = full.to_ascii_lowercase();
            let url: String = full.chars().take(MAX_URL_ANALYSED).collect();
            let analysed_full = if url.len() == full.len() { None } else { Some(()) };
            let url = if url.starts_with("//") { format!("https:{url}") } else { url };
            (build_info(&img, &url, analysed_full.is_some(), recipients, sender_domain), key)
        })
        .collect()
}

fn build_info(img: &RawImg, url: &str, over_long: bool, recipients: &[String], sender_domain: Option<&str>) -> ImageInfo {
    let UrlParts { host, rest } = split_url(url);
    let rest_lower = rest.to_ascii_lowercase();
    let st = img.style.as_deref().map(parse_style);
    let dim = |attr: &Option<String>, s: Option<u32>| s.or_else(|| attr.as_deref().and_then(parse_dim));
    let width = dim(&img.width, st.as_ref().and_then(|s| s.width));
    let height = dim(&img.height, st.as_ref().and_then(|s| s.height));
    let hidden = img.hidden_attr || st.as_ref().is_some_and(|s| s.hidden);

    let mut tracker_reasons: Vec<String> = Vec::new();
    let mut soft: Vec<String> = Vec::new();
    let mut service = None;

    if let Some(name) = known_tracker(&host, &rest_lower) {
        service = Some(name.to_string());
        tracker_reasons.push(format!("matches known tracker: {name}"));
    }
    for g in &db().generic {
        if rest_lower.contains(&g.contains) {
            tracker_reasons.push(g.reason.clone());
            break;
        }
    }
    if let Some(p) = rest_lower.split('?').next().and_then(|p| p.find("/o/").map(|i| (i, p))) {
        let tail = &rest.split('?').next().unwrap_or("")[p.0 + 3..];
        let seg: String = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
        if seg.len() >= 16 {
            tracker_reasons.push("path looks like an open-tracking token (/o/…)".into());
        }
    }
    let tiny = |d: Option<u32>| d.is_some_and(|v| v <= 2);
    if tiny(width) || tiny(height) {
        tracker_reasons.push(format!("tiny image ({}x{})", width.map_or("?".into(), |v| v.to_string()), height.map_or("?".into(), |v| v.to_string())));
    }
    if hidden {
        tracker_reasons.push("hidden with CSS".into());
    }
    if let Some(r) = embeds_recipient(url, recipients) {
        tracker_reasons.push(r.into());
    }

    // Soft signals (only used when the image is otherwise visible and not a tracker).
    let pairs = query_pairs(&rest);
    let mut param_hits: Vec<String> = pairs
        .iter()
        .map(|(k, _)| k.to_ascii_lowercase())
        .filter(|k| matches!(k.as_str(), "uid" | "rid" | "rcpt" | "recipient" | "subscriber" | "mid" | "cid" | "campaign" | "email") || k.starts_with("utm_"))
        .collect();
    param_hits.dedup();
    if !param_hits.is_empty() {
        soft.push(format!("tracking-style parameters: {}", param_hits.iter().take(4).cloned().collect::<Vec<_>>().join(", ")));
    }
    if has_path_token(&rest) || pairs.iter().any(|(_, v)| is_opaque(v)) {
        soft.push("URL carries a long opaque token".into());
    }
    if over_long {
        soft.push("unusually long URL".into());
    }
    if !soft.is_empty() {
        if let Some(sd) = sender_domain {
            if !sd.is_empty() && registrable(&host) != registrable(&sd.to_ascii_lowercase()) {
                soft.push("hosted on a different domain than the sender".into());
            }
        }
    }

    let (verdict, reasons) = if !tracker_reasons.is_empty() {
        ("tracker", tracker_reasons)
    } else if !soft.is_empty() {
        ("suspicious", soft)
    } else {
        ("clean", vec!["no tracking signs found".to_string()])
    };
    ImageInfo { url: truncate_display(url), host, width, height, hidden, verdict, reasons, tracker: service }
}

/// Analyse all remote images in the raw HTML (max 500). `recipient_emails` are the addresses
/// whose presence in a URL marks it as personalised.
pub fn analyze_remote_images(raw_html: &str, recipient_emails: &[String]) -> Vec<ImageInfo> {
    analyze_remote_images_from(raw_html, recipient_emails, None)
}

/// Like [`analyze_remote_images`], also using the sender's domain as a soft signal.
pub fn analyze_remote_images_from(raw_html: &str, recipient_emails: &[String], sender_domain: Option<&str>) -> Vec<ImageInfo> {
    analyze_full(raw_html, recipient_emails, sender_domain).into_iter().map(|(i, _)| i).collect()
}

/// Full, normalised (sanitiser-form: cleaned and lowercased) URLs of every image whose verdict
/// is "tracker". Pass to `sanitize_html_with` as the deny set.
pub fn tracker_url_set(raw_html: &str, recipient_emails: &[String]) -> HashSet<String> {
    analyze_full(raw_html, recipient_emails, None)
        .into_iter()
        .filter(|(i, _)| i.verdict == "tracker")
        .map(|(_, k)| k)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(html: &str) -> ImageInfo {
        let v = analyze_remote_images(html, &["me@example.org".to_string()]);
        assert_eq!(v.len(), 1, "{html}");
        v.into_iter().next().unwrap()
    }

    #[test]
    fn db_parses() {
        assert!(db().rules.len() >= 30);
        assert!(db().generic.len() >= 5);
    }

    #[test]
    fn known_open_pixels_are_trackers() {
        for (u, svc) in [
            ("https://us1.list-manage.com/track/open.php?u=abc&id=def&e=ghi", "Mailchimp"),
            ("https://u123.ct.sendgrid.net/wf/open?upn=AbCdEf", "SendGrid"),
            ("https://xyz.ablink.mail.example.com/o/abc", "SendGrid"),
            ("https://abc.r.us-east-1.awstrack.me/I0/0100/abc-000", "Amazon SES"),
            ("https://track.hubspotemail.net/e2t/o/AAA/BBB", "HubSpot"),
            ("https://mandrillapp.com/track/open.php?u=1&id=2", "Mandrill"),
            ("https://trk.klclick.com/wf/open?upem=abc", "Klaviyo"),
            ("https://pstmrk.it/open/abcdef", "Postmark"),
            ("https://www.facebook.com/tr?id=123&ev=PageView", "Facebook"),
            ("https://www.google-analytics.com/collect?v=1&tid=UA-1", "Google Analytics"),
        ] {
            // Large visible size so only the host/path rules can trigger.
            let i = one(&format!("<img src=\"{u}\" width=\"600\" height=\"200\">"));
            assert_eq!(i.verdict, "tracker", "{u}");
            assert!(i.tracker.as_deref().unwrap_or("").contains(svc), "{u}: {:?}", i.tracker);
            assert!(!i.reasons.is_empty());
        }
    }

    #[test]
    fn hidden_one_pixel_is_tracker() {
        let i = one("<img src=\"https://cdn.example.com/a.gif\" style=\"width:1px;height:1px;display:none\">");
        assert_eq!(i.verdict, "tracker");
        assert!(i.hidden);
        assert_eq!((i.width, i.height), (Some(1), Some(1)));
        let j = one("<img src=\"https://cdn.example.com/a.gif\" style=\"opacity:0\">");
        assert_eq!(j.verdict, "tracker");
    }

    #[test]
    fn visible_cdn_banner_is_clean() {
        let i = one("<img src=\"https://cdn.shop.example/img/banner-summer-sale-collection-2024.jpg\" width=\"600\" height=\"200\">");
        assert_eq!(i.verdict, "clean", "{:?}", i.reasons);
        assert_eq!(i.host, "cdn.shop.example");
        assert!(i.tracker.is_none());
    }

    #[test]
    fn recipient_embedded_variants() {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode("me@example.org");
        let b64u = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("xyz:me@example.org");
        for u in [
            "https://cdn.example.com/p.png?x=me@example.org".to_string(),
            "https://cdn.example.com/p.png?x=me%40example.org".to_string(),
            "https://cdn.example.com/p.png?x=ME%40Example.org".to_string(),
            format!("https://cdn.example.com/p.png?t={b64}"),
            format!("https://cdn.example.com/p/{b64u}.png"),
        ] {
            let i = one(&format!("<img src=\"{u}\" width=\"300\" height=\"100\">"));
            assert_eq!(i.verdict, "tracker", "{u}");
            assert!(i.reasons.iter().any(|r| r.contains("recipient")), "{u}: {:?}", i.reasons);
        }
        // someone else's address is not flagged for that reason
        let i = one("<img src=\"https://cdn.example.com/p.png?x=other@example.org\" width=\"300\" height=\"100\">");
        assert_ne!(i.verdict, "tracker");
    }

    #[test]
    fn suspicious_for_tokens_and_params() {
        let i = one("<img src=\"https://img.vendor.example/p/AbCdEf0123456789AbCdEf0123456789.png\" width=\"300\" height=\"100\">");
        assert_eq!(i.verdict, "suspicious");
        let j = one("<img src=\"https://img.vendor.example/banner.png?utm_source=x&uid=5\" width=\"300\" height=\"100\">");
        assert_eq!(j.verdict, "suspicious");
        let v = analyze_remote_images_from("<img src=\"https://img.vendor.example/banner.png?uid=5\" width=\"300\" height=\"100\">", &[], Some("news.example.org"));
        assert!(v[0].reasons.iter().any(|r| r.contains("different domain")));
    }

    #[test]
    fn parsing_variants() {
        let html = "<IMG SRC='https://a.example/x.png' WIDTH=1 HEIGHT=1><img\n src=https://b.example/y.png width=400>\
                    <img/src=\"//c.example/z.png\"><img src=\"cid:inline\"><img src=\"/relative.png\"><imgx src=\"https://no.example/\">\
                    <img src=\"https://d.example/a?x=1&amp;uid=7\" width=50>";
        let v = analyze_remote_images(html, &[]);
        let hosts: Vec<_> = v.iter().map(|i| i.host.as_str()).collect();
        assert_eq!(hosts, ["a.example", "b.example", "c.example", "d.example"]);
        assert_eq!(v[0].verdict, "tracker");
        assert_eq!(v[1].width, Some(400));
        assert!(v[3].url.contains("x=1&uid=7"));
        assert_eq!(v[3].verdict, "suspicious");
    }

    #[test]
    fn quoted_gt_in_attribute() {
        let v = analyze_remote_images("<img alt=\"a>b\" src=\"https://q.example/p.gif\" width=\"1\">", &[]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].verdict, "tracker");
    }

    #[test]
    fn hostile_input_is_bounded() {
        let t = std::time::Instant::now();
        assert!(analyze_remote_images(&"<img".repeat(100_000), &[]).is_empty());
        assert!(analyze_remote_images(&"<img src=\"https://x.example/".repeat(20_000), &[]).len() <= MAX_IMAGES);
        assert!(analyze_remote_images("<img src=\"https://a.example/", &[]).is_empty());
        let huge = format!("<img src=\"https://a.example/{}\">", "a".repeat(1_000_000));
        let v = analyze_remote_images(&huge, &[]);
        assert!(v.len() <= 1);
        let many = "<img src=\"https://a.example/p.png\">".repeat(2000);
        assert_eq!(analyze_remote_images(&many, &[]).len(), 500);
        assert!(analyze_remote_images("<img \u{00e9}=\"\u{1f600}\" src=\"https://é.example/\u{1f600}\">", &[]).len() <= 1);
        assert!(t.elapsed().as_secs() < 5, "too slow: {:?}", t.elapsed());
    }

    #[test]
    fn long_url_truncated_for_display_only() {
        let u = format!("https://a.example/{}", "b".repeat(2000));
        let v = analyze_remote_images(&format!("<img src=\"{u}\" width=\"50\">"), &[]);
        assert_eq!(v[0].url.chars().count(), 300);
        assert!(tracker_url_set(&format!("<img src=\"{u}\" style=\"width:1px\">"), &[]).contains(&u));
    }
}
