<script lang="ts">
  import { phone } from "../../lib/phone.svelte";
  import { app } from "../../lib/state.svelte";
  import type { Tab } from "../../lib/phonemodel";
  import { PANE_ICONS } from "../../lib/wstree"; // the same icons the Mac panes use
  const inboxUnread = $derived(app.unreadByFolder[app.account ?? ""]?.[app.inboxName] ?? 0);
  const TABS: { id: Tab; label: string; d: string }[] = [
    { id: "mail", label: "Mail", d: PANE_ICONS.mailbox },
    { id: "mira", label: "MIRA", d: "M12 3l1.8 4.7L18.500 9.500l-4.700 1.800L12 16l-1.800-4.700L5.500 9.500l4.700-1.800zM18 15l.8 2.200L21 18l-2.200.8L18 21l-.8-2.200L15 18l2.200-.8z" },
    { id: "calendar", label: "Calendar", d: PANE_ICONS.calendar },
    { id: "settings", label: "Settings", d: "M12 9a3 3 0 100 6 3 3 0 000-6zM12 2v3M12 19v3M2 12h3M19 12h3M4.900 4.900l2.100 2.100M17 17l2.100 2.100M4.900 19.100L7 17M17 7l2.100-2.100" },
  ];
</script>
<nav class="tabs" aria-label="Main">
  {#each TABS as t (t.id)}
    <button class:on={phone.nav.tab === t.id} aria-current={phone.nav.tab === t.id ? "page" : undefined} onclick={() => phone.tab(t.id)}>
      <svg viewBox="0 0 24 24" aria-hidden="true"><path d={t.d} /></svg>
      <span>{t.label}</span>
      {#if t.id === "mail" && inboxUnread > 0}<i class="badge mono">{inboxUnread > 99 ? "99+" : inboxUnread}</i>{/if}
    </button>
  {/each}
</nav>
<style>
  .tabs { flex: none; display: grid; grid-template-columns: repeat(4, 1fr); background: var(--bg-band); border-top: 1px solid var(--line); padding-bottom: env(safe-area-inset-bottom); }
  button { position: relative; min-height: 50px; background: none; border: 0; border-top: 2px solid transparent; color: var(--muted); display: grid; place-items: center; align-content: center; gap: 2px; padding: 4px 0; }
  button.on { color: var(--ink); border-top-color: var(--accent); }
  svg { width: 22px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  span { font-size: 0.6875rem; text-transform: uppercase; letter-spacing: .1em; font-weight: 500; }
  .badge { position: absolute; top: 3px; left: calc(50% + 6px); font-style: normal; font-size: 0.65rem; background: var(--signal); color: #0a0a0a; border-radius: 999px; padding: 0 5px; min-width: 16px; text-align: center; }
</style>
