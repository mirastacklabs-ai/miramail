// @ts-nocheck -- run with: node --test src/lib/aimd.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { parseMarkdown, parseInline, safeUrl, stripUnsafe } from "./aimd.ts";

const flat = (inl) => inl.map((n) => (n.t === "text" || n.t === "code" ? n.v : n.c ? flat(n.c) : "")).join("");

test("strips bidi, zero-width and tag characters", () => {
  const s = "a‮b​c⁦d\u{E0041}e\u0007f﻿g";
  assert.equal(stripUnsafe(s), "abcdefg");
  assert.equal(flat(parseMarkdown("he‮llo")[0].c), "hello");
});
test("paragraphs, bold, italic, inline code", () => {
  const b = parseMarkdown("Hello **bold** and *it* and `x < y`.\n\nSecond");
  assert.equal(b.length, 2);
  assert.deepEqual(b[0].c.map((n) => n.t), ["text", "strong", "text", "em", "text", "code", "text"]);
  assert.equal(b[0].c[5].v, "x < y");
});
test("snake_case is not italic", () => {
  assert.deepEqual(parseInline("use my_var_name here").map((n) => n.t), ["text"]);
});
test("headings are their own block", () => {
  const b = parseMarkdown("## Title here\nbody");
  assert.equal(b[0].t, "h"); assert.equal(flat(b[0].c), "Title here"); assert.equal(b[1].t, "p");
});
test("fenced code, including unclosed while streaming", () => {
  const b = parseMarkdown("```js\nlet a = '<b>';\n```\nafter");
  assert.equal(b[0].t, "code"); assert.equal(b[0].lang, "js"); assert.equal(b[0].v, "let a = '<b>';");
  assert.equal(b[1].t, "p");
  assert.equal(parseMarkdown("```\nstill going")[0].v, "still going");
});
test("lists ordered and unordered with nesting depth", () => {
  const b = parseMarkdown("- a\n  - b\n- c");
  assert.equal(b[0].t, "list"); assert.equal(b[0].ordered, false);
  assert.deepEqual(b[0].items.map((x) => x.depth), [0, 1, 0]);
  assert.equal(parseMarkdown("1. one\n2. two")[0].ordered, true);
});
test("simple tables", () => {
  const b = parseMarkdown("| A | B |\n|---|---|\n| 1 | **2** |\n| 3 | 4 |");
  assert.equal(b[0].t, "table");
  assert.equal(b[0].head.length, 2); assert.equal(b[0].rows.length, 2);
  assert.equal(b[0].rows[0][1][0].t, "strong");
});
test("links: http/https/mailto only", () => {
  assert.equal(safeUrl("https://example.org/a b"), null);
  assert.equal(safeUrl("javascript:alert(1)"), null);
  assert.equal(safeUrl("data:text/html,<script>"), null);
  assert.equal(safeUrl("file:///etc/passwd"), null);
  assert.equal(safeUrl("tel:123"), null);
  assert.equal(safeUrl("mailto:a@b.example"), "mailto:a@b.example");
  assert.equal(safeUrl("https://exa​mple.org/"), "https://example.org/");
  const ok = parseInline("[site](https://example.org/x)")[0];
  assert.equal(ok.t, "link"); assert.equal(ok.href, "https://example.org/x");
  const bad = parseInline("[click](javascript:alert(1))");
  assert.ok(!bad.some((n) => n.t === "link")); assert.match(flat(bad), /link blocked/);
});
test("bare URLs and autolinks become links, trailing punctuation excluded", () => {
  const links = parseInline("see https://example.org/a. then <https://b.example/z>").filter((x) => x.t === "link");
  assert.deepEqual(links.map((l) => l.href), ["https://example.org/a", "https://b.example/z"]);
});
test("a label that looks like another URL keeps the real href", () => {
  const l = parseInline("[https://bank.example](https://evil.example/)")[0];
  assert.equal(l.href, "https://evil.example/");
});
test("images are never loaded", () => {
  const n = parseInline("![tracking pixel](https://t.example/p.gif)");
  assert.equal(flat(n), "[image: tracking pixel, not loaded]");
  assert.ok(!JSON.stringify(n).includes("t.example"));
});
test("raw HTML stays literal text", () => {
  const b = parseMarkdown("<img src=x onerror=alert(1)><script>alert(1)</script>");
  assert.equal(b[0].t, "p");
  assert.equal(flat(b[0].c), "<img src=x onerror=alert(1)><script>alert(1)</script>");
});
test("blockquote and hr", () => {
  const b = parseMarkdown("> quoted\n\n---");
  assert.equal(b[0].t, "quote"); assert.equal(b[1].t, "hr");
});
test("pathological input terminates", () => {
  const t = "*".repeat(5000) + "[".repeat(3000) + "`".repeat(2000) + "_a".repeat(3000);
  assert.ok(Array.isArray(parseMarkdown(t)));
});
test("no raw-html directive anywhere in components/ai", () => {
  const dir = new URL("../components/ai/", import.meta.url);
  const files = readdirSync(dir).filter((f) => f.endsWith(".svelte"));
  for (const f of files) assert.ok(!readFileSync(new URL(f, dir), "utf8").includes("{@" + "html"), f);
});
