//! A tolerant `<a href>` scanner. It only reads text: nothing is rendered, fetched or executed.

use super::LinkFact;

pub const MAX_ANCHORS: usize = 300;
const MAX_HREF: usize = 8192;
const MAX_TEXT: usize = 300;

/// Decode the handful of entities that matter in attribute values and link text (named basics + numeric).
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(semi) = tail.find(';').filter(|&j| j <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => ent
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => n.parse::<u32>().ok(),
                })
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Value of attribute `name` inside a tag's attribute text (case-insensitive name, quoted or bare value).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace() || matches!(lower.as_bytes()[at - 1], b'"' | b'\'');
        if !before_ok {
            continue;
        }
        let after = tag[at + name.len()..].trim_start();
        let Some(val) = after.strip_prefix('=') else { continue };
        let val = val.trim_start();
        let v = match val.chars().next() {
            Some(q @ ('"' | '\'')) => val[1..].split(q).next().unwrap_or("").to_string(),
            _ => val.split(|c: char| c.is_ascii_whitespace() || c == '>').next().unwrap_or("").to_string(),
        };
        return Some(decode_entities(&v));
    }
    None
}

/// End of the tag that starts at `start` (index of `<`), honouring quotes. Index of the closing `>`.
fn tag_end(html: &str, start: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in html[start..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(start + i),
            _ => {}
        }
    }
    None
}

fn strip_tags(inner: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in inner.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Every `<a href=...>text</a>` (up to `max`), with entity-decoded href and tag-stripped, whitespace-collapsed text.
pub fn anchors(html: &str, max: usize) -> Vec<LinkFact> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut pos = 0;
    while out.len() < max {
        let Some(i) = lower[pos..].find("<a") else { break };
        let start = pos + i;
        pos = start + 2;
        let next = lower.as_bytes().get(start + 2).copied();
        if !matches!(next, Some(b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>')) {
            continue;
        }
        let Some(end) = tag_end(html, start) else { break };
        let tag = &html[start + 2..end];
        pos = end + 1;
        let Some(href) = attr(tag, "href") else { continue };
        let close = lower[pos..].find("</a").map(|j| pos + j).unwrap_or(html.len());
        let inner = &html[pos..close.min(html.len())];
        let text = decode_entities(&strip_tags(inner));
        out.push(LinkFact {
            href: href.chars().take(MAX_HREF).collect(),
            text: text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TEXT).collect(),
        });
    }
    out
}

/// `http(s)://` addresses written in plain text (used when a message has no HTML part). No visible text to compare.
pub fn text_urls(text: &str, max: usize) -> Vec<LinkFact> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let t = tok.trim_matches(|c: char| matches!(c, '<' | '>' | '(' | ')' | '[' | ']' | '"' | '\'' | ',' | ';')).trim_end_matches(['.', '!', '?', ':', ')', ']', '>', ',', ';', '\'', '"']);
        let l = t.to_ascii_lowercase();
        if (l.starts_with("http://") || l.starts_with("https://")) && out.len() < max {
            out.push(LinkFact { href: t.chars().take(MAX_HREF).collect(), text: String::new() });
        }
    }
    out
}

/// Scheme of an href after the browser-style clean-up (controls and whitespace removed, lower case).
pub fn scheme_of(href: &str) -> Option<String> {
    let cleaned: String = href.chars().filter(|c| !c.is_control() && !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    let colon = cleaned.find(':')?;
    let s = &cleaned[..colon];
    (!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))).then(|| s.to_string())
}

const DANGEROUS: &[&str] = &["javascript", "vbscript", "data", "file", "blob", "jar", "ms-msdt", "search-ms", "ms-officecmd", "x-apple.systempreferences", "shell"];

/// Anchors in the RAW html whose href scheme is dangerous. The sanitiser drops these, but their presence is evidence.
pub fn dangerous_scheme_count(raw_html: &str) -> u32 {
    anchors(raw_html, 2000).iter().filter(|a| scheme_of(&a.href).is_some_and(|s| DANGEROUS.contains(&s.as_str()))).count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_href_and_text() {
        let a = anchors(r#"<p><a class="x" href="https://a.test/?x=1&amp;y=2">  Go <b>now</b> </a> <A HREF='https://b.test'>b</A><abbr>x</abbr><a name=z>no href</a></p>"#, 10);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].href, "https://a.test/?x=1&y=2");
        assert_eq!(a[0].text, "Go now");
        assert_eq!(a[1].href, "https://b.test");
    }

    #[test]
    fn bare_and_odd_attributes() {
        let a = anchors("<a data-href=\"no\" href=https://c.test/x>c</a>", 5);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].href, "https://c.test/x");
    }

    #[test]
    fn dangerous_schemes_survive_tricks() {
        let h = r#"<a href="javascript:alert(1)">a</a><a href="JaVa&#9;script:x">b</a><a href="&#106;avascript:y">c</a><a href="data:text/html;base64,AAAA">d</a><a href="https://ok.test">e</a><a href="mailto:a@b.c">f</a>"#;
        assert_eq!(dangerous_scheme_count(h), 4);
        assert_eq!(dangerous_scheme_count("<a href=\"https://ok.test\">x</a>"), 0);
    }

    #[test]
    fn plain_text_urls() {
        let l = text_urls("See (https://a.test/x?y=1). Also <HTTP://b.test/z>, and mail a@b.c; ftp://no.test", 10);
        assert_eq!(l.iter().map(|x| x.href.as_str()).collect::<Vec<_>>(), vec!["https://a.test/x?y=1", "HTTP://b.test/z"]);
    }

    #[test]
    fn caps_and_garbage_do_not_panic() {
        let many = "<a href=\"https://x.test\">x</a>".repeat(1000);
        assert_eq!(anchors(&many, 50).len(), 50);
        assert!(anchors("<a href=\"unterminated", 5).is_empty());
        assert!(anchors("<a", 5).is_empty());
        assert_eq!(decode_entities("a &amp b &#x41; &#65; &bogus; &"), "a &amp b A A &bogus; &");
    }
}
