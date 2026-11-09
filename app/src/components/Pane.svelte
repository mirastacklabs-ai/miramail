<script lang="ts">
  import AiPane from "./ai/AiPane.svelte";
  import { workspace } from "../lib/workspace.svelte";
  import { PANE_ICONS as ICONS, type Leaf } from "../lib/wstree";
  import MessageList from "./MessageList.svelte";
  import Reader from "./Reader.svelte";
  import Compose from "./Compose.svelte";
  import EventEditor from "./EventEditor.svelte";
  import CalendarView from "./CalendarView.svelte";
  import RenderError from "./RenderError.svelte";

  /** bare: Tabs layout, the tab strip replaces this pane's header row. */
  let { leaf, bare = false }: { leaf: Leaf; bare?: boolean } = $props();
  const p = $derived(workspace.ws.panes[leaf.active]);
  const focused = $derived(workspace.ws.focus === leaf.active);
  const zoomed = $derived(workspace.ws.zoom === leaf.active);
  let menu = $state(false);
  const inWindow = workspace.mode === "pane";

  // ---- header drag (dock) ----
  let pending: { id: string; x: number; y: number; started: boolean } | null = null;
  function down(e: PointerEvent, id: string) {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".act, .menu")) return;
    pending = { id, x: e.clientX, y: e.clientY, started: false };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function move(e: PointerEvent) {
    if (!pending) return;
    if (!workspace.drag) {
      if (pending.started) { // Escape / cancel ended the drag: the next move must not restart it
        pending = null;
        try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
        return;
      }
      if (Math.hypot(e.clientX - pending.x, e.clientY - pending.y) < 6) return;
      pending.started = true;
      workspace.dragStart(pending.id, e.clientX, e.clientY);
    }
    workspace.dragMove(e.clientX, e.clientY);
  }
  function up(e: PointerEvent, cancel = false) {
    if (!pending) return;
    const { id, started } = pending;
    pending = null;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    if (workspace.drag) workspace.dragEnd(!cancel, { x: e.screenX, y: e.screenY });
    else if (!cancel && !started) workspace.focus(id);
  }
  function tabKey(e: KeyboardEvent, id: string) {
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); workspace.focus(id); }
  }
</script>

{#if p}
<!-- svelte-ignore a11y_no_static_element_interactions -->
<section class="pane" class:focused data-pane-id={leaf.active} aria-label={p.title} onpointerdowncapture={() => { if (!focused) workspace.focus(leaf.active); }} onfocusin={() => { if (!focused) workspace.focus(leaf.active); }}>
  {#if !bare}
  <header class="hd">
    <div class="tabs" role={leaf.tabs.length > 1 ? "tablist" : undefined}>
      {#each leaf.tabs as tid (tid)}
        {@const tp = workspace.ws.panes[tid]}
        {#if tp}
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div class="tab" class:on={tid === leaf.active} role={leaf.tabs.length > 1 ? "tab" : undefined} aria-selected={leaf.tabs.length > 1 ? tid === leaf.active : undefined} tabindex={leaf.tabs.length > 1 ? 0 : undefined}
            title="Drag to dock or tab-stack" onpointerdown={(e) => down(e, tid)} onpointermove={move} onpointerup={(e) => up(e)} onpointercancel={(e) => up(e, true)} onkeydown={(e) => tabKey(e, tid)}>
            <svg class="ic" viewBox="0 0 24 24" aria-hidden="true"><path d={ICONS[tp.kind]} /></svg>
            <span class="t">{tp.title}</span>
            {#if tp.dirty}<span class="dirty" role="img" aria-label="Unsent changes" title="Unsent changes"></span>{/if}
            {#if leaf.tabs.length > 1}
              <button class="act x" aria-label="Close {tp.title}" title="Close" onclick={() => workspace.requestClose(tid)}><svg viewBox="0 0 24 24"><path d="M7 7l10 10M17 7L7 17"/></svg></button>
            {/if}
          </div>
        {/if}
      {/each}
    </div>
    <div class="acts">
      <button class="act" aria-label="Park to tray" title="Park (keep state in the Drafts tray)" onclick={() => workspace.park(leaf.active)}><svg viewBox="0 0 24 24"><path d="M5 8h14M7 8v10h10V8M10 12h4M9 4h6"/></svg></button>
      <button class="act" class:on={zoomed} aria-label={zoomed ? "Unzoom pane" : "Zoom pane"} aria-pressed={zoomed} title={zoomed ? "Unzoom (⇧⌘↵)" : "Zoom (⇧⌘↵)"} onclick={() => workspace.toggleZoom(leaf.active)}>
        <svg viewBox="0 0 24 24"><path d={zoomed ? "M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" : "M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"}/></svg>
      </button>
      <button class="act" aria-label="Close pane" title="Close (⌘W)" onclick={() => workspace.requestClose(leaf.active)}><svg viewBox="0 0 24 24"><path d="M7 7l10 10M17 7L7 17"/></svg></button>
      <div class="mwrap">
        <button class="act" aria-label="Pane menu" aria-haspopup="menu" aria-expanded={menu} title="More" onclick={() => (menu = !menu)}><svg viewBox="0 0 24 24"><circle cx="6" cy="12" r="1.3"/><circle cx="12" cy="12" r="1.3"/><circle cx="18" cy="12" r="1.3"/></svg></button>
        {#if menu}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="scrimmenu" onpointerdown={() => (menu = false)}></div>
          <div class="menu" role="menu" tabindex="-1" onkeydown={(e) => e.key === "Escape" && (menu = false)}>
            <button role="menuitem" onclick={() => { menu = false; workspace.split("row"); }}>New message to the right</button>
            <button role="menuitem" onclick={() => { menu = false; workspace.split("col"); }}>New message below</button>
            <button role="menuitem" onclick={() => { menu = false; workspace.toggleZoom(leaf.active); }}>{zoomed ? "Unzoom" : "Zoom"}</button>
            <button role="menuitem" onclick={() => { menu = false; workspace.park(leaf.active); }}>Park to tray</button>
            <button role="menuitem" onclick={() => { menu = false; workspace.toggleView(); }}>Switch to {workspace.tabsOn ? "Tiles" : "Tabs"} layout <span class="kbd">⇧⌘T</span></button>
            <button role="menuitem" onclick={() => { menu = false; workspace.resetLayout(); }}>Reset layout</button>
            {#if inWindow}
              <button role="menuitem" onclick={() => { menu = false; workspace.attachFocused(); }}>Attach to main window <span class="kbd">⇧⌘A</span></button>
            {:else}
              {@const detachWhy = workspace.detachBlock(p?.kind, leaf.active)}
              <button role="menuitem" disabled={!!detachWhy} title={detachWhy ?? "Move this pane into its own window"} onclick={() => { menu = false; void workspace.detach(leaf.active); }}>Detach to window <span class="kbd">⇧⌘O</span></button>
            {/if}
            <button role="menuitem" onclick={() => { menu = false; void workspace.requestClose(leaf.active); }}>Close pane</button>
          </div>
        {/if}
      </div>
    </div>
  </header>
  {/if}
  <div class="bd">
    {#key p.id}
      <!-- A pane that throws while rendering shows its error instead of leaving the layout half-updated. -->
      <svelte:boundary onerror={(e) => console.error(`[miramail] ${p.kind} pane failed:`, e)}>
        {#if p.kind === "mailbox"}<MessageList />
        {:else if p.kind === "reader"}<Reader />
        {:else if p.kind === "compose"}<Compose id={p.id} />
        {:else if p.kind === "event"}<EventEditor id={p.id} />
        {:else if p.kind === "calendar"}<CalendarView />
        {:else}
          <AiPane />
        {/if}
        {#snippet failed(error, reset)}<RenderError {error} {reset} what={p.title || "This pane"} />{/snippet}
      </svelte:boundary>
    {/key}
  </div>
</section>
{/if}

<style>
  .pane { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; background: var(--bg); position: relative; overflow: hidden; border-top: 2px solid transparent; }
  .pane.focused { border-top-color: var(--accent); }
  .hd { flex: none; display: flex; align-items: center; justify-content: space-between; gap: 4px; height: 28px; border-bottom: 1px solid var(--line); background: var(--bg-band); padding: 0 2px 0 0; }
  .tabs { display: flex; min-width: 0; flex: 1; overflow: hidden; }
  .tab { display: flex; align-items: center; gap: 6px; min-width: 0; padding: 0 10px; height: 27px; color: var(--muted); cursor: grab; touch-action: none; border-bottom: 2px solid transparent; }
  .tab.on { color: var(--ink); }
  .tab:only-child { flex: 1; }
  .tab:not(:only-child).on { border-bottom-color: var(--accent); }
  .tab:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .t { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .1em; font-weight: 600; min-width: 0; }
  .ic { width: 13px; height: 13px; flex: none; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .dirty { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex: none; }
  .acts { display: flex; align-items: center; flex: none; }
  .act { width: 24px; height: 24px; display: inline-grid; place-items: center; background: none; border: 0; border-radius: var(--radius); color: var(--muted); padding: 0; }
  .act:hover, .act.on { color: var(--ink); background: color-mix(in srgb, var(--line) 60%, transparent); }
  .act svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .act.x { width: 18px; height: 18px; } .act.x svg { width: 11px; height: 11px; }
  .mwrap { position: relative; }
  .scrimmenu { position: fixed; inset: 0; z-index: 80; }
  .menu { position: absolute; right: 0; top: 26px; z-index: 81; min-width: 250px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); padding: 4px; display: grid; }
  .menu button { text-align: left; background: none; border: 0; padding: 7px 10px; color: var(--ink); font-size: 0.8667rem; border-radius: 2px; }
  .menu button:hover:not(:disabled) { background: color-mix(in srgb, var(--line) 60%, transparent); }
  .menu button:disabled { color: var(--muted); cursor: not-allowed; }
  .kbd { float: right; margin-left: 16px; color: var(--muted); font-size: 0.75rem; }
  .bd { flex: 1; min-height: 0; min-width: 0; display: flex; flex-direction: column; }
</style>
