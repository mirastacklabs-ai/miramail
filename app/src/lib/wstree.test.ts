// @ts-nocheck -- run with: node --test src/lib/wstree.test.ts (Node type stripping; no extra dependencies)
import { test } from "node:test";
import assert from "node:assert/strict";
import * as T from "./wstree.ts";

const mk = (id, kind = "compose") => ({ id, kind, title: id, dirty: false, payload: { text: "secret " + id } });
const ids = (ws) => T.paneIds(ws.root);

test("default layout is mailbox | reader", () => {
  const ws = T.defaultWs(0.4);
  assert.deepEqual(ids(ws), ["mailbox", "reader"]);
  assert.equal(ws.root.dir, "row");
  assert.equal(ws.root.ratio, 0.4);
  assert.equal(ws.focus, "mailbox");
});

test("openPane splits next to the focused pane, right by default", () => {
  let ws = T.focusPane(T.defaultWs(), "reader");
  ws = T.openPane(ws, mk("c1"));
  assert.deepEqual(ids(ws), ["mailbox", "reader", "c1"]);
  assert.equal(ws.focus, "c1");
  assert.equal(ws.root.b.t, "split");
  assert.equal(ws.root.b.a.active, "reader");
});

test("split before / below", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"), { near: "reader", zone: "left" });
  assert.deepEqual(ids(ws), ["mailbox", "c1", "reader"]);
  ws = T.openPane(ws, mk("c2"), { near: "reader", zone: "bottom" });
  assert.deepEqual(ids(ws), ["mailbox", "c1", "reader", "c2"]);
  const sp = T.nodeAt(ws.root, "bb");
  assert.equal(sp.dir, "col");
});

test("tab-stack via zone center keeps one visible pane per leaf", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"), { near: "reader", zone: "center" });
  const l = T.findLeaf(ws.root, "reader");
  assert.deepEqual(l.tabs, ["reader", "c1"]);
  assert.equal(l.active, "c1");
  assert.deepEqual(T.visibleIds(ws.root), ["mailbox", "c1"]);
});

test("close collapses the parent split and moves focus", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));
  ws = T.closePane(ws, "c1");
  assert.deepEqual(ids(ws), ["mailbox", "reader"]);
  assert.equal(ws.panes.c1, undefined);
  assert.ok(ws.focus && ids(ws).includes(ws.focus));
  ws = T.closePane(T.closePane(ws, "reader"), "mailbox");
  assert.equal(ws.root, null);
  assert.equal(ws.focus, null);
  assert.equal(T.closePane(ws, "nope"), ws);
});

test("close a tab keeps the stack and activates a neighbour", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"), { near: "reader", zone: "center" });
  ws = T.closePane(ws, "c1");
  assert.deepEqual(T.findLeaf(ws.root, "reader").tabs, ["reader"]);
  assert.equal(T.findLeaf(ws.root, "reader").active, "reader");
});

test("park and unpark keep state, drop from the tree", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));
  ws = T.parkPane(ws, "c1");
  assert.deepEqual(ids(ws), ["mailbox", "reader"]);
  assert.deepEqual(ws.parked, ["c1"]);
  assert.equal(ws.panes.c1.payload.text, "secret c1");
  ws = T.unparkPane(ws, "c1", { near: "mailbox", zone: "bottom" });
  assert.deepEqual(ws.parked, []);
  assert.deepEqual(ids(ws), ["mailbox", "c1", "reader"]);
  assert.equal(ws.focus, "c1");
});

test("dock onto an edge moves the pane and collapses its old place", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));       // mailbox | (reader | c1)
  ws = T.dockPane(ws, "c1", "mailbox", "top");
  assert.deepEqual(ids(ws), ["c1", "mailbox", "reader"]);
  assert.equal(ws.root.a.dir, "col");
  assert.equal(ws.root.b.t, "leaf");
  assert.equal(T.dockPane(ws, "c1", "c1", "left"), ws);
});

test("dock onto centre makes a tab stack; dragging a tab out splits it", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));
  ws = T.dockPane(ws, "c1", "mailbox", "center");
  assert.deepEqual(T.findLeaf(ws.root, "mailbox").tabs, ["mailbox", "c1"]);
  assert.equal(T.visibleIds(ws.root).length, 2);
  const same = T.dockPane(ws, "c1", "mailbox", "center");
  assert.deepEqual(T.findLeaf(same.root, "c1").tabs, ["mailbox", "c1"]);
  ws = T.dockPane(ws, "c1", "mailbox", "right");
  assert.deepEqual(T.findLeaf(ws.root, "mailbox").tabs, ["mailbox"]);
  assert.deepEqual(ids(ws), ["mailbox", "c1", "reader"]);
});

test("dock a parked pane", () => {
  let ws = T.parkPane(T.openPane(T.defaultWs(), mk("c1")), "c1");
  ws = T.dockPane(ws, "c1", "reader", "bottom");
  assert.deepEqual(ws.parked, []);
  assert.ok(ids(ws).includes("c1"));
});

test("movePane alias and swap", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));
  assert.equal(T.movePane, T.dockPane);
  ws = T.swapPanes(ws, "mailbox", "c1");
  assert.deepEqual(ids(ws), ["c1", "mailbox", "reader"]);
});

test("resize clamps to min sizes (280 x 200)", () => {
  let ws = T.defaultWs();
  ws = T.setRatioAt(ws, "", 0.01, 1000);
  assert.equal(ws.root.ratio, 0.28);
  ws = T.setRatioAt(ws, "", 0.99, 1000);
  assert.equal(ws.root.ratio, 0.72);
  ws = T.setRatioAt(ws, "", 0.5, 400);                  // cannot fit both: split by minimums
  assert.equal(ws.root.ratio, 0.5);
  ws = T.openPane(ws, mk("c1"), { near: "reader", zone: "bottom" });
  const p = "b";
  ws = T.setRatioAt(ws, p, 0.0, 600);                   // col split: 200 / 600
  assert.ok(Math.abs(T.nodeAt(ws.root, p).ratio - 200 / 600) < 1e-9);
  ws = T.setRatioAt(ws, p, 0.5, 600);
  assert.equal(T.nodeAt(ws.root, p).ratio, 0.5);
  assert.equal(T.setRatioAt(ws, "aa", 0.5, 600), ws);   // not a split
  ws = T.resetRatioAt(T.setRatioAt(ws, "", 0.3, 1000), "", 1000);
  assert.equal(ws.root.ratio, 0.5);
});

test("min size of nested splits adds along the axis", () => {
  const ws = T.openPane(T.defaultWs(), mk("c1"), { near: "reader" });       // mailbox | (reader | c1)
  assert.equal(T.minSize(ws.root, "row"), 840);
  assert.equal(T.minSize(ws.root, "col"), 200);
  const r = T.setRatioAt(ws, "", 0.1, 1200);
  assert.ok(Math.abs(r.root.ratio - 280 / 1200) < 1e-9);
  const r2 = T.setRatioAt(ws, "", 0.9, 1200);
  assert.ok(Math.abs(r2.root.ratio - (1 - 560 / 1200)) < 1e-9);
});

test("zoom and unzoom", () => {
  let ws = T.toggleZoom(T.defaultWs(), "reader");
  assert.equal(ws.zoom, "reader");
  assert.equal(ws.focus, "reader");
  ws = T.toggleZoom(ws);
  assert.equal(ws.zoom, null);
  ws = T.toggleZoom(T.defaultWs());
  assert.equal(ws.zoom, "mailbox");
  assert.equal(T.focusPane(ws, "reader").zoom, null);   // focusing another pane unzooms
  assert.equal(T.closePane(ws, "mailbox").zoom, null);
  assert.equal(T.toggleZoom(T.defaultWs(), "ghost").zoom, null);
});

test("focus order next / prev wraps over visible panes", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"), { near: "reader" });
  ws = T.focusPane(ws, "mailbox");
  ws = T.focusStep(ws, 1); assert.equal(ws.focus, "reader");
  ws = T.focusStep(ws, 1); assert.equal(ws.focus, "c1");
  ws = T.focusStep(ws, 1); assert.equal(ws.focus, "mailbox");
  ws = T.focusStep(ws, -1); assert.equal(ws.focus, "c1");
  assert.equal(T.focusStep({ ...T.defaultWs(), root: null, focus: null }, 1).focus, null);
});

test("spatial focus picks the neighbour in that direction", () => {
  // mailbox | (reader / c1)
  let ws = T.openPane(T.defaultWs(), mk("c1"), { near: "reader", zone: "bottom" });
  ws = T.focusPane(ws, "mailbox");
  assert.equal(T.focusDir(ws, "right").focus, "reader");            // top-right overlaps first
  assert.equal(T.focusDir(ws, "left").focus, "mailbox");            // nothing to the left
  const r = T.focusPane(ws, "reader");
  assert.equal(T.focusDir(r, "down").focus, "c1");
  assert.equal(T.focusDir(T.focusPane(r, "c1"), "up").focus, "reader");
  assert.equal(T.focusDir(T.focusPane(r, "c1"), "left").focus, "mailbox");
  assert.equal(T.focusDir(r, "up").focus, "reader");
});

test("serialize / deserialize roundtrip; draft payloads are not stored", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));
  ws = T.patchPane(ws, "reader", { payload: { uid: 5 } });
  ws = T.parkPane(T.openPane(ws, mk("c2", "event")), "c2");
  ws = T.toggleZoom(ws, "c1");
  const json = T.serialize(ws);
  assert.ok(!json.includes("secret"));
  const back = T.deserialize(json);
  assert.deepEqual(back.root, ws.root);
  assert.deepEqual(back.parked, ["c2"]);
  assert.equal(back.zoom, "c1");
  assert.deepEqual(back.panes.reader.payload, { uid: 5 });
  assert.equal(back.panes.c1.payload, null);
  assert.ok(json.length < 65536);
});

test("corrupt layouts fall back to the default", () => {
  const good = JSON.parse(T.serialize(T.defaultWs()));
  const bad = [
    "", "nope", "null", "[]", "{}", JSON.stringify({ ...good, v: 2 }),
    JSON.stringify({ ...good, root: { t: "leaf", tabs: [], active: "x" } }),
    JSON.stringify({ ...good, root: { t: "leaf", tabs: ["mailbox"], active: "reader" } }),
    JSON.stringify({ ...good, root: { t: "split", dir: "diag", ratio: 0.5, a: good.root.a, b: good.root.b } }),
    JSON.stringify({ ...good, root: { ...good.root, ratio: "x" } }),
    JSON.stringify({ ...good, root: { ...good.root, b: good.root.a } }),                     // duplicate id
    JSON.stringify({ ...good, panes: { ...good.panes, mailbox: { ...good.panes.mailbox, kind: "evil" } } }),
    JSON.stringify({ ...good, panes: { mailbox: good.panes.mailbox } }),                     // tree id missing in registry
    JSON.stringify({ ...good, parked: ["mailbox"] }),                                        // parked and in tree
    JSON.stringify({ ...good, root: { t: "leaf", tabs: ["bad id!"], active: "bad id!" } }),
    "x".repeat(70000),
  ];
  for (const b of bad) {
    assert.equal(T.parse(b), null, b.slice(0, 60));
    assert.deepEqual(T.deserialize(b), T.defaultWs());
  }
  // too deep (beyond MAX_DEPTH = 96) and not matching the registry: still rejected (the tree names panes that are not registered)
  let n = { t: "leaf", tabs: ["mailbox"], active: "mailbox" };
  for (let i = 0; i < 120; i++) n = { t: "split", dir: "row", ratio: 0.5, a: n, b: { t: "leaf", tabs: ["x" + i], active: "x" + i } };
  assert.equal(T.parse(JSON.stringify({ ...good, root: n })), null);
});

test("deserialize clamps wild ratios and repairs focus/zoom", () => {
  const good = JSON.parse(T.serialize(T.defaultWs()));
  const ws = T.deserialize(JSON.stringify({ ...good, root: { ...good.root, ratio: 7 }, focus: "ghost", zoom: "ghost" }));
  assert.equal(ws.root.ratio, 0.95);
  assert.equal(ws.focus, "mailbox");
  assert.equal(ws.zoom, null);
});

test("hydrateDrafts re-attaches payloads, drops orphan panes, parks orphan drafts", () => {
  let ws = T.openPane(T.openPane(T.defaultWs(), mk("c1")), mk("c2"));
  ws = T.deserialize(T.serialize(ws));
  const out = T.hydrateDrafts(ws, [
    { id: "c1", kind: "compose", title: "Hello", payload: { text: "hi" } },
    { id: "d9", kind: "event", title: "Lunch", payload: { s: 1 } },
  ]);
  assert.ok(!out.panes.c2);
  assert.deepEqual(out.panes.c1.payload, { text: "hi" });
  assert.equal(out.panes.c1.title, "Hello");
  assert.equal(out.panes.c1.dirty, true);
  assert.deepEqual(out.parked, ["d9"]);
  assert.ok(!ids(out).includes("c2"));
});

test("operations do not mutate their input", () => {
  const ws = T.defaultWs();
  const snap = JSON.stringify(ws);
  T.openPane(ws, mk("c1")); T.closePane(ws, "reader"); T.dockPane(ws, "reader", "mailbox", "left"); T.setRatioAt(ws, "", 0.3, 1000); T.toggleZoom(ws);
  assert.equal(JSON.stringify(ws), snap);
});

// ---- tabs view ----
test("tabGroups: mail pinned first, other panes one group each, parked excluded", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));
  ws = T.openPane(ws, mk("cal", "calendar"), { zone: "center" });
  ws = T.openPane(ws, mk("c2"));
  ws = T.parkPane(ws, "c2");
  const g = T.tabGroups(ws);
  assert.deepEqual(g.map((x) => x.key), ["mail", "c1", "cal"]);
  assert.deepEqual(g[0].ids, ["mailbox", "reader"]);
  assert.equal(g[0].title, "Mail");
  assert.equal(g[1].kind, "compose");
});

test("tabGroups: mail group survives with only one of its panes; none when the tree is empty", () => {
  let ws = T.closePane(T.defaultWs(), "mailbox");
  assert.deepEqual(T.tabGroups(ws).map((x) => [x.key, x.ids]), [["mail", ["reader"]]]);
  ws = T.closePane(ws, "reader");
  assert.deepEqual(T.tabGroups(ws), []);
  assert.equal(T.activeGroupKey(ws), null);
});

test("tabGroups: title and dirty flag follow the pane", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));
  ws = T.patchPane(ws, "c1", { title: "Hello", dirty: true });
  const g = T.tabGroups(ws).find((x) => x.key === "c1");
  assert.equal(g.title, "Hello");
  assert.equal(g.dirty, true);
  assert.equal(T.tabGroups(ws)[0].dirty, false);
});

test("activeGroupKey: derived from focus", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));
  assert.equal(T.activeGroupKey(ws), "c1");
  assert.equal(T.activeGroupKey(T.focusPane(ws, "reader")), "mail");
  assert.equal(T.activeGroupKey(T.focusPane(ws, "mailbox")), "mail");
  assert.equal(T.activeGroupKey({ ...ws, focus: null }), "mail");
  assert.equal(T.activeGroupKey({ ...ws, focus: "ghost" }), "mail");
});

test("stepGroup wraps both ways and tolerates unknown / empty", () => {
  const g = [{ key: "mail" }, { key: "a" }, { key: "b" }];
  assert.equal(T.stepGroup(g, "mail", 1), "a");
  assert.equal(T.stepGroup(g, "b", 1), "mail");
  assert.equal(T.stepGroup(g, "mail", -1), "b");
  assert.equal(T.stepGroup(g, "nope", 1), "mail");
  assert.equal(T.stepGroup([], "x", 1), null);
});

test("groupFocusId picks the first pane (message list for Mail)", () => {
  const ws = T.openPane(T.defaultWs(), mk("c1"));
  const g = T.tabGroups(ws);
  assert.equal(T.groupFocusId(g[0]), "mailbox");
  assert.equal(T.groupFocusId(g[1]), "c1");
});

test("groupFocusId: Mail group prefers mailbox even when the reader is laid out first", () => {
  let ws = T.openPane(T.defaultWs(), mk("c1"));
  ws = T.dockPane(ws, "reader", "mailbox", "left");
  const mail = T.tabGroups(ws)[0];
  assert.deepEqual(mail.ids, ["reader", "mailbox"]);
  assert.equal(T.groupFocusId(mail), "mailbox");
  assert.equal(T.groupFocusId(T.tabGroups(T.closePane(ws, "mailbox"))[0]), "reader");
});
