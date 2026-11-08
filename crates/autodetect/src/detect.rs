//! Discovery orchestration: all sources run in parallel under one deadline.

use crate::autoconfig;
use crate::email::{heuristic_candidates, heuristic_hosts, parse_email};
use crate::error::DetectError;
use crate::providers::ProviderDb;
use crate::rank::rank;
use crate::srv::{srv_candidates_for_domain, SrvRecord, SrvService};
use crate::types::*;
use hickory_resolver::TokioResolver;
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::task::JoinSet;
use tokio::time::timeout_at;

#[derive(Debug, Clone)]
pub struct DetectOptions {
    /// Total budget for all discovery sources (spec: 2s).
    pub budget: Duration,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self { budget: Duration::from_secs(2) }
    }
}

/// Discover ranked IMAP + SMTP candidates for `email`. Never sends a password;
/// no plain-HTTP requests.
pub async fn detect(email: &str) -> Result<DetectionResult, DetectError> {
    detect_with(&DetectOptions::default(), email).await
}

#[derive(Default)]
struct TaskOut {
    /// Spawn order; results are merged in this order so ranking never depends
    /// on which task happened to finish first.
    slot: usize,
    reports: Vec<SourceReport>,
    imap: Vec<Candidate>,
    smtp: Vec<Candidate>,
    /// Provider id found via MX.
    mx_provider: Option<String>,
}

type Found = (Vec<Candidate>, Vec<Candidate>, Option<String>);

async fn timed<F>(slot: usize, name: String, source: Source, deadline: tokio::time::Instant, fut: F) -> TaskOut
where
    F: Future<Output = Result<Found, String>>,
{
    let start = Instant::now();
    let res = timeout_at(deadline, fut).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let mut out = TaskOut { slot, ..Default::default() };
    let (outcome, detail) = match res {
        Err(_) => (SourceOutcome::TimedOut, None),
        Ok(Err(e)) => (SourceOutcome::Error, Some(e)),
        Ok(Ok((i, s, mx))) => {
            let n = i.len() + s.len();
            out.imap = i;
            out.smtp = s;
            out.mx_provider = mx;
            if n == 0 {
                (SourceOutcome::Miss, None)
            } else {
                (SourceOutcome::Hit { candidates: n }, None)
            }
        }
    };
    out.reports.push(SourceReport { name, source, outcome, detail, elapsed_ms });
    out
}

fn sort_outputs(outs: &mut [TaskOut]) {
    outs.sort_by_key(|o| o.slot);
}

fn no_records(e: &hickory_resolver::ResolveError) -> bool {
    e.is_no_records_found()
}

pub async fn detect_with(opts: &DetectOptions, email: &str) -> Result<DetectionResult, DetectError> {
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + opts.budget;
    let parsed = parse_email(email)?;
    let db = ProviderDb::bundled();
    let domain = parsed.domain.clone();

    let mut imap = Vec::new();
    let mut smtp = Vec::new();
    let mut reports = Vec::new();
    let mut provider_id: Option<String> = None;

    // Source 1: bundled data (offline, instant).
    if let Some(p) = db.by_domain(&domain) {
        if p.unsupported {
            return Err(DetectError::UnsupportedProvider { provider: p.name.clone() });
        }
        let (i, s) = p.candidates(Source::Bundled, &domain, true);
        reports.push(SourceReport {
            name: format!("bundled:{}", p.id),
            source: Source::Bundled,
            outcome: SourceOutcome::Hit { candidates: i.len() + s.len() },
            detail: None,
            elapsed_ms: 0,
        });
        imap.extend(i);
        smtp.extend(s);
        provider_id = Some(p.id.clone());
    } else {
        reports.push(SourceReport {
            name: "bundled".into(),
            source: Source::Bundled,
            outcome: SourceOutcome::Miss,
            detail: None,
            elapsed_ms: 0,
        });
    }

    let resolver: Result<TokioResolver, String> = TokioResolver::builder_tokio()
        .map(|mut b| {
            b.options_mut().attempts = 1;
            b.options_mut().timeout = opts.budget;
            b.build()
        })
        .map_err(|e| e.to_string());
    let http = autoconfig::client().map_err(|e| e.to_string());

    let mut set: JoinSet<TaskOut> = JoinSet::new();

    // Sources 2 + 3: HTTPS autoconfig.
    let urls = {
        let mut u1 = reqwest::Url::parse(&format!("https://autoconfig.{domain}/mail/config-v1.1.xml")).ok();
        if let Some(u) = u1.as_mut() {
            u.query_pairs_mut().append_pair("emailaddress", &parsed.address);
        }
        let u2 = reqwest::Url::parse(&format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml")).ok();
        [
            (format!("autoconfig.{domain}"), u1),
            (format!("{domain}/.well-known/autoconfig"), u2),
        ]
    };
    for (slot, (name, url)) in urls.into_iter().enumerate() {
        let http = http.clone();
        let addr = parsed.address.clone();
        set.spawn(timed(slot, name, Source::Autoconfig, deadline, async move {
            let client = http?;
            let url = url.ok_or("bad url")?;
            let body = autoconfig::fetch(&client, url).await?;
            let r = autoconfig::parse_autoconfig(&body, &addr)?;
            Ok((r.imap, r.smtp, None))
        }));
    }

    // Source 4: SRV.
    {
        let resolver = resolver.clone();
        let domain = domain.clone();
        set.spawn(timed(2, format!("srv:{domain}"), Source::Srv, deadline, async move {
            let r = resolver?;
            let lookups = SrvService::ALL.map(|svc| {
                let r = r.clone();
                let email_domain = domain.clone();
                let name = format!("{}.{}.", svc.label(), domain);
                async move {
                    match r.srv_lookup(name).await {
                        Ok(l) => {
                            let recs: Vec<SrvRecord> = l
                                .iter()
                                .map(|s| SrvRecord {
                                    priority: s.priority(),
                                    weight: s.weight(),
                                    port: s.port(),
                                    target: s.target().to_utf8(),
                                })
                                .collect();
                            Ok(srv_candidates_for_domain(svc, &recs, &email_domain))
                        }
                        Err(e) if no_records(&e) => Ok(Vec::new()),
                        Err(e) => Err(e.to_string()),
                    }
                }
            });
            let [a, b, c, d] = lookups;
            let (a, b, c, d) = tokio::join!(a, b, c, d);
            let (mut i, mut s) = (Vec::new(), Vec::new());
            let mut first_err = None;
            for (svc, res) in SrvService::ALL.into_iter().zip([a, b, c, d]) {
                match res {
                    Ok(v) if svc.is_imap() => i.extend(v),
                    Ok(v) => s.extend(v),
                    Err(e) => first_err = first_err.or(Some(e)),
                }
            }
            if i.is_empty() && s.is_empty() {
                if let Some(e) = first_err {
                    return Err(e);
                }
            }
            Ok((i, s, None))
        }));
    }

    // Source 5: MX -> provider.
    {
        let resolver = resolver.clone();
        let domain = domain.clone();
        set.spawn(timed(3, format!("mx:{domain}"), Source::Mx, deadline, async move {
            let r = resolver?;
            let mx = match r.mx_lookup(format!("{domain}.")).await {
                Ok(m) => m,
                Err(e) if no_records(&e) => return Ok((vec![], vec![], None)),
                Err(e) => return Err(e.to_string()),
            };
            let mut recs: Vec<(u16, String)> =
                mx.iter().map(|m| (m.preference(), m.exchange().to_utf8())).collect();
            recs.sort();
            let hosts: Vec<String> = recs.into_iter().map(|(_, h)| h).collect();
            match ProviderDb::bundled().by_mx(&hosts) {
                Some(p) if p.unsupported => Ok((vec![], vec![], Some(format!("!{}", p.name)))),
                Some(p) => {
                    let (i, s) = p.candidates(Source::Mx, &domain, false);
                    Ok((i, s, Some(p.id.clone())))
                }
                None => Ok((vec![], vec![], None)),
            }
        }));
    }

    // Source 6: heuristics, only for hosts that exist in DNS.
    {
        let resolver = resolver.clone();
        let domain = domain.clone();
        set.spawn(timed(4, format!("heuristic:{domain}"), Source::Heuristic, deadline, async move {
            let r = resolver?;
            let (ih, sh) = heuristic_hosts(&domain);
            let mut all: Vec<String> = ih.iter().chain(sh.iter()).cloned().collect();
            all.sort();
            all.dedup();
            let checks = all.iter().map(|h| {
                let r = r.clone();
                let h = format!("{h}.");
                async move { r.lookup_ip(h).await.is_ok() }
            });
            let results = join_all(checks).await;
            let live: Vec<&String> = all.iter().zip(results).filter(|(_, ok)| *ok).map(|(h, _)| h).collect();
            let ih: Vec<String> = ih.into_iter().filter(|h| live.contains(&h)).collect();
            let sh: Vec<String> = sh.into_iter().filter(|h| live.contains(&h)).collect();
            let (i, s) = heuristic_candidates(&ih, &sh);
            Ok((i, s, None))
        }));
    }

    let mut unsupported: Option<String> = None;
    let mut outs = Vec::new();
    while let Some(joined) = set.join_next().await {
        if let Ok(out) = joined {
            outs.push(out);
        }
    }
    sort_outputs(&mut outs);
    for out in outs {
        reports.extend(out.reports);
        imap.extend(out.imap);
        smtp.extend(out.smtp);
        if let Some(mx) = out.mx_provider {
            if let Some(name) = mx.strip_prefix('!') {
                unsupported = Some(name.to_string());
            } else if provider_id.is_none() {
                provider_id = Some(mx);
            }
        }
    }
    if let Some(provider) = unsupported {
        return Err(DetectError::UnsupportedProvider { provider });
    }

    let imap = rank(imap);
    let smtp = rank(smtp);
    if imap.is_empty() || smtp.is_empty() {
        return Err(DetectError::NoCandidates);
    }
    Ok(DetectionResult {
        email: parsed.address,
        domain,
        provider: provider_id,
        imap,
        smtp,
        sources: reports,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Minimal join_all (avoids the `futures` dependency).
async fn join_all<I, F>(it: I) -> Vec<F::Output>
where
    I: IntoIterator<Item = F>,
    F: Future,
{
    let mut set = Vec::new();
    for f in it {
        set.push(Box::pin(f));
    }
    let mut out = Vec::with_capacity(set.len());
    // Poll all concurrently via a tokio LocalSet-free approach: sequential
    // await would serialize lookups, so drive them with select-free polling.
    let mut pending: Vec<Option<std::pin::Pin<Box<F>>>> = set.into_iter().map(Some).collect();
    let mut results: Vec<Option<F::Output>> = (0..pending.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut done = true;
        for (i, slot) in pending.iter_mut().enumerate() {
            if let Some(f) = slot {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => {
                        results[i] = Some(v);
                        *slot = None;
                    }
                    std::task::Poll::Pending => done = false,
                }
            }
        }
        if done { std::task::Poll::Ready(()) } else { std::task::Poll::Pending }
    })
    .await;
    out.extend(results.into_iter().map(|r| r.expect("polled to completion")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(slot: usize, host: &str) -> TaskOut {
        TaskOut {
            slot,
            imap: vec![Candidate {
                host: host.into(),
                port: 993,
                tls: TlsMode::Implicit,
                source: Source::Autoconfig,
                username_hint: UsernameHint::Email,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn merge_order_is_independent_of_completion_order() {
        let mut a = vec![out(1, "wellknown.x"), out(0, "autoconfig.x")];
        let mut b = vec![out(0, "autoconfig.x"), out(1, "wellknown.x")];
        sort_outputs(&mut a);
        sort_outputs(&mut b);
        let hosts = |v: &[TaskOut]| rank(v.iter().flat_map(|o| o.imap.clone()).collect()).into_iter().map(|c| c.host).collect::<Vec<_>>();
        assert_eq!(hosts(&a), hosts(&b));
        assert_eq!(hosts(&a), ["autoconfig.x", "wellknown.x"]);
    }
}
