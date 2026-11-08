// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/richblocks.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { calloutHtml, CALLOUTS, buttonHtml, BUTTON_COLORS, safeButtonUrl, dividerHtml, checklistHtml, toggleCheck, MAX_BUTTON_LABEL } from "./richblocks.ts";
import { parseColor, contrastRatio, lintCompose } from "./composelint.ts";
import { htmlToText } from "./composetext.ts";

test("callouts: one-cell table, coloured left border, tinted background, explicit text colour", () => {
  for (const k of Object.keys(CALLOUTS)) {
    const h = calloutHtml(k);
    const c = CALLOUTS[k];
    assert.equal((h.match(/<td /g) ?? []).length, 1, k);
    assert.equal((h.match(/<table/g) ?? []).length, 1, k);
    assert.ok(h.includes(`border-left:4px solid ${c.bar}`) && h.includes(`background-color:${c.bg}`) && h.includes(`bgcolor="${c.bg}"`) && h.includes(`color:${c.fg}`), k);
    assert.ok(h.includes(`<b>${c.label}:</b>`), k);
    for (const bad of ["class=", "<style", "url(", "http"]) assert.ok(!h.includes(bad), `${k} ${bad}`);
    assert.ok(h.endsWith("<div><br></div>"));
    // Readable on its own background (dark-mode clients keep explicit pairs).
    assert.ok(contrastRatio(parseColor(c.fg), parseColor(c.bg)) >= 7, `${k} contrast`);
    assert.deepEqual(lintCompose(h), [], k);
  }
  assert.ok(calloutHtml("nope").includes("Note:"));
});

test("button: table cell carries colour and padding, link carries text; label and URL validated", () => {
  for (const col of BUTTON_COLORS) {
    const h = buttonHtml("Open it", "https://example.org/a?b=1&c=2", col.id);
    assert.ok(h.includes(`bgcolor="${col.bg}"`) && h.includes(`background-color:${col.bg}`) && h.includes(`color:${col.fg}`), col.id);
    assert.ok(h.includes('href="https://example.org/a?b=1&amp;c=2"'));
    assert.ok(h.includes("padding:10px 22px") && h.includes("border-radius:6px") && h.includes("display:inline-block"));
    assert.ok(contrastRatio(parseColor(col.fg), parseColor(col.bg)) >= 4.5, col.id);
    assert.deepEqual(lintCompose(h), [], col.id);
  }
  assert.ok(buttonHtml("<b>x</b> \"q\"", "example.org").includes("&lt;b&gt;x&lt;/b&gt; &quot;q&quot;"));
  assert.equal(buttonHtml("", "https://a.example"), null);
  assert.equal(buttonHtml("  ", "https://a.example"), null);
  assert.equal(buttonHtml("Go", ""), null);
  assert.equal(buttonHtml("x".repeat(500), "https://a.example").match(/>(x+)<\/a>/)[1].length, MAX_BUTTON_LABEL);
});

test("button URL: https, http and mailto only", () => {
  assert.equal(safeButtonUrl("https://a.example/p?q=1#f"), "https://a.example/p?q=1#f");
  assert.equal(safeButtonUrl(" http://a.example "), "http://a.example");
  assert.equal(safeButtonUrl("mailto:a@b.example?subject=Hi"), "mailto:a@b.example?subject=Hi");
  assert.equal(safeButtonUrl("a@b.example"), "mailto:a@b.example");
  assert.equal(safeButtonUrl("example.org/path"), "https://example.org/path");
  assert.equal(safeButtonUrl("localhost:8080/x"), null);
  assert.equal(safeButtonUrl("example.org:8080/x"), "https://example.org:8080/x");
  for (const bad of ["javascript:alert(1)", "JaVaScRiPt:alert(1)", "java\tscript:alert(1)", "data:text/html,x", "vbscript:x", "file:///etc/passwd", "ftp://a.example", "tel:123", "//a.example", "/relative", "https://", "mailto:", "https://a.example/\"onclick=x", "https://a.example/<b>", "not a url", "", "   "]) {
    assert.equal(safeButtonUrl(bad), null, JSON.stringify(bad));
  }
});

test("button and callout read well in the plain-text part", () => {
  const t = htmlToText(buttonHtml("Open it", "https://example.org/go"));
  assert.equal(t, "Open it (https://example.org/go)");
  assert.equal(htmlToText(calloutHtml("warning").replace("&nbsp;</td>", " careful</td>")), "Warning: careful");
});

test("divider and checklist", () => {
  assert.ok(dividerHtml().startsWith("<hr style=") && !dividerHtml().includes("url("));
  assert.equal(htmlToText("<div>a</div>" + dividerHtml() + "<div>b</div>"), "a\n----------\n\nb");
  const c = checklistHtml(3);
  assert.equal((c.match(/☐/g) ?? []).length, 3);
  assert.ok(!c.includes("<input"));
  assert.equal((checklistHtml(500).match(/☐/g) ?? []).length, 20);
  assert.equal(htmlToText(checklistHtml(2) .replace(/&nbsp;/g, "x")), "☐x\n☐x");
  assert.equal(toggleCheck("☐ milk"), "☑ milk");
  assert.equal(toggleCheck("☑ milk"), "☐ milk");
  assert.equal(toggleCheck("milk"), "milk");
});

// ---- lint ----

test("lint: remote images, missing alt", () => {
  const w = lintCompose('<p><img src="https://t.example/p.png" alt="ok"><img src="//t.example/q.png"><img src="data:image/png;base64,AAAA"><img src="data:image/png;base64,AAAA" alt="fine"><img src="" data-mm-dropped="1"></p>');
  const by = Object.fromEntries(w.map((x) => [x.id, x]));
  assert.equal(by["remote-image"].count, 2);
  assert.equal(by["no-alt"].count, 2); // the // one and the first data: one
  for (const x of w) assert.ok(x.message && x.hint, x.id);
  assert.deepEqual(lintCompose('<img src="data:image/png;base64,AAAA" alt="a chart">'), []);
});

test("lint: wide tables", () => {
  assert.equal(lintCompose('<table width="800"><tr><td>x</td></tr></table>')[0].id, "wide-table");
  assert.match(lintCompose('<table style="width:900px"><tr><td>x</td></tr></table>')[0].message, /900 px/);
  assert.equal(lintCompose('<table><tr><td width="400">a</td><td width="300">b</td></tr></table>')[0].id, "wide-table");
  assert.deepEqual(lintCompose('<table width="100%"><tr><td width="50%">a</td><td width="50%">b</td></tr></table>'), []);
  assert.deepEqual(lintCompose('<table width="600"><tr><td width="300">a</td><td width="300">b</td></tr></table>'), []);
});

test("lint: low contrast, with inherited colours and explicit pairs", () => {
  const w = lintCompose('<div style="color:#bbbbbb">pale</div>');
  assert.equal(w.length, 1);
  assert.equal(w[0].id, "low-contrast");
  assert.match(w[0].message, /#bbbbbb on #ffffff/);
  assert.equal(lintCompose('<table><tr><td bgcolor="#222222"><b>dark cell, default text</b></td></tr></table>')[0].id, "low-contrast");
  assert.deepEqual(lintCompose('<table><tr><td bgcolor="#222222" style="color:#ffffff"><b>fine</b></td></tr></table>'), []);
  assert.equal(lintCompose('<font color="yellow">y</font>')[0].id, "low-contrast");
  assert.deepEqual(lintCompose('<span style="color:rgb(0, 0, 0)">ok</span><p>plain</p>'), []);
  assert.deepEqual(lintCompose('<div style="color:#bbbbbb"> </div><div style="color:rgba(0,0,0,0.5)">alpha ignored</div>'), []);
  // The inner pair wins over the outer one.
  assert.deepEqual(lintCompose('<div style="background-color:#000000;color:#ffffff"><span style="background-color:#ffffff;color:#000000">x</span></div>'), []);
});

test("lint: clean mail has no warnings; hostile or huge input does not throw", () => {
  assert.deepEqual(lintCompose(""), []);
  assert.deepEqual(lintCompose("<p>Hello <b>world</b></p>"), []);
  lintCompose("<table".repeat(1000) + "<td style=\"color:#" + "f".repeat(100000) + "\">");
  lintCompose("<img src=\"data:image/png;base64," + "A".repeat(2_000_000) + "\" alt=\"x\">");
  assert.equal(parseColor("javascript:1"), null);
  assert.equal(parseColor("transparent"), null);
  assert.deepEqual(parseColor("#abc"), [170, 187, 204]);
});

// ---- review fixes ----

test("Q12: mailto buttons allow only subject, body, cc, bcc and never a line break or control character", () => {
  for (const ok of ["mailto:a@b.example", "mailto:a@b.example?subject=Hi%20there&body=Line%2Fone", "mailto:a@b.example,c@d.example?cc=e@f.example&bcc=g@h.example", "MAILTO:a@b.example?SUBJECT=x"]) assert.equal(safeButtonUrl(ok), ok, ok);
  for (const bad of [
    "mailto:a@b.example?subject=x%0d%0aBcc:%20evil@x.example", "mailto:a@b.example?body=x%0Ay", "mailto:a@b.example?subject=x%00", "mailto:a@b.example?subject=x%1b", "mailto:a@b.example?subject=%E2%80%A8",
    "mailto:a@b.example?from=boss@x.example", "mailto:a@b.example?attach=/etc/passwd", "mailto:a@b.example?in-reply-to=x", "mailto:a@b.example?x-header=1", "mailto:a@b.example?subject=ok&references=1",
    "mailto:a@b.example%0d%0aBcc:evil@x.example", "mailto:a@b.example\r\nBcc: e@x.example", "mailto:a@b.example?subject=a\nb", "mailto:not-an-address", "mailto:?subject=x", "mailto:a@b.example?subject=%zz",
  ]) assert.equal(safeButtonUrl(bad), null, JSON.stringify(bad));
  assert.equal(safeButtonUrl("https://a.example/" + "x".repeat(2100)), null);
  assert.ok(safeButtonUrl("https://a.example/" + "x".repeat(1900)));
  assert.equal(safeButtonUrl("mailto:a@b.example?body=" + "x".repeat(2100)), null);
  assert.equal(buttonHtml("Go", "mailto:a@b.example?bcc=x@y.example%0d%0a"), null);
  assert.equal(safeButtonUrl("bad%0d%0a@b.example"), null);
});

test("Q09: callout kinds that are Object.prototype names fall back to Note", () => {
  for (const k of ["constructor", "__proto__", "toString", "hasOwnProperty", "valueOf"]) assert.ok(calloutHtml(k).includes("Note:"), k);
});

test("Q13: hostile lint input with UNCLOSED tags finishes quickly and returns a list", () => {
  const t0 = performance.now();
  for (const h of ["<a".repeat(200_000), "<!--".repeat(100_000), "<div style=\"color:#".repeat(50_000), "<table width=\"".repeat(50_000), "<img src=\"data:image/png;base64,".repeat(20_000), "<td ".repeat(100_000)]) {
    assert.ok(Array.isArray(lintCompose(h)));
  }
  assert.ok(performance.now() - t0 < 5000, "lint took too long");
});
