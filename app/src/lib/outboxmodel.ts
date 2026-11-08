// Pure helpers for the outbox UI (no DOM, no Tauri): labels, countdowns, what actions an entry offers.
import type { OutboxItem } from "./types";
/** "Fri, 6 Nov, 08:00" in the Mac's locale. */
const at = (ts: number): string => new Date(ts).toLocaleString([], { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });

export const AUTO_RETRIES = 3;

/** Whole seconds left of the undo window (never negative). */
export function secondsLeft(it: Pick<OutboxItem, "dueAt">, now: number): number {
  return Math.max(0, Math.ceil((it.dueAt - now) / 1000));
}

/** The entries still inside their undo window, newest last. */
export function holding(items: OutboxItem[], now: number): OutboxItem[] {
  return items.filter((i) => i.state === "holding" && i.dueAt > now).sort((a, b) => a.createdAt - b.createdAt);
}

/** Entries that need a decision at launch: the app was closed while they were held. */
export const interrupted = (items: OutboxItem[]): OutboxItem[] => items.filter((i) => i.state === "interrupted");

/** Shown in the sidebar link and its badge: everything except a message still counting down toward sending. */
export const needsAttention = (items: OutboxItem[]): number => items.filter((i) => i.state === "failed" || i.state === "unknown" || i.state === "interrupted").length;

export function recipients(it: Pick<OutboxItem, "to">, max = 2): string {
  const t = it.to.map((a) => a.trim()).filter(Boolean);
  if (!t.length) return "(no recipient)";
  return t.length > max ? `${t.slice(0, max).join(", ")} +${t.length - max}` : t.join(", ");
}

/** One plain-words line describing where an entry is. */
export function describe(it: OutboxItem, now: number): string {
  switch (it.state) {
    case "holding": return it.dueAt > now ? `Sending in ${secondsLeft(it, now)} s` : "Sending…";
    case "sending": return "Sending…";
    case "failed":
      if (it.nextRetryAt != null) return `Not sent. Trying again in ${Math.max(1, Math.ceil((it.nextRetryAt - now) / 1000))} s (${it.failures} of ${AUTO_RETRIES + 1} attempts failed).`;
      return "Not sent. Automatic retries are used up: retry when you are ready.";
    case "unknown": return "Might have been sent. Check your Sent folder before sending again.";
    case "interrupted": return it.scheduledAt != null ? `Not sent: it was scheduled for ${at(it.scheduledAt)}, but MiraMail was not running then.` : "Not sent: MiraMail was closed before this message went out.";
    case "scheduled": return it.scheduledAt != null ? `Scheduled for ${at(it.scheduledAt)}. It is sent only while MiraMail is open.` : "Scheduled.";
  }
}

export interface Actions { undo: boolean; retry: boolean; edit: boolean; discard: boolean }
/** Which buttons an entry offers. While `sending` nothing can be done; that is what keeps a send exactly-once. */
export function actions(it: Pick<OutboxItem, "state">): Actions {
  switch (it.state) {
    case "holding": return { undo: true, retry: false, edit: false, discard: false };
    // Scheduled: change your mind (Edit), send it right away, or drop it. Never undone silently.
    case "sending": return { undo: false, retry: false, edit: false, discard: false };
    default: return { undo: false, retry: true, edit: true, discard: true };
  }
}

/** Parse the pane payload that Undo hands back; anything unusable yields null. */
export function parseRestore(s: string): Record<string, unknown> | null {
  try {
    const v = JSON.parse(s);
    return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;
  } catch { return null; }
}

/** Compose payload for a restored message: the forward re-attach is dropped (the files come back as real attachments). */
export function restoredPayload(raw: Record<string, unknown>): Record<string, unknown> {
  const { fwd: _fwd, seedHtml: _seed, ...rest } = raw;
  // Defaults first: a reduced restore (see buildRestore / the backend fallback) only carries recipients, subject and text.
  return { account: null, to: "", cc: "", bcc: "", subject: "", text: "", inReplyTo: null, showCc: false, ...rest };
}

/** Largest restore payload we hand the backend (UTF-8 bytes of the whole JSON; the backend caps at 1 MB). */
export const MAX_RESTORE_JSON = 900_000;
const utf8 = (s: string) => new TextEncoder().encode(s).length;
const PIC_DATA = /(<img\b[^>]*?\bsrc\s*=\s*)(["'])data:[^"']*\2/gi;

/**
 * The Compose-restore JSON for Send, always small enough to store. Measures the whole JSON, not just the HTML:
 * 1. everything as is; 2. pictures dropped (they then come back as attachments: `inlineCount` becomes 0);
 * 3. plain text only, truncated. Never throws and never refuses a send.
 */
export function buildRestore(payload: Record<string, unknown>, fullHtml: string | undefined, inlinePics: number, limit = MAX_RESTORE_JSON): { restore: string; inlineCount: number } {
  const full = JSON.stringify(fullHtml != null ? { ...payload, html: fullHtml } : payload);
  if (utf8(full) <= limit) return { restore: full, inlineCount: inlinePics };
  if (fullHtml != null) {
    const slim = JSON.stringify({ ...payload, html: fullHtml.replace(PIC_DATA, '$1"" data-mm-dropped="1"') });
    if (utf8(slim) <= limit) return { restore: slim, inlineCount: 0 };
  }
  const { html: _h, ...rest } = payload;
  const text = typeof rest.text === "string" ? rest.text.slice(0, 100_000) : "";
  const small = JSON.stringify({ ...rest, text, mode: "plain" });
  return { restore: utf8(small) <= limit ? small : "{}", inlineCount: 0 };
}
