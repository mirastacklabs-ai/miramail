// Local rules + template auto-reply store (AI chat P4). Loaded with the AI Preferences tab / AI pane only.
// Nothing here talks to a model. Rules and auto-replies run in the Rust backend, only while MiraMail is open.
import { aiApi, onAutomationEvent } from "./aiapi";
import { app } from "./state.svelte";
import type { AutoReplyConfig, AutomationState, DryRun, ReplyLogRow, Rule, RuleCard, RuleInput, RuleRun } from "./aitypes";

class Auto {
  loaded = $state(false);
  rules = $state<Rule[]>([]);
  runs = $state<RuleRun[]>([]);
  state = $state<AutomationState>({ rulesPaused: false, autoreplyPaused: false });
  /** Dry runs by rule id (loaded when a card or the list is expanded). */
  cards = $state<Record<number, RuleCard>>({});
  busy = $state<Record<number, "enabling" | "disabling" | "deleting" | "saving">>({});
  runBusy = $state<Record<number, boolean>>({});
  private listening = false;

  async load(force = false) {
    if (this.loaded && !force) return;
    try {
      const [rules, runs, state] = await Promise.all([aiApi.rulesList(), aiApi.ruleRuns(50), aiApi.automationState()]);
      this.rules = rules; this.runs = runs; this.state = state; this.loaded = true;
      void this.listen();
    } catch (e) { app.fail(e); }
  }
  private async listen() {
    if (this.listening) return;
    this.listening = true;
    try { await onAutomationEvent(() => { void this.refreshRuns(); }); } catch { this.listening = false; }
  }
  async refreshRuns() {
    try { [this.runs, this.rules] = await Promise.all([aiApi.ruleRuns(50), aiApi.rulesList()]); } catch { /* the list just stays as it was */ }
  }

  private put(r: Rule) {
    const i = this.rules.findIndex((x) => x.id === r.id);
    this.rules = i < 0 ? [...this.rules, r] : this.rules.map((x) => (x.id === r.id ? r : x));
    if (this.cards[r.id]) this.cards[r.id] = { ...this.cards[r.id], rule: r };
  }

  /** Create or edit (both keep the rule OFF until it is switched on). */
  async save(input: RuleInput): Promise<Rule> {
    const r = await aiApi.ruleSave(input);
    this.put(r);
    delete this.cards[r.id];
    return r;
  }
  async setEnabled(id: number, on: boolean) {
    if (this.busy[id]) return;
    this.busy[id] = on ? "enabling" : "disabling";
    try {
      this.put(await aiApi.ruleSetEnabled(id, on));
      app.notify(on ? "Rule is on. It acts on mail that arrives from now on." : "Rule is off.");
    } catch (e) { app.fail(e); }
    delete this.busy[id];
  }
  async remove(id: number) {
    if (this.busy[id]) return;
    this.busy[id] = "deleting";
    try { await aiApi.ruleDelete(id); this.rules = this.rules.filter((r) => r.id !== id); delete this.cards[id]; }
    catch (e) { app.fail(e); }
    delete this.busy[id];
  }
  /** Rule description + preview over existing mail (changes nothing). */
  async card(id: number, force = false): Promise<RuleCard | null> {
    if (this.cards[id] && !force) return this.cards[id];
    try { const c = await aiApi.ruleCard(id); this.cards[id] = c; this.put(c.rule); return c; }
    catch { return null; }
  }
  dryRun(account: string, spec: RuleInput["spec"]): Promise<DryRun> { return aiApi.ruleDryRun(account, spec); }

  /** Count for the confirm dialog of "Apply to existing messages". */
  private tokens: Record<number, string | null | undefined> = {};
  async previewExisting(id: number): Promise<DryRun> { const d = await aiApi.rulePreviewExisting(id); this.tokens[id] = d.token; return d; }
  async applyExisting(id: number): Promise<RuleRun[] | null> {
    if (this.busy[id]) return null;
    this.busy[id] = "saving";
    try {
      const runs = await aiApi.ruleApplyExisting(id, this.tokens[id]);
      delete this.tokens[id];
      const n = runs.reduce((a, r) => a + r.matched, 0);
      app.notify(n ? `Applied to ${n} message${n === 1 ? "" : "s"}. You can undo it under Recent runs.` : "Nothing needed changing.");
      await this.refreshRuns();
      return runs;
    } catch (e) { app.fail(e); return null; }
    finally { delete this.busy[id]; }
  }

  async setRulesPaused(paused: boolean) {
    try { this.state = await aiApi.rulesPause(paused); } catch (e) { app.fail(e); }
  }
  async setReplyPaused(paused: boolean) {
    try { this.state = await aiApi.autoreplyPause(paused); } catch (e) { app.fail(e); }
  }
  async undoRun(id: number) {
    if (this.runBusy[id]) return;
    this.runBusy[id] = true;
    try {
      const r = await aiApi.ruleRunUndo(id);
      app.notify(r.failed ? `Put back ${r.undone}; ${r.failed} could not be put back.${r.errors[0] ? " " + r.errors[0] : ""}` : `Put back ${r.undone}.`, r.failed ? "error" : "info");
    } catch (e) { app.fail(e); }
    delete this.runBusy[id];
    await this.refreshRuns();
  }

  // ---- auto-reply (per account) ----
  getReply(account: string): Promise<AutoReplyConfig> { return aiApi.autoreplyGet(account); }
  saveReply(account: string, c: AutoReplyConfig): Promise<AutoReplyConfig> { return aiApi.autoreplySet(account, c); }
  replyLog(account: string | null): Promise<ReplyLogRow[]> { return aiApi.autoreplyLog(account, 100); }
}

export const auto = new Auto();
