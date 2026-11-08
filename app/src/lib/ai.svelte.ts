// AI chat store: profiles, settings, the open conversation, streaming state and the token meter.
// Static import (bundled with the app); nothing is fetched from the backend until the AI pane or tab calls load().
import { aiApi, aiSend, onDraftEvent, onProposalEvent } from "./aiapi";
import { api, errMsg } from "./api";
import { app } from "./state.svelte";
import { workspace } from "./workspace.svelte";
import { DEFAULT_SETTINGS, decidedAll, followUpText, isEventKind, resultText, selectedIdx, tokenMeter, withTimeout } from "./aivm";
import type { AiEvent, AiSettings, Consent, Conversation, DraftEvent, DraftSummary, EgressRow, Profile, ProposalDetail, ProposalEvent, ProposalSummary, TestResult, UiMessage } from "./aitypes";

const summaryOf = (d: ProposalDetail): ProposalSummary => { const { items: _items, ...s } = d; return s; };

const lid = (p: string) => `${p}-${crypto.randomUUID().replace(/-/g, "").slice(0, 10)}`;
const nowSec = () => Math.floor(Date.now() / 1000);

class Ai {
  loaded = $state(false);
  loading = $state(false);
  /** Why the last load() failed (shown with a Retry button instead of an endless skeleton). */
  loadError = $state<string | null>(null);
  profiles = $state<Profile[]>([]);
  settings = $state<AiSettings>(structuredClone(DEFAULT_SETTINGS));
  conversations = $state<Conversation[]>([]);
  current = $state<Conversation | null>(null);
  messages = $state<UiMessage[]>([]);
  egress = $state.raw<EgressRow[]>([]);
  streaming = $state(false);
  stopping = $state(false);
  consentOpen = $state(false);
  /** "Edit permissions" dialog for the open conversation: the consent it starts from (null = closed). */
  editConsent = $state<Consent | null>(null);
  /** Estimated tokens used in the open conversation (sum of usage events, seeded from the egress log). */
  convTokens = $state(0);
  turnTokens = $state(0);
  /** Suggestions (proposals) of the open conversation, oldest first. */
  proposals = $state<ProposalSummary[]>([]);
  /** Preview rows, loaded when a card is expanded. The user's un-ticking lives here until Approve. */
  details = $state<Record<number, ProposalDetail>>({});
  /** Proposals with a request in flight. */
  pbusy = $state<Record<number, "approving" | "rejecting" | "undoing">>({});
  /** Short result text from the last approval (calendar suggestions); only known in this session. */
  notes = $state<Record<number, string>>({});
  /** Activity log: suggestions of every conversation, newest first. */
  activity = $state<ProposalSummary[]>([]);
  activityOpen = $state(false);
  /** Drafts the assistant wrote in the open conversation, oldest first. Nothing here can send; Compose does that. */
  drafts = $state<DraftSummary[]>([]);
  /** Drafts with a request in flight. */
  dbusy = $state<Record<number, "opening" | "dismissing">>({});
  private loadSeq = 0;
  private listening = false;

  get activeProfile(): Profile | null {
    return this.profiles.find((p) => p.id === this.current?.profileId) ?? null;
  }
  get meter() { return tokenMeter(this.convTokens, this.settings.perConversationTokens); }
  get turnMeter() { return tokenMeter(this.turnTokens, this.settings.perTurnTokens); }
  /** Providers that can be used right now. */
  get usable(): Profile[] { return this.profiles.filter((p) => !p.needsKeyReentry); }

  async load(force = false) {
    if ((this.loaded && !force) || this.loading) return;
    this.loading = true;
    this.loadError = null;
    try {
      const [p, s, c] = await Promise.all([
        withTimeout(aiApi.listProfiles(), "ai_list_profiles"),
        withTimeout(aiApi.getSettings(), "ai_get_settings"),
        withTimeout(aiApi.listConversations(), "ai_list_conversations"),
      ]);
      this.profiles = p; this.settings = s; this.conversations = c;
      this.loaded = true;
      void this.listen();
      if (!this.current && c.length) await this.open(c[0].id);
    } catch (e) {
      if (!this.loaded) this.loadError = errMsg(e);
      app.fail(e);
    }
    this.loading = false;
  }
  async reloadProfiles() {
    try { this.profiles = await aiApi.listProfiles(); } catch (e) { app.fail(e); }
  }

  // ---- providers + settings ----
  async saveProfile(p: Profile, apiKey: string | null): Promise<Profile> {
    const saved = await aiApi.saveProfile(p, apiKey);
    const i = this.profiles.findIndex((x) => x.id === saved.id);
    this.profiles = i < 0 ? [...this.profiles, saved] : this.profiles.map((x) => (x.id === saved.id ? saved : x));
    return saved;
  }
  async deleteProfile(id: string) {
    await aiApi.deleteProfile(id);
    this.profiles = this.profiles.filter((p) => p.id !== id);
  }
  test(p: Profile, apiKey: string | null): Promise<TestResult> { return aiApi.testProfile(p, apiKey); }
  listModels(p: Profile, apiKey: string | null): Promise<TestResult> { return aiApi.listModels(p, apiKey); }
  async saveSettings(s: AiSettings) { this.settings = await aiApi.setSettings(s); return this.settings; }

  // ---- conversations ----
  async open(id: string) {
    if (this.streaming) return;
    const seq = ++this.loadSeq;
    try {
      const full = await aiApi.getConversation(id);
      if (seq !== this.loadSeq) return;
      this.current = full.conversation;
      this.messages = full.messages.map((m) => ({ ...m }));
      this.convTokens = 0; this.turnTokens = 0;
      this.details = {};
      void this.loadProposals();
      void this.loadDrafts();
      await this.loadEgress();
      this.convTokens = this.egress.reduce((n, r) => n + (r.estTokens || 0), 0);
    } catch (e) { app.fail(e); }
  }
  async loadEgress() {
    const c = this.current;
    if (!c) { this.egress = []; return; }
    try { const rows = await aiApi.egress(c.id, 200); if (this.current?.id === c.id) this.egress = rows; } catch { /* inspector just shows nothing */ }
  }
  async start(profileId: string, consent: Consent) {
    const c = await aiApi.newConversation(profileId, consent);
    this.conversations = [c, ...this.conversations.filter((x) => x.id !== c.id)];
    this.loadSeq++;
    this.current = c; this.messages = []; this.egress = []; this.convTokens = 0; this.turnTokens = 0; this.proposals = []; this.details = {}; this.drafts = [];
    return c;
  }
  /** What "Edit permissions" prefills: the open conversation's consent as the backend holds it now. Read-only; nothing is sent anywhere. */
  async currentConsent(): Promise<Consent | null> {
    const c = this.current;
    if (!c) return null;
    return (await aiApi.getConversation(c.id)).consent ?? null;
  }
  /** Save from the "Edit permissions" dialog. Rejects (with the backend's reason) while a turn or an approval is running. */
  async updateConsent(consent: Consent) {
    const c = this.current;
    if (!c) return;
    const full = await aiApi.updateConsent(c.id, consent);
    if (this.current?.id !== c.id) return;
    this.current = full.conversation;
    this.conversations = this.conversations.map((x) => (x.id === full.conversation.id ? full.conversation : x));
    // Keep the thread as it is (cards, chips); add only the new notice line(s).
    const have = new Set(this.messages.map((m) => m.id));
    for (const m of full.messages) if (m.role === "notice" && !have.has(m.id)) this.messages.push({ ...m });
  }
  async remove(id: string) {
    await aiApi.deleteConversation(id);
    this.conversations = this.conversations.filter((c) => c.id !== id);
    if (this.current?.id === id) {
      this.current = null; this.messages = []; this.egress = []; this.convTokens = 0; this.proposals = []; this.details = {}; this.drafts = [];
      if (this.conversations.length) await this.open(this.conversations[0].id);
    }
  }

  // ---- streaming ----
  async send(text: string) {
    const conv = this.current;
    const t = text.trim();
    if (!conv || !t || this.streaming) return;
    const created = nowSec();
    this.messages.push({ id: lid("u"), role: "user", text: t, createdAt: created });
    this.messages.push({ id: lid("a"), role: "assistant", text: "", createdAt: created, streaming: true });
    const m = this.messages[this.messages.length - 1];
    this.streaming = true; this.stopping = false; this.turnTokens = 0;

    let pending = "", raf = 0, terminal = false;
    const flush = () => { raf = 0; if (pending) { m.text += pending; pending = ""; } };
    // A turn that ends (or is stopped) while a tool step is still marked running: it is not running any more.
    const settle = () => { for (const t of m.tools ?? []) if (t.running) { t.running = false; t.ok = false; t.summary = "stopped"; } };
    const onEvent = (e: AiEvent) => {
      switch (e.type) {
        case "text":
          pending += e.delta;
          if (!raf) raf = typeof requestAnimationFrame === "function" ? requestAnimationFrame(flush) : (setTimeout(flush, 16) as unknown as number);
          break;
        case "usage": { const n = (e.input || 0) + (e.output || 0); this.turnTokens = n; this.convTokens += n; break; }
        case "done": terminal = true; flush(); m.streaming = false; settle(); if (e.stop === "cancelled" || this.stopping) m.stopped = true; if (e.stop === "tool_limit") m.toolLimit = true; break;
        case "error": terminal = true; flush(); m.streaming = false; settle(); m.error = { message: e.message, kind: e.kind }; break;
        case "toolStart": (m.tools ??= []).push({ id: e.id, name: e.name, label: e.label, summary: "", ok: true, running: true }); break;
        case "toolEnd": { const c = m.tools?.find((t) => t.id === e.id); if (c) { c.ok = e.ok; c.summary = e.summary; c.running = false; } break; }
        case "proposal": { const pid = Number(e.id); (m.proposalIds ??= []).push(pid); void this.refreshProposal(pid); break; }
        case "draft": { const did = Number(e.id); (m.draftIds ??= []).push(did); void this.refreshDraft(did); break; }
        case "rule": { (m.ruleIds ??= []).push(Number(e.id)); break; }
        default: break;
      }
    };
    try {
      await aiSend(conv.id, t, onEvent);
    } catch (e) {
      if (!terminal) { terminal = true; flush(); m.streaming = false; settle(); m.error = { message: errMsg(e), kind: "request" }; }
    }
    if (raf) { if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(raf); flush(); }
    if (!terminal) { m.streaming = false; settle(); if (this.stopping) m.stopped = true; }
    this.streaming = false; this.stopping = false;
    if (this.current?.id === conv.id) void this.loadEgress();
    void aiApi.listConversations().then((c) => (this.conversations = c)).catch(() => {});
    if (this.followQueued) { this.followQueued = false; void this.followUp(); }
  }
  // ---- proposals: suggested changes, approved one by one ----

  /** Listen for state changes pushed by the backend (once). */
  private async listen() {
    if (this.listening) return;
    this.listening = true;
    try { await onProposalEvent((e) => this.applyEvent(e)); } catch { this.listening = false; }
    try { await onDraftEvent((e) => this.applyDraftEvent(e)); } catch { /* cards still update from the command results */ }
  }
  /** Progress / state push: update the card in place; fetch the rows again once the run is over. */
  applyEvent(e: ProposalEvent) {
    const patch = (p: ProposalSummary) => { p.state = e.state; p.counts = { ...e.counts }; };
    const a = this.proposals.find((p) => p.id === e.id); if (a) patch(a);
    const b = this.activity.find((p) => p.id === e.id); if (b) patch(b);
    const d = this.details[e.id]; if (d) patch(d);
    if (e.state !== "approved" && e.state !== "executing") void this.refreshProposal(e.id);
  }
  private upsert(p: ProposalSummary) {
    const cid = this.current?.id;
    if (cid !== undefined && String(p.convId) === String(cid)) {
      const i = this.proposals.findIndex((x) => x.id === p.id);
      if (i >= 0) this.proposals[i] = p; else this.proposals = [...this.proposals, p].sort((x, y) => x.createdAt - y.createdAt || x.id - y.id);
    }
    const j = this.activity.findIndex((x) => x.id === p.id);
    if (j >= 0) this.activity[j] = p;
  }
  async loadProposals() {
    const c = this.current;
    if (!c) { this.proposals = []; return; }
    try { const l = await aiApi.listProposals(c.id, 200); if (this.current?.id === c.id) this.proposals = [...l].reverse(); } catch { /* cards just don't show */ }
  }
  async loadActivity() {
    try { this.activity = await aiApi.listProposals(null, 200); } catch (e) { app.fail(e); }
  }
  async refreshProposal(id: number) {
    try {
      const d = await aiApi.getProposal(id);
      // Keep the user's un-ticked rows while the proposal is still waiting.
      const old = this.details[id];
      if (old && d.state === "proposed") for (const it of d.items) { const o = old.items.find((x) => x.idx === it.idx); if (o) it.selected = o.selected; }
      this.details[id] = d;
      this.upsert(summaryOf(d));
    } catch { /* a purged conversation takes its cards with it */ }
  }
  /** Expanding a card loads its rows. */
  async loadDetail(id: number) { if (!this.details[id]) await this.refreshProposal(id); }
  setSelected(id: number, idx: number, on: boolean) {
    const it = this.details[id]?.items.find((x) => x.idx === idx);
    if (it) it.selected = on;
  }
  setAllSelected(id: number, on: boolean) { for (const it of this.details[id]?.items ?? []) it.selected = on; }

  async approve(id: number) {
    if (this.pbusy[id]) return;
    const d = this.details[id];
    const selected = selectedIdx(d?.items ?? []);
    this.pbusy[id] = "approving";
    try {
      const r = await aiApi.approveProposal(id, selected);
      this.details[id] = r.proposal; this.upsert(summaryOf(r.proposal));
      if (r.note) this.notes[id] = r.note;
      app.notify(r.note || resultText(r.proposal));
      if (r.foldersChanged && app.account) { try { app.folders = await api.listFolders(app.account); } catch { /* sidebar refreshes on the next sync */ } }
    } catch (e) { app.fail(e); await this.refreshProposal(id); }
    delete this.pbusy[id];
    void this.followUp();
  }
  private followed = new Set<string>();
  private followQueued = false;
  /** After the user decides the last open card of a reply, tell the assistant so it carries on without being asked again. */
  private async followUp() {
    const conv = this.current;
    if (!conv) return;
    if (this.streaming) { this.followQueued = true; return; }
    // The cards of the current turn = every suggestion made since the user's last message. (Works for chats reopened from history too.)
    const lastUser = [...this.messages].reverse().find((m) => m.role === "user");
    if (!lastUser || this.followed.has(lastUser.id)) return;
    const sums = this.proposals.filter((p) => p.createdAt >= lastUser.createdAt);
    if (!decidedAll(sums)) return;
    this.followed.add(lastUser.id);
    await this.send(followUpText(sums));
  }
  async reject(id: number) {
    if (this.pbusy[id]) return;
    this.pbusy[id] = "rejecting";
    try { const d = await aiApi.rejectProposal(id); this.details[id] = d; this.upsert(summaryOf(d)); }
    catch (e) { app.fail(e); await this.refreshProposal(id); }
    delete this.pbusy[id];
    void this.followUp();
  }
  async undo(id: number) {
    if (this.pbusy[id]) return;
    this.pbusy[id] = "undoing";
    try {
      const r = await aiApi.undoProposal(id);
      this.details[id] = r.proposal; this.upsert(summaryOf(r.proposal));
      if (isEventKind(r.proposal.kind)) app.notify(r.failed ? (r.errors[0] || "Could not undo.") : "Undone.", r.failed ? "error" : "info");
      else app.notify(r.failed ? `Put back ${r.undone}; ${r.failed} could not be put back.${r.errors[0] ? " " + r.errors[0] : ""}` : `Put back ${r.undone}.`, r.failed ? "error" : "info");
    } catch (e) { app.fail(e); await this.refreshProposal(id); }
    delete this.pbusy[id];
  }

  // ---- drafts: written by the assistant, opened in Compose by the user, sent only from Compose ----

  private upsertDraft(d: DraftSummary) {
    const cid = this.current?.id;
    if (cid === undefined || String(d.convId) !== String(cid)) return;
    const i = this.drafts.findIndex((x) => x.id === d.id);
    if (i >= 0) this.drafts[i] = d; else this.drafts = [...this.drafts, d].sort((x, y) => x.createdAt - y.createdAt || x.id - y.id);
  }
  applyDraftEvent(e: DraftEvent) {
    const d = this.drafts.find((x) => x.id === e.id);
    if (d) d.state = e.state;
  }
  async loadDrafts() {
    const c = this.current;
    if (!c) { this.drafts = []; return; }
    try { const l = await aiApi.listDrafts(c.id, 200); if (this.current?.id === c.id) this.drafts = [...l].reverse(); } catch { /* cards just don't show */ }
  }
  async refreshDraft(id: number) {
    try { this.upsertDraft(await aiApi.getDraft(id)); } catch { /* a purged conversation takes its drafts with it */ }
  }
  /** Create a Compose pane prefilled from the draft. The user reads it and presses Send there; nothing is sent here. */
  async openDraft(id: number) {
    if (this.dbusy[id]) return;
    // Already open (or parked) from this draft: show that pane instead of making a second copy.
    const existing = workspace.findAiDraftPane(id);
    if (existing) { workspace.reveal(existing); return; }
    this.dbusy[id] = "opening";
    try {
      const pre = await aiApi.openDraft(id);
      workspace.openCompose({ to: pre.to, cc: pre.cc, bcc: pre.bcc, subject: pre.subject, text: pre.text, inReplyTo: pre.inReplyTo }, pre.account, { aiDraft: pre.aiDraft, aiDraftId: pre.id, aiWarnings: pre.warnings });
      const d = this.drafts.find((x) => x.id === id);
      if (d) d.state = "opened";
    } catch (e) { app.fail(e); await this.refreshDraft(id); }
    delete this.dbusy[id];
  }
  async dismissDraft(id: number) {
    if (this.dbusy[id]) return;
    this.dbusy[id] = "dismissing";
    try { this.upsertDraft(await aiApi.dismissDraft(id)); }
    catch (e) { app.fail(e); await this.refreshDraft(id); }
    delete this.dbusy[id];
  }

  async stop() {
    const c = this.current;
    if (!c || !this.streaming || this.stopping) return;
    this.stopping = true;
    try { await aiApi.cancel(c.id); } catch (e) { app.fail(e); }
  }
  /** Re-send the prompt of the last failed turn. (The backend may keep the failed attempt in its history.) */
  async retry() {
    if (this.streaming) return;
    const last = this.messages[this.messages.length - 1];
    if (!last?.error) return;
    const prev = this.messages[this.messages.length - 2];
    if (!prev || prev.role !== "user") return;
    const text = prev.text;
    this.messages.splice(this.messages.length - 2, 2);
    await this.send(text);
  }
}

export const ai = new Ai();
