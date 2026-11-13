//! iPhone background new-mail check (phase 3). No push service and no server of ours: iOS wakes the app now and then
//! (`BGAppRefreshTask`, see `plugins/mailcheck`), this connects to the user's own IMAP servers, stores what is new
//! through the normal sync path and posts local notifications.
//!
//! It only READS and STORES: it never sends, moves, flags, deletes or runs rules (blocked senders and auto-read mutes
//! are simply not announced; `localorg::classify_arrivals` is used for the lists, not for the actions). Everything
//! else (rules, auto-reply, Junk moves, auto-read, scheduled send, snooze, CalDAV, ...) holds back while the user is
//! away (presence.rs) and runs, or catches up, once they open the app: rules / auto-reply through their cursors (the
//! catch-up or launch pass), Junk moves / auto-read through the live poller, which starts from the `fg_floor` this
//! check records (see `take_floors`). The notification permission is only READ here (never prompted; the prompt stays
//! behind the user's tap in Settings): without it nothing is posted, mail is still stored and the marks still advance.
//! What to announce is decided by the same code as on the Mac (`localorg::classify_arrivals`, `notify::plan_with`, the
//! `PlatformPrefs` switches); this file adds only the part the Mac does not need: remembering, per account, how far
//! mail has already been announced, across launches (`Mark`).
//!
//! Never logged or returned: passwords, tokens, senders, subjects, bodies, addresses. Only counts.
// The check itself runs on iOS only; the decisions below are plain functions that are tested everywhere.
#![cfg_attr(not(target_os = "ios"), allow(dead_code))]

use crate::badge::{new_arrivals, Recent};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE: &str = "mailcheck_state.json";
/// Hard cap for one background run (iOS gives roughly 30 s; the rest is slack for waking up and finishing).
pub const BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
/// Most unread INBOX rows looked at per account per run.
pub const UNSEEN_WINDOW: u32 = 100;

/// How far an account's INBOX has been announced. Valid only for the UIDVALIDITY it was recorded under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub validity: u32,
    /// Everything up to this UID is either announced, or was there when the baseline was taken.
    pub last_uid: u32,
    /// A run started syncing and did not finish evaluating (cut short, killed, failed after storing, or every post
    /// failed): what the cache holds beyond `last_uid` may never have been announced, so the next run must not treat it
    /// as "already seen".
    #[serde(default)]
    pub inflight: bool,
    /// The newest INBOX UID the open app had in its cache when background checks started storing mail for it (the first
    /// run since the user was last in front of the app). The live poller takes it as its starting point, so the Junk
    /// moves / auto-read for mail stored meanwhile still happen once the user is back. `None` once taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fg_floor: Option<u32>,
}

/// One small JSON file next to the databases; missing or corrupt means "no baselines yet" (the next run only records).
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Marks {
    pub accounts: BTreeMap<String, Mark>,
}

impl Marks {
    pub fn load(path: &Path) -> Marks {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    /// Atomic write (temp file in the same directory, then rename): a kill never leaves half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, path)
    }

    /// [`Marks::save`], tried twice (a passing hiccup must not make the next run repeat notifications).
    pub fn save_retry(&self, path: &Path) -> std::io::Result<()> {
        self.save(path).or_else(|_| self.save(path))
    }

    /// Forget accounts that are removed or whose notifications are off, so turning them on again starts from a fresh
    /// baseline instead of announcing everything that arrived meanwhile. True when something was dropped.
    pub fn retain_accounts(&mut self, keep: &[String]) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a, _| keep.contains(a));
        self.accounts.len() != before
    }

    /// Hand every recorded `fg_floor` to the open app (account -> (validity, floor)) and clear them. True in the second
    /// value when marks changed (the caller saves).
    pub fn take_floors(&mut self) -> (BTreeMap<String, (u32, u32)>, bool) {
        let mut out = BTreeMap::new();
        for (a, m) in self.accounts.iter_mut() {
            if let Some(f) = m.fg_floor.take() {
                out.insert(a.clone(), (m.validity, f));
            }
        }
        let changed = !out.is_empty();
        (out, changed)
    }
}

/// The outcome of looking at one account's INBOX after its sync.
#[derive(Debug, PartialEq, Eq)]
pub struct Eval {
    /// Unread mail to announce, newest first (before the block / mute / VIP lists).
    pub arrivals: Vec<Recent>,
    pub next: Mark,
}

/// Decide what is new.
/// * `prev`: the stored mark. `None`, or another UIDVALIDITY (UIDs restarted): only record a baseline, announce nothing.
/// * `validity`, `uidnext`: the server's, from the sync that just ran (`uidnext - 1` is the newest UID that exists).
/// * `synced`: that sync succeeded. If not, `uidnext` may be ahead of what was stored, so the mark advances only to
///   `post_max` (the newest cached UID): mail not fetched yet is still announced by a later run.
/// * `pre_max`: the newest cached INBOX UID BEFORE this run's sync, if that cache belonged to `validity`. Mail the app
///   already had was visible to the user (or announced by the live poller), so it is not announced again. Ignored when
///   the previous run did not finish (`prev.inflight`): that run already folded its own `pre_max` into `last_uid`
///   ([`begin_run`]), and what the cache gained after it may be unannounced.
/// * `post_max`: newest cached INBOX UID after the sync. `unseen`: the cache's unread INBOX rows.
pub fn evaluate(prev: Option<Mark>, validity: u32, uidnext: Option<u32>, pre_max: Option<u32>, post_max: Option<u32>, synced: bool, unseen: &[Recent]) -> Eval {
    let top = if synced { uidnext.map_or(0, |n| n.saturating_sub(1)).max(post_max.unwrap_or(0)) } else { post_max.unwrap_or(0) };
    match prev {
        Some(p) if p.validity == validity => {
            let floor = if p.inflight { p.last_uid } else { p.last_uid.max(pre_max.unwrap_or(0)) };
            let (arrivals, _) = new_arrivals(Some(floor), unseen);
            Eval { arrivals, next: Mark { validity, last_uid: p.last_uid.max(top), inflight: false, fg_floor: p.fg_floor.or(pre_max) } }
        }
        _ => Eval { arrivals: Vec::new(), next: Mark { validity, last_uid: top, inflight: false, fg_floor: pre_max } },
    }
}

/// Record that a run started (so a kill mid-run is noticed). Only for an account that already has a finished mark.
/// `pre_max`: the newest cached INBOX UID before the sync, if that cache belongs to the mark's UIDVALIDITY; it is
/// folded into `last_uid` now, so a run that never finishes cannot make the next one announce mail the app already had,
/// and it becomes the `fg_floor` unless one is already recorded. True when the stored marks changed.
pub fn begin_run(marks: &mut Marks, email: &str, pre_max: Option<u32>) -> bool {
    match marks.accounts.get(email).copied() {
        Some(p) if !p.inflight => {
            let fg_floor = p.fg_floor.or(pre_max);
            marks.accounts.insert(email.to_string(), Mark { inflight: true, last_uid: p.last_uid.max(pre_max.unwrap_or(0)), fg_floor, ..p });
            true
        }
        _ => false,
    }
}

/// A run ended with an error before it could have stored anything (account / Keychain / storage trouble before the
/// sync, or no INBOX state at all): it left nothing unannounced behind, so put the mark back as it was instead of
/// leaving `inflight` set. A run that failed AFTER its sync may have stored mail; it keeps `inflight` (see [`Failure`]).
/// True when marks changed.
pub fn abort_run(marks: &mut Marks, email: &str, prev: Option<Mark>) -> bool {
    match (prev, marks.accounts.get(email)) {
        (Some(p), Some(now)) if now.inflight && !p.inflight => {
            marks.accounts.insert(email.to_string(), p);
            true
        }
        _ => false,
    }
}

/// How one account's check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The sync may have stored mail: the mark must stay `inflight` (restoring the old one would make the next run take
    /// that mail for "already there" and never announce it).
    pub after_sync: bool,
    pub msg: String,
}

impl Failure {
    pub fn before(msg: impl Into<String>) -> Self {
        Failure { after_sync: false, msg: msg.into() }
    }
    pub fn after(msg: impl Into<String>) -> Self {
        Failure { after_sync: true, msg: msg.into() }
    }
    /// Should the run put the previous mark back?
    pub fn restores_mark(&self) -> bool {
        !self.after_sync
    }
}

/// The mark to store after announcing. `None`: leave the in-flight mark from [`begin_run`] in place, because there was
/// something to show, it could be shown, and not a single notification got through: the next run tries again (a
/// repeat is better than a lost notification). Partly posted counts as done.
pub fn settle(next: Mark, planned: usize, posted: usize, allowed: bool) -> Option<Mark> {
    if allowed && planned > 0 && posted == 0 {
        None
    } else {
        Some(next)
    }
}

/// Set when a run recorded a floor (and at process start): the live poller takes the floors on its next tick.
pub static FLOORS_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The open app's side of `fg_floor`: load, take and clear the floors in the marks file at `path`. Only floors are
/// written back. If the user opens the app while a background run is still going, that run's own save may put the
/// floor back; the poller then repeats those block / mute actions next time, which is harmless (a moved message is no
/// longer in the Inbox, marking read twice changes nothing). Accounts whose notifications are off get no background
/// run, so nothing is stored for them and they need no floor.
pub fn take_floors(path: &Path) -> BTreeMap<String, (u32, u32)> {
    let mut marks = Marks::load(path);
    let (floors, changed) = marks.take_floors();
    if changed {
        let _ = marks.save_retry(path);
    }
    floors
}

#[cfg(target_os = "ios")]
pub use run::run;

#[cfg(target_os = "ios")]
mod run {
    use super::*;
    use crate::badge::{cache_unread_total, dock_count, to_recent};
    use crate::localorg::{classify_arrivals, LocalOrg};
    use crate::notify;
    use crate::plat_prefs::PlatformState;
    use crate::state::{err, AppState, INBOX};
    use std::sync::Arc;
    use std::time::Instant;
    use tauri::{AppHandle, Manager};

    /// What a run did; counts only.
    #[derive(Debug, Default)]
    pub struct Summary {
        pub accounts: u32,
        pub failed: u32,
        pub new: u32,
        pub notified: u32,
        pub cut: bool,
    }

    impl Summary {
        pub fn text(&self) -> String {
            format!("accounts={} failed={} new={} notified={} cut={}", self.accounts, self.failed, self.new, self.notified, self.cut)
        }
    }

    type Cancelled = Arc<dyn Fn() -> bool + Send + Sync>;

    async fn until_cancelled(c: &Cancelled) {
        while !c() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// One background run over every account that notifies. `cancelled` turns true when iOS says time is up.
    pub async fn run(app: &AppHandle, cancelled: Cancelled) -> Result<Summary, String> {
        let started = Instant::now();
        let st = app.state::<AppState>();
        let prefs = app.state::<PlatformState>().get();
        let storage = st.storage.clone();
        let path = app.path().app_data_dir().map_err(err)?.join(FILE);
        let all: Vec<String> = storage.list_accounts().map_err(err)?.into_iter().map(|a| a.email).collect();
        let targets: Vec<String> = if prefs.notify { all.iter().filter(|a| !prefs.notify_off.contains(a)).cloned().collect() } else { Vec::new() };

        let mut marks = Marks::load(&path);
        if marks.retain_accounts(&targets) {
            let _ = marks.save(&path);
        }
        let mut sum = Summary::default();
        if targets.is_empty() {
            return Ok(sum);
        }
        // Without permission nothing can be shown; the mail is still stored and the baselines still advance (no flood
        // of old notifications later if the user allows them).
        // Read-only (getNotificationSettings): a background run must never raise the permission prompt.
        let allowed = {
            let a = app.clone();
            let probe = tauri::async_runtime::spawn_blocking(move || tauri_plugin_mailcheck::notifications_allowed(&a));
            matches!(tokio::time::timeout(std::time::Duration::from_secs(3), probe).await, Ok(Ok(true)))
        };
        let lo = app.try_state::<LocalOrg>().map(|s| s.inner().clone());

        for email in &targets {
            let left = BUDGET.saturating_sub(started.elapsed());
            if cancelled() || left.is_zero() {
                sum.cut = true;
                break;
            }
            sum.accounts += 1;
            let prev = marks.accounts.get(email).copied();
            let work = check_account(&st, lo.as_ref(), &prefs, email, &mut marks, &path, all.len(), allowed, started, &cancelled);
            let r = tokio::select! {
                r = tokio::time::timeout(left, work) => r.map_err(|_| ()),
                _ = until_cancelled(&cancelled) => Err(()),
            };
            match r {
                Ok(Ok((new, notified))) => {
                    sum.new += new;
                    sum.notified += notified;
                }
                Ok(Err(f)) => {
                    sum.failed += 1;
                    // Only a failure before the sync puts the old mark back; after it, mail may have been stored.
                    if f.restores_mark() && abort_run(&mut marks, email, prev) {
                        let _ = marks.save_retry(&path);
                    }
                }
                Err(()) => {
                    sum.cut = true;
                    break;
                }
            }
        }

        // The open app's poller picks up the floors (Junk moves / auto-read for what was stored) when the user is back.
        if marks.accounts.values().any(|m| m.fg_floor.is_some()) {
            FLOORS_PENDING.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        // Badge: the Inbox unread count from the cache, whatever the outcome (mail may have been stored before a cut).
        let (app2, lo2, dock) = (app.clone(), lo.clone(), prefs.dock_badge);
        let _ = tauri::async_runtime::spawn_blocking(move || {
            let total = cache_unread_total(&app2.state::<AppState>().storage, lo2.as_ref());
            tauri_plugin_mailcheck::set_badge(&app2, dock_count(dock, total).unwrap_or(0));
        })
        .await;
        Ok(sum)
    }

    /// Sync one account's INBOX, work out what is new and announce it. Returns (new unread found, notifications posted).
    #[allow(clippy::too_many_arguments)]
    async fn check_account(
        st: &AppState,
        lo: Option<&LocalOrg>,
        prefs: &crate::plat_prefs::PlatformPrefs,
        email: &str,
        marks: &mut Marks,
        path: &Path,
        account_count: usize,
        allowed: bool,
        started: Instant,
        cancelled: &Cancelled,
    ) -> Result<(u32, u32), Failure> {
        let storage = &st.storage;
        let before = |e: String| Failure::before(e);
        let after = |e: String| Failure::after(e);
        let (pre_validity, _) = storage.folder_sync_state(email, INBOX).map_err(err).map_err(before)?;
        let pre_max = storage.max_uid(email, INBOX).map_err(err).map_err(before)?;
        let prev = marks.accounts.get(email).copied();
        // Mark the run as started: if it is cut or killed before it finishes, the next one will not trust the cache.
        let pre_for_prev = if prev.is_some_and(|p| Some(p.validity) == pre_validity) { pre_max } else { None };
        if begin_run(marks, email, pre_for_prev) {
            marks.save_retry(path).map_err(err).map_err(before)?;
        }
        // The normal sync path: stores new headers (per message), refreshes flags, handles UIDVALIDITY changes. A failure
        // after some rows were stored is not fatal: what is in the cache is evaluated below.
        let synced = match st.connect(email).await {
            Ok(mut conn) => {
                let r = conn.client.sync_folder(storage, email, INBOX, st.initial_sync_limit()).await;
                conn.close().await;
                r.map(|_| ()).map_err(err)
            }
            Err(e) => Err(e),
        };
        // From here on, mail may have been stored: a failure leaves the mark in flight.
        let (validity, uidnext) = storage.folder_sync_state(email, INBOX).map_err(err).map_err(after)?;
        let Some(validity) = validity else {
            // No INBOX state at all: nothing was stored, the old mark is still right.
            return Err(Failure::before(synced.err().unwrap_or_else(|| "no INBOX state".to_string())));
        };
        let post_max = storage.max_uid(email, INBOX).map_err(err).map_err(after)?;
        let unseen: Vec<Recent> = storage.recent_unseen(email, INBOX, UNSEEN_WINDOW).map_err(err).map_err(after)?.iter().map(to_recent).collect();
        let pre = if pre_validity == Some(validity) { pre_max } else { None };
        let ev = evaluate(prev, validity, uidnext, pre, post_max, synced.is_ok(), &unseen);
        let new = ev.arrivals.len() as u32;

        // Same lists as the Mac (block / mute / VIP-only); only `keep` is used: nothing is moved or marked here.
        let keep = match lo {
            Some(l) => classify_arrivals(storage, l, prefs, ev.arrivals.clone()).map_or(ev.arrivals, |c| c.keep),
            None => ev.arrivals,
        };
        let mut notified = 0u32;
        let plan = if allowed { notify::plan_with(&keep, prefs, account_count, notify::MAX_INDIVIDUAL_BG) } else { Vec::new() };
        for spec in &plan {
            if notify::show_confirmed(spec, prefs.notify_sound).await.is_ok() {
                notified += 1;
            }
        }
        match settle(ev.next, plan.len(), notified as usize, allowed) {
            Some(next) => {
                marks.accounts.insert(email.to_string(), next);
                // Kept in memory even if this fails: a later save in this run (next account) may still write it.
                marks.save_retry(path).map_err(err).map_err(after)?;
            }
            // Nothing got through: leave the in-flight mark so the next run tries again.
            None => return Err(Failure::after("no notification could be posted")),
        }
        // So a tap on a notification opens instantly: the bodies of the (at most 5) messages just announced, after the
        // notifications are out, inside the budget (3 s kept back for the badge and the finish). Counts only in the log.
        if notified > 0 {
            let uids: Vec<u32> = keep.iter().take(crate::notify::MAX_INDIVIDUAL_BG).map(|r| r.uid).collect();
            let deadline = started + BUDGET.saturating_sub(std::time::Duration::from_secs(3));
            if Instant::now() + std::time::Duration::from_secs(2) < deadline {
                let c = cancelled.clone();
                let _ = crate::prefetch::for_announced(st, email, uids, deadline, move || c()).await;
            }
        }
        // Syncing failed (offline / auth) but the cache was readable: report it as a failure so the task is not "successful".
        synced.map(|_| (new, notified)).map_err(after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(uid: u32, date: i64) -> Recent {
        Recent { account: "a@x.org".into(), folder: "INBOX".into(), uid, from: "Ann".into(), subject: Some("Hi".into()), date }
    }
    fn mark(v: u32, last: u32) -> Mark {
        Mark { validity: v, last_uid: last, inflight: false, fg_floor: None }
    }
    fn fl(m: Mark, f: u32) -> Mark {
        Mark { fg_floor: Some(f), ..m }
    }

    #[test]
    fn first_run_only_records_the_baseline_even_with_a_full_unread_inbox() {
        let unseen: Vec<Recent> = (1..=40).map(|u| r(u, u as i64)).collect();
        let e = evaluate(None, 7, Some(41), None, Some(40), true, &unseen);
        assert!(e.arrivals.is_empty());
        assert_eq!(e.next, mark(7, 40), "UIDNEXT - 1");
    }

    #[test]
    fn baseline_uses_the_cache_when_the_server_gave_no_uidnext() {
        assert_eq!(evaluate(None, 7, None, None, Some(12), true, &[]).next, mark(7, 12));
        assert_eq!(evaluate(None, 7, None, None, None, true, &[]).next, mark(7, 0));
    }

    #[test]
    fn newer_unread_mail_is_announced_newest_first_and_the_mark_advances() {
        let unseen = vec![r(10, 10), r(11, 11), r(12, 12)];
        let e = evaluate(Some(mark(7, 10)), 7, Some(13), None, Some(12), true, &unseen);
        assert_eq!(e.arrivals.iter().map(|x| x.uid).collect::<Vec<_>>(), vec![12, 11]);
        assert_eq!(e.next, mark(7, 12));
    }

    #[test]
    fn read_mail_is_not_in_the_unread_list_so_it_is_skipped() {
        // uid 11 arrived and was read elsewhere: the cache lists only unread rows
        let e = evaluate(Some(mark(7, 10)), 7, Some(13), None, Some(12), true, &[r(12, 12)]);
        assert_eq!(e.arrivals.len(), 1);
        assert_eq!(e.next.last_uid, 12);
    }

    #[test]
    fn a_uidvalidity_change_resets_silently() {
        let unseen = vec![r(1, 1), r(2, 2)];
        // (the caller passes no pre_max when the cache belonged to another UIDVALIDITY)
        let e = evaluate(Some(mark(7, 500)), 8, Some(3), None, Some(2), true, &unseen);
        assert!(e.arrivals.is_empty());
        assert_eq!(e.next, mark(8, 2), "UIDs restarted lower: the old mark would hide everything");
    }

    #[test]
    fn mail_the_app_already_held_before_the_run_is_not_announced_again() {
        // seen in the open app (cached, still unread) at uid 11-12; the mark is old (10); one genuinely new (13)
        let unseen = vec![r(11, 11), r(12, 12), r(13, 13)];
        let e = evaluate(Some(mark(7, 10)), 7, Some(14), Some(12), Some(13), true, &unseen);
        assert_eq!(e.arrivals.iter().map(|x| x.uid).collect::<Vec<_>>(), vec![13]);
        assert_eq!(e.next.last_uid, 13);
    }

    #[test]
    fn after_an_unfinished_run_the_cache_is_not_trusted() {
        let unseen = vec![r(11, 11), r(12, 12)];
        let prev = Mark { inflight: true, ..mark(7, 10) };
        let e = evaluate(Some(prev), 7, Some(13), Some(12), Some(12), true, &unseen);
        assert_eq!(e.arrivals.len(), 2, "better a repeat than a lost notification");
        assert!(!e.next.inflight);
    }

    #[test]
    fn the_mark_never_moves_backwards() {
        let e = evaluate(Some(mark(7, 50)), 7, Some(10), None, Some(9), true, &[]);
        assert_eq!(e.next.last_uid, 50);
    }

    #[test]
    fn a_failed_sync_advances_only_to_what_is_cached() {
        // the server says uid 20 exists but the sync died after storing up to 12: 13..20 must still be announced later
        let e = evaluate(Some(mark(7, 10)), 7, Some(21), None, Some(12), false, &[r(12, 12)]);
        assert_eq!(e.next.last_uid, 12);
        let e = evaluate(None, 7, Some(21), None, Some(12), false, &[]);
        assert_eq!(e.next, mark(7, 12), "baseline too");
        let e = evaluate(None, 7, Some(21), None, None, false, &[]);
        assert_eq!(e.next, mark(7, 0));
    }

    #[test]
    fn an_early_error_does_not_leave_the_run_marked_inflight() {
        let mut m = Marks::default();
        m.accounts.insert("a@x.org".into(), mark(7, 10));
        let prev = m.accounts.get("a@x.org").copied();
        assert!(begin_run(&mut m, "a@x.org", None));
        assert!(m.accounts["a@x.org"].inflight);
        assert!(abort_run(&mut m, "a@x.org", prev));
        assert_eq!(m.accounts["a@x.org"], mark(7, 10));
        // a run that completed (mark already replaced, not inflight) is left alone
        assert!(!abort_run(&mut m, "a@x.org", prev));
        // an unfinished earlier run stays inflight
        let stuck = Mark { inflight: true, ..mark(7, 10) };
        m.accounts.insert("a@x.org".into(), stuck);
        assert!(!begin_run(&mut m, "a@x.org", Some(30)), "an unfinished run's cache gain is not folded in");
        assert!(!abort_run(&mut m, "a@x.org", Some(stuck)));
        assert_eq!(m.accounts["a@x.org"], stuck);
    }

    #[test]
    fn a_cut_run_does_not_make_the_next_one_repeat_mail_the_app_already_had() {
        // Q04: mark 10, the app had cached up to 20 (seen by the user); the run stores 21-22 and is cut before evaluating.
        let mut m = Marks::default();
        m.accounts.insert("a@x.org".into(), mark(7, 10));
        assert!(begin_run(&mut m, "a@x.org", Some(20)));
        let left = m.accounts["a@x.org"];
        assert_eq!((left.last_uid, left.inflight), (20, true), "pre_max is folded in together with inflight");
        // next run: cache now up to 22 (pre_max 22 ignored because the last run did not finish), one more arrives (23)
        let unseen: Vec<Recent> = (11..=23).map(|u| r(u, u as i64)).collect();
        let e = evaluate(Some(left), 7, Some(24), Some(22), Some(23), true, &unseen);
        assert_eq!(e.arrivals.iter().map(|x| x.uid).collect::<Vec<_>>(), vec![23, 22, 21], "11-20 are not announced again");
        assert_eq!(e.next.last_uid, 23);
        assert!(!e.next.inflight);
    }

    #[test]
    fn only_a_failure_before_the_sync_restores_the_old_mark() {
        // Q05
        assert!(Failure::before("keychain").restores_mark());
        assert!(!Failure::after("storage after sync").restores_mark(), "stored rows would be taken for seen and never announced");
    }

    #[test]
    fn the_mark_stays_in_flight_when_no_notification_got_through() {
        // Q07
        let next = mark(7, 30);
        assert_eq!(settle(next, 2, 0, true), None, "every post failed: try again next run");
        assert_eq!(settle(next, 2, 1, true), Some(next), "partly posted counts as done");
        assert_eq!(settle(next, 0, 0, true), Some(next), "nothing to show");
        assert_eq!(settle(next, 0, 0, false), Some(next), "not allowed: marks still advance (no flood later)");
    }

    #[test]
    fn saving_twice_survives_a_passing_failure_and_reports_a_lasting_one() {
        // Q06: save_retry writes like save; a path that cannot be written reports the error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let mut m = Marks::default();
        m.accounts.insert("a@x.org".into(), mark(7, 10));
        m.save_retry(&path).unwrap();
        assert_eq!(Marks::load(&path), m);
        assert!(m.save_retry(&dir.path().join("missing").join(FILE)).is_err());
    }

    #[test]
    fn the_first_background_run_records_where_the_open_app_left_off() {
        // the open app had cached up to 20; background runs store 21.. until the user is back
        let mut m = Marks::default();
        m.accounts.insert("a@x.org".into(), mark(7, 10));
        begin_run(&mut m, "a@x.org", Some(20));
        assert_eq!(m.accounts["a@x.org"].fg_floor, Some(20));
        let e = evaluate(Some(m.accounts["a@x.org"]), 7, Some(23), Some(20), Some(22), true, &[r(21, 21), r(22, 22)]);
        assert_eq!(e.next.fg_floor, Some(20));
        m.accounts.insert("a@x.org".into(), e.next);
        // a later run keeps the first floor even though its own cache is higher
        begin_run(&mut m, "a@x.org", Some(22));
        assert_eq!(m.accounts["a@x.org"].fg_floor, Some(20));
        let e = evaluate(Some(m.accounts["a@x.org"]), 7, Some(25), Some(22), Some(24), true, &[]);
        assert_eq!(e.next.fg_floor, Some(20));
        m.accounts.insert("a@x.org".into(), e.next);
        // the open app takes it once
        let (floors, changed) = m.take_floors();
        assert!(changed);
        assert_eq!(floors["a@x.org"], (7, 20));
        assert_eq!(m.accounts["a@x.org"].fg_floor, None);
        assert!(!m.take_floors().1, "taken once");
        // the first run after that records a new floor
        assert_eq!(evaluate(None, 7, Some(3), Some(2), Some(2), true, &[]).next.fg_floor, Some(2), "baseline run too");
        assert_eq!(evaluate(Some(mark(7, 10)), 8, Some(3), None, Some(2), true, &[]).next.fg_floor, None, "UIDs restarted: no floor");
    }

    #[test]
    fn floors_are_taken_from_the_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        let mut m = Marks::default();
        m.accounts.insert("a@x.org".into(), fl(mark(7, 10), 9));
        m.accounts.insert("b@x.org".into(), mark(3, 4));
        m.save(&path).unwrap();
        let f = take_floors(&path);
        assert_eq!(f.len(), 1);
        assert_eq!(f["a@x.org"], (7, 9));
        assert!(take_floors(&path).is_empty());
        assert_eq!(Marks::load(&path).accounts["a@x.org"], mark(7, 10), "the rest of the mark is untouched");
    }

    #[test]
    fn marks_roundtrip_atomically_and_survive_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        assert_eq!(Marks::load(&path), Marks::default());
        let mut m = Marks::default();
        m.accounts.insert("a@x.org".into(), mark(7, 10));
        m.save(&path).unwrap();
        assert_eq!(Marks::load(&path), m);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no temp file left behind");
        std::fs::write(&path, b"{ nope").unwrap();
        assert_eq!(Marks::load(&path), Marks::default());
        // an older file without the inflight field still loads
        std::fs::write(&path, br#"{"accounts":{"a@x.org":{"validity":1,"last_uid":2}}}"#).unwrap();
        assert_eq!(Marks::load(&path).accounts["a@x.org"], mark(1, 2));
    }

    #[test]
    fn turning_an_account_off_forgets_its_mark() {
        let mut m = Marks::default();
        m.accounts.insert("a@x.org".into(), mark(1, 5));
        m.accounts.insert("b@x.org".into(), mark(1, 6));
        assert!(m.retain_accounts(&["a@x.org".to_string()]));
        assert_eq!(m.accounts.len(), 1);
        assert!(!m.retain_accounts(&["a@x.org".to_string()]));
    }
}
