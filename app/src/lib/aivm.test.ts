// @ts-nocheck -- run with: node --test src/lib/aivm.test.ts (Node type stripping; no extra dependencies)
import test from "node:test";
import assert from "node:assert/strict";
import {
  hostOf, isLoopbackHost, consentHeading, isLanHost, clientWarnings, badgeText, keyReentryNeeded, tokenMeter, fmtTokens, alignEgress,
  defaultScopes, buildConsent, chipText, chipDetail, validateProfileForm, validateBudgets, DEFAULT_SETTINGS, purgeLines, toMs, PRESETS,
  resultText, canApprove, approveLabel, placeProposals, selectedCount, selectedIdx, isPending, isRunning,
  canOpenDraft, canDismissDraft, draftPending, draftPreview, draftRecipients, draftStatusText, draftHeading, placeDrafts, AI_DRAFT_BANNER, DRAFT_PREVIEW_CHARS,
  withDefaultTicks, formatRangeLabel, isEventKind, eventHeading, warningTone, calendarOn,
  decidedAll, followUpText, toneNotice, TONE_SETUP_HINT, EDIT_KEEPS_READ_NOTE, editHeading, prefillScopes, prefillCalendars, consentChangeSummary,
} from "./aivm.ts";

test("loopback and LAN detection", () => {
  for (const h of ["localhost:11434", "127.0.0.1", "127.0.0.1:1234", "[::1]:8080", "foo.localhost"]) assert.ok(isLoopbackHost(h), h);
  for (const h of ["api.openai.com", "128.0.0.1", "10.0.0.5"]) assert.ok(!isLoopbackHost(h), h);
  for (const h of ["10.1.2.3", "192.168.1.9:8080", "172.16.0.1", "172.31.9.9", "nas.local", "box", "fd12::1"]) assert.ok(isLanHost(h), h);
  for (const h of ["172.32.0.1", "8.8.8.8", "api.openai.com", "localhost"]) assert.ok(!isLanHost(h), h);
});
test("hostOf only accepts http(s)", () => {
  assert.equal(hostOf("https://api.openai.com/v1"), "api.openai.com");
  assert.equal(hostOf("http://localhost:11434/v1"), "localhost:11434");
  assert.equal(hostOf("ftp://x.example"), null);
  assert.equal(hostOf("nope"), null);
});
test("warnings: cleartext non-loopback and LAN", () => {
  assert.deepEqual(clientWarnings("https://api.openai.com/v1"), []);
  assert.deepEqual(clientWarnings("http://localhost:11434/v1"), []);
  assert.deepEqual(clientWarnings("http://example.org/v1").map((w) => w.kind), ["cleartextRemote"]);
  assert.deepEqual(clientWarnings("http://192.168.1.5:8000/v1").map((w) => w.kind), ["cleartextRemote", "lanHost"]);
  assert.deepEqual(clientWarnings("https://nas.local/v1").map((w) => w.kind), ["lanHost"]);
  assert.match(clientWarnings("http://example.org")[0].message, /sent unencrypted/);
});
test("badge text", () => {
  assert.equal(badgeText("localhost:11434", "llama3"), "Local model");
  assert.equal(badgeText("api.openai.com", "gpt-4o"), "api.openai.com · gpt-4o");
});
test("key re-entry when the base URL changes", () => {
  const p = { baseUrl: "https://api.openai.com/v1", hasKey: true, needsKeyReentry: false };
  assert.equal(keyReentryNeeded(p, "https://api.openai.com/v1/"), false);
  assert.equal(keyReentryNeeded(p, "https://evil.example/v1"), true);
  assert.equal(keyReentryNeeded({ ...p, hasKey: false }, "https://evil.example/v1"), false);
  assert.equal(keyReentryNeeded({ ...p, needsKeyReentry: true }, p.baseUrl), true);
  assert.equal(keyReentryNeeded(null, "https://x.example"), false);
});
test("token meter", () => {
  assert.deepEqual(tokenMeter(0, 1000), { pct: 0, level: "ok", label: "0 / 1k" });
  assert.equal(tokenMeter(800, 1000).level, "warn");
  assert.equal(tokenMeter(1200, 1000).level, "full");
  assert.equal(tokenMeter(1200, 1000).pct, 100);
  assert.equal(fmtTokens(12345), "12k"); assert.equal(fmtTokens(1500), "1.5k"); assert.equal(fmtTokens(999), "999");
});
test("egress rows align to assistant messages from the newest end", () => {
  const msgs = [{ id: "u1", role: "user" }, { id: "a1", role: "assistant" }, { id: "u2", role: "user" }, { id: "a2", role: "assistant" }];
  const rows = [{ id: "e0", at: 1 }, { id: "e1", at: 2 }, { id: "e2", at: 3 }];
  const m = alignEgress(msgs, rows);
  assert.equal(m.get("a2").id, "e2"); assert.equal(m.get("a1").id, "e1");
  assert.equal(toMs(1700000000), 1700000000000); assert.equal(toMs(1700000000000), 1700000000000);
});
test("consent defaults turn Junk and Trash off", () => {
  const s = defaultScopes("a@b.example", [{ name: "INBOX", role: "inbox", selectable: true }, { name: "Junk", role: "junk", selectable: true }, { name: "Trash", role: "trash", selectable: true }, { name: "X", role: "other", selectable: false }]);
  assert.deepEqual(s.map((x) => [x.folder, x.allowed]), [["INBOX", true], ["Junk", false], ["Trash", false]]);
  assert.deepEqual(defaultScopes(null, []), []);
});
test("form and budget validation", () => {
  const ok = { name: "A", kind: "openai_chat", baseUrl: "http://192.168.1.2:8000/v1", model: "m", maxOutputTokens: 512 };
  assert.equal(validateProfileForm(ok), null);
  assert.ok(validateProfileForm({ ...ok, baseUrl: "ftp://x" }));
  assert.ok(validateProfileForm({ ...ok, model: " " }));
  assert.ok(validateProfileForm({ ...ok, maxOutputTokens: 1.5 }));
  assert.equal(validateBudgets(DEFAULT_SETTINGS), null);
  assert.ok(validateBudgets({ ...DEFAULT_SETTINGS, perDayTokens: 5 }));
});
test("presets match the agreed endpoints", () => {
  const by = Object.fromEntries(PRESETS.map((p) => [p.id, p]));
  assert.notEqual(PRESETS[0].id, "endlesstokens"); // never pre-selected
  assert.deepEqual(PRESETS.slice(0, 3).map((p) => p.id), ["openai", "anthropic", "endlesstokens"]);
  assert.equal(by.endlesstokens.baseUrl, "https://api.endlesstokens.ai/v1"); assert.equal(by.endlesstokens.model, ""); assert.equal(by.endlesstokens.kind, "openai_chat");
  assert.equal(by.openai.baseUrl, "https://api.openai.com/v1");
  assert.equal(by.anthropic.baseUrl, "https://api.anthropic.com"); assert.equal(by.anthropic.model, "claude-opus-5-5");
  assert.equal(by.openrouter.baseUrl, "https://openrouter.ai/api/v1");
  assert.equal(by.ollama.baseUrl, "http://localhost:11434/v1");
  assert.equal(by.lmstudio.baseUrl, "http://localhost:1234/v1");
});
test("purge report lines render scalar fields only", () => {
  assert.deepEqual(purgeLines({ egressRowsDeleted: 3, nested: { a: 1 } }), ["Egress rows deleted: 3"]);
  assert.deepEqual(purgeLines(null), []);
});

test("a turn with tool rounds pairs with ONE assistant message: last request, items merged", () => {
  const msgs = [{ id: "u1", role: "user" }, { id: "a1", role: "assistant" }, { id: "u2", role: "user" }, { id: "a2", role: "assistant" }];
  const it = (ref) => ({ ref, subject: `s${ref}`, from: "f" });
  const rows = [
    { id: "e0", at: 10, round: 0, items: [] },
    { id: "e1", at: 11, round: 0, items: [] },
    { id: "e2", at: 11, round: 1, items: [it("m:1")] },
    { id: "e3", at: 11, round: 2, items: [it("m:1"), it("m:2")] },
  ];
  const m = alignEgress(msgs, rows);
  assert.equal(m.get("a2").id, "e3");
  assert.equal(m.get("a2").requests, 3);
  assert.deepEqual(m.get("a2").items.map((x) => x.ref), ["m:1", "m:2"]);
  assert.equal(m.get("a1").id, "e0");
  assert.equal(m.get("a1").requests, undefined);
  // a failed send (row without assistant message) at the end shifts nothing it should not: pairing is from the newest end
  const only = alignEgress([{ id: "a1", role: "assistant" }], [{ id: "x", at: 1, round: 0, items: [] }, { id: "y", at: 2, round: 0, items: [] }]);
  assert.equal(only.get("a1").id, "y");
});
test("tool chip text", () => {
  assert.equal(chipText({ name: "search_mail", label: "from:foo", summary: "12 results", ok: true }), "Searched mail: from:foo - 12 results");
  assert.equal(chipText({ name: "list_folders", label: "", summary: "3 folders", ok: true }), "Listed folders - 3 folders");
  assert.equal(chipText({ name: "get_message", label: "m:3", summary: "", ok: true, running: true }), "Read message: m:3 …");
  assert.equal(chipText({ name: "get_message", label: "m:3", summary: "failed", ok: false }), "Read message: m:3 - could not run");
  assert.equal(chipText({ name: "mystery", label: "", summary: "", ok: true }), "Used tool");
  assert.match(chipDetail({ name: "x", ok: false, summary: "invalid call" }), /Nothing was changed/);
  assert.match(chipDetail({ name: "x", ok: true, summary: "1 result" }), /Read-only/);
});
test("consent carries the tools switch (default on)", () => {
  const sc = [{ account: "a", folder: "INBOX", allowed: true }];
  const red = { stripQuotes: true, shortenLinks: true, maskAddresses: false };
  assert.equal(buildConsent(sc, red).tools, true);
  assert.equal(buildConsent(sc, red, false).tools, false);
  const c = buildConsent(sc, red);
  c.scopes[0].allowed = false;
  assert.equal(sc[0].allowed, true, "scopes are copied");
});

// ---- proposals (P2) ----
const counts = (o = {}) => ({ total: 0, selected: 0, done: 0, failed: 0, skipped: 0, noop: 0, undone: 0, undoFailed: 0, ...o });

test("consent: suggest defaults on with tools and never without them", () => {
  const red = DEFAULT_SETTINGS.defaultRedaction;
  assert.equal(buildConsent([], red).suggest, true);
  assert.equal(buildConsent([], red, true, false).suggest, false);
  assert.equal(buildConsent([], red, false).suggest, false);
  assert.equal(buildConsent([], red, false, true).suggest, false, "no tools, no suggestions");
});

test("result text is built from the app's counts and is honest", () => {
  const r = (state, kind, c, error = "") => resultText({ state, kind, counts: counts(c), error });
  assert.match(r("proposed", "move", {}), /Nothing has been changed/);
  assert.equal(r("done", "move", { done: 42, selected: 42 }), "Done: 42 moved.");
  assert.equal(r("done", "trash", { done: 3, noop: 1 }), "Done: 3 moved to Trash, 1 already in place.");
  assert.equal(r("partially_done", "archive", { done: 38, failed: 3, skipped: 1 }), "Partly done: 38 archived, 4 not changed.");
  assert.match(r("failed", "move", { failed: 2 }, "No Trash folder."), /^Failed: nothing was changed\. No Trash folder\.$/);
  assert.equal(r("rejected", "move", {}), "Rejected. Nothing was changed.");
  assert.match(r("expired", "move", {}), /Nothing was changed/);
  assert.equal(r("undone", "move", { undone: 5 }), "Undone: 5 put back.");
  assert.match(r("done", "move", { done: 5, undoFailed: 2 }), /2 could not be put back/);
  assert.equal(r("done", "create_folder", { done: 1 }), "Folder created.");
  assert.match(r("executing", "move", { done: 10, selected: 40 }), /10 of 40/);
});

test("approve needs a ticked row and says how many will run", () => {
  const items = [{ idx: 0, selected: true }, { idx: 1, selected: false }, { idx: 2, selected: true }];
  assert.equal(selectedCount(items), 2);
  assert.deepEqual(selectedIdx(items), [0, 2]);
  assert.deepEqual(selectedIdx([]), [], "no rows known -> nothing is sent");
  assert.equal(approveLabel({ kind: "move" }, items), "Approve 2");
  assert.equal(approveLabel({ kind: "move" }, [{ selected: true }]), "Approve");
  assert.ok(canApprove({ state: "proposed", kind: "move" }, items));
  assert.ok(!canApprove({ state: "proposed", kind: "move" }, [{ selected: false }]));
  assert.ok(!canApprove({ state: "done", kind: "move" }, items), "only a waiting suggestion can be approved");
  assert.ok(canApprove({ state: "proposed", kind: "create_folder" }, []));
  assert.ok(isPending("proposed") && !isPending("approved") && isRunning("executing") && !isRunning("done"));
});

test("cards are placed on the assistant message of their turn", () => {
  const msgs = [
    { id: "u1", role: "user", createdAt: 100 }, { id: "a1", role: "assistant", createdAt: 105 },
    { id: "u2", role: "user", createdAt: 200 }, { id: "a2", role: "assistant", createdAt: 210 },
  ];
  const m = placeProposals(msgs, [{ id: 1, createdAt: 104 }, { id: 2, createdAt: 205 }, { id: 3, createdAt: 206 }]);
  assert.deepEqual(m.get("a1"), [1]);
  assert.deepEqual(m.get("a2"), [2, 3]);
  // a card newer than every message goes on the last reply; live ids win over timestamps
  assert.deepEqual(placeProposals(msgs, [{ id: 9, createdAt: 999 }]).get("a2"), [9]);
  const live = placeProposals(msgs, [{ id: 7, createdAt: 104 }], { a2: [7] });
  assert.deepEqual(live.get("a2"), [7]);
  assert.equal(live.get("a1"), undefined);
  assert.equal(placeProposals([{ id: "u", role: "user", createdAt: 1 }], [{ id: 1, createdAt: 1 }]).size, 0);
});

test("proposal tool chips read as suggestions", () => {
  assert.equal(chipText({ name: "propose_trash", label: "12 messages", summary: "waiting for approval", ok: true, running: false }), "Suggested moving to Trash: 12 messages - waiting for approval");
  assert.match(chipDetail({ name: "propose_move", ok: true, summary: "x", running: false }), /Nothing changes until you press Approve/);
});

// ---- drafts (P3) ----

const draft = (o = {}) => ({
  id: 1, convId: 1, kind: "reply", state: "proposed", title: "Reply to bob@shop.test: Re: Invoice", account: "me@example.org",
  to: ["bob@shop.test"], cc: ["carol@other.test"], subject: "Re: Invoice", body: "Paid today.", quote: "On x, Bob wrote:\n> hi", reason: "Bob asked",
  warnings: [], newAddresses: [], createdAt: 10, updatedAt: 10, ...o,
});

test("draft card: open stays available until dismissed, dismiss only while waiting; there is no send action", () => {
  assert.ok(canOpenDraft(draft()) && canDismissDraft(draft()) && draftPending("proposed"));
  assert.ok(canOpenDraft(draft({ state: "opened" })) && !canDismissDraft(draft({ state: "opened" })) && !draftPending("opened"));
  assert.ok(!canOpenDraft(draft({ state: "dismissed" })) && !canDismissDraft(draft({ state: "dismissed" })));
  // the view-model exports nothing that could send
  return import("./aivm.ts").then((m) => assert.deepEqual(Object.keys(m).filter((k) => /send/i.test(k)), []));
});

test("draft recipients mark addresses the mailbox has never seen (case-insensitive) and skip an empty Cc", () => {
  const r = draftRecipients(draft({ to: ["Bob@Shop.test", "new@person.test"], cc: [], newAddresses: ["NEW@person.test"] }));
  assert.equal(r.length, 1);
  assert.deepEqual(r[0], { label: "To", addresses: [{ address: "Bob@Shop.test", isNew: false }, { address: "new@person.test", isNew: true }] });
  const r2 = draftRecipients(draft({ newAddresses: ["carol@other.test"] }));
  assert.deepEqual(r2.map((x) => x.label), ["To", "Cc"]);
  assert.equal(r2[1].addresses[0].isNew, true);
});

test("draft preview cuts long bodies at a line or word boundary and says so", () => {
  assert.deepEqual(draftPreview("  short \r\nbody "), { text: "short \nbody", cut: false });
  const long = Array.from({ length: 80 }, (_, i) => `line number ${i}`).join("\n");
  const p = draftPreview(long);
  assert.ok(p.cut && p.text.length <= DRAFT_PREVIEW_CHARS && !p.text.endsWith(" ") && long.startsWith(p.text));
  assert.ok(/line number \d+$/.test(p.text), "ends on a whole line");
  const words = "word ".repeat(300);
  const w = draftPreview(words, 100);
  assert.ok(w.cut && w.text.length <= 100 && /word$/.test(w.text));
  assert.equal(draftPreview("x".repeat(50), 10).text.length, 10, "no boundary: hard cut");
});

test("draft status text never claims the mail was sent", () => {
  for (const s of ["proposed", "opened", "dismissed"]) {
    const t = draftStatusText({ state: s });
    assert.ok(/nothing|only if/i.test(t) && !/\bit was sent\b|\bsent!/i.test(t), t);
  }
  assert.match(draftStatusText({ state: "proposed" }), /Nothing has been sent/);
  assert.match(draftStatusText({ state: "opened" }), /only if you press Send/);
  assert.equal(draftHeading({ kind: "reply" }), "Draft reply");
  assert.equal(draftHeading({ kind: "new" }), "Draft message");
  assert.match(AI_DRAFT_BANNER, /^Drafted by AI .+ review before sending$/);
});

test("draft chips: verb and an honest explanation", () => {
  assert.equal(chipText({ name: "draft_reply", label: "m:3", summary: "draft ready for review", ok: true, running: false }), "Drafted a reply: m:3 - draft ready for review");
  assert.match(chipText({ name: "draft_new", label: "Hello", summary: "", ok: true }), /^Drafted a new message: Hello/);
  assert.match(chipDetail({ name: "draft_reply", ok: true, summary: "x", running: false }), /Nothing was sent/);
});

test("draft cards are placed like suggestions: live ones stay on their message, stored ones go to the turn's reply", () => {
  const msgs = [
    { id: "u1", role: "user", createdAt: 1 }, { id: "a1", role: "assistant", createdAt: 5 },
    { id: "u2", role: "user", createdAt: 6 }, { id: "a2", role: "assistant", createdAt: 9 },
  ];
  const m = placeDrafts(msgs, [{ id: 1, createdAt: 4 }, { id: 2, createdAt: 8 }]);
  assert.deepEqual(m.get("a1"), [1]);
  assert.deepEqual(m.get("a2"), [2]);
  assert.deepEqual(placeDrafts(msgs, [{ id: 1, createdAt: 4 }], { a2: [1] }).get("a2"), [1]);
});

test("withTimeout names the command that did not answer", async () => {
  const { withTimeout } = await import("./aivm.ts");
  assert.equal(await withTimeout(Promise.resolve(5), "ai_list_profiles", 50), 5);
  await assert.rejects(withTimeout(Promise.reject("nope"), "ai_list_profiles", 50), (e) => e === "nope");
  const never = new Promise(() => {});
  await assert.rejects(withTimeout(never, "ai_get_settings", 20), (e) => typeof e === "string" && e.includes("ai_get_settings") && e.includes("did not respond"));
});

// ---- calendar tools ----
test("range label is compact and falls back to the raw text", () => {
  assert.equal(formatRangeLabel("2026-11-05 \u2192 2026-11-12"), "5\u201312 Nov");
  assert.equal(formatRangeLabel("2026-11-28 \u2192 2026-12-03"), "28 Nov \u2013 3 Dec");
  assert.equal(formatRangeLabel("2026-12-28 \u2192 2027-01-03"), "28 Dec 2026 \u2013 3 Jan 2027");
  assert.equal(formatRangeLabel("30 min, 2026-11-10 \u2192 2026-11-20"), "30 min, 10\u201320 Nov");
  assert.equal(formatRangeLabel("2026-11-10T09:00 \u2192 2026-11-10T11:30"), "10 Nov 09:00\u201311:30");
  assert.equal(formatRangeLabel("2026-11-10T09:00 \u2192 2026-11-12T11:30"), "10\u201312 Nov");
  assert.equal(formatRangeLabel("e:12"), "e:12");
  assert.equal(formatRangeLabel("2026-13-10 \u2192 2026-13-12"), "2026-13-10 \u2192 2026-13-12");
  assert.equal(formatRangeLabel("next week"), "next week");
});

test("calendar chips: friendly verbs and range", () => {
  const c = (name, label, summary, o = {}) => chipText({ name, label, summary, ok: true, ...o });
  assert.equal(c("list_events", "2026-11-05 \u2192 2026-11-12", "14 events"), "Checked calendar: 5\u201312 Nov \u2014 14 events");
  assert.equal(c("free_busy", "2026-11-05 \u2192 2026-11-12", "3 busy blocks"), "Checked availability: 5\u201312 Nov \u2014 3 busy blocks");
  assert.equal(c("find_free_slots", "30 min, 2026-11-10 \u2192 2026-11-20", "1 slot"), "Looked for free time: 30 min, 10\u201320 Nov \u2014 1 slot");
  assert.equal(c("get_event", "e:12", "read"), "Checked calendar: e:12 \u2014 read");
  assert.equal(c("list_calendars", "", "2 calendars"), "Listed calendars \u2014 2 calendars");
  assert.equal(c("list_events", "2026-11-05 \u2192 2026-11-12", "", { running: true }), "Checked calendar: 5\u201312 Nov \u2026");
  assert.equal(c("list_events", "x", "failed", { ok: false }), "Checked calendar: x - could not run");
  assert.equal(c("propose_event", "Lunch", "waiting"), "Suggested an event: Lunch - waiting");
  assert.equal(c("propose_event_change", "", ""), "Suggested a change");
  assert.equal(c("propose_event_cancel", "", ""), "Suggested cancelling an event");
  assert.equal(c("propose_invite_response", "", ""), "Suggested an answer to an invitation");
  assert.equal(c("search_mail", "from:x", "2 results"), "Searched mail: from:x - 2 results", "mail chips unchanged");
});

test("event proposals: kinds, labels, approve rules, result text", () => {
  for (const k of ["event_create", "event_change", "event_cancel", "invite_response"]) assert.ok(isEventKind(k), k);
  for (const k of ["move", "archive", "trash", "flag", "create_folder"]) assert.ok(!isEventKind(k), k);
  const ev = (notify) => ({ notify });
  assert.equal(approveLabel({ kind: "event_create", event: ev(true) }, []), "Approve and send invitations");
  assert.equal(approveLabel({ kind: "event_change", event: ev(true) }, []), "Approve and send invitations");
  assert.equal(approveLabel({ kind: "event_cancel", event: ev(true) }, []), "Approve and send cancellation");
  assert.equal(approveLabel({ kind: "invite_response", event: ev(true) }, []), "Approve and send answer");
  assert.equal(approveLabel({ kind: "event_create", event: ev(false) }, []), "Approve");
  assert.equal(approveLabel({ kind: "event_create" }, []), "Approve");
  assert.ok(canApprove({ state: "proposed", kind: "event_create" }, []), "no rows, still enabled");
  assert.ok(!canApprove({ state: "done", kind: "event_create" }, []));
  const r = (state, error = "") => resultText({ state, kind: "event_create", counts: counts(), error });
  assert.equal(r("done"), "Done.");
  assert.equal(r("undone"), "Undone.");
  assert.match(r("failed", "Server said no."), /^Failed: nothing was changed\. Server said no\.$/);
  assert.match(r("rejected"), /Rejected/);
  assert.match(r("expired"), /Expired/);
  assert.match(r("proposed"), /Nothing has been changed/);
  assert.equal(eventHeading("event_cancel"), "Suggested cancellation");
});

test("warning tones", () => {
  assert.equal(warningTone("Approving will send invitations by email to 2 people (a, b)"), "notify");
  assert.equal(warningTone("Approving will send your answer by email to x"), "notify");
  assert.equal(warningTone("Conflicts with 'Gym'"), "caution");
  assert.equal(warningTone("ravi@example.org never appears in your mail."), "caution");
  assert.equal(warningTone("Approving will also create a Zoom meeting with your connected Zoom account"), "notify");
  assert.equal(warningTone("Zoom meeting not added: Zoom is not connected"), "info");
  assert.equal(warningTone("A Zoom meeting will be created."), "info");
});

test("consent carries calendar choices; old consents read as off", () => {
  const red = DEFAULT_SETTINGS.defaultRedaction;
  const cals = [{ id: "a", allowed: true }, { id: "b", allowed: false }];
  const on = buildConsent([], red, true, true, { enabled: true, calendars: cals });
  assert.deepEqual(on.calendar, { enabled: true, calendars: [{ id: "a", allowed: true }, { id: "b", allowed: false }] });
  assert.ok(calendarOn(on));
  assert.deepEqual(cals[0], { id: "a", allowed: true }, "input untouched");
  const off = buildConsent([], red, true, true, { enabled: false, calendars: cals });
  assert.deepEqual(off.calendar, { enabled: false, calendars: [{ id: "a", allowed: false }, { id: "b", allowed: false }] });
  const noTools = buildConsent([], red, false, false, { enabled: true, calendars: cals });
  assert.equal(noTools.calendar.enabled, false, "no tools, no calendar");
  assert.ok(noTools.calendar.calendars.every((c) => !c.allowed));
  const none = buildConsent([], red, true, true, { enabled: true, calendars: [{ id: "a", allowed: false }] });
  assert.equal(none.calendar.enabled, false, "nothing ticked = off");
  assert.deepEqual(buildConsent([], red).calendar, { enabled: false, calendars: [] });
  assert.equal(calendarOn({ scopes: [] }), false, "old consent");
});

test("every calendar is ticked when a chat starts", () => {
  const r = withDefaultTicks([{ id: "work", name: "Work" }, { id: "personal", name: "Me" }, { id: "holidays", name: "H" }]);
  assert.deepEqual(r.map((c) => [c.id, c.allowed]), [["work", true], ["personal", true], ["holidays", true]]);
  assert.equal(r[1].name, "Me");
});

test("consent heading names provider label and host", () => {
  assert.equal(consentHeading("Endless Tokens", "api.endlesstokens.ai", "m1"), "Start a conversation with Endless Tokens (api.endlesstokens.ai, m1)?");
  assert.equal(consentHeading(" ", "api.x.com", "m"), "Start a conversation with api.x.com (m)?");
  assert.equal(consentHeading("api.x.com", "api.x.com", "m"), "Start a conversation with api.x.com (m)?");
});

// ---- Edit permissions ----
test("edit heading names the provider and host and says it is an existing chat", () => {
  const h = editHeading("Endless Tokens", "api.endlesstokens.ai", "m1");
  assert.match(h, /Change what this chat may see/);
  assert.ok(h.includes("Endless Tokens") && h.includes("api.endlesstokens.ai") && h.includes("m1"));
  assert.equal(editHeading("api.x.ai", "api.x.ai", "g"), "Change what this chat may see with api.x.ai (g)?");
  assert.match(consentHeading("P", "h", "m"), /^Start a conversation/, "new-chat heading untouched");
});

test("edit prefill: current ticks kept, created folders ticked once, new folders offered unticked, vanished ones dropped", () => {
  const saved = [
    { account: "a", folder: "INBOX", allowed: true },
    { account: "a", folder: "Junk", allowed: false },
    { account: "a", folder: "Marketing", allowed: true }, // MIRA created it earlier (backend merges it in, ticked)
    { account: "a", folder: "Gone", allowed: true },
    { account: "a", folder: "Marketing", allowed: true }, // duplicate must not appear twice
  ];
  const live = ["INBOX", "Junk", "Marketing", "Receipts"].map((name) => ({ account: "a", name, selectable: true })).concat([{ account: "a", name: "[Hidden]", selectable: false }, { account: "other", name: "X", selectable: true }]);
  const out = prefillScopes(saved, live);
  assert.deepEqual(out.map((s) => [s.folder, s.allowed]), [["INBOX", true], ["Junk", false], ["Marketing", true], ["Receipts", false]]);
  assert.ok(out.every((s) => s.account === "a"), "other accounts are not offered");
  saved[0].allowed = false;
  assert.equal(out[0].allowed, true, "prefill copies");
  // when the live list is unknown (offline), the saved scopes are shown as they are
  assert.deepEqual(prefillScopes(saved.slice(0, 2), []).map((s) => s.folder), ["INBOX", "Junk"]);
});

test("edit prefill: calendars as saved, new ones unticked; a calendar that was off starts all ticked", () => {
  const live = [{ id: "a", name: "A" }, { id: "b", name: "B" }, { id: "c", name: "C" }];
  const on = prefillCalendars({ enabled: true, calendars: [{ id: "a", allowed: true }, { id: "b", allowed: false }] }, live);
  assert.deepEqual(on.map((c) => c.allowed), [true, false, false]);
  assert.deepEqual(prefillCalendars({ enabled: false, calendars: [] }, live).map((c) => c.allowed), [true, true, true]);
  assert.deepEqual(prefillCalendars(undefined, live).map((c) => c.allowed), [true, true, true]);
});

test("edit round trip: saving untouched ticks changes nothing; widening/narrowing is summarised like the backend line", () => {
  const red = DEFAULT_SETTINGS.defaultRedaction;
  const cal = { enabled: true, calendars: [{ id: "a", allowed: true }] };
  const old = buildConsent([{ account: "a", folder: "INBOX", allowed: true }, { account: "a", folder: "Archive", allowed: false }], red, true, true, cal);
  const same = buildConsent(prefillScopes(old.scopes, [{ account: "a", name: "INBOX", selectable: true }, { account: "a", name: "Archive", selectable: true }]), red, true, true, cal);
  assert.equal(consentChangeSummary(old, same), null);
  const wide = buildConsent(same.scopes.map((s) => ({ ...s, allowed: true })), red, true, true, cal);
  assert.equal(consentChangeSummary(old, wide), "Permissions changed by you: +1 folder");
  const narrow = buildConsent([{ account: "a", folder: "INBOX", allowed: false }, { account: "a", folder: "Archive", allowed: false }], red, true, true, { enabled: false, calendars: cal.calendars });
  assert.equal(consentChangeSummary(old, narrow), "Permissions changed by you: -1 folder, calendar off");
  assert.equal(consentChangeSummary(old, buildConsent(old.scopes, red, true, false, cal)), "Permissions changed by you: suggestions off");
  assert.equal(consentChangeSummary(old, buildConsent(old.scopes, { ...red, maskAddresses: !red.maskAddresses }, true, true, cal)), "Permissions changed by you: redaction changed");
});

test("tone notice names label and host, and says what is not sent", () => {
  const n = toneNotice("Endless Tokens", "api.endlesstokens.ai");
  assert.equal(n, "This draft's own text (no quoted message, no signature, no recipients) will be sent once to Endless Tokens (api.endlesstokens.ai) to check its tone. Email addresses you typed in the text are included unless address masking is on in Settings > AI. MiraMail does not store it; Endless Tokens's own policy applies. Nothing is sent until you press Send.");
  assert.equal(toneNotice("api.example.org", "api.example.org"), "This draft's own text (no quoted message, no signature, no recipients) will be sent once to api.example.org to check its tone. Email addresses you typed in the text are included unless address masking is on in Settings > AI. MiraMail does not store it; api.example.org's own policy applies. Nothing is sent until you press Send.");
  assert.match(toneNotice("  ", "h.example"), /sent once to h\.example to check/);
  assert.doesNotMatch(toneNotice("X", "h.example"), /Nothing is stored/);
  assert.equal(EDIT_KEEPS_READ_NOTE, "Mail MIRA read from a folder you remove is no longer sent with later messages. Your messages and MIRA's earlier replies stay in this chat; start a new chat to drop them.");
  assert.match(TONE_SETUP_HINT, /Set up an AI provider in Settings/);
});

test("follow-up after the user decides cards", () => {
  const c = { total: 3, selected: 3, done: 3, failed: 0, skipped: 0, noop: 0, undone: 0, undoFailed: 0 };
  assert.equal(decidedAll([]), false);
  assert.equal(decidedAll([{ state: "done" }, { state: "proposed" }]), false);
  assert.equal(decidedAll([{ state: "done" }, { state: "executing" }]), false);
  assert.equal(decidedAll([{ state: "done" }, { state: "rejected" }, { state: "failed" }]), true);
  const one = followUpText([{ id: 1, state: "done", kind: "move", counts: c }]);
  assert.match(one, /decided on your suggestion:\n1\. move: Done: 3 moved\./);
  assert.match(one, /carry on/);
  const two = followUpText([{ id: 1, state: "done", kind: "move", counts: c }, { id: 2, state: "rejected", kind: "create_folder", counts: c }]);
  assert.match(two, /suggestions:/);
  assert.match(two, /2\. create folder: I rejected it/);
});
