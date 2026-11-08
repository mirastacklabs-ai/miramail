// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/*.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { chunk, clickSel, describe, emptySel, nextAfterRemoval, prune, runChunks, selectAll, undoItems, type ChunkResult } from "./bulk.ts";
import type { Outcome } from "./orgtypes.ts";

const oc = (uid: number, ok = true, error: string | null = null): Outcome => ({ uid, ok, error, newUid: ok ? uid + 1000 : null, messageId: `m${uid}@x` });
const ok = (outcomes: Outcome[]): ChunkResult => ({ outcomes, destFolder: "Archive", destValidity: 9 });

test("chunk splits and keeps order", () => {
  assert.deepEqual(chunk([1, 2, 3, 4, 5], 2), [[1, 2], [3, 4], [5]]);
  assert.deepEqual(chunk([], 2), []);
  assert.equal(chunk(Array.from({ length: 450 }, (_, i) => i)).length, 3);
});

test("all chunks succeed: progress is reported per chunk", async () => {
  const seen: number[] = [];
  const s = await runChunks([1, 2, 3, 4, 5], async (c) => ok(c.map((u) => oc(u))), (p) => seen.push(p.done), undefined, 2);
  assert.deepEqual(seen, [2, 4, 5]);
  assert.equal(s.ok.length, 5);
  assert.equal(s.failed.length + s.notAttempted, 0);
  assert.equal(s.destFolder, "Archive");
  assert.equal(describe("Archived", s), "Archived 5 messages.");
  assert.equal(describe("Archived", { ...s, ok: s.ok.slice(0, 1) , total: 1 }), "Archived 1 message.");
});

test("per-item failures are collected and the run continues", async () => {
  const s = await runChunks([1, 2, 3, 4], async (c) => ok(c.map((u) => oc(u, u % 2 === 1, u % 2 ? null : "The server refused the move."))), undefined, undefined, 2);
  assert.deepEqual(s.ok.map((o) => o.uid), [1, 3]);
  assert.deepEqual(s.failed, [{ uid: 2, error: "The server refused the move." }, { uid: 4, error: "The server refused the move." }]);
  assert.equal(s.notAttempted, 0);
  assert.equal(describe("Moved", s), "Moved 2 of 4. 2 failed. The server refused the move.");
});

test("a chunk that throws fails as a whole and stops the run", async () => {
  let calls = 0;
  const s = await runChunks([1, 2, 3, 4, 5, 6], async (c) => {
    calls++;
    if (calls === 2) throw "UIDVALIDITY changed; resync the folder";
    return ok(c.map((u) => oc(u)));
  }, undefined, undefined, 2);
  assert.equal(calls, 2, "the third chunk is never sent");
  assert.deepEqual(s.ok.map((o) => o.uid), [1, 2]);
  assert.equal(s.failed.length, 2);
  assert.equal(s.notAttempted, 2);
  assert.equal(describe("Archived", s), "Archived 2 of 6. 2 failed. UIDVALIDITY changed; resync the folder 2 not attempted.");
});

test("cancelling stops before the next chunk", async () => {
  let stop = false;
  const s = await runChunks([1, 2, 3, 4, 5], async (c) => { stop = true; return ok(c.map((u) => oc(u))); }, undefined, () => stop, 2);
  assert.equal(s.ok.length, 2);
  assert.equal(s.notAttempted, 3);
  assert.ok(s.cancelled);
});

test("a short answer counts the missing items as failed, never as moved", async () => {
  const s = await runChunks([1, 2, 3], async () => ok([oc(1)]));
  assert.equal(s.ok.length, 1);
  assert.equal(s.failed.length, 2);
});

test("describe names several reasons compactly", () => {
  const base = { total: 3, ok: [], notAttempted: 0, cancelled: false, destFolder: null, destValidity: null };
  assert.match(describe("Moved", { ...base, failed: [{ uid: 1, error: "A" }, { uid: 2, error: "B" }, { uid: 3, error: "C" }] }), /A \(and 2 other reasons\)/);
});

test("undo covers only messages that can be found again", () => {
  const s = {
    total: 3, notAttempted: 0, cancelled: false, failed: [], destFolder: "Archive", destValidity: 9,
    ok: [oc(1), { ...oc(2), newUid: null }, { uid: 3, ok: true, error: null, newUid: null, messageId: null } as Outcome],
  };
  const u = undoItems(s);
  assert.equal(u.total, 3);
  assert.deepEqual(u.items, [{ uid: 1001, messageId: "m1@x" }, { uid: null, messageId: "m2@x" }]);
});

test("selection: toggle, range, select all, prune", () => {
  const rows = [10, 20, 30, 40, 50];
  let s = clickSel(emptySel(), rows, 20, { shift: false, meta: true });
  assert.deepEqual([...s.set], [20]);
  s = clickSel(s, rows, 40, { shift: true, meta: false });
  assert.deepEqual([...s.set].sort(), [20, 30, 40]);
  s = clickSel(s, rows, 30, { shift: false, meta: true });
  assert.deepEqual([...s.set].sort(), [20, 40]);
  s = clickSel(emptySel(), rows, 50, { shift: true, meta: false });
  assert.deepEqual([...s.set], [50], "shift with no anchor acts like a toggle");
  s = selectAll(rows);
  assert.equal(s.set.size, 5);
  assert.deepEqual([...prune(s, [10, 20]).set].sort(), [10, 20]);
  assert.equal(prune(s, rows), s, "unchanged object when nothing is pruned");
});

test("next row after removal follows the preference", () => {
  const rows = [1, 2, 3, 4, 5];
  assert.equal(nextAfterRemoval(rows, new Set([2, 3]), "next"), 4);
  assert.equal(nextAfterRemoval(rows, new Set([2, 3]), "previous"), 1);
  assert.equal(nextAfterRemoval(rows, new Set([4, 5]), "next"), 3);
  assert.equal(nextAfterRemoval(rows, new Set([1]), "previous"), 2);
  assert.equal(nextAfterRemoval(rows, new Set(rows), "next"), null);
  assert.equal(nextAfterRemoval(rows, new Set([2]), "none"), null);
});
