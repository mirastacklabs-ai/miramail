//! Calendar access for the assistant (AI chat, calendar tools).
//!
//! `aichat` does not know about the calendar crates or the calendar database. The app implements:
//!
//! * [`CalendarHost`]: READ-only. Lists calendars and events (recurrences expanded), computes free/busy
//!   and free slots (deterministic, DST-correct), resolves times in the user's zone, and answers the few
//!   questions the proposal planner needs (conflicts, "have I seen this address", Zoom connected).
//! * [`CalActionHost`]: WRITE. Used ONLY by [`crate::calexec::CalExecutor`] after the user clicked Approve
//!   (or Undo) on a card. The model never reaches it.
//!
//! Types here carry plain strings: times are already formatted in the user's zone by the host. Every
//! string that originates in an event (title, description, location, attendee names, calendar names) is
//! UNTRUSTED; `tools::cal` sanitises and wraps it before the model sees it.

use serde::{Deserialize, Serialize};

use crate::tools::{BoxFut, Scope};

/// Longest range (days) one read call may cover.
pub const MAX_RANGE_DAYS: i64 = 62;
/// Most events one `list_events` result carries.
pub const MAX_EVENTS_PER_RESULT: usize = 200;
/// Description characters shown by `get_event`.
pub const MAX_DESCRIPTION_CHARS: usize = 1500;
/// Attendees listed per event.
pub const MAX_ATTENDEES_SHOWN: usize = 12;
/// Most attendees a proposed event may have.
pub const MAX_PROPOSED_ATTENDEES: usize = 20;
/// Most slots `find_free_slots` returns.
pub const MAX_SLOTS: usize = 25;
/// Longest buffer between meetings the model may ask for (clamped); hosts read busy rows this far past a range.
pub const MAX_BUFFER_MINUTES: u32 = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalMeta {
    /// Opaque host id (never shown to the model).
    pub id: String,
    pub name: String,
    pub read_only: bool,
    /// `local` | `caldav`
    pub kind: String,
    /// The calendar new events go to when the model names none.
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttendeeRow {
    pub name: String,
    pub email: String,
    /// `accepted` | `declined` | `tentative` | `needs-action`
    pub status: String,
}

/// One event (or one occurrence of a recurring event) as the host reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventRow {
    pub calendar_id: String,
    pub uid: String,
    /// Occurrence key for recurring events (the original start), else empty.
    pub rid: String,
    pub title: String,
    pub location: String,
    pub description: String,
    /// Display text in the user's zone, e.g. `Thu 12 Nov 13:00-14:00`.
    pub when: String,
    /// ISO in the user's zone; all-day events carry dates (end exclusive).
    pub start: String,
    pub end: String,
    pub start_ts: i64,
    pub end_ts: i64,
    pub all_day: bool,
    pub recurring: bool,
    pub organizer: String,
    pub attendees: Vec<AttendeeRow>,
    pub attendee_count: usize,
    /// `organizer` | `accepted` | `tentative` | `declined` | `needs-action` | `none` (not an attendee)
    pub my_status: String,
    pub busy: bool,
    pub tentative: bool,
    pub has_zoom: bool,
    pub tzid: Option<String>,
    /// Changes whenever the event's content changes (etag + hash of the fields).
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventList {
    pub events: Vec<EventRow>,
    /// Matching events before any cap.
    pub total: usize,
    pub tz_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusyBlock {
    pub start: String,
    pub end: String,
    pub tentative: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BusyOut {
    pub blocks: Vec<BusyBlock>,
    /// All-day events (they do not block time): `(dates, title)`; titles are untrusted.
    pub all_day: Vec<(String, String)>,
    pub tz_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotReq {
    pub from: String,
    pub to: String,
    pub duration_minutes: u32,
    pub working_hours_only: bool,
    pub buffer_minutes: u32,
    pub max_results: u32,
    pub ignore_tentative: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotOut {
    pub start: String,
    pub end: String,
    /// Display text, e.g. `Thu 12 Nov 13:00-14:00`.
    pub when: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SlotsOut {
    pub slots: Vec<SlotOut>,
    pub tz_name: String,
    pub working_hours: String,
    pub note: String,
}

/// A resolved time range. `start` / `end`: RFC 3339 with offset for timed events, `YYYY-MM-DD` for all-day
/// (end exclusive, as stored).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct When {
    pub start: String,
    pub end: String,
    pub tzid: Option<String>,
    pub all_day: bool,
    pub start_ts: i64,
    pub end_ts: i64,
    pub display: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Person {
    pub email: String,
    pub name: String,
}

/// A new event as the app will create it (validated; the model's words are already cleaned).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EventSpec {
    pub title: String,
    pub when: When,
    pub location: String,
    pub description: String,
    pub attendees: Vec<Person>,
    pub reminder_minutes: Option<i64>,
    pub add_zoom: bool,
}

/// Fields of an existing event to change (`None` = unchanged). `when` is fully resolved.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EventChanges {
    pub title: Option<String>,
    pub when: Option<When>,
    pub location: Option<String>,
    pub description: Option<String>,
}

impl EventChanges {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.when.is_none() && self.location.is_none() && self.description.is_none()
    }
}

/// One line of the card's before/after table, written by the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardRow {
    pub label: String,
    pub before: Option<String>,
    pub after: String,
}

/// Where an invitation lives in the mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MsgCoord {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    pub attachment_id: String,
}

/// What a calendar proposal does on approval. Stored in the proposal's `params_json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CalParams {
    pub calendar_id: String,
    pub calendar_name: String,
    /// Existing event (change / cancel / respond to a stored event).
    pub uid: String,
    pub expected_version: String,
    pub new: Option<EventSpec>,
    pub changes: Option<EventChanges>,
    /// `accepted` | `tentative` | `declined`
    pub response: Option<String>,
    pub msg: Option<MsgCoord>,
    /// Approving also mails invitations / updates (`create`, `change`) or cancellations (`cancel`).
    pub notify: bool,
    pub card: Vec<CardRow>,
    /// What an Undo would also do ("will also send a cancellation"), shown on the card.
    pub undo_note: String,
}

/// What `invite_in_message` found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InviteInfo {
    pub uid: String,
    pub title: String,
    pub when: When,
    pub organizer: String,
    /// `REQUEST` | `CANCEL` | `REPLY` | `PUBLISH`
    pub method: String,
    pub my_status: String,
    pub attachment_id: String,
    pub already_in_calendar: bool,
    pub recurring_instance: bool,
    pub sender_unverified: bool,
}

/// Read access to the user's calendars. All methods are read-only by construction. The engine passes
/// only calendar ids it has already scope-checked; a host should still treat `calendars` as a hard limit.
pub trait CalendarHost: Send + Sync {
    /// Unix seconds now (injectable for tests).
    fn now(&self) -> i64;
    fn calendars(&self) -> Result<Vec<CalMeta>, String>;
    /// Events overlapping `[from, to)` in the listed calendars, recurrences expanded, sorted by start.
    /// `from` / `to`: `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM`, or RFC 3339, read in the user's zone. Refuses ranges
    /// longer than [`MAX_RANGE_DAYS`]. The host may cap the result; `total` says how many matched.
    fn list_events(&self, calendars: &[String], from: &str, to: &str) -> Result<EventList, String>;
    fn get_event(&self, calendar_id: &str, uid: &str, rid: &str) -> Result<Option<EventRow>, String>;
    fn free_busy(&self, calendars: &[String], from: &str, to: &str, ignore_tentative: bool) -> Result<BusyOut, String>;
    fn find_free_slots(&self, calendars: &[String], req: &SlotReq) -> Result<SlotsOut, String>;
    /// Resolve a start (and optional end) given by the model. `tz` = IANA name or `None` (user's zone).
    /// A date-only start is an all-day event whose `end` date is the LAST day (inclusive).
    fn resolve_when(&self, start: &str, end: Option<&str>, tz: Option<&str>) -> Result<When, String>;
    /// As [`resolve_when`](Self::resolve_when) for a change to `current`: only `start` keeps the duration,
    /// only `end` keeps the start.
    fn resolve_change(&self, current: &EventRow, start: Option<&str>, end: Option<&str>, tz: Option<&str>) -> Result<When, String>;
    /// Short descriptions of events in `calendars` that overlap `when` and block time (untrusted text),
    /// not counting the event `exclude_uid`.
    fn conflicts(&self, calendars: &[String], when: &When, exclude_uid: Option<&str>) -> Result<Vec<String>, String>;
    /// Does this address appear anywhere in the user's mail?
    fn address_known(&self, address: &str) -> Result<bool, String>;
    /// The user's own addresses (mail accounts).
    fn my_addresses(&self) -> Vec<String>;
    /// A Zoom account is already connected (local check, no network).
    fn zoom_connected(&self) -> bool;
    /// The invitation carried by one message, if any. Reads the message the way the mail tools do.
    fn invite_in_message(&self, scope: &Scope, uid: u32, uidvalidity: u32) -> BoxFut<'_, Result<Option<InviteInfo>, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub uid: String,
    pub calendar_id: String,
    pub version: String,
    /// Invitations were mailed.
    pub invited: bool,
    pub zoom_meeting_id: Option<String>,
    /// Something went wrong after the event was saved (e.g. the server push is queued); shown to the user.
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    /// Opaque JSON of the event BEFORE the change (what Undo restores).
    pub snapshot: String,
    pub version: String,
    pub notified: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cancelled {
    /// Opaque JSON of the event before it was removed.
    pub snapshot: String,
    pub notified: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Responded {
    /// The answer was mailed to the organizer.
    pub sent: bool,
    pub warning: Option<String>,
}

/// How an invite response finds its invitation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RespondTarget {
    Message(MsgCoord),
    Stored { calendar_id: String, uid: String },
}

/// Write access to calendars, used ONLY by the calendar executor after an explicit approval / undo. The
/// implementation reuses the app's own save / delete / respond paths (CalDAV push, iTIP mail, Zoom) and
/// keeps the local database consistent. It has no operation that sends anything the user did not approve.
pub trait CalActionHost: Send + Sync {
    fn calendar(&self, id: &str) -> Result<Option<CalMeta>, String>;
    /// The current stored event (`rid` empty = the event itself).
    fn lookup(&self, calendar_id: &str, uid: &str, rid: &str) -> Result<Option<EventRow>, String>;
    /// UIDVALIDITY of a mail folder as known locally.
    fn mail_validity(&self, account: &str, folder: &str) -> Result<Option<u32>, String>;
    /// A mail account exists, so invitations / updates / cancellations CAN be mailed.
    fn can_send(&self) -> bool;
    /// `notify` is the card's `notify`: invitations are mailed only when it is true (and the app's own rules
    /// allow it); it is never recomputed upward.
    fn create_event(&self, calendar_id: &str, spec: &EventSpec, notify: bool) -> BoxFut<'_, Result<Created, String>>;
    /// `expected_version` is rechecked under the calendar's lock, right before the write; a mismatch writes nothing.
    fn change_event(&self, calendar_id: &str, uid: &str, changes: &EventChanges, notify: bool, expected_version: &str) -> BoxFut<'_, Result<Changed, String>>;
    /// `expected_version`: as for [`change_event`](Self::change_event).
    fn cancel_event(&self, calendar_id: &str, uid: &str, notify: bool, expected_version: &str) -> BoxFut<'_, Result<Cancelled, String>>;
    /// Put an event back from a snapshot. `notify`: mail the attendees again.
    fn restore_event(&self, snapshot: &str, notify: bool) -> BoxFut<'_, Result<String, String>>;
    /// Delete an event this app created (Undo of a create). `notify`: mail a cancellation to attendees.
    fn remove_created(&self, calendar_id: &str, uid: &str, zoom_meeting_id: Option<&str>, notify: bool) -> BoxFut<'_, Result<(), String>>;
    fn respond_invite(&self, target: &RespondTarget, status: &str) -> BoxFut<'_, Result<Responded, String>>;
}
