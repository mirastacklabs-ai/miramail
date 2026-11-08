//! Incremental SSE parser (task A1).
//!
//! Follows the WHATWG `text/event-stream` rules: LF, CRLF and lone CR line endings (a CRLF split
//! across two chunks counts once), multi-line `data:` joined with `\n`, `:` comment lines ignored
//! (OpenRouter sends `: OPENROUTER PROCESSING`), one optional leading UTF-8 BOM stripped, a single
//! leading space after the colon stripped. Bytes are buffered until a full line arrives, so chunk
//! boundaries may fall anywhere, including inside a multi-byte UTF-8 character. Invalid UTF-8 is
//! replaced with U+FFFD rather than failing.
//!
//! Limits: a single line may not exceed [`MAX_LINE_BYTES`] and one event's data may not exceed
//! [`MAX_EVENT_BYTES`]; exceeding either returns [`SseError`] and poisons the parser (every later
//! call returns the same error). The parser never panics.
//!
//! Lenient at end of stream: [`SseParser::finish`] dispatches a final event that was not followed
//! by a blank line (several OpenAI-compatible servers end that way).

use std::fmt;

/// Max bytes in one line (without the terminator).
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Max bytes of joined `data` in one event.
pub const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// One dispatched SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field, if any (`None` means the default "message" type).
    pub event: Option<String>,
    /// Joined `data:` lines (without the final newline).
    pub data: String,
}

impl SseEvent {
    /// The OpenAI end-of-stream sentinel `data: [DONE]`.
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
    /// Event type with the SSE default applied.
    pub fn event_type(&self) -> &str {
        self.event.as_deref().unwrap_or("message")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseError {
    /// A line exceeded [`MAX_LINE_BYTES`] without a line terminator.
    LineTooLong,
    /// An event's data exceeded [`MAX_EVENT_BYTES`].
    EventTooLarge,
}

impl fmt::Display for SseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SseError::LineTooLong => write!(
                f,
                "The AI endpoint sent a stream line longer than {} MB. The response was stopped.",
                MAX_LINE_BYTES / (1024 * 1024)
            ),
            SseError::EventTooLarge => write!(
                f,
                "The AI endpoint sent a stream event larger than {} MB. The response was stopped.",
                MAX_EVENT_BYTES / (1024 * 1024)
            ),
        }
    }
}
impl std::error::Error for SseError {}

#[derive(Debug, Default)]
pub struct SseParser {
    line: Vec<u8>,
    /// Previous chunk ended in `\r`: skip one leading `\n`.
    skip_lf: bool,
    /// No line completed yet (BOM may still be pending).
    seen_first_line: bool,
    event: Option<String>,
    data: String,
    has_data: bool,
    poisoned: Option<SseError>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw body bytes; returns every event completed by them.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        if let Some(e) = self.poisoned {
            return Err(e);
        }
        let mut out = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            if self.skip_lf {
                self.skip_lf = false;
                if rest[0] == b'\n' {
                    rest = &rest[1..];
                    continue;
                }
            }
            match rest.iter().position(|&b| b == b'\n' || b == b'\r') {
                Some(pos) => {
                    if self.line.len() + pos > MAX_LINE_BYTES {
                        return Err(self.poison(SseError::LineTooLong));
                    }
                    self.line.extend_from_slice(&rest[..pos]);
                    if rest[pos] == b'\r' {
                        self.skip_lf = true;
                    }
                    rest = &rest[pos + 1..];
                    self.end_line(&mut out)?;
                }
                None => {
                    if self.line.len() + rest.len() > MAX_LINE_BYTES {
                        return Err(self.poison(SseError::LineTooLong));
                    }
                    self.line.extend_from_slice(rest);
                    rest = &[];
                }
            }
        }
        Ok(out)
    }

    /// End of stream: process a trailing unterminated line and dispatch a pending event.
    pub fn finish(&mut self) -> Result<Vec<SseEvent>, SseError> {
        if let Some(e) = self.poisoned {
            return Err(e);
        }
        let mut out = Vec::new();
        if !self.line.is_empty() {
            self.end_line(&mut out)?;
        }
        self.dispatch(&mut out);
        self.skip_lf = false;
        Ok(out)
    }

    fn poison(&mut self, e: SseError) -> SseError {
        self.poisoned = Some(e);
        self.line = Vec::new();
        self.data = String::new();
        e
    }

    fn end_line(&mut self, out: &mut Vec<SseEvent>) -> Result<(), SseError> {
        let mut raw = std::mem::take(&mut self.line);
        if !self.seen_first_line {
            self.seen_first_line = true;
            if raw.starts_with(BOM) {
                raw.drain(..BOM.len());
            }
        }
        if raw.is_empty() {
            self.dispatch(out);
            return Ok(());
        }
        let line = String::from_utf8_lossy(&raw);
        if line.starts_with(':') {
            return Ok(()); // comment / keep-alive
        }
        let (field, value) = match line.find(':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (&line[..], ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => {
                if self.data.len() + value.len() + 1 > MAX_EVENT_BYTES {
                    return Err(self.poison(SseError::EventTooLarge));
                }
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            // `id` and `retry` are irrelevant for one-shot POST streams; unknown fields are ignored.
            _ => {}
        }
        self.line = raw; // reuse the allocation
        self.line.clear();
        Ok(())
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        let event = self.event.take();
        if !self.has_data {
            return;
        }
        self.has_data = false;
        out.push(SseEvent { event: event.filter(|e| !e.is_empty()), data: std::mem::take(&mut self.data) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(input: &[u8]) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        let mut v = p.feed(input).unwrap();
        v.extend(p.finish().unwrap());
        v
    }

    fn ev(event: Option<&str>, data: &str) -> SseEvent {
        SseEvent { event: event.map(str::to_string), data: data.to_string() }
    }

    #[test]
    fn basic_lf_crlf_cr() {
        let want = vec![ev(Some("a"), "1"), ev(None, "2")];
        assert_eq!(all(b"event: a\ndata: 1\n\ndata: 2\n\n"), want);
        assert_eq!(all(b"event: a\r\ndata: 1\r\n\r\ndata: 2\r\n\r\n"), want);
        assert_eq!(all(b"event: a\rdata: 1\r\rdata: 2\r\r"), want);
    }

    #[test]
    fn multiline_data_comments_and_no_space() {
        assert_eq!(all(b": hi\ndata:a\ndata:  b\n: x\n\n"), vec![ev(None, "a\n b")]);
    }

    #[test]
    fn bom_stripped_once() {
        assert_eq!(all(b"\xEF\xBB\xBFdata: x\n\n"), vec![ev(None, "x")]);
        let mut p = SseParser::new();
        assert!(p.feed(b"\xEF").unwrap().is_empty());
        assert!(p.feed(b"\xBB").unwrap().is_empty());
        assert_eq!(p.feed(b"\xBFdata: y\n\n").unwrap(), vec![ev(None, "y")]);
    }

    #[test]
    fn event_without_data_is_dropped_and_type_reset() {
        assert_eq!(all(b"event: ping\n\ndata: z\n\n"), vec![ev(None, "z")]);
    }

    #[test]
    fn empty_data_is_dispatched() {
        assert_eq!(all(b"data:\n\n"), vec![ev(None, "")]);
    }

    #[test]
    fn done_sentinel() {
        let v = all(b"data: [DONE]\n\n");
        assert!(v[0].is_done());
    }

    #[test]
    fn finish_flushes_unterminated() {
        assert_eq!(all(b"data: tail"), vec![ev(None, "tail")]);
    }

    #[test]
    fn crlf_split_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: a\r").unwrap().is_empty());
        assert!(p.feed(b"\n").unwrap().is_empty());
        assert_eq!(p.feed(b"\r\n").unwrap(), vec![ev(None, "a")]);
    }

    #[test]
    fn mid_utf8_split() {
        let s = "data: héllo 🦀\n\n".as_bytes();
        for cut in 0..=s.len() {
            let mut p = SseParser::new();
            let mut v = p.feed(&s[..cut]).unwrap();
            v.extend(p.feed(&s[cut..]).unwrap());
            assert_eq!(v, vec![ev(None, "héllo 🦀")], "cut {cut}");
        }
    }

    #[test]
    fn invalid_utf8_is_lossy_not_panic() {
        assert_eq!(all(b"data: \xFF\xFE\n\n"), vec![ev(None, "\u{FFFD}\u{FFFD}")]);
    }

    #[test]
    fn line_cap() {
        let mut p = SseParser::new();
        let chunk = vec![b'a'; 64 * 1024];
        let mut err = None;
        for _ in 0..20 {
            if let Err(e) = p.feed(&chunk) {
                err = Some(e);
                break;
            }
        }
        assert_eq!(err, Some(SseError::LineTooLong));
        assert_eq!(p.feed(b"\n\n"), Err(SseError::LineTooLong));
        assert!(SseError::LineTooLong.to_string().contains("1 MB"));
        // exactly at the cap is fine
        let mut p = SseParser::new();
        let mut line = b"data:".to_vec();
        line.resize(MAX_LINE_BYTES, b'x');
        line.extend_from_slice(b"\n\n");
        assert_eq!(p.feed(&line).unwrap().len(), 1);
    }

    #[test]
    fn event_cap() {
        let mut p = SseParser::new();
        let mut line = b"data:".to_vec();
        line.resize(MAX_LINE_BYTES - 1, b'x');
        line.push(b'\n');
        let mut res = Ok(vec![]);
        for _ in 0..6 {
            res = p.feed(&line);
            if res.is_err() {
                break;
            }
        }
        assert_eq!(res, Err(SseError::EventTooLarge));
    }
}
