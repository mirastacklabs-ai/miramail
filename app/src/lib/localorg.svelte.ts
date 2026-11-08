// Local organisation state: conversation keys of the open folder, which threads are expanded, muted / pinned rows,
// VIP and blocked senders. Everything is local to this Mac (docs/ESSENTIALS_SPEC.md). Talks only to localApi.
import { app } from "./state.svelte";
import { errMsg } from "./api";
import { localApi, type BlockedEntry, type VipEntry } from "./localapi";
import { senderAddress, toggled } from "./thread";
import type { MessageSummary } from "./types";

class LocalOrg {
  /** uid -> conversation key, for the folder in `scope` (only messages that share a conversation). */
  private keys = $state<Record<number, string>>({});
  private scope = "";
  expanded = $state<Set<string>>(new Set());
  muted = $state<Set<number>>(new Set());
  pinned = $state<Set<number>>(new Set());
  private marksScope = "";
  vip = $state<VipEntry[]>([]);
  blocked = $state<BlockedEntry[]>([]);
  sendersLoaded = $state(false);
  vipSet = $derived(new Set(this.vip.map((v) => v.address)));

  private here(): string { return app.account ? `${app.account}\u0000${app.folder}` : ""; }
  /** Conversation key of a message in the open folder, if it shares a conversation with another one. */
  keyOf = (uid: number): string | undefined => (this.scope === this.here() ? this.keys[uid] : undefined);
  isVip(from: { address?: string | null } | null | undefined): boolean { return this.vipSet.has(senderAddress(from)); }

  private threadTok = 0;
  async refreshThreads() {
    const scope = this.here();
    if (!scope || !app.account) return;
    const tok = ++this.threadTok;
    try {
      const list = await localApi.threadMap(app.account, app.folder);
      if (tok !== this.threadTok) return;
      this.keys = Object.fromEntries(list.map((e) => [e.uid, e.key]));
      this.scope = scope;
    } catch { /* conversations are a convenience: the flat list stays */ }
  }
  private marksTok = 0;
  async refreshMarks() {
    const scope = this.here();
    if (!scope || !app.account) return;
    const tok = ++this.marksTok;
    try {
      const m = await localApi.marks(app.account, app.folder);
      if (tok !== this.marksTok) return;
      this.muted = new Set(m.muted); this.pinned = new Set(m.pinned); this.marksScope = scope;
    } catch { /* cosmetic */ }
  }
  isMuted(uid: number): boolean { return this.marksScope === this.here() && this.muted.has(uid); }
  isPinned(uid: number): boolean { return this.marksScope === this.here() && this.pinned.has(uid); }
  /** Pinned set for the open folder only (the list ignores pins from another folder while the new one loads). */
  get pinnedHere(): Set<number> { return this.marksScope === this.here() ? this.pinned : new Set(); }

  toggleExpanded(key: string, on?: boolean) { this.expanded = toggled(this.expanded, key, on); }

  async loadSenders() {
    try { const s = await localApi.senders(); this.vip = s.vip; this.blocked = s.blocked; this.sendersLoaded = true; } catch (e) { app.notify(errMsg(e), "error"); }
  }

  async setMuted(uids: number[], muted: boolean) {
    const account = app.account; if (!account || !uids.length) return;
    try {
      // One call per conversation is enough, but the backend maps any member to its whole conversation.
      const seen = new Set<string>();
      for (const uid of uids.slice(0, 30)) {
        const k = this.keyOf(uid) ?? `u${uid}`;
        if (seen.has(k)) continue;
        seen.add(k);
        await localApi.muteThread(account, app.folder, uid, muted);
      }
      await this.refreshMarks();
      app.notify(muted ? "Conversation muted: no notifications and no unread count." : "Conversation unmuted.");
    } catch (e) { app.fail(e); }
  }
  async setPinned(uids: number[], pinned: boolean) {
    const account = app.account; if (!account || !uids.length) return;
    try { await localApi.pin(account, app.folder, uids, pinned); await this.refreshMarks(); } catch (e) { app.fail(e); }
  }
  async setVip(m: Pick<MessageSummary, "from">, on: boolean) {
    const addr = senderAddress(m.from);
    if (!addr) return;
    try {
      await localApi.setVip(addr, m.from?.name ?? null, on); await this.loadSenders();
      app.notify(on ? `${addr} is a VIP: their mail always notifies, even on “VIP only” accounts.` : `${addr} is no longer a VIP.`);
    } catch (e) { app.fail(e); }
  }
  async addVip(address: string) {
    try { await localApi.setVip(address, null, true); await this.loadSenders(); } catch (e) { app.fail(e); }
  }
  async setBlocked(address: string, on: boolean) {
    try { await localApi.setBlocked(address, on); await this.loadSenders(); } catch (e) { app.fail(e); }
  }
  /** Ask first, then block: the consequences are spelled out. */
  async blockSender(m: Pick<MessageSummary, "from">) {
    const addr = senderAddress(m.from);
    if (!addr) return;
    const ok = await app.ask({
      title: `Block ${addr}?`,
      body: "New mail from this address that arrives in an Inbox while MiraMail is open is moved to Junk. Nothing is deleted, and mail that arrives while MiraMail is closed stays where the server put it. You can unblock them in Preferences > Senders.",
      detail: addr, ok: "Block sender", danger: true,
    });
    if (ok) { await this.setBlocked(addr, true); app.notify(`Blocked ${addr}.`); }
  }
}

export const local = new LocalOrg();
