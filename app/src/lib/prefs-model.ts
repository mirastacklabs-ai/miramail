// Pure preferences model: defaults, validation, serialisation, CSS mapping. No DOM, no storage.
// Designed so persistence can move from localStorage to SQLite `settings` unchanged (it is just a JSON blob).

export type UiFont = "mira" | "system" | "inter";
export type UiSize = "s" | "m" | "l";
export type BodyFont = "system" | "serif" | "mono" | "oswald";
export type Density = "compact" | "comfortable" | "spacious";
export type DateFormat = "relative" | "absolute";
export type MarkRead = "immediately" | "delay" | "never";
export type AfterRemove = "next" | "previous" | "none";
/** Undo-send hold in seconds (0 = send at once). */
export type UndoSendSecs = 0 | 5 | 10 | 30;
export const UNDO_SEND_OPTIONS: readonly UndoSendSecs[] = [0, 5, 10, 30];

export interface Prefs {
  uiFont: UiFont;
  uiSize: UiSize;
  bodyFont: BodyFont;
  bodySize: number;
  /** MIRA chat text and diagram scale (1 = normal). */
  chatScale: number;
  lineHeight: number;
  plainMono: boolean;
  density: Density;
  snippetLines: 0 | 1 | 2;
  showDot: boolean;
  dateFormat: DateFormat;
  markRead: MarkRead;
  afterRemove: AfterRemove;
  /** Composing: how long Send holds the message so it can be undone. */
  undoSendSecs: UndoSendSecs;
  /** Composing: mirror autosaved drafts to the account's Drafts folder on the server. */
  mirrorDrafts: boolean;
  /** Composing: local tone hint under the message body (nothing leaves this Mac). */
  toneMeter: boolean;
  /** Message list: group messages into conversations (computed locally; docs/ESSENTIALS_SPEC.md). */
  threading: boolean;
}

export const STORAGE_KEY = "miramail.prefs.v1";
export const VERSION = 1;
export const MARK_READ_DELAY_MS = 2000;

export const DEFAULTS: Readonly<Prefs> = Object.freeze({
  uiFont: "mira", uiSize: "m", bodyFont: "system", bodySize: 14, chatScale: 1, lineHeight: 1.5, plainMono: true,
  density: "comfortable", snippetLines: 1, showDot: true, dateFormat: "relative", markRead: "immediately", afterRemove: "next", undoSendSecs: 5, mirrorDrafts: true, toneMeter: true, threading: false,
});

export const BODY_SIZE = { min: 12, max: 22 } as const;
export const CHAT_SCALE = { min: 0.8, max: 1.8, step: 0.1 } as const;
export const LINE_HEIGHT = { min: 1.3, max: 2.0 } as const;

const ENUMS = {
  uiFont: ["mira", "system", "inter"], uiSize: ["s", "m", "l"], bodyFont: ["system", "serif", "mono", "oswald"],
  density: ["compact", "comfortable", "spacious"], dateFormat: ["relative", "absolute"],
  markRead: ["immediately", "delay", "never"], afterRemove: ["next", "previous", "none"],
} as const;

export const UI_PX: Record<UiSize, number> = { s: 14, m: 15, l: 16 };
export const DENSITY_PX: Record<Density, { row: number; folder: number }> = {
  compact: { row: 6, folder: 4 }, comfortable: { row: 10, folder: 7 }, spacious: { row: 14, folder: 10 },
};

export const UI_STACKS: Record<UiFont, string> = {
  mira: '"Oswald", -apple-system, system-ui, sans-serif',
  system: '-apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif',
  inter: '"Inter", "Helvetica Neue", -apple-system, system-ui, sans-serif',
};
export const BODY_STACKS: Record<BodyFont, string> = {
  system: '-apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, sans-serif',
  serif: 'Georgia, "Times New Roman", serif',
  mono: '"JetBrains Mono", ui-monospace, Menlo, monospace',
  oswald: '"Oswald", "Arial Narrow", sans-serif',
};
/** The sandboxed iframe cannot load the bundled webfonts (CSP font-src none), so use local fallbacks only. */
export const IFRAME_STACKS: Record<BodyFont, string> = {
  system: BODY_STACKS.system, serif: BODY_STACKS.serif,
  mono: 'ui-monospace, Menlo, "SF Mono", monospace', oswald: '"Arial Narrow", "Helvetica Neue", sans-serif',
};

const num = (v: unknown, lo: number, hi: number, fb: number, step: number) => {
  if (typeof v !== "number" || !Number.isFinite(v)) return fb;
  const c = Math.min(hi, Math.max(lo, v));
  return +(Math.round(c / step) * step).toFixed(2);
};
const oneOf = <T extends string>(list: readonly T[], v: unknown, fb: T): T => (typeof v === "string" && (list as readonly string[]).includes(v) ? (v as T) : fb);

/** Coerce anything into a complete, valid Prefs. Unknown keys are dropped; bad values fall back per key. */
export function validate(raw: unknown): Prefs {
  const r = (raw && typeof raw === "object" && !Array.isArray(raw) ? raw : {}) as Record<string, unknown>;
  const d = DEFAULTS;
  return {
    uiFont: oneOf(ENUMS.uiFont, r.uiFont, d.uiFont),
    uiSize: oneOf(ENUMS.uiSize, r.uiSize, d.uiSize),
    bodyFont: oneOf(ENUMS.bodyFont, r.bodyFont, d.bodyFont),
    bodySize: Math.round(num(r.bodySize, BODY_SIZE.min, BODY_SIZE.max, d.bodySize, 1)),
    chatScale: num(r.chatScale, CHAT_SCALE.min, CHAT_SCALE.max, d.chatScale, CHAT_SCALE.step),
    lineHeight: num(r.lineHeight, LINE_HEIGHT.min, LINE_HEIGHT.max, d.lineHeight, 0.05),
    plainMono: typeof r.plainMono === "boolean" ? r.plainMono : d.plainMono,
    density: oneOf(ENUMS.density, r.density, d.density),
    snippetLines: r.snippetLines === 0 || r.snippetLines === 1 || r.snippetLines === 2 ? r.snippetLines : d.snippetLines,
    showDot: typeof r.showDot === "boolean" ? r.showDot : d.showDot,
    dateFormat: oneOf(ENUMS.dateFormat, r.dateFormat, d.dateFormat),
    markRead: oneOf(ENUMS.markRead, r.markRead, d.markRead),
    afterRemove: oneOf(ENUMS.afterRemove, r.afterRemove, d.afterRemove),
    undoSendSecs: UNDO_SEND_OPTIONS.find((n) => n === r.undoSendSecs) ?? d.undoSendSecs,
    mirrorDrafts: typeof r.mirrorDrafts === "boolean" ? r.mirrorDrafts : d.mirrorDrafts,
    toneMeter: typeof r.toneMeter === "boolean" ? r.toneMeter : d.toneMeter,
    threading: typeof r.threading === "boolean" ? r.threading : d.threading,
  };
}

/** Parse a stored JSON string; null, corrupt JSON or wrong shape yields defaults. Accepts `{v, ...prefs}` or bare prefs. */
export function parse(s: string | null | undefined): Prefs {
  if (!s) return { ...DEFAULTS };
  try { return validate(JSON.parse(s)); } catch { return { ...DEFAULTS }; }
}
export const serialize = (p: Prefs): string => JSON.stringify({ v: VERSION, ...validate(p) });

/** CSS custom properties applied on :root. */
export function cssVars(p: Prefs): Record<string, string> {
  const dn = DENSITY_PX[p.density];
  return {
    "--ui-size": `${UI_PX[p.uiSize]}px`,
    "--font-ui": UI_STACKS[p.uiFont],
    "--body-font": p.plainMono ? BODY_STACKS.mono : BODY_STACKS[p.bodyFont],
    "--html-font": BODY_STACKS[p.bodyFont],
    "--body-size": `${p.bodySize}px`,
    "--chat-zoom": String(p.chatScale),
    "--body-lh": String(p.lineHeight),
    "--row-py": `${dn.row}px`,
    "--folder-py": `${dn.folder}px`,
    "--snip-lines": String(p.snippetLines),
  };
}

/** Base style for the sandboxed HTML message document. HTML mail's own styles come later and win. */
export function bodyBaseCss(p: Prefs): string {
  return `body{margin:0;padding:16px;font:${p.bodySize}px/${p.lineHeight} ${IFRAME_STACKS[p.bodyFont]};overflow-wrap:anywhere}`;
}
