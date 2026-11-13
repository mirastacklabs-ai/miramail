// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/linkgate.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { gateOf, gateFor, CheckBook, holdHtml, holdLinks, releaseLinks, markLinks, OncePerKey, HELD_ATTR, HOLD_STYLE, msgKey, countHeld } from "./linkgate.ts";
import { buildSrcdoc } from "./util.ts";

// A tiny DOM stand-in: elements with a tag and attributes; the "document" answers the selectors linkgate uses.
function el(tagName, attrs) {
  const a = new Map(Object.entries(attrs));
  return { tagName: tagName.toUpperCase(), a, getAttribute: (n) => (a.has(n) ? a.get(n) : null), setAttribute: (n, v) => a.set(n, v), removeAttribute: (n) => a.delete(n) };
}
function doc(els) {
  const style = { removed: false, remove() { this.removed = true; } };
  const isLink = (e) => e.tagName === "A" || e.tagName === "AREA";
  return {
    style,
    querySelectorAll(sel) {
      if (sel === "[href]") return els.filter((e) => e.a.has("href"));
      if (sel === "a[href], area[href]") return els.filter((e) => isLink(e) && e.a.has("href"));
      if (sel === `[${HELD_ATTR}]`) return els.filter((e) => e.a.has(HELD_ATTR));
      throw new Error(`unexpected selector ${sel}`);
    },
    querySelector(sel) { assert.equal(sel, "style[data-mm-hold]"); return style.removed ? null : style; },
  };
}

test("gate: ready only when both checks succeeded; any failure (once nothing is running) keeps links inert", () => {
  assert.equal(gateOf("pending", "pending"), "pending");
  assert.equal(gateOf("ok", "pending"), "pending");
  assert.equal(gateOf("failed", "pending"), "pending");
  assert.equal(gateOf("ok", "ok"), "ready");
  assert.equal(gateOf("ok", "failed"), "failed");
  assert.equal(gateOf("failed", "ok"), "failed");
  assert.notEqual(msgKey("a@x", "INBOX", 1, 1), msgKey("b@x", "INBOX", 1, 1));
  assert.notEqual(msgKey("a@x", "INBOX", 1, 1), msgKey("a@x", "Sent", 1, 1));
  assert.notEqual(msgKey("a@x", "INBOX", 1, 1), msgKey("a@x", "INBOX", 2, 1), "UIDVALIDITY is part of the key");
});

// Hostile input, deliberately NOT what the sanitizer emits (it already strips most of this; see mailcore
// sanitize.rs `reader_link_fixture`): the hold pass must not depend on it.
const HOSTILE = [
  `<A HREF="https://a.example/1" TARGET="_blank" style="pointer-events:auto!important">up</A>`,
  `<map><AREA Href='https://a.example/2' target=_top shape=rect></map>`,
  `<a href=https://a.example/3 tabindex=1 accesskey=x ping="https://p.example">unquoted</a>`,
  `<a/href="https://a.example/4">slash</a>`,
  `<a data-mm-href="https://evil.example" href="https://a.example/5">forged marker</a>`,
  `<svg><a xlink:href="https://a.example/6">s</a></svg>`,
  `<form action="https://a.example/7"><button formaction="https://a.example/8">b</button></form>`,
  `<meta http-equiv="refresh" content="0;url=https://a.example/9"><base href="https://a.example/10/"><link href="https://a.example/11">`,
  `<p title='x" <a href="https://a.example/12">'>quoted gt</p>`,
  `<!-- <a href="https://a.example/13"> --><style>a[href]{x:"<a href='https://a.example/14'>"}</style>`,
  `<div href="https://a.example/15">not a link</div><a href>empty</a><a hReF = "https://a.example/16" >spaced</a>`,
].join("");

test("holdHtml: no live href / navigation attribute is left in the string, whatever its case or quoting", () => {
  const { html, links } = holdHtml(HOSTILE);
  // Every a/area href became data-mm-href; nothing else that navigates survives as an attribute.
  // Scan the markup only (comments, raw text and the one quoted value that contains "<a href" are not tags).
  const scan = html.replace(/<!--[\s\S]*?-->/g, "").replace(/<style[\s\S]*?<\/style>/gi, "").replace(/title='[^']*'/g, "");
  const tags = scan.match(/<[a-z][^>]*>/gi) ?? [];
  assert.ok(tags.length >= 12, `scanned ${tags.length} tags`);
  for (const t of tags) {
    assert.ok(!/\s(href|xlink:href|action|formaction|target|ping|tabindex|accesskey)\s*=/i.test(t), `live attribute in ${t}`);
    assert.ok(!/\shref(\s|>|$)/i.test(t), `bare href in ${t}`);
  }
  assert.ok(!/<meta|<base|<link\b/i.test(html), "meta / base / link dropped");
  assert.ok(!html.includes("evil.example"), "the sender's own marker is dropped");
  assert.ok(html.includes(`<A ${HELD_ATTR}="https://a.example/1"`));
  assert.ok(html.includes(`<AREA ${HELD_ATTR}='https://a.example/2'`));
  assert.ok(html.includes(`${HELD_ATTR}=https://a.example/3`));
  assert.ok(html.includes(`${HELD_ATTR}="https://a.example/4"`));
  assert.ok(html.includes(`${HELD_ATTR}="https://a.example/16"`));
  assert.ok(html.includes(`title='x" <a href="https://a.example/12">'`), "attribute values are copied, not parsed");
  assert.ok(html.includes(`<!-- <a href="https://a.example/13"> -->`), "comments are copied");
  assert.ok(html.includes(`{x:"<a href='https://a.example/14'>"}</style>`), "raw text is copied");
  assert.ok(html.includes(`<div>not a link</div>`), "href on a non-link element is dropped, not held");
  assert.ok(!/<svg|<form action|<\/svg/i.test(html), "svg dropped with its content");
  assert.equal(links, 7, "a/area links held: 1, 2, 3, 4, 5, the empty one, 16");
  assert.equal(holdHtml(HOSTILE), holdHtml(HOSTILE), "memoised per body");
  assert.equal(holdHtml("<p>no links</p>").links, 0);
  assert.equal(holdHtml("a < b and <3").html, "a < b and <3", "text '<' is left alone");
});

test("comments end by the HTML rules: <!--> and <!---> close at once, --!> closes", () => {
  for (const c of ["<!-->", "<!--->", "<!-- x --!>", "<!-- x -->"]) {
    const { html, links } = holdHtml(`${c}<a href="https://a.example/c">x</a>-->`);
    assert.equal(links, 1, `${c}: the link after the comment is held`);
    assert.ok(!/\shref=/i.test(html) && html.includes(`${HELD_ATTR}="https://a.example/c"`), html);
  }
});

test("noscript content is markup (the frame runs no scripts): its links are held", () => {
  const { html, links } = holdHtml(`<noscript><a href="https://a.example/n">x</a></noscript>`);
  assert.equal(links, 1);
  assert.ok(!/\shref=/i.test(html), html);
});

test("foreign content and SMIL are dropped with everything inside", () => {
  const cases = [
    `<svg><a href="https://a.example/s1"><text>x</text></a></svg>`,
    `<SVG><svg><a xlink:href="https://a.example/s2">x</a></svg><a href="https://a.example/s3">y</a></SVG>`,
    `<math><mi xlink:href="https://a.example/m1">x</mi></math>`,
    `<svg><![CDATA[</svg>]]><a href="https://a.example/s4">x</a></svg>`,
    `<svg><!-- </svg> --><a href="https://a.example/s5">x</a></svg>`,
  ];
  for (const c of cases) {
    const { html, links } = holdHtml(`${c}<p>after</p>`);
    assert.equal(links, 0, c);
    assert.equal(html, "<p>after</p>", c);
  }
  const smil = holdHtml(`<a href="https://a.example/1"><set attributeName="href" to="https://evil.example"/><animate attributeName="href" values="https://evil.example"></animate><animateTransform/><animateMotion/>x</a>`);
  assert.ok(!/set|animate|evil/i.test(smil.html), smil.html);
  assert.equal(smil.links, 1);
});

test("end-tag attributes are parsed like start tags and dropped (a quoted '>' cannot end them early)", () => {
  const { html, links } = holdHtml(`<a href="https://a.example/1">x</a title='<a href="https://a.example/2">'><b>y</b>`);
  assert.equal(links, 1);
  assert.equal(html, `<a ${HELD_ATTR}="https://a.example/1">x</a><b>y</b>`);
});

test("the Reader srcdoc carries the held body: CSP first, hold style, then the message without live links", () => {
  const s = buildSrcdoc(`<A HREF="https://x.example">x</A>`, false, "", HOLD_STYLE, (h) => holdHtml(h).html);
  assert.ok(s.indexOf("default-src 'none'") < s.indexOf("data-mm-hold"));
  assert.ok(s.indexOf("data-mm-hold") < s.indexOf("<A "));
  assert.ok(!/HREF=/.test(s) && s.includes(`${HELD_ATTR}="https://x.example"`));
});

test("frame DOM: holdLinks removes any href left; release restores a/area under the markLinks rules", () => {
  const web = el("a", { href: "https://example.org/x", target: "_top" });
  const mail = el("a", { [HELD_ATTR]: "mailto:ann@example.org" });
  const anchor = el("a", { [HELD_ATTR]: "#sec" });
  const js = el("area", { [HELD_ATTR]: "javascript:alert(1)" });
  const div = el("div", { href: "https://example.org/d" });
  const d = doc([web, mail, anchor, js, div]);
  assert.equal(holdLinks(d), 2);
  assert.equal(countHeld(d), 4, "the frame's own count: a/area only");
  assert.equal(web.getAttribute("href"), null);
  assert.equal(web.getAttribute("target"), null);
  assert.equal(web.getAttribute(HELD_ATTR), "https://example.org/x");
  assert.equal(div.getAttribute("href"), null);
  assert.equal(div.getAttribute(HELD_ATTR), null, "non-link elements are never held (never restored)");
  assert.equal(releaseLinks(d), 4);
  assert.ok(d.style.removed, "the hold style is gone");
  assert.equal(web.getAttribute("href"), "https://example.org/x");
  assert.equal(web.getAttribute("target"), "_blank");
  assert.equal(web.getAttribute("rel"), "noopener noreferrer");
  assert.equal(mail.getAttribute("target"), "_blank");
  assert.equal(anchor.getAttribute("href"), "#sec");
  assert.equal(anchor.getAttribute("target"), null, "an anchor never becomes a new-window action");
  assert.equal(js.getAttribute("href"), null, "ignored kinds lose their href");
  for (const e of [web, mail, anchor, js]) assert.equal(e.getAttribute(HELD_ATTR), null);
  assert.equal(releaseLinks(d), 0, "idempotent");
});

test("a marker on a non-link element never becomes an href", () => {
  const p = el("p", { [HELD_ATTR]: "https://evil.example" });
  const d = doc([p]);
  assert.equal(releaseLinks(d), 0);
  assert.equal(p.getAttribute("href"), null);
  assert.equal(p.getAttribute(HELD_ATTR), null);
});

test("markLinks alone keeps the existing rules", () => {
  const web = el("a", { href: "https://example.org" });
  const odd = el("a", { href: "file:///etc/passwd", target: "_blank" });
  markLinks(doc([web, odd]));
  assert.equal(web.getAttribute("target"), "_blank");
  assert.equal(odd.getAttribute("href"), null);
  assert.equal(odd.getAttribute("target"), null);
});

test("link extraction runs once per message body (the per-frame loop seen on the iPhone cannot recur)", () => {
  const once = new OncePerKey();
  const html = '<a href="https://x">x</a>';
  for (let frame = 0; frame < 600; frame++) {
    if (once.shouldRun(html)) once.finish(html);
  }
  assert.equal(once.runs, 1);
  assert.ok(once.shouldRun("<p>other</p>"));
  // A job cancelled before it finished may run again for the same key.
  const c = new OncePerKey();
  assert.ok(c.shouldRun(html) && c.shouldRun(html));
});

// Source guards. The loop's root cause: Svelte subscribes an effect to every $state it reads, so
// `linksPending = …; if (!linksPending) return;` re-ran the effect each time the deferred extraction set
// `linksPending = false` (proven with the compiled runtime: 55 runs/s before, 1 after).
test("Reader: the link-extraction effect reads only derived body state and writes outside tracking", () => {
  const src = readFileSync(new URL("../components/Reader.svelte", import.meta.url), "utf8");
  assert.ok(!/if \(!linksPending\)/.test(src), "the effect reads linksPending again");
  const m = /\/\/ links-effect:start([\s\S]*?)\/\/ links-effect:end/.exec(src);
  assert.ok(m, "marked link-extraction effect");
  assert.match(m[1], /untrack\(/, "writes happen inside untrack");
  assert.match(m[1], /once\.shouldRun\(/, "guarded once per body");
  assert.ok(!/\.includes\("href"\)/.test(src), "links are counted by the hold pass, not a case-sensitive text search");
  assert.equal((src.match(/perfLog\(`links_/g) ?? []).length, 1, "one perf line, emitted per finished extraction");
  assert.match(src, /buildSrcdoc\(b\.html, [\s\S]*?HOLD_STYLE, holdBody\)/, "the srcdoc is always built from the held body");
  assert.equal((src.match(/\{@render retryChecks\(\)\}/g) ?? []).length, 1, "one retry control");
});

test("mail://link is ignored until the open message's links are enabled", () => {
  const src = readFileSync(new URL("./links.ts", import.meta.url), "utf8");
  const fn = /export function handleMailLink[\s\S]*?\n}/.exec(src)?.[0] ?? "";
  assert.match(fn, /if \(!app\.readyKey \|\| performance\.now\(\) - app\.openT0 < STALE_LINK_MS\) return;/);
  assert.ok(fn.indexOf("readyKey") < fn.indexOf("linkAction"), "checked first");
  const ps = readFileSync(new URL("../components/PrintSheet.svelte", import.meta.url), "utf8");
  assert.match(ps, /buildSrcdoc\(b\.html, [\s\S]*?holdHtml\(h\)\.html\)/, "the print copy is held too");
});

test("race: checks finished for INBOX/5 never enable links of Archive/5 (or a UIDVALIDITY change)", () => {
  const book = new CheckBook();
  const inbox = msgKey("a@x", "INBOX", 9, 5), archive = msgKey("a@x", "Archive", 9, 5), renumbered = msgKey("a@x", "INBOX", 10, 5);
  // Open INBOX/5; the body arrives, checks start.
  const t1 = book.reset();
  assert.ok(book.begin(t1, inbox));
  // The user switches folder (reset) and opens Archive/5 before the INBOX checks return.
  const t2 = book.reset();
  assert.equal(book.done(t1, inbox, "analysis", "ok"), false, "stale open: ignored");
  assert.equal(book.done(t1, inbox, "safety", "ok"), false);
  assert.equal(book.gate(archive), "pending");
  // Archive's body arrives; its own checks finish.
  assert.ok(book.begin(t2, archive));
  assert.equal(book.gate(archive), "pending");
  book.done(t2, archive, "analysis", "ok");
  book.done(t2, archive, "safety", "ok");
  assert.equal(book.gate(archive), "ready");
  assert.equal(book.gate(inbox), "pending", "never ready for a message that is not the checked one");
  assert.equal(book.gate(renumbered), "pending", "same folder and UID under another UIDVALIDITY: another message");
  assert.equal(book.gate(""), "pending", "nothing shown");
  // A result for the right token but another message is refused too.
  assert.equal(book.done(t2, inbox, "safety", "failed"), false);
  assert.equal(book.gate(archive), "ready");
  assert.equal(gateFor(archive, inbox, "ok", "ok"), "pending");
});

test("state.svelte.ts follows the model: captured account/folder, token bumps on switches, key-checked results", () => {
  const src = readFileSync(new URL("./state.svelte.ts", import.meta.url), "utf8");
  assert.match(src, /api\.getMessage\(acc, fol, uid,/, "getMessage uses the captured account and folder");
  assert.ok(!/api\.getMessage\(this\.account/.test(src));
  for (const fn of ["async selectAccount(", "async selectFolder("]) {
    const body = src.slice(src.indexOf(fn), src.indexOf(fn) + 400);
    assert.match(body, /this\.bodyToken\+\+/, `${fn} voids an open in flight`);
  }
  assert.match(src, /get linkGate\(\): GateState \{ return gateFor\(this\.checksKey, bodyKeyOf\(this\.body\?\.summary\)/);
  assert.equal((src.match(/const mine = \(\) => token === this\.bodyToken && key === this\.checksKey;/g) ?? []).length, 2, "both checks key-checked");
  assert.match(src, /api\.messageAnalysis\(m\.account, m\.folder, m\.uid, allow, skip, m\.uidvalidity\)/);
  assert.match(src, /api\.safetyReport\(m\.account, m\.folder, m\.uid, m\.uidvalidity\)/);
});
