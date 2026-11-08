//! Public data types. Everything is serde-serializable so a Tauri app can
//! hand it straight to the frontend.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// TLS from the first byte (993 / 465).
    Implicit,
    /// Plain connect then STARTTLS upgrade (143 / 587). Never used without upgrade.
    #[serde(rename = "starttls")]
    StartTls,
}

/// Where a candidate came from. Order of `rank()` is the spec's score order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Bundled provider data matched the email domain.
    Bundled,
    /// MX host matched a known provider.
    Mx,
    Autoconfig,
    Srv,
    Heuristic,
}

impl Source {
    pub fn rank(self) -> u8 {
        match self {
            Source::Bundled => 5,
            Source::Mx => 4,
            Source::Autoconfig => 3,
            Source::Srv => 2,
            Source::Heuristic => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsernameHint {
    /// Full email address only.
    Email,
    /// Full email first; the local part may be tried if that is rejected.
    LocalPartFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub source: Source,
    pub username_hint: UsernameHint,
}

impl Candidate {
    pub(crate) fn key(&self) -> (String, u16, TlsMode) {
        (self.host.to_ascii_lowercase(), self.port, self.tls)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceOutcome {
    /// Source produced this many candidates.
    Hit { candidates: usize },
    Miss,
    Error,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReport {
    /// Human label, e.g. `autoconfig.example.com`.
    pub name: String,
    pub source: Source,
    pub outcome: SourceOutcome,
    pub detail: Option<String>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionResult {
    pub email: String,
    pub domain: String,
    /// Id of the matched bundled provider (by domain or MX), if any.
    pub provider: Option<String>,
    pub imap: Vec<Candidate>,
    pub smtp: Vec<Candidate>,
    pub sources: Vec<SourceReport>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub username: String,
}

/// Verified settings. Deliberately contains no password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedSettings {
    pub email: String,
    pub provider: Option<String>,
    pub imap: ServerSettings,
    pub smtp: ServerSettings,
}
