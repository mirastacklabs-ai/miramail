// Gateway to the local organisation backend (src-tauri/src/localorg.rs): conversation threads, mute, pin, VIP, blocked.
// Own file so the shared api.ts stays untouched. Wire types are camelCase and mirror localorg.rs. All of this is
// local to this Mac. In a plain browser an in-memory mock answers.
import { invoke } from "@tauri-apps/api/core";
import { isTauri } from "./api";
import type { MessageSummary } from "./types";

export interface ThreadEntry { uid: number; key: string }
export interface Marks { muted: number[]; pinned: number[] }
export interface VipEntry { address: string; name: string }
export interface BlockedEntry { address: string; addedAt: number }
export interface Senders { vip: VipEntry[]; blocked: BlockedEntry[] }

const mockVip: VipEntry[] = [];
const mockBlocked: BlockedEntry[] = [];
const mockMuted = new Set<string>();
const mockPinned = new Set<string>();
const subjKey = (s: string) => s.replace(/^((re|fwd?|aw|sv|wg):\s*)+/gi, "").trim().toLowerCase();
async function mockRows(folder: string): Promise<MessageSummary[]> {
  const { mock } = await import("./mock");
  return (await mock.list_messages({ folder, limit: 10000, offset: 0 })) as MessageSummary[];
}

export const localApi = {
  threadMap: async (account: string, folder: string): Promise<ThreadEntry[]> => {
    if (isTauri) return invoke("thread_map", { account, folder });
    const rows = await mockRows(folder);
    const n = new Map<string, number>();
    for (const r of rows) n.set(subjKey(r.subject), (n.get(subjKey(r.subject)) ?? 0) + 1);
    return rows.filter((r) => (n.get(subjKey(r.subject)) ?? 0) > 1).map((r) => ({ uid: r.uid, key: subjKey(r.subject) }));
  },
  threadMembers: async (account: string, folder: string, uid: number): Promise<MessageSummary[]> => {
    if (isTauri) return invoke("thread_members", { account, folder, uid });
    const rows = await mockRows(folder);
    const me = rows.find((r) => r.uid === uid);
    return me ? rows.filter((r) => subjKey(r.subject) === subjKey(me.subject)).sort((a, b) => (a.date ?? 0) - (b.date ?? 0)) : [];
  },
  muteThread: async (account: string, folder: string, uid: number, muted: boolean): Promise<number> => {
    if (isTauri) return invoke("mute_thread", { account, folder, uid, muted });
    const rows = await mockRows(folder);
    const me = rows.find((r) => r.uid === uid);
    if (me) { const k = subjKey(me.subject); if (muted) mockMuted.add(k); else mockMuted.delete(k); }
    return 1;
  },
  marks: async (account: string, folder: string): Promise<Marks> => {
    if (isTauri) return invoke("local_marks", { account, folder });
    const rows = await mockRows(folder);
    return { muted: rows.filter((r) => mockMuted.has(subjKey(r.subject))).map((r) => r.uid), pinned: rows.filter((r) => mockPinned.has(`${folder}:${r.uid}`)).map((r) => r.uid) };
  },
  pin: async (account: string, folder: string, uids: number[], pinned: boolean): Promise<number> => {
    if (isTauri) return invoke("pin_messages", { account, folder, uids, pinned });
    for (const u of uids) { if (pinned) mockPinned.add(`${folder}:${u}`); else mockPinned.delete(`${folder}:${u}`); }
    return uids.length;
  },
  senders: async (): Promise<Senders> => (isTauri ? invoke("sender_lists") : { vip: [...mockVip], blocked: [...mockBlocked] }),
  setVip: async (address: string, name: string | null, on: boolean): Promise<void> => {
    if (isTauri) return invoke("set_vip", { address, name, on });
    const i = mockVip.findIndex((v) => v.address === address.toLowerCase());
    if (i >= 0) mockVip.splice(i, 1);
    if (on) mockVip.push({ address: address.toLowerCase(), name: name ?? "" });
  },
  setBlocked: async (address: string, on: boolean): Promise<void> => {
    if (isTauri) return invoke("set_blocked", { address, on });
    const i = mockBlocked.findIndex((v) => v.address === address.toLowerCase());
    if (i >= 0) mockBlocked.splice(i, 1);
    if (on) mockBlocked.push({ address: address.toLowerCase(), addedAt: Math.floor(Date.now() / 1000) });
  },
};
