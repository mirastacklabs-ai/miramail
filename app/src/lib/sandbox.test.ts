// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/sandbox.test.ts
// Guard for CLAUDE.md non-negotiable 5 (HTML mail is untrusted: sandboxed render, JS off, remote images blocked by
// default). Source-level: every <iframe> in a .svelte file is sandboxed without script, form, popup or navigation
// rights, and mail HTML only reaches an iframe through buildSrcdoc (which puts the CSP first).
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
const files = walk(root).map((f) => [f.slice(root.length), readFileSync(f, "utf8")]);

test("every iframe is sandboxed and never gets scripts, forms, popups or top navigation", () => {
  let n = 0;
  for (const [f, src] of files) {
    for (const m of src.matchAll(/<iframe\b[^>]*>/gi)) {
      n++;
      const tag = m[0];
      const sb = /\bsandbox\s*=\s*"([^"]*)"/i.exec(tag);
      assert.ok(sb, `${f}: iframe without a sandbox attribute: ${tag}`);
      for (const bad of ["allow-scripts", "allow-forms", "allow-popups", "allow-top-navigation", "allow-modals", "allow-downloads"]) {
        assert.ok(!sb[1].includes(bad), `${f}: iframe sandbox grants ${bad}`);
      }
    }
  }
  assert.ok(n >= 2, "expected the Reader and print iframes");
});

test("message HTML reaches an iframe only through buildSrcdoc", () => {
  for (const [f, src] of files) {
    if (!/<iframe\b[^>]*\bsrcdoc/i.test(src)) continue;
    assert.match(src, /const srcdoc = \$derived\(b\?\.html \? buildSrcdoc\(b\.html,/, `${f}: srcdoc is not built by buildSrcdoc`);
  }
});
