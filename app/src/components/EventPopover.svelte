<script lang="ts">
  import { onMount } from "svelte";
  import { api, openExternal } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { cal } from "../lib/cal.svelte";
  import { fmtSpan, localTz, occSpan } from "../lib/caldate";
  import type { CalEvent, Occurrence } from "../lib/types";
  import DeleteBar from "./DeleteBar.svelte";

  let { occ, x, y }: { occ: Occurrence; x: number; y: number } = $props();
  let ev = $state<CalEvent | null>(null);
  let failed = $state(false);
  let confirming = $state(false);
  let el = $state<HTMLDivElement>();
  const W = 340;
  const left = $derived(Math.max(8, Math.min(x + 8, window.innerWidth - W - 8)));
  const top = $derived(Math.max(8, Math.min(y, window.innerHeight - 360)));
  const sp = $derived(occSpan(occ));
  const calName = $derived(cal.calendars.find((c) => c.id === occ.calendarId));
  const joinUrl = $derived(ev?.zoom?.joinUrl ?? null);
  const place = $derived(ev?.location && !/^https?:\/\//i.test(ev.location) ? ev.location : "");

  onMount(() => {
    el?.focus();
    api.calGetEvent(occ.uid, occ.calendarId).then((e) => (ev = e)).catch((e) => { failed = true; app.fail(e); });
  });
  async function join() {
    if (!joinUrl || !/^https:\/\//i.test(joinUrl)) { app.notify("This Zoom link is not a secure https address.", "error"); return; }
    const ok = await app.ask({ title: "Join Zoom meeting?", body: "This will open the meeting link in your default browser.", detail: joinUrl, ok: "Open" });
    if (!ok) return;
    try { await openExternal(joinUrl); } catch (e) { app.fail(e); }
  }
  async function remove(notify: boolean) { await cal.deleteEvent(occ.uid, occ.calendarId, notify); }
</script>

<div class="catch" role="presentation" onmousedown={() => (cal.popover = null)}></div>
<div class="pop" role="dialog" aria-label="Event details: {occ.summary}" tabindex="-1" bind:this={el} style="left:{left}px;top:{top}px;width:{W}px;--c:{occ.color}"
  onkeydown={(e) => { if (e.key === "Escape" && !app.confirmReq) { e.stopPropagation(); cal.popover = null; } }}>
  <header>
    <h3 class="selectable">{occ.summary || "Untitled"}</h3>
    <button class="iconbtn" aria-label="Close" onclick={() => (cal.popover = null)}><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg></button>
  </header>
  <dl class="selectable">
    <dt>When</dt><dd>{fmtSpan(sp)}{#if !occ.allDay && ev?.tzid && ev.tzid !== localTz()} <span class="mono sub">({ev.tzid})</span>{/if}{#if occ.isRecurring} <span class="mono sub">· repeats</span>{/if}</dd>
    {#if place}<dt>Where</dt><dd>{place}</dd>{/if}
    {#if ev?.zoom}<dt>Zoom</dt><dd><button class="btn sm" onclick={join}>Join Zoom meeting</button>{#if ev.zoom.password}<span class="mono sub"> passcode {ev.zoom.password}</span>{/if}</dd>{/if}
    {#if ev?.attendees.length}
      <dt>Guests</dt>
      <dd><ul>{#each ev.attendees as a (a.email)}<li><span class="st {a.status}" title={a.status} aria-label={a.status}></span><span class="ell">{a.name || a.email}</span></li>{/each}</ul></dd>
    {/if}
    {#if ev?.description}<dt>Notes</dt><dd class="desc">{ev.description}</dd>{/if}
    <dt>Calendar</dt><dd><span class="dot" style="background:{occ.color}"></span>{calName?.name ?? "—"}</dd>
  </dl>
  {#if !ev && !failed}<div class="skeleton" style="height:12px;width:60%"></div>{/if}
  {#if confirming}
    <DeleteBar hasAttendees={!!ev?.attendees.length} onconfirm={remove} oncancel={() => (confirming = false)} />
  {:else}
    <div class="acts">
      <button class="btn danger sm" disabled={calName?.readOnly} onclick={() => (confirming = true)}>Delete</button>
      <button class="btn sm" disabled={calName?.readOnly} onclick={() => cal.edit(occ)}>Edit</button>
    </div>
  {/if}
</div>

<style>
  .catch { position: fixed; inset: 0; z-index: 40; }
  .pop { position: fixed; z-index: 50; background: var(--surface); border: 1px solid var(--line); border-top: 3px solid var(--c); border-radius: var(--radius); padding: 14px; display: grid; gap: 10px; max-height: calc(100vh - 24px); overflow-y: auto; outline: none; box-shadow: 0 8px 28px rgba(0,0,0,.35); }
  header { display: flex; justify-content: space-between; gap: 8px; align-items: flex-start; }
  h3 { font-size: 16px; letter-spacing: .03em; overflow-wrap: anywhere; line-height: 1.25; }
  dl { display: grid; grid-template-columns: 62px 1fr; gap: 6px 8px; margin: 0; font-size: 14px; }
  dt { color: var(--muted); text-transform: uppercase; font-size: 11px; letter-spacing: .1em; padding-top: 2px; }
  dd { margin: 0; min-width: 0; overflow-wrap: anywhere; } .desc { white-space: pre-wrap; max-height: 120px; overflow: auto; font-size: 13px; }
  .sub { color: var(--muted); } ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 2px; } li { display: flex; gap: 6px; align-items: center; }
  .st { width: 7px; height: 7px; border-radius: 50%; border: 1.5px solid var(--muted); flex: none; } .st.accepted { background: var(--accent); border-color: var(--accent); } .st.declined { border-color: var(--danger); } .st.tentative { border-color: var(--signal); }
  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 2px; margin-right: 6px; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
