// Pure helpers for local rules and the template auto-reply (AI chat P4). No model is involved in either.
// The backend validates everything again; these only give instant feedback and plain-English text.
import type { AutoReplyConfig, ReplyLogRow, RuleAction, RuleCondition, RuleField, RuleOp, RuleRun, RuleSpec } from "./aitypes";

export const AUTOMATION_NOTICE =
  "Rules and automatic replies run only while MiraMail is open on this Mac. For the times it is closed, set up your mail provider's own vacation responder or filters.";
export const MAX_CONDITIONS = 10;
export const MAX_ACTIONS = 3;
export const MAX_VALUE = 200;
export const MAX_GLOB_STARS = 4;
export const MAX_SIZE_KB = 1_048_576;
/** Colour label names; index = the value stored (1..7). Same palette as the message list. */
export const COLOUR_NAMES = ["", "Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Grey"] as const;
export const MAX_REPLY_BODY = 4000;
export const MAX_REPLY_SUBJECT = 200;

export const FIELDS: { value: RuleField; label: string }[] = [
  { value: "from", label: "Sender" },
  { value: "to", label: "To" },
  { value: "cc", label: "Cc" },
  { value: "subject", label: "Subject" },
  { value: "listId", label: "Mailing-list ID" },
  { value: "hasListUnsubscribe", label: "Has an unsubscribe link" },
  { value: "hasAttachment", label: "Has an attachment" },
  { value: "size", label: "Size" },
  { value: "folder", label: "Folder" },
];

const OP_LABEL: Record<RuleOp, string> = { equals: "is exactly", contains: "contains", glob: "matches pattern (* and ?)", domainIs: "is from domain", greaterThan: "is larger than (KB)" };

/** Which tests make sense for a field (same table as the backend). */
export function opsFor(field: RuleField): { value: RuleOp; label: string }[] {
  const allowed: RuleOp[] =
    field === "from" || field === "to" || field === "cc" ? ["equals", "contains", "glob", "domainIs"]
    : field === "subject" || field === "listId" ? ["equals", "contains", "glob"]
    : field === "hasListUnsubscribe" || field === "hasAttachment" ? ["equals"]
    : field === "size" ? ["greaterThan"]
    : ["equals", "glob"];
  return allowed.map((value) => ({ value, label: field === "hasListUnsubscribe" || field === "hasAttachment" ? "is" : OP_LABEL[value] }));
}

const isBoolField = (f: RuleField) => f === "hasListUnsubscribe" || f === "hasAttachment";
export function emptyCondition(field: RuleField = "from"): RuleCondition {
  return { field, op: opsFor(field)[0].value, value: isBoolField(field) ? "true" : "" };
}
export function emptySpec(): RuleSpec {
  return { v: 1, match: "all", conditions: [emptyCondition("from")], actions: [{ type: "archive" }] };
}
/** Switching the field keeps the test when it still applies, otherwise falls back to the first one. */
export function withField(c: RuleCondition, field: RuleField): RuleCondition {
  const ops = opsFor(field);
  const op = ops.some((o) => o.value === c.op) ? c.op : ops[0].value;
  return { field, op, value: isBoolField(field) ? "true" : field === c.field ? c.value : "" };
}

const plainAddress = (v: string) => /^[^\s<>",;()@]+@[^\s<>",;()@]+$/.test(v);
const plainDomain = (v: string) => v.length > 0 && v.length <= 253 && /^[a-z0-9-]+(\.[a-z0-9-]+)*$/i.test(v);
const hiddenChars = /[\u0000-\u001f\u007f-\u009f­​-‏‪-‮⁠-⁯﻿]/;

/** What is wrong with the rule, in plain words (empty = fine). Mirrors the backend rules. */
export function problems(spec: RuleSpec, name = ""): string[] {
  const out: string[] = [];
  if (!name.trim()) out.push("Give the rule a name.");
  if (spec.conditions.length === 0) out.push("Add at least one condition.");
  if (spec.conditions.length > MAX_CONDITIONS) out.push(`Use at most ${MAX_CONDITIONS} conditions.`);
  if (spec.actions.length === 0) out.push("Add at least one action.");
  if (spec.actions.length > MAX_ACTIONS) out.push(`Use at most ${MAX_ACTIONS} actions.`);
  spec.conditions.forEach((c, i) => {
    const n = `Condition ${i + 1}`;
    const v = c.value.trim();
    if (!v) { out.push(`${n} needs a value.`); return; }
    if (v.length > MAX_VALUE) out.push(`${n} is longer than ${MAX_VALUE} characters.`);
    if (hiddenChars.test(v)) out.push(`${n} contains control or hidden characters.`);
    if (isBoolField(c.field) && v !== "true" && v !== "false") out.push(`${n} must be yes or no.`);
    if (c.field === "size" && !(/^[0-9]+$/.test(v) && Number(v) >= 1 && Number(v) <= MAX_SIZE_KB)) out.push(`${n} needs a whole number of kilobytes from 1 to ${MAX_SIZE_KB}.`);
    if (c.op === "equals" && (c.field === "from" || c.field === "to" || c.field === "cc") && !plainAddress(v)) out.push(`${n} needs a full address such as name@example.com.`);
    if (c.op === "domainIs" && !plainDomain(v)) out.push(`${n} needs a domain such as example.com.`);
    if (c.op === "glob") {
      if ((v.match(/\*/g)?.length ?? 0) > MAX_GLOB_STARS) out.push(`${n}: use at most ${MAX_GLOB_STARS} '*'.`);
      if (/^[*?]+$/.test(v)) out.push(`${n}: a pattern of only wildcards would match every message.`);
    }
  });
  if (spec.conditions.length && spec.conditions.every((c) => c.field === "folder")) out.push("Add a condition that is not just a folder.");
  if (spec.match === "any" && spec.conditions.some((c) => c.field === "folder")) out.push("A folder condition only works when ALL conditions must match.");
  const dest = spec.actions.filter((a) => a.type === "moveTo" || a.type === "archive" || a.type === "trash").length;
  if (dest > 1) out.push("Choose only one of move, archive and trash.");
  if (new Set(spec.actions.map((a) => a.type)).size !== spec.actions.length) out.push("An action is listed twice.");
  for (const a of spec.actions) if (a.type === "colour" && !(Number.isInteger(a.colour) && a.colour >= 1 && a.colour <= 7)) out.push("Choose a colour label.");
  for (const a of spec.actions) if (a.type === "moveTo" && (!a.folder.trim() || a.folder.trim().toLowerCase() === "inbox")) out.push("Choose a folder to move to (not the Inbox).");
  return out;
}

const q = (s: string) => `"${s.replace(/[\u0000-\u001f\u007f-\u009f​-‏‪-‮⁠-⁯﻿]/g, " ").replace(/\s+/g, " ").trim().slice(0, 80)}"`;
const FIELD_NOUN: Record<RuleField, string> = { from: "sender", to: "recipient (To)", cc: "Cc", subject: "subject", listId: "mailing-list ID", hasListUnsubscribe: "unsubscribe link", hasAttachment: "attachment", size: "size", folder: "folder" };

/** Live preview text in the editor. Saved rules show the backend's own description. */
export function describeSpec(spec: RuleSpec): string {
  const parts = spec.conditions.filter((c) => c.field !== "folder").map((c) => {
    if (c.field === "hasListUnsubscribe") return c.value === "true" ? "the message has an unsubscribe link" : "the message has no unsubscribe link";
    if (c.field === "hasAttachment") return c.value === "true" ? "the message has an attachment" : "the message has no attachment";
    if (c.field === "size") return `the message is larger than ${c.value} KB`;
    const noun = FIELD_NOUN[c.field];
    switch (c.op) {
      case "equals": return `the ${noun} is exactly ${q(c.value)}`;
      case "contains": return `the ${noun} contains ${q(c.value)}`;
      case "glob": return `the ${noun} matches the pattern ${q(c.value)}`;
      default: return `the ${noun} is from the domain ${q(c.value)} (or a subdomain)`;
    }
  });
  const folders = spec.conditions.filter((c) => c.field === "folder").map((c) => q(c.value));
  const place = folders.length ? `the folder ${folders.join(" and ")}` : "the Inbox";
  const acts = spec.actions.map((a) => actionText(a));
  const when = parts.length ? parts.join(spec.match === "any" ? " OR " : " AND ") : "(no condition yet)";
  return `When a new message arrives in ${place} and ${when}, ${acts.length ? acts.join(" and ") : "(no action yet)"}.`;
}

export function actionText(a: RuleAction): string {
  switch (a.type) {
    case "moveTo": return `move it to ${q(a.folder)}`;
    case "archive": return "archive it";
    case "trash": return "move it to Trash (not deleted for good)";
    case "markRead": return "mark it as read";
    case "colour": return `give it the ${COLOUR_NAMES[a.colour] ?? "?"} colour label`;
    default: return "flag it";
  }
}

export const ACTION_CHOICES: { value: RuleAction["type"]; label: string }[] = [
  { value: "moveTo", label: "Move to a folder" },
  { value: "archive", label: "Archive" },
  { value: "trash", label: "Move to Trash" },
  { value: "markRead", label: "Mark as read" },
  { value: "flag", label: "Flag" },
  { value: "colour", label: "Colour label" },
];
export const newAction = (t: RuleAction["type"]): RuleAction => (t === "moveTo" ? { type: "moveTo", folder: "" } : t === "colour" ? { type: "colour", colour: 1 } : ({ type: t } as RuleAction));

// ---- dry run + runs ----

export function dryRunText(d: { scanned: number; matched: number } | null): string {
  if (!d) return "No preview available.";
  if (d.scanned === 0) return "There is no mail in the folders this rule covers yet, so there is nothing to preview.";
  return d.matched === 0
    ? `None of the ${d.scanned} newest messages in these folders would match.`
    : `${d.matched} of the ${d.scanned} newest messages in these folders would match. Nothing has been changed.`;
}

export function runStateText(r: Pick<RuleRun, "state" | "undoable">): string {
  switch (r.state) {
    case "undone": return "Undone";
    case "failed": return "Failed";
    case "partial": return "Partly done";
    default: return r.undoable ? "Done" : "Done (cannot be undone)";
  }
}

export function replyOutcomeText(r: Pick<ReplyLogRow, "outcome" | "detail">): string {
  switch (r.outcome) {
    case "sent": return "Reply sent";
    case "failed": return `Not sent${r.detail ? `: ${r.detail}` : ""}`;
    case "pending": return "Sending…";
    case "unknown": return "Probably sent (the connection broke before the server confirmed)";
    default: return `Skipped: ${r.detail || "no reason recorded"}`;
  }
}

// ---- auto-reply form ----

export const DEFAULT_REPLY: AutoReplyConfig = { enabled: false, subject: "", echoSubject: false, body: "", start: null, end: null, requireAuth: true, dailyCap: 20 };

const pad = (n: number) => String(n).padStart(2, "0");
/** unix seconds -> value of an input of type datetime-local, in local time ("" = none). */
export function toLocalInput(sec: number | null): string {
  if (sec == null) return "";
  const d = new Date(sec * 1000);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
/** Inverse of toLocalInput; invalid or empty input gives null. */
export function fromLocalInput(v: string): number | null {
  if (!v) return null;
  const t = new Date(v).getTime();
  return Number.isFinite(t) ? Math.floor(t / 1000) : null;
}

export function replyProblems(c: AutoReplyConfig): string[] {
  const out: string[] = [];
  if (c.enabled && !c.body.trim()) out.push("Type the reply text.");
  if (c.body.length > MAX_REPLY_BODY) out.push(`The reply text is longer than ${MAX_REPLY_BODY} characters.`);
  if (c.subject.length > MAX_REPLY_SUBJECT || /[\r\n]/.test(c.subject)) out.push("The subject must be one line of at most 200 characters.");
  if (c.subject.includes("{subject}") && !c.echoSubject) out.push("Turn on \"Include the original subject\" to use {subject}.");
  if (c.start != null && c.end != null && c.end <= c.start) out.push("The end of the window must be after its start.");
  if (!Number.isInteger(c.dailyCap) || c.dailyCap < 1 || c.dailyCap > 200) out.push("The daily limit must be between 1 and 200.");
  return out;
}

/** The warnings shown above the switch; deliberately blunt. */
export const AUTOREPLY_WARNINGS = [
  "This sends real email from your address, with the text you type here, to people who write to you.",
  "It never uses an AI model. It answers each sender at most once every 7 days, never answers newsletters, mailing lists, bounces or no-reply addresses, and stops at the daily limit.",
  "It only works while MiraMail is open on this Mac. Mail that arrives while it is closed is answered only if it is under 2 days old when MiraMail opens again; for real time off, also set up your provider's vacation responder.",
];
