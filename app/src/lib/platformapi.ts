// Gateway to the platform-integration backend (badge / tray / widget / notifications / mailto / launch at login).
// Own file so the shared api.ts stays untouched. Wire types are camelCase and mirror badge.rs / plat_prefs.rs / mailto.rs.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isTauri } from "./api";

export interface Recent { account: string; folder: string; uid: number; from: string; subject: string | null; date: number }
export interface InboxSummary {
  total: number; perAccount: Record<string, number>; accounts: string[]; inboxTotal: number;
  recent: Recent[]; widgetAccount: string | null; previews: boolean;
}
export type TrayStyle = "unread" | "total" | "hideZero";
export interface Geo { x: number; y: number; w: number; h: number }
export interface PlatformPrefs {
  dockBadge: boolean; trayShow: boolean; trayStyle: TrayStyle;
  widgetShow: boolean; widgetPreviews: boolean; widgetAccount: string | null; widgetGeo: Geo | null; mainGeo: Geo | null;
  notify: boolean; notifyPreview: boolean; notifySound: boolean; notifyOff: string[];
  /** Accounts that notify only for VIP senders. */
  notifyVipOnly: string[];
  /** Mark mail of muted conversations as read on arrival. */
  muteAutoRead: boolean;
}
/** Same shape as the compose init used by `workspace.openCompose`. */
export interface MailtoDraft { to: string[]; cc: string[]; bcc: string[]; subject: string; text: string; inReplyTo: null }
export interface OpenMessage { account: string; folder: string; uid: number; selectOnly?: boolean }

export const DEFAULT_PREFS: PlatformPrefs = {
  dockBadge: true, trayShow: false, trayStyle: "unread", widgetShow: false, widgetPreviews: false, widgetAccount: null,
  widgetGeo: null, mainGeo: null, notify: false, notifyPreview: true, notifySound: true, notifyOff: [], notifyVipOnly: [], muteAutoRead: false,
};
export const EMPTY_SUMMARY: InboxSummary = { total: 0, perAccount: {}, accounts: [], inboxTotal: 0, recent: [], widgetAccount: null, previews: false };

// Browser (no Tauri) fallbacks so `npm run dev` in a plain browser still renders.
let mockPrefs: PlatformPrefs = { ...DEFAULT_PREFS };
let mockLogin = false;

export const platformApi = {
  getSummary: (): Promise<InboxSummary> => (isTauri ? invoke("get_inbox_summary") : Promise.resolve({ ...EMPTY_SUMMARY })),
  getPrefs: (): Promise<PlatformPrefs> => (isTauri ? invoke("get_platform_prefs") : Promise.resolve({ ...mockPrefs })),
  setPrefs: (prefs: PlatformPrefs): Promise<PlatformPrefs> => {
    if (isTauri) return invoke("set_platform_prefs", { prefs });
    mockPrefs = { ...prefs };
    return Promise.resolve({ ...mockPrefs });
  },
  /** Bring the main window forward, optionally to an inbox or a message. */
  focusMain: (account?: string, folder?: string, uid?: number): Promise<void> =>
    isTauri ? invoke("focus_main", { account: account ?? null, folder: folder ?? null, uid: uid ?? null }) : Promise.resolve(),
  widgetMenu: (): Promise<void> => (isTauri ? invoke("widget_menu") : Promise.resolve()),
  takePendingMailto: (): Promise<MailtoDraft[]> => (isTauri ? invoke("take_pending_mailto") : Promise.resolve([])),
  notifyTest: (): Promise<void> => (isTauri ? invoke("notify_test") : Promise.resolve()),
  /** Build/runtime facts that decide which settings are offered (sandboxed App Store build, launch-at-login support). */
  platformInfo: (): Promise<{ sandboxed: boolean; launchAtLogin: boolean; notifications: boolean }> =>
    isTauri ? invoke("platform_info") : Promise.resolve({ sandboxed: false, launchAtLogin: true, notifications: true }),
  getLaunchAtLogin: (): Promise<boolean> => (isTauri ? invoke("get_launch_at_login") : Promise.resolve(mockLogin)),
  setLaunchAtLogin: (on: boolean): Promise<boolean> => {
    if (isTauri) return invoke("set_launch_at_login", { on });
    mockLogin = on;
    return Promise.resolve(on);
  },
};

async function on<T>(name: string, cb: (p: T) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listen<T>(name, (e) => cb(e.payload));
}
export const onBadge = (cb: (s: InboxSummary) => void) => on<InboxSummary>("mail://badge", cb);
export const onMenuAction = (cb: (action: string) => void) => on<string>("app://menu", cb);
export const onMailto = (cb: () => void) => on<null>("app://mailto", () => cb());
export const onOpenInbox = (cb: (account: string) => void) => on<string>("app://open-inbox", cb);
export const onOpenMessage = (cb: (m: OpenMessage) => void) => on<OpenMessage>("app://open-message", cb);
export const onNotice = (cb: (msg: string) => void) => on<string>("app://notice", cb);
export const onPrefsChanged = (cb: () => void) => on<null>("app://prefs-changed", () => cb());
