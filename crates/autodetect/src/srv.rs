//! DNS SRV (RFC 6186 / 8314) record handling. Parsing is pure and testable;
//! the resolver call lives in detect.rs.

use crate::types::{Candidate, Source, TlsMode, UsernameHint};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrvService {
    Imaps,
    Imap,
    Submissions,
    Submission,
}

impl SrvService {
    pub const ALL: [SrvService; 4] = [Self::Imaps, Self::Imap, Self::Submissions, Self::Submission];

    pub fn label(self) -> &'static str {
        match self {
            Self::Imaps => "_imaps._tcp",
            Self::Imap => "_imap._tcp",
            Self::Submissions => "_submissions._tcp",
            Self::Submission => "_submission._tcp",
        }
    }
    pub fn tls(self) -> TlsMode {
        match self {
            Self::Imaps | Self::Submissions => TlsMode::Implicit,
            Self::Imap | Self::Submission => TlsMode::StartTls,
        }
    }
    pub fn is_imap(self) -> bool {
        matches!(self, Self::Imaps | Self::Imap)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrvRecord {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub target: String,
}

/// Turn SRV records into candidates: lowest priority first, then highest weight.
/// A target of "." means "service not available" (RFC 2782) and is skipped.
///
/// WARNING: SRV is unauthenticated DNS and the TLS certificate is validated
/// against the *target* host, so a valid certificate does NOT protect against a
/// spoofed record. This unscoped form performs no trust check; discovery uses
/// [`srv_candidates_for_domain`], which drops untrusted targets.
pub fn srv_candidates(service: SrvService, records: &[SrvRecord]) -> Vec<Candidate> {
    let mut recs: Vec<&SrvRecord> = records
        .iter()
        .filter(|r| r.port != 0 && !r.target.trim_end_matches('.').is_empty())
        .collect();
    // Fully deterministic order (equal priority/weight ties broken by name, port).
    recs.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(b.weight.cmp(&a.weight))
            .then_with(|| a.target.to_ascii_lowercase().cmp(&b.target.to_ascii_lowercase()))
            .then(a.port.cmp(&b.port))
    });
    recs.into_iter()
        .map(|r| Candidate {
            host: r.target.trim_end_matches('.').to_ascii_lowercase(),
            port: r.port,
            tls: service.tls(),
            source: Source::Srv,
            username_hint: UsernameHint::LocalPartFallback,
        })
        .collect()
}

/// Like [`srv_candidates`] but keeps only targets that equal or are a subdomain
/// of `email_domain`, or that are hosts listed in the bundled provider data,
/// and that pass the host sanity filter (no IP literals, loopback, private or
/// single-label names). Everything else is dropped.
pub fn srv_candidates_for_domain(service: SrvService, records: &[SrvRecord], email_domain: &str) -> Vec<Candidate> {
    srv_candidates(service, records)
        .into_iter()
        .filter(|c| crate::hosts::srv_target_trusted(&c.host, email_domain))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(p: u16, w: u16, port: u16, t: &str) -> SrvRecord {
        SrvRecord { priority: p, weight: w, port, target: t.into() }
    }

    #[test]
    fn orders_by_priority_then_weight() {
        let c = srv_candidates(
            SrvService::Imaps,
            &[r(20, 5, 993, "b.example.com."), r(10, 1, 993, "a.example.com."), r(10, 9, 993, "c.example.com.")],
        );
        let hosts: Vec<_> = c.iter().map(|c| c.host.as_str()).collect();
        assert_eq!(hosts, ["c.example.com", "a.example.com", "b.example.com"]);
        assert!(c.iter().all(|c| c.tls == TlsMode::Implicit && c.source == Source::Srv));
    }

    #[test]
    fn dot_target_means_unavailable() {
        assert!(srv_candidates(SrvService::Imap, &[r(0, 0, 143, ".")]).is_empty());
        assert!(srv_candidates(SrvService::Imap, &[r(0, 0, 0, "x.example.com.")]).is_empty());
    }

    #[test]
    fn service_tls_mapping() {
        let rec = [r(0, 0, 587, "smtp.example.com.")];
        assert_eq!(srv_candidates(SrvService::Submission, &rec)[0].tls, TlsMode::StartTls);
        assert_eq!(srv_candidates(SrvService::Submissions, &rec)[0].tls, TlsMode::Implicit);
        assert!(SrvService::Imaps.is_imap() && !SrvService::Submission.is_imap());
    }

    #[test]
    fn ties_are_deterministic() {
        let a = [r(1, 1, 993, "b.example.com."), r(1, 1, 993, "a.example.com."), r(1, 1, 465, "a.example.com.")];
        let mut rev = a.clone();
        rev.reverse();
        let x = srv_candidates(SrvService::Imaps, &a);
        let y = srv_candidates(SrvService::Imaps, &rev);
        assert_eq!(x, y);
        assert_eq!(x[0].host, "a.example.com");
        assert_eq!(x[0].port, 465);
    }

    #[test]
    fn spoofed_targets_are_dropped() {
        let recs = [
            r(0, 0, 993, "imap.attacker.net."),
            r(1, 0, 993, "imap.example.com."),
            r(2, 0, 993, "imap.fastmail.com."),
            r(3, 0, 993, "127.0.0.1."),
            r(4, 0, 993, "localhost."),
            r(5, 0, 993, "evilexample.com."),
            r(6, 0, 993, "example.com.evil.net."),
            r(7, 0, 993, "box.internal."),
        ];
        let c = srv_candidates_for_domain(SrvService::Imaps, &recs, "example.com");
        let hosts: Vec<_> = c.iter().map(|c| c.host.as_str()).collect();
        assert_eq!(hosts, ["imap.example.com", "imap.fastmail.com"]);
    }
}
