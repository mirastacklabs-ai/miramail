// Pure helpers for batch operations: chunking, progress, partial-failure summary, undo items (tested in bulk.test.ts).
import type { MoveItem, Outcome } from "./orgtypes";

export const CHUNK = 200;

export interface ChunkResult { outcomes: Outcome[]; destFolder: string | null; destValidity: number | null }
export interface Progress { done: number; total: number; ok: number; failed: number }
export interface Failure { uid: number | null; error: string }
export interface Summary {
  total: number;
  ok: Outcome[];
  failed: Failure[];
  /** Items never sent because an earlier chunk failed as a whole, or the run was cancelled. */
  notAttempted: number;
  cancelled: boolean;
  destFolder: string | null;
  destValidity: number | null;
}

export function chunk<T>(items: readonly T[], size = CHUNK): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < items.length; i += size) out.push(items.slice(i, i + size));
  return out;
}

const msg = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : "The operation failed.");

/**
 * Run `run` on chunks of `items` one after another. A chunk that throws counts as failed as a whole and stops the
 * run (the rest is reported as not attempted: the server is probably unhappy). A chunk that returns outcomes may
 * have per-item failures; those are collected and the run continues.
 */
export async function runChunks<T>(
  items: readonly T[],
  run: (c: T[]) => Promise<ChunkResult>,
  onProgress?: (p: Progress) => void,
  shouldStop: () => boolean = () => false,
  size = CHUNK,
): Promise<Summary> {
  const s: Summary = { total: items.length, ok: [], failed: [], notAttempted: 0, cancelled: false, destFolder: null, destValidity: null };
  let done = 0;
  const parts = chunk(items, size);
  for (let i = 0; i < parts.length; i++) {
    if (shouldStop()) { s.cancelled = true; s.notAttempted = items.length - done; break; }
    const part = parts[i];
    try {
      const r = await run(part);
      s.destFolder = r.destFolder ?? s.destFolder;
      s.destValidity = r.destValidity ?? s.destValidity;
      for (const o of r.outcomes) {
        if (o.ok) s.ok.push(o); else s.failed.push({ uid: o.uid, error: o.error ?? "Failed." });
      }
      // The backend answers once per item; if it ever returns fewer, the rest counts as failed rather than silently lost.
      const missing = part.length - r.outcomes.length;
      for (let k = 0; k < missing; k++) s.failed.push({ uid: null, error: "No answer from the server for this message." });
    } catch (e) {
      for (let k = 0; k < part.length; k++) s.failed.push({ uid: null, error: msg(e) });
      done += part.length;
      s.notAttempted = items.length - done;
      onProgress?.({ done, total: items.length, ok: s.ok.length, failed: s.failed.length });
      return s;
    }
    done += part.length;
    onProgress?.({ done, total: items.length, ok: s.ok.length, failed: s.failed.length });
  }
  return s;
}

/** One sentence for the toast: "Archived 180 of 200. 20 failed: <reason>." */
export function describe(verb: string, s: Summary): string {
  const n = (x: number) => x.toLocaleString("en-US");
  if (!s.failed.length && !s.notAttempted) return `${verb} ${n(s.ok.length)} message${s.ok.length === 1 ? "" : "s"}.`;
  const reasons = [...new Set(s.failed.map((f) => f.error))];
  const why = reasons.length ? ` ${reasons.length === 1 ? reasons[0] : `${reasons[0]} (and ${reasons.length - 1} other reason${reasons.length === 2 ? "" : "s"})`}` : "";
  const parts = [`${verb} ${n(s.ok.length)} of ${n(s.total)}.`];
  if (s.failed.length) parts.push(`${n(s.failed.length)} failed.${why}`);
  if (s.notAttempted) parts.push(`${n(s.notAttempted)} not attempted.`);
  return parts.join(" ");
}

/** What an undo can move back: everything that moved and can be found again (new UID or Message-ID). */
export function undoItems(s: Summary): { items: MoveItem[]; total: number } {
  const items = s.ok.filter((o) => o.newUid != null || o.messageId).map((o) => ({ uid: o.newUid, messageId: o.messageId }));
  return { items, total: s.ok.length };
}

// ---- selection (Shift-range, Cmd-toggle, select all) ----

export interface Sel { set: Set<number>; anchor: number | null }
export const emptySel = (): Sel => ({ set: new Set(), anchor: null });

/** Plain click: no selection change handled by caller. Cmd/Ctrl-click toggles; Shift-click selects the range from the anchor. */
export function clickSel(s: Sel, rows: readonly number[], uid: number, mods: { shift: boolean; meta: boolean }): Sel {
  const set = new Set(s.set);
  if (mods.shift && s.anchor != null && rows.includes(s.anchor)) {
    const a = rows.indexOf(s.anchor), b = rows.indexOf(uid);
    if (b < 0) return s;
    const [lo, hi] = a < b ? [a, b] : [b, a];
    for (let i = lo; i <= hi; i++) set.add(rows[i]);
    return { set, anchor: s.anchor };
  }
  if (set.has(uid)) set.delete(uid); else set.add(uid);
  return { set, anchor: uid };
}

export const selectAll = (rows: readonly number[]): Sel => ({ set: new Set(rows), anchor: rows[0] ?? null });

/** Drop selected uids that are no longer listed. */
export function prune(s: Sel, rows: readonly number[]): Sel {
  const have = new Set(rows);
  const set = new Set([...s.set].filter((u) => have.has(u)));
  return set.size === s.set.size ? s : { set, anchor: s.anchor != null && have.has(s.anchor) ? s.anchor : null };
}

/** Row to select after removing `gone` from `rows`: the next surviving row after the first removed one, else the previous. */
export function nextAfterRemoval(rows: readonly number[], gone: ReadonlySet<number>, pick: "next" | "previous" | "none"): number | null {
  if (pick === "none") return null;
  const first = rows.findIndex((u) => gone.has(u));
  if (first < 0) return null;
  const rest = rows.filter((u) => !gone.has(u));
  if (!rest.length) return null;
  const before = rows.slice(0, first).filter((u) => !gone.has(u)).length;
  return pick === "previous" ? rest[Math.max(0, before - 1)] : rest[Math.min(before, rest.length - 1)];
}
