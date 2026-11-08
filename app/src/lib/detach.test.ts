// @ts-nocheck -- run with: node --test src/lib/detach.test.ts (Node type stripping; no extra dependencies)
import { test } from "node:test";
import assert from "node:assert/strict";
import * as D from "./detach.ts";

const compose = { account: "me@x.org", to: "a@b.c", cc: "", bcc: "", subject: "Hi", text: "body\nline 2", inReplyTo: { messageId: "<m@x>", references: null }, showCc: true, base: "[[],[],\"\",\"\"]", aiDraft: true, aiDraftId: 7, aiWarnings: ["w"] };
const event = { init: { uid: "u1", calendarId: "c1", start: 1700000000000, end: 1700003600000, allDay: false }, form: { summary: "Lunch" }, created: ["z1"], removed: [], base: "snap" };

test("entry detection: hash, injected id, hostile values", () => {
  assert.deepEqual(D.parseEntry("", undefined), { mode: "main" });
  assert.deepEqual(D.parseEntry("#other", undefined), { mode: "main" });
  assert.deepEqual(D.parseEntry("#pane=c-abc_1", undefined), { mode: "pane", id: "c-abc_1" });
  assert.deepEqual(D.parseEntry("#x", "aichat"), { mode: "pane", id: "aichat" }, "injected id wins");
  for (const bad of ["#pane=", "#pane=a/b", "#pane=a b", "#pane=..%2Fx", "#pane=%E0%A4%A", "#pane=" + "a".repeat(65), "#pane=a&pane=b", "#pane=<script>"]) {
    assert.deepEqual(D.parseEntry(bad, undefined), { mode: "invalid" }, bad);
  }
  for (const bad of ["", 5, {}, "a/b", "a".repeat(65)]) assert.deepEqual(D.parseEntry("", bad), { mode: "invalid" });
  assert.deepEqual(D.parseEntry(null, undefined), { mode: "main" });
});

test("only compose / event / aichat are detachable", () => {
  for (const k of ["compose", "event", "aichat"]) assert.ok(D.isDetachable(k));
  for (const k of ["mailbox", "reader", "calendar", "", null, undefined, 3, "Compose"]) assert.ok(!D.isDetachable(k), String(k));
  assert.equal(D.makeEnvelope("mailbox", "t", false, {}), null);
});

test("compose payload round trip", () => {
  const e = D.makeEnvelope("compose", "Hi", true, compose);
  const back = D.parseEnvelope(D.serializeEnvelope(e));
  assert.deepEqual(back, e);
  assert.deepEqual(back.payload, compose);
  assert.equal(back.dirty, true);
});

test("event payload round trip", () => {
  const e = D.makeEnvelope("event", "Lunch", true, event);
  assert.deepEqual(D.parseEnvelope(D.serializeEnvelope(e)).payload, event);
  const fresh = D.makeEnvelope("event", "New event", false, { init: {}, form: null, created: [], removed: [] });
  assert.deepEqual(D.parseEnvelope(JSON.stringify(fresh)).payload, { init: {}, form: null, created: [], removed: [] });
});

test("ai payload round trip is the conversation id only", () => {
  const e = D.makeEnvelope("aichat", "AI chat", false, { conversationId: "c-abc123.x", extra: "dropped" });
  assert.deepEqual(D.parseEnvelope(D.serializeEnvelope(e)).payload, { conversationId: "c-abc123.x" });
  assert.deepEqual(D.makeEnvelope("aichat", "AI", false, {}).payload, { conversationId: null });
  assert.equal(D.makeEnvelope("aichat", "AI", false, { conversationId: "a b" }), null);
  assert.equal(D.makeEnvelope("aichat", "AI", false, { conversationId: 5 }), null);
});

test("hostile / corrupt envelopes are rejected, unknown keys stripped", () => {
  for (const bad of ["", "nope", "[]", "null", "5", '{"v":2}', '{"v":1,"kind":"compose","title":"t","dirty":false,"payload":"x"}',
    '{"v":1,"kind":"mailbox","title":"t","dirty":false,"payload":{}}',
    '{"v":1,"kind":"compose","title":"t","dirty":"no","payload":{}}',
    '{"v":1,"kind":"compose","title":"t","dirty":false,"payload":{"to":1}}',
    '{"v":1,"kind":"event","title":"t","dirty":false,"payload":{"init":{"start":"x"},"created":[],"removed":[]}}',
    '{"v":1,"kind":"event","title":"t","dirty":false,"payload":{"init":{},"created":[1],"removed":[]}}']) {
    assert.equal(D.parseEnvelope(bad), null, bad);
  }
  assert.equal(D.parseEnvelope("x".repeat(2 * 1024 * 1024)), null);
  const polluted = JSON.parse('{"v":1,"kind":"compose","title":"t","dirty":false,"payload":{"__proto__":{"evil":1},"account":null,"to":"","cc":"","bcc":"","subject":"","text":"","base":"","evil":"x"}}');
  const e = D.parseEnvelope(polluted);
  assert.ok(e);
  assert.equal(e.payload.evil, undefined);
  assert.equal(({}).evil, undefined);
  assert.equal(D.parseEnvelope({ v: 1, kind: "aichat", title: "t".repeat(500), dirty: false, payload: {} }).title.length, 200);
});

test("drag-out threshold", () => {
  assert.equal(D.outsideWindow(100, 100, 800, 600), false);
  assert.equal(D.outsideWindow(-10, 100, 800, 600), false, "inside the 24px slack");
  assert.equal(D.outsideWindow(-25, 100, 800, 600), true);
  assert.equal(D.outsideWindow(100, -25, 800, 600), true);
  assert.equal(D.outsideWindow(825, 100, 800, 600), true);
  assert.equal(D.outsideWindow(824, 100, 800, 600), false);
  assert.equal(D.outsideWindow(100, 625, 800, 600), true);
  assert.equal(D.outsideWindow(-1, 0, 800, 600, 0), true, "attach uses threshold 0");
  assert.equal(D.outsideWindow(NaN, 0, 800, 600), false);
  assert.equal(D.outsideWindow(Infinity, 0, 800, 600), false);
});

test("detach geometry puts the pointer on the new header and clamps size", () => {
  assert.deepEqual(D.detachGeometry(1000, 500, 600, 400), { x: 840, y: 458, w: 600, h: 400 });
  assert.deepEqual(D.detachGeometry(1000, 500), { x: 840, y: 458, w: 720, h: 560 });
  const tiny = D.detachGeometry(0, 0, 10, 10);
  assert.equal(tiny.w, D.MIN_W); assert.equal(tiny.h, D.MIN_H);
  const huge = D.detachGeometry(0, 0, 1e9, 1e9);
  assert.equal(huge.w, D.MAX_W); assert.equal(huge.h, D.MAX_H);
  assert.deepEqual(D.detachGeometry(NaN, Infinity, NaN, NaN), { x: -160, y: -42, w: 720, h: 560 });
  const c = D.cascadeGeometry(50, 60, 500, 400);
  assert.ok(c.x > 50 && c.y > 60);
});

test("dock zones: centre, four edges, outside, degenerate", () => {
  const r = { l: 100, t: 100, w: 400, h: 200 };
  assert.equal(D.zoneAt(r, 300, 200), "center");
  assert.equal(D.zoneAt(r, 105, 200), "left");
  assert.equal(D.zoneAt(r, 495, 200), "right");
  assert.equal(D.zoneAt(r, 300, 102), "top");
  assert.equal(D.zoneAt(r, 300, 298), "bottom");
  assert.equal(D.zoneAt(r, 99, 200), null);
  assert.equal(D.zoneAt(r, 500, 200), null);
  assert.equal(D.zoneAt({ l: 0, t: 0, w: 0, h: 10 }, 0, 0), null);
  assert.equal(D.zoneAt(r, NaN, 200), null);
  assert.deepEqual(D.zoneBox(r, "right"), { l: 300, t: 100, w: 200, h: 200 });
  assert.deepEqual(D.zoneBox(r, "bottom"), { l: 100, t: 200, w: 400, h: 100 });
  assert.deepEqual(D.zoneBox(r, "center"), r);
});

test("dock spec for an attach: first non-excluded pane under the point", () => {
  const boxes = [{ id: "mailbox", l: 0, t: 0, w: 300, h: 600 }, { id: "reader", l: 300, t: 0, w: 700, h: 600 }];
  assert.deepEqual(D.dockAt(boxes, 150, 300), { target: "mailbox", zone: "center" });
  assert.deepEqual(D.dockAt(boxes, 990, 300), { target: "reader", zone: "right" });
  assert.deepEqual(D.dockAt(boxes, 150, 300, "mailbox"), null);
  assert.equal(D.dockAt(boxes, 2000, 2000), null);
  assert.deepEqual(D.screenToClient(1500, 400, 1200, 100), { x: 300, y: 300 });
});

test("attach message validation", () => {
  const env = D.makeEnvelope("compose", "Hi", true, compose);
  const ok = D.parseAttach({ paneId: "c-1", envelope: JSON.stringify(env), dock: { target: "reader", zone: "left" }, at: null, park: false });
  assert.deepEqual(ok.dock, { target: "reader", zone: "left" });
  assert.equal(ok.park, false);
  assert.deepEqual(D.parseAttach({ paneId: "c-1", envelope: env, dock: null, at: { x: 5, y: 6 }, park: true }).at, { x: 5, y: 6 });
  for (const bad of [null, 5, {}, { paneId: "a/b", envelope: env }, { paneId: "c-1", envelope: "{}" },
    { paneId: "c-1", envelope: env, dock: { target: "r", zone: "middle" } }, { paneId: "c-1", envelope: env, dock: { target: "../x", zone: "left" } },
    { paneId: "c-1", envelope: env, at: { x: "1", y: 2 } }, { paneId: "c-1", envelope: env, at: { x: NaN, y: 2 } }]) {
    assert.equal(D.parseAttach(bad), null, JSON.stringify(bad));
  }
});

test("persisted multi-window layout: valid records pass, hostile input is filtered", () => {
  const good = [
    { label: "main", focused: true, x: 0, y: 0, w: 1180, h: 760 },
    { label: "pane-c-1", paneId: "c-1", kind: "compose", focused: false, x: 100, y: 120, w: 700, h: 500 },
    { label: "pane-aichat", paneId: "aichat", kind: "aichat", focused: false },
  ];
  const w = D.parseWindows(JSON.stringify(good));
  assert.equal(w.length, 3);
  assert.deepEqual(w[1].geo, { x: 100, y: 120, w: 700, h: 500 });
  assert.equal(w[2].geo, null);
  assert.deepEqual(D.detachedIds(w), ["c-1", "aichat"]);
  // corrupt / hostile
  for (const bad of ["", "nope", "{}", "null", "5", '{"a":1}', "[[[[", "x".repeat(300000)]) assert.deepEqual(D.parseWindows(bad), [], bad.slice(0, 20));
  const mixed = D.parseWindows([
    null, 5, "x", { label: "evil" }, { label: "pane-a/b", paneId: "a/b", kind: "compose" },
    { label: "pane-c-1", paneId: "other", kind: "compose" },          // label/pane mismatch
    { label: "pane-m", paneId: "m", kind: "mailbox" },               // not detachable
    { label: "pane-c-2", paneId: "c-2", kind: "compose", x: 1e12, y: 0, w: 10, h: 10 }, // absurd geometry -> geo dropped, record kept
    { label: "pane-c-3", paneId: "c-3", kind: "event", x: 0, y: 0, w: -5, h: 10 },
    { label: "pane-c-2", paneId: "c-2", kind: "compose" },           // duplicate label
    { label: "pane-d", paneId: "c-2", kind: "compose" },             // label mismatch
    { label: "main" }, { label: "main" },
  ]);
  assert.deepEqual(mixed.map((m) => m.label), ["pane-c-2", "pane-c-3", "main"]);
  assert.equal(mixed[0].geo, null);
  assert.equal(mixed[1].geo, null);
  const many = D.parseWindows(Array.from({ length: 500 }, (_, i) => ({ label: `pane-p${i}`, paneId: `p${i}`, kind: "aichat" })));
  assert.ok(many.length <= 32);
  assert.ok(D.validLabel("pane-x") && D.validLabel("main") && !D.validLabel("pane-") && !D.validLabel("pane-a b") && !D.validLabel(null));
});

test("composer editor state survives a pane handoff and bad values are dropped", () => {
  const base = { account: "me@x.org", to: "", cc: "", bcc: "", subject: "", text: "", inReplyTo: null, showCc: false, base: "[]" };
  const full = { ...base, html: "<div>x</div>", seedHtml: "<div>q</div>", mode: "rich", kind: "reply", sigId: "s1", sigApplied: true, from: "alias@x.org", fwd: { account: "me@x.org", folder: "INBOX", uid: 4, uidvalidity: 77, as: "inline", atts: [{ id: "2", name: "a.pdf", mime: "application/pdf", size: 5, extra: 1 }, { id: 3 }] } };
  const out = D.sanitizeCompose(full);
  assert.deepEqual({ ...out }, { ...base, html: "<div>x</div>", seedHtml: "<div>q</div>", mode: "rich", kind: "reply", sigId: "s1", sigApplied: true, from: "alias@x.org", fwd: { account: "me@x.org", folder: "INBOX", uid: 4, uidvalidity: 77, as: "inline", atts: [{ id: "2", name: "a.pdf", mime: "application/pdf", size: 5 }] } });
  const bad = D.sanitizeCompose({ ...base, mode: "evil", kind: 1, html: 5, from: 3, fwd: { account: "a", folder: "b", uid: "1", as: "inline", atts: [] } });
  for (const k of ["mode", "kind", "html", "from", "fwd"]) assert.ok(!(k in bad), k);
});

// ---- moving between windows ----
import { moveBlock } from "./detach.ts";
import { readFileSync } from "node:fs";
{
  const base = { desktop: true, kind: "compose", attachments: 0, holding: false, dialogOpen: false, dir: "to-window" as const };
  test("moveBlock: allowed, then each blocker", () => {
    assert.equal(moveBlock(base), null);
    assert.ok(moveBlock({ ...base, desktop: false }));
    assert.ok(moveBlock({ ...base, kind: "mailbox" }));
    assert.equal(moveBlock({ ...base, kind: "mailbox", dir: "to-main" }), null, "kind only matters when leaving main");
    assert.match(moveBlock({ ...base, holding: true })!, /being sent/);
    assert.ok(moveBlock({ ...base, dialogOpen: true }));
    assert.match(moveBlock({ ...base, attachments: 2 })!, /Attached files/);
    assert.ok(moveBlock({ ...base, attachments: 1, dir: "to-main" }), "attach back is refused too");
  });
  test("api.ts listens through the window-scoped listener for targeted events", () => {
    const src = readFileSync(new URL("./api.ts", import.meta.url), "utf8");
    const body = (name: string) => src.slice(src.indexOf(`function ${name}`), src.indexOf("\n}\n", src.indexOf(`function ${name}`)));
    for (const fn of ["onClosePane", "onWsEvent"]) {
      assert.ok(body(fn).includes("listenSelf"), fn);
      assert.ok(!/[^.\w]listen[<(]/.test(body(fn)), `${fn} must not use the global listen()`);
    }
    assert.ok(src.includes("getCurrentWebviewWindow().listen"));
  });
}
