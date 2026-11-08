<script lang="ts">
  let { hasAttendees, onconfirm, oncancel }: { hasAttendees: boolean; onconfirm: (notify: boolean) => void; oncancel: () => void } = $props();
  let notify = $state(true);
</script>
<div class="del" role="alertdialog" aria-label="Confirm delete" onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); oncancel(); } }} tabindex="-1">
  <p>Delete this event? Recurring events are deleted entirely. This cannot be undone.</p>
  {#if hasAttendees}<label class="chk"><input type="checkbox" bind:checked={notify} /> Notify attendees</label>{/if}
  <div class="acts">
    <button class="btn ghost sm" onclick={oncancel}>Cancel</button>
    <!-- svelte-ignore a11y_autofocus -->
    <button class="btn sm warn" autofocus onclick={() => onconfirm(hasAttendees && notify)}>Delete</button>
  </div>
</div>
<style>
  .del { border: 1px solid var(--line); border-left: 2px solid var(--signal); border-radius: var(--radius); padding: 10px 12px; display: grid; gap: 8px; background: var(--bg); }
  p { margin: 0; font-size: 13px; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 13px; }
  .acts { display: flex; gap: 8px; justify-content: flex-end; }
  .warn { background: var(--signal); border-color: var(--signal); color: #0a0a0a; }
</style>
