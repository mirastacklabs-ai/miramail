// Address book state: the Contacts view (open/closed, list, groups) and the helpers the Reader card and Compose use.
import { contactsApi } from "./contactsapi";
import { app } from "./state.svelte";
import type { Contact, ContactGroup } from "./contactstypes";
export { splitList, mailbox } from "./contactsmodel";

/** Biggest vCard file the import will read (the backend enforces the same limit). */
export const MAX_VCF_BYTES = 8 * 1024 * 1024;

class ContactsUi {
  open = $state(false);
  loading = $state(false);
  list = $state<Contact[]>([]);
  groups = $state<ContactGroup[]>([]);
  query = $state("");
  group = $state<string | null>(null);
  /** Contact being edited: "new" or an id. */
  editing = $state<"new" | number | null>(null);
  /** Pre-filled values for a new contact (e.g. from the Reader card). */
  draft = $state<{ name: string; email: string } | null>(null);
  private seq = 0;

  show(draft: { name: string; email: string } | null = null) {
    this.open = true;
    this.draft = draft;
    this.editing = draft ? "new" : null;
    void this.reload();
  }
  close() { this.open = false; this.editing = null; this.draft = null; }

  async reload() {
    const mine = ++this.seq;
    this.loading = true;
    try {
      const [list, groups] = await Promise.all([contactsApi.list(this.query, this.group), contactsApi.groups()]);
      if (mine === this.seq) { this.list = list; this.groups = groups; }
    } catch (e) { app.fail(e); }
    if (mine === this.seq) this.loading = false;
  }
}

export const contactsUi = new ContactsUi();
