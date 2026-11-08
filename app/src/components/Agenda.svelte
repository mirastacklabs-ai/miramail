<script lang="ts">
  import { cal } from "../lib/cal.svelte";
  import { dayKey, daysIn, fmtTime, occSpan, sameDay, sod, touchesDay } from "../lib/caldate";
  import type { Occurrence } from "../lib/types";
  const items = $derived(cal.occ.map((o) => ({ o, sp: occSpan(o) })).sort((a, b) => Number(b.sp.allDay) - Number(a.sp.allDay) || a.sp.start.getTime() - b.sp.start.getTime()));
  const groups = $derived(daysIn(cal.range).map((d) => ({ d, list: items.filter((x) => touchesDay(x.sp, d)) })).filter((g) => g.list.length));
  const today = sod(new Date());
  function open(e: MouseEvent, o: Occurrence) { const r = (e.currentTarget as HTMLElement).getBoundingClientRect(); cal.popover = { occ: o, x: r.left + 280, y: r.top }; }
</script>

<div class="ag" role="list" aria-label="Agenda, {cal.title}">
  {#if cal.loading}
    {#each [0, 1, 2, 3] as i (i)}<div class="skeleton sk" style="width:{80 - i * 9}%"></div>{/each}
  {:else if groups.length === 0}
    <div class="empty"><strong>Nothing scheduled</strong><span>No events in the next 30 days. Press <span class="mono">n</span> to add one.</span></div>
  {:else}
    {#each groups as g (dayKey(g.d))}
      <section class="day" role="listitem" aria-label={g.d.toLocaleDateString([], { dateStyle: "full" })}>
        <h3 class:today={sameDay(g.d, today)}><span class="mono n">{g.d.getDate()}</span> {g.d.toLocaleDateString([], { weekday: "long", month: "short" })}</h3>
        <ul>
          {#each g.list as x (x.o.uid + x.o.recurrenceStart)}
            <li><button class="it" style="--c:{x.o.color}" onclick={(e) => open(e, x.o)}>
              <span class="tm mono">{x.sp.allDay ? "all day" : `${fmtTime(x.sp.start)} – ${fmtTime(x.sp.end)}`}</span>
              <span class="sm ell">{x.o.summary || "Untitled"}{#if x.o.hasZoom}<span class="zb">Zoom</span>{/if}</span>
              {#if x.o.location && !/^https?:/i.test(x.o.location)}<span class="loc mono ell">{x.o.location}</span>{/if}
            </button></li>
          {/each}
        </ul>
      </section>
    {/each}
  {/if}
</div>

<style>
  .ag { flex: 1; overflow-y: auto; padding: 8px 28px 32px; }
  .day { display: grid; grid-template-columns: 150px 1fr; gap: 12px; padding: 12px 0; border-bottom: 1px solid var(--line); }
  h3 { font-size: 12px; letter-spacing: .1em; color: var(--muted); font-weight: 500; } h3.today { color: var(--accent); }
  h3 .n { font-size: 16px; color: inherit; } h3:not(.today) .n { color: var(--ink); }
  ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 4px; }
  .it { width: 100%; display: grid; grid-template-columns: 150px 1fr auto; gap: 12px; align-items: center; text-align: left; background: none; border: 0; border-left: 3px solid var(--c); border-radius: 2px; padding: 6px 10px; color: var(--text); }
  .it:hover { background: color-mix(in srgb, var(--c) 14%, transparent); }
  .tm { color: var(--muted); } .sm { color: var(--ink); } .loc { color: var(--muted); max-width: 220px; }
  .zb { margin-left: 8px; font: 600 10px var(--font-mono); color: var(--accent); border: 1px solid var(--accent); border-radius: 2px; padding: 0 4px; }
  .sk { height: 18px; margin: 16px 0; }
  .empty { display: grid; gap: 8px; justify-items: center; margin: 80px auto; color: var(--muted); text-align: center; } .empty strong { color: var(--ink); text-transform: uppercase; letter-spacing: .12em; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  @media (max-width: 800px) { .day { grid-template-columns: 1fr; } }
</style>
