<script lang="ts">
  // Bottom sheet for the phone compose screen (Format, Send later, Tone, More). Sits inside a positioned parent;
  // dismissed by the scrim, the close button or Escape.
  import type { Snippet } from "svelte";
  import { PHONE_ICONS } from "../../lib/phoneicons";
  import { sheetTapIgnored } from "../../lib/phonemodel";
  let { title, onclose, children, tall = false }: { title: string; onclose: () => void; children: Snippet; tall?: boolean } = $props();
  // Keep taps in the sheet from moving the editor's selection.
  // A long press opens the sheet under the finger; the click iOS sends on lift must hit nothing (scrim or item).
  const openedAt = Date.now();
  const swallow = (e: Event) => { if (sheetTapIgnored(openedAt, Date.now())) { e.stopPropagation(); e.preventDefault(); return true; } return false; };
  const closeScrim = (e: Event) => { if (!swallow(e)) onclose(); };
  const keep = (e: Event) => { if (!(e.target instanceof HTMLElement && e.target.closest("input,textarea,select"))) e.preventDefault(); };
</script>
<svelte:window onkeydown={(e) => e.key === "Escape" && onclose()} />
<!-- Close on click, not pointerdown: the click that follows a pointerdown-close would land on whatever is under the finger (Send, Cancel, a chip x).
     pointerdown only keeps the editor focused. -->
<div class="scrim" role="presentation" onpointerdown={(e) => e.preventDefault()} onclick={closeScrim}></div>
<!-- Not aria-modal: moving focus into the sheet would dismiss the keyboard and the editor selection the sheet acts on. -->
<div class="sheet" class:tall role="dialog" aria-label={title} tabindex="-1" onpointerdown={keep} onclickcapture={swallow}>
  <header><h2>{title}</h2><button type="button" class="x" aria-label="Close" onclick={onclose}><svg viewBox="0 0 24 24" aria-hidden="true"><path d={PHONE_ICONS.close} /></svg></button></header>
  <div class="in">{@render children()}</div>
</div>
<style>
  .scrim { position: absolute; inset: 0; z-index: 60; background: color-mix(in srgb, #000 45%, transparent); }
  .sheet { position: absolute; left: 0; right: 0; bottom: 0; z-index: 61; max-height: 72%; display: flex; flex-direction: column; background: var(--surface); border-top: 1px solid var(--line); border-radius: 14px 14px 0 0; }
  .sheet.tall { max-height: 88%; }
  header { flex: none; display: flex; align-items: center; justify-content: space-between; padding: 0 4px 0 16px; min-height: 48px; border-bottom: 1px solid var(--line); }
  h2 { margin: 0; font-size: 0.95rem; letter-spacing: .1em; }
  .x { width: 44px; height: 44px; background: none; border: 0; color: var(--muted); display: grid; place-items: center; }
  .x svg { width: 22px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .in { overflow-y: auto; -webkit-overflow-scrolling: touch; overscroll-behavior: contain; padding: 8px 12px calc(env(safe-area-inset-bottom) + 12px); }
</style>
