use serde::{Serialize, Serializer};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tls: {0}")]
    Tls(String),
    #[error("imap: {0}")]
    Imap(String),
    #[error("smtp: {0}")]
    Smtp(String),
    /// The SMTP exchange broke (timeout, dropped connection, ...) at a point where the server may or may not have
    /// accepted the message. Callers that must not send twice treat it as "possibly sent".
    #[error("smtp (outcome unknown): {0}")]
    SmtpUnknown(String),
    #[error("storage: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("secret store: {0}")]
    Secret(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("authentication failed")]
    AuthFailed,
}

impl Error {
    /// True when a send failed in a way that does not prove the message was NOT delivered.
    pub fn outcome_unknown(&self) -> bool {
        matches!(self, Error::SmtpUnknown(_) | Error::Timeout(_))
    }
}

impl From<async_imap::error::Error> for Error {
    fn from(e: async_imap::error::Error) -> Self {
        Error::Imap(e.to_string())
    }
}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
