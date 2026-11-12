// Pure logic for the phone layout (no Svelte, no DOM): when it applies, the navigation stack, folder list, touch gestures.

import { fmtN } from "./older.ts";

/** Phone layout applies on a mobile OS (iOS / Android) when the screen's short side is phone-sized. Never on a desktop,
 *  however narrow the window. `force` ("phone") is a dev switch for a browser (?layout=phone). */
export interface PhoneEnv { ua: string; maxTouchPoints: number; shortSide: number; force?: string | null; /** true only in a dev build (import.meta.env.DEV): the ?layout=phone switch is ignored otherwise */ dev?: boolean }
export const PHONE_MAX_SHORT_SIDE = 600;

export function isMobileOs(ua: string, maxTouchPoints: number): boolean {
  if (/iPhone|iPad|iPod|Android/i.test(ua)) return true;
  return /Macintosh/.test(ua) && maxTouchPoints > 1; // iPadOS reports a Mac user agent; real Macs report 0 touch points
}
export function isPhoneLayout(e: PhoneEnv): boolean {
  if (e.force === "phone" && e.dev === true) return true;
  return isMobileOs(e.ua, e.maxTouchPoints) && e.shortSide > 0 && e.shortSide < PHONE_MAX_SHORT_SIDE;
}

// ---- navigation stack: Mail (Mailboxes > List > Reader) | MIRA | Calendar | Settings (list > section) ----
export type Tab = "mail" | "mira" | "calendar" | "settings";
export type MailScreen = "boxes" | "list" | "reader";
export interface Nav { tab: Tab; mail: MailScreen; section: string | null }
export const startNav = (): Nav => ({ tab: "mail", mail: "boxes", section: null });

/** app.view only says "Calendar is the current area" (Mail otherwise): the Mac sidebar / panes key off it. */
export const viewForTab = (t: Tab): "mail" | "calendar" => (t === "calendar" ? "calendar" : "mail");
/** After the active account changes (switch or removal) the old list / reader no longer belong to it: back to the Mailboxes screen. */
export const navAfterAccountChange = (n: Nav): Nav => ({ ...n, mail: "boxes" });
/** The remembered folder is kept per account. */
export const folderKey = (account: string | null | undefined): string => `miramail.phone.folder:${account ?? ""}`;

export function selectTab(n: Nav, tab: Tab): Nav {
  // Tapping the active tab pops to its root, like iOS.
  if (n.tab === tab) return tab === "mail" ? { ...n, mail: "boxes" } : tab === "settings" ? { ...n, section: null } : n;
  return { ...n, tab };
}
export const openList = (n: Nav): Nav => ({ ...n, tab: "mail", mail: "list" });
export const openReader = (n: Nav): Nav => ({ ...n, tab: "mail", mail: "reader" });
export const openSection = (n: Nav, section: string): Nav => ({ ...n, tab: "settings", section });
export function canGoBack(n: Nav): boolean {
  return n.tab === "mail" ? n.mail !== "boxes" : n.tab === "settings" ? n.section !== null : false;
}
export function back(n: Nav): Nav {
  if (n.tab === "mail") return n.mail === "reader" ? { ...n, mail: "list" } : n.mail === "list" ? { ...n, mail: "boxes" } : n;
  if (n.tab === "settings" && n.section !== null) return { ...n, section: null };
  return n;
}
/** The tab bar hides while a message is open (the reader has its own toolbar). */
export const showTabBar = (n: Nav): boolean => !(n.tab === "mail" && n.mail === "reader");

// ---- remembered folder ----
export function startFolder(folders: { name: string; role: string; selectable: boolean }[], saved: string | null): string | null {
  const ok = (f: { selectable: boolean }) => f.selectable;
  const s = saved ? folders.find((f) => f.name === saved && ok(f)) : undefined;
  return (s ?? folders.find((f) => f.role === "inbox" && ok(f)) ?? folders.find(ok))?.name ?? null;
}

// ---- folder rows for the Mailboxes screen ----
export interface FolderLike { name: string; role: string; selectable: boolean; delimiter?: string | null }
export interface FolderRow { name: string; label: string; depth: number; selectable: boolean; role: string }
const ROLE_ORDER = ["inbox", "drafts", "sent", "archive", "junk", "trash", "other"];
const delimOf = (f: FolderLike) => f.delimiter || (f.name.includes("/") ? "/" : f.name.includes(".") && f.name.toUpperCase().startsWith("INBOX.") ? "." : "/");
const roleLabel = (r: string) => r[0].toUpperCase() + r.slice(1);
export function folderRows(folders: FolderLike[]): FolderRow[] {
  const all = [...folders];
  const top = (f: FolderLike) => f.name.split(delimOf(f))[0];
  const rank = (f: FolderLike) => ROLE_ORDER.indexOf(all.find((x) => x.name === top(f))?.role ?? f.role);
  all.sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
  const out: FolderRow[] = [];
  for (const f of all) {
    const d = delimOf(f), parts = f.name.split(d);
    const hasKids = all.some((o) => o.name.startsWith(f.name + d));
    if (!f.selectable && !hasKids) continue;
    const label = parts.length === 1 ? (f.role === "other" ? f.name : roleLabel(f.role)) : parts[parts.length - 1];
    out.push({ name: f.name, label, depth: parts.length - 1, selectable: f.selectable, role: f.role });
  }
  return out;
}

// ---- touch gestures ----
/** Edge swipe back: starts at the left edge, travels right, mostly horizontal. */
export const EDGE_ZONE = 24;
export const isEdgeStart = (startX: number): boolean => startX <= EDGE_ZONE;
/** A row swipe may only begin outside the edge zone, so it never competes with the back swipe. */
export const rowSwipeMayStart = (startX: number): boolean => !isEdgeStart(startX);
export function isBackSwipe(startX: number, dx: number, dy: number): boolean {
  return isEdgeStart(startX) && dx >= 70 && Math.abs(dx) > Math.abs(dy) * 1.6;
}
export const PULL_TRIGGER = 70;
export const PULL_DAMPING = 0.5;
export const PULL_MAX = 110;
/** Pulled offset (after damping) at which releasing starts a sync. */
export const PULL_READY_OFFSET = PULL_TRIGGER * 0.5 + 10;
/** Pull-to-refresh: only from the very top, damped; `ready` once past the trigger. */
export function pull(dy: number, scrollTop: number): { offset: number; ready: boolean } {
  if (scrollTop > 0 || dy <= 0) return { offset: 0, ready: false };
  const offset = Math.min(PULL_MAX, dy * PULL_DAMPING);
  return { offset, ready: offset >= PULL_READY_OFFSET };
}
export const SWIPE_ACTION_W = 144; // two 72 px buttons revealed by a left swipe
export const SWIPE_READ_AT = 90;   // right swipe past this toggles read / unread
export const SWIPE_OPEN_AT = 56;
export type SwipeEnd = "open" | "close" | "toggle-read";
/** Where a row swipe ends. `dx` is the total drag (negative = left), `wasOpen` whether the action buttons were showing. */
export function swipeEnd(dx: number, wasOpen: boolean): SwipeEnd {
  if (wasOpen) return dx >= 40 ? "close" : "open";
  if (dx >= SWIPE_READ_AT) return "toggle-read";
  return dx <= -SWIPE_OPEN_AT ? "open" : "close";
}
/** Row offset while dragging, clamped (no rubber-band beyond the buttons). */
export function swipeOffset(dx: number, wasOpen: boolean): number {
  const x = (wasOpen ? -SWIPE_ACTION_W : 0) + dx;
  return Math.max(-SWIPE_ACTION_W, Math.min(SWIPE_READ_AT + 30, x));
}

// ---- on-screen keyboard ----
/** Height covered by the keyboard given the layout viewport and the visual viewport. */
export function keyboardInset(layoutH: number, vvH: number, vvTop: number): number {
  return Math.max(0, Math.round(layoutH - vvH - vvTop));
}

// ---- row swipe buttons per folder (same moves the Mac offers: archive / trash / back to the inbox; nothing permanent) ----
export type RowAction = { id: "archive" | "trash" | "inbox"; label: string };
export function rowActions(role: string | undefined): RowAction[] {
  if (role === "trash") return [{ id: "inbox", label: "Inbox" }];
  if (role === "archive") return [{ id: "inbox", label: "Inbox" }, { id: "trash", label: "Trash" }];
  if (role === "junk") return [{ id: "inbox", label: "Not junk" }, { id: "trash", label: "Trash" }];
  return [{ id: "archive", label: "Archive" }, { id: "trash", label: "Trash" }];
}

/** A swipe action on a row may only touch the open mailbox: rows from another account or folder (search results across
 *  the mailbox) would otherwise be acted on by uid alone, which can name a different message there. */
export function rowSwipeAllowed(row: { account?: string; folder?: string }, cur: { account: string | null; folder: string }): boolean {
  return (!row.account || row.account === cur.account) && (!row.folder || row.folder === cur.folder);
}
/** The click that ends a swipe (within this window) must not open the row. */
export const SWIPE_CLICK_GUARD_MS = 350;
/** Taps on a bottom sheet this soon after it opened are the tail of the long press that opened it (iOS' synthetic click), not a choice. */
export const SHEET_TAP_GUARD_MS = 350;
export const sheetTapIgnored = (openedAt: number, now: number): boolean => now - openedAt < SHEET_TAP_GUARD_MS;
/** A row's gesture is owned by a long press that already fired (data-lp="1"): the swipe must ignore the rest of it. */
export const longPressOwns = (flag: string | undefined): boolean => flag === "1";
export const swipedRecently = (stamp: string | undefined, now: number): boolean => now - Number(stamp ?? 0) < SWIPE_CLICK_GUARD_MS;

// ---- compose ----
/** Send is available when what Compose's send enforces holds: at least one To recipient and every entered address
 *  (To, Cc, Bcc) valid. Cc/Bcc alone is not enough because the send path requires a To. */
export const canSendTo = (to: string[], others: string[], valid: (a: string) => boolean): boolean =>
  to.length > 0 && [...to, ...others].every(valid);
/** What a recipient chip shows: ALWAYS the real address. The display name is secondary and dropped when it itself looks
 *  like an address (`"ceo@company.com" <attacker@evil.example>` must not read as ceo@company.com). */
export function chipParts(c: string): { addr: string; name: string } {
  const m = c.match(/^\s*"?([^"<]*?)"?\s*<(.+)>\s*$/);
  if (!m) return { addr: c.trim(), name: "" };
  const name = m[1].trim();
  return { addr: m[2].trim(), name: name.includes("@") ? "" : name };
}
/** Extra pixels to scroll so the caret [top, bottom] sits inside [viewTop, viewBottom] with `margin` to spare; 0 when already visible. */
export function caretScrollDelta(caretTop: number, caretBottom: number, viewTop: number, viewBottom: number, margin = 16): number {
  if (caretBottom + margin > viewBottom) return caretBottom + margin - viewBottom;
  if (caretTop - margin < viewTop) return caretTop - margin - viewTop;
  return 0;
}
/** Title of the compose nav bar: the subject once there is one, else "New Message" / "Reply". */
export const composeTitle = (subject: string, isReply: boolean): string => subject.trim() || (isReply ? "Reply" : "New Message");
/** Subtle draft status under the title ("" = show nothing). */
export function draftStatus(mirror: string): string {
  return mirror === "Draft saved on server" ? "Draft saved" : mirror ? "Draft not saved on server" : "";
}
/** Spoken label of a list row. */
export function rowLabel(r: { unread: boolean; flagged: boolean; from: string; subject: string; when: string; snippet?: string }): string {
  return [r.unread ? "Unread" : "", r.flagged ? "Flagged" : "", r.from, r.subject || "No subject", r.when].filter(Boolean).join(", ");
}

// ---- list: long-press, search count, result location, bulk actions ----
export const LONG_PRESS_MS = 450;
export const LONG_PRESS_SLOP = 10;
/** A finger that travels further than this is scrolling or swiping, not pressing. */
export const longPressMoved = (dx: number, dy: number): boolean => Math.hypot(dx, dy) > LONG_PRESS_SLOP;

/** Same wording as the Mac list: Trash gives "Delete forever", Junk gives "Not junk", Trash has no junk action. */
export function bulkActions(role: string | undefined): { trashLabel: string; junk: "junk" | "notjunk" | null } {
  return { trashLabel: role === "trash" ? "Delete forever" : "Trash", junk: role === "junk" ? "notjunk" : role === "trash" ? null : "junk" };
}
/** Folder / account line under a search result ("" when the row is in the folder the user is looking at). */
export function whereLine(scope: "folder" | "account" | "all", m: { account: string; folder: string }, curFolder: string): string {
  if (scope === "folder" && m.folder === curFolder) return "";
  return (scope === "all" ? m.account + " · " : "") + m.folder;
}
/** "N of M messages" / backend result count line. */
export function listCountText(o: { backend: boolean; searching: boolean; hasResults: boolean; total: number; scope: "folder" | "account" | "all"; filterActive: boolean; filtered: number; loaded: number }): string {
  if (o.backend) {
    if (o.searching && !o.hasResults) return "Searching…";
    return `${fmtN(o.total)} match${o.total === 1 ? "" : "es"}${o.scope === "folder" ? " in this folder" : o.scope === "account" ? " in this account" : " in all accounts"}`;
  }
  return o.filterActive ? `${fmtN(o.filtered)} of ${fmtN(o.loaded)} messages` : `${fmtN(o.loaded)} messages`;
}

// ---- folder tree (collapse, unread incl. hidden descendants, long-press menu) ----
export interface TreeRow extends FolderRow { hasKids: boolean; collapsed: boolean; count: number }
/** Folder rows with collapse state. Descendants of a collapsed folder are hidden; a collapsed folder's unread count includes them. */
export function folderTree(folders: FolderLike[], collapsed: Record<string, boolean>, unread: Record<string, number>): TreeRow[] {
  const byName = new Map(folders.map((f) => [f.name, f]));
  const out: TreeRow[] = [];
  for (const r of folderRows(folders)) {
    const f = byName.get(r.name);
    if (!f) continue;
    const d = delimOf(f), parts = f.name.split(d);
    let hidden = false;
    for (let i = 1; i < parts.length; i++) if (collapsed[parts.slice(0, i).join(d)]) hidden = true;
    if (hidden) continue;
    const hasKids = folders.some((o) => o.name.startsWith(f.name + d));
    const isCollapsed = hasKids && !!collapsed[f.name];
    let count = unread[f.name] ?? 0;
    if (isCollapsed) for (const [k, v] of Object.entries(unread)) if (k.startsWith(f.name + d)) count += v;
    out.push({ ...r, hasKids, collapsed: isCollapsed, count });
  }
  return out;
}
export const isSpecialFolder = (f: { role: string; name: string }): boolean => f.role !== "other" || f.name.toUpperCase() === "INBOX";
export interface FolderMenuItem { id: "live" | "sub" | "rename" | "move" | "delete" | "empty" | "subscribe"; label: string; danger?: boolean; disabled?: boolean }
/** Entries of the folder long-press sheet; same set and rules as the Mac sidebar menu. `subscribed` null = still loading. */
export function folderMenu(f: { role: string; name: string }, o: { inboxName: string; live: boolean; movable: boolean; subscribed: boolean | null }): FolderMenuItem[] {
  const out: FolderMenuItem[] = [];
  const inbox = f.name === o.inboxName;
  out.push({ id: "live", label: inbox ? "INBOX is always live" : o.live ? "Stop keeping live" : "Keep live", disabled: inbox });
  out.push({ id: "sub", label: "New subfolder…" });
  if (!isSpecialFolder(f)) {
    out.push({ id: "rename", label: "Rename…" });
    if (o.movable) out.push({ id: "move", label: "Move to…" });
    out.push({ id: "delete", label: "Delete folder…", danger: true });
  }
  if (f.role === "trash" || f.role === "junk") out.push({ id: "empty", label: `Empty ${f.role === "trash" ? "Trash" : "Junk"}…`, danger: true });
  out.push(o.subscribed === null ? { id: "subscribe", label: "Checking subscription…", disabled: true } : { id: "subscribe", label: o.subscribed ? "Unsubscribe from folder" : "Subscribe to folder" });
  return out;
}
