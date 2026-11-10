// Types for the AI chat feature. Mirrors the Rust command contract (camelCase). Pure: no runtime imports.

export type ProfileKind = "openai_chat" | "anthropic";
export interface ProfileWarning { kind: "cleartextRemote" | "lanHost"; message: string }
export interface Profile {
  id: string; name: string; kind: ProfileKind; baseUrl: string; model: string; maxOutputTokens: number;
  anthropicVersion?: string | null;
  hasKey: boolean; keyOrigin?: string | null; needsKeyReentry: boolean;
  warnings: ProfileWarning[];
  testedAt?: number | null; testOk?: boolean | null;
}
/** Result of "Check tone". Plain text only; validated and capped by the backend. */
export type ToneName = "warm" | "neutral" | "firm" | "tense" | "angry";
export interface ToneSuggestion { label: "Warmer" | "Shorter" | "Firmer" | "Clearer"; text: string }
/** Placeholder the backend put in what it sent ([link-1], [person-1]) and the user's own text it stands for. Returned to the UI only. */
export interface ToneRestore { token: string; original: string }
export interface ToneReport { tone: ToneName; clarity: number | null; notes: string[]; suggestions: ToneSuggestion[]; restore?: ToneRestore[] }
export interface TestResult { ok: boolean; latencyMs: number; models: string[]; supportsTools: boolean; message: string }

export interface Redaction { stripQuotes: boolean; shortenLinks: boolean; maskAddresses: boolean }
export interface PurgeSettings { egressRetentionDays: number | null; conversationRetentionDays: number | null; purgeOnAccountRemoval: boolean }
export interface AiSettings {
  perTurnTokens: number; perConversationTokens: number; perDayTokens: number;
  defaultRedaction: Redaction; purge: PurgeSettings;
}
/** The backend does not fix the report shape: render whatever scalar fields it returns. */
export type PurgeReport = Record<string, unknown>;

export interface ConsentScope { account: string; folder: string; allowed: boolean }
/**
 * `tools`: let the assistant search and read mail (read-only) in the allowed folders. Off = plain chat.
 * `suggest`: the assistant may queue suggested changes; each one only happens after the user approves it. Needs `tools`.
 */
export interface Consent { scopes: ConsentScope[]; redaction: Redaction; tools: boolean; suggest: boolean; calendar?: CalendarConsent }
/** Calendars the assistant may read (needs `tools`). Old consents lack this: treat as disabled. */
export interface CalendarConsent { enabled: boolean; calendars: { id: string; allowed: boolean }[] }

export interface Conversation { id: string; title: string; profileId: string; endpointHost: string; model: string; createdAt: number; updatedAt: number; toolsEnabled?: boolean; suggestEnabled?: boolean; calendarEnabled?: boolean }
/** One tool call shown as a chip: `label` comes from the call's arguments, `summary` from the app ("12 results"). */
export interface ToolChip { id: string; name: string; label: string; summary: string; ok: boolean; running?: boolean }
/** `notice`: a line the app wrote (e.g. "Permissions changed by you: +1 folder"); never sent to the model. */
export interface AiMessage { id: string; role: "user" | "assistant" | "notice"; text: string; createdAt: number; tools?: ToolChip[] }
/** `consent` = what the conversation may see now (folders MIRA created earlier included, ticked). */
export interface ConversationFull { conversation: Conversation; messages: AiMessage[]; consent?: Consent }

export interface EgressItem { ref: string; subject: string; from: string }
/** `round`: request index within the user turn (0 = first, 1.. = after tool results). `requests`: set on a merged turn row. */
export interface EgressRow { id: string; at: number; host: string; model: string; bytes: number; estTokens: number; requestJson: string; items: EgressItem[]; round?: number; requests?: number }

export type AiEvent =
  | { type: "text"; delta: string }
  | { type: "usage"; input: number; output: number }
  | { type: "done"; stop: string }
  | { type: "error"; message: string; kind: string }
  | { type: "toolStart"; id: string; name: string; label: string }
  | { type: "toolEnd"; id: string; ok: boolean; summary: string }
  | { type: "proposal"; id: string; summary: string }
  | { type: "draft"; id: string; summary: string }
  | { type: "rule"; id: string; summary: string };                      // P4: a rule was proposed and saved OFF; id = rule id (decimal string)

// ---- Proposals (P2): suggested changes the user approves one by one ----
export type ProposalKind = "move" | "archive" | "trash" | "flag" | "create_folder" | "rule_change" | "event_create" | "event_change" | "event_cancel" | "invite_response";
export type ProposalState = "proposed" | "approved" | "executing" | "done" | "failed" | "partially_done" | "rejected" | "expired" | "undone";
export type ItemStatus = "pending" | "done" | "failed" | "skipped" | "noop" | "undone" | "undo_failed" | "unknown";
export interface ProposalCounts { total: number; selected: number; done: number; failed: number; skipped: number; noop: number; undone: number; undoFailed: number; unknown?: number }
/** Event proposals only: `rows` is a before/after table written by the app; `notify` = approving emails people. */
export interface EventCard { calendarName: string; rows: { label: string; before?: string | null; after: string }[]; notify: boolean; undoNote: string }
/** `title`, counts and `warnings` are written by the app from the resolved messages. `reason` is the model's own (untrusted) text. */
export interface ProposalSummary {
  id: number; convId: number | string; kind: ProposalKind; state: ProposalState; title: string; reason: string; warnings: string[]; error: string;
  createdAt: number; updatedAt: number; expiresAt: number; counts: ProposalCounts; undoable: boolean;
  /** Set for the event kinds (counts.total is 0 and there are no items). */
  event?: EventCard;
}
export interface ProposalItem {
  idx: number; account: string; folder: string; sender: string; subject: string; seen: boolean; flagged: boolean;
  toFolder: string; selected: boolean; status: ItemStatus; error: string;
}
export interface ProposalDetail extends ProposalSummary { items: ProposalItem[] }
export interface ApproveResult { proposal: ProposalDetail; ok: number; failed: number; skipped: number; noop: number; foldersChanged: boolean; note?: string }
export interface UndoResult { proposal: ProposalDetail; undone: number; failed: number; errors: string[] }
/** Payload of the `ai://proposal` event. */
export interface ProposalEvent { id: number; convId: number | string; state: ProposalState; counts: ProposalCounts }

// ---- Drafts (P3): messages the assistant wrote; the user opens them in Compose and sends them there ----
export type DraftKind = "reply" | "new";
/** There is no "sent": sending happens in Compose, which the model cannot reach. */
export type DraftState = "proposed" | "opened" | "dismissed";
/**
 * `to`, `cc`, `subject`, `warnings`, `newAddresses` and `quote` are computed by MiraMail; `body` is the model's cleaned plain text;
 * `reason` is the model's own (untrusted) words and is shown only as "AI's reason".
 */
export interface DraftSummary {
  id: number; convId: number | string; kind: DraftKind; state: DraftState; title: string; account: string;
  to: string[]; cc: string[]; subject: string; body: string; quote: string; reason: string;
  warnings: string[]; newAddresses: string[]; createdAt: number; updatedAt: number;
}
/** What `ai_open_draft` returns: the initial values of a Compose pane. */
export interface DraftPrefill {
  id: number; account: string; to: string[]; cc: string[]; bcc: string[]; subject: string; text: string;
  inReplyTo: { messageId: string; references: string | null } | null; aiDraft: boolean;
  /** The app's notes about the draft; shown under the AI banner in Compose. */
  warnings: string[];
}
/** Payload of the `ai://draft` event. */
export interface DraftEvent { id: number; convId: number | string; state: DraftState }

/** UI-side message: adds streaming / error state to the persisted shape. */
export interface UiMessage extends AiMessage {
  streaming?: boolean;
  /** Suggestions queued while this reply was being produced (ids), so cards show up at once. */
  proposalIds?: number[];
  /** Drafts written while this reply was being produced (ids). */
  draftIds?: number[];
  /** Rules proposed while this reply was being produced (ids). They are saved OFF. */
  ruleIds?: number[];
  stopped?: boolean;
  /** The turn ended because the assistant kept asking for tools (stop reason "tool_limit"). */
  toolLimit?: boolean;
  error?: { message: string; kind: string };
}

// ---- Local rules + template auto-reply (P4). No model is involved in running either. ----
export type RuleField = "from" | "to" | "cc" | "subject" | "listId" | "hasListUnsubscribe" | "hasAttachment" | "size" | "folder";
/** `greaterThan` is only for `size` (value = whole kilobytes). */
export type RuleOp = "equals" | "contains" | "glob" | "domainIs" | "greaterThan";
export interface RuleCondition { field: RuleField; op: RuleOp; value: string }
/** The only things a rule can do. There is no delete-forever, forward, reply or send. */
export type RuleAction = { type: "moveTo"; folder: string } | { type: "archive" } | { type: "trash" } | { type: "markRead" } | { type: "flag" } | { type: "colour"; colour: number };
export interface RuleSpec { v: 1; match: "all" | "any"; conditions: RuleCondition[]; actions: RuleAction[] }
export interface Rule {
  id: number; account: string; name: string; enabled: boolean; origin: "user" | "ai"; reason: string;
  createdAt: number; updatedAt: number; lastRunAt: number | null;
  /** false = the stored rule is damaged; it never runs. */
  valid: boolean; spec: RuleSpec | null;
  /** Plain-English description written by MiraMail (never by the model). */
  description: string;
}
export interface RuleInput { id: number | null; account: string; name: string; spec: RuleSpec }
export interface DryRun { scanned: number; matched: number; token?: string | null; sample: { folder: string; from: string; subject: string }[] }
export interface RuleCard { rule: Rule; dryRun: DryRun | null }
export type RunState = "done" | "partial" | "undone" | "failed";
export interface RuleRun { id: number; ruleId: number; ruleName: string; account: string; folder: string; ts: number; matched: number; summary: string; state: RunState; undoable: boolean }
export interface AutomationState { rulesPaused: boolean; autoreplyPaused: boolean }
export interface AutoReplyConfig {
  enabled: boolean; subject: string; body: string;
  /** Opt-in: `{subject}` / the default subject include the original subject (cut to 60 characters). */
  echoSubject: boolean;
  /** Unix seconds; the reply window is [start, end). */
  start: number | null; end: number | null;
  requireAuth: boolean; dailyCap: number;
}
export interface ReplyLogRow { id: number; account: string; ts: number; sender: string; subject: string; outcome: "sent" | "skipped" | "failed" | "pending" | "unknown"; reason: string; detail: string }
export interface ReplyPreview { raw: string; to: string; subject: string; body: string }
