<script lang="ts">
  // Tabs layout: one browser-style strip over the same panes the tiles show. No state of its own: the active tab is the focused pane's group.
  import { tick, onDestroy } from "svelte";
  import { isDetachable } from "../lib/detach";
  import { workspace } from "../lib/workspace.svelte";
  import { insertionSlot, moveTab, slotToIndex } from "../lib/tabreorder";
  import { PANE_ICONS, MAIL_KEY, activeGroupKey, groupFocusId, tabGroups, type TabGroup } from "../lib/wstree";

  const groups = $derived(tabGroups(workspace.ws));
  const active = $derived(activeGroupKey(workspace.ws));
  /** Roving tabindex: the tab holding keyboard focus (falls back to the active one). */
  let rov = $state<string | null>(null);
  const stop = $derived(rov && groups.some((g) => g.key === rov) ? rov : active);
  let strip = $state<HTMLDivElement>();

  // A new active tab (click, shortcut, closed neighbour) takes over the roving stop.
  $effect(() => { void active; rov = null; });
  $effect(() => {
    const on = () => { void tick().then(() => strip?.querySelector<HTMLElement>(`[data-key="${CSS.escape(active ?? "")}"]`)?.focus()); };
    window.addEventListener("miramail:tab-focus", on);
    return () => window.removeEventListener("miramail:tab-focus", on);
  });
  $effect(() => {
    const k = active;
    void tick().then(() => strip?.querySelector<HTMLElement>(`[data-key="${CSS.escape(k ?? "")}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" }));
  });

  // ---- drag a tab: along the strip it reorders (insertion marker, commit on release); pulled away from the strip it tears off ----
  // Tear-off takes over once the pointer is more than TEAR_PX above/below the bar (the drop point is then judged by workspace.dragMove /
  // dragEnd exactly as for pane headers); coming back near the bar cancels the tear-off and reorders again. Escape cancels either.
  const TEAR_PX = 24;
  let pending: { pid: number; g: TabGroup; x: number; y: number; started: boolean; mode: "reorder" | "tear" | "cancelled" } | null = null;
  let dest = $state<number | null>(null); // index the dragged tab would land on while reordering
  let dragId = $state<string | null>(null);
  const dragKey = $derived(dest === null ? null : dragId);
  const fromIdx = $derived(dragKey === null ? -1 : groups.findIndex((x) => x.key === dragKey));
  function down(e: PointerEvent, g: TabGroup) {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".x")) return;
    dragId = g.key;
    pending = { pid: e.pointerId, g, x: e.clientX, y: e.clientY, started: false, mode: "reorder" };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    window.addEventListener("keydown", esc, true);
  }
  function esc(e: KeyboardEvent) {
    if (e.key !== "Escape" || !pending) return;
    if (pending.mode === "reorder") { e.stopPropagation(); dest = null; pending.mode = "cancelled"; }
    // tear mode: workspace's own Escape handling cancels workspace.drag; the next move sees pending.started with no drag and stops
  }
  function awayFromBar(x: number, y: number): boolean {
    if (x < 0 || x > window.innerWidth) return true; // left the window sideways
    const r = strip?.parentElement?.getBoundingClientRect();
    return !!r && (y < r.top - TEAR_PX || y > r.bottom + TEAR_PX);
  }
  function gap(x: number): number {
    const mids = [...(strip?.querySelectorAll<HTMLElement>(".tab") ?? [])].map((el) => { const r = el.getBoundingClientRect(); return r.left + r.width / 2; });
    return insertionSlot(mids, x);
  }
  /** Only detachable kinds (and only where this window can detach) may leave the strip; the rest stay in reorder mode. */
  function canTear(g: TabGroup): boolean {
    return g.key !== MAIL_KEY && isDetachable(workspace.ws.panes[groupFocusId(g)]?.kind) && (workspace.canDetach || workspace.mode === "pane");
  }
  // Autoscroll the strip while a reorder drag sits near its left/right edge, so off-screen tabs can be targets.
  let raf = 0, lastX = 0;
  function scrollTick() {
    raf = 0;
    if (!strip || !pending || pending.mode !== "reorder" || !pending.started) return;
    const r = strip.getBoundingClientRect(), EDGE = 32;
    const dx = lastX < r.left + EDGE ? -Math.ceil((r.left + EDGE - lastX) / 3) : lastX > r.right - EDGE ? Math.ceil((lastX - (r.right - EDGE)) / 3) : 0;
    if (dx) { strip.scrollLeft += dx; refresh(); }
    raf = requestAnimationFrame(scrollTick);
  }
  function refresh() {
    if (!pending || pending.mode !== "reorder") return;
    const keys = groups.map((x) => x.key), from = keys.indexOf(pending.g.key);
    const t = moveTab(keys, from, slotToIndex(from, gap(lastX))).indexOf(pending.g.key);
    dest = t === from ? null : t;
  }
  /** Abort any drag in progress (component destroyed, or pointer capture lost): listeners, rAF and a started tear-off all end. */
  function teardown() {
    window.removeEventListener("keydown", esc, true);
    if (raf) { cancelAnimationFrame(raf); raf = 0; }
    if (pending?.mode === "tear" && workspace.drag) workspace.dragEnd(false);
    pending = null;
    dest = null;
  }
  onDestroy(teardown);
  function lost(e: PointerEvent) {
    if (pending && e.pointerId === pending.pid && !workspace.drag) teardown(); // a tear-off keeps the pointer through pointerup, which ends it
  }
  function move(e: PointerEvent) {
    if (!pending || pending.mode === "cancelled" || e.pointerId !== pending.pid) return;
    lastX = e.clientX;
    if (!pending.started && !workspace.drag && Math.hypot(e.clientX - pending.x, e.clientY - pending.y) < 6) return;
    if (pending.mode === "tear" && !workspace.drag) { stop_(e); return; } // Escape ended the tear-off: the next move must not restart it
    pending.started = true;
    if (!awayFromBar(e.clientX, e.clientY) && pending.mode === "tear" && workspace.drag) {
      workspace.dragEnd(false); // back over the bar: reorder instead
      pending.mode = "reorder";
    }
    if (pending.mode === "reorder" && canTear(pending.g) && awayFromBar(e.clientX, e.clientY)) pending.mode = "tear";
    if (pending.mode === "reorder") {
      refresh();
      if (!raf) raf = requestAnimationFrame(scrollTick);
      return;
    }
    dest = null;
    if (!workspace.drag) workspace.dragStart(groupFocusId(pending.g), pending.x, pending.y);
    workspace.dragMove(e.clientX, e.clientY);
  }
  function stop_(e: PointerEvent) {
    if (raf) { cancelAnimationFrame(raf); raf = 0; }
    pending = null;
    dest = null;
    window.removeEventListener("keydown", esc, true);
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
  }
  function up(e: PointerEvent, cancel = false) {
    if (!pending || e.pointerId !== pending.pid) return;
    const { g, started, mode } = pending;
    const d = dest;
    stop_(e);
    if (workspace.drag) workspace.dragEnd(!cancel, { x: e.screenX, y: e.screenY });
    else if (mode === "reorder" && d !== null && !cancel) {
      const keys = groups.map((x) => x.key), from = keys.indexOf(g.key);
      workspace.reorderTabs(moveTab(keys, from, d).filter((k) => k !== MAIL_KEY));
    } else if (!cancel && !started) { rov = null; workspace.selectGroup(g.key); }
  }
  function aux(e: MouseEvent, g: TabGroup) {
    if (e.button !== 1) return;
    e.preventDefault();
    close(g);
  }
  function close(g: TabGroup) { if (g.key !== MAIL_KEY) void workspace.requestClose(g.ids[0]).then(() => workspace.tabFocus()); }

  function key(e: KeyboardEvent, g: TabGroup, i: number) {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); rov = null; workspace.selectGroup(g.key); }
    else if (e.key === "ArrowLeft" || e.key === "ArrowRight" || e.key === "Home" || e.key === "End") {
      e.preventDefault();
      const n = groups.length;
      const j = e.key === "Home" ? 0 : e.key === "End" ? n - 1 : (i + (e.key === "ArrowRight" ? 1 : -1) + n) % n;
      rov = groups[j].key;
      void tick().then(() => strip?.querySelector<HTMLElement>(`[data-key="${CSS.escape(groups[j].key)}"]`)?.focus());
    } else if (e.key === "Delete" || e.key === "Backspace") { e.preventDefault(); close(g); }
  }
</script>

<div class="bar">
  <div class="strip" role="tablist" aria-label="Open tabs" bind:this={strip}>
    {#each groups as g, i (g.key)}
      <div class="tab" class:on={g.key === active} class:drag={g.key === dragKey} class:before={dest !== null && i === dest && dest < fromIdx} class:after={dest !== null && i === dest && dest > fromIdx} role="tab" id="tab-{g.key}" aria-controls="tp-{g.key}" aria-keyshortcuts={g.key === MAIL_KEY ? undefined : "Delete"} aria-selected={g.key === active} tabindex={g.key === stop ? 0 : -1} data-key={g.key}
        title={g.key === MAIL_KEY ? "Mail" : `${g.title} (drag to reorder, or out of the window to detach)`}
        onpointerdown={(e) => down(e, g)} onpointermove={move} onpointerup={(e) => up(e)} onpointercancel={(e) => up(e, true)} onlostpointercapture={lost}
        onauxclick={(e) => aux(e, g)} onkeydown={(e) => key(e, g, i)} onfocus={() => (rov = g.key)}>
        <svg class="ic" viewBox="0 0 24 24" aria-hidden="true"><path d={PANE_ICONS[g.kind]} /></svg>
        <span class="t">{g.title}</span>
        {#if g.dirty}<span class="dirty" role="img" aria-label="Unsent changes" title="Unsent changes"></span>{/if}
        {#if g.key !== MAIL_KEY}
          <button class="x" tabindex="-1" aria-label="Close {g.title}" title="Close (⌘W)" onclick={() => close(g)}><svg viewBox="0 0 24 24"><path d="M7 7l10 10M17 7L7 17"/></svg></button>
        {/if}
      </div>
    {/each}
  </div>
  <button class="add" aria-label="New message" title="New message (⌘T)" onclick={() => workspace.newCompose()}><svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg></button>
  <button class="lay" aria-label="Switch to Tiles layout" title="Switch to Tiles layout (⇧⌘T)" onclick={() => workspace.toggleView()}><svg viewBox="0 0 24 24"><rect x="4" y="4" width="7" height="16"/><rect x="13" y="4" width="7" height="7"/><rect x="13" y="13" width="7" height="7"/></svg></button>
</div>

<style>
  .bar { flex: none; display: flex; align-items: center; height: 30px; border-bottom: 1px solid var(--line); background: var(--bg-band); padding-right: 2px; }
  .strip { display: flex; flex: 0 1 auto; min-width: 0; height: 100%; overflow-x: auto; overflow-y: hidden; scrollbar-width: none; }
  .strip::-webkit-scrollbar { display: none; }
  .tab { flex: 0 1 200px; min-width: 96px; display: flex; align-items: center; gap: 6px; padding: 0 8px 0 10px; height: 100%; box-sizing: border-box; color: var(--muted); cursor: default; touch-action: none; border-bottom: 2px solid transparent; border-right: 1px solid var(--line); user-select: none; }
  .tab { position: relative; }
  .tab.drag { opacity: .6; }
  .tab.before::before, .tab.after::after { content: ""; position: absolute; top: 3px; bottom: 3px; width: 2px; background: var(--accent); border-radius: 1px; }
  .tab.before::before { left: -1px; }
  .tab.after::after { right: -1px; }
  .tab:hover { color: var(--ink); }
  .tab.on { color: var(--ink); border-bottom-color: var(--accent); background: var(--bg); }
  .tab:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .t { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .1em; font-weight: 600; min-width: 0; }
  .ic { width: 13px; height: 13px; flex: none; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .dirty { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex: none; }
  .x, .add, .lay { display: inline-grid; place-items: center; background: none; border: 0; border-radius: var(--radius); color: var(--muted); padding: 0; flex: none; }
  .x { width: 18px; height: 18px; }
  .add { width: 24px; height: 24px; margin-left: 4px; }
  .lay { width: 24px; height: 24px; margin-left: auto; }
  .x:hover, .add:hover, .lay:hover { color: var(--ink); background: color-mix(in srgb, var(--line) 60%, transparent); }
  .add:focus-visible, .x:focus-visible, .lay:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .x svg { width: 11px; height: 11px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .add svg, .lay svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
</style>
