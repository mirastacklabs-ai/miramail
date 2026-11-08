//! Wire types shared with the TypeScript UI (camelCase JSON). See docs/CALENDAR_SPEC.md.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CalendarKind {
    Local,
    Caldav,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarInfo {
    pub id: String,
    pub name: String,
    /// `#rrggbb`
    pub color: String,
    pub kind: CalendarKind,
    pub url: Option<String>,
    pub username: Option<String>,
    pub visible: bool,
    pub read_only: bool,
    pub last_sync: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttendeeRole {
    Chair,
    Required,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttendeeStatus {
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
}

impl AttendeeStatus {
    pub fn as_partstat(self) -> &'static str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::Accepted => "ACCEPTED",
            Self::Declined => "DECLINED",
            Self::Tentative => "TENTATIVE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attendee {
    pub email: String,
    pub name: Option<String>,
    pub role: AttendeeRole,
    pub status: AttendeeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoomMeeting {
    pub meeting_id: String,
    pub join_url: String,
    pub password: Option<String>,
    /// Host start URL (contains the host token). Memory/Keychain only: NEVER written to ICS, SQLite,
    /// iTIP or CalDAV, and never read back from them.
    pub start_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventStatus {
    Confirmed,
    Tentative,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CalEvent {
    pub uid: String,
    pub calendar_id: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    /// All-day: `YYYY-MM-DD`; timed: RFC 3339 with offset.
    pub start: String,
    /// Same format as `start`; all-day end is EXCLUSIVE.
    pub end: String,
    pub all_day: bool,
    /// IANA tz of `start`. On a timed event `None` means a FLOATING time (RFC 5545 form 1): `start`/`end`
    /// carry the wall-clock digits with a `+00:00` placeholder offset and the UI must interpret them in
    /// the viewer's local zone. `Some("UTC")` is a real UTC instant.
    pub tzid: Option<String>,
    /// e.g. `FREQ=WEEKLY;BYDAY=MO,WE` (no `RRULE:` prefix).
    pub rrule: Option<String>,
    /// Excluded occurrence starts (same format as `start`; a bare date also matches timed events).
    pub exdates: Vec<String>,
    pub attendees: Vec<Attendee>,
    pub organizer: Option<Attendee>,
    pub status: EventStatus,
    pub reminder_minutes: Option<i64>,
    pub zoom: Option<ZoomMeeting>,
    pub sequence: i64,
    pub href: Option<String>,
    pub etag: Option<String>,
    /// DTSTAMP as RFC 3339 UTC (iTIP tie-break). `None` -> "now" when an ICS is built.
    pub dtstamp: Option<String>,
}

impl Default for CalEvent {
    fn default() -> Self {
        Self {
            uid: String::new(),
            calendar_id: String::new(),
            summary: String::new(),
            description: String::new(),
            location: String::new(),
            start: String::new(),
            end: String::new(),
            all_day: false,
            tzid: None,
            rrule: None,
            exdates: Vec::new(),
            attendees: Vec::new(),
            organizer: None,
            status: EventStatus::Confirmed,
            reminder_minutes: None,
            zoom: None,
            sequence: 0,
            href: None,
            etag: None,
            dtstamp: None,
        }
    }
}

/// A RECURRENCE-ID override of one occurrence of a recurring event.
/// Keyed by (`event.uid`, `event.calendar_id`, `recurrence_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventOverride {
    /// Original occurrence start (same wire format as `CalEvent::start`).
    pub recurrence_id: String,
    pub event: CalEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    pub uid: String,
    pub calendar_id: String,
    pub recurrence_start: String,
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub summary: String,
    pub location: String,
    pub color: String,
    pub has_zoom: bool,
    pub is_recurring: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InviteMethod {
    Request,
    Reply,
    Cancel,
    Publish,
}

impl InviteMethod {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "REQUEST" => Some(Self::Request),
            "REPLY" => Some(Self::Reply),
            "CANCEL" => Some(Self::Cancel),
            "PUBLISH" => Some(Self::Publish),
            _ => None,
        }
    }
    /// A REPLY must never be imported as an event; it only updates an attendee (`itip::apply_reply`).
    pub fn is_importable(self) -> bool {
        !matches!(self, Self::Reply)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "REQUEST",
            Self::Reply => "REPLY",
            Self::Cancel => "CANCEL",
            Self::Publish => "PUBLISH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub method: InviteMethod,
    pub event: CalEvent,
    pub my_status: Option<AttendeeStatus>,
    pub already_in_calendar: bool,
    pub attachment_id: String,
    /// Set when the iCalendar part addresses ONE instance of a series (RECURRENCE-ID, no master):
    /// `event` is that instance. Route it with `itip::apply_request_series` / `apply_cancel_series`,
    /// never as a replacement of the master.
    #[serde(default)]
    pub recurrence_id: Option<String>,
    /// Further RECURRENCE-ID components of the same part (master + overrides, or several instances).
    #[serde(default)]
    pub overrides: Vec<EventOverride>,
    /// The carrying mail's `Authentication-Results` do not show DMARC/DKIM `pass` for the From domain.
    /// Set by the app (the calendar crate never sees mail headers); such an invite may be shown and
    /// answered, but a CANCEL or an update to a stored event needs an explicit user confirmation.
    #[serde(default)]
    pub sender_unverified: bool,
}
