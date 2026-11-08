import { attrValue, decodeEntities, tokens } from "./csshide.ts";

// Compatibility lint for the rich composer: pure functions that look at the HTML about to be sent and list what some
// mail apps may not show. Advisory only (never blocks sending). No runtime imports.

export interface LintWarning { id: "remote-image" | "no-alt" | "wide-table" | "low-contrast"; message: string; hint: string; count: number }

export const MAX_TABLE_PX = 600;
export const MIN_CONTRAST = 4.5;

type RGB = [number, number, number];
const NAMED: Record<string, RGB> = {
  black: [0, 0, 0], white: [255, 255, 255], red: [255, 0, 0], green: [0, 128, 0], blue: [0, 0, 255], gray: [128, 128, 128], grey: [128, 128, 128], silver: [192, 192, 192],
  maroon: [128, 0, 0], navy: [0, 0, 128], purple: [128, 0, 128], teal: [0, 128, 128], yellow: [255, 255, 0], orange: [255, 165, 0], lime: [0, 255, 0], aqua: [0, 255, 255],
  fuchsia: [255, 0, 255], olive: [128, 128, 0], lightgray: [211, 211, 211], lightgrey: [211, 211, 211], darkgray: [169, 169, 169], darkgrey: [169, 169, 169], pink: [255, 192, 203], gold: [255, 215, 0],
};

/** #rgb, #rrggbb, rgb()/rgba() (opaque only) or a basic colour name; null for anything else (transparent, inherit, gradients). */
export function parseColor(v: string | null | undefined): RGB | null {
  const s = (v ?? "").trim().toLowerCase().replace(/\s*!important$/, "");
  let m = /^#([0-9a-f]{3})$/.exec(s);
  if (m) return [...m[1]].map((h) => parseInt(h + h, 16)) as RGB;
  m = /^#([0-9a-f]{6})$/.exec(s);
  if (m) return [0, 2, 4].map((i) => parseInt(m![1].slice(i, i + 2), 16)) as RGB;
  m = /^rgba?\(\s*(\d{1,3})\s*[, ]\s*(\d{1,3})\s*[, ]\s*(\d{1,3})\s*(?:[,/]\s*([\d.]+%?)\s*)?\)$/.exec(s);
  if (m) {
    const a = m[4] == null ? 1 : m[4].endsWith("%") ? parseFloat(m[4]) / 100 : parseFloat(m[4]);
    return a >= 0.99 ? [Math.min(255, +m[1]), Math.min(255, +m[2]), Math.min(255, +m[3])] : null;
  }
  return NAMED[s] ?? null;
}

const lum = ([r, g, b]: RGB) => {
  const f = (c: number) => { const x = c / 255; return x <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4; };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
};
/** WCAG contrast ratio, 1 to 21. */
export function contrastRatio(a: RGB, b: RGB): number {
  const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}
const hex = (c: RGB) => "#" + c.map((x) => x.toString(16).padStart(2, "0")).join("");

const dec = decodeEntities;

const attr = attrValue;
function styleProp(attrs: string, prop: string): string | null {
  const st = attr(attrs, "style");
  if (!st) return null;
  let found: string | null = null;
  for (const d of st.split(";")) {
    const i = d.indexOf(":");
    if (i > 0 && d.slice(0, i).trim().toLowerCase() === prop) found = d.slice(i + 1).trim();
  }
  return found;
}
const px = (v: string | null | undefined): number | null => { const m = /^\s*(\d+(?:\.\d+)?)\s*(px)?\s*$/i.exec(v ?? ""); return m ? parseFloat(m[1]) : null; };

const VOID = new Set(["br", "hr", "img", "input", "meta", "link", "col", "wbr", "base", "area", "source", "track", "param"]);
const SKIP = new Set(["script", "style", "head", "title"]);
const isRemote = (s: string) => /^(https?:)?\/\//i.test(s);

/** Everything the lint looks for, in one pass over the markup. `html` may contain pictures as data: URIs; their payload is ignored. */
export function lintCompose(html: string): LintWarning[] {
  const src = html.replace(/(data:[a-z/+.-]+;base64,)[A-Za-z0-9+/=\s]*/gi, "$1");
  let remote = 0, noAlt = 0, wide = 0, widest = 0;
  const lowPairs = new Map<string, number>();
  const stack: { name: string; fg: RGB | null; bg: RGB | null }[] = [];
  let rowPx: number[] = [];
  let skip = "", skipN = 0;
  const checkWide = (w: number) => { if (w > MAX_TABLE_PX) { wide++; widest = Math.max(widest, w); } };
  for (const m of tokens(src)) {
    if (m.text) {
      if (skip) continue;
      if (dec(src.slice(m.index, m.end)).replace(/[\s ]/g, "") === "") continue;
      const e = stack[stack.length - 1];
      const fg = e?.fg ?? null, bg = e?.bg ?? null;
      if (!fg && !bg) continue; // nothing explicit: the app's own defaults apply
      const f = fg ?? [0, 0, 0], b = bg ?? [255, 255, 255];
      if (contrastRatio(f, b) < MIN_CONTRAST) { const k = `${hex(f)} on ${hex(b)}`; lowPairs.set(k, (lowPairs.get(k) ?? 0) + 1); }
      continue;
    }
    const { closing, tag, attrs } = m;
    if (skip) { if (tag === skip) { if (!closing) skipN++; else if (--skipN <= 0) skip = ""; } continue; }
    if (SKIP.has(tag)) { if (!closing) { skip = tag; skipN = 1; } continue; }
    if (tag === "img") {
      if (closing) continue;
      const s = (attr(attrs, "src") ?? "").trim();
      if (isRemote(s)) remote++;
      if (s && !(attr(attrs, "alt") ?? "").trim()) noAlt++; // pictures whose data was dropped from a draft have no src: not counted
      continue;
    }
    if (VOID.has(tag)) continue;
    if (closing) {
      const k = stack.map((s) => s.name).lastIndexOf(tag);
      if (k >= 0) stack.splice(k);
      if (tag === "tr") { const sum = rowPx.reduce((a, b) => a + b, 0); rowPx = []; checkWide(sum); }
      continue;
    }
    const parent = stack[stack.length - 1];
    let fg = parent?.fg ?? null, bg = parent?.bg ?? null;
    const c = parseColor(styleProp(attrs, "color") ?? (tag === "font" ? attr(attrs, "color") : null));
    if (c) fg = c;
    const bgShort = styleProp(attrs, "background");
    const b2 = parseColor(styleProp(attrs, "background-color") ?? (bgShort && parseColor(bgShort) ? bgShort : null) ?? attr(attrs, "bgcolor"));
    if (b2) bg = b2;
    const w = px(styleProp(attrs, "width")) ?? px(attr(attrs, "width"));
    if (tag === "table" && w != null) checkWide(w);
    if ((tag === "td" || tag === "th") && w != null) rowPx.push(w); // a row whose px cell widths add up past the limit
    stack.push({ name: tag, fg, bg });
  }
  const out: LintWarning[] = [];
  if (remote) out.push({ id: "remote-image", count: remote, message: remote === 1 ? "A picture is loaded from the web." : `${remote} pictures are loaded from the web.`, hint: "MiraMail does not send web-linked pictures. Paste or drop the picture itself so it goes with the message as an attachment." });
  if (noAlt) out.push({ id: "no-alt", count: noAlt, message: noAlt === 1 ? "A picture has no alt text." : `${noAlt} pictures have no alt text.`, hint: "When pictures are blocked or read aloud nothing shows; say in the text what the picture shows." });
  if (wide) out.push({ id: "wide-table", count: wide, message: `A table is ${Math.round(widest)} px wide.`, hint: `Some mail apps cut off anything past about ${MAX_TABLE_PX} px. Set the table or its columns to percentages, or make them narrower.` });
  if (lowPairs.size) {
    const ex = [...lowPairs.keys()][0];
    const n = [...lowPairs.values()].reduce((a, b) => a + b, 0);
    out.push({ id: "low-contrast", count: n, message: `${n === 1 ? "Some text is" : `${n} pieces of text are`} hard to read (${ex}).`, hint: "Use a darker text colour on a light background (or the other way round); dark-mode mail apps may change the rest." });
  }
  return out;
}
