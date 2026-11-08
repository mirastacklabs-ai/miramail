//! Push (IDLE) with polling fallback and reconnect-with-backoff.

use crate::imap::ImapClient;
use crate::types::{ServerConfig, WatchEvent};
use crate::{Error, Result};
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, timeout};

/// Polling interval when the server has no IDLE.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// IDLE is restarted this often. Kept short so a dead socket after sleep/wake
/// is noticed quickly (monotonic timers pause during system sleep).
pub const IDLE_CYCLE: Duration = Duration::from_secs(120);

/// 1s, 2s, 4s ... capped at 60s.
pub fn backoff_delay(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.min(6)).min(60))
}

/// True if far more wall-clock time passed than the monotonic wait implies
/// (machine slept): the connection must be considered dead.
pub fn slept_through(expected: Duration, wall_elapsed: Duration) -> bool {
    wall_elapsed > expected + Duration::from_secs(30)
}

/// Watch `folder` until `stop` flips to true (or its sender is dropped).
/// Emits `WatchEvent`s; the caller reacts to `NewMail` with `sync_folder`.
pub async fn watch_folder(
    cfg: ServerConfig,
    username: String,
    password: String,
    folder: String,
    events: mpsc::Sender<WatchEvent>,
    mut stop: watch::Receiver<bool>,
) {
    let mut attempt: u32 = 0;
    loop {
        if *stop.borrow() {
            return;
        }
        match run_once(&cfg, &username, &password, &folder, &events, &mut stop, &mut attempt).await {
            Ok(()) => return, // stopped
            Err(e) => {
                let fatal = matches!(e, Error::AuthFailed);
                if events.send(WatchEvent::Disconnected { reason: e.to_string() }).await.is_err() {
                    return;
                }
                if fatal {
                    return; // bad credentials: retrying only risks a lockout
                }
            }
        }
        let delay = backoff_delay(attempt);
        attempt = attempt.saturating_add(1);
        if events.send(WatchEvent::Reconnecting { attempt, delay_secs: delay.as_secs() }).await.is_err() {
            return;
        }
        tokio::select! {
            _ = sleep(delay) => {}
            r = stop.changed() => { if r.is_err() || *stop.borrow() { return; } }
        }
    }
}

const IDLE_INIT_TIMEOUT: Duration = Duration::from_secs(20);
const IDLE_DONE_TIMEOUT: Duration = Duration::from_secs(20);

async fn run_once(
    cfg: &ServerConfig,
    username: &str,
    password: &str,
    folder: &str,
    events: &mpsc::Sender<WatchEvent>,
    stop: &mut watch::Receiver<bool>,
    attempt: &mut u32,
) -> Result<()> {
    let mut client = ImapClient::connect(cfg, username, password).await?;
    let idle = client.supports_idle();
    let (_, mut exists, mut uidnext) = client.select(folder).await?;
    let _ = events.send(WatchEvent::Connected { idle }).await;
    let mut stopped = false;

    loop {
        if stopped || *stop.borrow() {
            let _ = client.logout().await;
            return Ok(());
        }
        let wall = SystemTime::now();
        let wait = if idle { IDLE_CYCLE } else { POLL_INTERVAL };

        if idle {
            let mut session = client.into_session();
            let mut h = session.idle();
            timeout(IDLE_INIT_TIMEOUT, h.init()).await.map_err(|_| Error::Timeout("idle init"))??;
            let (fut, interrupt) = h.wait_with_timeout(wait);
            let res = tokio::select! {
                r = timeout(wait + Duration::from_secs(30), fut) => Some(r),
                r = stop.changed() => { stopped = r.is_err() || *stop.borrow(); drop(interrupt); None }
            };
            match res {
                Some(Err(_)) => return Err(Error::Timeout("idle")),
                Some(Ok(Err(e))) => return Err(e.into()),
                _ => {}
            }
            // After a suspend the socket is dead; DONE would just hang.
            if slept_through(wait, wall.elapsed().unwrap_or_default()) {
                return Err(Error::Timeout("resumed from sleep"));
            }
            session = timeout(IDLE_DONE_TIMEOUT, h.done()).await.map_err(|_| Error::Timeout("idle done"))??;
            client = ImapClient::from_session(session, true);
        } else {
            tokio::select! {
                _ = sleep(wait) => {}
                r = stop.changed() => { stopped = r.is_err() || *stop.borrow(); }
            }
        }
        if stopped || *stop.borrow() {
            continue;
        }
        let elapsed = wall.elapsed().unwrap_or_default();
        if slept_through(wait, elapsed) {
            return Err(Error::Timeout("resumed from sleep"));
        }
        // Re-SELECT doubles as liveness check and change detection.
        let (_, ex, un) = timeout(Duration::from_secs(20), client.select(folder))
            .await
            .map_err(|_| Error::Timeout("select"))??;
        if ex != exists || un != uidnext {
            let grew = ex > exists || un.zip(uidnext).is_some_and(|(a, b)| a > b);
            exists = ex;
            uidnext = un;
            if grew && events.send(WatchEvent::NewMail).await.is_err() {
                return Ok(());
            }
        }
        // A full connect + IDLE/poll + SELECT cycle worked: connection is healthy.
        *attempt = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let v: Vec<u64> = (0..10).map(|a| backoff_delay(a).as_secs()).collect();
        assert_eq!(v, vec![1, 2, 4, 8, 16, 32, 60, 60, 60, 60]);
        assert_eq!(backoff_delay(u32::MAX).as_secs(), 60);
    }

    #[test]
    fn sleep_detection() {
        let w = Duration::from_secs(120);
        assert!(!slept_through(w, Duration::from_secs(121)));
        assert!(!slept_through(w, Duration::from_secs(149)));
        assert!(slept_through(w, Duration::from_secs(3600)));
    }

    #[tokio::test]
    async fn dropped_stop_sender_stops_watcher() {
        let (tx, _rx) = mpsc::channel(8);
        let (stop_tx, stop_rx) = watch::channel(false);
        drop(stop_tx);
        let cfg = ServerConfig { host: "127.0.0.1".into(), port: 1, tls: crate::types::Tls::Implicit };
        let r = timeout(Duration::from_secs(10), watch_folder(cfg, "u".into(), "p".into(), "INBOX".into(), tx, stop_rx)).await;
        assert!(r.is_ok(), "watcher must exit when the stop sender is dropped");
    }

    #[tokio::test]
    async fn stop_before_start_returns_immediately() {
        let (tx, mut rx) = mpsc::channel(4);
        let (stop_tx, stop_rx) = watch::channel(true);
        let cfg = ServerConfig { host: "127.0.0.1".into(), port: 1, tls: crate::types::Tls::Implicit };
        watch_folder(cfg, "u".into(), "p".into(), "INBOX".into(), tx, stop_rx).await;
        assert!(rx.try_recv().is_err());
        drop(stop_tx);
    }
}
