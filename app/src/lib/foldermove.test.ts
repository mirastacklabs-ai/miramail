// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/foldermove.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { movedName, moveTargets, canDropFolder, isMovable, personalPrefix } from "./foldermove.ts";

const F = (name, role = "other", delimiter = "/") => ({ name, role, selectable: true, delimiter });
const tree = [F("INBOX", "inbox"), F("Trash", "trash"), F("Projects"), F("Projects/Alpha"), F("Projects/Alpha/Deep"), F("Misc"), F("Archive2")];
const by = (n, t = tree) => t.find((f) => f.name === n);

test("destinations exclude itself, descendants, current parent and name clashes", () => {
  const t = moveTargets(tree, by("Projects/Alpha"));
  assert.equal(t.top, true);
  assert.deepEqual(t.folders.map((f) => f.name).sort(), ["Archive2", "INBOX", "Misc", "Trash"].sort());
  const top = moveTargets(tree, by("Misc"));
  assert.equal(top.top, false, "already top level");
  assert.ok(!top.folders.some((f) => f.name === "Misc"));
  const clash = [...tree, F("Misc/Alpha")];
  assert.ok(!moveTargets(clash, by("Projects/Alpha", clash)).folders.some((f) => f.name === "Misc"));
  assert.ok(!moveTargets([...tree, F("alpha")], by("Projects/Alpha")).top, "case-insensitive clash at top");
});

test("special folders and folders without a delimiter cannot move", () => {
  assert.equal(isMovable(tree, by("INBOX")), false);
  assert.equal(isMovable(tree, by("Trash")), false);
  assert.equal(isMovable(tree, by("Misc")), true);
  const nd = [F("INBOX", "inbox"), { name: "A", role: "other", selectable: true, delimiter: null }, F("B")];
  assert.equal(isMovable(nd, nd[1]), false);
  assert.equal(canDropFolder(tree, by("INBOX"), "Misc"), false);
  assert.equal(isMovable([...tree, F("Misc/Sent", "sent")], by("Misc")), false, "holds a role folder");
});

test("drop validity", () => {
  const a = by("Projects");
  assert.equal(canDropFolder(tree, a, "Projects"), false);
  assert.equal(canDropFolder(tree, a, "Projects/Alpha/Deep"), false);
  assert.equal(canDropFolder(tree, a, "Misc"), true);
  assert.equal(canDropFolder(tree, a, null), false, "already top level");
  assert.equal(canDropFolder(tree, by("Projects/Alpha"), null), true);
  assert.equal(canDropFolder(tree, by("Projects/Alpha"), "Projects"), false, "same parent");
  assert.equal(movedName(tree, by("Projects/Alpha"), "Misc"), "Misc/Alpha");
});

test("INBOX-prefixed namespace keeps the prefix at the top level", () => {
  const d = [F("INBOX", "inbox", "."), F("INBOX.Sent", "sent", "."), F("INBOX.Work", "other", "."), F("INBOX.Work.Q1", "other", "."), F("INBOX.Misc", "other", ".")];
  assert.equal(personalPrefix(d), "INBOX.");
  assert.equal(movedName(d, d[3], null), "INBOX.Q1");
  assert.equal(canDropFolder(d, d[4], null), false);
  assert.equal(canDropFolder(d, d[4], "INBOX"), false, "INBOX is the same as top level there");
  assert.equal(movedName(d, d[4], "INBOX.Work"), "INBOX.Work.Misc");
  assert.equal(personalPrefix(tree), null);
});
