// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/device.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { thisDevice } from "./device.ts";

const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
test("device name", () => {
  assert.equal(thisDevice("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)", 5), "this iPhone");
  assert.equal(thisDevice(MAC, 0), "this Mac");
  assert.equal(thisDevice(MAC, 5), "this iPad", "iPadOS reports a Mac user agent with touch points");
  assert.equal(thisDevice("Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X)", 5), "this iPad");
});
