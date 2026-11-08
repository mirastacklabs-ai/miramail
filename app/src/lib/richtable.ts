// Pure table model for the rich composer: parse the editor's table HTML into a model, edit it (rows, columns, header,
// zebra, border, column widths), serialise it back as email-safe markup, and turn clipboard content (Excel, Google
// Sheets, web tables, tab-separated text) into a clean table. No DOM, no Tauri, no runtime imports, so
// `node --experimental-strip-types --test` can load it.
//
// Markup rules (what Outlook and web clients all render): semantic <table>/<thead>/<tbody>/<th>/<td>, inline styles
// only, attribute fallbacks (width, cellpadding, cellspacing, border, bgcolor, align, colspan, rowspan), no classes,
// no <style>, no external resources. The backend sanitiser (mailcore::compose_html) lets exactly these through.

import { attrsHide, attrValue, decodeEntities, tokens } from "./csshide.ts";

export type Align = "left" | "center" | "right";
export interface Cell { html: string; align?: Align; colspan?: number; rowspan?: number }
export interface TableModel { rows: Cell[][]; header: boolean; zebra: boolean; border: boolean; /** percent per grid column, null = auto */ widths: (number | null)[] }

export const MAX_INSERT_ROWS = 30, MAX_INSERT_COLS = 10; // the "Insert table" chooser
export const MAX_ROWS = 200, MAX_COLS = 30; // editing and pasted tables
export const MAX_SPAN = 50;

export const BORDER_COLOR = "#BBBBBB", HEADER_BG = "#EEF0F3", ZEBRA_BG = "#F5F7FA", CELL_TEXT = "#1F2328";

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const decode = decodeEntities;
const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, Math.trunc(n)));
export const clampDims = (rows: number, cols: number) => ({ rows: clamp(Number.isFinite(rows) ? rows : 2, 1, MAX_INSERT_ROWS), cols: clamp(Number.isFinite(cols) ? cols : 2, 1, MAX_INSERT_COLS) });

// ---- serialising ----

const span = (c: Cell) => ({ cs: clamp(c.colspan ?? 1, 1, MAX_SPAN), rs: clamp(c.rowspan ?? 1, 1, MAX_SPAN) });

/** The model as email-safe HTML (no trailing paragraph; the editor adds one). */
export function serializeTable(t: TableModel): string {
  const pad = "6px 8px", bd = t.border ? `border:1px solid ${BORDER_COLOR};` : "";
  const hdr = t.header ? 1 : 0;
  const body = t.rows.map((row, ri) => {
    const isHead = ri < hdr, zebra = !isHead && t.zebra && (ri - hdr) % 2 === 1;
    const bg = isHead ? HEADER_BG : zebra ? ZEBRA_BG : "";
    const cells = row.map((c, ci) => {
      const { cs, rs } = span(c);
      const w = ri === 0 ? t.widths[ci] : null; // widths sit on the first row (Outlook and the web both use it)
      const attrs = [
        c.align ? ` align="${c.align}"` : isHead ? ` align="left"` : "",
        w ? ` width="${w}%"` : "",
        cs > 1 ? ` colspan="${cs}"` : "", rs > 1 ? ` rowspan="${rs}"` : "",
        bg ? ` bgcolor="${bg}"` : "",
      ].join("");
      const st = [`padding:${pad}`, bd.slice(0, -1) || null, "vertical-align:top", bg ? `background-color:${bg}` : null, bg ? `color:${CELL_TEXT}` : null, isHead ? "font-weight:bold" : null, c.align ? `text-align:${c.align}` : isHead ? "text-align:left" : null].filter(Boolean).join(";");
      const tag = isHead ? "th" : "td";
      return `<${tag}${attrs} style="${st}">${c.html.trim() === "" ? "<br>" : c.html}</${tag}>`;
    }).join("");
    return `<tr>${cells}</tr>`;
  });
  const head = hdr ? `<thead>${body.slice(0, hdr).join("")}</thead>` : "";
  const rest = body.slice(hdr).join("");
  return `<table width="100%" cellpadding="6" cellspacing="0" border="${t.border ? 1 : 0}" style="border-collapse:collapse;width:100%">${head}${rest ? `<tbody>${rest}</tbody>` : ""}</table>`;
}

export function emptyCell(): Cell { return { html: "" }; }

export function newTable(rows: number, cols: number, opts: Partial<Pick<TableModel, "header" | "zebra" | "border">> = {}): TableModel {
  const d = clampDims(rows, cols);
  return { rows: Array.from({ length: d.rows }, () => Array.from({ length: d.cols }, emptyCell)), header: false, zebra: false, border: true, widths: Array(d.cols).fill(null), ...opts };
}

// ---- parsing ----

interface RawCell { head: boolean; attrs: string; inner: string; hidden: boolean }
interface RawRow { cells: RawCell[]; hidden: boolean }
interface RawTable { attrs: string; rows: RawRow[]; thead: boolean; hidden: boolean }

/** Value of an attribute in a tag's attribute text, entity-decoded; null when absent. */
export function attrOf(attrs: string, name: string): string | null { return attrValue(attrs, name); }
export function styleOf(attrs: string, prop: string): string | null {
  const st = attrOf(attrs, "style");
  if (!st) return null;
  for (const d of st.split(";")) {
    const i = d.indexOf(":");
    if (i > 0 && d.slice(0, i).trim().toLowerCase() === prop) return d.slice(i + 1).trim().toLowerCase();
  }
  return null;
}

/**
 * First `<table>` of `html` with its rows and cells (cell content kept verbatim; nested tables stay inside their cell).
 * With `paste`, rows / cells / sections / the table itself that their own attributes hide are flagged `hidden`.
 */
function parseRaw(html: string, paste = false): { table: RawTable; start: number; end: number } | null {
  let depth = 0, start = -1, table: RawTable | null = null;
  let row: RawRow | null = null, cell: { raw: RawCell; from: number } | null = null, inHead = false, sectionHidden = false;
  const closeCell = (at: number) => { if (cell && row) { cell.raw.inner = html.slice(cell.from, at); row.cells.push(cell.raw); } cell = null; };
  const closeRow = (at: number) => { closeCell(at); if (row && table) table.rows.push(row); row = null; };
  for (const m of tokens(html)) {
    if (m.text) continue;
    const { closing, tag, attrs } = m;
    if (tag === "table") {
      if (!closing) {
        if (depth === 0) { start = m.index; table = { attrs, rows: [], thead: false, hidden: paste && attrsHide(attrs, true) }; }
        depth++;
      } else if (depth > 0 && --depth === 0 && table) {
        closeRow(m.index);
        return { table, start, end: m.end };
      }
      continue;
    }
    if (depth !== 1 || !table) continue; // inside a nested table: part of the cell content
    if (tag === "thead" || tag === "tbody" || tag === "tfoot") {
      closeRow(m.index);
      if (tag === "thead") { inHead = !closing; if (!closing) table.thead = true; }
      sectionHidden = !closing && paste && attrsHide(attrs, true);
    } else if (tag === "tr") { closeRow(m.index); if (!closing) row = { cells: [], hidden: sectionHidden || (paste && attrsHide(attrs, true)) }; }
    else if (tag === "td" || tag === "th") {
      if (!closing) {
        closeCell(m.index);
        if (!row) row = { cells: [], hidden: sectionHidden };
        cell = { raw: { head: tag === "th" || inHead, attrs, inner: "", hidden: paste && attrsHide(attrs, true) }, from: m.end };
      } else closeCell(m.index);
    }
  }
  return null;
}

const alignOf = (attrs: string): Align | undefined => {
  const a = (styleOf(attrs, "text-align") ?? attrOf(attrs, "align") ?? "").trim().toLowerCase();
  return a === "left" || a === "center" || a === "right" ? a : undefined;
};
const spanOf = (attrs: string, name: string) => { const n = parseInt(attrOf(attrs, name) ?? "1", 10); return Number.isFinite(n) && n > 1 ? Math.min(n, MAX_SPAN) : undefined; };

function cellOf(r: RawCell, html: string): Cell {
  const c: Cell = { html };
  const al = alignOf(r.attrs), cs = spanOf(r.attrs, "colspan"), rs = spanOf(r.attrs, "rowspan");
  if (al) c.align = al;
  if (cs) c.colspan = cs;
  if (rs) c.rowspan = rs;
  return c;
}

/** Number of grid columns (colspans counted, cells hidden under a rowspan from above included). */
export function gridCols(t: TableModel): number { return layout(t).cols; }

export interface Placed { r: number; i: number; c: number; cell: Cell }
/** Where every cell sits on the grid, taking rowspans from earlier rows into account. */
export function layout(t: TableModel): { cells: Placed[]; cols: number } {
  const taken: Set<number>[] = t.rows.map(() => new Set());
  const cells: Placed[] = [];
  let cols = 0;
  t.rows.forEach((row, r) => {
    let c = 0;
    row.forEach((cell, i) => {
      while (taken[r].has(c)) c++;
      const sp = span(cell), cs = Math.min(sp.cs, MAX_COLS), rs = sp.rs;
      cells.push({ r, i, c, cell });
      for (let dr = 0; dr < rs && r + dr < t.rows.length; dr++) for (let dc = 0; dc < cs; dc++) taken[r + dr].add(c + dc);
      c += cs;
      cols = Math.max(cols, c);
    });
    for (const x of taken[r]) cols = Math.max(cols, x + 1);
  });
  return { cells, cols };
}

/** Most cells an editable table may have (the editing caps: MAX_ROWS x MAX_COLS) and the budget for sum(rowspan x colspan). */
export const MAX_EDIT_CELLS = MAX_ROWS * MAX_COLS, MAX_SPAN_BUDGET = 20_000;
export const TOO_LARGE_NOTE = "This table is too large to edit here.";

/** True when a table is beyond what the table tools handle (rows, cells, or the merged-cell area), checked before any grid is built. */
export function tableTooLarge(t: TableModel): boolean {
  if (t.rows.length > MAX_ROWS) return true;
  let cells = 0, area = 0;
  for (const row of t.rows) {
    cells += row.length;
    if (cells > MAX_EDIT_CELLS) return true;
    for (const c of row) { const { cs, rs } = span(c); area += Math.min(cs, MAX_COLS) * rs; }
    if (area > MAX_SPAN_BUDGET) return true;
  }
  return false;
}

/** Index of `tr` among `all` (every <tr> under a table in document order, nested tables' rows included), counting only rows that belong to `table`. */
export function ownRowIndex<T>(all: readonly T[], ownerOf: (tr: T) => unknown, table: unknown, tr: T): number {
  let n = 0;
  for (const x of all) { if (ownerOf(x) !== table) continue; if (x === tr) return n; n++; }
  return -1;
}

/** Parse the first table in `html` (the editor's own markup, or anything of the same shape). */
export function parseTable(html: string): TableModel | null {
  const p = parseRaw(html);
  if (!p || !p.table.rows.length) return null;
  const { table } = p;
  const raw = table.rows.map((r) => r.cells);
  const rows = raw.map((r) => r.map((c) => cellOf(c, c.inner.trim() === "<br>" ? "" : c.inner)));
  const t: TableModel = { rows, header: table.thead || raw[0].every((c) => c.head) && raw[0].length > 0, zebra: false, border: false, widths: [] };
  const b = parseInt(attrOf(table.attrs, "border") ?? "0", 10);
  t.border = Number.isFinite(b) && b > 0;
  const hdr = t.header ? 1 : 0;
  t.zebra = raw.slice(hdr).some((r, i) => i % 2 === 1 && r.some((c) => (attrOf(c.attrs, "bgcolor") ?? "").toLowerCase() === ZEBRA_BG.toLowerCase()));
  if (tableTooLarge(t)) return t; // no grid is built for a table the tools will refuse anyway
  const { cols, cells } = layout(t);
  t.widths = Array(cols).fill(null);
  for (const p2 of cells) {
    if (p2.r !== 0) continue;
    const w = /^(\d{1,3})%$/.exec((attrOf(raw[0][p2.i].attrs, "width") ?? "").trim());
    if (w && (p2.cell.colspan ?? 1) === 1) t.widths[p2.c] = clamp(+w[1], 1, 100);
  }
  return t;
}

// ---- editing ----

export type TableOp =
  | { type: "rowAbove" | "rowBelow" | "delRow" | "colLeft" | "colRight" | "delCol" | "header" | "zebra" | "border" }
  | { type: "width"; value: number | null };

export interface OpResult { html: string; r: number; i: number; note?: string }

const copy = (t: TableModel): TableModel => ({ ...t, rows: t.rows.map((r) => r.map((c) => ({ ...c }))), widths: [...t.widths] });

function addRowAt(t: TableModel, at: number): boolean {
  if (t.rows.length >= MAX_ROWS) return false;
  const hdr = t.header ? 1 : 0;
  const pos = clamp(Math.max(at, hdr), 0, t.rows.length);
  const { cells, cols } = layout(t);
  const covered = new Set<number>();
  if (pos > 0 && pos < t.rows.length) {
    for (const p of cells) {
      const { cs, rs } = span(p.cell);
      if (p.r < pos && p.r + rs > pos) { p.cell.rowspan = rs + 1; for (let k = 0; k < cs; k++) covered.add(p.c + k); }
    }
  }
  const row: Cell[] = [];
  for (let c = 0; c < cols; c++) if (!covered.has(c)) row.push(emptyCell());
  t.rows.splice(pos, 0, row);
  return true;
}

function delRowAt(t: TableModel, ri: number): boolean {
  if (t.rows.length <= 1 || ri < 0 || ri >= t.rows.length) return false;
  const { cells } = layout(t);
  for (const p of cells) {
    const { rs } = span(p.cell);
    if (p.r < ri && p.r + rs > ri) p.cell.rowspan = rs - 1 || undefined;
  }
  const moved = cells.filter((p) => p.r === ri && span(p.cell).rs > 1 && ri + 1 < t.rows.length);
  const next = t.rows[ri + 1];
  if (next) {
    const nextCols = new Map(cells.filter((p) => p.r === ri + 1).map((p) => [p.i, p.c]));
    for (const p of moved.reverse()) {
      const spare = { ...p.cell, rowspan: span(p.cell).rs - 1 > 1 ? span(p.cell).rs - 1 : undefined };
      if (!spare.rowspan) delete spare.rowspan;
      let at = next.findIndex((_, k) => (nextCols.get(k) ?? 0) >= p.c);
      if (at < 0) at = next.length;
      next.splice(at, 0, spare);
    }
  }
  t.rows.splice(ri, 1);
  if (ri === 0 && t.header) t.header = false;
  return true;
}

function addColAt(t: TableModel, at: number): boolean {
  const { cells, cols } = layout(t);
  if (cols >= MAX_COLS) return false;
  const pos = clamp(at, 0, cols);
  t.rows.forEach((row, r) => {
    const here = cells.filter((p) => p.r === r);
    // A cell (from this row or via rowspan from above) that straddles the new column just gets wider, once, in its own row.
    const straddle = cells.find((p) => p.c < pos && p.c + span(p.cell).cs > pos && p.r <= r && p.r + span(p.cell).rs > r);
    if (straddle) { if (straddle.r === r) straddle.cell.colspan = span(straddle.cell).cs + 1; return; }
    const before = here.filter((p) => p.c < pos).length;
    row.splice(before, 0, emptyCell());
  });
  t.widths.splice(pos, 0, null);
  return true;
}

function delColAt(t: TableModel, ci: number): boolean {
  const { cells, cols } = layout(t);
  if (cols <= 1 || ci < 0 || ci >= cols) return false;
  const drop = new Set<Cell>();
  for (const p of cells) {
    const { cs } = span(p.cell);
    if (p.c <= ci && ci < p.c + cs) { if (cs > 1) p.cell.colspan = cs - 1 || undefined; else drop.add(p.cell); }
  }
  t.rows = t.rows.map((row) => row.filter((c) => !drop.has(c)));
  t.widths.splice(ci, 1);
  return true;
}

/** Apply `op` to the table in `html` with the caret in row `r`, cell index `i`; returns the new markup and where the caret goes. */
export function applyTableOp(html: string, op: TableOp, r: number, i: number): OpResult | null {
  const model = parseTable(html);
  if (!model) return null;
  if (tableTooLarge(model)) return { html, r, i, note: TOO_LARGE_NOTE };
  const t = copy(model);
  const at = layout(t).cells.find((p) => p.r === r && p.i === i);
  const col = at?.c ?? 0;
  let ok = true, nr = r, ni = i, note: string | undefined;
  switch (op.type) {
    case "rowAbove": ok = addRowAt(t, r); nr = Math.max(r, t.header ? 1 : 0); break;
    case "rowBelow": ok = addRowAt(t, r + span(t.rows[r]?.[i] ?? emptyCell()).rs); nr = r + span(t.rows[r]?.[i] ?? emptyCell()).rs; break;
    case "delRow": ok = delRowAt(t, r); nr = Math.min(r, t.rows.length - 1); break;
    case "colLeft": ok = addColAt(t, col); break;
    case "colRight": ok = addColAt(t, col + (at ? span(at.cell).cs : 1)); ni = i + 1; break;
    case "delCol": ok = delColAt(t, col); ni = Math.max(0, Math.min(i, (t.rows[nr]?.length ?? 1) - 1)); break;
    case "header": t.header = !t.header; break;
    case "zebra": t.zebra = !t.zebra; break;
    case "border": t.border = !t.border; break;
    case "width": {
      if (op.value != null && !(op.value >= 1 && op.value <= 100)) { ok = false; note = "Enter a width between 1 and 100 percent, or leave it empty for automatic."; break; }
      t.widths[col] = op.value == null ? null : Math.round(op.value);
      break;
    }
  }
  if (!ok) return { html: serializeTable(model), r, i, note: note ?? limitNote(op.type) };
  const nrC = clamp(nr, 0, t.rows.length - 1);
  return { html: serializeTable(t), r: nrC, i: clamp(ni, 0, Math.max(0, (t.rows[nrC]?.length ?? 1) - 1)) };
}

function limitNote(type: string): string {
  if (type === "delRow") return "A table needs at least one row.";
  if (type === "delCol") return "A table needs at least one column.";
  if (type.startsWith("row")) return `Tables are limited to ${MAX_ROWS} rows.`;
  if (type.startsWith("col")) return `Tables are limited to ${MAX_COLS} columns.`;
  return "";
}

export interface TableState { rows: number; cols: number; header: boolean; zebra: boolean; border: boolean; width: number | null; /** set (only) when the table is too large for the tools */ tooLarge?: true }
/** What the toolbar shows for the table around the caret. */
export function tableState(html: string, r: number, i: number): TableState | null {
  const t = parseTable(html);
  if (!t) return null;
  if (tableTooLarge(t)) return { rows: t.rows.length, cols: 0, header: false, zebra: false, border: false, width: null, tooLarge: true };
  const { cols, cells } = layout(t);
  const at = cells.find((p) => p.r === r && p.i === i);
  return { rows: t.rows.length, cols, header: t.header, zebra: t.zebra, border: t.border, width: at ? (t.widths[at.c] ?? null) : null };
}

/** Where Tab / Shift+Tab goes from cell (r, i), given each row's cell count; "addRow" = Tab in the very last cell. */
export function tabTarget(rowLens: number[], r: number, i: number, back: boolean): { r: number; i: number } | "addRow" | null {
  if (!rowLens.length || r < 0 || r >= rowLens.length) return null;
  if (!back) {
    if (i + 1 < rowLens[r]) return { r, i: i + 1 };
    if (r + 1 < rowLens.length && rowLens[r + 1] > 0) return { r: r + 1, i: 0 };
    return "addRow";
  }
  if (i > 0) return { r, i: i - 1 };
  for (let k = r - 1; k >= 0; k--) if (rowLens[k] > 0) return { r: k, i: rowLens[k] - 1 };
  return null;
}

// ---- clipboard ----

export const MAX_PASTE_CELLS = 5000;
/** Clipboard HTML (or text) beyond this is never inspected for a table: it goes down the ordinary, sanitised paste path. */
export const MAX_PASTE_HTML = 1_000_000;

const NUMERIC = /^[\s$€£¥]*[-+(]?[\s$€£¥]*\d[\d,.\s']*%?\)?\s*[%$€£¥]?\s*$/;
export const looksNumeric = (text: string) => text.length <= 64 && NUMERIC.test(text);

const SKIP_CONTENT = new Set(["script", "style", "head", "title", "xml", "noscript", "template", "svg", "math", "object", "iframe"]);
const VOID = new Set(["br", "hr", "img", "input", "meta", "link", "col", "wbr", "base"]);
const BLOCKISH = new Set(["p", "div", "li", "h1", "h2", "h3", "h4", "h5", "h6", "tr", "blockquote", "pre"]);
/** Pasted content that is hidden (display:none, visibility, zero opacity / font size, off-screen, `hidden`, `mso-hide:all`) is dropped, never revealed. */
const hides = (attrs: string): boolean => attrsHide(attrs, true);
const fmt = (attrs: string) => {
  const w = (styleOf(attrs, "font-weight") ?? "").replace(/\s*!important/, "");
  const it = (styleOf(attrs, "font-style") ?? "").replace(/\s*!important/, "");
  return { b: w === "bold" || w === "bolder" || (/^\d+$/.test(w) && +w >= 600), i: it === "italic" || it === "oblique" };
};
const safeHref = (h: string) => { const n = h.replace(/[\u0000-\u0020]/g, "").toLowerCase(); return /^(https?:|mailto:)/.test(n) ? h.trim() : null; };

/** Cell content from Excel / Sheets / the web: text, line breaks, bold, italic, underline, strike and safe links only. */
export function cleanCellHtml(inner: string, cellAttrs = ""): string {
  const out: string[] = [];
  const stack: { name: string; closer: string }[] = [];
  let skip = "", skipN = 0;
  const base = fmt(cellAttrs);
  const head = (base.b ? "<b>" : "") + (base.i ? "<i>" : ""), tail = (base.i ? "</i>" : "") + (base.b ? "</b>" : "");
  for (const m of tokens(inner)) {
    if (m.text) {
      if (!skip) { const t = decode(inner.slice(m.index, m.end)).replace(/[\s\u00a0]+/g, " "); if (t) out.push(esc(t)); }
      continue;
    }
    const { closing, tag, attrs } = m;
    if (skip) { if (tag === skip) { if (!closing) skipN++; else if (--skipN <= 0) skip = ""; } continue; }
    if (!closing && !VOID.has(tag) && !/\/\s*$/.test(attrs) && (SKIP_CONTENT.has(tag) || hides(attrs))) { skip = tag; skipN = 1; continue; }
    if (tag === "br") { out.push("<br>"); continue; }
    if (VOID.has(tag)) continue;
    if (closing) {
      const k = stack.map((s) => s.name).lastIndexOf(tag);
      if (k >= 0) { const popped = stack.splice(k); for (let j = popped.length - 1; j >= 0; j--) out.push(popped[j].closer); }
      if (BLOCKISH.has(tag)) out.push("<br>");
      continue;
    }
    const f = fmt(attrs);
    let open = "", close = "";
    if (tag === "b" || tag === "strong" || f.b) { open += "<b>"; close = "</b>" + close; }
    if (tag === "i" || tag === "em" || f.i) { open += "<i>"; close = "</i>" + close; }
    if (tag === "u") { open += "<u>"; close = "</u>" + close; }
    if (tag === "s" || tag === "strike" || tag === "del") { open += "<s>"; close = "</s>" + close; }
    if (tag === "a") { const h = safeHref(attrOf(attrs, "href") ?? ""); if (h) { open += `<a href="${esc(h)}">`; close = "</a>" + close; } }
    stack.push({ name: tag, closer: close });
    out.push(open);
  }
  for (let j = stack.length - 1; j >= 0; j--) out.push(stack[j].closer);
  let h = head + out.join("") + tail;
  h = h.replace(/ {2,}/g, " ");
  for (let n = 0; n < 4; n++) h = h.replace(/<(b|i|u|s)>\s*<\/\1>/g, "");
  h = h.replace(/ {2,}/g, " ").replace(/\s*<br>\s*/g, "<br>").replace(/(<br>){3,}/g, "<br><br>").replace(/^(\s|<br>)+/, "").replace(/(\s|<br>)+$/, "").trim();
  return h.replace(/<[^>]*>/g, "").trim() === "" ? "" : h;
}

const plainOf = (html: string) => decode(html.replace(/<br>/g, " ").replace(/<[^>]*>/g, "")).replace(/\s+/g, " ").trim();

export type PasteResult = { kind: "table"; model: TableModel; truncated: boolean } | { kind: "inline"; html: string };

function finish(rows: Cell[][], header: boolean, truncated = false): PasteResult | null {
  if (rows.length > MAX_ROWS) { rows = rows.slice(0, MAX_ROWS); truncated = true; }
  rows = rows.map((row) => {
    let used = 0;
    const kept: Cell[] = [];
    for (const c of row) {
      if (used >= MAX_COLS) { truncated = true; break; }
      const cs = span(c).cs;
      if (used + cs > MAX_COLS) { c.colspan = MAX_COLS - used; truncated = true; if (c.colspan <= 1) delete c.colspan; }
      used += Math.min(cs, MAX_COLS - used);
      kept.push(c);
    }
    return kept;
  });
  while (rows.length > 1 && rows[rows.length - 1].every((c) => !c.html)) rows.pop();
  const total = rows.reduce((n, r) => n + r.length, 0);
  if (total > MAX_PASTE_CELLS) { rows = rows.slice(0, Math.max(1, Math.floor(MAX_PASTE_CELLS / Math.max(1, ...rows.map((r) => r.length))))); truncated = true; }
  rows.forEach((row, ri) => row.forEach((c) => { if (c.rowspan) c.rowspan = Math.min(c.rowspan, rows.length - ri); if (c.rowspan === 1) delete c.rowspan; }));
  // Merged-cell area beyond the budget (e.g. thousands of 50 x 30 cells): drop the row merges so the grid stays cheap to lay out.
  if (tableTooLarge({ rows, header: false, zebra: false, border: false, widths: [] })) rows.forEach((row) => row.forEach((c) => { delete c.rowspan; }));
  if (!rows.some((r) => r.some((c) => c.html))) return null;
  if (rows.length === 1 && rows[0].length === 1) return { kind: "inline", html: rows[0][0].html };
  const t: TableModel = { rows, header, zebra: false, border: true, widths: [] };
  t.widths = Array(layout(t).cols).fill(null);
  return { kind: "table", model: t, truncated };
}

/** True when `html` has visible text outside `[from, to)` (ignoring style / script / head / title / xml content and tags). */
function textOutside(html: string, from: number, to: number): boolean {
  let skip = "", skipN = 0;
  for (const m of tokens(html)) {
    if (m.text) { if (!skip && (m.end <= from || m.index >= to) && decode(html.slice(m.index, m.end)).replace(/[\s\u00a0]+/g, "") !== "") return true; continue; }
    if (m.index >= from && m.index < to) continue;
    if (skip) { if (m.tag === skip) { if (!m.closing) skipN++; else if (--skipN <= 0) skip = ""; } continue; }
    if (!m.closing && !/\/\s*$/.test(m.attrs) && ["style", "script", "head", "title", "xml"].includes(m.tag)) { skip = m.tag; skipN = 1; }
  }
  return false;
}

/** A lone `<table>` copied from Excel, Numbers, Google Sheets or a web page, cleaned. Null when `html` is anything else (or too large to inspect). */
export function tableFromHtml(html: string): PasteResult | null {
  if (html.length > MAX_PASTE_HTML) return null;
  const p = parseRaw(html, true);
  if (!p || !p.table.rows.length) return null;
  // A hidden table is not a "paste a table" case: the ordinary (sanitising) paste path removes it.
  if (p.table.hidden) return null;
  // Content outside the table (an article that merely contains one) is not a "paste a table" case either.
  if (textOutside(html, p.start, p.end)) return null;
  // Hidden rows / sections / cells are dropped, not rebuilt as visible ones.
  let truncated = false;
  let raw = p.table.rows.filter((r) => !r.hidden).map((r) => r.cells.filter((c) => !c.hidden)).filter((cells) => cells.length > 0);
  if (!raw.length) return null;
  if (raw.length > MAX_ROWS + 1) { raw = raw.slice(0, MAX_ROWS + 1); truncated = true; }
  const header = p.table.thead || raw[0].every((c) => c.head);
  const rows = raw.map((cells) => {
    if (cells.length > MAX_COLS + 1) { cells = cells.slice(0, MAX_COLS + 1); truncated = true; }
    return cells.map((rc) => {
      const h = cleanCellHtml(rc.inner, rc.attrs);
      const c = cellOf(rc, h);
      if (!c.align && looksNumeric(plainOf(h))) c.align = "right";
      return c;
    });
  });
  return finish(rows, header, truncated);
}

const unquote = (f: string) => (f.length >= 2 && f.startsWith('"') && f.endsWith('"') ? f.slice(1, -1).replace(/""/g, '"') : f);
const countTabs = (l: string) => { let n = 0; for (let i = l.indexOf("\t"); i >= 0; i = l.indexOf("\t", i + 1)) n++; return n; };

/** Tab-separated text (Excel / Sheets plain-text flavour): at least two lines with the same number (>= 1) of tabs. */
export function tableFromTsv(text: string): PasteResult | null {
  let truncated = false;
  if (text.length > MAX_PASTE_HTML) { const cut = text.lastIndexOf("\n", MAX_PASTE_HTML); if (cut <= 0) return null; text = text.slice(0, cut); truncated = true; }
  let lines = text.replace(/\r\n?/g, "\n").replace(/\n+$/, "").split("\n");
  if (lines.length < 2) return null;
  if (lines.length > MAX_ROWS + 1) { lines = lines.slice(0, MAX_ROWS + 1); truncated = true; }
  const tabs = countTabs(lines[0]);
  if (tabs < 1 || lines.some((l) => countTabs(l) !== tabs)) return null;
  const rows = lines.map((l) => {
    let fields = l.split("\t", MAX_COLS + 1);
    if (fields.length > MAX_COLS) { fields = fields.slice(0, MAX_COLS); truncated = true; }
    return fields.map((f) => {
      const v = unquote(f.trim());
      const c: Cell = { html: esc(v).replace(/\n/g, "<br>") };
      if (looksNumeric(v)) c.align = "right";
      return c;
    });
  });
  return finish(rows, false, truncated);
}

/**
 * What a paste should become: a clean table (from HTML when the clipboard has any, else tab-separated text), or null for an
 * ordinary paste. Never throws: any failure while inspecting the clipboard means "not a table".
 */
export function tableFromClipboard(html: string, text: string): PasteResult | null {
  try {
    if (html.trim()) return tableFromHtml(html);
    return text ? tableFromTsv(text) : null;
  } catch { return null; }
}

/** Table (as email-safe markup) plus a paragraph after it so the caret can leave the table. */
export const tableBlock = (t: TableModel) => serializeTable(t) + "<div><br></div>";
