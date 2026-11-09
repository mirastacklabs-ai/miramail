// In-memory mock backend used only when running in a plain browser (dev).
import type {
  AccountInfo, Advanced, AttachmentData, Folder, FolderRole, ImageInfo, MessageBody, MessageSummary, Outgoing, SyncReport, LoadOlderResult, FlagName, SyncSettings, LiveStatus, DraftRec, Signature, SigDefaults, Identity, AddrSuggestion, OutboxItem, OutboxRestored,
} from "./types";
import { calMock, mockInvite } from "./mockcal";

const now = Math.floor(Date.now() / 1000);
const flags = (seen: boolean, flagged = false) => ({ seen, flagged, answered: false, draft: false, deleted: false });
const ME = "you@example.org";

const accounts: AccountInfo[] = [];
const drafts = new Map<string, DraftRec>();
const mockOutbox = new Map<string, { it: OutboxItem; restore: string; atts: AttachmentData[] }>();
let layoutJson: string | null = null;
const folders: Folder[] = [
  { name: "INBOX", role: "inbox", selectable: true },
  { name: "Sent", role: "sent", selectable: true },
  { name: "Drafts", role: "drafts", selectable: true },
  { name: "Archive", role: "archive", selectable: true },
  { name: "Junk", role: "junk", selectable: true },
  { name: "Trash", role: "trash", selectable: true },
  { name: "Projects", role: "other", selectable: true, delimiter: "/" },
  { name: "Projects/Alpha", role: "other", selectable: true, delimiter: "/" },
  { name: "Projects/Beta", role: "other", selectable: true, delimiter: "/" },
];

interface M { s: MessageSummary; text: string; html: string | null; blocked: number; att: { id: string; name: string; mime: string; size: number }[] }
let msgs: M[] = [];
function mk(uid: number, folder: string, from: [string, string], subject: string, ageH: number, seen: boolean, text: string, html: string | null = null, blocked = 0, flagged = false, att: M["att"] = []): M {
  return {
    s: { account: ME, folder, uid, messageId: `<m${uid}@mock>`, subject, from: { name: from[0], address: from[1] }, to: [{ name: null, address: ME }],
      date: now - ageH * 3600, flags: flags(seen, flagged), snippet: text.replace(/\s+/g, " ").slice(0, 120), hasBody: true },
    text, html, blocked, att,
  };
}
function seed() {
  msgs = [
    mk(1, "INBOX", ["Ada Lovelace", "ada@analytical.example"], "Notes on the Bernoulli program", 1, false,
      "Hello,\n\nI have finished the notes. The key section is G, which computes the Bernoulli numbers.\nSee https://example.org/notes for the full text.\n\nAda",
      `<p>Hello,</p><p>I have finished the notes. The key section is <b>G</b>, which computes the Bernoulli numbers.</p><p>See <a href="https://example.org/notes">the full text</a>.</p>`, 0, true,
      [{ id: "a1", name: "notes.pdf", mime: "application/pdf", size: 183204 }]),
    mk(2, "INBOX", ["Fastmail Weekly", "news@fastmail.example"], "Your week in review", 5, false,
      "Your week in review. Images are hidden.",
      `<div style="font-family:sans-serif"><h2>Your week in review</h2><p>Here is what happened.</p><img src="https://cdn.fastmail.example/img/week.png" width="200" height="80" alt="banner"><img src="https://us1.list-manage.com/track/open.php?u=1&amp;id=2" width="1" height="1" alt=""><p>Thanks for reading.</p><a href="https://example.com/unsubscribe?u=1">Unsubscribe</a></div>`, 2),
    mk(3, "INBOX", ["Grace Hopper", "grace@navy.example"], "Re: Compiler timeline", 26, true,
      "Let's meet Thursday to review the A-0 timeline.\nBring the punch cards.", null),
    mk(4, "INBOX", ["Linus", "linus@kernel.example"], "Merge window", 70, true, "Merge window opens Monday. Please send pull requests early.", null, 0, true),
    mk(5, "INBOX", ["Billing", "billing@host.example"], "Invoice #1042", 200, true, "Your invoice is attached.", null, 0, false, [{ id: "a2", name: "invoice-1042.pdf", mime: "application/pdf", size: 52110 }]),
    mk(6, "Sent", [ME, ME], "Hello", 300, true, "Sent message sample.", null),
  ];
  for (let i = 7; i < 40; i++) msgs.push(mk(i, "INBOX", ["Person " + i, `p${i}@example.net`], `Thread number ${i}`, 300 + i * 9, true, `Body of message ${i}. Lorem ipsum dolor sit amet.`));
}
seed();

/** Stand-in for the backend's tracker analysis (mock data only). */
function mockImages(html: string): ImageInfo[] {
  const out: ImageInfo[] = [];
  for (const mt of html.matchAll(/<img [^>]*src="(https?:[^"]*)"[^>]*>/g)) {
    const url = mt[1].replace(/&amp;/g, "&");
    const host = new URL(url).hostname;
    const w = /width="(\d+)"/.exec(mt[0]), h = /height="(\d+)"/.exec(mt[0]);
    const width = w ? +w[1] : null, height = h ? +h[1] : null;
    const tiny = (width ?? 99) <= 2 || (height ?? 99) <= 2;
    if (host.endsWith("list-manage.com") || tiny) {
      out.push({ url, host, width, height, hidden: false, verdict: "tracker", tracker: host.endsWith("list-manage.com") ? "Mailchimp open tracking" : null,
        reasons: [...(host.endsWith("list-manage.com") ? ["matches known tracker: Mailchimp open tracking"] : []), ...(tiny ? [`tiny image (${width ?? "?"}x${height ?? "?"})`] : [])] });
    } else out.push({ url, host, width, height, hidden: false, verdict: "clean", tracker: null, reasons: ["no tracking signs found"] });
  }
  return out;
}

const wait = <T,>(v: T, ms = 120) => new Promise<T>((r) => setTimeout(() => r(structuredClone(v)), ms));
const roleFolder = (role: FolderRole) => folders.find((f) => f.role === role)?.name ?? "INBOX";

let syncSettings: SyncSettings = { foldersPerCycle: 2, intervalSeconds: 60, maxLiveFolders: 5, initialSyncMessages: 200 };

/** Mock server: INBOX holds MOCK_INBOX_TOTAL messages, only the seeded ones are "cached" until load_older fetches more. */
const MOCK_INBOX_TOTAL = 3412;
let olderUid = 100_000;
const serverTotal = (folder: string) => {
  const cached = msgs.filter((m) => m.s.folder === folder).length;
  return folder === "INBOX" ? Math.max(cached, MOCK_INBOX_TOTAL - removedInbox) : cached;
};
let removedInbox = 0;
const liveSet = new Set<string>();
let mockSigs: Signature[] = [];
let mockSigDefaults: SigDefaults[] = [];
let mockIdents: Identity[] = [];
let mockSeq = 1;

export const mock = {
  ...calMock,
  get_sync_settings: () => wait(syncSettings, 60),
  async set_sync_settings(a: { settings: SyncSettings }) {
    const s = a.settings;
    if (s.foldersPerCycle < 1 || s.foldersPerCycle > 20) throw "Folders per cycle must be between 1 and 20.";
    if (s.intervalSeconds < 15 || s.intervalSeconds > 3600) throw "Cycle interval must be between 15 and 3600 seconds.";
    if (s.maxLiveFolders < 1 || s.maxLiveFolders > 10) throw "Live folders must be between 1 and 10.";
    if (!Number.isInteger(s.initialSyncMessages) || s.initialSyncMessages < 50 || s.initialSyncMessages > 5000) throw "Initial messages per folder must be between 50 and 5000.";
    syncSettings = { ...s };
    return wait(syncSettings, 150);
  },
  list_live_folders: () => wait([...liveSet], 60),
  live_states: () => wait(Object.fromEntries([...liveSet].map((f) => [f, { state: "live" } as LiveStatus])), 60),
  async set_folder_live(a: { folder: string; live: boolean }): Promise<LiveStatus> {
    await wait(null, 600);
    if (!a.live) { liveSet.delete(a.folder); return { state: "off" }; }
    if (a.folder === "Junk") return { state: "error", message: "The server limits how many connections you can have open. Turn off another live folder and try again. Server said: imap: NO [LIMIT] Too many simultaneous connections" };
    if (!liveSet.has(a.folder) && liveSet.size >= syncSettings.maxLiveFolders) throw `You can keep at most ${syncSettings.maxLiveFolders} folders live.`;
    liveSet.add(a.folder);
    return a.folder === "Archive" ? { state: "polling", message: "Server doesn't support push; checking every 60 s." } : { state: "live" };
  },
  async add_account(a: { email: string; password: string; advanced?: Advanced }) {
    await wait(null, 900);
    if (a.password === "wrong") throw "The server rejected that email or password. Check them and try again.";
    const info: AccountInfo = a.advanced
      ? { email: a.email, username: a.advanced.username, imap: a.advanced.imap, smtp: a.advanced.smtp, provider: null }
      : { email: a.email, username: a.email, imap: { host: "imap.example.org", port: 993, tls: "implicit" }, smtp: { host: "smtp.example.org", port: 465, tls: "implicit" }, provider: "Mock Provider" };
    accounts.push(info);
    return structuredClone(info);
  },
  list_accounts: () => wait(accounts),
  async remove_account(a: { email: string }) { const i = accounts.findIndex((x) => x.email === a.email); if (i >= 0) accounts.splice(i, 1); return wait(undefined, 50); },
  list_folders: () => wait(folders),
  unread_counts: () => {
    const r: Record<string, number> = {};
    for (const m of msgs) if (!m.s.flags.seen) r[m.s.folder] = (r[m.s.folder] ?? 0) + 1;
    return wait(r, 150);
  },
  sync_folder: (a: { folder: string }) => wait<SyncReport>({ new: 0, updated: 0, removed: 0, uidvalidityReset: false, serverTotal: serverTotal(a.folder) }, 300),
  async load_older(a: { folder: string; count?: number }): Promise<LoadOlderResult> {
    await wait(null, 450);
    const total = serverTotal(a.folder);
    const cached = msgs.filter((m) => m.s.folder === a.folder);
    const n = Math.min(Math.max(1, Math.min(a.count ?? 200, 1000)), Math.max(0, total - cached.length));
    const oldest = cached.reduce((t, m) => Math.min(t, m.s.date ?? t), now);
    for (let i = 0; i < n; i++) {
      const uid = olderUid--;
      msgs.push(mk(uid, a.folder, ["Archive " + (uid % 97), `old${uid % 97}@example.net`], `Older message ${uid}`, (now - oldest) / 3600 + 24 + i * 6, uid % 5 !== 0, `Body of older message ${uid}.`));
    }
    const left = total - msgs.filter((m) => m.s.folder === a.folder).length;
    return { loaded: n, remainingOnServer: Math.max(0, left), serverTotal: total, reachedStart: left <= 0 };
  },
  list_messages: (a: { folder: string; limit: number; offset: number }) =>
    wait(msgs.filter((m) => m.s.folder === a.folder).map((m) => m.s).sort((x, y) => (y.date ?? 0) - (x.date ?? 0)).slice(a.offset, a.offset + a.limit), 250),
  async get_message(a: { folder: string; uid: number; allowRemoteImages: boolean; skipTrackers?: boolean; markSeen?: boolean }): Promise<MessageBody> {
    const m = msgs.find((x) => x.s.folder === a.folder && x.s.uid === a.uid);
    if (!m) throw "That message no longer exists.";
    if (a.markSeen !== false) m.s.flags.seen = true;
    let html = m.html;
    const images = html ? mockImages(html) : [];
    const skip = a.skipTrackers ?? true;
    if (!a.allowRemoteImages) { if (html) html = html.replace(/<img [^>]*src="https?:[^"]*"[^>]*>/g, ""); }
    else if (skip && html) html = html.replace(/<img [^>]*src="https?:\/\/us1\.list-manage\.com[^"]*"[^>]*>/g, "");
    return wait({ summary: m.s, text: m.text, html, blockedRemoteCount: a.allowRemoteImages ? 0 : m.blocked, images, attachments: m.att, replyTo: [], cc: m.s.uid === 1 ? [{ name: "Charles", address: "charles@engine.example" }] : [], references: null, invite: m.s.uid === 3 && m.s.folder === "INBOX" ? mockInvite() : null });
  },
  safety_report: (a: { uid: number }) => wait(
    a.uid === 2
      ? { score: 28, level: "caution", reasons: [
          { code: "LINK_SHORTENER", severity: "low", title: "Link uses a URL shortener", detail: "A link uses the URL shortener bit.ly, which hides the real destination.", count: 1 },
          { code: "SENDER_FIRST_TIME", severity: "low", title: "First message from this sender", detail: "No earlier mail from news@fastmail.example is stored on this Mac.", count: 1 },
          { code: "SENDER_REPLYTO_MISMATCH", severity: "medium", title: "Replies go to a different domain", detail: "Replies would go to other.example but the message comes from fastmail.example.", count: 1 } ] }
      : { score: 0, level: "none_found", reasons: [] }, 80),
  get_attachment: (): Promise<AttachmentData> => wait({ name: "file.txt", mime: "text/plain", dataBase64: "aGVsbG8=" }),
  async set_flag(a: { folder: string; uid: number; flag: FlagName; value: boolean }) {
    const m = msgs.find((x) => x.s.folder === a.folder && x.s.uid === a.uid); if (m) m.s.flags[a.flag] = a.value; return wait(undefined, 40);
  },
  async move_message(a: { folder: string; uid: number; destRole: FolderRole }) {
    const m = msgs.find((x) => x.s.folder === a.folder && x.s.uid === a.uid); if (m) { if (a.folder === "INBOX") removedInbox++; m.s.folder = roleFolder(a.destRole); } return wait(undefined, 80);
  },
  async move_message_to(a: { folder: string; uid: number; destFolder: string }) {
    const m = msgs.find((x) => x.s.folder === a.folder && x.s.uid === a.uid); if (m) { if (a.folder === "INBOX") removedInbox++; m.s.folder = a.destFolder; } return wait(undefined, 80);
  },
  async delete_message(a: { folder: string; uid: number }) { if (a.folder === "INBOX") removedInbox++; msgs = msgs.filter((x) => !(x.s.folder === a.folder && x.s.uid === a.uid)); return wait(undefined, 80); },
  // ---- send pipeline (browser dev only): a held message is "sent" by a timer; undo cancels it ----
  async outbox_submit(a: { account: string; message: Outgoing; restore: string; inlineCount: number; delaySecs: number; sendAt?: number | null }): Promise<OutboxItem> {
    if (!a.message.to.length) throw "Add at least one recipient.";
    const id = `o${mockSeq++}`, t = Date.now();
    const atts = (a.message.attachments ?? []).slice(0, Math.max(0, (a.message.attachments ?? []).length - a.inlineCount));
    const it: OutboxItem = { id, account: a.account, to: a.message.to, subject: a.message.subject, state: a.sendAt ? "scheduled" : "holding", error: null, dueAt: a.sendAt ?? t + a.delaySecs * 1000, nextRetryAt: null, failures: 0, attachments: a.message.attachments?.length ?? 0, createdAt: t, scheduledAt: a.sendAt ?? null };
    mockOutbox.set(id, { it, restore: a.restore, atts });
    const ping = (kind: string) => window.dispatchEvent(new CustomEvent("mock-outbox", { detail: { id, kind } }));
    setTimeout(() => {
      const e = mockOutbox.get(id);
      if (!e || (e.it.state !== "holding" && e.it.state !== "scheduled")) return;
      mockOutbox.delete(id);
      ping("sent");
    }, (a.sendAt ? a.sendAt - t : a.delaySecs * 1000) + 50);
    setTimeout(() => ping("added"), 0);
    return wait(it, 20);
  },
  outbox_list: () => wait([...mockOutbox.values()].map((e) => e.it), 10),
  async outbox_undo(a: { id: string }): Promise<OutboxRestored> {
    const e = mockOutbox.get(a.id);
    if (!e) throw "That message has already been sent or removed.";
    // Two-step like the backend: parked until outbox_confirm_edit.
    e.it = { ...e.it, state: "interrupted" };
    return wait({ restore: e.restore, attachments: e.atts }, 10);
  },
  async outbox_confirm_edit(a: { id: string }) {
    const e = mockOutbox.get(a.id);
    if (e && e.it.state === "interrupted") mockOutbox.delete(a.id);
    window.dispatchEvent(new CustomEvent("mock-outbox", { detail: { id: a.id, kind: "removed" } }));
    return wait(undefined, 10);
  },
  async outbox_retry(a: { id: string }) { const e = mockOutbox.get(a.id); if (e) { e.it = { ...e.it, state: "holding", dueAt: Date.now() }; } return wait(undefined, 10); },
  async outbox_discard(a: { id: string }) { mockOutbox.delete(a.id); window.dispatchEvent(new CustomEvent("mock-outbox", { detail: { id: a.id, kind: "removed" } })); return wait(undefined, 10); },
  draft_mirror_save: () => wait("unsupported" as const, 10),
  draft_mirror_remove: () => wait(undefined, 10),
  get_raw_message: (a: { uid: number }): Promise<AttachmentData> => wait({ name: `message-${a.uid}.eml`, mime: "message/rfc822", dataBase64: "RnJvbTogYUBiLmMNCg0KaGk=" }),
  composer_data: () => wait({ signatures: mockSigs, defaults: mockSigDefaults, identities: mockIdents }, 30),
  async save_signature(a: { signature: Signature }) {
    if (!a.signature.name.trim()) throw "Give the signature a name.";
    const sig = { ...a.signature, id: a.signature.id || `s${mockSeq++}` };
    mockSigs = mockSigs.some((x) => x.id === sig.id) ? mockSigs.map((x) => (x.id === sig.id ? sig : x)) : [...mockSigs, sig];
    return wait(sig, 30);
  },
  async delete_signature(a: { id: string }) {
    mockSigs = mockSigs.filter((x) => x.id !== a.id);
    mockSigDefaults = mockSigDefaults.map((d) => ({ ...d, newId: d.newId === a.id ? null : d.newId, replyId: d.replyId === a.id ? null : d.replyId }));
    mockIdents = mockIdents.map((i) => (i.signatureId === a.id ? { ...i, signatureId: null } : i));
    return wait(undefined, 30);
  },
  async set_signature_defaults(a: { defaults: SigDefaults }) { mockSigDefaults = [...mockSigDefaults.filter((d) => d.account !== a.defaults.account), a.defaults]; return wait(undefined, 30); },
  async save_identity(a: { identity: Identity }) {
    if (!/^[^\s@<>,]+@[^\s@<>,]+\.[^\s@<>,]+$/.test(a.identity.address.trim())) throw "That doesn't look like an email address.";
    const ident = { ...a.identity, address: a.identity.address.trim(), id: a.identity.id || `i${mockSeq++}` };
    mockIdents = mockIdents.some((x) => x.id === ident.id) ? mockIdents.map((x) => (x.id === ident.id ? ident : x)) : [...mockIdents, ident];
    return wait(ident, 30);
  },
  async delete_identity(a: { id: string }) { mockIdents = mockIdents.filter((x) => x.id !== a.id); return wait(undefined, 30); },
  sanitize_compose: (a: { html: string }) => wait(a.html.replace(/<(script|style|iframe|object|embed)[\s\S]*?<\/\1>/gi, "").replace(/\son\w+="[^"]*"/gi, ""), 10),
  suggest_addresses(a: { prefix: string; limit?: number; account?: string }): Promise<AddrSuggestion[]> {
    const n = a.prefix.trim().toLowerCase();
    const seen = new Map<string, AddrSuggestion>();
    for (const m of msgs) for (const x of [m.s.from, ...m.s.to]) if (x && x.address !== ME && `${x.name ?? ""} ${x.address}`.toLowerCase().includes(n)) seen.set(x.address, { name: x.name, address: x.address });
    return wait(n ? [...seen.values()].slice(0, a.limit ?? 8) : [], 30);
  },
  save_attachment_to_downloads: (a: { id: string }) => wait(`/Users/you/Downloads/${a.id}.pdf`, 200),
  // ---- workspace persistence (browser dev only; lost on reload like the rest of the mock) ----
  async save_draft(a: { draft: DraftRec }) { const t = Date.now(); drafts.set(a.draft.id, { ...a.draft, updatedAt: t }); return wait(t, 10); },
  list_drafts: () => wait([...drafts.values()].sort((x, y) => y.updatedAt - x.updatedAt), 10),
  async delete_draft(a: { id: string }) { drafts.delete(a.id); return wait(undefined, 10); },
  async save_layout(a: { json: string }) { layoutJson = a.json; return wait(undefined, 10); },
  load_layout: () => wait(layoutJson, 10),
  // Detached windows need the desktop app; the browser mock degrades to "none open".
  detach_pane: () => Promise.reject("Moving a pane to its own window needs the desktop app."),
  get_pane: () => wait(null as string | null, 0),
  save_pane: () => wait(undefined, 0),
  attach_pane: () => Promise.reject("Moving a pane to its own window needs the desktop app."),
  release_pane: () => wait(undefined, 0),
  close_pane_window: () => wait(undefined, 0),
  list_windows: () => wait([] as unknown[], 0),
  window_at: () => wait(null as string | null, 0),
  focus_window: () => wait(undefined, 0),
};
