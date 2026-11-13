// Tiny localisation layer (no dependency). English is the only catalogue today.
//
// HOW TO TRANSLATE THE REST OF THE UI
//  1. Replace a literal in a component with `t("area.name")` and add the English text to `en` below
//     (keys are `area.name`; use `{var}` placeholders for values: t("vac.until", { date })).
//  2. To add a language: create `lib/i18n-<code>.ts` exporting `Partial<Catalogue>` and register it with
//     `registerLocale("<code>", cat)`; missing keys fall back to English, so a translation can be partial.
//  3. `setLocale("<code>")` switches at runtime (call it from a future Preferences > General language picker).
// Converted so far: Preferences tab names, the Vacation reply tab, the theme names. Everything else is still
// English literals (see docs/ESSENTIALS_SPEC.md, "Localisation").
import { thisDevice } from "./device.ts";

export const en = {
  "prefs.title": "Preferences",
  "prefs.sections": "Preferences sections",
  "prefs.tab.general": "General",
  "prefs.tab.appearance": "Appearance",
  "prefs.tab.composing": "Composing",
  "prefs.tab.signatures": "Signatures",
  "prefs.tab.vacation": "Vacation reply",
  "prefs.tab.sync": "Sync",
  "prefs.tab.notifications": "Notifications",
  "prefs.tab.privacy": "Privacy",
  "prefs.tab.ai": "AI",
  "prefs.tab.shortcuts": "Shortcuts",
  "prefs.tab.about": "About",

  "theme.label": "Theme",
  "theme.system": "System",
  "theme.dark": "Dark",
  "theme.light": "Light",
  "theme.contrast": "High contrast",

  "vac.title": "Vacation reply",
  "vac.intro": `MiraMail asks your mail server to answer incoming mail for you, so it keeps working while ${thisDevice()} is off. It uses your mail server's ManageSieve service; nothing goes anywhere else.`,
  "vac.account": "Account",
  "vac.checking": "Checking what your server supports...",
  "vac.unsupported": "Not available for this account",
  "vac.recheck": "Check again",
  "vac.on": "Send automatic replies",
  "vac.subject": "Subject",
  "vac.subject.ph": "Out of office",
  "vac.message": "Message",
  "vac.start": "First day",
  "vac.end": "Last day",
  "vac.dates.note": "Leave both dates empty to reply until you turn this off.",
  "vac.dates.unsupported": "This server cannot limit replies to dates, so the responder stays on until you turn it off.",
  "vac.days": "Days between replies to the same sender",
  "vac.onlyToMe": "Only reply to mail addressed to my address",
  "vac.addresses": "My addresses (comma separated)",
  "vac.save": "Save and turn on",
  "vac.update": "Save changes",
  "vac.turnOff": "Turn off",
  "vac.status.on": "Automatic replies are ON for {account}.",
  "vac.status.off": "Automatic replies are off.",
  "vac.saved": "Vacation reply saved and turned on.",
  "vac.disabled": "Vacation reply turned off.",
  "vac.restored": "Turned off. Your previous mail filter \"{name}\" is active again.",
  "vac.confirm.title": "Replace your active mail filter?",
  "vac.confirm.body": "Your server is already using the filter \"{name}\". Only one filter can be active, so turning on the vacation reply switches that one off while it runs. Nothing is deleted.",
  "vac.confirm.go": "Replace it",
  "vac.cancel": "Cancel",
  "vac.restore": "Switch my previous mail filter back on when I turn this off",
  "vac.advanced": "Advanced: server",
  "vac.host": "ManageSieve host",
  "vac.port": "Port",
  "vac.tls": "Security",
  "vac.tls.starttls": "STARTTLS",
  "vac.tls.implicit": "TLS",
  "vac.sieveNote": "Default: the same host as your IMAP server, port 4190, STARTTLS.",
  "vac.err.message": "Write the message people should receive.",
  "vac.err.days": "Days between replies must be 1 to 30.",
  "vac.err.dates": "The last day is before the first day.",
  "vac.err.addresses": "Add at least one address, or turn off \"only reply to my address\".",
} as const;

export type Key = keyof typeof en;
export type Catalogue = Record<Key, string>;

const locales = new Map<string, Partial<Catalogue>>([["en", en]]);
let current = "en";

export function registerLocale(code: string, cat: Partial<Catalogue>): void { locales.set(code, cat); }
export function setLocale(code: string): void { if (locales.has(code)) current = code; }
export function getLocale(): string { return current; }

/** Look up `key` (current locale, then English) and fill `{name}` placeholders. Unknown placeholders stay as written. */
export function t(key: Key, vars?: Record<string, string | number>): string {
  const raw = locales.get(current)?.[key] ?? en[key];
  return vars ? raw.replace(/\{(\w+)\}/g, (m, k: string) => (k in vars ? String(vars[k]) : m)) : raw;
}
