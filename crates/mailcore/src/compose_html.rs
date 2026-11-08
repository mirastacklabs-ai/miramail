//! Sanitiser for HTML the user composes (rich editor output, pasted HTML, rich signatures).
//!
//! Stricter than the reader's policy in one way (no remote image of any kind survives, and nothing may
//! reference `cid:`) and looser in another (`data:image/{png,jpeg,gif,webp};base64,...` inline images are kept so
//! the editor can show pasted pictures). Scripts, event handlers, forms, frames, `<style>`, svg, `javascript:` and
//! `data:` links are removed; inline `style` is limited to the same allow-list the reader uses.

use crate::sanitize::{filter_style, normalize_url, style_hides, style_is_dangerous};
use ammonia::Builder;
use std::borrow::Cow;
use std::collections::HashSet;

/// Largest HTML body the composer accepts (inline pasted images included).
pub const MAX_COMPOSE_HTML: usize = 8 * 1024 * 1024;

/// Most tables kept in one message, deepest nesting, most cells (`td`/`th`) and largest `colspan`/`rowspan`. Past a
/// limit the table tags are unwrapped (the text stays), so a hostile paste cannot make the recipient's client choke.
pub const MAX_TABLES: usize = 50;
pub const MAX_TABLE_DEPTH: usize = 6;
pub const MAX_CELLS: usize = 10_000;
pub const MAX_SPAN: u32 = 50;

const TABLE_TAGS: [&str; 7] = ["table", "thead", "tbody", "tfoot", "tr", "th", "td"];

/// `NN` or `NN%` (1-4 digits), the only width/height forms the table attributes accept.
fn valid_dim(v: &str) -> Option<String> {
    let t = v.trim();
    let digits = t.strip_suffix('%').unwrap_or(t);
    (!digits.is_empty() && digits.len() <= 4 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| t.to_string())
}

/// `#rgb`, `#rrggbb` or a colour name.
fn valid_color(v: &str) -> Option<String> {
    let t = v.trim();
    let hex = t.strip_prefix('#').is_some_and(|h| matches!(h.len(), 3 | 6) && h.bytes().all(|b| b.is_ascii_hexdigit()));
    let name = (3..=20).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphabetic());
    (hex || name).then(|| t.to_string())
}

fn small_number(v: &str, max: u32) -> Option<String> {
    let t = v.trim();
    (!t.is_empty() && t.len() <= 4 && t.bytes().all(|b| b.is_ascii_digit())).then(|| t.parse::<u32>().ok()).flatten().map(|n| n.min(max).to_string())
}

/// Validate one presentational attribute of a table element; `None` drops it.
fn table_attr(element: &str, attr: &str, v: &str) -> Option<String> {
    match (element, attr) {
        ("table" | "td" | "th", "width" | "height") => valid_dim(v),
        ("table", "cellpadding" | "cellspacing") => small_number(v, 50),
        ("table", "border") => small_number(v, 10),
        (_, "dir") => matches!(v.trim().to_ascii_lowercase().as_str(), "ltr" | "rtl" | "auto").then(|| v.trim().to_ascii_lowercase()),
        (_, "align") => matches!(v.trim().to_ascii_lowercase().as_str(), "left" | "right" | "center" | "justify").then(|| v.trim().to_ascii_lowercase()),
        ("thead" | "tbody" | "tfoot" | "tr" | "td" | "th", "valign") => matches!(v.trim().to_ascii_lowercase().as_str(), "top" | "middle" | "bottom" | "baseline").then(|| v.trim().to_ascii_lowercase()),
        ("table" | "tr" | "td" | "th", "bgcolor") => valid_color(v),
        ("td" | "th", "colspan" | "rowspan") => small_number(v, MAX_SPAN).filter(|n| n != "0"),
        _ => None,
    }
}

/// A `data:image/...;base64,<payload>` URI of a raster type, with a clean base64 payload.
fn is_safe_data_image(norm: &str) -> bool {
    let Some(rest) = ["png", "jpeg", "gif", "webp"].iter().find_map(|t| norm.strip_prefix(&format!("data:image/{t};base64,"))) else { return false };
    !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

fn is_safe_link(norm: &str) -> bool {
    norm.starts_with("http:") || norm.starts_with("https:") || norm.starts_with("mailto:")
}

/// Sanitise composer HTML. Always returns well-formed, inert markup.
pub fn sanitize_compose_html(html: &str) -> String {
    let mut b = Builder::default();
    b.link_rel(Some("noopener noreferrer"))
        // `data` is admitted by the scheme check only so pasted images survive; the filter below restricts it to `img src`.
        .url_schemes(HashSet::from(["http", "https", "mailto", "data"]))
        .clean_content_tags(HashSet::from(["script", "style", "iframe", "object", "embed", "noscript", "template", "svg", "math", "title", "head"]))
        .add_tags(["font", "u", "s", "strike", "div", "span"])
        // Layout helpers we never emit; the table attributes below are re-validated one by one in the filter.
        .rm_tags(["col", "colgroup"])
        .add_tag_attributes("table", ["width", "height", "align", "cellpadding", "cellspacing", "border", "bgcolor"])
        .add_tag_attributes("thead", ["valign"])
        .add_tag_attributes("tbody", ["valign"])
        .add_tag_attributes("tfoot", ["valign"])
        .add_tag_attributes("tr", ["valign", "bgcolor"])
        .add_tag_attributes("td", ["width", "height", "align", "valign", "bgcolor"])
        .add_tag_attributes("th", ["width", "height", "align", "valign", "bgcolor"])
        .add_tag_attributes("font", ["face", "size", "color"])
        .add_generic_attributes(["style", "dir"])
        .add_tag_attributes("div", ["align"])
        .add_tag_attributes("p", ["align"])
        .attribute_filter(|element, attr, value| -> Option<Cow<str>> {
            match (element, attr) {
                ("img", "src") => {
                    let norm = normalize_url(value);
                    // Keep the original spelling of the payload (base64 is case sensitive).
                    is_safe_data_image(&norm).then(|| Cow::Owned(value.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect::<String>().trim().to_string()))
                }
                // A hiding style (display:none, opacity:0, ...) marks the element for removal below: the composer must
                // never send text the user cannot see. `mm-hidden` has no colon, so input cannot forge it by accident
                // (and forging it only removes the sender's own content).
                (_, "style") if style_hides(value) => Some(Cow::Borrowed(HIDDEN_MARK)),
                // Table elements keep their safe declarations and lose the rest (pasted tables carry many harmless-but-odd
                // properties); everything else is all-or-nothing as before.
                (e, "style") if TABLE_TAGS.contains(&e) => filter_style(value).map(Cow::Owned),
                (_, "style") => (!style_is_dangerous(value)).then(|| value.into()),
                (e, _) if TABLE_TAGS.contains(&e) => table_attr(e, attr, value).map(Cow::Owned),
                (_, "href" | "cite") => is_safe_link(&normalize_url(value)).then(|| value.into()),
                (_, "src" | "background" | "poster" | "action" | "formaction" | "data" | "srcset") => None,
                _ => Some(value.into()),
            }
        });
    limit_tables(&remove_hidden(&b.clean(html).to_string()))
}

const HIDDEN_MARK: &str = "mm-hidden";
const VOID: [&str; 14] = ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

/// End of the tag starting at `i` (index just past its `>`), skipping quoted attribute values.
fn tag_end(b: &[u8], mut i: usize) -> usize {
    let mut q = 0u8;
    while i < b.len() {
        match (q, b[i]) {
            (0, b'"' | b'\'') => q = b[i],
            (0, b'>') => return i + 1,
            (c, x) if c != 0 && c == x => q = 0,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Drop every element (with its content) that the filter above marked as hidden. Works on ammonia's output, which is
/// well-formed with lowercase tag names and quoted attributes.
fn remove_hidden(html: &str) -> String {
    if !html.contains(HIDDEN_MARK) {
        return html.to_string();
    }
    let b = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let (mut i, mut last) = (0usize, 0usize);
    let name_of = |tag: &str| -> String { tag.trim_start_matches(['<', '/']).chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase() };
    while i < b.len() {
        if b[i] != b'<' || !b.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
            i += 1;
            continue;
        }
        let end = tag_end(b, i);
        let tag = &html[i..end];
        if !tag.contains(&format!("style=\"{HIDDEN_MARK}\"")) {
            i = end;
            continue;
        }
        out.push_str(&html[last..i]);
        let name = name_of(tag);
        let mut j = end;
        if !VOID.contains(&name.as_str()) {
            let mut depth = 1usize;
            while j < b.len() && depth > 0 {
                if b[j] == b'<' && b.get(j + 1).is_some_and(|c| *c == b'/' || c.is_ascii_alphabetic()) {
                    let e = tag_end(b, j);
                    let t = &html[j..e];
                    if name_of(t) == name {
                        if t.starts_with("</") { depth -= 1 } else { depth += 1 }
                    }
                    j = e;
                } else {
                    j += 1;
                }
            }
        }
        i = j;
        last = j;
    }
    out.push_str(&html[last.min(html.len())..]);
    out
}

fn tag_name(tag: &str) -> String {
    tag.trim_start_matches(['<', '/']).chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase()
}

/// Enforce the table limits on ammonia's (well-formed) output by removing the table tags of whatever is over a limit:
/// more than [`MAX_TABLES`] tables, nesting deeper than [`MAX_TABLE_DEPTH`], or a top-level table that would push the
/// message past [`MAX_CELLS`] cells (that whole table is unwrapped, never half of it). The text inside stays, and
/// every removed cell / row leaves a space / line break behind so the text does not run together.
fn limit_tables(html: &str) -> String {
    if !html.contains("<table") {
        return html.to_string();
    }
    let b = html.as_bytes();
    // Pass 1: cells inside each table (nested tables included), indexed by table open order.
    let mut cells_in: Vec<usize> = Vec::new();
    {
        let (mut i, mut open): (usize, Vec<(usize, usize)>) = (0, Vec::new());
        while i < b.len() {
            if b[i] != b'<' || !b.get(i + 1).is_some_and(|c| *c == b'/' || c.is_ascii_alphabetic()) {
                i += 1;
                continue;
            }
            let end = tag_end(b, i);
            let tag = &html[i..end];
            match (tag_name(tag).as_str(), tag.starts_with("</")) {
                ("table", false) => {
                    cells_in.push(0);
                    open.push((cells_in.len() - 1, 0));
                }
                ("table", true) => {
                    if let Some((idx, n)) = open.pop() {
                        cells_in[idx] = n;
                        if let Some(p) = open.last_mut() {
                            p.1 += n;
                        }
                    }
                }
                ("td" | "th", false) => {
                    if let Some(t) = open.last_mut() {
                        t.1 += 1;
                    }
                }
                _ => {}
            }
            i = end;
        }
        for (idx, n) in open.into_iter().rev() {
            cells_in[idx] = n; // unclosed tables (cannot happen in ammonia output, but never index out of range)
        }
    }
    let mut out = String::with_capacity(html.len());
    let (mut i, mut last) = (0usize, 0usize);
    let mut stack: Vec<bool> = Vec::new();
    let mut tables: Vec<bool> = Vec::new();
    let (mut n_tables, mut n_cells, mut bad, mut seen, mut kept_open) = (0usize, 0usize, 0usize, 0usize, 0usize);
    while i < b.len() {
        if b[i] != b'<' || !b.get(i + 1).is_some_and(|c| *c == b'/' || c.is_ascii_alphabetic()) {
            i += 1;
            continue;
        }
        let end = tag_end(b, i);
        let tag = &html[i..end];
        let name = tag_name(tag);
        if !TABLE_TAGS.contains(&name.as_str()) {
            i = end;
            continue;
        }
        let closing = tag.starts_with("</");
        let keep = if closing {
            if name == "table" {
                match tables.pop() {
                    Some(false) => bad -= 1,
                    Some(true) => kept_open -= 1,
                    None => {}
                }
            }
            stack.pop().unwrap_or(true)
        } else {
            let parent_ok = bad == 0;
            let k = match name.as_str() {
                "table" => {
                    let own = cells_in.get(seen).copied().unwrap_or(0);
                    seen += 1;
                    let top = kept_open == 0; // not inside a kept table: pays for its own cells
                    let k = parent_ok && tables.len() < MAX_TABLE_DEPTH && n_tables < MAX_TABLES && (!top || n_cells + own <= MAX_CELLS);
                    if k && top {
                        n_cells += own;
                    }
                    n_tables += usize::from(k);
                    kept_open += usize::from(k);
                    tables.push(k);
                    bad += usize::from(!k);
                    k
                }
                _ => parent_ok,
            };
            stack.push(k);
            k
        };
        if !keep {
            out.push_str(&html[last..i]);
            if closing {
                out.push_str(match name.as_str() {
                    "td" | "th" => " ",
                    "tr" | "table" => "<br>",
                    _ => "",
                });
            }
            last = end;
        }
        i = end;
    }
    out.push_str(&html[last..]);
    out
}

#[cfg(test)]
mod tests {
    /// The editor's restore path (app/src/lib/composetext.ts safeInitialHtml) carries the signature and quote markers
    /// through this sanitiser on `lang`, because it strips data-* attributes. This test pins that contract.
    #[test]
    fn restore_markers_survive_on_lang() {
        let out = sanitize_compose_html(
            "<div lang=\"x-mm-sig:abc123\"><div>Ada</div></div><div lang=\"x-mm-quote\"><div>On Mon, B wrote:</div><blockquote>q</blockquote></div>",
        );
        assert!(out.contains("lang=\"x-mm-sig:abc123\""), "signature marker lost: {out}");
        assert!(out.contains("lang=\"x-mm-quote\""), "quote marker lost: {out}");
        let stripped = sanitize_compose_html("<div data-mm-sig=\"abc123\">Ada</div>");
        assert!(!stripped.contains("data-mm-sig"), "data-* must still be stripped: {stripped}");
    }

    use super::*;

    fn lower(h: &str) -> String {
        sanitize_compose_html(h).to_ascii_lowercase()
    }

    #[test]
    fn keeps_editor_formatting() {
        let o = sanitize_compose_html(
            "<div><b>b</b><i>i</i><u>u</u><s>s</s><strike>k</strike><ul><li>x</li></ul><ol><li>y</li></ol><blockquote>q</blockquote><span style=\"color: rgb(255, 0, 0); font-family: Georgia; font-size: x-large\">c</span><a href=\"https://example.org\">l</a><font face=\"Arial\" size=\"4\">f</font></div>",
        );
        for want in ["<b>b</b>", "<i>i</i>", "<u>u</u>", "<s>s</s>", "<ul>", "<ol>", "<blockquote>", "rgb(255, 0, 0)", "Georgia", "x-large", "href=\"https://example.org\"", "face=\"Arial\""] {
            assert!(o.contains(want), "missing {want}: {o}");
        }
    }

    #[test]
    fn xss_payloads_are_neutralised() {
        for h in [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "<a href=\"javascript:alert(1)\">x</a>",
            "<a href=\" JaVaScRiPt:alert(1)\">x</a>",
            "<a href=\"java&#x09;script:alert(1)\">x</a>",
            "<a href=\"data:text/html;base64,PHNjcmlwdD4=\">x</a>",
            "<a href=\"vbscript:x\">x</a>",
            "<svg onload=alert(1)><circle/></svg>",
            "<iframe src=\"https://evil.example\"></iframe>",
            "<form action=\"https://evil.example\"><input name=a><button>b</button></form>",
            "<object data=\"https://evil.example\"></object><embed src=\"https://evil.example\">",
            "<div onmouseover=\"alert(1)\" style=\"background:url(https://evil.example/p)\">x</div>",
            "<style>@import url(https://evil.example/x.css)</style>",
            "<meta http-equiv=refresh content=\"0;url=https://evil.example\">",
            "<base href=\"https://evil.example/\"><link rel=stylesheet href=\"https://evil.example/x.css\">",
            "<body onload=alert(1)>b</body>",
            "<math><mi xlink:href=\"javascript:x\">a</mi></math>",
            "<scr<script>ipt>alert(1)</scr</script>ipt>",
            "<img src=\"data:image/svg+xml;base64,PHN2ZyBvbmxvYWQ9YWxlcnQoMSk+\">",
            "<img src=\"data:text/html;base64,PHNjcmlwdD4=\">",
            "<img src=\"data:image/png;base64,AAAA\" onerror=\"alert(1)\">",
            "<img src=\"data:image/png;base64,AAAA\" srcset=\"https://evil.example/a 2x\">",
            "<table background=\"https://evil.example/bg\"><tr><td>x</td></tr></table>",
            "<video poster=\"https://evil.example/p\" src=\"https://evil.example/v\"></video>",
        ] {
            let o = lower(h);
            for bad in ["<script", "onerror", "onload", "onmouseover", "javascript", "vbscript", "evil.example", "<svg", "<iframe", "<form", "<input", "<object", "<embed", "<style", "<meta", "<base", "<link", "<math", "http-equiv", "text/html", "image/svg", "srcset"] {
                assert!(!o.contains(bad), "{bad} survived in {h} -> {o}");
            }
            assert!(!o.contains("href=\"data:"), "{h} -> {o}");
        }
    }

    #[test]
    fn remote_and_cid_images_are_dropped() {
        for h in [
            "<img src=\"https://t.example/p.gif\">",
            "<img src=\"http://t.example/p.gif\">",
            "<img src=\"//t.example/p.gif\">",
            "<img src=\"HTTPS://T.example/p.gif\">",
            "<img src=\"ht&#9;tps://t.example/p\">",
            "<img src=\"cid:abc\">",
            "<img src=\"relative.png\">",
            "<img src=\"ftp://t.example/x\">",
            "<img src=\"\\\\t.example/p\">",
        ] {
            let o = lower(h);
            assert!(!o.contains("src"), "{h} -> {o}");
            assert!(!o.contains("t.example") && !o.contains("cid:"), "{h} -> {o}");
        }
    }

    #[test]
    fn inline_data_images_survive_with_payload_intact() {
        let o = sanitize_compose_html("<p><img src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAHnOcQAAAAABJRU5ErkJggg==\" alt=\"x\"></p>");
        assert!(o.contains("src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAHnOcQAAAAABJRU5ErkJggg==\""), "{o}");
        for ok in ["jpeg", "gif", "webp"] {
            assert!(sanitize_compose_html(&format!("<img src=\"data:image/{ok};base64,AAAA\">")).contains("data:image/"), "{ok}");
        }
        // Malformed payloads are not images.
        assert!(!sanitize_compose_html("<img src=\"data:image/png;base64,AA\"onerror=x>").contains("onerror"));
        assert!(!sanitize_compose_html("<img src=\"data:image/png;base64,AA AA<script>\">").to_ascii_lowercase().contains("script"));
    }

    #[test]
    fn links_are_limited_to_web_and_mail() {
        let o = sanitize_compose_html("<a href=\"https://a.example\">1</a><a href=\"mailto:x@y.z\">2</a><a href=\"tel:123\">3</a><a href=\"/rel\">4</a>");
        assert!(o.contains("https://a.example") && o.contains("mailto:x@y.z"), "{o}");
        assert!(!o.contains("tel:") && !o.contains("/rel"), "{o}");
        assert!(o.contains("rel=\"noopener noreferrer\""), "{o}");
    }

    #[test]
    fn dangerous_styles_are_dropped_safe_ones_kept() {
        assert!(!lower("<span style=\"position:fixed;top:0\">x</span>").contains("style"));
        assert!(!lower("<span style=\"background:url(https://evil.example/p)\">x</span>").contains("evil"));
        assert!(!lower("<span style=\"width:expression(alert(1))\">x</span>").contains("expression"));
        assert!(sanitize_compose_html("<span style=\"color:#336699;font-weight:bold\">x</span>").contains("color:#336699"));
    }

    #[test]
    fn q02_hidden_nodes_are_removed_not_revealed() {
        for hide in ["display:none", "display: NONE !important", "opacity:0", "opacity: 0.0", "font-size:0", "font-size: 0px", "visibility:hidden", "color:red;display:none"] {
            let o = sanitize_compose_html(&format!("<p>shown <span style=\"{hide}\">SECRET <b>deep</b> <span>nested</span></span>after</p>"));
            assert!(!o.contains("SECRET") && !o.contains("deep") && !o.contains("nested") && !o.contains("mm-hidden"), "{hide}: {o}");
            assert!(o.contains("shown") && o.contains("after"), "{hide}: {o}");
            assert!(o.starts_with("<p>") && o.ends_with("</p>"), "{hide}: {o}");
        }
        // Hidden void element and a hidden block among visible siblings.
        let o = sanitize_compose_html("<div>a</div><div style=\"display:none\"><div>x</div>y</div><div>b</div><br style=\"display:none\">");
        assert_eq!(o, "<div>a</div><div>b</div>");
        // Forged marker alone is not a style at all.
        assert!(!sanitize_compose_html("<span style=\"mm-hidden\">visible</span>").contains("mm-hidden"));
        // Visible look-alikes survive.
        let o = sanitize_compose_html("<span style=\"opacity:0.5;font-size:10px;display:inline\">ok</span>");
        assert!(o.contains("ok") && o.contains("opacity:0.5"), "{o}");
    }

    // ---- tables and email constructs ----

    const DATA_TABLE: &str = "<table width=\"100%\" cellpadding=\"6\" cellspacing=\"0\" border=\"1\" align=\"center\" bgcolor=\"#ffffff\" style=\"border-collapse:collapse;width:100%\"><thead><tr><th align=\"left\" valign=\"top\" width=\"50%\" bgcolor=\"#eeeeee\" style=\"border:1px solid #bbbbbb;padding:6px;font-weight:bold;background-color:#eeeeee;text-align:left;vertical-align:top\">Name</th><th colspan=\"2\" rowspan=\"1\">Qty</th></tr></thead><tbody><tr bgcolor=\"#f5f7fa\" valign=\"middle\"><td width=\"50\" style=\"color:#112233;font-family:Arial;height:20px\">A</td><td>1</td><td>2</td></tr></tbody></table>";

    #[test]
    fn data_table_survives_with_attributes_and_styles() {
        let o = sanitize_compose_html(DATA_TABLE);
        for want in [
            "<table", "<thead>", "<tbody>", "<tr", "<th", "<td", "width=\"100%\"", "cellpadding=\"6\"", "cellspacing=\"0\"", "border=\"1\"", "align=\"center\"", "bgcolor=\"#ffffff\"",
            "valign=\"top\"", "valign=\"middle\"", "colspan=\"2\"", "rowspan=\"1\"", "border-collapse:collapse", "background-color:#eeeeee", "text-align:left", "vertical-align:top", "padding:6px", "font-weight:bold",
            "color:#112233", "height:20px", "width=\"50%\"",
        ] {
            assert!(o.contains(want), "missing {want}: {o}");
        }
    }

    #[test]
    fn table_style_keeps_safe_declarations_only() {
        let o = sanitize_compose_html("<table><tr><td style=\"mso-number-format:0;color:red;position:absolute;background:url(https://evil.example/x);border:1px solid #000\">x</td></tr></table>");
        assert!(o.contains("color:red") && o.contains("border:1px solid #000"), "{o}");
        for bad in ["mso-", "position", "url", "evil.example"] {
            assert!(!o.contains(bad), "{bad}: {o}");
        }
        for h in [
            "<td style=\"background-image:url(https://evil.example/x)\">x</td>",
            "<td style=\"background:url(//evil.example/x)\">x</td>",
            "<td style=\"width:expression(alert(1))\">x</td>",
            "<td style=\"behavior:url(x.htc)\">x</td>",
            "<td style=\"@import 'x'\">x</td>",
            "<td style=\"color:red;/* c */background:red\">x</td>",
        ] {
            let o = format!("<table><tr>{h}</tr></table>");
            let o = lower(&o);
            for bad in ["url", "expression", "behavior", "@import", "evil", "/*"] {
                assert!(!o.contains(bad), "{bad} in {h}: {o}");
            }
        }
    }

    #[test]
    fn table_attack_attributes_are_dropped() {
        let o = lower("<table background=\"https://evil.example/bg\" onclick=\"x()\" width=\"javascript:1\" border=\"1;url(x)\"><tr background=\"https://evil.example/r\" bgcolor=\"url(https://evil.example)\"><td background=\"//evil.example/c\" bgcolor=\"red\" width=\"10px\" headers=\"a\" char=\"x\" nowrap>x</td></tr></table>");
        for bad in ["background=", "evil", "onclick", "javascript", "url(", "headers", "char=", "nowrap", "width=\"10px"] {
            assert!(!o.contains(bad), "{bad}: {o}");
        }
        assert!(o.contains("bgcolor=\"red\""), "{o}");
        // Scripts and forms inside cells are still removed.
        let o = lower("<table><tr><td><script>alert(1)</script><form action=\"https://evil.example\"><input name=a></form><a href=\"javascript:alert(1)\" onclick=\"x()\">l</a></td></tr></table>");
        for bad in ["<script", "<form", "<input", "javascript", "onclick", "evil"] {
            assert!(!o.contains(bad), "{bad}: {o}");
        }
        // style/link/base/colgroup never survive.
        let o = lower("<table><colgroup><col width=\"10\"></colgroup><style>td{background:url(https://evil.example)}</style><tr><td>x</td></tr></table><link rel=stylesheet href=\"https://evil.example/a.css\"><base href=\"https://evil.example/\">");
        for bad in ["<style", "<link", "<base", "<col", "evil"] {
            assert!(!o.contains(bad), "{bad}: {o}");
        }
    }

    #[test]
    fn huge_spans_are_clamped_and_junk_dropped() {
        let o = sanitize_compose_html("<table><tr><td colspan=\"9999\" rowspan=\"60\">a</td><td colspan=\"99999\" rowspan=\"4000000000\">z</td><td colspan=\"0\" rowspan=\"-3\">b</td><td colspan=\"x\">c</td></tr></table>");
        assert!(o.contains("colspan=\"50\"") && o.contains("rowspan=\"50\""), "{o}"); // too large to be plausible: clamped (or dropped when absurd)
        assert!(!o.contains("99999") && !o.contains("4000000000") && !o.contains("colspan=\"0\"") && !o.contains("rowspan=\"-3\"") && !o.contains("colspan=\"x\""), "{o}");
    }

    #[test]
    fn deeply_nested_tables_are_flattened_beyond_the_depth_cap() {
        let n = 5000;
        let html = format!("{}deep text{}", "<table><tr><td>".repeat(n), "</td></tr></table>".repeat(n));
        let o = sanitize_compose_html(&html);
        assert!(o.contains("deep text"), "{o}");
        assert_eq!(o.matches("<table").count(), MAX_TABLE_DEPTH, "{o}");
        assert_eq!(o.matches("</table>").count(), MAX_TABLE_DEPTH);
        assert_eq!(o.matches("<td").count(), o.matches("</td>").count());
        let ok = sanitize_compose_html("<table><tr><td><table><tr><td>inner</td></tr></table></td></tr></table>");
        assert_eq!(ok.matches("<table").count(), 2, "{ok}");
    }

    #[test]
    fn table_and_cell_counts_are_capped() {
        let many = "<table><tr><td>x</td></tr></table>".repeat(MAX_TABLES + 30);
        let o = sanitize_compose_html(&many);
        assert_eq!(o.matches("<table").count(), MAX_TABLES);
        assert_eq!(o.matches("</table>").count(), MAX_TABLES);
        assert_eq!(o.matches('x').count(), MAX_TABLES + 30, "text stays");
        // A table over the cell cap is unwrapped as a whole (never half a table), the text stays and nothing runs together.
        let cells = format!("<table><tr>{}</tr></table>", "<td>c</td>".repeat(MAX_CELLS + 100));
        let o = sanitize_compose_html(&cells);
        assert_eq!(o.matches("<t").count(), 0, "{}", &o[..200.min(o.len())]);
        assert_eq!(o.matches('c').count(), MAX_CELLS + 100);
        assert!(o.starts_with("c c c"), "{}", &o[..50]);
        // Tables that fit stay, the one that would break the budget goes.
        let half = format!("<table><tr>{}</tr></table>", "<td>k</td>".repeat(MAX_CELLS - 5));
        let o = sanitize_compose_html(&format!("{half}<table><tr><td>one</td><td>two</td><td>three</td><td>four</td><td>five</td><td>six</td></tr></table>"));
        assert_eq!(o.matches("<table").count(), 1, "first fits, second does not");
        assert!(o.contains("one two three four five six"), "text of the unwrapped table keeps its separators");
    }

    #[test]
    fn hidden_table_parts_are_removed() {
        let o = sanitize_compose_html("<table><tr><td>shown</td><td style=\"display:none\">SECRET</td></tr><tr style=\"visibility:hidden\"><td>HIDDENROW</td></tr></table><table style=\"font-size:0\"><tr><td>WHOLE</td></tr></table>");
        assert!(o.contains("shown"), "{o}");
        for bad in ["SECRET", "HIDDENROW", "WHOLE", "mm-hidden"] {
            assert!(!o.contains(bad), "{bad}: {o}");
        }
    }

    // Exact markup the editor emits for callouts, buttons and dividers (app/src/lib/richblocks.ts).
    const CALLOUT: &str = "<table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"border-collapse:collapse;width:100%\"><tr><td bgcolor=\"#E8F1FD\" style=\"background-color:#E8F1FD;border-left:4px solid #2F6FDE;padding:10px 14px;color:#1B2A41\"><b>Info:</b> hello</td></tr></table>";
    const BUTTON: &str = "<table cellpadding=\"0\" cellspacing=\"0\" border=\"0\" style=\"border-collapse:separate\"><tr><td align=\"center\" bgcolor=\"#2F6FDE\" style=\"background-color:#2F6FDE;border-radius:6px;padding:10px 22px\"><a href=\"https://example.org/go?a=1&amp;b=2\" style=\"color:#FFFFFF;text-decoration:none;font-weight:bold;display:inline-block;font-family:Helvetica, Arial, sans-serif;font-size:15px;line-height:20px\">Go now</a></td></tr></table>";

    #[test]
    fn callout_and_button_markup_survives_intact() {
        let o = sanitize_compose_html(CALLOUT);
        for want in ["bgcolor=\"#E8F1FD\"", "background-color:#E8F1FD", "border-left:4px solid #2F6FDE", "padding:10px 14px", "color:#1B2A41", "<b>Info:</b> hello"] {
            assert!(o.contains(want), "{want}: {o}");
        }
        let o = sanitize_compose_html(BUTTON);
        for want in ["bgcolor=\"#2F6FDE\"", "border-radius:6px", "href=\"https://example.org/go?a=1&amp;b=2\"", "color:#FFFFFF", "text-decoration:none", "display:inline-block", "font-size:15px", "line-height:20px", "rel=\"noopener noreferrer\"", ">Go now</a>"] {
            assert!(o.contains(want), "{want}: {o}");
        }
        let o = sanitize_compose_html("<div>a</div><hr style=\"border:0;border-top:1px solid #C9CDD3;margin:12px 0\"><div>\u{2610} todo</div><div>\u{2611} done</div>");
        assert!(o.contains("<hr") && o.contains("border-top:1px solid #C9CDD3") && o.contains('\u{2610}') && o.contains('\u{2611}'), "{o}");
    }

    #[test]
    fn button_links_keep_only_safe_schemes() {
        for h in ["javascript:alert(1)", "data:text/html,x", "vbscript:x", "file:///etc/passwd", "ftp://x.example/a"] {
            let o = lower(&BUTTON.replace("https://example.org/go?a=1&amp;b=2", h));
            assert!(!o.contains("href"), "{h}: {o}");
        }
        assert!(sanitize_compose_html(&BUTTON.replace("https://example.org/go?a=1&amp;b=2", "mailto:a@b.example")).contains("href=\"mailto:a@b.example\""));
        assert!(sanitize_compose_html(&BUTTON.replace("https://example.org/go?a=1&amp;b=2", "http://a.example")).contains("href=\"http://a.example\""));
    }

    // Exact output of app/src/lib/richtable.ts `serializeTable` (3x2, header + zebra, widths 70/30, one right-aligned cell). The same file is
    // read by app/src/lib/richtable.test.ts, which asserts the TS serializer still produces exactly this.
    const EDITOR_TABLE: &str = include_str!("../../../app/src/lib/fixtures/editor_table.html");

    #[test]
    fn editor_generated_table_survives_with_every_declaration() {
        let o = sanitize_compose_html(EDITOR_TABLE);
        assert_eq!(o.matches("padding:6px 8px;border:1px solid #BBBBBB;vertical-align:top").count(), 6, "{o}");
        for want in ["width=\"70%\"", "width=\"30%\"", "bgcolor=\"#EEF0F3\"", "bgcolor=\"#F5F7FA\"", "background-color:#EEF0F3;color:#1F2328;font-weight:bold;text-align:left", "text-align:right", "align=\"right\"", "<thead>", "<tbody>", "cellpadding=\"6\"", "border-collapse:collapse;width:100%"] {
            assert!(o.contains(want), "missing {want}: {o}");
        }
        assert_eq!(o.matches("<th ").count(), 2);
        assert_eq!(o.matches("<td ").count(), 4);
    }

    #[test]
    fn sanitising_tables_is_idempotent() {
        for h in [DATA_TABLE, CALLOUT, BUTTON] {
            let once = sanitize_compose_html(h);
            assert_eq!(sanitize_compose_html(&once), once);
        }
    }

    #[test]
    fn unwrapped_tables_keep_cell_and_row_separators_and_are_stable() {
        let deep = format!("{}a</td><td>b{}", "<table><tr><td>".repeat(8), "</td></tr></table>".repeat(8));
        let o = sanitize_compose_html(&deep);
        assert_eq!(o.matches("<table").count(), MAX_TABLE_DEPTH);
        assert_eq!(sanitize_compose_html(&o), o, "idempotent on cap-unwrapped output");
        let words: Vec<&str> = o.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).filter(|w| matches!(*w, "a" | "b")).collect();
        assert_eq!(words, ["a", "b"]);
        let flat = sanitize_compose_html(&format!("{}<table><tr><td>x</td><td>y</td></tr><tr><td>z</td></tr></table>{}", "<table><tr><td>".repeat(MAX_TABLE_DEPTH), "</td></tr></table>".repeat(MAX_TABLE_DEPTH)));
        assert!(flat.contains("x y <br>z <br>"), "{flat}");
        assert!(!flat.contains("xy") && !flat.contains("yz"), "{flat}");
        assert_eq!(sanitize_compose_html(&flat), flat);
        let many = sanitize_compose_html(&"<table><tr><td>p</td><td>q</td></tr></table>".repeat(MAX_TABLES + 5));
        assert_eq!(sanitize_compose_html(&many), many);
        assert!(!many.contains("pq"), "removed cells leave a space");
    }

    #[test]
    fn dir_is_kept_on_table_elements_only_when_valid() {
        let o = sanitize_compose_html("<table dir=\"rtl\"><tr dir=\"LTR\"><td dir=\"auto\">x</td><td dir=\"evil\">y</td></tr></table>");
        assert!(o.contains("<table dir=\"rtl\"") && o.contains("<tr dir=\"ltr\"") && o.contains("<td dir=\"auto\"") && !o.contains("evil"), "{o}");
    }

    #[test]
    fn hidden_text_is_judged_numerically() {
        for hide in [
            "opacity:-1", "opacity:+0", "opacity:0e0", "opacity:0.001", "opacity:.05", "opacity:0%", "opacity: 0 !important", "OPACITY:0", "opacity:1e-3",
            "font-size:0e0px", "font-size:+0px", "font-size:0.01px", "font-size:1px", "font-size:-3pt", "font-size:0em", "font-size:0%", "FONT-SIZE : 0.5PX ! important", "font-size:1e-2rem",
            "color:transparent", "color:rgba(0,0,0,0)", "color: rgba(255 ,255, 255, 0.05)", "color:rgb(0 0 0 / 0)", "color:hsla(0,0%,0%,0%)", "color:#ffffff00", "color:#fff0",
            "font:0px Arial", "font:italic bold 0.2px/1 serif",
        ] {
            let o = sanitize_compose_html(&format!("<p>shown <span style=\"{hide}\">SECRET</span> after</p>"));
            assert!(!o.contains("SECRET"), "{hide} must hide: {o}");
            assert!(o.contains("shown") && o.contains("after"), "{hide}: {o}");
        }
        for show in [
            "font-size:small", "font-size:medium", "font-size:large", "font-size:smaller", "font-size:inherit", "font-size:x-large", "font-size:2px", "font-size:12px", "font-size:1em", "font-size:0.8em",
            "opacity:inherit", "opacity:1", "opacity:0.5", "opacity:0.1", "opacity:50%", "color:rgba(0,0,0,0.5)", "color:rgb(0 0 0 / 50%)", "color:#112233", "color:#ffffffff", "color:red", "color:inherit",
            "font:12px/0 Arial", "font:bold large serif",
        ] {
            let o = sanitize_compose_html(&format!("<p><span style=\"{show}\">VISIBLE</span></p>"));
            assert!(o.contains("VISIBLE"), "{show} must stay: {o}");
        }
    }

    #[test]
    fn negative_offsets_cannot_push_text_off_screen() {
        for st in ["text-indent:-9999px", "margin-left:-9999px", "margin:0 0 0 -5000px", "margin-top:-1e4px", "text-indent: -.5em"] {
            let o = sanitize_compose_html(&format!("<div style=\"{st};color:red\">TEXT</div><table><tr><td style=\"{st}\">CELL</td></tr></table>"));
            assert!(o.contains("TEXT") && o.contains("CELL"), "{st}: text stays visible: {o}");
            assert!(!o.contains("-9999") && !o.contains("-5000") && !o.contains("-1e4") && !o.contains("-.5em"), "{st}: {o}");
        }
        let o = sanitize_compose_html("<div style=\"margin:0 auto;text-indent:2em;padding-left:4px\">ok</div>");
        assert!(o.contains("margin:0 auto") && o.contains("text-indent:2em"), "{o}");
    }

    #[test]
    fn fixture_is_the_ts_serializer_output_and_survives() {
        assert!(EDITOR_TABLE.starts_with("<table width=\"100%\"") && !EDITOR_TABLE.ends_with('\n'));
        assert_eq!(sanitize_compose_html(EDITOR_TABLE).matches("<td ").count(), 4);
    }

    #[test]
    fn empty_and_plain_text_inputs() {
        assert_eq!(sanitize_compose_html(""), "");
        assert_eq!(sanitize_compose_html("just text &amp; more"), "just text &amp; more");
    }

    #[test]
    fn q09_align_and_height_survive_on_tables() {
        let o = sanitize_compose_html("<table align=\"center\" height=\"40\"><tr><td align=\"center\" height=\"50%\" valign=\"middle\">B</td><th align=\"right\">h</th></tr></table>");
        for k in ["<table align=\"center\"", "height=\"40\"", "<td align=\"center\"", "height=\"50%\"", "<th align=\"right\""] {
            assert!(o.contains(k), "{k} in {o}");
        }
        let o = sanitize_compose_html("<table><tr><td align=\"javascript:1\" height=\"9px\" style=\"x\">B</td></tr></table>");
        assert!(!o.contains("align") && !o.contains("height") && !o.contains("javascript"), "{o}");
    }
}
