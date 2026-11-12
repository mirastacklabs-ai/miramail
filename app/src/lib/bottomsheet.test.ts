// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/bottomsheet.test.ts
// Safety: the sheet scrim must close on click. Closing on pointerdown lets the follow-up click land on whatever is
// under the finger (Send, Cancel, a chip x) -> accidental send/discard.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const src = readFileSync(new URL("../components/mobile/BottomSheet.svelte", import.meta.url), "utf8");
const scrim = src.match(/<div class="scrim".*$/m)?.[0] ?? "";

test("bottom sheet: scrim closes on click, never on pointerdown", () => {
  assert.ok(scrim, "scrim element found");
  assert.match(scrim, /onclick=\{closeScrim\}/);
  const pd = scrim.split("onclick=")[0]; // everything before onclick: the pointerdown handler
  assert.match(pd, /onpointerdown=/);
  assert.ok(!/onclose/.test(pd), "pointerdown must not close the sheet");
});

test("bottom sheet: closeScrim defers to onclose, and taps right after opening are swallowed", () => {
  assert.match(src, /const closeScrim = .*onclose\(\)/);
  assert.match(src, /sheetTapIgnored\(openedAt/);
  assert.match(src, /onclickcapture=\{swallow\}/);
});
