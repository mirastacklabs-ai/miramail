//! MiraMail auto-detection engine (spec: docs/AUTODETECT_SPEC.md).
//!
//! `detect(email)` discovers ranked IMAP/SMTP candidates without ever sending a
//! password; `verify(...)` then checks credentials against them.

mod autoconfig;
mod detect;
mod email;
mod error;
mod hosts;
mod providers;
mod rank;
mod srv;
mod types;
mod verify;

pub use detect::{detect, detect_with, DetectOptions};
pub use error::{AuthHint, DetectError, Stage, VerifyError};
pub use types::*;
pub use verify::verify;

/// Building blocks exposed for tests and the CLI harness.
pub mod internals {
    pub use crate::autoconfig::parse_autoconfig;
    pub use crate::providers::{glob_match, Provider, ProviderDb};
    pub use crate::rank::rank;
    pub use crate::srv::{srv_candidates, srv_candidates_for_domain, SrvRecord, SrvService};
}
