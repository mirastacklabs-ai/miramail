<script lang="ts">
  import { layout } from "../lib/layout.svelte";
  let { which, label }: { which: "sidebar" | "list"; label: string } = $props();
  let startX = 0, startW = 0;

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    startX = e.clientX; startW = layout[which];
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    layout.dragging = true;
    document.body.classList.add("resizing");
  }
  function move(e: PointerEvent) {
    if (!layout.dragging) return;
    layout.set(which, startW + (e.clientX - startX));
  }
  function up(e: PointerEvent) {
    if (!layout.dragging) return;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    layout.dragging = false;
    document.body.classList.remove("resizing");
    layout.save();
  }
  function key(e: KeyboardEvent) {
    const step = e.shiftKey ? 48 : 16;
    if (e.key === "ArrowLeft") { e.preventDefault(); layout.nudge(which, -step); }
    else if (e.key === "ArrowRight") { e.preventDefault(); layout.nudge(which, step); }
    else if (e.key === "Home" || e.key === "Enter") { e.preventDefault(); layout.reset(which); }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex -->
<div
  class="divider"
  role="separator"
  aria-orientation="vertical"
  aria-label={label}
  aria-valuenow={layout[which]}
  aria-valuemin={which === "sidebar" ? 180 : 260}
  aria-valuemax={which === "sidebar" ? 420 : 700}
  tabindex="0"
  title="Drag to resize · double-click to reset"
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
  ondblclick={() => layout.reset(which)}
  onkeydown={key}
><span></span></div>

<style>
  .divider { flex: none; width: 9px; margin: 0 -4px; position: relative; z-index: 5; cursor: col-resize; touch-action: none; display: flex; justify-content: center; }
  .divider span { width: 1px; background: transparent; transition: background .12s, width .12s; }
  .divider:hover span, .divider:focus-visible span { background: var(--accent); width: 2px; }
  :global(body.resizing) { cursor: col-resize !important; user-select: none !important; }
  :global(body.resizing iframe) { pointer-events: none; }
</style>
