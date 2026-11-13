// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/*.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { buildPrintHtml, stripRemote } from "./printdoc.ts";
import type { MessageBody } from "./types";

const body = (over: Record<string, unknown> = {}) =>
  ({
    summary: { subject: "Hi <b>", from: { name: "A", address: "a@x.org" }, to: [{ name: "", address: "b@y.org" }], date: 1700000000 },
    cc: [],
    attachments: [{ name: "r.pdf" }],
    html: '<p>Hello <a href="https://evil.example/x">link</a></p><img src="https://t.example/p.gif"><img src="data:image/png;base64,AAAA">',
    text: "plain & text",
    ...over,
  }) as unknown as MessageBody;

test("header block is escaped and lists attachments", () => {
  const h = buildPrintHtml(body());
  assert.match(h, /<h1 class="mm">Hi &lt;b&gt;<\/h1>/);
  assert.match(h, /From<\/th><td>A &lt;a@x\.org&gt;/);
  assert.match(h, /Attached<\/th><td>r\.pdf/);
});

test("remote images are always stripped; data images stay", () => {
  const off = buildPrintHtml(body());
  assert.doesNotMatch(off, /t\.example/);
  assert.match(off, /data:image\/png/);
  assert.match(off, /img-src data:;/);
});

test("links are inert", () => {
  const h = buildPrintHtml(body());
  assert.doesNotMatch(h, /\shref=/i);
});

test("plain text is escaped inside a pre", () => {
  const h = buildPrintHtml(body({ html: null }));
  assert.match(h, /<pre class="mm">plain &amp; text<\/pre>/);
  assert.match(buildPrintHtml(body(), true), /<pre class="mm">/);
});

test("stripRemote handles css urls, imports and quoting styles", () => {
  assert.equal(stripRemote("<div style=\"background:url(https://a/b.png)\">"), '<div style="background:url()">');
  assert.doesNotMatch(stripRemote("<style>@import url(http://a/x.css);p{}</style>"), /import/);
  assert.doesNotMatch(stripRemote("<img src='//a/b.png'><img SRC=http://a/b>"), /a\/b/);
  assert.match(stripRemote('<img src="cid:abc">'), /cid:abc/);
});
