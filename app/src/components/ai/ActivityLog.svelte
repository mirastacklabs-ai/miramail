<script lang="ts">
  // What the assistant's suggestions did, across all conversations, with Undo while it can still be undone.
  import { onMount } from "svelte";
  import { ai } from "../../lib/ai.svelte";
  import { isRunning, resultText } from "../../lib/aivm";

  onMount(() => { void ai.loadActivity(); });
  const when = (t: number) => new Date(t * 1000).toLocaleString();
</script>

<div class="log">
  <div class="bar">
    <h3>Activity</h3>
    <button class="btn ghost sm" onclick={() => (ai.activityOpen = false)}>Back to chat</button>
  </div>
  {#if ai.activity.length === 0}
    <p class="empty">Nothing yet. When you approve a change the assistant suggested, it is listed here and can be undone.</p>
  {:else}
    <ul>
      {#each ai.activity as p (p.id)}
        <li>
          <div class="row">
            <span class="t">{p.title}</span>
            <span class="when mono">{when(p.updatedAt)}</span>
          </div>
          <p class="res">{resultText(p)}</p>
          {#if p.undoable && !isRunning(p.state)}
            <button class="btn ghost sm" disabled={!!ai.pbusy[p.id]} onclick={() => ai.undo(p.id).then(() => ai.loadActivity())}>{ai.pbusy[p.id] === "undoing" ? "Putting back…" : "Undo"}</button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .log { flex: 1; min-height: 0; overflow-y: auto; padding: 10px 14px; display: grid; gap: 10px; align-content: start; }
  .bar { display: flex; justify-content: space-between; align-items: center; }
  h3 { margin: 0; font-size: 0.9333rem; letter-spacing: .08em; }
  ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
  li { border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; display: grid; gap: 4px; justify-items: start; }
  .row { display: flex; justify-content: space-between; gap: 10px; width: 100%; flex-wrap: wrap; }
  .t { font-size: 0.8667rem; overflow-wrap: anywhere; }
  .when { color: var(--muted); font-size: 0.72rem; }
  .res, .empty { margin: 0; color: var(--muted); font-size: 0.8rem; }
</style>
