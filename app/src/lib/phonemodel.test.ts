// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/phonemodel.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import * as m from "./phonemodel.ts";

const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 26_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148";
const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";

test("phone layout: only on a mobile OS with a phone-sized screen", () => {
  assert.equal(m.isPhoneLayout({ ua: IPHONE, maxTouchPoints: 5, shortSide: 390 }), true);
  assert.equal(m.isPhoneLayout({ ua: IPHONE, maxTouchPoints: 5, shortSide: 844 }), false); // landscape: short side is still 390 in real use
  assert.equal(m.isPhoneLayout({ ua: "Mozilla/5.0 (Linux; Android 15; Pixel 9)", maxTouchPoints: 5, shortSide: 412 }), true);
});
test("the Mac never gets the phone layout, however narrow", () => {
  assert.equal(m.isPhoneLayout({ ua: MAC, maxTouchPoints: 0, shortSide: 300 }), false);
  assert.equal(m.isPhoneLayout({ ua: MAC, maxTouchPoints: 0, shortSide: 1080 }), false);
});
test("iPad (Mac user agent + touch) is mobile but too big for the phone layout", () => {
  assert.equal(m.isMobileOs(MAC, 5), true);
  assert.equal(m.isPhoneLayout({ ua: MAC, maxTouchPoints: 5, shortSide: 820 }), false);
});
test("?layout=phone works only in a dev build, never in a shipped Mac build", () => {
  assert.equal(m.isPhoneLayout({ ua: MAC, maxTouchPoints: 0, shortSide: 1000, force: "phone" }), false);
  assert.equal(m.isPhoneLayout({ ua: MAC, maxTouchPoints: 0, shortSide: 1000, force: "phone", dev: false }), false);
  assert.equal(m.isPhoneLayout({ ua: MAC, maxTouchPoints: 0, shortSide: 1000, force: "phone", dev: true }), true);
  assert.equal(m.isPhoneLayout({ ua: MAC, maxTouchPoints: 0, shortSide: 1000, force: "other" }), false);
});

test("navigation stack: boxes > list > reader and back", () => {
  let n = m.startNav();
  assert.equal(m.canGoBack(n), false);
  n = m.openList(n); assert.equal(n.mail, "list"); assert.equal(m.canGoBack(n), true);
  n = m.openReader(n); assert.equal(n.mail, "reader"); assert.equal(m.showTabBar(n), false);
  n = m.back(n); assert.equal(n.mail, "list"); assert.equal(m.showTabBar(n), true);
  n = m.back(n); assert.equal(n.mail, "boxes");
  assert.deepEqual(m.back(n), n);
});
test("tabs keep their own stack; tapping the active tab pops to root", () => {
  let n = m.openReader(m.startNav());
  n = m.selectTab(n, "calendar"); assert.equal(n.tab, "calendar"); assert.equal(n.mail, "reader");
  n = m.selectTab(n, "mail"); assert.equal(n.mail, "reader"); // returns where it was
  n = m.selectTab(n, "mail"); assert.equal(n.mail, "boxes");
  n = m.openSection(n, "appearance"); assert.equal(n.tab, "settings"); assert.equal(m.canGoBack(n), true);
  n = m.back(n); assert.equal(n.section, null); assert.equal(m.canGoBack(n), false);
  assert.equal(m.canGoBack(m.selectTab(n, "mira")), false);
});

test("start folder: remembered one if it still exists, else inbox, else first selectable", () => {
  const fs = [{ name: "Archive", role: "archive", selectable: true }, { name: "INBOX", role: "inbox", selectable: true }, { name: "X", role: "other", selectable: false }];
  assert.equal(m.startFolder(fs, "Archive"), "Archive");
  assert.equal(m.startFolder(fs, "Gone"), "INBOX");
  assert.equal(m.startFolder(fs, "X"), "INBOX");
  assert.equal(m.startFolder([{ name: "A", role: "other", selectable: true }], null), "A");
  assert.equal(m.startFolder([], null), null);
});

test("folder rows: special folders first, depth and labels", () => {
  const rows = m.folderRows([
    { name: "Trash", role: "trash", selectable: true }, { name: "Work/Sub", role: "other", selectable: true }, { name: "Work", role: "other", selectable: false, delimiter: "/" },
    { name: "INBOX", role: "inbox", selectable: true }, { name: "Sent", role: "sent", selectable: true },
  ]);
  assert.deepEqual(rows.map((r) => [r.label, r.depth]), [["Inbox", 0], ["Sent", 0], ["Trash", 0], ["Work", 0], ["Sub", 1]]);
  assert.equal(rows.find((r) => r.name === "Work").selectable, false);
});

test("edge swipe back needs a left-edge start, enough travel, mostly horizontal", () => {
  assert.equal(m.isBackSwipe(8, 90, 10), true);
  assert.equal(m.isBackSwipe(80, 120, 5), false);
  assert.equal(m.isBackSwipe(8, 40, 0), false);
  assert.equal(m.isBackSwipe(8, 90, 80), false);
});
test("pull to refresh only from the top and past the trigger", () => {
  assert.deepEqual(m.pull(100, 5), { offset: 0, ready: false });
  assert.equal(m.pull(30, 0).ready, false);
  assert.equal(m.pull(200, 0).ready, true);
  assert.ok(m.pull(1000, 0).offset <= 110);
});
test("row swipe: left opens the buttons, right toggles read, small drags close", () => {
  assert.equal(m.swipeEnd(-80, false), "open");
  assert.equal(m.swipeEnd(-20, false), "close");
  assert.equal(m.swipeEnd(120, false), "toggle-read");
  assert.equal(m.swipeEnd(60, true), "close");  // dragging an open row back right
  assert.equal(m.swipeEnd(0, true), "open");    // tap-drag nothing: stays open
  assert.equal(m.swipeOffset(-400, false), -m.SWIPE_ACTION_W);
  assert.equal(m.swipeOffset(0, true), -m.SWIPE_ACTION_W);
});
test("keyboard inset", () => {
  assert.equal(m.keyboardInset(844, 844, 0), 0);
  assert.equal(m.keyboardInset(844, 500, 0), 344);
  assert.equal(m.keyboardInset(844, 500, 344), 0); // page scrolled up by the keyboard: visual viewport top moved
});
test("row swipe buttons never offer a permanent delete", () => {
  for (const r of ["inbox", "sent", "archive", "junk", "trash", "other", undefined]) {
    assert.ok(m.rowActions(r).length >= 1 && m.rowActions(r).length <= 2);
    assert.ok(m.rowActions(r).every((a) => ["archive", "trash", "inbox"].includes(a.id)));
  }
  assert.deepEqual(m.rowActions("inbox").map((a) => a.id), ["archive", "trash"]);
  assert.deepEqual(m.rowActions("trash").map((a) => a.id), ["inbox"]);
});

// ---- review fixes ----
test("Q01: row swipe actions only act on rows of the open account + folder", () => {
  const cur = { account: "a@x.org", folder: "INBOX" };
  assert.equal(m.rowSwipeAllowed({ account: "a@x.org", folder: "INBOX" }, cur), true);
  assert.equal(m.rowSwipeAllowed({ account: "a@x.org", folder: "Archive" }, cur), false); // search result from another folder
  assert.equal(m.rowSwipeAllowed({ account: "b@x.org", folder: "INBOX" }, cur), false); // same folder name, other account
  assert.equal(m.rowSwipeAllowed({}, cur), true); // plain list rows carry no origin
});
test("Q02: the back swipe and the row swipe never start from the same touch", () => {
  for (const x of [0, 10, 24]) { assert.equal(m.isEdgeStart(x), true); assert.equal(m.rowSwipeMayStart(x), false); }
  for (const x of [25, 100, 380]) { assert.equal(m.isEdgeStart(x), false); assert.equal(m.rowSwipeMayStart(x), true); }
  // exactly one of the two gestures can claim any start x
  for (let x = 0; x < 400; x++) assert.notEqual(m.isEdgeStart(x), m.rowSwipeMayStart(x));
  assert.equal(m.isBackSwipe(24, 70, 10), true);
  assert.equal(m.isBackSwipe(25, 200, 0), false);
  assert.equal(m.isBackSwipe(10, 69, 0), false);   // not far enough
  assert.equal(m.isBackSwipe(10, 100, 70), false); // too diagonal
});
test("Q03: the click after a swipe is swallowed for a short window only", () => {
  assert.equal(m.swipedRecently(String(1000), 1000 + m.SWIPE_CLICK_GUARD_MS - 1), true);
  assert.equal(m.swipedRecently(String(1000), 1000 + m.SWIPE_CLICK_GUARD_MS), false);
  assert.equal(m.swipedRecently(undefined, 5000), false);
});
test("Q04: pull-to-refresh thresholds are named and consistent", () => {
  assert.equal(m.PULL_READY_OFFSET, m.PULL_TRIGGER * 0.5 + 10);
  assert.equal(m.pull((m.PULL_READY_OFFSET - 1) / m.PULL_DAMPING, 0).ready, false);
  assert.equal(m.pull(m.PULL_READY_OFFSET / m.PULL_DAMPING, 0).ready, true);
  assert.equal(m.pull(10000, 0).offset, m.PULL_MAX);
  assert.deepEqual(m.pull(100, 1), { offset: 0, ready: false }); // not at the top
});
test("Q05: an account change returns to the Mailboxes screen; the folder is remembered per account", () => {
  let n = m.openReader(m.startNav());
  n = m.navAfterAccountChange(n);
  assert.deepEqual(n, { tab: "mail", mail: "boxes", section: null });
  assert.equal(m.canGoBack(n), false);
  const s = m.navAfterAccountChange({ tab: "settings", mail: "list", section: "general" });
  assert.equal(s.tab, "settings"); assert.equal(s.section, "general"); assert.equal(s.mail, "boxes");
  assert.notEqual(m.folderKey("a@x.org"), m.folderKey("b@x.org"));
  assert.equal(m.folderKey(null), m.folderKey(undefined));
});
test("Q10: app.view follows the tab: calendar only on the Calendar tab", () => {
  assert.equal(m.viewForTab("calendar"), "calendar");
  for (const t of ["mail", "mira", "settings"]) assert.equal(m.viewForTab(t), "mail");
});
test("Q08/Q14: swipe thresholds at the boundary", () => {
  assert.equal(m.swipeEnd(m.SWIPE_READ_AT - 1, false), "close");
  assert.equal(m.swipeEnd(m.SWIPE_READ_AT, false), "toggle-read");
  assert.equal(m.swipeEnd(-(m.SWIPE_OPEN_AT - 1), false), "close");
  assert.equal(m.swipeEnd(-m.SWIPE_OPEN_AT, false), "open");
  assert.equal(m.swipeEnd(39, true), "open");
  assert.equal(m.swipeEnd(40, true), "close");
  assert.equal(m.swipeOffset(-1000, false), -m.SWIPE_ACTION_W);
  assert.equal(m.swipeOffset(1000, false), m.SWIPE_READ_AT + 30);
  assert.equal(m.swipeOffset(0, true), -m.SWIPE_ACTION_W);
});
test("Q14: dotted INBOX.* layouts and nested folders", () => {
  const f = (name, role = "other", selectable = true, delimiter = null) => ({ name, role, selectable, delimiter });
  const rows = m.folderRows([f("INBOX", "inbox"), f("INBOX.Sent", "sent"), f("INBOX.Projects", "other"), f("INBOX.Projects.2026", "other"), f("INBOX.Trash", "trash")]);
  const by = Object.fromEntries(rows.map((r) => [r.name, r]));
  assert.equal(by["INBOX.Projects.2026"].label, "2026");
  assert.equal(by["INBOX.Projects.2026"].depth, 2);
  assert.equal(by["INBOX.Sent"].depth, 1);
  assert.equal(rows[0].name, "INBOX");
  // an unselectable parent is kept only while it has children
  const kids = m.folderRows([f("INBOX", "inbox"), f("INBOX.Group", "other", false), f("INBOX.Group.A")]);
  assert.ok(kids.some((r) => r.name === "INBOX.Group" && !r.selectable));
  assert.ok(!m.folderRows([f("INBOX", "inbox"), f("INBOX.Empty", "other", false)]).some((r) => r.name === "INBOX.Empty"));
  // slash layout
  assert.equal(m.folderRows([f("INBOX", "inbox"), f("Work/Clients", "other", true, "/")]).find((r) => r.name === "Work/Clients").depth, 1);
});
test("Q14: back stack after an account change and a tab round trip", () => {
  let n = m.openReader(m.openList(m.startNav()));
  n = m.selectTab(n, "settings"); n = m.openSection(n, "ai");
  n = m.navAfterAccountChange(n);
  assert.equal(m.back(n).section, null); // settings stack intact
  n = m.selectTab(n, "mail");
  assert.equal(n.mail, "boxes"); assert.equal(m.canGoBack(n), false);
});
test("compose: Send needs a valid recipient; title and draft status", () => {
  const valid = (a) => /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(a);
  assert.equal(m.canSendTo([], [], valid), false);
  assert.equal(m.canSendTo(["nope"], [], valid), false);
  assert.equal(m.canSendTo(["nope", "a@b.co"], [], valid), false, "every entered address must be valid");
  assert.equal(m.canSendTo(["a@b.co"], [], valid), true);
  assert.equal(m.canSendTo(["a@b.co"], ["c@d.co", "x"], valid), false, "a bad Cc/Bcc blocks too");
  assert.equal(m.canSendTo([], ["c@d.co"], valid), false, "send requires a To recipient");
  assert.equal(m.canSendTo(["a@b.co"], ["c@d.co"], valid), true);
  assert.equal(m.composeTitle("  ", false), "New Message");
  assert.equal(m.composeTitle("", true), "Reply");
  assert.equal(m.composeTitle(" Re: Hi ", true), "Re: Hi");
  assert.equal(m.draftStatus("Draft saved on server"), "Draft saved");
  assert.equal(m.draftStatus(""), "");
  assert.equal(m.draftStatus("Draft not saved on server"), "Draft not saved on server");
});
test("compose: caret scroll keeps the caret clear of the edges and the format bar", () => {
  assert.equal(m.caretScrollDelta(100, 120, 0, 400), 0);
  assert.equal(m.caretScrollDelta(380, 400, 0, 400, 16), 16 + 400 - 400 + 0 + 0 || 16); // bottom: needs +16
  assert.equal(m.caretScrollDelta(380, 400, 0, 400, 16), 16);
  assert.equal(m.caretScrollDelta(-20, 0, 0, 400, 16), -36);
});
test("Q13: spoken row label", () => {
  assert.equal(m.rowLabel({ unread: true, flagged: true, from: "Ann", subject: "Lunch", when: "Mon" }), "Unread, Flagged, Ann, Lunch, Mon");
  assert.equal(m.rowLabel({ unread: false, flagged: false, from: "Ann", subject: "", when: "Mon" }), "Ann, No subject, Mon");
});

test("compose: recipient chips always show the real address", () => {
  assert.deepEqual(m.chipParts("a@b.co"), { addr: "a@b.co", name: "" });
  assert.deepEqual(m.chipParts("Ann <a@b.co>"), { addr: "a@b.co", name: "Ann" });
  assert.deepEqual(m.chipParts('"Ann Lee" <a@b.co>'), { addr: "a@b.co", name: "Ann Lee" });
  // spoof: the name looks like another address -> only the real address is shown
  assert.deepEqual(m.chipParts('"ceo@company.com" <attacker@evil.example>'), { addr: "attacker@evil.example", name: "" });
  assert.deepEqual(m.chipParts("<x@y.zz>"), { addr: "x@y.zz", name: "" });
});

// ---- list / folder tree additions ----
const F = (name: string, role = "other", selectable = true, delimiter = "/") => ({ name, role, selectable, delimiter });
test("long press: small jitter holds, travel cancels", () => {
  assert.equal(m.longPressMoved(3, 4), false);
  assert.equal(m.longPressMoved(12, 0), true);
});
test("bulk actions follow the folder role like the Mac list", () => {
  assert.deepEqual(m.bulkActions("inbox"), { trashLabel: "Trash", junk: "junk" });
  assert.deepEqual(m.bulkActions("trash"), { trashLabel: "Delete forever", junk: null });
  assert.deepEqual(m.bulkActions("junk"), { trashLabel: "Trash", junk: "notjunk" });
});
test("whereLine: only for rows outside the open folder or across accounts", () => {
  assert.equal(m.whereLine("folder", { account: "a@x", folder: "INBOX" }, "INBOX"), "");
  assert.equal(m.whereLine("account", { account: "a@x", folder: "Sent" }, "INBOX"), "Sent");
  assert.equal(m.whereLine("all", { account: "a@x", folder: "INBOX" }, "INBOX"), "a@x · INBOX");
});
test("listCountText", () => {
  const base = { backend: false, searching: false, hasResults: false, total: 0, scope: "folder" as const, filterActive: false, filtered: 0, loaded: 1200 };
  assert.match(m.listCountText(base), /messages$/);
  assert.match(m.listCountText({ ...base, filterActive: true, filtered: 3, loaded: 10 }), /^3 of 10 messages$/);
  assert.equal(m.listCountText({ ...base, backend: true, searching: true }), "Searching…");
  assert.equal(m.listCountText({ ...base, backend: true, total: 1, scope: "all" }), "1 match in all accounts");
  assert.equal(m.listCountText({ ...base, backend: true, total: 2, scope: "account", hasResults: true }), "2 matches in this account");
});
test("folderTree: collapse hides descendants and folds their unread into the parent", () => {
  const fs = [F("INBOX", "inbox"), F("Work", "other"), F("Work/A"), F("Work/A/B"), F("Work/C"), F("Trash", "trash")];
  const unread = { INBOX: 2, "Work/A": 1, "Work/A/B": 4, "Work/C": 5 };
  const open = m.folderTree(fs, {}, unread);
  assert.deepEqual(open.map((r) => r.name), ["INBOX", "Trash", "Work", "Work/A", "Work/A/B", "Work/C"].sort((a, b) => open.findIndex((r) => r.name === a) - open.findIndex((r) => r.name === b)));
  assert.equal(open.find((r) => r.name === "Work").count, 0); // expanded: own count only
  assert.equal(open.find((r) => r.name === "Work").hasKids, true);
  const closed = m.folderTree(fs, { Work: true }, unread);
  assert.equal(closed.some((r) => r.name === "Work/A"), false);
  const w = closed.find((r) => r.name === "Work");
  assert.equal(w.collapsed, true);
  assert.equal(w.count, 10);
  const mid = m.folderTree(fs, { "Work/A": true }, unread);
  assert.equal(mid.some((r) => r.name === "Work/A/B"), false);
  assert.equal(mid.find((r) => r.name === "Work/A").count, 5);
  assert.equal(mid.find((r) => r.name === "Work").count, 0);
});
test("folderTree: collapsed flag ignored for a folder without children", () => {
  const r = m.folderTree([F("INBOX", "inbox")], { INBOX: true }, { INBOX: 3 });
  assert.equal(r[0].collapsed, false);
  assert.equal(r[0].count, 3);
});
test("folderMenu mirrors the Mac sidebar menu", () => {
  const o = { inboxName: "INBOX", live: false, movable: true, subscribed: true };
  const ids = (f, x = o) => m.folderMenu(f, x).map((i) => i.id);
  assert.deepEqual(ids({ name: "INBOX", role: "inbox" }), ["live", "sub", "subscribe"]);
  assert.equal(m.folderMenu({ name: "INBOX", role: "inbox" }, o)[0].disabled, true);
  assert.deepEqual(ids({ name: "Work", role: "other" }), ["live", "sub", "rename", "move", "delete", "subscribe"]);
  assert.deepEqual(ids({ name: "Work", role: "other" }, { ...o, movable: false }), ["live", "sub", "rename", "delete", "subscribe"]);
  assert.deepEqual(ids({ name: "Trash", role: "trash" }), ["live", "sub", "empty", "subscribe"]);
  const l = m.folderMenu({ name: "Work", role: "other" }, { ...o, live: true, subscribed: null });
  assert.equal(l[0].label, "Stop keeping live");
  assert.equal(l[l.length - 1].disabled, true);
});

test("sheet tap guard: taps right after a long press opens a sheet are ignored", () => {
  assert.equal(m.sheetTapIgnored(1000, 1000), true);
  assert.equal(m.sheetTapIgnored(1000, 1000 + m.SHEET_TAP_GUARD_MS - 1), true);
  assert.equal(m.sheetTapIgnored(1000, 1000 + m.SHEET_TAP_GUARD_MS), false);
});
test("a fired long press owns the rest of the gesture", () => {
  assert.equal(m.longPressOwns("1"), true);
  assert.equal(m.longPressOwns(undefined), false);
  assert.equal(m.longPressOwns(""), false);
});
