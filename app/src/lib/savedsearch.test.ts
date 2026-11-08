// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/*.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { addSaved, defaultName, MAX_SAVED, parseSaved, removeSaved, serializeSaved } from "./savedsearch.ts";

test("round trip and validation", () => {
  const l = addSaved([], "Invoices", "invoice has:attachment", "all", "a1");
  assert.deepEqual(parseSaved(serializeSaved(l)), [{ id: "a1", name: "Invoices", query: "invoice has:attachment", scope: "all" }]);
  assert.deepEqual(parseSaved(null), []);
  assert.deepEqual(parseSaved("{not json"), []);
  assert.deepEqual(parseSaved('{"a":1}'), []);
  assert.deepEqual(parseSaved('[1,null,{"id":"x","name":"","query":"q"},{"id":"y","name":"n","query":"  "}]'), []);
  assert.equal(parseSaved('[{"id":"x","name":"n","query":"q","scope":"bogus"}]')[0].scope, "all", "unknown scope falls back");
});

test("adding replaces a same-named search and ignores blanks", () => {
  let l = addSaved([], "Work", "from:boss", "folder", "1");
  l = addSaved(l, "work", "from:boss is:unread", "account", "2");
  assert.equal(l.length, 1);
  assert.equal(l[0].query, "from:boss is:unread");
  assert.equal(addSaved(l, " ", "x", "all"), l);
  assert.equal(addSaved(l, "n", "  ", "all"), l);
  assert.deepEqual(removeSaved(l, "2"), []);
});

test("the list is capped", () => {
  let l = [] as ReturnType<typeof parseSaved>;
  for (let i = 0; i < MAX_SAVED + 10; i++) l = addSaved(l, `s${i}`, `q${i}`, "all", String(i));
  assert.equal(l.length, MAX_SAVED);
  assert.equal(l[l.length - 1].name, `s${MAX_SAVED + 9}`);
});

test("default name", () => { assert.equal(defaultName("  from:a   is:unread "), "from:a is:unread"); });
