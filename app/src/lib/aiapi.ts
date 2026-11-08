// Typed gateway to the AI commands. All traffic to the model goes through Rust; the frontend never makes a network call.
// In a plain browser the in-memory mock (lib/mockai.ts, loaded on demand) answers instead.
import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isTauri } from "./api";
import type { AiEvent, AiSettings, ApproveResult, AutoReplyConfig, AutomationState, Consent, Conversation, ConversationFull, DraftEvent, DraftPrefill, DraftSummary, DryRun, EgressRow, Profile, ProposalDetail, ProposalEvent, ProposalSummary, PurgeReport, ReplyLogRow, ReplyPreview, Rule, RuleCard, RuleInput, RuleRun, RuleSpec, TestResult, ToneReport, UndoResult } from "./aitypes";

async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri) return invoke<T>(cmd, args);
  const { mockAi } = await import("./mockai");
  const fn = (mockAi as unknown as Record<string, (a: Record<string, unknown>) => Promise<unknown>>)[cmd];
  if (!fn) throw `Unknown mock command ${cmd}`;
  // Same as the IPC: the backend receives JSON, never a live $state proxy (which the mock could not structuredClone).
  return (await fn(JSON.parse(JSON.stringify(args)) as Record<string, unknown>)) as T;
}

// The Rust side uses numeric ids for conversations, messages and egress rows; the UI keeps them as strings.
// Convert at this boundary only (the in-browser mock already uses string ids).
const toWireId = (id: string): string | number => (isTauri ? Number(id) : id);
const normConv = (c: Conversation): Conversation => ({ ...c, id: String(c.id) });
const normFull = (f: ConversationFull): ConversationFull => ({
  conversation: normConv(f.conversation),
  messages: f.messages.map((m) => ({ ...m, id: String(m.id) })),
  consent: f.consent,
});
const normEgress = (r: EgressRow): EgressRow => ({ ...r, id: String(r.id) });
// The Rust side lists egress rows newest first; the UI wants them oldest first (the mock already does).

export const aiApi = {
  listProfiles: () => call<Profile[]>("ai_list_profiles"),
  /** apiKey: undefined/null keeps the saved key, "" clears it, any other string replaces it. */
  saveProfile: (profile: Profile | Omit<Profile, "hasKey" | "needsKeyReentry" | "warnings">, apiKey?: string | null) =>
    call<Profile>("ai_save_profile", { profile, apiKey: apiKey ?? null }),
  deleteProfile: (id: string) => call<void>("ai_delete_profile", { id }),
  testProfile: (profile: Profile | Omit<Profile, "hasKey" | "needsKeyReentry" | "warnings">, apiKey?: string | null) =>
    call<TestResult>("ai_test_profile", { profile, apiKey: apiKey ?? null }),
  /** Model list only (one GET /models; never a chat request). */
  listModels: (profile: Profile | Omit<Profile, "hasKey" | "needsKeyReentry" | "warnings">, apiKey?: string | null) =>
    call<TestResult>("ai_list_models", { profile, apiKey: apiKey ?? null }),
  /** "Check tone" in Compose: ONE request carrying only `text` (the draft's own words). Nothing is stored. */
  checkTone: (profileId: string, text: string) => call<ToneReport>("ai_check_tone", { profileId, text }).then((r) => ({ ...r, restore: r.restore ?? [] })),
  getSettings: () => call<AiSettings>("ai_get_settings"),
  setSettings: (s: AiSettings) => call<AiSettings>("ai_set_settings", { settings: s }),
  newConversation: (profileId: string, consent: Consent) => call<Conversation>("ai_new_conversation", { profileId, consent }).then(normConv),
  /** "Edit permissions" Save. The only way a conversation's consent changes; the backend refuses while a turn or an approval is running. */
  updateConsent: (convId: string, consent: Consent) => call<ConversationFull>("ai_update_consent", { convId: toWireId(convId), consent }).then(normFull),
  listConversations: () => call<Conversation[]>("ai_list_conversations").then((l) => l.map(normConv)),
  getConversation: (id: string) => call<ConversationFull>("ai_get_conversation", { id: toWireId(id) }).then(normFull),
  deleteConversation: (id: string) => call<void>("ai_delete_conversation", { id: toWireId(id) }),
  cancel: (convId: string) => call<void>("ai_cancel", { convId: toWireId(convId) }),
  egress: (convId: string, limit: number) =>
    call<EgressRow[]>("ai_egress", { convId: toWireId(convId), limit }).then((l) => (isTauri ? [...l].reverse() : l).map(normEgress)),
  purgeNow: () => call<PurgeReport>("ai_purge_now"),
  // Proposals: the only way a suggested change reaches the mailbox is approveProposal (and undoProposal).
  /** `convId` omitted = the activity log across conversations. */
  listProposals: (convId?: string | null, limit = 100) => call<ProposalSummary[]>("ai_list_proposals", { convId: convId ? toWireId(convId) : null, limit }),
  getProposal: (id: number) => call<ProposalDetail>("ai_get_proposal", { id }),
  /** `selected`: `idx` of the preview rows the user ticked. Rows not listed are not executed. */
  approveProposal: (id: number, selected: number[] = []) => call<ApproveResult>("ai_approve_proposal", { id, selected }),
  rejectProposal: (id: number) => call<ProposalDetail>("ai_reject_proposal", { id }),
  undoProposal: (id: number) => call<UndoResult>("ai_undo_proposal", { id }),
  // Drafts: nothing here can send. openDraft returns the Compose prefill; the user presses Send in Compose.
  listDrafts: (convId?: string | null, limit = 100) => call<DraftSummary[]>("ai_list_drafts", { convId: convId ? toWireId(convId) : null, limit }),
  getDraft: (id: number) => call<DraftSummary>("ai_get_draft", { id }),
  openDraft: (id: number) => call<DraftPrefill>("ai_open_draft", { id }),
  dismissDraft: (id: number) => call<DraftSummary>("ai_dismiss_draft", { id }),
  // Rules (P4): no model involved. New rules are saved OFF; enabling starts at "mail that arrives from now on".
  rulesList: () => call<Rule[]>("ai_rules_list"),
  ruleSave: (rule: RuleInput) => call<Rule>("ai_rule_save", { rule }),
  ruleSetEnabled: (id: number, enabled: boolean) => call<Rule>("ai_rule_set_enabled", { id, enabled }),
  ruleDelete: (id: number) => call<void>("ai_rule_delete", { id }),
  /** The rule plus a dry run over existing mail. Changes nothing. */
  ruleCard: (id: number) => call<RuleCard>("ai_rule_card", { id }),
  ruleDryRun: (account: string, spec: RuleSpec) => call<DryRun>("ai_rule_dry_run", { account, spec }),
  /** "Apply to existing messages": how many cached messages the rule would change now. Changes nothing. */
  rulePreviewExisting: (id: number) => call<DryRun>("ai_rule_preview_existing", { id }),
  /** Run the rule once over the cached mail that matches. Each folder becomes one run that can be undone. */
  ruleApplyExisting: (id: number, token?: string | null) => call<RuleRun[]>("ai_rule_apply_existing", { id, token: token ?? null }),
  automationState: () => call<AutomationState>("ai_automation_state"),
  rulesPause: (paused: boolean) => call<AutomationState>("ai_rules_pause", { paused }),
  ruleRuns: (limit = 50) => call<RuleRun[]>("ai_rule_runs", { limit }),
  ruleRunUndo: (runId: number) => call<{ undone: number; failed: number; errors: string[] }>("ai_rule_run_undo", { runId }),
  // Auto-reply (P4): template only, OFF by default, sent through the account's own mail server.
  autoreplyGet: (account: string) => call<AutoReplyConfig>("ai_autoreply_get", { account }),
  autoreplySet: (account: string, config: AutoReplyConfig) => call<AutoReplyConfig>("ai_autoreply_set", { account, config }),
  autoreplyPause: (paused: boolean) => call<AutomationState>("ai_autoreply_pause", { paused }),
  /** Renders the reply for a made-up sender. NOTHING is sent. */
  autoreplyPreview: (account: string, config: AutoReplyConfig, subject?: string) => call<ReplyPreview>("ai_autoreply_preview", { account, config, subject: subject ?? null }),
  autoreplyLog: (account: string | null, limit = 100) => call<ReplyLogRow[]>("ai_autoreply_log", { account, limit }),
};

/** Subscribe to `ai://automation` (a rule ran or an auto-reply was decided). In a browser there are no pushes. */
export async function onAutomationEvent(cb: (e: { kind: "rules" | "autoreply" }) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listen<{ kind: "rules" | "autoreply" }>("ai://automation", (e) => cb(e.payload));
}

/** Subscribe to draft state changes (`ai://draft`). Returns the unsubscribe function. In a browser the mock has no pushes. */
export async function onDraftEvent(cb: (e: DraftEvent) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listen<DraftEvent>("ai://draft", (e) => cb(e.payload));
}

/** Subscribe to proposal state changes (`ai://proposal`). Returns the unsubscribe function. In a browser the mock has no pushes. */
export async function onProposalEvent(cb: (e: ProposalEvent) => void): Promise<() => void> {
  if (!isTauri) {
    const { mockAi } = await import("./mockai");
    return mockAi.subscribe(cb);
  }
  return listen<ProposalEvent>("ai://proposal", (e) => cb(e.payload));
}

/**
 * Stream one turn. Resolves when the backend call returns; `onEvent` receives every AiEvent in order.
 * A rejection (plain string) is passed to the caller, which turns it into an error state if no terminal event arrived.
 */
export async function aiSend(convId: string, text: string, onEvent: (e: AiEvent) => void): Promise<void> {
  if (isTauri) {
    const ch = new Channel<AiEvent>();
    ch.onmessage = onEvent;
    await invoke<void>("ai_send", { convId: toWireId(convId), text, onEvent: ch });
    return;
  }
  const { mockAi } = await import("./mockai");
  await mockAi.ai_send({ convId, text, onEvent: { onmessage: onEvent } });
}
