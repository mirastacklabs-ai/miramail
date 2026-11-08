<script lang="ts">
  // Sidebar entry: shown only while the outbox is not empty.
  import { outbox } from "../lib/outbox.svelte";
  import { needsAttention } from "../lib/outboxmodel";
  const n = $derived(outbox.items.length);
  const warn = $derived(needsAttention(outbox.items));
  const sched = $derived(outbox.items.filter((i) => i.state === "scheduled").length);
</script>

{#if n}
  <section class="ob" aria-label="Outbox">
    <button class="row" onclick={() => (outbox.open = true)} title="Messages waiting to be sent">
      <span class="k mono">Outbox</span>
      <span class="ti">{warn ? `${warn} need${warn === 1 ? "s" : ""} attention` : sched === n ? `${sched} scheduled` : "Sending…"}</span>
      <span class="n mono" class:warn={warn > 0}>{n}</span>
    </button>
  </section>
{/if}

<style>
  .ob { border-top: 1px solid var(--line); padding-top: 8px; flex: none; }
  .row { width: 100%; display: flex; align-items: center; gap: 8px; background: none; border: 0; border-left: 2px solid var(--accent); padding: 5px 6px; text-align: left; color: var(--muted); border-radius: 0 var(--radius) var(--radius) 0; }
  .row:hover { color: var(--ink); background: color-mix(in srgb, var(--line) 50%, transparent); }
  .k { color: var(--accent); font-size: 0.7rem; flex: none; }
  .ti { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 0.8667rem; }
  .n { color: var(--accent); font-size: 0.8rem; }
  .n.warn { color: var(--danger); }
</style>
