//! MiraMail calendar core: pure logic + SQLite storage. No network access.
//!
//! * [`types`]   wire types (camelCase JSON, mirrors `app/src/lib/types.ts`)
//! * [`ics`]     RFC 5545 parse/build
//! * [`rrule`]   RRULE parsing and DST-correct expansion
//! * [`itip`]    RFC 5546 REQUEST / REPLY / CANCEL helpers
//! * [`storage`] rusqlite store

pub mod avail;
pub mod error;
pub mod ics;
pub mod itip;
pub mod rrule;
pub mod storage;
mod patch;
mod tm;
mod tz;
pub mod types;

pub use error::{Error, Result};
pub use ics::{
    build_ics, build_ics_series, parse_ics, parse_ics_opts, try_build_ics, try_build_ics_series, try_build_ics_series_opts, ParseOptions, ParsedCalendar, MAX_ATTENDEES,
    MAX_EXDATES, MAX_ICS_BYTES, MAX_RRULE_LEN,
};
pub use rrule::{expand_event, parse_rule, Rule, MAX_OCCURRENCES};
pub use storage::{new_event_uid, new_id, Calendar, CalendarPatch, EventRef, NewCalendar, SyncState, DEFAULT_CALENDAR_ID};
pub use types::*;
