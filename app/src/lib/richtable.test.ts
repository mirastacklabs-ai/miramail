// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/richtable.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import {
  newTable, serializeTable, parseTable, applyTableOp, tableState, tabTarget, clampDims, layout, cleanCellHtml, tableFromHtml, tableFromTsv, tableFromClipboard,
  looksNumeric, tableBlock, MAX_ROWS, MAX_COLS, MAX_INSERT_ROWS, MAX_INSERT_COLS, MAX_PASTE_HTML, MAX_PASTE_CELLS, TOO_LARGE_NOTE, tableTooLarge, ownRowIndex,
} from "./richtable.ts";
import { readFileSync } from "node:fs";
import { styleHides, attrsHide, decodeEntities } from "./csshide.ts";
import { buildRestore } from "./outboxmodel.ts";
import { htmlForDraft, extractInlineImages } from "./composetext.ts";

const rt = (t) => parseTable(serializeTable(t));
const texts = (t) => t.rows.map((r) => r.map((c) => c.html));
const filled = (rows, cols) => { const t = newTable(rows, cols); t.rows.forEach((r, i) => r.forEach((c, j) => (c.html = `r${i}c${j}`))); return t; };
const op = (t, o, r = 0, i = 0) => applyTableOp(serializeTable(t), o, r, i);

test("insert dimensions are clamped to a sane cap", () => {
  assert.deepEqual(clampDims(0, 0), { rows: 1, cols: 1 });
  assert.deepEqual(clampDims(999, 999), { rows: MAX_INSERT_ROWS, cols: MAX_INSERT_COLS });
  assert.deepEqual(clampDims(NaN, 3.7), { rows: 2, cols: 3 });
  const t = newTable(500, 500);
  assert.equal(t.rows.length, MAX_INSERT_ROWS);
  assert.equal(t.rows[0].length, MAX_INSERT_COLS);
});

test("serialised markup is email safe: attributes, inline styles, no classes, style tags or resources", () => {
  const h = serializeTable(newTable(2, 3, { header: true, zebra: true }));
  for (const want of ["<table", 'width="100%"', 'cellpadding="6"', 'cellspacing="0"', 'border="1"', "border-collapse:collapse", "<thead>", "<th", "<tbody>", "<td", 'bgcolor="#EEF0F3"', "font-weight:bold"]) assert.ok(h.includes(want), want);
  for (const bad of ["class=", "<style", "<link", "url(", "http", "<script", "id="]) assert.ok(!h.includes(bad), bad);
  assert.equal((h.match(/<th /g) ?? []).length, 3);
  assert.equal((h.match(/<td/g) ?? []).length, 3);
  assert.ok(!serializeTable(newTable(1, 1, { border: false })).includes("border:1px"));
});

test("header, zebra and border flags survive serialise -> parse", () => {
  for (const header of [false, true]) for (const zebra of [false, true]) for (const border of [false, true]) {
    const t = rt({ ...filled(5, 2), header, zebra, border });
    assert.deepEqual([t.header, t.zebra, t.border], [header, zebra, border], JSON.stringify({ header, zebra, border }));
  }
  const t = rt(filled(3, 2));
  assert.deepEqual(texts(t), [["r0c0", "r0c1"], ["r1c0", "r1c1"], ["r2c0", "r2c1"]]);
});

test("zebra stripes alternate on body rows only", () => {
  const h = serializeTable({ ...filled(4, 1), header: true, zebra: true });
  const rows = h.match(/<tr>[\s\S]*?<\/tr>/g);
  assert.ok(rows[0].includes("#EEF0F3"));
  assert.ok(!rows[1].includes("#F5F7FA") && rows[2].includes("#F5F7FA") && !rows[3].includes("#F5F7FA"));
});

test("add and remove rows keep the content in order and respect the header", () => {
  const t = filled(3, 2);
  let r = op(t, { type: "rowBelow" }, 0, 0);
  assert.deepEqual(texts(parseTable(r.html)).map((x) => x[0]), ["r0c0", "", "r1c0", "r2c0"]);
  assert.equal(r.r, 1);
  r = op(t, { type: "rowAbove" }, 1, 0);
  assert.deepEqual(texts(parseTable(r.html)).map((x) => x[0]), ["r0c0", "", "r1c0", "r2c0"]);
  r = op(t, { type: "delRow" }, 1, 0);
  assert.deepEqual(texts(parseTable(r.html)).map((x) => x[0]), ["r0c0", "r2c0"]);
  assert.equal(r.r, 1);
  // Nothing goes above the header row.
  const h = { ...filled(2, 2), header: true };
  r = op(h, { type: "rowAbove" }, 0, 0);
  const p = parseTable(r.html);
  assert.equal(p.header, true);
  assert.equal(p.rows[0][0].html, "r0c0");
  assert.equal(r.r, 1);
  // Last row cannot be removed.
  r = op(filled(1, 2), { type: "delRow" }, 0, 0);
  assert.ok(r.note && parseTable(r.html).rows.length === 1);
});

test("deleting the header row clears the header flag", () => {
  const r = op({ ...filled(3, 2), header: true }, { type: "delRow" }, 0, 0);
  const p = parseTable(r.html);
  assert.equal(p.header, false);
  assert.equal(p.rows.length, 2);
});

test("add and remove columns, with widths following the columns", () => {
  let t = filled(2, 3);
  t.widths = [20, 30, 50];
  let r = op(t, { type: "colRight" }, 0, 0);
  let p = parseTable(r.html);
  assert.deepEqual(texts(p), [["r0c0", "", "r0c1", "r0c2"], ["r1c0", "", "r1c1", "r1c2"]]);
  assert.deepEqual(p.widths, [20, null, 30, 50]);
  r = op(t, { type: "colLeft" }, 0, 1);
  p = parseTable(r.html);
  assert.deepEqual(texts(p)[0], ["r0c0", "", "r0c1", "r0c2"]);
  r = op(t, { type: "delCol" }, 1, 1);
  p = parseTable(r.html);
  assert.deepEqual(texts(p), [["r0c0", "r0c2"], ["r1c0", "r1c2"]]);
  assert.deepEqual(p.widths, [20, 50]);
  r = op(filled(2, 1), { type: "delCol" }, 0, 0);
  assert.ok(r.note);
});

test("rows and columns are capped", () => {
  let r = op(newTable(1, 1), { type: "rowBelow" });
  let html = r.html;
  for (let k = 0; k < MAX_ROWS + 5; k++) { const x = applyTableOp(html, { type: "rowBelow" }, 0, 0); html = x.html; if (x.note) { assert.match(x.note, /limited/); break; } }
  assert.equal(parseTable(html).rows.length, MAX_ROWS);
  html = serializeTable(newTable(1, 1));
  for (let k = 0; k < MAX_COLS + 5; k++) { const x = applyTableOp(html, { type: "colRight" }, 0, 0); html = x.html; if (x.note) break; }
  assert.equal(layout(parseTable(html)).cols, MAX_COLS);
});

test("column width: percentage or auto", () => {
  const t = filled(2, 2);
  let r = op(t, { type: "width", value: 30 }, 1, 1);
  let p = parseTable(r.html);
  assert.deepEqual(p.widths, [null, 30]);
  assert.ok(r.html.includes('width="30%"'));
  assert.equal(tableState(r.html, 1, 1).width, 30);
  r = applyTableOp(r.html, { type: "width", value: null }, 0, 1);
  assert.deepEqual(parseTable(r.html).widths, [null, null]);
  assert.ok(applyTableOp(r.html, { type: "width", value: 500 }, 0, 0).note);
  assert.ok(applyTableOp(r.html, { type: "width", value: 0 }, 0, 0).note);
});

test("toggles", () => {
  const t = filled(3, 2);
  assert.equal(parseTable(op(t, { type: "header" }).html).header, true);
  assert.equal(parseTable(op(t, { type: "zebra" }).html).zebra, true);
  assert.equal(parseTable(op(t, { type: "border" }).html).border, false);
  assert.deepEqual(tableState(serializeTable(t), 0, 0), { rows: 3, cols: 2, header: false, zebra: false, border: true, width: null });
  assert.equal(applyTableOp("<p>no table</p>", { type: "header" }, 0, 0), null);
});

test("cell content (formatting, nested markup) is kept through operations", () => {
  const t = filled(2, 2);
  t.rows[0][0].html = '<b>bold</b> and <a href="https://example.org">link</a>';
  t.rows[1][1].align = "right";
  const p = parseTable(op(t, { type: "rowBelow" }).html);
  assert.equal(p.rows[0][0].html, '<b>bold</b> and <a href="https://example.org">link</a>');
  assert.equal(p.rows[2][1].align, "right");
});

test("merged cells: colspan and rowspan round-trip and column ops widen/shrink spans", () => {
  const html = '<table><tr><td colspan="2">wide</td><td>c</td></tr><tr><td>a</td><td rowspan="2">b</td><td>c2</td></tr><tr><td>a3</td><td>c3</td></tr></table>';
  const t = parseTable(html);
  assert.equal(t.rows[0][0].colspan, 2);
  assert.equal(layout(t).cols, 3);
  assert.equal(parseTable(serializeTable(t)).rows[1][1].rowspan, 2);
  // Adding a column inside the 2-wide cell widens it instead of splitting it.
  let p = parseTable(applyTableOp(html, { type: "colRight" }, 1, 0).html);
  assert.equal(p.rows[0][0].colspan, 3);
  assert.equal(p.rows[1].length, 4);
  // Removing a column that the 2-wide cell covers shrinks it.
  p = parseTable(applyTableOp(html, { type: "delCol" }, 1, 0).html);
  assert.equal(p.rows[0][0].colspan, undefined);
  // Adding a row under a rowspan extends it.
  p = parseTable(applyTableOp(html, { type: "rowBelow" }, 1, 0).html);
  assert.equal(p.rows[1][1].rowspan, 3);
  assert.equal(p.rows[2].length, 2);
  // Removing the row that starts a rowspan hands the cell to the next row.
  p = parseTable(applyTableOp(html, { type: "delRow" }, 1, 0).html);
  assert.equal(p.rows.length, 2);
  assert.equal(p.rows[1].some((c) => c.html === "b"), true);
});

test("Tab navigation: next cell, next row, add a row at the very end, Shift+Tab back", () => {
  const lens = [2, 2, 2];
  assert.deepEqual(tabTarget(lens, 0, 0, false), { r: 0, i: 1 });
  assert.deepEqual(tabTarget(lens, 0, 1, false), { r: 1, i: 0 });
  assert.equal(tabTarget(lens, 2, 1, false), "addRow");
  assert.deepEqual(tabTarget(lens, 1, 0, true), { r: 0, i: 1 });
  assert.equal(tabTarget(lens, 0, 0, true), null);
  assert.equal(tabTarget([], 0, 0, false), null);
});

test("tableBlock leaves a paragraph after the table", () => {
  assert.ok(tableBlock(newTable(1, 1)).endsWith("</table><div><br></div>"));
});

// ---- paste ----

const EXCEL = `<html xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:x="urn:schemas-microsoft-com:office:excel" xmlns="http://www.w3.org/TR/REC-html40">
<head><meta name=ProgId content=Excel.Sheet><meta name=Generator content="Microsoft Excel 15"><link rel=File-List href="file:///C:/Temp/filelist.xml">
<style><!--table {mso-displayed-decimal-separator:"\\.";} .xl65 {mso-number-format:"0\\.00";} --></style></head>
<body link="#0563C1" vlink="#954F72">
<!--StartFragment-->
<table border=0 cellpadding=0 cellspacing=0 width=192 style='border-collapse:collapse;width:144pt'>
 <col width=64 span=3 style='width:48pt'>
 <tr height=20 style='height:15.0pt'>
  <td height=20 class=xl65 width=64 style='height:15.0pt;width:48pt'><b><font face="Calibri" color="#000000">Item</font></b></td>
  <td class=xl65 width=64 style='width:48pt;font-weight:700;mso-ignore:padding'>Qty</td>
  <td class=xl65 align=right width=64 style='width:48pt'><o:p></o:p>Price</td>
 </tr>
 <tr height=20 style='height:15.0pt'>
  <td height=20 class=xl66 style='height:15.0pt'><i>Apples</i> &amp; pears</td>
  <td class=xl66 align=right x:num>12</td>
  <td class=xl67 align=right x:num="3.5" x:fmla="=A1*2">$3.50</td>
 </tr>
 <tr height=20 style='height:15.0pt'>
  <td colspan=2 height=20 class=xl66 style='height:15.0pt;mso-number-format:"\\@"'>Total <span style='display:none'>SECRET</span></td>
  <td class=xl68 x:num>1,234.50</td>
 </tr>
 <![if supportMisalignedColumns]><tr height=0 style='display:none'><td width=64 style='width:48pt'>HIDDENROW</td></tr><![endif]>
</table>
<!--EndFragment-->
</body></html>`;

test("Excel html: junk removed, bold/italic/numeric alignment/colspan kept", () => {
  const r = tableFromHtml(EXCEL);
  assert.equal(r.kind, "table");
  const t = r.model;
  assert.equal(t.rows.length, 3); // the hidden trailing row is gone
  const out = serializeTable(t);
  for (const bad of ["mso-", "class=", "xl65", "<col", "<font", "SECRET", "HIDDENROW", "StartFragment", "<style", "<meta", "face=", "x:num", "o:p", "supportMisaligned", "file:///"]) assert.ok(!out.includes(bad), bad + " in " + out);
  assert.equal(t.rows[0][0].html, "<b>Item</b>");
  assert.equal(t.rows[0][1].html, "<b>Qty</b>");
  assert.equal(t.rows[0][2].html, "Price");
  assert.equal(t.rows[1][0].html, "<i>Apples</i> &amp; pears");
  assert.equal(t.rows[1][1].align, "right");
  assert.equal(t.rows[1][2].align, "right");
  assert.equal(t.rows[2][0].colspan, 2);
  assert.equal(t.rows[2][0].html, "Total");
  assert.equal(t.rows[2][1].align, "right"); // 1,234.50 recognised as a number
  assert.equal(t.border, true);
});

const SHEETS = `<meta charset='utf-8'><google-sheets-html-origin><style type="text/css"><!--td {border: 1px solid #cccccc;}br {mso-data-placement:same-cell;}--></style><table xmlns="http://www.w3.org/1999/xhtml" cellspacing="0" cellpadding="0" dir="ltr" border="1" style="table-layout:fixed;font-size:10pt;font-family:Arial;width:0px;border-collapse:collapse;border:none" data-sheets-root="1"><colgroup><col width="100"/><col width="100"/></colgroup><tbody><tr style="height:21px;"><td style="overflow:hidden;padding:2px 3px 2px 3px;vertical-align:bottom;font-weight:bold;" data-sheets-value="{&quot;1&quot;:2,&quot;2&quot;:&quot;Name&quot;}">Name</td><td style="overflow:hidden;font-weight:bold;text-align:right;">Score</td></tr><tr style="height:21px;"><td style="overflow:hidden;" data-sheets-value="{}">Ann <a href="https://example.org/a">site</a> <a href="javascript:alert(1)">bad</a></td><td style="overflow:hidden;text-align:right;" data-sheets-numberformat="{}">98.5%</td></tr></tbody></table></google-sheets-html-origin>`;

test("Google Sheets html", () => {
  const r = tableFromHtml(SHEETS);
  assert.equal(r.kind, "table");
  const t = r.model;
  assert.equal(t.rows[0][0].html, "<b>Name</b>");
  assert.equal(t.rows[0][1].align, "right");
  assert.equal(t.rows[1][0].html, 'Ann <a href="https://example.org/a">site</a> bad');
  assert.equal(t.rows[1][1].html, "98.5%");
  const out = serializeTable(t);
  for (const bad of ["data-sheets", "javascript", "overflow", "google-sheets", "colgroup", "<style"]) assert.ok(!out.includes(bad), bad);
});

test("web table with thead/th becomes a header table; nested tables stay inside the cell text", () => {
  const r = tableFromHtml('<table class="x"><thead><tr><th>A</th><th>B</th></tr></thead><tbody><tr><td>1</td><td><table><tr><td>in</td></tr></table></td></tr></tbody></table>');
  assert.equal(r.model.header, true);
  assert.equal(r.model.rows.length, 2);
  assert.equal(r.model.rows[1][1].html, "in");
});

test("html with more than a table is an ordinary paste", () => {
  assert.equal(tableFromHtml("<p>Intro paragraph that matters</p><table><tr><td>a</td><td>b</td></tr></table>"), null);
  assert.equal(tableFromHtml("<p>nothing</p>"), null);
  assert.equal(tableFromHtml("<table></table>"), null);
  assert.equal(tableFromClipboard("<p>x</p>", "a\tb\nc\td"), null); // html present and not a table: do not guess from text
});

test("a single copied cell pastes as inline text, not a 1x1 table", () => {
  const r = tableFromHtml("<table><tr><td><b>Hi</b> there</td></tr></table>");
  assert.deepEqual(r, { kind: "inline", html: "<b>Hi</b> there" });
});

test("hostile clipboard html is flattened to safe cell markup", () => {
  const r = tableFromHtml('<table><tr><td onclick="x()" style="background:url(https://evil.example/x)"><img src="https://evil.example/p.gif"><script>alert(1)</script><a href="data:text/html,x" onclick="y()">l</a><b onmouseover="z()">ok</b></td><td>2</td></tr></table>');
  const out = serializeTable(r.model);
  for (const bad of ["onclick", "evil", "<script", "alert", "data:", "onmouseover", "<img", "url("]) assert.ok(!out.includes(bad), bad + " in " + out);
  assert.ok(out.includes("<b>ok</b>"));
});

test("TSV: several lines with equal tab counts become a table", () => {
  const r = tableFromTsv("Name\tQty\tPrice\r\nApples\t3\t$1.50\r\n\"Big, \"\"red\"\" pears\"\t12\t2\r\n");
  assert.equal(r.kind, "table");
  assert.deepEqual(texts(r.model), [["Name", "Qty", "Price"], ["Apples", "3", "$1.50"], ['Big, &quot;red&quot; pears', "12", "2"]]);
  assert.equal(r.model.rows[1][1].align, "right");
  assert.equal(r.model.rows[1][0].align, undefined);
  assert.equal(r.model.header, false);
});

test("TSV: not a table when counts differ, one line, or no tabs", () => {
  assert.equal(tableFromTsv("a\tb\nc\td\te"), null);
  assert.equal(tableFromTsv("a\tb"), null);
  assert.equal(tableFromTsv("one\ntwo\nthree"), null);
  assert.equal(tableFromTsv(""), null);
  assert.ok(tableFromClipboard("", "a\tb\nc\td"));
});

test("TSV text is escaped", () => {
  const r = tableFromTsv("<script>alert(1)</script>\t&\n<b>x</b>\t\"");
  const out = serializeTable(r.model);
  assert.ok(!out.includes("<script") && !out.includes("<b>x"));
  assert.ok(out.includes("&lt;script&gt;") && out.includes("&amp;"));
});

test("pasted tables are capped in rows and columns", () => {
  const r = tableFromTsv(Array(MAX_ROWS + 50).fill("x\ty\tz").join("\n"));
  assert.equal(r.truncated, true);
  assert.equal(r.model.rows.length, MAX_ROWS);
  const c = tableFromTsv(Array(MAX_ROWS + 50).fill(Array(MAX_COLS + 10).fill("x").join("\t")).join("\n"));
  assert.equal(c.model.rows[0].length, MAX_COLS);
  assert.ok(c.model.rows.reduce((n, row) => n + row.length, 0) <= 5000, "total cell cap");
  const wide = tableFromHtml("<table><tr><td colspan='9999'>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>");
  assert.ok(wide.model.rows[0][0].colspan <= MAX_COLS);
});

test("cleanCellHtml", () => {
  assert.equal(cleanCellHtml("<p class=MsoNormal>One<o:p></o:p></p><p>Two</p>"), "One<br>Two");
  assert.equal(cleanCellHtml('<span style="font-weight:700">B</span> <span style="font-style:italic">I</span>'), "<b>B</b> <i>I</i>");
  assert.equal(cleanCellHtml("a&nbsp;&nbsp;b &lt;c&gt;"), "a b &lt;c&gt;");
  assert.equal(cleanCellHtml("<![if !vml]>text<![endif]><!--[if gte mso 9]><xml>junk</xml><![endif]-->"), "text");
  assert.equal(cleanCellHtml("<b></b>   "), "");
  assert.equal(cleanCellHtml("x", "style='font-weight:bold'"), "<b>x</b>");
  assert.equal(cleanCellHtml('<div style="display:none">hidden</div>shown'), "shown");
});

test("looksNumeric", () => {
  for (const v of ["12", "1,234.50", "$3.50", "-4", "(12)", "98.5%", "€ 10", "1 000"]) assert.ok(looksNumeric(v), v);
  for (const v of ["abc", "12 apples", "", "A1", "2024-01-05x"]) assert.ok(!looksNumeric(v), v);
});

test("draft/restore path: serialised tables survive the real htmlForDraft and JSON round trips", () => {
  const pic = '<img src="data:image/png;base64,' + "A".repeat(2000) + '">';
  const html = serializeTable({ ...filled(3, 3), header: true, zebra: true }) + pic;
  const slim = htmlForDraft(html, 500); // over the limit: the picture data goes, the table stays
  assert.ok(!slim.includes("AAAA") && slim.includes('data-mm-dropped="1"'));
  const back = JSON.parse(JSON.stringify({ html: slim })).html;
  assert.deepEqual(texts(parseTable(back)), texts({ rows: filled(3, 3).rows }));
  assert.equal(parseTable(back).zebra, true);
  assert.equal(htmlForDraft(html, 1_000_000), html);
});

test("size-reduction paths keep tables: htmlForDraft, extractInlineImages and buildRestore drop pictures first, never the table", () => {
  const table = serializeTable({ ...filled(4, 3), header: true, zebra: true });
  const pic = '<img src="data:image/png;base64,' + "A".repeat(400_000) + '" alt="x">';
  const html = table + pic;
  const draft = htmlForDraft(html, 100_000);
  assert.ok(draft.includes(table) && !draft.includes("AAAA"));
  const ex = extractInlineImages(html);
  assert.ok(ex.html.includes(table) && ex.images.length === 1);
  const r = buildRestore({ text: "t", to: "a@b.example" }, html, 1, 50_000);
  const back = JSON.parse(r.restore);
  assert.equal(r.inlineCount, 0);
  assert.ok(back.html.includes(table), "table kept when only the picture is dropped");
  // Table alone too big for the limit: falls back to plain text (never refuses).
  const big = buildRestore({ text: "t" }, serializeTable(filled(30, 10)), 0, 500);
  assert.equal(JSON.parse(big.restore).html, undefined);
});

// ---- review fixes (Q01-Q10) ----

const cellTexts = (r) => (r?.kind === "table" ? r.model.rows.map((row) => row.map((c) => c.html)) : r);
const timed = (label, fn, ms = 1500) => { const t0 = performance.now(); const out = fn(); const dt = performance.now() - t0; assert.ok(dt < ms, `${label} took ${Math.round(dt)} ms`); return out; };

test("Q01: hidden rows, cells, sections and tables with NON-empty content are dropped from a pasted table, never rebuilt visible", () => {
  const hides = ["display:none", "visibility:hidden", "visibility:collapse", "font-size:0", "font-size:0px", "opacity:0", "mso-hide:all", "position:absolute;left:-9999px", "text-indent:-9999px", "color:transparent", "max-height:0;overflow:hidden", "DISPLAY : NONE !important", "display&colon;none"];
  for (const h of hides) {
    const r = tableFromHtml(`<table><tr><td>keep1</td><td style="${h}">SECRETCELL</td><td>keep2</td></tr><tr style="${h}"><td>SECRETROW</td><td>x</td></tr><tbody style="${h}"><tr><td>SECRETSECTION</td><td>y</td></tr></tbody><tr><td>keep3</td><td>keep4</td></tr></table>`);
    assert.deepEqual(cellTexts(r), [["keep1", "keep2"], ["keep3", "keep4"]], h);
  }
  assert.deepEqual(cellTexts(tableFromHtml("<table><tr><td>a</td><td hidden>SECRET</td><td>b</td></tr><tr hidden><td>SECRETROW</td></tr><tr><td>c</td><td>d</td></tr></table>")), [["a", "b"], ["c", "d"]]);
  // A hidden table is not a paste-a-table case at all (the ordinary, sanitising paste path removes it).
  for (const h of ["display:none", "font-size:0", "opacity:0", "visibility:hidden"]) assert.equal(tableFromHtml(`<table style="${h}"><tr><td>SECRET</td><td>x</td></tr></table>`), null, h);
  assert.equal(tableFromHtml("<table hidden><tr><td>SECRET</td><td>x</td></tr></table>"), null);
  // Hidden nested table / element inside a visible cell.
  const n = tableFromHtml('<table><tr><td>v<table style="display:none"><tr><td>SECRETNESTED</td></tr></table></td><td>w<span style="opacity:0">SECRETSPAN</span></td></tr></table>');
  assert.deepEqual(cellTexts(n), [["v", "w"]]);
  // A fake `style=` inside another attribute's value cannot override the real hiding style.
  assert.equal(attrsHide(`title="style='opacity:1'" style="display:none"`, true), true);
  assert.deepEqual(cellTexts(tableFromHtml(`<table><tr><td>a</td><td title="x" style="display:none" data-x="style='color:red'">SECRET</td><td>b</td></tr></table>`)), [["a", "b"]]);
});

test("Q02: font-size / opacity keywords are NOT hidden; real zero and near-zero numbers are", () => {
  for (const kw of ["font-size: small", "font-size:medium", "font-size:large", "font-size:smaller", "font-size:inherit", "font-size:x-small", "font-size:12px", "font-size:2px", "opacity:inherit", "opacity:0.5", "opacity:1"]) {
    assert.equal(styleHides(kw), false, kw);
    assert.equal(styleHides(kw, true), false, kw);
    const r = tableFromHtml(`<table><tr><td style="${kw}">VISIBLE</td><td>b</td></tr></table>`);
    assert.deepEqual(cellTexts(r), [["VISIBLE", "b"]], kw);
    assert.equal(cleanCellHtml(`<span style="${kw}">VISIBLE</span>`), "VISIBLE", kw);
  }
  for (const h of ["opacity:0", "opacity:0e0", "opacity:-1", "opacity:+0", "opacity:0.001", "opacity:.05", "opacity:0%", "OPACITY: 0 !important", "font-size:0", "font-size:0e0px", "font-size:+0px", "font-size:0.01px", "font-size:1px", "font-size:0pt", "font-size:0em", "font-size:-2px", "font-size:0 !IMPORTANT", "color:transparent", "color:rgba(0,0,0,0)", "color:rgb(0 0 0 / 0%)", "color:#fff0", "font:0px Arial", "op/**/acity:0", "opacity:\\30"]) {
    assert.equal(styleHides(h), true, h);
    assert.equal(cleanCellHtml(`a<span style="${h}">HIDDEN</span>b`), "ab", h);
  }
  assert.equal(attrsHide('style="font-size:0e0px"'), true);
  assert.equal(attrsHide("style='opacity&#58;0'"), true);
  assert.equal(attrsHide("style='opacity:&#48;'"), true);
});

test("Q04: hostile unterminated markup is linear time and never throws; oversized clipboard html is skipped", () => {
  const n = 200_000;
  for (const [label, h] of [["<a", "<a".repeat(n)], ["<!--", "<!--".repeat(n)], ["<style", "<style".repeat(n)], ["<table<td", "<table<td".repeat(n / 2)], ["<td '", "<table><tr><td '".repeat(n / 8)], ["<x a=\"", "<x a=\"".repeat(n / 2)], ["<table><tr><td>+<a", "<table><tr><td>" + "<b".repeat(n)], ["</", "</".repeat(n)], ["<![", "<![".repeat(n)]]) {
    timed(label, () => { tableFromHtml(h); tableFromClipboard(h, ""); cleanCellHtml(h); parseTable(h); });
  }
  // Many real cells: row/column caps keep the work small.
  const cells = "<table><tr>" + "<td>x</td>".repeat(50_000) + "</tr></table>";
  const wide = timed("50k cells", () => tableFromHtml(cells));
  assert.equal(wide.model.rows[0].length, MAX_COLS);
  assert.equal(wide.truncated, true);
  // Beyond 1 MB the clipboard html is not inspected at all.
  const huge = "<table><tr><td>a</td><td>b</td></tr></table>" + " ".repeat(MAX_PASTE_HTML);
  assert.equal(tableFromHtml(huge), null);
  assert.equal(tableFromClipboard(huge, "a\tb\nc\td"), null);
  // tableFromClipboard turns any internal throw into "not a table".
  assert.equal(tableFromClipboard({ trim() { throw new Error("boom"); } }, ""), null);
});

test("Q04: unterminated tags still behave like a browser (rest of the input is swallowed, earlier content kept)", () => {
  assert.deepEqual(cellTexts(tableFromHtml("<table><tr><td>a</td><td>b</td></tr></table>")), [["a", "b"]]);
  assert.deepEqual(cellTexts(tableFromHtml('<table><tr><td>a</td><td>b</td></tr></table><b onclick="x')), [["a", "b"]]);
  assert.equal(cleanCellHtml("x &lt; y <b"), "x &lt; y");
  assert.equal(cleanCellHtml("1 < 2 and 3 > 2"), "1 &lt; 2 and 3 &gt; 2");
});

test("Q05: huge merged tables are refused by the table tools, quickly", () => {
  const cell = '<td colspan="50" rowspan="50">x</td>';
  const bomb = "<table>" + `<tr>${cell.repeat(100)}</tr>`.repeat(100) + "</table>"; // 10 000 cells, every one 50 x 50
  assert.equal(tableTooLarge(parseTable(bomb)), true);
  const st = timed("tableState", () => tableState(bomb, 0, 0));
  assert.equal(st.tooLarge, true);
  const res = timed("applyTableOp", () => applyTableOp(bomb, { type: "rowBelow" }, 0, 0));
  assert.equal(res.note, TOO_LARGE_NOTE);
  assert.equal(res.html, bomb, "unchanged");
  const manyRows = "<table>" + "<tr><td>x</td></tr>".repeat(MAX_ROWS + 1) + "</table>";
  assert.equal(tableState(manyRows, 0, 0).tooLarge, true);
  const manyCells = "<table>" + `<tr>${"<td>x</td>".repeat(30)}</tr>`.repeat(MAX_ROWS) + "<tr><td>y</td></tr></table>";
  assert.equal(tableState(manyCells, 0, 0).tooLarge, true);
  // The largest table the tools can build themselves is still editable.
  let full = serializeTable(newTable(1, 1));
  assert.equal(tableState(serializeTable({ ...newTable(MAX_ROWS, MAX_COLS) }), 0, 0).tooLarge, undefined);
  // Normal tables have no tooLarge key at all.
  assert.deepEqual(Object.keys(tableState(full, 0, 0)).sort(), ["border", "cols", "header", "rows", "width", "zebra"]);
  // layout clamps absurd colspans.
  assert.equal(layout({ rows: [[{ html: "", colspan: 99999, rowspan: 99999 }]], header: false, zebra: false, border: true, widths: [] }).cols <= MAX_COLS, true);
  // Pasted merge bombs are flattened before they become a model.
  const pasted = timed("paste bomb", () => tableFromHtml("<table>" + `<tr>${'<td colspan="50" rowspan="50">x</td>'.repeat(100)}</tr>`.repeat(50) + "</table>"));
  assert.ok(pasted.model.rows.length <= MAX_ROWS);
  timed("layout of paste", () => layout(pasted.model));
});

test("Q06: a multi-MB tab-separated paste is cut before cells are built", () => {
  const line = Array(200).fill("cell").join("\t");
  const big = Array(20_000).fill(line).join("\n"); // ~4 MB
  assert.ok(big.length > 3_000_000);
  const r = timed("tsv", () => tableFromTsv(big));
  assert.equal(r.truncated, true);
  assert.equal(r.model.rows.length <= MAX_ROWS, true);
  assert.equal(r.model.rows[0].length, MAX_COLS);
  const longLine = timed("one huge line", () => tableFromTsv("a\tb\n" + "c".repeat(5_000_000) + "\td"));
  assert.ok(longLine === null || longLine.kind === "table");
  assert.equal(tableFromTsv("x".repeat(3_000_000)), null);
});

test("Q09: prototype-ish entity names stay literal text", () => {
  for (const e of ["constructor", "__proto__", "toString", "hasOwnProperty", "valueOf"]) {
    assert.equal(decodeEntities(`&${e};`), `&${e};`, e);
    assert.equal(cleanCellHtml(`a &${e}; b`), `a &amp;${e}; b`, e);
  }
  assert.equal(decodeEntities("&amp;&lt;&NBSP;"), "&< ");
});

test("Q10: row order is document order and quoted / foreign rows are not counted", () => {
  const tfootFirst = "<table><tfoot><tr><td>F</td></tr></tfoot><tbody><tr><td>B1</td></tr><tr><td>B2</td></tr></tbody></table>";
  assert.deepEqual(texts(parseTable(tfootFirst)).flat(), ["F", "B1", "B2"]);
  // The DOM lists tfoot rows last (`table.rows`); the model keeps source order. The pure index helper counts source order.
  const own = { id: "outer" }, inner = { id: "inner" };
  const trs = [{ n: "F", t: own }, { n: "nested", t: inner }, { n: "B1", t: own }, { n: "B2", t: own }];
  assert.equal(ownRowIndex(trs, (x) => x.t, own, trs[2]), 1);
  assert.equal(ownRowIndex(trs, (x) => x.t, own, trs[3]), 2);
  assert.equal(ownRowIndex(trs, (x) => x.t, own, trs[0]), 0);
  assert.equal(ownRowIndex(trs, (x) => x.t, own, trs[1]), -1);
  // Editing the second source row hits B1, not the footer.
  const res = applyTableOp(tfootFirst, { type: "delRow" }, 1, 0);
  assert.deepEqual(texts(parseTable(res.html)).flat(), ["F", "B2"]);
});

test("Q13: the Rust test fixture is exactly what the TypeScript serializer emits", () => {
  const fixture = readFileSync(new URL("./fixtures/editor_table.html", import.meta.url), "utf8");
  const model = { rows: [[{ html: "Item" }, { html: "Qty" }], [{ html: "A" }, { html: "1", align: "right" }], [{ html: "" }, { html: "" }]], header: true, zebra: true, border: true, widths: [70, 30] };
  assert.equal(serializeTable(model), fixture);
  assert.deepEqual(texts(parseTable(fixture)), [["Item", "Qty"], ["A", "1"], ["", ""]]);
});

test("pasted-source hiding extras do not apply to the editor's own checks", () => {
  assert.equal(styleHides("text-indent:-9999px"), false);
  assert.equal(styleHides("text-indent:-9999px", true), true);
  assert.equal(styleHides("margin-left:-10px", true), false);
  assert.equal(MAX_PASTE_CELLS, 5000);
});
