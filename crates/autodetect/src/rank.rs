//! Candidate ranking (spec section 2): source score first, then implicit TLS
//! before STARTTLS, then the order the source listed them in. Duplicates
//! (same host/port/tls) keep their best-ranked copy.

use crate::types::{Candidate, TlsMode};
use std::collections::HashSet;

pub fn rank(mut cands: Vec<Candidate>) -> Vec<Candidate> {
    // Stable sort: ties keep source-provided order (e.g. Zoho imappro before imap).
    cands.sort_by_key(|c| {
        (
            std::cmp::Reverse(c.source.rank()),
            match c.tls {
                TlsMode::Implicit => 0u8,
                TlsMode::StartTls => 1,
            },
        )
    });
    let mut seen = HashSet::new();
    cands.retain(|c| seen.insert(c.key()));
    cands
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Source, UsernameHint};

    fn c(host: &str, port: u16, tls: TlsMode, source: Source) -> Candidate {
        Candidate { host: host.into(), port, tls, source, username_hint: UsernameHint::Email }
    }

    #[test]
    fn source_beats_tls_and_tls_breaks_ties() {
        let out = rank(vec![
            c("h.x", 993, TlsMode::Implicit, Source::Heuristic),
            c("s.x", 993, TlsMode::Implicit, Source::Srv),
            c("a.x", 143, TlsMode::StartTls, Source::Autoconfig),
            c("m.x", 143, TlsMode::StartTls, Source::Mx),
            c("a.x", 993, TlsMode::Implicit, Source::Autoconfig),
            c("b.x", 993, TlsMode::Implicit, Source::Bundled),
        ]);
        let hosts: Vec<_> = out.iter().map(|c| (c.host.as_str(), c.port)).collect();
        assert_eq!(
            hosts,
            [("b.x", 993), ("m.x", 143), ("a.x", 993), ("a.x", 143), ("s.x", 993), ("h.x", 993)]
        );
    }

    #[test]
    fn dedupes_keeping_best_source_and_stable_order() {
        let out = rank(vec![
            c("imappro.zoho.com", 993, TlsMode::Implicit, Source::Mx),
            c("imap.zoho.com", 993, TlsMode::Implicit, Source::Bundled),
            c("imappro.zoho.com", 993, TlsMode::Implicit, Source::Bundled),
            c("IMAP.zoho.com", 993, TlsMode::Implicit, Source::Mx),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].host, "imap.zoho.com");
        assert_eq!(out[1].host, "imappro.zoho.com");
        assert_eq!(out[1].source, Source::Bundled);
    }
}
