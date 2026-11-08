<script lang="ts">
  import { app } from "../lib/state.svelte";
  import OrgOverlays from "./OrgOverlays.svelte";
  let okBtn = $state<HTMLButtonElement>();
  const req = $derived(app.confirmReq);
  $effect(() => { if (req) okBtn?.focus(); });
  function done(v: boolean) { const r = app.confirmReq; app.confirmReq = null; r?.resolve(v); }
  function key(e: KeyboardEvent) {
    if (!app.confirmReq) return;
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); done(false); }
    else if (e.key === "Tab") { // simple focus trap
      const btns = Array.from(document.querySelectorAll<HTMLElement>(".confirm button"));
      const i = btns.indexOf(document.activeElement as HTMLElement);
      e.preventDefault(); btns[(i + (e.shiftKey ? -1 : 1) + btns.length) % btns.length]?.focus();
    }
  }
</script>

<svelte:window onkeydowncapture={key} />

<OrgOverlays />

{#if req}
  <div class="scrim">
    <div class="confirm" role="alertdialog" aria-modal="true" aria-labelledby="cf-t" aria-describedby="cf-b">
      <h2 id="cf-t">{req.title}</h2>
      <p id="cf-b">{req.body}</p>
      {#if req.detail}<code class="mono selectable">{req.detail}</code>{/if}
      <div class="acts">
        <button class="btn ghost" onclick={() => done(false)}>Cancel</button>
        <button class="btn" class:warn={req.danger} bind:this={okBtn} onclick={() => done(true)}>{req.ok}</button>
      </div>
    </div>
  </div>
{/if}

<!-- Live regions stay mounted (screen readers only announce changes inside regions that already exist). -->
<div class="sr-only" role="status" aria-live="polite" aria-atomic="true">{app.toast && app.toast.kind !== "error" ? app.toast.msg : ""}</div>
<div class="sr-only" role="alert" aria-atomic="true">{app.toast?.kind === "error" ? app.toast.msg : ""}</div>
{#if app.toast}
  {#key app.toast.id}
    <div class="toast" class:err={app.toast.kind === "error"}>
      <span class="selectable" aria-hidden="true">{app.toast.msg}</span>
      <button class="iconbtn" aria-label="Dismiss" onclick={() => (app.toast = null)}><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg></button>
    </div>
  {/key}
{/if}

<style>
  .scrim { position: fixed; inset: 0; background: color-mix(in srgb, #000 55%, transparent); display: grid; place-items: center; z-index: 100; padding: 20px; }
  .confirm { width: 100%; max-width: 420px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); padding: 22px; display: grid; gap: 12px; }
  h2 { font-size: 15px; letter-spacing: .1em; }
  p { margin: 0; color: var(--text); font-size: 14px; }
  code { display: block; padding: 8px 10px; border: 1px solid var(--line); border-radius: var(--radius); color: var(--accent); overflow-wrap: anywhere; max-height: 120px; overflow: auto; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
  .warn { background: var(--signal); border-color: var(--signal); color: #0a0a0a; }
  .toast { position: fixed; bottom: 18px; left: 50%; transform: translateX(-50%); z-index: 120; display: flex; align-items: center; gap: 10px; max-width: min(560px, 90vw);
    background: var(--surface); color: var(--ink); border: 1px solid var(--line); border-left: 2px solid var(--accent); border-radius: var(--radius); padding: 8px 8px 8px 14px; font-size: 14px; animation: up .18s ease-out; }
  .toast.err { border-left-color: var(--signal); }
  @keyframes up { from { opacity: 0; transform: translate(-50%, 8px); } }
</style>
