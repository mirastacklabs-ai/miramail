<script lang="ts">
  import { thisDevice } from "../lib/device";
  // Preferences > Senders: VIP senders, blocked senders, muted conversations. All local to this Mac.
  import { onMount } from "svelte";
  import { local } from "../lib/localorg.svelte";
  import { platformPrefs } from "../lib/platformprefs.svelte";
  import { app } from "../lib/state.svelte";

  let vipAddr = $state(""), blockAddr = $state("");
  onMount(() => { void local.loadSenders(); void platformPrefs.load(); });
  const when = (ts: number) => new Date(ts * 1000).toLocaleDateString([], { dateStyle: "medium" });
  async function addVip() { const a = vipAddr.trim(); if (!a) return; await local.addVip(a); if (local.vipSet.has(a.toLowerCase()) || local.vip.length) vipAddr = ""; }
  async function addBlock() {
    const a = blockAddr.trim(); if (!a) return;
    const before = local.blocked.length;
    await local.setBlocked(a, true);
    if (local.blocked.length > before) { blockAddr = ""; app.notify(`Blocked ${a.toLowerCase()}.`); }
  }
</script>

<section aria-labelledby="sd-h">
  <h3 id="sd-h">Senders and conversations</h3>
  <p class="note">These lists live only on {thisDevice()}, in MiraMail's own database. Nothing is sent to your mail server or anywhere else.</p>

  <h4>VIP senders</h4>
  <p class="note">Mail from a VIP always notifies, even for accounts set to “VIP only” (Preferences > Notifications). Add one from a message's right-click menu, or type an address here.</p>
  <form class="add" novalidate onsubmit={(e) => { e.preventDefault(); void addVip(); }}>
    <label class="sr-only" for="vip-a">VIP email address</label>
    <input id="vip-a" type="email" placeholder="name@example.com" bind:value={vipAddr} autocomplete="off" spellcheck="false" />
    <button class="btn ghost sm" type="submit" disabled={!vipAddr.trim()}>Add VIP</button>
  </form>
  {#if local.vip.length}
    <ul class="list">
      {#each local.vip as v (v.address)}
        <li><span class="a ell">{v.name ? `${v.name} · ` : ""}<span class="mono">{v.address}</span></span><button class="btn ghost sm" onclick={() => local.setVip({ from: { name: v.name || null, address: v.address } }, false)}>Remove</button></li>
      {/each}
    </ul>
  {:else}<p class="empty">No VIP senders yet.</p>{/if}

  <h4>Blocked senders</h4>
  <p class="note">New mail from a blocked address that arrives in an Inbox <strong>while MiraMail is open</strong> is moved to the account's Junk folder. It is never deleted, and mail that arrives while MiraMail is closed stays where the server put it.</p>
  <form class="add" novalidate onsubmit={(e) => { e.preventDefault(); void addBlock(); }}>
    <label class="sr-only" for="blk-a">Email address to block</label>
    <input id="blk-a" type="email" placeholder="spammer@example.com" bind:value={blockAddr} autocomplete="off" spellcheck="false" />
    <button class="btn ghost sm" type="submit" disabled={!blockAddr.trim()}>Block</button>
  </form>
  {#if local.blocked.length}
    <ul class="list">
      {#each local.blocked as b (b.address)}
        <li><span class="a ell"><span class="mono">{b.address}</span> <span class="when">since {when(b.addedAt)}</span></span><button class="btn ghost sm" onclick={() => local.setBlocked(b.address, false)}>Unblock</button></li>
      {/each}
    </ul>
  {:else}<p class="empty">No blocked senders.</p>{/if}

  <h4>Muted conversations</h4>
  <p class="note">Mute a conversation from a message's right-click menu. Muted conversations never notify and do not count in the unread badge.</p>
  <label class="chk"><input type="checkbox" checked={platformPrefs.v.muteAutoRead} disabled={!platformPrefs.loaded} onchange={(e) => platformPrefs.patch({ muteAutoRead: e.currentTarget.checked })} /> Mark new mail in muted conversations as read when it arrives (while MiraMail is open)</label>
</section>

<style>
  section { display: grid; gap: 10px; }
  h3 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; text-transform: uppercase; color: var(--muted); }
  h4 { margin: 10px 0 0; font-size: 0.9333rem; font-weight: 600; color: var(--ink); }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .empty { margin: 0; font-size: 0.8667rem; color: var(--muted); }
  .add { display: flex; gap: 8px; }
  .add input { flex: 1; min-width: 0; background: transparent; color: var(--ink); border: 1px solid var(--line); border-radius: var(--radius); padding: 6px 10px; font: 400 0.8667rem var(--font-mono); }
  .list { list-style: none; margin: 0; padding: 0; display: grid; gap: 2px; max-height: 220px; overflow: auto; border: 1px solid var(--line); border-radius: var(--radius); }
  .list li { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 4px 8px; }
  .list li + li { border-top: 1px solid var(--line); }
  .a { min-width: 0; font-size: 0.8667rem; }
  .when { color: var(--muted); font-size: 0.75rem; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chk { display: flex; gap: 8px; align-items: flex-start; font-size: 0.9333rem; }
  .chk input { accent-color: var(--accent); margin-top: 3px; }
</style>
