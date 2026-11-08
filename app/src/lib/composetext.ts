// Pure helpers for the composer (no DOM, no Tauri): html <-> text, signatures, quoting, inline images, address tokens.
// Kept free of runtime imports so `node --experimental-strip-types --test` can load it.
import type { Identity, SigDefaults, SigPlacement, Signature } from "./types";
import { attrsHide, attrValue, decodeEntities as decodeEnt } from "./csshide.ts";

export const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

export const decodeEntities = decodeEnt;

const BLOCK = new Set(["div", "p", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "blockquote", "pre", "table", "tr", "hr"]);
const SKIP = new Set(["script", "style", "head", "title"]);

/** An element whose inline style hides it (display:none, visibility:hidden, opacity < 0.1, font-size < 2px, transparent colour; numeric, same rules as the backend). */
export function hidesItself(attrs: string): boolean {
  return attrsHide(attrs) || (attrValue(attrs, "style") ?? "").trim().toLowerCase() === "mm-hidden";
}
const VOID = new Set(["br", "hr", "img", "input", "meta", "link", "wbr"]);

// ---- tables in the plain-text part ----

const TAG_RE_SRC = '<!--[\\s\\S]*?-->|<(\\/?)([a-zA-Z][a-zA-Z0-9]*)((?:[^>"\']|"[^"]*"|\'[^\']*\')*)>';
let tableNest = 0;
const MAX_TEXT_TABLE_NEST = 6;

/** Index just past the `</table>` that closes the table whose open tag ended at `from`. */
function tableEnd(html: string, from: number): number {
  const re = new RegExp(TAG_RE_SRC, "g");
  re.lastIndex = from;
  let depth = 1, m: RegExpExecArray | null;
  while ((m = re.exec(html))) {
    if (m[2]?.toLowerCase() !== "table") continue;
    if (m[1]) { if (--depth === 0) return m.index + m[0].length; } else depth++;
  }
  return html.length;
}

/** Lines for one table: rows as `cell | cell`, a dashed line under a header row, and just the content for a one-cell table (button, callout). */
function tableLines(tableHtml: string): string[] {
  const re = new RegExp(TAG_RE_SRC, "g");
  type RawCell = { head: boolean; attrs: string; inner: string };
  const rows: { cells: RawCell[]; attrs: string }[] = [];
  let depth = 0, row: { cells: RawCell[]; attrs: string } | null = null, cell: { raw: RawCell; from: number } | null = null, m: RegExpExecArray | null;
  let cap: { from: number; attrs: string } | null = null;
  const captions: string[] = [];
  const closeCell = (at: number) => { if (cell && row) { cell.raw.inner = tableHtml.slice(cell.from, at); row.cells.push(cell.raw); } cell = null; };
  const closeRow = (at: number) => { closeCell(at); if (row) rows.push(row); row = null; };
  while ((m = re.exec(tableHtml))) {
    if (!m[2]) continue;
    const closing = !!m[1], tag = m[2].toLowerCase(), attrs = m[3] ?? "";
    if (tag === "table") { if (!closing) depth++; else if (--depth <= 0) break; continue; }
    if (depth !== 1) continue;
    if (tag === "caption") {
      if (!closing) cap = { from: m.index + m[0].length, attrs };
      else if (cap) { if (!hidesItself(cap.attrs)) captions.push(htmlToText(tableHtml.slice(cap.from, m.index)).replace(/\s+/g, " ").trim()); cap = null; }
      continue;
    }
    if (tag === "tr") { closeRow(m.index); if (!closing) row = { cells: [], attrs }; }
    else if (tag === "td" || tag === "th") {
      if (!closing) { closeCell(m.index); if (!row) row = { cells: [], attrs: "" }; cell = { raw: { head: tag === "th", attrs, inner: "" }, from: m.index + m[0].length }; }
      else closeCell(m.index);
    }
  }
  closeRow(tableHtml.length);
  tableNest++;
  try {
    const grid: { text: string[]; head: boolean }[][] = [];
    for (const r of rows) {
      if (hidesItself(r.attrs)) continue;
      const out: { text: string[]; head: boolean }[] = [];
      for (const c of r.cells) {
        if (hidesItself(c.attrs)) continue;
        const text = htmlToText(c.inner).split("\n");
        while (text.length && text[text.length - 1] === "") text.pop();
        out.push({ text, head: c.head });
        const cs = parseInt(/colspan\s*=\s*["']?(\d+)/i.exec(c.attrs)?.[1] ?? "1", 10);
        for (let k = 1; k < Math.min(Number.isFinite(cs) ? cs : 1, 50); k++) out.push({ text: [], head: c.head });
      }
      if (out.length) grid.push(out);
    }
    const flat = grid.flat();
    const lines: string[] = captions.filter(Boolean);
    if (flat.length === 0) return lines;
    if (flat.length === 1) return [...lines, ...flat[0].text];
    grid.forEach((r, i) => {
      const cells = r.map((c) => c.text.join(" ").replace(/\s+/g, " ").trim());
      if (cells.every((c) => c === "")) return;
      lines.push(cells.join(" | ").replace(/[ \t]+$/, ""));
      if (i === 0 && grid.length > 1 && r.every((c) => c.head)) lines.push(cells.map((c) => "-".repeat(Math.max(3, c.length))).join(" | "));
    });
    return lines;
  } finally { tableNest--; }
}

/** Readable plain-text rendering of editor / signature HTML (lists, quotes, links, the `-- ` delimiter kept). */
export function htmlToText(html: string): string {
  const out: string[] = [];
  let cur = "", curDepth = 0, depth = 0, pre = 0, skip = "", skipN = 0;
  const lists: { ordered: boolean; n: number }[] = [];
  const links: { href: string; text: string }[] = [];
  const commit = (force: boolean) => {
    if (cur === "" && !force) return;
    const line = cur === "-- " ? cur : cur.replace(/[ \t]+$/, "");
    out.push(line === "" ? "" : "> ".repeat(curDepth) + line);
    cur = "";
  };
  const blank = () => { commit(false); if (out.length && out[out.length - 1] !== "") out.push(""); };
  const add = (t: string) => {
    if (!t) return;
    if (cur === "") curDepth = depth;
    cur += t;
    if (links.length) links[links.length - 1].text += t;
  };
  const re = /<!--[\s\S]*?-->|<(\/?)([a-zA-Z][a-zA-Z0-9]*)((?:[^>"']|"[^"]*"|'[^']*')*)>|([^<]+|<)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(html))) {
    if (m[4] !== undefined) {
      if (skip) continue;
      let t = m[4];
      if (!pre) { t = t.replace(/[ \t\r\n\f]+/g, " "); if (cur === "") t = t.replace(/^ /, ""); }
      else t = t.replace(/\r\n/g, "\n");
      t = decodeEntities(t);
      if (pre && t.includes("\n")) { const parts = t.split("\n"); parts.forEach((p, i) => { if (i) commit(true); add(p); }); }
      else add(t);
      continue;
    }
    if (!m[2]) continue; // comment
    const closing = !!m[1], tag = m[2].toLowerCase(), attrs = m[3] ?? "";
    if (skip) {
      if (tag === skip) { if (!closing) skipN++; else if (--skipN <= 0) skip = ""; }
      continue;
    }
    if (SKIP.has(tag)) { if (!closing) { skip = tag; skipN = 1; } continue; }
    // Hidden elements (pasted `display:none` spans ...) never reach the plain-text part.
    if (!closing && !VOID.has(tag) && !/\/\s*$/.test(attrs) && hidesItself(attrs)) { skip = tag; skipN = 1; continue; }
    if (!closing && VOID.has(tag) && hidesItself(attrs)) continue;
    if (tag === "table" && !closing && tableNest < MAX_TEXT_TABLE_NEST) {
      const end = tableEnd(html, m.index + m[0].length);
      commit(false);
      for (const l of tableLines(html.slice(m.index, end))) out.push(l === "" ? "" : "> ".repeat(depth) + l);
      blank();
      re.lastIndex = end;
      continue;
    }
    if (tag === "br") { commit(true); continue; }
    // Cells of a table past the nesting cap: no table layout, but the cells must not run together.
    if (tag === "td" || tag === "th") { if (cur !== "" && !cur.endsWith(" ")) add(" "); continue; }
    if (tag === "a") {
      if (!closing) { const h = /href\s*=\s*(?:"([^"]*)"|'([^']*)')/i.exec(attrs); links.push({ href: decodeEntities(h?.[1] ?? h?.[2] ?? ""), text: "" }); }
      else {
        const l = links.pop();
        if (l && /^(https?:|mailto:)/i.test(l.href)) {
          const shown = l.text.trim().replace(/^mailto:/i, "");
          if (shown && shown !== l.href && shown !== l.href.replace(/^mailto:/i, "")) { add(` (${l.href})`); }
        }
      }
      continue;
    }
    if (tag === "pre") { pre = closing ? Math.max(0, pre - 1) : pre + 1; commit(false); continue; }
    if (tag === "ul" || tag === "ol") {
      if (!closing) { commit(false); lists.push({ ordered: tag === "ol", n: 0 }); } else { commit(false); lists.pop(); if (!lists.length) blank(); }
      continue;
    }
    if (tag === "li") {
      commit(false);
      if (!closing) {
        const l = lists[lists.length - 1];
        if (l) l.n++;
        add("  ".repeat(Math.max(0, lists.length - 1)) + (l?.ordered ? `${l.n}. ` : "- "));
      }
      continue;
    }
    if (tag === "blockquote") {
      commit(false);
      if (!closing) depth++; else depth = Math.max(0, depth - 1);
      continue;
    }
    if (tag === "hr") { commit(false); add("----------"); commit(false); continue; }
    if (tag === "p") { if (closing) blank(); else commit(false); continue; }
    if (BLOCK.has(tag) || /^h[1-6]$/.test(tag)) { commit(false); if (closing && /^h[1-6]$/.test(tag)) blank(); }
  }
  commit(false);
  while (out.length && out[out.length - 1] === "") out.pop();
  return out.join("\n");
}

/** Plain text -> editor HTML. Runs of `>` lines become nested blockquotes; blank lines are kept. */
export function textToHtml(text: string): string {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const render = (ls: string[]): string => {
    let h = "", i = 0;
    while (i < ls.length) {
      if (ls[i].startsWith(">")) {
        const block: string[] = [];
        while (i < ls.length && ls[i].startsWith(">")) block.push(ls[i++].replace(/^> ?/, ""));
        h += `<blockquote>${render(block)}</blockquote>`;
      } else {
        const l = ls[i++];
        h += l === "" ? "<div><br></div>" : `<div>${esc(l).replace(/^ +/, (s) => "&nbsp;".repeat(s.length)).replace(/ {2,}/g, (s) => " " + "&nbsp;".repeat(s.length - 1))}</div>`;
      }
    }
    return h;
  };
  return render(lines);
}

/** Plain text -> editor HTML with the quoted original wrapped in a marked block (so signature placement still works). */
export function plainToRichBody(text: string): string {
  const q = findQuoteStart(text);
  if (q < 0) return textToHtml(text);
  return textToHtml(text.slice(0, q).replace(/\s+$/, "") + (text.slice(0, q).trim() ? "\n" : "")) + QUOTE_OPEN + textToHtml(text.slice(q).replace(/\s+$/, "")) + "</div>";
}

// ---- plain-text signatures ----

export const plainSigBlock = (sigText: string) => "-- \n" + sigText.replace(/\r\n/g, "\n").replace(/\s+$/, "");

/** Index of the quoted original ("On ... wrote:" / forwarded-message banner) in plain text, or -1. */
export function findQuoteStart(text: string): number {
  const m = /(^|\n)(On [^\n]{1,200} wrote:\n(?:>|\n|$)|-{10} Forwarded message -{10}\n)/.exec(text);
  return m ? m.index + m[1].length : -1;
}

/** Add `sigText` (with the `-- ` delimiter) above or below the quoted original, or at the end. */
export function insertPlainSig(text: string, sigText: string, placement: SigPlacement): string {
  if (!sigText.trim()) return text;
  const block = plainSigBlock(sigText);
  const q = findQuoteStart(text);
  if (q < 0) {
    const lead = text.replace(/\s+$/, "");
    return (lead ? lead + "\n\n" : "\n\n") + block + "\n";
  }
  const lead = text.slice(0, q), quote = text.slice(q).replace(/\s+$/, "");
  if (placement === "above") return (lead.replace(/\s+$/, "") ? lead.replace(/\s+$/, "") + "\n\n" : "\n\n") + block + "\n\n" + quote + "\n";
  return lead + quote + "\n\n" + block + "\n";
}

/** Remove a previously inserted signature block (the last exact match); text without it is returned unchanged. */
export function removePlainSig(text: string, sigText: string): string {
  if (!sigText.trim()) return text;
  const block = plainSigBlock(sigText);
  const i = text.lastIndexOf(block);
  if (i < 0) return text;
  const b = text.slice(0, i).replace(/\s+$/, ""), a = text.slice(i + block.length).replace(/^\s+/, "");
  if (!a) return b ? b + "\n" : "";
  return (b ? b : "") + "\n\n" + a;
}

// ---- rich signatures / quotes (string level, so they are testable) ----

const SIG_OPEN = '<div data-mm-sig="';
export const QUOTE_OPEN = '<div data-mm-quote="1">';

/** Signature as an editor block. Plain-text signatures get the `-- ` delimiter line. */
export function sigHtmlBlock(sig: Signature): string {
  const inner = sig.rich && sig.html.trim() ? sig.html : "<div>-- </div>" + sig.text.replace(/\r\n/g, "\n").replace(/\s+$/, "").split("\n").map((l) => (l ? `<div>${esc(l)}</div>` : "<div><br></div>")).join("");
  return `${SIG_OPEN}${esc(sig.id)}"><div><br></div>${inner}</div>`;
}

export function insertRichSig(html: string, block: string, placement: SigPlacement): string {
  const q = html.indexOf(QUOTE_OPEN);
  return q >= 0 && placement === "above" ? html.slice(0, q) + block + html.slice(q) : html + block;
}

/** Remove the signature block (balanced `<div>` scan, so nested divs in the signature are fine). */
export function removeRichSig(html: string): string {
  const s = html.indexOf(SIG_OPEN);
  if (s < 0) return html;
  const re = /<(\/?)div\b[^>]*>/gi;
  re.lastIndex = s;
  let depth = 0, m: RegExpExecArray | null;
  while ((m = re.exec(html))) {
    depth += m[1] ? -1 : 1;
    if (depth === 0) return html.slice(0, s) + html.slice(m.index + m[0].length);
  }
  return html.slice(0, s);
}

/**
 * HTML for the editor that did not just come from the editor itself (restored draft, window handoff, reply/forward seed):
 * run it through the backend compose sanitiser first. The sanitiser drops `data-mm-*` attributes, so the signature and
 * quote markers are carried through it on `lang` (kept by the sanitiser) and put back afterwards. If sanitising fails or
 * returns something unusable, fall back to the plain-text conversion: unsanitised HTML is never returned.
 */
export async function safeInitialHtml(html: string, sanitize: (h: string) => Promise<string>): Promise<string> {
  if (!html) return "";
  try {
    const guarded = html
      .replace(/<div data-mm-sig="([^"<>]*)">/g, '<div lang="x-mm-sig:$1">')
      .replace(/<div data-mm-quote="1">/g, '<div lang="x-mm-quote">');
    const out = await sanitize(guarded);
    if (typeof out !== "string") throw new Error("bad sanitiser result");
    return out
      .replace(/<div lang="x-mm-sig:([^"<>]*)">/g, '<div data-mm-sig="$1">')
      .replace(/<div lang="x-mm-quote">/g, QUOTE_OPEN);
  } catch {
    return plainToRichBody(htmlToText(html));
  }
}

export const hasRichSig = (html: string) => html.includes(SIG_OPEN);

/** Turn pasted/typed signature text into HTML for a rich signature ("import from existing text"). */
export function importSignatureText(raw: string): { text: string; html: string } {
  const text = raw.replace(/\r\n/g, "\n").replace(/^\s*-- ?\n/, "").replace(/\s+$/, "").replace(/^\n+/, "");
  return { text, html: textToHtml(text) };
}

const stripCid = (h: string) => h.replace(/<img\b[^>]*\bsrc\s*=\s*["']?cid:[^>]*>/gi, "");

/** "On <when>, <who> wrote:" + the original (HTML preferred) as a quote block. */
export function replyQuoteHtml(attribution: string, origHtml: string | null, origText: string | null): string {
  const body = origHtml && origHtml.trim() ? stripCid(origHtml) : textToHtml((origText ?? "").replace(/\r\n/g, "\n").trimEnd());
  return `${QUOTE_OPEN}<div>${esc(attribution)}</div><blockquote>${body}</blockquote></div>`;
}

/** The forwarded-message banner, header lines and original body. */
export function forwardQuoteHtml(head: [string, string][], origHtml: string | null, origText: string | null): string {
  const body = origHtml && origHtml.trim() ? stripCid(origHtml) : textToHtml((origText ?? "").replace(/\r\n/g, "\n").trimEnd());
  const lines = head.filter(([, v]) => v).map(([k, v]) => `<div><b>${esc(k)}:</b> ${esc(v)}</div>`).join("");
  return `${QUOTE_OPEN}<div>---------- Forwarded message ----------</div>${lines}<div><br></div>${body}</div>`;
}

// ---- signature selection ----

export const sigsForAccount = (sigs: Signature[], account: string | null) => sigs.filter((s) => !s.accounts.length || (account != null && s.accounts.includes(account)));

/** Which signature a new message starts with: the chosen identity's, else the account default for new / reply mail. */
export function pickSignature(sigs: Signature[], defaults: SigDefaults[], identities: Identity[], account: string | null, kind: "new" | "reply", from?: string | null): Signature | null {
  if (!account) return null;
  const avail = sigsForAccount(sigs, account);
  const ident = from ? identities.find((i) => i.account === account && i.address.toLowerCase() === from.toLowerCase()) : undefined;
  const byIdent = ident?.signatureId ? avail.find((s) => s.id === ident.signatureId) : undefined;
  if (byIdent) return byIdent;
  const d = defaults.find((x) => x.account === account);
  const id = kind === "reply" ? d?.replyId : d?.newId;
  return (id && avail.find((s) => s.id === id)) || null;
}

// ---- inline images ----

export interface InlineImage { name: string; mime: string; dataBase64: string; size: number }

/** Pull `data:image/...;base64,` pictures out of the HTML so they can travel as ordinary attachments. */
export function extractInlineImages(html: string): { html: string; images: InlineImage[] } {
  const images: InlineImage[] = [];
  const seen = new Set<string>();
  const out = html.replace(/<img\b[^>]*?\bsrc\s*=\s*(["'])data:image\/(png|jpeg|gif|webp);base64,([A-Za-z0-9+/=]+)\1[^>]*>/gi, (_m, _q, ext: string, b64: string) => {
    if (!seen.has(b64)) {
      seen.add(b64);
      const e = ext.toLowerCase() === "jpeg" ? "jpg" : ext.toLowerCase();
      images.push({ name: `image-${images.length + 1}.${e}`, mime: `image/${ext.toLowerCase()}`, dataBase64: b64, size: Math.floor((b64.length * 3) / 4) - (b64.endsWith("==") ? 2 : b64.endsWith("=") ? 1 : 0) });
    }
    return "";
  });
  return { html: out, images };
}

/** Drafts are capped at 1 MB by the backend: drop picture data from the draft copy when it would not fit. */
export function htmlForDraft(html: string, limit = 700_000): string {
  if (html.length <= limit) return html;
  return html.replace(/(<img\b[^>]*?\bsrc\s*=\s*)(["'])data:[^"']*\2/gi, '$1"" data-mm-dropped="1"');
}

// ---- recipient fields ----

/** The partial address being typed at the end of a recipients field. */
export const lastToken = (v: string) => v.slice(Math.max(v.lastIndexOf(","), v.lastIndexOf(";"), v.lastIndexOf("\n")) + 1).trim();

export function replaceLastToken(v: string, mailbox: string): string {
  const i = Math.max(v.lastIndexOf(","), v.lastIndexOf(";"), v.lastIndexOf("\n"));
  return (i < 0 ? "" : v.slice(0, i + 1) + " ") + mailbox + ", ";
}

/** `Name <addr>` (name quoted when it holds specials) or the bare address. */
export function formatMailbox(name: string | null | undefined, address: string): string {
  const n = (name ?? "").replace(/["<>\\\r\n]/g, "").trim();
  if (!n) return address;
  return /[,;:()@.]/.test(n) ? `"${n}" <${address}>` : `${n} <${address}>`;
}
