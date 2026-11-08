//! vCard 3.0 / 4.0 reader and vCard 3.0 writer for the local address book. Pure: no I/O, no network.
//!
//! The reader is tolerant (LF or CRLF, folded lines, quoted-printable values with soft breaks, `item1.EMAIL`
//! group prefixes, unknown properties, missing END, junk between cards) and bounded (input size, card count and
//! per-field sizes), so a hostile or huge `.vcf` can only ever produce a bounded list of plain text fields.
//! Photos and every other binary or unknown property are ignored, never decoded.

pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CARDS: usize = 5000;
pub const MAX_NAME: usize = 200;
pub const MAX_EMAILS: usize = 10;
pub const MAX_PHONES: usize = 10;
pub const MAX_GROUPS: usize = 10;
pub const MAX_NOTES: usize = 5000;
pub const MAX_FIELD: usize = 200;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Card {
    pub name: String,
    pub org: String,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub notes: String,
    /// `CATEGORIES` values (become groups).
    pub groups: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub cards: Vec<Card>,
    /// Cards with neither a usable e-mail address nor a name, or beyond the card limit.
    pub skipped: usize,
    /// The input was cut at [`MAX_INPUT_BYTES`].
    pub truncated: bool,
}

/// Split into logical lines: unfold continuation lines (leading space/tab) and join quoted-printable soft breaks.
fn logical_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // State of the line being built: decided once at its start, never rescanned.
    let mut qp = false;
    let mut oversize = false;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let folded = (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty();
        // A quoted-printable value ending in `=` continues on the next physical line (no indentation needed).
        let qp_cont = !folded && qp && out.last().is_some_and(|l| l.ends_with('='));
        if folded || qp_cont {
            let last = out.last_mut().unwrap();
            if qp_cont {
                last.pop();
            }
            let add = if folded { &line[1..] } else { line };
            if last.len() + add.len() > MAX_LOGICAL_LINE {
                oversize = true;
            } else if !oversize {
                last.push_str(add);
            }
            continue;
        }
        if oversize {
            // The previous logical line blew the cap: drop it entirely.
            *out.last_mut().unwrap() = String::new();
        }
        oversize = line.len() > MAX_LOGICAL_LINE;
        qp = !oversize && is_qp_line(line);
        out.push(if oversize { String::new() } else { line.to_string() });
    }
    if oversize {
        if let Some(l) = out.last_mut() {
            *l = String::new();
        }
    }
    out
}

/// A logical (unfolded) line longer than this is skipped.
const MAX_LOGICAL_LINE: usize = 64 * 1024;

fn is_qp_line(line: &str) -> bool {
    let head = line.split(':').next().unwrap_or("").to_ascii_uppercase();
    head.contains("QUOTED-PRINTABLE")
}

fn qp_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'=' && i + 2 < b.len() {
            let h = |c: u8| (c as char).to_digit(16);
            if let (Some(x), Some(y)) = (h(b[i + 1]), h(b[i + 2])) {
                out.push((x * 16 + y) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `\n`, `\N` -> newline; `\,` `\;` `\\` -> the character.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// Split on an unescaped separator.
fn split_unescaped(s: &str, sep: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut esc = false;
    for c in s.chars() {
        if esc {
            parts.last_mut().unwrap().push('\\');
            parts.last_mut().unwrap().push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == sep {
            parts.push(String::new());
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    if esc {
        parts.last_mut().unwrap().push('\\');
    }
    parts
}

/// Invisible zero-width / bidi-override / BOM characters that could spoof a displayed name.
fn is_format_char(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

fn clean(s: &str, max: usize) -> String {
    let t: String = s.chars().filter(|c| (!c.is_control() || *c == '\n') && !is_format_char(*c)).collect();
    t.trim().chars().take(max).collect()
}

fn clean_line(s: &str, max: usize) -> String {
    clean(&s.replace('\n', " "), max)
}

/// A plausible plain address: one `@`, no spaces or angle brackets.
pub fn valid_email(e: &str) -> bool {
    let mut p = e.split('@');
    let (Some(l), Some(d), None) = (p.next(), p.next(), p.next()) else { return false };
    e.len() <= 254 && !l.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.') && !e.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"' | '(' | ')'))
}

fn push_unique(v: &mut Vec<String>, x: String, max: usize) {
    if !x.is_empty() && v.len() < max && !v.iter().any(|y| y.eq_ignore_ascii_case(&x)) {
        v.push(x);
    }
}

pub fn parse(input: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut text = input;
    if text.len() > MAX_INPUT_BYTES {
        let mut cut = MAX_INPUT_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text = &text[..cut];
        out.truncated = true;
    }
    let text = text.trim_start_matches('\u{feff}');
    let mut cur: Option<(Card, String)> = None; // (card, name from N as fallback)
    let finish = |cur: &mut Option<(Card, String)>, out: &mut Parsed| {
        if let Some((mut c, n)) = cur.take() {
            if c.name.is_empty() {
                c.name = n;
            }
            if (c.emails.is_empty() && c.name.is_empty()) || out.cards.len() >= MAX_CARDS {
                out.skipped += 1;
            } else {
                out.cards.push(c);
            }
        }
    };
    for line in logical_lines(text) {
        let Some((head, value)) = line.split_once(':') else { continue };
        let head_u = head.to_ascii_uppercase();
        let mut parts = head_u.split(';');
        let prop_full = parts.next().unwrap_or("");
        let prop = prop_full.rsplit('.').next().unwrap_or(prop_full); // drop `item1.`
        let params: Vec<&str> = parts.collect();
        match prop {
            "BEGIN" if value.trim().eq_ignore_ascii_case("VCARD") => {
                finish(&mut cur, &mut out); // a card without END
                cur = Some((Card::default(), String::new()));
                continue;
            }
            "END" if value.trim().eq_ignore_ascii_case("VCARD") => {
                finish(&mut cur, &mut out);
                continue;
            }
            _ => {}
        }
        let Some((card, nfallback)) = cur.as_mut() else { continue };
        let qp = params.iter().any(|p| p.contains("QUOTED-PRINTABLE"));
        let val = if qp { qp_decode(value) } else { value.to_string() };
        match prop {
            "FN" => {
                if card.name.is_empty() {
                    card.name = clean_line(&unescape(&val), MAX_NAME);
                }
            }
            "N" => {
                // family;given;additional;prefix;suffix
                let f: Vec<String> = split_unescaped(&val, ';').iter().map(|s| unescape(s)).collect();
                let g = |i: usize| f.get(i).map(|s| s.trim().to_string()).unwrap_or_default();
                let joined = [g(3), g(1), g(2), g(0), g(4)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                *nfallback = clean_line(&joined, MAX_NAME);
            }
            "ORG" => {
                if card.org.is_empty() {
                    let first = split_unescaped(&val, ';').into_iter().map(|s| unescape(&s)).filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join(", ");
                    card.org = clean_line(&first, MAX_FIELD);
                }
            }
            "EMAIL" => {
                let v = unescape(&val);
                let v = v.trim().trim_start_matches("mailto:").trim().to_ascii_lowercase();
                if valid_email(&v) {
                    push_unique(&mut card.emails, v, MAX_EMAILS);
                }
            }
            "TEL" => {
                let v = clean_line(unescape(&val).trim().trim_start_matches("tel:"), 40);
                push_unique(&mut card.phones, v, MAX_PHONES);
            }
            "NOTE" => {
                let n = clean(&unescape(&val), MAX_NOTES);
                if card.notes.is_empty() {
                    card.notes = n;
                }
            }
            "CATEGORIES" => {
                for g in split_unescaped(&val, ',') {
                    push_unique(&mut card.groups, clean_line(&unescape(&g), 60), MAX_GROUPS);
                }
            }
            _ => {} // PHOTO, LOGO, KEY, SOUND, X-*, ADR, ... ignored
        }
    }
    finish(&mut cur, &mut out);
    out
}

// ---------------------------------------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------------------------------------

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            ';' => o.push_str("\\;"),
            ',' => o.push_str("\\,"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            c if c.is_control() => {}
            c => o.push(c),
        }
    }
    o
}

/// Fold at 75 octets (never inside a character), CRLF + space continuation.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut len = 0usize;
    for c in line.chars() {
        let n = c.len_utf8();
        if len + n > 75 {
            out.push_str("\r\n ");
            len = 1;
        }
        out.push(c);
        len += n;
    }
    out.push_str("\r\n");
    out
}

pub fn write(cards: &[Card]) -> String {
    let mut s = String::new();
    for c in cards {
        s.push_str("BEGIN:VCARD\r\nVERSION:3.0\r\n");
        let name = if c.name.is_empty() { c.emails.first().cloned().unwrap_or_default() } else { c.name.clone() };
        s.push_str(&fold(&format!("FN:{}", esc(&name))));
        s.push_str(&fold(&format!("N:{};;;;", esc(&name))));
        if !c.org.is_empty() {
            s.push_str(&fold(&format!("ORG:{}", esc(&c.org))));
        }
        for e in &c.emails {
            s.push_str(&fold(&format!("EMAIL;TYPE=INTERNET:{}", esc(e))));
        }
        for p in &c.phones {
            s.push_str(&fold(&format!("TEL:{}", esc(p))));
        }
        if !c.groups.is_empty() {
            s.push_str(&fold(&format!("CATEGORIES:{}", c.groups.iter().map(|g| esc(g)).collect::<Vec<_>>().join(","))));
        }
        if !c.notes.is_empty() {
            s.push_str(&fold(&format!("NOTE:{}", esc(&c.notes))));
        }
        s.push_str("END:VCARD\r\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_v3_and_v4() {
        let v3 = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nEMAIL;TYPE=INTERNET,WORK:Ada@Example.org\r\nTEL;TYPE=CELL:+44 20 7946 0000\r\nORG:Analytical Engines;R&D\r\nCATEGORIES:Friends,Work\r\nNOTE:Hello\\, world\\nline2\r\nEND:VCARD\r\n";
        let p = parse(v3);
        assert_eq!(p.cards.len(), 1);
        let c = &p.cards[0];
        assert_eq!(c.name, "Ada Lovelace");
        assert_eq!(c.emails, vec!["ada@example.org"]);
        assert_eq!(c.phones, vec!["+44 20 7946 0000"]);
        assert_eq!(c.org, "Analytical Engines, R&D");
        assert_eq!(c.groups, vec!["Friends", "Work"]);
        assert_eq!(c.notes, "Hello, world\nline2");
        let v4 = "BEGIN:VCARD\nVERSION:4.0\nFN:Bob\nEMAIL;PREF=1:mailto:bob@x.test\nTEL;VALUE=uri:tel:+1-555-0100\nEND:VCARD\n";
        let p = parse(v4);
        assert_eq!((p.cards[0].emails.clone(), p.cards[0].phones.clone()), (vec!["bob@x.test".to_string()], vec!["+1-555-0100".to_string()]));
    }

    #[test]
    fn folded_lines_are_joined_even_inside_a_word_and_with_lf_only() {
        let t = "BEGIN:VCARD\nVERSION:3.0\nFN:Long Na\n me Here\nEMAIL:long.addr\n ess@exam\n\tple.org\nNOTE:one\n two\nEND:VCARD\n";
        let c = &parse(t).cards[0];
        assert_eq!(c.name, "Long Name Here");
        assert_eq!(c.emails, vec!["long.address@example.org"]);
        assert_eq!(c.notes, "onetwo");
    }

    #[test]
    fn quoted_printable_with_soft_breaks_and_utf8() {
        let t = "BEGIN:VCARD\nVERSION:2.1\nFN;CHARSET=UTF-8;ENCODING=QUOTED-PRINTABLE:Jos=C3=A9 =\nGarc=C3=ADa\nEMAIL:jose@x.es\nNOTE;ENCODING=QUOTED-PRINTABLE:caf=C3=A9=0Anext =3D ok\nEND:VCARD\n";
        let c = &parse(t).cards[0];
        assert_eq!(c.name, "José García");
        assert_eq!(c.notes, "café\nnext = ok");
        // malformed escapes pass through instead of panicking
        assert_eq!(qp_decode("a=ZZb=4"), "a=ZZb=4");
        assert_eq!(qp_decode("="), "=");
    }

    #[test]
    fn name_falls_back_to_n_and_group_prefixes_and_junk_are_tolerated() {
        let t = "garbage line\nBEGIN:vcard\nitem1.EMAIL;type=pref:a@b.co\nitem1.X-ABLabel:work\nN:Hopper;Grace;Brewster;Rear Admiral;\nPHOTO;ENCODING=b:/9j/4AAQSkZJRgABAQEASABIAAD\n //AAAA\nEND:VCARD\nBEGIN:VCARD\nEMAIL:no-end@x.test\n";
        let p = parse(t);
        assert_eq!(p.cards.len(), 2, "a missing END still closes the card at the end");
        assert_eq!(p.cards[0].name, "Rear Admiral Grace Brewster Hopper");
        assert_eq!(p.cards[0].emails, vec!["a@b.co"]);
        assert_eq!(p.cards[1].emails, vec!["no-end@x.test"]);
        assert_eq!(p.cards[1].name, "");
    }

    #[test]
    fn invalid_and_empty_cards_are_skipped_or_filtered() {
        let t = "BEGIN:VCARD\nEMAIL:not an address\nEND:VCARD\nBEGIN:VCARD\nFN:Name Only\nEND:VCARD\nBEGIN:VCARD\nEMAIL:a@b\nEMAIL:<x@y.z>\nEMAIL:ok@fine.org\nEND:VCARD\n";
        let p = parse(t);
        assert_eq!(p.skipped, 1);
        assert_eq!(p.cards.len(), 2);
        assert!(p.cards[0].emails.is_empty() && p.cards[0].name == "Name Only");
        assert_eq!(p.cards[1].emails, vec!["ok@fine.org"]);
    }

    #[test]
    fn limits_hold_for_huge_input() {
        // too many cards
        let mut t = String::new();
        for i in 0..(MAX_CARDS + 50) {
            t.push_str(&format!("BEGIN:VCARD\nFN:P{i}\nEMAIL:p{i}@x.test\nEND:VCARD\n"));
        }
        let p = parse(&t);
        assert_eq!(p.cards.len(), MAX_CARDS);
        assert_eq!(p.skipped, 50);
        // giant fields are clipped; many emails/phones capped
        let mut c = String::from("BEGIN:VCARD\nFN:");
        c.push_str(&"x".repeat(60_000));
        c.push_str("\nNOTE:");
        c.push_str(&"n".repeat(60_000));
        for i in 0..50 {
            c.push_str(&format!("\nEMAIL:e{i}@x.test\nTEL:{i}{i}{i}"));
        }
        c.push_str("\nEND:VCARD\n");
        let c = &parse(&c).cards[0];
        assert_eq!(c.name.chars().count(), MAX_NAME);
        assert_eq!(c.notes.chars().count(), MAX_NOTES);
        assert_eq!((c.emails.len(), c.phones.len()), (MAX_EMAILS, MAX_PHONES));
        // a logical line over 64 KB is skipped outright
        let over = format!("BEGIN:VCARD\nFN:{}\nEMAIL:a@b.co\nEND:VCARD\n", "x".repeat(MAX_LOGICAL_LINE + 1));
        let p = parse(&over);
        assert_eq!(p.cards[0].name, "");
        assert_eq!(p.cards[0].emails, vec!["a@b.co"]);
        // input beyond the byte cap is cut (on a char boundary) and flagged
        let big = format!("BEGIN:VCARD\nFN:A\nEMAIL:a@b.co\nEND:VCARD\n{}", "é".repeat(MAX_INPUT_BYTES));
        let p = parse(&big);
        assert!(p.truncated);
        assert_eq!(p.cards.len(), 1);
        // pathological one-line / deeply folded input stays linear and does not panic
        let folded = format!("BEGIN:VCARD\nFN:A\n{}\nEND:VCARD", "\n x".repeat(200_000));
        let _ = parse(&folded);
        let _ = parse(&"=".repeat(1_000_000));
        let _ = parse("\u{feff}BEGIN:VCARD\r\nFN:\u{202e}Evil\u{0}\r\nEND:VCARD");
    }

    #[test]
    fn hostile_qp_soft_break_input_is_linear() {
        let mut t = String::from("BEGIN:VCARD\nNOTE;QUOTED-PRINTABLE=\n");
        t.push_str(&"a=\n".repeat(2_500_000));
        t.push_str("END:VCARD\n");
        let start = std::time::Instant::now();
        let _ = parse(&t);
        // Linear: ~30 ms optimised; the unoptimised test build gets a looser bound (quadratic would take minutes).
        let limit = if cfg!(debug_assertions) { 4000 } else { 1000 };
        assert!(start.elapsed().as_millis() < limit, "took {:?}", start.elapsed());
    }

    #[test]
    fn format_chars_are_stripped_from_name_org_groups() {
        let c = &parse("BEGIN:VCARD\nFN:A\u{202e}B\u{200b}C\u{2066}D\u{feff}\nORG:O\u{200f}rg\nCATEGORIES:G\u{202a}r\nEMAIL:a@b.co\nEND:VCARD").cards[0];
        assert_eq!(c.name, "ABCD");
        assert_eq!(c.org, "Org");
        assert_eq!(c.groups, vec!["Gr"]);
    }

    #[test]
    fn control_and_bidi_free_names() {
        let c = &parse("BEGIN:VCARD\nFN:Bad\u{7}Name\u{0}\nEMAIL:a@b.co\nEND:VCARD").cards[0];
        assert_eq!(c.name, "BadName");
    }

    #[test]
    fn writer_round_trips_and_folds_without_splitting_characters() {
        let cards = vec![
            Card { name: "Ünï, \"Q\"; x".into(), org: "Org\\Co".into(), emails: vec!["a@x.test".into(), "b@x.test".into()], phones: vec!["+1 555".into()], notes: "l1\nl2, end;".into(), groups: vec!["A,B".into(), "C".into()] },
            Card { name: "é".repeat(120), emails: vec!["long@x.test".into()], ..Default::default() },
        ];
        let text = write(&cards);
        assert!(text.contains("\r\n") && text.lines().all(|l| l.len() <= 76), "folded at 75 octets");
        let back = parse(&text);
        assert_eq!(back.cards.len(), 2);
        assert_eq!(back.cards[0], Card { name: "Ünï, \"Q\"; x".into(), ..cards[0].clone() });
        assert_eq!(back.cards[1].name, "é".repeat(120).chars().take(MAX_NAME).collect::<String>());
    }
}
