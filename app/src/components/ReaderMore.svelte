<script lang="ts">
  import { app } from "../lib/state.svelte";
  import { org } from "../lib/org.svelte";
  import { snoozeUi } from "../lib/t2.svelte";

  let open = $state(false);
  const b = $derived(app.body);
  const role = $derived(app.folders.find((f) => f.name === app.folder)?.role);
  const sender = $derived(b?.summary.from?.address);
  function run(fn: () => void) { open = false; fn(); }
</script>

<div class="more">
  <button class="btn ghost sm" aria-haspopup="menu" aria-expanded={open} onclick={() => (open = !open)}>More ▾</button>
  {#if open}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="scrim" onmousedown={() => (open = false)} onkeydown={(e) => e.key === "Escape" && (open = false)}></div>
    <div class="menu" role="menu">
      {#if role !== "trash" && b}<button role="menuitem" onclick={() => run(() => snoozeUi.open(b.summary.uid))}>Snooze…</button>{/if}
      <button role="menuitem" onclick={() => run(() => org.printing++)}>Print…</button>
      <button role="menuitem" onclick={() => run(() => void org.saveEml())}>Save as .eml</button>
      <button role="menuitem" onclick={() => run(() => void org.viewSource("headers"))}>View full headers</button>
      <button role="menuitem" onclick={() => run(() => void org.viewSource("raw"))}>View raw source</button>
      <hr />
      {#if role === "junk"}<button role="menuitem" onclick={() => run(() => void app.moveTo("inbox"))}>Not Junk</button>
      {:else if role !== "trash"}<button role="menuitem" onclick={() => run(() => void app.moveTo("junk"))}>Mark as Junk</button>{/if}
      {#if sender}
        {#if org.senderAllowed(sender)}<button role="menuitem" onclick={() => run(() => org.disallowSender(sender))}>Stop auto-loading images from {sender}</button>
        {:else}<button role="menuitem" onclick={() => run(() => { org.allowSender(sender); void app.open(b!.summary.uid, true, true); })}>Always load images from {sender}</button>{/if}
      {/if}
    </div>
  {/if}
</div>

<style>
  .more { position: relative; }
  .scrim { position: fixed; inset: 0; z-index: 80; }
  .menu { position: absolute; right: 0; top: calc(100% + 4px); z-index: 81; min-width: 230px; max-width: 340px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); padding: 4px; }
  .menu button { display: block; width: 100%; text-align: left; background: none; border: 0; padding: 7px 10px; color: var(--ink); font-size: 0.8667rem; cursor: pointer; overflow-wrap: anywhere; }
  .menu button:hover { background: color-mix(in srgb, var(--line) 50%, transparent); }
  hr { border: 0; border-top: 1px solid var(--line); margin: 4px 0; }
</style>
