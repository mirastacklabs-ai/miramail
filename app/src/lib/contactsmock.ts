// In-memory stand-in for the address book commands (plain-browser dev only).
import type { Contact, ContactInput, ContactSuggest } from "./contactstypes";

const wait = <T,>(v: T, ms = 60) => new Promise<T>((r) => setTimeout(() => r(structuredClone(v)), ms));
let seq = 3;
let book: Contact[] = [
  { id: 1, name: "Ada Lovelace", org: "Analytical Engines", emails: ["ada@example.org"], phones: ["+44 20 7946 0000"], notes: "", groups: ["Friends"], updatedAt: 1 },
  { id: 2, name: "Grace Hopper", org: "", emails: ["grace@navy.example"], phones: [], notes: "", groups: ["Friends", "Work"], updatedAt: 1 },
];
const first = (c: Contact) => ({ name: c.name || null, address: c.emails[0] });
const groupNames = () => [...new Set(book.flatMap((c) => c.groups))].sort();

export const contactsMock = {
  contacts_list: (a: { query: string; group: string | null }) => {
    const q = (a.query ?? "").toLowerCase();
    return wait(book.filter((c) => (!a.group || c.groups.includes(a.group)) && (!q || [c.name, c.org, c.notes, ...c.emails, ...c.phones].join(" ").toLowerCase().includes(q))).sort((x, y) => x.name.localeCompare(y.name)));
  },
  async contacts_save(a: { contact: ContactInput }) {
    const i = a.contact;
    if (!i.name.trim() && !i.emails.length) throw "Give the contact a name or an e-mail address.";
    for (const e of i.emails) if (!/^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/.test(e)) throw `"${e}" is not a valid e-mail address.`;
    const clash = book.find((c) => c.id !== i.id && c.emails.some((e) => i.emails.map((x) => x.toLowerCase()).includes(e)));
    if (clash) throw `That address already belongs to "${clash.name}".`;
    const c: Contact = { id: i.id ?? seq++, name: i.name.trim(), org: i.org, emails: i.emails.map((e) => e.trim().toLowerCase()), phones: i.phones, notes: i.notes, groups: i.groups, updatedAt: Date.now() / 1000 };
    book = i.id ? book.map((x) => (x.id === i.id ? c : x)) : [...book, c];
    return wait(c);
  },
  async contacts_delete(a: { id: number }) { book = book.filter((c) => c.id !== a.id); return wait(undefined); },
  contacts_groups: () => wait(groupNames().map((name, id) => ({ id: id + 1, name, count: book.filter((c) => c.groups.includes(name)).length }))),
  async contacts_group_rename(a: { id: number; name: string }) { const old = groupNames()[a.id - 1]; book = book.map((c) => ({ ...c, groups: c.groups.map((g) => (g === old ? a.name : g)) })); return wait(undefined); },
  async contacts_group_delete(a: { id: number }) { const old = groupNames()[a.id - 1]; book = book.map((c) => ({ ...c, groups: c.groups.filter((g) => g !== old) })); return wait(undefined); },
  contacts_expand_group: (a: { name: string }) => wait(book.filter((c) => c.groups.includes(a.name) && c.emails.length).map(first)),
  contacts_suggest: (a: { prefix: string }): Promise<ContactSuggest> => {
    const q = a.prefix.trim().toLowerCase();
    if (!q) return wait({ contacts: [], groups: [] });
    return wait({
      contacts: book.filter((c) => c.emails.length && `${c.name} ${c.emails.join(" ")}`.toLowerCase().includes(q)).map(first),
      groups: groupNames().filter((g) => g.toLowerCase().includes(q)).map((name) => ({ name, members: book.filter((c) => c.groups.includes(name) && c.emails.length).map(first) })),
    });
  },
  contacts_card: (a: { address: string }) => wait({ contact: book.find((c) => c.emails.includes(a.address.toLowerCase())) ?? null, messages: 4, newest: Math.floor(Date.now() / 1000) - 86400 }),
  contacts_from_mail: () => wait([{ name: "Newsletter", address: "news@shop.example" }, { name: "Bob", address: "bob@friend.example" }].filter((x) => !book.some((c) => c.emails.includes(x.address)))),
  async contacts_add_many(a: { entries: { name: string | null; address: string }[] }) {
    let n = 0;
    for (const e of a.entries) if (!book.some((c) => c.emails.includes(e.address))) { book.push({ id: seq++, name: e.name ?? "", org: "", emails: [e.address], phones: [], notes: "", groups: [], updatedAt: 1 }); n++; }
    return wait(n);
  },
  async contacts_import(a: { text: string }) {
    const n = (a.text.match(/BEGIN:VCARD/gi) ?? []).length;
    return wait({ added: n, merged: 0, skipped: 0, truncated: false });
  },
  contacts_export: () => wait("/Users/you/Downloads/contacts.vcf"),
};
