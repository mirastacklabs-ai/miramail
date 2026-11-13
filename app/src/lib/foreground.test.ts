// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/foreground.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { ForegroundGate, isDeferredMsg } from "./foreground-model.ts";

const tickAll = () => new Promise((r) => setTimeout(r, 0));

test("a normal launch: ready and present at once, no 'enter' callback", async () => {
  const g = new ForegroundGate();
  let entered = 0;
  g.onEnter(() => entered++);
  assert.equal(g.known, false);
  assert.equal(g.present, false, "nothing that talks to a server runs before the first report");
  g.apply({ present: true, seq: 0 });
  await g.ready();
  await g.whenPresent();
  assert.equal(g.present, true);
  assert.equal(entered, 0);
});

test("a background launch holds network work until the user arrives", async () => {
  const g = new ForegroundGate();
  let entered = 0, ran = false;
  g.onEnter(() => entered++);
  g.apply({ present: false, seq: 0 });
  await g.ready();
  assert.equal(g.present, false);
  void g.whenPresent().then(() => (ran = true));
  await tickAll();
  assert.equal(ran, false);
  g.apply({ present: true, seq: 1 });
  await tickAll();
  assert.equal(ran, true);
  assert.equal(entered, 1, "the page catches up (folders, sync, unread counts)");
});

test("an answer older than an event already seen is ignored (listen first, then ask)", () => {
  const g = new ForegroundGate();
  g.apply({ present: true, seq: 3 }); // event
  g.apply({ present: false, seq: 2 }); // stale launch_state answer
  assert.equal(g.present, true);
  g.apply({ present: true, seq: 3 }); // same state again: harmless
  assert.equal(g.present, true);
});

test("leaving and coming back fires 'enter' each time; a repeat report does not", () => {
  const g = new ForegroundGate();
  let entered = 0;
  const off = g.onEnter(() => entered++);
  g.apply({ present: true, seq: 0 });
  g.apply({ present: false, seq: 1 });
  g.apply({ present: true, seq: 2 });
  g.apply({ present: true, seq: 2 });
  assert.equal(entered, 1);
  g.apply({ present: false, seq: 3 });
  g.apply({ present: true, seq: 4 });
  assert.equal(entered, 2);
  off();
  g.apply({ present: false, seq: 5 });
  g.apply({ present: true, seq: 6 });
  assert.equal(entered, 2, "unsubscribed");
});

test("the refusal text is recognised by its prefix", () => {
  assert.equal(isDeferredMsg("deferred: MiraMail is in the background"), true, "same text as presence.rs DEFERRED");
  assert.equal(isDeferredMsg("Couldn't load folders. Check your connection."), false);
});
