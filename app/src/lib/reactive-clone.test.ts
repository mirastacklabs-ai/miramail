// @ts-nocheck -- run with: node --test src/lib/reactive-clone.test.ts (Node type stripping; no extra dependencies)
// Regression for "Preferences > AI shows Loading… / stays on General": AiSettings.svelte called
// structuredClone(ai.settings). ai.settings is a Svelte 5 $state proxy, and a Proxy cannot be structured-cloned,
// so the component threw DataCloneError while initialising and the tab never rendered.
import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { compileModule } from "svelte/compiler";

/** Compile a .svelte.js module with the real Svelte compiler and run it on the real client runtime. */
async function runRunes(src: string) {
  const { js } = compileModule(src, { generate: "client", filename: "probe.svelte.js" });
  const runtime = import.meta.resolve("svelte/internal/client");
  const code = js.code.replace(/(["'])svelte\/internal\/client\1/g, JSON.stringify(runtime));
  return import("data:text/javascript;base64," + Buffer.from(code).toString("base64"));
}

test("a $state object cannot be structuredClone'd; $state.snapshot gives a plain copy", async () => {
  const m = await runRunes(`
    class Store { settings = $state({ perTurnTokens: 1, purge: { egressRetentionDays: 30 } }); }
    export const store = new Store();
    export const clone = () => structuredClone(store.settings);
    export const snap = () => $state.snapshot(store.settings);
  `);
  assert.throws(() => m.clone(), (e) => e?.name === "DataCloneError");
  const s = m.snap();
  assert.deepEqual(s, { perTurnTokens: 1, purge: { egressRetentionDays: 30 } });
  assert.doesNotThrow(() => structuredClone(s));
  s.purge.egressRetentionDays = 7;
  assert.equal(m.store.settings.purge.egressRetentionDays, 30, "the snapshot is detached from the store");
});

/** Every rune file (.svelte, .svelte.ts) under src/. */
function runeFiles(dir: string, out: string[] = []): string[] {
  for (const n of readdirSync(dir)) {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) runeFiles(p, out);
    else if (n.endsWith(".svelte") || n.endsWith(".svelte.ts")) out.push(p);
  }
  return out;
}

test("rune files never structuredClone reactive values (use $state.snapshot)", () => {
  const src = fileURLToPath(new URL("..", import.meta.url));
  const bad: string[] = [];
  for (const f of runeFiles(src)) {
    readFileSync(f, "utf8").split("\n").forEach((line, i) => {
      if (/^\s*(\/\/|\*|<!--)/.test(line)) return;
      for (const m of line.matchAll(/structuredClone\(\s*([^)]*)\)/g)) {
        // Only an UPPER_CASE module constant is known to be a plain object.
        if (!/^[A-Z][A-Z0-9_]*$/.test(m[1].trim())) bad.push(`${relative(src, f)}:${i + 1}: ${line.trim()}`);
      }
    });
  }
  assert.deepEqual(bad, [], "structuredClone of a $state proxy throws DataCloneError; use $state.snapshot(...)");
});
