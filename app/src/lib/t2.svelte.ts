// Snooze UI state (main window): the "Snooze…" dialog, the list of snoozed messages, and the one-time question about
// creating a Snoozed folder. The backend owns the schedule (docs/APP_API.md "Snooze"); this mirrors it.
import { errMsg } from "./api";
import { app } from "./state.svelte";
import { orgApi } from "./orgapi";
import { onSnoozeChanged, t2, type Snooze } from "./t2api";
import { whenLabel } from "./snoozetimes";

const askedKey = (account: string) => `miramail.snooze.folder.${account}`;
function remembered(account: string): "yes" | "no" | null {
  try { const v = localStorage.getItem(askedKey(account)); return v === "yes" || v === "no" ? v : null; } catch { return null; }
}
function remember(account: string, v: "yes" | "no") { try { localStorage.setItem(askedKey(account), v); } catch { /* asked again next time */ } }

class SnoozeUi {
  items = $state<Snooze[]>([]);
  /** The message the Snooze dialog is open for. */
  target = $state<{ account: string; folder: string; uid: number } | null>(null);
  listOpen = $state(false);
  busy = $state(false);
  private started = false;

  async init() {
    if (this.started) return;
    this.started = true;
    app.snoozeCountFor = (email) => this.items.filter((s) => s.account === email).length;
    await onSnoozeChanged(() => { void this.refresh(); if (app.account) void app.loadMessages(); });
    await this.refresh();
  }
  async refresh() {
    try { this.items = await t2.snoozeList(); } catch { /* backend unavailable: keep what we have */ }
    if (!this.items.length) this.listOpen = false;
  }

  /** Open the dialog for a message of the open folder. */
  open(uid: number) {
    if (!app.account) return;
    this.target = { account: app.account, folder: app.folder, uid };
  }

  /** Decide whether to use the account's Snoozed folder; asks at most once per account before creating one. */
  private async chooseFolder(account: string): Promise<boolean> {
    const info = await t2.snoozeInfo(account);
    if (info.folder) return true;
    const was = remembered(account);
    if (was === "no") return false;
    const create = await app.ask({
      title: "Create a Snoozed folder?",
      body: "MiraMail can keep snoozed messages in a folder called “Snoozed” on your mail server and move them back to your Inbox when they are due. Otherwise they stay where they are and are only hidden in MiraMail until then. You are asked this once per account.",
      ok: "Create folder",
    });
    remember(account, create ? "yes" : "no");
    if (!create) return false;
    try { await orgApi.folderCreate(account, null, "Snoozed"); return true; }
    catch (e) { app.notify(`Couldn't create the folder (${errMsg(e)}). The message is hidden here instead.`, "error"); return false; }
  }

  async snooze(when: Date) {
    const tg = this.target;
    if (!tg || this.busy) return;
    this.busy = true;
    try {
      const useFolder = await this.chooseFolder(tg.account);
      const s = await t2.snoozeAdd(tg.account, tg.folder, tg.uid, when.getTime(), useFolder);
      this.target = null;
      if (app.account === tg.account && app.folder === tg.folder) {
        if (app.selectedUid === tg.uid) { app.selectedUid = null; app.body = null; }
        await app.loadMessages();
      }
      void app.refreshUnread(tg.account);
      await this.refresh();
      app.notify(`Snoozed until ${whenLabel(s.wakeAt, new Date())}. MiraMail must be open for it to come back.`);
    } catch (e) { app.fail(e); }
    finally { this.busy = false; }
  }

  /** Remove the entry only; the message itself is not touched. */
  async forget(id: number) {
    try { await t2.snoozeForget(id); app.notify("Snooze forgotten. The message was not touched."); } catch (e) { app.fail(e); }
    await this.refresh();
  }

  async unsnooze(id: number) {
    try {
      await t2.snoozeCancel(id);
      app.notify("Back in your Inbox.");
      if (app.account) { void app.loadMessages(); void app.refreshUnread(); }
    } catch (e) { app.fail(e); }
    await this.refresh();
  }
}
export const snoozeUi = new SnoozeUi();
