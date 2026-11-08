// Wire types of the address book commands (src-tauri/src/contacts.rs). camelCase, like the Rust serde structs.
export interface Contact {
  id: number; name: string; org: string; emails: string[]; phones: string[]; notes: string; groups: string[]; updatedAt: number;
}
export interface ContactInput { id: number | null; name: string; org: string; emails: string[]; phones: string[]; notes: string; groups: string[] }
export interface ContactGroup { id: number; name: string; count: number }
export interface ContactAddr { name: string | null; address: string }
export interface ContactSuggest { contacts: ContactAddr[]; groups: { name: string; members: ContactAddr[] }[] }
export interface ContactCardInfo { contact: Contact | null; messages: number; newest: number }
export interface ContactImportResult { added: number; merged: number; skipped: number; truncated: boolean }
