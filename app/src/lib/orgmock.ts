// In-memory stand-in for the organising commands (plain-browser dev only). Reads the main mock.
import { mock } from "./mock";
import type { Folder, MessageSummary } from "./types";
import type { BulkResult, ColourEntry } from "./orgtypes";

const wait = <T,>(v: T, ms = 120) => new Promise<T>((r) => setTimeout(() => r(structuredClone(v)), ms));
const colours = new Map<string, number>();
const subs = new Set<string>();
let extra: Folder[] = [];
const ME = "you@example.org";

async function allFolders(): Promise<Folder[]> {
  const base = (await mock.list_folders()) as Folder[];
  return [...base, ...extra];
}

export const orgMock = {
  async search_mail(a: { query: string; scope: string; account: string | null; folder: string | null; limit: number; offset: number }) {
    const folders = a.scope === "folder" ? [a.folder ?? "INBOX"] : (await allFolders()).filter((f) => f.selectable).map((f) => f.name);
    let rows: MessageSummary[] = [];
    for (const f of folders) rows = rows.concat((await mock.list_messages({ folder: f, limit: 10000, offset: 0 })) as MessageSummary[]);
    const words = a.query.split(/\s+/).filter((w) => w && !/^-?[a-z_]+:/i.test(w)).map((w) => w.toLowerCase());
    const att = /has:attachment/i.test(a.query);
    const hits = rows.filter((m) => words.every((w) => `${m.subject} ${m.snippet} ${m.from?.address ?? ""}`.toLowerCase().includes(w))).filter((m) => !att || /invoice|notes/i.test(m.subject));
    return wait({ hits: hits.slice(a.offset, a.offset + a.limit), total: hits.length, hints: [] });
  },
  search_server: () => wait({ found: 0, searched: 1, skipped: 0, notes: ["Mock: no server."] }, 400),
  async folder_create(a: { parent: string | null; name: string }) { extra.push({ name: a.parent ? `${a.parent}/${a.name}` : a.name, role: "other", selectable: true, delimiter: "/" }); return wait(await allFolders(), 200); },
  async folder_rename(a: { folder: string; newName: string }) { extra = extra.map((f) => (f.name === a.folder ? { ...f, name: a.newName } : f)); return wait({ folders: await allFolders(), notes: [] }, 200); },
  async folder_move(a: { folder: string; newParent: string | null }) {
    const leaf = a.folder.split("/").pop()!, to = a.newParent ? `${a.newParent}/${leaf}` : leaf;
    extra = extra.map((f) => (f.name === a.folder ? { ...f, name: to } : f.name.startsWith(a.folder + "/") ? { ...f, name: to + f.name.slice(a.folder.length) } : f));
    return wait({ folders: await allFolders(), notes: [] }, 200);
  },
  folder_message_count: (a: { folder: string }) => mock.list_messages({ folder: a.folder, limit: 10000, offset: 0 }).then((r: unknown[]) => r.length),
  async folder_delete(a: { folder: string }) { extra = extra.filter((f) => f.name !== a.folder); return wait(await allFolders(), 200); },
  async folder_subscribe(a: { folder: string; subscribed: boolean }) { if (a.subscribed) subs.add(a.folder); else subs.delete(a.folder); return wait(undefined, 100); },
  async folder_subscriptions() { return wait([...(await allFolders()).map((f) => f.name)].filter((n) => !subs.has("!" + n)), 100); },
  async folder_empty(a: { folder: string }) {
    const rows = (await mock.list_messages({ folder: a.folder, limit: 10000, offset: 0 })) as MessageSummary[];
    for (const m of rows) await mock.delete_message({ folder: a.folder, uid: m.uid });
    return wait(rows.length, 300);
  },
  async bulk_move(a: { folder: string; items: { uid?: number; messageId?: string }[]; destFolder: string | null; destRole: string | null }): Promise<BulkResult> {
    const dest = a.destFolder ?? ((await allFolders()).find((f) => f.role === a.destRole)?.name ?? "Archive");
    const outcomes = [];
    for (const it of a.items) {
      if (it.uid != null) await mock.move_message_to({ folder: a.folder, uid: it.uid, destFolder: dest });
      outcomes.push({ uid: it.uid ?? null, ok: true, error: null, newUid: it.uid ?? null, messageId: it.messageId ?? null });
    }
    return wait({ outcomes, destFolder: dest, destValidity: 1 }, 250);
  },
  async bulk_flag(a: { folder: string; uids: number[]; flag: "seen" | "flagged"; value: boolean }): Promise<BulkResult> {
    for (const uid of a.uids) await mock.set_flag({ folder: a.folder, uid, flag: a.flag, value: a.value });
    return wait({ outcomes: a.uids.map((u) => ({ uid: u, ok: true, error: null, newUid: null, messageId: null })), destFolder: null, destValidity: null }, 200);
  },
  async bulk_delete_forever(a: { folder: string; uids: number[] }): Promise<BulkResult> {
    for (const uid of a.uids) await mock.delete_message({ folder: a.folder, uid });
    return wait({ outcomes: a.uids.map((u) => ({ uid: u, ok: true, error: null, newUid: null, messageId: null })), destFolder: null, destValidity: null }, 200);
  },
  set_colour(a: { folder: string; uid: number; colour: number }) { colours.set(`${a.folder}:${a.uid}`, a.colour); return wait(undefined, 30); },
  list_colours(a: { folder: string }): Promise<ColourEntry[]> {
    return wait([...colours].filter(([k, v]) => k.startsWith(a.folder + ":") && v > 0).map(([k, v]) => ({ uid: +k.split(":")[1], colour: v })), 30);
  },
  unsubscribe_info: (a: { uid: number; folder: string }) =>
    wait(a.folder === "INBOX" && a.uid === 2 ? { mailto: "unsubscribe@fastmail.example", httpsHost: "fastmail.example", httpsUrl: "https://fastmail.example/u/1", oneClick: true } : null, 60),
  sender_images_verified: () => wait(true, 20),
  unsubscribe_mailto: () => wait(undefined, 400),
  unsubscribe_one_click: () => wait(undefined, 500),
  get_source: (a: { uid: number }) => wait({ headers: `From: someone@example.org\r\nTo: ${ME}\r\nSubject: Mock ${a.uid}\r\n`, raw: `From: someone@example.org\r\nTo: ${ME}\r\nSubject: Mock ${a.uid}\r\n\r\nBody\r\n`, size: 90, truncated: false }, 100),
  save_eml: () => wait("/Users/you/Downloads/message.eml", 100),
  open_attachment: () => wait(undefined, 100),
  print_page: async () => { window.print(); },
};
