// Reader link gate: the message text shows at once, but its links stay inert until BOTH local checks for that message
// (message_analysis + safety_report) have finished. A failed check keeps them inert (safe default) and the Reader
// offers a retry. Pure + DOM-shape helpers so they are tested without a browser (linkgate.test.ts).
//
// Layers: (1) `holdHtml` rewrites the HTML STRING before it becomes the srcdoc, so the frame never parses a live link
// (href -> data-mm-href on <a>/<area>, any other href / xlink:href / action / formaction / target / ping / tabindex
// dropped, <meta>/<base> dropped); (2) `holdLinks` re-checks the parsed frame DOM; (3) `handleMailLink` ignores
// mail://link while the gate is not ready. The sanitizer (mailcore sanitize.rs, `reader_link_fixture` test) already
// leaves only plain `<a href>`; these layers do not rely on that.
import { linkAction } from "./util.ts";

export type CheckState = "pending" | "ok" | "failed";
export type GateState = "pending" | "ready" | "failed";

/** Ready only when both checks succeeded; failed once neither is still running and one failed. */
export function gateOf(analysis: CheckState, safety: CheckState): GateState {
  if (analysis === "pending" || safety === "pending") return "pending";
  return analysis === "ok" && safety === "ok" ? "ready" : "failed";
}

/** One identity per opened message (account, folder, UIDVALIDITY, uid). */
export const msgKey = (account: string, folder: string, uidvalidity: number, uid: number) => `${account}\u0000${folder}\u0000${uidvalidity}\u0000${uid}`;

/** The key of a shown message (its body summary); "" for none. */
export const bodyKeyOf = (m: { account: string; folder: string; uidvalidity?: number; uid: number } | null | undefined) =>
  m ? msgKey(m.account, m.folder, m.uidvalidity ?? 0, m.uid) : "";

/** The gate of the message on screen: the checks' results count only when they were run for exactly that message
 *  (account, folder, UIDVALIDITY, uid); anything else is "pending". */
export function gateFor(checksKey: string, shownKey: string, analysis: CheckState, safety: CheckState): GateState {
  if (!shownKey || checksKey !== shownKey) return "pending";
  return gateOf(analysis, safety);
}

/** Pure model of the app's check bookkeeping (state.svelte.ts follows it; linkgate.test.ts races it). */
export class CheckBook {
  key = "";
  token = 0;
  analysis: CheckState = "pending";
  safety: CheckState = "pending";
  /** A new open (or folder / account switch): every earlier result is void. */
  reset(): number { this.key = ""; this.analysis = this.safety = "pending"; return ++this.token; }
  /** The body arrived: checks now run for this message. */
  begin(token: number, key: string): boolean { if (token !== this.token) return false; this.key = key; return true; }
  /** A check finished; applied only for the same open AND message. */
  done(token: number, key: string, which: "analysis" | "safety", s: CheckState): boolean {
    if (token !== this.token || key !== this.key) return false;
    this[which] = s;
    return true;
  }
  gate(shownKey: string): GateState { return gateFor(this.key, shownKey, this.analysis, this.safety); }
}

/** Where a held link keeps its address (set only by `holdHtml` / `holdLinks`; a sender's own copy is removed). */
export const HELD_ATTR = "data-mm-href";
/** Styling of held links, plus a click block for anything that still had an href. Removed on release. */
export const HOLD_STYLE = `<style data-mm-hold>a,area{pointer-events:none!important;cursor:default!important}[${HELD_ATTR}]{text-decoration:underline dotted}</style>`;

const LINK_TAGS = new Set(["a", "area"]);
const DROP_ATTRS = new Set(["target", "xlink:href", "action", "formaction", "ping", "tabindex", "accesskey", HELD_ATTR]);
/** Dropped wherever they appear (SMIL elements can set an href on another element). */
const DROP_TAGS = new Set(["meta", "base", "link", "set", "animate", "animatetransform", "animatemotion"]);
/** Foreign content (its own parsing rules, SVG links): dropped with everything inside. */
const FOREIGN = new Set(["svg", "math"]);
/** Raw-text elements: their content is not markup, copied as is up to the matching end tag. (Not noscript: the frame
 *  runs no scripts, so its content IS parsed as markup.) */
const RAW_TEXT = new Set(["script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes"]);

export interface Held { html: string; links: number }
let memo: { src: string; out: Held } | null = null;

const isWs = (c: string) => c === " " || c === "\t" || c === "\n" || c === "\r" || c === "\f";
const isAlpha = (c: string) => (c >= "a" && c <= "z") || (c >= "A" && c <= "Z");

/** End of a comment that starts at `i` ("<!--"), by the HTML rules: "<!-->" and "<!--->" end at once; else the
 *  first "-->" or "--!>"; else the end of input. */
export function commentEnd(html: string, i: number): number {
  const p = i + 4;
  if (html[p] === ">") return p + 1;
  if (html.startsWith("->", p)) return p + 2;
  const a = html.indexOf("-->", p), b = html.indexOf("--!>", p);
  if (a < 0 && b < 0) return html.length;
  if (b < 0 || (a >= 0 && a < b)) return a + 3;
  return b + 4;
}

interface Attr { name: string; value: string }
/** Attributes of a tag whose name ends at `j`: (attributes, index after the tag, whether ">" was seen). */
function readAttrs(html: string, j: number): { attrs: Attr[]; end: number; closed: boolean } {
  const n = html.length, attrs: Attr[] = [];
  while (j < n) {
    while (j < n && isWs(html[j])) j++;
    if (j >= n) break;
    if (html[j] === ">") return { attrs, end: j + 1, closed: true };
    if (html[j] === "/") { j++; continue; } // "/" between attributes means nothing in HTML content
    const ns = j;
    j++; // the first name char may be anything but whitespace, "/" or ">" (even "=")
    while (j < n && !isWs(html[j]) && html[j] !== "/" && html[j] !== ">" && html[j] !== "=") j++;
    const name = html.slice(ns, j);
    let k = j;
    while (k < n && isWs(html[k])) k++;
    let value = "";
    if (html[k] === "=") {
      k++;
      while (k < n && isWs(html[k])) k++;
      const q = html[k];
      if (q === '"' || q === "'") {
        const e = html.indexOf(q, k + 1);
        const end = e < 0 ? n : e + 1;
        value = html.slice(k, end); k = end;
      } else {
        const vs = k;
        while (k < n && !isWs(html[k]) && html[k] !== ">") k++;
        value = html.slice(vs, k);
      }
      j = k;
    }
    attrs.push({ name, value });
  }
  return { attrs, end: n, closed: false };
}

/** Index after the foreign element (svg / math) whose start tag ended at `i`: nested ones counted, CDATA and
 *  comments skipped. Ending too late only drops more; ending too early leaves the rest to the normal (holding) pass. */
function foreignEnd(html: string, i: number): number {
  let depth = 1;
  const n = html.length;
  const re = /<(\/?)(svg|math)[\s/>]|<!\[CDATA\[|<!--/gi;
  re.lastIndex = i;
  for (let m = re.exec(html); m; m = re.exec(html)) {
    if (m[0].startsWith("<![")) { const e = html.indexOf("]]>", m.index); if (e < 0) return n; re.lastIndex = e + 3; continue; }
    if (m[0].startsWith("<!--")) { re.lastIndex = commentEnd(html, m.index); continue; }
    depth += m[1] ? -1 : 1;
    if (depth === 0) { const e = html.indexOf(">", m.index); return e < 0 ? n : e + 1; }
  }
  return n;
}

/**
 * The HTML with every link held: `<a>` / `<area>` keep their address in `data-mm-href` (no href, so nothing can open
 * it: click, keyboard, drag or context menu); everything else that navigates is dropped, and so are svg / math with
 * their content and SMIL set / animate*. `links` = links held. Pure string pass (an HTML-tokenizer subset: quoted /
 * unquoted attribute values, any case, comments by the HTML rules, raw text, end-tag attributes).
 */
export function holdHtml(html: string): Held {
  if (memo?.src === html) return memo.out;
  let out = "", links = 0, i = 0;
  const n = html.length;
  while (i < n) {
    const lt = html.indexOf("<", i);
    if (lt < 0) { out += html.slice(i); break; }
    out += html.slice(i, lt);
    i = lt;
    const c1 = html[i + 1] ?? "";
    if (html.startsWith("<!--", i)) { const end = commentEnd(html, i); out += html.slice(i, end); i = end; continue; }
    if (c1 === "!" || c1 === "?" || (c1 === "/" && !isAlpha(html[i + 2] ?? ""))) {
      // Bogus comment / doctype ("</>" is dropped by the parser too): up to the first ">".
      const e = html.indexOf(">", i + 1);
      const end = e < 0 ? n : e + 1;
      out += html.slice(i, end); i = end; continue;
    }
    const endTag = c1 === "/";
    const ts = i + (endTag ? 2 : 1);
    if (!isAlpha(html[ts] ?? "")) { out += "<"; i++; continue; } // text "<"
    let j = ts;
    while (j < n && !isWs(html[j]) && html[j] !== "/" && html[j] !== ">") j++;
    const rawName = html.slice(ts, j);
    const tag = rawName.toLowerCase();
    const r = readAttrs(html, j);
    i = r.end;
    if (endTag) {
      // The parser ignores an end tag's attributes; so do we (they are not copied).
      if (!DROP_TAGS.has(tag) && !FOREIGN.has(tag)) out += `</${rawName}${r.closed ? ">" : ""}`;
      continue;
    }
    if (FOREIGN.has(tag)) { if (r.closed) i = foreignEnd(html, i); continue; }
    if (DROP_TAGS.has(tag)) continue;
    let attrs = "";
    for (const { name, value } of r.attrs) {
      const lname = name.toLowerCase();
      if (DROP_ATTRS.has(lname)) continue;
      if (lname === "href") {
        if (LINK_TAGS.has(tag)) { attrs += ` ${HELD_ATTR}${value ? `=${value}` : ""}`; links++; }
        continue;
      }
      attrs += ` ${name}${value ? `=${value}` : ""}`;
    }
    out += `<${rawName}${attrs}${r.closed ? ">" : ""}`;
    if (r.closed && RAW_TEXT.has(tag)) {
      const m = new RegExp(`</${tag}[\\s/>]`, "i").exec(html.slice(i));
      const end = m ? i + m.index : n;
      out += html.slice(i, end); i = end;
    }
  }
  const res = { html: out, links };
  memo = { src: html, out: res };
  return res;
}

/** The minimum of a DOM element / document these helpers use (a real Document satisfies it). */
export interface LinkEl {
  tagName: string;
  getAttribute(n: string): string | null;
  setAttribute(n: string, v: string): void;
  removeAttribute(n: string): void;
}
export interface LinkRoot {
  querySelectorAll(sel: string): ArrayLike<LinkEl>;
  querySelector?(sel: string): { remove(): void } | null;
}

/** Second layer on the parsed frame (and when the gate leaves "ready"): no element keeps an href. */
export function holdLinks(d: LinkRoot): number {
  const els = Array.from(d.querySelectorAll("[href]"));
  for (const a of els) {
    if (LINK_TAGS.has(a.tagName.toLowerCase())) a.setAttribute(HELD_ATTR, a.getAttribute("href") ?? "");
    a.removeAttribute("href");
    a.removeAttribute("target");
  }
  return els.length;
}

/** Links held in a parsed frame (the DOM's own count, any case / element). */
export function countHeld(d: LinkRoot): number {
  return Array.from(d.querySelectorAll(`[${HELD_ATTR}]`)).filter((a) => LINK_TAGS.has(a.tagName.toLowerCase())).length;
}

/**
 * The existing Reader rules: web / mail links get target=_blank (a click becomes a new-window navigation that
 * linknav.rs cancels and forwards as "mail://link"); "#anchor" links keep navigating inside the frame; anything else
 * loses its href so it does nothing.
 */
export function markLinks(d: LinkRoot): void {
  for (const a of Array.from(d.querySelectorAll("a[href], area[href]"))) {
    const kind = linkAction(a.getAttribute("href")).kind;
    if (kind === "web" || kind === "mail") { a.setAttribute("target", "_blank"); a.setAttribute("rel", "noopener noreferrer"); }
    else {
      a.removeAttribute("target"); // an anchor must never become a new-window action (about:srcdoc#x is allowed by the hook)
      if (kind === "ignore") a.removeAttribute("href");
    }
  }
}

/** Checks done: held <a>/<area> get their href back under the `markLinks` rules; the hold style goes. Idempotent. */
export function releaseLinks(d: LinkRoot): number {
  let n = 0;
  for (const a of Array.from(d.querySelectorAll(`[${HELD_ATTR}]`))) {
    const href = a.getAttribute(HELD_ATTR);
    a.removeAttribute(HELD_ATTR);
    if (href === null || !LINK_TAGS.has(a.tagName.toLowerCase())) continue;
    a.setAttribute("href", href);
    n++;
  }
  markLinks(d);
  d.querySelector?.("style[data-mm-hold]")?.remove();
  return n;
}

/**
 * Runs a job at most once per key until the key changes (the Reader's link list: once per message body). A job that
 * was cancelled before it finished may run again; one that finished never does for the same key.
 */
export class OncePerKey {
  private done: string | null = null;
  runs = 0;
  shouldRun(key: string): boolean { return key !== this.done; }
  finish(key: string): void { this.done = key; this.runs++; }
}
