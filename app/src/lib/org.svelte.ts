// Organising state + actions: whole-mailbox search, bulk operations with undo, folder management, colour labels,
// unsubscribe, saved searches, per-sender image allow-list. Talks to the backend only through orgApi.
import { app } from "./state.svelte";
import { prefs } from "./prefs.svelte";
import { errMsg } from "./api";
import { orgApi } from "./orgapi";
import { chunk, clickSel, describe, emptySel, nextAfterRemoval, prune, runChunks, selectAll, undoItems, type ChunkResult, type Progress, type Sel } from "./bulk";
import { needsBackend, NO_CHIPS } from "./query";
import { addAllowed, isAllowed, loadAllowed, removeAllowed, storeAllowed } from "./imgallow";
import { addSaved, defaultName, loadSaved, removeSaved, storeSaved, type Saved } from "./savedsearch";
import type { Folder, FolderRole, MessageSummary } from "./types";
import { canDropFolder, isMovable } from "./foldermove";
import type { FolderOp, SearchScope, SourceView, UnsubInfo } from "./orgtypes";

export interface UndoToast { id: number; msg: string; run: () => Promise<void> }
export interface PromptReq { title: string; label: string; value: string; ok: string; hint?: string; resolve: (v: string | null) => void }
export interface MovePickerReq { folder: string; resolve: (parent: string | null | undefined) => void }
export interface SourceReq { title: string; tab: "headers" | "raw"; view: SourceView | null; error: string | null }

const PAGE = 50;
const fmt = (n: number) => n.toLocaleString("en-US");
const roleOf = (name: string): FolderRole | undefined => app.folders.find((f) => f.name === name)?.role;
const leafOf = (f: Folder) => f.name.split(f.delimiter || "/").pop() ?? f.name;

class Org {
  // ---- search ----
  scope = $state<SearchScope>("folder");
  results = $state<MessageSummary[] | null>(null);
  total = $state(0);
  hints = $state<string[]>([]);
  searching = $state(false);
  searchError = $state<string | null>(null);
  serverBusy = $state(false);
  serverNote = $state<string | null>(null);
  /** Where the user came from when they opened a search result (shown as "Back to results"). */
  parked = $state<{ query: string; scope: SearchScope } | null>(null);
  private tok = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;

  /** The list shows backend search results instead of the loaded folder. */
  get backend(): boolean { return !!app.query.trim() && (this.scope !== "folder" || needsBackend(app.query)); }
  /** Query sent to the backend: the text plus the quick-filter chips as operators. */
  get backendQuery(): string {
    const c = app.chips;
    return [app.query.trim(), c.unread ? "is:unread" : "", c.flagged ? "is:flagged" : "", c.today ? "after:today" : "", c.week ? "newer_than:7d" : ""].filter(Boolean).join(" ");
  }
  schedule() {
    clearTimeout(this.timer);
    if (!this.backend || !app.account) { this.tok++; this.results = null; this.total = 0; this.hints = []; this.searching = false; this.searchError = null; this.serverNote = null; return; }
    this.searching = true;
    this.timer = setTimeout(() => void this.search(0), 220);
  }
  async search(offset: number) {
    if (!app.account) return;
    const tok = ++this.tok;
    this.searching = true; this.searchError = null;
    try {
      const r = await orgApi.searchMail(this.backendQuery, this.scope, app.account, app.folder, PAGE, offset);
      if (tok !== this.tok) return;
      this.results = offset ? [...(this.results ?? []), ...r.hits] : r.hits;
      this.total = r.total; this.hints = r.hints;
    } catch (e) { if (tok === this.tok) { this.searchError = errMsg(e); this.results = offset ? this.results : []; } }
    if (tok === this.tok) this.searching = false;
  }
  loadMoreResults() { return this.search(this.results?.length ?? 0); }
  get moreResults(): boolean { return !!this.results && this.results.length < this.total; }
  /** Ask the mail server too (read-only SEARCH), then re-run the local search over what it cached. */
  async searchServer() {
    if (!app.account || this.serverBusy) return;
    this.serverBusy = true; this.serverNote = null;
    try {
      const r = await orgApi.searchServer(this.backendQuery, this.scope, app.account, app.folder);
      this.serverNote = `The server searched ${r.searched} folder${r.searched === 1 ? "" : "s"} and listed ${fmt(r.found)} message${r.found === 1 ? "" : "s"}${r.skipped ? `; ${r.skipped} skipped (not synced yet or unsupported)` : ""}.${r.notes.length ? " " + r.notes[0] : ""}`;
      await this.search(0);
    } catch (e) { this.serverNote = errMsg(e); }
    this.serverBusy = false;
  }
  async openResult(m: MessageSummary) {
    this.parked = { query: app.query, scope: this.scope };
    if (app.account !== m.account) await app.selectAccount(m.account);
    if (app.folder !== m.folder) await app.selectFolder(m.folder);
    app.clearFilters(); this.scope = "folder";
    if (!app.messages.some((x) => x.uid === m.uid)) app.messages = [...app.messages, m].sort((a, b) => (b.date ?? 0) - (a.date ?? 0));
    await app.open(m.uid);
  }
  backToResults() {
    const p = this.parked; if (!p) return;
    this.parked = null; this.scope = p.scope; app.query = p.query;
  }

  // ---- saved searches ----
  saved = $state<Saved[]>(loadSaved());
  async saveCurrent() {
    const q = app.query.trim();
    if (!q) return;
    const name = await this.askText({ title: "Save search", label: "Name", value: defaultName(q), ok: "Save", hint: `Saves “${q}” and where it searches (${this.scope === "folder" ? "this folder" : this.scope === "account" ? "this account" : "all accounts"}).` });
    if (!name?.trim()) return;
    this.saved = addSaved(this.saved, name, q, this.scope); storeSaved(this.saved);
  }
  applySaved(s: Saved) { this.parked = null; this.scope = s.scope; app.chips = { ...NO_CHIPS }; app.query = s.query; app.view = "mail"; }
  deleteSaved(id: string) { this.saved = removeSaved(this.saved, id); storeSaved(this.saved); }

  // ---- selection ----
  sel = $state<Sel>(emptySel());
  get count(): number { return this.sel.set.size; }
  isSel(uid: number): boolean { return this.sel.set.has(uid); }
  private rows(): number[] { return app.filtered.map((m) => m.uid); }
  click(uid: number, mods: { shift: boolean; meta: boolean }) { this.sel = clickSel(this.sel, this.rows(), uid, mods); }
  toggleCurrent() { if (app.selectedUid != null) this.sel = clickSel(this.sel, this.rows(), app.selectedUid, { shift: false, meta: true }); }
  selectAllRows() { this.sel = selectAll(this.rows()); }
  clearSel() { if (this.sel.set.size) this.sel = emptySel(); }
  pruneSel() { const p = prune(this.sel, this.rows()); if (p !== this.sel) this.sel = p; }
  /** The uids an action on `uid` applies to: the whole selection when `uid` is part of it, else just `uid`. */
  targets(uid: number): number[] { return this.sel.set.has(uid) ? [...this.sel.set] : [uid]; }

  // ---- bulk operations ----
  progress = $state<(Progress & { label: string }) | null>(null);
  private stop = false;
  cancelProgress() { this.stop = true; }
  undo = $state<UndoToast | null>(null);
  private undoId = 0;
  private undoTimer: ReturnType<typeof setTimeout> | undefined;
  private offerUndo(msg: string, run: () => Promise<void>) {
    const id = ++this.undoId;
    this.undo = { id, msg, run };
    clearTimeout(this.undoTimer);
    this.undoTimer = setTimeout(() => { if (this.undo?.id === id) this.undo = null; }, 12000);
  }
  dismissUndo() { this.undo = null; clearTimeout(this.undoTimer); }
  async runUndo() {
    const u = this.undo; if (!u) return;
    this.dismissUndo();
    try { await u.run(); } catch (e) { app.fail(e); }
  }

  /** Remove rows from the open folder's list and pick the next row per Preferences. */
  private dropLocal(uids: number[]) {
    const gone = new Set(uids);
    const next = nextAfterRemoval(app.filtered.map((m) => m.uid), gone, prefs.v.afterRemove);
    const unseen = app.messages.filter((m) => gone.has(m.uid) && !m.flags.seen).length;
    if (unseen) app.adjustUnread(app.folder, -unseen);
    app.messages = app.messages.filter((m) => !gone.has(m.uid));
    if (app.serverTotal !== null) app.serverTotal = Math.max(0, app.serverTotal - uids.length);
    if (app.selectedUid != null && gone.has(app.selectedUid)) { app.body = null; app.selectedUid = next; }
    this.sel = prune(this.sel, app.messages.map((m) => m.uid));
  }

  private async withProgress<T>(label: string, total: number, fn: () => Promise<T>): Promise<T> {
    this.stop = false;
    this.progress = { label, done: 0, total, ok: 0, failed: 0 };
    try { return await fn(); } finally { this.progress = null; }
  }
  private onProgress = (p: Progress) => { if (this.progress) this.progress = { ...this.progress, ...p }; };

  /** Move messages of the open folder (archive, trash, junk, any folder). Undo-able. */
  async moveUids(uids: number[], dest: { role?: FolderRole; folder?: string }, from = app.folder, acct = app.account) {
    const account = acct;
    if (!account || !uids.length) return;
    if (dest.role === "trash" && roleOf(from) === "trash") return this.deleteForever(uids, account, from);
    const verb = dest.role === "archive" ? "Archived" : dest.role === "trash" ? "Moved to Trash:" : dest.role === "junk" ? "Marked as junk:" : dest.role === "inbox" ? "Moved to Inbox:" : "Moved";
    const s = await this.withProgress(dest.role === "archive" ? "Archiving" : "Moving", uids.length, () =>
      runChunks(uids, (c) => orgApi.bulkMove(account, from, c.map((uid) => ({ uid })), dest) as Promise<ChunkResult>, this.onProgress, () => this.stop));
    const moved = new Set(s.ok.map((o) => o.uid).filter((u): u is number => u != null));
    if (from === app.folder && account === app.account) this.dropLocal([...moved]);
    void app.refreshUnread(account);
    const text = describe(verb, s);
    const u = undoItems(s);
    if (s.failed.length || s.notAttempted) app.notify(text, "error");
    if (u.items.length && s.destFolder) {
      const destFolder = s.destFolder, destValidity = s.destValidity;
      const note = u.items.length < u.total ? ` (${fmt(u.total - u.items.length)} can't be undone)` : "";
      this.offerUndo(`${text}${note}`, async () => {
        const back = await this.withProgress("Moving back", u.items.length, () =>
          runChunks(u.items, (c) => orgApi.bulkMove(account, destFolder, c, { folder: from }, destValidity), this.onProgress, () => this.stop));
        app.notify(describe("Moved back", back), back.failed.length ? "error" : "info");
        if (account === app.account) { await app.loadMessages(); void app.sync(true); void app.refreshUnread(account); }
      });
    } else if (!s.failed.length && !s.notAttempted) app.notify(text);
  }

  /** Mark read/unread or flag/unflag. Optimistic; the messages the server did not confirm are rolled back. */
  async flagUids(uids: number[], flag: "seen" | "flagged", value: boolean, acct = app.account, fold = app.folder) {
    const account = acct, folder = fold;
    if (!account || !uids.length) return;
    const before = new Map<number, boolean>();
    for (const m of app.messages) if (uids.includes(m.uid) && m.flags[flag] !== value) { before.set(m.uid, m.flags[flag]); m.flags[flag] = value; }
    if (flag === "seen") app.adjustUnread(folder, (value ? -1 : 1) * before.size, account);
    const s = await this.withProgress(flag === "seen" ? "Updating" : "Flagging", uids.length, () =>
      runChunks(uids, (c) => orgApi.bulkFlag(account, folder, c, flag, value), this.onProgress, () => this.stop));
    const okSet = new Set(s.ok.map((o) => o.uid));
    let rolled = 0;
    for (const m of app.messages) if (before.has(m.uid) && !okSet.has(m.uid)) { m.flags[flag] = before.get(m.uid)!; rolled++; }
    if (flag === "seen" && rolled) app.adjustUnread(folder, (value ? 1 : -1) * rolled, account);
    const verb = flag === "seen" ? (value ? "Marked as read:" : "Marked as unread:") : value ? "Flagged" : "Unflagged";
    const text = describe(verb, s);
    if (s.failed.length || s.notAttempted) app.notify(text, "error"); else app.notify(text);
  }

  /** Permanent deletion (only reachable from Trash), after a confirm naming the count. */
  async deleteForever(uids: number[], acct = app.account, fold = app.folder) {
    const account = acct, folder = fold;
    if (!account || !uids.length) return;
    const n = uids.length;
    const ok = await app.ask({ title: `Delete ${fmt(n)} message${n === 1 ? "" : "s"} forever?`, body: "They will be permanently deleted from the server. This cannot be undone.", ok: "Delete forever", danger: true });
    if (!ok) return;
    const s = await this.withProgress("Deleting", n, () => runChunks(uids, (c) => orgApi.bulkDeleteForever(account, folder, c), this.onProgress, () => this.stop));
    const gone = s.ok.map((o) => o.uid).filter((u): u is number => u != null);
    if (folder === app.folder && account === app.account) this.dropLocal(gone);
    void app.refreshUnread(account);
    const text = describe("Deleted", s);
    if (s.failed.length || s.notAttempted) app.notify(text, "error"); else app.notify(text);
  }

  // ---- colour labels ----
  colours = $state<Record<number, number>>({});
  private colourKey = "";
  async loadColours() {
    if (!app.account) return;
    const key = `${app.account}\u0000${app.folder}`;
    this.colourKey = key;
    try {
      const list = await orgApi.listColours(app.account, app.folder);
      if (this.colourKey === key) this.colours = Object.fromEntries(list.map((c) => [c.uid, c.colour]));
    } catch { /* colours are cosmetic */ }
  }
  async setColour(uids: number[], colour: number) {
    if (!app.account) return;
    let failed = "";
    for (const uid of uids) {
      try { await orgApi.setColour(app.account, app.folder, uid, colour); this.colours = { ...this.colours, [uid]: colour }; } catch (e) { failed = errMsg(e); }
    }
    if (failed) app.notify(failed, "error");
  }

  // ---- text prompt + folder management ----
  prompt = $state<PromptReq | null>(null);
  askText(r: Omit<PromptReq, "resolve">): Promise<string | null> {
    return new Promise((resolve) => { this.prompt = { ...r, resolve }; });
  }
  subs = $state<Record<string, string[] | undefined>>({});
  async loadSubs(account = app.account) {
    if (!account) return;
    try { this.subs[account] = await orgApi.folderSubscriptions(account); } catch { /* unknown: the menu hides the item */ }
  }
  isSubscribed(folder: string): boolean | null {
    const l = app.account ? this.subs[app.account] : undefined;
    return l ? l.includes(folder) : null;
  }
  async toggleSubscribe(folder: string) {
    const account = app.account, cur = this.isSubscribed(folder);
    if (!account || cur === null) return;
    try {
      await orgApi.folderSubscribe(account, folder, !cur);
      this.subs[account] = cur ? (this.subs[account] ?? []).filter((f) => f !== folder) : [...(this.subs[account] ?? []), folder];
      app.notify(cur ? "Unsubscribed from the folder (it stays on the server)." : "Subscribed to the folder.");
    } catch (e) { app.fail(e); }
  }
  private setFolders(list: Folder[]) { app.folders = list; this.dragFolder = null; this.folderDrop = null; }
  async createFolder(parent: string | null) {
    const account = app.account; if (!account) return;
    const name = await this.askText({ title: parent ? "New subfolder" : "New folder", label: "Name", value: "", ok: "Create", hint: "Letters, digits, spaces, - and _ only." });
    if (!name?.trim()) return;
    try { this.setFolders(await orgApi.folderCreate(account, parent, name)); app.notify("Folder created."); void this.loadSubs(account); } catch (e) { app.fail(e); }
  }
  async renameFolder(f: Folder) {
    const account = app.account; if (!account) return;
    const name = await this.askText({ title: "Rename folder", label: "New name", value: leafOf(f), ok: "Rename", hint: "Letters, digits, spaces, - and _ only. Messages in this folder and its subfolders are loaded again from the server, and MIRA's access to them is removed." });
    if (!name?.trim() || name.trim() === leafOf(f)) return;
    try {
      const touched = this.touchesOpen(f);
      const r = await orgApi.folderRename(account, f.name, name);
      this.afterFolderOp(account, r, touched, "Folder renamed.");
    } catch (e) { app.fail(e); }
  }
  /** Is the open folder or the open message inside `f` (or below it)? */
  private touchesOpen(f: Folder): boolean {
    const d = f.delimiter || "/";
    const inside = (n: string | undefined) => !!n && (n === f.name || n.startsWith(f.name + d));
    return inside(app.folder) || inside(app.body?.summary.folder);
  }
  private afterFolderOp(account: string, r: FolderOp, touched: boolean, done: string) {
    if (app.account !== account) { void app.refreshUnread(account); return; }
    this.setFolders(r.folders);
    app.notify(done);
    for (const n of r.notes) app.notify(n);
    if (touched) {
      // The old (folder, uid) names are gone: close the message and go to the Inbox.
      app.body = null; app.selectedUid = null;
      void app.selectFolder(app.inboxName);
    }
    void app.refreshUnread(account);
    void this.loadSubs(account);
  }
  // ---- moving folders ----
  /** Folder being dragged in the sidebar (messages use `app.dragMsg`; the two never overlap). */
  dragFolder = $state<string | null>(null);
  /** Drop highlight while dragging a folder: a folder name, or "" for the top-level zone. */
  folderDrop = $state<string | null>(null);
  /** In-app "Move to…" picker. Resolves to the chosen parent (null = top level) or undefined when cancelled. */
  movePicker = $state<MovePickerReq | null>(null);
  pickMoveTarget(folder: string): Promise<string | null | undefined> {
    return new Promise((resolve) => { this.movePicker = { folder, resolve }; });
  }
  /** Menu entry: pick a destination in the in-app picker, then move (no extra confirm). */
  async moveFolderPicked(f: Folder) {
    const account = app.account; if (!account || !isMovable(app.folders, f)) return;
    const parent = await this.pickMoveTarget(f.name);
    if (parent !== undefined) await this.moveFolder(account, f, parent);
  }
  /** Drag and drop: confirm in-app, then move. */
  async moveFolderDropped(f: Folder, parent: string | null) {
    const account = app.account; if (!account || !canDropFolder(app.folders, f, parent)) return;
    const to = parent === null ? "the top level" : `“${parent}”`;
    const ok = await app.ask({ title: `Move “${leafOf(f)}” into ${to}?`, body: "The folder and its subfolders move on the server. Their messages are loaded again from there, and MIRA's access to them is removed.", ok: "Move" });
    if (ok) await this.moveFolder(account, f, parent);
  }
  async moveFolder(account: string, f: Folder, parent: string | null) {
    try {
      const touched = this.touchesOpen(f);
      const r = await orgApi.folderMove(account, f.name, parent);
      this.afterFolderOp(account, r, touched, "Folder moved.");
    } catch (e) { app.fail(e); }
  }
  async deleteFolder(f: Folder) {
    const account = app.account; if (!account) return;
    let count: number;
    try { count = await orgApi.folderMessageCount(account, f.name); } catch (e) { app.fail(e); return; }
    const ok = await app.ask({ title: `Delete folder “${leafOf(f)}”?`, body: count ? `The folder and the ${fmt(count)} message${count === 1 ? "" : "s"} in it will be deleted from the server.` : "The empty folder will be deleted from the server.", ok: count ? "Continue" : "Delete folder", danger: true });
    if (!ok) return;
    if (count > 0) {
      const sure = await app.ask({ title: `Permanently delete ${fmt(count)} message${count === 1 ? "" : "s"}?`, body: `“${f.name}” is not empty. Deleting it permanently deletes all ${fmt(count)} message${count === 1 ? "" : "s"} in it. This cannot be undone.`, ok: `Delete ${fmt(count)} message${count === 1 ? "" : "s"} and the folder`, danger: true });
      if (!sure) return;
    }
    try {
      const wasOpen = app.folder === f.name;
      this.setFolders(await orgApi.folderDelete(account, f.name, count > 0 ? count : null));
      app.notify("Folder deleted.");
      if (wasOpen) await app.selectFolder(app.inboxName);
      void app.refreshUnread(account);
    } catch (e) { app.fail(e); }
  }
  async emptyFolder(f: Folder) {
    const account = app.account; if (!account) return;
    let count: number;
    try { count = await orgApi.folderMessageCount(account, f.name); } catch (e) { app.fail(e); return; }
    if (count === 0) { app.notify(`${f.role === "trash" ? "Trash" : "Junk"} is already empty.`); return; }
    const label = f.role === "trash" ? "Trash" : "Junk";
    const ok = await app.ask({ title: `Empty ${label}?`, body: `${fmt(count)} message${count === 1 ? "" : "s"} will be permanently deleted from the server. This cannot be undone.`, ok: `Delete ${fmt(count)} forever`, danger: true });
    if (!ok) return;
    try {
      const n = await orgApi.folderEmpty(account, f.name);
      app.notify(`${label} emptied (${fmt(n)} message${n === 1 ? "" : "s"}).`);
      if (app.folder === f.name) { app.messages = []; app.body = null; app.selectedUid = null; app.serverTotal = 0; }
      void app.refreshUnread(account);
    } catch (e) { app.fail(e); }
  }

  // ---- reader: source, unsubscribe ----
  source = $state<SourceReq | null>(null);
  async viewSource(tab: "headers" | "raw") {
    const m = app.body?.summary; if (!m) return;
    this.source = { title: m.subject || "(no subject)", tab, view: null, error: null };
    try {
      const v = await orgApi.getSource(m.account, m.folder, m.uid);
      if (this.source) this.source = { ...this.source, view: v };
    } catch (e) { if (this.source) this.source = { ...this.source, error: errMsg(e) }; }
  }
  async saveEml() {
    const m = app.body?.summary; if (!m) return;
    try { app.notify(`Saved to ${await orgApi.saveEml(m.account, m.folder, m.uid)}`); } catch (e) { app.fail(e); }
  }

  /** Bumped by "Print"; the Reader's print sheet reacts. */
  printing = $state(0);
  /** Options for the open message, stamped with the message they were loaded for (checked before acting). */
  unsub = $state<(UnsubInfo & { account: string; folder: string; uid: number }) | null>(null);
  private unsubTok = 0;
  async loadUnsub(m: MessageSummary | undefined) {
    const tok = ++this.unsubTok;
    this.unsub = null;
    if (!m) return;
    try { const u = await orgApi.unsubscribeInfo(m.account, m.folder, m.uid); if (tok === this.unsubTok) this.unsub = u ? { ...u, account: m.account, folder: m.folder, uid: m.uid } : null; } catch { /* no unsubscribe offered */ }
  }
  unsubBusy = $state(false);
  /** `kind`: "oneClick" contacts the sender's server (https, after a confirm naming the host); "mailto" sends mail through SMTP. */
  async unsubscribe(kind: "oneClick" | "mailto") {
    const m = app.body?.summary, u = this.unsub;
    if (!m || !u || this.unsubBusy) return;
    if (u.account !== m.account || u.folder !== m.folder || u.uid !== m.uid) return; // options belong to another message
    const ok = kind === "oneClick"
      ? await app.ask({ title: `Unsubscribe via ${u.httpsHost}?`, body: `MiraMail will contact ${u.httpsHost}, the sender's server, with a single request asking to be removed from this list. That server will see your IP address. Nothing else is sent.`, detail: u.httpsHost ?? "", ok: "Contact and unsubscribe" })
      : await app.ask({ title: "Send an unsubscribe email?", body: `MiraMail will send a short email from ${m.account} to ${u.mailto} asking to be removed from this list.`, detail: u.mailto ?? "", ok: "Send" });
    if (!ok) return;
    // The reader may have moved on while the confirm was open: never act for a different message.
    if (app.body?.summary.uid !== m.uid || app.body.summary.folder !== m.folder || app.body.summary.account !== m.account || this.unsub !== u) return;
    this.unsubBusy = true;
    try {
      if (kind === "oneClick") await orgApi.unsubscribeOneClick(m.account, m.folder, m.uid); else await orgApi.unsubscribeMailto(m.account, m.folder, m.uid);
      app.notify(kind === "oneClick" ? `Asked ${u.httpsHost} to unsubscribe you.` : "Unsubscribe email sent.");
    } catch (e) { app.fail(e); }
    this.unsubBusy = false;
  }

  // ---- per-sender "always load images" ----
  allowed = $state<string[]>(loadAllowed());
  senderAllowed(addr: string | null | undefined): boolean { return isAllowed(this.allowed, addr); }
  /** On the allow-list AND this message is DKIM-verified for its From domain. No auth data = stays blocked. */
  async autoLoadOk(addr: string | null | undefined, account: string, folder: string, uid: number): Promise<boolean> {
    if (!isAllowed(this.allowed, addr)) return false;
    try { return await orgApi.senderImagesVerified(account, folder, uid); } catch { return false; }
  }
  allowSender(addr: string | null | undefined) { this.allowed = addAllowed(this.allowed, addr); storeAllowed(this.allowed); }
  disallowSender(addr: string | null | undefined) { this.allowed = removeAllowed(this.allowed, addr); storeAllowed(this.allowed); }
}

export const org = new Org();
export { chunk };

// Single-message archive / trash / drag-and-drop go through the same set-safe batch path, so they get the undo toast too.
app.mover = (uids, dest, from) => org.moveUids(uids, dest, from);
app.senderAllowed = (addr, account, folder, uid) => org.autoLoadOk(addr, account, folder, uid);
