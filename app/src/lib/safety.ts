// Pure helpers for the Reader's mail-safety banner (docs/SAFETY_CHECKS.md). The backend computes the report on this
// Mac (no network); this file only shapes wording and keeps the local "This is fine" list.
// There is no "safe" label anywhere: the best result is "no red flags found".

export type SafetyLevel = "none_found" | "caution" | "warning";
export type SafetySeverity = "info" | "low" | "medium" | "high" | "critical";
export interface SafetyReason { code: string; severity: SafetySeverity; title: string; detail: string; count: number }
export interface SafetyReport { score: number; level: SafetyLevel; reasons: SafetyReason[] }

export const NONE_FOUND_TEXT = "No red flags found. This check cannot prove a message is safe.";
export const PRIVACY_NOTE = "Checked on this Mac. Nothing was sent anywhere. This check cannot prove a message is safe.";
export const DISMISS_KEY = "miramail.safetyfine.v1";
export const MAX_DISMISSED = 500;
export const TOP_REASONS = 3;

export function headline(level: SafetyLevel): string {
  switch (level) {
    case "warning": return "Warning signs found in this message";
    case "caution": return "A few things about this message look off";
    default: return NONE_FOUND_TEXT;
  }
}

const RANK: Record<SafetySeverity, number> = { info: 0, low: 1, medium: 2, high: 3, critical: 4 };

/** Reasons worth showing up front: the most severe non-info ones (the backend already sorts, this stays safe if not). */
export function topReasons(r: SafetyReport, n = TOP_REASONS): SafetyReason[] {
  return r.reasons.filter((x) => x.severity !== "info").sort((a, b) => RANK[b.severity] - RANK[a.severity]).slice(0, n);
}

/** Everything, most severe first, for the "Show details" list. */
export function allReasons(r: SafetyReport): SafetyReason[] {
  return [...r.reasons].sort((a, b) => RANK[b.severity] - RANK[a.severity]);
}

export const reasonTitle = (x: SafetyReason): string => (x.count > 1 ? `${x.title} (${x.count})` : x.title);

/** True when the banner has something to say (a caution or warning with at least one non-info reason). */
export const needsBanner = (r: SafetyReport | null): boolean => !!r && r.level !== "none_found" && r.reasons.some((x) => x.severity !== "info");

/** Local per-message key: account, folder, UIDVALIDITY and uid (a bare uid can name another message after a renumber). */
export function dismissKey(account: string, folder: string, uidvalidity: number | undefined, uid: number): string {
  return [account, folder, uidvalidity ?? 0, uid].join("\u0001");
}

export function parseDismissed(raw: string | null | undefined): string[] {
  if (!raw) return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return [...new Set(v.filter((x): x is string => typeof x === "string" && x.length > 0 && x.length < 1000))].slice(-MAX_DISMISSED);
  } catch { return []; }
}
export const isDismissed = (list: readonly string[], key: string): boolean => list.includes(key);
export function addDismissed(list: readonly string[], key: string): string[] {
  if (!key || list.includes(key)) return [...list];
  return [...list, key].slice(-MAX_DISMISSED);
}
export const removeDismissed = (list: readonly string[], key: string): string[] => list.filter((k) => k !== key);
export function loadDismissed(): string[] { try { return parseDismissed(localStorage.getItem(DISMISS_KEY)); } catch { return []; } }
export function storeDismissed(l: readonly string[]) { try { localStorage.setItem(DISMISS_KEY, JSON.stringify(l)); } catch { /* session only */ } }
