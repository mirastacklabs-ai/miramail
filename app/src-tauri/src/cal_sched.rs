//! In-process schedulers: CalDAV background sync (launch + every 10 minutes) and event reminders.
//! Both run only while the app runs. Nothing here contacts Zoom.

use crate::cal_logic;
use crate::cal_state::{CalState, RealCalDav};
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDateTime, TimeZone, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const FIRST_SYNC_DELAY: Duration = Duration::from_secs(8);
pub const SYNC_INTERVAL: Duration = Duration::from_secs(10 * 60);
pub const REMINDER_TICK: Duration = Duration::from_secs(30);
/// Longest reminder lead honoured (also the look-ahead window).
pub const MAX_LEAD_MINUTES: i64 = 7 * 24 * 60;

/// Run `tick` after `first`, then every `every`, until the task is dropped/aborted.
/// A slow tick delays the next one (no overlap, no catch-up burst).
pub async fn periodic<F, Fut>(first: Duration, every: Duration, mut tick: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::time::sleep(first).await;
    loop {
        tick().await;
        tokio::time::sleep(every).await;
    }
}

pub fn changed(app: &AppHandle) {
    let _ = app.emit("cal://changed", ());
    crate::widget_snapshot::request(app, false);
}

/// One background CalDAV sync pass. Emits `cal://changed` when a sync was attempted (state/errors may have changed).
/// iPhone: skipped while the user is away (presence.rs); the next 10-minute tick after they are back syncs.
pub async fn sync_tick(app: &AppHandle) {
    if !crate::presence::user_present() {
        return;
    }
    let st = app.state::<CalState>();
    if !cal_logic::has_caldav(&st) {
        return;
    }
    if let Ok(out) = cal_logic::sync_calendars(&st, &RealCalDav, None).await {
        if out.attempted > 0 {
            changed(app);
        }
    }
}

pub fn start(app: &AppHandle) {
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        periodic(FIRST_SYNC_DELAY, SYNC_INTERVAL, || sync_tick(&a)).await;
    });
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut sent: HashMap<String, i64> = HashMap::new();
        periodic(Duration::from_secs(5), REMINDER_TICK, || {
            reminder_tick(&a, &mut sent);
            std::future::ready(())
        })
        .await;
    });
}

// ---------------------------------------------------------------------------------------------
// Reminders
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderEvent {
    pub uid: String,
    pub calendar_id: String,
    pub summary: String,
    pub start: String,
}

/// A timed occurrence with a reminder, ready for the due check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub uid: String,
    pub calendar_id: String,
    pub summary: String,
    pub start: String,
    pub start_epoch: i64,
    pub minutes: i64,
}

/// Instant of a wire start. Floating times (`tzid` None: wall-clock digits + placeholder offset) are read in local time.
pub fn start_epoch(start: &str, tzid: Option<&str>) -> Option<i64> {
    if tzid.is_none() {
        let naive = NaiveDateTime::parse_from_str(start.get(..19)?, "%Y-%m-%dT%H:%M:%S").ok()?;
        return Local.from_local_datetime(&naive).earliest().map(|d| d.timestamp());
    }
    DateTime::parse_from_rfc3339(start).ok().map(|d| d.timestamp())
}

/// Reminders to fire now: `start - minutes <= now < start` (a 0-minute reminder fires in the minute after the start),
/// each occurrence at most once (`sent` maps key -> start epoch and is pruned).
pub fn due_reminders(now: i64, cands: &[Candidate], sent: &mut HashMap<String, i64>) -> Vec<ReminderEvent> {
    sent.retain(|_, start| *start > now - 24 * 3600);
    let mut out = Vec::new();
    for c in cands {
        if !(0..=MAX_LEAD_MINUTES).contains(&c.minutes) {
            continue;
        }
        let at = c.start_epoch - c.minutes * 60;
        if now < at || now >= c.start_epoch.max(at + 60) {
            continue;
        }
        let key = format!("{}\u{1}{}\u{1}{}", c.calendar_id, c.uid, c.start);
        if sent.insert(key, c.start_epoch).is_none() {
            out.push(ReminderEvent { uid: c.uid.clone(), calendar_id: c.calendar_id.clone(), summary: c.summary.clone(), start: c.start.clone() });
        }
    }
    out
}

fn candidates(st: &CalState, now: DateTime<Utc>) -> Vec<Candidate> {
    let from = now.to_rfc3339();
    let to = (now + ChronoDuration::minutes(MAX_LEAD_MINUTES)).to_rfc3339();
    let Ok(occs) = st.db(|c| c.list_occurrences(&from, &to, true)) else { return vec![] };
    let mut out = Vec::new();
    /// (reminder minutes, tzid) of an event, if it has a reminder.
    type Info = Option<(i64, Option<String>)>;
    let mut seen: HashMap<(String, String), Info> = HashMap::new();
    for o in occs.into_iter().filter(|o| !o.all_day) {
        let key = (o.uid.clone(), o.calendar_id.clone());
        let info = seen
            .entry(key)
            .or_insert_with(|| {
                st.db(|c| c.get_event(&o.uid, &o.calendar_id)).ok().flatten().and_then(|e| e.reminder_minutes.map(|m| (m, e.tzid)))
            })
            .clone();
        let Some((minutes, tzid)) = info else { continue };
        if let Some(start_epoch) = start_epoch(&o.start, tzid.as_deref()) {
            out.push(Candidate { uid: o.uid, calendar_id: o.calendar_id, summary: o.summary, start: o.start, start_epoch, minutes });
        }
    }
    out
}

pub fn reminder_tick(app: &AppHandle, sent: &mut HashMap<String, i64>) {
    let st = app.state::<CalState>();
    let now = Utc::now();
    for r in due_reminders(now.timestamp(), &candidates(&st, now), sent) {
        let _ = app.emit("cal://reminder", r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn cand(start_epoch: i64, minutes: i64) -> Candidate {
        Candidate { uid: "u".into(), calendar_id: "c".into(), summary: "Standup".into(), start: format!("t{start_epoch}"), start_epoch, minutes }
    }

    #[test]
    fn reminder_fires_once_inside_the_window() {
        let mut sent = HashMap::new();
        let c = [cand(10_000, 10)]; // reminder at 9_400
        assert!(due_reminders(9_399, &c, &mut sent).is_empty());
        let r = due_reminders(9_400, &c, &mut sent);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].summary, "Standup");
        assert!(due_reminders(9_430, &c, &mut sent).is_empty(), "only once");
        assert!(due_reminders(10_000, &c, &mut sent).is_empty());
    }

    #[test]
    fn late_start_is_missed_and_zero_minute_reminder_fires_at_start() {
        let mut sent = HashMap::new();
        assert!(due_reminders(10_000, &[cand(10_000, 10)], &mut sent).is_empty(), "already started");
        let z = [cand(20_000, 0)];
        assert!(due_reminders(19_999, &z, &mut sent).is_empty());
        assert_eq!(due_reminders(20_010, &z, &mut sent).len(), 1);
    }

    #[test]
    fn out_of_range_leads_are_ignored_and_sent_is_pruned() {
        let mut sent = HashMap::new();
        assert!(due_reminders(0, &[cand(100, -1), cand(100, MAX_LEAD_MINUTES + 1)], &mut sent).is_empty());
        assert_eq!(due_reminders(95, &[cand(100, 1)], &mut sent).len(), 1);
        assert_eq!(sent.len(), 1);
        due_reminders(100 + 2 * 24 * 3600, &[], &mut sent);
        assert!(sent.is_empty());
    }

    #[test]
    fn start_epoch_reads_rfc3339_and_floating() {
        assert_eq!(start_epoch("2026-11-06T15:00:00+05:30", Some("Asia/Kolkata")), Some(1_793_957_400));
        assert!(start_epoch("2026-11-06T15:00:00+00:00", None).is_some());
        assert_eq!(start_epoch("garbage", Some("UTC")), None);
        assert_eq!(start_epoch("2026-11-06", None), None);
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_runs_after_first_delay_then_every_interval() {
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let h = tokio::spawn(periodic(Duration::from_secs(8), Duration::from_secs(600), move || {
            n2.fetch_add(1, Ordering::SeqCst);
            std::future::ready(())
        }));
        tokio::time::sleep(Duration::from_secs(7)).await;
        assert_eq!(n.load(Ordering::SeqCst), 0, "nothing before the launch delay");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(n.load(Ordering::SeqCst), 1, "sync on launch");
        tokio::time::sleep(Duration::from_secs(590)).await;
        assert_eq!(n.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_secs(20)).await;
        assert_eq!(n.load(Ordering::SeqCst), 2, "every 10 minutes");
        h.abort();
    }
}
