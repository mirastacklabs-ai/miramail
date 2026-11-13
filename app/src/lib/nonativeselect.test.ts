// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/nonativeselect.test.ts
// Guard: macOS draws native popups itself (unbrandable): <select>, date/time/colour inputs, <datalist>.
// Use components/Select.svelte, DateTimeSelect.svelte, ComboInput.svelte and the palette in RichEditor instead.
import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

function walk(dir, out = []) {
  for (const n of readdirSync(dir)) {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (p.endsWith(".svelte")) out.push(p);
  }
  return out;
}
const root = new URL("..", import.meta.url).pathname;
const read = (f) => readFileSync(f, "utf8");

test("no .svelte file uses a native <select>", () => {
  const bad = walk(root).filter((f) => !f.endsWith("components/Select.svelte") && /<select[\s>]/.test(read(f)));
  assert.deepEqual(bad, []);
});

test("no .svelte file uses a native date/time/colour input", () => {
  // Any <input ...> whose type attribute (any order, quoted or not) is one of the popup types.
  const re = /<input\b[^>]*?\btype\s*=\s*(?:"|')?\s*(date|time|datetime-local|color|month|week)\b/i;
  assert.deepEqual(walk(root).filter((f) => re.test(read(f))), []);
});

test("no .svelte file uses a native <datalist>", () => {
  assert.deepEqual(walk(root).filter((f) => /<datalist[\s>]/i.test(read(f))), []);
});

test("a form with an email input has novalidate (no native validation bubble)", () => {
  const bad = walk(root).filter((f) => { const t = read(f); return /<input\b[^>]*\btype\s*=\s*["']?email/i.test(t) && /<form\b/i.test(t) && !/<form\b[^>]*\bnovalidate\b/i.test(t); });
  assert.deepEqual(bad, []);
});

test("no native alert / confirm / prompt in components or lib", () => {
  const files = (dir) => { const out = []; const w = (d) => { for (const n of readdirSync(d)) { const p = join(d, n); if (statSync(p).isDirectory()) w(p); else if (/\.(svelte|ts)$/.test(p) && !/\.test\.ts$/.test(p)) out.push(p); } }; w(dir); return out; };
  const strip = (t) => t.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "").replace(/\s\/\/ .*$/gm, "");
  const re = /(?:window\.)?(?<![\w.$])(?:alert|confirm|prompt)\(/;
  const bad = files(root).filter((f) => re.test(strip(read(f))));
  assert.deepEqual(bad, []);
});
