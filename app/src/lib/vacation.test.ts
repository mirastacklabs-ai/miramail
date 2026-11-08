// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/*.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { defaultSettings, parseAddresses, validate, overrideFor } from "./vacation.ts";

test("validate", () => {
  const s = defaultSettings("me@x.org");
  assert.equal(validate(s), "vac.err.message");
  s.message = "Away";
  assert.equal(validate(s), null);
  assert.equal(validate({ ...s, days: 0 }), "vac.err.days");
  assert.equal(validate({ ...s, days: 31 }), "vac.err.days");
  assert.equal(validate({ ...s, start: "2026-12-20", end: "2026-12-01" }), "vac.err.dates");
  assert.equal(validate({ ...s, addresses: [] }), "vac.err.addresses");
  assert.equal(validate({ ...s, addresses: [], onlyToMe: false }), null);
});

test("parseAddresses dedups and lowercases", () => {
  assert.deepEqual(parseAddresses("A@x.org, b@x.org;a@x.org  c@x.org"), ["a@x.org", "b@x.org", "c@x.org"]);
});

test("overrideFor only sends changes", () => {
  assert.equal(overrideFor({ host: "", port: "4190", tls: "starttls" }), undefined);
  assert.deepEqual(overrideFor({ host: " s.x.org ", port: "5190", tls: "implicit" }), { host: "s.x.org", port: 5190, tls: "implicit" });
  assert.equal(overrideFor({ host: "", port: "99999", tls: "starttls" }), undefined);
});
