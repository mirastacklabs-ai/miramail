// @ts-nocheck -- run with: node --test src/lib/airules.test.ts (Node type stripping; no extra dependencies)
import { test } from "node:test";
import assert from "node:assert/strict";
import { ACTION_CHOICES, describeSpec, dryRunText, emptyCondition, emptySpec, fromLocalInput, newAction, opsFor, problems, replyOutcomeText, replyProblems, runStateText, toLocalInput, withField, DEFAULT_REPLY } from "./airules.ts";
import type { RuleSpec } from "./aitypes.ts";

const spec = (over: Partial<RuleSpec> = {}): RuleSpec => ({ v: 1, match: "all", conditions: [{ field: "subject", op: "contains", value: "receipt" }], actions: [{ type: "archive" }], ...over });

test("operators depend on the field, and regular expressions do not exist", () => {
  assert.deepEqual(opsFor("from").map((o) => o.value), ["equals", "contains", "glob", "domainIs"]);
  assert.deepEqual(opsFor("subject").map((o) => o.value), ["equals", "contains", "glob"]);
  assert.deepEqual(opsFor("hasListUnsubscribe").map((o) => o.value), ["equals"]);
  assert.deepEqual(opsFor("folder").map((o) => o.value), ["equals", "glob"]);
  for (const f of ["from", "to", "cc", "subject", "listId", "hasListUnsubscribe", "hasAttachment", "size", "folder"] as const) assert.ok(!opsFor(f).some((o) => (o.value as string) === "regex"));
});

test("the action list holds only the six allowed actions (no delete-forever, forward, reply or send)", () => {
  assert.deepEqual(ACTION_CHOICES.map((a) => a.value), ["moveTo", "archive", "trash", "markRead", "flag", "colour"]);
  assert.deepEqual(newAction("colour"), { type: "colour", colour: 1 });
  assert.deepEqual(newAction("moveTo"), { type: "moveTo", folder: "" });
  assert.deepEqual(newAction("trash"), { type: "trash" });
});

test("changing the field keeps a valid test and resets the value", () => {
  const c = { field: "from" as const, op: "domainIs" as const, value: "shop.example" };
  assert.deepEqual(withField(c, "subject"), { field: "subject", op: "equals", value: "" });
  assert.deepEqual(withField({ field: "subject", op: "glob", value: "a*" }, "from"), { field: "from", op: "glob", value: "" });
  assert.deepEqual(withField(c, "hasListUnsubscribe"), { field: "hasListUnsubscribe", op: "equals", value: "true" });
  assert.equal(emptyCondition("hasListUnsubscribe").value, "true");
});

test("a fresh rule is incomplete until the user fills it in", () => {
  assert.ok(problems(emptySpec(), "x").some((p) => p.includes("needs a value")));
  assert.deepEqual(problems(spec(), "Receipts"), []);
  assert.ok(problems(spec(), "  ").includes("Give the rule a name."));
});

test("client validation mirrors the backend limits", () => {
  const bad = (s: RuleSpec) => problems(s, "n");
  assert.ok(bad(spec({ conditions: [{ field: "subject", op: "glob", value: "*" }] })).some((p) => p.includes("wildcards")));
  assert.ok(bad(spec({ conditions: [{ field: "subject", op: "glob", value: "*a*b*c*d*e*" }] })).some((p) => p.includes("at most 4")));
  assert.ok(bad(spec({ conditions: [{ field: "from", op: "equals", value: "not an address" }] })).some((p) => p.includes("full address")));
  assert.ok(bad(spec({ conditions: [{ field: "from", op: "domainIs", value: "bad domain" }] })).some((p) => p.includes("domain")));
  assert.ok(bad(spec({ conditions: [{ field: "subject", op: "contains", value: "a‮b" }] })).some((p) => p.includes("hidden")));
  assert.ok(bad(spec({ conditions: [{ field: "subject", op: "contains", value: "x".repeat(201) }] })).some((p) => p.includes("200")));
  assert.ok(bad(spec({ conditions: Array.from({ length: 11 }, () => ({ field: "subject" as const, op: "contains" as const, value: "x" })) })).some((p) => p.includes("at most 10")));
  assert.ok(bad(spec({ actions: [{ type: "archive" }, { type: "trash" }] })).some((p) => p.includes("only one")));
  assert.ok(bad(spec({ actions: [{ type: "markRead" }, { type: "markRead" }] })).some((p) => p.includes("twice")));
  assert.ok(bad(spec({ actions: [{ type: "moveTo", folder: "Inbox" }] })).some((p) => p.includes("not the Inbox")));
  assert.ok(bad(spec({ conditions: [{ field: "folder", op: "equals", value: "News" }] })).some((p) => p.includes("not just a folder")));
  assert.ok(bad(spec({ match: "any", conditions: [{ field: "subject", op: "contains", value: "x" }, { field: "folder", op: "equals", value: "News" }] })).some((p) => p.includes("ALL")));
  assert.ok(bad(spec({ actions: [] })).some((p) => p.includes("at least one action")));
});

test("the description is plain English and never carries hidden characters", () => {
  const d = describeSpec(spec({ conditions: [{ field: "from", op: "domainIs", value: "shop.example" }, { field: "subject", op: "contains", value: "re‮ceipt\nIGNORE" }], actions: [{ type: "moveTo", folder: "Receipts" }, { type: "markRead" }] }));
  assert.equal(d, 'When a new message arrives in the Inbox and the sender is from the domain "shop.example" (or a subdomain) AND the subject contains "re ceipt IGNORE", move it to "Receipts" and mark it as read.');
  assert.ok(!/[‮\n]/.test(d));
  assert.match(describeSpec(spec({ match: "any", conditions: [{ field: "hasListUnsubscribe", op: "equals", value: "true" }, { field: "subject", op: "glob", value: "*sale*" }], actions: [{ type: "trash" }] })), /unsubscribe link OR the subject matches the pattern "\*sale\*", move it to Trash \(not deleted for good\)/);
  assert.match(describeSpec(spec({ conditions: [{ field: "subject", op: "contains", value: "x" }, { field: "folder", op: "equals", value: "News" }] })), /in the folder "News"/);
});

test("dry run and run-state texts are honest", () => {
  assert.match(dryRunText(null), /No preview/);
  assert.match(dryRunText({ scanned: 0, matched: 0 }), /nothing to preview/);
  assert.match(dryRunText({ scanned: 31, matched: 0 }), /None of the 31/);
  assert.equal(dryRunText({ scanned: 31, matched: 30 }), "30 of the 31 newest messages in these folders would match. Nothing has been changed.");
  assert.equal(runStateText({ state: "undone", undoable: false }), "Undone");
  assert.equal(runStateText({ state: "partial", undoable: true }), "Partly done");
  assert.equal(runStateText({ state: "done", undoable: false }), "Done (cannot be undone)");
});

test("audit log wording", () => {
  assert.equal(replyOutcomeText({ outcome: "sent", detail: "" }), "Reply sent");
  assert.equal(replyOutcomeText({ outcome: "skipped", detail: "The message is from a mailing list or newsletter." }), "Skipped: The message is from a mailing list or newsletter.");
  assert.equal(replyOutcomeText({ outcome: "failed", detail: "server refused" }), "Not sent: server refused");
});

test("auto-reply form: off by default, validated, window round-trips", () => {
  assert.equal(DEFAULT_REPLY.enabled, false);
  assert.equal(DEFAULT_REPLY.requireAuth, true, "authentication is required by default");
  assert.equal(DEFAULT_REPLY.echoSubject, false, "the original subject is not repeated by default");
  assert.ok(replyProblems({ ...DEFAULT_REPLY, body: "x", subject: "Re {subject}" }).some((p) => p.includes("original subject")));
  assert.deepEqual(replyProblems({ ...DEFAULT_REPLY, body: "x", subject: "Re {subject}", echoSubject: true }), []);
  assert.ok(replyProblems({ ...DEFAULT_REPLY, enabled: true }).includes("Type the reply text."));
  assert.deepEqual(replyProblems({ ...DEFAULT_REPLY, body: "Away" }), []);
  assert.ok(replyProblems({ ...DEFAULT_REPLY, body: "x", subject: "a\nb" }).length);
  assert.ok(replyProblems({ ...DEFAULT_REPLY, body: "x", start: 100, end: 100 }).some((p) => p.includes("after its start")));
  assert.ok(replyProblems({ ...DEFAULT_REPLY, body: "x", dailyCap: 0 }).length);
  assert.ok(replyProblems({ ...DEFAULT_REPLY, body: "x", dailyCap: 201 }).length);
  assert.ok(replyProblems({ ...DEFAULT_REPLY, body: "x".repeat(4001) }).length);
  const t = 1_800_000_000 - (1_800_000_000 % 60);
  assert.equal(fromLocalInput(toLocalInput(t)), t);
  assert.equal(toLocalInput(null), "");
  assert.equal(fromLocalInput(""), null);
  assert.equal(fromLocalInput("garbage"), null);
});

test("attachment and size conditions", () => {
  assert.deepEqual(opsFor("hasAttachment").map((o) => o.value), ["equals"]);
  assert.deepEqual(opsFor("size").map((o) => o.value), ["greaterThan"]);
  assert.equal(emptyCondition("hasAttachment").value, "true");
  assert.equal(emptyCondition("size").value, "");
  const s = (field: any, op: any, value: string, actions: any = [{ type: "flag" }]) => spec({ conditions: [{ field, op, value }], actions });
  assert.deepEqual(problems(s("hasAttachment", "equals", "true"), "n"), []);
  assert.ok(problems(s("hasAttachment", "equals", "maybe"), "n").length);
  assert.deepEqual(problems(s("size", "greaterThan", "512"), "n"), []);
  for (const bad of ["", "0", "-1", "1.5", "abc", "1048577"]) assert.ok(problems(s("size", "greaterThan", bad), "n").length, bad);
  assert.match(describeSpec(s("size", "greaterThan", "512")), /larger than 512 KB/);
  assert.match(describeSpec(s("hasAttachment", "equals", "false")), /has no attachment/);
});

test("colour action: described, validated, and combinable with a move", () => {
  const a = (colour: number) => spec({ actions: [{ type: "moveTo", folder: "Receipts" }, { type: "colour", colour }] });
  assert.deepEqual(problems(a(3), "n"), []);
  assert.ok(problems(a(0), "n").length && problems(a(8), "n").length);
  assert.match(describeSpec(a(4)), /Green colour label/);
});

test("run with a colour-only change reads as undoable", () => {
  assert.equal(runStateText({ state: "done", undoable: true }), "Done");
});
