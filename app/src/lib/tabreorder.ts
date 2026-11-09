// Pure tab-reorder helpers (no DOM, no runes): runs under node --test. The Mail tab is pinned first.
import { MAIL_KEY } from "./wstree.ts";

/** Gap (0..n) a pointer at `x` points at, given each tab's horizontal midpoint in strip order. */
export function insertionSlot(mids: number[], x: number): number {
  let n = 0;
  for (const m of mids) if (x > m) n++;
  return n;
}

/** Final index of the dragged tab when it is dropped into gap `slot` (gap indexes refer to the list before the move). */
export function slotToIndex(from: number, slot: number): number {
  return slot > from ? slot - 1 : slot;
}

/** Move keys[from] to index `to`. The pinned Mail tab never moves and nothing lands before it. Returns the same array when nothing changes. */
export function moveTab(keys: string[], from: number, to: number): string[] {
  const n = keys.length;
  if (!Number.isInteger(from) || !Number.isInteger(to) || from < 0 || from >= n) return keys;
  if (keys[from] === MAIL_KEY) return keys;
  const min = keys[0] === MAIL_KEY ? 1 : 0;
  const t = Math.max(min, Math.min(n - 1, to));
  if (t === from) return keys;
  const out = keys.slice();
  out.splice(from, 1);
  out.splice(t, 0, keys[from]);
  return out;
}
