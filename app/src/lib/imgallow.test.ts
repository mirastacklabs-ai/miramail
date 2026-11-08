// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/*.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { addAllowed, isAllowed, MAX_ALLOWED, normAddr, parseAllow, removeAllowed } from "./imgallow.ts";

test("addresses are normalised and validated", () => {
  assert.equal(normAddr(" News@Example.COM "), "news@example.com");
  for (const bad of ["", null, undefined, "no-at", "a@b", "a b@c.de", "<a@b.cd>", "a@b.cd, c@d.ef", '"x"@y.zz']) assert.equal(normAddr(bad), null, String(bad));
});

test("allow, check, remove", () => {
  let l = addAllowed([], "News@Example.com");
  l = addAllowed(l, "news@example.com");
  assert.deepEqual(l, ["news@example.com"]);
  assert.ok(isAllowed(l, "NEWS@example.com"));
  assert.ok(!isAllowed(l, "other@example.com"));
  assert.ok(!isAllowed(l, null), "unknown sender is never allowed");
  assert.deepEqual(addAllowed(l, "garbage"), l);
  assert.deepEqual(removeAllowed(l, "News@example.com"), []);
});

test("storage parsing is defensive and capped", () => {
  assert.deepEqual(parseAllow("not json"), []);
  assert.deepEqual(parseAllow('{"a":1}'), []);
  assert.deepEqual(parseAllow('["a@b.cd", 5, null, "A@B.CD", "bad"]'), ["a@b.cd"]);
  const many = JSON.stringify(Array.from({ length: MAX_ALLOWED + 50 }, (_, i) => `u${i}@x.org`));
  assert.equal(parseAllow(many).length, MAX_ALLOWED);
});
