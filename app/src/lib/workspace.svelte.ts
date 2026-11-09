// Workspace store: reactive wrapper around the pure tree in wstree.ts, plus draft autosave, layout persistence and shortcuts.
// Pane payloads live in a plain Map (not reactive) so typing never re-renders the tree; only title / dirty flow into `ws`.
import { api, isTauri, onWsEvent } from "./api";
import * as D from "./detach";
import { app } from "./state.svelte";
import { layout as widths } from "./layout.svelte";
import * as T from "./wstree";
import type { AttachmentData, Outgoing } from "./types";

export interface ComposePayload {
  account: string | null; to: string; cc: string; bcc: string; subject: string; text: string;
  inReplyTo: Outgoing["inReplyTo"]; showCc: boolean;
  /** Written by the AI assistant: Compose shows a "Drafted by AI" banner until the message is sent or discarded. */
  aiDraft?: boolean;
  /** Id of the AI draft this pane was opened from (so opening it again focuses this pane), and the app's notes on it. */
  aiDraftId?: number;
  aiWarnings?: string[];
  /** Manual reply whose Reply-To differs from From: Compose shows it as a visible notice (phishing guard). */
  replyNotice?: string;
  /** Composer editor state. `text` always holds the plain rendering; `html` is the rich body when mode is "rich". */
  html?: string;
  mode?: "plain" | "rich";
  /** Rich-mode HTML to start from (replies / forwards keep the original's formatting); Compose consumes it once. */
  seedHtml?: string;
  /** "reply" | "new": which signature default applies. Signature id currently in the body ("" = none). */
  kind?: "new" | "reply";
  sigId?: string;
  sigApplied?: boolean;
  /** Sender address chosen in Compose (account address or a saved identity of it). */
  from?: string;
  /** Forward: re-attach the original's files (inline) or the whole message (as .eml); fetched by Compose, never stored. */
  fwd?: { account: string; folder: string; uid: number; uidvalidity?: number; as: "inline" | "attach"; atts: { id: string; name: string; mime: string; size: number }[] };
  /** JSON of the initial [to, cc, subject, text]: a pane is dirty only when it differs. */
  base: string;
}
export interface EventInit { uid?: string; calendarId?: string; start?: number; end?: number; allDay?: boolean }
export interface EventPayload { init: EventInit; form: Record<string, unknown> | null; created: string[]; removed: string[]; base?: string }
export type CloseChoice = "park" | "discard" | "cancel";
export interface DragState { id: string; x: number; y: number; target: { id: string; zone: T.Zone } | null; box: { l: number; t: number; w: number; h: number } | null; /** pointer is outside the window: release detaches (main) or tries the main window (detached) */ out: boolean }

/** Files handed to a Compose pane that Undo / Edit re-opened (in memory only: never part of the autosaved draft). */
const restoredAtts = new Map<string, AttachmentData[]>();
export const takeRestoredAtts = (id: string): AttachmentData[] => { const a = restoredAtts.get(id) ?? []; restoredAtts.delete(id); return a; };

/** In-memory companions of a Compose pane: files (never part of the saved draft) and a rich body too big for the draft copy. */
export interface ComposeMem { atts: { name: string; mime: string; size: number; dataBase64: string; src?: string }[]; html?: string }
const composeMem = new Map<string, ComposeMem>();
export const getComposeMem = (id: string): ComposeMem | null => composeMem.get(id) ?? null;
export function setComposeMem(id: string, v: ComposeMem | null) { if (v) composeMem.set(id, v); else composeMem.delete(id); }
const memAtts = (id: string) => (composeMem.get(id)?.atts.length ?? 0) > 0;

const AUTOSAVE_MS = 1000, LAYOUT_MS = 600, NARROW = 900;
const newId = (p: string) => `${p}-${crypto.randomUUID().replace(/-/g, "").slice(0, 10)}`;
const fresh = (ratio?: number) => T.defaultWs(ratio ?? defaultRatio());
function defaultRatio() {
  const w = (typeof window === "undefined" ? 1400 : window.innerWidth) - widths.sidebar;
  return w > 0 ? Math.min(0.7, Math.max(0.25, widths.list / w)) : 0.4;
}
const isDraft = (k: T.PaneKind) => T.isDraftKind(k);
export type View = "tiles" | "tabs";
const VIEW_KEY = "miramail.ws.view";
function loadView(): View { try { return localStorage.getItem(VIEW_KEY) === "tabs" ? "tabs" : "tiles"; } catch { return "tiles"; } }

class Workspace {
  ws = $state.raw<T.Ws>(fresh());
  ready = $state(false);
  closeReq = $state<{ id: string; resolve: (c: CloseChoice) => void } | null>(null);
  drag = $state.raw<DragState | null>(null);
  hostW = $state(1200);
  /** Layout: side-by-side tiles or one browser-style tab at a time. Same panes either way (a view of the same tree). */
  view = $state<View>(loadView());
  /** "main": the full app. "pane": a detached window holding exactly one pane (see initPane). */
  mode = $state<"main" | "pane">("main");
  paneId: string | null = null;
  /** Pane ids that currently live in their own windows (main window view). */
  detached = $state<string[]>([]);
  readonly canDetach = isTauri;
  private busy = new Set<string>();
  /** Compose panes mid-send or mid-dialog (ids), and how many user-added files each holds: neither may cross windows. */
  private holds = new Set<string>();
  private attCounts = new Map<string, number>();
  /** Attach messages that arrived before init() finished (receive would be overwritten by init's layout). */
  private pendingAttach: unknown[] = [];
  private lastMove = 0;
  private payloads = new Map<string, unknown>();
  private timers = new Map<string, ReturnType<typeof setTimeout>>();
  private saved = new Set<string>();
  private layoutTimer: ReturnType<typeof setTimeout> | undefined;
  private lastClose = 0;

  /** Tabs layout is on (a detached window always shows its single pane as a tile). */
  get tabsOn(): boolean { return this.view === "tabs" && this.mode !== "pane"; }
  setView(v: View) {
    if (v !== "tiles" && v !== "tabs") return;
    this.view = v;
    try { localStorage.setItem(VIEW_KEY, v); } catch { /* ignore */ }
  }
  toggleView() { this.setView(this.view === "tabs" ? "tiles" : "tabs"); }
  /** Tabs: activate a tab group (focus moves into it). */
  selectGroup(key: string) {
    const g = T.tabGroups(this.ws).find((x) => x.key === key);
    if (g && T.activeGroupKey(this.ws) !== key) this.focus(T.groupFocusId(g));
  }
  /** Set by the platform layer: true while a popover, modal or the shortcut sheet owns the keyboard (kept out of here to avoid import cycles). */
  keyBlock: (() => boolean) | null = null;
  /** Ask the tab strip to move keyboard focus to the active tab. */
  tabFocus() { if (typeof window !== "undefined") window.dispatchEvent(new Event("miramail:tab-focus")); }
  /** Tabs: next / previous tab (wraps). */
  stepTab(delta: 1 | -1) {
    const k = T.stepGroup(T.tabGroups(this.ws), T.activeGroupKey(this.ws), delta);
    if (k) this.selectGroup(k);
  }
  /** Tabs: Cmd+1..8 = Nth tab, Cmd+9 = last. */
  jumpTab(n: number) {
    const g = T.tabGroups(this.ws);
    const t = n >= 9 ? g[g.length - 1] : g[n - 1];
    if (t) this.selectGroup(t.key);
  }

  get focused(): T.Pane | null { return this.ws.focus ? this.ws.panes[this.ws.focus] ?? null : null; }
  /** Draft-kind panes (open or parked) plus any parked pane: what the sidebar tray lists. */
  get tray() {
    return Object.values(this.ws.panes)
      .filter((p) => this.ws.parked.includes(p.id) || (isDraft(p.kind) && T.findLeaf(this.ws.root, p.id)))
      .map((p) => ({ id: p.id, kind: p.kind, title: p.title, dirty: p.dirty, parked: this.ws.parked.includes(p.id) }));
  }

  // ---- lifecycle ----
  async init() {
    let ws: T.Ws | null = null;
    try { ws = T.parse(await api.loadLayout()); } catch { /* fall back to the default */ }
    ws ??= fresh();
    // Panes living in detached windows are owned there: never also show them here (layout rows or drafts).
    let away: string[] = [];
    try { away = D.detachedIds(D.parseWindows(await api.listWindows())); } catch { /* browser mock: none */ }
    this.detached = away;
    for (const id of away) ws = T.closePane(ws, id);
    try {
      const rows = (await api.listDrafts()).filter((d) => !away.includes(d.id));
      for (const d of rows) this.payloads.set(d.id, d.payload);
      ws = T.hydrateDrafts(ws, rows.map((d) => ({ id: d.id, kind: d.kind, title: d.title, payload: d.payload })));
      for (const d of rows) this.saved.add(d.id);
    } catch { /* drafts unavailable: draft panes that had no payload are dropped below */
      for (const p of Object.values(ws.panes)) if (isDraft(p.kind)) ws = T.closePane(ws, p.id);
    }
    this.ws = ws;
    const fk = ws.focus ? ws.panes[ws.focus]?.kind : null;
    if (fk === "calendar") app.view = "calendar";
    app.onAccountRemoved = (email) => this.dropAccount(email);
    this.ready = true;
    this.touchLayout();
    const flush = () => this.flushAll();
    window.addEventListener("beforeunload", flush);
    document.addEventListener("visibilitychange", () => { if (document.visibilityState === "hidden") flush(); });
    const queued = this.pendingAttach.splice(0);
    for (const raw of queued) await this.receive(raw);
  }

  /** Detached window entry: load the one pane this window owns from the backend handoff row. */
  async initPane(id: string): Promise<boolean> {
    this.mode = "pane";
    this.paneId = id;
    let env: D.Envelope | null = null;
    try { env = D.parseEnvelope(await api.getPane()); } catch { /* handled below */ }
    if (!env) { app.notify("This window's pane could not be restored.", "error"); return false; }
    this.payloads.set(id, env.payload);
    this.ws = T.openPane({ root: null, panes: {}, parked: [], focus: null, zoom: null }, { id, kind: env.kind, title: env.title, dirty: env.dirty, payload: null });
    if (env.dirty && isDraft(env.kind)) this.saved.add(id);
    this.ready = true;
    this.bindFlush();
    return true;
  }
  private bindFlush() {
    const flush = () => this.flushAll();
    window.addEventListener("beforeunload", flush);
    document.addEventListener("visibilitychange", () => { if (document.visibilityState === "hidden") flush(); });
  }

  private set(next: T.Ws) {
    if (next === this.ws) return;
    this.ws = next;
    if (this.mode === "pane") { if (this.ready && !Object.keys(next.panes).length) void api.closePaneWindow().catch(() => {}); } // last pane gone: the window goes too
    else this.touchLayout();
  }
  private touchLayout() {
    if (!this.ready || this.mode === "pane") return;
    clearTimeout(this.layoutTimer);
    this.layoutTimer = setTimeout(() => { void api.saveLayout(T.serialize(this.ws)).catch(() => {}); }, LAYOUT_MS);
  }
  /** Write pending layout + drafts now (window hidden / closing). */
  flushAll() {
    if (this.mode === "pane") { if (this.paneId) void this.savePaneRow(this.paneId); }
    else if (this.layoutTimer) { clearTimeout(this.layoutTimer); this.layoutTimer = undefined; void api.saveLayout(T.serialize(this.ws)).catch(() => {}); }
    for (const id of [...this.timers.keys()]) this.flushDraft(id);
  }

  // ---- payloads + drafts ----
  getPayload<P>(id: string): P | null { return (this.payloads.get(id) ?? this.ws.panes[id]?.payload ?? null) as P | null; }
  /** Called by pane components on every edit. Cheap: only title/dirty changes touch reactive state. */
  setPayload(id: string, payload: unknown, meta: { title: string; dirty: boolean }) {
    const p = this.ws.panes[id];
    if (!p) return;
    this.payloads.set(id, payload);
    if (p.title !== meta.title || p.dirty !== meta.dirty) this.set(T.patchPane(this.ws, id, { title: meta.title, dirty: meta.dirty }));
    clearTimeout(this.timers.get(id));
    this.timers.set(id, setTimeout(() => this.flushDraft(id), AUTOSAVE_MS));
  }
  /** Detached window: keep the backend handoff row current (relaunch restore, attach). */
  private async savePaneRow(id: string) {
    const p = this.ws.panes[id];
    if (!p || id !== this.paneId) return;
    const env = D.makeEnvelope(p.kind, p.title, p.dirty, this.payloads.get(id) ?? null);
    if (env) { try { await api.savePane(D.serializeEnvelope(env)); } catch { /* window may be closing */ } }
  }
  private flushDraft(id: string) {
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    if (this.mode === "pane") void this.savePaneRow(id);
    const p = this.ws.panes[id];
    if (!p || !isDraft(p.kind)) return;
    const payload = this.payloads.get(id);
    if (!(p.dirty || memAtts(id)) || payload === undefined) { // files held only in memory still count as unsent work
      if (this.saved.delete(id)) void api.deleteDraft(id).catch(() => {});
      return;
    }
    const account = p.kind === "compose" ? (payload as ComposePayload).account ?? null : null;
    this.saved.add(id);
    void api.saveDraft({ id, kind: p.kind as "compose" | "event", account, title: p.title, payload, updatedAt: 0 }).catch((e) => app.fail(e));
  }
  private forget(id: string) {
    this.holds.delete(id);
    this.attCounts.delete(id);
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    this.payloads.delete(id);
    composeMem.delete(id);
    // Always try the delete when the pane could have a draft: a save may be in flight.
    if (this.saved.delete(id) || isDraft(this.ws.panes[id]?.kind ?? "mailbox")) void api.deleteDraft(id).catch(() => {});
    // Sent or discarded: the copy mirrored to the server's Drafts folder goes too (the backend does nothing if there is none).
    if (this.ws.panes[id]?.kind === "compose") void api.draftMirrorRemove(id).catch(() => {});
  }

  // ---- opening panes ----
  private zoneForNew(): T.Zone { return this.hostW < NARROW ? "center" : "right"; }
  openCompose(init: { to: string[]; cc: string[]; bcc: string[]; subject: string; text: string; inReplyTo: Outgoing["inReplyTo"] }, account: string | null = app.account, opts: { aiDraft?: boolean; aiDraftId?: number; aiWarnings?: string[]; extra?: Partial<ComposePayload> } = {}) {
    const id = newId("c");
    const payload: ComposePayload = {
      account, to: init.to.join(", "), cc: init.cc.join(", "), bcc: init.bcc.join(", "), subject: init.subject, text: init.text,
      inReplyTo: init.inReplyTo, showCc: init.cc.length > 0,
      // An AI draft counts as unsaved work from the start (so it is autosaved and closing it asks first).
      base: opts.aiDraft ? JSON.stringify([[], [], "", ""]) : JSON.stringify([init.to, init.cc, init.subject, init.text]),
      ...(opts.aiDraft ? { aiDraft: true, ...(opts.aiDraftId != null ? { aiDraftId: opts.aiDraftId } : {}), ...(opts.aiWarnings?.length ? { aiWarnings: opts.aiWarnings } : {}) } : {}),
      ...(opts.extra ?? {}),
    };
    return this.openNew(id, "compose", init.subject || "New message", payload);
  }
  /** Undo / Edit from the outbox: a Compose pane with the message exactly as it was, files included. */
  openRestoredCompose(payload: ComposePayload, atts: AttachmentData[]): string {
    // A detached window would open this in yet another window, and files can't make that trip: refuse (the Outbox keeps the entry).
    if (this.mode === "pane" && atts.length) throw new Error("Open the main window to edit this message: it has attached files.");
    const id = newId("c");
    if (atts.length) restoredAtts.set(id, atts);
    return this.openNew(id, "compose", payload.subject.trim() || "New message", payload);
  }
  /** New compose / event pane. A detached window holds exactly one pane, so there it opens in a window of its own. */
  private openNew(id: string, kind: "compose" | "event", title: string, payload: unknown): string {
    if (this.mode === "pane") {
      const env = D.makeEnvelope(kind, title, false, payload);
      const g = D.cascadeGeometry(window.screenX, window.screenY);
      if (env) void api.detachPane(id, D.serializeEnvelope(env), g.x, g.y, g.w, g.h).catch((e) => app.fail(e));
      return id;
    }
    this.payloads.set(id, payload);
    this.set(T.openPane(this.ws, { id, kind, title, dirty: false, payload: null }, { zone: this.zoneForNew() }));
    return id;
  }
  /** The open (or parked) Compose pane that was made from AI draft `draftId`, if any. */
  findAiDraftPane(draftId: number): string | null {
    for (const id of Object.keys(this.ws.panes)) {
      if (this.ws.panes[id].kind === "compose" && this.getPayload<ComposePayload>(id)?.aiDraftId === draftId) return id;
    }
    return null;
  }
  /** Bring a pane to the front, un-parking it first. */
  reveal(id: string) {
    if (this.ws.parked.includes(id)) this.unpark(id);
    else this.focus(id);
  }
  openEvent(init: EventInit) {
    const id = newId("e");
    const payload: EventPayload = { init, form: null, created: [], removed: [] };
    return this.openNew(id, "event", init.uid ? "Edit event" : "New event", payload);
  }
  /** Singleton panes (mailbox, reader, calendar): focus if in the tree, restore if parked, else open. */
  ensure(kind: "mailbox" | "reader" | "calendar", opts: { zoom?: boolean } = {}) {
    const id = kind;
    let ws = this.ws;
    if (ws.parked.includes(id)) ws = T.unparkPane(ws, id);
    else if (!ws.panes[id]) {
      const title = kind === "calendar" ? "Calendar" : kind === "mailbox" ? "Messages" : "Reader";
      ws = T.openPane(ws, { id, kind, title, dirty: false, payload: null }, { near: kind === "reader" ? ws.panes.mailbox ? "mailbox" : null : null, zone: kind === "mailbox" ? "left" : "right" });
    }
    ws = T.focusPane(ws, id);
    if (opts.zoom && !this.tabsOn && ws.zoom !== id) ws = T.toggleZoom(ws, id);
    this.set(ws);
  }
  /** Cmd+J: open the AI chat beside the focused pane; focus it if open elsewhere; close it when it already has focus. */
  toggleAi(opts: { openOnly?: boolean } = {}) {
    const id = "aichat";
    if (this.mode === "pane") return;
    if (this.detached.includes(id)) { void api.focusWindow(D.labelFor(id)).catch((e) => app.fail(e)); return; }
    let ws = this.ws;
    if (ws.focus === id && !ws.parked.includes(id)) { if (!opts.openOnly) this.close(id); return; }
    if (ws.parked.includes(id)) ws = T.unparkPane(ws, id, { zone: this.zoneForNew() });
    else if (!ws.panes[id]) ws = T.openPane(ws, { id, kind: "aichat", title: "MIRA", dirty: false, payload: null }, { zone: this.zoneForNew() });
    else ws = T.focusPane(ws, id);
    this.set(ws);
    queueMicrotask(() => window.dispatchEvent(new Event("miramail:ai-focus")));
    setTimeout(() => window.dispatchEvent(new Event("miramail:ai-focus")), 120); // the pane may not have mounted yet
  }
  /** Sidebar Mail / Calendar switch. Idempotent. */
  showView(v: "mail" | "calendar") {
    const had = !!this.ws.panes.calendar && !this.ws.parked.includes("calendar");
    if (v === "calendar") this.ensure("calendar", { zoom: !had });
    else {
      let ws = this.ws;
      if (ws.zoom === "calendar") ws = { ...ws, zoom: null };
      this.set(ws);
      this.ensure("mailbox");
      if (!this.ws.panes.reader) this.ensure("reader");
    }
  }

  // ---- closing / parking ----
  /** Close with a prompt when the pane holds unsent work. */
  async requestClose(id: string | null = this.ws.focus) {
    const p = id ? this.ws.panes[id] : null;
    if (!p || !id) return;
    if (isDraft(p.kind) && (p.dirty || memAtts(id))) {
      if (this.closeReq) return;
      const c = await new Promise<CloseChoice>((resolve) => { this.closeReq = { id, resolve }; });
      this.closeReq = null;
      if (c === "cancel") return;
      if (c === "park") { this.park(id); return; }
    }
    this.close(id);
  }
  answerClose(c: CloseChoice) { this.closeReq?.resolve(c); }
  /** Discard: remove for good, delete its draft, release Zoom meetings an unsaved event created. */
  close(id: string) {
    const p = this.ws.panes[id];
    if (!p) return;
    if (p.kind === "event") for (const m of this.getPayload<EventPayload>(id)?.created ?? []) void api.zoomDeleteMeeting(m).catch(() => {});
    this.finish(id);
  }
  /** After a successful send / save: remove and delete the draft, no side effects. */
  finish(id: string) {
    if (!this.ws.panes[id]) return;
    this.forget(id);
    this.set(T.closePane(this.ws, id));
  }
  closeFocused() {
    // Tabs: the Mail tab is pinned, Cmd+W must not close the message list or reader.
    if (this.tabsOn && T.activeGroupKey(this.ws) === T.MAIL_KEY) return;
    const t = Date.now();
    if (t - this.lastClose < 150) return; // native menu + keydown can both report one Cmd+W
    this.lastClose = t;
    void this.requestClose().then(() => { if (this.tabsOn) this.tabFocus(); });
  }
  park(id: string) {
    if (this.mode === "pane") { void this.attachToMain({ park: true }); return; } // parking = back to the main window's tray
    const p = this.ws.panes[id];
    if (!p || !T.findLeaf(this.ws.root, id)) return;
    if (isDraft(p.kind)) this.flushDraft(id);
    this.set(T.parkPane(this.ws, id));
  }
  unpark(id: string) { this.set(T.unparkPane(this.ws, id, { zone: this.zoneForNew() })); }
  dropAccount(email: string) {
    for (const p of Object.values(this.ws.panes)) {
      if (p.kind === "compose" && (this.getPayload<ComposePayload>(p.id)?.account ?? null) === email) this.close(p.id);
    }
  }

  // ---- focus / layout ops ----
  focus(id: string) { this.set(T.focusPane(this.ws, id)); }
  /** Cmd+D / Cmd+Shift+D: a fresh compose pane beside / below the focused one. */
  split(dir: T.Dir) {
    if (!app.account) return;
    if (this.mode === "pane") { this.newCompose(); return; }
    const id = newId("c");
    const payload: ComposePayload = { account: app.account, to: "", cc: "", bcc: "", subject: "", text: "", inReplyTo: null, showCc: false, base: JSON.stringify([[], [], "", ""]) };
    this.payloads.set(id, payload);
    this.set(T.openPane(this.ws, { id, kind: "compose", title: "New message", dirty: false, payload: null }, { zone: dir === "row" ? "right" : "bottom" }));
  }
  step(d: 1 | -1) { this.set(T.focusStep(this.ws, d)); }
  arrow(dir: T.Arrow) { this.set(T.focusDir(this.ws, dir, this.area())); }
  toggleZoom(id?: string) { if (this.tabsOn) return; this.set(T.toggleZoom(this.ws, id ?? this.ws.focus)); }
  setRatio(path: string, ratio: number, size: number) { this.set(T.setRatioAt(this.ws, path, ratio, size)); }
  resetRatio(path: string, size: number) { this.set(T.resetRatioAt(this.ws, path, size)); }
  dock(id: string, target: string, zone: T.Zone) { this.set(T.dockPane(this.ws, id, target, zone)); }
  resetLayout() {
    let ws = fresh();
    for (const p of Object.values(this.ws.panes)) {
      if (isDraft(p.kind)) ws = { ...ws, panes: { ...ws.panes, [p.id]: p }, parked: [...ws.parked, p.id] };
    }
    this.set(ws);
  }
  private area(): T.Rect {
    const el = typeof document === "undefined" ? null : document.querySelector(".ws-root");
    const r = el?.getBoundingClientRect();
    return r && r.width > 0 ? { x: 0, y: 0, w: r.width, h: r.height } : { x: 0, y: 0, w: 1000, h: 1000 };
  }

  // ---- header drag to dock ----
  dragStart(id: string, x: number, y: number) { this.drag = { id, x, y, target: null, box: null, out: false }; document.body.classList.add("pane-dragging"); }
  dragMove(x: number, y: number) {
    const d = this.drag;
    if (!d) return;
    let target: DragState["target"] = null, box: DragState["box"] = null;
    const kind = this.ws.panes[d.id]?.kind;
    // Pointer beyond the window: release detaches (main) / tries to attach (detached window). Never for mailbox / reader / calendar.
    const out = !!kind && D.isDetachable(kind) && (this.mode === "pane" || this.canDetach) && D.outsideWindow(x, y, window.innerWidth, window.innerHeight, this.mode === "pane" ? 0 : D.DRAG_OUT_PX);
    if (out) { this.drag = { ...d, x, y, target: null, box: null, out: true }; return; }
    if (this.mode === "pane" || this.tabsOn) { this.drag = { ...d, x, y, target: null, box: null, out: false }; return; } // no dock zones: a drop inside just cancels
    for (const el of document.elementsFromPoint(x, y)) {
      const host = (el as HTMLElement).closest?.("[data-pane-id]") as HTMLElement | null;
      if (!host) continue;
      const tid = host.dataset.paneId!;
      if (tid === d.id || !this.ws.panes[tid]) break;
      const r = host.getBoundingClientRect();
      const rect = { l: r.left, t: r.top, w: r.width, h: r.height };
      const zone = D.zoneAt(rect, x, y) ?? "center";
      target = { id: tid, zone };
      box = D.zoneBox(rect, zone);
      break;
    }
    this.drag = { ...d, x, y, target, box, out: false };
  }
  /** `screen`: the release point in screen coordinates (needed to place a detached window / find the window under the pointer). */
  dragEnd(commit: boolean, screen?: { x: number; y: number }) {
    const d = this.drag;
    this.drag = null;
    document.body.classList.remove("pane-dragging");
    if (!commit || !d) return;
    if (d.out) {
      if (!screen) return;
      if (this.mode === "pane") void this.dropOnWindow(screen);
      else { const r = this.paneRect(d.id); void this.detach(d.id, D.detachGeometry(screen.x, screen.y, r?.width, r?.height)); }
      return;
    }
    if (d.target) this.dock(d.id, d.target.id, d.target.zone);
  }

  // ---- detach / attach (docs/WORKSPACE_SPEC.md) ----
  private paneRect(id: string): DOMRect | null { return typeof document === "undefined" ? null : document.querySelector(`[data-pane-id="${id}"]`)?.getBoundingClientRect() ?? null; }
  /** Compose reports a send / dialog in progress (blocks moving the pane) and its user-added file count. */
  setHold(id: string, on: boolean) { if (on) this.holds.add(id); else this.holds.delete(id); }
  setAttachmentCount(id: string, n: number) { if (n > 0) this.attCounts.set(id, n); else this.attCounts.delete(id); }
  /** True while a detach / attach of this pane is under way. */
  moving(id: string): boolean { return this.busy.has(id); }
  /** Why a pane can't go to its own window (null = it can). `id` adds the per-pane checks (send in flight, files). */
  detachBlock(kind: T.PaneKind | undefined, id?: string): string | null {
    return D.moveBlock({ desktop: this.canDetach, kind, attachments: id ? this.attCounts.get(id) ?? 0 : 0, holding: id ? this.holds.has(id) : false, dialogOpen: !!app.confirmReq || !!this.closeReq, dir: "to-window" });
  }
  /** Why a detached window's pane can't go back to the main window (null = it can). */
  attachBlock(id: string): string | null {
    return D.moveBlock({ desktop: this.canDetach, kind: this.ws.panes[id]?.kind, attachments: this.attCounts.get(id) ?? 0, holding: this.holds.has(id), dialogOpen: !!app.confirmReq, dir: "to-main" });
  }
  /** Move a pane into its own window (single owner: it leaves this window's tree only after the backend created the new one). */
  async detach(id: string, geo?: D.Geometry): Promise<boolean> {
    const p = this.ws.panes[id];
    if (this.mode !== "main" || !p) return false;
    const why = this.detachBlock(p.kind, id);
    if (why) { app.notify(why); return false; }
    if (this.busy.has(id)) return false;
    this.busy.add(id);
    try {
      if (isDraft(p.kind)) this.flushDraft(id); // the shared drafts store has the latest text before ownership changes
      const env = D.makeEnvelope(p.kind, p.title, p.dirty, this.getPayload(id));
      if (!env) { app.notify("This pane can't be moved to its own window."); return false; }
      const r = this.paneRect(id);
      const g = geo ?? D.cascadeGeometry(window.screenX, window.screenY, r?.width, r?.height);
      await api.detachPane(id, D.serializeEnvelope(env), g.x, g.y, g.w, g.h);
      this.detached = [...this.detached.filter((x) => x !== id), id];
      this.release(id);
      return true;
    } catch (e) { app.fail(e); return false; }
    finally { this.busy.delete(id); }
  }
  /** Hand a pane over without touching its draft (unlike `forget`, which deletes it). */
  private release(id: string) {
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    this.payloads.delete(id);
    composeMem.delete(id);
    this.saved.delete(id);
    this.set(T.closePane(this.ws, id));
  }
  /** Detached window -> main window (menu, shortcut, or park). */
  async attachToMain(o: { at?: { x: number; y: number }; park?: boolean } = {}) {
    const id = this.paneId;
    if (this.mode !== "pane" || !id || !this.ws.panes[id] || this.busy.has(id)) return;
    const why = this.attachBlock(id);
    if (why) { app.notify(why); return; }
    this.busy.add(id);
    try {
      const p = this.ws.panes[id];
      if (isDraft(p.kind)) this.flushDraft(id);
      await this.savePaneRow(id); // the main window reads this row
      await api.attachPane(id, D.MAIN_LABEL, { at: o.at, park: o.park });
    } catch (e) { app.fail(e); }
    finally { this.busy.delete(id); }
  }
  /** Released outside this detached window: if the main window is under the pointer, attach there. */
  private async dropOnWindow(screen: { x: number; y: number }) {
    try {
      const label = await api.windowAt(screen.x, screen.y);
      if (label === D.MAIN_LABEL) await this.attachToMain({ at: screen });
      else app.notify("Drop the pane on the main window to attach it.");
    } catch (e) { app.fail(e); }
  }
  /** Main window: a pane arrives from a detached window. */
  async receive(raw: unknown) {
    const m = D.parseAttach(raw);
    if (!m || this.mode !== "main") return;
    // Before init() finished, `this.ws` is about to be replaced: hold the message and replay it once the layout is in place.
    if (!this.ready) { if (this.pendingAttach.length < 32) this.pendingAttach.push(raw); return; }
    const { paneId: id, envelope: e } = m;
    this.detached = this.detached.filter((x) => x !== id);
    if (this.ws.panes[id]) { this.set(T.focusPane(this.ws, id)); void api.releasePane(id).catch(() => {}); return; } // singleton (AI chat) reopened meanwhile: keep that one
    this.payloads.set(id, e.payload);
    const pane: T.Pane = { id, kind: e.kind, title: e.title, dirty: e.dirty, payload: null };
    let ws = this.ws;
    if (m.park) ws = { ...ws, panes: { ...ws.panes, [id]: pane }, parked: [...ws.parked, id] };
    else {
      const dock = m.dock && T.findLeaf(ws.root, m.dock.target) ? m.dock : this.dockFromScreen(m.at);
      ws = T.openPane(ws, pane, dock ? { near: dock.target, zone: dock.zone } : { zone: this.zoneForNew() });
    }
    if (e.dirty && isDraft(e.kind)) this.saved.add(id);
    this.set(ws);
    // The backend row is the only other copy: drop it only once the layout holding the pane is on disk.
    try { await this.saveLayoutNow(); await api.releasePane(id); } catch { /* the row is transient; the next launch clears it, the pane is already in the layout/draft store */ }
  }
  private async saveLayoutNow() {
    clearTimeout(this.layoutTimer);
    this.layoutTimer = undefined;
    await api.saveLayout(T.serialize(this.ws));
  }
  private dockFromScreen(at: { x: number; y: number } | null) {
    if (!at || typeof document === "undefined") return null;
    const c = D.screenToClient(at.x, at.y, window.screenX, window.screenY);
    const boxes: D.PaneBox[] = [];
    document.querySelectorAll<HTMLElement>("[data-pane-id]").forEach((el) => {
      const r = el.getBoundingClientRect();
      boxes.push({ id: el.dataset.paneId!, l: r.left, t: r.top, w: r.width, h: r.height });
    });
    return D.dockAt(boxes, c.x, c.y);
  }
  detachFocused() {
    const t = Date.now();
    if (t - this.lastMove < 150) return; // native menu + keydown can both report one shortcut
    this.lastMove = t;
    if (this.mode === "pane") { app.notify("This pane is already in its own window. Use Move Pane to Main Window to put it back."); return; }
    if (this.ws.focus) void this.detach(this.ws.focus);
  }
  attachFocused() {
    const t = Date.now();
    if (t - this.lastMove < 150) return;
    this.lastMove = t;
    if (this.mode === "pane") void this.attachToMain();
    else app.notify("This pane is already in the main window.");
  }
  /** Red button on a detached window: ask about unsent work first, the window closes itself once its pane is gone. */
  async requestWindowClose() { if (this.paneId) await this.requestClose(this.paneId); }
  /** Subscribe to menu / backend window events. Returns an unsubscribe function. */
  async bindNative(): Promise<() => void> {
    const offs = await Promise.all([
      onWsEvent("ws://detach-pane", () => this.detachFocused()),
      onWsEvent("ws://attach-pane", () => this.attachFocused()),
      onWsEvent("pane://attach", (p) => { void this.receive(p); }),
      onWsEvent("pane://detached", (p) => { const id = (p as { paneId?: unknown } | null)?.paneId; if (typeof id === "string" && D.ID_RE.test(id) && !this.detached.includes(id)) this.detached = [...this.detached, id]; }),
      onWsEvent("pane://closed", (p) => { const id = (p as { paneId?: unknown } | null)?.paneId; if (typeof id === "string") this.detached = this.detached.filter((x) => x !== id); }),
      onWsEvent("pane://flush", () => this.flushAll()),
      onWsEvent("pane://close-request", () => { void this.requestWindowClose(); }),
    ]);
    return () => offs.forEach((f) => f());
  }

  // ---- shortcuts ----
  /** Returns true when the event was a workspace shortcut (and has been consumed). */
  key(e: KeyboardEvent): boolean {
    // Ctrl+Tab / Ctrl+Shift+Tab: next / previous tab (before the Ctrl guard below).
    if (e.ctrlKey && !e.metaKey && !e.altKey && e.key === "Tab") {
      if (!this.tabsOn || !app.account || app.confirmReq || this.closeReq || app.showSettings || this.keyBlock?.()) return false;
      this.stepTab(e.shiftKey ? -1 : 1);
      this.tabFocus();
      e.preventDefault(); e.stopPropagation();
      return true;
    }
    if (!e.metaKey || e.ctrlKey || !app.account || app.confirmReq || this.closeReq) return false;
    const k = e.key.toLowerCase();
    // Cmd+J is listed under "Anywhere": from Preferences it closes the dialog and shows the AI pane (never hides it).
    if (app.showSettings) {
      if (k !== "j" || e.altKey || e.shiftKey) return false;
      app.showSettings = false;
      this.toggleAi({ openOnly: true });
      e.preventDefault(); e.stopPropagation();
      return true;
    }
    let ok = true;
    if (e.altKey) {
      if (e.shiftKey) return false;
      if (e.code === "Digit0") this.resetLayout();
      else if (k.startsWith("arrow")) { if (!this.tabsOn) this.arrow(k.slice(5) as T.Arrow); } // no spatial focus in tabs
      else ok = false;
    } else if (e.shiftKey) {
      if (k === "d") this.split("col");
      else if (e.key === "Enter") this.toggleZoom();
      else if (k === "n") this.newEventKey();
      else if (k === "o") this.detachFocused();
      else if (k === "a") this.attachFocused();
      else if (k === "t") this.toggleView();
      else ok = false;
    } else if (k === "j") this.toggleAi();
    else if (k === "d") this.split("row");
    else if (k === "w") this.closeFocused();
    else if (k === "[") { if (this.tabsOn) { this.stepTab(-1); this.tabFocus(); } else this.step(-1); }
    else if (k === "]") { if (this.tabsOn) { this.stepTab(1); this.tabFocus(); } else this.step(1); }
    else if (k === "n" || k === "t") this.newCompose();
    // Cmd+1 / Cmd+2 also fire the native View menu (view-mail / view-calendar), which maps to jumpTab in tabs mode: do not run twice.
    else if (this.tabsOn && /^[3-9]$/.test(e.key) && !this.keyBlock?.()) { this.jumpTab(+e.key); this.tabFocus(); }
    else ok = false;
    if (ok) { e.preventDefault(); e.stopPropagation(); }
    return ok;
  }
  newCompose() { this.openCompose({ to: [], cc: [], bcc: [], subject: "", text: "", inReplyTo: null }); }
  /** Set by App (keeps this module free of a cal.svelte import cycle). */
  onNewEvent: (() => void) | null = null;
  private newEventKey() { this.onNewEvent?.(); }
}

export const workspace = new Workspace();
