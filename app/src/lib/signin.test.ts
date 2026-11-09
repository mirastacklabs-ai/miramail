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

test("cleanAddress: every other invisible format character is removed too (G1)", () => {
  const cps = [0x00ad, 0x034f, 0x061c, 0x115f, 0x1160, 0x17b4, 0x17b5, 0x180b, 0x180c, 0x180d, 0x180e, 0x180f, 0x2061, 0x2062, 0x2063, 0x2064,
    0x2066, 0x2067, 0x2068, 0x2069, 0x206a, 0x206f, 0x3164, 0xfe00, 0xfe0f, 0xffa0, 0xe0001, 0xe0041, 0xe0100, 0xe01ef];
  for (const cp of cps) {
    const c = String.fromCodePoint(cp);
    assert.equal(cleanAddress(`${c}ann${c}@exa${c}mple.org${c}`), "ann@example.org", `U+${cp.toString(16)}`);
  }
  // Variation selectors / tag characters do not damage the surrounding (valid) non-ASCII text.
  assert.equal(cleanAddress("jürgen\u{E0100}@münchen.de"), "jürgen@münchen.de");
  assert.equal(cleanAddress("\u{1F600}@example.org"), "\u{1F600}@example.org"); // an emoji itself is not touched
  assert.equal(cleanAddress("josé@example.org"), "josé@example.org");
});

test("Onboarding.svelte uses cleanAddress for the address it submits", () => {
  const src = readFileSync(new URL("../components/Onboarding.svelte", import.meta.url), "utf8");
  assert.match(src, /import \{ cleanAddress \} from "\.\.\/lib\/signin"/);
  assert.match(src, /const addr = cleanAddress\(email\);/);
  assert.ok(!/email\.replace\(/.test(src), "inline stripping came back");
});
