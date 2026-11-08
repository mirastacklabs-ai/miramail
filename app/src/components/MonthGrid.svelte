<script lang="ts">
  import { cal } from "../lib/cal.svelte";
  import { dayKey, daysIn, fmtTime, occSpan, sameDay, sod, touchesDay } from "../lib/caldate";
  import type { Occurrence } from "../lib/types";
  const MAX = 3;
  const days = $derived(daysIn(cal.range));
  const weeks = $derived(Array.from({ length: days.length / 7 }, (_, w) => days.slice(w * 7, w * 7 + 7)));
  const items = $derived(cal.occ.map((o) => ({ o, sp: occSpan(o) })).sort((a, b) => Number(b.sp.allDay) - Number(a.sp.allDay) || a.sp.start.getTime() - b.sp.start.getTime()));
  const forDay = (d: Date) => items.filter((x) => touchesDay(x.sp, d));
  const today = sod(new Date());
  function open(e: MouseEvent, o: Occurrence) { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); cal.popover = { occ: o, x: r.right, y: r.top }; }
  function dbl(d: Date) { const s = new Date(d); s.setHours(9, 0, 0, 0); cal.newEvent({ start: s, end: new Date(s.getTime() + 3600000), allDay: false }); }
</script>

<div class="mg" role="grid" aria-label="Calendar month view, {cal.title}" style="--rows:{weeks.length}">
  <div class="row heads" role="row">
    {#each days.slice(0, 7) as d (d.getDay())}<div class="h" role="columnheader">{d.toLocaleDateString([], { weekday: "short" })}</div>{/each}
  </div>
  {#each weeks as w, wi (wi)}
    <div class="row" role="row">
      {#each w as d (dayKey(d))}
        {@const list = forDay(d)}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="cell" role="gridcell" tabindex="-1" class:out={d.getMonth() !== cal.anchor.getMonth()} class:today={sameDay(d, today)} ondblclick={() => dbl(d)}
          aria-label="{d.toLocaleDateString([], { dateStyle: 'full' })}, {list.length} event{list.length === 1 ? '' : 's'}">
          <button class="dn mono" aria-label="Open day {d.toLocaleDateString([], { dateStyle: 'full' })}" onclick={() => { cal.mode = "day"; cal.go(d); }}>{d.getDate()}</button>
          {#each list.slice(0, MAX) as x (x.o.uid + x.o.recurrenceStart)}
            <button class="chip" class:allday={x.sp.allDay} style="--c:{x.o.color}" onclick={(e) => open(e, x.o)}
              aria-label="{x.o.summary || 'Untitled'}, {x.sp.allDay ? 'all day' : fmtTime(x.sp.start)}">
              {#if !x.sp.allDay}<span class="mono tm">{fmtTime(x.sp.start).replace(/\s?(AM|PM)/i, (m) => m.trim().toLowerCase()[0])}</span>{/if}
              <span class="ell">{x.o.summary || "Untitled"}</span>
            </button>
          {/each}
          {#if list.length > MAX}
            <button class="more mono" onclick={() => { cal.mode = "day"; cal.go(d); }} aria-label="{list.length - MAX} more events, open day">+{list.length - MAX} more</button>
          {/if}
        </div>
      {/each}
    </div>
  {/each}
  {#if cal.loading}<div class="skbar skeleton" aria-hidden="true"></div>{/if}
</div>

<style>
  .mg { flex: 1; min-height: 0; display: flex; flex-direction: column; position: relative; }
  .row { display: grid; grid-template-columns: repeat(7, minmax(0, 1fr)); }
  .row:not(.heads) { flex: 1; min-height: 0; }
  .heads { border-bottom: 1px solid var(--line); }
  .h { padding: 6px 8px; font-size: 11px; text-transform: uppercase; letter-spacing: .12em; color: var(--muted); }
  .cell { border-right: 1px solid var(--line); border-bottom: 1px solid var(--line); padding: 3px 4px; display: flex; flex-direction: column; gap: 2px; min-height: 0; overflow: hidden; }
  .cell.out { background: color-mix(in srgb, var(--bg-band) 70%, transparent); } .cell.out .dn { color: var(--muted); }
  .dn { align-self: flex-start; background: none; border: 0; color: var(--text); width: 24px; height: 24px; border-radius: 50%; padding: 0; font-size: 12px; }
  .dn:hover { background: var(--line); }
  .cell.today .dn { background: var(--accent); color: var(--bg); font-weight: 700; }
  .chip { display: flex; gap: 5px; align-items: baseline; background: none; border: 0; border-left: 3px solid var(--c); border-radius: 2px; color: var(--text); font-size: 12px; padding: 1px 5px; text-align: left; min-width: 0; }
  .chip.allday { background: color-mix(in srgb, var(--c) 30%, var(--bg)); color: var(--ink); }
  .chip:hover { background: color-mix(in srgb, var(--c) 25%, transparent); }
  .tm { color: var(--muted); font-size: 10px; flex: none; }
  .more { background: none; border: 0; color: var(--accent); font-size: 11px; text-align: left; padding: 1px 5px; }
  .more:hover { text-decoration: underline; }
  .skbar { position: absolute; top: 0; left: 0; right: 0; height: 2px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>
