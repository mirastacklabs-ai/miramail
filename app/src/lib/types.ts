// Wire types mirroring docs/APP_API.md (camelCase).
export type Tls = "implicit" | "starttls";
export interface ServerConfig { host: string; port: number; tls: Tls }
export interface AccountInfo { email: string; username: string; imap: ServerConfig; smtp: ServerConfig; provider: string | null }
export interface Advanced { imap: ServerConfig; smtp: ServerConfig; username: string }
export type FolderRole = "inbox" | "sent" | "drafts" | "trash" | "archive" | "junk" | "other";
export interface Folder { name: string; role: FolderRole; selectable: boolean; delimiter?: string | null }
export interface Flags { seen: boolean; flagged: boolean; answered: boolean; draft: boolean; deleted: boolean }
export interface Address { name: string | null; address: string }
export interface MessageSummary {
  account: string; folder: string; uid: number; messageId: string | null; subject: string;
  from: Address | null; to: Address[]; date: number | null; flags: Flags; snippet: string; hasBody: boolean;
  /** UIDVALIDITY the uid was seen under (forward state stores it). */
  uidvalidity?: number;
}
export interface Attachment { id: string; name: string; mime: string; size: number }
export type ImageVerdict = "tracker" | "suspicious" | "clean";
/** Advisory static analysis of one remote image (cannot observe server behaviour). */
export interface ImageInfo {
  url: string; host: string; width: number | null; height: number | null; hidden: boolean;
  verdict: ImageVerdict; reasons: string[]; tracker: string | null;
}
export interface MessageBody {
  summary: MessageSummary; text: string | null; html: string | null;
  blockedRemoteCount: number; images: ImageInfo[]; attachments: Attachment[]; cc: Address[]; references: string | null;
  invite: Invite | null;
}
export interface Outgoing {
  to: string[]; cc: string[]; bcc: string[]; subject: string; text: string;
  inReplyTo: { messageId: string; references: string | null } | null;
  attachments?: { name: string; mime: string; dataBase64: string }[];
  /** Rich mode: composer HTML (sanitised again by the backend); `text` is the plain alternative. */
  html?: string;
  /** Sender: the account's own address or a saved identity of it (the backend refuses anything else). */
  fromAddress?: string;
}
export interface SyncReport { new: number; updated: number; removed: number; uidvalidityReset: boolean; serverTotal: number }
export interface LoadOlderResult { loaded: number; remainingOnServer: number; serverTotal: number; reachedStart: boolean }
export interface AttachmentData { name: string; mime: string; dataBase64: string }
export type ConnState = "connected" | "offline" | "reconnecting";
export interface NewMailEvent { account: string; folder: string }
export interface StatusEvent { account: string; state: ConnState }
export interface SyncedEvent { account: string; folders: string[] }
export interface SyncSettings { foldersPerCycle: number; intervalSeconds: number; maxLiveFolders: number; initialSyncMessages: number }
export type LiveState = "connecting" | "live" | "polling" | "error" | "off";
export interface LiveStatus { state: LiveState; message?: string }
export interface LiveEvent { account: string; folder: string; state: LiveState; message?: string }
export type FlagName = "seen" | "flagged";

// ---- Calendar + Zoom (docs/CALENDAR_SPEC.md) ----
export interface CalendarInfo {
  id: string; name: string; color: string; kind: "local" | "caldav"; url: string | null; username: string | null;
  visible: boolean; readOnly: boolean; lastSync: number | null; error: string | null;
}
export type AttendeeStatus = "needs-action" | "accepted" | "declined" | "tentative";
export interface Attendee { email: string; name: string | null; role: "chair" | "required" | "optional"; status: AttendeeStatus }
export interface ZoomMeeting { meetingId: string; joinUrl: string; password: string | null; startUrl: string | null }
export interface CalEvent {
  uid: string; calendarId: string; summary: string; description: string; location: string;
  start: string; end: string; allDay: boolean; tzid: string | null; rrule: string | null; exdates: string[];
  attendees: Attendee[]; organizer: Attendee | null; status: "confirmed" | "tentative" | "cancelled";
  reminderMinutes: number | null; zoom: ZoomMeeting | null; sequence: number; href: string | null; etag: string | null;
}
export interface Occurrence {
  uid: string; calendarId: string; recurrenceStart: string; start: string; end: string; allDay: boolean;
  summary: string; location: string; color: string; hasZoom: boolean; isRecurring: boolean;
}
export interface Invite { method: "REQUEST" | "REPLY" | "CANCEL" | "PUBLISH"; event: CalEvent; myStatus: AttendeeStatus | null; alreadyInCalendar: boolean; attachmentId: string; senderUnverified?: boolean }
export interface ZoomStatus { configured: boolean; connected: boolean; clientId: string | null; account: string | null; redirectUri: string }
export type CalPreset = "icloud" | "fastmail" | "zoho" | "custom";
/** Zoho data centres (the part after calendar.zoho.). `in`, `jp`, `ca`, `sa` are unverified guesses. */
export const ZOHO_REGIONS = ["com", "in", "eu", "com.au", "jp", "ca", "sa"] as const;
/** Prefill for the Add-calendar form, inferred from the mail account. */
export interface CaldavSuggestion { preset: CalPreset; region: string | null; url: string | null; username: string }
/** A saved mail account at the same provider as a calendar preset (no secrets). */
export interface ReusableAccount { email: string; provider: CalPreset }
/** Failure of cal_add_caldav: plain-words message plus a non-secret trail (steps, HTTP statuses, hosts). */
export interface AddCalError { message: string; diagnostics: string[] }
export type RespondStatus = "accepted" | "tentative" | "declined";

// ---- Workspace persistence (docs/APP_API.md) ----
export interface DraftRec { id: string; kind: "compose" | "event"; account: string | null; title: string; payload: unknown; updatedAt: number }

// ---- Composer: signatures, identities, address book (docs/APP_API.md) ----
export type SigPlacement = "above" | "below";
export interface Signature { id: string; name: string; rich: boolean; text: string; html: string; placement: SigPlacement; accounts: string[] }
export interface SigDefaults { account: string; newId: string | null; replyId: string | null }
export interface Identity { id: string; account: string; name: string; address: string; replyTo: string; signatureId: string | null }
export interface ComposerData { signatures: Signature[]; defaults: SigDefaults[]; identities: Identity[] }
export interface AddrSuggestion { name: string | null; address: string }

// ---- Send pipeline: undo hold, outbox, drafts mirror (docs/APP_API.md "Send pipeline") ----
export type OutboxState = "holding" | "sending" | "failed" | "unknown" | "interrupted" | "scheduled";
export interface OutboxItem {
  id: string; account: string; to: string[]; subject: string; state: OutboxState; error: string | null;
  dueAt: number; nextRetryAt: number | null; failures: number; attachments: number; createdAt: number;
  /** Send later: Unix ms the message is to go out (state "scheduled"). */
  scheduledAt?: number | null;
}
export interface OutboxRestored { restore: string; attachments: AttachmentData[] }
export interface OutboxChanged { id: string; kind: "added" | "sent" | "failed" | "unknown" | "removed" }
export type MirrorStatus = "saved" | "unsupported" | "empty";
