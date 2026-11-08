// @ts-nocheck -- run with: node --test src/lib/layout.test.ts (Node type stripping; no extra dependencies)
import { test } from "node:test";
import assert from "node:assert/strict";
import { clampWidth, READER_MIN } from "./layout-math.ts";

test("clamps to limits", () => {
  assert.equal(clampWidth("sidebar", 10, 340, 1600), 180);
  assert.equal(clampWidth("sidebar", 9999, 340, 3000), 420);
  assert.equal(clampWidth("list", 100, 232, 1600), 260);
  assert.equal(clampWidth("list", 5000, 232, 3000), 700);
});
test("never squeezes the reader below its minimum", () => {
  const w = clampWidth("list", 700, 232, 1000);
  assert.ok(1000 - 232 - w >= READER_MIN - 2);
});
