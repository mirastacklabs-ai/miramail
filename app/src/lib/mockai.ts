// In-memory stand-in for the AI commands, used only in a plain browser (dev). Lost on reload.
import type { AiEvent, AiMessage, AiSettings, ApproveResult, AutoReplyConfig, AutomationState, Consent, Conversation, DraftPrefill, DraftSummary, EgressItem, EgressRow, Profile, ProposalCounts, ProposalDetail, ProposalEvent, ProposalItem, ProposalSummary, PurgeReport, ReplyLogRow, Rule, RuleCard, RuleInput, RuleRun, RuleSpec, TestResult, ToolChip, UndoResult } from "./aitypes";
import { DEFAULT_SETTINGS, clientWarnings, hostOf, keyReentryNeeded } from "./aivm";

const wait = <T>(v: T, ms = 120): Promise<T> => new Promise((r) => setTimeout(() => r(structuredClone(v)), ms));
const sec = () => Math.floor(Date.now() / 1000);
const uid = (p: string) => `${p}-${Math.random().toString(16).slice(2, 12)}`;

let profiles: Profile[] = [];
const keys = new Map<string, string>();
let settings: AiSettings = structuredClone(DEFAULT_SETTINGS);
const convs = new Map<string, { c: Conversation; messages: AiMessage[]; egress: EgressRow[]; consent: Consent }>();
const cancelled = new Set<string>();

// ---- proposals (P2): a few fake rows so the approval UI can be tried in a plain browser ----
const proposals = new Map<number, ProposalDetail>();
const subscribers = new Set<(e: ProposalEvent) => void>();
let nextProposal = 1;
const counts = (items: ProposalItem[]): ProposalCounts => {
  const n = (s: ProposalItem["status"]) => items.filter((i) => i.status === s).length;
  return { total: items.length, selected: items.filter((i) => i.selected).length, done: n("done"), failed: n("failed"), skipped: n("skipped"), noop: n("noop"), undone: n("undone"), undoFailed: n("undo_failed"), unknown: n("unknown") };
};
const push = (d: ProposalDetail) => { d.counts = counts(d.items); d.updatedAt = sec(); for (const cb of subscribers) cb({ id: d.id, convId: d.convId, state: d.state, counts: structuredClone(d.counts) }); };
// ---- drafts (P3) ----
const drafts = new Map<number, DraftSummary & { quote: string }>();
let nextDraft = 1;
const SENDERS = ["Shop News <news@shop.test>", "Deals <deals@promo.test>", "Weekly Digest <digest@blog.test>"];

// ---- rules + auto-reply (P4): in-memory stand-ins so the screens can be tried in a plain browser ----
let rules: Rule[] = [];
let nextRule = 1;
let runs: RuleRun[] = [];
let automation: AutomationState = { rulesPaused: false, autoreplyPaused: false };
const replies = new Map<string, AutoReplyConfig>();
const describeMock = (s: RuleSpec) => `When a new message arrives in the Inbox and ${s.conditions.map((c) => `the ${c.field} ${c.op} "${c.value}"`).join(s.match === "any" ? " OR " : " AND ")}, ${s.actions.map((a) => a.type).join(" and ")}.`;
const dryMock = (): RuleCard["dryRun"] => ({ scanned: 40, matched: 6, sample: SENDERS.map((f, i) => ({ folder: "INBOX", from: f, subject: `Newsletter #${40 + i}` })) });

const REPLY = [
  "Here is a **short plan** for your inbox:\n\n",
  "1. Skim the *unread* messages first.\n2. Reply to anything from people you know.\n3. Archive newsletters.\n\n",
  "| Folder | Action |\n|---|---|\n| INBOX | Reply or archive |\n| Junk | Ignore |\n\n",
  "Use `Cmd+J` to bring this pane back. Docs: https://example.org/help\n\n",
  "```\nno mail has been sent to me yet\n```\n",
];
const CAL_REPLY = ["Your week is fairly full, but I found **two free times** for a one hour lunch:\n\n", "- Thu 12 Nov, 12:00\u201313:00\n- Fri 13 Nov, 13:00\u201314:00\n\n", "I suggested Friday below; approve it to add it to your calendar.\n"];
const LONG = Array.from({ length: 40 }, (_, i) => `Line ${i + 1} of a long streamed answer that can be stopped.\n`);

export const mockAi = {
  ai_rules_list: () => wait(rules),
  async ai_rule_save(a: { rule: RuleInput }) {
    const i = a.rule.id;
    const old = rules.find((r) => r.id === i);
    const r: Rule = { id: old?.id ?? nextRule++, account: a.rule.account, name: a.rule.name, enabled: old?.enabled ?? false, origin: old?.origin ?? "user", reason: old?.reason ?? "", createdAt: old?.createdAt ?? sec(), updatedAt: sec(), lastRunAt: old?.lastRunAt ?? null, valid: true, spec: a.rule.spec, description: describeMock(a.rule.spec) };
    rules = old ? rules.map((x) => (x.id === r.id ? r : x)) : [...rules, r];
    return wait(r);
  },
  async ai_rule_set_enabled(a: { id: number; enabled: boolean }) {
    const r = rules.find((x) => x.id === a.id);
    if (!r) throw "That rule no longer exists.";
    r.enabled = a.enabled;
    return wait(r);
  },
  async ai_rule_delete(a: { id: number }) { rules = rules.filter((r) => r.id !== a.id); return wait(undefined, 60); },
  async ai_rule_card(a: { id: number }) {
    const r = rules.find((x) => x.id === a.id);
    if (!r) throw "That rule no longer exists.";
    return wait({ rule: r, dryRun: dryMock() } as RuleCard);
  },
  ai_rule_dry_run: () => wait(dryMock()),
  ai_rule_preview_existing: () => wait(dryMock()),
  async ai_rule_apply_existing(a: { id: number }) {
    const r = rules.find((x) => x.id === a.id);
    if (!r) throw "That rule no longer exists.";
    if (automation.rulesPaused) throw "Rules are paused. Turn \"Pause all rules\" off first.";
    const run: RuleRun = { id: runs.length + 1, ruleId: r.id, ruleName: r.name, account: r.account, folder: "INBOX", ts: sec(), matched: 6, summary: "6 changed", state: "done", undoable: true };
    runs = [run, ...runs];
    return wait([run]);
  },
  ai_automation_state: () => wait(automation),
  async ai_rules_pause(a: { paused: boolean }) { automation = { ...automation, rulesPaused: a.paused }; return wait(automation, 60); },
  async ai_autoreply_pause(a: { paused: boolean }) { automation = { ...automation, autoreplyPaused: a.paused }; return wait(automation, 60); },
  ai_rule_runs: () => wait(runs),
  async ai_rule_run_undo(a: { runId: number }) { runs = runs.map((r) => (r.id === a.runId ? { ...r, state: "undone", undoable: false } : r)); return wait({ undone: 3, failed: 0, errors: [] }); },
  async ai_autoreply_get(a: { account: string }) { return wait(replies.get(a.account) ?? { enabled: false, subject: "", echoSubject: false, body: "", start: null, end: null, requireAuth: true, dailyCap: 20 }); },
  async ai_autoreply_set(a: { account: string; config: AutoReplyConfig }) {
    if (a.config.enabled && !a.config.body.trim()) throw "Type the reply text first.";
    replies.set(a.account, a.config);
    return wait(a.config);
  },
  async ai_autoreply_preview(a: { account: string; config: AutoReplyConfig; subject: string | null }) {
    const orig = a.config.echoSubject ? (a.subject ?? "").slice(0, 60) : "";
    const subject = a.config.subject.trim() ? a.config.subject.replace("{subject}", orig) : a.config.echoSubject ? `Auto: ${orig}` : "Automatic reply";
    const raw = `From: ${a.account}\r\nTo: sender@example.com\r\nSubject: ${subject}\r\nIn-Reply-To: <original-message@example.com>\r\nAuto-Submitted: auto-replied\r\n\r\n${a.config.body}\r\n`;
    return wait({ raw, to: "sender@example.com", subject, body: a.config.body });
  },
  ai_autoreply_log: () => wait<ReplyLogRow[]>([{ id: 1, account: "demo@example.org", ts: sec() - 3600, sender: "news@shop.test", subject: "Sale", outcome: "skipped", reason: "list_mail", detail: "The message is from a mailing list or newsletter." }]),

  ai_list_profiles: () => wait(profiles),
  async ai_save_profile(a: { profile: Profile; apiKey: string | null }) {
    const p = a.profile;
    const old = profiles.find((x) => x.id === p.id) ?? null;
    if (a.apiKey === "") keys.delete(p.id);
    else if (a.apiKey) keys.set(p.id, a.apiKey);
    else if (old && keyReentryNeeded(old, p.baseUrl)) keys.delete(p.id);
    const saved: Profile = {
      ...p, hasKey: keys.has(p.id), keyOrigin: keys.has(p.id) ? hostOf(p.baseUrl) : null, needsKeyReentry: false,
      warnings: clientWarnings(p.baseUrl),
    };
    if (old && old.hasKey && !saved.hasKey && a.apiKey == null) saved.needsKeyReentry = true;
    profiles = old ? profiles.map((x) => (x.id === p.id ? saved : x)) : [...profiles, saved];
    return wait(saved, 200);
  },
  async ai_delete_profile(a: { id: string }) { profiles = profiles.filter((p) => p.id !== a.id); keys.delete(a.id); return wait(undefined, 60); },
  async ai_test_profile(a: { profile: Profile; apiKey: string | null }): Promise<TestResult> {
    const p = a.profile;
    if (/fail/i.test(p.baseUrl)) return wait({ ok: false, latencyMs: 0, models: [], supportsTools: false, message: "Could not connect to the endpoint." }, 500);
    const models = p.kind === "anthropic" ? ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"] : p.baseUrl.includes("11434") ? ["llama3.2", "qwen2.5"] : ["gpt-4o-mini", "gpt-4o"];
    return wait({ ok: true, latencyMs: 180, models, supportsTools: true, message: "Connected." }, 600);
  },
  async ai_list_models(a: { profile: Profile; apiKey: string | null }): Promise<TestResult> {
    const p = a.profile;
    if (p.kind === "anthropic") return wait({ ok: true, latencyMs: 0, models: [], supportsTools: false, message: "No model list available from this endpoint." }, 100);
    if (/fail/i.test(p.baseUrl)) return wait({ ok: false, latencyMs: 0, models: [], supportsTools: false, message: "Could not connect to the endpoint." }, 300);
    const models = p.baseUrl.includes("11434") ? ["llama3.2", "qwen2.5"] : ["gpt-4o-mini", "gpt-4o"];
    return wait({ ok: true, latencyMs: 0, models, supportsTools: false, message: `${models.length} models listed.` }, 300);
  },
  async ai_check_tone(a: { profileId: string; text: string }) {
    if (!profiles.some((p) => p.id === a.profileId)) throw "Set up an AI provider in Settings > AI first.";
    if (!a.text.trim()) throw "Write something first.";
    return wait({ tone: /!!|NOW|unacceptable/i.test(a.text) ? "tense" : "neutral", clarity: 4, notes: ["Mock reply: the real check runs in the app."], suggestions: [{ label: "Warmer", text: "Hi,\n\n" + a.text.trim() + "\n\nThanks!" }], restore: [] }, 500);
  },
  ai_get_settings: () => wait(settings),
  async ai_set_settings(a: { settings: AiSettings }) { settings = structuredClone(a.settings); return wait(settings, 80); },
  async ai_new_conversation(a: { profileId: string; consent: Consent }) {
    const p = profiles.find((x) => x.id === a.profileId);
    if (!p) throw "That provider no longer exists.";
    const t = sec();
    const c: Conversation = { id: uid("cv"), title: "New conversation", profileId: p.id, endpointHost: hostOf(p.baseUrl) ?? "", model: p.model, createdAt: t, updatedAt: t, toolsEnabled: !!a.consent.tools, suggestEnabled: !!(a.consent.tools && a.consent.suggest), calendarEnabled: !!(a.consent.tools && a.consent.calendar?.enabled && a.consent.calendar.calendars.some((k) => k.allowed)) };
    convs.set(c.id, { c, messages: [], egress: [], consent: a.consent });
    return wait(c, 80);
  },
  ai_list_conversations: () => wait([...convs.values()].map((x) => x.c).sort((a, b) => b.updatedAt - a.updatedAt), 60),
  async ai_get_conversation(a: { id: string }) {
    const x = convs.get(a.id);
    if (!x) throw "Conversation not found.";
    return wait({ conversation: x.c, messages: x.messages, consent: x.consent }, 60);
  },
  async ai_update_consent(a: { convId: string; consent: Consent }) {
    const x = convs.get(a.convId);
    if (!x) throw "That conversation no longer exists.";
    const c = a.consent;
    const flat = (k: Consent) => JSON.stringify([k.scopes.filter((s) => s.allowed), k.calendar, k.tools, k.suggest, k.redaction]);
    if (flat(c) !== flat(x.consent)) {
      x.consent = c;
      x.c = { ...x.c, toolsEnabled: !!c.tools, suggestEnabled: !!(c.tools && c.suggest), calendarEnabled: !!(c.tools && c.calendar?.enabled && c.calendar.calendars.some((k) => k.allowed)) };
      x.messages.push({ id: uid("n"), role: "notice", text: "Permissions changed by you", createdAt: sec() });
    }
    return wait({ conversation: x.c, messages: x.messages, consent: x.consent }, 80);
  },
  async ai_delete_conversation(a: { id: string }) { convs.delete(a.id); return wait(undefined, 60); },
  async ai_cancel(a: { convId: string }) { cancelled.add(a.convId); return wait(undefined, 10); },
  async ai_egress(a: { convId: string; limit: number }) { return wait((convs.get(a.convId)?.egress ?? []).slice(-a.limit), 40); },
  subscribe(cb: (e: ProposalEvent) => void) { subscribers.add(cb); return () => { subscribers.delete(cb); }; },
  async ai_list_proposals(a: { convId: string | null; limit?: number }) {
    const l = [...proposals.values()].filter((p) => !a.convId || p.convId === a.convId).sort((x, y) => y.createdAt - x.createdAt || y.id - x.id);
    return wait(l.map(({ items: _i, ...s }): ProposalSummary => s), 60);
  },
  async ai_get_proposal(a: { id: number }) {
    const d = proposals.get(a.id);
    if (!d) throw "That suggestion no longer exists.";
    return wait(d, 60);
  },
  async ai_approve_proposal(a: { id: number; selected?: number[] }): Promise<ApproveResult> {
    const d = proposals.get(a.id);
    if (!d) throw "That suggestion no longer exists.";
    if (d.state !== "proposed") throw `This suggestion can't be changed any more (it is ${d.state.replace("_", " ")}).`;
    if (d.event) {
      d.state = "executing"; push(d);
      await new Promise((r) => setTimeout(r, 500));
      d.state = "done"; d.undoable = true; push(d);
      return wait({ proposal: d, ok: 1, failed: 0, skipped: 0, noop: 0, foldersChanged: false, note: "Event created and invitations sent." }, 40);
    }
    for (const it of d.items) it.selected = (a.selected ?? []).includes(it.idx);
    if (!d.items.some((i) => i.selected)) throw "Select at least one message.";
    d.state = "executing"; push(d);
    await new Promise((r) => setTimeout(r, 500));
    // Demo: the last selected row fails so the "partly done" wording can be seen when 3+ rows are selected.
    const sel = d.items.filter((i) => i.selected);
    for (const it of sel) { it.status = "done"; }
    if (sel.length >= 3) { const bad = sel[sel.length - 1]; bad.status = "failed"; bad.error = "The message is no longer in the folder."; }
    d.state = sel.some((i) => i.status === "failed") ? "partially_done" : "done";
    d.undoable = true; push(d);
    const c = counts(d.items);
    return wait({ proposal: d, ok: c.done, failed: c.failed, skipped: c.skipped, noop: c.noop, foldersChanged: false, note: "" }, 40);
  },
  async ai_reject_proposal(a: { id: number }) {
    const d = proposals.get(a.id);
    if (!d || d.state !== "proposed") throw "This suggestion can't be changed any more.";
    d.state = "rejected"; push(d);
    return wait(d, 40);
  },
  async ai_undo_proposal(a: { id: number }): Promise<UndoResult> {
    const d = proposals.get(a.id);
    if (!d || !d.undoable || (d.state !== "done" && d.state !== "partially_done")) throw "This suggestion can't be changed any more.";
    await new Promise((r) => setTimeout(r, 300));
    let n = 0;
    for (const it of d.items) if (it.status === "done") { it.status = "undone"; n++; }
    d.state = "undone"; d.undoable = false; push(d);
    return wait({ proposal: d, undone: n, failed: 0, errors: [] }, 40);
  },
  async ai_list_drafts(a: { convId: string | null; limit?: number }) {
    const l = [...drafts.values()].filter((d) => !a.convId || String(d.convId) === String(a.convId)).sort((x, y) => y.createdAt - x.createdAt || y.id - x.id);
    return wait(l, 60);
  },
  async ai_get_draft(a: { id: number }) {
    const d = drafts.get(a.id);
    if (!d) throw "That draft no longer exists.";
    return wait(d, 40);
  },
  async ai_open_draft(a: { id: number }): Promise<DraftPrefill> {
    const d = drafts.get(a.id);
    if (!d) throw "That draft no longer exists.";
    if (d.state === "dismissed") throw "That draft was dismissed.";
    d.state = "opened"; d.updatedAt = sec();
    const text = d.quote ? `${d.body}\n\n${d.quote}\n` : d.body;
    return wait({ id: d.id, account: d.account, to: d.to, cc: d.cc, bcc: [], subject: d.subject, text, inReplyTo: d.kind === "reply" ? { messageId: "demo-1@shop.test", references: null } : null, aiDraft: true, warnings: d.warnings }, 40);
  },
  async ai_dismiss_draft(a: { id: number }) {
    const d = drafts.get(a.id);
    if (!d) throw "That draft no longer exists.";
    if (d.state !== "proposed") throw d.state === "opened" ? "That draft was already opened in Compose." : "That draft was dismissed.";
    d.state = "dismissed"; d.updatedAt = sec();
    return wait(d, 40);
  },
  async ai_purge_now(): Promise<PurgeReport> { return wait({ egressRowsDeleted: 3, conversationsDeleted: 0 }, 300); },

  async ai_send(a: { convId: string; text: string; onEvent: { onmessage: (e: AiEvent) => void } }) {
    const x = convs.get(a.convId);
    if (!x) throw "Conversation not found.";
    const emit = a.onEvent.onmessage;
    cancelled.delete(a.convId);
    const t = sec();
    x.messages.push({ id: uid("m"), role: "user", text: a.text, createdAt: t });
    if (x.c.title === "New conversation") x.c.title = a.text.slice(0, 40);
    const pause = async (ms: number) => { await new Promise((r) => setTimeout(r, ms)); return cancelled.has(a.convId); };
    const stopNow = (out: string, chips: ToolChip[]) => {
      cancelled.delete(a.convId);
      for (const c of chips) if (c.running) { c.running = false; c.ok = false; c.summary = "stopped"; }
      x.messages.push({ id: uid("m"), role: "assistant", text: out, createdAt: sec(), tools: chips.length ? chips.map((c) => ({ ...c })) : undefined });
      emit({ type: "done", stop: "cancelled" });
    };
    const log = (round: number, extra: unknown[], items: EgressItem[]) => {
      const req = { model: x.c.model, messages: [{ role: "user", content: a.text }, ...extra], consent: x.consent.redaction, tools: x.consent.tools ? ["search_mail", "get_message", "get_thread", "…"] : undefined };
      const json = JSON.stringify(req);
      x.egress.push({ id: uid("eg"), at: sec(), host: x.c.endpointHost, model: x.c.model, bytes: json.length, estTokens: Math.ceil(json.length / 4), requestJson: json, items: items.map((i) => ({ ...i })), round });
      return Math.ceil(json.length / 4);
    };

    const calTurn = !!x.c.calendarEnabled && /\b(calendar|schedule|meeting)\b/i.test(a.text);
    // Mail tools: a lookup-looking question runs a search, then reads one message (browser dev mode only).
    const chips: ToolChip[] = [];
    let inTok = 0;
    if (x.consent.tools && !calTurn && /\b(mail|inbox|search|find|invoice|unread|from:|who|when)\b/i.test(a.text) && !/\berror\b/i.test(a.text)) {
      const q = (/from:\S+/i.exec(a.text)?.[0] ?? a.text.replace(/[^\w:@. -]/g, "").trim().split(/\s+/).slice(0, 3).join(" ")) || "mail";
      const items: EgressItem[] = [
        { ref: "m:1", subject: "Quarterly invoice", from: "Alice Example" },
        { ref: "m:2", subject: "Re: Quarterly invoice", from: "Bob" },
      ];
      inTok += log(0, [], []);
      const c1: ToolChip = { id: uid("tc"), name: "search_mail", label: q, summary: "", ok: true, running: true };
      chips.push(c1);
      emit({ type: "toolStart", id: c1.id, name: c1.name, label: c1.label });
      if (await pause(500)) return stopNow("", chips);
      c1.running = false; c1.summary = "2 results";
      emit({ type: "toolEnd", id: c1.id, ok: true, summary: c1.summary });
      inTok += log(1, [{ role: "tool", content: "2 matching message(s) …" }], items);
      const c2: ToolChip = { id: uid("tc"), name: "get_message", label: "m:1", summary: "", ok: true, running: true };
      chips.push(c2);
      emit({ type: "toolStart", id: c2.id, name: c2.name, label: c2.label });
      if (await pause(500)) return stopNow("", chips);
      c2.running = false; c2.summary = "read";
      emit({ type: "toolEnd", id: c2.id, ok: true, summary: "read" });
      const c3: ToolChip = { id: uid("tc"), name: "get_message", label: "m:9", summary: "", ok: true, running: true };
      chips.push(c3);
      emit({ type: "toolStart", id: c3.id, name: c3.name, label: c3.label });
      await pause(200);
      c3.running = false; c3.ok = false; c3.summary = "failed";
      emit({ type: "toolEnd", id: c3.id, ok: false, summary: "failed" });
      inTok += log(2, [{ role: "tool", content: "2 matching message(s) …" }, { role: "tool", content: "Message m:1 …" }], items);
    } else {
      inTok += log(0, [], []);
    }

    // Calendar: look at the week, find free time, then suggest a lunch event (browser dev mode only).
    if (calTurn) {
      const step = async (name: string, label: string, summary: string) => {
        const c: ToolChip = { id: uid("tc"), name, label, summary: "", ok: true, running: true };
        chips.push(c);
        emit({ type: "toolStart", id: c.id, name, label });
        const stop = await pause(400);
        c.running = false; c.summary = summary;
        emit({ type: "toolEnd", id: c.id, ok: true, summary });
        return stop;
      };
      if (await step("list_events", "2026-11-09 \u2192 2026-11-16", "6 events")) return stopNow("", chips);
      if (await step("find_free_slots", "60 min, 2026-11-09 \u2192 2026-11-16", "2 slots")) return stopNow("", chips);
      if (x.consent.suggest) {
        const c6: ToolChip = { id: uid("tc"), name: "propose_event", label: "Lunch with Ravi", summary: "waiting for approval", ok: true };
        chips.push(c6);
        const pid = nextProposal++;
        const title = "Create 'Lunch with Ravi' Fri 13 Nov 13:00\u201314:00 on Personal";
        const d: ProposalDetail = {
          id: pid, convId: x.c.id, kind: "event_create", state: "proposed", title, reason: "Ravi asked for lunch next week and Friday is free.",
          warnings: ["Conflicts with 'Gym' (Fri 13 Nov 12:30\u201313:30).", "ravi@example.org never appears in your mail.", "Approving will send invitations by email to 1 person (ravi@example.org)."],
          error: "", createdAt: sec(), updatedAt: sec(), expiresAt: sec() + 86400, counts: counts([]), undoable: false, items: [],
          event: {
            calendarName: "Personal", notify: true, undoNote: "Undo deletes the event and sends a cancellation to the invitee.",
            rows: [{ label: "Title", after: "Lunch with Ravi" }, { label: "When", after: "Fri 13 Nov 13:00\u201314:00" }, { label: "Calendar", after: "Personal" }, { label: "Invitees", after: "ravi@example.org" }],
          },
        };
        proposals.set(pid, d);
        emit({ type: "toolEnd", id: c6.id, ok: true, summary: c6.summary });
        emit({ type: "proposal", id: String(pid), summary: title });
      }
    }

    // A tidy-up request queues a suggestion (needs "suggest changes" in the consent).
    if (x.consent.tools && x.consent.suggest && /\b(archive|trash|tidy|clean|newsletters?)\b/i.test(a.text)) {
      const c4: ToolChip = { id: uid("tc"), name: "propose_archive", label: "6 messages", summary: "", ok: true, running: true };
      chips.push(c4);
      emit({ type: "toolStart", id: c4.id, name: c4.name, label: c4.label });
      if (await pause(300)) return stopNow("", chips);
      c4.running = false; c4.summary = "waiting for approval";
      const pid = nextProposal++;
      const items: ProposalItem[] = Array.from({ length: 6 }, (_, i) => ({
        idx: i, account: "demo@example.org", folder: "INBOX", sender: SENDERS[i % SENDERS.length], subject: `Newsletter #${40 + i}: this week's offers`, seen: i % 3 !== 0, flagged: false,
        toFolder: "Archive", selected: true, status: "pending", error: "",
      }));
      const unread = items.filter((i) => !i.seen).length;
      const d: ProposalDetail = {
        id: pid, convId: x.c.id, kind: "archive", state: "proposed", title: `Archive ${items.length} messages`, reason: "They look like promotional newsletters you never opened.",
        warnings: unread ? [`includes ${unread} unread`] : [], error: "", createdAt: sec(), updatedAt: sec(), expiresAt: sec() + 86400, counts: counts(items), undoable: false, items,
      };
      proposals.set(pid, d);
      emit({ type: "toolEnd", id: c4.id, ok: true, summary: c4.summary });
      emit({ type: "proposal", id: String(pid), summary: d.title });
    }

    // A "reply" / "draft" request writes a reply draft plus a new message to an unfamiliar address (needs "suggest changes").
    if (x.consent.tools && x.consent.suggest && /\b(reply|draft|write to)\b/i.test(a.text)) {
      const c5: ToolChip = { id: uid("tc"), name: "draft_reply", label: "m:1", summary: "", ok: true, running: true };
      chips.push(c5);
      emit({ type: "toolStart", id: c5.id, name: c5.name, label: c5.label });
      if (await pause(300)) return stopNow("", chips);
      c5.running = false; c5.summary = "draft ready for review";
      const mk = (o: Partial<DraftSummary>): DraftSummary => ({
        id: nextDraft++, convId: x.c.id, kind: "reply", state: "proposed", title: "", account: "demo@example.org", to: [], cc: [], subject: "", body: "", quote: "", reason: "",
        warnings: [], newAddresses: [], createdAt: sec(), updatedAt: sec(), ...o,
      });
      const r = mk({
        title: "Reply to alice@example.org: Re: Quarterly invoice", to: ["alice@example.org"], cc: ["bob@example.org"], subject: "Re: Quarterly invoice",
        body: "Hi Alice,\n\nThanks for the reminder. I will pay the invoice on Friday and send you the confirmation.\n\nBest,\nDemo", quote: "On 2026-10-26 07:33 UTC, Alice Example <alice@example.org> wrote:\n> Please pay the Q3 invoice.",
        reason: "You asked for a short, polite reply.",
      });
      const n = mk({
        kind: "new", title: "New message to carol@new-company.test: Intro", to: ["carol@new-company.test"], subject: "Intro", body: "Hello Carol, nice to meet you. See https://example.org/about for who we are.",
        newAddresses: ["carol@new-company.test"], warnings: ["New address: carol@new-company.test does not appear anywhere in your mail. Check it carefully.", "The text contains 1 link. Check where it leads before sending."],
      });
      for (const d of [r, n]) { drafts.set(d.id, d); }
      emit({ type: "toolEnd", id: c5.id, ok: true, summary: c5.summary });
      emit({ type: "draft", id: String(r.id), summary: r.title });
      emit({ type: "draft", id: String(n.id), summary: n.title });
    }

    const chunks = /\berror\b/i.test(a.text) ? REPLY.slice(0, 1) : calTurn ? CAL_REPLY : /\blong\b/i.test(a.text) ? LONG : REPLY;
    let out = "";
    for (const ch of chunks) {
      for (const piece of ch.match(/.{1,12}/gs) ?? []) {
        if (await pause(25)) return stopNow(out, chips);
        out += piece;
        emit({ type: "text", delta: piece });
      }
    }
    if (/\berror\b/i.test(a.text)) {
      emit({ type: "error", message: "The provider returned an error (HTTP 529: overloaded). Try again.", kind: "provider" });
      return;
    }
    x.messages.push({ id: uid("m"), role: "assistant", text: out, createdAt: sec(), tools: chips.length ? chips.map((c) => ({ ...c })) : undefined });
    x.c.updatedAt = sec();
    emit({ type: "usage", input: inTok, output: Math.ceil(out.length / 4) });
    emit({ type: "done", stop: "end_turn" });
  },
};
