<script lang="ts">
  import { workspace } from "../lib/workspace.svelte";
  import { activeGroupKey, findLeaf, leaf, tabGroups, type Node } from "../lib/wstree";
  import Pane from "./Pane.svelte";
  import TabBar from "./TabBar.svelte";
  import PaneDivider from "./PaneDivider.svelte";

  const ws = $derived(workspace.ws);
  const zoomLeaf = $derived(ws.zoom ? findLeaf(ws.root, ws.zoom) : null);
  const closing = $derived(workspace.closeReq ? ws.panes[workspace.closeReq.id] : null);
  let dlgEl = $state<HTMLDivElement>();

  // ---- tabs layout: one group at a time under the tab strip (detached windows always use tiles) ----
  const tabs = $derived(workspace.tabsOn);
  const groups = $derived(tabs ? tabGroups(ws) : []);
  const activeKey = $derived(tabs ? activeGroupKey(ws) : null);
  // Mail tab: message list | reader with a local divider (ratio is view state, not part of the layout tree).
  const MIN_R = 0.25, MAX_R = 0.6;
  let mailRatio = $state(0.38);
  let dividing = $state(false);
  const clampR = (r: number) => Math.min(MAX_R, Math.max(MIN_R, r));
  function dDown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dividing = true;
    document.body.classList.add("pane-resizing", "pr-col");
  }
  function dMove(e: PointerEvent) {
    if (!dividing) return;
    const host = (e.currentTarget as HTMLElement).parentElement!.getBoundingClientRect();
    if (host.width > 0) mailRatio = clampR((e.clientX - host.left) / host.width);
  }
  function dUp(e: PointerEvent) {
    if (!dividing) return;
    try { (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId); } catch { /* ignore */ }
    dividing = false;
    document.body.classList.remove("pane-resizing", "pr-col");
  }
  function dKey(e: KeyboardEvent) {
    const step = e.shiftKey ? 0.06 : 0.02;
    if (e.key === "ArrowLeft") { e.preventDefault(); mailRatio = clampR(mailRatio - step); }
    else if (e.key === "ArrowRight") { e.preventDefault(); mailRatio = clampR(mailRatio + step); }
    else if (e.key === "Home" || e.key === "Enter") { e.preventDefault(); mailRatio = 0.38; }
  }

  function key(e: KeyboardEvent) {
    if (e.key === "Escape" && workspace.drag) { e.preventDefault(); workspace.dragEnd(false); }
  }
  function dlgKey(e: KeyboardEvent) {
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); workspace.answerClose("cancel"); }
    else if (e.key === "Tab" && dlgEl) {
      const b = Array.from(dlgEl.querySelectorAll<HTMLElement>("button"));
      const i = b.indexOf(document.activeElement as HTMLElement);
      e.preventDefault(); b[(i + (e.shiftKey ? -1 : 1) + b.length) % b.length]?.focus();
    }
  }
  $effect(() => { if (closing) dlgEl?.querySelector<HTMLElement>("[data-first]")?.focus(); });
</script>

<svelte:window onkeydown={key} />

<div class="ws-root" class:tabbed={tabs && !!ws.root} bind:clientWidth={workspace.hostW}>
  {#if !ws.root}
    <div class="empty">
      <p>No panes open.</p>
      <button class="btn ghost" onclick={() => workspace.resetLayout()}>Reset layout</button>
    </div>
  {:else if tabs}
    <TabBar />
    <div class="tabbody">
      <!-- Every tab stays mounted (files, sending state, Zoom meetings and unsaved edits survive a switch); inactive ones are display:none. -->
      {#each groups as g (g.key)}
        <div class="tabpanel" role="tabpanel" id="tp-{g.key}" aria-labelledby="tab-{g.key}" hidden={g.key !== activeKey}>
          {#if g.key === "mail" && g.ids.length > 1}
            <div class="cell" style="flex: {mailRatio} 1 0"><Pane leaf={leaf(g.ids[0])} bare /></div>
            <!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex -->
            <div class="mdiv" class:on={dividing} role="separator" aria-orientation="vertical" aria-label="Resize message list and reader" aria-valuenow={Math.round(mailRatio * 100)} aria-valuemin={Math.round(MIN_R * 100)} aria-valuemax={Math.round(MAX_R * 100)} tabindex="0"
              title="Drag to resize · double-click to reset" onpointerdown={dDown} onpointermove={dMove} onpointerup={dUp} onpointercancel={dUp} ondblclick={() => (mailRatio = 0.38)} onkeydown={dKey}><span></span></div>
            <div class="cell" style="flex: {1 - mailRatio} 1 0"><Pane leaf={leaf(g.ids[1])} bare /></div>
          {:else}
            {#each g.ids as id (id)}<Pane leaf={leaf(id)} bare />{/each}
          {/if}
        </div>
      {/each}
    </div>
  {:else if zoomLeaf}
    <Pane leaf={zoomLeaf} />
  {:else}
    {@render node(ws.root, "")}
  {/if}
</div>

{#snippet node(n: Node, path: string)}
  {#if n.t === "leaf"}
    <Pane leaf={n} />
  {:else}
    <div class="split {n.dir}">
      <div class="cell" style="flex: {n.ratio} 1 0">{@render node(n.a, path + "a")}</div>
      <PaneDivider dir={n.dir} {path} ratio={n.ratio} />
      <div class="cell" style="flex: {1 - n.ratio} 1 0">{@render node(n.b, path + "b")}</div>
    </div>
  {/if}
{/snippet}

{#if workspace.drag}
  {@const d = workspace.drag}
  {#if d.box}<div class="zone" style="left:{d.box.l}px;top:{d.box.t}px;width:{d.box.w}px;height:{d.box.h}px" aria-hidden="true"></div>{/if}
  <div class="ghost mono" style="left:{d.x + 12}px;top:{d.y + 12}px" aria-hidden="true">{ws.panes[d.id]?.title}{d.out ? (workspace.mode === "pane" ? " → main window" : " → new window") : d.target ? ` → ${d.target.zone}` : ""}</div>
{/if}

{#if closing && workspace.closeReq}
  <div class="scrim">
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="dlg" role="alertdialog" aria-modal="true" aria-labelledby="cp-t" aria-describedby="cp-b" tabindex="-1" bind:this={dlgEl} onkeydown={dlgKey}>
      <h2 id="cp-t">Close “{closing.title}”?</h2>
      <p id="cp-b">It has unsent changes. Park it to finish later from the Drafts tray, or discard it.</p>
      <div class="acts">
        <button class="btn ghost" onclick={() => workspace.answerClose("cancel")}>Cancel</button>
        <button class="btn ghost" onclick={() => workspace.answerClose("discard")}>Discard</button>
        <button class="btn" data-first onclick={() => workspace.answerClose("park")}>Park</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .ws-root { flex: 1; min-width: 0; min-height: 0; display: flex; padding-top: 28px; background: var(--bg); }
  .ws-root.tabbed { flex-direction: column; }
  .tabbody { flex: 1; min-width: 0; min-height: 0; display: flex; }
  .tabpanel { flex: 1; min-width: 0; min-height: 0; display: flex; }
  .tabpanel[hidden] { display: none; }
  .mdiv { flex: none; position: relative; z-index: 5; touch-action: none; display: flex; justify-content: center; align-items: center; background: var(--line); width: 1px; margin: 0 -3px; padding: 0 3px; cursor: col-resize; box-sizing: content-box; }
  .mdiv span { position: absolute; top: 0; bottom: 0; width: 2px; background: transparent; transition: background .12s; }
  .mdiv:hover span, .mdiv:focus-visible span, .mdiv.on span { background: var(--accent); }
  @media (prefers-reduced-motion: reduce) { .mdiv span { transition: none; } }
  .split { flex: 1; min-width: 0; min-height: 0; display: flex; }
  .split.col { flex-direction: column; }
  .cell { min-width: 0; min-height: 0; display: flex; }
  .empty { flex: 1; display: grid; place-content: center; gap: 12px; justify-items: center; color: var(--muted); }
  .zone { position: fixed; z-index: 90; pointer-events: none; background: color-mix(in srgb, var(--accent) 22%, transparent); border: 2px solid var(--accent); border-radius: var(--radius); }
  .ghost { position: fixed; z-index: 91; pointer-events: none; background: var(--surface); border: 1px solid var(--line); border-left: 2px solid var(--accent); padding: 3px 8px; max-width: 280px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--ink); }
  .scrim { position: fixed; inset: 0; background: color-mix(in srgb, #000 55%, transparent); display: grid; place-items: center; z-index: 100; padding: 20px; }
  .dlg { width: 100%; max-width: 420px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); padding: 22px; display: grid; gap: 12px; outline: none; }
  h2 { font-size: 15px; letter-spacing: .1em; overflow-wrap: anywhere; }
  p { margin: 0; font-size: 14px; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 6px; }
</style>
