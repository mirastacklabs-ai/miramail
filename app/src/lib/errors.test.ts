// @ts-nocheck -- run with: node --test src/lib/errors.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { describeError, installErrorSurface, isIgnored } from "./errors.ts";

test("describeError gives one readable line", () => {
  assert.equal(describeError("plain"), "plain");
  assert.equal(describeError(new Error("boom")), "boom");
  assert.equal(describeError(new TypeError("bad")), "TypeError: bad");
  assert.equal(describeError({ name: "DataCloneError", message: "The object can not be cloned." }), "DataCloneError: The object can not be cloned.");
  assert.equal(describeError({ code: 3 }), '{"code":3}');
  assert.equal(describeError(undefined), "Unknown error");
  assert.equal(describeError(42), "42");
});

function fire(t, type, props) { const e = new Event(type); Object.assign(e, props); t.dispatchEvent(e); }

test("uncaught errors and rejections reach notify, deduplicated, and uninstall works", () => {
  const t = new EventTarget(), got = [];
  let now = 0;
  const off = installErrorSurface(t, (m) => got.push(m), { dedupeMs: 1000, now: () => now, log: () => {} });
  fire(t, "error", { error: { name: "DataCloneError", message: "The object can not be cloned." } });
  fire(t, "error", { error: { name: "DataCloneError", message: "The object can not be cloned." } }); // same, within 1 s
  fire(t, "unhandledrejection", { reason: "The AI backend did not respond (ai_get_settings, 8 s)." });
  fire(t, "error", { message: "ResizeObserver loop completed with undelivered notifications." });
  now = 2000;
  fire(t, "error", { error: { name: "DataCloneError", message: "The object can not be cloned." } }); // reported again later
  assert.deepEqual(got, [
    "Unexpected error: DataCloneError: The object can not be cloned.",
    "Unexpected error: The AI backend did not respond (ai_get_settings, 8 s).",
    "Unexpected error: DataCloneError: The object can not be cloned.",
  ]);
  off();
  fire(t, "error", { error: new Error("after uninstall") });
  assert.equal(got.length, 3);
  assert.ok(isIgnored("ResizeObserver loop limit exceeded"));
});
