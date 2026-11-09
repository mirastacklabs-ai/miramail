// Pure logic for detaching / attaching panes as native windows. No Svelte runes, no DOM, no Tauri: runs under node --test.
// Everything that crosses a window boundary (URL hash, IPC payloads, events, persisted window layout) is validated here.

export type DetachKind = "compose" | "event" | "aichat";
/** Kinds whose state is self-contained JSON. mailbox / reader / calendar share app-global state in the main webview. */
export const DETACHABLE: readonly DetachKind[] = ["compose", "event", "aichat"];
export const isDetachable = (k: unknown): k is DetachKind => typeof k === "string" && (DETACHABLE as readonly string[]).includes(k);

export const ID_RE = /^[A-Za-z0-9_-]{1,64}$/;
const CONV_RE = /^[A-Za-z0-9_.:-]{1,128}$/;
export const MAIN_LABEL = "main";
export const labelFor = (id: string) => `pane-${id}`;
export const validLabel = (l: unknown): l is string => l === MAIN_LABEL || (typeof l === "string" && l.startsWith("pane-") && ID_RE.test(l.slice(5)));

// ---- entry mode (#pane=<id>) ----
export type Entry = { mode: "main" } | { mode: "pane"; id: string } | { mode: "invalid" };
/** `injected` is the id set by the backend's init script; the URL hash is the fallback. A malformed id never falls back to the full app. */
export function parseEntry(hash: string | null | undefined, injected?: unknown): Entry {
  if (injected !== undefined && injected !== null) return typeof injected === "string" && ID_RE.test(injected) ? { mode: "pane", id: injected } : { mode: "invalid" };
  if (typeof hash !== "string") return { mode: "main" };
  const m = /^#pane=(.*)$/.exec(hash);
  if (!m) return { mode: "main" };
  let id = m[1];
  try { id = decodeURIComponent(id); } catch { return { mode: "invalid" }; }
  return ID_RE.test(id) ? { mode: "pane", id } : { mode: "invalid" };
}

// ---- payload envelope: serialise / rehydrate per kind ----
export interface Envelope { v: 1; kind: DetachKind; title: string; dirty: boolean; payload: Record<string, unknown> }

const isObj = (x: unknown): x is Record<string, unknown> => !!x && typeof x === "object" && !Array.isArray(x);
const str = (x: unknown, d = "") => (typeof x === "string" ? x : d);
const strs = (x: unknown): string[] | null => (Array.isArray(x) && x.length <= 1000 && x.every((s) => typeof s === "string") ? (x as string[]) : null);
const MAX_BYTES = 1024 * 1024 + 4096;

/** Copy only known compose fields (drops unknown / prototype keys). Null when the shape is wrong. */
export function sanitizeCompose(p: unknown): Record<string, unknown> | null {
  if (!isObj(p)) return null;
  for (const k of ["to", "cc", "bcc", "subject", "text", "base"]) if (typeof p[k] !== "string") return null;
  if (p.account !== null && typeof p.account !== "string") return null;
  let inReplyTo: unknown = null;
  if (p.inReplyTo != null) {
    const r = p.inReplyTo;
    if (!isObj(r) || typeof r.messageId !== "string" || (r.references != null && typeof r.references !== "string")) return null;
    inReplyTo = { messageId: r.messageId, references: (r.references as string | null | undefined) ?? null };
  }
  const out: Record<string, unknown> = { account: p.account, to: p.to, cc: p.cc, bcc: p.bcc, subject: p.subject, text: p.text, inReplyTo, showCc: p.showCc === true, base: p.base };
  if (p.aiDraft === true) out.aiDraft = true;
  if (typeof p.aiDraftId === "number" && Number.isFinite(p.aiDraftId)) out.aiDraftId = p.aiDraftId;
  const w = strs(p.aiWarnings);
  if (w?.length) out.aiWarnings = w;
  if (typeof p.replyNotice === "string" && p.replyNotice) out.replyNotice = p.replyNotice.slice(0, 1000);
  // Composer editor state (rich body, mode, signature, sender, forward source).
  if (typeof p.html === "string") out.html = p.html;
  if (typeof p.seedHtml === "string") out.seedHtml = p.seedHtml;
  if (p.mode === "plain" || p.mode === "rich") out.mode = p.mode;
  if (p.kind === "new" || p.kind === "reply") out.kind = p.kind;
  if (typeof p.sigId === "string") out.sigId = p.sigId;
  if (p.sigApplied === true) out.sigApplied = true;
  if (typeof p.from === "string") out.from = p.from;
  const f = p.fwd;
  if (isObj(f) && typeof f.account === "string" && typeof f.folder === "string" && typeof f.uid === "number" && Number.isFinite(f.uid) && (f.as === "inline" || f.as === "attach") && Array.isArray(f.atts)) {
    const atts = f.atts.filter((a) => isObj(a) && typeof a.id === "string" && typeof a.name === "string" && typeof a.mime === "string" && typeof a.size === "number").slice(0, 200).map((a) => ({ id: a.id, name: a.name, mime: a.mime, size: a.size }));
    out.fwd = { account: f.account, folder: f.folder, uid: f.uid, ...(typeof f.uidvalidity === "number" && Number.isFinite(f.uidvalidity) ? { uidvalidity: f.uidvalidity } : {}), as: f.as, atts };
  }
  return out;
}
export function sanitizeEvent(p: unknown): Record<string, unknown> | null {
  if (!isObj(p) || !isObj(p.init)) return null;
  const i = p.init, init: Record<string, unknown> = {};
  if (i.uid != null) { if (typeof i.uid !== "string") return null; init.uid = i.uid; }
  if (i.calendarId != null) { if (typeof i.calendarId !== "string") return null; init.calendarId = i.calendarId; }
  for (const k of ["start", "end"]) if (i[k] != null) { if (typeof i[k] !== "number" || !Number.isFinite(i[k])) return null; init[k] = i[k]; }
  if (i.allDay != null) { if (typeof i.allDay !== "boolean") return null; init.allDay = i.allDay; }
  const created = strs(p.created ?? []), removed = strs(p.removed ?? []);
  if (!created || !removed) return null;
  if (p.form != null && !isObj(p.form)) return null;
  const out: Record<string, unknown> = { init, form: p.form ?? null, created, removed };
  if (typeof p.base === "string") out.base = p.base;
  return out;
}
export function sanitizeAi(p: unknown): Record<string, unknown> | null {
  if (!isObj(p)) return null;
  const c = p.conversationId;
  if (c !== null && c !== undefined && !(typeof c === "string" && CONV_RE.test(c))) return null;
  return { conversationId: (c as string | null | undefined) ?? null };
}
const SANITIZE: Record<DetachKind, (p: unknown) => Record<string, unknown> | null> = { compose: sanitizeCompose, event: sanitizeEvent, aichat: sanitizeAi };

export function makeEnvelope(kind: string, title: string, dirty: boolean, payload: unknown): Envelope | null {
  if (!isDetachable(kind)) return null;
  const p = SANITIZE[kind](payload);
  return p ? { v: 1, kind, title: str(title).slice(0, 200), dirty: dirty === true, payload: p } : null;
}
export function serializeEnvelope(e: Envelope): string { return JSON.stringify(e); }
/** Validate untrusted JSON (string or already parsed). Null for anything inconsistent. */
export function parseEnvelope(input: unknown): Envelope | null {
  let o: unknown = input;
  if (typeof input === "string") {
    if (input.length > MAX_BYTES) return null;
    try { o = JSON.parse(input); } catch { return null; }
  }
  if (!isObj(o) || o.v !== 1 || typeof o.title !== "string" || typeof o.dirty !== "boolean") return null;
  return makeEnvelope(o.kind as string, o.title, o.dirty, o.payload);
}

// ---- drag-out / geometry ----
export const DRAG_OUT_PX = 24;
export const DEFAULT_W = 720, DEFAULT_H = 560, MIN_W = 420, MIN_H = 320, MAX_W = 2400, MAX_H = 1800;
/** Is the pointer beyond the window's client area by more than `threshold` px? Non-finite coordinates are never "outside". */
export function outsideWindow(x: number, y: number, w: number, h: number, threshold = DRAG_OUT_PX): boolean {
  if (![x, y, w, h].every(Number.isFinite)) return false;
  return x < -threshold || y < -threshold || x > w + threshold || y > h + threshold;
}
export interface Geometry { x: number; y: number; w: number; h: number }
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
/** Window rectangle for a pane released at screen point (sx, sy): the pointer lands on the new window's pane header. */
export function detachGeometry(sx: number, sy: number, paneW?: number, paneH?: number): Geometry {
  const w = clamp(Number.isFinite(paneW) ? (paneW as number) : DEFAULT_W, MIN_W, MAX_W);
  const h = clamp(Number.isFinite(paneH) ? (paneH as number) : DEFAULT_H, MIN_H, MAX_H);
  const px = Number.isFinite(sx) ? sx : 0, py = Number.isFinite(sy) ? sy : 0;
  return { x: Math.round(px - Math.min(w / 2, 160)), y: Math.round(py - 42), w: Math.round(w), h: Math.round(h) };
}
/** Keyboard / menu detach: no pointer, so cascade from the source window's origin. */
export const cascadeGeometry = (winX: number, winY: number, paneW?: number, paneH?: number): Geometry =>
  detachGeometry((Number.isFinite(winX) ? winX : 0) + 120 + Math.min((Number.isFinite(paneW) ? (paneW as number) : DEFAULT_W) / 2, 160), (Number.isFinite(winY) ? winY : 0) + 100, paneW, paneH);

// ---- dock spec ----
export type Zone = "left" | "right" | "top" | "bottom" | "center";
export const ZONES: readonly Zone[] = ["left", "right", "top", "bottom", "center"];
export interface Box { l: number; t: number; w: number; h: number }
/** Which docking zone of `r` is under (x, y)? Centre = 50% inner area; otherwise the nearest edge. Null outside / degenerate. */
export function zoneAt(r: Box, x: number, y: number): Zone | null {
  if (![r.l, r.t, r.w, r.h, x, y].every(Number.isFinite) || r.w <= 0 || r.h <= 0) return null;
  if (x < r.l || y < r.t || x >= r.l + r.w || y >= r.t + r.h) return null;
  const rx = (x - r.l) / r.w, ry = (y - r.t) / r.h;
  const m = Math.min(rx, 1 - rx, ry, 1 - ry);
  return m > 0.25 ? "center" : m === rx ? "left" : m === 1 - rx ? "right" : m === ry ? "top" : "bottom";
}
/** Highlight rectangle for a zone. */
export function zoneBox(r: Box, z: Zone): Box {
  const half = z === "left" || z === "right", vert = z === "top" || z === "bottom";
  return { l: r.l + (z === "right" ? r.w / 2 : 0), t: r.t + (z === "bottom" ? r.h / 2 : 0), w: r.w * (half ? 0.5 : 1), h: r.h * (vert ? 0.5 : 1) };
}
export interface PaneBox extends Box { id: string }
/** Dock spec for a point over a set of pane boxes (first hit wins), skipping `exclude`. */
export function dockAt(boxes: readonly PaneBox[], x: number, y: number, exclude?: string): { target: string; zone: Zone } | null {
  for (const b of boxes) {
    if (b.id === exclude) continue;
    const zone = zoneAt(b, x, y);
    if (zone) return { target: b.id, zone };
  }
  return null;
}
/** Screen point -> the receiving window's client coordinates. */
export const screenToClient = (sx: number, sy: number, winScreenX: number, winScreenY: number) => ({ x: sx - winScreenX, y: sy - winScreenY });

// ---- attach message (backend -> main window) ----
export interface AttachMsg {
  paneId: string; envelope: Envelope; park: boolean;
  dock: { target: string; zone: Zone } | null;
  at: { x: number; y: number } | null;
}
export function parseAttach(raw: unknown): AttachMsg | null {
  if (!isObj(raw) || typeof raw.paneId !== "string" || !ID_RE.test(raw.paneId)) return null;
  const envelope = parseEnvelope(raw.envelope);
  if (!envelope) return null;
  let dock: AttachMsg["dock"] = null;
  if (raw.dock != null) {
    const d = raw.dock;
    if (!isObj(d) || typeof d.target !== "string" || !ID_RE.test(d.target) || !ZONES.includes(d.zone as Zone)) return null;
    dock = { target: d.target, zone: d.zone as Zone };
  }
  let at: AttachMsg["at"] = null;
  if (raw.at != null) {
    const a = raw.at;
    if (!isObj(a) || typeof a.x !== "number" || typeof a.y !== "number" || !Number.isFinite(a.x) || !Number.isFinite(a.y)) return null;
    at = { x: a.x, y: a.y };
  }
  return { paneId: raw.paneId, envelope, park: raw.park === true, dock, at };
}

// ---- persisted multi-window layout (what list_windows reports / relaunch restores) ----
export interface WinRec { label: string; paneId: string | null; kind: DetachKind | null; focused: boolean; geo: Geometry | null }
const MAX_WINDOWS = 32;
const SANE = 100000;
/** Validate the window list. Returns only well-formed records; hostile / corrupt input yields []. Duplicate labels and panes are dropped. */
export function parseWindows(input: unknown): WinRec[] {
  let o: unknown = input;
  if (typeof input === "string") {
    if (input.length > 256 * 1024) return [];
    try { o = JSON.parse(input); } catch { return []; }
  }
  if (!Array.isArray(o)) return [];
  const out: WinRec[] = [], labels = new Set<string>(), panes = new Set<string>();
  for (const w of o.slice(0, MAX_WINDOWS * 2)) {
    if (out.length >= MAX_WINDOWS) break;
    if (!isObj(w) || !validLabel(w.label) || labels.has(w.label)) continue;
    let paneId: string | null = null, kind: DetachKind | null = null;
    if (w.label !== MAIN_LABEL) {
      if (typeof w.paneId !== "string" || !ID_RE.test(w.paneId) || labelFor(w.paneId) !== w.label || panes.has(w.paneId) || !isDetachable(w.kind)) continue;
      paneId = w.paneId; kind = w.kind;
    }
    let geo: Geometry | null = null;
    if (isObj(w) && w.x != null) {
      const g = { x: w.x, y: w.y, w: w.w, h: w.h } as Record<string, unknown>;
      if (Object.values(g).every((n) => typeof n === "number" && Number.isFinite(n) && Math.abs(n as number) <= SANE) && (g.w as number) > 0 && (g.h as number) > 0) geo = g as unknown as Geometry;
    }
    labels.add(w.label);
    if (paneId) panes.add(paneId);
    out.push({ label: w.label, paneId, kind, focused: w.focused === true, geo });
  }
  return out;
}
/** Pane ids owned by detached windows (main must not show their drafts in its tray). */
export const detachedIds = (wins: readonly WinRec[]): string[] => wins.flatMap((w) => (w.paneId ? [w.paneId] : []));

// ---- moving between windows: when is it not allowed? ----
export interface MoveState {
  /** Running in the desktop app (not the browser mock). */
  desktop: boolean;
  kind: string | undefined;
  /** Files the user added to a Compose pane. They live only in that webview and cannot cross windows. */
  attachments: number;
  /** A send is in flight, or a dialog about this pane (e.g. "Send without a subject?") is open. */
  holding: boolean;
  /** A confirm dialog is open anywhere in this window. */
  dialogOpen: boolean;
  /** `to-window` (detach) or `to-main` (attach / park). */
  dir: "to-window" | "to-main";
}
/** Why a pane can't move now (null = it can). Pure: the same rule drives the keyboard, menu, button and drag paths. */
export function moveBlock(s: MoveState): string | null {
  if (!s.desktop) return "Moving a pane to its own window needs the desktop app.";
  if (s.dir === "to-window" && (!s.kind || !isDetachable(s.kind))) return "Mailbox, reader and calendar share this window's mail state, so they stay in the main window.";
  if (s.holding) return "This message is being sent. Try again in a moment.";
  if (s.dialogOpen) return "Answer the open question first.";
  if (s.attachments > 0) return "Attached files can't move between windows yet. Remove them, or send the message first.";
  return null;
}
