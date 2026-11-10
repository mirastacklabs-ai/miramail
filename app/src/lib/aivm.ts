// Pure view-model helpers for the AI chat UI (no Svelte, no DOM): safe to run under node --test.
import type { AiMessage, AiSettings, CalendarConsent, Consent, ConsentScope, DraftState, DraftSummary, EgressItem, EventCard, EgressRow, Profile, ProfileKind, ProfileWarning, ProposalItem, ProposalState, ProposalSummary, Redaction, ToolChip } from "./aitypes";

export interface Preset { id: string; label: string; kind: ProfileKind; baseUrl: string; model: string; maxOutputTokens: number; needsKey: boolean }
export const PRESETS: readonly Preset[] = [
  { id: "openai", label: "OpenAI", kind: "openai_chat", baseUrl: "https://api.openai.com/v1", model: "", maxOutputTokens: 1024, needsKey: true },
  { id: "anthropic", label: "Anthropic", kind: "anthropic", baseUrl: "https://api.anthropic.com", model: "claude-opus-5-5", maxOutputTokens: 1024, needsKey: true },
  // Endless Tokens (MIRASTACK LABS PRIVATE LIMITED): optional separate service, one preset among others and never pre-selected. OpenAI-compatible; model detected from GET /models once the key is entered.
  { id: "endlesstokens", label: "Endless Tokens (MIRASTACK LABS)", kind: "openai_chat", baseUrl: "https://api.endlesstokens.ai/v1", model: "", maxOutputTokens: 1024, needsKey: true },
  { id: "openrouter", label: "OpenRouter", kind: "openai_chat", baseUrl: "https://openrouter.ai/api/v1", model: "", maxOutputTokens: 1024, needsKey: true },
  { id: "ollama", label: "Ollama (local)", kind: "openai_chat", baseUrl: "http://localhost:11434/v1", model: "", maxOutputTokens: 1024, needsKey: false },
  { id: "lmstudio", label: "LM Studio (local)", kind: "openai_chat", baseUrl: "http://localhost:1234/v1", model: "", maxOutputTokens: 1024, needsKey: false },
  { id: "custom", label: "Custom", kind: "openai_chat", baseUrl: "", model: "", maxOutputTokens: 1024, needsKey: false },
];

/** "host[:port]" of a base URL, or null when it is not a parseable http(s) URL. */
export function hostOf(baseUrl: string): string | null {
  try {
    const u = new URL(baseUrl.trim());
    return (u.protocol === "http:" || u.protocol === "https:") && u.hostname ? u.host : null;
  } catch { return null; }
}
function stripPort(host: string) {
  const h = host.trim().toLowerCase();
  if (h.startsWith("[")) return h.slice(1, h.indexOf("]") === -1 ? undefined : h.indexOf("]"));
  return h.includes(":") && h.indexOf(":") === h.lastIndexOf(":") ? h.slice(0, h.indexOf(":")) : h;
}
export function isLoopbackHost(host: string): boolean {
  const h = stripPort(host);
  return h === "localhost" || h.endsWith(".localhost") || h === "::1" || /^127(\.\d{1,3}){3}$/.test(h);
}
export function isLanHost(host: string): boolean {
  const h = stripPort(host);
  if (isLoopbackHost(h)) return false;
  const m = /^(\d{1,3})\.(\d{1,3})\.\d{1,3}\.\d{1,3}$/.exec(h);
  if (m) {
    const a = +m[1], b = +m[2];
    return a === 10 || (a === 192 && b === 168) || (a === 172 && b >= 16 && b <= 31) || (a === 169 && b === 254) || (a === 100 && b >= 64 && b <= 127);
  }
  if (h.includes(":")) return /^(f[cd][0-9a-f]{2}|fe80)/.test(h);
  return /\.(local|lan|internal|home|home\.arpa)$/.test(h) || (!h.includes(".") && h.length > 0);
}
/** Preview of what the backend will warn about; the backend's own `warnings` stay authoritative. */
export function clientWarnings(baseUrl: string): ProfileWarning[] {
  let u: URL;
  try { u = new URL(baseUrl.trim()); } catch { return []; }
  const out: ProfileWarning[] = [];
  if (u.protocol === "http:" && !isLoopbackHost(u.host)) out.push({ kind: "cleartextRemote", message: "API key and mail content are sent unencrypted" });
  if (isLanHost(u.host)) out.push({ kind: "lanHost", message: "This endpoint is on your local network. Only use a server you control." });
  return out;
}
export function mergeWarnings(a: ProfileWarning[], b: ProfileWarning[]): ProfileWarning[] {
  const seen = new Set<string>(), out: ProfileWarning[] = [];
  for (const w of [...a, ...b]) if (!seen.has(w.kind)) { seen.add(w.kind); out.push(w); }
  return out;
}

export function badgeText(host: string, model: string): string {
  return isLoopbackHost(host) ? "Local model" : `${host} · ${model}`;
}

/** Consent-dialog heading for a remote provider: names the label AND the host that will receive the content. */
export function consentHeading(name: string, host: string, model: string): string {
  const label = name.trim();
  return label && label !== host ? `Start a conversation with ${label} (${host}, ${model})?` : `Start a conversation with ${host} (${model})?`;
}

/** Notice shown before every "Check tone" request: names the provider label AND host, and what is (not) sent. */
export function toneNotice(name: string, host: string): string {
  const label = name.trim();
  const who = label && label !== host ? `${label} (${host})` : host;
  return `This draft's own text (no quoted message, no signature, no recipients) will be sent once to ${who} to check its tone. Email addresses you typed in the text are included unless address masking is on in Settings > AI. MiraMail does not store it; ${label && label !== host ? label : host}'s own policy applies. Nothing is sent until you press Send.`;
}
/** One sentence in the "Edit permissions" dialog. */
export const EDIT_KEEPS_READ_NOTE = "Mail MIRA read from a folder you remove is no longer sent with later messages. Your messages and MIRA's earlier replies stay in this chat; start a new chat to drop them.";
export const TONE_SETUP_HINT = "Set up an AI provider in Settings to use this.";
export const TONE_LOCAL_NOTICE = "Nothing leaves this Mac: the provider runs locally.";

/** Heading of the "Edit permissions" dialog: names the label AND the host, and says it is an existing chat. */
export function editHeading(name: string, host: string, model: string): string {
  const label = name.trim();
  return label && label !== host ? `Change what this chat may see with ${label} (${host}, ${model})?` : `Change what this chat may see with ${host} (${model})?`;
}

/**
 * Folder tick boxes for "Edit permissions": the conversation's CURRENT scopes (folders MIRA created earlier are already
 * in them, ticked), plus folders that exist now in the same accounts but were never offered (unticked: widening needs a tick).
 * Saved folders that no longer exist in an account whose live list is known are dropped.
 */
export function prefillScopes(saved: ConsentScope[], live: { account: string; name: string; selectable: boolean }[]): ConsentScope[] {
  const liveAccounts = new Set(live.map((l) => l.account));
  const seen = new Set<string>();
  const out: ConsentScope[] = [];
  const key = (a: string, f: string) => a + "\u0000" + f;
  for (const s of saved) {
    if (liveAccounts.has(s.account) && !live.some((l) => l.account === s.account && l.name === s.folder)) continue;
    if (seen.has(key(s.account, s.folder))) continue;
    seen.add(key(s.account, s.folder));
    out.push({ ...s });
  }
  const accounts = new Set(saved.filter((s) => s.allowed).map((s) => s.account));
  for (const l of live) {
    if (!l.selectable || !accounts.has(l.account) || seen.has(key(l.account, l.name))) continue;
    seen.add(key(l.account, l.name));
    out.push({ account: l.account, folder: l.name, allowed: false });
  }
  return out;
}

/** Calendar tick boxes for "Edit permissions": ticks as saved (calendars new since then unticked); when the calendar was off, all start ticked so the toggle alone turns it on. */
export function prefillCalendars<T extends { id: string }>(saved: CalendarConsent | undefined, live: T[]): (T & { allowed: boolean })[] {
  const on = !!saved?.enabled;
  return live.map((c) => ({ ...c, allowed: on ? saved!.calendars.some((k) => k.id === c.id && k.allowed) : true }));
}

/** The same one-line summary the backend writes into the transcript, or null when nothing would change. */
export function consentChangeSummary(old: Consent, next: Consent): string | null {
  const set = (c: Consent) => new Set(c.scopes.filter((s) => s.allowed).map((s) => s.account + "\u0000" + s.folder));
  const [o, n] = [set(old), set(next)];
  const added = [...n].filter((k) => !o.has(k)).length, removed = [...o].filter((k) => !n.has(k)).length;
  const parts: string[] = [];
  if (added) parts.push(`+${added} folder${added === 1 ? "" : "s"}`);
  if (removed) parts.push(`-${removed} folder${removed === 1 ? "" : "s"}`);
  const cal = (c: Consent) => new Set(c.tools && c.calendar?.enabled ? c.calendar.calendars.filter((k) => k.allowed).map((k) => k.id) : []);
  const [co, cn] = [cal(old), cal(next)];
  if ((co.size === 0) !== (cn.size === 0)) parts.push(`calendar ${cn.size === 0 ? "off" : "on"}`);
  else if (co.size !== cn.size || [...co].some((k) => !cn.has(k))) parts.push("calendars changed");
  if (old.tools !== next.tools) parts.push(`mail tools ${next.tools ? "on" : "off"}`);
  if ((old.tools && old.suggest) !== (next.tools && next.suggest)) parts.push(`suggestions ${next.tools && next.suggest ? "on" : "off"}`);
  if (JSON.stringify(old.redaction) !== JSON.stringify(next.redaction)) parts.push("redaction changed");
  return parts.length ? `Permissions changed by you: ${parts.join(", ")}` : null;
}

const normUrl = (s: string) => { try { const u = new URL(s.trim()); return `${u.protocol}//${u.host}${u.pathname.replace(/\/+$/, "")}`; } catch { return s.trim().replace(/\/+$/, ""); } };
/** True when a profile that has a saved key now points at a different base URL: the key must be typed again. */
export function keyReentryNeeded(original: Profile | null, baseUrl: string): boolean {
  if (!original) return false;
  if (original.needsKeyReentry) return true;
  return original.hasKey && normUrl(original.baseUrl) !== normUrl(baseUrl);
}

export function fmtTokens(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "0";
  if (n < 1000) return String(Math.round(n));
  return `${(n / 1000).toFixed(n < 10000 ? 1 : 0).replace(/\.0$/, "")}k`;
}
export interface Meter { pct: number; level: "ok" | "warn" | "full"; label: string }
export function tokenMeter(used: number, cap: number): Meter {
  const c = cap > 0 ? cap : 1;
  const pct = Math.max(0, Math.min(100, Math.round((used / c) * 100)));
  return { pct, level: used >= c ? "full" : pct >= 80 ? "warn" : "ok", label: `${fmtTokens(used)} / ${fmtTokens(cap)}` };
}

/** Egress timestamps may be unix seconds or milliseconds. */
export const toMs = (t: number) => (t < 1e12 ? t * 1000 : t);

/**
 * Pair each assistant message with the egress record of its turn. `rows` must be oldest first.
 * A turn is one request (plain chat) or several (one per tool round: `round` 0, 1, 2 ...); its record is the
 * LAST request (which contains every tool result sent so far) with the mail items of all its requests merged.
 * Pairing runs from the newest end: failed sends add rows without an assistant message.
 */
export function alignEgress(messages: AiMessage[], rows: EgressRow[]): Map<string, EgressRow> {
  const out = new Map<string, EgressRow>();
  const sorted = [...rows].sort((a, b) => toMs(a.at) - toMs(b.at));
  const turns: EgressRow[][] = [];
  for (const r of sorted) {
    if (turns.length === 0 || (r.round ?? 0) === 0) turns.push([r]);
    else turns[turns.length - 1].push(r);
  }
  const merged = turns.map((t): EgressRow => {
    const last = t[t.length - 1];
    if (t.length === 1) return last;
    const seen = new Set<string>();
    const items: EgressItem[] = [];
    for (const r of t) for (const it of r.items ?? []) if (!seen.has(it.ref)) { seen.add(it.ref); items.push(it); }
    return { ...last, items, requests: t.length };
  });
  let r = merged.length - 1;
  for (let i = messages.length - 1; i >= 0 && r >= 0; i--) {
    if (messages[i].role !== "assistant") continue;
    out.set(messages[i].id, merged[r--]);
  }
  return out;
}

const TOOL_VERB: Record<string, string> = {
  propose_move: "Suggested moving", propose_archive: "Suggested archiving", propose_trash: "Suggested moving to Trash", propose_flag: "Suggested flag change", propose_create_folder: "Suggested new folder", propose_rule_change: "Suggested rule change",
  list_accounts: "Listed accounts", list_rules: "Read mail rules", list_folders: "Listed folders", search_mail: "Searched mail", server_search: "Searched the mail server",
  draft_reply: "Drafted a reply", draft_new: "Drafted a new message",
  mail_stats: "Checked mail statistics", get_message: "Read message", get_thread: "Read conversation",
  list_calendars: "Listed calendars", list_events: "Checked calendar", get_event: "Checked calendar", free_busy: "Checked availability", find_free_slots: "Looked for free time",
  propose_event: "Suggested an event", propose_event_change: "Suggested a change", propose_event_cancel: "Suggested cancelling an event", propose_invite_response: "Suggested an answer to an invitation",
};
const CAL_READ = new Set(["list_calendars", "list_events", "get_event", "free_busy", "find_free_slots"]);
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const RANGE = /^(?:(.*?),\s*)?(\d{4})-(\d{2})-(\d{2})(?:T(\d{2}:\d{2}))?\s*\u2192\s*(\d{4})-(\d{2})-(\d{2})(?:T(\d{2}:\d{2}))?$/;
/** "2026-11-05 \u2192 2026-11-12" -> "5\u201312 Nov"; "28 Nov \u2013 3 Dec" across months; unparseable labels come back unchanged. A "30 min, " prefix is kept. */
export function formatRangeLabel(label: string): string {
  const m = RANGE.exec(label.trim());
  if (!m) return label;
  const [, pre, y1, m1, d1, t1, y2, m2, d2, t2] = m;
  const ok = (mo: string, d: string) => +mo >= 1 && +mo <= 12 && +d >= 1 && +d <= 31;
  if (!ok(m1, d1) || !ok(m2, d2)) return label;
  const a = `${+d1} ${MONTHS[+m1 - 1]}`, b = `${+d2} ${MONTHS[+m2 - 1]}`;
  let out: string;
  if (y1 === y2 && m1 === m2 && d1 === d2) out = t1 && t2 ? `${a} ${t1}\u2013${t2}` : a;
  else if (y1 === y2 && m1 === m2) out = `${+d1}\u2013${+d2} ${MONTHS[+m1 - 1]}`;
  else if (y1 === y2) out = `${a} \u2013 ${b}`;
  else out = `${a} ${y1} \u2013 ${b} ${y2}`;
  return pre ? `${pre}, ${out}` : out;
}
/** "Searched mail: from:foo - 12 results" */
export function chipText(c: Pick<ToolChip, "name" | "label" | "summary" | "ok" | "running">): string {
  const verb = TOOL_VERB[c.name] ?? "Used tool";
  const cal = CAL_READ.has(c.name);
  const shown = cal ? formatRangeLabel(c.label) : c.label;
  const label = shown ? `: ${shown}` : "";
  const tail = c.running ? " …" : c.ok ? (c.summary ? `${cal ? " \u2014 " : " - "}${c.summary}` : "") : " - could not run";
  return `${verb}${label}${tail}`;
}
/** Plain-words explanation shown when a chip is expanded. */
export function chipDetail(c: Pick<ToolChip, "name" | "ok" | "summary" | "running">): string {
  if (c.running) return "Working on it.";
  if (c.name.startsWith("propose_") && c.ok) return "A suggestion was queued for your approval. Nothing changes until you press Approve.";
  if (c.name.startsWith("draft_") && c.ok) return "A draft was written for you to review. Nothing was sent: you decide whether to open it in Compose, and sending happens only there.";
  if (!c.ok) return `The assistant asked for something MiraMail refused or could not do (${c.summary || "failed"}). Nothing was changed.`;
  return "Read-only. What the model received from this step is listed under Sent to model.";
}

export function prettyJson(s: unknown): string {
  if (typeof s !== "string") { try { return JSON.stringify(s, null, 2); } catch { return String(s); } }
  try { return JSON.stringify(JSON.parse(s), null, 2); } catch { return s; }
}
export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  return n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`;
}

export const DEFAULT_REDACTION: Redaction = { stripQuotes: true, shortenLinks: true, maskAddresses: true };
export const DEFAULT_SETTINGS: AiSettings = {
  perTurnTokens: 4000, perConversationTokens: 50000, perDayTokens: 200000,
  defaultRedaction: DEFAULT_REDACTION,
  purge: { egressRetentionDays: 30, conversationRetentionDays: null, purgeOnAccountRemoval: true },
};

/** Folder scopes for the consent dialog: Junk and Trash start off. */
export function defaultScopes(account: string | null, folders: { name: string; role: string; selectable: boolean }[]): ConsentScope[] {
  if (!account) return [];
  return folders.filter((f) => f.selectable).map((f) => ({ account, folder: f.name, allowed: f.role !== "junk" && f.role !== "trash" }));
}
/** `suggest` (default on when tools are on): the assistant may queue changes that the user approves one by one. It is meaningless without `tools`. */
export function buildConsent(scopes: ConsentScope[], redaction: Redaction, tools = true, suggest = tools, calendar: CalendarConsent = { enabled: false, calendars: [] }): Consent {
  // Calendar reading needs tools, the toggle, and at least one ticked calendar; unticked (or disabled) calendars are never `allowed`.
  const on = tools && calendar.enabled && calendar.calendars.some((c) => c.allowed);
  return {
    scopes: scopes.map((s) => ({ ...s })), redaction: { ...redaction }, tools, suggest: tools && suggest,
    calendar: { enabled: on, calendars: calendar.calendars.map((c) => ({ id: c.id, allowed: on && c.allowed })) },
  };
}
/** Old consents have no `calendar`: that means off. */
export const calendarOn = (c: Pick<Consent, "calendar">) => !!c.calendar?.enabled;

export const EGRESS_OPTIONS: { v: number | null; label: string }[] = [
  { v: 0, label: "Don't keep a log" }, { v: 7, label: "7 days" }, { v: 30, label: "30 days" }, { v: 90, label: "90 days" }, { v: null, label: "Never delete" },
];
export const CONV_OPTIONS: { v: number | null; label: string }[] = [
  { v: null, label: "Keep until I delete them" }, { v: 7, label: "Delete after 7 days" }, { v: 30, label: "Delete after 30 days" }, { v: 90, label: "Delete after 90 days" }, { v: 365, label: "Delete after 1 year" },
];

export interface ProfileForm { name: string; kind: ProfileKind; baseUrl: string; model: string; maxOutputTokens: number }
/** Returns a user-facing problem, or null when the form can be saved. */
export function validateProfileForm(f: ProfileForm): string | null {
  if (!f.name.trim()) return "Give this provider a name.";
  if (!hostOf(f.baseUrl)) return "Base URL must start with http:// or https:// and include a host.";
  if (!f.model.trim()) return "Choose or type a model name.";
  if (!Number.isInteger(f.maxOutputTokens) || f.maxOutputTokens < 16 || f.maxOutputTokens > 200000) return "Max output tokens must be a whole number from 16 to 200000.";
  return null;
}
export function validateBudgets(s: AiSettings): string | null {
  for (const [k, v] of [["Per turn", s.perTurnTokens], ["Per conversation", s.perConversationTokens], ["Per day", s.perDayTokens]] as const) {
    if (!Number.isInteger(v) || v < 100 || v > 100_000_000) return `${k} budget must be a whole number of tokens from 100 to 100,000,000.`;
  }
  return null;
}

/** "backend field names" -> readable lines for the purge result toast. */
export function purgeLines(report: unknown): string[] {
  if (!report || typeof report !== "object") return [];
  const out: string[] = [];
  for (const [k, v] of Object.entries(report as Record<string, unknown>)) {
    if (typeof v === "number" || typeof v === "string" || typeof v === "boolean") {
      out.push(`${k.replace(/([A-Z])/g, " $1").replace(/_/g, " ").toLowerCase().replace(/^./, (c) => c.toUpperCase())}: ${v}`);
    }
  }
  return out;
}

// ---- proposals (P2) ----

export const EVENT_KINDS = ["event_create", "event_change", "event_cancel", "invite_response"] as const;
/** Cards with no message rows (Approve sends `selected: []`): a new folder, a change to an existing rule. */
export const isRowless = (k: string) => k === "create_folder" || k === "rule_change";
/** Calendar proposals: no message rows, Approve sends `selected: []`. */
export const isEventKind = (k: string) => (EVENT_KINDS as readonly string[]).includes(k);
export const eventHeading = (k: string) => k === "event_create" ? "Suggested event" : k === "event_change" ? "Suggested event change" : k === "event_cancel" ? "Suggested cancellation" : "Suggested answer to an invitation";
/** The user's own calendar (same id as the backend's `DEFAULT_CALENDAR_ID`): the only one ticked when a chat starts. */
export const DEFAULT_CALENDAR_ID = "personal";
/** Starting state of the calendar tick boxes: every calendar ticked (the dialog is the explicit consent; untick to exclude). */
export const withDefaultTicks = <T extends { id: string }>(cals: T[]): (T & { allowed: boolean })[] => cals.map((c) => ({ ...c, allowed: true }));
/** "Approving will send/create ..." is the prominent line (mail goes out, a Zoom meeting is made); conflicts and "never appears in your mail" are cautions. */
export function warningTone(w: string): "notify" | "caution" | "info" {
  if (/^approving will (also )?(send|create)/i.test(w)) return "notify";
  if (/conflict|overlap|never appears? in your mail/i.test(w)) return "caution";
  return "info";
}
const PAST: Record<string, string> = { move: "moved", archive: "archived", trash: "moved to Trash", flag: "updated", create_folder: "created" };

/** Still waiting for a decision (the card shows Approve / Reject). */
export const isPending = (s: ProposalState) => s === "proposed";
/** Running right now. */
export const isRunning = (s: ProposalState) => s === "approved" || s === "executing";

/** Every card of the last reply has been decided and none is still running: time to tell the assistant (it cannot see the click). */
export function decidedAll(sums: Pick<ProposalSummary, "state">[]): boolean {
  return sums.length > 0 && sums.every((p) => !isPending(p.state) && !isRunning(p.state));
}
/** The message MiraMail sends for the user after they decide: the app's own result lines only, no mail content. */
export function followUpText(sums: Pick<ProposalSummary, "id" | "state" | "kind" | "counts" | "error">[]): string {
  const lines = sums.slice(0, 10).map((p, i) => `${i + 1}. ${p.kind.replace(/_/g, " ")}: ${p.state === "rejected" ? "I rejected it. Nothing was changed." : resultText(p)}`);
  return `I have decided on your suggestion${sums.length === 1 ? "" : "s"}:\n${lines.join("\n")}\nPlease carry on with what I asked.`;
}

/** One honest sentence about what happened, from the app's own counts. */
export function resultText(p: Pick<ProposalSummary, "state" | "kind" | "counts" | "error"> & { title?: string }): string {
  const c = p.counts;
  if (isEventKind(p.kind) || p.kind === "rule_change") {
    switch (p.state) {
      case "proposed": return "Waiting for your approval. Nothing has been changed.";
      case "approved":
      case "executing": return "Working\u2026";
      case "done": return "Done.";
      case "partially_done": return "Partly done.";
      case "failed": return `Failed: nothing was changed.${p.error ? ` ${p.error}` : ""}`;
      case "rejected": return "Rejected. Nothing was changed.";
      case "expired": return "Expired without a decision. Nothing was changed.";
      case "undone": return p.kind === "rule_change" && /^Turn on rule/.test(p.title ?? "") ? "Undone. Messages it already moved stay where they are; undo those from the rule's activity." : "Undone.";
    }
  }
  const verb = PAST[p.kind] ?? "changed";
  const bad = c.failed + c.skipped;
  const extra = (c.undoFailed ? ` ${c.undoFailed} could not be put back.` : "") + (c.unknown ? ` ${c.unknown} could not be confirmed (the connection failed); check the folder or use Undo.` : "");
  switch (p.state) {
    case "proposed": return "Waiting for your approval. Nothing has been changed.";
    case "approved":
    case "executing": return `Working… ${c.done + bad + c.noop + (c.unknown ?? 0)} of ${c.selected}`;
    case "done": return p.kind === "create_folder" ? (c.done ? "Folder created." : "That folder already existed.") : `Done: ${c.done} ${verb}${c.noop ? `, ${c.noop} already in place` : ""}.${extra}`;
    case "partially_done": return `Partly done: ${c.done} ${verb}, ${bad} not changed.${extra}`;
    case "failed": return `Failed: nothing was changed.${p.error ? ` ${p.error}` : ""}`;
    case "rejected": return "Rejected. Nothing was changed.";
    case "expired": return "Expired without a decision. Nothing was changed.";
    case "undone": return `Undone: ${c.undone} put back.${extra}`;
  }
}

export function selectedCount(items: Pick<ProposalItem, "selected">[]): number {
  return items.reduce((n, i) => n + (i.selected ? 1 : 0), 0);
}
/** `idx` of the ticked rows: this is what Approve sends (the backend runs only these). */
export function selectedIdx(items: Pick<ProposalItem, "idx" | "selected">[]): number[] {
  return items.filter((i) => i.selected).map((i) => i.idx);
}
/** Approve needs at least one ticked row (a "create folder" card has no rows). */
export function canApprove(p: Pick<ProposalSummary, "state" | "kind">, items: Pick<ProposalItem, "selected">[]): boolean {
  return p.state === "proposed" && (isRowless(p.kind) || isEventKind(p.kind) || selectedCount(items) > 0);
}
/** Title for the Approve button with the number that will actually run. */
export function approveLabel(p: Pick<ProposalSummary, "kind"> & { event?: Pick<EventCard, "notify"> }, items: Pick<ProposalItem, "selected">[]): string {
  if (isEventKind(p.kind)) {
    if (!p.event?.notify) return "Approve";
    return p.kind === "event_cancel" ? "Approve and send cancellation" : p.kind === "invite_response" ? "Approve and send answer" : "Approve and send invitations";
  }
  if (isRowless(p.kind)) return "Approve";
  const n = selectedCount(items);
  return n === items.length ? "Approve" : `Approve ${n}`;
}

/**
 * Which assistant message shows which card. Cards created during the open turn (`live`: message id -> proposal ids) stay on
 * that message; stored ones go to the first assistant message written at or after the proposal (the turn's message is
 * saved after its tool calls ran), else the last assistant message.
 */
export function placeProposals(
  messages: Pick<AiMessage, "id" | "role" | "createdAt">[],
  proposals: Pick<ProposalSummary, "id" | "createdAt">[],
  live: Record<string, number[]> = {},
): Map<string, number[]> {
  const out = new Map<string, number[]>();
  const put = (mid: string, id: number) => { const l = out.get(mid) ?? []; if (!l.includes(id)) l.push(id); out.set(mid, l); };
  const placed = new Set<number>();
  for (const [mid, ids] of Object.entries(live)) if (messages.some((m) => m.id === mid)) for (const id of ids) { put(mid, id); placed.add(id); }
  const asst = messages.filter((m) => m.role === "assistant");
  const sorted = [...proposals].sort((a, b) => a.createdAt - b.createdAt || a.id - b.id);
  for (const p of sorted) {
    if (placed.has(p.id) || asst.length === 0) continue;
    const m = asst.find((x) => x.createdAt >= p.createdAt) ?? asst[asst.length - 1];
    put(m.id, p.id);
  }
  return out;
}

/** Rows shown at once in a preview table (a proposal can hold thousands). */
export const PREVIEW_PAGE = 200;

// ---- drafts (P3) ----

/** Characters of the body shown on the card before "Show all". */
export const DRAFT_PREVIEW_CHARS = 600;
export const AI_DRAFT_BANNER = "Drafted by AI \u2014 review before sending";

/** Waiting for the user (the card shows Open in Compose / Dismiss). */
export const draftPending = (s: DraftState) => s === "proposed";
/** Open in Compose stays available until the draft is dismissed (the pane may have been closed). */
export const canOpenDraft = (d: Pick<DraftSummary, "state">) => d.state !== "dismissed";
export const canDismissDraft = (d: Pick<DraftSummary, "state">) => d.state === "proposed";

/** First `max` characters of the body, cut at a line or word boundary, plus whether it was cut. */
export function draftPreview(body: string, max = DRAFT_PREVIEW_CHARS): { text: string; cut: boolean } {
  const t = body.replace(/\r\n/g, "\n").trim();
  if (t.length <= max) return { text: t, cut: false };
  let end = t.lastIndexOf("\n", max);
  if (end < max * 0.6) end = t.lastIndexOf(" ", max);
  if (end < max * 0.6) end = max;
  return { text: t.slice(0, end).trimEnd(), cut: true };
}

/** Recipient lines for the card. Addresses are the app's; `new` marks addresses the user's mail has never seen. */
export function draftRecipients(d: Pick<DraftSummary, "to" | "cc" | "newAddresses">): { label: string; addresses: { address: string; isNew: boolean }[] }[] {
  const fresh = new Set(d.newAddresses.map((a) => a.toLowerCase()));
  const mk = (l: string[]) => l.map((address) => ({ address, isNew: fresh.has(address.toLowerCase()) }));
  const out = [{ label: "To", addresses: mk(d.to) }];
  if (d.cc.length) out.push({ label: "Cc", addresses: mk(d.cc) });
  return out;
}

/** One honest sentence about where the draft stands. */
export function draftStatusText(d: Pick<DraftSummary, "state">): string {
  switch (d.state) {
    case "proposed": return "Nothing has been sent. Open it in Compose to edit it and send it yourself.";
    case "opened": return "Opened in Compose. It is sent only if you press Send there.";
    case "dismissed": return "Dismissed. Nothing was sent.";
  }
}

/** Short heading: a reply or a new message. */
export const draftHeading = (d: Pick<DraftSummary, "kind">) => (d.kind === "reply" ? "Draft reply" : "Draft message");

/** Which assistant message shows which draft card (same rule as suggestions). */
export const placeDrafts = placeProposals;

/** How long the AI tab / pane waits for the backend before showing an error instead of a skeleton. */
export const AI_LOAD_TIMEOUT_MS = 8000;

/** Reject with a message naming `label` (the command) when `p` has not settled within `ms`. */
export function withTimeout<T>(p: Promise<T>, label: string, ms = AI_LOAD_TIMEOUT_MS): Promise<T> {
  let t: ReturnType<typeof setTimeout> | undefined;
  const timer = new Promise<never>((_, reject) => {
    t = setTimeout(() => reject(`The AI backend did not respond (${label}, ${Math.round(ms / 1000)} s).`), ms);
  });
  return Promise.race([p, timer]).finally(() => clearTimeout(t));
}
