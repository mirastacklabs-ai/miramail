// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/touchgrid.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { lpStart, lpArm, lpMove, lpBlocksScroll, minuteAt, moveStartMin, columnAt, createRange, GRID_LONG_MS } from "./touchgrid.ts";

test("long press: a still finger arms, a travelling finger scrolls", () => {
  assert.equal(GRID_LONG_MS, 400);
  assert.equal(lpStart(1), "pending");
  assert.equal(lpStart(2), "idle", "pinch is not a press");
  assert.equal(lpArm(lpStart(1)), "armed");
  assert.equal(lpMove("pending", 3, 3), "pending", "jitter is tolerated");
  assert.equal(lpMove("pending", 0, 30), "cancelled", "vertical swipe = scroll");
  assert.equal(lpArm(lpMove("pending", 0, 30)), "cancelled", "timer after a swipe does nothing");
  assert.equal(lpMove("armed", 0, 200), "armed", "armed stays armed while dragging");
  assert.equal(lpBlocksScroll("armed"), true);
  assert.equal(lpBlocksScroll("pending"), false);
  assert.equal(lpBlocksScroll("cancelled"), false);
});
test("minute math", () => {
  assert.equal(minuteAt(148, 100, 48), 60);
  assert.equal(minuteAt(50, 100, 48), 0, "clamped above");
  assert.equal(minuteAt(99999, 100, 48), 1440, "clamped below");
  assert.equal(moveStartMin(600, 30, 60), 570);
  assert.equal(moveStartMin(10, 30, 60), 0, "not before midnight");
  assert.equal(moveStartMin(1430, 0, 60), 1380, "ends by midnight");
  assert.equal(moveStartMin(1430, 0, 2000), 0, "longer than a day pins to 0");
});
test("column and create range", () => {
  const r = [{ left: 0, right: 100 }, { left: 100, right: 200 }];
  assert.equal(columnAt(150, r), 1);
  assert.equal(columnAt(-5, r), 0);
  assert.equal(columnAt(500, r), 1);
  assert.equal(columnAt(5, []), -1);
  assert.deepEqual(createRange(540, 540), { start: 540, end: 600 });
  assert.deepEqual(createRange(600, 540), { start: 540, end: 600 });
  assert.deepEqual(createRange(1410, 1410), { start: 1410, end: 1440 });
});

import { readFileSync } from "node:fs";
test("TimeGrid: pointer handlers ignore touch (the touch path owns touch gestures)", () => {
  const src = readFileSync(new URL("../components/TimeGrid.svelte", import.meta.url), "utf8");
  assert.match(src, /function move\(e: PointerEvent\) \{ if \(e\.pointerType === "touch"\) return;/);
  assert.match(src, /function up\(e: PointerEvent, d: Date\) \{\s*if \(e\.pointerType === "touch"/);
  assert.match(src, /onpointercancel=\{\(e\) => \{ if \(e\.pointerType !== "touch"\)/);
  assert.match(src, /function down\(e: PointerEvent, d: Date\) \{\s*if \(e\.pointerType === "touch"/);
});
