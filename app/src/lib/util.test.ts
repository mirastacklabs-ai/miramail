// @ts-nocheck -- run with: node --test src/lib/util.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { linkAction, replyDraft, forwardDraft, reSubject, splitAddrs, validAddr, fmtSize, buildSrcdoc, addrLabel } from "./util.ts";

const ME = "me@example.org";
const A = (address, name = null) => ({ address, name });
function body({ from = A("ann@example.net", "Ann"), to = [A(ME)], cc = [], subject = "Plan", text = "line 1\r\n> quoted\r\nline 3", messageId = "m1@example.net", references = null, date = 1_750_000_000, replyTo = [] } = {}) {
  return { summary: { from, to, subject, messageId, date, folder: "INBOX", uid: 7 }, text, html: null, blockedRemoteCount: 0, images: [], attachments: [], cc, replyTo, references, invite: null };
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

test("Reply-To is honoured by Reply and Reply-all and a notice names both addresses", () => {
  const b = body({ to: [A(ME), A("bob@example.net")], replyTo: [A("help@desk.example", "Desk")] });
  const r = replyDraft(b, false, ME);
  assert.deepEqual(r.to, ["help@desk.example"]);
  assert.equal(r.notice, "Replies go to help@desk.example, not the sender (ann@example.net).");
  const all = replyDraft(b, true, ME);
  assert.deepEqual(all.to, ["help@desk.example"]);
  assert.deepEqual(all.cc, ["bob@example.net"]);
  assert.ok(all.notice);
});

test("no notice when Reply-To is the sender (any case) or absent", () => {
  assert.equal(replyDraft(body(), false, ME).notice, null);
  const same = replyDraft(body({ replyTo: [A("ANN@example.net")] }), false, ME);
  assert.deepEqual(same.to, ["ANN@example.net"]);
  assert.equal(same.notice, null);
});

test("multiple Reply-To addresses all receive the reply; garbage Reply-To is ignored", () => {
  const m = replyDraft(body({ replyTo: [A("a@x.org"), A("b@x.org"), A("A@X.org")] }), false, ME);
  assert.deepEqual(m.to, ["a@x.org", "b@x.org"]);
  assert.match(m.notice, /a@x.org, b@x.org/);
  const g = replyDraft(body({ replyTo: [A("not an address"), A(""), A("<>")] }), false, ME);
  assert.deepEqual(g.to, ["ann@example.net"]);
  assert.equal(g.notice, null);
  assert.deepEqual(replyDraft(body({ replyTo: undefined }), false, ME).to, ["ann@example.net"]);
});

test("Reply-To that is only me falls back to the sender; own sent mail ignores Reply-To; missing From is flagged", () => {
  const meOnly = replyDraft(body({ replyTo: [A(ME)] }), false, ME);
  assert.deepEqual(meOnly.to, ["ann@example.net"]);
  assert.equal(meOnly.notice, null);
  const sent = replyDraft(body({ from: A(ME), to: [A("bob@example.net")], replyTo: [A("evil@x.org")] }), false, ME);
  assert.deepEqual(sent.to, ["bob@example.net"]);
  assert.equal(sent.notice, null);
  const nofrom = replyDraft(body({ from: null, replyTo: [A("r@x.org")] }), false, ME);
  assert.deepEqual(nofrom.to, ["r@x.org"]);
  assert.match(nofrom.notice, /unknown/);
});

test("reSubject / forward subject recognise RE :, Re[2]:, Re(2): and do not stack another prefix", () => {
  for (const v of ["RE : Plan", "Re[2]: Plan", "Re(2): Plan", "re:Plan"]) assert.equal(reSubject(v), v, v);
  assert.equal(reSubject("Plan"), "Re: Plan");
  assert.equal(reSubject("Report: Plan"), "Re: Report: Plan");
  for (const v of ["Fwd : Plan", "FW: Plan", "Fwd[2]: Plan"]) assert.equal(forwardDraft(body({ subject: v })).subject, v, v);
});

test("saved identities count as me: replying to mail sent from an alias goes to its recipients (G2)", () => {
  const alias = "sales@example.org";
  const b = body({ from: A("Sales@Example.org"), to: [A("bob@example.net")], cc: [A(alias), A("dee@example.net")] });
  assert.deepEqual(replyDraft(b, false, ME, [alias]).to, ["bob@example.net"]);
  assert.deepEqual(replyDraft(b, true, ME, [alias]).cc, ["dee@example.net"]);
  // Without the alias list it is treated as a stranger (the old behaviour).
  assert.deepEqual(replyDraft(b, false, ME).to, ["Sales@Example.org"]);
  // Reply-all from a stranger never lists my alias.
  assert.deepEqual(replyDraft(body({ to: [A(ME), A(alias)] }), true, ME, [alias]).cc, []);
});

test("linkAction: web, mail, anchor, everything else ignored", () => {
  assert.deepEqual(linkAction("https://example.org/a?b=1#c"), { kind: "web", url: "https://example.org/a?b=1#c" });
  assert.deepEqual(linkAction(" http://example.org "), { kind: "web", url: "http://example.org/" });
  assert.deepEqual(linkAction("#top"), { kind: "anchor" });
  assert.deepEqual(linkAction("mailto:a@b.co?subject=Hi%20there"), { kind: "mail", to: ["a@b.co"], subject: "Hi there" });
  assert.deepEqual(linkAction("mailto:a@b.co,c@d.org"), { kind: "mail", to: ["a@b.co", "c@d.org"], subject: "" });
  for (const bad of ["", null, undefined, "javascript:alert(1)", "data:text/html,x", "file:///etc/passwd", "ftp://x.org", "cid:abc", "https://", "https://user@evil.example/", "mailto:", "mailto:notanaddress", "https://exa mple.org", "not a url", "x-apple.systempreferences:"]) {
    assert.deepEqual(linkAction(bad as string), { kind: "ignore" }, String(bad));
  }
  assert.equal(linkAction("https://example.org/" + "a".repeat(9000)).kind, "ignore");
});
