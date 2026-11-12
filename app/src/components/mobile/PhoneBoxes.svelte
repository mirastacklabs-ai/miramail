<script lang="ts">
  import { app } from "../../lib/state.svelte";
  import { org } from "../../lib/org.svelte";
  import { phone } from "../../lib/phone.svelte";
  import { outbox } from "../../lib/outbox.svelte";
  import { needsAttention } from "../../lib/outboxmodel";
  import { snoozeUi } from "../../lib/t2.svelte";
  import { workspace } from "../../lib/workspace.svelte";
  import { contactsUi } from "../../lib/contacts.svelte";
  import { isMovable } from "../../lib/foldermove";
  import { folderMenu, folderTree, openList } from "../../lib/phonemodel";
  import { justSwiped } from "../../lib/phonegestures";
  import { longPress } from "../../lib/phonelongpress";
  import { replyFresh } from "../../lib/compose";
  import BottomSheet from "./BottomSheet.svelte";
  import Logo from "../Logo.svelte";

  let collapsed = $state<Record<string, boolean>>({});
  const unread = $derived(app.unreadByFolder[app.account ?? ""] ?? {});
  const rows = $derived(folderTree(app.folders, collapsed, unread));

  // ---- quick entries (shown only while there is something) ----
  const obN = $derived(outbox.items.length);
  const obWarn = $derived(needsAttention(outbox.items));
  const obSched = $derived(outbox.items.filter((i) => i.state === "scheduled").length);
  const szN = $derived(snoozeUi.items.length);
  const szBad = $derived(snoozeUi.items.filter((s) => s.state === "failed").length);
  const tray = $derived(workspace.tray);
  const trayLabel = (k: string) => (k === "compose" ? "Mail" : k === "event" ? "Event" : k === "aichat" ? "AI" : k);
  async function discard(id: string, title: string) {
    if (await app.ask({ title: "Discard this draft?", body: `\u201c${title}\u201d will be deleted.`, ok: "Discard", danger: true })) workspace.close(id);
  }

  // ---- saved searches ----
  let editSaved = $state(false);
  $effect(() => { if (!org.saved.length) editSaved = false; });
  function runSaved(s: (typeof org.saved)[number]) { org.applySaved(s); phone.nav = openList(phone.nav); }

  // ---- folder long-press sheet ----
  let menuFor = $state<string | null>(null);
  const menuF = $derived(menuFor ? app.folders.find((f) => f.name === menuFor) : undefined);
  const items = $derived(menuF ? folderMenu(menuF, { inboxName: app.inboxName, live: app.isLive(menuF.name), movable: isMovable(app.folders, menuF), subscribed: org.isSubscribed(menuF.name) }) : []);
  function openMenu(name: string) { menuFor = name; if (org.isSubscribed(name) === null) void org.loadSubs(); }
  function run(id: string) {
    const f = menuF; menuFor = null;
    if (!f) return;
    if (id === "live") void app.toggleLive(f.name);
    else if (id === "sub") void org.createFolder(f.name);
    else if (id === "rename") void org.renameFolder(f);
    else if (id === "move") void org.moveFolderPicked(f);
    else if (id === "delete") void org.deleteFolder(f);
    else if (id === "empty") void org.emptyFolder(f);
    else if (id === "subscribe") void org.toggleSubscribe(f.name);
  }
  function openRow(e: Event, name: string) { if (justSwiped(e.currentTarget as HTMLElement)) return; void phone.openFolder(name); }
</script>
<section class="screen">
  <header class="bar">
    <span class="brand"><Logo size={16} word={false} /></span>
    <h1>Mailboxes</h1>
    <button class="ibtn" aria-label="New folder" onclick={() => org.createFolder(null)}><svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg></button>
    <button class="ibtn" aria-label="New message" onclick={() => replyFresh()}><svg viewBox="0 0 24 24"><path d="M4 20l4-1 11-11-3-3L5 16zM14 6l3 3"/></svg></button>
  </header>
  <div class="scroll">
    <ul class="top">
      {#if obN}<li><button class="row" onclick={() => (outbox.open = true)}>
        <span class="k mono">Outbox</span><span class="grow ell">{obWarn ? `${obWarn} need${obWarn === 1 ? "s" : ""} attention` : obSched === obN ? `${obSched} scheduled` : "Sending…"}</span>
        <span class="n mono" class:warn={obWarn > 0}>{obN}</span><svg class="chev" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></button></li>{/if}
      {#if szN}<li><button class="row" onclick={() => (snoozeUi.listOpen = true)}>
        <span class="k mono">Snoozed</span><span class="grow ell">{szBad ? `${szBad} could not return` : "Coming back later"}</span>
        <span class="n mono" class:warn={szBad > 0}>{szN}</span><svg class="chev" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></button></li>{/if}
      <li><button class="row" onclick={() => contactsUi.show()}><span class="grow">Contacts</span><svg class="chev" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></button></li>
    </ul>

    {#if tray.length}
      <div class="label grp">Drafts <span class="mono n">{tray.length}</span></div>
      <ul>
        {#each tray as it (it.id)}
          <li class="split">
            <button class="row" onclick={() => (it.parked ? workspace.unpark(it.id) : workspace.focus(it.id))}>
              <span class="k mono">{trayLabel(it.kind)}</span><span class="grow ell">{it.title}</span>
              {#if it.dirty}<span class="udot" role="img" aria-label="Unsent changes"></span>{/if}
              {#if !it.parked}<span class="mono st">open</span>{/if}
            </button>
            <button class="x" aria-label="Discard {it.title}" onclick={() => discard(it.id, it.title)}><svg viewBox="0 0 24 24"><path d="M7 7l10 10M17 7L7 17"/></svg></button>
          </li>
        {/each}
      </ul>
    {/if}

    <div class="label grp">Accounts</div>
    <ul>
      {#each app.accounts as a (a.email)}
        <li><button class="row" class:cur={a.email === app.account} onclick={() => app.selectAccount(a.email)}>
          <span class="dot {app.status[a.email] ?? 'unknown'}" role="img" aria-label="Connection {app.status[a.email] ?? 'unknown'}"></span>
          <span class="mono ell grow">{a.email}</span>{#if a.email === app.account}<span class="check">&#10003;</span>{/if}
        </button></li>
      {/each}
    </ul>

    {#if org.saved.length}
      <div class="label grp sh">Saved searches<button class="edit" onclick={() => (editSaved = !editSaved)}>{editSaved ? "Done" : "Edit"}</button></div>
      <ul>
        {#each org.saved as s (s.id)}
          <li class="split">
            <button class="row" onclick={() => runSaved(s)}>
              <svg class="mag" viewBox="0 0 24 24"><circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5"/></svg>
              <span class="grow ell">{s.name}</span><span class="mono st">{s.scope === "folder" ? "folder" : s.scope === "account" ? "account" : "all"}</span>
            </button>
            {#if editSaved}<button class="x del" aria-label="Delete saved search {s.name}" onclick={() => org.deleteSaved(s.id)}><svg viewBox="0 0 24 24"><path d="M7 7l10 10M17 7L7 17"/></svg></button>{/if}
          </li>
        {/each}
      </ul>
    {/if}

    <div class="label grp">Folders</div>
    {#if rows.length === 0}
      {#each [0, 1, 2, 3, 4] as i (i)}<div class="skeleton sk"></div>{/each}
    {:else}
      <ul>
        {#each rows as r (r.name)}
          {@const ls = r.selectable ? app.liveStateOf(r.name) : undefined}
          <li class="split" use:longPress={{ enabled: () => r.selectable, onLong: () => openMenu(r.name) }}>
            {#if r.hasKids}
              <button class="tw" style="margin-left:{r.depth * 18}px" aria-label="{r.collapsed ? 'Expand' : 'Collapse'} {r.label}" aria-expanded={!r.collapsed} onclick={() => (collapsed[r.name] = !r.collapsed)}>
                <svg viewBox="0 0 24 24" class:rot={!r.collapsed}><path d="M9 6l6 6-6 6"/></svg>
              </button>
            {/if}
            {#if r.selectable}
              <button class="row" style="padding-left:{r.hasKids ? 4 : 16 + r.depth * 18}px" onclick={(e) => openRow(e, r.name)}>
                <span class="grow ell">{r.label}</span>
                {#if ls && ls.state !== "off"}<span class="live {ls.state}" role="img" aria-label="Live: {ls.state}"></span>{/if}
                {#if r.count > 0}<span class="unread"><span class="udot"></span><span class="mono">{r.count}</span></span>{/if}
                <svg class="chev" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg>
              </button>
            {:else}<div class="row head" style="padding-left:{r.hasKids ? 4 : 16 + r.depth * 18}px"><span class="ell grow">{r.label}</span>{#if r.count > 0}<span class="unread"><span class="udot"></span><span class="mono">{r.count}</span></span>{/if}</div>{/if}
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  {#if menuF}
    <BottomSheet title={menuF.role === "other" ? menuF.name.split(/[/.]/).pop() ?? menuF.name : menuF.role[0].toUpperCase() + menuF.role.slice(1)} onclose={() => (menuFor = null)}>
      <div class="list">
        {#each items as it (it.id)}<button class="mi" class:danger={it.danger} disabled={it.disabled} onclick={() => run(it.id)}>{it.label}</button>{/each}
      </div>
    </BottomSheet>
  {/if}
</section>
<style>
  .screen { position: relative; flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .bar { flex: none; display: flex; align-items: center; gap: 10px; padding: 6px 12px; min-height: 52px; border-bottom: 1px solid var(--line); background: var(--bg-band); }
  h1 { font-size: 1.1rem; flex: 1; letter-spacing: .08em; }
  .ibtn { width: 44px; height: 44px; background: none; border: 0; color: var(--accent); display: grid; place-items: center; }
  .ibtn svg { width: 22px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .scroll { flex: 1; min-height: 0; overflow-y: auto; -webkit-overflow-scrolling: touch; padding-bottom: 16px; }
  .grp { padding: 16px 16px 6px; }
  ul { list-style: none; margin: 0; padding: 0; border-top: 1px solid var(--line); }
  li { border-bottom: 1px solid var(--line); }
  .row { width: 100%; min-height: 48px; display: flex; align-items: center; gap: 10px; background: none; border: 0; padding: 8px 16px; text-align: left; color: var(--ink); font-size: 1rem; }
  .row.head { color: var(--muted); text-transform: uppercase; letter-spacing: .1em; font-size: 0.8rem; }
  .row.cur { color: var(--accent); }
  .grow { flex: 1; min-width: 0; } .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chev { width: 16px; height: 16px; fill: none; stroke: var(--muted); stroke-width: 1.7; flex: none; }
  .unread { display: inline-flex; align-items: center; gap: 6px; color: var(--muted); }
  .udot { width: 8px; height: 8px; border-radius: 50%; background: var(--accent); }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--muted); flex: none; }
  .dot.connected { background: var(--accent); } .dot.reconnecting { background: var(--signal); } .dot.offline { background: transparent; border: 1.5px solid var(--muted); }
  .sk { height: 20px; margin: 14px 16px; }
  .top { border-top: 0; }
  li.split { display: flex; align-items: stretch; }
  li.split > .row { flex: 1; min-width: 0; }
  .tw { flex: none; width: 44px; min-height: 48px; background: none; border: 0; display: grid; place-items: center; color: var(--muted); }
  .tw svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; transition: transform .15s; }
  .tw svg.rot { transform: rotate(90deg); }
  .k { color: var(--accent); font-size: 0.72rem; flex: none; }
  .n { color: var(--accent); } .n.warn { color: var(--danger); }
  .st { color: var(--muted); font-size: 0.72rem; flex: none; }
  .x { flex: none; width: 48px; background: none; border: 0; color: var(--muted); display: grid; place-items: center; }
  .x.del { color: var(--danger); }
  .x svg { width: 18px; height: 18px; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; }
  .mag { width: 18px; height: 18px; fill: none; stroke: var(--muted); stroke-width: 1.7; stroke-linecap: round; flex: none; }
  .sh { display: flex; align-items: center; justify-content: space-between; padding-right: 4px; }
  .edit { min-height: 44px; min-width: 44px; background: none; border: 0; color: var(--accent); font-size: 0.8rem; text-transform: uppercase; letter-spacing: .08em; }
  .live { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex: none; }
  .live.error { background: var(--danger); }
  .list { display: grid; }
  .mi { min-height: 48px; display: flex; align-items: center; background: none; border: 0; border-bottom: 1px solid var(--line); text-align: left; padding: 0 8px; color: var(--ink); font-size: 1rem; }
  .mi.danger { color: var(--signal); } .mi:disabled { opacity: .45; }
</style>
