// Pure helpers for the address book UI (no Svelte, no Tauri), tested with node.
import type { ContactAddr, ContactSuggest } from "./contactstypes";

/** Split a free-text field (commas, semicolons or new lines) into trimmed non-empty parts. */
export function splitList(v: string): string[] {
  return v.split(/[,;\n]/).map((x) => x.trim()).filter(Boolean);
}

/** "Name <a@b.c>" for a compose recipient list (quotes a name that needs it). */
export function mailbox(name: string | null, address: string): string {
  const n = (name ?? "").replace(/[\r\n]+/g, " ").trim();
  if (!n) return address;
  return /[",;<>()@:]/.test(n) ? `"${n.replace(/["\\]/g, "\\$&")}" <${address}>` : `${n} <${address}>`;
}

/** One row of the recipient autocomplete: a person, or a group that expands into all its members. */
export interface Pick { name: string | null; address: string; group?: string; members?: ContactAddr[] }

/** Groups first, then address-book contacts, then mail-history suggestions; one row per address, at most `limit` rows. */
export function mergeSuggestions(book: ContactSuggest, history: { name: string | null; address: string }[], limit: number): Pick[] {
  const out: Pick[] = [];
  const seen = new Set<string>();
  for (const g of book.groups) out.push({ name: g.name, address: "", group: g.name, members: g.members });
  for (const c of [...book.contacts, ...history]) {
    const k = c.address.toLowerCase();
    if (seen.has(k)) continue;
    seen.add(k);
    out.push({ name: c.name, address: c.address });
  }
  return out.slice(0, limit);
}
