// @ts-nocheck -- run with: node --test src/lib/trackers.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { summarize, warningFor, bannerText, imageStatus, sizeLabel } from "./trackers.ts";

const img = (verdict, extra = {}) => ({ url: "https://h.example/x", host: "h.example", width: null, height: null, hidden: false, verdict, reasons: [], tracker: null, ...extra });

test("summarize counts and dedupes services", () => {
  const s = summarize([img("tracker", { tracker: "Mailchimp open tracking" }), img("tracker", { tracker: "Mailchimp open tracking" }), img("suspicious"), img("clean")]);
  assert.deepEqual(s, { total: 4, trackers: 2, suspicious: 1, clean: 1, services: ["Mailchimp open tracking"] });
});

test("warning wording: tracker vs suspicious vs none", () => {
  assert.equal(warningFor(summarize([img("clean")])), null);
  const t = warningFor(summarize([img("tracker", { tracker: "SendGrid open tracking" })]));
  assert.equal(t.level, "tracker");
  assert.match(t.headline, /may be tracking you/);
  assert.match(t.body, /1 image looks like a tracking pixel \(SendGrid open tracking\)/);
  assert.match(t.body, /IP address/);
  const w = warningFor(summarize([img("suspicious"), img("suspicious")]));
  assert.equal(w.level, "suspicious");
  assert.match(w.headline, /could be tracking you/);
  assert.doesNotMatch(w.body, /tracking pixel/);
});

test("banner text", () => {
  assert.equal(bannerText(3, summarize([img("tracker"), img("clean")])), "3 remote images blocked — 1 looks like a tracker");
  assert.equal(bannerText(1, summarize([img("clean")])), "1 remote image blocked");
});

test("status after loading", () => {
  assert.equal(imageStatus(img("tracker"), false, true), "Not loaded");
  assert.equal(imageStatus(img("tracker"), true, true), "Skipped (tracker)");
  assert.equal(imageStatus(img("tracker"), true, false), "Loaded");
  assert.equal(imageStatus(img("suspicious"), true, true), "Loaded");
});

test("size label", () => {
  assert.equal(sizeLabel(img("clean", { width: 1, height: 1 })), "1×1");
  assert.equal(sizeLabel(img("clean")), "size unknown");
});
