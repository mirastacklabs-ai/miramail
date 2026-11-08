// Safe, minimal Markdown for model output. Produces a plain AST; the renderer builds Svelte nodes from it.
// No raw HTML is ever emitted or interpreted: model text is data. Images are never loaded.

export type Inline =
  | { t: "text"; v: string }
  | { t: "strong"; c: Inline[] }
  | { t: "em"; c: Inline[] }
  | { t: "code"; v: string }
  | { t: "link"; href: string; c: Inline[] };
export type Block =
  | { t: "p"; c: Inline[] }
  | { t: "h"; c: Inline[] }
  | { t: "code"; lang: string; v: string; closed?: boolean }
  | { t: "list"; ordered: boolean; items: { depth: number; c: Inline[] }[] }
  | { t: "table"; head: Inline[][]; rows: Inline[][][] }
  | { t: "quote"; c: Block[] }
  | { t: "hr" };

export const MAX_INPUT = 200_000;

/** Remove bidi controls, zero-width / invisible formatting, Unicode tag characters and C0/C1 controls (keeps \n and \t). */
export function stripUnsafe(s: string): string {
  return s
    .replace(/\r\n?/g, "\n")
    .replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F]/g, "")
    .replace(/[؜᠎​-‏‪-‮⁠-⁤⁦-⁯﻿￹-￻]/g, "")
    .replace(/[\u{E0000}-\u{E007F}]/gu, "");
}

/** Normalised URL string when the scheme is http, https or mailto; otherwise null. */
export function safeUrl(raw: string): string | null {
  const s = stripUnsafe(raw).trim();
  if (!s || /\s/.test(s)) return null;
  try {
    const u = new URL(s);
    if (u.protocol === "mailto:") return u.pathname ? u.href : null;
    if ((u.protocol === "http:" || u.protocol === "https:") && u.hostname) return u.href;
  } catch { /* not a URL */ }
  return null;
}

const MAX_DEPTH = 4;

function matchBracket(s: string, i: number, open: string, close: string): number {
  let d = 0;
  for (let j = i; j < s.length; j++) {
    if (s[j] === "\\") { j++; continue; }
    if (s[j] === open) d++;
    else if (s[j] === close && --d === 0) return j;
  }
  return -1;
}
/** `[label](url "title")` starting at s[i] === "[". */
function matchLink(s: string, i: number): { label: string; url: string; end: number } | null {
  const j = matchBracket(s, i, "[", "]");
  if (j < 0 || s[j + 1] !== "(") return null;
  const k = matchBracket(s, j + 1, "(", ")");
  if (k < 0) return null;
  const url = s.slice(j + 2, k).trim().split(/\s+/)[0] ?? "";
  return { label: s.slice(i + 1, j), url: url.replace(/^<|>$/g, ""), end: k + 1 };
}

const isWord = (c: string | undefined) => !!c && /[\p{L}\p{N}]/u.test(c);
const balanced = (u: string) => (u.match(/\(/g)?.length ?? 0) >= (u.match(/\)/g)?.length ?? 0);

export function parseInline(s: string, depth = 0, allowLinks = true): Inline[] {
  if (depth > MAX_DEPTH) return s ? [{ t: "text", v: s }] : [];
  const out: Inline[] = [];
  let buf = "";
  const flush = () => { if (buf) { out.push({ t: "text", v: buf }); buf = ""; } };
  let i = 0;
  while (i < s.length) {
    const ch = s[i];
    if (ch === "\\" && i + 1 < s.length && /[\\`*_{}[\]()#+\-.!|<>~]/.test(s[i + 1])) { buf += s[i + 1]; i += 2; continue; }
    if (ch === "`") {
      const j = s.indexOf("`", i + 1);
      if (j > i + 1) { flush(); out.push({ t: "code", v: s.slice(i + 1, j) }); i = j + 1; continue; }
    }
    if (ch === "!" && s[i + 1] === "[") {
      const m = matchLink(s, i + 1);
      if (m) { flush(); const alt = m.label.replace(/[[\]]/g, "").trim(); out.push({ t: "text", v: `[image: ${alt || "image"}, not loaded]` }); i = m.end; continue; }
    }
    if (ch === "[") {
      const m = matchLink(s, i);
      if (m) {
        flush();
        const href = allowLinks ? safeUrl(m.url) : null;
        const kids = parseInline(m.label, depth + 1, false);
        if (href) out.push({ t: "link", href, c: kids.length ? kids : [{ t: "text", v: href }] });
        else { out.push(...kids); out.push({ t: "text", v: " [link blocked]" }); }
        i = m.end; continue;
      }
    }
    if (ch === "<" && allowLinks) {
      const m = /^<((?:https?:\/\/|mailto:)[^\s<>]+)>/.exec(s.slice(i));
      const href = m ? safeUrl(m[1]) : null;
      if (m && href) { flush(); out.push({ t: "link", href, c: [{ t: "text", v: m[1] }] }); i += m[0].length; continue; }
    }
    if ((ch === "*" || ch === "_") && s[i + 1] === ch) {
      const j = s.indexOf(ch + ch, i + 2);
      if (j > i + 2 && !(ch === "_" && isWord(s[i - 1]))) { flush(); out.push({ t: "strong", c: parseInline(s.slice(i + 2, j), depth + 1, allowLinks) }); i = j + 2; continue; }
    }
    if (ch === "*" || ch === "_") {
      const j = s.indexOf(ch, i + 1);
      if (j > i + 1 && s[i + 1] !== " " && s[j - 1] !== " " && !(ch === "_" && (isWord(s[i - 1]) || isWord(s[j + 1])))) {
        flush(); out.push({ t: "em", c: parseInline(s.slice(i + 1, j), depth + 1, allowLinks) }); i = j + 1; continue;
      }
    }
    if (allowLinks && (ch === "h" || ch === "H") && !isWord(s[i - 1])) {
      const m = /^https?:\/\/[^\s<>]+/i.exec(s.slice(i));
      if (m) {
        let raw = m[0].replace(/[.,;:!?'"*_]+$/, "");
        while (raw.endsWith(")") && !balanced(raw)) raw = raw.slice(0, -1);
        const href = safeUrl(raw);
        if (href) { flush(); out.push({ t: "link", href, c: [{ t: "text", v: raw }] }); i += raw.length; continue; }
      }
    }
    buf += ch; i++;
  }
  flush();
  return out;
}

const FENCE = /^\s{0,3}(```+|~~~+)\s*([A-Za-z0-9_+.-]*)[^`]*$/;
const HEAD = /^\s{0,3}#{1,6}\s+(.*?)\s*#*\s*$/;
const LIST = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const HR = /^\s{0,3}([-*_])(\s*\1){2,}\s*$/;
const SEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

function splitRow(line: string): string[] {
  let t = line.trim();
  if (t.startsWith("|")) t = t.slice(1);
  if (t.endsWith("|") && !t.endsWith("\\|")) t = t.slice(0, -1);
  const cells: string[] = []; let cur = "";
  for (let i = 0; i < t.length; i++) {
    if (t[i] === "\\" && t[i + 1] === "|") { cur += "|"; i++; }
    else if (t[i] === "|") { cells.push(cur.trim()); cur = ""; }
    else cur += t[i];
  }
  cells.push(cur.trim());
  return cells;
}
const startsBlock = (l: string) => FENCE.test(l) || HEAD.test(l) || LIST.test(l) || HR.test(l) || /^\s{0,3}>/.test(l);

export function parseBlocks(src: string, depth = 0): Block[] {
  const lines = src.split("\n");
  const out: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) { i++; continue; }
    const f = FENCE.exec(line);
    if (f) {
      const mark = f[1][0], len = f[1].length;
      const close = new RegExp("^\\s{0,3}\\" + mark + "{" + len + ",}\\s*$");
      const body: string[] = [];
      i++;
      while (i < lines.length && !close.test(lines[i])) body.push(lines[i++]);
      const closed = i < lines.length; // false = EOF reached (still streaming)
      i++; // closing fence (or EOF while streaming)
      out.push({ t: "code", lang: f[2], v: body.join("\n"), closed });
      continue;
    }
    if (HR.test(line) && !LIST.test(line)) { out.push({ t: "hr" }); i++; continue; }
    const h = HEAD.exec(line);
    if (h) { out.push({ t: "h", c: parseInline(h[1]) }); i++; continue; }
    if (/^\s{0,3}>/.test(line) && depth < 3) {
      const q: string[] = [];
      while (i < lines.length && /^\s{0,3}>/.test(lines[i])) q.push(lines[i++].replace(/^\s{0,3}>\s?/, ""));
      out.push({ t: "quote", c: parseBlocks(q.join("\n"), depth + 1) });
      continue;
    }
    if (line.includes("|") && i + 1 < lines.length && lines[i + 1].includes("-") && SEP.test(lines[i + 1])) {
      const head = splitRow(line).map((c) => parseInline(c));
      i += 2;
      const rows: Inline[][][] = [];
      while (i < lines.length && lines[i].trim() && lines[i].includes("|")) rows.push(splitRow(lines[i++]).map((c) => parseInline(c)));
      out.push({ t: "table", head, rows });
      continue;
    }
    const li = LIST.exec(line);
    if (li) {
      const ordered = /\d/.test(li[2][0]);
      const items: { depth: number; c: Inline[] }[] = [];
      while (i < lines.length) {
        const m = LIST.exec(lines[i]);
        if (m) { items.push({ depth: Math.min(2, Math.floor(m[1].replace(/\t/g, "  ").length / 2)), c: parseInline(m[3]) }); i++; }
        else if (lines[i].trim() && /^\s+\S/.test(lines[i]) && items.length) { const last = items[items.length - 1]; last.c = [...last.c, { t: "text", v: " " }, ...parseInline(lines[i].trim())]; i++; }
        else break;
      }
      out.push({ t: "list", ordered, items });
      continue;
    }
    const para: string[] = [line];
    i++;
    while (i < lines.length && lines[i].trim() && !startsBlock(lines[i])) para.push(lines[i++]);
    out.push({ t: "p", c: parseInline(para.join("\n")) });
  }
  return out;
}

export function parseMarkdown(text: string): Block[] {
  return parseBlocks(stripUnsafe(text.length > MAX_INPUT ? text.slice(0, MAX_INPUT) : text));
}
/** Sanitised plain text, used by the copy button. */
export const plainText = (text: string) => stripUnsafe(text);
