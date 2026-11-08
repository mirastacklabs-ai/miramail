<script lang="ts">
  import { org } from "../lib/org.svelte";
  import Modal from "./Modal.svelte";
  import { fmtSize } from "../lib/util";

  // ---- text prompt ----
  let value = $state("");
  $effect(() => { if (org.prompt) value = org.prompt.value; });
  function closePrompt(v: string | null) { const p = org.prompt; org.prompt = null; p?.resolve(v); }

  // ---- source viewer ----
  const src = $derived(org.source);
  const text = $derived(src?.view ? (src.tab === "headers" ? src.view.headers : src.view.raw) : "");
</script>

{#if org.prompt}
  {@const p = org.prompt}
  <Modal title={p.title} width={420} onclose={() => closePrompt(null)} onsubmit={() => closePrompt(value)}>
    <form onsubmit={(e) => { e.preventDefault(); closePrompt(value); }} class="f">
      <label class="label" for="org-prompt">{p.label}</label>
      <input id="org-prompt" class="inp" data-autofocus bind:value maxlength="100" autocomplete="off" spellcheck="false" />
      {#if p.hint}<p class="hint">{p.hint}</p>{/if}
      <div class="acts"><button type="button" class="btn ghost" onclick={() => closePrompt(null)}>Cancel</button><button class="btn" disabled={!value.trim()}>{p.ok}</button></div>
    </form>
  </Modal>
{/if}

{#if src}
  <Modal title={src.tab === "headers" ? "Full headers" : "Raw source"} width={860} onclose={() => (org.source = null)}>
    <div class="seg" role="group" aria-label="Source view">
      <button class:on={src.tab === "headers"} aria-pressed={src.tab === "headers"} onclick={() => org.source && (org.source = { ...org.source, tab: "headers" })}>Headers</button>
      <button class:on={src.tab === "raw"} aria-pressed={src.tab === "raw"} onclick={() => org.source && (org.source = { ...org.source, tab: "raw" })}>Raw source</button>
      <span class="sub ell" title={src.title}>{src.title}</span>
    </div>
    {#if src.error}<p class="err selectable" role="alert">{src.error}</p>
    {:else if !src.view}<p class="hint">Loading…</p>
    {:else}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <pre class="src selectable" tabindex="0" aria-label={src.tab === "headers" ? "Message headers" : "Message source"}>{text}</pre>
      <p class="hint">{fmtSize(src.view.size)}{src.view.truncated && src.tab === "raw" ? ", showing the first 4 MB" : ""}. Read-only; nothing here is sent anywhere.</p>
    {/if}
  </Modal>
{/if}

{#if org.progress}
  <div class="prog" role="status" aria-live="polite">
    <span class="spin" aria-hidden="true"></span>
    <span class="mono">{org.progress.label} {org.progress.done.toLocaleString("en-US")} of {org.progress.total.toLocaleString("en-US")}{org.progress.failed ? `, ${org.progress.failed.toLocaleString("en-US")} failed` : ""}…</span>
    {#if org.progress.total > 200}<button class="lnk" onclick={() => org.cancelProgress()}>Cancel</button>{/if}
  </div>
{/if}

{#if org.undo}
  {#key org.undo.id}
    <div class="undo" role="status">
      <span class="selectable">{org.undo.msg}</span>
      <button class="btn sm" onclick={() => org.runUndo()}>Undo</button>
      <button class="iconbtn" aria-label="Dismiss" onclick={() => org.dismissUndo()}><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg></button>
    </div>
  {/key}
{/if}

<style>
  .f { display: grid; gap: 8px; }
  .inp { width: 100%; background: transparent; color: var(--ink); border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; font: 400 0.9rem var(--font-mono); }
  .inp:focus { outline: 2px solid var(--accent); border-color: transparent; }
  .hint { margin: 0; font-size: 0.8rem; color: var(--muted); line-height: 1.4; }
  .err { margin: 0; color: var(--danger); overflow-wrap: anywhere; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 4px; }
  .seg { display: flex; gap: 0; align-items: center; }
  .seg button { background: none; border: 0; border-bottom: 2px solid transparent; padding: 5px 12px; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .12em; color: var(--muted); }
  .seg button.on { color: var(--ink); border-bottom-color: var(--signal); }
  .sub { margin-left: auto; color: var(--muted); font-size: 0.8rem; max-width: 50%; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .src { margin: 0; padding: 10px 12px; max-height: 60vh; overflow: auto; border: 1px solid var(--line); border-radius: var(--radius); font: 400 0.75rem/1.45 var(--font-mono); white-space: pre-wrap; overflow-wrap: anywhere; color: var(--text); background: var(--bg); }
  .prog { position: fixed; bottom: 110px; left: 50%; transform: translateX(-50%); z-index: 119; display: flex; align-items: center; gap: 10px; background: var(--surface); border: 1px solid var(--line); border-left: 2px solid var(--accent); border-radius: var(--radius); padding: 8px 14px; font-size: 0.8rem; }
  .spin { width: 12px; height: 12px; border: 2px solid var(--line); border-top-color: var(--accent); border-radius: 50%; animation: rot .8s linear infinite; }
  @keyframes rot { to { transform: rotate(360deg); } }
  .lnk { background: none; border: 0; color: var(--accent); cursor: pointer; font: inherit; text-decoration: underline; }
  .undo { position: fixed; bottom: 64px; left: 50%; transform: translateX(-50%); z-index: 121; display: flex; align-items: center; gap: 10px; max-width: min(620px, 92vw); background: var(--surface); color: var(--ink); border: 1px solid var(--line); border-left: 2px solid var(--accent); border-radius: var(--radius); padding: 8px 8px 8px 14px; font-size: 14px; }
</style>
