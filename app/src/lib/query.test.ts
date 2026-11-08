// @ts-nocheck -- run with: node --test src/lib/query.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { parseQuery, applyFilter, chipTerms, NO_CHIPS, removeToken, suggest, topAddresses, norm } from "./query.ts";

const NOW = new Date(2026, 10, 5, 12, 0, 0).getTime() / 1000; // 2026-11-05 noon local
const at = (y, mo, d, h = 9) => new Date(y, mo - 1, d, h).getTime() / 1000;
const mk = (uid, o = {}) => ({
  account: "a", folder: "INBOX", uid, messageId: null, subject: "", from: null, to: [], date: NOW, snippet: "",
  flags: { seen: true, flagged: false, answered: false, draft: false, deleted: false }, hasBody: false, ...o,
});
const A = (address, name = null) => ({ name, address });
const rows = [
  mk(1, { subject: "Quarterly report", from: A("boss@corp.example", "The Boss"), to: [A("me@x.example")], date: at(2026, 11, 5, 8), flags: { seen: false, flagged: true, answered: false } , snippet: "numbers inside" }),
  mk(2, { subject: "Café menu", from: A("news@letter.example", "Newsletter"), to: [A("me@x.example")], date: at(2026, 11, 4), snippet: "weekly specials" }),
  mk(3, { subject: "Re: lunch", from: A("ana@friends.example", "Ána Müller"), to: [A("me@x.example"), A("bob@friends.example", "Bob")], date: at(2026, 10, 20), flags: { seen: true, flagged: false, answered: true }, snippet: "see you" }),
  mk(4, { subject: "Old thing", from: A("old@x.example"), date: at(2025, 1, 1), snippet: "from the past", cc: [A("carl@x.example", "Carl")], hasAttachment: true }),
  mk(5, { subject: "No date", from: A("nd@x.example"), date: null }),
];
const caps = { cc: true, attachment: true };
const uids = (q, c) => { const p = parseQuery(q, NOW, c); return applyFilter(rows, p.terms).map((m) => m.uid); };

test("bare words = contains over subject/snippet/from/to, AND-ed", () => {
  assert.deepEqual(uids("report"), [1]);
  assert.deepEqual(uids("numbers"), [1]);
  assert.deepEqual(uids("boss"), [1]);
  assert.deepEqual(uids("bob"), [3]);
  assert.deepEqual(uids("weekly cafe"), [2]);
  assert.deepEqual(uids("report weekly"), []);
  assert.deepEqual(uids(""), [1, 2, 3, 4, 5]);
});
test("from / to / subject", () => {
  assert.deepEqual(uids("from:boss"), [1]);
  assert.deepEqual(uids("from:letter.example"), [2]);
  assert.deepEqual(uids("to:bob"), [3]);
  assert.deepEqual(uids("subject:lunch"), [3]);
  assert.deepEqual(uids("subject:boss"), []);
});
test("contains: and body: are aliases", () => {
  assert.deepEqual(uids("contains:specials"), [2]);
  assert.deepEqual(uids("body:specials"), [2]);
});
test("cc and has:attachment only when capable; otherwise hint and ignored", () => {
  assert.deepEqual(uids("cc:carl", caps), [4]);
  assert.deepEqual(uids("has:attachment", caps), [4]);
  assert.deepEqual(uids("-has:attachment", caps), [1, 2, 3, 5]);
  const p = parseQuery("cc:carl has:attachment", NOW);
  assert.equal(p.terms.length, 0);
  assert.equal(p.hints.length, 2);
  assert.equal(parseQuery("has:video", NOW, caps).hints.length, 1);
});
test("is: values", () => {
  assert.deepEqual(uids("is:unread"), [1]);
  assert.deepEqual(uids("is:read"), [2, 3, 4, 5]);
  assert.deepEqual(uids("is:flagged"), [1]);
  assert.deepEqual(uids("is:unflagged"), [2, 3, 4, 5]);
  assert.deepEqual(uids("is:answered"), [3]);
  assert.deepEqual(uids("IS:Unread"), [1]);
  assert.match(parseQuery("is:bogus", NOW).hints[0], /Unknown is:bogus/);
});
test("quoted phrases", () => {
  assert.deepEqual(uids('subject:"quarterly report"'), [1]);
  assert.deepEqual(uids('subject:"report quarterly"'), []);
  assert.deepEqual(uids('"see you"'), [3]);
  assert.deepEqual(uids('from:"the boss" is:flagged'), [1]);
});
test("negation", () => {
  assert.deepEqual(uids("-from:newsletter is:read"), [3, 4, 5]);
  assert.deepEqual(uids("-is:read"), [1]);
  assert.deepEqual(uids("-weekly report"), [1]);
  assert.doesNotThrow(() => uids("-")); // lone dash is a plain word
});
test("absolute dates are local start-of-day; before exclusive, after inclusive", () => {
  assert.deepEqual(uids("after:2026-11-05"), [1]);
  assert.deepEqual(uids("after:2026-11-04"), [1, 2]);
  assert.deepEqual(uids("before:2026-11-05"), [2, 3, 4]);
  assert.deepEqual(uids("after:2026-10-01 before:2026-11-05"), [2, 3]);
  assert.deepEqual(uids("-before:2026-11-05"), [1, 5]);
  assert.deepEqual(uids("after:2026/11/05"), [1]);
});
test("relative dates", () => {
  assert.deepEqual(uids("after:today"), [1]);
  assert.deepEqual(uids("before:today"), [2, 3, 4]);
  assert.deepEqual(uids("after:yesterday"), [1, 2]);
  assert.deepEqual(uids("newer_than:2d"), [1, 2]);
  assert.deepEqual(uids("older_than:7d"), [3, 4]);
  assert.deepEqual(uids("older_than:1y"), [4]);
  assert.deepEqual(uids("newer_than:1m"), [1, 2, 3]);
});
test("invalid dates give a hint, never throw, and are dropped", () => {
  for (const q of ["before:2026-13-01", "after:2026-02-30", "after:soon", "older_than:xd", "newer_than:7"]) {
    const p = parseQuery(q, NOW);
    assert.equal(p.terms.length, 0, q);
    assert.equal(p.hints.length, 1, q);
  }
  assert.match(parseQuery("before:nope", NOW).hints[0], /Invalid date "nope"/);
});
test("unknown operators fall back to plain text; empty value ignored", () => {
  const p = parseQuery("foo:bar", NOW);
  assert.deepEqual(p.terms.map((t) => [t.field, t.value]), [["any", "foo:bar"]]);
  assert.deepEqual(uids("from:"), [1, 2, 3, 4, 5]);
  assert.deepEqual(uids("label:x"), []);
});
test("case and diacritic insensitive", () => {
  assert.deepEqual(uids("from:muller"), [3]);
  assert.deepEqual(uids("from:ÁNA"), [3]);
  assert.deepEqual(uids("subject:café"), [2]);
  assert.deepEqual(uids("CAFE"), [2]);
  assert.equal(norm("Crème Brûlée"), "creme brulee");
});
test("chips combine with AND with the query", () => {
  const f = (c, q = "") => applyFilter(rows, [...chipTerms({ ...NO_CHIPS, ...c }, NOW), ...parseQuery(q, NOW).terms]).map((m) => m.uid);
  assert.deepEqual(f({}), [1, 2, 3, 4, 5]);
  assert.deepEqual(f({ unread: true }), [1]);
  assert.deepEqual(f({ flagged: true, unread: true }), [1]);
  assert.deepEqual(f({ today: true }), [1]);
  assert.deepEqual(f({ week: true }), [1, 2]);
  assert.deepEqual(f({ week: true }, "from:news"), [2]);
  assert.deepEqual(f({ unread: true }, "from:news"), []);
});
test("token ranges allow removal", () => {
  const q = 'from:boss subject:"a b" -is:read';
  const p = parseQuery(q, NOW);
  assert.equal(p.terms.length, 3);
  assert.equal(removeToken(q, p.terms[1]), "from:boss -is:read");
  assert.equal(removeToken(q, p.terms[0]), 'subject:"a b" -is:read');
});
test("suggestions", () => {
  assert.deepEqual(suggest("fr").items.map((i) => i.label), ["from:"]);
  assert.ok(suggest("is:").items.length >= 5);
  assert.equal(suggest("hello ").items.length, 0);
  assert.equal(suggest("hello").items.length, 0);
  const s = suggest("x from:b", rows);
  assert.equal(s.prefix, "x ");
  assert.deepEqual(s.items.map((i) => i.label), ["boss@corp.example"]);
  assert.deepEqual(topAddresses(rows, "to", 1), ["me@x.example"]);
});
test("5000 rows filter is fast after prep", () => {
  const big = Array.from({ length: 5000 }, (_, i) => mk(i, { subject: `msg ${i}`, from: A(`u${i}@x.example`), snippet: "lorem ipsum" }));
  const t = parseQuery("from:u49 -subject:7 is:read", NOW).terms;
  applyFilter(big, t);
  const s = performance.now();
  const r = applyFilter(big, t);
  assert.ok(r.length > 0);
  assert.ok(performance.now() - s < 50, `took ${performance.now() - s}ms`);
});

test("folder: is a known operator and needsBackend spots cc/has/folder", async () => {
  const { parseQuery, needsBackend } = await import("./query.ts");
  const p = parseQuery("folder:Archive invoice");
  assert.equal(p.terms.length, 2);
  assert.equal(p.terms[0].kind, "folder");
  assert.ok(needsBackend("from:a cc:bob"));
  assert.ok(needsBackend("-has:attachment"));
  assert.ok(needsBackend("x folder:inbox"));
  assert.ok(!needsBackend("from:a subject:b is:unread"));
  assert.ok(!needsBackend("cc:"), "still typing");
  const full = parseQuery("cc:Bob has:attachment", 0, { cc: true, attachment: true });
  assert.deepEqual(full.terms.map((t) => t.kind), ["text", "attachment"]);
  assert.equal(full.hints.length, 0);
});
