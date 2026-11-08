//! MiraMail mail core: IMAP / SMTP / MIME / sanitising / SQLite cache / secrets.
//!
//! All types that cross to the frontend derive `Serialize`/`Deserialize`.
//! Nothing here logs passwords, tokens or message bodies.

pub mod addrbook;
pub mod ai_text;
pub mod batch;
pub mod cachectl;
pub mod compose_html;
pub mod draftsync;
pub mod error;
pub mod foldermgmt;
pub mod imap;
pub mod mime;
pub mod sanitize;
pub mod search;
pub mod secrets;
pub mod sieve;
pub mod smtp;
pub mod storage;
pub mod threading;
pub mod tracking;
pub mod types;
pub mod unsub;
pub mod watch;

pub use ai_text::ai_text;
pub use error::{Error, Result};
pub use imap::ImapClient;
pub use mime::{attachment_bytes, parse_message, render_html, render_html_with, ParsedMessage};
pub use tracking::{analyze_remote_images, analyze_remote_images_from, tracker_url_set, ImageInfo};
pub use compose_html::{sanitize_compose_html, MAX_COMPOSE_HTML};
pub use sanitize::{sanitize_html, sanitize_html_with, SanitizedHtml};
pub use secrets::{InMemorySecretStore, KeychainSecretStore, SecretStore};
pub use storage::Storage;
pub use cachectl::{CachePolicy, CacheUsage};
pub use batch::{validate_new_folder_name, BatchItem, FlagReport, ItemOutcome, MoveReport, MoveVerdict, VerifyReport, BATCH_CHUNK};
pub use imap::{build_search_criteria, ServerQuery};
pub use search::{ScopeKey, SearchHit, SearchParams, SearchResult, FolderStats};
pub use types::*;
