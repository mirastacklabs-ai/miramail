// @ts-nocheck -- run with: node --test src/lib/util.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { replyDraft, forwardDraft, reSubject, splitAddrs, validAddr, fmtSize, buildSrcdoc, addrLabel } from "./util.ts";

const ME = "me@example.org";
const A = (address, name = null) => ({ address, name });
function body({ from = A("ann@example.net", "Ann"), to = [A(ME)], cc = [], subject = "Plan", text = "line 1\r\n> quoted\r\nline 3", messageId = "m1@example.net", references = null, date = 1_750_000_000 } = {}) {
  return { summary: { from, to, subject, messageId, date, folder: "INBOX", uid: 7 }, text, html: null, blockedRemoteCount: 0, images: [], attachments: [], cc, references, invite: null };
}

test("reply goes to the sender only; reply-all adds the other recipients and never me", () => {
  const b = body({ to: [A(ME), A("bob@example.net")], cc: [A("cy@example.net"), A("ME@Example.org")] });
  const r = replyDraft(b, false, ME);
  assert.deepEqual([r.to, r.cc, r.bcc], [["ann@example.net"], [], []]);
  const all = replyDraft(b, true, ME);
  assert.deepEqual(all.to, ["ann@example.net"]);
  assert.deepEqual(all.cc, ["bob@example.net", "cy@example.net"]);
  assert.deepEqual(all.inReplyTo, { messageId: "m1@example.net", references: null });
});

test("reply-all does not list the sender twice or duplicate an address that differs only in case", () => {
  const b = body({ from: A("Ann@Example.net"), to: [A("ann@example.net"), A("bob@example.net")], cc: [A("BOB@example.net"), A("dee@example.net")] });
  const all = replyDraft(b, true, ME);
  assert.deepEqual(all.to, ["Ann@Example.net"]);
  assert.deepEqual(all.cc, ["bob@example.net", "dee@example.net"]);
});

test("replying to a message I sent addresses the people I wrote to, not me", () => {
  const b = body({ from: A("Me@Example.org"), to: [A("bob@example.net"), A("cy@example.net")], cc: [A("dee@example.net")] });
  assert.deepEqual(replyDraft(b, false, ME).to, ["bob@example.net", "cy@example.net"]);
  const all = replyDraft(b, true, ME);
  assert.deepEqual(all.to, ["bob@example.net", "cy@example.net"]);
  assert.deepEqual(all.cc, ["dee@example.net"]);
  // A note to myself (only me as recipient) still answers me rather than producing an empty To.
  assert.deepEqual(replyDraft(body({ from: A(ME), to: [A(ME)] }), false, ME).to, [ME]);
});

test("reply quotes every line, keeps nested quotes, and handles missing sender, date and text", () => {
  const r = replyDraft(body(), false, ME);
  assert.ok(r.text.endsWith("wrote:\n> line 1\n>> quoted\n> line 3\n"), JSON.stringify(r.text));
  assert.ok(!r.text.includes("\r"));
  const bare = replyDraft(body({ from: null, date: null, text: null, messageId: null }), true, ME);
  assert.deepEqual(bare.to, []);
  assert.equal(bare.inReplyTo, null);
  assert.ok(bare.text.includes("On an earlier date, someone wrote:"), bare.text);
});

test("subjects: one Re:/Fwd: prefix, never stacked, whitespace trimmed", () => {
  assert.equal(reSubject("Plan"), "Re: Plan");
  assert.equal(reSubject("  RE: Plan "), "RE: Plan");
  assert.equal(reSubject("re:Plan"), "re:Plan");
  assert.equal(reSubject(""), "Re:");
  assert.equal(forwardDraft(body({ subject: "Plan" })).subject, "Fwd: Plan");
  assert.equal(forwardDraft(body({ subject: " FW: Plan" })).subject, "FW: Plan");
  assert.equal(forwardDraft(body({ subject: "fwd: Plan" })).subject, "fwd: Plan");
});

test("forward has no recipients, no threading and carries the original header block", () => {
  const f = forwardDraft(body({ to: [A("bob@example.net", "Bob")] }));
  assert.deepEqual([f.to, f.cc, f.bcc, f.inReplyTo], [[], [], [], null]);
  assert.ok(f.text.includes("From: Ann <ann@example.net>") && f.text.includes("To: Bob <bob@example.net>") && f.text.includes("Subject: Plan"), f.text);
  assert.ok(!forwardDraft(body({ from: null })).text.includes("null"));
});

test("splitAddrs / validAddr", () => {
  assert.deepEqual(splitAddrs("a@x.org, b@x.org;c@x.org\nd@x.org,,  ,"), ["a@x.org", "b@x.org", "c@x.org", "d@x.org"]);
  assert.deepEqual(splitAddrs(""), []);
  assert.deepEqual(splitAddrs('"Doe, John" <j@x.org>'), ['"Doe, John" <j@x.org>']);
  for (const ok of ["a@x.org", "Ann <a@x.org>", '"Doe, J" <j@x.co.uk>', "a+tag@sub.x.org"]) assert.ok(validAddr(ok), ok);
  for (const bad of ["", "a", "a@x", "a @x.org", "<a@x.org", "a@x.org>", "a@@x.org", "Ann <a@x>"]) assert.ok(!validAddr(bad), bad);
});

test("fmtSize boundaries", () => {
  assert.equal(fmtSize(0), "0 B");
  assert.equal(fmtSize(1023), "1023 B");
  assert.equal(fmtSize(1024), "1 KB");
  assert.equal(fmtSize(1048575), "1024 KB");
  assert.equal(fmtSize(1048576), "1.0 MB");
});

test("buildSrcdoc: CSP blocks everything but inline styles and data: images; https images only when allowed", () => {
  const off = buildSrcdoc("<p>x</p>", false);
  assert.ok(off.startsWith('<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; img-src data:; style-src \'unsafe-inline\'">'), off);
  assert.ok(!off.includes("https:") && !off.includes("script-src"));
  const on = buildSrcdoc("<p>x</p>", true);
  assert.ok(on.includes("img-src data: https:;") && !on.includes("http:;"), on);
  // The CSP comes before the message, so markup in the message cannot precede it.
  const hostile = buildSrcdoc("<meta http-equiv=\"Content-Security-Policy\" content=\"img-src *\">", false);
  assert.ok(hostile.indexOf("default-src 'none'") < hostile.indexOf("img-src *"));
});

test("addrLabel", () => {
  assert.equal(addrLabel(null), "Unknown sender");
  assert.equal(addrLabel(A("a@x.org", "  ")), "a@x.org");
  assert.equal(addrLabel(A("a@x.org", " Ann ")), "Ann");
});
