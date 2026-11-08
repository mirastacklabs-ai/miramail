// Outbox store (main window only): the held / queued / failed messages, the Undo bar and the Outbox view.
// The backend owns the queue (docs/APP_API.md "Send pipeline"); this is a reactive mirror plus the user actions.
import { api, errMsg, onOutbox } from "./api";
import { app } from "./state.svelte";
import { workspace, type ComposePayload } from "./workspace.svelte";
import { actions, holding, interrupted, parseRestore, recipients, restoredPayload } from "./outboxmodel";
import type { OutboxItem } from "./types";

class OutboxStore {
  items = $state<OutboxItem[]>([]);
  /** Clock for countdowns; only ticks while something is counting. */
  now = $state(Date.now());
  /** The Outbox view is open. */
  open = $state(false);
  private ticker: ReturnType<typeof setInterval> | undefined;
  private started = false;
  private asked = new Set<string>();

  /** Entries still inside their undo window (drives the "Sending… Undo" bar). */
  get held() { return holding(this.items, this.now); }

  async init() {
    if (this.started) return;
    this.started = true;
    await onOutbox((e) => {
      if (e.kind === "sent") app.notify("Message sent");
      else if (e.kind === "failed") app.notify("A message could not be sent. It is waiting in the Outbox.", "error");
      else if (e.kind === "unknown") app.notify("A message may or may not have gone out. Open the Outbox to decide.", "error");
      void this.refresh();
    });
    await this.refresh();
    void this.promptInterrupted();
  }

  async refresh() {
    try { this.items = await api.outboxList(); } catch { /* backend unavailable: keep what we have */ }
    this.syncTicker();
    if (!this.items.length) this.open = false;
  }

  private syncTicker() {
    const need = this.items.some((i) => i.state === "holding" || (i.state === "failed" && i.nextRetryAt != null));
    if (need && !this.ticker) this.ticker = setInterval(() => { this.now = Date.now(); }, 250);
    else if (!need && this.ticker) { clearInterval(this.ticker); this.ticker = undefined; }
    if (need) this.now = Date.now();
  }

  /** Launch: a message held when the app was closed is never sent or dropped on its own; ask once per entry. */
  async promptInterrupted() {
    for (const it of interrupted(this.items)) {
      if (this.asked.has(it.id)) continue;
      this.asked.add(it.id);
      const send = await app.ask({
        title: "You have an unsent message",
        body: `To ${recipients(it)}${it.subject ? ` — “${it.subject.slice(0, 60)}”` : ""}. ${it.scheduledAt != null ? "It was scheduled for a time when MiraMail was closed." : "MiraMail was closed before it went out."} Send it now, or leave it in the Outbox to edit or discard.`,
        ok: "Send now",
      });
      if (send) await this.retry(it.id);
      else this.open = true;
    }
  }

  /** Undo while held, or Edit when it failed / may have been sent: back into an open Compose pane. */
  async restore(id: string) {
    const it = this.items.find((i) => i.id === id);
    if (it && !actions(it).undo && !actions(it).edit) { app.notify("It is being sent right now, too late to undo."); return; }
    try {
      const r = await api.outboxUndo(id);
      const raw = parseRestore(r.restore);
      if (!raw) { app.notify("The message could not be restored.", "error"); return; }
      const payload = { ...restoredPayload(raw), base: JSON.stringify([[], [], "", ""]) } as unknown as ComposePayload;
      workspace.openRestoredCompose(payload, r.attachments);
      // The pane is open: only now may the backend forget the entry (until then a reload leaves it in the Outbox).
      await api.outboxConfirmEdit(id);
      await this.refresh();
    } catch (e) { app.notify(errMsg(e), "error"); void this.refresh(); }
  }

  async retry(id: string) {
    const it = this.items.find((i) => i.id === id);
    if (it?.state === "unknown") {
      const ok = await app.ask({ title: "Send it again?", body: "This message might already have been delivered. Check your Sent folder first; sending again could deliver it twice.", ok: "Send again", danger: true });
      if (!ok) return;
    }
    try { await api.outboxRetry(id); } catch (e) { app.notify(errMsg(e), "error"); }
    await this.refresh();
  }

  async discard(id: string) {
    const it = this.items.find((i) => i.id === id);
    const ok = await app.ask({ title: "Discard this message?", body: `“${it?.subject || "No subject"}” has not been delivered and will be deleted.`, ok: "Discard", danger: true });
    if (!ok) return;
    try { await api.outboxDiscard(id); } catch (e) { app.notify(errMsg(e), "error"); }
    await this.refresh();
  }
}

export const outbox = new OutboxStore();
