//! Error types and their mapping to the user messages of spec section 5.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetectError {
    #[error("invalid email address: {0}")]
    InvalidEmail(String),
    /// Google and Microsoft (parked until a later version).
    #[error("{provider} accounts are not supported yet")]
    UnsupportedProvider { provider: String },
    #[error("no mail server settings found")]
    NoCandidates,
}

impl DetectError {
    pub fn user_message(&self) -> String {
        match self {
            DetectError::InvalidEmail(_) => "That doesn't look like an email address.".into(),
            DetectError::UnsupportedProvider { .. } => {
                "Gmail, Outlook and Microsoft 365 accounts aren't supported yet. They are planned for a later version.".into()
            }
            DetectError::NoCandidates => {
                "Couldn't find your mail server. Enter the settings in Advanced.".into()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Imap,
    Smtp,
}

/// Extra, actionable hints attached to an authentication failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthHint {
    /// Zoho: IMAP access is a per-account switch and is off by default.
    ZohoImapDisabled { url: String },
    /// Provider requires an app password instead of the account password.
    AppPassword { provider: String, url: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerifyError {
    /// Hard stop: no "trust anyway" in v0.1.
    #[error("certificate for {host} is not valid: {reason}")]
    CertificateInvalid { host: String, reason: String },
    #[error("could not reach a mail server")]
    Unreachable { details: Vec<String> },
    #[error("authentication failed ({stage:?})")]
    AuthFailed {
        stage: Stage,
        /// Server's own text (never contains the password).
        server_message: String,
        hints: Vec<AuthHint>,
    },
    #[error("no candidates to verify")]
    NoCandidates,
}

impl VerifyError {
    pub fn user_message(&self) -> String {
        match self {
            VerifyError::CertificateInvalid { host, reason } => format!(
                "The security certificate of {host} is not valid ({reason}). \
                 For your safety MiraMail won't connect."
            ),
            VerifyError::Unreachable { .. } | VerifyError::NoCandidates => {
                "Couldn't reach your mail server. Check network or enter settings in Advanced."
                    .into()
            }
            VerifyError::AuthFailed { hints, .. } => {
                let mut msg = String::from("The server rejected your email or password.");
                for h in hints {
                    match h {
                        AuthHint::ZohoImapDisabled { url } => msg.push_str(&format!(
                            " IMAP is off in Zoho. Enable it in Zoho Mail \u{2192} Settings \u{2192} IMAP Access ({url})."
                        )),
                        AuthHint::AppPassword { provider, url } => msg.push_str(&format!(
                            " Use an App Password from {provider} security settings ({url})."
                        )),
                    }
                }
                msg
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_error_messages() {
        let e = DetectError::UnsupportedProvider { provider: "Microsoft".into() };
        assert!(e.user_message().starts_with("Gmail, Outlook"));
        assert!(DetectError::NoCandidates.user_message().contains("Advanced"));
    }

    #[test]
    fn serde_tagged_shape() {
        let e = VerifyError::CertificateInvalid { host: "h".into(), reason: "r".into() };
        let j = serde_json_like(&e);
        assert!(j.contains("certificate_invalid"));
    }

    // serde is available but serde_json is not a dependency; round-trip via toml.
    fn serde_json_like<T: Serialize>(v: &T) -> String {
        toml::Value::try_from(v).map(|v| v.to_string()).unwrap_or_default()
    }
}
