// Single typed gateway to the Rust backend. Falls back to an in-memory mock in a plain browser.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  AccountInfo, Advanced, AttachmentData, CalEvent, CalPreset, CaldavSuggestion, ReusableAccount, CalendarInfo, ConnState, Occurrence, RespondStatus, ZoomMeeting, ZoomStatus, FlagName, Folder, FolderRole, MessageBody, MessageAnalysis,
  LoadOlderResult, MessageSummary, ComposerData, Signature, SigDefaults, Identity, AddrSuggestion, NewMailEvent, Outgoing, StatusEvent, SyncReport, SyncSettings, SyncedEvent, LiveEvent, LiveStatus, DraftRec,
  OutboxItem, OutboxRestored, OutboxChanged, MirrorStatus,
} from "./types";
import type { SafetyReport } from "./safety";
import { mock } from "./mock";

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Normalise any rejection into a user-facing string. */
export function errMsg(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  try { return JSON.stringify(e); } catch { return "Something went wrong."; }
}

async function call<T>(cmd: keyof typeof mock, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri) return invoke<T>(cmd, args);
  const fn = mock[cmd] as unknown as (a: Record<string, unknown>) => Promise<T>;
  return fn(args);
}

export const api = {
  addAccount: (email: string, password: string, advanced?: Advanced) =>
    call<AccountInfo>("add_account", { email, password, advanced: advanced ?? null }),
  listAccounts: () => call<AccountInfo[]>("list_accounts"),
  removeAccount: (email: string) => call<void>("remove_account", { email }),
  listFolders: (account: string) => call<Folder[]>("list_folders", { account }),
  unreadCounts: (account: string) => call<Record<string, number>>("unread_counts", { account }),
  syncFolder: (account: string, folder: string) => call<SyncReport>("sync_folder", { account, folder }),
  listMessages: (account: string, folder: string, limit: number, offset: number) =>
    call<MessageSummary[]>("list_messages", { account, folder, limit, offset }),
  loadOlder: (account: string, folder: string, count?: number) => call<LoadOlderResult>("load_older", { account, folder, count }),
  getMessage: (account: string, folder: string, uid: number, allowRemoteImages: boolean, skipTrackers = true, markSeen = true) =>
    call<MessageBody>("get_message", { account, folder, uid, allowRemoteImages, skipTrackers, markSeen }),
  /** Local, read-only warning-sign check for a cached message (no network). */
  /** Timing line for the device log (durations only, never content). Fire-and-forget. */
  /** `uidvalidity`: the shown message's; the backend refuses when the cached message has another (it is not the same message). */
  messageAnalysis: (account: string, folder: string, uid: number, allowRemoteImages: boolean, skipTrackers = true, uidvalidity?: number) =>
    call<MessageAnalysis>("message_analysis", { account, folder, uid, allowRemoteImages, skipTrackers, uidvalidity }),
  perfLog: (event: string, ms: number) => { void call<void>("perf_log", { event, ms: Math.max(0, Math.round(ms)) }).catch(() => {}); },
  safetyReport: (account: string, folder: string, uid: number, uidvalidity?: number) => call<SafetyReport>("safety_report", { account, folder, uid, uidvalidity }),
  getAttachment: (account: string, folder: string, uid: number, id: string, uidvalidity?: number) =>
    call<AttachmentData>("get_attachment", { account, folder, uid, id, uidvalidity }),
  setFlag: (account: string, folder: string, uid: number, flag: FlagName, value: boolean) =>
    call<void>("set_flag", { account, folder, uid, flag, value }),
  moveMessage: (account: string, folder: string, uid: number, destRole: FolderRole) =>
    call<void>("move_message", { account, folder, uid, destRole }),
  moveMessageToFolder: (account: string, folder: string, uid: number, destFolder: string) =>
    call<void>("move_message_to", { account, folder, uid, destFolder }),
  deleteMessage: (account: string, folder: string, uid: number) =>
    call<void>("delete_message", { account, folder, uid }),
  /** Compose's Send: validate, hold for `delaySecs` (undo window), then send from the outbox. */
  outboxSubmit: (account: string, message: Outgoing, restore: string, inlineCount: number, delaySecs: number, sendAt?: number, composeId?: string) =>
    call<OutboxItem>("outbox_submit", { account, message, restore, inlineCount, delaySecs, sendAt: sendAt ?? null, composeId: composeId ?? null }),
  outboxList: () => call<OutboxItem[]>("outbox_list"),
  outboxUndo: (id: string) => call<OutboxRestored>("outbox_undo", { id }),
  outboxConfirmEdit: (id: string) => call<void>("outbox_confirm_edit", { id }),
  outboxRetry: (id: string) => call<void>("outbox_retry", { id }),
  outboxDiscard: (id: string) => call<void>("outbox_discard", { id }),
  draftMirrorSave: (draftId: string, account: string, message: Outgoing) => call<MirrorStatus>("draft_mirror_save", { draftId, account, message }),
  draftMirrorRemove: (draftId: string) => call<void>("draft_mirror_remove", { draftId }),
  getRawMessage: (account: string, folder: string, uid: number, uidvalidity?: number) => call<AttachmentData>("get_raw_message", { account, folder, uid, uidvalidity }),
  saveAttachmentToDownloads: (account: string, folder: string, uid: number, id: string) =>
    call<string>("save_attachment_to_downloads", { account, folder, uid, id }),
  getSyncSettings: () => call<SyncSettings>("get_sync_settings"),
  setSyncSettings: (settings: SyncSettings) => call<SyncSettings>("set_sync_settings", { settings }),
  listLiveFolders: (account: string) => call<string[]>("list_live_folders", { account }),
  liveStates: (account: string) => call<Record<string, LiveStatus>>("live_states", { account }),
  setFolderLive: (account: string, folder: string, live: boolean) => call<LiveStatus>("set_folder_live", { account, folder, live }),

  // ---- composer: signatures, identities, address book ----
  composerData: () => call<ComposerData>("composer_data"),
  saveSignature: (signature: Signature) => call<Signature>("save_signature", { signature }),
  deleteSignature: (id: string) => call<void>("delete_signature", { id }),
  setSignatureDefaults: (defaults: SigDefaults) => call<void>("set_signature_defaults", { defaults }),
  saveIdentity: (identity: Identity) => call<Identity>("save_identity", { identity }),
  deleteIdentity: (id: string) => call<void>("delete_identity", { id }),
  sanitizeCompose: (html: string) => call<string>("sanitize_compose", { html }),
  suggestAddresses: (prefix: string, limit = 8, account?: string) => call<AddrSuggestion[]>("suggest_addresses", { prefix, limit, account }),

  // ---- workspace: drafts + layout ----
  saveDraft: (draft: DraftRec) => call<number>("save_draft", { draft }),
  listDrafts: () => call<DraftRec[]>("list_drafts"),
  deleteDraft: (id: string) => call<void>("delete_draft", { id }),
  saveLayout: (json: string) => call<void>("save_layout", { json }),
  loadLayout: () => call<string | null>("load_layout"),

  // ---- workspace: detached pane windows (docs/WORKSPACE_SPEC.md) ----
  detachPane: (paneId: string, payloadJson: string, x: number, y: number, w: number, h: number) =>
    call<string>("detach_pane", { paneId, payloadJson, x, y, w, h }),
  getPane: () => call<string | null>("get_pane"),
  savePane: (payloadJson: string) => call<void>("save_pane", { payloadJson }),
  attachPane: (paneId: string, targetLabel: string, opts: { dock?: { target: string; zone: string }; at?: { x: number; y: number }; park?: boolean } = {}) =>
    call<void>("attach_pane", { paneId, targetLabel, dock: opts.dock ?? null, at: opts.at ?? null, park: opts.park ?? false }),
  releasePane: (paneId: string) => call<void>("release_pane", { paneId }),
  closePaneWindow: () => call<void>("close_pane_window"),
  listWindows: () => call<unknown>("list_windows"),
  windowAt: (screenX: number, screenY: number) => call<string | null>("window_at", { screenX, screenY }),
  focusWindow: (label: string) => call<void>("focus_window", { label }),

  // ---- calendar ----
  calListCalendars: () => call<CalendarInfo[]>("cal_list_calendars"),
  calCreateLocal: (name: string, color: string) => call<CalendarInfo>("cal_create_local", { name, color }),
  calAddCaldav: (preset: CalPreset, url: string | undefined, username: string, password: string, region?: string) =>
    call<CalendarInfo[]>("cal_add_caldav", { preset, url, region, username, password }),
  /** Connect with a saved mail account's login. The password is read by the backend and never crosses IPC. */
  calAddCaldavReuse: (preset: CalPreset, reuseMailAccount: string, region?: string) =>
    call<CalendarInfo[]>("cal_add_caldav", { preset, region, reuseMailAccount }),
  calReusableAccounts: (preset: CalPreset) => call<ReusableAccount[]>("cal_reusable_accounts", { preset }),
  calSuggestCaldav: (account?: string) => call<CaldavSuggestion>("cal_suggest_caldav", { account }),
  calUpdateCalendar: (id: string, patch: { name?: string; color?: string; visible?: boolean }) =>
    call<void>("cal_update_calendar", { id, ...patch }),
  calRemoveCalendar: (id: string) => call<void>("cal_remove_calendar", { id }),
  calSync: (calendarId?: string) => call<{ calendars: number; events: number }>("cal_sync", { calendarId }),
  calListEvents: (start: string, end: string) => call<Occurrence[]>("cal_list_events", { start, end }),
  calGetEvent: (uid: string, calendarId: string) => call<CalEvent>("cal_get_event", { uid, calendarId }),
  calSaveEvent: (event: CalEvent, sendInvites: boolean) => call<CalEvent>("cal_save_event", { event, sendInvites }),
  calDeleteEvent: (uid: string, calendarId: string, notifyAttendees: boolean) =>
    call<void>("cal_delete_event", { uid, calendarId, notifyAttendees }),
  calRespondInvite: (account: string, folder: string, uid: number, attachmentId: string, status: RespondStatus, confirmUnverified = false) =>
    call<void>("cal_respond_invite", { account, folder, uid, attachmentId, status, confirmUnverified }),
  calImportInvite: (account: string, folder: string, uid: number, attachmentId: string, confirmUnverified = false) =>
    call<CalEvent>("cal_import_invite", { account, folder, uid, attachmentId, confirmUnverified }),
  zoomStatus: () => call<ZoomStatus>("zoom_status"),
  zoomSetClient: (clientId: string, clientSecret?: string) => call<void>("zoom_set_client", { clientId, clientSecret }),
  zoomConnect: () => call<ZoomStatus>("zoom_connect"),
  zoomDisconnect: () => call<void>("zoom_disconnect"),
  zoomCreateMeeting: (topic: string, start: string, durationMinutes: number, timezone: string, agenda?: string) =>
    call<ZoomMeeting>("zoom_create_meeting", { topic, start, durationMinutes, timezone, agenda }),
  zoomDeleteMeeting: (meetingId: string) => call<void>("zoom_delete_meeting", { meetingId }),
};

/** Backend emits `cal://changed` after sync/save; the mock dispatches a window event with the same meaning. */
export async function onCalChanged(cb: () => void): Promise<() => void> {
  if (!isTauri) {
    const h = () => cb();
    window.addEventListener("mock-cal-changed", h);
    return () => window.removeEventListener("mock-cal-changed", h);
  }
  return listen("cal://changed", () => cb());
}

/** Step names while a CalDAV server is being connected ("Looking up your calendar server", ...). */
export async function onCalAddProgress(cb: (step: string) => void): Promise<() => void> {
  if (!isTauri) {
    const h = (e: Event) => cb((e as CustomEvent<string>).detail);
    window.addEventListener("mock-cal-progress", h);
    return () => window.removeEventListener("mock-cal-progress", h);
  }
  return listen<string>("cal://add-progress", (e) => cb(e.payload));
}

export async function onNewMail(cb: (e: NewMailEvent) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listen<NewMailEvent>("mail://new", (e) => cb(e.payload));
}
export async function onSynced(cb: (e: SyncedEvent) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listen<SyncedEvent>("mail://synced", (e) => cb(e.payload));
}
export async function onLive(cb: (e: LiveEvent) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listen<LiveEvent>("mail://live", (e) => cb(e.payload));
}
export async function onStatus(cb: (e: StatusEvent) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listen<StatusEvent>("mail://status", (e) => cb(e.payload));
}

/** Listen on THIS window only. A bare `listen()` has target Any, so it also receives events that the backend aimed at
 *  one other window with `emit_to(label)`; every window would then act on a close / detach / attach meant for one. */
async function listenSelf<T>(name: string, cb: (payload: T) => void): Promise<() => void> {
  const { getCurrentWebviewWindow } = await import("@tauri-apps/api/webviewWindow");
  return getCurrentWebviewWindow().listen<T>(name, (e) => cb(e.payload));
}

/** Native menu "Close Pane" (Cmd+W is owned by the menu on macOS, so the webview may never see the key). */
export async function onClosePane(cb: () => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listenSelf("ws://close-pane", () => cb());
}

/** Native menu / backend window events. `ws://*` are targeted at the focused window; `pane://*` see detach.ts for payloads. */
export async function onWsEvent(name: "ws://detach-pane" | "ws://attach-pane" | "pane://close-request" | "pane://flush" | "pane://detached" | "pane://closed" | "pane://attach", cb: (payload: unknown) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listenSelf<unknown>(name, cb);
}

/** A link click in a message frame, cancelled by the backend's navigation hook (linknav.rs) and forwarded to this window only. Payload: the URL. */
export async function onMailLink(cb: (url: string) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listenSelf<string>("mail://link", cb);
}

export async function openExternal(url: string): Promise<void> {
  if (isTauri) {
    await invoke("open_link", { url });
  } else {
    console.info("[mock] open", url);
  }
}
export type { ConnState };

/** Outbox changes (any window). The browser mock dispatches a window event with the same payload. */
export async function onOutbox(cb: (e: OutboxChanged) => void): Promise<() => void> {
  if (!isTauri) {
    const h = (e: Event) => cb((e as CustomEvent<OutboxChanged>).detail);
    window.addEventListener("mock-outbox", h);
    return () => window.removeEventListener("mock-outbox", h);
  }
  return listen<OutboxChanged>("outbox://changed", (e) => cb(e.payload));
}
