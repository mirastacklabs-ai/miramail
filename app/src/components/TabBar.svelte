<script lang="ts">
  // Tabs layout: one browser-style strip over the same panes the tiles show. No state of its own: the active tab is the focused pane's group.
  import { tick } from "svelte";
  import { workspace } from "../lib/workspace.svelte";
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

  // ---- drag a tab out of the window to tear it off (same path as pane headers; no dock zones in tabs mode) ----
  let pending: { g: TabGroup; x: number; y: number; started: boolean } | null = null;
  function down(e: PointerEvent, g: TabGroup) {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".x")) return;
    pending = { g, x: e.clientX, y: e.clientY, started: false };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function move(e: PointerEvent) {
    if (!pending) return;
    if (!workspace.drag) {
      if (pending.started) { stop_(e); return; } // Escape / cancel ended the drag: the next move must not restart it
      if (Math.hypot(e.clientX - pending.x, e.clientY - pending.y) < 6) return;
      pending.started = true;
      workspace.dragStart(groupFocusId(pending.g), e.clientX, e.clientY);
    }
    workspace.dragMove(e.clientX, e.clientY);
  }
  function stop_(e: PointerEvent) {
    pending = null;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
  }
  function up(e: PointerEvent, cancel = false) {
    if (!pending) return;
    const { g, started } = pending;
    stop_(e);
    if (workspace.drag) workspace.dragEnd(!cancel, { x: e.screenX, y: e.screenY });
    else if (!cancel && !started) { rov = null; workspace.selectGroup(g.key); }
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
      <div class="tab" class:on={g.key === active} role="tab" id="tab-{g.key}" aria-controls="tp-{g.key}" aria-keyshortcuts={g.key === MAIL_KEY ? undefined : "Delete"} aria-selected={g.key === active} tabindex={g.key === stop ? 0 : -1} data-key={g.key}
        title={g.key === MAIL_KEY ? "Mail" : `${g.title} (drag out of the window to detach)`}
        onpointerdown={(e) => down(e, g)} onpointermove={move} onpointerup={(e) => up(e)} onpointercancel={(e) => up(e, true)}
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
</div>

<style>
  .bar { flex: none; display: flex; align-items: center; height: 30px; border-bottom: 1px solid var(--line); background: var(--bg-band); padding-right: 2px; }
  .strip { display: flex; flex: 0 1 auto; min-width: 0; height: 100%; overflow-x: auto; overflow-y: hidden; scrollbar-width: none; }
  .strip::-webkit-scrollbar { display: none; }
  .tab { flex: 0 1 200px; min-width: 96px; display: flex; align-items: center; gap: 6px; padding: 0 8px 0 10px; height: 100%; box-sizing: border-box; color: var(--muted); cursor: default; touch-action: none; border-bottom: 2px solid transparent; border-right: 1px solid var(--line); user-select: none; }
  .tab:hover { color: var(--ink); }
  .tab.on { color: var(--ink); border-bottom-color: var(--accent); background: var(--bg); }
  .tab:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .t { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .1em; font-weight: 600; min-width: 0; }
  .ic { width: 13px; height: 13px; flex: none; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .dirty { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex: none; }
  .x, .add { display: inline-grid; place-items: center; background: none; border: 0; border-radius: var(--radius); color: var(--muted); padding: 0; flex: none; }
  .x { width: 18px; height: 18px; }
  .add { width: 24px; height: 24px; margin-left: 4px; }
  .x:hover, .add:hover { color: var(--ink); background: color-mix(in srgb, var(--line) 60%, transparent); }
  .add:focus-visible, .x:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .x svg { width: 11px; height: 11px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .add svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
</style>
