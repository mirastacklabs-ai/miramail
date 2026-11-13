<script lang="ts">
  import { tick, untrack } from "svelte";
  import { app } from "../../lib/state.svelte";
  import { org } from "../../lib/org.svelte";
  import { local } from "../../lib/localorg.svelte";
  import { snoozeUi } from "../../lib/t2.svelte";
  import { vipFilter } from "../../lib/vipfilter.svelte";
  import { phone } from "../../lib/phone.svelte";
  import { replyFresh } from "../../lib/compose";
  import { addrLabel, listDate } from "../../lib/util";
  import { prefs } from "../../lib/prefs.svelte";
  import { OPERATORS, parseQuery, removeToken, suggest } from "../../lib/query";
  import { buildRows, orderUids, threadConfirm, type ListRow } from "../../lib/thread";
  import { clickSel } from "../../lib/bulk";
  import { endOfDay, fmtN, indexOfDate, loadedLabel, nearBottom, shouldAutoLoad, showSearchOlder, stepFor } from "../../lib/older";
  import type { FolderRole } from "../../lib/types";
  import { COLOURS, COLOUR_CSS, type SearchScope } from "../../lib/orgtypes";
  import { bulkActions, listCountText, rowActions, rowLabel, rowSwipeAllowed, SWIPE_ACTION_W, whereLine, type SwipeEnd } from "../../lib/phonemodel";
  import { justSwiped, pullRefresh, swipeRow } from "../../lib/phonegestures";
  import { longPress } from "../../lib/phonelongpress";
  import BottomSheet from "./BottomSheet.svelte";
  import DateTimeSelect from "../DateTimeSelect.svelte";
  import type { MessageSummary } from "../../lib/types";

  const vf = vipFilter;
  /** Backend search results (whole mailbox / account / cc: has: folder:) replace the loaded folder while active. */
  const rows = $derived<MessageSummary[]>(org.backend ? (org.results ?? []) : vf.on ? app.filtered.filter((m) => local.isVip(m.from)) : app.filtered);
  const threadOn = $derived(prefs.v.threading && !org.backend);
  const display = $derived<ListRow<MessageSummary>[]>(
    org.backend ? rows.map((m) => ({ m, depth: 0 as const, head: false, count: 1, unread: m.flags.seen ? 0 : 1, expanded: false, uids: [m.uid], pinned: false }))
      : buildRows(rows, local.keyOf, { threaded: threadOn, expanded: local.expanded, pinned: local.pinnedHere, selectedUid: app.selectedUid }),
  );
  const rowKey = (m: { account: string; folder: string; uid: number }) => (org.backend ? `${m.account}\u0000${m.folder}\u0000${m.uid}` : String(m.uid));
  const view = $derived(org.backend ? parseQuery(app.query, undefined, { cc: true, attachment: true }) : app.parsed);
  const role = $derived(app.folders.find((f) => f.name === app.folder)?.role);
  const title = $derived(role && role !== "other" ? role[0].toUpperCase() + role.slice(1) : app.folder.split(/[/.]/).pop() || app.folder);
  const actions = $derived(rowActions(role));
  const ba = $derived(bulkActions(role));
  const SCOPES: { v: SearchScope; label: string }[] = [{ v: "folder", label: "This folder" }, { v: "account", label: "Account" }, { v: "all", label: "All accounts" }];

  // ---- the same plumbing the Mac list runs (backend search, selection pruning, colours, conversations, VIP list) ----
  $effect(() => { void [app.query, org.scope, app.chips.unread, app.chips.flagged, app.chips.today, app.chips.week, app.account, app.folder]; untrack(() => org.schedule()); });
  $effect(() => { void [app.messages.length, app.filtered.length]; untrack(() => org.pruneSel()); });
  $effect(() => { void [app.account, app.folder]; untrack(() => { org.clearSel(); selectMode = false; sheet = null; rowCtx = null; moveUids = []; void org.loadColours(); }); });
  $effect(() => {
    void [app.account, app.folder, app.messages.length, prefs.v.threading];
    const t = setTimeout(() => { if (prefs.v.threading) void local.refreshThreads(); void local.refreshMarks(); }, 120);
    return () => clearTimeout(t);
  });
  $effect(() => { untrack(() => { if (!local.sendersLoaded) void local.loadSenders(); }); });

  // ---- search field ----
  let text = $state(app.query);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let focused = $state(false);
  $effect(() => { const v = app.query; if (v !== text && !timer) text = v; }); // external change (folder switch, Clear, token removal)
  function push(v: string, now = false) {
    text = v;
    clearTimeout(timer);
    if (now) { timer = undefined; app.query = v; return; }
    timer = setTimeout(() => { timer = undefined; app.query = text; }, 80);
  }
  const sug = $derived(suggest(text, app.messages));
  const showSug = $derived(focused && sug.items.length > 0);
  const showScope = $derived(focused || !!app.query.trim());
  let inputEl = $state<HTMLInputElement>();
  function accept(i: number) { const it = sug.items[i]; if (!it) return; push(sug.prefix + it.insert, true); inputEl?.focus(); }
  function dropToken(t: { start: number; end: number }) { push(removeToken(app.query, t), true); }
  function clearAll() { push("", true); app.clearFilters(); vf.on = false; }

  // ---- quick filters ----
  const unreadN = $derived(app.messages.reduce((n, m) => n + (m.flags.seen ? 0 : 1), 0));
  const flaggedN = $derived(app.messages.reduce((n, m) => n + (m.flags.flagged ? 1 : 0), 0));
  const vipN = $derived(app.messages.reduce((n, m) => n + (local.isVip(m.from) ? 1 : 0), 0));
  const chipDefs = [{ key: "unread", label: "Unread" }, { key: "flagged", label: "Flagged" }, { key: "today", label: "Today" }, { key: "week", label: "This week" }] as const;
  const noChips = $derived(!Object.values(app.chips).some(Boolean) && !vf.on);
  const countText = $derived(listCountText({ backend: org.backend, searching: org.searching, hasResults: !!org.results, total: org.total, scope: org.scope, filterActive: app.filterActive, filtered: app.filtered.length, loaded: app.messages.length }));

  // ---- scrolling, pull to sync, paging ----
  let openUid = $state<string | null>(null);
  let offsets = $state<Record<string, number>>({});
  let pullOff = $state(0), pullReady = $state(false), refreshing = $state(false);
  let scrollEl = $state<HTMLDivElement>();
  async function refresh() {
    if (refreshing) return; // no re-entry while a sync runs
    refreshing = true;
    try { await app.sync(); void app.refreshUnread(); } finally { refreshing = false; }
  }
  function nearEnd() {
    const e = scrollEl;
    if (!e || app.listLoading || !app.messages.length) return;
    const near = nearBottom(e.scrollTop, e.clientHeight, e.scrollHeight, 600);
    if (shouldAutoLoad({ near, moreAvailable: app.moreAvailable, busy: app.olderBusy, errored: !!app.olderError, bulk: !!app.bulk, filtered: app.filterActive })) void app.loadMore();
  }
  $effect(() => { void [app.messages.length, app.moreAvailable, app.olderBusy, app.olderError, app.bulk, app.filterActive]; untrack(() => queueMicrotask(nearEnd)); });
  const moreLabel = $derived(app.cacheMore ? "Show more" : `Load ${fmtN(stepFor(app.remaining))} older`);
  const searchOlder = $derived(showSearchOlder(app.filterActive, rows.length, app.moreAvailable));

  // ---- jump to date ----
  let jumpDate = $state("");
  const today = new Date(Date.now() - new Date().getTimezoneOffset() * 60000).toISOString().slice(0, 10);
  const jumpTs = $derived(endOfDay(jumpDate));
  function goJump() { if (jumpTs == null) return; sheet = null; void app.jumpToDate(jumpTs); }
  // "Jump to date" finished: bring the first row at or before that day to the top.
  $effect(() => {
    const j = app.jumpTo;
    if (!j) return;
    untrack(() => {
      const i = Math.min(indexOfDate(display.map((r) => r.m), j.ts), Math.max(0, display.length - 1));
      const uid = display[i] ? rowKey(display[i].m) : undefined;
      void tick().then(() => { if (uid != null) scrollEl?.querySelector(`[data-uid="${uid}"]`)?.scrollIntoView({ block: "start" }); });
    });
  });

  // ---- swipe actions: by uid within the OPEN account + folder; other rows (search results elsewhere) have none ----
  const swipeOk = (m: MessageSummary) => rowSwipeAllowed(m, { account: app.account, folder: app.folder });
  const stands = (r: ListRow<MessageSummary>) => r.head && !r.expanded && r.uids.length > 1;
  function end(r: ListRow<MessageSummary>, e: SwipeEnd) {
    const m = r.m;
    const k = rowKey(m);
    delete offsets[k];
    if (!swipeOk(m)) return;
    if (e === "open") { openUid = k; offsets[k] = -SWIPE_ACTION_W; }
    else { if (openUid === k) openUid = null; if (e === "toggle-read") toggleRead(r); }
  }
  /** A whole collapsed conversation is named before it moves (the grouping can join unrelated mail), like the Mac keys. */
  async function act(r: ListRow<MessageSummary>, id: "archive" | "trash" | "inbox") {
    const m = r.m;
    openUid = null;
    delete offsets[rowKey(m)];
    if (!swipeOk(m)) return;
    if (!stands(r)) { await app.moveTo(id, m.uid); return; } // same path as the Mac: batch mover, Undo toast
    const { account, folder } = { account: app.account, folder: app.folder }; // captured: the user may switch while the dialog is open
    if (id === "inbox") { void org.moveUids(r.uids, { role: "inbox" }, folder, account); return; }
    const members = r.uids.map((u) => app.messages.find((x) => x.uid === u)).filter((x): x is MessageSummary => !!x);
    const c = threadConfirm(members.length ? members : r.uids.map(() => ({ subject: "", from: null })), id === "archive" ? "Archive" : "Trash");
    if (await app.ask({ title: c.title, body: c.body, ok: id === "archive" ? "Archive" : "Move to Trash", danger: id !== "archive" })) void org.moveUids(r.uids, { role: id }, folder, account);
  }
  /** VoiceOver / button route to the same read toggle as the right swipe. */
  function toggleRead(r: ListRow<MessageSummary>) {
    if (!swipeOk(r.m)) return;
    if (stands(r)) void org.flagUids(r.uids, "seen", r.unread > 0);
    else void app.setFlag("seen", !r.m.flags.seen, r.m.uid);
  }
  const label = (m: MessageSummary) => rowLabel({ unread: !m.flags.seen, flagged: m.flags.flagged, from: m.from ? addrLabel(m.from) : "Unknown sender", subject: m.subject, when: listDate(m.date, prefs.v.dateFormat) });

  // ---- selection ----
  let selectMode = $state(false);
  const inSelect = $derived(!org.backend && (selectMode || org.count > 0));
  const isPicked = (r: ListRow<MessageSummary>) => (stands(r) ? r.uids.every((u) => org.isSel(u)) : org.isSel(r.m.uid));
  function togglePick(r: ListRow<MessageSummary>) {
    if (!stands(r)) { org.sel = clickSel(org.sel, orderUids(display), r.m.uid, { shift: false, meta: true }); return; }
    const set = new Set(org.sel.set);
    if (r.uids.every((u) => set.has(u))) r.uids.forEach((u) => set.delete(u)); else r.uids.forEach((u) => set.add(u));
    org.sel = { set, anchor: r.m.uid };
  }
  function doneSelect() { org.clearSel(); selectMode = false; }
  function tap(e: Event, r: ListRow<MessageSummary>) {
    const m = r.m;
    if (justSwiped(e.currentTarget as HTMLElement)) return;
    if (openUid !== null) { openUid = null; offsets = {}; return; }
    if (org.backend) { void org.openResult(m); phone.nav = { ...phone.nav, mail: "reader" }; return; }
    if (inSelect) { togglePick(r); return; }
    org.clearSel();
    void phone.openMessage(m.uid);
  }
  const ids = () => [...org.sel.set];
  const selAllSeen = $derived(ids().length > 0 && ids().every((u) => app.messages.find((m) => m.uid === u)?.flags.seen));
  const selAllFlagged = $derived(ids().length > 0 && ids().every((u) => app.messages.find((m) => m.uid === u)?.flags.flagged));

  // ---- sheets: row actions, move-to, operator help, jump to date ----
  type Sheet = null | "row" | "move" | "help" | "jump";
  let sheet = $state<Sheet>(null);
  let rowCtx = $state<{ uid: number; uids: number[]; account: string; folder: string } | null>(null);
  let moveUids = $state<number[]>([]);
  let moveSrc = $state<{ account: string; folder: string } | null>(null);
  /** The folder + account the open sheet was raised in. A stale sheet never acts on another folder. */
  const here = (c: { account: string; folder: string } | null) => !!c && c.account === app.account && c.folder === app.folder;
  const mv = (c: { account: string; folder: string }, uids: number[], dest: { role?: FolderRole; folder?: string }) => { if (here(c)) void org.moveUids(uids, dest, c.folder); };
  const fl = (c: { account: string; folder: string }, uids: number[], flag: "seen" | "flagged", v: boolean) => { if (here(c)) void org.flagUids(uids, flag, v, c.account, c.folder); };
  function openRowMenu(r: ListRow<MessageSummary>) {
    if (org.backend) return;
    openUid = null; offsets = {};
    const m = r.m;
    rowCtx = { uid: m.uid, uids: stands(r) && !org.isSel(m.uid) ? r.uids : org.targets(m.uid), account: app.account ?? "", folder: app.folder };
    sheet = "row";
  }
  const menuUids = $derived(rowCtx?.uids ?? []);
  const menuMsgs = $derived(menuUids.map((u) => app.messages.find((m) => m.uid === u)).filter((m): m is MessageSummary => !!m));
  const menuMsg = $derived(rowCtx ? app.messages.find((m) => m.uid === rowCtx!.uid) : undefined);
  const menuAllSeen = $derived(menuMsgs.length > 0 && menuMsgs.every((m) => m.flags.seen));
  const menuAllFlagged = $derived(menuMsgs.length > 0 && menuMsgs.every((m) => m.flags.flagged));
  const menuPinned = $derived(menuUids.length > 0 && menuUids.every((u) => local.isPinned(u)));
  const menuMuted = $derived(menuUids.length > 0 && menuUids.every((u) => local.isMuted(u)));
  function menuDo(f: () => void) { f(); sheet = null; }
  function startSelect() {
    if (rowCtx) org.sel = { set: new Set(rowCtx.uids), anchor: rowCtx.uid };
    selectMode = true; sheet = null;
  }
  function askMove(uids: number[], src: { account: string; folder: string }) { moveUids = uids; moveSrc = src; sheet = "move"; }
  const moveTargets = $derived(app.sortedFolders.filter((f) => f.name !== app.folder && f.selectable));
  const folderText = (f: { name: string; role: string }) => (f.role === "other" ? f.name : f.role[0].toUpperCase() + f.role.slice(1));
  function doMove(dest: string) { const u = moveUids, src = moveSrc; sheet = null; if (src) mv(src, u, { folder: dest }); }
</script>

<section class="screen">
  <header class="bar">
    <button class="back" onclick={() => phone.back()} aria-label="Mailboxes"><svg viewBox="0 0 24 24"><path d="M15 6l-6 6 6 6"/></svg><span>Mailboxes</span></button>
    <h1>{title}</h1>
    <span class="tools">
      <button class="ibtn" class:on={inSelect} aria-label={inSelect ? "Done selecting" : "Select messages"} aria-pressed={inSelect} disabled={org.backend} onclick={() => (inSelect ? doneSelect() : (selectMode = true))}>
        <svg viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" rx="2"/><path d="M8 12l3 3 5-6"/></svg>
      </button>
      <button class="ibtn" aria-label="Jump to date" disabled={!!app.bulk || (!app.moreAvailable && !app.messages.length)} onclick={() => (sheet = "jump")}>
        <svg viewBox="0 0 24 24"><path d="M4 6h16v14H4zM4 10h16M8 3v4M16 3v4"/></svg>
      </button>
      <button class="ibtn" aria-label="New message" onclick={() => replyFresh()}><svg viewBox="0 0 24 24"><path d="M4 20l4-1 11-11-3-3L5 16zM14 6l3 3"/></svg></button>
    </span>
  </header>

  <div class="search">
    <div class="field">
      <input type="search" placeholder="Search" value={text} bind:this={inputEl} oninput={(e) => push(e.currentTarget.value)}
        onfocus={() => (focused = true)} onblur={() => (focused = false)} onkeydown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }}
        autocomplete="off" autocapitalize="off" spellcheck="false" aria-label="Search messages" />
      <button class="q" aria-label="Search operators" onpointerdown={(e) => e.preventDefault()} onclick={() => (sheet = "help")}>?</button>
    </div>
    {#if showSug}
      <div class="strip" role="listbox" aria-label="Suggestions">
        {#each sug.items as it, i (it.insert)}
          <button class="sg" role="option" aria-selected="false" onpointerdown={(e) => e.preventDefault()} onclick={() => accept(i)}><span class="mono">{it.label}</span><span class="d">{it.desc}</span></button>
        {/each}
      </div>
    {/if}
    {#if showScope}
      <div class="scoperow">
        <div class="scope" role="group" aria-label="Search in">
          {#each SCOPES as sc (sc.v)}<button class:on={org.scope === sc.v} aria-pressed={org.scope === sc.v} onpointerdown={(e) => e.preventDefault()} onclick={() => { org.scope = sc.v; }}>{sc.label}</button>{/each}
        </div>
        <button class="save" aria-label="Save this search" disabled={!app.query.trim()} onpointerdown={(e) => e.preventDefault()} onclick={() => org.saveCurrent()}>Save</button>
      </div>
    {/if}
  </div>

  {#if org.parked && !org.backend}
    <button class="parked" onclick={() => org.backToResults()}>&larr; Back to search results</button>
  {/if}

  <div class="chips" role="group" aria-label="Quick filters">
    <button class="chip" aria-pressed={noChips} onclick={() => { app.chips = { unread: false, flagged: false, today: false, week: false }; vf.on = false; }}>All</button>
    {#each chipDefs as c (c.key)}
      <button class="chip" aria-pressed={app.chips[c.key]} onclick={() => { app.chips[c.key] = !app.chips[c.key]; }}>
        {c.label}{#if c.key === "unread" && unreadN}<span class="n">{unreadN}</span>{:else if c.key === "flagged" && flaggedN}<span class="n">{flaggedN}</span>{/if}
      </button>
    {/each}
    <button class="chip" aria-pressed={vf.on} disabled={org.backend} onclick={() => { vf.on = !vf.on; }}>VIP{#if vipN}<span class="n">{vipN}</span>{/if}</button>
  </div>

  <div class="meta" aria-live="polite">
    {#if view.terms.length || (org.backend ? org.hints.length : app.parsed.hints.length)}
      <div class="toks">
        {#each view.terms as t (t.start)}
          <span class="tok" class:neg={t.neg}><span class="mono">{t.raw}</span><button aria-label="Remove {t.raw}" onclick={() => dropToken(t)}>&times;</button></span>
        {/each}
        {#if org.backend ? org.hints.length : app.parsed.hints.length}<span class="hint">{org.backend ? org.hints[0] : app.parsed.hints[0]}</span>{/if}
      </div>
    {/if}
    <div class="cnt1"><span class="count mono">{countText}</span>{#if app.filterActive || vf.on}<button class="clr" onclick={clearAll}>Clear</button>{/if}</div>
  </div>

  {#if app.bulk}
    <div class="bulk" role="status">
      <span class="spinner" aria-hidden="true"></span>
      <span class="mono">Loading older… {fmtN(app.messages.length)}{app.serverTotal != null ? ` of ${fmtN(app.serverTotal)}` : ""}</span>
      <button class="clr" onclick={() => app.cancelBulk()}>Cancel</button>
    </div>
  {/if}

  <div class="ptr" style="height:{refreshing ? 36 : pullOff}px" aria-live="polite">{#if refreshing}Syncing…{:else if pullOff > 0}{pullReady ? "Release to sync" : "Pull to sync"}{/if}</div>
  <div class="scroll" bind:this={scrollEl} onscroll={nearEnd}
    use:pullRefresh={{ canPull: () => !refreshing, onPull: (o, r) => { pullOff = o; pullReady = r; }, onRelease: (r) => { if (r) void refresh(); } }}>
    {#if app.listLoading && rows.length === 0}
      {#each [0, 1, 2, 3, 4, 5] as i (i)}<div class="skeleton sk"></div>{/each}
    {:else if rows.length === 0}
      <div class="empty">
        {#if org.backend}
          {#if org.searching}<strong>Searching…</strong>
          {:else if org.searchError}<strong>Search failed</strong><span class="selectable">{org.searchError}</span>
          {:else}<strong>No matches</strong><span>Nothing cached matches. The mail server can look through mail that isn't downloaded yet.</span>{/if}
          <button class="big" disabled={org.serverBusy || org.searching} onclick={() => org.searchServer()}>{org.serverBusy ? "Asking the server…" : "Search the mail server too"}</button>
          {#if org.serverNote}<span class="selectable">{org.serverNote}</span>{/if}
        {:else if vf.on && !app.filterActive}<strong>No VIP mail</strong><span>None of the loaded messages is from a VIP sender. Add one from a message's action sheet.</span>
          <button class="big" onclick={() => (vf.on = false)}>Show all</button>
        {:else if app.filterActive}<strong>No matches</strong><span>Nothing in the {fmtN(app.messages.length)} loaded messages matches.</span>
          <button class="big" onclick={clearAll}>Clear filters</button>
        {:else}<strong>Nothing here</strong><span>{app.syncing ? "Fetching messages…" : "This folder is empty. New mail will appear automatically."}</span>{/if}
      </div>
    {:else}
      {#each display as r (rowKey(r.m))}
        {@const m = r.m}
        {@const picked = !org.backend && isPicked(r)}
        {@const col = org.backend ? 0 : (org.colours[m.uid] ?? 0)}
        {@const unread = r.head && !r.expanded ? r.unread > 0 : !m.flags.seen}
        {@const pin = !org.backend && local.isPinned(m.uid)}
        {@const mute = !org.backend && local.isMuted(m.uid)}
        {@const star = !org.backend && local.isVip(m.from)}
        {@const where = org.backend ? whereLine(org.scope, m, app.folder) : ""}
        <div class="swrap" class:child={r.depth === 1}>
          {#if swipeOk(m) && !inSelect}
            <button class="under left" tabindex="-1" aria-label={unread ? "Mark as read" : "Mark as unread"} onclick={() => toggleRead(r)}>{unread ? "Read" : "Unread"}</button>
            <div class="under right">
              {#each actions as a (a.id)}
                <button class="ubtn {a.id}" tabindex={openUid === rowKey(m) ? 0 : -1} aria-label="{a.label}: {m.subject || 'message'}" onclick={() => act(r, a.id)}>{a.label}</button>
              {/each}
            </div>
          {/if}
          <div class="item" class:anim={!(rowKey(m) in offsets) || openUid === rowKey(m)} style="transform:translateX({offsets[rowKey(m)] ?? 0}px)" data-uid={rowKey(m)}
            use:swipeRow={{ enabled: () => swipeOk(m) && !inSelect, isOpen: () => openUid === rowKey(m), onMove: (o) => (offsets[rowKey(m)] = o), onEnd: (e) => end(r, e) }}
            use:longPress={{ enabled: () => !org.backend && !inSelect, onLong: () => openRowMenu(r) }}>
            <button class="row" class:unread class:muted={mute} class:picked aria-pressed={inSelect ? picked : undefined} aria-label={label(m)} onclick={(e) => tap(e, r)}>
              <span class="dotc">
                {#if inSelect}<span class="ring" class:on={picked} aria-hidden="true">{#if picked}<svg viewBox="0 0 24 24"><path d="M5 12l4 4 10-10"/></svg>{/if}</span>
                {:else if unread && prefs.v.showDot}<span class="udot" role="img" aria-label="Unread"></span>{/if}
              </span>
              <span class="main">
                <span class="l1"><span class="from ell">{#if col}<span class="cdot" style="background:{COLOUR_CSS[col]}" role="img" aria-label="{COLOURS[col]} label"></span>{/if}{m.from ? addrLabel(m.from) : "Unknown sender"}</span>
                  {#if m.flags.flagged}<svg class="ic flag" viewBox="0 0 24 24" role="img" aria-label="Flagged"><path d="M6 21V4h11l-2 4 2 4H6"/></svg>{/if}
                  {#if star}<svg class="ic vip" viewBox="0 0 24 24" role="img" aria-label="VIP sender"><path d="M12 3l2.6 5.6 6.1.7-4.5 4.2 1.2 6L12 16.6 6.6 19.5l1.2-6L3.3 9.3l6.1-.7z"/></svg>{/if}
                  {#if mute}<svg class="ic" viewBox="0 0 24 24" role="img" aria-label="Muted conversation"><path d="M6 9a6 6 0 0112 0c0 5 2 6 2 6H4s2-1 2-6M10 19a2 2 0 004 0M4 4l16 16"/></svg>{/if}
                  {#if pin}<svg class="ic pinic" viewBox="0 0 24 24" role="img" aria-label="Pinned"><path d="M9 3h6l-1 6 3 3H7l3-3zM12 12v9"/></svg>{/if}
                  <span class="when mono">{listDate(m.date, prefs.v.dateFormat)}</span></span>
                {#if where}<span class="where mono ell">{where}</span>{/if}
                <span class="subj ell">{m.subject || "(no subject)"}</span>
                {#if prefs.v.snippetLines > 0}<span class="snip" style="-webkit-line-clamp:{prefs.v.snippetLines}">{m.snippet}</span>{/if}
              </span>
            </button>
            {#if r.head && r.key && !inSelect}
              <button class="tgl" aria-expanded={r.expanded} aria-label="{r.expanded ? 'Collapse' : 'Expand'} conversation of {r.count} messages{r.unread ? `, ${r.unread} unread` : ''}"
                onclick={() => { if (r.expanded && r.key && app.selectedUid != null && r.uids.includes(app.selectedUid) && app.selectedUid !== m.uid) app.selectedUid = m.uid; local.toggleExpanded(r.key!); }}>
                <span class="tn mono">{r.count}</span>{#if r.unread}<span class="tu mono">{r.unread} new</span>{/if}
                <svg viewBox="0 0 24 24" class:open={r.expanded}><path d="M9 6l6 6-6 6"/></svg>
              </button>
            {/if}
          </div>
        </div>
      {/each}

      <div class="foot">
        {#if org.backend}
          {#if org.moreResults}<button class="big" disabled={org.searching} onclick={() => org.loadMoreResults()}>Show more results</button>{/if}
          {#if rows.length}<button class="big" disabled={org.serverBusy} onclick={() => org.searchServer()}>{org.serverBusy ? "Asking the server…" : "Search the mail server too"}</button>{/if}
          {#if org.serverNote}<div class="selectable">{org.serverNote}</div>{/if}
        {:else}
          {#if searchOlder}
            <div class="hint2">Showing matches among the {fmtN(app.messages.length)} loaded messages only.
              <button class="big" disabled={app.olderBusy || !!app.bulk} onclick={() => app.loadMore()}>Search older messages: load more</button></div>
          {/if}
          {#if app.messages.length || app.serverTotal != null}<div class="cnt mono" aria-live="polite">{loadedLabel(app.messages.length, app.serverTotal)}</div>{/if}
          {#if app.olderBusy && !app.bulk}
            <div class="busy" role="status"><span class="spinner" aria-hidden="true"></span> Loading older messages…</div>
          {:else if app.olderError}
            <div class="errb" role="alert"><span class="selectable">{app.olderError}</span><button class="big" onclick={() => app.loadMore()}>Retry</button></div>
          {:else if app.moreAvailable && !app.bulk}
            <div class="acts">
              <button class="big" onclick={() => app.loadMore()}>{moreLabel}</button>
              <button class="big" onclick={() => app.loadAll()}>Load all</button>
            </div>
          {:else if app.serverTotal != null && app.messages.length && !app.bulk}
            <div class="end mono">Start of folder</div>
          {/if}
        {/if}
      </div>
    {/if}
  </div>

  {#if inSelect}
    <div class="selbar" role="toolbar" aria-label="Actions for selected messages">
      <div class="s1">
        <span class="mono">{fmtN(org.count)} selected</span><span class="grow"></span>
        {#if org.count < rows.length}<button class="tb" onclick={() => org.selectAllRows()}>Select all {fmtN(rows.length)}</button>{/if}
        <button class="tb strong" onclick={doneSelect}>Done</button>
      </div>
      <div class="s2">
        <button class="tb" disabled={!org.count} onclick={() => org.moveUids(ids(), { role: "archive" })}>Archive</button>
        <button class="tb" disabled={!org.count} onclick={() => askMove(ids(), { account: app.account ?? "", folder: app.folder })}>Move to…</button>
        <button class="tb danger" disabled={!org.count} onclick={() => org.moveUids(ids(), { role: "trash" })}>{ba.trashLabel}</button>
        {#if ba.junk === "notjunk"}<button class="tb" disabled={!org.count} onclick={() => org.moveUids(ids(), { role: "inbox" })}>Not junk</button>
        {:else if ba.junk === "junk"}<button class="tb" disabled={!org.count} onclick={() => org.moveUids(ids(), { role: "junk" })}>Junk</button>{/if}
        <button class="tb" disabled={!org.count || selAllSeen} onclick={() => org.flagUids(ids(), "seen", true)}>Read</button>
        <button class="tb" disabled={!org.count} onclick={() => org.flagUids(ids(), "seen", false)}>Unread</button>
        <button class="tb" disabled={!org.count || selAllFlagged} onclick={() => org.flagUids(ids(), "flagged", true)}>Flag</button>
        <button class="tb" disabled={!org.count} onclick={() => org.flagUids(ids(), "flagged", false)}>Unflag</button>
      </div>
    </div>
  {/if}

  {#if sheet === "row" && rowCtx}
    <BottomSheet title={menuUids.length > 1 ? `${fmtN(menuUids.length)} messages` : "Message"} onclose={() => (sheet = null)} tall>
      <div class="list">
        <button class="mi" onclick={startSelect}>Select</button>
        <button class="mi" onclick={() => menuDo(() => mv(rowCtx!, menuUids, { role: "archive" }))}>Archive</button>
        <button class="mi danger" onclick={() => menuDo(() => mv(rowCtx!, menuUids, { role: "trash" }))}>{role === "trash" ? "Delete forever" : "Move to Trash"}</button>
        {#if ba.junk === "notjunk"}<button class="mi" onclick={() => menuDo(() => mv(rowCtx!, menuUids, { role: "inbox" }))}>Not junk</button>
        {:else if ba.junk === "junk"}<button class="mi" onclick={() => menuDo(() => mv(rowCtx!, menuUids, { role: "junk" }))}>Mark as junk</button>{/if}
        <button class="mi" onclick={() => menuDo(() => fl(rowCtx!, menuUids, "seen", !menuAllSeen))}>{menuAllSeen ? "Mark as unread" : "Mark as read"}</button>
        <button class="mi" onclick={() => menuDo(() => fl(rowCtx!, menuUids, "flagged", !menuAllFlagged))}>{menuAllFlagged ? "Unflag" : "Flag"}</button>
        {#if menuUids.length === 1 && role !== "trash"}<button class="mi" onclick={() => menuDo(() => snoozeUi.open(menuUids[0]))}>Snooze…</button>{/if}
        <button class="mi" onclick={() => { if (rowCtx) askMove(menuUids, { account: rowCtx.account, folder: rowCtx.folder }); }}>Move to…</button>
        <div class="gh mono">Conversation and sender</div>
        <button class="mi" onclick={() => menuDo(() => void local.setPinned(menuUids, !menuPinned))}>{menuPinned ? "Unpin" : "Pin to top"}</button>
        <button class="mi" onclick={() => menuDo(() => void local.setMuted(menuUids, !menuMuted))}>{menuMuted ? "Unmute conversation" : "Mute conversation"}</button>
        {#if menuMsg?.from}
          <button class="mi" onclick={() => menuDo(() => void local.setVip(menuMsg, !local.isVip(menuMsg.from)))}>{local.isVip(menuMsg.from) ? "Remove sender from VIP" : "Add sender to VIP"}</button>
          <button class="mi danger" onclick={() => menuDo(() => void local.blockSender(menuMsg))}>Block sender…</button>
        {/if}
        <div class="gh mono">Colour label</div>
        <div class="sw" role="group" aria-label="Colour label">
          {#each [1, 2, 3, 4, 5, 6, 7] as c (c)}
            <button class="swatch" style="background:{COLOUR_CSS[c]}" aria-label={COLOURS[c]} onclick={() => menuDo(() => void org.setColour(menuUids, c))}></button>
          {/each}
          <button class="swatch none" aria-label="No colour" onclick={() => menuDo(() => void org.setColour(menuUids, 0))}>&times;</button>
        </div>
      </div>
    </BottomSheet>
  {:else if sheet === "move"}
    <BottomSheet title="Move to…" onclose={() => (sheet = null)} tall>
      <div class="list">
        {#each moveTargets as f (f.name)}<button class="mi" onclick={() => doMove(f.name)}>{folderText(f)}</button>{:else}<p class="note">No other folder to move to.</p>{/each}
      </div>
    </BottomSheet>
  {:else if sheet === "help"}
    <BottomSheet title="Search operators" onclose={() => (sheet = null)} tall>
      <div class="help">
        {#each OPERATORS as o (o.op)}<div class="op"><span class="mono">{o.op}</span><span class="d">{o.desc}</span></div>{/each}
        <p class="note">Prefix with <span class="mono">-</span> to exclude; quote phrases: <span class="mono">subject:"quarterly report"</span>. Plain words match subject, preview, from and to. &ldquo;This folder&rdquo; filters the loaded list instantly; &ldquo;Account&rdquo; and &ldquo;All accounts&rdquo; (and cc:, has:attachment, folder:) search everything cached on this device, body text included for messages whose body was downloaded. If nothing is cached, ask the mail server from the button under the results.</p>
      </div>
    </BottomSheet>
  {:else if sheet === "jump"}
    <BottomSheet title="Jump to date" onclose={() => (sheet = null)}>
      <div class="jump">
        <DateTimeSelect mode="date" idPrefix="pjump-date" ariaPrefix="Jump to" past days={730} max={today} emptyLabel="Pick a date" value={jumpDate} onchange={(v) => (jumpDate = v)} />
        <p class="note">Loads older messages until this day is listed, or the folder starts. You can cancel while it runs.</p>
        <button class="big" disabled={jumpTs == null} onclick={goJump}>Jump</button>
      </div>
    </BottomSheet>
  {/if}
</section>

<style>
  .screen { position: relative; flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .bar { flex: none; display: grid; grid-template-columns: auto 1fr auto; align-items: center; gap: 4px; padding: 6px 4px; min-height: 52px; border-bottom: 1px solid var(--line); background: var(--bg-band); }
  h1 { font-size: 1.05rem; letter-spacing: .08em; text-align: center; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .back { justify-self: start; min-height: 44px; min-width: 44px; display: inline-flex; align-items: center; background: none; border: 0; color: var(--accent); font-size: 0.95rem; padding: 0 8px 0 2px; }
  .back svg, .ibtn svg { width: 22px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .tools { display: inline-flex; }
  .ibtn { width: 44px; height: 44px; background: none; border: 0; color: var(--accent); display: grid; place-items: center; }
  .ibtn.on { color: var(--ink); background: color-mix(in srgb, var(--accent) 22%, transparent); border-radius: var(--radius); }
  .ibtn:disabled { opacity: .4; }
  .search { flex: none; padding: 8px 12px 4px; display: grid; gap: 6px; }
  .field { position: relative; }
  .field input { width: 100%; min-height: 44px; background: transparent; color: var(--ink); border: 1px solid var(--line); border-radius: var(--radius); padding: 0 48px 0 12px; font: 400 16px var(--font-mono); }
  .q { position: absolute; right: 0; top: 0; width: 44px; height: 44px; background: none; border: 0; color: var(--muted); font: 600 1rem var(--font-mono); }
  .strip, .chips { display: flex; gap: 8px; overflow-x: auto; scrollbar-width: none; -webkit-overflow-scrolling: touch; }
  .strip::-webkit-scrollbar, .chips::-webkit-scrollbar, .s2::-webkit-scrollbar { display: none; }
  .sg { flex: none; min-height: 44px; display: inline-flex; align-items: baseline; gap: 8px; padding: 0 12px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); color: var(--ink); font-size: 0.8rem; align-self: center; }
  .sg .d { color: var(--muted); }
  .scoperow { display: flex; gap: 8px; align-items: center; }
  .scope { flex: 1; display: flex; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  .scope button { flex: 1; min-height: 44px; background: none; border: 0; color: var(--muted); font-size: 0.78rem; padding: 0 4px; }
  .scope button + button { border-left: 1px solid var(--line); }
  .scope button.on { background: color-mix(in srgb, var(--accent) 22%, transparent); color: var(--ink); }
  .save { flex: none; min-height: 44px; min-width: 56px; background: none; border: 1px solid var(--line); border-radius: var(--radius); color: var(--accent); font-size: 0.8rem; text-transform: uppercase; letter-spacing: .08em; }
  .save:disabled { opacity: .4; }
  .parked { flex: none; min-height: 44px; background: color-mix(in srgb, var(--accent) 12%, transparent); border: 0; border-bottom: 1px solid var(--line); color: var(--accent); text-align: left; padding: 0 14px; font-size: 0.9rem; }
  .chips { flex: none; padding: 6px 12px; }
  .chip { position: relative; flex: none; min-height: 34px; display: inline-flex; align-items: center; gap: 6px; padding: 0 14px; background: none; border: 1px solid var(--line); border-radius: 17px; color: var(--muted); font-size: 0.85rem; }
  .chip::before { content: ""; position: absolute; inset: -5px -2px; } /* 44 px touch target */
  .chip[aria-pressed="true"] { color: var(--ink); border-color: var(--accent); background: color-mix(in srgb, var(--accent) 22%, transparent); }
  .chip:disabled { opacity: .4; }
  .chip .n { font: 500 0.72rem var(--font-mono); color: var(--accent); }
  .meta { flex: none; padding: 2px 14px 6px; display: grid; gap: 4px; border-bottom: 1px solid var(--line); font-size: 0.78rem; color: var(--muted); }
  .toks { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; }
  .tok { display: inline-flex; align-items: center; border: 1px solid var(--line); border-radius: var(--radius); padding-left: 8px; color: var(--ink); }
  .tok.neg { border-color: var(--signal); }
  .tok button { width: 44px; height: 32px; background: none; border: 0; color: var(--muted); font-size: 1.1rem; margin: -2px 0; }
  .hint { color: var(--signal); }
  .cnt1 { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .clr { min-height: 44px; min-width: 44px; background: none; border: 0; color: var(--accent); font-size: 0.8rem; text-transform: uppercase; letter-spacing: .08em; margin: -10px -10px -10px 0; }
  .bulk { flex: none; display: flex; align-items: center; gap: 8px; padding: 0 14px; min-height: 44px; border-bottom: 1px solid var(--line); background: color-mix(in srgb, var(--accent) 10%, transparent); font-size: 0.78rem; }
  .bulk .mono { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .bulk .clr { margin: 0; }
  .spinner { width: 12px; height: 12px; flex: none; border: 2px solid var(--line); border-top-color: var(--accent); border-radius: 50%; animation: prot .8s linear infinite; display: inline-block; vertical-align: -2px; }
  @keyframes prot { to { transform: rotate(360deg); } }
  .ptr { flex: none; overflow: hidden; display: grid; place-items: center; font-size: 0.75rem; color: var(--muted); text-transform: uppercase; letter-spacing: .1em; }
  .scroll { flex: 1; min-height: 0; overflow-y: auto; overflow-x: hidden; overscroll-behavior-y: contain; -webkit-overflow-scrolling: touch; }
  .swrap { position: relative; overflow: hidden; border-bottom: 1px solid var(--line); }
  .swrap.child { padding-left: 22px; background: var(--bg); }
  .under { position: absolute; top: 0; bottom: 0; display: flex; align-items: stretch; }
  .under.left { left: 0; width: 120px; border: 0; background: var(--accent); color: #0a0a0a; align-items: center; padding-left: 18px; font-weight: 600; text-transform: uppercase; letter-spacing: .08em; font-size: 0.8rem; }
  .under.right { right: 0; width: 144px; }
  .ubtn { flex: 1; border: 0; color: #0a0a0a; font-weight: 600; text-transform: uppercase; letter-spacing: .06em; font-size: 0.75rem; background: var(--accent); }
  .ubtn.trash { background: var(--signal); }
  .item { position: relative; display: flex; align-items: stretch; background: var(--bg); touch-action: pan-y; -webkit-user-select: none; user-select: none; -webkit-touch-callout: none; }
  .item.anim { transition: transform .18s ease-out; }
  .row { flex: 1; min-width: 0; min-height: 76px; display: flex; gap: 4px; align-items: flex-start; background: none; border: 0; padding: 10px 14px 10px 4px; text-align: left; color: var(--text); }
  .row.picked { background: color-mix(in srgb, var(--accent) 14%, transparent); }
  .row.muted { opacity: .65; }
  .dotc { width: 26px; flex: none; display: grid; place-items: start center; padding-top: 5px; }
  .udot { width: 9px; height: 9px; margin-top: 2px; border-radius: 50%; background: var(--accent); }
  .ring { width: 20px; height: 20px; border-radius: 50%; border: 1.5px solid var(--muted); display: grid; place-items: center; }
  .ring.on { background: var(--accent); border-color: var(--accent); }
  .ring svg { width: 14px; height: 14px; fill: none; stroke: #0a0a0a; stroke-width: 2.4; stroke-linecap: round; stroke-linejoin: round; }
  .main { flex: 1; min-width: 0; display: grid; gap: 2px; }
  .l1 { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .from { flex: 1; min-width: 0; font-size: 1.02rem; color: var(--ink); }
  .row.unread .from { font-weight: 600; }
  .cdot { display: inline-block; width: 9px; height: 9px; border-radius: 50%; margin-right: 6px; vertical-align: 0; }
  .when { flex: none; color: var(--muted); font-size: 0.72rem; }
  .ic { width: 14px; height: 14px; flex: none; fill: none; stroke: var(--muted); stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .ic.flag { fill: var(--signal); stroke: var(--signal); }
  .ic.vip { fill: var(--accent); stroke: var(--accent); }
  .ic.pinic { stroke: var(--accent); }
  .where { font-size: 0.7rem; color: var(--accent); }
  .subj { font-size: 0.93rem; color: var(--ink); } .row.unread .subj { font-weight: 500; }
  .snip { font-size: 0.85rem; color: var(--muted); display: -webkit-box; -webkit-box-orient: vertical; overflow: hidden; }
  .ell { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tgl { flex: none; min-width: 44px; min-height: 44px; align-self: center; display: inline-flex; align-items: center; gap: 4px; padding: 0 10px 0 4px; background: none; border: 0; color: var(--muted); font-size: 0.72rem; }
  .tgl svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; transition: transform .15s; }
  .tgl svg.open { transform: rotate(90deg); }
  .tu { color: var(--accent); }
  .empty { display: grid; gap: 10px; justify-items: center; text-align: center; color: var(--muted); padding: 40px 20px; }
  .empty strong { color: var(--ink); }
  .big { min-height: 44px; padding: 0 16px; background: none; border: 1px solid var(--line); border-radius: var(--radius); color: var(--accent); text-transform: uppercase; letter-spacing: .08em; font-size: 0.8rem; }
  .big:disabled { opacity: .4; }
  .foot { display: grid; gap: 8px; justify-items: center; padding: 14px 14px 22px; text-align: center; font-size: 0.8rem; color: var(--muted); }
  .foot .acts { display: flex; gap: 8px; flex-wrap: wrap; justify-content: center; }
  .foot .errb { display: grid; gap: 8px; justify-items: center; color: var(--danger); overflow-wrap: anywhere; }
  .foot .busy { display: inline-flex; gap: 8px; align-items: center; }
  .foot .end { opacity: .7; }
  .hint2 { display: grid; gap: 8px; justify-items: center; }
  .sk { height: 56px; margin: 12px 14px; }
  .selbar { flex: none; border-top: 1px solid var(--line); background: var(--bg-band); }
  .s1 { display: flex; align-items: center; gap: 4px; padding: 0 8px 0 14px; font-size: 0.8rem; color: var(--muted); }
  .grow { flex: 1; }
  .s2 { display: flex; overflow-x: auto; scrollbar-width: none; -webkit-overflow-scrolling: touch; border-top: 1px solid var(--line); }
  .tb { flex: none; min-height: 44px; padding: 0 14px; background: none; border: 0; color: var(--accent); font-size: 0.82rem; text-transform: uppercase; letter-spacing: .06em; }
  .tb.strong { font-weight: 700; }
  .tb.danger { color: var(--signal); }
  .tb:disabled { opacity: .35; }
  .list { display: grid; }
  .mi { min-height: 48px; display: flex; align-items: center; background: none; border: 0; border-bottom: 1px solid var(--line); text-align: left; padding: 0 8px; color: var(--ink); font-size: 1rem; }
  .mi.danger { color: var(--signal); }
  .gh { padding: 14px 8px 4px; color: var(--muted); font-size: 0.72rem; text-transform: uppercase; letter-spacing: .1em; }
  .sw { display: flex; gap: 10px; flex-wrap: wrap; padding: 8px; }
  .swatch { width: 44px; height: 44px; border-radius: 50%; border: 2px solid var(--line); color: var(--muted); font-size: 1.2rem; }
  .swatch.none { background: none; }
  .help { display: grid; gap: 2px; }
  .op { display: flex; gap: 12px; padding: 8px 4px; align-items: baseline; border-bottom: 1px solid var(--line); font-size: 0.85rem; }
  .op .mono { flex: none; min-width: 104px; color: var(--ink); }
  .d { color: var(--muted); }
  .note { margin: 8px 4px; color: var(--muted); font-size: 0.8rem; }
  .jump { display: grid; gap: 12px; padding: 8px 4px; }
</style>
