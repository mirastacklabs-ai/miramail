//! Authentication results (SPF / DKIM / DMARC) from the ONE `Authentication-Results` header MiraMail stores
//! (the first one in the header block, whitespace-collapsed, capped at 1000 chars). ARC and `Received` headers are
//! not stored, so nothing here claims to read them. A header written by a server that is not the account's own
//! (authserv-id mismatch) is never believed: it yields `AUTH_UNKNOWN`.

use super::urls::reg_domain;
use super::{Collector, SafetyInput, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verdict {
    #[default]
    Absent,
    Pass,
    Fail,
    SoftFail,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthResults {
    /// The authserv-id belongs to one of the trusted hosts.
    pub trusted: bool,
    pub spf: Verdict,
    pub dkim: Verdict,
    pub dmarc: Verdict,
}

fn strip_comments(v: &str) -> String {
    let (mut depth, mut out) = (0u32, String::new());
    for c in v.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn verdict(v: &str) -> Verdict {
    match v {
        "pass" => Verdict::Pass,
        "fail" => Verdict::Fail,
        "softfail" => Verdict::SoftFail,
        "none" | "" => Verdict::Absent,
        _ => Verdict::Other,
    }
}

fn merge(cur: Verdict, new: Verdict) -> Verdict {
    // Several DKIM signatures: any pass wins, then any fail.
    match (cur, new) {
        (Verdict::Pass, _) | (_, Verdict::Pass) => Verdict::Pass,
        (Verdict::Fail, _) | (_, Verdict::Fail) => Verdict::Fail,
        (Verdict::Absent, n) => n,
        (c, _) => c,
    }
}

pub fn parse(ar: &str, hosts: &[String]) -> AuthResults {
    let ar = strip_comments(ar);
    let mut parts = ar.split(';');
    let id = parts.next().unwrap_or("").split_whitespace().next().unwrap_or("").trim_end_matches('.').to_ascii_lowercase();
    let trusted = !id.is_empty() && hosts.iter().any(|h| !h.is_empty() && reg_domain(&id) == reg_domain(h) && !reg_domain(h).is_empty());
    let mut out = AuthResults { trusted, ..Default::default() };
    for part in parts {
        let first = part.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        let Some((method, res)) = first.split_once('=') else { continue };
        let v = verdict(res);
        match method {
            "spf" => out.spf = merge(out.spf, v),
            "dkim" => out.dkim = merge(out.dkim, v),
            "dmarc" => out.dmarc = merge(out.dmarc, v),
            _ => {}
        }
    }
    out
}

pub fn check(input: &SafetyInput, c: &mut Collector) {
    let ar = parse(&input.authentication_results, &input.trusted_auth_hosts);
    if !ar.trusted {
        c.add("AUTH_UNKNOWN", Severity::Info, "Your mail server did not record a sender-authentication result that MiraMail can trust, so SPF, DKIM and DMARC were not judged.");
        return;
    }
    let dmarc_pass = ar.dmarc == Verdict::Pass;
    // Forwarding and mailing lists break SPF/DKIM legitimately; a DMARC pass (or a list) makes those findings weak.
    let weak = dmarc_pass || input.is_list_mail;
    if ar.dmarc == Verdict::Fail {
        c.add("AUTH_DMARC_FAIL", Severity::High, "The sender's domain publishes a policy (DMARC) and this message did not meet it, which is typical of forged mail.");
    }
    if ar.dkim == Verdict::Fail {
        c.add("AUTH_DKIM_FAIL", if weak { Severity::Low } else { Severity::Medium }, "The message's cryptographic signature (DKIM) did not verify.");
    }
    match ar.spf {
        Verdict::Fail => c.add("AUTH_SPF_FAIL", if weak { Severity::Low } else { Severity::Medium }, "The server that sent this message is not authorised by the sender's domain (SPF fail)."),
        Verdict::SoftFail => c.add("AUTH_SPF_FAIL", Severity::Low, "The sending server is probably not authorised by the sender's domain (SPF softfail)."),
        _ => {}
    }
    let any_pass = [ar.spf, ar.dkim, ar.dmarc].contains(&Verdict::Pass);
    let any_fail = [ar.spf, ar.dkim, ar.dmarc].iter().any(|v| matches!(v, Verdict::Fail | Verdict::SoftFail));
    if !any_pass && !any_fail {
        c.add("AUTH_NONE", Severity::Low, "None of SPF, DKIM or DMARC passed, so nothing confirms who really sent this.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> Vec<String> {
        vec!["imap.mailhost.test".into(), "mailhost.test".into()]
    }

    #[test]
    fn parses_methods_and_ignores_comments() {
        let r = parse("mx.mailhost.test; spf=pass (sender ok; fail=x) smtp.mailfrom=a@b.c; dkim=fail header.d=b.c; dkim=pass header.d=other.d; dmarc=fail (p=reject) header.from=b.c", &hosts());
        assert!(r.trusted);
        assert_eq!(r.spf, Verdict::Pass);
        assert_eq!(r.dkim, Verdict::Pass);
        assert_eq!(r.dmarc, Verdict::Fail);
    }

    #[test]
    fn untrusted_or_empty_is_unknown_not_pass() {
        let r = parse("evil.example; dmarc=pass; dkim=pass", &hosts());
        assert!(!r.trusted);
        assert!(!parse("", &hosts()).trusted);
        assert!(!parse("mx.mailhost.test; dkim=pass", &[]).trusted);
    }

    #[test]
    fn first_token_only_and_softfail() {
        let r = parse("mx.mailhost.test; spf=softfail smtp.mailfrom=x@y.z; dmarc=none", &hosts());
        assert_eq!(r.spf, Verdict::SoftFail);
        assert_eq!(r.dmarc, Verdict::Absent);
    }
}
