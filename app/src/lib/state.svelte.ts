import { api, errMsg, onLive, onNewMail, onStatus, onSynced } from "./api";
import { applyFilter, chipTerms, NO_CHIPS, parseQuery, type Chips, type Parsed } from "./query";
import { MARK_READ_DELAY_MS } from "./prefs-model";
import { prefs } from "./prefs.svelte";
import { BULK_STEP, CACHE_PAGE, coversDate, LOAD_ALL_CONFIRM_ABOVE, fmtN, mergeRows, OLDER_STEP, remainingOnServer } from "./older";
import type { AccountInfo, ConnState, Folder, FolderRole, LiveStatus, MessageBody, MessageSummary, Outgoing } from "./types";

export type Theme = "system" | "dark" | "light" | "contrast";
const ROLE_ORDER: FolderRole[] = ["inbox", "sent", "drafts", "archive", "junk", "trash", "other"];

function lsGet(k: string): string | null { try { return localStorage.getItem(k); } catch { return null; } }
function lsSet(k: string, v: string) { try { localStorage.setItem(k, v); } catch { /* ignore */ } }

export interface ComposeInit { to: string[]; cc: string[]; bcc: string[]; subject: string; text: string; inReplyTo: Outgoing["inReplyTo"] }
export interface ConfirmReq { title: string; body: string; ok: string; danger?: boolean; detail?: string; resolve: (v: boolean) => void }

export const liveKey = (account: string, folder: string) => `${account}\u0000${folder}`;

class AppState {
  ready = $state(false);
  accounts = $state<AccountInfo[]>([]);
  account = $state<string | null>(null);
  folders = $state<Folder[]>([]);
  folder = $state("INBOX");
  messages = $state<MessageSummary[]>([]);
  listLoading = $state(false);
  syncing = $state(false);
  selectedUid = $state<number | null>(null);
  body = $state<MessageBody | null>(null);
  bodyLoading = $state(false);
  imagesAllowed = $state(false);
  /** When images are allowed: whether tracker-verdict images were left out. */
  skipTrackers = $state(true);
  query = $state("");
  status = $state<Record<string, ConnState>>({});
  /** account -> folder name -> unseen count (IMAP STATUS, adjusted optimistically). */
  unreadByFolder = $state<Record<string, Record<string, number>>>({});
  toast = $state<{ msg: string; kind: "error" | "info"; id: number } | null>(null);
  /** Set by the workspace: drop panes that belong to a removed account. */
  onAccountRemoved: ((email: string) => void) | null = null;
  /** Set by the snooze UI: how many snoozed messages an account has (for the remove-account warning). */
  snoozeCountFor: ((email: string) => number) | null = null;
  confirmReq = $state<ConfirmReq | null>(null);
  theme = $state<Theme>("system");
  addingAccount = $state(false);
  showAbout = $state(false);
  view = $state<"mail" | "calendar">("mail");
  showSettings = $state(false);
  /** account -> persisted live folders (INBOX is implicit). */
  liveFolders = $state<Record<string, string[]>>({});
  /** account -> folder -> watcher state of this session. */
  liveStatus = $state<Record<string, Record<string, LiveStatus>>>({});
  /** "account\u0000folder" -> persistent inline error under the folder row. */
  liveError = $state<Record<string, string>>({});
  liveBusy = $state<Record<string, boolean>>({});
  /** Folders whose `mail://new` already reloaded the list; the following `mail://synced` skips the duplicate reload. */
  private newHandled = new Set<string>();

  // ---- older mail (docs/APP_API.md `load_older`) ----
  /** Messages in the open folder on the server (null until the first sync/load reports it). */
  serverTotal = $state<number | null>(null);
  /** The oldest message of the folder is cached; nothing older exists on the server. */
  reachedStart = $state(false);
  /** Cached rows exist beyond the ones listed (pulled from SQLite before asking the server). */
  cacheMore = $state(false);
  olderBusy = $state(false);
  olderError = $state<string | null>(null);
  /** "Load all" / "Jump to date" in progress. */
  bulk = $state<{ kind: "all" | "date" } | null>(null);
  /** Set when a jump finished: the list scrolls to the first row at or before `ts`. */
  jumpTo = $state<{ ts: number; id: number } | null>(null);
  private listToken = 0;
  private bulkId = 0;
  private jumpSeq = 0;
  get remaining(): number { return remainingOnServer(this.messages.length, this.serverTotal, this.reachedStart); }
  get moreAvailable(): boolean { return this.cacheMore || this.remaining > 0; }
  private resetOlder() {
    this.listToken++; this.bulkId++;
    this.serverTotal = null; this.reachedStart = false; this.cacheMore = false;
    this.olderBusy = false; this.olderError = null; this.bulk = null;
  }
  private toastId = 0;
  private bodyToken = 0;

  /** Quick-filter chips; combined (AND) with `query`. Remembered per folder in memory only. */
  chips = $state<Chips>({ ...NO_CHIPS });
  private filterMem = new Map<string, { query: string; chips: Chips }>();
  private parsedMemo: { key: string; val: Parsed } | null = null;
  /** Parsed `query` (memoised per query string and minute, so keystrokes parse once). */
  get parsed(): Parsed {
    const key = `${this.query}\u0000${Math.floor(Date.now() / 60000)}`;
    if (this.parsedMemo?.key !== key) this.parsedMemo = { key, val: parseQuery(this.query) };
    return this.parsedMemo.val;
  }
  get filterActive(): boolean { return !!this.query.trim() || Object.values(this.chips).some(Boolean); }
  get filtered(): MessageSummary[] {
    if (!this.filterActive) return this.messages;
    return applyFilter(this.messages, [...chipTerms(this.chips, Date.now() / 1000), ...this.parsed.terms]);
  }
  clearFilters() { this.query = ""; this.chips = { ...NO_CHIPS }; }
  private rememberFilter() {
    if (this.account) this.filterMem.set(liveKey(this.account, this.folder), { query: this.query, chips: { ...this.chips } });
  }
  get sortedFolders(): Folder[] {
    return [...this.folders].filter((f) => f.selectable).sort((a, b) => {
      const d = ROLE_ORDER.indexOf(a.role) - ROLE_ORDER.indexOf(b.role);
      return d || a.name.localeCompare(b.name);
    });
  }
  get current(): AccountInfo | undefined { return this.accounts.find((a) => a.email === this.account); }
  get selected(): MessageSummary | undefined { return this.messages.find((m) => m.uid === this.selectedUid); }

  fail(e: unknown) { this.notify(errMsg(e), "error"); }
  notify(msg: string, kind: "error" | "info" = "info") {
    const id = ++this.toastId;
    this.toast = { msg, kind, id };
    setTimeout(() => { if (this.toast?.id === id) this.toast = null; }, kind === "error" ? 7000 : 3500);
  }
  ask(r: Omit<ConfirmReq, "resolve">): Promise<boolean> {
    return new Promise((resolve) => { this.confirmReq = { ...r, resolve }; });
  }

  setTheme(t: Theme) {
    this.theme = t; lsSet("miramail.theme", t);
    const el = document.documentElement;
    if (t === "system") el.removeAttribute("data-theme"); else el.setAttribute("data-theme", t);
  }
  cycleTheme() { this.setTheme(this.theme === "system" ? "light" : this.theme === "light" ? "dark" : "system"); }

  async init() {
    const t = lsGet("miramail.theme");
    this.setTheme(t === "dark" || t === "light" || t === "contrast" ? t : "system");
    try {
      this.accounts = await api.listAccounts();
      if (this.accounts.length) await this.selectAccount(this.accounts[0].email);
    } catch (e) { this.fail(e); }
    this.ready = true;
    void onNewMail((e) => { void this.onNew(e.account, e.folder); });
    void onStatus((e) => { this.status[e.account] = e.state; });
    void onSynced((e) => { void this.onSynced(e.account, e.folders); });
    void onLive((e) => {
      this.liveStatus[e.account] = { ...(this.liveStatus[e.account] ?? {}), [e.folder]: { state: e.state, message: e.message } };
      if (e.state === "error" && e.message) this.liveError[liveKey(e.account, e.folder)] = e.message;
      else if (e.state === "live" || e.state === "polling") delete this.liveError[liveKey(e.account, e.folder)];
    });
  }

  /** Detached pane windows: theme + account list only. No folder selection, no sync, no mail event listeners (the main window owns those). */
  async initLite() {
    const t = lsGet("miramail.theme");
    this.setTheme(t === "dark" || t === "light" || t === "contrast" ? t : "system");
    try {
      this.accounts = await api.listAccounts();
      if (this.accounts.length) this.account = this.accounts[0].email;
    } catch (e) { this.fail(e); }
    this.ready = true;
  }

  async accountAdded(info: AccountInfo) {
    this.accounts = [...this.accounts.filter((a) => a.email !== info.email), info];
    this.addingAccount = false;
    await this.selectAccount(info.email);
  }

  async selectAccount(email: string) {
    this.account = email; this.folders = []; this.messages = []; this.body = null; this.selectedUid = null; this.resetOlder();
    this.filterMem.clear(); this.clearFilters();
    this.folder = "INBOX";
    try { this.folders = await api.listFolders(email); } catch (e) { this.fail(e); }
    void this.refreshUnread(email);
    void this.loadLive(email);
    const inbox = this.folders.find((f) => f.role === "inbox");
    this.folder = inbox?.name ?? "INBOX";
    await this.loadMessages();
    void this.sync(true);
  }

  async selectFolder(name: string) {
    this.rememberFilter();
    this.folder = name; this.messages = []; this.body = null; this.selectedUid = null; this.resetOlder();
    const mem = this.account ? this.filterMem.get(liveKey(this.account, name)) : undefined;
    this.query = mem?.query ?? ""; this.chips = { ...(mem?.chips ?? NO_CHIPS) };
    await this.loadMessages();
    void this.sync(true);
  }

  async loadMessages() {
    if (!this.account) return;
    const [acc, fol] = [this.account, this.folder];
    this.listLoading = this.messages.length === 0;
    try {
      // Keep every row already listed (older mail the user loaded) plus a little head-room for new mail.
      const want = Math.max(CACHE_PAGE, this.messages.length + 64);
      const rows = await api.listMessages(acc, fol, want, 0);
      if (acc === this.account && fol === this.folder) {
        const prev = new Set(this.messages.map((m) => m.uid));
        const fresh = prev.size ? rows.filter((m) => !prev.has(m.uid) && m.date != null && m.date >= (this.messages[0]?.date ?? 0)).length : 0;
        if (fresh && this.serverTotal !== null) this.serverTotal += fresh; // new mail raises the total too
        this.messages = rows;
        this.cacheMore = rows.length >= want;
        this.syncUnreadFromList(acc, fol);
      }
    } catch (e) { this.fail(e); }
    this.listLoading = false;
  }
  /** The list is the whole folder: its unread count is exact (otherwise IMAP STATUS decides). */
  private syncUnreadFromList(acc: string, fol: string) {
    if (!this.cacheMore && this.serverTotal !== null && this.messages.length >= this.serverTotal) {
      this.setUnread(acc, fol, this.messages.filter((m) => !m.flags.seen).length);
    }
  }

  /** Next page: cached rows first (local), then older headers from the server. Returns rows added. */
  async loadMore(): Promise<number> {
    return this.cacheMore ? this.showMoreCached() : this.loadOlder(OLDER_STEP);
  }

  private async showMoreCached(): Promise<number> {
    if (!this.account || this.olderBusy) return 0;
    const [acc, fol, tok] = [this.account, this.folder, this.listToken];
    this.olderBusy = true; this.olderError = null;
    try {
      const rows = await api.listMessages(acc, fol, CACHE_PAGE, this.messages.length);
      if (tok !== this.listToken) return 0;
      const before = this.messages.length;
      this.messages = mergeRows(this.messages, rows);
      this.cacheMore = rows.length >= CACHE_PAGE;
      return this.messages.length - before;
    } catch (e) {
      if (tok === this.listToken) this.olderError = errMsg(e);
      return 0;
    } finally { if (tok === this.listToken) this.olderBusy = false; }
  }

  /** Fetch up to `count` older headers from the server and append them to the list. Returns rows loaded. */
  async loadOlder(count = OLDER_STEP, refresh = true): Promise<number> {
    if (!this.account || this.olderBusy) return 0;
    const [acc, fol, tok] = [this.account, this.folder, this.listToken];
    this.olderBusy = true; this.olderError = null;
    try {
      const r = await api.loadOlder(acc, fol, count);
      if (tok !== this.listToken) return 0;
      this.serverTotal = r.serverTotal;
      this.reachedStart = r.reachedStart || r.loaded === 0; // nothing loadable: stop offering more (no endless retries)
      if (r.loaded) {
        const limit = r.loaded + 100;
        try {
          const rows = await api.listMessages(acc, fol, limit, this.messages.length);
          if (tok !== this.listToken) return 0;
          this.messages = mergeRows(this.messages, rows);
          this.cacheMore = rows.length >= limit;
        } catch (e) { this.cacheMore = true; throw e; } // the rows are cached; a retry pulls them from SQLite
      }
      if (refresh) void this.refreshUnread(acc); // sidebar counts come from the server, not the loaded rows
      return r.loaded;
    } catch (e) {
      if (tok === this.listToken) this.olderError = errMsg(e);
      return 0;
    } finally { if (tok === this.listToken) this.olderBusy = false; }
  }

  /** Repeat batches until `done()` or the start of the folder. False if cancelled, failed or superseded. */
  private async runBulk(kind: "all" | "date", done: () => boolean): Promise<boolean> {
    if (this.bulk || !this.account) return false;
    const [tok, id] = [this.listToken, ++this.bulkId];
    const live = () => tok === this.listToken && id === this.bulkId;
    this.bulk = { kind }; this.olderError = null;
    let ok = true;
    try {
      while (live() && !done() && this.moreAvailable) {
        const [before, wasCache, wasStart] = [this.messages.length, this.cacheMore, this.reachedStart];
        const n = wasCache ? await this.showMoreCached() : await this.loadOlder(BULK_STEP, false);
        if (this.olderError) { ok = false; break; }
        if (n === 0 && !wasCache && this.reachedStart === wasStart && this.messages.length === before) break; // no progress
      }
      ok = ok && live();
    } finally {
      if (live()) this.bulk = null;
    }
    if (tok === this.listToken && this.account) void this.refreshUnread(this.account);
    return ok;
  }
  cancelBulk() { this.bulkId++; this.bulk = null; }

  /** Load every older message (batches of 500). Asks first when a lot is left. */
  async loadAll() {
    if (this.bulk || !this.moreAvailable) return;
    if (this.remaining > LOAD_ALL_CONFIRM_ABOVE) {
      const ok = await this.ask({
        title: `Load all ${fmtN(this.remaining)} older messages?`,
        body: "This downloads the headers of every message in this folder. It can take a while and uses disk space on this Mac. You can cancel at any time; what is already loaded stays.",
        ok: "Load all",
      });
      if (!ok) return;
    }
    await this.runBulk("all", () => false);
  }

  /** Load older batches until a message dated at or before `ts` (unix seconds) is listed or the folder starts. */
  async jumpToDate(ts: number) {
    if (this.bulk) return;
    const finished = coversDate(this.messages, ts) || (await this.runBulk("date", () => coversDate(this.messages, ts)));
    if (finished) this.jumpTo = { ts, id: ++this.jumpSeq };
  }
  get inboxName() { return this.folders.find((f) => f.role === "inbox")?.name ?? "INBOX"; }

  async sync(silent = false) {
    if (!this.account || this.syncing) return;
    const [acc, fol] = [this.account, this.folder];
    this.syncing = true;
    try {
      const r = await api.syncFolder(acc, fol);
      if (acc === this.account && fol === this.folder) {
        if (r.uidvalidityReset) { this.reachedStart = false; this.messages = []; }
        if (r.new || r.updated || r.removed || r.uidvalidityReset) await this.loadMessages();
        if (acc === this.account && fol === this.folder) { this.serverTotal = r.serverTotal; this.syncUnreadFromList(acc, fol); }
      }
    } catch (e) { if (!silent) this.fail(e); }
    this.syncing = false;
  }

  async onNew(account: string, folder: string) {
    let total: number | null = null;
    // The backend already synced non-INBOX folders (background loop / live watcher) before emitting.
    if (folder === "INBOX" || folder === this.inboxName) {
      try { total = (await api.syncFolder(account, folder)).serverTotal; } catch { /* status dot shows connectivity */ }
    } else {
      this.newHandled.add(liveKey(account, folder));
    }
    if (account === this.account && folder === this.folder) {
      await this.loadMessages();
      if (total !== null && account === this.account && folder === this.folder) this.serverTotal = total; // authoritative
    }
    await this.refreshUnread(account);
  }

  /** Background round-robin tick finished: refresh unread dots and the open folder (unless `mail://new` just did). */
  async onSynced(account: string, folders: string[]) {
    const handled = folders.some((f) => this.newHandled.delete(liveKey(account, f)));
    void this.refreshUnread(account);
    if (!handled && account === this.account && folders.includes(this.folder)) await this.loadMessages();
  }

  async loadLive(account: string) {
    try {
      const [list, states] = await Promise.all([api.listLiveFolders(account), api.liveStates(account)]);
      this.liveFolders[account] = list;
      this.liveStatus[account] = states;
    } catch { /* settings modal and sidebar just show no live folders */ }
  }

  isLive(folder: string, account = this.account): boolean {
    return folder === this.inboxName || (!!account && (this.liveFolders[account] ?? []).includes(folder));
  }
  liveStateOf(folder: string, account = this.account): LiveStatus | undefined {
    if (!account) return undefined;
    if (folder === this.inboxName) {
      const c = this.status[account];
      return c === "connected" ? { state: "live" } : c === "offline" ? { state: "error", message: "Offline" } : { state: "connecting" };
    }
    if (this.liveBusy[liveKey(account, folder)]) return { state: "connecting" };
    return this.liveStatus[account]?.[folder] ?? (this.isLive(folder, account) ? { state: "connecting" } : undefined);
  }

  /** Toggle "keep live". On server refusal the toggle is rolled back and the error stays under the row until dismissed. */
  async toggleLive(folder: string, want = !this.isLive(folder)) {
    const account = this.account;
    if (!account || folder === this.inboxName) return;
    const k = liveKey(account, folder);
    if (this.liveBusy[k]) return;
    this.liveBusy[k] = true;
    delete this.liveError[k];
    try {
      const r = await api.setFolderLive(account, folder, want);
      const cur = (this.liveFolders[account] ?? []).filter((f) => f !== folder);
      if (r.state === "error") {
        this.liveError[k] = r.message ?? "The server refused to keep this folder live.";
        this.notify(this.liveError[k], "error");
        this.liveFolders[account] = cur.concat(want ? [] : [folder]);
      } else {
        this.liveFolders[account] = want ? [...cur, folder] : cur;
        const st = { ...(this.liveStatus[account] ?? {}) };
        if (want) st[folder] = r; else delete st[folder];
        this.liveStatus[account] = st;
        if (r.state === "polling" && r.message) this.notify(r.message);
      }
    } catch (e) {
      this.liveError[k] = errMsg(e);
      this.fail(e);
    }
    delete this.liveBusy[k];
  }
  dismissLiveError(folder: string, account = this.account) { if (account) delete this.liveError[liveKey(account, folder)]; }

  setUnread(account: string, folder: string, n: number) {
    this.unreadByFolder[account] = { ...(this.unreadByFolder[account] ?? {}), [folder]: Math.max(0, n) };
  }
  adjustUnread(folder: string, delta: number, account = this.account) {
    if (!account) return;
    this.setUnread(account, folder, (this.unreadByFolder[account]?.[folder] ?? 0) + delta);
  }
  /** Fetch per-folder unseen counts (IMAP STATUS). Failures are silent; the status dot shows connectivity. */
  async refreshUnread(account = this.account) {
    if (!account) return;
    try { this.unreadByFolder[account] = await api.unreadCounts(account); } catch { /* ignore */ }
  }

  private seenTimer: ReturnType<typeof setTimeout> | undefined;
  async open(uid: number, allowImages = false, skipTrackers = true) {
    if (!this.account) return;
    const token = ++this.bodyToken;
    clearTimeout(this.seenTimer);
    const mode = prefs.v.markRead;
    const markSeen = mode === "immediately";
    const [acc, fol] = [this.account, this.folder];
    this.selectedUid = uid;
    // Senders the user chose "always load images" for: images load (tracker-flagged ones are still skipped).
    // ... but only when this very message is DKIM-verified for its From domain (From text alone is forgeable).
    if (!allowImages && this.senderAllowed) {
      const ok = await this.senderAllowed(this.messages.find((x) => x.uid === uid)?.from?.address, acc, fol, uid);
      if (token !== this.bodyToken) return;
      if (ok) allowImages = true;
    }
    this.imagesAllowed = allowImages;
    this.skipTrackers = skipTrackers;
    this.bodyLoading = true;
    if (!allowImages) this.body = null;
    try {
      const b = await api.getMessage(this.account, this.folder, uid, allowImages, skipTrackers, markSeen);
      if (token !== this.bodyToken) return;
      this.body = b;
      const m = this.messages.find((x) => x.uid === uid);
      if (m && !m.flags.seen) {
        if (markSeen) {
          m.flags.seen = true;
          this.adjustUnread(this.folder, -1);
        } else if (mode === "delay") {
          this.seenTimer = setTimeout(() => {
            if (this.selectedUid === uid && this.account === acc && this.folder === fol) void this.setFlag("seen", true, uid);
          }, MARK_READ_DELAY_MS);
        }
      }
    } catch (e) { if (token === this.bodyToken) this.fail(e); }
    if (token === this.bodyToken) this.bodyLoading = false;
  }

  move(delta: number, openIt = false) {
    const rows = this.filtered;
    if (!rows.length) return;
    const i = rows.findIndex((m) => m.uid === this.selectedUid);
    const n = Math.min(rows.length - 1, Math.max(0, i < 0 ? 0 : i + delta));
    this.selectedUid = rows[n].uid;
    if (openIt) void this.open(rows[n].uid);
    queueMicrotask(() => document.querySelector(`[data-uid="${rows[n].uid}"]`)?.scrollIntoView({ block: "nearest" }));
  }

  private removeLocal(uid: number) {
    const rows = this.filtered;
    const i = rows.findIndex((m) => m.uid === uid);
    if (this.messages.find((m) => m.uid === uid)?.flags.seen === false) this.adjustUnread(this.folder, -1);
    this.messages = this.messages.filter((m) => m.uid !== uid);
    if (this.serverTotal !== null) this.serverTotal = Math.max(0, this.serverTotal - 1);
    const f = this.filtered, pick = prefs.v.afterRemove;
    const next = pick === "none" ? undefined : pick === "previous" ? f[Math.max(0, i - 1)] : f[Math.min(i, f.length - 1)];
    this.body = null;
    this.selectedUid = next?.uid ?? null;
  }

  /** Set by lib/org.svelte.ts: routes moves through the batch path (progress, partial failures, undo toast). */
  mover: ((uids: number[], dest: { role?: FolderRole; folder?: string }, from?: string) => Promise<void>) | null = null;
  /** Set by lib/org.svelte.ts: sender is on the "always load images" allow-list. */
  senderAllowed: ((addr: string | null | undefined, account: string, folder: string, uid: number) => Promise<boolean>) | null = null;

  async moveTo(role: FolderRole, uid = this.selectedUid) {
    if (!this.account || uid == null) return;
    if (this.mover) return this.mover([uid], { role });
    try {
      await api.moveMessage(this.account, this.folder, uid, role);
      this.removeLocal(uid);
      void this.refreshUnread();
      this.notify(role === "trash" ? "Moved to Trash" : role === "archive" ? "Archived" : "Moved");
    } catch (e) { this.fail(e); }
  }

  /** Dragged message (drag and drop onto a folder in the sidebar). */
  dragMsg = $state<{ uid: number; folder: string; uids?: number[] } | null>(null);
  dropFolder = $state<string | null>(null);

  async moveToFolder(dest: string, uid: number, from = this.folder) {
    if (!this.account || dest === from) return;
    if (this.mover) return this.mover([uid], { folder: dest }, from);
    const m = this.messages.find((x) => x.uid === uid);
    try {
      await api.moveMessageToFolder(this.account, from, uid, dest);
      this.removeLocal(uid);
      void this.refreshUnread();
      this.notify(`Moved${m?.subject ? ` “${m.subject.slice(0, 40)}”` : ""} to ${dest.split(/[/.]/).pop() || dest}`);
    } catch (e) { this.fail(e); }
  }

  async deleteForever(uid = this.selectedUid) {
    if (!this.account || uid == null) return;
    const ok = await this.ask({ title: "Delete forever?", body: "This message will be permanently deleted. This cannot be undone.", ok: "Delete forever", danger: true });
    if (!ok) return;
    try { await api.deleteMessage(this.account, this.folder, uid); this.removeLocal(uid); this.notify("Deleted"); } catch (e) { this.fail(e); }
  }

  async setFlag(flag: "seen" | "flagged", value: boolean, uid = this.selectedUid) {
    if (!this.account || uid == null) return;
    const m = this.messages.find((x) => x.uid === uid);
    if (!m) return;
    const prev = m.flags[flag];
    m.flags[flag] = value;
    const [acc, fol] = [this.account, this.folder];
    const changed = flag === "seen" && prev !== value;
    if (changed) this.adjustUnread(fol, value ? -1 : 1, acc);
    try { await api.setFlag(acc, fol, uid, flag, value); } catch (e) { m.flags[flag] = prev; if (changed) this.adjustUnread(fol, value ? 1 : -1, acc); this.fail(e); }
  }

  async removeAccount(email: string) {
    const snoozed = this.snoozeCountFor?.(email) ?? 0;
    const ok = await this.ask({ title: "Remove account?", body: `${email} and its cached mail will be removed from this Mac, along with its saved password. Your mail on the server is not touched.${snoozed ? ` ${snoozed} snoozed message${snoozed === 1 ? "" : "s"} will no longer come back to your Inbox on their own (any already moved to a Snoozed folder stay there on the server).` : ""}`, ok: "Remove", danger: true });
    if (!ok) return;
    try {
      await api.removeAccount(email);
      this.onAccountRemoved?.(email);
      this.accounts = this.accounts.filter((a) => a.email !== email);
      if (this.account === email) {
        this.account = null; this.messages = []; this.body = null; this.folders = [];
        if (this.accounts.length) await this.selectAccount(this.accounts[0].email);
      }
    } catch (e) { this.fail(e); }
  }
}

export const app = new AppState();

