//! Link and domain checks. Everything is computed from the link text and the address itself: no DNS, no fetch.

use super::data::{brand_label, common_tld, is_esp_host, is_official, is_shortener, suspicious_tld, BRANDS, NEAR_BRAND_OK};
use super::urls::{domain_of_address, edit_distance, has_mixed_script, parse_url, reg_domain, reg_label, skeletons, unicode_host, HostKind, Url};
use super::{clean, Collector, SafetyInput, Severity};
use std::collections::HashSet;

const MAX_LINKS: usize = 200;
const LONG_URL: usize = 2000;
const REDIRECT_PARAMS: &[&str] = &["url", "u", "redirect", "redir", "redirect_url", "redirect_to", "target", "dest", "destination", "goto", "link", "to"];

/// Labels worth protecting from imitation: the sender's own, the user's own domains.
fn known_labels(input: &SafetyInput) -> Vec<String> {
    let mut v: Vec<String> = input.own_addresses.iter().map(|a| reg_label(&domain_of_address(a))).filter(|l| l.len() >= 4).collect();
    if let Some(f) = &input.from {
        let l = reg_label(&domain_of_address(&f.address));
        if l.len() >= 4 {
            v.push(l);
        }
    }
    v.sort();
    v.dedup();
    v
}

/// Domain-level look-alike checks for one host. `who` = "sender" or "link", used in the wording only.
/// `known` = labels this host must not imitate (it may legitimately equal them).
pub fn check_domain(host: &str, who: &str, known: &[String], c: &mut Collector) {
    if is_esp_host(host) {
        return;
    }
    let uni = unicode_host(host);
    let non_ascii = !uni.is_ascii();
    let shown = clean(&uni, 60);
    let reg_ascii = reg_domain(host);
    let label = reg_label(&uni).to_lowercase();
    let mut flagged = false;

    if has_mixed_script(&uni) {
        c.add("DOMAIN_MIXED_SCRIPT", Severity::High, format!("The {who} address \"{shown}\" mixes letters from different alphabets in one word, a look-alike trick."));
        flagged = true;
    }

    // Skeleton (look-alike folding) equals a protected name but the label itself is different.
    let mut imitated: Option<String> = None;
    // The whole label and each hyphen-separated word of it ("micros0ft-online").
    let mut cands: Vec<String> = vec![label.clone()];
    cands.extend(label.split('-').filter(|t| t.len() >= 5 && *t != label).map(str::to_string));
    for (orig, s) in cands.iter().flat_map(|o| skeletons(o).into_iter().map(move |s| (o, s))) {
        if s == *orig {
            continue;
        }
        if let Some(b) = BRANDS.iter().map(|(b, _)| *b).find(|b| *b == s && !is_official(b, &reg_ascii)) {
            imitated = Some(b.to_string());
        } else if known.contains(&s) {
            imitated = Some(s.clone());
        }
        if imitated.is_some() {
            break;
        }
    }
    if let Some(name) = imitated {
        c.add("DOMAIN_CONFUSABLE", Severity::High, format!("The {who} address \"{shown}\" is written to look like \"{}\" but is a different domain.", clean(&name, 30)));
        flagged = true;
    }

    if !flagged && (non_ascii || host.split('.').any(|l| l.starts_with("xn--"))) {
        c.add("DOMAIN_PUNYCODE", Severity::Low, format!("The {who} address \"{shown}\" uses non-English letters, which can hide look-alikes."));
    }

    // One edit away from a well-known name or from the user's own domain.
    if !flagged && label.is_ascii() {
        let near_brand = BRANDS.iter().map(|(b, _)| *b).find(|b| b.len() >= 6 && label != *b && !NEAR_BRAND_OK.contains(&label.as_str()) && edit_distance(&label, b, 1) == 1 && !is_official(b, &reg_ascii));
        let near_known = known.iter().find(|k| k.len() >= 6 && **k != label && edit_distance(&label, k, 1) == 1);
        if let Some(n) = near_brand.map(str::to_string).or_else(|| near_known.cloned()) {
            c.add("DOMAIN_EDIT_DISTANCE", Severity::Medium, format!("The {who} address \"{shown}\" differs from \"{}\" by a single letter.", clean(&n, 30)));
        }
    }

    // A well-known name used as a sub-domain or as a hyphenated word of someone else's domain.
    let labels: Vec<&str> = host.split('.').collect();
    let reg_parts = reg_ascii.split('.').count();
    let sub: Vec<&str> = labels[..labels.len().saturating_sub(reg_parts)].to_vec();
    let reg_tokens: Vec<&str> = label.split('-').collect();
    for (b, _) in BRANDS.iter().filter(|(b, _)| b.len() >= 5) {
        if is_official(b, &reg_ascii) {
            continue;
        }
        let in_sub = sub.iter().any(|l| l.split('-').any(|t| t == *b));
        let in_reg = reg_tokens.len() > 1 && reg_tokens.contains(b);
        if in_sub || in_reg {
            c.add("DOMAIN_BRAND_IN_SUBDOMAIN", Severity::Medium, format!("The {who} address \"{shown}\" contains \"{b}\" but belongs to {}.", clean(&reg_ascii, 40)));
            break;
        }
    }

    if let Some(t) = suspicious_tld(host) {
        c.add("DOMAIN_SUSPICIOUS_TLD", Severity::Low, format!("The {who} address \"{shown}\" ends in .{t}, an ending that is often used for throw-away sites."));
    }
}

/// First "web address" shown in a link's visible text: `https://x.y/..`, `www.x.y` or a bare `x.tld` with a common TLD.
pub fn text_host(text: &str) -> Option<String> {
    for tok in text.split_whitespace() {
        let tok = tok.trim_matches(|c: char| matches!(c, '"' | '\'' | '(' | ')' | '<' | '>' | '[' | ']' | ',' | ';' | ':' | '!' | '?' | '.' | '*'));
        if tok.is_empty() || tok.contains('@') {
            continue;
        }
        let lower = tok.to_lowercase();
        let (body, explicit) = match lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) {
            Some(b) => (b.to_string(), true),
            None => (lower.clone(), lower.starts_with("www.")),
        };
        let host = body.split(['/', '?', '#', ':']).next().unwrap_or("").trim_end_matches('.').to_string();
        if !host.contains('.') {
            continue;
        }
        let labels: Vec<&str> = host.split('.').collect();
        if labels.iter().any(|l| l.is_empty() || l.starts_with('-') || l.ends_with('-') || !l.chars().all(|c| c.is_alphanumeric() || c == '-')) {
            continue;
        }
        let tld = *labels.last()?;
        let tld_ok = tld.chars().all(|c| c.is_alphabetic()) && (2..=24).contains(&tld.chars().count());
        if tld_ok && (explicit || common_tld(tld)) {
            return Some(host);
        }
    }
    None
}

fn related(a: &str, b: &str) -> bool {
    a.len() >= 4 && b.len() >= 4 && (a.starts_with(b) || b.starts_with(a))
}

fn describe_ip(u: &Url) -> String {
    match &u.kind {
        HostKind::Ipv4 { addr, canonical: true } => format!("a numeric address ({addr})"),
        HostKind::Ipv4 { addr, canonical: false } => format!("a numeric address ({addr}) written in a disguised form (\"{}\")", clean(&u.host, 40)),
        HostKind::Ipv6 { addr, embedded_v4: Some(v4) } => format!("a numeric address ({addr}, which embeds IPv4 {v4})"),
        HostKind::Ipv6 { addr, .. } => format!("a numeric address ({addr})"),
        HostKind::Domain => String::new(),
    }
}

fn open_redirect_target(u: &Url) -> Option<String> {
    let q = u.rest.split_once('?')?.1.split('#').next()?;
    for kv in q.split('&') {
        let Some((k, v)) = kv.split_once('=') else { continue };
        if !REDIRECT_PARAMS.contains(&k.to_ascii_lowercase().as_str()) {
            continue;
        }
        if let Some(t) = parse_url(&decode_pct(v)).filter(|t| matches!(t.scheme.as_str(), "http" | "https")) {
            return Some(t.host);
        }
    }
    None
}

fn decode_pct(s: &str) -> String {
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
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn check(input: &SafetyInput, c: &mut Collector) {
    let known = known_labels(input);
    let from_dom = input.from.as_ref().map(|f| domain_of_address(&f.address)).unwrap_or_default();
    let from_reg = reg_domain(&from_dom);
    if !from_dom.is_empty() {
        let own: Vec<String> = known.iter().filter(|k| **k != reg_label(&from_dom)).cloned().collect();
        check_domain(&from_dom, "sender", &own, c);
    }
    if input.dangerous_scheme_links > 0 {
        c.add("LINK_DANGEROUS_SCHEME", Severity::High, format!("{} link(s) in the message run code or open local data (javascript:, data: or similar). MiraMail removes them, but a legitimate sender has no reason to include them.", input.dangerous_scheme_links));
    }

    let mut seen: HashSet<&str> = HashSet::new();
    for l in input.links.iter() {
        let href = l.href.trim();
        if !seen.insert(href) {
            continue;
        }
        if seen.len() > MAX_LINKS {
            break;
        }
        let Some(u) = parse_url(href) else { continue };
        if !matches!(u.scheme.as_str(), "http" | "https") {
            continue;
        }
        let shown_host = clean(&u.host, 60);
        if href.chars().count() > LONG_URL {
            c.add("LINK_VERY_LONG", Severity::Low, format!("A link to {shown_host} is {} characters long; very long addresses can hide where they lead.", href.chars().count()));
        }
        if let Some(ui) = u.userinfo.as_deref().filter(|s| !s.is_empty()) {
            c.add_floor("LINK_USERINFO", Severity::High, 60, format!("A link shows \"{}\" before an @ sign, but really goes to {shown_host}.", clean(ui, 40)));
        }
        if u.trailing_dot {
            c.add("LINK_TRAILING_DOT", Severity::Medium, format!("A link to \"{shown_host}.\" ends in a dot, a trick used to dodge look-alike checks."));
        }
        if u.pct_host {
            c.add("LINK_OBFUSCATED_HOST", Severity::Medium, format!("A link's website name is written with %-escapes (decodes to {shown_host})."));
        }
        let default_port = match u.scheme.as_str() {
            "http" => 80,
            _ => 443,
        };
        if let Some(p) = u.port.filter(|p| *p != default_port) {
            c.add("LINK_NONSTANDARD_PORT", Severity::Medium, format!("A link to {shown_host} uses port {p} instead of the normal one."));
        }

        match &u.kind {
            HostKind::Ipv4 { canonical, .. } => {
                c.add("LINK_IP_LITERAL", Severity::High, format!("A link goes to {} instead of a website name.", describe_ip(&u)));
                if !canonical {
                    c.add("LINK_OBFUSCATED_HOST", Severity::Medium, format!("A link's address is written in a disguised numeric form (\"{shown_host}\")."));
                }
                continue;
            }
            HostKind::Ipv6 { .. } => {
                c.add("LINK_IP_LITERAL", Severity::High, format!("A link goes to {} instead of a website name.", describe_ip(&u)));
                continue;
            }
            HostKind::Domain => {}
        }

        let esp = is_esp_host(&u.host);
        if esp {
            c.add("LINK_TRACKING_REDIRECT", Severity::Info, format!("Some links go through a mailing or link-protection service ({shown_host}). That is normal for newsletters and company mail."));
        } else {
            check_domain(&u.host, "link", &known, c);
            if is_shortener(&u.host) {
                c.add("LINK_SHORTENER", Severity::Low, format!("A link uses the URL shortener {shown_host}, which hides the real destination."));
            }
            if let Some(target) = open_redirect_target(&u) {
                if reg_domain(&target) != reg_domain(&u.host) {
                    c.add("LINK_OPEN_REDIRECT", Severity::Medium, format!("A link on {shown_host} forwards you to a different site ({}).", clean(&target, 40)));
                }
            }
        }

        // Visible text shows one website, the link goes to another.
        if !esp {
            if let Some(th) = text_host(&l.text) {
                let (t_reg, h_reg) = (reg_domain(&th), reg_domain(&u.host));
                let (t_label, h_label) = (reg_label(&th), reg_label(&u.host));
                let official = brand_label(&t_label).is_some_and(|b| is_official(b, &h_reg));
                if t_reg != h_reg && !official {
                    let sev = if brand_label(&t_label).is_some() || t_reg == from_reg || is_shortener(&u.host) {
                        Severity::High
                    } else if related(&t_label, &h_label) {
                        Severity::Low
                    } else {
                        Severity::Medium
                    };
                    c.add("LINK_TEXT_MISMATCH", sev, format!("A link says \"{}\" but really goes to {shown_host}.", clean(&th, 60)));
                }
            }
        }
    }
}
