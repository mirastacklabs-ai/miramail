// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/*.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { describeIdentities, planDefaults, planIdentities, planSignatures } from "./t2restore.ts";
import { splitAddrs } from "./util.ts";

const sig = (id, text = "hi") => ({ id, name: "S", rich: false, text, html: "", placement: "below", accounts: [] });
const ident = (id, replyTo = "", signatureId = null) => ({ id, account: "a@x.org", name: "N", address: `${id}@x.org`, replyTo, signatureId });

test("signature with an existing id is never overwritten", () => {
  const plan = planSignatures([sig("s1", "changed"), sig("s2"), sig("s3")], [sig("s1", "mine"), sig("s2")]);
  assert.deepEqual(plan.map((p) => p.kind), ["add-fresh", "skip", "add"]);
  assert.equal(plan[0].sig.id, "", "fresh id requested");
  assert.equal(plan[2].sig.id, "s3");
});

test("identities: reply-to cleared by default, existing ids skipped, signature ids remapped", () => {
  const map = new Map([["s1", "s9"]]);
  const inc = [ident("i1", "boss@evil.test", "s1"), ident("i2", "r@x.org", "gone"), ident("i3")];
  const out = planIdentities(inc, ["i3"], ["a@x.org"], map, false);
  assert.deepEqual(out.map((i) => [i.id, i.replyTo, i.signatureId]), [["i1", "", "s9"], ["i2", "", null]]);
  const kept = planIdentities(inc, [], ["a@x.org"], map, true);
  assert.equal(kept[0].replyTo, "boss@evil.test");
  assert.equal(planIdentities(inc, [], ["other@x.org"], map, true).length, 0);
});

test("defaults are remapped and dangling references dropped", () => {
  const d = planDefaults([{ account: "a@x.org", newId: "s1", replyId: "zzz" }, { account: "b@x.org", newId: null, replyId: null }], ["a@x.org"], new Map([["s1", "s9"]]));
  assert.deepEqual(d, [{ account: "a@x.org", newId: "s9", replyId: null }]);
});

test("preview describes identities and flags reply-to", () => {
  const r = describeIdentities([ident("i1", "x@y.org"), { name: "", address: "b@x.org", replyTo: "" }]);
  assert.equal(r.hasReplyTo, true);
  assert.match(r.lines[0], /i1@x\.org.*x@y\.org/);
  assert.equal(describeIdentities("nope").lines.length, 0);
});

test("recipient parser keeps a quoted display name with a comma or escaped quotes in one piece", () => {
  assert.deepEqual(splitAddrs('"Doe, John" <j@x.org>, b@x.org'), ['"Doe, John" <j@x.org>', "b@x.org"]);
  assert.deepEqual(splitAddrs('"Say \\"hi\\", Bob" <b@x.org>; c@x.org'), ['"Say \\"hi\\", Bob" <b@x.org>', "c@x.org"]);
});
