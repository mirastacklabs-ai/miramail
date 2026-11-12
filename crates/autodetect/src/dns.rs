//! DNS lookups used by discovery (MX, SRV, host existence), behind one small interface so each platform
//! can bring its own backend:
//!   * everything except iOS: hickory-resolver (reads the system resolver config; unchanged behaviour);
//!   * iOS: Apple's DNS-SD (`DNSServiceQueryRecord`, dnssd.rs) / getaddrinfo, because the app sandbox denies
//!     reading /etc/resolv.conf, which hickory needs. Both ask the user's own configured DNS via the system.
//!
//! An Android backend can be added as another `cfg` impl of the same three methods.

use crate::srv::SrvRecord;

/// IDN domains must be sent as A-labels (punycode): DNS-SD and getaddrinfo do not convert for us. Uses the URL
/// parser's host handling (already a dependency through reqwest); ASCII names pass through unchanged. A trailing
/// dot is preserved. `None` when the name is not a valid host.
#[cfg_attr(not(any(target_os = "ios", test)), allow(dead_code))]
pub(crate) fn to_ascii_name(name: &str) -> Option<String> {
    // Anything that could change meaning in a URL (or is not a hostname char) is rejected before parsing.
    if name.contains(['@', '/', '?', '#', ':', '%', '[', ']', '\\']) || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (bare, dot) = match name.strip_suffix('.') {
        Some(b) => (b, "."),
        None => (name, ""),
    };
    let out = if name.is_ascii() {
        bare.to_string()
    } else {
        let url = reqwest::Url::parse(&format!("https://{bare}/")).ok()?;
        url.host_str()?.to_string()
    };
    // DNS limits: non-empty, total <= 253 bytes (without the root dot), every label 1..=63 bytes.
    if out.is_empty() || out.len() > 253 || out.split('.').any(|l| l.is_empty() || l.len() > 63) {
        return None;
    }
    Some(format!("{out}{dot}"))
}

#[cfg(not(target_os = "ios"))]
mod imp {
    use super::*;
    use hickory_resolver::TokioResolver;
    use std::time::Duration;

    #[derive(Clone)]
    pub struct Dns(Result<TokioResolver, String>);

    impl Dns {
        pub fn new(budget: Duration) -> Self {
            Dns(TokioResolver::builder_tokio()
                .map(|mut b| {
                    b.options_mut().attempts = 1;
                    b.options_mut().timeout = budget;
                    b.build()
                })
                .map_err(|e| e.to_string()))
        }

        fn r(&self) -> Result<&TokioResolver, String> {
            self.0.as_ref().map_err(|e| e.clone())
        }

        /// (preference, exchange with trailing dot); empty = no records.
        pub async fn mx(&self, domain: &str) -> Result<Vec<(u16, String)>, String> {
            match self.r()?.mx_lookup(format!("{domain}.")).await {
                Ok(m) => Ok(m.iter().map(|m| (m.preference(), m.exchange().to_utf8())).collect()),
                Err(e) if e.is_no_records_found() => Ok(Vec::new()),
                Err(e) => Err(e.to_string()),
            }
        }

        /// `name` is the full owner name with trailing dot, e.g. `_imaps._tcp.example.com.`
        pub async fn srv(&self, name: String) -> Result<Vec<SrvRecord>, String> {
            match self.r()?.srv_lookup(name).await {
                Ok(l) => Ok(l
                    .iter()
                    .map(|s| SrvRecord {
                        priority: s.priority(),
                        weight: s.weight(),
                        port: s.port(),
                        target: s.target().to_utf8(),
                    })
                    .collect()),
                Err(e) if e.is_no_records_found() => Ok(Vec::new()),
                Err(e) => Err(e.to_string()),
            }
        }

        /// Err only when the resolver itself is unusable; a failed lookup is Ok(false).
        pub async fn host_exists(&self, host: &str) -> Result<bool, String> {
            Ok(self.r()?.lookup_ip(format!("{host}.")).await.is_ok())
        }
    }
}

#[cfg(target_os = "ios")]
mod imp {
    use super::*;
    use crate::{dnssd, dnswire};
    use std::time::Duration;

    #[derive(Clone)]
    pub struct Dns {
        budget: Duration,
    }

    impl Dns {
        pub fn new(budget: Duration) -> Self {
            Dns { budget }
        }

        pub async fn mx(&self, domain: &str) -> Result<Vec<(u16, String)>, String> {
            let name = to_ascii_name(&format!("{domain}.")).ok_or("invalid domain name")?;
            let raw = dnssd::query(name, dnssd::TYPE_MX, self.budget).await?;
            Ok(raw.iter().filter_map(|r| dnswire::parse_mx(r)).collect())
        }

        pub async fn srv(&self, name: String) -> Result<Vec<SrvRecord>, String> {
            let name = to_ascii_name(&name).ok_or("invalid domain name")?;
            let raw = dnssd::query(name, dnssd::TYPE_SRV, self.budget).await?;
            Ok(raw.iter().filter_map(|r| dnswire::parse_srv(r)).collect())
        }

        pub async fn host_exists(&self, host: &str) -> Result<bool, String> {
            // getaddrinfo goes through mDNSResponder, which the sandbox allows. Absolute (trailing-dot) name so the
            // resolver search list is never applied; IDN as A-labels.
            let Some(ascii) = to_ascii_name(host) else { return Ok(false) };
            let fqdn = if ascii.ends_with('.') { ascii } else { format!("{ascii}.") };
            let found = match tokio::net::lookup_host((fqdn.as_str(), 0)).await {
                Ok(mut it) => it.next().is_some(),
                Err(_) => false,
            };
            Ok(found)
        }
    }
}

pub use imp::Dns;

#[cfg(test)]
mod tests {
    use super::to_ascii_name;

    #[test]
    fn rejects_malformed_names() {
        let long_label = "a".repeat(64);
        let ok_label = "a".repeat(63);
        let too_long = format!("{}.com", vec!["a".repeat(60); 5].join("."));
        assert_eq!(to_ascii_name(&format!("{ok_label}.com")).as_deref(), Some(format!("{ok_label}.com").as_str()));
        for bad in ["", ".", "a..b", ".a.com", "a b.com", "a\tb.com", "a\nb.com", "a\\b.com", "a\u{0}b.com", &format!("{long_label}.com"), &format!("{long_label}ü.com"), &too_long, "münchen..de", "mün chen.de"] {
            assert_eq!(to_ascii_name(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn ascii_passthrough() {
        assert_eq!(to_ascii_name("example.com.").as_deref(), Some("example.com."));
        assert_eq!(to_ascii_name("_imaps._tcp.example.com.").as_deref(), Some("_imaps._tcp.example.com."));
    }

    #[test]
    fn idn_to_punycode() {
        assert_eq!(to_ascii_name("münchen.de").as_deref(), Some("xn--mnchen-3ya.de"));
        assert_eq!(to_ascii_name("münchen.de.").as_deref(), Some("xn--mnchen-3ya.de."));
        assert_eq!(
            to_ascii_name("_imaps._tcp.münchen.de.").as_deref(),
            Some("_imaps._tcp.xn--mnchen-3ya.de.")
        );
        assert_eq!(to_ascii_name("MÜNCHEN.de").as_deref(), Some("xn--mnchen-3ya.de"));
    }

    #[test]
    fn invalid_host_is_none() {
        assert_eq!(to_ascii_name("bad host ü.de"), None);
        for bad in ["a@b.de", "a/b.de", "a?b.de", "a#b.de", "a:80.de", "a%41.de", "[::1]", "ü@x.de", "x.de/ü"] {
            assert_eq!(to_ascii_name(bad), None, "{bad}");
        }
    }
}
