<script lang="ts">
  import { onDestroy } from "svelte";
  import { api } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { orgApi } from "../lib/orgapi";
  import { phone } from "../lib/phone.svelte";
  import { fmtSize } from "../lib/util";
  import { previewKind, openable } from "../lib/attach";
  import type { Attachment, MessageBody } from "../lib/types";

  let { b }: { b: MessageBody } = $props();
  let shown = $state<{ id: string; kind: "image" | "pdf"; url: string } | null>(null);
  let busy = $state<string | null>(null);

  function drop() { if (shown?.url.startsWith("blob:")) URL.revokeObjectURL(shown.url); shown = null; }
  $effect(() => { void b.summary.uid; drop(); });
  onDestroy(drop);

  const NA = "Saving and opening attachments isn't available on iPhone yet. Image attachments can be previewed here.";
  async function save(a: Attachment) {
    if (phone.on) { app.notify(NA, "error"); return; }
    try { app.notify(`Saved to ${await api.saveAttachmentToDownloads(app.account!, b.summary.folder, b.summary.uid, a.id)}`); } catch (e) { app.fail(e); }
  }
  async function open(a: Attachment) {
    if (phone.on) { app.notify(NA, "error"); return; }
    busy = a.id;
    try { await orgApi.openAttachment(b.summary.account, b.summary.folder, b.summary.uid, a.id); } catch (e) { app.fail(e); }
    busy = null;
  }
  async function preview(a: Attachment) {
    if (shown?.id === a.id) { drop(); return; }
    const kind = previewKind(a.mime, a.size);
    if (!kind) return;
    // The inline PDF frame stays blank in the sandboxed App Store build (confirmed in TestFlight), so a PDF preview opens it in the system viewer, like Open.
    if (kind.kind === "pdf") { await open(a); return; }
    busy = a.id;
    try {
      const d = await api.getAttachment(b.summary.account, b.summary.folder, b.summary.uid, a.id);
      drop();
      if (kind.kind === "image") shown = { id: a.id, kind: "image", url: `data:${kind.mime};base64,${d.dataBase64.replace(/[^A-Za-z0-9+/=]/g, "")}` };
      else {
        const bin = atob(d.dataBase64);
        if (!bin.startsWith("%PDF-")) throw "That file doesn't look like a PDF, so it won't be previewed. Save it instead.";
        const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
        shown = { id: a.id, kind: "pdf", url: URL.createObjectURL(new Blob([bytes], { type: "application/pdf" })) };
      }
    } catch (e) { app.fail(e); }
    busy = null;
  }
</script>

<div class="atts">
  <div class="label">Attachments</div>
  <ul>
    {#each b.attachments as a (a.id)}
      <li>
        <div class="att">
          <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><path d="M21 12.500l-8.500 8.500a5 5 0 01-7-7L14 5.500a3.300 3.300 0 014.700 4.700L10 19a1.700 1.700 0 01-2.400-2.400l7.600-7.600"/></svg>
          <span class="ell" title={a.name}>{a.name}</span><span class="mono sz">{fmtSize(a.size)}</span>
          <span class="btns">
            {#if previewKind(a.mime, a.size)}<button class="btn ghost sm" disabled={busy === a.id} onclick={() => preview(a)} aria-expanded={shown?.id === a.id}>{shown?.id === a.id ? "Hide" : "Preview"}</button>{/if}
            {#if openable(a.name)}<button class="btn ghost sm" disabled={busy === a.id} onclick={() => open(a)} title="Open with the default app">Open</button>{/if}
            <button class="btn ghost sm" onclick={() => save(a)} aria-label="Save {a.name} to Downloads">Save</button>
          </span>
        </div>
        {#if shown?.id === a.id}
          <div class="prev">
            {#if shown.kind === "image"}<img src={shown.url} alt="Preview of {a.name}" />
            {:else}<iframe title="Preview of {a.name}" sandbox="" src={shown.url}></iframe><p class="hint">If the preview is blank, use Open or Save.</p>{/if}
          </div>
        {/if}
      </li>
    {/each}
  </ul>
</div>

<style>
  ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 6px; grid-template-columns: minmax(0, 1fr); }
  li { min-width: 0; }
  .label { margin-bottom: 8px; }
  .att { display: flex; align-items: center; gap: 10px; border: 1px solid var(--line); background: var(--surface); border-radius: var(--radius); padding: 5px 8px 5px 12px; color: var(--text); max-width: 560px; }
  .sz { color: var(--muted); margin-left: auto; flex: none; }
  .btns { display: inline-flex; gap: 4px; flex: none; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .prev { margin-top: 6px; max-width: 560px; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; background: #fff; }
  .prev img { display: block; max-width: 100%; max-height: 70vh; margin: 0 auto; }
  .prev iframe { display: block; width: 100%; height: 60vh; border: 0; background: #fff; }
  .hint { margin: 0; padding: 4px 8px; font-size: 0.75rem; color: var(--muted); background: var(--surface); }
</style>
