// Typed gateway to the organising / reader commands (src-tauri/src/organise.rs). In a plain browser the
// in-memory mock (lib/orgmock.ts, loaded on demand) answers instead.
import { invoke } from "@tauri-apps/api/core";
import { isTauri } from "./api";
import type { Folder, FolderRole } from "./types";
import type { FolderOp, BulkResult, ColourEntry, MoveItem, SearchResult, SearchScope, ServerSearchResult, SourceView, UnsubInfo } from "./orgtypes";

async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri) return invoke<T>(cmd, args);
  const { orgMock } = await import("./orgmock");
  const fn = (orgMock as unknown as Record<string, (a: Record<string, unknown>) => Promise<unknown>>)[cmd];
  if (!fn) throw `Unknown mock command ${cmd}`;
  return (await fn(JSON.parse(JSON.stringify(args)) as Record<string, unknown>)) as T;
}

/** Seconds east of UTC, for `today` / `before:` day boundaries. */
const tz = () => -new Date().getTimezoneOffset() * 60;

export const orgApi = {
  searchMail: (query: string, scope: SearchScope, account: string | null, folder: string | null, limit = 50, offset = 0) =>
    call<SearchResult>("search_mail", { query, scope, account, folder, limit, offset, tzOffset: tz() }),
  searchServer: (query: string, scope: SearchScope, account: string | null, folder: string | null) =>
    call<ServerSearchResult>("search_server", { query, scope, account, folder, tzOffset: tz() }),

  folderCreate: (account: string, parent: string | null, name: string) => call<Folder[]>("folder_create", { account, parent, name }),
  folderMove: (account: string, folder: string, newParent: string | null) => call<FolderOp>("folder_move", { account, folder, newParent }),
  folderRename: (account: string, folder: string, newName: string) => call<FolderOp>("folder_rename", { account, folder, newName }),
  folderMessageCount: (account: string, folder: string) => call<number>("folder_message_count", { account, folder }),
  folderDelete: (account: string, folder: string, confirmCount: number | null) => call<Folder[]>("folder_delete", { account, folder, confirmCount }),
  folderSubscribe: (account: string, folder: string, subscribed: boolean) => call<void>("folder_subscribe", { account, folder, subscribed }),
  folderSubscriptions: (account: string) => call<string[]>("folder_subscriptions", { account }),
  folderEmpty: (account: string, folder: string) => call<number>("folder_empty", { account, folder }),

  bulkMove: (account: string, folder: string, items: MoveItem[], dest: { folder?: string; role?: FolderRole }, expectedValidity: number | null = null) =>
    call<BulkResult>("bulk_move", { account, folder, items, destFolder: dest.folder ?? null, destRole: dest.role ?? null, expectedValidity }),
  bulkFlag: (account: string, folder: string, uids: number[], flag: "seen" | "flagged", value: boolean) =>
    call<BulkResult>("bulk_flag", { account, folder, uids, flag, value }),
  bulkDeleteForever: (account: string, folder: string, uids: number[]) => call<BulkResult>("bulk_delete_forever", { account, folder, uids }),

  setColour: (account: string, folder: string, uid: number, colour: number) => call<void>("set_colour", { account, folder, uid, colour }),
  listColours: (account: string, folder: string) => call<ColourEntry[]>("list_colours", { account, folder }),

  /** Is this message DKIM-verified for its From domain (the condition for auto-loading images)? */
  senderImagesVerified: (account: string, folder: string, uid: number) => call<boolean>("sender_images_verified", { account, folder, uid }),
  unsubscribeInfo: (account: string, folder: string, uid: number) => call<UnsubInfo | null>("unsubscribe_info", { account, folder, uid }),
  unsubscribeMailto: (account: string, folder: string, uid: number) => call<void>("unsubscribe_mailto", { account, folder, uid }),
  unsubscribeOneClick: (account: string, folder: string, uid: number) => call<void>("unsubscribe_one_click", { account, folder, uid }),

  getSource: (account: string, folder: string, uid: number) => call<SourceView>("get_source", { account, folder, uid }),
  saveEml: (account: string, folder: string, uid: number) => call<string>("save_eml", { account, folder, uid }),
  openAttachment: (account: string, folder: string, uid: number, id: string) => call<void>("open_attachment", { account, folder, uid, id }),
  /** iPhone only (src-tauri/src/share.rs): system share sheet / Quick Look / print sheet. */
  shareAttachment: (account: string, folder: string, uid: number, id: string) => call<void>("share_attachment", { account, folder, uid, id }),
  previewAttachment: (account: string, folder: string, uid: number, id: string) => call<void>("preview_attachment", { account, folder, uid, id }),
  shareEml: (account: string, folder: string, uid: number) => call<void>("share_eml", { account, folder, uid }),
  printMessage: (html: string, title: string) => call<void>("print_message", { html, title }),
  printPage: () => call<void>("print_page"),
};
