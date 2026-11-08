// Vacation reply (server-side Sieve) types + pure helpers. Wire shapes match src-tauri/src/vacation.rs (camelCase).

export interface VacationSettings {
  subject: string;
  message: string;
  /** YYYY-MM-DD or null */
  start: string | null;
  end: string | null;
  days: number;
  onlyToMe: boolean;
  addresses: string[];
}

export type VacationStatus =
  | { kind: "unsupported"; reason: string }
  | { kind: "ready"; active: boolean; settings: VacationSettings | null; otherActive: string | null; datesSupported: boolean };

export type EnableOutcome = { kind: "done"; replaced: string | null } | { kind: "needs_confirm"; active: string };
export interface DisableOutcome { restored: string | null; wasActive: boolean }

/** Advanced override; any field left out uses the default (IMAP host, 4190, STARTTLS). */
export interface SieveOverride { host?: string; port?: number; tls?: "starttls" | "implicit" }

export function defaultSettings(email: string): VacationSettings {
  return { subject: "", message: "", start: null, end: null, days: 7, onlyToMe: true, addresses: [email] };
}

export function parseAddresses(text: string): string[] {
  const seen: string[] = [];
  for (const a of text.split(/[,;\s]+/)) { const x = a.trim().toLowerCase(); if (x && !seen.includes(x)) seen.push(x); }
  return seen;
}

/** Returns an i18n key for the first problem, or null. The server validates again; this only saves a round trip. */
export function validate(s: VacationSettings): "vac.err.message" | "vac.err.days" | "vac.err.dates" | "vac.err.addresses" | null {
  if (!s.message.trim()) return "vac.err.message";
  if (!Number.isInteger(s.days) || s.days < 1 || s.days > 30) return "vac.err.days";
  if (s.start && s.end && s.end < s.start) return "vac.err.dates";
  if (s.onlyToMe && s.addresses.length === 0) return "vac.err.addresses";
  return null;
}

/** Override to send: undefined unless the user changed something. */
export function overrideFor(o: { host: string; port: string; tls: "starttls" | "implicit" }): SieveOverride | undefined {
  const host = o.host.trim();
  const port = Number.parseInt(o.port, 10);
  const out: SieveOverride = {};
  if (host) out.host = host;
  if (Number.isInteger(port) && port > 0 && port < 65536 && port !== 4190) out.port = port;
  if (o.tls === "implicit") out.tls = "implicit";
  return Object.keys(out).length ? out : undefined;
}
