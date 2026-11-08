use serde::{Deserialize, Serialize};

/// Transport security. Plaintext connections are intentionally unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tls {
    /// TLS from the first byte (IMAP 993 / SMTP 465).
    Implicit,
    /// Upgrade with STARTTLS (IMAP 143 / SMTP 587).
    StartTls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub tls: Tls,
}

/// Contains no password; passwords live in a `SecretStore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConfig {
    pub email: String,
    pub imap: ServerConfig,
    pub smtp: ServerConfig,
    pub username: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Archive,
    Junk,
    Other,
}

impl FolderRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            FolderRole::Inbox => "inbox",
            FolderRole::Sent => "sent",
            FolderRole::Drafts => "drafts",
            FolderRole::Trash => "trash",
            FolderRole::Archive => "archive",
            FolderRole::Junk => "junk",
            FolderRole::Other => "other",
        }
    }
    pub fn from_db(s: &str) -> Self {
        match s {
            "inbox" => FolderRole::Inbox,
            "sent" => FolderRole::Sent,
            "drafts" => FolderRole::Drafts,
            "trash" => FolderRole::Trash,
            "archive" => FolderRole::Archive,
            "junk" => FolderRole::Junk,
            _ => FolderRole::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    pub delimiter: Option<String>,
    pub role: FolderRole,
    pub selectable: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAddress {
    pub name: Option<String>,
    pub address: String,
}

/// Row shown in the message list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSummary {
    pub account: String,
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    pub message_id: Option<String>,
    pub subject: String,
    pub from: String,
    pub to: String,
    /// Unix seconds (UTC).
    pub date: i64,
    pub flags: Flags,
    /// Filled once the body has been fetched; empty before.
    pub snippet: String,
    pub has_body: bool,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagChange {
    Seen(bool),
    Flagged(bool),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub new: u32,
    pub updated: u32,
    pub removed: u32,
    /// UIDVALIDITY changed: the local cache of this folder was discarded.
    pub uidvalidity_reset: bool,
    /// Messages in the mailbox on the server (EXISTS) when the sync ran.
    pub server_total: u32,
}

/// Result of `ImapClient::load_older`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadOlderReport {
    /// Headers fetched and cached by this call.
    pub loaded: u32,
    /// Messages on the server older than the oldest cached one (exact once the whole UID range
    /// was searched, otherwise an estimate of at least 1 while `reached_start` is false).
    pub remaining_on_server: u32,
    /// Messages in the mailbox (EXISTS).
    pub server_total: u32,
    /// Lowest cached UID after the call.
    pub oldest_uid: Option<u32>,
    /// Nothing older is left on the server.
    pub reached_start: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingAttachment {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

/// Parent message info for a reply (sets In-Reply-To / References).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyTo {
    pub message_id: String,
    pub references: Vec<String>,
}

/// An iTIP (RFC 5546) `text/calendar` alternative part. `method` must be REQUEST, REPLY or CANCEL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarPart {
    pub method: String,
    pub ics: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingMessage {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    pub attachments: Vec<OutgoingAttachment>,
    pub in_reply_to: Option<ReplyTo>,
    /// When set, the message is multipart/alternative: the human-readable `text` (and `html`) plus
    /// a `text/calendar; method=...` part.
    #[serde(default)]
    pub calendar: Option<CalendarPart>,
    /// Extra RFC 5322 header fields (name, value), e.g. `Auto-Submitted: auto-replied`. Only a short list of
    /// names is accepted and values must be printable ASCII; see `smtp::EXTRA_HEADER_NAMES`.
    #[serde(default)]
    pub extra_headers: Vec<(String, String)>,
}

/// Events emitted by `watch::watch_folder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchEvent {
    Connected { idle: bool },
    NewMail,
    Disconnected { reason: String },
    Reconnecting { attempt: u32, delay_secs: u64 },
}
