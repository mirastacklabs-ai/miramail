//! Wire types of the AI commands (camelCase). Contract: docs/AI_API.md.

use aichat::budget::{DEFAULT_PER_CONVERSATION, DEFAULT_PER_DAY, DEFAULT_PER_TURN};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Warning {
    /// `cleartextRemote` | `lanHost`
    pub kind: String,
    pub message: String,
}

/// Both the argument of `ai_save_profile` / `ai_test_profile` and what the commands return.
/// On input the computed fields (`hasKey`, `keyOrigin`, `needsKeyReentry`, `warnings`) are ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    /// `[A-Za-z0-9_-]{1,64}`; empty on save = the backend generates one.
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// `openai_chat` | `anthropic`
    pub kind: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub max_output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_version: Option<String>,
    #[serde(default)]
    pub has_key: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_origin: Option<String>,
    #[serde(default)]
    pub needs_key_reentry: bool,
    #[serde(default)]
    pub warnings: Vec<Warning>,
    /// Informational, supplied by the frontend after a successful test (unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_ok: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub models: Vec<String>,
    pub supports_tools: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Redaction {
    pub strip_quotes: bool,
    pub shorten_links: bool,
    pub mask_addresses: bool,
}

impl Default for Redaction {
    fn default() -> Self {
        Redaction { strip_quotes: true, shorten_links: true, mask_addresses: false }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PurgeCfg {
    pub egress_retention_days: Option<u32>,
    pub conversation_retention_days: Option<u32>,
    pub purge_on_account_removal: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub per_turn_tokens: u64,
    pub per_conversation_tokens: u64,
    pub per_day_tokens: u64,
    pub default_redaction: Redaction,
    pub purge: PurgeCfg,
}

impl Default for Settings {
    fn default() -> Self {
        let p = aichat::store::PurgeSettings::default();
        Settings {
            per_turn_tokens: DEFAULT_PER_TURN,
            per_conversation_tokens: DEFAULT_PER_CONVERSATION,
            per_day_tokens: DEFAULT_PER_DAY,
            default_redaction: Redaction::default(),
            purge: PurgeCfg {
                egress_retention_days: p.egress_retention_days,
                conversation_retention_days: p.conversation_retention_days,
                purge_on_account_removal: p.purge_on_account_removal,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub account: String,
    pub folder: String,
    pub allowed: bool,
}

/// One calendar the user ticked (or not) in the consent dialog. `id` is the calendar's id in `calendar.db`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalScope {
    pub id: String,
    pub allowed: bool,
}

/// "Let MIRA see my calendar" and which calendars. Absent in consents saved before the calendar tools:
/// those stay without calendar access.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CalendarConsent {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub calendars: Vec<CalScope>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Consent {
    #[serde(default)]
    pub scopes: Vec<Scope>,
    #[serde(default)]
    pub redaction: Redaction,
    /// Read-only mail tools (search / read) for this conversation. Absent in consents saved before
    /// P1, which therefore keep plain-chat behaviour; the consent dialog sends `true` by default.
    #[serde(default)]
    pub tools: bool,
    /// "Assistant may suggest changes (you approve each one)": offers the `propose_*` tools. Only
    /// meaningful with `tools`; absent in older consents, which therefore stay read-only.
    #[serde(default)]
    pub suggest: bool,
    /// Calendar tools for this conversation (only meaningful with `tools`). Event titles, times, locations and
    /// attendees of the ticked calendars may be sent to the AI endpoint inside this conversation.
    #[serde(default)]
    pub calendar: CalendarConsent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: i64,
    pub title: String,
    pub profile_id: String,
    pub endpoint_host: String,
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Mail tools were enabled in this conversation's consent.
    #[serde(default)]
    pub tools_enabled: bool,
    /// The assistant may queue suggestions (proposals) in this conversation.
    #[serde(default)]
    pub suggest_enabled: bool,
    /// The calendar tools were enabled (with at least one calendar ticked) in this conversation's consent.
    #[serde(default)]
    pub calendar_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    /// `user` | `assistant` | `notice` (a line the app wrote, e.g. "Permissions changed by you: ..."; never sent to the AI endpoint)
    pub role: String,
    pub text: String,
    pub created_at: i64,
    /// Tool calls made while producing this reply (assistant messages only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolChip>,
}

/// One tool call, as shown in the thread ("Searched mail: from:foo - 12 results").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolChip {
    pub id: String,
    pub name: String,
    pub label: String,
    pub summary: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationWithMessages {
    pub conversation: Conversation,
    pub messages: Vec<Message>,
    /// The consent now in force (folders MIRA created earlier are included, ticked): what "Edit permissions" prefills.
    pub consent: Consent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EgressItem {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub from: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EgressRow {
    pub id: i64,
    /// Request index within the user turn: 0 = first request, 1.. = after tool results.
    pub round: i64,
    pub at: i64,
    pub host: String,
    pub model: String,
    pub bytes: i64,
    pub est_tokens: i64,
    pub request_json: String,
    pub items: Vec<EgressItem>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PurgeReport {
    pub egress_deleted: usize,
    pub conversations_deleted: usize,
}

/// Streamed to the webview through a Tauri `Channel`. `proposal`: a suggestion was queued (`id` is the
/// proposal id as a string, `summary` its app-written title).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum AiEvent {
    Text { delta: String },
    Usage { input: u64, output: u64 },
    Done { stop: String },
    Error { message: String, kind: String },
    ToolStart { id: String, name: String, label: String },
    ToolEnd { id: String, ok: bool, summary: String },
    Proposal { id: String, summary: String },
    /// A draft was written (P3): `id` is the draft id as a string, `summary` its app-written title.
    Draft { id: String, summary: String },
    /// A rule was proposed and saved OFF (P4): `id` is the rule id as a string, `summary` its app-written title.
    Rule { id: String, summary: String },
}

// ------------------------------------------------------------------------------------------
// Proposals (P2)
// ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProposalCounts {
    pub total: u32,
    pub selected: u32,
    pub done: u32,
    pub failed: u32,
    pub skipped: u32,
    pub noop: u32,
    pub undone: u32,
    pub undo_failed: u32,
    /// Moves whose result could not be confirmed after a connection failure.
    pub unknown: u32,
}

/// One suggestion as listed (activity log) and as shown on the card. Title, counts and warnings are
/// written by the app; `reason` is the model's own words and must be labelled as such.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProposalSummary {
    pub id: i64,
    pub conv_id: i64,
    /// `move` | `archive` | `trash` | `flag` | `create_folder` | `event_create` | `event_change` | `event_cancel` | `invite_response`
    pub kind: String,
    /// `proposed` | `approved` | `executing` | `done` | `failed` | `partially_done` | `rejected` | `expired` | `undone`
    pub state: String,
    pub title: String,
    /// "AI's reason", untrusted text.
    pub reason: String,
    pub warnings: Vec<String>,
    pub error: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub counts: ProposalCounts,
    /// Undo would reverse at least one executed step.
    pub undoable: bool,
    /// Calendar kinds only: the before/after table written by the app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<EventCard>,
}

/// One row of a calendar card (`before` is shown struck through next to `after`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EventCardRow {
    pub label: String,
    pub before: Option<String>,
    pub after: String,
}

/// What a calendar proposal will do, in the app's words. `notify`: approving also sends email (invitations,
/// an update, a cancellation or an answer); the card must say so before the Approve button.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EventCard {
    pub calendar_name: String,
    pub rows: Vec<EventCardRow>,
    pub notify: bool,
    pub undo_note: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProposalItem {
    pub idx: i64,
    pub account: String,
    pub folder: String,
    pub sender: String,
    pub subject: String,
    pub seen: bool,
    pub flagged: bool,
    /// Where it goes (empty for flag changes).
    pub to_folder: String,
    pub selected: bool,
    /// `pending` | `done` | `failed` | `skipped` | `noop` | `undone` | `undo_failed`
    pub status: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProposalDetail {
    #[serde(flatten)]
    pub summary: ProposalSummary,
    pub items: Vec<ProposalItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApproveResult {
    pub proposal: ProposalDetail,
    pub ok: u32,
    pub failed: u32,
    pub skipped: u32,
    pub noop: u32,
    /// A new folder was created: reload the folder list.
    pub folders_changed: bool,
    /// Short result text for calendar proposals ("Event created and invitations sent."); empty for mail.
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UndoResult {
    pub proposal: ProposalDetail,
    pub undone: u32,
    pub failed: u32,
    pub errors: Vec<String>,
}

/// Emitted on `ai://proposal` whenever a proposal changes state or progresses.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProposalEvent {
    pub id: i64,
    pub conv_id: i64,
    pub state: String,
    pub counts: ProposalCounts,
}

// ------------------------------------------------------------------------------------------
// Drafts (P3)
// ------------------------------------------------------------------------------------------

/// A draft the assistant wrote. `to`, `cc`, `subject` and `warnings` were computed by MiraMail; `body` is the
/// model's cleaned plain text; `reason` is the model's own words (untrusted: show only as "AI's reason").
/// There is no "sent" state: sending happens in Compose.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DraftSummary {
    pub id: i64,
    pub conv_id: i64,
    /// `reply` | `new`
    pub kind: String,
    /// `proposed` | `opened` | `dismissed`
    pub state: String,
    pub title: String,
    /// Mail account the draft will be sent from (Compose is opened on it).
    pub account: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// The quoted original that will be appended under the body (replies only).
    pub quote: String,
    pub reason: String,
    pub warnings: Vec<String>,
    /// Recipients of a `new` draft that appear nowhere in the user's mail.
    pub new_addresses: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DraftReplyTo {
    pub message_id: String,
    pub references: Option<String>,
}

/// What the webview opens Compose with (`ai_open_draft`). Same shape as the Compose initial values; `ai_draft`
/// tells Compose to show the "Drafted by AI" banner.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DraftPrefill {
    pub id: i64,
    pub account: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    /// The model's body followed by the app-built quote.
    pub text: String,
    pub in_reply_to: Option<DraftReplyTo>,
    pub ai_draft: bool,
    /// The app's notes about the draft (new addresses, redirects, links), shown under the AI banner in Compose.
    pub warnings: Vec<String>,
}

/// Emitted on `ai://draft` when a draft changes state.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DraftEvent {
    pub id: i64,
    pub conv_id: i64,
    pub state: String,
}
