<script lang="ts">
  // Preferences > Signatures: named signatures (plain or rich), per-account defaults, and From identities.
  import { onMount } from "svelte";
  import { api, errMsg } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import Select from "./Select.svelte";
  import { composer } from "../lib/composer.svelte";
  import { htmlToText, importSignatureText, sigsForAccount, textToHtml } from "../lib/composetext";
  import type { Identity, Signature } from "../lib/types";
  import RichEditor from "./compose/RichEditor.svelte";

  const blank = (): Signature => ({ id: "", name: "", rich: false, text: "", html: "", placement: "below", accounts: [] });
  let sel = $state<string | null>(null); // saved id, "new", or null
  let draft = $state<Signature>(blank());
  let busy = $state(false);
  let importText = $state("");
  let idDraft = $state<Identity | null>(null);

  onMount(() => { void composer.ensure().then(() => composer.reload()).catch((e) => app.fail(e)); });

  function select(s: Signature) { draft = { ...s, accounts: [...s.accounts] }; sel = s.id; importText = ""; }
  function addNew() { draft = blank(); sel = "new"; importText = ""; }
  function setRich(rich: boolean) {
    if (rich === draft.rich) return;
    if (rich) draft.html = textToHtml(draft.text); else draft.text = htmlToText(draft.html);
    draft.rich = rich;
  }
  function doImport() {
    const r = importSignatureText(importText);
    draft.text = r.text; draft.html = draft.rich ? r.html : draft.html;
    if (!draft.rich) draft.html = "";
    importText = "";
  }
  function toggleAccount(email: string, on: boolean) {
    draft.accounts = on ? [...draft.accounts, email] : draft.accounts.filter((a) => a !== email);
  }
  async function save() {
    busy = true;
    try {
      const s: Signature = { ...$state.snapshot(draft), text: draft.rich ? htmlToText(draft.html) : draft.text };
      const saved = await api.saveSignature(s);
      await composer.reload();
      select(composer.signatures.find((x) => x.id === saved.id) ?? saved);
      app.notify("Signature saved");
    } catch (e) { app.notify(errMsg(e), "error"); }
    busy = false;
  }
  async function remove() {
    if (sel == null || sel === "new") { sel = null; return; }
    const ok = await app.ask({ title: "Delete this signature?", body: "Accounts and identities that use it will have no signature.", ok: "Delete", danger: true });
    if (!ok) return;
    try { await api.deleteSignature(sel); await composer.reload(); sel = null; } catch (e) { app.notify(errMsg(e), "error"); }
  }

  const defOf = (acc: string) => composer.defaults.find((d) => d.account === acc) ?? { account: acc, newId: null, replyId: null };
  async function setDefault(acc: string, which: "newId" | "replyId", v: string) {
    try {
      await api.setSignatureDefaults({ ...defOf(acc), [which]: v || null });
      await composer.reload();
    } catch (e) { app.notify(errMsg(e), "error"); }
  }

  const idsOf = (acc: string) => composer.identities.filter((i) => i.account === acc);
  const newIdentity = (acc: string): Identity => ({ id: "", account: acc, name: "", address: "", replyTo: "", signatureId: null });
  async function saveIdentity() {
    if (!idDraft) return;
    try {
      await api.saveIdentity({ ...$state.snapshot(idDraft), signatureId: idDraft.signatureId || null });
      await composer.reload();
      idDraft = null;
      app.notify("Identity saved");
    } catch (e) { app.notify(errMsg(e), "error"); }
  }
  async function removeIdentity(i: Identity) {
    const ok = await app.ask({ title: "Delete this identity?", body: `${i.address} will no longer be offered as a sender.`, ok: "Delete", danger: true });
    if (!ok) return;
    try { await api.deleteIdentity(i.id); await composer.reload(); if (idDraft?.id === i.id) idDraft = null; } catch (e) { app.notify(errMsg(e), "error"); }
  }
</script>

<h3>Signatures</h3>
<p class="note">Signatures are stored on this Mac. Plain-text signatures are added with the standard “-- ” separator line.</p>

<div class="sigs">
  <ul class="list" aria-label="Signatures">
    {#each composer.signatures as s (s.id)}
      <li><button type="button" class:on={sel === s.id} onclick={() => select(s)}>{s.name}<span class="dim">{s.rich ? "rich" : "plain"}</span></button></li>
    {/each}
    <li><button type="button" class="add" class:on={sel === "new"} onclick={addNew}>+ New signature</button></li>
  </ul>

  {#if sel != null}
    <div class="edit">
      <label class="f">Name<input bind:value={draft.name} placeholder="Work, Personal, …" autocomplete="off" /></label>
      <div class="f">Format
        <div class="seg" role="group" aria-label="Signature format">
          <button type="button" class:on={!draft.rich} aria-pressed={!draft.rich} onclick={() => setRich(false)}>Plain</button>
          <button type="button" class:on={draft.rich} aria-pressed={draft.rich} onclick={() => setRich(true)}>Rich</button>
        </div>
      </div>
      <div class="body">
        {#if draft.rich}
          {#key sel + String(draft.rich)}
            <RichEditor html={draft.html} oninput={(h) => (draft.html = h)} onerror={(m) => app.notify(m, "error")} label="Signature" />
          {/key}
        {:else}
          <textarea bind:value={draft.text} rows="6" aria-label="Signature text" spellcheck="true"></textarea>
        {/if}
      </div>
      <details class="imp">
        <summary>Import from existing text</summary>
        <textarea bind:value={importText} rows="4" placeholder="Paste a signature from another mail app" aria-label="Signature text to import"></textarea>
        <button type="button" class="btn ghost sm" onclick={doImport} disabled={!importText.trim()}>Use this text</button>
      </details>
      <div class="f"><span id="sg-place-l">Placement in replies and forwards</span>
        <Select value={draft.placement} labelledby="sg-place-l" options={[{ value: "below", label: "Below the quoted text" }, { value: "above", label: "Above the quoted text" }]} onchange={(v) => { if (draft) draft.placement = v as typeof draft.placement; }} />
      </div>
      <fieldset class="accts"><legend>Available for</legend>
        {#each app.accounts as a (a.email)}
          <label class="chk"><input type="checkbox" checked={draft.accounts.includes(a.email)} onchange={(e) => toggleAccount(a.email, e.currentTarget.checked)} />{a.email}</label>
        {/each}
        <span class="dim">{draft.accounts.length ? "Only the ticked accounts." : "None ticked: every account."}</span>
      </fieldset>
      <div class="acts">
        <button type="button" class="btn" onclick={save} disabled={busy || !draft.name.trim()}>Save</button>
        <button type="button" class="btn danger" onclick={remove} disabled={busy}>{sel === "new" ? "Discard" : "Delete"}</button>
      </div>
    </div>
  {/if}
</div>

<h3 class="gap">Defaults</h3>
<p class="note">Which signature a new message starts with, per account. Replies and forwards can use a different one.</p>
{#each app.accounts as a (a.email)}
  <div class="row"><span class="mono acc">{a.email}</span>
    <div class="sdef"><span id="sg-n-{a.email}">New mail</span>
      <Select value={defOf(a.email).newId ?? ""} labelledby="sg-n-{a.email}" block={false} options={[{ value: "", label: "None" }, ...sigsForAccount(composer.signatures, a.email).map((s) => ({ value: s.id, label: s.name }))]} onchange={(v) => setDefault(a.email, "newId", v)} /></div>
    <div class="sdef"><span id="sg-r-{a.email}">Replies / forwards</span>
      <Select value={defOf(a.email).replyId ?? ""} labelledby="sg-r-{a.email}" block={false} options={[{ value: "", label: "None" }, ...sigsForAccount(composer.signatures, a.email).map((s) => ({ value: s.id, label: s.name }))]} onchange={(v) => setDefault(a.email, "replyId", v)} /></div>
  </div>
{/each}

<h3 class="gap">Identities</h3>
<p class="note">Extra “From” addresses for an account (aliases). They send through the same account and server, so only add addresses your provider lets this account send as. The account's own address is always available.</p>
{#each app.accounts as a (a.email)}
  <div class="idblock">
    <div class="row"><span class="mono acc">{a.email}</span><button type="button" class="btn ghost sm" onclick={() => (idDraft = newIdentity(a.email))}>Add identity</button></div>
    {#each idsOf(a.email) as i (i.id)}
      <div class="row sub"><span>{i.name ? `${i.name} <${i.address}>` : i.address}{i.replyTo ? `, replies to ${i.replyTo}` : ""}</span>
        <span class="grow"></span>
        <button type="button" class="btn ghost sm" onclick={() => (idDraft = { ...i })}>Edit</button>
        <button type="button" class="btn danger sm" onclick={() => removeIdentity(i)}>Delete</button></div>
    {/each}
    {#if idDraft && idDraft.account === a.email}
      <form class="idform" onsubmit={(e) => { e.preventDefault(); void saveIdentity(); }}>
        <label class="f">Name<input bind:value={idDraft.name} autocomplete="off" /></label>
        <label class="f">Address<input bind:value={idDraft.address} autocomplete="off" spellcheck="false" placeholder="alias@example.org" /></label>
        <label class="f">Reply-to (optional)<input bind:value={idDraft.replyTo} autocomplete="off" spellcheck="false" /></label>
        <div class="f"><span id="sg-id-l">Signature</span>
          <Select value={idDraft.signatureId ?? ""} labelledby="sg-id-l" options={[{ value: "", label: "Account default" }, ...sigsForAccount(composer.signatures, a.email).map((s) => ({ value: s.id, label: s.name }))]} onchange={(v) => { if (idDraft) idDraft.signatureId = v || null; }} /></div>
        <div class="acts"><button type="submit" class="btn" disabled={!idDraft.address.trim()}>Save identity</button>
          <button type="button" class="btn ghost" onclick={() => (idDraft = null)}>Cancel</button></div>
      </form>
    {/if}
  </div>
{/each}

<style>
  .note { color: var(--muted); font-size: 0.8667rem; margin: 6px 0 12px; }
  .gap { margin-top: 26px; }
  .sigs { display: grid; grid-template-columns: 180px 1fr; gap: 16px; }
  .list { list-style: none; margin: 0; padding: 0; display: grid; gap: 2px; align-content: start; }
  .list button { width: 100%; text-align: left; background: none; border: 1px solid transparent; border-radius: var(--radius); color: var(--text); padding: 6px 8px; display: flex; justify-content: space-between; gap: 8px; }
  .list button.on { border-color: var(--signal); background: color-mix(in srgb, var(--line) 45%, transparent); }
  .list .add { color: var(--accent); }
  .dim { color: var(--muted); font-size: 0.75rem; }
  .edit { display: grid; gap: 10px; min-width: 0; }
  .f { display: grid; gap: 4px; font-size: 0.8rem; color: var(--muted); }
  input:not([type="checkbox"]), textarea { background: var(--surface); color: var(--ink); border: 1px solid var(--line); border-radius: var(--radius); padding: 6px 8px; font: 400 13px var(--font-mono); min-width: 0; }
  textarea { width: 100%; box-sizing: border-box; resize: vertical; }
  .body { border: 1px solid var(--line); border-radius: var(--radius); min-height: 150px; display: flex; flex-direction: column; }
  .body textarea { border: 0; flex: 1; }
  .seg { display: inline-flex; width: max-content; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  .seg button { background: none; border: 0; padding: 4px 12px; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .1em; color: var(--muted); border-bottom: 2px solid transparent; }
  .seg button.on { color: var(--ink); border-bottom-color: var(--signal); }
  .accts { border: 1px solid var(--line); border-radius: var(--radius); display: grid; gap: 4px; padding: 8px 10px; font-size: 0.8667rem; }
  .accts legend { color: var(--muted); font-size: 0.8rem; padding: 0 4px; }
  .chk { display: flex; gap: 8px; align-items: center; }
  .acts { display: flex; gap: 8px; }
  .imp summary { cursor: pointer; font-size: 0.8rem; color: var(--muted); margin-bottom: 6px; }
  .imp { display: grid; gap: 6px; }
  .row { display: flex; align-items: center; gap: 14px; flex-wrap: wrap; padding: 6px 0; font-size: 0.8667rem; }
  .sdef { display: flex; align-items: center; gap: 6px; color: var(--muted); }
  .row.sub { padding-left: 14px; border-left: 2px solid var(--line); }
  .acc { min-width: 200px; color: var(--ink); }
  .grow { flex: 1; }
  .idform { display: grid; gap: 8px; padding: 10px; border: 1px solid var(--line); border-radius: var(--radius); margin: 6px 0 10px 14px; }
  @media (max-width: 640px) { .sigs { grid-template-columns: 1fr; } }
</style>
