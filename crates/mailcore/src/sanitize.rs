//! HTML sanitising for untrusted mail (ammonia).
//!
//! Policy: no scripts, frames, forms, objects, svg, meta, base, link or
//! `<style>` elements; no event handlers; only http/https/mailto/cid URL
//! schemes; remote `<img>` removed unless the caller opts in; inline `style`
//! attributes kept only when they cannot fetch resources or escape layout.

use ammonia::Builder;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizedHtml {
    pub html: String,
    /// Number of remote image references removed.
    pub blocked_remote_count: usize,
    /// Remote images dropped because their URL was in the deny set (only counted when
    /// remote images are allowed; otherwise they are part of `blocked_remote_count`).
    #[serde(default)]
    pub skipped_tracker_count: usize,
}

/// Normalise a URL attribute value the way the WHATWG URL parser does before
/// scheme detection: strip ASCII tab/LF/CR everywhere, trim C0 controls and
/// space, map `\` to `/`, lowercase.
pub(crate) fn normalize_url(url: &str) -> String {
    clean_url(url).to_ascii_lowercase()
}

/// [`normalize_url`] without the lowercasing: what is written back into the page. Paths, queries and `cid:`
/// Content-IDs are case-sensitive (Outlook's `image001.png@01DA2B3C...`, signed CDN links), so only the scheme
/// checks may ignore case.
fn clean_url(url: &str) -> String {
    let stripped: String = url.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    stripped.trim_matches(|c: char| c <= ' ').chars().map(|c| if c == '\\' { '/' } else { c }).collect()
}

fn is_remote(norm: &str) -> bool {
    norm.starts_with("http:") || norm.starts_with("https:") || norm.starts_with("//") || norm.starts_with("ftp:")
}

fn is_safe_inline_src(norm: &str) -> bool {
    norm.starts_with("cid:")
        || ["png", "jpeg", "gif", "webp"].iter().any(|t| norm.starts_with(&format!("data:image/{t};")) || norm.starts_with(&format!("data:image/{t},")))
}

/// Inline style policy (strict allowlist). Returns true if the whole attribute
/// must be dropped. Any disallowed property, any `(` other than
/// rgb()/rgba()/hsl()/hsla(), and any comment, escape, `@`, `{`/`}` or
/// angle bracket rejects the attribute, so nothing can fetch a resource.
pub(crate) fn style_is_dangerous(v: &str) -> bool {
    if v.contains("/*") || v.contains("*/") || v.contains('\\') || v.contains(['@', '{', '}', '<', '>']) {
        return true;
    }
    if v.chars().any(|c| c.is_control()) {
        return true;
    }
    // Split declarations on ';' outside quotes.
    let mut decls = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in v.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                ';' => decls.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return true;
    }
    decls.push(cur);
    for d in decls {
        let d = d.trim();
        if d.is_empty() {
            continue;
        }
        let Some((prop, val)) = d.split_once(':') else { return true };
        if !prop_allowed(&prop.trim().to_ascii_lowercase()) || !value_allowed(&prop.trim().to_ascii_lowercase(), val) {
            return true;
        }
    }
    false
}

/// Reader policy for an inline `style`: keep every declaration that is individually safe and drop the rest.
/// (`style_is_dangerous` rejects the WHOLE attribute on one unknown property such as the `outline:none` that
/// React-Email style templates put on every image, which threw away `display`, `vertical-align`, margins and
/// sizes and made layouts collapse.) Structural danger (comments, escapes, braces, quotes left open) still drops
/// the whole attribute, and every kept declaration passes the same property and value checks as before.
pub(crate) fn filter_style(v: &str) -> Option<String> {
    if v.contains("/*") || v.contains("*/") || v.contains('\\') || v.contains(['@', '{', '}', '<', '>']) || v.chars().any(|c| c.is_control()) {
        return None;
    }
    let mut decls = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in v.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                ';' => decls.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    decls.push(cur);
    let kept: Vec<String> = decls
        .iter()
        .filter_map(|d| {
            let d = d.trim();
            let (prop, val) = d.split_once(':')?;
            let prop = prop.trim().to_ascii_lowercase();
            if prop == "background" && background_has_image(val) {
                // `background:#1a1a1a url(hero.jpg)`: never keep the image, but keep the fallback colour so
                // white-on-dark sections do not turn white on white.
                return background_colour(val).map(|c| format!("background-color:{c}"));
            }
            (prop_allowed(&prop) && value_allowed(&prop, val)).then(|| format!("{prop}:{}", val.trim()))
        })
        .collect();
    (!kept.is_empty()).then(|| kept.join(";"))
}

/// Leading CSS number (sign, digits, fraction, exponent) and the alphabetic / `%` unit after it. `None` for keywords
/// (`small`, `inherit`, ...) and anything else that is not a plain number with a unit.
fn css_num(s: &str) -> Option<(f64, &str)> {
    let s = s.trim();
    let b = s.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let digits_from = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let mut n_digits = i - digits_from;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let f = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        n_digits += i - f;
    }
    if n_digits == 0 {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if b.get(j).is_some_and(u8::is_ascii_digit) {
            while b.get(j).is_some_and(u8::is_ascii_digit) {
                j += 1;
            }
            i = j;
        }
    }
    let n: f64 = s[..i].parse().ok()?;
    let unit = &s[i..];
    (unit.bytes().all(|c| c.is_ascii_alphabetic() || c == b'%')).then_some((n, unit))
}

/// Opacity below 0.1 (or 10 %) cannot be read.
fn opacity_hides(v: &str) -> bool {
    css_num(v).is_some_and(|(n, u)| if u == "%" { n < 10.0 } else { u.is_empty() && n < 0.1 })
}

/// A font size below about 2 px (any unit) cannot be read; keywords such as `small` or `inherit` are not hidden.
fn font_size_hides(v: &str) -> bool {
    css_num(v).is_some_and(|(n, u)| match u.to_ascii_lowercase().as_str() {
        "" | "px" => n < 2.0,
        "pt" => n < 1.5,
        "em" | "rem" | "ex" | "ch" | "pc" => n < 0.125,
        "%" => n < 12.5,
        "mm" => n < 0.5,
        "cm" => n < 0.05,
        "in" => n < 0.02,
        "q" => n < 2.0,
        "vw" | "vh" | "vmin" | "vmax" => n < 0.2,
        _ => n <= 0.0,
    })
}

/// `transparent` or a colour whose alpha is below 0.1 (`rgba`, `hsla`, `rgb(r g b / a)`, `#rgba`, `#rrggbbaa`).
fn color_hides(v: &str) -> bool {
    let v = v.trim();
    if v == "transparent" {
        return true;
    }
    if let Some(h) = v.strip_prefix('#') {
        let a = match h.len() {
            4 => u8::from_str_radix(&h[3..4].repeat(2), 16),
            8 => u8::from_str_radix(&h[6..8], 16),
            _ => return false,
        };
        return a.is_ok_and(|a| a < 26);
    }
    let (Some(o), Some(c)) = (v.find('('), v.rfind(')')) else { return false };
    if o >= c || !["rgb", "rgba", "hsl", "hsla"].contains(&v[..o].trim()) {
        return false;
    }
    let inner = &v[o + 1..c];
    let alpha = if let Some((_, a)) = inner.split_once('/') {
        a.trim()
    } else {
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 4 { parts[3].trim() } else { return false }
    };
    css_num(alpha).is_some_and(|(n, u)| if u == "%" { n < 10.0 } else { u.is_empty() && n < 0.1 })
}

/// True when a (non-dangerous) inline style hides its element: `display:none`, `visibility:hidden|collapse`,
/// `opacity` below 0.1, a font size below 2 px, or (for `color`) a transparent colour. Values are parsed as numbers
/// (sign, exponent, any unit, `!important`), so `0e0`, `-1`, `+0px` and `0.001` all count while keywords like
/// `font-size:small` do not. The reader keeps such styles (marketing mail uses them); the composer must not,
/// because hidden text would be sent without the user ever seeing it.
pub(crate) fn style_hides(v: &str) -> bool {
    v.split(';').any(|d| {
        let Some((prop, val)) = d.split_once(':') else { return false };
        let val = val.to_ascii_lowercase();
        let val = val.split('!').next().unwrap_or("").trim();
        match prop.trim().to_ascii_lowercase().as_str() {
            "display" => val == "none",
            "visibility" => matches!(val, "hidden" | "collapse"),
            "opacity" => opacity_hides(val),
            "font-size" => font_size_hides(val),
            "color" => color_hides(val),
            "font" => val.split_whitespace().any(|t| {
                let size = t.split('/').next().unwrap_or("");
                css_num(size).is_some_and(|(_, u)| !u.is_empty() || size.trim_start_matches(['+', '-']).starts_with('0')) && font_size_hides(size)
            }),
            _ => false,
        }
    })
}

/// Mail-hiding tricks beyond [`style_hides`]: `mso-hide:all`, text pushed off-screen (large negative
/// `left`/`top`/`margin*`/`text-indent`) and zero-size boxes that clip their content (`overflow:hidden` with a zero
/// `width`/`height`/`max-width`/`max-height`). Used for the text that is handed to the AI.
pub(crate) fn style_hides_strict(v: &str) -> bool {
    if style_hides(v) {
        return true;
    }
    let mut overflow_hidden = false;
    let mut zero_size = false;
    for d in v.split(';') {
        let Some((prop, val)) = d.split_once(':') else { continue };
        let val = val.to_ascii_lowercase();
        let val = val.split('!').next().unwrap_or("").trim().to_string();
        let prop = prop.trim().to_ascii_lowercase();
        match prop.as_str() {
            "mso-hide" if val == "all" => return true,
            "overflow" | "overflow-x" | "overflow-y" => overflow_hidden |= val.split_whitespace().any(|t| t == "hidden" || t == "clip"),
            "width" | "height" | "max-width" | "max-height" => zero_size |= css_num(&val).is_some_and(|(n, _)| n <= 0.0),
            "left" | "top" | "right" | "bottom" | "text-indent" | "margin" | "margin-left" | "margin-top" | "margin-right" | "margin-bottom" => {
                let off = val.split(|c: char| c.is_whitespace() || c == ',').filter_map(css_num).any(|(n, u)| {
                    n <= match u {
                        "em" | "rem" | "ex" | "ch" => -10.0,
                        "%" | "vw" | "vh" => -50.0,
                        _ => -100.0,
                    }
                });
                if off {
                    return true;
                }
            }
            _ => {}
        }
    }
    overflow_hidden && zero_size
}

fn background_has_image(val: &str) -> bool {
    let low = val.to_ascii_lowercase();
    ["url(", "image-set(", "gradient("].iter().any(|k| low.contains(k))
}

/// First plain colour token of a `background` shorthand (outside parentheses), validated strictly.
fn background_colour(val: &str) -> Option<String> {
    let low = val.to_ascii_lowercase();
    let low = low.split('!').next().unwrap_or("");
    let (mut depth, mut cur) = (0i32, String::new());
    let mut toks: Vec<String> = Vec::new();
    for c in low.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            c if c.is_whitespace() && depth <= 0 => {
                if !cur.is_empty() {
                    toks.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    toks.into_iter().find(|t| valid_colour_token(t))
}

fn valid_colour_token(t: &str) -> bool {
    if let Some(h) = t.strip_prefix('#') {
        return matches!(h.len(), 3 | 4 | 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit());
    }
    if let Some(o) = t.find('(') {
        let (name, rest) = (&t[..o], &t[o + 1..]);
        return matches!(name, "rgb" | "rgba")
            && rest.strip_suffix(')').is_some_and(|a| !a.is_empty() && a.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b',' | b' ' | b'%' | b'/')) && a.bytes().any(|b| b.is_ascii_digit()));
    }
    COLOUR_NAMES.contains(&t)
}

const COLOUR_NAMES: &[&str] = &[
    "transparent", "black", "silver", "gray", "grey", "white", "maroon", "red", "purple", "fuchsia", "green", "lime", "olive", "yellow", "navy", "blue", "teal", "aqua", "orange",
    "aliceblue", "antiquewhite", "aquamarine", "azure", "beige", "bisque", "blanchedalmond", "blueviolet", "brown", "burlywood", "cadetblue", "chartreuse", "chocolate", "coral",
    "cornflowerblue", "cornsilk", "crimson", "cyan", "darkblue", "darkcyan", "darkgoldenrod", "darkgray", "darkgrey", "darkgreen", "darkkhaki", "darkmagenta", "darkolivegreen",
    "darkorange", "darkorchid", "darkred", "darksalmon", "darkseagreen", "darkslateblue", "darkslategray", "darkslategrey", "darkturquoise", "darkviolet", "deeppink", "deepskyblue",
    "dimgray", "dimgrey", "dodgerblue", "firebrick", "floralwhite", "forestgreen", "gainsboro", "ghostwhite", "gold", "goldenrod", "greenyellow", "honeydew", "hotpink", "indianred",
    "indigo", "ivory", "khaki", "lavender", "lavenderblush", "lawngreen", "lemonchiffon", "lightblue", "lightcoral", "lightcyan", "lightgoldenrodyellow", "lightgray", "lightgrey",
    "lightgreen", "lightpink", "lightsalmon", "lightseagreen", "lightskyblue", "lightslategray", "lightslategrey", "lightsteelblue", "lightyellow", "limegreen", "linen", "magenta",
    "mediumaquamarine", "mediumblue", "mediumorchid", "mediumpurple", "mediumseagreen", "mediumslateblue", "mediumspringgreen", "mediumturquoise", "mediumvioletred", "midnightblue",
    "mintcream", "mistyrose", "moccasin", "navajowhite", "oldlace", "olivedrab", "orangered", "orchid", "palegoldenrod", "palegreen", "paleturquoise", "palevioletred", "papayawhip",
    "peachpuff", "peru", "pink", "plum", "powderblue", "rosybrown", "royalblue", "saddlebrown", "salmon", "sandybrown", "seagreen", "seashell", "sienna", "skyblue", "slateblue",
    "slategray", "slategrey", "snow", "springgreen", "steelblue", "tan", "thistle", "tomato", "turquoise", "violet", "wheat", "whitesmoke", "yellowgreen", "rebeccapurple",
];

fn prop_allowed(p: &str) -> bool {
    matches!(
        p,
        "color" | "background-color" | "width" | "height" | "min-width" | "max-width" | "min-height" | "max-height"
            | "line-height" | "text-align" | "vertical-align" | "display" | "background" | "float" | "clear"
            | "white-space" | "word-break" | "table-layout" | "opacity" | "list-style-type"
    ) || ["font-", "text-", "margin", "padding", "border"].iter().any(|pre| p.starts_with(pre))
}

fn value_allowed(prop: &str, val: &str) -> bool {
    let low = val.to_ascii_lowercase();
    // Only colour functions may use parentheses (outside or inside quotes).
    for (i, _) in low.match_indices('(') {
        let name: String = low[..i].chars().rev().take_while(|c| c.is_ascii_alphabetic()).collect::<Vec<_>>().into_iter().rev().collect();
        if !matches!(name.as_str(), "rgb" | "rgba" | "hsl" | "hsla") {
            return false;
        }
    }
    // Negative offsets push text off-screen (`text-indent:-9999px`); nothing legitimate in mail needs them.
    if (prop.starts_with("margin") || prop == "text-indent") && low.split(|c: char| c.is_whitespace() || c == ',').any(|t| t.strip_prefix('-').is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit() || c == '.'))) {
        return false;
    }
    if prop == "display" {
        // `!important` with optional whitespace around the `!`; anything else after `!` is rejected.
        let v = match low.split_once('!') {
            Some((a, b)) if b.trim() == "important" => a.trim(),
            Some(_) => return false,
            None => low.trim(),
        };
        return matches!(
            v,
            "block" | "inline" | "inline-block" | "none" | "table" | "table-cell" | "table-row" | "flex" | "inline-flex" | "list-item"
                | "table-row-group" | "table-header-group" | "table-footer-group"
        );
    }
    !["expression", "javascript", "behavior", "binding", "url", "image-set", "attr"].iter().any(|bad| low.contains(bad))
}

/// Sanitise untrusted HTML for display.
pub fn sanitize_html(html: &str, allow_remote_images: bool) -> SanitizedHtml {
    sanitize_html_with(html, allow_remote_images, &HashSet::new())
}

/// Like [`sanitize_html`], but when remote images are allowed, any `<img>` whose normalised
/// URL (see `normalize_url`; `tracking::tracker_url_set` produces this form) is in
/// `deny_urls` loses its `src` and so never loads.
pub fn sanitize_html_with(html: &str, allow_remote_images: bool, deny_urls: &HashSet<String>) -> SanitizedHtml {
    let blocked = Arc::new(AtomicUsize::new(0));
    let counter = blocked.clone();
    let skipped = Arc::new(AtomicUsize::new(0));
    let skip_counter = skipped.clone();
    let deny = Arc::new(deny_urls.clone());

    let mut b = Builder::default();
    b.link_rel(Some("noopener noreferrer"))
        .url_schemes(HashSet::from(["http", "https", "mailto", "cid"]))
        .clean_content_tags(HashSet::from(["script", "style", "iframe", "object", "embed", "noscript", "template", "svg", "math", "title", "head"]))
        .add_generic_attributes(["style", "dir", "lang"])
        // Presentational attributes that HTML email layouts depend on (no URLs, no script).
        // Without them tables collapse and images end up in the wrong place.
        .add_tag_attributes("table", ["width", "height", "align", "bgcolor", "border", "cellpadding", "cellspacing"])
        .add_tag_attributes("tr", ["align", "valign", "bgcolor"])
        .add_tag_attributes("td", ["width", "height", "align", "valign", "bgcolor", "nowrap"])
        .add_tag_attributes("th", ["width", "height", "align", "valign", "bgcolor", "nowrap"])
        .add_tag_attributes("div", ["align"])
        .add_tag_attributes("p", ["align"])
        .add_tag_attributes("center", ["align"])
        .add_tag_attributes("img", ["align", "border", "hspace", "vspace"])
        .attribute_filter(move |element, attr, value| -> Option<Cow<str>> {
            match (element, attr) {
                ("img", "src") => {
                    let norm = normalize_url(value);
                    if is_remote(&norm) {
                        if allow_remote_images {
                            if deny.contains(&norm) {
                                skip_counter.fetch_add(1, Ordering::Relaxed);
                                return None;
                            }
                            Some(clean_url(value).into())
                        } else {
                            counter.fetch_add(1, Ordering::Relaxed);
                            None
                        }
                    } else if is_safe_inline_src(&norm) {
                        Some(clean_url(value).into())
                    } else {
                        None
                    }
                }
                (_, "style") => filter_style(value).map(Cow::Owned),
                ("a", "href") => Some(value.into()),
                _ => Some(value.into()),
            }
        });
    // `cid:` hrefs make no sense on links; ammonia allows scheme on all URL attrs.
    let html = b.clean(html).to_string();
    SanitizedHtml { html, blocked_remote_count: blocked.load(Ordering::Relaxed), skipped_tracker_count: skipped.load(Ordering::Relaxed) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(h: &str) -> String {
        sanitize_html(h, false).html
    }
    fn lower(h: &str) -> String {
        s(h).to_ascii_lowercase()
    }

    #[test]
    fn script_removed_with_content() {
        let o = lower("<p>a</p><script>alert(1)</script><SCRIPT src=//evil/x.js></SCRIPT>");
        assert!(!o.contains("script") && !o.contains("alert"), "{o}");
        assert!(o.contains("<p>a</p>"));
    }

    #[test]
    fn event_handlers_removed() {
        for h in [
            "<img src=x onerror=alert(1)>",
            "<a href=\"#\" onclick=\"x()\">a</a>",
            "<body onload=alert(1)>hi</body>",
            "<div onmouseover=x()>d</div>",
            "<img src=\"cid:a\" ONERROR=\"x\">",
        ] {
            let o = lower(h);
            assert!(!o.contains("onerror") && !o.contains("onclick") && !o.contains("onload") && !o.contains("onmouseover"), "{o}");
        }
    }

    #[test]
    fn javascript_urls_removed() {
        for h in [
            "<a href=\"javascript:alert(1)\">x</a>",
            "<a href=\" JaVaScRiPt:alert(1)\">x</a>",
            "<a href=\"java&#x09;script:alert(1)\">x</a>",
            "<a href=\"data:text/html;base64,PHNjcmlwdD4=\">x</a>",
            "<a href=\"vbscript:x\">x</a>",
            "<img src=\"javascript:alert(1)\">",
            "<img src=\"data:image/svg+xml;base64,AAAA\">",
        ] {
            let o = lower(h);
            assert!(!o.contains("javascript") && !o.contains("data:") && !o.contains("vbscript"), "{h} -> {o}");
        }
    }

    #[test]
    fn svg_removed() {
        let o = lower("<svg onload=alert(1)><script>x</script><circle r=1/></svg><p>ok</p>");
        assert!(!o.contains("svg") && !o.contains("onload") && !o.contains("circle"), "{o}");
        assert!(o.contains("ok"));
        let o = lower("<math><mi xlink:href=\"javascript:x\">a</mi></math>");
        assert!(!o.contains("javascript") && !o.contains("math"), "{o}");
    }

    #[test]
    fn frames_forms_objects_removed() {
        let o = lower("<iframe src=\"https://evil\"></iframe><form action=\"https://evil\"><input name=p><button>go</button><textarea>t</textarea></form><object data=x></object><embed src=x><applet code=x></applet>");
        for bad in ["iframe", "<form", "<input", "<button", "<textarea", "object", "embed", "applet", "evil"] {
            assert!(!o.contains(bad), "{bad} in {o}");
        }
    }

    #[test]
    fn meta_refresh_base_link_removed() {
        let o = lower("<meta http-equiv=\"refresh\" content=\"0;url=https://evil\"><base href=\"https://evil/\"><link rel=stylesheet href=\"https://evil/x.css\"><p>t</p>");
        assert!(!o.contains("meta") && !o.contains("<base") && !o.contains("<link") && !o.contains("evil"), "{o}");
        assert!(o.contains("<p>t</p>"));
    }

    #[test]
    fn style_element_removed() {
        let o = lower("<style>@import url(https://evil/x.css); body{background:url(https://evil/p)}</style><p>x</p>");
        assert!(!o.contains("style") && !o.contains("evil") && !o.contains("import"), "{o}");
    }

    #[test]
    fn css_url_in_style_attr_removed() {
        for h in [
            "<div style=\"background:url(https://evil/p.gif)\">a</div>",
            "<div style=\"background: URL( 'https://evil' )\">a</div>",
            "<div style=\"background:u\\72l(https://evil)\">a</div>",
            "<div style=\"background:ur/**/l(https://evil)\">a</div>",
            "<div style=\"background:\\75rl(https://evil)\">a</div>",
            "<div style=\"background:\\000075 rl(https://evil)\">a</div>",
            "<div style=\"background:\\55\\52\\4c(https://evil)\">a</div>",
            "<div style=\"width:expression(alert(1))\">a</div>",
            "<div style=\"@import 'https://evil'\">a</div>",
            "<div style=\"behavior:url(x.htc)\">a</div>",
            "<div style=\"position:fixed;top:0;left:0;width:100%;height:100%\">a</div>",
            "<div style=\"background-image:image-set('https://evil' 1x)\">a</div>",
        ] {
            let o = lower(h);
            // Declarations that are safe on their own may survive (reader policy keeps them); nothing may fetch or overlay.
            assert!(!o.contains("evil") && !o.contains("url") && !o.contains("position") && !o.contains("expression") && !o.contains("behavior") && !o.contains("image-set") && !o.contains('@'), "{h} -> {o}");
            assert!(o.contains(">a<"));
        }
    }

    #[test]
    fn q01_comment_inside_string_cannot_hide_url() {
        for h in [
            "<div style=\"font-family:'/*';background:url(https://t.example/p.gif);x:'*/'\">a</div>",
            "<div style=\"font-family:'/*';background-color:red;background-image:url(https://t.example/p.gif);font-family:'*/'\">a</div>",
            "<div style=\"background:url(https://t.example/p.gif)\">a</div>",
            "<div style=\"color:red;background-color:url(https://t.example/p.gif)\">a</div>",
            "<div style=\"font-family:'unterminated;color:red\">a</div>",
        ] {
            let o = lower(h);
            assert!(!o.contains("t.example") && !o.contains("url"), "{h} -> {o}");
        }
    }

    #[test]
    fn q01_allowlisted_styles_kept() {
        let o = s("<p style=\"color:rgb(1,2,3);background-color:#fff;font-family:'Helvetica Neue',Arial;margin:0 auto;border:1px solid #ccc;display:block;text-align:center\">x</p>");
        assert!(o.contains("rgb(1,2,3)") && o.contains("Helvetica Neue"), "{o}");
        assert!(!s("<p style=\"z-index:5\">x</p>").contains("style"));
        assert!(!s("<p style=\"display:grid\">x</p>").contains("style"));
    }

    #[test]
    fn q02_obfuscated_remote_img_blocked() {
        for h in [
            "<img src=\"ht&#9;tps://t.example/p\">",
            "<img src=\"&#1;https://t.example/p\">",
            "<img src=\"ht&#10;tp://t.example/p\">",
            "<img src=\"\\\\t.example/p\">",
            "<img src=\"/\\t.example/p\">",
            "<img src=\"\\/t.example/p\">",
            "<img src=\" \thttps://t.example/p\">",
        ] {
            let r = sanitize_html(h, false);
            assert!(!r.html.contains("t.example"), "{h} -> {}", r.html);
            assert!(!r.html.contains("src"), "{h} -> {}", r.html);
        }
        let r = sanitize_html("<img src=\"ht&#9;tps://t.example/p\"><img src=\"//t.example/q\">", false);
        assert_eq!(r.blocked_remote_count, 2);
    }

    #[test]
    fn q02_only_cid_and_data_image_survive_when_blocking() {
        let r = sanitize_html("<img src=\"cid:a\"><img src=\"relative.png\"><img src=\"ftp://x/y\"><img src=\"data:text/html,x\">", false);
        assert!(r.html.contains("cid:a"), "{}", r.html);
        assert!(!r.html.contains("relative") && !r.html.contains("ftp") && !r.html.contains("data:"), "{}", r.html);
        assert_eq!(r.blocked_remote_count, 0); // ftp is dropped by the scheme allowlist
    }

    #[test]
    fn benign_style_kept() {
        let o = s("<p style=\"color:#333;font-size:14px\">x</p>");
        assert!(o.contains("color:#333"), "{o}");
    }

    #[test]
    fn remote_images_blocked_and_counted() {
        let r = sanitize_html(
            "<img src=\"https://t.example/p.gif\" width=1 height=1><img src=\"http://t.example/q.png\"><img src=\"//t.example/r.png\"><img src=\"HTTPS://T.example/s\"><img src=\"cid:ok\">",
            false,
        );
        assert_eq!(r.blocked_remote_count, 4, "{}", r.html);
        assert!(!r.html.to_ascii_lowercase().contains("t.example"), "{}", r.html);
        assert!(r.html.contains("cid:ok"));
    }

    #[test]
    fn style_keeps_safe_declarations_and_drops_only_the_unsafe_ones() {
        let r = sanitize_html("<img src=\"https://x.example/a.png\" style=\"display:inline-block;outline:none;border:1px solid #ddd;vertical-align:middle;margin-right:6px;background-color:#4D6BFE\">", true);
        assert!(r.html.contains("display:inline-block") && r.html.contains("vertical-align:middle") && r.html.contains("background-color:#4D6BFE"), "{}", r.html);
        assert!(!r.html.contains("outline"), "{}", r.html);
        let bad = sanitize_html("<p style=\"color:red;background:url(https://t.example/p);width:10px\">x</p>", true);
        assert!(bad.html.contains("color:red") && bad.html.contains("width:10px") && !bad.html.contains("url("), "{}", bad.html);
        let esc = sanitize_html("<p style=\"color:red;width:\\31 0px\">x</p>", true);
        assert!(!esc.html.contains("style"), "{}", esc.html);
        let all_bad = sanitize_html("<p style=\"position:fixed;top:0\">x</p>", true);
        assert!(!all_bad.html.contains("style"), "{}", all_bad.html);
    }

    #[test]
    fn remote_images_allowed_when_opted_in() {
        let r = sanitize_html("<img src=\"https://x.example/a.png\">", true);
        assert_eq!(r.blocked_remote_count, 0);
        assert!(r.html.contains("https://x.example/a.png"));
    }

    /// URL paths and query strings are case-sensitive (CDNs, signed S3 links); only the scheme check may be
    /// case-insensitive. A loaded image must keep its exact URL.
    #[test]
    fn tp_allowed_remote_image_keeps_url_case() {
        let r = sanitize_html("<img src=\"https://cdn.example/Images/Logo.PNG?X-Amz-Signature=AbC\">", true);
        assert!(r.html.contains("https://cdn.example/Images/Logo.PNG?X-Amz-Signature=AbC"), "{}", r.html);
        // The deny list still matches case-insensitively (it holds normalised URLs).
        let deny: HashSet<String> = ["https://t.example/open.gif?u=abc".to_string()].into();
        let r = sanitize_html_with("<img src=\"https://T.example/Open.gif?u=ABC\">", true, &deny);
        assert!(!r.html.contains("src"), "{}", r.html);
        assert_eq!(r.skipped_tracker_count, 1);
    }

    /// Obfuscation that only works through normalisation must still be caught when images are allowed, and the
    /// value that reaches the page must be the cleaned one (no tab/newline smuggling).
    #[test]
    fn tp_allowed_remote_image_output_is_cleaned() {
        let r = sanitize_html("<img src=\" ht&#9;tps://cdn.example/A.png\">", true);
        assert!(r.html.contains("src=\"https://cdn.example/A.png\""), "{}", r.html);
        let r = sanitize_html("<img src=\"JaVa&#9;ScRiPt:alert(1)\">", true);
        assert!(!r.html.to_ascii_lowercase().contains("javascript") && !r.html.contains("src"), "{}", r.html);
    }

    /// Hostile and huge input: no panic, nothing executable survives, and the cost stays linear enough to finish.
    #[test]
    fn tp_deeply_nested_and_huge_input() {
        let deep = format!("{}<img src=x onerror=alert(1)>{}", "<div>".repeat(5000), "</div>".repeat(5000));
        let o = lower(&deep);
        assert!(!o.contains("onerror"));
        let wide = "<p style=\"color:red\">\u{1F600} caf\u{e9} \u{202e}x</p>".repeat(20_000);
        let o = s(&wide);
        assert!(o.contains("\u{1F600}") && o.contains("color:red"));
        let attrs = format!("<a href=\"https://ok.example\" {}>x</a>", (0..2000).map(|i| format!("data-a{i}=\"1\"")).collect::<Vec<_>>().join(" "));
        assert!(!s(&attrs).contains("data-a"));
    }

    /// Styles that hide or overlay via properties the reader allowlist must never let through.
    #[test]
    fn tp_style_overlay_and_resource_properties_dropped() {
        for h in [
            "<div style=\"position:absolute;inset:0\">a</div>",
            "<div style=\"z-index:99999\">a</div>",
            "<div style=\"content:url(https://t.example/p)\">a</div>",
            "<div style=\"list-style-image:url(https://t.example/p)\">a</div>",
            "<div style=\"border-image:url(https://t.example/p) 30\">a</div>",
            "<div style=\"cursor:url(https://t.example/c),auto\">a</div>",
            "<div style=\"mask:url(https://t.example/m)\">a</div>",
            "<div style=\"font-family:x;src:url(https://t.example/f)\">a</div>",
            "<div style=\"background-color:var(--x)\">a</div>",
            "<div style=\"width:calc(100% - 1px)\">a</div>",
            "<div style=\"transform:translate(-9999px)\">a</div>",
        ] {
            let o = lower(h);
            for bad in ["t.example", "position", "z-index", "url(", "var(", "calc(", "transform", "inset"] {
                assert!(!o.contains(bad), "{h} -> {o}");
            }
        }
    }

    #[test]
    fn tracking_attrs_not_leaking() {
        // srcset / background / poster / ping are not allowed attributes.
        let o = lower("<img src=\"cid:a\" srcset=\"https://t.example/a 2x\"><table background=\"https://t.example/bg\"><tr><td background=\"https://t.example/bg2\">x</td></tr></table><a href=\"https://l.example\" ping=\"https://t.example/ping\">l</a><video poster=\"https://t.example/p\" src=\"https://t.example/v\"></video><audio src=\"https://t.example/a\"></audio>");
        assert!(!o.contains("t.example"), "{o}");
    }

    #[test]
    fn links_get_rel_and_stay_inert() {
        let o = s("<a href=\"https://example.org/x\" target=\"_blank\">go</a>");
        assert!(o.contains("rel=\"noopener noreferrer\""), "{o}");
        assert!(o.contains("href=\"https://example.org/x\""));
        let o = s("<a href=\"mailto:a@b.c\">m</a>");
        assert!(o.contains("mailto:a@b.c"));
    }

    #[test]
    fn nested_and_malformed_tricks() {
        for h in [
            "<scr<script>ipt>alert(1)</scr</script>ipt>",
            "<img src=\"x\"onerror=\"alert(1)\">",
            "<<script>alert(1)//<</script>",
            "<div><!--[if gte mso 9]><script>alert(1)</script><![endif]--></div>",
            "<noscript><p title=\"</noscript><img src=x onerror=alert(1)>\">",
            "<a href=\"&#106;avascript:alert(1)\">x</a>",
            "<select><template><img src=x onerror=alert(1)></template></select>",
        ] {
            let o = lower(h);
            assert!(!o.contains("onerror") && !o.contains("<script") && !o.contains("javascript:"), "{h} -> {o}");
        }
    }

    #[test]
    fn plain_content_preserved() {
        let o = s("<h1>T</h1><p>a <b>b</b> <i>c</i></p><ul><li>x</li></ul><table><tr><td>1</td></tr></table>");
        for t in ["<h1>", "<b>b</b>", "<ul>", "<table>", "<td>1</td>"] {
            assert!(o.contains(t), "{t} {o}");
        }
    }

    #[test]
    fn empty_input() {
        let r = sanitize_html("", false);
        assert_eq!(r.html, "");
        assert_eq!(r.blocked_remote_count, 0);
    }

    #[test]
    fn keeps_email_layout_attributes_but_stays_safe() {
        let r = sanitize_html(
            r##"<table width="600" align="center" cellpadding="0" cellspacing="0" bgcolor="#fff" background="https://t.example/b.gif" onload="x()"><tr><td valign="top" width="300" style="float:left;background:#eee"><img src="https://x.example/a.png" width="300" align="left"></td></tr></table>"##,
            true,
        );
        for want in ["width=\"600\"", "align=\"center\"", "cellpadding", "bgcolor", "valign=\"top\"", "float:left"] {
            assert!(r.html.contains(want), "missing {want}: {}", r.html);
        }
        assert!(!r.html.contains("background=\""), "{}", r.html);
        assert!(!r.html.contains("onload"), "{}", r.html);
        let r2 = sanitize_html(r#"<td style="background:#eee url(https://t.example/p.gif)">x</td>"#, true);
        assert!(!r2.html.contains("url("), "{}", r2.html);
    }

    #[test]
    fn deny_set_drops_listed_images_only() {
        let html = "<img src=\"https://t.example/open.gif?u=A&amp;b=1\"><img src=\"https://cdn.example/banner.png\"><img src=\"cid:x\">";
        let deny: HashSet<String> = ["https://t.example/open.gif?u=a&b=1".to_string()].into();
        let r = sanitize_html_with(html, true, &deny);
        assert!(!r.html.contains("t.example"), "{}", r.html);
        assert!(r.html.contains("https://cdn.example/banner.png"), "{}", r.html);
        assert_eq!((r.blocked_remote_count, r.skipped_tracker_count), (0, 1));
        // not allowed at all: everything blocked as before
        let r = sanitize_html_with(html, false, &deny);
        assert_eq!(r.blocked_remote_count, 2);
        assert!(!r.html.contains("cdn.example"));
    }
    #[test]
    fn style_hides_is_numeric() {
        for h in [
            "opacity:-1", "opacity:+0", "opacity:0e0", "opacity:0.001", "opacity:.09", "opacity:0%", "OPACITY : 0 ! important",
            "font-size:0e0px", "font-size:+0px", "font-size:0.01px", "font-size:1.9px", "font-size:0", "font-size:0pt", "font-size:0.1em",
            "color:transparent", "color:rgba(0,0,0,0)", "color:RGBA(1, 2, 3, 0.05)", "color:rgb(1 2 3 / 0)", "color:#0000", "color:#11223300",
            "display:none", "visibility:collapse", "font:0px/1 Arial",
        ] {
            assert!(style_hides(h), "{h} should hide");
        }
        for h in [
            "font-size:small", "font-size:medium", "font-size:large", "font-size:smaller", "font-size:inherit", "font-size:2px", "font-size:1em",
            "opacity:1", "opacity:0.1", "opacity:inherit", "opacity:50%", "color:red", "color:rgba(0,0,0,0.5)", "color:#000", "color:#00000080",
            "background-color:transparent", "background:rgba(0,0,0,0)", "font:12px/0 Arial", "line-height:0",
        ] {
            assert!(!style_hides(h), "{h} should not hide");
        }
    }

    #[test]
    fn reader_keeps_normal_layout_styles_but_drops_negative_offsets() {
        let o = s("<div style=\"margin:0 auto;margin-top:12px;padding:4px;text-indent:1em;opacity:0;font-size:0;width:600px\">x</div>");
        for keep in ["margin:0 auto", "margin-top:12px", "padding:4px", "text-indent:1em", "width:600px", "opacity:0", "font-size:0"] {
            assert!(o.contains(keep), "{keep}: {o}");
        }
        let o = s("<div style=\"margin-left:-9999px;text-indent:-9999px;margin:-1px;color:red\">x</div>");
        assert!(!o.contains("-9999") && !o.contains("margin") && o.contains("color:red"), "{o}");
    }

    #[test]
    fn q02_background_shorthand_with_url_keeps_only_the_colour() {
        let o = s("<table><tr><td style=\"background:#1a1a1a url(hero.jpg) no-repeat;color:#fff\">x</td></tr></table>");
        assert!(o.contains("background-color:#1a1a1a") && o.contains("color:#fff") && !o.contains("url") && !o.contains("hero"), "{o}");
        let o = s("<table><tr><td style=\"background:url('https://evil/x.png') rgb(10, 20, 30)\">x</td></tr></table>");
        assert!(o.contains("background-color:rgb(10, 20, 30)") && !o.contains("evil") && !o.contains("url"), "{o}");
        let o = s("<table><tr><td style=\"background: Navy url(a.png)\">x</td></tr></table>");
        assert!(o.contains("background-color:navy"), "{o}");
        // No valid colour token: nothing is kept, and a bogus token is not trusted.
        let o = s("<table><tr><td style=\"background:url(a.png) expression(x) no-repeat\">x</td></tr></table>");
        assert!(!o.contains("background") && !o.contains("expression"), "{o}");
        let o = s("<table><tr><td style=\"background:url(a.png) rgb(javascript:1)\">x</td></tr></table>");
        assert!(!o.contains("background") && !o.contains("javascript"), "{o}");
        // A plain colour shorthand is unchanged.
        assert!(s("<table><tr><td style=\"background:#fff\">x</td></tr></table>").contains("background:#fff"));
    }

    #[test]
    fn q04_important_with_space_and_more_display_values() {
        for v in ["none ! important", "none!important", "none !important", "none !  IMPORTANT"] {
            let o = s(&format!("<span style=\"display:{v}\">pre</span>"));
            assert!(o.contains("display:none"), "{v} -> {o}");
        }
        assert!(style_hides("display:none ! important"));
        for v in ["flex", "inline-flex", "list-item", "table-row-group", "table-header-group", "table-footer-group"] {
            assert!(s(&format!("<div style=\"display:{v}\">a</div>")).contains(&format!("display:{v}")), "{v}");
        }
        for v in ["contents", "grid", "none ! foo", "flex url(x)", "none;position:fixed"] {
            let o = s(&format!("<div style=\"display:{v}\">a</div>"));
            assert!(!o.contains("display:contents") && !o.contains("display:grid") && !o.contains("position") && !o.contains("foo"), "{v} -> {o}");
        }
    }

    #[test]
    fn q06_newsletter_fixture_keeps_layout_and_hides_preheader() {
        let html = r##"<html><head><style>.x{display:none}</style></head><body bgcolor="#f4f4f4">
<div style="display:none;font-size:1px;color:#f4f4f4;line-height:1px;max-height:0;opacity:0;overflow:hidden;mso-hide:all">PREHEADER SECRET</div>
<span style="mso-hide:all;display:none ! important">MSO SECRET</span>
<table width="600" align="center" cellpadding="0" cellspacing="0" style="margin:0 auto;mso-table-lspace:0pt;mso-table-rspace:0pt;border-collapse:collapse">
<tr><td bgcolor="#1a1a1a" background="https://x/hero.jpg" style="background:#1a1a1a url(https://x/hero.jpg) no-repeat center;padding:24px;color:#ffffff;mso-line-height-rule:exactly">
<h1 style="color:#ffffff;font-size:28px;margin:0">Big Sale</h1></td></tr>
<tr><td style="line-height:0;font-size:0;height:20px">&nbsp;</td></tr>
<tr><td style="font-size:0"><span style="font-size:16px;color:#333333">Visible body text</span></td></tr>
</table></body></html>"##;
        let o = s(html);
        assert!(!o.contains("SECRET") || o.contains("display:none"), "{o}");
        // Preheaders stay hidden: the hiding declaration survives on the element.
        assert!(o.contains("display:none") && o.contains("opacity:0"), "{o}");
        assert!(o.contains("Big Sale") && o.contains("Visible body text"), "{o}");
        for kept in ["margin:0 auto", "line-height:0", "color:#ffffff", "background-color:#1a1a1a", "font-size:0", "padding:24px", "bgcolor=\"#1a1a1a\"", "align=\"center\""] {
            assert!(o.contains(kept), "{kept} missing in {o}");
        }
        assert!(!o.contains("url(") && !o.contains("hero.jpg") && !o.contains("mso-"), "{o}");
        assert!(!o.contains("<style") && !o.contains(".x{"), "{o}");
    }
}

/// The Reader's link gate (app/src/lib/linkgate.ts `holdHtml`) relies on this output shape: only `<a href>` survives as a
/// link; names are lower-case, values double-quoted with `"`, `<`, `>` escaped; no comments, raw-text elements, svg,
/// area hrefs, targets, forms, meta refresh, tabindex, ping or data-* attributes.
#[cfg(test)]
mod reader_link_fixture {
    #[test]
    fn hostile_links_leave_only_plain_a_href() {
        let h = r#"<A HREF="https://a.example/1" TARGET="_blank" style="pointer-events:auto!important;color:red">up</A><map name=m><area href="https://a.example/2" shape=rect coords="0,0,1,1" target=_top></map><svg><a xlink:href="https://a.example/3">s</a></svg><form action="https://a.example/4"><button formaction="https://a.example/5">b</button></form><meta http-equiv="refresh" content="0;url=https://a.example/6"><a href='https://a.example/7' data-mm-href="https://evil" tabindex=1 ping="https://a.example/8">q</a><xmp><a href="https://a.example/9">x</a></xmp><!-- <a href="https://a.example/10"> --><p title='x" <a href="https://a.example/11">'>t</p>"#;
        let o = super::sanitize_html(h, false).html;
        assert_eq!(
            o,
            r#"<a href="https://a.example/1" style="color:red" rel="noopener noreferrer">up</a><map><area></map>b<a href="https://a.example/7" rel="noopener noreferrer">q</a>&lt;a href="https://a.example/9"&gt;x&lt;/a&gt;<p title="x&quot; &lt;a href=&quot;https://a.example/11&quot;&gt;">t</p>"#
        );
        for gone in ["target", "xlink", "action", "refresh", "tabindex", "ping", "data-mm", "pointer-events", "<svg", "<form", "<meta", "<!--"] {
            assert!(!o.to_ascii_lowercase().contains(gone), "{gone} in {o}");
        }
    }
}
