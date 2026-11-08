// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/composetext.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import {
  htmlToText, textToHtml, plainSigBlock, findQuoteStart, insertPlainSig, removePlainSig, sigHtmlBlock, insertRichSig, removeRichSig, hasRichSig,
  importSignatureText, replyQuoteHtml, forwardQuoteHtml, pickSignature, sigsForAccount, extractInlineImages, htmlForDraft, lastToken, replaceLastToken,
  formatMailbox, decodeEntities, plainToRichBody,
} from "./composetext.ts";

const sig = (o = {}) => ({ id: "s1", name: "Work", rich: false, text: "Ada\nExample Ltd", html: "", placement: "below", accounts: [], ...o });

test("htmlToText handles blocks, breaks, entities and links", () => {
  assert.equal(htmlToText("<div>Hello &amp; welcome</div><div><br></div><div>Bye</div>"), "Hello & welcome\n\nBye");
  assert.equal(htmlToText("<p>one</p><p>two</p>"), "one\n\ntwo");
  assert.equal(htmlToText("a<br>b<br><br>c"), "a\nb\n\nc");
  assert.equal(htmlToText('<a href="https://x.example/p">site</a> and <a href="https://y.example">https://y.example</a>'), "site (https://x.example/p) and https://y.example");
  assert.equal(htmlToText("<style>p{color:red}</style><script>alert(1)</script><div>x</div>"), "x");
  assert.equal(htmlToText("x&nbsp;&nbsp;y &#65;&#x42;"), "x  y AB");
});

test("htmlToText renders lists and nested quotes", () => {
  assert.equal(htmlToText("<ul><li>a</li><li>b</li></ul><ol><li>c</li><li>d</li></ol>"), "- a\n- b\n\n1. c\n2. d");
  assert.equal(htmlToText("<div>On Mon, Bob wrote:</div><blockquote><div>hi</div><blockquote><div>older</div></blockquote></blockquote>"), "On Mon, Bob wrote:\n> hi\n> > older");
});

test("htmlToText keeps the signature delimiter", () => {
  const t = htmlToText("<div>Body</div><div><br></div><div>-- </div><div>Ada</div>");
  assert.equal(t, "Body\n\n-- \nAda");
});

test("textToHtml escapes, keeps blank lines and turns > runs into blockquotes", () => {
  assert.equal(textToHtml("a <b> & c\n\nd"), "<div>a &lt;b&gt; &amp; c</div><div><br></div><div>d</div>");
  assert.equal(textToHtml("On x wrote:\n> hi\n>> deep"), "<div>On x wrote:</div><blockquote><div>hi</div><blockquote><div>deep</div></blockquote></blockquote>");
  assert.equal(textToHtml("  indented"), "<div>&nbsp;&nbsp;indented</div>");
  assert.ok(!textToHtml("<script>alert(1)</script>").includes("<script"));
});

test("text <-> html round trip is stable for ordinary mail", () => {
  const t = "Hi Bob,\n\nSee you Thursday.\n\nOn Mon, Bob wrote:\n> earlier\n> > oldest";
  assert.equal(htmlToText(textToHtml(t)), t);
});

test("plain signatures use the '-- ' delimiter and respect placement", () => {
  assert.equal(plainSigBlock("Ada\n"), "-- \nAda");
  assert.equal(insertPlainSig("", "Ada", "below"), "\n\n-- \nAda\n");
  assert.equal(insertPlainSig("Thanks", "Ada", "below"), "Thanks\n\n-- \nAda\n");
  const reply = "\n\nOn Mon, Bob wrote:\n> hello\n";
  assert.equal(insertPlainSig(reply, "Ada", "above"), "\n\n-- \nAda\n\nOn Mon, Bob wrote:\n> hello\n");
  assert.equal(insertPlainSig(reply, "Ada", "below"), "\n\nOn Mon, Bob wrote:\n> hello\n\n-- \nAda\n");
  const fwd = "\n\n---------- Forwarded message ----------\nFrom: x\n\nbody\n";
  assert.ok(insertPlainSig(fwd, "Ada", "above").startsWith("\n\n-- \nAda\n\n---------- Forwarded"));
  assert.equal(insertPlainSig("x", "  ", "below"), "x");
});

test("findQuoteStart ignores prose that merely contains 'On'", () => {
  assert.equal(findQuoteStart("On the other hand\nit is fine"), -1);
  assert.equal(findQuoteStart("hi\nOn Mon, Bob wrote:\n> q"), 3);
});

test("removePlainSig is the inverse of insertPlainSig and swaps cleanly", () => {
  for (const t of ["", "Thanks", "\n\nOn Mon, Bob wrote:\n> hello\n"]) {
    for (const p of ["above", "below"]) {
      const withSig = insertPlainSig(t, "Ada\nLtd", p);
      const back = removePlainSig(withSig, "Ada\nLtd");
      assert.ok(!back.includes("Ada"), `${JSON.stringify(t)} ${p}`);
      assert.equal(back.replace(/\s+/g, " ").trim(), t.replace(/\s+/g, " ").trim());
    }
  }
  const swapped = insertPlainSig(removePlainSig(insertPlainSig("Hi", "Ada", "below"), "Ada"), "Bob", "below");
  assert.equal(swapped, "Hi\n\n-- \nBob\n");
  assert.equal(removePlainSig("untouched", "Ada"), "untouched");
});

test("rich signature blocks insert above the quote, below it, and remove cleanly with nested divs", () => {
  const s = sig({ rich: true, html: "<div><b>Ada</b></div><div>Ltd</div>" });
  const block = sigHtmlBlock(s);
  assert.ok(block.startsWith('<div data-mm-sig="s1">') && hasRichSig(block));
  const quote = replyQuoteHtml("On Mon, Bob wrote:", "<p>hi</p>", null);
  const lead = "<div><br></div>";
  const above = insertRichSig(lead + quote, block, "above");
  assert.ok(above.indexOf("data-mm-sig") < above.indexOf("data-mm-quote"));
  const below = insertRichSig(lead + quote, block, "below");
  assert.ok(below.indexOf("data-mm-sig") > below.indexOf("data-mm-quote"));
  assert.equal(removeRichSig(above), lead + quote);
  assert.equal(removeRichSig(below), lead + quote);
  assert.equal(removeRichSig("<div>no sig</div>"), "<div>no sig</div>");
});

test("plain-text signature in rich mode carries the delimiter line", () => {
  const h = sigHtmlBlock(sig({ text: "Ada\n\nLtd <x>" }));
  assert.equal(htmlToText(h), "\n-- \nAda\n\nLtd <x>");
  assert.ok(!h.includes("<x>"));
});

test("reply and forward quote HTML", () => {
  const r = replyQuoteHtml("On Mon, <Bob> wrote:", null, "line1\nline2");
  assert.ok(r.includes("On Mon, &lt;Bob&gt; wrote:") && r.includes("<blockquote><div>line1</div><div>line2</div></blockquote>"));
  assert.ok(!replyQuoteHtml("x", '<p>a</p><img src="cid:abc123" alt="">', null).includes("cid:"));
  const f = forwardQuoteHtml([["From", "Bob <b@x.org>"], ["Date", ""], ["Subject", "Hi"]], null, "body");
  assert.equal(htmlToText(f), "---------- Forwarded message ----------\nFrom: Bob <b@x.org>\nSubject: Hi\n\nbody");
});

test("importSignatureText strips a leading delimiter and builds html", () => {
  const r = importSignatureText("-- \nAda Lovelace\nExample Ltd\n\n");
  assert.equal(r.text, "Ada Lovelace\nExample Ltd");
  assert.equal(r.html, "<div>Ada Lovelace</div><div>Example Ltd</div>");
  assert.equal(importSignatureText("Plain\r\ntext").text, "Plain\ntext");
});

test("pickSignature: identity signature beats the account default; new vs reply defaults; account scoping", () => {
  const a = sig({ id: "a" }), b = sig({ id: "b" }), c = sig({ id: "c", accounts: ["other@y.org"] });
  const defaults = [{ account: "me@x.org", newId: "a", replyId: "b" }];
  const idents = [{ id: "i1", account: "me@x.org", name: "", address: "alias@x.org", replyTo: "", signatureId: "b" }];
  const all = [a, b, c];
  assert.equal(pickSignature(all, defaults, idents, "me@x.org", "new")?.id, "a");
  assert.equal(pickSignature(all, defaults, idents, "me@x.org", "reply")?.id, "b");
  assert.equal(pickSignature(all, defaults, idents, "me@x.org", "new", "ALIAS@x.org")?.id, "b");
  assert.equal(pickSignature(all, [{ account: "me@x.org", newId: "c", replyId: null }], [], "me@x.org", "new"), null, "not offered for this account");
  assert.equal(pickSignature(all, [], [], "me@x.org", "new"), null);
  assert.equal(pickSignature(all, defaults, idents, null, "new"), null);
  assert.deepEqual(sigsForAccount(all, "me@x.org").map((s) => s.id), ["a", "b"]);
});

test("extractInlineImages moves data: pictures out of the html", () => {
  const html = '<div>hi<img src="data:image/png;base64,iVBORw0KGgo=" alt="x"></div><img src="data:image/jpeg;base64,/9j/4AAQ"><img src="data:image/png;base64,iVBORw0KGgo=">';
  const r = extractInlineImages(html);
  assert.equal(r.html, "<div>hi</div>");
  assert.deepEqual(r.images.map((i) => [i.name, i.mime]), [["image-1.png", "image/png"], ["image-2.jpg", "image/jpeg"]]);
  assert.equal(r.images[0].size, 8);
  assert.equal(extractInlineImages('<img src="https://t.example/p.gif">').images.length, 0);
});

test("htmlForDraft drops picture data only when the draft would be too large", () => {
  const small = '<img src="data:image/png;base64,AAAA">';
  assert.equal(htmlForDraft(small), small);
  const big = '<img src="data:image/png;base64,' + "A".repeat(800_000) + '"><div>text</div>';
  const t = htmlForDraft(big);
  assert.ok(t.length < 200 && t.includes("<div>text</div>") && !t.includes("AAAA"));
});

test("recipient token helpers", () => {
  assert.equal(lastToken("a@b.c, Gra"), "Gra");
  assert.equal(lastToken("Gra"), "Gra");
  assert.equal(lastToken("a@b.c, "), "");
  assert.equal(replaceLastToken("a@b.c, Gra", "Grace <g@n.org>"), "a@b.c, Grace <g@n.org>, ");
  assert.equal(replaceLastToken("Gra", "g@n.org"), "g@n.org, ");
  assert.equal(formatMailbox("Doe, John", "j@x.org"), '"Doe, John" <j@x.org>');
  assert.equal(formatMailbox("Ada", "a@x.org"), "Ada <a@x.org>");
  assert.equal(formatMailbox(null, "a@x.org"), "a@x.org");
  assert.equal(formatMailbox('E"vil<>', "a@x.org"), "Evil <a@x.org>");
});

test("decodeEntities leaves unknown entities alone", () => {
  assert.equal(decodeEntities("&bogus; &amp; &#0;"), "&bogus; & &#0;");
});

test("plainToRichBody marks the quote so a rich signature can sit above it", () => {
  const h = plainToRichBody("\n\nOn Mon, Bob wrote:\n> hi\n");
  assert.ok(h.includes('<div data-mm-quote="1"><div>On Mon, Bob wrote:</div><blockquote><div>hi</div></blockquote></div>'), h);
  const withSig = insertRichSig(h, sigHtmlBlock(sig({ text: "Ada" })), "above");
  assert.ok(withSig.indexOf("data-mm-sig") < withSig.indexOf("data-mm-quote"));
  assert.equal(plainToRichBody("just text"), "<div>just text</div>");
});

import { splitAddrs, validAddr } from "./util.ts";
test("splitAddrs keeps commas inside a quoted display name", () => {
  assert.deepEqual(splitAddrs('a@b.c, "Doe, John" <j@x.org>; d@e.f\ng@h.i'), ["a@b.c", '"Doe, John" <j@x.org>', "d@e.f", "g@h.i"]);
  assert.deepEqual(splitAddrs(" , ;"), []);
  assert.ok(validAddr('"Doe, John" <j@x.org>'));
});

test("htmlToText drops text hidden by display:none / opacity:0 / font-size:0 / visibility:hidden (nested too)", () => {
  for (const st of ["display:none", "DISPLAY: none !important", "opacity:0", "opacity: 0.0", "font-size:0", "font-size: 0px", "visibility:hidden", "color:red; display:none"]) {
    const t = htmlToText(`<div>seen <span style="${st}">SECRET <span>inner</span> <b>x</b></span>after</div>`);
    assert.equal(t, "seen after", st);
  }
  assert.equal(htmlToText('<div>a</div><div style="display:none"><div>x</div>y</div><div>b</div>'), "a\nb");
  assert.equal(htmlToText('<span style="opacity:0.5;font-size:10px">ok</span>'), "ok");
  assert.equal(htmlToText('<p>x<br style="display:none">y</p>'), "xy");
});

test("htmlToText renders tables as ' | ' rows, header underline, and one-cell tables as plain content", () => {
  const t = htmlToText('<div>before</div><table><thead><tr><th>Name</th><th>Qty</th></tr></thead><tbody><tr><td>Apples</td><td align="right">3</td></tr><tr><td>Pears &amp; figs</td><td><b>12</b></td></tr></tbody></table><div>after</div>');
  assert.equal(t, "before\nName | Qty\n---- | ---\nApples | 3\nPears & figs | 12\n\nafter");
  assert.equal(htmlToText("<table><tr><td>a</td><td>b</td><td>c</td></tr></table>"), "a | b | c");
  assert.equal(htmlToText('<table><tr><td colspan="2">wide</td><td>c</td></tr><tr><td>a</td><td>b</td><td>c2</td></tr></table>'), "wide |  | c\na | b | c2");
  assert.equal(htmlToText('<table><tr><td><a href="https://e.example/x">Go now</a></td></tr></table>'), "Go now (https://e.example/x)");
  assert.equal(htmlToText("<table><tr><td>one</td></tr></table><div>two</div>"), "one\n\ntwo");
});

test("htmlToText tables: hidden cells and rows never reach the text, quoted tables get '> ', nesting is bounded", () => {
  assert.equal(htmlToText('<table><tr><td>a</td><td style="display:none">SECRET</td></tr><tr style="visibility:hidden"><td>HID</td></tr><tr><td>b</td><td>c</td></tr></table>'), "a\nb | c");
  assert.equal(htmlToText("<blockquote><table><tr><td>a</td><td>b</td></tr></table></blockquote>"), "> a | b");
  assert.equal(htmlToText("<table><tr><td>x<table><tr><td>in1</td><td>in2</td></tr></table></td><td>y</td></tr></table>"), "x in1 | in2 | y");
  const deep = "<table><tr><td>".repeat(3000) + "bottom" + "</td></tr></table>".repeat(3000);
  assert.ok(htmlToText(deep).includes("bottom"));
  assert.equal(htmlToText("<table></table><p>x</p>"), "x");
  assert.equal(htmlToText("<table><tr><td>list<ul><li>a</li><li>b</li></ul></td><td>z</td></tr></table>"), "list - a - b | z");
});

// ---- review fixes ----

test("Q02: size / opacity keywords stay in the plain text; numeric zero spellings are hidden", () => {
  for (const kw of ["font-size: small", "font-size:medium", "font-size:large", "font-size:smaller", "font-size:inherit", "opacity:inherit", "font-size:12px", "opacity:0.5"]) {
    assert.equal(htmlToText(`<div>seen <span style="${kw}">KEPT</span></div>`), "seen KEPT", kw);
    assert.equal(htmlToText(`<table><tr><td style="${kw}">KEPT</td><td>b</td></tr></table>`), "KEPT | b", kw);
  }
  for (const st of ["opacity:0", "opacity:0e0", "opacity:-1", "opacity:+0", "opacity:0.001", "font-size:0e0px", "font-size:+0px", "font-size:0.01px", "FONT-SIZE : 0 !IMPORTANT", "font-size:0pt", "color:transparent", "color:rgba(0,0,0,0)", "display&colon;none", "opacity:&#48;"]) {
    assert.equal(htmlToText(`<div>seen <span style="${st}">SECRET</span>after</div>`), "seen after", st);
  }
});

test("Q07: tables past the nesting cap keep their cells and rows apart; captions are rendered; depth cap is 6", () => {
  const nest = (n, inner) => "<table><tr><td>".repeat(n) + inner + "</td></tr></table>".repeat(n);
  const inner = "<table><tr><td>x</td><td>y</td></tr><tr><td>z</td><td>w</td></tr></table>";
  const t7 = htmlToText(nest(9, inner));
  assert.match(t7, /x\s+y/);
  assert.match(t7, /z\s+w/);
  assert.doesNotMatch(t7, /xy|yz|zw/);
  // Up to 6 nested tables (5 wrappers + the inner one) are laid out with ' | '; a 7th is unwrapped (and still readable, see above).
  assert.equal(htmlToText(nest(4, inner)).includes("x | y"), true);
  assert.equal(htmlToText(nest(5, inner)).includes("x | y"), true);
  assert.equal(htmlToText(nest(6, inner)).includes("x | y"), false);
  // Caption.
  assert.equal(htmlToText("<table><caption>Totals &amp; more</caption><tr><td>a</td><td>b</td></tr></table>"), "Totals & more\na | b");
  assert.equal(htmlToText('<table><caption style="display:none">SECRET</caption><tr><td>a</td><td>b</td></tr></table>'), "a | b");
  assert.equal(htmlToText("<table><caption>Only</caption></table>"), "Only");
  // Still stack safe.
  assert.ok(htmlToText("<table><tr><td>".repeat(5000) + "bottom" + "</td></tr></table>".repeat(5000)).includes("bottom"));
});

test("Q09: prototype-ish entity names stay literal in decodeEntities and htmlToText", () => {
  for (const e of ["constructor", "__proto__", "toString", "hasOwnProperty"]) {
    assert.equal(decodeEntities(`&${e};`), `&${e};`);
    assert.equal(htmlToText(`<p>a &${e}; b</p>`), `a &${e}; b`);
  }
  assert.equal(decodeEntities("&amp; &NDASH; &#65; &#x42;"), "& – A B");
});

import { safeInitialHtml, QUOTE_OPEN } from "./composetext.ts";

test("safeInitialHtml: sanitises, keeps sig/quote markers, falls back to text on failure", async () => {
  // fake sanitiser: drops onerror and every data-* attribute, keeps lang (like the backend)
  const fake = async (h: string) => h.replace(/ onerror="[^"]*"/g, "").replace(/ data-[a-z-]+="[^"]*"/g, "");
  const src = `<div>hi <img src="x" onerror="alert(1)"></div>${QUOTE_OPEN}q</div><div data-mm-sig="s1"><div>-- </div></div>`;
  const out = await safeInitialHtml(src, fake);
  assert.ok(!out.includes("onerror"));
  assert.ok(out.includes(QUOTE_OPEN));
  assert.ok(out.includes('<div data-mm-sig="s1">'));
  assert.ok(!out.includes("lang="));
  const bad = await safeInitialHtml("<p>a</p><img src=x onerror=alert(1)>", async () => { throw new Error("nope"); });
  assert.ok(!bad.includes("onerror") && !bad.includes("<img"));
  assert.ok(bad.includes("a"));
  assert.equal(await safeInitialHtml("", fake), "");
});
