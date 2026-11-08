// @ts-nocheck -- run with: node --test src/lib/prefs-model.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { DEFAULTS, validate, parse, serialize, cssVars, bodyBaseCss, STORAGE_KEY } from "./prefs-model.ts";

test("defaults are valid and stable", () => {
  assert.deepEqual(validate(DEFAULTS), { ...DEFAULTS });
  assert.deepEqual(validate(undefined), { ...DEFAULTS });
  assert.deepEqual(validate(null), { ...DEFAULTS });
  assert.deepEqual(validate([1, 2]), { ...DEFAULTS });
  assert.match(STORAGE_KEY, /prefs/);
});

test("numbers are clamped and snapped", () => {
  assert.equal(validate({ bodySize: 5 }).bodySize, 12);
  assert.equal(validate({ bodySize: 99 }).bodySize, 22);
  assert.equal(validate({ bodySize: 15.6 }).bodySize, 16);
  assert.equal(validate({ lineHeight: 0.5 }).lineHeight, 1.3);
  assert.equal(validate({ lineHeight: 9 }).lineHeight, 2);
  assert.equal(validate({ lineHeight: 1.62 }).lineHeight, 1.6);
  assert.equal(validate({ bodySize: "16" }).bodySize, DEFAULTS.bodySize);
  assert.equal(validate({ bodySize: NaN }).bodySize, DEFAULTS.bodySize);
  assert.equal(validate({ lineHeight: Infinity }).lineHeight, DEFAULTS.lineHeight);
});

test("enums and booleans fall back per key, valid keys survive", () => {
  const p = validate({ uiFont: "comic", density: "spacious", snippetLines: 3, showDot: "yes", markRead: "never", afterRemove: 7, dateFormat: "absolute" });
  assert.equal(p.uiFont, DEFAULTS.uiFont);
  assert.equal(p.density, "spacious");
  assert.equal(p.snippetLines, DEFAULTS.snippetLines);
  assert.equal(p.showDot, DEFAULTS.showDot);
  assert.equal(p.markRead, "never");
  assert.equal(p.afterRemove, DEFAULTS.afterRemove);
  assert.equal(p.dateFormat, "absolute");
  assert.equal(validate({ snippetLines: 0 }).snippetLines, 0);
});

test("corrupt storage falls back to defaults", () => {
  for (const s of [null, undefined, "", "{", "not json", "42", "null", '"x"', "[1]"]) assert.deepEqual(parse(s), { ...DEFAULTS });
});

test("unknown keys are dropped (migration) and version tag is ignored", () => {
  const p = parse(JSON.stringify({ v: 0, legacyTheme: "x", bodySize: 18, __proto__: { bodySize: 1 }, extra: { a: 1 } }));
  assert.equal(p.bodySize, 18);
  assert.deepEqual(Object.keys(p).sort(), Object.keys(DEFAULTS).sort());
  assert.equal("legacyTheme" in p, false);
});

test("serialize round-trips", () => {
  const p = validate({ uiSize: "l", bodyFont: "serif", lineHeight: 1.8, plainMono: false });
  assert.deepEqual(parse(serialize(p)), p);
  assert.equal(JSON.parse(serialize(p)).v, 1);
});

test("css vars map prefs", () => {
  const v = cssVars(validate({ uiSize: "s", density: "compact", snippetLines: 2, plainMono: false, bodyFont: "serif" }));
  assert.equal(v["--ui-size"], "14px");
  assert.equal(v["--row-py"], "6px");
  assert.equal(v["--snip-lines"], "2");
  assert.match(v["--body-font"], /Georgia/);
  assert.match(cssVars(DEFAULTS)["--body-font"], /JetBrains Mono/);
});

test("iframe base style carries font, size, line height and no webfont", () => {
  const css = bodyBaseCss(validate({ bodyFont: "mono", bodySize: 18, lineHeight: 1.7 }));
  assert.match(css, /18px\/1\.7/);
  assert.match(css, /ui-monospace/);
  assert.doesNotMatch(css, /JetBrains/);
  assert.doesNotMatch(css, /url\(|@import/);
});

test("composing prefs: undo-send delay and drafts mirror", () => {
  assert.equal(DEFAULTS.undoSendSecs, 5);
  assert.equal(DEFAULTS.mirrorDrafts, true);
  for (const n of [0, 5, 10, 30]) assert.equal(validate({ undoSendSecs: n }).undoSendSecs, n);
  for (const bad of [7, -1, "5", null, NaN, 60]) assert.equal(validate({ undoSendSecs: bad }).undoSendSecs, 5);
  assert.equal(validate({ mirrorDrafts: false }).mirrorDrafts, false);
  assert.equal(validate({ mirrorDrafts: "no" }).mirrorDrafts, true);
  assert.equal(DEFAULTS.toneMeter, true);
  assert.equal(validate({ toneMeter: false }).toneMeter, false);
  assert.equal(validate({ toneMeter: "x" }).toneMeter, true);
  // Old stored prefs without the new keys get the defaults.
  const old = parse(JSON.stringify({ v: 1, uiFont: "system" }));
  assert.equal(old.undoSendSecs, 5);
  assert.equal(old.uiFont, "system");
});

test("threading is an opt-in boolean", () => {
  assert.equal(DEFAULTS.threading, false);
  assert.equal(validate({ threading: true }).threading, true);
  assert.equal(validate({ threading: "yes" }).threading, false);
  assert.equal(parse(JSON.stringify({ v: 1 })).threading, false);
});
