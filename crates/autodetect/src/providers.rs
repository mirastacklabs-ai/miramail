//! Bundled provider data (data/providers.toml) and MX -> provider mapping.

use crate::types::{Candidate, Source, TlsMode, UsernameHint};
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
pub struct ServerEntry {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub mx: Vec<String>,
    #[serde(default)]
    pub unsupported: bool,
    #[serde(default)]
    pub generic: bool,
    #[serde(default)]
    pub local_part_fallback: bool,
    pub app_password_url: Option<String>,
    pub imap_disabled_url: Option<String>,
    #[serde(default)]
    pub imap: Vec<ServerEntry>,
    #[serde(default)]
    pub smtp: Vec<ServerEntry>,
    pub personal_imap: Option<Vec<ServerEntry>>,
    pub personal_smtp: Option<Vec<ServerEntry>>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderDb {
    #[serde(rename = "provider")]
    pub providers: Vec<Provider>,
}

const BUNDLED: &str = include_str!("../data/providers.toml");

impl ProviderDb {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// The bundled database. The data file is checked by a unit test, so a
    /// parse failure here is a build-time bug, not a runtime condition.
    pub fn bundled() -> &'static ProviderDb {
        static DB: OnceLock<ProviderDb> = OnceLock::new();
        DB.get_or_init(|| ProviderDb::parse(BUNDLED).expect("bundled providers.toml is invalid"))
    }

    pub fn by_id(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn by_domain(&self, domain: &str) -> Option<&Provider> {
        let d = domain.to_ascii_lowercase();
        self.providers
            .iter()
            .find(|p| p.domains.iter().any(|x| x.eq_ignore_ascii_case(&d)))
    }

    /// First provider (in MX preference order given by the caller) whose MX
    /// patterns match any exchange host.
    pub fn by_mx<S: AsRef<str>>(&self, exchanges_in_pref_order: &[S]) -> Option<&Provider> {
        for ex in exchanges_in_pref_order {
            if let Some(p) = self
                .providers
                .iter()
                .find(|p| p.mx.iter().any(|pat| glob_match(pat, ex.as_ref())))
            {
                return Some(p);
            }
        }
        None
    }
}

impl Provider {
    fn username_hint(&self) -> UsernameHint {
        if self.local_part_fallback {
            UsernameHint::LocalPartFallback
        } else {
            UsernameHint::Email
        }
    }

    fn to_candidates(&self, entries: &[ServerEntry], source: Source, domain: &str) -> Vec<Candidate> {
        entries
            .iter()
            .map(|e| Candidate {
                host: e.host.replace("{domain}", domain),
                port: e.port,
                tls: e.tls,
                source,
                username_hint: self.username_hint(),
            })
            .collect()
    }

    /// Candidates for a custom domain hosted at this provider (via MX), or for
    /// the provider's own domains when `personal` is true (Zoho: non-pro first).
    pub fn candidates(&self, source: Source, domain: &str, personal: bool) -> (Vec<Candidate>, Vec<Candidate>) {
        let (i, s) = if personal {
            (
                self.personal_imap.as_deref().unwrap_or(&self.imap),
                self.personal_smtp.as_deref().unwrap_or(&self.smtp),
            )
        } else {
            (&self.imap[..], &self.smtp[..])
        };
        (
            self.to_candidates(i, source, domain),
            self.to_candidates(s, source, domain),
        )
    }
}

/// Case-insensitive glob with `*` wildcards; a trailing dot on `text` is ignored.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p = pattern.to_ascii_lowercase();
    let t = text.trim_end_matches('.').to_ascii_lowercase();
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == t;
    }
    let mut rest = t.as_str();
    let first = parts[0];
    if !rest.starts_with(first) {
        return false;
    }
    rest = &rest[first.len()..];
    let last = parts[parts.len() - 1];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_parses_and_is_sane() {
        let db = ProviderDb::bundled();
        assert!(db.providers.len() >= 25);
        for p in &db.providers {
            if !p.unsupported {
                assert!(!p.imap.is_empty() && !p.smtp.is_empty(), "{}", p.id);
            }
            for e in p.imap.iter().chain(p.smtp.iter()) {
                // Plaintext is never offered: implicit must be 993/465, starttls anything.
                assert!(e.port != 143 || e.tls == TlsMode::StartTls, "{}", p.id);
            }
        }
        let mut ids: Vec<_> = db.providers.iter().map(|p| &p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), db.providers.len(), "duplicate ids");
    }

    #[test]
    fn glob() {
        assert!(glob_match("mx*.zoho.com", "mx.zoho.com."));
        assert!(glob_match("mx*.zoho.com", "mx2.zoho.com"));
        assert!(!glob_match("mx*.zoho.com", "mx.zoho.com.au"));
        assert!(!glob_match("mx*.zoho.com", "evilmx.zoho.com"));
        assert!(glob_match("*.messagingengine.com", "in1-smtp.messagingengine.com"));
        assert!(!glob_match("*.messagingengine.com", "messagingengine.com.evil.net"));
        assert!(glob_match("aspmx.l.google.com", "ASPMX.L.GOOGLE.COM."));
        assert!(!glob_match("*.zoho.com", "zoho.com"));
    }

    #[test]
    fn mx_mapping() {
        let db = ProviderDb::bundled();
        let p = db.by_mx(&["mx.zoho.eu.", "mx2.zoho.eu."]).unwrap();
        assert_eq!(p.id, "zoho-eu");
        assert_eq!(db.by_mx(&["in1-smtp.messagingengine.com"]).unwrap().id, "fastmail");
        assert_eq!(db.by_mx(&["aspmx.l.google.com"]).unwrap().id, "google");
        assert_eq!(db.by_mx(&["x.mail.protection.outlook.com"]).unwrap().id, "microsoft");
        // Gmail works with an app password; Microsoft is still blocked (OAuth only).
        let g = db.by_domain("gmail.com").unwrap();
        assert!(!g.unsupported);
        assert!(g.app_password_url.as_deref().unwrap().starts_with("https://myaccount.google.com/"));
        let (i, s) = g.candidates(Source::Bundled, "gmail.com", true);
        assert_eq!((i[0].host.as_str(), s[0].host.as_str()), ("imap.gmail.com", "smtp.gmail.com"));
        assert!(db.by_domain("outlook.com").unwrap().unsupported);
        assert_eq!(db.by_mx(&["mailserver.purelymail.com"]).unwrap().id, "purelymail");
        assert!(db.by_mx(&["mail.example.org"]).is_none());
        // Preference order: first matching exchange wins.
        assert_eq!(db.by_mx(&["a.mx.migadu.com", "mx.zoho.com"]).unwrap().id, "migadu");
    }

    #[test]
    fn zoho_personal_tries_non_pro_first_custom_tries_pro_first() {
        let db = ProviderDb::bundled();
        let z = db.by_domain("zoho.com").unwrap();
        let (imap, smtp) = z.candidates(Source::Bundled, "zoho.com", true);
        assert_eq!(imap[0].host, "imap.zoho.com");
        assert_eq!(smtp[0].host, "smtp.zoho.com");
        let (imap, smtp) = z.candidates(Source::Mx, "acme.io", false);
        assert_eq!(imap[0].host, "imappro.zoho.com");
        assert_eq!(smtp[0].host, "smtppro.zoho.com");
        let (imap, _) = db.by_domain("zoho.in").unwrap().candidates(Source::Bundled, "zoho.in", true);
        assert_eq!(imap[0].host, "imap.zoho.in");
    }

    #[test]
    fn gmx_regions_use_their_own_servers() {
        let db = ProviderDb::bundled();
        let (i, s) = db.by_domain("gmx.net").unwrap().candidates(Source::Bundled, "gmx.net", true);
        assert_eq!((i[0].host.as_str(), s[0].host.as_str()), ("imap.gmx.net", "mail.gmx.net"));
        for d in ["gmx.com", "gmx.us"] {
            let (i, s) = db.by_domain(d).unwrap().candidates(Source::Bundled, d, true);
            assert_eq!((i[0].host.as_str(), s[0].host.as_str()), ("imap.gmx.com", "mail.gmx.com"), "{d}");
        }
        assert_eq!(db.by_mx(&["mx00.emig.gmx.net."]).unwrap().id, "gmx");
        assert_eq!(db.by_mx(&["mx00.gmx.com."]).unwrap().id, "gmx-com");
    }
}
