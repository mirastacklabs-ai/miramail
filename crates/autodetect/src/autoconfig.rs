//! Thunderbird autoconfig (config-v1.1.xml) parsing and HTTPS-only fetch.

use crate::types::{Candidate, Source, TlsMode, UsernameHint};
use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AutoconfigResult {
    pub imap: Vec<Candidate>,
    pub smtp: Vec<Candidate>,
}

#[derive(Default)]
struct ServerBuf {
    kind: String,
    hostname: String,
    port: Option<u16>,
    socket: String,
    username: String,
    auth: Vec<String>,
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn auth_ok(auth: &[String]) -> bool {
    if auth.is_empty() {
        return true;
    }
    auth.iter().any(|a| {
        matches!(
            a.to_ascii_lowercase().as_str(),
            "password-cleartext" | "plain" | "password-encrypted" | "secure" | "normal"
        )
    })
}

/// Parse a Thunderbird autoconfig document. `%EMAILADDRESS%`, `%EMAILLOCALPART%`
/// and `%EMAILDOMAIN%` placeholders are expanded. Plaintext servers and
/// OAuth-only/other non-password servers are skipped. Malformed XML yields an error;
/// DTDs/entities are never expanded (quick-xml does not resolve external entities).
pub fn parse_autoconfig(xml: &str, email: &str) -> Result<AutoconfigResult, String> {
    let (local, domain) = email.rsplit_once('@').ok_or("bad email")?;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = AutoconfigResult::default();
    let mut cur: Option<ServerBuf> = None;
    let mut field: Option<String> = None;
    let mut seen_root = false;

    loop {
        match reader.read_event() {
            Err(e) => return Err(format!("xml error at {}: {e}", reader.error_position())),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if name == "clientConfig" {
                    seen_root = true;
                }
                match name.as_str() {
                    "incomingServer" | "outgoingServer" => {
                        let mut buf = ServerBuf::default();
                        for a in e.attributes().flatten() {
                            if a.key.local_name().as_ref() == b"type" {
                                buf.kind = String::from_utf8_lossy(&a.value).to_ascii_lowercase();
                            }
                        }
                        cur = Some(buf);
                    }
                    _ if cur.is_some() => field = Some(name),
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                let v = t.unescape().map_err(|e| e.to_string())?.trim().to_string();
                apply_text(cur.as_mut(), field.as_deref(), v);
            }
            // CDATA is plain text for our purposes (<hostname><![CDATA[...]]></hostname>).
            Ok(Event::CData(t)) => {
                let v = t.decode().map_err(|e| e.to_string())?.trim().to_string();
                apply_text(cur.as_mut(), field.as_deref(), v);
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if name == "incomingServer" || name == "outgoingServer" {
                    if let Some(b) = cur.take() {
                        finish(b, local, domain, &mut out);
                    }
                }
                field = None;
            }
            _ => {}
        }
    }
    if !seen_root {
        return Err("not an autoconfig document".into());
    }
    Ok(out)
}

fn apply_text(buf: Option<&mut ServerBuf>, field: Option<&str>, v: String) {
    if let (Some(buf), Some(f)) = (buf, field) {
        match f {
            "hostname" => buf.hostname = v,
            "port" => buf.port = v.parse().ok(),
            "socketType" => buf.socket = v.to_ascii_uppercase(),
            "username" => buf.username = v,
            "authentication" => buf.auth.push(v),
            _ => {}
        }
    }
}

fn finish(b: ServerBuf, local: &str, domain: &str, out: &mut AutoconfigResult) {
    let tls = match b.socket.as_str() {
        "SSL" => TlsMode::Implicit,
        "STARTTLS" => TlsMode::StartTls,
        _ => return, // plain / unknown: never offered
    };
    if !auth_ok(&b.auth) {
        return;
    }
    let host = b
        .hostname
        .replace("%EMAILDOMAIN%", domain)
        .replace("%EMAILLOCALPART%", local)
        .to_ascii_lowercase();
    if !valid_host(&host) || !crate::hosts::host_is_sane(&host) {
        return;
    }
    let Some(port) = b.port.filter(|p| *p != 0) else { return };
    let username_hint = if b.username.contains("%EMAILLOCALPART%") || (!b.username.is_empty() && !b.username.contains('%') && b.username == local) {
        UsernameHint::LocalPartFallback
    } else {
        UsernameHint::Email
    };
    let c = Candidate { host, port, tls, source: Source::Autoconfig, username_hint };
    match b.kind.as_str() {
        "imap" => out.imap.push(c),
        "smtp" => out.smtp.push(c),
        _ => {}
    }
}

const MAX_BODY: usize = 256 * 1024;

const MAX_REDIRECTS: usize = 3;

/// The registrable-domain scope for redirects: the email domain, derived from
/// the first request host (`autoconfig.<domain>` or `<domain>`).
fn scope_domain(first: &reqwest::Url) -> Option<String> {
    let h = first.host_str()?.to_ascii_lowercase();
    Some(h.strip_prefix("autoconfig.").unwrap_or(&h).to_string())
}

/// Validate one redirect hop. Only HTTPS (default port) redirects that stay
/// inside `scope` are followed, and the query (which carries the user's email
/// address) is dropped.
fn redirect_target(scope: &str, current: &reqwest::Url, location: &str) -> Result<reqwest::Url, String> {
    let mut next = current.join(location).map_err(|_| "bad redirect location".to_string())?;
    if next.scheme() != "https" {
        return Err("refusing non-https redirect".into());
    }
    if next.port().is_some_and(|p| p != 443) || !next.username().is_empty() || next.password().is_some() {
        return Err("refusing redirect to unusual port or credentials".into());
    }
    let host = next.host_str().ok_or("redirect without host")?;
    if !crate::hosts::within_domain(host, scope) {
        return Err("refusing redirect outside the email domain".into());
    }
    next.set_query(None);
    next.set_fragment(None);
    Ok(next)
}

/// HTTPS-only fetch with a bounded body. Redirects are followed manually (the
/// client from [`client`] never follows any): HTTPS only, same domain only, and
/// the `emailaddress` query is not forwarded.
pub async fn fetch(client: &reqwest::Client, url: reqwest::Url) -> Result<String, String> {
    if url.scheme() != "https" {
        return Err("refusing non-https url".into());
    }
    let scope = scope_domain(&url).ok_or("url without host")?;
    let mut url = url;
    let mut resp = {
        let mut hops = 0;
        loop {
            let r = client.get(url.clone()).send().await.map_err(|e| e.without_url().to_string())?;
            if !r.status().is_redirection() {
                break r;
            }
            if hops >= MAX_REDIRECTS {
                return Err("too many redirects".into());
            }
            hops += 1;
            let loc = r
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("redirect without location")?;
            url = redirect_target(&scope, &url, loc)?;
        }
    };
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.without_url().to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return Err("response too large".into());
        }
    }
    String::from_utf8(body).map_err(|_| "response not utf-8".into())
}

pub fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .https_only(true)
        // Redirects are handled (and restricted) in `fetch`.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("MiraMail-autodetect")
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <displayName>Example</displayName>
    <incomingServer type="imap">
      <hostname>imap.%EMAILDOMAIN%</hostname><port>143</port>
      <socketType>STARTTLS</socketType><username>%EMAILLOCALPART%</username>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>IMAP.example.com</hostname><port>993</port>
      <socketType>SSL</socketType><username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>imap.plain.example.com</hostname><port>143</port>
      <socketType>plain</socketType><username>%EMAILADDRESS%</username>
    </incomingServer>
    <incomingServer type="pop3">
      <hostname>pop.example.com</hostname><port>995</port><socketType>SSL</socketType>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>oauth.example.com</hostname><port>993</port><socketType>SSL</socketType>
      <authentication>OAuth2</authentication>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.com</hostname><port>465</port>
      <socketType>SSL</socketType><username>%EMAILADDRESS%</username>
      <authentication>password-cleartext</authentication>
    </outgoingServer>
    <outgoingServer type="smtp">
      <hostname>bad host!</hostname><port>587</port><socketType>STARTTLS</socketType>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn parses_fixture() {
        let r = parse_autoconfig(FIXTURE, "bob@example.com").unwrap();
        assert_eq!(r.imap.len(), 2);
        assert_eq!(r.imap[0].host, "imap.example.com");
        assert_eq!(r.imap[0].port, 143);
        assert_eq!(r.imap[0].tls, TlsMode::StartTls);
        assert_eq!(r.imap[0].username_hint, UsernameHint::LocalPartFallback);
        assert_eq!(r.imap[1].host, "imap.example.com");
        assert_eq!(r.imap[1].tls, TlsMode::Implicit);
        assert_eq!(r.imap[1].username_hint, UsernameHint::Email);
        assert_eq!(r.smtp.len(), 1);
        assert_eq!(r.smtp[0].port, 465);
        assert!(r.imap.iter().all(|c| c.source == Source::Autoconfig));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_autoconfig("<html><body>404</body></html>", "a@b.co").is_err());
        assert!(parse_autoconfig("<clientConfig><emailProvider>", "a@b.co").map_or(true, |r| r.imap.is_empty()));
    }

    #[test]
    fn doctype_entities_not_expanded() {
        let xml = r#"<!DOCTYPE x [<!ENTITY h SYSTEM "file:///etc/passwd">]>
<clientConfig><emailProvider><incomingServer type="imap"><hostname>&h;</hostname>
<port>993</port><socketType>SSL</socketType></incomingServer></emailProvider></clientConfig>"#;
        if let Ok(r) = parse_autoconfig(xml, "a@b.co") {
            assert!(r.imap.is_empty());
        }
    }

    #[test]
    fn cdata_is_text() {
        let xml = r#"<clientConfig><emailProvider>
<incomingServer type="imap"><hostname><![CDATA[imap.example.com]]></hostname>
<port><![CDATA[993]]></port><socketType><![CDATA[SSL]]></socketType></incomingServer>
</emailProvider></clientConfig>"#;
        let r = parse_autoconfig(xml, "a@example.com").unwrap();
        assert_eq!(r.imap.len(), 1);
        assert_eq!(r.imap[0].host, "imap.example.com");
        assert_eq!(r.imap[0].port, 993);
        assert_eq!(r.imap[0].tls, TlsMode::Implicit);
    }

    /// Shapes seen in real ISPDB / provider files: OAuth2 listed before a password method, an OAuth-only server, POP3,
    /// lower-case socketType, `%EMAILDOMAIN%` in the host, out-of-range / missing / zero ports, a namespace prefix,
    /// comments and whitespace around values.
    #[test]
    fn tp_real_world_variants() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- provider file -->
<clientConfig version="1.1"><emailProvider id="example.com">
  <domain>example.com</domain>
  <incomingServer type="imap">
    <hostname> imap.%EMAILDOMAIN% </hostname><port> 993 </port><socketType>ssl</socketType>
    <username>%EMAILLOCALPART%@%EMAILDOMAIN%</username>
    <authentication>OAuth2</authentication><authentication>password-cleartext</authentication>
  </incomingServer>
  <incomingServer type="imap">
    <hostname>oauth-only.example.com</hostname><port>993</port><socketType>SSL</socketType>
    <authentication>OAuth2</authentication>
  </incomingServer>
  <incomingServer type="pop3">
    <hostname>pop.example.com</hostname><port>995</port><socketType>SSL</socketType>
  </incomingServer>
  <incomingServer type="IMAP">
    <hostname>imap2.example.com</hostname><port>99999</port><socketType>SSL</socketType>
  </incomingServer>
  <incomingServer type="imap">
    <hostname>imap3.example.com</hostname><socketType>SSL</socketType>
  </incomingServer>
  <outgoingServer type="smtp">
    <hostname>smtp.example.com</hostname><port>0</port><socketType>SSL</socketType>
  </outgoingServer>
  <outgoingServer type="smtp">
    <hostname>SMTP.Example.COM</hostname><port>587</port><socketType>StartTLS</socketType>
    <!-- no authentication element at all -->
  </outgoingServer>
  <outgoingServer type="smtp">
    <hostname>plain.example.com</hostname><port>25</port><socketType>plain</socketType>
  </outgoingServer>
</emailProvider></clientConfig>"#;
        let r = parse_autoconfig(xml, "Bob@example.com").unwrap();
        assert_eq!(r.imap.len(), 1, "{:?}", r.imap);
        assert_eq!((r.imap[0].host.as_str(), r.imap[0].port, r.imap[0].tls), ("imap.example.com", 993, TlsMode::Implicit));
        assert_eq!(r.smtp.len(), 1, "{:?}", r.smtp);
        assert_eq!((r.smtp[0].host.as_str(), r.smtp[0].port, r.smtp[0].tls), ("smtp.example.com", 587, TlsMode::StartTls));
        // Namespaced root and elements still parse (local names are compared).
        let ns = r#"<c:clientConfig xmlns:c="urn:x"><c:emailProvider><c:incomingServer type="imap"><c:hostname>imap.example.com</c:hostname><c:port>993</c:port><c:socketType>SSL</c:socketType></c:incomingServer></c:emailProvider></c:clientConfig>"#;
        assert_eq!(parse_autoconfig(ns, "a@example.com").unwrap().imap.len(), 1);
    }

    /// The placeholders come from the user's own address; a local part can never turn into a different host.
    #[test]
    fn tp_placeholders_cannot_build_a_foreign_or_broken_host() {
        let xml = r#"<clientConfig><emailProvider><incomingServer type="imap"><hostname>%EMAILLOCALPART%.example.com</hostname><port>993</port><socketType>SSL</socketType></incomingServer></emailProvider></clientConfig>"#;
        assert!(parse_autoconfig(xml, "evil.attacker.net/x@example.com").unwrap().imap.is_empty());
        assert!(parse_autoconfig(xml, "a b@example.com").unwrap().imap.is_empty());
        assert_eq!(parse_autoconfig(xml, "bob@example.com").unwrap().imap[0].host, "bob.example.com");
        assert!(parse_autoconfig(xml, "no-at-sign").is_err());
        // Empty documents and an empty provider are not errors that crash, just nothing to offer.
        assert!(parse_autoconfig("", "a@example.com").is_err());
        assert!(parse_autoconfig("<clientConfig/>", "a@example.com").map_or(true, |r| r.imap.is_empty() && r.smtp.is_empty()));
    }

    #[test]
    fn unsafe_hosts_are_rejected() {
        let mk = |h: &str| {
            format!(
                r#"<clientConfig><emailProvider><incomingServer type="imap"><hostname>{h}</hostname>
<port>993</port><socketType>SSL</socketType></incomingServer></emailProvider></clientConfig>"#
            )
        };
        for bad in ["localhost", "127.0.0.1", "192.168.0.10", "169.254.169.254", "mailhost", "nas.local", "10.1.2.3"] {
            assert!(parse_autoconfig(&mk(bad), "a@example.com").unwrap().imap.is_empty(), "{bad}");
        }
        assert_eq!(parse_autoconfig(&mk("imap.example.com"), "a@example.com").unwrap().imap.len(), 1);
    }

    fn u(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).unwrap()
    }

    #[test]
    fn redirects_stay_in_domain_https_and_drop_query() {
        let first = u("https://autoconfig.example.com/mail/config-v1.1.xml?emailaddress=a%40example.com");
        let scope = scope_domain(&first).unwrap();
        assert_eq!(scope, "example.com");
        let ok = redirect_target(&scope, &first, "https://config.example.com/c.xml?emailaddress=a%40example.com").unwrap();
        assert_eq!(ok.as_str(), "https://config.example.com/c.xml");
        let rel = redirect_target(&scope, &first, "/other.xml").unwrap();
        assert_eq!(rel.as_str(), "https://autoconfig.example.com/other.xml");
        assert_eq!(scope_domain(&u("https://example.com/.well-known/x")).unwrap(), "example.com");
        for bad in [
            "http://autoconfig.example.com/x",
            "https://evil.net/x",
            "https://example.com.evil.net/x",
            "https://evilexample.com/x",
            "https://autoconfig.example.com:8443/x",
            "https://user:pw@autoconfig.example.com/x",
        ] {
            assert!(redirect_target(&scope, &first, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn client_never_follows_redirects_itself() {
        assert!(client().is_ok());
    }
}
