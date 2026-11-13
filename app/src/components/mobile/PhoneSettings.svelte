<script lang="ts">
  import { app } from "../../lib/state.svelte";
  import { phone } from "../../lib/phone.svelte";
  import Settings from "../Settings.svelte";
  import InboxCountSection from "../InboxCountSection.svelte";
  import WidgetsSection from "../WidgetsSection.svelte";
  import BottomSheet from "./BottomSheet.svelte";
  import { cal } from "../../lib/cal.svelte";
  import { onMount } from "svelte";
  import { platformPrefs } from "../../lib/platformprefs.svelte";
  const ALL: { title: string; items: { id: string; label: string }[] }[] = [
    { title: "Mail", items: [{ id: "general", label: "General" }, { id: "appearance", label: "Appearance" }, { id: "composing", label: "Composing" }, { id: "signatures", label: "Signatures" }, { id: "vacation", label: "Vacation reply" }, { id: "sync", label: "Sync" }, { id: "senders", label: "Senders" }, { id: "rules", label: "Rules" }] },
    { title: "Privacy and alerts", items: [{ id: "privacy", label: "Privacy" }, { id: "notifications", label: "Notifications" }, { id: "widgets", label: "Widgets" }] },
    { title: "MIRA and about", items: [{ id: "ai", label: "MIRA (AI)" }, { id: "about", label: "About" }] },
  ];
  let acct = $state<string | null>(null); // account whose sheet is open
  const openZoom = () => { void cal.refreshZoom(); cal.zoomOpen = true; };
  const removeAcct = async (email: string) => { acct = null; await app.removeAccount(email); };
  onMount(() => { if (!platformPrefs.loaded) void platformPrefs.load(); });
  // The Notifications entry is hidden only where the backend has no notification support (platform_info.notifications = false).
  const GROUPS = $derived(ALL.map((g) => ({ ...g, items: g.items.filter((i) => i.id !== "notifications" || platformPrefs.notificationsAvailable) })));
  const label = $derived(GROUPS.flatMap((g) => g.items).find((i) => i.id === phone.nav.section)?.label ?? "");
</script>
<section class="screen">
  <header class="bar">
    {#if phone.nav.section}
      <button class="back" onclick={() => phone.back()} aria-label="Settings"><svg viewBox="0 0 24 24"><path d="M15 6l-6 6 6 6"/></svg><span>Settings</span></button>
      <h1>{label}</h1><span></span>
    {:else}<span></span><h1>Settings</h1><span></span>{/if}
  </header>
  <div class="scroll">
    {#if phone.nav.section}
      {#key phone.nav.section}<div class="sec">
        {#if phone.nav.section === "general"}<InboxCountSection />{:else if phone.nav.section === "widgets"}<WidgetsSection />{:else}<Settings section={phone.nav.section} />{/if}
      </div>{/key}
    {:else}
      <div class="label grp">Accounts</div>
      <ul>
        {#each app.accounts as a (a.email)}<li><button class="row" aria-haspopup="dialog" onclick={() => (acct = a.email)}><span class="mono ell">{a.email}</span><svg class="chev" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></button></li>{/each}
        <li><button class="row" onclick={() => (app.addingAccount = true)}><span class="acc">Add account</span></button></li>
      </ul>
      {#each GROUPS as g (g.title)}
        <div class="label grp">{g.title}</div>
        <ul>
          {#each g.items as i (i.id)}
            <li><button class="row" onclick={() => phone.section(i.id)}><span>{i.label}</span><svg class="chev" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></button></li>
          {/each}
        </ul>
      {/each}
      <div class="label grp">Integrations</div>
      <ul>
        <li><button class="row" onclick={openZoom}><span>Zoom</span><span class="st">{cal.zoom?.connected ? "Connected" : "Not connected"}</span></button></li>
      </ul>
    {/if}
  </div>
  {#if acct}
    <BottomSheet title="Account" onclose={() => (acct = null)}>
      <p class="who mono">{acct}</p>
      <button class="row danger" onclick={() => acct && removeAcct(acct)}>Remove this account</button>
    </BottomSheet>
  {/if}
</section>
<style>
  .screen { flex: 1; min-height: 0; display: flex; flex-direction: column; position: relative; }
  .bar { flex: none; display: grid; grid-template-columns: 1fr auto 1fr; align-items: center; padding: 6px 4px; min-height: 52px; border-bottom: 1px solid var(--line); background: var(--bg-band); }
  h1 { font-size: 1.05rem; letter-spacing: .08em; text-align: center; }
  .back { justify-self: start; min-height: 44px; display: inline-flex; align-items: center; background: none; border: 0; color: var(--accent); font-size: 0.95rem; padding: 0 8px 0 2px; }
  .back svg { width: 22px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .scroll { flex: 1; min-height: 0; overflow-y: auto; -webkit-overflow-scrolling: touch; padding-bottom: 24px; }
  .sec { padding: 14px; }
  .grp { padding: 18px 16px 6px; }
  ul { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); }
  li { border-bottom: 1px solid var(--line); }
  .row { width: 100%; min-height: 48px; display: flex; align-items: center; justify-content: space-between; gap: 10px; background: none; border: 0; padding: 8px 16px; text-align: left; color: var(--ink); font-size: 1rem; }
  .st { color: var(--muted); font-size: 0.85rem; }
  .who { margin: 4px 4px 8px; overflow-wrap: anywhere; color: var(--muted); }
  .danger { color: var(--danger); justify-content: center; border: 1px solid var(--line); border-radius: 10px; }
  .acc { color: var(--accent); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chev { width: 16px; height: 16px; fill: none; stroke: var(--muted); stroke-width: 1.7; flex: none; }
</style>
