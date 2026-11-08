<script lang="ts">
  import { onMount, type Snippet } from "svelte";
  import { app } from "../lib/state.svelte";
  let { title, onclose, width = 520, children, onsubmit }: { title: string; onclose: () => void; width?: number; children: Snippet; onsubmit?: () => void } = $props();
  let el = $state<HTMLDivElement>();
  const FOCUSABLE = 'a[href],button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),[tabindex]:not([tabindex="-1"])';

  onMount(() => {
    const prev = document.activeElement as HTMLElement | null;
    (el?.querySelector<HTMLElement>("[data-autofocus]") ?? el?.querySelector<HTMLElement>(FOCUSABLE))?.focus();
    return () => prev?.focus?.();
  });
  function key(e: KeyboardEvent) {
    if (app.confirmReq) return;
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); onclose(); }
    else if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && onsubmit) { e.preventDefault(); onsubmit(); }
    else if (e.key === "Tab" && el) {
      const f = Array.from(el.querySelectorAll<HTMLElement>(FOCUSABLE)).filter((x) => x.offsetParent !== null);
      if (!f.length) return;
      const i = f.indexOf(document.activeElement as HTMLElement);
      if (e.shiftKey && i <= 0) { e.preventDefault(); f[f.length - 1].focus(); }
      else if (!e.shiftKey && i === f.length - 1) { e.preventDefault(); f[0].focus(); }
    }
  }
</script>

<div class="scrim" role="presentation" onmousedown={(e) => { if (e.target === e.currentTarget) onclose(); }}>
  <div class="modal" role="dialog" aria-modal="true" aria-label={title} style="max-width:{width}px" bind:this={el} onkeydown={key} tabindex="-1">
    <header><h2>{title}</h2><button class="iconbtn" aria-label="Close" onclick={onclose}><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg></button></header>
    <div class="body">{@render children()}</div>
  </div>
</div>

<style>
  .scrim { position: fixed; inset: 0; background: color-mix(in srgb, #000 55%, transparent); display: grid; place-items: center; z-index: 90; padding: 20px; }
  .modal { width: 100%; max-height: calc(100vh - 40px); display: flex; flex-direction: column; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); outline: none; }
  header { display: flex; justify-content: space-between; align-items: center; padding: 14px 18px; border-bottom: 1px solid var(--line); }
  h2 { font-size: 0.9333rem; letter-spacing: .12em; }
  .body { overflow-y: auto; padding: 18px; display: grid; gap: 14px; min-height: 0; }
</style>
