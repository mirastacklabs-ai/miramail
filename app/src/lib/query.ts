// Pure search-operator parser + filter for the message list (no Svelte, no DOM; unit-tested with node --test).
import type { Address, MessageSummary } from "./types";

export type Row = MessageSummary & { cc?: Address[]; hasAttachment?: boolean };
export interface Caps { cc: boolean; attachment: boolean }
export type TextField = "from" | "to" | "cc" | "subject" | "any";
export type IsValue = "unread" | "read" | "flagged" | "unflagged" | "answered" | "unanswered";
export type Term =
  | { kind: "text"; field: TextField; value: string; neg: boolean; raw: string; start: number; end: number }
  | { kind: "is"; value: IsValue; neg: boolean; raw: string; start: number; end: number }
  | { kind: "date"; op: "before" | "after"; ts: number; neg: boolean; raw: string; start: number; end: number }
  | { kind: "attachment"; neg: boolean; raw: string; start: number; end: number }
  | { kind: "folder"; value: string; neg: boolean; raw: string; start: number; end: number };
export interface Parsed { terms: Term[]; hints: string[] }
export interface Chips { unread: boolean; flagged: boolean; today: boolean; week: boolean }
export const NO_CHIPS: Chips = { unread: false, flagged: false, today: false, week: false };

export const OPERATORS: { op: string; desc: string }[] = [
  { op: "from:", desc: "Sender name or address" },
  { op: "to:", desc: "Recipient name or address" },
  { op: "cc:", desc: "Cc recipient (searches the whole folder)" },
  { op: "subject:", desc: "Words in the subject" },
  { op: "contains:", desc: "Subject, preview, from, to (alias body:)" },
  { op: "is:", desc: "unread, read, flagged, unflagged, answered" },
  { op: "before:", desc: "Before a date: 2026-01-31, today, yesterday" },
  { op: "after:", desc: "On or after a date" },
  { op: "older_than:", desc: "Older than 7d (h, d, w, m, y)" },
  { op: "newer_than:", desc: "Newer than 2d (h, d, w, m, y)" },
  { op: "has:", desc: "has:attachment (searches the whole folder)" },
  { op: "folder:", desc: "Folder name or role, e.g. folder:archive" },
];

/** True when the query uses an operator only the backend search can answer (cc:, has:, folder:). */
export const needsBackend = (input: string): boolean => /(?:^|\s)-?(?:cc|has|folder):\S/i.test(input);

/** Lowercase + strip diacritics. */
export const norm = (s: string): string => s.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

const IS_VALUES = new Set<IsValue>(["unread", "read", "flagged", "unflagged", "answered", "unanswered"]);
const DAY = 86400;
const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() / 1000;
const addDays = (ts: number, n: number) => { const d = new Date(ts * 1000); d.setDate(d.getDate() + n); return d.getTime() / 1000; };

/** Parse a day keyword or YYYY-MM-DD to local start-of-day seconds, or null if invalid. */
export function parseDay(v: string, now: number): number | null {
  const s = v.toLowerCase();
  const today = startOfDay(new Date(now * 1000));
  if (s === "today") return today;
  if (s === "yesterday") return addDays(today, -1);
  const m = /^(\d{4})[-/](\d{2})[-/](\d{2})$/.exec(s);
  if (!m) return null;
  const [y, mo, d] = [+m[1], +m[2], +m[3]];
  const dt = new Date(y, mo - 1, d);
  if (dt.getFullYear() !== y || dt.getMonth() !== mo - 1 || dt.getDate() !== d) return null;
  return dt.getTime() / 1000;
}
const UNIT: Record<string, number> = { h: 3600, d: DAY, w: 7 * DAY, m: 30 * DAY, y: 365 * DAY };
export function parseSpan(v: string): number | null {
  const m = /^(\d{1,4})([hdwmy])$/i.exec(v);
  return m ? +m[1] * UNIT[m[2].toLowerCase()] : null;
}

interface RawTok { text: string; start: number; end: number }
/** Split on whitespace, keeping "quoted phrases" (also after `op:`) together. */
function tokenize(input: string): RawTok[] {
  const out: RawTok[] = [];
  let i = 0;
  const n = input.length;
  while (i < n) {
    while (i < n && /\s/.test(input[i])) i++;
    if (i >= n) break;
    const start = i;
    let q = false;
    while (i < n && (q || !/\s/.test(input[i]))) { if (input[i] === '"') q = !q; i++; }
    out.push({ text: input.slice(start, i), start, end: i });
  }
  return out;
}
const unquote = (v: string) => v.replace(/"/g, "");

export function parseQuery(input: string, now = Date.now() / 1000, caps: Caps = { cc: false, attachment: false }): Parsed {
  const terms: Term[] = [];
  const hints: string[] = [];
  for (const t of tokenize(input)) {
    let body = t.text;
    let neg = false;
    if (body.length > 1 && body[0] === "-") { neg = true; body = body.slice(1); }
    const base = { neg, raw: t.text, start: t.start, end: t.end };
    const m = /^([A-Za-z_]+):(.*)$/s.exec(body);
    const op = m ? m[1].toLowerCase() : "";
    const val = m ? unquote(m[2]).trim() : "";
    const known = ["from", "to", "cc", "subject", "contains", "body", "is", "before", "after", "older_than", "newer_than", "has", "folder"];
    if (!m || !known.includes(op)) {
      const v = unquote(body).trim();
      if (v) terms.push({ kind: "text", field: "any", value: norm(v), ...base });
      continue;
    }
    if (!val) continue; // still typing "from:"
    switch (op) {
      case "from": case "to": case "subject":
        terms.push({ kind: "text", field: op, value: norm(val), ...base }); break;
      case "cc":
        if (caps.cc) terms.push({ kind: "text", field: "cc", value: norm(val), ...base });
        else hints.push("cc: isn't available in the message list yet"); break;
      case "contains": case "body":
        terms.push({ kind: "text", field: "any", value: norm(val), ...base }); break;
      case "is": {
        const v = val.toLowerCase() as IsValue;
        if (IS_VALUES.has(v)) terms.push({ kind: "is", value: v, ...base });
        else hints.push(`Unknown is:${val} — try unread, read, flagged, unflagged, answered`);
        break;
      }
      case "before": case "after": {
        const ts = parseDay(val, now);
        if (ts == null) hints.push(`Invalid date "${val}" — use YYYY-MM-DD, today or yesterday`);
        else terms.push({ kind: "date", op, ts, ...base });
        break;
      }
      case "older_than": case "newer_than": {
        const span = parseSpan(val);
        if (span == null) hints.push(`Invalid span "${val}" — use e.g. 7d, 2w, 3m`);
        else terms.push({ kind: "date", op: op === "older_than" ? "before" : "after", ts: now - span, ...base });
        break;
      }
      case "folder":
        terms.push({ kind: "folder", value: norm(val), ...base }); break;
      case "has":
        if (val.toLowerCase() !== "attachment") hints.push(`Unknown has:${val} — try has:attachment`);
        else if (caps.attachment) terms.push({ kind: "attachment", ...base });
        else hints.push("has:attachment isn't available in the message list yet");
        break;
    }
  }
  return { terms, hints };
}

/** Chips expressed as query terms so they share one evaluation path. */
export function chipTerms(c: Chips, now: number): Term[] {
  const r = { neg: false, raw: "", start: 0, end: 0 };
  const out: Term[] = [];
  if (c.unread) out.push({ kind: "is", value: "unread", ...r });
  if (c.flagged) out.push({ kind: "is", value: "flagged", ...r });
  if (c.today) out.push({ kind: "date", op: "after", ts: startOfDay(new Date(now * 1000)), ...r });
  if (c.week) out.push({ kind: "date", op: "after", ts: addDays(startOfDay(new Date(now * 1000)), -6), ...r });
  return out;
}

interface Prep { from: string; to: string; cc: string; subject: string; any: string }
const prepCache = new WeakMap<object, Prep>();
const addrs = (l: Address[] | null | undefined) => (l ?? []).map((a) => `${a.name ?? ""} ${a.address}`).join(" ");
function prep(m: Row): Prep {
  let p = prepCache.get(m);
  if (!p) {
    const from = norm(`${m.from?.name ?? ""} ${m.from?.address ?? ""}`);
    const to = norm(addrs(m.to));
    const subject = norm(m.subject ?? "");
    p = { from, to, cc: norm(addrs(m.cc)), subject, any: `${subject}\n${norm(m.snippet ?? "")}\n${from}\n${to}` };
    prepCache.set(m, p);
  }
  return p;
}

function test(t: Term, m: Row): boolean {
  switch (t.kind) {
    case "text": return prep(m)[t.field].includes(t.value);
    case "is":
      switch (t.value) {
        case "unread": return !m.flags.seen;
        case "read": return m.flags.seen;
        case "flagged": return m.flags.flagged;
        case "unflagged": return !m.flags.flagged;
        case "answered": return m.flags.answered;
        case "unanswered": return !m.flags.answered;
      }
      return false;
    case "date": return m.date != null && (t.op === "before" ? m.date < t.ts : m.date >= t.ts);
    case "attachment": return !!m.hasAttachment;
    case "folder": return true; // answered by the backend search; the loaded list is one folder
  }
}
export const matches = (terms: Term[], m: Row): boolean => terms.every((t) => test(t, m) !== t.neg);

export function applyFilter<T extends Row>(rows: T[], terms: Term[]): T[] {
  return terms.length ? rows.filter((m) => matches(terms, m)) : rows;
}

/** Remove one token (by its source range) from the query string. */
export function removeToken(input: string, t: { start: number; end: number }): string {
  return (input.slice(0, t.start) + input.slice(t.end)).replace(/\s{2,}/g, " ").trim();
}

export function topAddresses(rows: Row[], field: "from" | "to", limit = 5): string[] {
  const c = new Map<string, number>();
  for (const m of rows) for (const a of field === "from" ? (m.from ? [m.from] : []) : m.to) c.set(a.address, (c.get(a.address) ?? 0) + 1);
  return [...c].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).slice(0, limit).map((x) => x[0]);
}

export interface Suggestion { insert: string; label: string; desc: string }
/** Suggestions for the token being typed at the end of the input. */
export function suggest(input: string, rows: Row[] = []): { prefix: string; items: Suggestion[] } {
  const m = /(?:^|\s)(-?)([A-Za-z_]*)(:?)([^\s"]*)$/.exec(input);
  if (!m || (!m[2] && !m[3])) return { prefix: "", items: [] };
  const neg = m[1], word = m[2].toLowerCase(), colon = m[3], arg = m[4];
  const tokenStart = input.length - (neg.length + word.length + colon.length + arg.length);
  const prefix = input.slice(0, tokenStart);
  if (!colon) {
    return { prefix, items: OPERATORS.filter((o) => o.op.startsWith(word)).map((o) => ({ insert: neg + o.op, label: o.op, desc: o.desc })) };
  }
  const a = norm(arg);
  if (word === "from" || word === "to") {
    return { prefix, items: topAddresses(rows, word, 20).filter((x) => norm(x).includes(a)).slice(0, 5).map((x) => ({ insert: `${neg}${word}:${x} `, label: x, desc: "" })) };
  }
  if (word === "is") return { prefix, items: ["unread", "read", "flagged", "unflagged", "answered"].filter((v) => v.startsWith(a)).map((v) => ({ insert: `${neg}is:${v} `, label: v, desc: "" })) };
  if (word === "before" || word === "after") return { prefix, items: ["today", "yesterday"].filter((v) => v.startsWith(a)).map((v) => ({ insert: `${neg}${word}:${v} `, label: v, desc: "" })) };
  if (word === "folder") return { prefix, items: ["inbox", "archive", "sent", "drafts", "trash", "junk"].filter((v) => v.startsWith(a)).map((v) => ({ insert: `${neg}folder:${v} `, label: v, desc: "" })) };
  if (word === "has") return { prefix, items: "attachment".startsWith(a) && a !== "attachment" ? [{ insert: `${neg}has:attachment `, label: "attachment", desc: "" }] : [] };
  return { prefix, items: [] };
}
