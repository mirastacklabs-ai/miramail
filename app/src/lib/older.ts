// Pure helpers for "load older messages" and the windowed message list (tested in older.test.ts).

/** Messages fetched per click / auto-load step. */
export const OLDER_STEP = 200;
/** Batch size for "Load all" and "Jump to date". */
export const BULK_STEP = 500;
/** Cached rows pulled from SQLite per page. */
export const CACHE_PAGE = 1000;
/** Folders with more than this many unloaded messages ask before "Load all". */
export const LOAD_ALL_CONFIRM_ABOVE = 1000;
/** "Few results" threshold for the search-older hint. */
export const FEW_RESULTS = 20;

export interface Window { start: number; end: number }

/** Rows `[start, end)` to render for a scroll offset, with `overscan` extra rows each side. */
export function windowRange(scrollTop: number, viewH: number, rowH: number, n: number, overscan = 6): Window {
  if (n <= 0 || !(rowH > 0)) return { start: 0, end: Math.max(0, n) };
  const top = Math.max(0, scrollTop);
  const first = Math.min(n - 1, Math.floor(top / rowH));
  const last = Math.min(n, Math.ceil((top + Math.max(0, viewH)) / rowH));
  return { start: Math.max(0, first - overscan), end: Math.min(n, Math.max(last, first + 1) + overscan) };
}

/** Scroll offset that brings row `index` fully into view (unchanged when it already is). */
export function scrollToReveal(index: number, scrollTop: number, viewH: number, rowH: number): number {
  const top = index * rowH, bottom = top + rowH;
  if (top < scrollTop) return top;
  if (bottom > scrollTop + viewH) return Math.max(0, bottom - viewH);
  return scrollTop;
}

export function nearBottom(scrollTop: number, viewH: number, scrollH: number, px = 500): boolean {
  return scrollH - (scrollTop + viewH) <= px;
}

interface Row { uid: number; date: number | null }

/** Union by uid (incoming wins), ordered like the cache query: date DESC, uid DESC. */
export function mergeRows<T extends Row>(existing: T[], incoming: T[]): T[] {
  if (!incoming.length) return existing;
  const by = new Map<number, T>();
  for (const r of existing) by.set(r.uid, r);
  for (const r of incoming) by.set(r.uid, r);
  return [...by.values()].sort((a, b) => (b.date ?? 0) - (a.date ?? 0) || b.uid - a.uid);
}

const nf = new Intl.NumberFormat("en-US");
export const fmtN = (n: number) => nf.format(n);

/** "Showing 200 of 3,412 messages" (total unknown: "200 messages loaded"). */
export function loadedLabel(shown: number, total: number | null): string {
  if (total == null || total < shown) return `${fmtN(shown)} message${shown === 1 ? "" : "s"} loaded`;
  return `Showing ${fmtN(shown)} of ${fmtN(total)} message${total === 1 ? "" : "s"}`;
}

/** Messages on the server that are not loaded yet (0 when unknown). */
export function remainingOnServer(shown: number, total: number | null, reachedStart: boolean): number {
  if (reachedStart || total == null) return 0;
  return Math.max(0, total - shown);
}

/** True when the oldest loaded message is at or before `ts` (unix seconds). */
export function coversDate(rows: Row[], ts: number): boolean {
  let min = Infinity;
  for (const r of rows) if (r.date != null && r.date < min) min = r.date;
  return min <= ts;
}

/** Index of the first row (newest-first list) dated at or before `ts`; rows.length when none. */
export function indexOfDate(rows: Row[], ts: number): number {
  const i = rows.findIndex((r) => (r.date ?? 0) <= ts);
  return i < 0 ? rows.length : i;
}

/** "YYYY-MM-DD" (date input) -> unix seconds at the END of that local day, or null. */
export function endOfDay(iso: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso.trim());
  if (!m) return null;
  const [y, mo, d] = [+m[1], +m[2], +m[3]];
  const dt = new Date(y, mo - 1, d, 23, 59, 59);
  if (dt.getFullYear() !== y || dt.getMonth() !== mo - 1 || dt.getDate() !== d) return null;
  return Math.floor(dt.getTime() / 1000);
}

/** Show "Search older messages: load more" when a filter finds few rows and more mail exists. */
export function showSearchOlder(filterActive: boolean, matches: number, moreAvailable: boolean): boolean {
  return filterActive && moreAvailable && matches < FEW_RESULTS;
}

/** Auto-load only while scrolling an unfiltered list; never during bulk loads or after an error. */
export function shouldAutoLoad(o: { near: boolean; moreAvailable: boolean; busy: boolean; errored: boolean; bulk: boolean; filtered: boolean }): boolean {
  return o.near && o.moreAvailable && !o.busy && !o.errored && !o.bulk && !o.filtered;
}

/** Batch size for the next button press: never more than what is left. */
export function stepFor(remaining: number, step = OLDER_STEP): number {
  return remaining > 0 ? Math.min(step, remaining) : step;
}
