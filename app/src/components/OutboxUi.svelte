<script lang="ts">
  // Main window only: the "Sending… Undo" bar and the Outbox view (docs/ESSENTIALS_SPEC.md, send pipeline).
  import { outbox } from "../lib/outbox.svelte";
  import { actions, describe, recipients } from "../lib/outboxmodel";
  import Modal from "./Modal.svelte";

  const held = $derived(outbox.held);
  const latest = $derived(held[held.length - 1]);
</script>

<!-- One announcement when a send is held; the per-second countdown below is deliberately not live. -->
<div class="sr-only" role="status" aria-live="polite">{latest && !outbox.open ? "Sending message. Undo is available for a few seconds." : ""}</div>
{#if latest && !outbox.open}
  <div class="bar">
    <span class="selectable">Sending{held.length > 1 ? ` ${held.length} messages` : ""}… <span class="mono">{describe(latest, outbox.now).replace("Sending in ", "")}</span></span>
    <button class="btn ghost sm" onclick={() => outbox.restore(latest.id)}>Undo</button>
  </div>
{/if}

{#if outbox.open && outbox.items.length}
  <Modal title="Outbox" onclose={() => (outbox.open = false)} width={620}>
    <ul class="list">
      {#each outbox.items as it (it.id)}
        {@const a = actions(it)}
        <li class="it" class:bad={it.state === "failed" || it.state === "unknown"}>
          <div class="who"><span class="to">{recipients(it)}</span><span class="sub">{it.subject || "(no subject)"}</span></div>
          <div class="st">{describe(it, outbox.now)}</div>
          {#if it.error && it.state !== "interrupted"}<div class="er selectable">{it.error}</div>{/if}
          <div class="acts">
            {#if a.undo}<button class="btn ghost sm" onclick={() => outbox.restore(it.id)}>Undo</button>{/if}
            {#if a.retry}<button class="btn sm" onclick={() => outbox.retry(it.id)}>{it.state === "interrupted" || it.state === "scheduled" ? "Send now" : "Retry"}</button>{/if}
            {#if a.edit}<button class="btn ghost sm" onclick={() => outbox.restore(it.id)}>Edit</button>{/if}
            {#if a.discard}<button class="btn ghost sm" onclick={() => outbox.discard(it.id)}>Discard</button>{/if}
          </div>
        </li>
      {/each}
    </ul>
    <p class="note">MiraMail never resends a message on its own after a restart, and stops retrying after three automatic attempts. Scheduled messages are sent only while MiraMail is open; if their time passed while it was closed, you are asked first.</p>
  </Modal>
{/if}

<style>
  .bar { position: fixed; bottom: 66px; left: 50%; transform: translateX(-50%); z-index: 119; display: flex; align-items: center; gap: 14px; background: var(--surface); border: 1px solid var(--line); border-left: 3px solid var(--accent); border-radius: var(--radius); padding: 8px 12px; font-size: 14px; max-width: min(560px, 90vw); box-shadow: 0 6px 24px color-mix(in srgb, #000 35%, transparent); }
  .list { list-style: none; margin: 0; padding: 0; display: grid; gap: 10px; }
  .it { border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; display: grid; gap: 6px; }
  .it.bad { border-left: 3px solid var(--danger); }
  .who { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .to { color: var(--ink); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sub { color: var(--muted); font-size: 0.85rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .st { font-size: 0.85rem; color: var(--text); }
  .er { font-size: 0.8rem; color: var(--danger); }
  .acts { display: flex; gap: 8px; flex-wrap: wrap; }
  .note { margin: 10px 0 0; color: var(--muted); font-size: 0.8rem; }
</style>
