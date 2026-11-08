// Gateway for the Tier 2 essentials: snooze, mailbox export, settings backup, storage controls.
// Wire types are camelCase and mirror the Rust structs in snooze.rs, cache_ctl.rs, mbox_export.rs and backup.rs
// (docs/APP_API.md). Outside Tauri (plain browser dev) a small in-memory mock answers.
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { isTauri } from "./api";

export interface Snooze {
  id: number; account: string; folder: string; uid: number; uidvalidity: number; messageId: string | null;
  mode: "moved" | "hidden"; snoozeFolder: string | null; subject: string; from: string;
  wakeAt: number; createdAt: number; state: "snoozed" | "waking" | "failed"; error: string | null;
  destUid?: number | null; destValidity?: number | null; attempts?: number; nextTry?: number | null;
}
export interface SnoozeInfo { folder: string | null }

export interface CacheUsage { messages: number; withBody: number; bodyBytes: number; textBytes: number }
export interface CachePolicy { keepDays: number | null; maxMb: number | null }
export interface StorageReport { dbBytes: number; accounts: { account: string; usage: CacheUsage; policy: CachePolicy }[] }

export interface ExportProgress { state: "running" | "done" | "cancelled" | "error"; done: number; total: number; skipped: number; path: string | null; error: string | null }

export interface BackupSectionPreview { key: string; label: string; count: number }
export interface BackupPreview { version: number; createdAt: number | null; sections: BackupSectionPreview[]; accounts: string[]; data: Record<string, unknown> }

// ---- browser-dev mock ----
let mockSnoozes: Snooze[] = [];
let mockSeq = 1;
const mockPolicy: Record<string, CachePolicy> = {};
const wait = <T>(v: T, ms = 20) => new Promise<T>((r) => setTimeout(() => r(v), ms));
const mockFns: Record<string, (a: any) => Promise<any>> = {
  snooze_info: () => wait({ folder: null }),
  snooze_add: (a) => {
    if (a.wakeAt <= Date.now()) return Promise.reject("Pick a time in the future.");
    const s: Snooze = { id: mockSeq++, account: a.account, folder: a.folder, uid: a.uid, uidvalidity: 1, messageId: null, mode: "hidden", snoozeFolder: null, subject: "(mock message)", from: "someone@example.org", wakeAt: a.wakeAt, createdAt: Date.now(), state: "snoozed", error: null };
    mockSnoozes.push(s);
    return wait(s);
  },
  snooze_list: () => wait([...mockSnoozes].sort((x, y) => x.wakeAt - y.wakeAt)),
  snooze_cancel: (a) => { mockSnoozes = mockSnoozes.filter((s) => s.id !== a.id); return wait(undefined); },
  snooze_forget: (a) => { mockSnoozes = mockSnoozes.filter((s) => s.id !== a.id); return wait(undefined); },
  snooze_count: (a) => wait(mockSnoozes.filter((s) => s.account === a.account).length),
  mbox_export: () => Promise.reject("Mailbox export needs the MiraMail app."),
  mbox_cancel: () => wait(undefined),
  backup_export: () => Promise.reject("Backup needs the MiraMail app."),
  backup_inspect: () => Promise.reject("Restore needs the MiraMail app."),
  storage_usage: () => wait({ dbBytes: 12_345_678, accounts: [{ account: "demo@example.org", usage: { messages: 120, withBody: 80, bodyBytes: 9_000_000, textBytes: 1_000_000 }, policy: mockPolicy["demo@example.org"] ?? { keepDays: null, maxMb: null } }] } satisfies StorageReport),
  storage_clear_bodies: () => wait(80),
  storage_set_policy: (a) => { mockPolicy[a.account] = a.policy; return wait(a.policy); },
};

async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri) return invoke<T>(cmd, args);
  return mockFns[cmd](args) as Promise<T>;
}

export const t2 = {
  snoozeInfo: (account: string) => call<SnoozeInfo>("snooze_info", { account }),
  snoozeAdd: (account: string, folder: string, uid: number, wakeAt: number, useFolder: boolean) => call<Snooze>("snooze_add", { account, folder, uid, wakeAt, useFolder }),
  snoozeList: () => call<Snooze[]>("snooze_list"),
  snoozeCancel: (id: number) => call<void>("snooze_cancel", { id }),
  snoozeForget: (id: number) => call<void>("snooze_forget", { id }),
  snoozeCount: (account: string) => call<number>("snooze_count", { account }),
  mboxExport: (account: string, folders: string[] | null) => call<void>("mbox_export", { account, folders }),
  mboxCancel: () => call<void>("mbox_cancel"),
  backupExport: (sections: Record<string, unknown>) => call<string>("backup_export", { sections }),
  backupInspect: (text: string) => call<BackupPreview>("backup_inspect", { text }),
  storageUsage: () => call<StorageReport>("storage_usage"),
  storageClearBodies: (account: string) => call<number>("storage_clear_bodies", { account }),
  storageSetPolicy: (account: string, policy: CachePolicy) => call<CachePolicy>("storage_set_policy", { account, policy }),
};

async function on<T>(event: string, cb: (p: T) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  return listen<T>(event, (e) => cb(e.payload));
}
export const onSnoozeChanged = (cb: () => void) => on<unknown>("snooze://changed", () => cb());
export const onExportProgress = (cb: (p: ExportProgress) => void) => on<ExportProgress>("export://progress", cb);
