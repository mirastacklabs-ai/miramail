// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/*.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { addDismissed, allReasons, dismissKey, headline, isDismissed, MAX_DISMISSED, needsBanner, NONE_FOUND_TEXT, parseDismissed, reasonTitle, removeDismissed, topReasons } from "./safety.ts";

const r = (code, severity, count = 1) => ({ code, severity, title: code.toLowerCase(), detail: "d", count });
const report = (level, reasons) => ({ score: 50, level, reasons });

test("headlines never call a message safe", () => {
  assert.equal(headline("none_found"), NONE_FOUND_TEXT);
  for (const l of ["warning", "caution"]) assert.ok(!/\bsafe\b/i.test(headline(l)), l);
  // The only place the word appears is the explicit disclaimer.
  assert.match(NONE_FOUND_TEXT, /cannot prove a message is safe/);
});

test("top reasons skip info and keep the most severe three", () => {
  const rep = report("warning", [r("A", "info"), r("B", "low"), r("C", "critical"), r("D", "medium"), r("E", "high"), r("F", "high")]);
  assert.deepEqual(topReasons(rep).map((x) => x.code), ["C", "E", "F"]);
  assert.equal(allReasons(rep)[0].code, "C");
  assert.equal(allReasons(rep).length, 6);
  assert.equal(topReasons(report("caution", [r("A", "info")])).length, 0);
});

test("banner is needed only for caution/warning with a real reason", () => {
  assert.equal(needsBanner(null), false);
  assert.equal(needsBanner(report("none_found", [r("A", "low")])), false);
  assert.equal(needsBanner(report("caution", [r("A", "info")])), false);
  assert.equal(needsBanner(report("caution", [r("A", "medium")])), true);
  assert.equal(needsBanner(report("warning", [r("A", "high")])), true);
});

test("reason titles show a count when a code repeats", () => {
  assert.equal(reasonTitle(r("LINK_SHORTENER", "low", 3)), "link_shortener (3)");
  assert.equal(reasonTitle(r("LINK_SHORTENER", "low", 1)), "link_shortener");
});

test("dismissal keys are per account, folder, uidvalidity and uid", () => {
  const a = dismissKey("me@x.org", "INBOX", 5, 10);
  assert.notEqual(a, dismissKey("me@x.org", "INBOX", 6, 10));
  assert.notEqual(a, dismissKey("me@x.org", "INBOX", 5, 11));
  assert.notEqual(a, dismissKey("me@x.org", "Archive", 5, 10));
  assert.notEqual(a, dismissKey("you@x.org", "INBOX", 5, 10));
  assert.equal(dismissKey("a", "b", undefined, 1), dismissKey("a", "b", 0, 1));
});

test("dismiss list: add, check, remove, dedupe and cap", () => {
  let l = [];
  const k = dismissKey("a", "INBOX", 1, 1);
  l = addDismissed(l, k);
  assert.ok(isDismissed(l, k));
  assert.deepEqual(addDismissed(l, k), l);
  assert.ok(!isDismissed(removeDismissed(l, k), k));
  assert.deepEqual(addDismissed([], ""), []);
  let big = [];
  for (let i = 0; i < MAX_DISMISSED + 20; i++) big = addDismissed(big, dismissKey("a", "INBOX", 1, i));
  assert.equal(big.length, MAX_DISMISSED);
  assert.ok(isDismissed(big, dismissKey("a", "INBOX", 1, MAX_DISMISSED + 19)));
  assert.ok(!isDismissed(big, dismissKey("a", "INBOX", 1, 0)));
});

test("stored list is parsed defensively", () => {
  assert.deepEqual(parseDismissed(null), []);
  assert.deepEqual(parseDismissed("not json"), []);
  assert.deepEqual(parseDismissed('{"a":1}'), []);
  assert.deepEqual(parseDismissed('["x", 5, "", "x", null, "y"]'), ["x", "y"]);
});
