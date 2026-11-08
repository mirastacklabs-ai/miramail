//! Email address parsing and heuristic host guesses.

use crate::error::DetectError;
use crate::types::{Candidate, Source, TlsMode, UsernameHint};

pub struct ParsedEmail {
    pub address: String,
    pub domain: String,
}

pub fn parse_email(input: &str) -> Result<ParsedEmail, DetectError> {
    let s = input.trim();
    // Never echo the raw input back (it may be a pasted password or other secret).
    let bad = |why: &str| DetectError::InvalidEmail(why.to_string());
    let (local, domain) = s.rsplit_once('@').ok_or_else(|| bad("missing @"))?;
    let domain = domain.trim_end_matches('.').to_lowercase();
    let labels_ok = domain.contains('.')
        && domain.len() <= 253
        && domain
            .split('.')
            .all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-')
                && l.chars().all(|c| c.is_alphanumeric() || c == '-'));
    if local.is_empty() || local.len() > 64 || local.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad("invalid local part"));
    }
    if !labels_ok {
        return Err(bad("invalid domain"));
    }
    Ok(ParsedEmail { address: format!("{local}@{domain}"), domain })
}

/// Lowest-priority guesses (spec source 6). Callers must confirm the hosts
/// resolve in DNS before offering them.
pub fn heuristic_hosts(domain: &str) -> (Vec<String>, Vec<String>) {
    (
        vec![format!("imap.{domain}"), format!("mail.{domain}")],
        vec![format!("smtp.{domain}"), format!("mail.{domain}")],
    )
}

pub fn heuristic_candidates(imap_hosts: &[String], smtp_hosts: &[String]) -> (Vec<Candidate>, Vec<Candidate>) {
    let mk = |h: &String, port, tls| Candidate {
        host: h.clone(),
        port,
        tls,
        source: Source::Heuristic,
        username_hint: UsernameHint::LocalPartFallback,
    };
    (
        imap_hosts.iter().map(|h| mk(h, 993, TlsMode::Implicit)).collect(),
        smtp_hosts
            .iter()
            .flat_map(|h| [mk(h, 465, TlsMode::Implicit), mk(h, 587, TlsMode::StartTls)])
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalises() {
        let p = parse_email("  Bob@Example.COM ").unwrap();
        assert_eq!(p.domain, "example.com");
        assert_eq!(p.address, "Bob@example.com");
    }

    #[test]
    fn rejects_bad() {
        for s in ["", "bob", "@x.com", "bob@", "bob@localhost", "b ob@x.com", "bob@x..com", "bob@-x.com", "a@b/c.com"] {
            assert!(parse_email(s).is_err(), "{s}");
        }
    }

    #[test]
    fn invalid_email_does_not_echo_input() {
        for s in ["hunter2-secret-pw", "p@ss word@x", "secretlocal@bad_domain", "sec ret@x.com"] {
            let e = parse_email(s).err().expect("invalid");
            let shown = format!("{e} {e:?}");
            assert!(!shown.contains("secret") && !shown.contains("hunter2") && !shown.contains("p@ss"), "{shown}");
        }
    }

    #[test]
    fn heuristics() {
        let (i, s) = heuristic_hosts("example.org");
        assert_eq!(i, ["imap.example.org", "mail.example.org"]);
        assert_eq!(s, ["smtp.example.org", "mail.example.org"]);
        let (ic, sc) = heuristic_candidates(&i, &s);
        assert_eq!(ic.len(), 2);
        assert_eq!(sc.len(), 4);
        assert!(ic.iter().chain(&sc).all(|c| c.source == Source::Heuristic));
    }
}
