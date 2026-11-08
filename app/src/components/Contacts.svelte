<script lang="ts">
  // The address book: local contacts and groups (Preferences-free, reachable from the sidebar). Nothing here talks to
  // a server. vCard files are read through the file picker and parsed by the backend; export writes a NEW file in
  // Downloads (never overwrites).
  import Modal from "./Modal.svelte";
  import Select from "./Select.svelte";
  import ComboInput from "./ComboInput.svelte";
  import { contactsApi } from "../lib/contactsapi";
  import { contactsUi, MAX_VCF_BYTES, splitList } from "../lib/contacts.svelte";
  import { errMsg } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { workspace } from "../lib/workspace.svelte";
  import type { Contact, ContactAddr } from "../lib/contactstypes";

  let name = $state(""), org = $state(""), emails = $state(""), phones = $state(""), notes = $state(""), groups = $state("");
  let saving = $state(false);
  let suggestions = $state<ContactAddr[] | null>(null), picked = $state<Record<string, boolean>>({});
  let fileEl = $state<HTMLInputElement>();
  let timer: ReturnType<typeof setTimeout> | undefined;

  const current = $derived(typeof contactsUi.editing === "number" ? contactsUi.list.find((c) => c.id === contactsUi.editing) ?? null : null);
  const groupOpts = $derived([{ value: "", label: "All contacts" }, ...contactsUi.groups.map((g) => ({ value: g.name, label: `${g.name} (${g.count})` }))]);

  $effect(() => {
    // Fill the form when the selection changes (or a new contact is started, possibly pre-filled).
    const e = contactsUi.editing;
    if (e === null) return;
    const c = current, d = contactsUi.draft;
    name = c?.name ?? d?.name ?? ""; org = c?.org ?? ""; emails = c ? c.emails.join(", ") : d?.email ?? "";
    phones = c ? c.phones.join(", ") : ""; notes = c?.notes ?? ""; groups = c ? c.groups.join(", ") : "";
  });

  function search(v: string) { contactsUi.query = v; clearTimeout(timer); timer = setTimeout(() => void contactsUi.reload(), 150); }
  function pickGroup(v: string) { contactsUi.group = v || null; void contactsUi.reload(); }

  async function save() {
    if (saving) return;
    saving = true;
    try {
      const c = await contactsApi.save({ id: typeof contactsUi.editing === "number" ? contactsUi.editing : null, name, org, emails: splitList(emails), phones: splitList(phones), notes, groups: splitList(groups) });
      contactsUi.draft = null;
      contactsUi.editing = c.id;
      await contactsUi.reload();
      app.notify("Contact saved");
    } catch (e) { app.notify(errMsg(e), "error"); }
    saving = false;
  }
  async function remove(c: Contact) {
    const ok = await app.ask({ title: `Delete ${c.name || c.emails[0]}?`, body: "The contact is removed from this Mac's address book. Mail from them is not touched.", ok: "Delete", danger: true });
    if (!ok) return;
    try { await contactsApi.remove(c.id); contactsUi.editing = null; await contactsUi.reload(); } catch (e) { app.fail(e); }
  }
  function composeTo(c: Contact) {
    if (!c.emails[0]) return;
    contactsUi.close();
    workspace.openCompose({ to: [c.name ? `${c.name.replace(/[",<>]/g, " ").trim()} <${c.emails[0]}>` : c.emails[0]], cc: [], bcc: [], subject: "", text: "", inReplyTo: null });
  }
  async function renameGroup() {
    const g = contactsUi.groups.find((x) => x.name === contactsUi.group);
    if (!g) return;
    const n = window.prompt("New group name", g.name)?.trim();
    if (!n || n === g.name) return;
    try { await contactsApi.groupRename(g.id, n); contactsUi.group = n; await contactsUi.reload(); } catch (e) { app.notify(errMsg(e), "error"); }
  }
  async function deleteGroup() {
    const g = contactsUi.groups.find((x) => x.name === contactsUi.group);
    if (!g) return;
    const ok = await app.ask({ title: `Delete the group "${g.name}"?`, body: "Only the group is removed. Its contacts stay in the address book.", ok: "Delete group", danger: true });
    if (!ok) return;
    try { await contactsApi.groupDelete(g.id); contactsUi.group = null; await contactsUi.reload(); } catch (e) { app.fail(e); }
  }

  async function importFile(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const f = input.files?.[0];
    input.value = "";
    if (!f) return;
    if (f.size > MAX_VCF_BYTES) { app.notify("That file is too large to import (limit 8 MB).", "error"); return; }
    try {
      const r = await contactsApi.importVcf(await f.text());
      app.notify(`Imported: ${r.added} added, ${r.merged} updated, ${r.skipped} skipped${r.truncated ? ". The file was cut at 8 MB" : ""}.`);
      await contactsUi.reload();
    } catch (err) { app.notify(errMsg(err), "error"); }
  }
  async function exportFile() {
    try { app.notify(`Saved ${await contactsApi.exportVcf(contactsUi.group)}`); } catch (e) { app.notify(errMsg(e), "error"); }
  }

  async function findInMail() {
    try { suggestions = await contactsApi.fromMail(40); picked = Object.fromEntries(suggestions.map((s) => [s.address, true])); } catch (e) { app.fail(e); }
  }
  async function addPicked() {
    const sel = (suggestions ?? []).filter((s) => picked[s.address]);
    try {
      const n = await contactsApi.addMany(sel);
      app.notify(`${n} contact${n === 1 ? "" : "s"} added`);
      suggestions = null;
      await contactsUi.reload();
    } catch (e) { app.fail(e); }
  }
</script>

<Modal title="Contacts" width={880} onclose={() => contactsUi.close()}>
  <div class="cols">
    <section class="left" aria-label="Contact list">
      <input type="search" placeholder="Search contacts" aria-label="Search contacts" value={contactsUi.query} oninput={(e) => search(e.currentTarget.value)} data-autofocus />
      <Select value={contactsUi.group ?? ""} options={groupOpts} ariaLabel="Show group" onchange={pickGroup} />
      {#if contactsUi.group}
        <div class="rw"><button class="btn ghost sm" onclick={renameGroup}>Rename group</button><button class="btn ghost sm" onclick={deleteGroup}>Delete group</button></div>
      {/if}
      <ul class="list" aria-label="Contacts">
        {#each contactsUi.list as c (c.id)}
          <li><button class="row" class:on={contactsUi.editing === c.id} onclick={() => { contactsUi.draft = null; contactsUi.editing = c.id; }}>
            <span class="nm">{c.name || c.emails[0] || "(no name)"}</span>
            {#if c.name && c.emails[0]}<span class="ad mono">{c.emails[0]}</span>{/if}
          </button></li>
        {:else}
          <li class="empty">{contactsUi.loading ? "Loading…" : contactsUi.query || contactsUi.group ? "No matching contacts." : "No contacts yet. Add one, import a .vcf file, or pick people from your mail."}</li>
        {/each}
      </ul>
      <div class="rw">
        <button class="btn sm" onclick={() => { contactsUi.draft = null; contactsUi.editing = "new"; }}>New contact</button>
        <button class="btn ghost sm" onclick={findInMail}>From my mail…</button>
      </div>
      <div class="rw">
        <button class="btn ghost sm" onclick={() => fileEl?.click()}>Import .vcf…</button>
        <button class="btn ghost sm" onclick={exportFile}>Export{contactsUi.group ? " group" : ""}</button>
        <input bind:this={fileEl} type="file" accept=".vcf,.vcard,text/vcard,text/x-vcard" hidden onchange={importFile} />
      </div>
    </section>

    <section class="right" aria-label="Contact details">
      {#if suggestions}
        <h3>People you correspond with</h3>
        <p class="help">From mail already on this Mac. Tick the ones to add to your address book.</p>
        {#if suggestions.length === 0}<p class="help">Everyone from your recent mail is already saved.</p>{/if}
        <ul class="sug">
          {#each suggestions as s (s.address)}
            <li><label class="chk"><input type="checkbox" bind:checked={picked[s.address]} /> <span>{s.name || s.address}</span> <span class="ad mono">{s.name ? s.address : ""}</span></label></li>
          {/each}
        </ul>
        <div class="rw"><button class="btn sm" disabled={!suggestions.some((s) => picked[s.address])} onclick={addPicked}>Add selected</button><button class="btn ghost sm" onclick={() => (suggestions = null)}>Cancel</button></div>
      {:else if contactsUi.editing === null}
        <p class="help">Select a contact, or add a new one. Contacts live only on this Mac. Groups work in Compose: type the group name in To or Cc and pick it to fill in all its members.</p>
      {:else}
        <form class="ed" onsubmit={(e) => { e.preventDefault(); void save(); }}>
          <div class="field"><label class="label" for="ct-n">Name</label><input id="ct-n" bind:value={name} maxlength="200" /></div>
          <div class="field"><label class="label" for="ct-e">E-mail addresses (separate with commas)</label><input id="ct-e" bind:value={emails} placeholder="name@example.com" /></div>
          <div class="field"><label class="label" for="ct-p">Phone numbers</label><input id="ct-p" bind:value={phones} /></div>
          <div class="field"><label class="label" for="ct-o">Organisation</label><input id="ct-o" bind:value={org} maxlength="200" /></div>
          <div class="field"><label class="label" for="ct-g">Groups (separate with commas)</label><ComboInput id="ct-g" bind:value={groups} placeholder="Family, Work" options={contactsUi.groups.map((g) => g.name)} /></div>
          <div class="field"><label class="label" for="ct-t">Notes</label><textarea id="ct-t" bind:value={notes} rows="3" maxlength="5000"></textarea></div>
          <div class="rw">
            <button type="submit" class="btn sm" disabled={saving}>{saving ? "Saving…" : "Save contact"}</button>
            {#if current}
              <button type="button" class="btn ghost sm" disabled={!current.emails[0]} onclick={() => composeTo(current)}>Compose to</button>
              <button type="button" class="btn ghost sm" onclick={() => remove(current)}>Delete</button>
            {/if}
          </div>
        </form>
      {/if}
    </section>
  </div>
</Modal>

<style>
  .cols { display: grid; grid-template-columns: minmax(220px, 300px) minmax(0, 1fr); gap: 18px; min-height: 360px; }
  .left, .right { display: grid; gap: 10px; align-content: start; min-width: 0; }
  .list, .sug { list-style: none; margin: 0; padding: 0; display: grid; gap: 2px; max-height: 300px; overflow: auto; border: 1px solid var(--line); border-radius: var(--radius); }
  .row { display: grid; gap: 1px; width: 100%; text-align: left; background: none; border: 0; padding: 6px 10px; color: var(--text); }
  .row:hover, .row.on { background: color-mix(in srgb, var(--accent) 16%, transparent); }
  .nm { color: var(--ink); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ad { color: var(--muted); font-size: 0.75rem; overflow: hidden; text-overflow: ellipsis; }
  .empty { padding: 10px; color: var(--muted); font-size: 0.8667rem; line-height: 1.5; }
  .rw { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; }
  .help { margin: 0; color: var(--muted); font-size: 0.8667rem; line-height: 1.5; }
  .ed { display: grid; gap: 10px; }
  .chk { display: flex; gap: 8px; align-items: baseline; padding: 4px 8px; font-size: 0.8667rem; }
  input[type="checkbox"] { accent-color: var(--accent); }
  h3 { font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); margin: 0; }
  @media (max-width: 640px) { .cols { grid-template-columns: 1fr; } }
</style>
