<script lang="ts">
  import { tick, untrack } from "svelte";
  import { app } from "../lib/state.svelte";
  import Select from "./Select.svelte";
  import DateTimeSelect from "./DateTimeSelect.svelte";
  import { endOfDay, fmtN, indexOfDate, loadedLabel, nearBottom, scrollToReveal, shouldAutoLoad, showSearchOlder, stepFor, windowRange } from "../lib/older";
  import { addrLabel, listDate } from "../lib/util";
  import { prefs } from "../lib/prefs.svelte";
  import { OPERATORS, parseQuery, removeToken, suggest, type Chips } from "../lib/query";
  import { org } from "../lib/org.svelte";
  import { clickSel } from "../lib/bulk";
  import { COLOURS, COLOUR_CSS, type SearchScope } from "../lib/orgtypes";
  import { local } from "../lib/localorg.svelte";
  import { snoozeUi } from "../lib/t2.svelte";
  import { buildRows, keyNav, navTarget, orderUids, senderAddress, threadConfirm, threadUids, type ListRow } from "../lib/thread";
  import type { MessageSummary } from "../lib/types";

  let { searchEl = $bindable() }: { searchEl?: HTMLInputElement } = $props();
  /** Backend search results (whole mailbox / account / cc: has: folder:) replace the loaded folder while active. */
  let vipOnly = $state(false);
  const rows = $derived(org.backend ? (org.results ?? []) : vipOnly ? app.filtered.filter((m) => local.isVip(m.from)) : app.filtered);
  /** Conversation view (local, from the cache): off during a mailbox-wide search. */
  const threadOn = $derived(prefs.v.threading && !org.backend);
  /** What the list shows: pinned messages first, conversations grouped when threading is on. */
  const display = $derived<ListRow<MessageSummary>[]>(
    org.backend ? rows.map((m) => ({ m, depth: 0 as const, head: false, count: 1, unread: m.flags.seen ? 0 : 1, expanded: false, uids: [m.uid], pinned: false }))
      : buildRows(rows, local.keyOf, { threaded: threadOn, expanded: local.expanded, pinned: local.pinnedHere, selectedUid: app.selectedUid }),
  );
  const rowKey = (m: { account: string; folder: string; uid: number }) => (org.backend ? `${m.account}\u0000${m.folder}\u0000${m.uid}` : String(m.uid));
  const view = $derived(org.backend ? parseQuery(app.query, undefined, { cc: true, attachment: true }) : app.parsed);
  const SCOPES: { v: SearchScope; label: string }[] = [{ v: "folder", label: "This folder" }, { v: "account", label: "Account" }, { v: "all", label: "All accounts" }];
  let selectMode = $state(false);
  let cmenu = $state<{ x: number; y: number; uid: number; uids?: number[] } | null>(null);
  const folderRole = $derived(app.folders.find((f) => f.name === app.folder)?.role);

  // Backend search: re-run whenever the query, scope, chips or the open folder change.
  $effect(() => {
    void [app.query, org.scope, app.chips.unread, app.chips.flagged, app.chips.today, app.chips.week, app.account, app.folder];
    untrack(() => org.schedule());
  });
  $effect(() => { void [app.messages.length, app.filtered.length]; untrack(() => org.pruneSel()); });
  $effect(() => { void [app.account, app.folder]; untrack(() => { org.clearSel(); void org.loadColours(); }); });
  // Conversations, muted / pinned rows and the VIP list (all local; the backend reads only the message cache).
  $effect(() => {
    void [app.account, app.folder, app.messages.length, prefs.v.threading];
    const t = setTimeout(() => { if (prefs.v.threading) void local.refreshThreads(); void local.refreshMarks(); }, 120);
    return () => clearTimeout(t);
  });
  $effect(() => { untrack(() => { if (!local.sendersLoaded) void local.loadSenders(); }); });
  const title = $derived(app.sortedFolders.find((f) => f.name === app.folder)?.role ?? app.folder);

  // Local text updates instantly; app.query (and thus the filter) follows after an 80 ms debounce.
  let text = $state(app.query);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let focused = $state(false), dismissed = $state(false), help = $state(false), active = $state(0);
  $effect(() => { const q = app.query; if (q !== text && !timer) text = q; }); // external change (folder switch, Clear, token removal)
  function push(v: string, now = false) {
    text = v; dismissed = false; active = 0;
    clearTimeout(timer);
    if (now) { timer = undefined; app.query = v; return; }
    timer = setTimeout(() => { timer = undefined; app.query = text; }, 80);
  }
  const sug = $derived(suggest(text, app.messages));
  const popOpen = $derived(focused && !dismissed && sug.items.length > 0);
  function accept(i: number) {
    const it = sug.items[i]; if (!it) return;
    push(sug.prefix + it.insert, true); searchEl?.focus();
  }
  function onKey(e: KeyboardEvent) {
    if (popOpen && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault(); const n = sug.items.length; active = (active + (e.key === "ArrowDown" ? 1 : n - 1)) % n;
    } else if (popOpen && (e.key === "Tab" || (e.key === "Enter" && !text.endsWith(sug.items[active].insert)))) {
      e.preventDefault(); accept(active);
    } else if (e.key === "Escape") {
      e.stopPropagation();
      if (help) help = false;
      else if (popOpen) dismissed = true;
      else { push("", true); app.clearFilters(); (e.currentTarget as HTMLElement).blur(); }
    } else if (e.key === "Enter") (e.currentTarget as HTMLElement).blur();
  }

  // ---- windowed rendering: only the rows near the viewport exist in the DOM (rows have one uniform height) ----
  const ROW_GUESS = 64;
  let rowsEl = $state<HTMLDivElement>();
  let sectionEl = $state<HTMLElement>();
  let moreBtn = $state<HTMLButtonElement>();
  let scrollTop = $state(0), viewH = $state(600), rowH = $state(0);
  const eff = $derived(rowH || ROW_GUESS);
  const win = $derived(windowRange(scrollTop, viewH, eff, display.length));
  const visible = $derived(display.slice(win.start, win.end));
  function onScroll() { if (rowsEl) scrollTop = rowsEl.scrollTop; }
  function measure() {
    const el = rowsEl?.querySelector(".row");
    const h = el ? el.getBoundingClientRect().height : 0;
    if (h > 0 && Math.abs(h - rowH) > 0.25) rowH = h;
  }
  $effect(() => {
    if (!rowsEl) return;
    const ro = new ResizeObserver(() => { viewH = rowsEl!.clientHeight; measure(); });
    ro.observe(rowsEl);
    return () => ro.disconnect();
  });
  $effect(() => { void visible; void JSON.stringify(prefs.v); void tick().then(measure); }); // density / snippet lines / zoom change the row height

  // Keep the keyboard selection on screen (rows far away are not in the DOM).
  $effect(() => {
    const uid = app.selectedUid;
    untrack(() => {
      if (uid == null || !rowsEl) return;
      const i = display.findIndex((r) => r.m.uid === uid);
      if (i < 0) return;
      const t = scrollToReveal(i, rowsEl.scrollTop, rowsEl.clientHeight, eff);
      if (t !== rowsEl.scrollTop) rowsEl.scrollTop = t;
    });
  });
  // "Jump to date" finished: scroll to the first row at or before that day.
  $effect(() => {
    const j = app.jumpTo;
    if (!j) return;
    untrack(() => {
      if (!rowsEl) return;
      rowsEl.scrollTop = Math.min(indexOfDate(display.map((r) => r.m), j.ts), Math.max(0, display.length - 1)) * eff;
    });
  });

  // ---- load older ----
  function maybeAuto() {
    if (!rowsEl || app.listLoading || !app.messages.length) return;
    const near = nearBottom(rowsEl.scrollTop, rowsEl.clientHeight, rowsEl.scrollHeight);
    if (shouldAutoLoad({ near, moreAvailable: app.moreAvailable, busy: app.olderBusy, errored: !!app.olderError, bulk: !!app.bulk, filtered: app.filterActive })) void app.loadMore();
  }
  $effect(() => { void [app.messages.length, app.olderBusy, app.olderError, app.moreAvailable, app.bulk, app.filterActive, scrollTop, viewH]; void tick().then(maybeAuto); });
  const moreLabel = $derived(app.cacheMore ? "Show more" : `Load ${fmtN(stepFor(app.remaining))} older`);
  const searchOlder = $derived(showSearchOlder(app.filterActive, rows.length, app.moreAvailable));

  function winKey(e: KeyboardEvent) {
    if (e.metaKey || e.ctrlKey || e.altKey || !sectionEl) return;
    const a = document.activeElement;
    if (!(a === document.body || sectionEl.contains(a))) return;
    const tag = (e.target as HTMLElement | null)?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA") return;
    if (e.key === "End" && rowsEl) {
      e.preventDefault();
      rowsEl.scrollTop = rowsEl.scrollHeight;
      void tick().then(() => moreBtn?.focus()); // End, then Enter, loads more
    } else if (e.key === "Escape" && app.bulk && sectionEl.contains(a)) {
      e.stopPropagation(); app.cancelBulk();
    }
  }

  // ---- jump to date ----
  let jumpOpen = $state(false), jumpDate = $state("");
  const today = new Date(Date.now() - new Date().getTimezoneOffset() * 60000).toISOString().slice(0, 10);
  const jumpTs = $derived(endOfDay(jumpDate));
  function goJump() { if (jumpTs == null) return; jumpOpen = false; void app.jumpToDate(jumpTs); }

  const unreadN = $derived(app.messages.reduce((n, m) => n + (m.flags.seen ? 0 : 1), 0));
  const flaggedN = $derived(app.messages.reduce((n, m) => n + (m.flags.flagged ? 1 : 0), 0));
  const chipDefs: { key: keyof Chips; label: string }[] = [
    { key: "unread", label: "Unread" }, { key: "flagged", label: "Flagged" }, { key: "today", label: "Today" }, { key: "week", label: "This week" },
  ];
  const noChips = $derived(!Object.values(app.chips).some(Boolean) && !vipOnly);
  const vipN = $derived(app.messages.reduce((n, m) => n + (local.isVip(m.from) ? 1 : 0), 0));
  function clearAll() { push("", true); app.clearFilters(); }
  function dropToken(t: { start: number; end: number }) { push(removeToken(app.query, t), true); searchEl?.focus(); }

  // ---- selection, bulk actions, context menu ----
  /** A collapsed conversation row stands for every message of it. */
  const stands = (r: ListRow<MessageSummary>) => r.head && !r.expanded && r.uids.length > 1;
  const isPicked = (r: ListRow<MessageSummary>) => (stands(r) ? r.uids.every((u) => org.isSel(u)) : org.isSel(r.m.uid));
  function togglePick(r: ListRow<MessageSummary>) {
    if (!stands(r)) { org.sel = clickSel(org.sel, orderUids(display), r.m.uid, { shift: false, meta: true }); return; }
    const set = new Set(org.sel.set);
    if (r.uids.every((u) => set.has(u))) r.uids.forEach((u) => set.delete(u)); else r.uids.forEach((u) => set.add(u));
    org.sel = { set, anchor: r.m.uid };
  }
  function rowClick(e: MouseEvent, m: { account: string; folder: string; uid: number } & Parameters<typeof org.openResult>[0], r?: ListRow<MessageSummary>) {
    if (org.backend) { void org.openResult(m); return; }
    const meta = e.metaKey || e.ctrlKey;
    if (e.shiftKey || meta || selectMode) {
      e.preventDefault();
      if (e.shiftKey && org.sel.anchor == null && app.selectedUid != null) org.sel = { set: new Set(), anchor: app.selectedUid };
      if (r && !e.shiftKey) togglePick(r);
      else org.sel = clickSel(org.sel, orderUids(display), m.uid, { shift: e.shiftKey, meta: meta || selectMode });
      return;
    }
    org.clearSel();
    void app.open(m.uid);
  }
  function rowMenu(e: MouseEvent, m: { uid: number }, r?: ListRow<MessageSummary>) {
    if (org.backend) return;
    e.preventDefault();
    cmenu = { x: Math.min(e.clientX, window.innerWidth - 230), y: Math.min(e.clientY, window.innerHeight - 470), uid: m.uid, uids: r && stands(r) ? r.uids : undefined };
  }
  const menuUids = $derived(cmenu ? (cmenu.uids && !org.isSel(cmenu.uid) ? cmenu.uids : org.targets(cmenu.uid)) : []);
  const menuMsg = $derived(cmenu ? app.messages.find((m) => m.uid === cmenu!.uid) : undefined);
  const menuPinned = $derived(menuUids.length > 0 && menuUids.every((u) => local.isPinned(u)));
  const menuMuted = $derived(menuUids.length > 0 && menuUids.every((u) => local.isMuted(u)));
  function menuDo(f: () => void) { f(); cmenu = null; }
  function extend(dir: 1 | -1) {
    const uids = orderUids(display);
    const i = uids.indexOf(app.selectedUid ?? -1);
    const n = uids[i + dir];
    if (i < 0 || n == null) return;
    if (org.sel.anchor == null) org.sel = { set: new Set([uids[i]]), anchor: uids[i] };
    org.sel = clickSel(org.sel, uids, n, { shift: true, meta: false });
    app.selectedUid = n;
    queueMicrotask(() => document.querySelector(`[data-uid="${n}"]`)?.scrollIntoView({ block: "nearest" }));
  }
  async function confirmThreadMove(uids: number[], archive: boolean) {
    const members = uids.map((u) => app.messages.find((m) => m.uid === u)).filter((m): m is NonNullable<typeof m> => !!m);
    const c = threadConfirm(members.length ? members : uids.map(() => ({ subject: "", from: null })), archive ? "Archive" : "Trash");
    const ok = await app.ask({ title: c.title, body: c.body, ok: archive ? "Archive" : "Move to Trash", danger: !archive });
    if (ok) void org.moveUids(uids, { role: archive ? "archive" : "trash" });
    else org.clearSel();
  }
  /** Runs before the app-wide single-message shortcuts: with a selection, a / # / e act on the whole selection. */
  function selKey(e: KeyboardEvent) {
    if (org.backend || !sectionEl || e.altKey || app.confirmReq) return;
    const a = document.activeElement;
    if (!(a === document.body || sectionEl.contains(a))) return;
    const tag = (e.target as HTMLElement | null)?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
    const meta = e.metaKey || e.ctrlKey;
    const eat = () => { e.preventDefault(); e.stopPropagation(); };
    if (meta && e.key.toLowerCase() === "a" && !e.shiftKey) { eat(); org.selectAllRows(); return; }
    if (meta) return;
    if (threadOn && !e.shiftKey) {
      // Conversation view: j / k step over visible rows only; Left / Right collapse and expand.
      if (e.key === "j" || e.key === "k") {
        const n = navTarget(display, app.selectedUid, e.key === "j" ? 1 : -1);
        if (n != null) { eat(); app.selectedUid = n; }
        return;
      }
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        const a = keyNav(display, app.selectedUid, e.key === "ArrowLeft" ? "left" : "right");
        if (a.act === "none") return;
        eat();
        if (a.act === "expand") local.toggleExpanded(a.key, true);
        else if (a.act === "collapse") { if (a.select != null) app.selectedUid = a.select; local.toggleExpanded(a.key, false); }
        else app.selectedUid = a.uid;
        return;
      }
    }
    // a / e / # / Delete / I / U / s on a collapsed conversation act on all its messages in this folder.
    if (threadOn && !org.count && app.selectedUid != null && ["a", "e", "#", "Delete", "Backspace", "I", "U", "s"].includes(e.key)) {
      const all = threadUids(display, app.selectedUid);
      if (all.length > 1) {
        org.sel = { set: new Set(all), anchor: app.selectedUid };
        // Moving a whole conversation names what it covers first (the grouping can join unrelated mail).
        if (["a", "e", "#", "Delete", "Backspace"].includes(e.key)) { eat(); void confirmThreadMove(all, e.key === "a" || e.key === "e"); return; }
      }
    }
    if (e.shiftKey && (e.key === "ArrowDown" || e.key === "ArrowUp")) { eat(); extend(e.key === "ArrowDown" ? 1 : -1); return; }
    if (e.key === "x" && !e.shiftKey) { eat(); org.toggleCurrent(); return; }
    if (!org.count) return;
    const ids = [...org.sel.set];
    switch (e.key) {
      case "Escape": eat(); org.clearSel(); selectMode = false; break;
      case "a": case "e": eat(); void org.moveUids(ids, { role: "archive" }); break;
      case "#": case "Delete": case "Backspace": eat(); void org.moveUids(ids, { role: "trash" }); break;
      case "I": eat(); void org.flagUids(ids, "seen", true); break;
      case "U": eat(); void org.flagUids(ids, "seen", false); break;
      case "s": eat(); void org.flagUids(ids, "flagged", !ids.every((u) => app.messages.find((m) => m.uid === u)?.flags.flagged)); break;
    }
  }
  let moveTo = $state("");
  const moveTargets = $derived(app.sortedFolders.filter((f) => f.name !== app.folder));
  function bulkMove() {
    const dest = moveTo; moveTo = "";
    if (dest) void org.moveUids([...org.sel.set], { folder: dest });
  }
  const resultsLabel = $derived(`${fmtN(org.total)} match${org.total === 1 ? "" : "es"}${org.scope === "folder" ? " in this folder" : org.scope === "account" ? " in this account" : " in all accounts"}`);
</script>

<svelte:window onkeydown={winKey} onkeydowncapture={selKey} />
<section class="list" aria-label="Messages" bind:this={sectionEl}>
  <header>
    <h2>{title === "other" ? app.folder : title}</h2>
    <span class="tools">
      <button class="iconbtn" aria-label="Group into conversations" aria-pressed={prefs.v.threading} class:on={prefs.v.threading} title="Group messages into conversations (←/→ collapse and expand)" disabled={org.backend} onclick={() => prefs.set("threading", !prefs.v.threading)}>
        <svg viewBox="0 0 24 24"><path d="M4 5h12v8H9l-3 3v-3H4zM18 9h2v9h-2v3l-3-3H9v-2"/></svg>
      </button>
      <button class="iconbtn" aria-label="Select messages" aria-pressed={selectMode} title="Select messages (Cmd-click, Shift-click, x)" class:on={selectMode} disabled={org.backend} onclick={() => { selectMode = !selectMode; if (!selectMode) org.clearSel(); }}>
        <svg viewBox="0 0 24 24"><rect x="4" y="4" width="16" height="16" rx="2"/><path d="M8 12l3 3 5-6"/></svg>
      </button>
      <button class="iconbtn" aria-label="Jump to date" aria-expanded={jumpOpen} title="Jump to date…" disabled={!!app.bulk || (!app.moreAvailable && !app.messages.length)} onclick={() => { jumpOpen = !jumpOpen; }}>
        <svg viewBox="0 0 24 24"><path d="M4 6h16v14H4zM4 10h16M8 3v4M16 3v4"/></svg>
      </button>
      <button class="iconbtn" aria-label="Refresh" title="Refresh" onclick={() => app.sync()} class:spin={app.syncing}>
        <svg viewBox="0 0 24 24"><path d="M20 11a8 8 0 10-2.3 5.7M20 4v7h-7"/></svg>
      </button>
    </span>
    {#if jumpOpen}
      <div class="jump" role="dialog" aria-label="Jump to date" tabindex="-1" onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); jumpOpen = false; } else if (e.key === "Enter" && jumpTs != null && (e.target as HTMLElement).getAttribute("role") !== "combobox") goJump(); }}>
        <label class="label" for="jump-date">Jump to date</label>
        <DateTimeSelect mode="date" idPrefix="jump-date" ariaPrefix="Jump to" past days={730} max={today} emptyLabel="Pick a date" value={jumpDate} onchange={(v) => (jumpDate = v)} />
        <p class="note">Loads older messages until this day is listed, or the folder starts. You can cancel while it runs.</p>
        <div class="jacts"><button class="btn sm" disabled={jumpTs == null} onclick={goJump}>Jump</button><button class="btn ghost sm" onclick={() => { jumpOpen = false; }}>Close</button></div>
      </div>
    {/if}
  </header>
  <div class="search">
    <div class="field">
      <label class="sr-only" for="search">Search messages</label>
      <input id="search" type="search" placeholder="Search  ( / )" autocomplete="off" spellcheck="false" value={text} bind:this={searchEl}
        role="combobox" aria-expanded={popOpen} aria-controls="sugg" aria-autocomplete="list" aria-activedescendant={popOpen ? `sg${active}` : undefined}
        oninput={(e) => push(e.currentTarget.value)} onfocus={() => { focused = true; }} onblur={() => { focused = false; }} onkeydown={onKey} />
      <button class="iconbtn q" aria-label="Search operators" aria-expanded={help} title="Search operators" onclick={() => { help = !help; }}>?</button>
    </div>
    {#if popOpen}
      <ul id="sugg" class="pop" role="listbox" aria-label="Suggestions">
        {#each sug.items as it, i (it.insert)}
          <li id="sg{i}" role="option" aria-selected={i === active} class:on={i === active} onmousedown={(e) => { e.preventDefault(); accept(i); }}>
            <span class="mono">{it.label}</span><span class="d">{it.desc}</span>
          </li>
        {/each}
      </ul>
    {/if}
    {#if help}
      <div class="pop help" role="region" aria-label="Search operators">
        {#each OPERATORS as o (o.op)}<div><span class="mono">{o.op}</span><span class="d">{o.desc}</span></div>{/each}
        <div class="note">Prefix with <span class="mono">-</span> to exclude; quote phrases: <span class="mono">subject:"quarterly report"</span>. Plain words match subject, preview, from and to. “This folder” filters the loaded list instantly; “Account” and “All accounts” (and cc:, has:attachment, folder:) search everything cached on this Mac, body text included for messages whose body was downloaded. If nothing is cached, ask the mail server from the button under the results.</div>
      </div>
    {/if}
    <div class="scope" role="group" aria-label="Search in">
      {#each SCOPES as sc (sc.v)}
        <button class:on={org.scope === sc.v} aria-pressed={org.scope === sc.v} onclick={() => { org.scope = sc.v; }}>{sc.label}</button>
      {/each}
      <button class="save" title="Save this search in the sidebar" aria-label="Save this search" disabled={!app.query.trim()} onclick={() => org.saveCurrent()}>
        <svg viewBox="0 0 24 24"><path d="M12 3l2.6 5.600 6.100.700-4.500 4.200 1.200 6L12 16.600 6.600 19.500l1.200-6L3.300 9.300l6.100-.700z"/></svg>Save
      </button>
    </div>
    {#if org.parked && !org.backend}
      <button class="back" onclick={() => org.backToResults()}>← Back to search results</button>
    {/if}
    <div class="chips" role="group" aria-label="Quick filters">
      <button class="chip" aria-pressed={noChips} onclick={() => { app.chips = { unread: false, flagged: false, today: false, week: false }; vipOnly = false; }}>All</button>
      {#each chipDefs as c (c.key)}
        <button class="chip" aria-pressed={app.chips[c.key]} onclick={() => { app.chips[c.key] = !app.chips[c.key]; }}>
          {c.label}{#if c.key === "unread" && unreadN}<span class="n">{unreadN}</span>{:else if c.key === "flagged" && flaggedN}<span class="n">{flaggedN}</span>{/if}
        </button>
      {/each}
      <button class="chip" aria-pressed={vipOnly} disabled={org.backend} title="Only mail from your VIP senders (add them from the message menu)" onclick={() => { vipOnly = !vipOnly; }}>VIP{#if vipN}<span class="n">{vipN}</span>{/if}</button>
    </div>
    <div class="meta" aria-live="polite">
      <div class="toks">
        {#each view.terms as t (t.start)}
          <span class="tok" class:neg={t.neg}><span class="mono">{t.raw}</span><button aria-label="Remove {t.raw}" onclick={() => dropToken(t)}>×</button></span>
        {/each}
        {#if org.backend ? org.hints.length : app.parsed.hints.length}<span class="hint">{org.backend ? org.hints[0] : app.parsed.hints[0]}</span>{/if}
      </div>
      <span class="count mono">{org.backend ? (org.searching && !org.results ? "Searching…" : resultsLabel) : app.filterActive ? `${fmtN(app.filtered.length)} of ${fmtN(app.messages.length)}` : fmtN(app.messages.length)} {org.backend ? "" : "messages"}</span>
      {#if app.filterActive}<button class="clr" onclick={clearAll}>Clear</button>{/if}
    </div>
  </div>

  {#if org.count > 0 && !org.backend}
    <div class="selbar" role="toolbar" aria-label="Actions for selected messages">
      <span class="mono n">{fmtN(org.count)} selected</span>
      <button class="sb" onclick={() => org.moveUids([...org.sel.set], { role: "archive" })}>Archive</button>
      <Select id="bulk-move" value="" placeholder="Move to…" compact block={false} ariaLabel="Move to folder" options={moveTargets.map((f) => ({ value: f.name, label: f.role === "other" ? f.name : f.role }))} onchange={(v) => { moveTo = v; bulkMove(); }} />
      <button class="sb" onclick={() => org.moveUids([...org.sel.set], { role: "trash" })}>{folderRole === "trash" ? "Delete forever" : "Trash"}</button>
      {#if folderRole === "junk"}<button class="sb" onclick={() => org.moveUids([...org.sel.set], { role: "inbox" })}>Not junk</button>
      {:else if folderRole !== "trash"}<button class="sb" onclick={() => org.moveUids([...org.sel.set], { role: "junk" })}>Junk</button>{/if}
      <button class="sb" onclick={() => org.flagUids([...org.sel.set], "seen", true)}>Read</button>
      <button class="sb" onclick={() => org.flagUids([...org.sel.set], "seen", false)}>Unread</button>
      <button class="sb" onclick={() => org.flagUids([...org.sel.set], "flagged", true)}>Flag</button>
      <button class="sb" onclick={() => org.flagUids([...org.sel.set], "flagged", false)}>Unflag</button>
      <span class="grow"></span>
      {#if org.count < rows.length}<button class="sb" onclick={() => org.selectAllRows()}>Select all {fmtN(rows.length)}</button>{/if}
      <button class="sb" onclick={() => { org.clearSel(); selectMode = false; }}>Clear</button>
    </div>
  {/if}

  {#if app.bulk}
    <div class="bulk" role="status">
      <span class="spinner" aria-hidden="true"></span>
      <span class="mono">Loading older messages… {fmtN(app.messages.length)}{app.serverTotal != null ? ` of ${fmtN(app.serverTotal)}` : ""}</span>
      <button class="clr" onclick={() => app.cancelBulk()}>Cancel</button>
    </div>
  {/if}

  <div class="rows" role="listbox" aria-label="Message list" aria-busy={app.olderBusy} bind:this={rowsEl} onscroll={onScroll}>
    {#if app.listLoading}
      {#each [0, 1, 2, 3, 4, 5, 6] as i (i)}
        <div class="sk"><div class="skeleton" style="width:{40 + (i * 13) % 30}%;height:12px"></div><div class="skeleton" style="width:{70 - (i * 7) % 25}%;height:12px"></div><div class="skeleton" style="width:90%;height:10px"></div></div>
      {/each}
    {:else}
      {#if rows.length === 0}
        <div class="empty">
          {#if org.backend}
            {#if org.searching}<strong>Searching…</strong>
            {:else if org.searchError}<strong>Search failed</strong><span class="selectable">{org.searchError}</span>
            {:else}<strong>No matches</strong><span>Nothing cached matches. The mail server can look through mail that isn't downloaded yet.</span>{/if}
            <button class="clr big" disabled={org.serverBusy || org.searching} onclick={() => org.searchServer()}>{org.serverBusy ? "Asking the server…" : "Search the mail server too"}</button>
            {#if org.serverNote}<span class="selectable">{org.serverNote}</span>{/if}
          {:else if vipOnly && !app.filterActive}<strong>No VIP mail</strong><span>None of the loaded messages is from a VIP sender. Add one from a message's right-click menu.</span>
            <button class="clr big" onclick={() => { vipOnly = false; }}>Show all</button>
          {:else if app.filterActive}<strong>No matches</strong><span>Nothing in the {fmtN(app.messages.length)} loaded messages matches.</span>
            <button class="clr big" onclick={clearAll}>Clear filters</button>
          {:else}<strong>Nothing here</strong><span>{app.syncing ? "Fetching messages…" : "This folder is empty. New mail will appear automatically."}</span>{/if}
        </div>
      {:else}
        <div class="inner" style="padding-top:{win.start * eff}px;padding-bottom:{(rows.length - win.end) * eff}px">
          {#each visible as r, vi (rowKey(r.m))}
            {@const m = r.m}
            {@const cur = !org.backend && m.uid === app.selectedUid}
            {@const picked = !org.backend && isPicked(r)}
            {@const col = org.backend ? 0 : (org.colours[m.uid] ?? 0)}
            {@const rowUnread = r.head && !r.expanded ? r.unread > 0 : !m.flags.seen}
            {@const pin = !org.backend && local.isPinned(m.uid)}
            {@const mute = !org.backend && local.isMuted(m.uid)}
            {@const star = !org.backend && local.isVip(m.from)}
            <div class="row" role="option" tabindex="-1" aria-selected={picked || cur} aria-setsize={display.length} aria-posinset={win.start + vi + 1} class:sel={cur} class:multi={picked} class:unread={rowUnread} class:child={r.depth === 1} class:muted={mute}
              data-uid={m.uid} onclick={(e) => rowClick(e, m, r)} onkeydown={() => {}} oncontextmenu={(e) => rowMenu(e, m, r)}
              draggable={!org.backend}
              ondragstart={(e) => {
                const many = picked && org.count > 1;
                const wholeThread = !many && stands(r);
                app.dragMsg = { uid: m.uid, folder: m.folder, uids: many ? [...org.sel.set] : wholeThread ? r.uids : undefined };
                if (e.dataTransfer) { e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", many ? `${org.count} messages` : wholeThread ? `${r.uids.length} messages` : m.subject || "message"); }
              }}
              ondragend={() => { app.dragMsg = null; app.dropFolder = null; }}>
              <span class="mark">
                {#if !org.backend && (selectMode || org.count > 0)}
                  <input type="checkbox" class="pick" checked={picked} tabindex="-1" aria-label="Select message" onclick={(e) => e.stopPropagation()} onchange={() => togglePick(r)} />
                {:else if m.flags.flagged}<span class="flag" role="img" aria-label="Flagged"></span>{:else if rowUnread && prefs.v.showDot}<span class="udot" role="img" aria-label="Unread"></span>{/if}
              </span>
              <div class="body">
                <div class="l1"><span class="from ell">{#if col}<span class="cdot" style="background:{COLOUR_CSS[col]}" role="img" aria-label="{COLOURS[col]} label"></span>{/if}{addrLabel(m.from)}</span>
                  <span class="icons">
                    {#if star}<svg class="ic vipic" viewBox="0 0 24 24" role="img" aria-label="VIP sender"><path d="M12 3l2.6 5.6 6.1.7-4.5 4.2 1.2 6L12 16.6 6.6 19.5l1.2-6L3.3 9.3l6.1-.7z"/></svg>{/if}
                    {#if mute}<svg class="ic" viewBox="0 0 24 24" role="img" aria-label="Muted conversation"><path d="M6 9a6 6 0 0112 0c0 5 2 6 2 6H4s2-1 2-6M10 19a2 2 0 004 0M4 4l16 16"/></svg>{/if}
                    {#if pin}<svg class="ic pinic" viewBox="0 0 24 24" role="img" aria-label="Pinned"><path d="M9 3h6l-1 6 3 3H7l3-3zM12 12v9"/></svg>{/if}
                    <span class="mono date">{listDate(m.date, prefs.v.dateFormat)}</span>
                  </span></div>
                {#if org.backend && (org.scope !== "folder" || m.folder !== app.folder)}<div class="where mono ell">{org.scope === "all" ? m.account + " · " : ""}{m.folder}</div>{/if}
                <div class="l2"><span class="subj ell">{m.subject || "(no subject)"}</span>
                  {#if r.head && r.key}
                    <button class="tgl" aria-expanded={r.expanded} aria-label="{r.expanded ? 'Collapse' : 'Expand'} conversation of {r.count} messages{r.unread ? `, ${r.unread} unread` : ''}" title={r.expanded ? "Collapse conversation (←)" : "Expand conversation (→)"}
                      onclick={(e) => { e.stopPropagation(); if (r.expanded && r.key && app.selectedUid != null && r.uids.includes(app.selectedUid) && app.selectedUid !== m.uid) app.selectedUid = m.uid; local.toggleExpanded(r.key!); }}>
                      <svg viewBox="0 0 24 24" class:open={r.expanded}><path d="M9 6l6 6-6 6"/></svg><span class="tn mono">{r.count}</span>{#if r.unread}<span class="tu mono">{r.unread} new</span>{/if}
                    </button>
                  {/if}
                </div>
                {#if prefs.v.snippetLines > 0}<div class="snip">{m.snippet}</div>{/if}
              </div>
            </div>
          {/each}
        </div>
      {/if}

      <div class="foot">
        {#if org.backend}
          {#if org.moreResults}<button class="btn ghost sm" disabled={org.searching} onclick={() => org.loadMoreResults()}>Show more results</button>{/if}
          {#if rows.length}<button class="clr" disabled={org.serverBusy} onclick={() => org.searchServer()}>{org.serverBusy ? "Asking the server…" : "Search the mail server too"}</button>{/if}
          {#if org.serverNote}<div class="selectable">{org.serverNote}</div>{/if}
        {:else}
        {#if searchOlder}
          <div class="hint2">Showing matches among the {fmtN(app.messages.length)} loaded messages only.
            <button class="clr" disabled={app.olderBusy || !!app.bulk} onclick={() => app.loadMore()}>Search older messages: load more</button></div>
        {/if}
        {#if app.messages.length || app.serverTotal != null}<div class="cnt mono" aria-live="polite">{loadedLabel(app.messages.length, app.serverTotal)}</div>{/if}
        {#if app.olderBusy && !app.bulk}
          <div class="busy" role="status"><span class="spinner" aria-hidden="true"></span> Loading older messages…</div>
        {:else if app.olderError}
          <div class="err" role="alert"><span class="selectable">{app.olderError}</span>
            <button class="btn ghost sm" onclick={() => app.loadMore()}>Retry</button></div>
        {:else if app.moreAvailable && !app.bulk}
          <div class="acts">
            <button class="btn ghost sm" bind:this={moreBtn} onclick={() => app.loadMore()}>{moreLabel}</button>
            <button class="btn ghost sm" onclick={() => app.loadAll()}>Load all</button>
          </div>
        {:else if app.serverTotal != null && app.messages.length && !app.bulk}
          <div class="end mono">Start of folder</div>
        {/if}
        {/if}
      </div>
    {/if}
  </div>
</section>

{#if cmenu}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrimmenu" onmousedown={() => (cmenu = null)} oncontextmenu={(e) => { e.preventDefault(); cmenu = null; }} onkeydown={(e) => e.key === "Escape" && (cmenu = null)}></div>
  <div class="cmenu" role="menu" style="left:{cmenu.x}px;top:{cmenu.y}px">
    <div class="ch mono">{menuUids.length > 1 ? `${fmtN(menuUids.length)} messages` : "Message"}</div>
    <button role="menuitem" onclick={() => menuDo(() => void org.moveUids(menuUids, { role: "archive" }))}>Archive</button>
    <button role="menuitem" onclick={() => menuDo(() => void org.moveUids(menuUids, { role: "trash" }))}>{folderRole === "trash" ? "Delete forever" : "Move to Trash"}</button>
    {#if folderRole === "junk"}<button role="menuitem" onclick={() => menuDo(() => void org.moveUids(menuUids, { role: "inbox" }))}>Not Junk</button>
    {:else if folderRole !== "trash"}<button role="menuitem" onclick={() => menuDo(() => void org.moveUids(menuUids, { role: "junk" }))}>Mark as Junk</button>{/if}
    <button role="menuitem" onclick={() => menuDo(() => void org.flagUids(menuUids, "seen", false))}>Mark as unread</button>
    {#if menuUids.length === 1 && folderRole !== "trash"}<button role="menuitem" onclick={() => menuDo(() => snoozeUi.open(menuUids[0]))}>Snooze…</button>{/if}
    <button role="menuitem" onclick={() => menuDo(() => void org.flagUids(menuUids, "seen", true))}>Mark as read</button>
    <button role="menuitem" onclick={() => menuDo(() => void org.flagUids(menuUids, "flagged", true))}>Flag</button>
    <div class="ch mono">Conversation and sender</div>
    <button role="menuitem" onclick={() => menuDo(() => void local.setPinned(menuUids, !menuPinned))}>{menuPinned ? "Unpin" : "Pin to top"}</button>
    <button role="menuitem" onclick={() => menuDo(() => void local.setMuted(menuUids, !menuMuted))}>{menuMuted ? "Unmute conversation" : "Mute conversation"}</button>
    {#if menuMsg?.from}
      <button role="menuitem" onclick={() => menuDo(() => void local.setVip(menuMsg, !local.isVip(menuMsg.from)))}>{local.isVip(menuMsg.from) ? "★ Remove sender from VIP" : "☆ Add sender to VIP"}</button>
      <button role="menuitem" onclick={() => menuDo(() => void local.blockSender(menuMsg))}>Block sender…</button>
    {/if}
    <div class="ch mono">Colour label</div>
    <div class="sw" role="group" aria-label="Colour label">
      {#each [1, 2, 3, 4, 5, 6, 7] as c (c)}
        <button class="swatch" style="background:{COLOUR_CSS[c]}" title={COLOURS[c]} aria-label={COLOURS[c]} onclick={() => menuDo(() => void org.setColour(menuUids, c))}></button>
      {/each}
      <button class="swatch none" title="No colour" aria-label="No colour" onclick={() => menuDo(() => void org.setColour(menuUids, 0))}>×</button>
    </div>
  </div>
{/if}

<style>
  .list { flex: 1; min-width: 0; display: flex; flex-direction: column; min-height: 0; background: var(--bg); }
  header { display: flex; align-items: center; justify-content: space-between; padding: 10px 14px 6px 16px; position: relative; }
  .tools { display: inline-flex; gap: 2px; }
  .iconbtn:disabled { opacity: .4; cursor: not-allowed; }
  .jump { position: absolute; right: 10px; top: calc(100% - 2px); z-index: 6; width: 250px; display: grid; gap: 8px; padding: 12px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); box-shadow: 0 8px 24px #0006; }
  .jump .note { margin: 0; padding: 0; border: 0; font-size: 0.7667rem; }
  .jacts { display: flex; gap: 8px; }
  .bulk { display: flex; align-items: center; gap: 8px; padding: 6px 14px; border-top: 1px solid var(--line); background: color-mix(in srgb, var(--accent) 10%, transparent); font-size: 0.7667rem; }
  .bulk .mono { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .spinner { width: 12px; height: 12px; flex: none; border: 2px solid var(--line); border-top-color: var(--accent); border-radius: 50%; animation: rot .8s linear infinite; display: inline-block; vertical-align: -2px; }
  .inner { box-sizing: content-box; }
  .foot { display: grid; gap: 8px; justify-items: center; padding: 14px 14px 22px; text-align: center; font-size: 0.8rem; color: var(--muted); }
  .foot .acts { display: flex; gap: 8px; flex-wrap: wrap; justify-content: center; }
  .foot .err { display: grid; gap: 8px; justify-items: center; color: var(--danger); overflow-wrap: anywhere; }
  .foot .busy { display: inline-flex; gap: 8px; align-items: center; }
  .foot .end { opacity: .7; }
  .hint2 { display: grid; gap: 4px; justify-items: center; }
  h2 { font-size: 1rem; letter-spacing: .12em; }
  .search { padding: 4px 12px 8px; position: relative; }
  .field { position: relative; }
  .field input { padding-right: 30px; }
  .q { position: absolute; right: 2px; top: 50%; transform: translateY(-50%); width: 24px; height: 24px; font: 600 0.8rem var(--font-mono); color: var(--muted); }
  .pop { position: absolute; left: 12px; right: 12px; top: 40px; z-index: 5; margin: 0; padding: 4px; list-style: none; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); box-shadow: 0 8px 24px #0006; font-size: 0.8rem; }
  .pop li, .help > div:not(.note) { display: flex; gap: 10px; padding: 4px 6px; border-radius: 3px; cursor: pointer; align-items: baseline; }
  .pop li.on { background: color-mix(in srgb, var(--accent) 22%, transparent); }
  .pop .mono { color: var(--ink); flex: none; min-width: 84px; }
  .pop .d { color: var(--muted); }
  .help { padding: 8px; max-height: 60vh; overflow: auto; }
  .help > div { cursor: default !important; }
  .note { margin-top: 6px; padding: 6px; color: var(--muted); line-height: 1.4; border-top: 1px solid var(--line); }
  .chips { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 8px; }
  .chip { font: 500 0.8rem var(--font-ui); letter-spacing: .04em; color: var(--text); background: transparent; border: 1px solid var(--line); border-radius: 999px; padding: 3px 10px; cursor: pointer; display: inline-flex; gap: 6px; align-items: center; }
  .chip:hover { border-color: var(--muted); }
  .chip:focus-visible, .clr:focus-visible, .tok button:focus-visible, .q:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .chip[aria-pressed="true"] { background: var(--accent); color: var(--bg); border-color: var(--accent); }
  .n { font: 500 0.6667rem var(--font-mono); opacity: .8; }
  .meta { display: flex; align-items: center; gap: 8px; height: 24px; margin-top: 6px; font-size: 0.7333rem; color: var(--muted); }
  .toks { flex: 1; min-width: 0; display: flex; gap: 4px; align-items: center; overflow-x: auto; scrollbar-width: none; white-space: nowrap; }
  .tok { display: inline-flex; align-items: center; gap: 2px; border: 1px solid var(--line); border-radius: 3px; padding: 0 2px 0 6px; color: var(--text); }
  .tok.neg { border-color: var(--danger); }
  .tok button { background: none; border: 0; color: var(--muted); cursor: pointer; padding: 0 4px; font-size: 0.9333rem; line-height: 1; }
  .hint { color: var(--danger); }
  .count { flex: none; }
  .clr { flex: none; background: none; border: 0; color: var(--accent); cursor: pointer; font: 500 0.7333rem var(--font-ui); letter-spacing: .08em; text-transform: uppercase; padding: 2px 4px; }
  .clr.big { justify-self: center; border: 1px solid var(--line); border-radius: var(--radius); padding: 6px 12px; margin-top: 6px; }
  .search input { width: 100%; background: transparent; color: var(--ink); border: 1px solid var(--line); border-radius: var(--radius); padding: 7px 10px; font: 400 0.8rem var(--font-mono); }
  .search input:focus { outline: 2px solid var(--accent); border-color: transparent; }
  .rows { overflow-y: auto; flex: 1; border-top: 1px solid var(--line); }
  .row { display: flex; gap: 4px; padding: var(--row-py) 14px var(--row-py) 6px; border-bottom: 1px solid var(--line); border-left: 2px solid transparent; cursor: pointer; }
  .row:hover { background: color-mix(in srgb, var(--line) 40%, transparent); }
  .row.sel { border-left-color: var(--accent); background: color-mix(in srgb, var(--line) 55%, transparent); }
  .mark { width: 14px; flex: none; display: grid; justify-items: center; padding-top: 5px; }
  .udot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); }
  .flag { width: 7px; height: 7px; background: var(--signal); transform: rotate(45deg); }
  .body { min-width: 0; flex: 1; display: grid; gap: 2px; }
  .l1 { display: flex; justify-content: space-between; gap: 8px; align-items: baseline; }
  .from { color: var(--text); font-size: 0.9333rem; }
  .unread .from, .unread .subj { color: var(--ink); font-weight: 600; }
  .date { color: var(--muted); flex: none; font-size: 0.7333rem; }
  .subj { font-size: 0.9333rem; color: var(--text); }
  .snip { min-height: calc(var(--snip-lines) * 1lh); font-size: 0.8667rem; color: var(--muted); display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: var(--snip-lines); line-clamp: var(--snip-lines); overflow: hidden; overflow-wrap: anywhere; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .sk { padding: 14px 16px; border-bottom: 1px solid var(--line); display: grid; gap: 8px; }
  .empty { padding: 40px 24px; display: grid; gap: 6px; text-align: center; color: var(--muted); font-size: 0.9333rem; }
  .empty strong { color: var(--ink); text-transform: uppercase; letter-spacing: .1em; font-weight: 600; }
  .spin svg { animation: rot 1s linear infinite; }
  .iconbtn.on { color: var(--accent); }
  .scope { display: flex; gap: 4px; align-items: center; margin-top: 8px; }
  .scope button { font: 500 0.7333rem var(--font-ui); letter-spacing: .06em; color: var(--muted); background: transparent; border: 0; border-bottom: 2px solid transparent; padding: 2px 6px; cursor: pointer; }
  .scope button.on { color: var(--ink); border-bottom-color: var(--signal); }
  .scope button:disabled { opacity: .4; cursor: default; }
  .scope .save { margin-left: auto; display: inline-flex; gap: 4px; align-items: center; }
  .scope .save svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linejoin: round; }
  .back { margin-top: 8px; background: none; border: 0; color: var(--accent); cursor: pointer; font: 500 0.7667rem var(--font-ui); padding: 2px 0; }
  .selbar { display: flex; flex-wrap: wrap; gap: 4px 6px; align-items: center; padding: 6px 12px; border-top: 1px solid var(--line); background: color-mix(in srgb, var(--accent) 12%, transparent); font-size: 0.7667rem; }
  .selbar .n { color: var(--ink); margin-right: 4px; }
  .selbar .grow { flex: 1; }
  .sb { font: 500 0.7333rem var(--font-ui); letter-spacing: .04em; color: var(--text); background: transparent; border: 1px solid var(--line); border-radius: var(--radius); padding: 2px 8px; cursor: pointer; }
  .sb:hover { border-color: var(--muted); color: var(--ink); }
  .row.multi { background: color-mix(in srgb, var(--accent) 16%, transparent); }
  .pick { width: 13px; height: 13px; margin: 0; accent-color: var(--accent); cursor: pointer; }
  .cdot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; margin-right: 6px; vertical-align: 0; }
  .where { font-size: 0.6833rem; color: var(--muted); }
  .l1 .icons { display: inline-flex; align-items: center; gap: 6px; flex: none; }
  .ic { width: 12px; height: 12px; fill: none; stroke: var(--muted); stroke-width: 1.8; stroke-linecap: round; stroke-linejoin: round; flex: none; }
  .vipic { stroke: var(--signal); fill: color-mix(in srgb, var(--signal) 55%, transparent); }
  .pinic { stroke: var(--accent); }
  .row.muted .from, .row.muted .subj { opacity: .6; }
  .row.child { padding-left: 26px; }
  .l2 { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .l2 .subj { flex: 1; }
  .tgl { flex: none; display: inline-flex; align-items: center; gap: 4px; background: none; border: 1px solid var(--line); border-radius: 999px; padding: 0 6px 0 2px; color: var(--muted); cursor: pointer; line-height: 1.2; font-size: 0.6833rem; }
  .tgl:hover { border-color: var(--muted); color: var(--ink); }
  .tgl:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .tgl svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; transition: transform .12s; }
  .tgl svg.open { transform: rotate(90deg); }
  .tgl .tu { color: var(--accent); }
  .chip:disabled { opacity: .4; cursor: not-allowed; }
  .scrimmenu { position: fixed; inset: 0; z-index: 80; }
  .cmenu { position: fixed; z-index: 81; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); padding: 4px; min-width: 200px; }
  .cmenu button[role="menuitem"] { display: block; width: 100%; text-align: left; background: none; border: 0; padding: 6px 10px; color: var(--ink); font-size: 0.8667rem; cursor: pointer; }
  .cmenu button[role="menuitem"]:hover { background: color-mix(in srgb, var(--line) 50%, transparent); }
  .ch { padding: 6px 10px 2px; font-size: 0.6667rem; letter-spacing: .1em; text-transform: uppercase; color: var(--muted); }
  .sw { display: flex; gap: 6px; padding: 4px 10px 8px; }
  .swatch { width: 18px; height: 18px; border-radius: 50%; border: 1px solid var(--line); cursor: pointer; padding: 0; color: var(--muted); font-size: 0.7rem; line-height: 1; }
  .swatch.none { background: transparent; }
  @keyframes rot { to { transform: rotate(360deg); } }
</style>
