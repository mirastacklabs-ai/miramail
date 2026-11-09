// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/tabreorder.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { moveTab, insertionSlot, slotToIndex } from "./tabreorder.ts";
import * as T from "./wstree.ts";

const K = ["mail", "a", "b", "c"];
test("moveTab moves right and left", () => {
  assert.deepEqual(moveTab(K, 1, 3), ["mail", "b", "c", "a"]);
  assert.deepEqual(moveTab(K, 3, 1), ["mail", "c", "a", "b"]);
});
test("moveTab pins Mail and clamps", () => {
  assert.equal(moveTab(K, 0, 2), K);
  assert.deepEqual(moveTab(K, 2, 0), ["mail", "b", "a", "c"]);
  assert.deepEqual(moveTab(K, 1, 99), ["mail", "b", "c", "a"]);
  assert.equal(moveTab(K, 2, 2), K);
  assert.equal(moveTab(K, 9, 1), K);
  assert.deepEqual(moveTab(["a", "b"], 1, 0), ["b", "a"]);
});
test("insertionSlot and slotToIndex", () => {
  assert.equal(insertionSlot([10, 30, 50], 5), 0);
  assert.equal(insertionSlot([10, 30, 50], 35), 2);
  assert.equal(insertionSlot([10, 30, 50], 99), 3);
  assert.equal(slotToIndex(1, 3), 2);
  assert.equal(slotToIndex(2, 1), 1);
  assert.equal(slotToIndex(2, 2), 2);
});
const mk = (id, kind = "compose") => ({ id, kind, title: id, dirty: false, payload: null });
test("reorderTabs re-seats non-Mail panes, keeps Mail and focus", () => {
  let ws = T.defaultWs();
  for (const id of ["c1", "c2", "c3"]) ws = T.openPane(ws, mk(id));
  const keys = () => T.tabGroups(ws).map((g) => g.key);
  assert.deepEqual(keys(), ["mail", "c1", "c2", "c3"]);
  const focus = ws.focus;
  const next = T.reorderTabs(ws, ["c3", "c1", "c2"]);
  assert.deepEqual(T.tabGroups(next).map((g) => g.key), ["mail", "c3", "c1", "c2"]);
  assert.deepEqual([...T.paneIds(next.root)].sort(), [...T.paneIds(ws.root)].sort());
  assert.equal(next.focus, focus);
  assert.equal(T.reorderTabs(ws, ["c1", "c2", "c3"]), ws);
  assert.deepEqual(T.tabGroups(T.reorderTabs(ws, ["c2", "zzz", "c2"])).map((g) => g.key), ["mail", "c2", "c1", "c3"]);
  assert.deepEqual(T.tabGroups(T.deserialize(T.serialize(next))).map((g) => g.key), ["mail", "c3", "c1", "c2"]);
});
