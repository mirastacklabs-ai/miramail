// @ts-nocheck -- run with: node --test src/lib/older.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import {
  windowRange, scrollToReveal, nearBottom, mergeRows, loadedLabel, remainingOnServer, coversDate, indexOfDate, endOfDay,
  showSearchOlder, shouldAutoLoad, stepFor, fmtN,
} from "./older.ts";

test("windowRange covers the viewport plus overscan and clamps", () => {
  assert.deepEqual(windowRange(0, 600, 60, 10_000, 5), { start: 0, end: 15 });
  assert.deepEqual(windowRange(6000, 600, 60, 10_000, 5), { start: 95, end: 115 });
  assert.deepEqual(windowRange(599_400, 600, 60, 10_000, 5), { start: 9985, end: 10_000 });
  assert.deepEqual(windowRange(0, 600, 60, 0), { start: 0, end: 0 });
  assert.deepEqual(windowRange(0, 600, 0, 50), { start: 0, end: 50 }); // unmeasured row height: render all
  assert.deepEqual(windowRange(-50, 600, 60, 3, 5), { start: 0, end: 3 });
  // scrolled far past the end (list shrank): still renders the tail
  const w = windowRange(1e9, 600, 60, 100, 5);
  assert.ok(w.end === 100 && w.start >= 90 && w.start < 100);
});

test("rendered window stays small for huge lists", () => {
  const w = windowRange(250_000, 800, 64, 10_000);
  assert.ok(w.end - w.start <= 30);
});

test("scrollToReveal", () => {
  assert.equal(scrollToReveal(0, 300, 600, 60), 0);
  assert.equal(scrollToReveal(5, 300, 600, 60), 300); // already visible
  assert.equal(scrollToReveal(20, 300, 600, 60), 1200 + 60 - 600);
  assert.equal(scrollToReveal(2, 300, 600, 60), 120);
});

test("nearBottom", () => {
  assert.equal(nearBottom(0, 600, 10_000), false);
  assert.equal(nearBottom(9000, 600, 10_000), true);
  assert.equal(nearBottom(0, 600, 500), true); // short list
});

const r = (uid, date) => ({ uid, date, subject: "s" + uid });
test("mergeRows dedupes by uid, newest first, incoming wins", () => {
  const a = [r(5, 500), r(4, 400)];
  const m = mergeRows(a, [r(3, 300), r(4, 400), r(1, 100)]);
  assert.deepEqual(m.map((x) => x.uid), [5, 4, 3, 1]);
  assert.equal(mergeRows(a, []), a);
  const upd = mergeRows(a, [{ ...r(4, 400), subject: "new" }]);
  assert.equal(upd.find((x) => x.uid === 4).subject, "new");
  // equal dates tie-break by uid desc; null dates sort last
  assert.deepEqual(mergeRows([r(1, 10)], [r(2, 10), r(3, null)]).map((x) => x.uid), [2, 1, 3]);
});

test("labels", () => {
  assert.equal(loadedLabel(200, 3412), "Showing 200 of 3,412 messages");
  assert.equal(loadedLabel(1, 1), "Showing 1 of 1 message");
  assert.equal(loadedLabel(200, null), "200 messages loaded");
  assert.equal(loadedLabel(5, 3), "5 messages loaded"); // stale total smaller than loaded
  assert.equal(fmtN(1234567), "1,234,567");
});

test("remainingOnServer and stepFor", () => {
  assert.equal(remainingOnServer(200, 3412, false), 3212);
  assert.equal(remainingOnServer(3412, 3412, false), 0);
  assert.equal(remainingOnServer(200, 3412, true), 0);
  assert.equal(remainingOnServer(200, null, false), 0);
  assert.equal(remainingOnServer(500, 300, false), 0);
  assert.equal(stepFor(3212), 200);
  assert.equal(stepFor(37), 37);
  assert.equal(stepFor(0), 200);
});

test("coversDate / indexOfDate", () => {
  const rows = [r(3, 300), r(2, 200), r(1, 100)];
  assert.equal(coversDate(rows, 150), true);
  assert.equal(coversDate(rows, 100), true);
  assert.equal(coversDate(rows, 99), false);
  assert.equal(coversDate([], 99), false);
  assert.equal(coversDate([r(1, null)], 99), false);
  assert.equal(indexOfDate(rows, 250), 1);
  assert.equal(indexOfDate(rows, 50), 3);
  assert.equal(indexOfDate(rows, 1000), 0);
});

test("endOfDay parses date input values", () => {
  const t = endOfDay("2025-03-01");
  const d = new Date(t * 1000);
  assert.deepEqual([d.getFullYear(), d.getMonth(), d.getDate(), d.getHours(), d.getMinutes()], [2025, 2, 1, 23, 59]);
  assert.equal(endOfDay(""), null);
  assert.equal(endOfDay("2025-02-30"), null);
  assert.equal(endOfDay("03/01/2025"), null);
});

test("search-older hint and auto-load gating", () => {
  assert.equal(showSearchOlder(true, 3, true), true);
  assert.equal(showSearchOlder(true, 50, true), false);
  assert.equal(showSearchOlder(false, 0, true), false);
  assert.equal(showSearchOlder(true, 0, false), false);
  const ok = { near: true, moreAvailable: true, busy: false, errored: false, bulk: false, filtered: false };
  assert.equal(shouldAutoLoad(ok), true);
  for (const k of ["busy", "errored", "bulk", "filtered"]) assert.equal(shouldAutoLoad({ ...ok, [k]: true }), false, k);
  assert.equal(shouldAutoLoad({ ...ok, near: false }), false);
  assert.equal(shouldAutoLoad({ ...ok, moreAvailable: false }), false);
});
