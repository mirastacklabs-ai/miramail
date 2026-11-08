// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/composerkeys.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { composerKeyAction as a, insertNewline } from "./composerkeys.ts";

const k = (o: object = {}) => ({ key: "Enter", shiftKey: false, altKey: false, metaKey: false, ctrlKey: false, ...o });

test("plain Enter sends when allowed", () => { assert.equal(a(k(), true), "send"); });
test("Cmd/Ctrl+Enter still sends", () => {
  assert.equal(a(k({ metaKey: true }), true), "send");
  assert.equal(a(k({ ctrlKey: true }), true), "send");
});
test("Shift+Enter is a native newline, even when send is disabled", () => {
  assert.equal(a(k({ shiftKey: true }), true), "default");
  assert.equal(a(k({ shiftKey: true, metaKey: true }), false), "default");
});
test("Alt+Enter inserts a newline", () => { assert.equal(a(k({ altKey: true }), false), "newline"); });
test("empty or streaming: Enter is swallowed, not sent", () => {
  assert.equal(a(k(), false), "none");
  assert.equal(a(k({ metaKey: true }), false), "none");
});
test("IME composition never sends", () => {
  assert.equal(a(k({ isComposing: true }), true), "pass");
  assert.equal(a(k({ keyCode: 229 }), true), "pass");
});
test("other keys pass", () => { assert.equal(a(k({ key: "a" }), true), "pass"); });
test("insertNewline", () => {
  assert.deepEqual(insertNewline("abcd", 2, 2), { text: "ab\ncd", caret: 3 });
  assert.deepEqual(insertNewline("abcd", 1, 3), { text: "a\nd", caret: 2 });
  assert.deepEqual(insertNewline("abcd", 3, 1), { text: "a\nd", caret: 2 });
  assert.deepEqual(insertNewline("", 0, 0), { text: "\n", caret: 1 });
});
