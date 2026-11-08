// Shared, pure helpers for reading untrusted tag attributes and inline styles: entity decoding, an attribute parser, and
// the "does this style hide its element" check (numeric, mirroring crates/mailcore/src/sanitize.rs `style_hides`).
// No DOM, no runtime imports: loadable by `node --experimental-strip-types --test`.

// ---- entities ----

const ENTITY_LIST: [string, string][] = [
  ["amp", "&"], ["lt", "<"], ["gt", ">"], ["quot", '"'], ["apos", "'"], ["nbsp", " "], ["ndash", "–"], ["mdash", "—"], ["hellip", "…"], ["copy", "©"],
  ["euro", "€"], ["pound", "£"], ["yen", "¥"], ["colon", ":"], ["semi", ";"], ["tab", "\t"], ["newline", "\n"], ["excl", "!"], ["percnt", "%"], ["period", "."],
  ["comma", ","], ["lpar", "("], ["rpar", ")"], ["sol", "/"], ["num", "#"], ["lowbar", "_"], ["hyphen", "-"],
];
/** Named entities we decode. A Map, so `&constructor;` and `&__proto__;` are never found and stay literal text. */
export const ENTITIES: ReadonlyMap<string, string> = new Map(ENTITY_LIST);

export function decodeEntities(s: string): string {
  return s.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (m, e: string) => {
    if (e[0] === "#") {
      const n = e[1].toLowerCase() === "x" ? parseInt(e.slice(2), 16) : parseInt(e.slice(1), 10);
      return Number.isFinite(n) && n > 0 && n < 0x110000 ? String.fromCodePoint(n) : m;
    }
    return ENTITIES.get(e.toLowerCase()) ?? m;
  });
}

// ---- attributes ----

const isWs = (c: number) => c === 32 || c === 9 || c === 10 || c === 12 || c === 13;
let lastAttrs = "", lastMap = new Map<string, string>();

/** Attributes of a tag (the text after the tag name), HTML-tokenizer style: first occurrence wins, values entity-decoded, linear time. */
export function parseAttrs(attrs: string): Map<string, string> {
  if (attrs === lastAttrs) return lastMap;
  const out = new Map<string, string>();
  const n = attrs.length;
  let i = 0;
  while (i < n) {
    while (i < n && (isWs(attrs.charCodeAt(i)) || attrs[i] === "/")) i++;
    const s = i;
    while (i < n && !isWs(attrs.charCodeAt(i)) && attrs[i] !== "=" && attrs[i] !== "/") i++;
    const name = attrs.slice(s, i).toLowerCase();
    while (i < n && isWs(attrs.charCodeAt(i))) i++;
    let value = "";
    if (attrs[i] === "=") {
      i++;
      while (i < n && isWs(attrs.charCodeAt(i))) i++;
      const q = attrs[i];
      if (q === '"' || q === "'") {
        const e = attrs.indexOf(q, i + 1);
        value = attrs.slice(i + 1, e < 0 ? n : e);
        i = e < 0 ? n : e + 1;
      } else {
        const v = i;
        while (i < n && !isWs(attrs.charCodeAt(i))) i++;
        value = attrs.slice(v, i);
      }
    } else if (i === s) i++; // lone "=" or stray character: make progress
    if (name && !out.has(name)) out.set(name, decodeEntities(value));
  }
  lastAttrs = attrs; lastMap = out;
  return out;
}

export const attrValue = (attrs: string, name: string): string | null => parseAttrs(attrs).get(name.toLowerCase()) ?? null;

// ---- tokenizer ----

export interface Tok { text: boolean; index: number; end: number; closing: boolean; tag: string; attrs: string }
const isAlpha = (c: number) => (c >= 65 && c <= 90) || (c >= 97 && c <= 122);
const isSp = isWs;

/**
 * Linear-time HTML tokenizer (text runs and tags; comments, doctypes and processing instructions are skipped). Like a
 * browser it lets an unterminated tag or comment swallow the rest of the input, so `<a` x 1e6, `<!--` x 1e6 or
 * `<style` x 1e6 cost one pass, not one pass per `<` (the regex it replaces was quadratic on those).
 */
export function* tokens(html: string): Generator<Tok> {
  const n = html.length;
  let i = 0;
  while (i < n) {
    const lt = html.indexOf("<", i);
    if (lt < 0) { yield { text: true, index: i, end: n, closing: false, tag: "", attrs: "" }; return; }
    if (lt > i) yield { text: true, index: i, end: lt, closing: false, tag: "", attrs: "" };
    const c1 = html.charCodeAt(lt + 1);
    if (html.startsWith("<!--", lt)) { const e = html.indexOf("-->", lt + 4); if (e < 0) return; i = e + 3; continue; }
    if (c1 === 33 || c1 === 63) { const e = html.indexOf(">", lt + 2); if (e < 0) return; i = e + 1; continue; }
    const closing = c1 === 47;
    let j = lt + (closing ? 2 : 1);
    if (!isAlpha(html.charCodeAt(j))) { yield { text: true, index: lt, end: lt + 1, closing: false, tag: "", attrs: "" }; i = lt + 1; continue; }
    const ns = j;
    while (j < n) { const c = html.charCodeAt(j); if (isSp(c) || c === 47 || c === 62) break; j++; }
    const tag = html.slice(ns, j).toLowerCase();
    const as = j;
    let last = 0;
    for (; j < n; j++) {
      const c = html.charCodeAt(j);
      if (c === 62) break;
      if ((c === 34 || c === 39) && last === 61) { const e = html.indexOf(html[j], j + 1); if (e < 0) return; j = e; last = c; continue; }
      if (!isSp(c)) last = c;
    }
    if (j >= n) return;
    yield { text: false, index: lt, end: j + 1, closing, tag, attrs: html.slice(as, j) };
    i = j + 1;
  }
}

// ---- styles ----

/** CSS with comments removed (indexOf scan, linear) and escapes resolved, lower-cased. */
export function cssText(style: string): string {
  let s = style;
  if (s.includes("/*")) {
    let out = "", at = 0;
    for (;;) {
      const a = s.indexOf("/*", at);
      if (a < 0) { out += s.slice(at); break; }
      out += s.slice(at, a);
      const b = s.indexOf("*/", a + 2);
      if (b < 0) break;
      at = b + 2;
    }
    s = out;
  }
  if (s.includes("\\")) s = s.replace(/\\([0-9a-f]{1,6})[ \t\n\r\f]?|\\([\s\S])/gi, (_m, hex?: string, ch?: string) => {
    if (hex) { const n = parseInt(hex, 16); return n > 0 && n < 0x110000 ? String.fromCodePoint(n) : ""; }
    return ch ?? "";
  });
  return s.toLowerCase();
}

/** Longest single declaration value we inspect; anything longer is "not a number" and never hidden-by-number. */
export const MAX_CSS_VALUE = 200;

const isCssWs = (c: number) => c === 32 || (c >= 9 && c <= 13) || c === 0xa0 || c === 0xfeff || c === 0x2028 || c === 0x2029 || c === 0x1680 || (c >= 0x2000 && c <= 0x200a) || c === 0x202f || c === 0x205f || c === 0x3000;
const isDig = (c: number) => c >= 48 && c <= 57;

/** Leading CSS number (sign, digits, fraction, exponent) and the unit after it; null for keywords (`small`, `inherit`), anything else, and values over MAX_CSS_VALUE. Single linear scan. */
export function cssNum(s: string): { n: number; unit: string } | null {
  if (s.length > MAX_CSS_VALUE + 2 * MAX_CSS_VALUE) return null; // cheap guard before trimming huge whitespace runs
  s = s.trim();
  if (s.length > MAX_CSS_VALUE || s.length === 0) return null;
  const len = s.length;
  let i = 0;
  const c0 = s.charCodeAt(0);
  if (c0 === 43 || c0 === 45) i++;
  const d0 = i;
  while (i < len && isDig(s.charCodeAt(i))) i++;
  let nd = i - d0;
  if (i < len && s.charCodeAt(i) === 46) {
    i++;
    const f = i;
    while (i < len && isDig(s.charCodeAt(i))) i++;
    nd += i - f;
  }
  if (nd === 0) return null;
  if (i < len && (s.charCodeAt(i) | 32) === 101) { // exponent only when digits follow, else `e` starts the unit (em, ex)
    let j = i + 1;
    if (j < len && (s.charCodeAt(j) === 43 || s.charCodeAt(j) === 45)) j++;
    const e0 = j;
    while (j < len && isDig(s.charCodeAt(j))) j++;
    if (j > e0) i = j;
  }
  const numEnd = i;
  while (i < len && isCssWs(s.charCodeAt(i))) i++;
  const u0 = i;
  while (i < len) {
    const c = s.charCodeAt(i) | 32;
    if ((c >= 97 && c <= 122) || s.charCodeAt(i) === 37) i++; else break;
  }
  const unit = s.slice(u0, i);
  while (i < len && isCssWs(s.charCodeAt(i))) i++;
  if (i !== len) return null;
  const n = Number(s.slice(0, numEnd));
  return Number.isNaN(n) ? null : { n, unit: unit.toLowerCase() };
}

export function opacityHides(v: string): boolean {
  const c = cssNum(v);
  return !!c && (c.unit === "%" ? c.n < 10 : c.unit === "" && c.n < 0.1);
}

/** A font size below about 2 px in any unit; keywords (small, medium, large, smaller, inherit ...) are never hidden. */
export function fontSizeHides(v: string): boolean {
  const c = cssNum(v);
  if (!c) return false;
  const { n } = c;
  switch (c.unit) {
    case "": case "px": return n < 2;
    case "pt": return n < 1.5;
    case "em": case "rem": case "ex": case "ch": case "pc": return n < 0.125;
    case "%": return n < 12.5;
    case "mm": return n < 0.5;
    case "cm": return n < 0.05;
    case "in": return n < 0.02;
    case "q": return n < 2;
    case "vw": case "vh": case "vmin": case "vmax": return n < 0.2;
    default: return n <= 0;
  }
}

/** `transparent`, or a colour with alpha below 0.1 (rgba, hsla, `rgb(r g b / a)`, #rgba, #rrggbbaa). */
export function colorHides(v: string): boolean {
  v = v.trim();
  if (v === "transparent") return true;
  if (v[0] === "#") {
    const h = v.slice(1);
    if (!/^[0-9a-f]+$/.test(h)) return false;
    if (h.length === 4) return parseInt(h[3] + h[3], 16) < 26;
    if (h.length === 8) return parseInt(h.slice(6), 16) < 26;
    return false;
  }
  const m = /^(?:rgba?|hsla?)\(([^)]*)\)$/.exec(v);
  if (!m) return false;
  let a: string | undefined;
  if (m[1].includes("/")) a = m[1].split("/")[1];
  else { const p = m[1].split(","); if (p.length === 4) a = p[3]; }
  if (a === undefined) return false;
  const c = cssNum(a);
  return !!c && (c.unit === "%" ? c.n < 10 : c.unit === "" && c.n < 0.1);
}

/** Pasted-source extras: a size of 0 with overflow hidden, or text pushed far off-screen. */
const OFFSCREEN = /^(left|right|top|bottom|text-indent|margin(-left|-right|-top|-bottom)?)$/;

/**
 * True when an inline style hides its element. Same rules as the backend sanitiser (display:none, visibility:hidden|collapse,
 * opacity < 0.1, font-size < 2px, transparent `color`). With `paste` also `mso-hide:all`, off-screen offsets (<= -500px / any
 * unit) and a zero box with overflow hidden: used for pasted tables, where hidden content must be dropped, not revealed.
 */
export function styleHides(style: string, paste = false): boolean {
  const decls = new Map<string, string>();
  for (const d of cssText(style).split(";")) {
    const i = d.indexOf(":");
    if (i <= 0) continue;
    const prop = d.slice(0, i).trim();
    let val = d.slice(i + 1);
    const bang = val.indexOf("!");
    if (bang >= 0) val = val.slice(0, bang);
    val = val.trim();
    if (val.length > MAX_CSS_VALUE) continue; // hostile or junk value: not inspected, not hidden
    decls.set(prop, val);
  }
  for (const [prop, val] of decls) {
    switch (prop) {
      case "display": if (val === "none") return true; break;
      case "visibility": if (val === "hidden" || val === "collapse") return true; break;
      case "opacity": if (opacityHides(val)) return true; break;
      case "font-size": if (fontSizeHides(val)) return true; break;
      case "color": if (colorHides(val)) return true; break;
      case "font": for (const t of val.split(/\s+/)) { const size = t.split("/")[0]; const c = cssNum(size); if (c && (c.unit !== "" || /^[+-]?0/.test(size)) && fontSizeHides(size)) return true; } break;
      default:
        if (!paste) break;
        if (prop === "mso-hide" && val === "all") return true;
        if (OFFSCREEN.test(prop)) { const c = cssNum(val); if (c && c.n <= -500) return true; }
    }
  }
  if (paste) {
    const zero = (p: string) => { const c = cssNum(decls.get(p) ?? ""); return !!c && c.n === 0; };
    if ((zero("height") || zero("max-height") || zero("width") || zero("max-width")) && /^(hidden|clip)$/.test(decls.get("overflow") ?? "")) return true;
  }
  return false;
}

/** True when a tag's attributes hide it (see `styleHides`); with `paste` also the `hidden` attribute. */
export function attrsHide(attrs: string, paste = false): boolean {
  const a = parseAttrs(attrs);
  if (paste && a.has("hidden")) return true;
  const st = a.get("style");
  return !!st && styleHides(st, paste);
}
