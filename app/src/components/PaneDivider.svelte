<script lang="ts">
  import { workspace } from "../lib/workspace.svelte";
  import type { Dir } from "../lib/wstree";
  let { dir, path, ratio }: { dir: Dir; path: string; ratio: number } = $props();
  let dragging = $state(false);
  const vertical = $derived(dir === "row"); // a row split has a vertical bar between its children

  function box(e: Event) { return ((e.currentTarget as HTMLElement).parentElement as HTMLElement).getBoundingClientRect(); }
  const size = (r: DOMRect) => (dir === "row" ? r.width : r.height);

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragging = true;
    document.body.classList.add("pane-resizing", vertical ? "pr-col" : "pr-row");
  }
  function move(e: PointerEvent) {
    if (!dragging) return;
    const r = box(e);
    const s = size(r);
    if (s <= 0) return;
    const pos = dir === "row" ? e.clientX - r.left : e.clientY - r.top;
    workspace.setRatio(path, pos / s, s);
  }
  function up(e: PointerEvent) {
    if (!dragging) return;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    dragging = false;
    document.body.classList.remove("pane-resizing", "pr-col", "pr-row");
  }
  function key(e: KeyboardEvent) {
    const step = e.shiftKey ? 0.06 : 0.02;
    const [dec, inc] = vertical ? ["ArrowLeft", "ArrowRight"] : ["ArrowUp", "ArrowDown"];
    const s = size(box(e));
    if (e.key === dec) { e.preventDefault(); workspace.setRatio(path, ratio - step, s); }
    else if (e.key === inc) { e.preventDefault(); workspace.setRatio(path, ratio + step, s); }
    else if (e.key === "Home" || e.key === "Enter") { e.preventDefault(); workspace.resetRatio(path, s); }
  }
  const pct = $derived(Math.round(ratio * 100));
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex -->
<div
  class="pdiv {vertical ? 'v' : 'h'}"
  class:on={dragging}
  role="separator"
  aria-orientation={vertical ? "vertical" : "horizontal"}
  aria-label={vertical ? "Resize panes (left | right)" : "Resize panes (top | bottom)"}
  aria-valuenow={pct}
  aria-valuemin={5}
  aria-valuemax={95}
  tabindex="0"
  title="Drag to resize · double-click to reset"
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
  ondblclick={(e) => workspace.resetRatio(path, size(box(e)))}
  onkeydown={key}
><span></span></div>

<style>
  .pdiv { flex: none; position: relative; z-index: 5; touch-action: none; display: flex; justify-content: center; align-items: center; background: var(--line); }
  .pdiv.v { width: 1px; margin: 0 -3px; padding: 0 3px; cursor: col-resize; box-sizing: content-box; }
  .pdiv.h { height: 1px; margin: -3px 0; padding: 3px 0; cursor: row-resize; box-sizing: content-box; flex-direction: column; }
  .pdiv span { position: absolute; background: transparent; transition: background .12s; }
  .pdiv.v span { top: 0; bottom: 0; width: 2px; }
  .pdiv.h span { left: 0; right: 0; height: 2px; }
  .pdiv:hover span, .pdiv:focus-visible span, .pdiv.on span { background: var(--accent); }
  :global(body.pane-resizing) { user-select: none !important; }
  :global(body.pr-col) { cursor: col-resize !important; }
  :global(body.pr-row) { cursor: row-resize !important; }
  :global(body.pane-resizing iframe), :global(body.pane-dragging iframe) { pointer-events: none; }
  :global(body.pane-dragging) { cursor: grabbing !important; user-select: none !important; }
</style>
