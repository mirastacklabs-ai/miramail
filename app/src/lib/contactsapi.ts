// Typed gateway to the address book commands. In a plain browser the in-memory mock (lib/contactsmock.ts) answers.
import { invoke } from "@tauri-apps/api/core";
import { isTauri } from "./api";
import type { Contact, ContactAddr, ContactCardInfo, ContactGroup, ContactImportResult, ContactInput, ContactSuggest } from "./contactstypes";

async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri) return invoke<T>(cmd, args);
  const { contactsMock } = await import("./contactsmock");
  const fn = (contactsMock as unknown as Record<string, (a: Record<string, unknown>) => Promise<unknown>>)[cmd];
  if (!fn) throw `Unknown mock command ${cmd}`;
  return (await fn(JSON.parse(JSON.stringify(args)) as Record<string, unknown>)) as T;
}

export const contactsApi = {
  list: (query = "", group: string | null = null) => call<Contact[]>("contacts_list", { query, group }),
  save: (contact: ContactInput) => call<Contact>("contacts_save", { contact }),
  remove: (id: number) => call<void>("contacts_delete", { id }),
  groups: () => call<ContactGroup[]>("contacts_groups"),
  groupRename: (id: number, name: string) => call<void>("contacts_group_rename", { id, name }),
  groupDelete: (id: number) => call<void>("contacts_group_delete", { id }),
  expandGroup: (name: string) => call<ContactAddr[]>("contacts_expand_group", { name }),
  suggest: (prefix: string, limit = 5) => call<ContactSuggest>("contacts_suggest", { prefix, limit }),
  /** The saved contact for a sender (if any) and how many cached messages came from them. */
  card: (address: string) => call<ContactCardInfo>("contacts_card", { address }),
  /** Frequent correspondents from the cached mail that are not saved yet. */
  fromMail: (limit = 40) => call<ContactAddr[]>("contacts_from_mail", { limit }),
  addMany: (entries: ContactAddr[]) => call<number>("contacts_add_many", { entries }),
  importVcf: (text: string) => call<ContactImportResult>("contacts_import", { text }),
  /** Writes a new .vcf file in Downloads (never overwrites) and returns its path. */
  exportVcf: (group: string | null = null) => call<string>("contacts_export", { group }),
};
