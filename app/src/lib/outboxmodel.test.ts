// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/outboxmodel.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { secondsLeft, holding, interrupted, needsAttention, recipients, describe, actions, parseRestore, restoredPayload, buildRestore } from "./outboxmodel.ts";

const it = (o = {}) => ({ id: "o1", account: "a@x.org", to: ["b@y.org"], subject: "S", state: "holding", error: null, dueAt: 10_000, nextRetryAt: null, failures: 0, attachments: 0, createdAt: 1, ...o });

test("countdown never goes negative and rounds up", () => {
  assert.equal(secondsLeft(it(), 5_001), 5);
  assert.equal(secondsLeft(it(), 9_999), 1);
  assert.equal(secondsLeft(it(), 10_000), 0);
  assert.equal(secondsLeft(it(), 99_000), 0);
});

test("only entries still inside the window count as holding", () => {
  const l = [it({ id: "a", createdAt: 2 }), it({ id: "b", createdAt: 1 }), it({ id: "c", dueAt: 1 }), it({ id: "d", state: "failed" })];
  assert.deepEqual(holding(l, 5_000).map((x) => x.id), ["b", "a"]);
});

test("launch prompt and sidebar attention", () => {
  const l = [it({ state: "interrupted" }), it({ state: "unknown" }), it({ state: "failed" }), it({ state: "holding" }), it({ state: "sending" })];
  assert.equal(interrupted(l).length, 1);
  assert.equal(needsAttention(l), 3);
});

test("recipients are abbreviated", () => {
  assert.equal(recipients(it({ to: [] })), "(no recipient)");
  assert.equal(recipients(it({ to: ["a@b.c", "d@e.f", "g@h.i", "j@k.l"] })), "a@b.c, d@e.f +2");
});

test("descriptions say what happens next, in plain words", () => {
  assert.match(describe(it(), 5_000), /Sending in 5 s/);
  assert.match(describe(it({ state: "failed", nextRetryAt: 35_000, failures: 1 }), 5_000), /again in 30 s/);
  assert.match(describe(it({ state: "failed", nextRetryAt: null, failures: 4 }), 5_000), /used up/);
  assert.match(describe(it({ state: "unknown" }), 0), /Sent folder/);
  assert.match(describe(it({ state: "interrupted" }), 0), /closed/);
});

test("a message that is being sent offers no actions (exactly-once)", () => {
  assert.deepEqual(actions(it({ state: "sending" })), { undo: false, retry: false, edit: false, discard: false });
  assert.deepEqual(actions(it({ state: "holding" })), { undo: true, retry: false, edit: false, discard: false });
  for (const s of ["failed", "unknown", "interrupted"]) assert.deepEqual(actions(it({ state: s })), { undo: false, retry: true, edit: true, discard: true });
});

test("restore payload parsing is defensive and drops the forward marker", () => {
  assert.equal(parseRestore("nope"), null);
  assert.equal(parseRestore("[1]"), null);
  assert.equal(parseRestore("null"), null);
  const p = parseRestore('{"to":"a","fwd":{"uid":1},"seedHtml":"<p>x</p>","html":"<p>y</p>"}');
  assert.deepEqual(restoredPayload(p), { account: null, to: "a", cc: "", bcc: "", subject: "", text: "", inReplyTo: null, showCc: false, html: "<p>y</p>" });
});

test("buildRestore keeps everything when small, drops pictures then rich body when large, never refuses", () => {
  const base = { to: "a@b.c", subject: "s", text: "hello" };
  const pic = '<img src="data:image/png;base64,' + "A".repeat(1000) + '"><p>hi</p>';
  const small = buildRestore(base, pic, 1, 10_000);
  assert.equal(small.inlineCount, 1);
  assert.ok(small.restore.includes("AAAA"));
  const noPics = buildRestore(base, pic, 1, 600);
  assert.equal(noPics.inlineCount, 0);
  assert.ok(!noPics.restore.includes("AAAA") && noPics.restore.includes("<p>hi</p>"));
  // Big text beyond the limit even without pictures: plain, truncated, still valid JSON under the limit.
  const hugeText = { ...base, text: "x".repeat(2_000_000) };
  const r = buildRestore(hugeText, "<p>" + "y".repeat(2_000_000) + "</p>", 0);
  assert.ok(new TextEncoder().encode(r.restore).length <= 900_000);
  const v = JSON.parse(r.restore);
  assert.equal(v.mode, "plain");
  assert.equal(v.to, "a@b.c");
  assert.ok(!("html" in v));
  // Multi-byte text is measured in bytes, not characters.
  const multi = buildRestore({ ...base, text: "\u00e9".repeat(500_000) }, undefined, 0);
  assert.ok(new TextEncoder().encode(multi.restore).length <= 900_000);
});

test("scheduled entries: described, never undoable, can be sent now / edited / discarded", () => {
  const it = { id: "o1", account: "a", to: ["b@x.org"], subject: "s", state: "scheduled" as const, error: null, dueAt: 5, nextRetryAt: null, failures: 0, attachments: 0, createdAt: 1, scheduledAt: 5 };
  assert.match(describe(it, 1), /^Scheduled for .*only while MiraMail is open/);
  assert.deepEqual(actions(it), { undo: false, retry: true, edit: true, discard: true });
  assert.equal(needsAttention([it]), 0, "a message waiting for its time is not an alarm");
  assert.match(describe({ ...it, state: "interrupted" }, 1), /scheduled for .*not running/);
});
