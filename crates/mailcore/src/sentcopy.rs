//! Decide whether MiraMail must APPEND its own copy of a sent message to the Sent folder.
//!
//! Many providers (Zoho, Gmail, Microsoft, ...) file SMTP submissions into Sent themselves, so an unconditional
//! APPEND leaves two copies. After a send we look for the message's Message-ID in Sent, a few times (the server
//! may file its copy asynchronously). Found: do not append. Not found, no Message-ID, or any error: append
//! (a missing Sent copy is worse than a duplicate).

use crate::{batch::find_by_message_id, ImapClient, Result};
use std::time::Duration;

/// Delays before each successive check when the server is unknown or is known to save its own copy
/// (checks at 0, 1.5, 4.5 and 10.5 s).
pub const WAIT_SCHEDULE_MS: [u64; 4] = [0, 1500, 3000, 6000];
/// The delays to use. Always the full schedule: one slow filing must not teach us "this server never saves a
/// copy" and bring duplicates back; the checks run in the background after the send, so waiting costs nothing
/// visible. `known` is kept for the log/future tuning.
pub fn schedule(_known_server_saves: Option<bool>) -> &'static [u64] {
    &WAIT_SCHEDULE_MS
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub append: bool,
    /// What to remember for the account: `Some(true)` the server saved its own copy, `Some(false)` it did not,
    /// `None` nothing learned (no Message-ID or the check failed).
    pub learned: Option<bool>,
}

/// Run the checks. `check()` returns whether the Sent folder holds the message now; `sleep` waits between checks.
pub async fn decide<C, CF, S, SF>(has_message_id: bool, known: Option<bool>, mut check: C, mut sleep: S) -> Decision
where
    C: FnMut() -> CF,
    CF: std::future::Future<Output = Result<bool>>,
    S: FnMut(Duration) -> SF,
    SF: std::future::Future<Output = ()>,
{
    if !has_message_id {
        return Decision { append: true, learned: None };
    }
    for ms in schedule(known) {
        if *ms > 0 {
            sleep(Duration::from_millis(*ms)).await;
        }
        match check().await {
            Ok(true) => return Decision { append: false, learned: Some(true) },
            Ok(false) => {}
            Err(_) => return Decision { append: true, learned: None },
        }
    }
    Decision { append: true, learned: Some(false) }
}

/// Is a message with this Message-ID (without `<>`) in `folder`? SELECTs the folder (fresh view of new mail).
pub async fn folder_has_message_id(client: &mut ImapClient, folder: &str, message_id: &str) -> Result<bool> {
    client.select(folder).await?;
    Ok(!find_by_message_id(client, message_id).await?.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use std::cell::{Cell, RefCell};

    async fn run(mid: bool, known: Option<bool>, script: Vec<Result<bool>>) -> (Decision, usize, Vec<u64>) {
        let script = RefCell::new(script.into_iter());
        let calls = Cell::new(0usize);
        let sleeps = RefCell::new(Vec::new());
        let d = decide(
            mid,
            known,
            || {
                calls.set(calls.get() + 1);
                let r = script.borrow_mut().next().unwrap_or(Ok(false));
                async move { r }
            },
            |d| {
                sleeps.borrow_mut().push(d.as_millis() as u64);
                async {}
            },
        )
        .await;
        (d, calls.get(), sleeps.into_inner())
    }

    #[tokio::test]
    async fn found_immediately_means_no_append() {
        let (d, calls, sleeps) = run(true, None, vec![Ok(true)]).await;
        assert_eq!(d, Decision { append: false, learned: Some(true) });
        assert_eq!((calls, sleeps.len()), (1, 0));
    }

    #[tokio::test]
    async fn found_on_a_later_attempt_means_no_append() {
        let (d, calls, sleeps) = run(true, None, vec![Ok(false), Ok(false), Ok(true)]).await;
        assert!(!d.append);
        assert_eq!((calls, sleeps), (3, vec![1500, 3000]));
    }

    #[tokio::test]
    async fn never_found_appends_after_all_attempts() {
        let (d, calls, sleeps) = run(true, None, vec![]).await;
        assert_eq!(d, Decision { append: true, learned: Some(false) });
        assert_eq!((calls, sleeps), (4, vec![1500, 3000, 6000]));
        assert!(sleeps_total() < 12_000);
    }

    fn sleeps_total() -> u64 {
        WAIT_SCHEDULE_MS.iter().sum()
    }

    #[tokio::test]
    async fn server_seen_not_to_save_still_waits_the_full_schedule() {
        let (d, calls, sleeps) = run(true, Some(false), vec![Ok(false), Ok(false), Ok(false), Ok(true)]).await;
        assert_eq!(d, Decision { append: false, learned: Some(true) });
        assert_eq!((calls, sleeps), (4, vec![1500, 3000, 6000]));
    }
    #[tokio::test]
    async fn server_known_to_save_waits_the_long_schedule() {
        let (d, calls, _) = run(true, Some(true), vec![]).await;
        assert!(d.append);
        assert_eq!(calls, 4);
    }

    #[tokio::test]
    async fn no_message_id_appends_without_checking() {
        let (d, calls, _) = run(false, Some(true), vec![Ok(true)]).await;
        assert_eq!(d, Decision { append: true, learned: None });
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn search_error_appends_and_learns_nothing() {
        let (d, calls, _) = run(true, None, vec![Ok(false), Err(Error::Timeout("search")), Ok(true)]).await;
        assert_eq!(d, Decision { append: true, learned: None });
        assert_eq!(calls, 2);
    }
}
