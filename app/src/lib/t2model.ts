// Pure helpers for the Tier 2 Preferences sections (storage limits, export progress, backup restore). No DOM, no Tauri.
import type { CacheUsage, ExportProgress } from "./t2api";

/** Parse a limit field. "" = no limit (null); a whole number 1..max = that limit; anything else = undefined (invalid). */
export function parseLimit(text: string, max: number): number | null | undefined {
  const t = text.trim();
  if (t === "") return null;
  if (!/^\d{1,9}$/.test(t)) return undefined;
  const n = Number(t);
  return n >= 1 && n <= max ? n : undefined;
}
export const MAX_DAYS = 36_500;
export const MAX_MB = 1_000_000;

/** Bytes the cache holds for an account (raw bodies plus extracted search text). */
export const usageBytes = (u: CacheUsage): number => u.bodyBytes + u.textBytes;

/** "12.3 MB" style size (decimal units, like Finder). */
export function bytesLabel(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "0 B";
  if (n < 1000) return `${Math.round(n)} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1000, i = 0;
  while (v >= 1000 && i < units.length - 1) { v /= 1000; i++; }
  return `${v >= 100 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** One line for the export progress bar. */
export function exportLine(p: ExportProgress): string {
  switch (p.state) {
    case "running": return `Exporting… ${p.done} of ${p.total}`;
    case "done": return `Exported ${p.done - p.skipped} of ${p.total} messages${p.skipped ? ` (${p.skipped} could not be fetched and were skipped)` : ""}.`;
    case "cancelled": return `Stopped after ${p.done} of ${p.total}. The file holds the messages written so far.`;
    case "error": return p.error ?? "The export failed.";
  }
}

/** Which restore sections need an existing account, and which of their items can be applied here. */
export interface Applicable { applicable: number; skipped: number }
export function applicable(items: unknown, accounts: string[]): Applicable {
  if (!Array.isArray(items)) return { applicable: 0, skipped: 0 };
  let a = 0, s = 0;
  for (const it of items) {
    const acc = it && typeof it === "object" ? (it as { account?: unknown }).account : undefined;
    if (typeof acc === "string" && !accounts.includes(acc)) s++; else a++;
  }
  return { applicable: a, skipped: s };
}

/** Rule key used to avoid restoring a rule twice. */
export const ruleKey = (r: { account: string; name: string }): string => `${r.account}\u0000${r.name.trim().toLowerCase()}`;
