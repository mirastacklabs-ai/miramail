// @ts-nocheck -- run with: node --test src/lib/wstree.fuzz.test.ts (Node type stripping; no extra dependencies)
// Invariant tests for the workspace tree: random (but seeded, so deterministic) sequences of every state operation,
// checked after each step. A failure prints the seed and step so it can be replayed.
import { test } from "node:test";
import assert from "node:assert/strict";
import * as T from "./wstree.ts";

/** mulberry32: tiny deterministic PRNG. */
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const pick = (r, xs) => xs[Math.floor(r() * xs.length)];
const ZONES = ["left", "right", "top", "bottom", "center"];
const KINDS = ["compose", "event", "aichat", "reader", "calendar"];

function depth(n) { return !n ? 0 : n.t === "leaf" ? 1 : 1 + Math.max(depth(n.a), depth(n.b)); }
function splitPaths(n, p = "") { return !n || n.t === "leaf" ? [] : [p, ...splitPaths(n.a, p + "a"), ...splitPaths(n.b, p + "b")]; }

function check(ws, where) {
  const tree = T.paneIds(ws.root);
  const vis = T.visibleIds(ws.root);
  assert.equal(new Set(tree).size, tree.length, `${where}: duplicate id in tree ${tree}`);
  for (const id of tree) assert.ok(ws.panes[id], `${where}: tree id ${id} not registered`);
  assert.equal(new Set(ws.parked).size, ws.parked.length, `${where}: duplicate parked`);
  for (const id of ws.parked) {
    assert.ok(ws.panes[id], `${where}: parked ${id} not registered`);
    assert.ok(!tree.includes(id), `${where}: ${id} both parked and in tree`);
  }
  for (const id of Object.keys(ws.panes)) assert.ok(tree.includes(id) || ws.parked.includes(id), `${where}: orphan pane ${id}`);
  if (!ws.root) assert.equal(ws.focus, null, `${where}: focus without a tree`);
  else assert.ok(vis.includes(ws.focus), `${where}: focus ${ws.focus} is not a visible pane (visible: ${vis})`);
  if (ws.zoom !== null) assert.ok(tree.includes(ws.zoom), `${where}: zoom ${ws.zoom} not in tree`);
  const walk = (n) => {
    if (!n) return;
    if (n.t === "leaf") { assert.ok(n.tabs.length > 0 && n.tabs.includes(n.active), `${where}: bad leaf ${JSON.stringify(n)}`); return; }
    assert.ok(Number.isFinite(n.ratio) && n.ratio > 0 && n.ratio < 1, `${where}: ratio ${n.ratio}`);
    walk(n.a); walk(n.b);
  };
  walk(ws.root);
}

function step(ws, r, next) {
  const tree = T.paneIds(ws.root);
  const all = Object.keys(ws.panes);
  const any = () => (all.length ? pick(r, all) : "nope");
  const inTree = () => (tree.length ? pick(r, tree) : "nope");
  switch (Math.floor(r() * 12)) {
    case 0: case 1: {
      const near = r() < 0.3 ? null : r() < 0.9 ? inTree() : "ghost";
      return [`open ${next}`, T.openPane(ws, { id: next, kind: pick(r, KINDS), title: next, dirty: false, payload: { n: next } }, { near, zone: pick(r, ZONES) })];
    }
    case 2: { const id = any(); return [`close ${id}`, T.closePane(ws, id)]; }
    case 3: { const id = inTree(); return [`park ${id}`, T.parkPane(ws, id)]; }
    case 4: { const id = ws.parked.length ? pick(r, ws.parked) : "nope"; return [`unpark ${id}`, T.unparkPane(ws, id, { zone: pick(r, ZONES) })]; }
    case 5: { const a = any(), b = inTree(), z = pick(r, ZONES); return [`dock ${a}->${b}@${z}`, T.dockPane(ws, a, b, z)]; }
    // Any pane in the tree, background tabs included (B7: the focus must stay on a visible pane).
    case 6: { const a = inTree(), b = inTree(); return [`swap ${a} ${b}`, T.swapPanes(ws, a, b)]; }
    case 7: { const id = inTree(); return [`zoom ${id}`, T.toggleZoom(ws, id)]; }
    case 8: { const id = any(); return [`focus ${id}`, T.focusPane(ws, id)]; }
    case 9: { const d = r() < 0.5 ? 1 : -1; return [`step ${d}`, T.focusStep(ws, d)]; }
    case 10: { const d = pick(r, ["left", "right", "up", "down"]); return [`dir ${d}`, T.focusDir(ws, d)]; }
    default: {
      const ps = splitPaths(ws.root);
      const p = ps.length ? pick(r, ps) : "";
      const ratio = pick(r, [r(), -5, 7, NaN, Infinity, 0, 1]);
      return [`ratio ${p}=${ratio}`, T.setRatioAt(ws, p, ratio, pick(r, [0, 300, 1200, 4000]))];
    }
  }
}

test("random operation sequences keep every workspace invariant (fixed seeds)", () => {
  for (let seed = 1; seed <= 300; seed++) {
    const r = rng(seed);
    let ws = T.defaultWs(0.4);
    let n = 0;
    for (let i = 0; i < 60; i++) {
      const before = JSON.stringify(ws);
      const [what, out] = step(ws, r, `p${n++}`);
      assert.equal(JSON.stringify(ws), before, `seed ${seed} step ${i} (${what}) mutated its input`);
      ws = out;
      check(ws, `seed ${seed} step ${i} (${what})`);
    }
  }
});

test("serialize -> parse round-trips any layout the operations can build (within the persisted limits)", () => {
  for (let seed = 1; seed <= 300; seed++) {
    const r = rng(seed * 7919);
    let ws = T.defaultWs(0.4);
    for (let i = 0; i < 40; i++) ws = step(ws, r, `p${i}`)[1];
    if (Object.keys(ws.panes).length > 64 || depth(ws.root) > 33) continue;
    const back = T.parse(T.serialize(ws));
    assert.ok(back, `seed ${seed}: a layout the app built was rejected on reload: ${T.serialize(ws)}`);
    assert.deepEqual(T.paneIds(back.root), T.paneIds(ws.root), `seed ${seed}`);
    assert.deepEqual(T.visibleIds(back.root), T.visibleIds(ws.root), `seed ${seed}`);
    assert.deepEqual(back.parked, ws.parked, `seed ${seed}`);
    assert.equal(back.focus, ws.focus, `seed ${seed}`);
    assert.equal(back.zoom, ws.zoom, `seed ${seed}`);
    check(back, `seed ${seed} after reload`);
  }
});

test("parking the focused pane moves focus to a pane the user can see", () => {
  // Leaf A holds only x (focused); leaf B is a stack [p, q] showing q. Parking x must not focus the hidden tab p
  // unless p is also brought to the front.
  let ws = { root: null, panes: {}, parked: [], focus: null, zoom: null };
  const mk = (id) => ({ id, kind: "compose", title: id, dirty: false, payload: null });
  ws = T.openPane(ws, mk("x"));
  ws = T.openPane(ws, mk("p"), { near: "x", zone: "right" });
  ws = T.openPane(ws, mk("q"), { near: "p", zone: "center" });
  ws = T.focusPane(ws, "x");
  assert.deepEqual(T.visibleIds(ws.root), ["x", "q"]);
  const out = T.parkPane(ws, "x");
  assert.ok(T.visibleIds(out.root).includes(out.focus), `focus ${out.focus} is hidden; visible ${T.visibleIds(out.root)}`);
  assert.deepEqual(out.parked, ["x"]);
});

const draftPane = (i) => ({ id: `c${i}`, kind: "compose", title: "d", dirty: true, payload: null });
const park = (ws, n) => { for (let i = 0; i < n; i++) ws = { ...ws, panes: { ...ws.panes, [`c${i}`]: draftPane(i) }, parked: [...ws.parked, `c${i}`] }; return ws; };

test("layouts with many panes or a deep split survive a reload instead of resetting", () => {
  // The drafts table allows 200 drafts, all registered as parked panes (plus the fixed panes).
  for (const n of [65, 200]) {
    const ws = park(T.defaultWs(), n);
    const back = T.parse(T.serialize(ws));
    assert.ok(back, `${n} parked drafts: layout rejected`);
    assert.equal(back.parked.length, n);
    assert.equal(Object.keys(back.panes).length, n + 2);
  }
  // 200 drafts with 80-character titles still fit the size limit.
  let big = T.defaultWs();
  for (let i = 0; i < 200; i++) big = { ...big, panes: { ...big.panes, [`c${i}`]: { ...draftPane(i), title: "t".repeat(200) } }, parked: [...big.parked, `c${i}`] };
  assert.ok(T.parse(T.serialize(big)), "200 drafts with maximal titles");
  // A split chain deeper than the old limit of 32 (every open pane docked to the right of the last one).
  let ws = T.defaultWs();
  for (let i = 0; i < 40; i++) ws = T.openPane(ws, { id: `p${i}`, kind: "aichat", title: "x", dirty: false, payload: null });
  const back = T.parse(T.serialize(ws));
  assert.ok(back, "40-deep tree rejected");
  assert.deepEqual(T.paneIds(back.root), T.paneIds(ws.root));
});

test("a layout beyond the load limits degrades instead of being dropped", () => {
  // Tree deeper than MAX_DEPTH: the deep branch is folded into one tab stack, every pane survives, nothing duplicated.
  let ws = T.defaultWs();
  for (let i = 0; i < 150; i++) ws = T.openPane(ws, { id: `p${i}`, kind: "aichat", title: "x", dirty: false, payload: null });
  const json = T.serialize(ws);
  const back = T.parse(json);
  assert.ok(back, "too-deep tree: layout dropped");
  assert.deepEqual([...T.paneIds(back.root)].sort(), [...T.paneIds(ws.root)].sort());
  assert.ok(T.visibleIds(back.root).includes(back.focus));
  // Hostile: far more panes than any user can have. Still loads (surplus unplaced panes cut), never throws.
  const huge = JSON.parse(T.serialize(T.defaultWs()));
  for (let i = 0; i < 600; i++) { huge.panes[`h${i}`] = { id: `h${i}`, kind: "compose", title: "d", payload: null }; huge.parked.push(`h${i}`); }
  const loaded = T.parse(JSON.stringify(huge));
  assert.ok(loaded, "600 parked panes: layout dropped");
  assert.ok(Object.keys(loaded.panes).length <= 256);
  assert.ok(loaded.panes.mailbox && loaded.panes.reader);
  for (const id of loaded.parked) assert.ok(loaded.panes[id]);
});

test("swapPanes keeps the focus on a visible pane when the focused pane moves behind another tab (B7)", () => {
  let ws = T.defaultWs();
  ws = T.openPane(ws, { id: "p", kind: "aichat", title: "p", dirty: false, payload: null }, { near: "reader", zone: "center" });
  ws = T.openPane(ws, { id: "q", kind: "aichat", title: "q", dirty: false, payload: null }, { near: "reader", zone: "center" });
  // reader stack = [reader, q, p] with q active; focus mailbox, swap mailbox with background tab p.
  ws = T.focusPane(ws, "mailbox");
  const out = T.swapPanes(ws, "mailbox", "p");
  assert.equal(out.focus, "mailbox");
  assert.ok(T.visibleIds(out.root).includes("mailbox"), "focused pane is hidden after swap");
  assert.ok(T.visibleIds(out.root).includes(out.focus));
});
