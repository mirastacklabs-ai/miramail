// Pure date / layout / RRULE helpers for the calendar UI. No dependencies; tested in caldate.test.ts.
import type { Occurrence } from "./types";

export type CalMode = "day" | "week" | "month" | "agenda";
export const MIN = 60000;
export const AGENDA_DAYS = 30;

export const sod = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate());
export function addDays(d: Date, n: number): Date { const x = new Date(d); x.setDate(x.getDate() + n); return x; }
export function addMonths(d: Date, n: number): Date {
  const x = new Date(d.getFullYear(), d.getMonth() + n, 1);
  const dim = new Date(x.getFullYear(), x.getMonth() + 1, 0).getDate();
  x.setDate(Math.min(d.getDate(), dim));
  return x;
}
const p2 = (n: number) => String(n).padStart(2, "0");
export const dayKey = (d: Date) => `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}`;
export const sameDay = (a: Date, b: Date) => dayKey(a) === dayKey(b);
export function parseDayKey(s: string): Date { const [y, m, d] = s.split("-").map(Number); return new Date(y, m - 1, d); }

/** First weekday (0 = Sunday .. 6) for a locale: Intl weekInfo when available, else Sunday for US-style regions, else Monday. */
export function weekStartDay(locale?: string): number {
  const loc = locale ?? (typeof navigator !== "undefined" ? navigator.language : "en-US");
  try {
    const l = new Intl.Locale(loc) as unknown as { getWeekInfo?: () => { firstDay: number }; weekInfo?: { firstDay: number } };
    const wi = l.getWeekInfo?.() ?? l.weekInfo;
    if (wi && typeof wi.firstDay === "number") return wi.firstDay % 7;
  } catch { /* fall through */ }
  const region = (loc.split("-")[1] ?? "").toUpperCase();
  if (region === "US" || region === "CA" || region === "MX" || region === "JP" || region === "IN" || loc === "en") return 0;
  return 1;
}

export function startOfWeek(d: Date, ws: number): Date {
  const diff = (d.getDay() - ws + 7) % 7;
  return addDays(sod(d), -diff);
}

/** Half-open visible range [start, end) for a mode around an anchor day. */
export function rangeFor(mode: CalMode, anchor: Date, ws: number): [Date, Date] {
  const a = sod(anchor);
  switch (mode) {
    case "day": return [a, addDays(a, 1)];
    case "week": { const s = startOfWeek(a, ws); return [s, addDays(s, 7)]; }
    case "month": {
      const first = new Date(a.getFullYear(), a.getMonth(), 1);
      const dim = new Date(a.getFullYear(), a.getMonth() + 1, 0).getDate();
      const s = startOfWeek(first, ws);
      const offset = (first.getDay() - ws + 7) % 7;
      return [s, addDays(s, Math.ceil((offset + dim) / 7) * 7)];
    }
    case "agenda": return [a, addDays(a, AGENDA_DAYS)];
  }
}
export function daysIn([s, e]: [Date, Date]): Date[] {
  const out: Date[] = [];
  for (let d = s; d < e; d = addDays(d, 1)) out.push(d);
  return out;
}
export function stepAnchor(mode: CalMode, anchor: Date, dir: 1 | -1): Date {
  switch (mode) {
    case "day": return addDays(anchor, dir);
    case "week": return addDays(anchor, 7 * dir);
    case "month": return addMonths(anchor, dir);
    case "agenda": return addDays(anchor, AGENDA_DAYS * dir);
  }
}
/** Length of the local calendar day in minutes (1380 / 1440 / 1500 around DST changes). */
export function dayLengthMinutes(d: Date): number { return Math.round((addDays(sod(d), 1).getTime() - sod(d).getTime()) / MIN); }

// ---- occurrences ----
export interface Span { start: Date; end: Date; allDay: boolean }
export function occSpan(o: Pick<Occurrence, "start" | "end" | "allDay">): Span {
  if (o.allDay) {
    const s = parseDayKey(o.start.slice(0, 10));
    let e = parseDayKey(o.end.slice(0, 10));
    if (e <= s) e = addDays(s, 1);
    return { start: s, end: e, allDay: true };
  }
  const s = new Date(o.start);
  let e = new Date(o.end);
  if (!(e > s)) e = new Date(s.getTime() + 30 * MIN);
  return { start: s, end: e, allDay: false };
}
/** Does the span touch the local day? */
export const touchesDay = (sp: Span, day: Date) => sp.start < addDays(sod(day), 1) && sp.end > sod(day) || (sp.start.getTime() === sp.end.getTime() && sameDay(sp.start, day));
export const spansMultipleDays = (sp: Span) => sp.allDay || dayKey(sp.start) !== dayKey(new Date(sp.end.getTime() - 1));

export interface Placed<T> { item: T; start: number; end: number; col: number; cols: number }
/** Column layout for overlapping intervals (start/end in minutes). Clusters of transitively overlapping items share a column count. */
export function layoutColumns<T>(items: { item: T; start: number; end: number }[]): Placed<T>[] {
  const sorted = [...items].sort((a, b) => a.start - b.start || b.end - a.end);
  const out: Placed<T>[] = [];
  let cluster: Placed<T>[] = [];
  let clusterEnd = -Infinity;
  const flush = () => { const n = cluster.reduce((m, p) => Math.max(m, p.col + 1), 0); for (const p of cluster) p.cols = n; out.push(...cluster); cluster = []; };
  const colEnds: number[] = [];
  for (const it of sorted) {
    if (it.start >= clusterEnd) { flush(); colEnds.length = 0; clusterEnd = -Infinity; }
    let col = colEnds.findIndex((e) => e <= it.start);
    if (col < 0) { col = colEnds.length; colEnds.push(it.end); } else colEnds[col] = it.end;
    cluster.push({ item: it.item, start: it.start, end: it.end, col, cols: 1 });
    clusterEnd = Math.max(clusterEnd, it.end);
  }
  flush();
  return out;
}

/** Minutes-from-local-midnight positions of a timed span clipped to `day`; null when it does not touch the day. */
export function clipToDay(sp: Span, day: Date): { start: number; end: number } | null {
  const d0 = sod(day), d1 = addDays(d0, 1);
  if (sp.end <= d0 || sp.start >= d1) { if (!(sp.start.getTime() === sp.end.getTime() && sp.start >= d0 && sp.start < d1)) return null; }
  const s = sp.start < d0 ? 0 : sp.start.getHours() * 60 + sp.start.getMinutes();
  const e = sp.end >= d1 ? 24 * 60 : sp.end.getHours() * 60 + sp.end.getMinutes();
  return { start: s, end: Math.max(e, s + 15) };
}

// ---- time zones ----
export const localTz = () => Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
interface Wall { y: number; mo: number; d: number; h: number; mi: number; s: number }
const fmtCache = new Map<string, Intl.DateTimeFormat>();
function wallIn(ms: number, tz: string): Wall {
  let f = fmtCache.get(tz);
  if (!f) {
    f = new Intl.DateTimeFormat("en-US", { timeZone: tz, hourCycle: "h23", year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit" });
    fmtCache.set(tz, f);
  }
  const g: Record<string, number> = {};
  for (const p of f.formatToParts(new Date(ms))) if (p.type !== "literal") g[p.type] = Number(p.value);
  return { y: g.year, mo: g.month, d: g.day, h: g.hour % 24, mi: g.minute, s: g.second };
}
/** UTC offset of `tz` at instant `ms`, in minutes. */
export function tzOffsetMinutes(tz: string, ms: number): number {
  const w = wallIn(ms, tz);
  return (Date.UTC(w.y, w.mo - 1, w.d, w.h, w.mi, w.s) - Math.floor(ms / 1000) * 1000) / MIN;
}
/** Instant (ms) of a wall-clock time in `tz`. Nonexistent (spring-forward) times resolve forward. */
export function wallToInstant(y: number, mo: number, d: number, h: number, mi: number, tz: string): number {
  const guess = Date.UTC(y, mo - 1, d, h, mi);
  const off = tzOffsetMinutes(tz, guess);
  const t1 = guess - off * MIN;
  const off2 = tzOffsetMinutes(tz, t1);
  if (off2 === off) return t1;
  const t2 = guess - off2 * MIN;
  return tzOffsetMinutes(tz, t2) === off2 ? t2 : t1; // t1 when the wall time falls in a spring-forward gap
}
export function fmtOffset(min: number): string {
  const a = Math.abs(min);
  return `${min < 0 ? "-" : "+"}${p2(Math.floor(a / 60))}:${p2(a % 60)}`;
}
/** RFC 3339 string for instant `ms` expressed in `tz`. */
export function toRfc(ms: number, tz: string): string {
  const w = wallIn(ms, tz);
  return `${w.y}-${p2(w.mo)}-${p2(w.d)}T${p2(w.h)}:${p2(w.mi)}:${p2(w.s)}${fmtOffset(tzOffsetMinutes(tz, ms))}`;
}
/** Wall-clock date + time of an RFC 3339 instant in `tz`, as input values. */
export function rfcToWall(iso: string, tz: string): { date: string; time: string } {
  const w = wallIn(new Date(iso).getTime(), tz);
  return { date: `${w.y}-${p2(w.mo)}-${p2(w.d)}`, time: `${p2(w.h)}:${p2(w.mi)}` };
}
export function wallToRfc(date: string, time: string, tz: string): string {
  const [y, mo, d] = date.split("-").map(Number);
  const [h, mi] = time.split(":").map(Number);
  return toRfc(wallToInstant(y, mo, d, h, mi, tz), tz);
}
export function timezones(): string[] {
  try { const f = (Intl as unknown as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf; if (f) return f("timeZone"); } catch { /* ignore */ }
  return [localTz(), "UTC"];
}

// ---- RRULE presets ----
export type RepeatPreset = "none" | "daily" | "weekdays" | "weekly" | "monthly" | "yearly" | "custom";
const DOW = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"];
export function presetRrule(p: Exclude<RepeatPreset, "custom">, start: Date): string | null {
  switch (p) {
    case "none": return null;
    case "daily": return "FREQ=DAILY";
    case "weekdays": return "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR";
    case "weekly": return `FREQ=WEEKLY;BYDAY=${DOW[start.getDay()]}`;
    case "monthly": return `FREQ=MONTHLY;BYMONTHDAY=${start.getDate()}`;
    case "yearly": return `FREQ=YEARLY;BYMONTH=${start.getMonth() + 1};BYMONTHDAY=${start.getDate()}`;
  }
}
export const normRrule = (s: string) => s.trim().replace(/^RRULE:/i, "").replace(/\s+/g, "").toUpperCase();
export function presetFromRrule(rrule: string | null, start: Date): RepeatPreset {
  if (!rrule) return "none";
  const r = normRrule(rrule);
  for (const p of ["daily", "weekdays", "weekly", "monthly", "yearly"] as const) if (presetRrule(p, start) === r) return p;
  return "custom";
}
const DAY_RE = /^[+-]?\d{0,2}(MO|TU|WE|TH|FR|SA|SU)$/;
/** Basic RRULE validation. Returns an error message or null when acceptable. */
export function validateRrule(text: string): string | null {
  const r = normRrule(text);
  if (!r) return "Enter a rule such as FREQ=WEEKLY;BYDAY=MO,WE";
  const kv = new Map<string, string>();
  for (const part of r.split(";")) {
    const i = part.indexOf("=");
    if (i < 1 || i === part.length - 1) return `Malformed part “${part}”.`;
    const k = part.slice(0, i);
    if (kv.has(k)) return `${k} appears twice.`;
    kv.set(k, part.slice(i + 1));
  }
  const freq = kv.get("FREQ");
  if (!freq) return "FREQ is required.";
  if (!["SECONDLY", "MINUTELY", "HOURLY", "DAILY", "WEEKLY", "MONTHLY", "YEARLY"].includes(freq)) return "FREQ must be DAILY, WEEKLY, MONTHLY or YEARLY.";
  if (["SECONDLY", "MINUTELY", "HOURLY"].includes(freq)) return "Sub-daily repeats are not supported.";
  const int = (v: string, lo: number, hi: number) => /^[+-]?\d+$/.test(v) && Number(v) >= lo && Number(v) <= hi;
  for (const [k, v] of kv) {
    switch (k) {
      case "FREQ": break;
      case "INTERVAL": case "COUNT": if (!int(v, 1, 100000)) return `${k} must be a positive number.`; break;
      case "UNTIL": if (!/^\d{8}(T\d{6}Z?)?$/.test(v)) return "UNTIL must look like 20271231 or 20271231T235959Z."; break;
      case "BYDAY": if (!v.split(",").every((x) => DAY_RE.test(x))) return "BYDAY must be days such as MO,WE or 1FR."; break;
      case "BYMONTHDAY": if (!v.split(",").every((x) => int(x, -31, 31) && Number(x) !== 0)) return "BYMONTHDAY must be 1..31 or -31..-1."; break;
      case "BYMONTH": if (!v.split(",").every((x) => int(x, 1, 12))) return "BYMONTH must be 1..12."; break;
      case "BYSETPOS": if (!v.split(",").every((x) => int(x, -366, 366) && Number(x) !== 0)) return "BYSETPOS must be a non-zero number."; break;
      case "WKST": if (!DOW.includes(v)) return "WKST must be a day such as MO."; break;
      default: return `${k} is not supported.`;
    }
  }
  if (kv.has("COUNT") && kv.has("UNTIL")) return "Use COUNT or UNTIL, not both.";
  return null;
}

// ---- formatting ----
export function titleFor(mode: CalMode, anchor: Date, ws: number): string {
  const [s, e] = rangeFor(mode, anchor, ws);
  if (mode === "day") return anchor.toLocaleDateString([], { weekday: "long", month: "long", day: "numeric", year: "numeric" });
  if (mode === "month") return anchor.toLocaleDateString([], { month: "long", year: "numeric" });
  const last = addDays(e, -1);
  const same = s.getFullYear() === last.getFullYear();
  const a = s.toLocaleDateString([], same ? { month: "short", day: "numeric" } : { month: "short", day: "numeric", year: "numeric" });
  const b = last.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
  return `${a} – ${b}`;
}
export const fmtTime = (d: Date) => d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
export function fmtSpan(sp: Span): string {
  const dOpt: Intl.DateTimeFormatOptions = { weekday: "short", month: "short", day: "numeric" };
  if (sp.allDay) {
    const last = addDays(sp.end, -1);
    return sameDay(sp.start, last) ? `${sp.start.toLocaleDateString([], dOpt)} · all day` : `${sp.start.toLocaleDateString([], dOpt)} – ${last.toLocaleDateString([], dOpt)}`;
  }
  const endInc = new Date(sp.end.getTime() - 1);
  if (sameDay(sp.start, endInc)) return `${sp.start.toLocaleDateString([], dOpt)} · ${fmtTime(sp.start)} – ${fmtTime(sp.end)}`;
  return `${sp.start.toLocaleDateString([], dOpt)} ${fmtTime(sp.start)} – ${sp.end.toLocaleDateString([], dOpt)} ${fmtTime(sp.end)}`;
}
export function hourLabel(h: number): string { return new Date(2000, 0, 1, h).toLocaleTimeString([], { hour: "numeric" }); }
export const snap = (min: number, step = 15) => Math.round(min / step) * step;
