// Pure workspace model: binary split tree + pane registry. No Svelte runes, no DOM, so it runs under node --test.
// Every function returns a new Ws (inputs are never mutated). All state is plain JSON, ready for detach/attach.

export type Dir = "row" | "col";
export type Zone = "left" | "right" | "top" | "bottom" | "center";
export type PaneKind = "mailbox" | "reader" | "compose" | "calendar" | "event" | "aichat";
export const PANE_KINDS: readonly PaneKind[] = ["mailbox", "reader", "compose", "calendar", "event", "aichat"];
/** Kinds whose payload lives in the drafts table (the layout only stores their id/title). */
export const DRAFT_KINDS: readonly PaneKind[] = ["compose", "event"];
/** Stroke paths (24x24 viewBox) shared by pane headers and the tab strip. */
export const PANE_ICONS: Record<PaneKind, string> = {
  mailbox: "M4 6h16v12H4zM4 7l8 6 8-6",
  reader: "M6 3h9l4 4v14H6zM9 12h7M9 16h7",
  compose: "M4 20l4-1 11-11-3-3L5 16zM14 6l3 3",
  calendar: "M4 6h16v14H4zM4 10h16M8 3v4M16 3v4",
  event: "M4 6h16v14H4zM4 10h16M12 13v4M10 15h4",
  aichat: "M4 5h16v11H9l-5 4z",
};
export const MIN_W = 280;
export const MIN_H = 200;

export interface Leaf { t: "leaf"; tabs: string[]; active: string }
export interface Split { t: "split"; dir: Dir; ratio: number; a: Node; b: Node }
export type Node = Leaf | Split;
export interface Pane { id: string; kind: PaneKind; title: string; dirty: boolean; payload: unknown }
export interface Ws {
  root: Node | null;
  panes: Record<string, Pane>;
  /** Registered but not in the tree (draft tray). */
  parked: string[];
  focus: string | null;
  zoom: string | null;
}
export interface Rect { x: number; y: number; w: number; h: number }

export const leaf = (id: string): Leaf => ({ t: "leaf", tabs: [id], active: id });
export const isDraftKind = (k: PaneKind) => DRAFT_KINDS.includes(k);

// ---- tree queries ----
/** Every pane id in the tree (all tabs), in layout order. */
export function paneIds(n: Node | null): string[] {
  if (!n) return [];
  return n.t === "leaf" ? [...n.tabs] : [...paneIds(n.a), ...paneIds(n.b)];
}
/** Only the pane shown in each leaf (the active tab), in layout order. */
export function visibleIds(n: Node | null): string[] {
  if (!n) return [];
  return n.t === "leaf" ? [n.active] : [...visibleIds(n.a), ...visibleIds(n.b)];
}
export function findLeaf(n: Node | null, id: string): Leaf | null {
  if (!n) return null;
  if (n.t === "leaf") return n.tabs.includes(id) ? n : null;
  return findLeaf(n.a, id) ?? findLeaf(n.b, id);
}
export function nodeAt(n: Node | null, path: string): Node | null {
  let cur: Node | null = n;
  for (const c of path) {
    if (!cur || cur.t !== "split") return null;
    cur = c === "a" ? cur.a : cur.b;
  }
  return cur;
}

// ---- tree edits (immutable) ----
function removeId(n: Node | null, id: string): Node | null {
  if (!n) return null;
  if (n.t === "leaf") {
    const i = n.tabs.indexOf(id);
    if (i < 0) return n;
    const tabs = n.tabs.filter((x) => x !== id);
    if (!tabs.length) return null;
    return { t: "leaf", tabs, active: n.active === id ? tabs[Math.min(i, tabs.length - 1)] : n.active };
  }
  const a = removeId(n.a, id), b = removeId(n.b, id);
  if (a && b) return a === n.a && b === n.b ? n : { ...n, a, b };
  return a ?? b;
}
function mapLeaf(n: Node, id: string, f: (l: Leaf) => Node): Node {
  if (n.t === "leaf") return n.tabs.includes(id) ? f(n) : n;
  return { ...n, a: mapLeaf(n.a, id, f), b: mapLeaf(n.b, id, f) };
}
export const zoneDir = (z: Exclude<Zone, "center">): { dir: Dir; before: boolean } =>
  z === "left" ? { dir: "row", before: true } : z === "right" ? { dir: "row", before: false } : z === "top" ? { dir: "col", before: true } : { dir: "col", before: false };

function place(root: Node | null, id: string, near: string | null, zone: Zone): Node {
  if (!root || !near || !findLeaf(root, near)) return root ? { t: "split", dir: "row", ratio: 0.5, a: root, b: leaf(id) } : leaf(id);
  if (zone === "center") return mapLeaf(root, near, (l) => ({ ...l, tabs: [...l.tabs.slice(0, l.tabs.indexOf(l.active) + 1), id, ...l.tabs.slice(l.tabs.indexOf(l.active) + 1)], active: id }));
  const { dir, before } = zoneDir(zone);
  return mapLeaf(root, near, (l) => ({ t: "split", dir, ratio: 0.5, a: before ? leaf(id) : l, b: before ? l : leaf(id) }));
}

// ---- state constructors ----
export function defaultWs(ratio = 0.4): Ws {
  const r = Math.min(0.85, Math.max(0.15, ratio));
  return {
    root: { t: "split", dir: "row", ratio: r, a: leaf("mailbox"), b: leaf("reader") },
    panes: {
      mailbox: { id: "mailbox", kind: "mailbox", title: "Messages", dirty: false, payload: null },
      reader: { id: "reader", kind: "reader", title: "Reader", dirty: false, payload: null },
    },
    parked: [], focus: "mailbox", zoom: null,
  };
}

// ---- state ops ----
/** Register a new pane and place it next to `near` (default: the focused pane). zone "center" = tab-stack. */
export function openPane(ws: Ws, pane: Pane, opts: { near?: string | null; zone?: Zone } = {}): Ws {
  if (ws.panes[pane.id]) return focusPane(ws, pane.id);
  const near = opts.near ?? ws.focus ?? visibleIds(ws.root)[0] ?? null;
  return { ...ws, panes: { ...ws.panes, [pane.id]: pane }, root: place(ws.root, pane.id, near, opts.zone ?? "right"), focus: pane.id, zoom: null };
}

/** Make a pane the focused one (and the active tab of its stack). Parked/unknown ids are ignored. */
export function focusPane(ws: Ws, id: string): Ws {
  if (!ws.root || !findLeaf(ws.root, id)) return ws;
  const root = mapLeaf(ws.root, id, (l) => (l.active === id ? l : { ...l, active: id }));
  const zoom = ws.zoom && ws.zoom !== id ? null : ws.zoom;
  return { ...ws, root, focus: id, zoom };
}

function nextFocus(ws: Ws, gone: string): string | null {
  if (ws.focus !== gone) return ws.focus;
  const order = visibleIds(ws.root);
  const all = paneIds(ws.root);
  const i = all.indexOf(gone);
  const rest = all.filter((x) => x !== gone);
  return rest.find((_, k) => k >= i) ?? rest[rest.length - 1] ?? (order.length ? order[0] : null);
}

/** Remove the pane for good (registry + tree + tray). */
export function closePane(ws: Ws, id: string): Ws {
  if (!ws.panes[id]) return ws;
  const panes = { ...ws.panes };
  delete panes[id];
  const focus = nextFocus(ws, id);
  const root = removeId(ws.root, id);
  const f = focus && findLeaf(root, focus) ? focus : visibleIds(root)[0] ?? null;
  return { root: f ? mapLeaf(root!, f, (l) => (l.active === f ? l : { ...l, active: f })) : root, panes, parked: ws.parked.filter((x) => x !== id), focus: f, zoom: ws.zoom === id ? null : ws.zoom };
}

/** Collapse a pane into the tray: state stays in the registry, the tree forgets it. */
export function parkPane(ws: Ws, id: string): Ws {
  if (!ws.panes[id] || !findLeaf(ws.root, id)) return ws;
  const focus = nextFocus(ws, id);
  const root = removeId(ws.root, id);
  const f = focus && findLeaf(root, focus) ? focus : visibleIds(root)[0] ?? null;
  // Like closePane: the new focus may be a background tab of another stack, so bring it to the front.
  return { ...ws, root: f ? mapLeaf(root!, f, (l) => (l.active === f ? l : { ...l, active: f })) : root, parked: [...ws.parked, id], focus: f, zoom: ws.zoom === id ? null : ws.zoom };
}

export function unparkPane(ws: Ws, id: string, opts: { near?: string | null; zone?: Zone } = {}): Ws {
  if (!ws.parked.includes(id) || !ws.panes[id]) return ws;
  const near = opts.near ?? ws.focus ?? visibleIds(ws.root)[0] ?? null;
  return { ...ws, parked: ws.parked.filter((x) => x !== id), root: place(ws.root, id, near, opts.zone ?? "right"), focus: id, zoom: null };
}

/** Dock `id` onto an edge of `target`'s leaf, or tab-stack it on the centre. `id` may be in the tree or parked. */
export function dockPane(ws: Ws, id: string, target: string, zone: Zone): Ws {
  if (id === target || !ws.panes[id] || !ws.panes[target] || !findLeaf(ws.root, target)) return ws;
  const src = findLeaf(ws.root, id);
  if (src && zone === "center" && src.tabs.includes(target)) return focusPane(ws, id);
  const root = removeId(ws.root, id);
  if (!root || !findLeaf(root, target)) return ws;
  return { ...ws, root: place(root, id, target, zone), parked: ws.parked.filter((x) => x !== id), focus: id, zoom: null };
}
export const movePane = dockPane;

export function swapPanes(ws: Ws, x: string, y: string): Ws {
  if (x === y || !ws.root || !findLeaf(ws.root, x) || !findLeaf(ws.root, y)) return ws;
  const sw = (n: Node): Node => n.t === "leaf"
    ? { ...n, tabs: n.tabs.map((t) => (t === x ? y : t === y ? x : t)), active: n.active === x ? y : n.active === y ? x : n.active }
    : { ...n, a: sw(n.a), b: sw(n.b) };
  // A swap can move the focused pane into a stack where another tab is in front: bring the focus to the front again.
  return focusPane({ ...ws, root: sw(ws.root) }, ws.focus ?? x);
}

export function toggleZoom(ws: Ws, id: string | null = ws.focus): Ws {
  if (!id || !findLeaf(ws.root, id)) return ws;
  if (ws.zoom === id) return { ...ws, zoom: null };
  return { ...focusPane(ws, id), zoom: id };
}

export function patchPane(ws: Ws, id: string, patch: Partial<Pick<Pane, "title" | "dirty" | "payload">>): Ws {
  const p = ws.panes[id];
  if (!p) return ws;
  return { ...ws, panes: { ...ws.panes, [id]: { ...p, ...patch } } };
}

// ---- resize ----
/** Smallest extent of a subtree along `dir` (width for row, height for col). */
export function minSize(n: Node, dir: Dir): number {
  if (n.t === "leaf") return dir === "row" ? MIN_W : MIN_H;
  const a = minSize(n.a, dir), b = minSize(n.b, dir);
  return n.dir === dir ? a + b : Math.max(a, b);
}
/** Clamp a split ratio so both sides keep their minimum size inside `size` px. Too small to fit: split by minimums. */
export function clampRatio(n: Split, ratio: number, size: number): number {
  const ma = minSize(n.a, n.dir), mb = minSize(n.b, n.dir);
  if (!(size > 0) || !Number.isFinite(ratio)) return 0.5;
  if (ma + mb >= size) return ma / (ma + mb);
  return Math.min(Math.max(ratio, ma / size), 1 - mb / size);
}
/** Set the ratio of the split at `path` ("" = root, "a"/"b" steps) given its pixel extent along its direction. */
export function setRatioAt(ws: Ws, path: string, ratio: number, size: number): Ws {
  const set = (n: Node, p: string): Node => {
    if (n.t !== "split") return n;
    if (!p) return { ...n, ratio: clampRatio(n, ratio, size) };
    return p[0] === "a" ? { ...n, a: set(n.a, p.slice(1)) } : { ...n, b: set(n.b, p.slice(1)) };
  };
  return ws.root && nodeAt(ws.root, path)?.t === "split" ? { ...ws, root: set(ws.root, path) } : ws;
}
export const resetRatioAt = (ws: Ws, path: string, size: number) => setRatioAt(ws, path, 0.5, size);

// ---- geometry + focus ----
export function rects(n: Node | null, r: Rect, out: Record<string, Rect> = {}): Record<string, Rect> {
  if (!n) return out;
  if (n.t === "leaf") { out[n.active] = r; return out; }
  if (n.dir === "row") {
    const w = r.w * n.ratio;
    rects(n.a, { x: r.x, y: r.y, w, h: r.h }, out); rects(n.b, { x: r.x + w, y: r.y, w: r.w - w, h: r.h }, out);
  } else {
    const h = r.h * n.ratio;
    rects(n.a, { x: r.x, y: r.y, w: r.w, h }, out); rects(n.b, { x: r.x, y: r.y + h, w: r.w, h: r.h - h }, out);
  }
  return out;
}
export function focusStep(ws: Ws, delta: 1 | -1): Ws {
  const v = visibleIds(ws.root);
  if (!v.length) return ws;
  const i = ws.focus ? v.indexOf(ws.focus) : -1;
  return focusPane(ws, v[i < 0 ? 0 : (i + delta + v.length) % v.length]);
}
export type Arrow = "left" | "right" | "up" | "down";
export function focusDir(ws: Ws, dir: Arrow, area: Rect = { x: 0, y: 0, w: 1000, h: 1000 }): Ws {
  if (!ws.focus) return focusStep(ws, 1);
  const rs = rects(ws.root, area);
  const cur = rs[ws.focus];
  if (!cur) return ws;
  const eps = 1e-6;
  const ov = (a0: number, a1: number, b0: number, b1: number) => Math.min(a1, b1) - Math.max(a0, b0);
  let best: { id: string; gap: number; ov: number } | null = null;
  for (const [id, c] of Object.entries(rs)) {
    if (id === ws.focus) continue;
    const gap = dir === "right" ? c.x - (cur.x + cur.w) : dir === "left" ? cur.x - (c.x + c.w) : dir === "down" ? c.y - (cur.y + cur.h) : cur.y - (c.y + c.h);
    if (gap < -eps) continue;
    const o = dir === "left" || dir === "right" ? ov(cur.y, cur.y + cur.h, c.y, c.y + c.h) : ov(cur.x, cur.x + cur.w, c.x, c.x + c.w);
    if (o <= eps) continue;
    if (!best || gap < best.gap - eps || (Math.abs(gap - best.gap) <= eps && o > best.ov)) best = { id, gap, ov: o };
  }
  return best ? focusPane(ws, best.id) : ws;
}

// ---- tabs view (a second way to lay out the same panes; no state of its own, everything derives from the tree + focus) ----
export interface TabGroup { key: string; ids: string[]; title: string; kind: PaneKind; dirty: boolean }
export const MAIL_KEY = "mail";
const isMailKind = (k: PaneKind | undefined) => k === "mailbox" || k === "reader";
/** Mailbox + reader form one pinned "Mail" group (first); every other pane in the tree is its own group, in layout order. Parked panes are not tabs. */
export function tabGroups(ws: Ws): TabGroup[] {
  const all = paneIds(ws.root).filter((id) => ws.panes[id]);
  const mail = all.filter((id) => isMailKind(ws.panes[id].kind));
  const out: TabGroup[] = [];
  if (mail.length) out.push({ key: MAIL_KEY, ids: mail, title: "Mail", kind: "mailbox", dirty: mail.some((id) => ws.panes[id].dirty) });
  for (const id of all) {
    const p = ws.panes[id];
    if (!isMailKind(p.kind)) out.push({ key: id, ids: [id], title: p.title, kind: p.kind, dirty: p.dirty });
  }
  return out;
}
/** Re-order the non-Mail tabs to `keys` (tab keys = pane ids; unknown / missing ids are ignored, missing ones keep their relative order at the end).
 *  Tab order is layout order, so the non-Mail panes are re-seated into the same tree slots in the new order: the tree shape and
 *  Mail panes stay put, only which pane sits in which slot changes (visible in Tiles too). */
export function reorderTabs(ws: Ws, keys: string[]): Ws {
  const cur = tabGroups(ws).filter((g) => g.key !== MAIL_KEY).map((g) => g.key);
  const want = keys.filter((k, i) => cur.includes(k) && keys.indexOf(k) === i);
  const order = [...want, ...cur.filter((k) => !want.includes(k))];
  if (order.every((k, i) => k === cur[i])) return ws;
  const seat = new Map<string, string>(); // old id -> id now in its slot
  cur.forEach((old, j) => seat.set(old, order[j]));
  const walk = (n: Node): Node => {
    if (n.t === "split") return { ...n, a: walk(n.a), b: walk(n.b) };
    const tabs = n.tabs.map((id) => seat.get(id) ?? id);
    return { t: "leaf", tabs, active: seat.get(n.active) ?? n.active };
  };
  return ws.root ? { ...ws, root: walk(ws.root) } : ws;
}
/** Key of the group the focus is in; falls back to the first group; null when there are no tabs. */
export function activeGroupKey(ws: Ws): string | null {
  const groups = tabGroups(ws);
  if (!groups.length) return null;
  const f = ws.focus ? groups.find((g) => g.ids.includes(ws.focus!)) : undefined;
  return (f ?? groups[0]).key;
}
/** Key of the group `delta` steps from `activeKey`, wrapping. Unknown active key counts as "before the first". */
export function stepGroup(groups: TabGroup[], activeKey: string | null, delta: number): string | null {
  if (!groups.length) return null;
  const i = groups.findIndex((g) => g.key === activeKey);
  const n = groups.length;
  return groups[(((i < 0 ? 0 : i + delta) % n) + n) % n].key;
}
/** The pane to focus when a group's tab is picked (the mail group opens on the message list). */
export const groupFocusId = (g: TabGroup): string => (g.key === MAIL_KEY && g.ids.includes("mailbox") ? "mailbox" : g.ids[0]);

// ---- persistence ----
/** Load limits. MAX_PANES leaves headroom over the drafts table cap (200, app/src-tauri/src/drafts.rs, every draft is a
 *  registered pane) plus the fixed panes; MAX_DEPTH and MAX_LAYOUT_BYTES must match drafts.rs (the backend refuses to save
 *  a layout beyond them). They only guard against hostile or corrupt data. */
const MAX_PANES = 256, MAX_DEPTH = 96, MAX_JSON = 256 * 1024;
const ID_RE = /^[A-Za-z0-9_-]{1,64}$/;

/** JSON for the layout row. Draft-kind payloads are dropped (they live in the drafts table). */
export function serialize(ws: Ws): string {
  const panes: Record<string, unknown> = {};
  for (const p of Object.values(ws.panes)) {
    panes[p.id] = { id: p.id, kind: p.kind, title: p.title.slice(0, 200), payload: isDraftKind(p.kind) ? null : (p.payload ?? null) };
  }
  return JSON.stringify({ v: 1, root: ws.root, panes, parked: ws.parked, focus: ws.focus, zoom: ws.zoom });
}

/** Every pane id below `n` (iterative: safe for any depth), validated. Null when a leaf is malformed. */
function collectIds(n: unknown, seen: Set<string>, out: string[]): boolean {
  const stack: unknown[] = [n];
  while (stack.length) {
    const o = stack.pop() as Record<string, unknown> | null;
    if (!o || typeof o !== "object") return false;
    if (o.t === "leaf") {
      if (!Array.isArray(o.tabs) || !o.tabs.length || o.tabs.length > MAX_PANES) return false;
      for (const t of o.tabs) { if (typeof t !== "string" || !ID_RE.test(t) || seen.has(t)) return false; seen.add(t); out.push(t); }
    } else if (o.t === "split") {
      stack.push(o.b, o.a);
    } else return false;
  }
  return true;
}

function validNode(n: unknown, depth: number, seen: Set<string>): Node | null {
  if (!n || typeof n !== "object") return null;
  const o = n as Record<string, unknown>;
  // Deeper than the limit: do not drop the layout, fold the rest of this branch into one tab stack so every pane survives.
  if (depth > MAX_DEPTH) {
    const ids: string[] = [];
    if (!collectIds(o, seen, ids) || !ids.length) return null;
    return { t: "leaf", tabs: ids.slice(0, MAX_PANES), active: ids[0] };
  }
  if (o.t === "leaf") {
    if (!Array.isArray(o.tabs) || !o.tabs.length || o.tabs.length > MAX_PANES || typeof o.active !== "string") return null;
    for (const t of o.tabs) { if (typeof t !== "string" || !ID_RE.test(t) || seen.has(t)) return null; seen.add(t); }
    if (!o.tabs.includes(o.active)) return null;
    return { t: "leaf", tabs: [...(o.tabs as string[])], active: o.active };
  }
  if (o.t === "split") {
    if ((o.dir !== "row" && o.dir !== "col") || typeof o.ratio !== "number" || !Number.isFinite(o.ratio)) return null;
    const a = validNode(o.a, depth + 1, seen), b = validNode(o.b, depth + 1, seen);
    if (!a || !b) return null;
    return { t: "split", dir: o.dir, ratio: Math.min(0.95, Math.max(0.05, o.ratio)), a, b };
  }
  return null;
}

/** Parse + validate persisted JSON. Returns null for anything inconsistent (caller falls back to the default layout). */
export function parse(json: string | null | undefined): Ws | null {
  if (typeof json !== "string" || json.length > MAX_JSON) return null;
  let o: any;
  try { o = JSON.parse(json); } catch { return null; }
  if (!o || typeof o !== "object" || o.v !== 1 || !o.panes || typeof o.panes !== "object" || Array.isArray(o.panes)) return null;
  const seen = new Set<string>();
  let root: Node | null = null;
  if (o.root !== null) { root = validNode(o.root, 0, seen); if (!root) return null; }
  const panes: Record<string, Pane> = {};
  // Over the cap (only hostile data gets here): keep the panes the tree shows first, drop the surplus unplaced ones.
  const keys = Object.keys(o.panes).sort((x, y) => Number(seen.has(y)) - Number(seen.has(x))).slice(0, Math.max(MAX_PANES, seen.size));
  for (const k of keys) {
    const p = o.panes[k];
    if (!p || typeof p !== "object" || p.id !== k || !ID_RE.test(k) || !PANE_KINDS.includes(p.kind) || typeof p.title !== "string") return null;
    panes[k] = { id: k, kind: p.kind, title: p.title.slice(0, 200), dirty: false, payload: isDraftKind(p.kind) ? null : (p.payload ?? null) };
  }
  const parked: string[] = [];
  if (!Array.isArray(o.parked)) return null;
  for (const id of o.parked) {
    if (typeof id === "string" && !panes[id] && Object.hasOwn(o.panes, id)) continue; // cut by the registry cap
    if (typeof id !== "string" || !panes[id] || seen.has(id) || parked.includes(id)) return null;
    parked.push(id);
  }
  for (const id of seen) if (!panes[id]) return null;
  const focus = typeof o.focus === "string" && seen.has(o.focus) ? o.focus : visibleIds(root)[0] ?? null;
  const zoom = typeof o.zoom === "string" && seen.has(o.zoom) ? o.zoom : null;
  // A folded branch (or an old file) can leave the saved focus behind another tab: bring it to the front.
  if (root && focus && !visibleIds(root).includes(focus)) root = mapLeaf(root, focus, (l) => ({ ...l, active: focus }));
  return { root, panes, parked, focus, zoom };
}
export function deserialize(json: string | null | undefined, fallback: () => Ws = defaultWs): Ws {
  return parse(json) ?? fallback();
}

export interface DraftRow { id: string; kind: PaneKind; title: string; payload: unknown }
/** Re-attach draft payloads after load: draft panes without a draft vanish, drafts without a pane go to the tray. */
export function hydrateDrafts(ws: Ws, drafts: DraftRow[]): Ws {
  const byId = new Map(drafts.map((d) => [d.id, d]));
  let out = ws;
  for (const p of Object.values(ws.panes)) if (isDraftKind(p.kind) && !byId.has(p.id)) out = closePane(out, p.id);
  for (const d of drafts) {
    const p = out.panes[d.id];
    if (p) out = patchPane(out, d.id, { payload: d.payload, title: d.title || p.title, dirty: true });
    else out = { ...out, panes: { ...out.panes, [d.id]: { id: d.id, kind: d.kind, title: d.title, dirty: true, payload: d.payload } }, parked: [...out.parked, d.id] };
  }
  return out;
}
