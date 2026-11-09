<script lang="ts">
  import { app, liveKey } from "../lib/state.svelte";
  import type { Folder } from "../lib/types";
  import Logo from "./Logo.svelte";
  import { workspace } from "../lib/workspace.svelte";
  import { layout } from "../lib/layout.svelte";
  import { replyFresh } from "../lib/compose";
  import CalPanel from "./CalPanel.svelte";
  import DraftTray from "./DraftTray.svelte";
  import OutboxLink from "./OutboxLink.svelte";
  import SnoozedLink from "./SnoozedLink.svelte";
  import { org } from "../lib/org.svelte";
  import { canDropFolder, isMovable } from "../lib/foldermove";
  import { contactsUi } from "../lib/contacts.svelte";

  const dotTitle = (s?: string) => s ?? "unknown";
  const unreadMap = $derived(app.unreadByFolder[app.account ?? ""] ?? {});
  /** Own unread, plus descendants' when the folder is collapsed (so hidden unread mail still shows a dot). */
  const count = (n: Node) => {
    let c = unreadMap[n.f.name] ?? 0;
    if (n.hasKids && collapsed[n.f.name]) {
      const d = delimOf(n.f);
      for (const [k, v] of Object.entries(unreadMap)) if (k.startsWith(n.f.name + d)) c += v;
    }
    return c;
  };
  const delimOf = (f: Folder) => f.delimiter || (f.name.includes("/") ? "/" : f.name.includes(".") && f.name.toUpperCase().startsWith("INBOX.") ? "." : "/");
  const label = (f: Folder) => (f.role === "other" ? f.name : f.role === "inbox" ? "Inbox" : f.role[0].toUpperCase() + f.role.slice(1));
  const ROLE_ORDER = ["inbox", "drafts", "sent", "archive", "junk", "trash", "other"];
  type Node = { f: Folder; depth: number; leaf: string; hasKids: boolean; selectable: boolean };
  let collapsed = $state<Record<string, boolean>>({});
  // Build a flat, depth-annotated list from IMAP hierarchy (delimiter-separated names).
  const tree = $derived.by(() => {
    const all = [...app.folders];
    const names = new Set(all.map((f) => f.name));
    const top = (f: Folder) => f.name.split(delimOf(f))[0];
    const rank = (f: Folder) => { const r = all.find((x) => x.name === top(f))?.role ?? f.role; return ROLE_ORDER.indexOf(r); };
    all.sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
    const out: Node[] = [];
    for (const f of all) {
      const d = delimOf(f);
      const parts = f.name.split(d);
      // hide if any ancestor is collapsed
      let hidden = false;
      for (let i = 1; i < parts.length; i++) if (collapsed[parts.slice(0, i).join(d)]) hidden = true;
      if (hidden) continue;
      const hasKids = all.some((o) => o.name.startsWith(f.name + d));
      if (!f.selectable && !hasKids) continue;
      out.push({ f, depth: parts.length - 1, leaf: parts[parts.length - 1], hasKids, selectable: f.selectable });
    }
    void names;
    return out;
  });
  let menu = $state<{ x: number; y: number; folder: string } | null>(null);
  const isInbox = (f: Folder) => f.name === app.inboxName;
  const liveTitle = (f: Folder) => (isInbox(f) ? "INBOX is always live" : app.isLive(f.name) ? "Live: keeping a connection open. Click to turn off" : "Keep live (own connection, instant updates)");
  const errOf = (f: Folder) => app.account ? app.liveError[liveKey(app.account, f.name)] : undefined;
  function ctx(e: MouseEvent, f: Folder) {
    e.preventDefault();
    menu = { x: Math.min(e.clientX, window.innerWidth - 220), y: Math.min(e.clientY, window.innerHeight - 260), folder: f.name };
    if (org.isSubscribed(f.name) === null) void org.loadSubs();
  }
  const menuFolder = $derived(menu ? app.folders.find((f) => f.name === menu!.folder) : undefined);
  const special = (f: Folder) => f.role !== "other" || f.name.toUpperCase() === "INBOX";
  const dragged = $derived(org.dragFolder !== null ? app.folders.find((f) => f.name === org.dragFolder) : undefined);
  const folderDropOk = (target: string | null) => !!dragged && canDropFolder(app.folders, dragged, target);
  // A folder drag that never ended (its row was re-rendered away) must not block message drops.
  $effect(() => { void app.folders; void app.folder; org.dragFolder = null; org.folderDrop = null; });
  function endFolderDrag() { org.dragFolder = null; org.folderDrop = null; }
  function dropFolderOn(target: string | null) {
    const f = dragged; endFolderDrag();
    if (f && canDropFolder(app.folders, f, target)) void org.moveFolderDropped(f, target);
  }
  function act(fn: () => void) { menu = null; fn(); }
  const nodeLabel = (n: Node) => (n.depth === 0 ? label(n.f) : n.leaf);
  const themeLabel = $derived(app.theme === "system" ? "Theme: system" : app.theme === "dark" ? "Theme: dark" : app.theme === "contrast" ? "Theme: high contrast" : "Theme: light");
</script>

<aside class="side" aria-label="Accounts and folders" style="width:{layout.sidebar}px">
  <button class="brand" aria-label="About MiraMail" title="About MiraMail" onclick={() => (app.showAbout = true)}><Logo size={16} /></button>

  <button class="btn ghost askai" title="Contacts: your local address book" aria-label="Open contacts" onclick={() => contactsUi.show()}>
    <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="8" r="3.5"/><path d="M5 20c0-3.6 3-6 7-6s7 2.4 7 6"/></svg>
    <span>Contacts</span>
  </button>

  <div class="switch" role="group" aria-label="Application view">
    <button class:on={app.view === "mail"} aria-pressed={app.view === "mail"} title="Mail (g m)" onclick={() => (app.view = "mail")}>Mail</button>
    <button class:on={app.view === "calendar"} aria-pressed={app.view === "calendar"} title="Calendar (g c)" onclick={() => (app.view = "calendar")}>Calendar</button>
  </div>

  <button class="btn ghost askai" title="MIRA, Mirador Intelligent Research Assistant (⌘J)" aria-label="Open MIRA, the AI assistant" onclick={() => workspace.toggleAi({ openOnly: true })}>
    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3l1.8 4.7L18.5 9.5l-4.7 1.8L12 16l-1.8-4.7L5.5 9.5l4.7-1.8z"/><path d="M18 15l.8 2.2L21 18l-2.2.8L18 21l-.8-2.200L15 18l2.200-.8z"/></svg>
    <span>MIRA</span><kbd class="mono">⌘J</kbd>
  </button>

  {#if app.accounts.length > 1}
    <div class="accts" role="group" aria-label="Accounts">
      {#each app.accounts as a (a.email)}
        <button class="acct" class:on={a.email === app.account} aria-pressed={a.email === app.account} onclick={() => app.selectAccount(a.email)} title={a.email}>
          <span class="dot {app.status[a.email] ?? 'unknown'}" role="img" aria-label="Connection {dotTitle(app.status[a.email])}"></span>
          <span class="mono ell">{a.email}</span>
        </button>
      {/each}
    </div>
  {/if}

  {#if app.view === "calendar"}
    <CalPanel />
  {:else}
  <button class="btn compose" onclick={() => replyFresh()}>Compose</button>

  {#if org.saved.length}
    <div class="saved" role="group" aria-label="Saved searches">
      <div class="shead mono">Saved searches</div>
      {#each org.saved as s (s.id)}
        <div class="srow">
          <button class="sbtn ell" title="{s.query} ({s.scope === 'folder' ? 'folder' : s.scope === 'account' ? 'account' : 'all accounts'})" onclick={() => org.applySaved(s)}>
            <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="11" cy="11" r="6"/><path d="M20 20l-4.500-4.500"/></svg><span class="ell">{s.name}</span>
          </button>
          <button class="sx" aria-label="Delete saved search {s.name}" title="Delete" onclick={() => org.deleteSaved(s.id)}>×</button>
        </div>
      {/each}
    </div>
  {/if}

  <nav class="folders" aria-label="Folders">
    {#each tree as n (n.f.name)}
      {@const f = n.f}
      {@const c = count(n)}
      <div class="row" style="padding-left:{n.depth * 14}px">
        {#if n.hasKids}
          <button class="twist" aria-label={collapsed[f.name] ? "Expand" : "Collapse"} aria-expanded={!collapsed[f.name]} onclick={() => (collapsed[f.name] = !collapsed[f.name])}>
            <svg viewBox="0 0 24 24" class:rot={!collapsed[f.name]}><path d="M9 6l6 6-6 6"/></svg>
          </button>
        {:else}<span class="twist sp"></span>{/if}
        {#if n.selectable}
          {@const ls = app.liveStateOf(f.name)}
          <button class="folder" class:on={f.name === app.folder} class:drop={app.dropFolder === f.name || org.folderDrop === f.name} aria-current={f.name === app.folder ? "true" : undefined} onclick={() => app.selectFolder(f.name)} oncontextmenu={(e) => ctx(e, f)}
            draggable={isMovable(app.folders, f)}
            ondragstart={(e) => { org.dragFolder = f.name; if (e.dataTransfer) { e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", f.name); } }}
            ondragend={endFolderDrag}
            ondragover={(e) => { if (org.dragFolder !== null && !app.dragMsg) { if (folderDropOk(f.name)) { e.preventDefault(); if (e.dataTransfer) e.dataTransfer.dropEffect = "move"; org.folderDrop = f.name; } return; } if (app.dragMsg && app.dragMsg.folder !== f.name) { e.preventDefault(); if (e.dataTransfer) e.dataTransfer.dropEffect = "move"; app.dropFolder = f.name; } }}
            ondragleave={() => { if (app.dropFolder === f.name) app.dropFolder = null; if (org.folderDrop === f.name) org.folderDrop = null; }}
            ondrop={(e) => { e.preventDefault(); if (org.dragFolder !== null && !app.dragMsg) { dropFolderOn(f.name); return; } const d = app.dragMsg; app.dropFolder = null; app.dragMsg = null; if (d && d.folder !== f.name) void (d.uids && d.uids.length > 1 ? org.moveUids(d.uids, { folder: f.name }, d.folder) : app.moveToFolder(f.name, d.uid, d.folder)); }}>
            <span class="ell">{nodeLabel(n)}</span>
            {#if ls && ls.state !== "off"}<span class="live {ls.state}" role="img" aria-label="Live: {ls.state}{ls.message ? '. ' + ls.message : ''}" title={ls.message ?? `Live: ${ls.state}`}></span>{/if}
            {#if c > 0}<span class="unread" role="img" aria-label="{c} unread"><span class="udot"></span><span class="mono n" aria-hidden="true">{c}</span></span>{/if}
          </button>
          <button class="keep" class:locked={isInbox(f)} class:active={app.isLive(f.name)} aria-pressed={app.isLive(f.name)} aria-label="Keep {nodeLabel(n)} live"
            title={liveTitle(f)} disabled={isInbox(f) || !!app.liveBusy[liveKey(app.account ?? "", f.name)]} onclick={() => app.toggleLive(f.name)}>
            <svg viewBox="0 0 24 24"><path d="M3 12h4l3-7 4 14 3-7h4"/></svg>
          </button>
        {:else}
          <span class="folder head ell">{nodeLabel(n)}</span>
        {/if}
      </div>
      {#if errOf(f)}
        <div class="liveerr" role="alert" style="margin-left:{n.depth * 14 + 16}px">
          <span class="selectable">{errOf(f)}</span>
          <span class="la"><button class="lnk" onclick={() => app.toggleLive(f.name, true)}>Retry</button><button class="lnk" onclick={() => app.dismissLiveError(f.name)}>Dismiss</button></span>
        </div>
      {/if}
    {:else}
      {#each [0, 1, 2, 3, 4] as i (i)}<div class="skeleton sk"></div>{/each}
    {/each}
  </nav>
  {/if}

  {#if dragged && folderDropOk(null)}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="topzone" class:drop={org.folderDrop === ""} role="group" aria-label="Drop here to move the folder to the top level"
      ondragover={(e) => { e.preventDefault(); if (e.dataTransfer) e.dataTransfer.dropEffect = "move"; org.folderDrop = ""; }}
      ondragleave={() => { if (org.folderDrop === "") org.folderDrop = null; }}
      ondrop={(e) => { e.preventDefault(); dropFolderOn(null); }}>Drop here for top level</div>
  {/if}

  {#if app.view !== "calendar"}<button class="newf" onclick={() => org.createFolder(null)}>+ New folder</button>{/if}

  {#if menu}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="scrimmenu" onmousedown={() => (menu = null)} oncontextmenu={(e) => { e.preventDefault(); menu = null; }} onkeydown={(e) => e.key === "Escape" && (menu = null)}></div>
    <div class="menu" role="menu" style="left:{menu.x}px;top:{menu.y}px">
      <button role="menuitemcheckbox" aria-checked={app.isLive(menu.folder)} disabled={menu.folder === app.inboxName}
        onclick={() => { const f = menu!.folder; menu = null; void app.toggleLive(f); }}>
        {menu.folder === app.inboxName ? "INBOX is always live" : app.isLive(menu.folder) ? "Stop keeping live" : "Keep live"}
      </button>
      {#if menuFolder}
        {@const f = menuFolder}
        <button role="menuitem" onclick={() => act(() => void org.createFolder(f.name))}>New subfolder…</button>
        {#if !special(f)}
          <button role="menuitem" onclick={() => act(() => void org.renameFolder(f))}>Rename…</button>
          {#if isMovable(app.folders, f)}<button role="menuitem" onclick={() => act(() => void org.moveFolderPicked(f))}>Move to…</button>{/if}
          <button role="menuitem" class="danger" onclick={() => act(() => void org.deleteFolder(f))}>Delete folder…</button>
        {/if}
        {#if f.role === "trash" || f.role === "junk"}
          <button role="menuitem" class="danger" onclick={() => act(() => void org.emptyFolder(f))}>Empty {f.role === "trash" ? "Trash" : "Junk"}…</button>
        {/if}
        {#if org.isSubscribed(f.name) === null}
          <button role="menuitem" disabled>Checking subscription…</button>
        {:else}
          <button role="menuitem" onclick={() => act(() => void org.toggleSubscribe(f.name))}>{org.isSubscribed(f.name) ? "Unsubscribe from folder" : "Subscribe to folder"}</button>
        {/if}
      {/if}
    </div>
  {/if}

  <OutboxLink />
  <SnoozedLink />
  <DraftTray />

  <div class="bottom">
    {#if app.accounts.length <= 1 && app.current}
      <span class="dot {app.status[app.current.email] ?? 'unknown'}" role="img" aria-label="Connection {dotTitle(app.status[app.current.email])}"></span>
      <span class="mono ell grow" title={app.current.email}>{app.current.email}</span>
    {:else}<span class="grow"></span>{/if}
    <button class="iconbtn" aria-label={themeLabel} title={themeLabel} onclick={() => app.cycleTheme()}>
      {#if app.theme === "light"}<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></svg>
      {:else if app.theme === "dark"}<svg viewBox="0 0 24 24"><path d="M21 12.8A9 9 0 1111.2 3a7 7 0 009.8 9.8z"/></svg>
      {:else}<svg viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="12" rx="1"/><path d="M8 20h8M12 16v4"/></svg>{/if}
    </button>
    <button class="iconbtn" aria-label="Settings" title="Settings" onclick={() => (app.showSettings = true)}>
      <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M4.9 19.1L7 17M17 7l2.1-2.1"/></svg>
    </button>
    <button class="iconbtn" aria-label="Add account" title="Add account" onclick={() => (app.addingAccount = true)}>
      <svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg>
    </button>
    <button class="iconbtn" aria-label="Remove this account" title="Remove this account" onclick={() => app.account && app.removeAccount(app.account)}>
      <svg viewBox="0 0 24 24"><path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/></svg>
    </button>
  </div>
</aside>

<style>
  .side { flex: none; background: var(--bg-band); border-right: 1px solid var(--line); display: flex; flex-direction: column; padding: 36px 12px 10px; gap: 14px; min-height: 0; }
  .brand { padding: 0 6px; background: none; border: 0; text-align: left; cursor: pointer; }
  .compose { width: 100%; }
  .askai { width: 100%; display: flex; align-items: center; gap: 8px; justify-content: flex-start; }
  .askai svg { width: 15px; height: 15px; fill: none; stroke: var(--accent); stroke-width: 1.8; stroke-linejoin: round; flex: none; }
  .askai kbd { margin-left: auto; color: var(--muted); font-size: .75em; }
  .switch { display: grid; grid-template-columns: 1fr 1fr; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  .switch button { background: none; border: 0; border-bottom: 2px solid transparent; padding: 6px 0; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .12em; color: var(--muted); }
  .switch button:hover { color: var(--ink); }
  .switch button.on { color: var(--ink); border-bottom-color: var(--accent); font-weight: 600; }
  .accts { display: grid; gap: 2px; }
  .acct { display: flex; align-items: center; gap: 8px; background: none; border: 0; border-left: 2px solid transparent; padding: 6px 8px; text-align: left; color: var(--muted); border-radius: 0 var(--radius) var(--radius) 0; }
  .acct.on { color: var(--ink); border-left-color: var(--accent); }
  .acct:hover { color: var(--ink); }
  .folders { display: grid; gap: 1px; align-content: start; flex: 1; overflow-y: auto; overflow-x: hidden; min-height: 0; scrollbar-width: thin; scrollbar-color: var(--line) transparent; }
  .folders::-webkit-scrollbar { width: 8px; }
  .folders::-webkit-scrollbar-thumb { background: var(--line); border-radius: 4px; }
  .folders::-webkit-scrollbar-track { background: transparent; }
  .row { display: flex; align-items: center; }
  .twist { width: 16px; height: 22px; flex: none; background: none; border: 0; padding: 0; color: var(--muted); display: grid; place-items: center; }
  .twist svg { width: 11px; height: 11px; fill: none; stroke: currentColor; stroke-width: 2; transition: transform .12s; }
  .twist svg.rot { transform: rotate(90deg); }
  .twist.sp { visibility: hidden; }
  .folder.drop { background: color-mix(in srgb, var(--accent) 22%, transparent); outline: 1px dashed var(--accent); outline-offset: -2px; color: var(--ink); }
  .topzone { border: 1px dashed var(--line); border-radius: var(--radius); padding: 6px 10px; text-align: center; font-size: 0.7667rem; letter-spacing: .06em; color: var(--muted); }
  .topzone.drop { background: color-mix(in srgb, var(--accent) 22%, transparent); border-color: var(--accent); color: var(--ink); }
  .folder.head { cursor: default; opacity: .7; }
  .folder { flex: 1; min-width: 0; display: flex; justify-content: space-between; align-items: center; gap: 8px; background: none; border: 0; border-left: 2px solid transparent; padding: var(--folder-py) 10px; text-align: left; text-transform: uppercase; letter-spacing: .1em; font-size: 0.8667rem; color: var(--muted); border-radius: 0 var(--radius) var(--radius) 0; }
  .folder:hover { color: var(--ink); background: color-mix(in srgb, var(--line) 50%, transparent); }
  .folder.on { color: var(--ink); border-left-color: var(--accent); font-weight: 600; }
  .unread { display: inline-flex; align-items: center; gap: 6px; flex: none; }
  .udot { width: 6px; height: 6px; border-radius: 50%; background: var(--accent); }
  .n { color: var(--accent); }
  .keep { flex: none; width: 22px; height: 22px; display: grid; place-items: center; background: none; border: 0; padding: 0; color: var(--muted); opacity: 0; border-radius: var(--radius); }
  .row:hover .keep, .row:focus-within .keep, .keep.active, .keep:focus-visible { opacity: 1; }
  .keep.active { color: var(--accent); } .keep.locked { opacity: .45; cursor: default; } .keep:disabled { cursor: default; }
  .keep svg { width: 13px; height: 13px; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
  .live { width: 7px; height: 7px; border-radius: 50%; flex: none; border: 1.5px solid var(--accent); background: var(--accent); animation: pulse 2s ease-in-out infinite; }
  .live.polling { animation: none; opacity: .8; }
  .live.connecting { animation: none; background: transparent; border-color: var(--muted); }
  .live.error { animation: none; background: transparent; border-color: var(--danger); }
  @keyframes pulse { 50% { opacity: .35; transform: scale(.8); } }
  @media (prefers-reduced-motion: reduce) { .live { animation: none; } }
  .liveerr { border: 1px solid var(--danger); border-radius: var(--radius); padding: 6px 8px; margin: 2px 0 4px; font-size: 0.8rem; display: grid; gap: 4px; overflow-wrap: anywhere; }
  .la { display: flex; gap: 12px; } .lnk { background: none; border: 0; padding: 0; color: var(--accent); font-size: 0.8rem; text-decoration: underline; cursor: pointer; }
  .scrimmenu { position: fixed; inset: 0; z-index: 80; }
  .menu { position: fixed; z-index: 81; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); padding: 4px; min-width: 190px; }
  .menu button { width: 100%; text-align: left; background: none; border: 0; padding: 7px 10px; color: var(--ink); font-size: 0.8667rem; } .menu button:hover:not(:disabled) { background: color-mix(in srgb, var(--line) 50%, transparent); } .menu button:disabled { color: var(--muted); }
  .menu button.danger { color: var(--danger); }
  .saved { display: grid; gap: 1px; }
  .shead { font-size: 0.6667rem; letter-spacing: .12em; text-transform: uppercase; color: var(--muted); padding: 0 8px 2px; }
  .srow { display: flex; align-items: center; }
  .sbtn { flex: 1; min-width: 0; display: flex; align-items: center; gap: 8px; background: none; border: 0; padding: 4px 8px; text-align: left; color: var(--muted); font-size: 0.8333rem; border-radius: var(--radius); }
  .sbtn:hover { color: var(--ink); background: color-mix(in srgb, var(--line) 50%, transparent); }
  .sbtn svg { width: 12px; height: 12px; flex: none; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; }
  .sx { flex: none; width: 20px; height: 20px; background: none; border: 0; color: var(--muted); opacity: 0; cursor: pointer; font-size: 1rem; line-height: 1; }
  .srow:hover .sx, .sx:focus-visible { opacity: 1; }
  .newf { background: none; border: 0; text-align: left; padding: 2px 8px; color: var(--muted); font-size: 0.7667rem; letter-spacing: .06em; cursor: pointer; }
  .newf:hover { color: var(--ink); }
  .sk { height: 14px; margin: 9px 10px; }
  .bottom { display: flex; align-items: center; gap: 2px; border-top: 1px solid var(--line); padding-top: 10px; }
  .grow { flex: 1; min-width: 0; color: var(--muted); margin-right: 4px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .dot { width: 7px; height: 7px; border-radius: 50%; flex: none; background: var(--muted); }
  .dot.connected { background: var(--accent); }
  .dot.reconnecting { background: var(--signal); }
  .dot.offline { background: transparent; border: 1.5px solid var(--muted); }
</style>
