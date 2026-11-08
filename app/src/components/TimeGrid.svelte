<script lang="ts">
  import { onMount, tick } from "svelte";
  import { cal } from "../lib/cal.svelte";
  import { clipToDay, dayKey, fmtTime, hourLabel, layoutColumns, occSpan, sameDay, snap, sod, touchesDay, type Span } from "../lib/caldate";
  import type { Occurrence } from "../lib/types";

  let { days }: { days: Date[] } = $props();
  const HH = 48;
  let scroller = $state<HTMLDivElement>();
  let now = $state(new Date());
  let drag = $state<{ day: string; a: number; b: number } | null>(null);

  const items = $derived(cal.occ.map((o) => ({ o, sp: occSpan(o) })));
  const allDay = (d: Date) => items.filter((x) => (x.sp.allDay || x.sp.end.getTime() - x.sp.start.getTime() >= 86400000) && touchesDay(x.sp, d));
  const timed = (d: Date) =>
    layoutColumns(items.filter((x) => !(x.sp.allDay || x.sp.end.getTime() - x.sp.start.getTime() >= 86400000)).flatMap((x) => {
      const c = clipToDay(x.sp, d); return c ? [{ item: x, start: c.start, end: c.end }] : [];
    }));
  const hasAllDay = $derived(days.some((d) => allDay(d).length > 0));
  const nowMin = $derived(now.getHours() * 60 + now.getMinutes());

  onMount(() => {
    const iv = setInterval(() => (now = new Date()), 30000);
    return () => clearInterval(iv);
  });
  // Scroll to ~1h before now (or 7:00) when the view changes.
  $effect(() => {
    void cal.mode; void days[0]?.getTime();
    tick().then(() => { if (scroller) scroller.scrollTop = Math.max(0, ((days.some((d) => sameDay(d, new Date())) ? new Date().getHours() : 8) - 1) * HH); });
  });

  function open(e: MouseEvent, o: Occurrence) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    cal.popover = { occ: o, x: r.right, y: r.top };
  }
  const minAt = (e: PointerEvent, el: HTMLElement) => Math.min(24 * 60, Math.max(0, snap(((e.clientY - el.getBoundingClientRect().top) / HH) * 60)));
  const at = (d: Date, min: number) => new Date(d.getFullYear(), d.getMonth(), d.getDate(), 0, min);
  function down(e: PointerEvent, d: Date) {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".ev")) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const m = minAt(e, el);
    drag = { day: dayKey(d), a: m, b: m };
  }
  function move(e: PointerEvent) { if (drag) drag.b = minAt(e, e.currentTarget as HTMLElement); }
  function up(d: Date) {
    if (!drag) return;
    const [s, en] = [Math.min(drag.a, drag.b), Math.max(drag.a, drag.b)];
    drag = null;
    if (en - s >= 15) cal.newEvent({ start: at(d, s), end: at(d, en), allDay: false });
  }
  function dbl(e: MouseEvent, d: Date) {
    if ((e.target as HTMLElement).closest(".ev")) return;
    const m = Math.floor(((e.clientY - (e.currentTarget as HTMLElement).getBoundingClientRect().top) / HH)) * 60;
    cal.newEvent({ start: at(d, m), end: at(d, m + 60), allDay: false });
  }
  const label = (x: { o: Occurrence; sp: Span }) =>
    `${x.o.summary || "Untitled"}, ${x.sp.allDay ? "all day" : fmtTime(x.sp.start) + " to " + fmtTime(x.sp.end)}${x.o.location ? ", " + x.o.location : ""}${x.o.hasZoom ? ", Zoom" : ""}`;
</script>

<div class="tg" role="grid" aria-label="Calendar {cal.mode} view, {cal.title}" aria-rowcount="3" style="--n:{days.length}">
  <div class="hdr" role="row">
    <div class="gut"></div>
    {#each days as d (dayKey(d))}
      <div class="dh" role="columnheader" class:today={sameDay(d, now)} aria-label={d.toLocaleDateString([], { dateStyle: "full" })}>
        <span class="wd">{d.toLocaleDateString([], { weekday: "short" })}</span><span class="dn mono">{d.getDate()}</span>
      </div>
    {/each}
  </div>
  {#if hasAllDay}
    <div class="adrow" role="row">
      <div class="gut mono">all-day</div>
      {#each days as d (dayKey(d))}
        <div class="adc" role="gridcell" aria-label="All-day events, {d.toLocaleDateString([], { dateStyle: 'full' })}">
          {#each allDay(d) as x (x.o.uid + x.o.recurrenceStart)}
            <button class="chip" style="--c:{x.o.color}" aria-label={label(x)} onclick={(e) => open(e, x.o)}><span class="ell">{x.o.summary || "Untitled"}</span></button>
          {/each}
        </div>
      {/each}
    </div>
  {/if}
  <div class="scroll" bind:this={scroller}>
    {#if cal.loading}<div class="skbar skeleton" aria-hidden="true"></div>{/if}
    <div class="body" style="height:{24 * HH}px">
      <div class="gut hours" aria-hidden="true">
        {#each Array.from({ length: 24 }, (_, h) => h) as h (h)}<div class="hl mono" style="top:{h * HH}px">{h === 0 ? "" : hourLabel(h)}</div>{/each}
      </div>
      <div class="cols" role="row">
        {#each days as d (dayKey(d))}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="col" role="gridcell" tabindex="-1" aria-label="{d.toLocaleDateString([], { dateStyle: 'full' })}. Drag to create an event." class:today={sameDay(d, now)}
            onpointerdown={(e) => down(e, d)} onpointermove={move} onpointerup={() => up(d)} onpointercancel={() => (drag = null)} ondblclick={(e) => dbl(e, d)}>
            {#each timed(d) as p (p.item.o.uid + p.item.o.recurrenceStart + dayKey(d))}
              {@const x = p.item}
              <button class="ev" style="--c:{x.o.color};top:{(p.start / 60) * HH}px;height:{Math.max(((p.end - p.start) / 60) * HH - 1, 14)}px;left:calc({(p.col / p.cols) * 100}% + 1px);width:calc({100 / p.cols}% - 3px)"
                aria-label={label(x)} onclick={(e) => open(e, x.o)}>
                <span class="t ell">{x.o.summary || "Untitled"}{#if x.o.isRecurring}<span aria-hidden="true"> ↻</span>{/if}</span>
                {#if p.end - p.start >= 40}<span class="s mono ell">{fmtTime(x.sp.start)}{x.o.hasZoom ? " · Zoom" : ""}</span>{/if}
              </button>
            {/each}
            {#if drag && drag.day === dayKey(d) && drag.a !== drag.b}
              <div class="sel" style="top:{(Math.min(drag.a, drag.b) / 60) * HH}px;height:{(Math.abs(drag.b - drag.a) / 60) * HH}px" aria-hidden="true"></div>
            {/if}
            {#if sameDay(d, now)}<div class="now" style="top:{(nowMin / 60) * HH}px" aria-hidden="true"></div>{/if}
          </div>
        {/each}
      </div>
    </div>
  </div>
</div>

<style>
  .tg { display: flex; flex-direction: column; min-height: 0; flex: 1; }
  .hdr, .adrow { display: grid; grid-template-columns: 52px repeat(var(--n), 1fr); border-bottom: 1px solid var(--line); flex: none; padding-right: 10px; }
  .gut { width: 52px; flex: none; color: var(--muted); font-size: 10px; text-align: right; padding: 4px 8px 0 0; text-transform: uppercase; }
  .dh { display: flex; flex-direction: column; align-items: center; padding: 6px 0; gap: 2px; border-left: 1px solid var(--line); }
  .wd { font-size: 11px; text-transform: uppercase; letter-spacing: .12em; color: var(--muted); }
  .dn { font-size: 15px; color: var(--ink); width: 28px; height: 28px; display: grid; place-items: center; border-radius: 50%; }
  .dh.today .wd { color: var(--accent); } .dh.today .dn { background: var(--accent); color: var(--bg); font-weight: 700; }
  .adc { border-left: 1px solid var(--line); padding: 3px; display: grid; gap: 2px; align-content: start; min-width: 0; }
  .chip { display: flex; background: color-mix(in srgb, var(--c) 28%, var(--bg)); border: 0; border-left: 3px solid var(--c); border-radius: 2px; color: var(--ink); font-size: 12px; padding: 2px 6px; text-align: left; min-width: 0; }
  .chip:hover { background: color-mix(in srgb, var(--c) 45%, var(--bg)); }
  .scroll { overflow-y: auto; flex: 1; min-height: 0; position: relative; }
  .body { position: relative; display: flex; }
  .hours { position: relative; }
  .hl { position: absolute; right: 8px; transform: translateY(-50%); color: var(--muted); font-size: 10px; }
  .cols { flex: 1; display: grid; grid-template-columns: repeat(var(--n), 1fr); min-width: 0; }
  .col { position: relative; border-left: 1px solid var(--line); touch-action: none; cursor: crosshair;
    background-image: repeating-linear-gradient(to bottom, transparent 0, transparent 47px, var(--line) 47px, var(--line) 48px); }
  .col.today { background-color: color-mix(in srgb, var(--accent) 4%, transparent); }
  .ev { position: absolute; display: flex; flex-direction: column; align-items: flex-start; overflow: hidden; text-align: left; cursor: pointer; padding: 2px 6px;
    background: color-mix(in srgb, var(--c) 30%, var(--bg)); border: 0; border-left: 3px solid var(--c); border-radius: 2px; color: var(--ink); font-size: 12px; line-height: 1.25; min-width: 0; }
  .ev:hover { background: color-mix(in srgb, var(--c) 48%, var(--bg)); z-index: 2; }
  .ev .t { font-weight: 600; max-width: 100%; } .ev .s { color: var(--text); font-size: 10.5px; max-width: 100%; opacity: .85; }
  .sel { position: absolute; left: 2px; right: 2px; background: color-mix(in srgb, var(--accent) 25%, transparent); border: 1px dashed var(--accent); border-radius: 2px; pointer-events: none; }
  .now { position: absolute; left: 0; right: 0; height: 2px; background: var(--accent); z-index: 3; pointer-events: none; }
  .now::before { content: ""; position: absolute; left: -4px; top: -3px; width: 8px; height: 8px; border-radius: 50%; background: var(--accent); }
  .skbar { position: absolute; top: 0; left: 52px; right: 0; height: 2px; z-index: 4; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>
