//! Host-name sanity and trust checks for server names that come from
//! unauthenticated or third-party sources (SRV records, autoconfig XML).

use crate::providers::ProviderDb;
use std::collections::HashSet;
use std::sync::OnceLock;

/// Suffixes that never name a public mail server.
const LOCAL_SUFFIXES: [&str; 9] = [
    "localhost", "local", "localdomain", "internal", "lan", "home.arpa", "intranet", "corp", "private",
];

/// Syntactically valid, multi-label, non-IP, non-local DNS host name.
/// Rejects IP literals (incl. numeric/hex dotted forms), loopback, link-local,
/// private-range and single-label names so a remote source cannot point the
/// client at the user's own machine or LAN.
pub fn host_is_sane(h: &str) -> bool {
    let h = h.trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() || h.len() > 253 || h.contains(':') {
        return false;
    }
    let labels: Vec<&str> = h.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    if !labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) {
        return false;
    }
    // A real TLD is never all digits and never a hex-number form like "0x7f":
    // that is an IPv4 literal in disguise.
    let tld = labels[labels.len() - 1];
    if tld.bytes().all(|b| b.is_ascii_digit()) || tld.starts_with("0x") {
        return false;
    }
    !LOCAL_SUFFIXES
        .iter()
        .any(|s| h == *s || h.ends_with(&format!(".{s}")))
}

/// `host` equals `domain` or is a subdomain of it (label-boundary aware).
pub fn within_domain(host: &str, domain: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    let d = domain.trim_end_matches('.').to_ascii_lowercase();
    !d.is_empty() && (h == d || h.ends_with(&format!(".{d}")))
}

/// Every concrete host named in the bundled provider data.
pub fn bundled_hosts() -> &'static HashSet<String> {
    static HOSTS: OnceLock<HashSet<String>> = OnceLock::new();
    HOSTS.get_or_init(|| {
        let mut set = HashSet::new();
        for p in &ProviderDb::bundled().providers {
            if p.generic {
                continue;
            }
            for e in p
                .imap
                .iter()
                .chain(&p.smtp)
                .chain(p.personal_imap.iter().flatten())
                .chain(p.personal_smtp.iter().flatten())
            {
                if !e.host.contains('{') {
                    set.insert(e.host.to_ascii_lowercase());
                }
            }
        }
        set
    })
}

/// SRV targets are unauthenticated DNS and the TLS certificate is checked
/// against the *target*, so a spoofed record would happily send the password to
/// an attacker host holding a valid certificate. Accept only targets inside the
/// email domain or already listed in the bundled provider data.
pub fn srv_target_trusted(target: &str, email_domain: &str) -> bool {
    host_is_sane(target)
        && (within_domain(target, email_domain)
            || bundled_hosts().contains(&target.trim_end_matches('.').to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanity() {
        for ok in ["imap.example.com", "IMAP.Example.com.", "a-b.example.co.uk", "mail.xn--bcher-kva.de"] {
            assert!(host_is_sane(ok), "{ok}");
        }
        for bad in [
            "", "localhost", "mail", "127.0.0.1", "10.0.0.5", "192.168.1.1", "169.254.169.254", "0x7f.1",
            "2130706433", "::1", "[::1]", "a.localhost", "printer.local", "nas.lan", "svc.internal", "x.home.arpa",
            "bad host!.com", "-a.example.com", "a..example.com", "1.2.3.4.",
        ] {
            assert!(!host_is_sane(bad), "{bad}");
        }
    }

    #[test]
    fn domain_boundary() {
        assert!(within_domain("imap.example.com", "example.com"));
        assert!(within_domain("example.com.", "example.com"));
        assert!(!within_domain("evilexample.com", "example.com"));
        assert!(!within_domain("example.com.evil.net", "example.com"));
    }

    #[test]
    fn srv_trust() {
        assert!(srv_target_trusted("imap.example.com", "example.com"));
        assert!(srv_target_trusted("imap.fastmail.com.", "custom.org"));
        assert!(!srv_target_trusted("imap.attacker.net", "example.com"));
        assert!(!srv_target_trusted("127.0.0.1", "127.0.0.1"));
        assert!(!srv_target_trusted("localhost", "localhost"));
    }
}
