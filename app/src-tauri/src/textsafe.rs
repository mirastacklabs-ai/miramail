//! Display-string hygiene shared by mailto, notifications, widget and badge: invisible Unicode format / bidi
//! characters let a sender spoof what the user reads ("invoice\u{202E}fdp.exe"), so they are removed.

/// True for bidi controls, zero-width / directional marks, BOM and line/paragraph separators.
pub fn is_format_char(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}' | '\u{FEFF}' | '\u{2028}' | '\u{2029}')
}

/// Remove format/bidi characters (keeps ordinary text, newlines and tabs).
pub fn strip_format(s: &str) -> String {
    s.chars().filter(|c| !is_format_char(*c)).collect()
}

/// For one-line display strings: also drops control characters.
pub fn clean_display(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() && !is_format_char(*c)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_bidi_and_zero_width() {
        assert_eq!(strip_format("a\u{202E}b\u{2066}c\u{200B}d\u{FEFF}e\u{2028}f\u{2029}g\u{200F}"), "abcdefg");
        assert_eq!(strip_format("Zoë 你好\nok"), "Zoë 你好\nok");
        assert_eq!(clean_display("A\u{7}n\u{202E}n\n"), "Ann");
    }
}
