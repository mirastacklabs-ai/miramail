//! MiraMail calendar network layer.
//!
//! * [`caldav`] CalDAV client: RFC 6764/4791 discovery, ctag/etag sync, PUT/DELETE with preconditions.
//! * [`zoom`]   Zoom OAuth 2.0 (auth code + PKCE, loopback redirect) and the Meetings API.
//!
//! Network is only ever used for the user's configured CalDAV server and, after the user connects it,
//! `zoom.us` / `api.zoom.us`. Secrets, tokens, ICS bodies and descriptions are never logged or put in errors.

pub mod caldav;
pub mod zoom;

#[cfg(test)]
mod testutil;

pub use caldav::{CalDavClient, Diag, CalDavError, Config, DiscoveredCalendar, SyncSummary};
pub use zoom::{AuthSession, InMemoryTokenStore, TokenStore, ZoomClient, ZoomConfig, ZoomError, ZoomStatus};
