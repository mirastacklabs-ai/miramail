<script lang="ts">
  import { cal } from "../lib/cal.svelte";
  import { addMonths, dayKey, daysIn, rangeFor, sameDay, sod } from "../lib/caldate";
  import { relDate } from "../lib/util";
  import type { CalendarInfo } from "../lib/types";

  let mini = $state(new Date(cal.anchor.getFullYear(), cal.anchor.getMonth(), 1));
  $effect(() => { const a = cal.anchor; mini = new Date(a.getFullYear(), a.getMonth(), 1); });
  const cells = $derived(daysIn(rangeFor("month", mini, cal.ws)));
  const heads = $derived(cells.slice(0, 7).map((d) => d.toLocaleDateString([], { weekday: "narrow" })));
  const headLong = $derived(cells.slice(0, 7).map((d) => d.toLocaleDateString([], { weekday: "long" })));
  const today = sod(new Date());
  const inView = (d: Date) => d >= cal.range[0] && d < cal.range[1];

  let menuFor = $state<string | null>(null);
  let renaming = $state<string | null>(null);
  let draft = $state("");
  function startRename(c: CalendarInfo) { renaming = c.id; draft = c.name; menuFor = null; }
  function finishRename(c: CalendarInfo) { if (renaming === c.id) { renaming = null; void cal.rename(c, draft); } }
  const syncText = (c: CalendarInfo) => c.error ? c.error : c.kind === "local" ? "Local" : c.lastSync ? `Synced ${relDate(c.lastSync)}` : "Not synced yet";
  const hasRemote = $derived(cal.calendars.some((c) => c.kind === "caldav"));
</script>

<div class="panel">
  <button class="btn newbtn" onclick={() => cal.newEvent()}>New event</button>

  <section aria-label="Mini month">
    <div class="mhead">
      <button class="iconbtn" aria-label="Previous month" onclick={() => (mini = addMonths(mini, -1))}><svg viewBox="0 0 24 24"><path d="M15 6l-6 6 6 6"/></svg></button>
      <span class="label ttl" aria-live="polite">{mini.toLocaleDateString([], { month: "long", year: "numeric" })}</span>
      <button class="iconbtn" aria-label="Next month" onclick={() => (mini = addMonths(mini, 1))}><svg viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></button>
    </div>
    <div class="mgrid" role="grid" aria-label="Pick a day">
      <div class="mrow" role="row">{#each heads as h, i (i)}<span class="dow" role="columnheader" aria-label={headLong[i]}>{h}</span>{/each}</div>
      {#each Array.from({ length: cells.length / 7 }, (_, w) => cells.slice(w * 7, w * 7 + 7)) as week, wi (wi)}
        <div class="mrow" role="row">
          {#each week as d (dayKey(d))}
            <button role="gridcell" class="md" class:out={d.getMonth() !== mini.getMonth()} class:today={sameDay(d, today)} class:view={inView(d)}
              aria-label={d.toLocaleDateString([], { dateStyle: "full" })} aria-current={sameDay(d, today) ? "date" : undefined} aria-selected={sameDay(d, cal.anchor)}
              onclick={() => cal.go(d)}>{d.getDate()}</button>
          {/each}
        </div>
      {/each}
    </div>
  </section>

  <section class="cals" aria-label="Calendars">
    <div class="chead">
      <span class="label">Calendars</span>
      {#if hasRemote}
        <button class="iconbtn" class:spin={cal.syncing} aria-label="Sync calendars" title="Sync calendars" disabled={cal.syncing} onclick={() => cal.sync()}>
          <svg viewBox="0 0 24 24"><path d="M20 11a8 8 0 10-2.3 5.7M20 4v7h-7"/></svg>
        </button>
      {/if}
      <button class="iconbtn" aria-label="Add calendar" title="Add calendar" onclick={() => (cal.addOpen = true)}><svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg></button>
    </div>
    {#each cal.calendars as c (c.id)}
      <div class="crow">
        <label class="cl" style="--c:{c.color}">
          <input type="checkbox" checked={c.visible} onchange={(e) => cal.setVisible(c, e.currentTarget.checked)} aria-label="Show {c.name}" />
          <span class="box" aria-hidden="true"></span>
          {#if renaming === c.id}
            <!-- svelte-ignore a11y_autofocus -->
            <input class="rn" bind:value={draft} autofocus aria-label="Calendar name" onclick={(e) => e.preventDefault()}
              onkeydown={(e) => { e.stopPropagation(); if (e.key === "Enter") finishRename(c); else if (e.key === "Escape") renaming = null; }} onblur={() => finishRename(c)} />
          {:else}<span class="nm ell" title={c.name}>{c.name}</span>{/if}
        </label>
        <button class="iconbtn" aria-label="Options for {c.name}" aria-haspopup="menu" aria-expanded={menuFor === c.id} onclick={() => (menuFor = menuFor === c.id ? null : c.id)}>
          <svg viewBox="0 0 24 24"><circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/></svg>
        </button>
        {#if menuFor === c.id}
          <div class="menu" role="menu" tabindex="-1" onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); menuFor = null; } }}>
            <button role="menuitem" onclick={() => startRename(c)}>Rename</button>
            {#if c.kind === "caldav"}<button role="menuitem" onclick={() => { menuFor = null; void cal.sync(c.id); }}>Sync now</button>{/if}
            <button role="menuitem" class="dg" onclick={() => { menuFor = null; void cal.remove(c); }}>Remove</button>
          </div>
        {/if}
        <div class="sub mono ell" class:err={!!c.error} title={syncText(c)}>{syncText(c)}{c.readOnly ? " · read-only" : ""}</div>
      </div>
    {:else}
      {#each [0, 1] as i (i)}<div class="skeleton sk"></div>{/each}
    {/each}
  </section>

  <button class="zchip" onclick={() => (cal.zoomOpen = true)} aria-label="Zoom settings: {cal.zoom?.connected ? 'connected' : 'not connected'}">
    <span class="zd" class:on={cal.zoom?.connected}></span>
    <span class="label">Zoom</span>
    <span class="mono ell">{cal.zoom?.connected ? (cal.zoom.account ?? "connected") : cal.zoom?.configured ? "not connected" : "set up"}</span>
  </button>
</div>

<style>
  .panel { display: flex; flex-direction: column; gap: 16px; flex: 1; min-height: 0; overflow-y: auto; padding-right: 2px; }
  .newbtn { width: 100%; }
  .mhead { display: flex; align-items: center; justify-content: space-between; margin-bottom: 4px; }
  .ttl { text-align: center; }
  .mrow { display: grid; grid-template-columns: repeat(7, 1fr); }
  .dow { text-align: center; font-size: 10px; color: var(--muted); padding: 2px 0; text-transform: uppercase; }
  .md { background: none; border: 0; border-bottom: 2px solid transparent; border-radius: 0; padding: 4px 0; font: 400 11px var(--font-mono); color: var(--text); text-align: center; }
  .md.out { color: var(--muted); opacity: .6; }
  .md.view { background: color-mix(in srgb, var(--line) 55%, transparent); }
  .md:hover { color: var(--ink); background: var(--line); }
  .md.today { color: var(--accent); font-weight: 700; border-bottom-color: var(--accent); }
  .md[aria-selected="true"] { outline: 1px solid var(--accent); outline-offset: -1px; }
  .cals { display: grid; gap: 6px; }
  .chead { display: flex; align-items: center; gap: 2px; } .chead .label { flex: 1; }
  .crow { position: relative; display: grid; grid-template-columns: 1fr auto; align-items: center; gap: 0 2px; }
  .cl { display: flex; align-items: center; gap: 8px; min-width: 0; cursor: pointer; padding: 3px 0; }
  .cl input[type="checkbox"] { position: absolute; opacity: 0; width: 1px; height: 1px; }
  .box { width: 12px; height: 12px; border-radius: 2px; border: 2px solid var(--c); flex: none; }
  .cl input:checked + .box { background: var(--c); }
  .cl input:focus-visible + .box { outline: 2px solid var(--accent); outline-offset: 2px; }
  .nm { font-size: 13px; color: var(--text); letter-spacing: .04em; }
  .rn { background: transparent; color: var(--ink); border: 1px solid var(--accent); border-radius: var(--radius); font: 400 12px var(--font-mono); padding: 2px 6px; width: 100%; min-width: 0; }
  .sub { grid-column: 1 / -1; color: var(--muted); font-size: 10.5px; padding-left: 20px; }
  .sub.err { color: var(--danger); white-space: normal; }
  .menu { position: absolute; right: 0; top: 28px; z-index: 20; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); display: grid; min-width: 120px; }
  .menu button { background: none; border: 0; text-align: left; padding: 7px 12px; font-size: 12px; text-transform: uppercase; letter-spacing: .1em; color: var(--text); }
  .menu button:hover { background: var(--line); } .menu .dg { color: var(--danger); }
  .sk { height: 14px; }
  .zchip { margin-top: auto; display: flex; align-items: center; gap: 8px; background: none; border: 1px solid var(--line); border-radius: var(--radius); padding: 7px 10px; text-align: left; min-width: 0; }
  .zchip:hover { border-color: var(--muted); }
  .zchip .mono { color: var(--muted); flex: 1; }
  .zd { width: 7px; height: 7px; border-radius: 50%; border: 1.5px solid var(--muted); flex: none; } .zd.on { background: var(--accent); border-color: var(--accent); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .spin svg { animation: spin 1s linear infinite; } @keyframes spin { to { transform: rotate(360deg); } }
</style>
