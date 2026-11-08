<script lang="ts">
  // Sidebar entry: shown only while something is snoozed.
  import { snoozeUi } from "../lib/t2.svelte";
  const n = $derived(snoozeUi.items.length);
  const bad = $derived(snoozeUi.items.filter((s) => s.state === "failed").length);
</script>

{#if n}
  <section class="sz" aria-label="Snoozed">
    <button class="row" onclick={() => (snoozeUi.listOpen = true)} title="Messages that will return to your Inbox later">
      <span class="k mono">Snoozed</span>
      <span class="ti">{bad ? `${bad} could not return` : "Coming back later"}</span>
      <span class="n mono" class:warn={bad > 0}>{n}</span>
    </button>
  </section>
{/if}

<style>
  .sz { padding-top: 6px; flex: none; }
  .row { width: 100%; display: flex; align-items: center; gap: 8px; background: none; border: 0; border-left: 2px solid var(--accent); padding: 5px 6px; text-align: left; color: var(--muted); border-radius: 0 var(--radius) var(--radius) 0; }
  .row:hover { color: var(--ink); background: color-mix(in srgb, var(--line) 50%, transparent); }
  .k { color: var(--accent); font-size: 0.7rem; flex: none; }
  .ti { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 0.8667rem; }
  .n { color: var(--accent); font-size: 0.8rem; }
  .n.warn { color: var(--danger); }
</style>
