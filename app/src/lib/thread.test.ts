// @ts-nocheck -- run with: node --test src/lib/thread.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import { buildRows, orderUids, threadUids, navTarget, keyNav, toggled, senderAddress, earlierMembers, normSubject, threadConfirm } from "./thread.ts";

const m = (uid, seen = true, date = 1000 - uid) => ({ uid, date, flags: { seen } });
const keys = (o) => (uid) => o[uid];
const base = { threaded: true, expanded: new Set(), pinned: new Set(), selectedUid: null };
const uids = (rows) => rows.map((r) => r.m.uid);

// newest first: 1,2,3,4,5 ; thread A = 1,3,5 ; thread B = 2,4 ; none for 6
const msgs = [m(1, false), m(2), m(3, false), m(4), m(5), m(6)];
const K = keys({ 1: "A", 3: "A", 5: "A", 2: "B", 4: "B" });

test("conversations group at the position of their newest message, collapsed by default", () => {
  const rows = buildRows(msgs, K, base);
  assert.deepEqual(uids(rows), [1, 2, 6]);
  assert.deepEqual(rows.map((r) => [r.head, r.count, r.unread]), [[true, 3, 2], [true, 2, 0], [false, 1, 0]]);
  assert.deepEqual(rows[0].uids, [1, 3, 5]);
  assert.equal(rows[0].key, "A");
});

test("threading off keeps every message as its own row", () => {
  const rows = buildRows(msgs, K, { ...base, threaded: false });
  assert.deepEqual(uids(rows), [1, 2, 3, 4, 5, 6]);
  assert.ok(rows.every((r) => !r.head && r.count === 1));
});

test("expanded conversations list their children indented; the selection forces expansion", () => {
  let rows = buildRows(msgs, K, { ...base, expanded: new Set(["B"]) });
  assert.deepEqual(uids(rows), [1, 2, 4, 6]);
  assert.deepEqual(rows.map((r) => r.depth), [0, 0, 1, 0]);
  assert.equal(rows[1].expanded, true);
  rows = buildRows(msgs, K, { ...base, selectedUid: 5 });
  assert.deepEqual(uids(rows), [1, 3, 5, 2, 6], "uid 5 is a child of A, so A opens");
  rows = buildRows(msgs, K, { ...base, selectedUid: 1 });
  assert.deepEqual(uids(rows), [1, 2, 6], "selecting the header does not expand");
});

test("a conversation with one message left in the list is a plain row", () => {
  const rows = buildRows([m(1), m(2), m(3)], keys({ 1: "A", 2: "B", 3: "A" }), base);
  assert.deepEqual(rows.map((r) => [r.m.uid, r.head]), [[1, true], [2, false]]);
  const solo = buildRows([m(1), m(2)], keys({ 1: "A", 2: "B" }), base);
  assert.ok(solo.every((r) => !r.head));
});

test("pinned messages go first and leave their conversation", () => {
  const rows = buildRows(msgs, K, { ...base, pinned: new Set([4, 6]) });
  assert.deepEqual(uids(rows), [4, 6, 1, 2]);
  assert.deepEqual(rows.map((r) => r.pinned), [true, true, false, false]);
  assert.equal(rows[3].head, false, "B lost a member and has only uid 2 left");
  const flat = buildRows(msgs, K, { ...base, threaded: false, pinned: new Set([3]) });
  assert.deepEqual(uids(flat), [3, 1, 2, 4, 5, 6]);
});

test("order and actions cover hidden members", () => {
  const rows = buildRows(msgs, K, base);
  assert.deepEqual(orderUids(rows), [1, 3, 5, 2, 4, 6]);
  assert.deepEqual(threadUids(rows, 1), [1, 3, 5]);
  assert.deepEqual(threadUids(rows, 6), [6]);
  assert.deepEqual(threadUids(rows, 99), [99]);
  const open = buildRows(msgs, K, { ...base, expanded: new Set(["A"]) });
  assert.deepEqual(threadUids(open, 1), [1], "an expanded header acts on itself");
  assert.deepEqual(orderUids(open), [1, 3, 5, 2, 4, 6]);
});

test("j/k navigation walks visible rows and clamps", () => {
  const rows = buildRows(msgs, K, base);
  assert.equal(navTarget(rows, 1, 1), 2);
  assert.equal(navTarget(rows, 2, 1), 6);
  assert.equal(navTarget(rows, 6, 1), 6);
  assert.equal(navTarget(rows, 1, -1), 1);
  assert.equal(navTarget(rows, null, 1), 1);
  assert.equal(navTarget([], 1, 1), null);
});

test("left / right collapse, expand and hop", () => {
  const closed = buildRows(msgs, K, base);
  assert.deepEqual(keyNav(closed, 1, "right"), { act: "expand", key: "A" });
  assert.deepEqual(keyNav(closed, 1, "left"), { act: "none" });
  assert.deepEqual(keyNav(closed, 6, "right"), { act: "none" });
  const open = buildRows(msgs, K, { ...base, expanded: new Set(["A"]) });
  assert.deepEqual(keyNav(open, 1, "left"), { act: "collapse", key: "A" });
  assert.deepEqual(keyNav(open, 1, "right"), { act: "select", uid: 3 });
  assert.deepEqual(keyNav(open, 3, "left"), { act: "collapse", key: "A", select: 1 });
  assert.deepEqual(keyNav(open, 3, "right"), { act: "none" });
  assert.deepEqual(keyNav(open, 99, "left"), { act: "none" });
});

test("toggled never mutates its input", () => {
  const s = new Set(["a"]);
  assert.deepEqual([...toggled(s, "b")], ["a", "b"]);
  assert.deepEqual([...toggled(s, "a")], []);
  assert.deepEqual([...toggled(s, "a", true)], ["a"]);
  assert.deepEqual([...s], ["a"]);
});

test("sender addresses are lower-cased and tolerate nothing", () => {
  assert.equal(senderAddress({ name: "A", address: " Ann@X.ORG " }), "ann@x.org");
  assert.equal(senderAddress(null), "");
  assert.equal(senderAddress(undefined), "");
  assert.equal(senderAddress("Bob@Y.org"), "bob@y.org");
});

test("earlier members are those before the selected one, oldest first", () => {
  const mem = [
    { uid: 3, folder: "INBOX", date: 30 }, { uid: 1, folder: "INBOX", date: 10 }, { uid: 1, folder: "Sent", date: 20 }, { uid: 9, folder: "INBOX", date: 40 },
  ];
  assert.deepEqual(earlierMembers(mem, "INBOX", 3).map((x) => `${x.folder}:${x.uid}`), ["INBOX:1", "Sent:1"]);
  assert.deepEqual(earlierMembers(mem, "INBOX", 1), []);
  assert.deepEqual(earlierMembers(mem, "INBOX", 77), []);
});

test("5000 rows arrange quickly", () => {
  const many = Array.from({ length: 5000 }, (_, i) => m(i + 1, i % 3 === 0, 100000 - i));
  const k = (uid) => `t${uid % 1000}`;
  const t = Date.now();
  const rows = buildRows(many, k, { ...base, expanded: new Set(["t1"]) });
  assert.ok(Date.now() - t < 500);
  assert.equal(rows.filter((r) => r.head).length, 1000);
  assert.equal(orderUids(rows).length, 5000);
});

test("normSubject strips reply prefixes and case", () => {
  assert.equal(normSubject("Re: RE:  Fwd: Hello   World"), "hello world");
  assert.equal(normSubject(null), "");
  for (const v of ["RE : Hello", "Re[2]: Hello", "Re(2): Hello", "AW: Hello", "AW : Hello", "SV: Hello", "Fwd: Hello", "FW: Hello", "Re (2) : Re[10]: Hello", "re:Hello"]) assert.equal(normSubject(v), "hello", v);
  for (const v of ["Report: Q3", "Re", "Re[x]: Hello", "Resume: Hello"]) assert.equal(normSubject(v), v.toLowerCase(), v);
});

test("thread confirm shows the count, and senders + subjects only when the subjects differ", () => {
  const same = threadConfirm([{ subject: "Plan", from: "a@x" }, { subject: "Re: Plan", from: "b@x" }], "Archive");
  assert.equal(same.mixed, false);
  assert.match(same.body, /2 messages/);
  assert.doesNotMatch(same.body, /a@x/);
  const mixed = threadConfirm([{ subject: "Plan", from: { name: "Ann", address: "a@x" } }, { subject: "Invoice", from: { address: "evil@x" } }], "Trash");
  assert.equal(mixed.mixed, true);
  assert.match(mixed.title, /2 messages/);
  assert.match(mixed.body, /Ann: Plan/);
  assert.match(mixed.body, /evil@x: Invoice/);
  const many = threadConfirm(Array.from({ length: 12 }, (_, i) => ({ subject: `S${i}`, from: "a@x" })), "Archive");
  assert.match(many.body, /and 4 more/);
});
