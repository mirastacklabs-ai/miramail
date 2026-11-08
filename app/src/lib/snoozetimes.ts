// Pure helpers for Snooze and Send later: preset times (local time zone), custom-time validation, labels.
// No DOM, no Tauri. `now` is always passed in so everything is testable with a fixed clock.

export interface Preset { id: "later" | "tomorrow" | "weekend" | "nextweek"; label: string; at: Date }

const at = (base: Date, addDays: number, h: number, m = 0): Date => {
  const d = new Date(base.getFullYear(), base.getMonth(), base.getDate() + addDays, h, m, 0, 0);
  return d;
};

/** Furthest ahead a snooze / scheduled send may be (one year, matches the backend). */
export const MAX_AHEAD_MS = 365 * 86_400_000;
/** Smallest lead for a custom time. */
export const MIN_AHEAD_MS = 60_000;

/**
 * The quick choices, always in the future relative to `now`:
 * - later today: 18:00 before 15:00, else three hours from now when that is still today (otherwise not offered);
 * - tomorrow morning 08:00; this weekend = coming Saturday 08:00; next week = coming Monday 08:00.
 */
export function presets(now: Date): Preset[] {
  const out: Preset[] = [];
  let later: Date | null = null;
  if (now.getHours() < 15) later = at(now, 0, 18);
  else {
    const t = new Date(now.getTime() + 3 * 3_600_000);
    t.setSeconds(0, 0);
    t.setMinutes(Math.ceil(t.getMinutes() / 15) * 15);
    if (t.getDate() === now.getDate() && t.getTime() > now.getTime()) later = t;
  }
  if (later) out.push({ id: "later", label: "Later today", at: later });
  out.push({ id: "tomorrow", label: "Tomorrow morning", at: at(now, 1, 8) });
  const dow = now.getDay(); // 0 Sunday .. 6 Saturday
  const toSat = dow === 6 ? 7 : (6 - dow + 7) % 7;
  // Friday evening, Saturday and Sunday all mean "the coming Saturday"; on Saturday that is next week's.
  out.push({ id: "weekend", label: "This weekend", at: at(now, toSat === 0 ? 7 : toSat, 8) });
  const toMon = ((1 - dow + 7) % 7) || 7;
  out.push({ id: "nextweek", label: "Next week", at: at(now, toMon, 8) });
  return out;
}

/** `value` of an `<input type="datetime-local">` for a date (local time, minutes precision). */
export function toLocalInput(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** Parse a datetime-local value into a Date, or null when empty / invalid. */
export function fromLocalInput(v: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(v.trim());
  if (!m) return null;
  const [y, mo, d, h, mi] = m.slice(1).map(Number);
  const dt = new Date(y, mo - 1, d, h, mi, 0, 0);
  // Reject rolled-over dates such as 2026-02-31.
  return dt.getFullYear() === y && dt.getMonth() === mo - 1 && dt.getDate() === d ? dt : null;
}

/** Why a chosen time cannot be used, or null when it is fine. */
export function checkTime(when: Date | null, now: Date): string | null {
  if (!when) return "Enter a date and time.";
  const ms = when.getTime() - now.getTime();
  if (ms < MIN_AHEAD_MS) return "Pick a time at least a minute from now.";
  if (ms > MAX_AHEAD_MS) return "Pick a time within the next year.";
  return null;
}

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "Tomorrow 08:00", "Sat 8 Nov 08:00", "Today 18:00". */
export function whenLabel(ts: number, now: Date): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  const hm = `${p(d.getHours())}:${p(d.getMinutes())}`;
  const day0 = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day0(d) - day0(now)) / 86_400_000);
  if (diff === 0) return `Today ${hm}`;
  if (diff === 1) return `Tomorrow ${hm}`;
  return `${DAYS[d.getDay()]} ${d.getDate()} ${MONTHS[d.getMonth()]}${d.getFullYear() !== now.getFullYear() ? ` ${d.getFullYear()}` : ""} ${hm}`;
}
