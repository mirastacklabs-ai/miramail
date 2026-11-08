// @ts-nocheck -- run with: node --test src/lib/signin.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { cleanAddress } from "./signin.ts";

test("cleanAddress: a normal address is unchanged apart from surrounding whitespace", () => {
  assert.equal(cleanAddress("ann@example.org"), "ann@example.org");
  assert.equal(cleanAddress("  Ann.Lee+tag@Example.ORG \n"), "Ann.Lee+tag@Example.ORG"); // case is the backend's job
  assert.equal(cleanAddress("\tann@example.org\r\n"), "ann@example.org");
});

test("cleanAddress: invisible characters from a paste are removed anywhere in the address", () => {
  const z = ["​", "‌", "‍", "‎", "‏", "‪", "‫", "‬", "‭", "‮", "⁠", "﻿", " "];
  for (const c of z) {
    assert.equal(cleanAddress(`${c}ann@exa${c}mple.org${c}`), "ann@example.org", `U+${c.charCodeAt(0).toString(16)}`);
  }
  // A BOM + no-break spaces around the address (copied from a PDF) still leave a clean address.
  assert.equal(cleanAddress("﻿  ann@example.org  "), "ann@example.org");
  // An RTL override cannot disguise the domain.
  assert.equal(cleanAddress("ann@‮gro.elpmaxe"), "ann@gro.elpmaxe");
});

test("cleanAddress: empty and whitespace-only input become empty (the form then asks for an address)", () => {
  for (const s of ["", "   ", "​​", " ﻿ \t"]) assert.equal(cleanAddress(s), "");
});

test("cleanAddress: visible characters, unicode and odd input are never altered or dropped", () => {
  assert.equal(cleanAddress("jürgen@münchen.de"), "jürgen@münchen.de");
  assert.equal(cleanAddress("a b@example.org"), "a b@example.org"); // inner space is left for validation to reject
  assert.equal(cleanAddress("<ann@example.org>"), "<ann@example.org>");
  const long = "a".repeat(10_000) + "@example.org";
  assert.equal(cleanAddress(long), long);
});

test("cleanAddress: other invisible format characters are currently NOT removed", () => {
  // Pins today's behaviour (kept identical when this was moved out of Onboarding.svelte). Soft hyphen, invisible
  // operators and bidi isolates pass through, and the backend then fails sign-in with a generic error.
  // Recorded as a gap in docs/TEST_PLAN.md (G1).
  for (const c of ["­", "⁡", "⁦", "⁩"]) assert.equal(cleanAddress(`ann${c}@example.org`), `ann${c}@example.org`);
});

test("Onboarding.svelte uses cleanAddress for the address it submits", () => {
  const src = readFileSync(new URL("../components/Onboarding.svelte", import.meta.url), "utf8");
  assert.match(src, /import \{ cleanAddress \} from "\.\.\/lib\/signin"/);
  assert.match(src, /const addr = cleanAddress\(email\);/);
  assert.ok(!/email\.replace\(/.test(src), "inline stripping came back");
});
