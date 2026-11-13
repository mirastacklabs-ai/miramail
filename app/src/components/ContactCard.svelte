<script lang="ts">
  import { thisDevice } from "../lib/device";
  // Small card next to the sender in the Reader: what the address book knows, how much mail is cached from them,
  // "Add to contacts" and "Compose to". Looks only at the local address book and the local mail cache.
  import { contactsApi } from "../lib/contactsapi";
  import { contactsUi, mailbox } from "../lib/contacts.svelte";
  import { workspace } from "../lib/workspace.svelte";
  import { fullDate } from "../lib/util";
  import type { ContactCardInfo } from "../lib/contactstypes";

  let { name, address }: { name: string | null; address: string } = $props();
  let open = $state(false), info = $state<ContactCardInfo | null>(null), failed = $state(false);
  let seq = 0;

  async function toggle() {
    open = !open;
    if (!open) return;
    const mine = ++seq;
    info = null; failed = false;
    try { const r = await contactsApi.card(address); if (mine === seq) info = r; } catch { if (mine === seq) failed = true; }
  }
  function compose() { open = false; workspace.openCompose({ to: [mailbox(name, address)], cc: [], bcc: [], subject: "", text: "", inReplyTo: null }); }
  function add() { open = false; contactsUi.show({ name: name ?? "", email: address }); }
  function edit(id: number) { open = false; contactsUi.show(null); contactsUi.editing = id; }
  function key(e: KeyboardEvent) { if (e.key === "Escape" && open) { e.stopPropagation(); open = false; } }
</script>

<span class="cc" onkeydown={key} role="presentation">
  <button type="button" class="tog" aria-expanded={open} aria-label="Contact card for {address}" title="Contact card" onclick={toggle}>
    <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="8" r="3.5"/><path d="M5 20c0-3.6 3-6 7-6s7 2.4 7 6"/></svg>
  </button>
  {#if open}
    <div class="pop" role="dialog" aria-label="Contact card">
      <strong class="selectable">{info?.contact?.name || name || address}</strong>
      <span class="mono dim selectable">{address}</span>
      {#if failed}
        <span class="dim">The address book couldn't be read.</span>
      {:else if !info}
        <span class="dim">Loading…</span>
      {:else}
        {#if info.contact}
          {#if info.contact.org}<span class="dim selectable">{info.contact.org}</span>{/if}
          {#each info.contact.phones as p (p)}<span class="mono selectable">{p}</span>{/each}
          {#if info.contact.groups.length}<span class="dim">Groups: {info.contact.groups.join(", ")}</span>{/if}
        {:else}
          <span class="dim">Not in your contacts.</span>
        {/if}
        <span class="dim">{info.messages} message{info.messages === 1 ? "" : "s"} from them on {thisDevice()}{info.newest ? `, latest ${fullDate(info.newest)}` : ""}</span>
      {/if}
      <span class="rw">
        {#if info?.contact}
          <button class="btn ghost sm" onclick={() => edit(info!.contact!.id)}>Edit contact</button>
        {:else}
          <button class="btn sm" onclick={add}>Add to contacts</button>
        {/if}
        <button class="btn ghost sm" onclick={compose}>Compose to</button>
      </span>
    </div>
  {/if}
</span>

<style>
  .cc { position: relative; display: inline-block; vertical-align: middle; margin-left: 6px; }
  .tog { background: none; border: 1px solid var(--line); border-radius: var(--radius); padding: 1px 3px; color: var(--muted); line-height: 0; }
  .tog:hover, .tog[aria-expanded="true"] { color: var(--ink); border-color: var(--accent); }
  .tog svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.6; }
  .pop { position: absolute; top: 100%; left: 0; z-index: 30; margin-top: 4px; min-width: 240px; max-width: 340px; display: grid; gap: 4px; padding: 10px 12px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); box-shadow: 0 6px 24px rgba(0, 0, 0, .25); font: 400 0.8rem var(--font-body, inherit); text-align: left; }
  .dim { color: var(--muted); }
  .rw { display: flex; gap: 6px; margin-top: 4px; }
</style>
