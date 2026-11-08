<script lang="ts">
  // Shown by a <svelte:boundary> when a view throws while rendering: the message and a Retry, never stale content.
  import { describeError } from "../lib/errors";

  let { error, reset, what }: { error: unknown; reset: () => void; what: string } = $props();
  const msg = $derived(describeError(error));
</script>

<div class="rerr" role="alert">
  <strong>{what} could not be shown.</strong>
  <p class="mono selectable">{msg}</p>
  <div><button class="btn ghost sm" onclick={reset}>Retry</button></div>
</div>

<style>
  .rerr { margin: 12px; display: grid; gap: 8px; border: 1px solid var(--line); border-left: 3px solid var(--signal); border-radius: var(--radius); padding: 10px 12px; font-size: 0.8667rem; }
  .rerr p { margin: 0; color: var(--muted); overflow-wrap: anywhere; font-size: 0.8rem; }
</style>
