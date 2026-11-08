// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/t2model.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { applicable, bytesLabel, exportLine, parseLimit, ruleKey, usageBytes } from "./t2model.ts";

test("parseLimit", () => {
  assert.equal(parseLimit("", 100), null);
  assert.equal(parseLimit("  ", 100), null);
  assert.equal(parseLimit("30", 100), 30);
  assert.equal(parseLimit("0", 100), undefined);
  assert.equal(parseLimit("101", 100), undefined);
  assert.equal(parseLimit("-3", 100), undefined);
  assert.equal(parseLimit("2.5", 100), undefined);
  assert.equal(parseLimit("abc", 100), undefined);
});

test("sizes", () => {
  assert.equal(bytesLabel(0), "0 B");
  assert.equal(bytesLabel(999), "999 B");
  assert.equal(bytesLabel(1500), "1.5 KB");
  assert.equal(bytesLabel(12_345_678), "12.3 MB");
  assert.equal(bytesLabel(250_000_000), "250 MB");
  assert.equal(bytesLabel(-1), "0 B");
  assert.equal(usageBytes({ messages: 1, withBody: 1, bodyBytes: 10, textBytes: 5 }), 15);
});

test("export progress lines", () => {
  const base = { done: 5, total: 10, skipped: 0, path: null, error: null };
  assert.match(exportLine({ ...base, state: "running" }), /5 of 10/);
  assert.match(exportLine({ ...base, state: "done", done: 10, skipped: 2 }), /Exported 8 of 10.*2 could not/);
  assert.match(exportLine({ ...base, state: "cancelled" }), /Stopped after 5 of 10/);
  assert.equal(exportLine({ ...base, state: "error", error: "Disk full" }), "Disk full");
});

test("restore applicability only counts items for accounts that exist here", () => {
  const items = [{ account: "a@x.org" }, { account: "gone@x.org" }, { name: "no account field" }];
  assert.deepEqual(applicable(items, ["a@x.org"]), { applicable: 2, skipped: 1 });
  assert.deepEqual(applicable(undefined, []), { applicable: 0, skipped: 0 });
  assert.equal(ruleKey({ account: "a", name: " Foo " }), ruleKey({ account: "a", name: "foo" }));
});
