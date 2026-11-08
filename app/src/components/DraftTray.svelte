<script lang="ts">
  import { app } from "../lib/state.svelte";
  import { workspace } from "../lib/workspace.svelte";

  const items = $derived(workspace.tray);
  const label = (k: string) => (k === "compose" ? "Mail" : k === "event" ? "Event" : k === "aichat" ? "AI" : k);

  async function discard(id: string, title: string) {
    const ok = await app.ask({ title: "Discard this draft?", body: `“${title}” will be deleted.`, ok: "Discard", danger: true });
    if (ok) workspace.close(id);
  }
</script>

{#if items.length}
  <section class="tray" aria-label="Drafts">
    <h3 class="label">Drafts <span class="mono n">{items.length}</span></h3>
    <ul>
      {#each items as it (it.id)}
        <li class:open={!it.parked}>
          <button class="row" title={it.parked ? "Restore this draft beside the focused pane" : "Focus this draft"} onclick={() => (it.parked ? workspace.unpark(it.id) : workspace.focus(it.id))}>
            <span class="k mono">{label(it.kind)}</span>
            <span class="ti">{it.title}</span>
            {#if it.dirty}<span class="dirty" role="img" aria-label="Unsent changes"></span>{/if}
            {#if !it.parked}<span class="st mono">open</span>{/if}
          </button>
          <button class="x" aria-label="Discard {it.title}" title="Discard" onclick={() => discard(it.id, it.title)}><svg viewBox="0 0 24 24"><path d="M7 7l10 10M17 7L7 17"/></svg></button>
        </li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .tray { display: grid; gap: 4px; border-top: 1px solid var(--line); padding-top: 8px; max-height: 30%; overflow-y: auto; flex: none; }
  h3 { display: flex; justify-content: space-between; font-weight: 500; letter-spacing: .12em; }
  .n { color: var(--accent); }
  ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 1px; }
  li { display: flex; align-items: center; }
  .row { flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px; background: none; border: 0; border-left: 2px solid transparent; padding: 5px 6px; text-align: left; color: var(--muted); border-radius: 0 var(--radius) var(--radius) 0; }
  li.open .row { border-left-color: var(--accent); }
  .row:hover { color: var(--ink); background: color-mix(in srgb, var(--line) 50%, transparent); }
  .k { color: var(--accent); font-size: 0.7rem; flex: none; }
  .ti { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; flex: 1; font-size: 0.8667rem; }
  .st { font-size: 0.7rem; flex: none; }
  .dirty { width: 6px; height: 6px; border-radius: 50%; background: var(--accent); flex: none; }
  .x { width: 20px; height: 20px; display: grid; place-items: center; background: none; border: 0; padding: 0; color: var(--muted); opacity: 0; border-radius: var(--radius); }
  li:hover .x, li:focus-within .x { opacity: 1; }
  .x:hover { color: var(--danger); }
  .x svg { width: 11px; height: 11px; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; }
</style>
