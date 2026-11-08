<script lang="ts">
  import { app } from "../lib/state.svelte";
  import { org } from "../lib/org.svelte";

  const b = $derived(app.body);
  const addr = $derived(b?.summary.from?.address);
  const allowed = $derived(org.senderAllowed(addr));
</script>

{#if addr}
  {#if allowed && !app.imagesAllowed && b && b.blockedRemoteCount > 0}
    <p class="note"><span class="dot"></span>Images from {addr} are not loaded automatically because your mail server could not verify this message really came from that domain.
      <button class="lnk" onclick={() => void app.open(b.summary.uid, true, true)}>Load images once</button>
      <button class="lnk" onclick={() => org.disallowSender(addr)}>Stop</button></p>
  {:else if allowed}
    <p class="note"><span class="dot"></span>Images load automatically for {addr}. Tracking images are still blocked.
      <button class="lnk" onclick={() => org.disallowSender(addr)}>Stop</button></p>
  {:else if !app.imagesAllowed && b && b.blockedRemoteCount > 0}
    <p class="note"><button class="lnk" onclick={() => { org.allowSender(addr); void app.open(b.summary.uid, true, true); }}>Always load images from {addr}</button></p>
  {/if}
{/if}

<style>
  .note { margin: 0; font-size: 0.8333rem; color: var(--muted); display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--accent); flex: none; }
  .lnk { background: none; border: 0; padding: 0; color: var(--accent); text-decoration: underline; cursor: pointer; font: inherit; overflow-wrap: anywhere; }
</style>
