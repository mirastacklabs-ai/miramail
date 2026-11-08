<script lang="ts">
  import Select from "./Select.svelte";
  import DateTimeSelect from "./DateTimeSelect.svelte";
  import { onMount, untrack } from "svelte";
  import { api } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { cal } from "../lib/cal.svelte";
  import { addDays, dayKey, localTz, MIN, normRrule, parseDayKey, presetFromRrule, presetRrule, rfcToWall, timezones, toRfc, validateRrule, wallToInstant, wallToRfc, type RepeatPreset } from "../lib/caldate";
  import type { Attendee, CalEvent, ZoomMeeting } from "../lib/types";
  import { workspace, type EventPayload } from "../lib/workspace.svelte";
  import DeleteBar from "./DeleteBar.svelte";

  let { id: pid }: { id: string } = $props();
  const myId = untrack(() => pid);
  const pl = workspace.getPayload<EventPayload>(myId)!;
  const ini = pl.init;
  const init = { uid: ini.uid, calendarId: ini.calendarId, start: ini.start != null ? new Date(ini.start) : undefined, end: ini.end != null ? new Date(ini.end) : undefined, allDay: ini.allDay };
  let base = pl.base ?? "";
  const pad = (n: number) => String(n).padStart(2, "0");
  const dStr = (d: Date) => dayKey(d);
  const tStr = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;

  let loading = $state(!!init.uid);
  let orig = $state<CalEvent | null>(null);
  let summary = $state("");
  let allDay = $state(init.allDay ?? false);
  const s0 = init.start ?? new Date(), e0 = init.end ?? new Date(s0.getTime() + 3600000);
  let startDate = $state(dStr(s0)); let startTime = $state(tStr(s0));
  let endDate = $state(dStr(e0)); let endTime = $state(tStr(e0));
  let tz = $state(localTz());
  let repeat = $state<RepeatPreset>("none");
  let customRule = $state("");
  let location = $state("");
  let description = $state("");
  let attendees = $state<Attendee[]>([]);
  let attInput = $state("");
  let attErr = $state("");
  let calendarId = $state(init.calendarId ?? cal.defaultCalendar?.id ?? "");
  let reminder = $state<number | null>(10);
  let zoom = $state<ZoomMeeting | null>(null);
  let zoomBlock = $state("");
  let zoomBusy = $state(false);
  let saving = $state(false);
  let err = $state("");
  let confirmDel = $state(false);
  let created: string[] = [...pl.created];   // Zoom meetings created in this session (kept in the payload so discard can clean up)
  let removed: string[] = [...pl.removed];   // saved meetings the user detached; deleted after a successful save
  let dur = Math.max(15, Math.round((e0.getTime() - s0.getTime()) / MIN));

  const zones = $derived.by(() => { const z = timezones(); return z.includes(tz) ? z : [tz, ...z]; });
  const isNew = !init.uid;
  const writable = $derived(cal.calendars.filter((c) => !c.readOnly || c.id === calendarId));
  const REMINDERS: [number | null, string][] = [[null, "None"], [0, "At time of event"], [5, "5 minutes before"], [10, "10 minutes before"], [15, "15 minutes before"], [30, "30 minutes before"], [60, "1 hour before"], [1440, "1 day before"]];

  const formNow = () => ({ summary, allDay, startDate, startTime, endDate, endTime, tz, repeat, customRule, location, description, attendees, calendarId, reminder, zoom, zoomBlock, dur });
  function applyForm(f: Record<string, any>) {
    ({ summary, allDay, startDate, startTime, endDate, endTime, tz, repeat, customRule, location, description, attendees, calendarId, reminder, zoom, zoomBlock, dur } = { ...formNow(), ...f });
  }
  // Mirror this instance into the workspace (draft autosave is debounced there). Dirty = differs from the loaded state.
  $effect(() => {
    if (loading) return;
    const f = formNow();
    const snap = JSON.stringify(f);
    untrack(() => {
      if (!base) base = snap;
      const payload: EventPayload = { init: ini, form: JSON.parse(snap), created: [...created], removed: [...removed], base };
      workspace.setPayload(myId, payload, { title: f.summary.trim() || (isNew ? "New event" : "Edit event"), dirty: snap !== base || created.length > 0 });
    });
  });

  /** The saved (unsent) form, read fresh so a Retry never overwrites edits made while the load had failed. */
  const savedForm = () => workspace.getPayload<EventPayload>(myId)?.form ?? null;
  let loadErr = $state("");
  const errText = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : "Could not load this event.");
  onMount(() => { void load(); });
  async function load() {
    const f0 = savedForm();
    if (!init.uid) { if (f0) applyForm(f0); return; }
    loadErr = "";
    try {
      const e = await api.calGetEvent(init.uid, init.calendarId!);
      orig = e; summary = e.summary; allDay = e.allDay; location = e.location; description = e.description; attendees = e.attendees;
      calendarId = e.calendarId; reminder = e.reminderMinutes; zoom = e.zoom;
      if (e.allDay) {
        startDate = e.start.slice(0, 10); endDate = dayKey(addDays(parseDayKey(e.end.slice(0, 10)), -1));
        if (endDate < startDate) endDate = startDate;
        dur = 60;
      } else {
        tz = e.tzid ?? localTz();
        ({ date: startDate, time: startTime } = rfcToWall(e.start, tz)); ({ date: endDate, time: endTime } = rfcToWall(e.end, tz));
        dur = Math.max(15, Math.round((new Date(e.end).getTime() - new Date(e.start).getTime()) / MIN));
      }
      repeat = presetFromRrule(e.rrule, parseDayKey(startDate));
      if (repeat === "custom") customRule = e.rrule ?? "";
      if (!base) base = JSON.stringify(formNow());
      const f = savedForm();
      if (f) applyForm(f);
    } catch (e) {
      // Never close here: closing deletes the draft and any Zoom meeting created in this session.
      loadErr = errText(e);
      const f = savedForm();
      if (f) { applyForm(f); loading = false; } // the unsent edits stay visible; saving is blocked until the event loads
      return;
    }
    loading = false;
  }
  function retry() { loading = true; void load(); }

  const startInstant = () => { const [y, m, d] = startDate.split("-").map(Number); const [h, mi] = (startTime || "00:00").split(":").map(Number); return wallToInstant(y, m, d, h, mi, tz); };
  const endInstant = () => { const [y, m, d] = endDate.split("-").map(Number); const [h, mi] = (endTime || "00:00").split(":").map(Number); return wallToInstant(y, m, d, h, mi, tz); };
  function startChanged() {
    if (!startDate) return;
    if (allDay) { if (endDate < startDate) endDate = startDate; return; }
    ({ date: endDate, time: endTime } = rfcToWall(toRfc(startInstant() + dur * MIN, tz), tz));
  }
  function endChanged() {
    if (allDay) return;
    const d = Math.round((endInstant() - startInstant()) / MIN);
    if (d > 0) dur = d;
  }

  function commitAtt(): boolean {
    const raw = attInput.trim();
    if (!raw) { attErr = ""; return true; }
    let ok = true;
    for (const part of raw.split(/[,;\s]+/).filter(Boolean)) {
      const m = part.match(/<([^>]+)>/); const email = (m ? m[1] : part).toLowerCase();
      if (!/^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/.test(email)) { attErr = `“${part}” is not a valid email address.`; ok = false; continue; }
      if (!attendees.some((a) => a.email.toLowerCase() === email)) attendees = [...attendees, { email, name: null, role: "required", status: "needs-action" }];
    }
    if (ok) { attInput = ""; attErr = ""; } else attInput = raw;
    return ok;
  }
  function attKey(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === ",") { e.preventDefault(); e.stopPropagation(); commitAtt(); }
    else if (e.key === "Backspace" && !attInput && attendees.length) attendees = attendees.slice(0, -1);
  }

  const ruleErr = $derived(repeat === "custom" ? validateRrule(customRule) : null);

  function timesOk(): string | null {
    if (!startDate || !endDate) return "Pick a start and end date.";
    if (allDay) return endDate < startDate ? "The end date is before the start date." : null;
    if (!startTime || !endTime) return "Pick a start and end time.";
    return endInstant() > startInstant() ? null : "The end must be after the start.";
  }

  async function addZoom() {
    if (!cal.zoom?.connected) { app.notify("Connect Zoom first (Zoom settings).", "info"); cal.zoomOpen = true; return; }
    const bad = timesOk(); if (bad || allDay) { err = allDay ? "Zoom meetings need a start time. Turn off All day first." : bad!; return; }
    zoomBusy = true; err = "";
    try {
      const m = await api.zoomCreateMeeting(summary.trim() || "Meeting", wallToRfc(startDate, startTime, tz), Math.max(1, Math.round((endInstant() - startInstant()) / MIN)), tz, description.trim() || undefined);
      created.push(m.meetingId);
      zoom = m; location = m.joinUrl;
      zoomBlock = `Join Zoom Meeting\n${m.joinUrl}\nMeeting ID: ${m.meetingId}${m.password ? `\nPasscode: ${m.password}` : ""}`;
      description = description.trim() ? `${description.trimEnd()}\n\n${zoomBlock}` : zoomBlock;
    } catch (e) { err = typeof e === "string" ? e : "Could not create the Zoom meeting."; app.fail(e); }
    zoomBusy = false;
  }
  function removeZoom() {
    if (!zoom) return;
    const id = zoom.meetingId;
    if (created.includes(id)) { created = created.filter((x) => x !== id); void api.zoomDeleteMeeting(id).catch(() => {}); } else removed.push(id);
    if (location === zoom.joinUrl) location = "";
    const block = zoomBlock || `Join Zoom Meeting\n${zoom.joinUrl}`;
    if (zoomBlock && description.includes(block)) description = description.replace(`\n\n${block}`, "").replace(block, "").trim();
    else description = description.split("\n").filter((l) => !l.includes(zoom!.joinUrl)).join("\n").trim();
    zoom = null; zoomBlock = "";
  }

  function build(): CalEvent | null {
    err = "";
    if (!commitAtt()) return null;
    const bad = timesOk(); if (bad) { err = bad; return null; }
    if (ruleErr) { err = ruleErr; return null; }
    if (!calendarId) { err = "Choose a calendar."; return null; }
    const startDay = parseDayKey(startDate);
    const rrule = repeat === "custom" ? normRrule(customRule) : presetRrule(repeat, startDay);
    const base: CalEvent = orig ? { ...orig } : {
      uid: `${crypto.randomUUID()}@miramail`, calendarId, summary: "", description: "", location: "", start: "", end: "", allDay: false, tzid: null, rrule: null,
      exdates: [], attendees: [], organizer: null, status: "confirmed", reminderMinutes: null, zoom: null, sequence: 0, href: null, etag: null,
    };
    return {
      ...base, calendarId, summary: summary.trim(), description, location: location.trim(), allDay,
      start: allDay ? startDate : wallToRfc(startDate, startTime, tz),
      end: allDay ? dayKey(addDays(parseDayKey(endDate), 1)) : wallToRfc(endDate, endTime, tz),
      tzid: allDay ? null : tz, rrule: rrule ?? null, exdates: rrule ? base.exdates : [], attendees, reminderMinutes: reminder, zoom,
    };
  }
  async function save(send: boolean) {
    if (saving || loading || loadErr) return;
    const ev = build(); if (!ev) return;
    if (send && !ev.attendees.length) return;
    saving = true;
    const ok = await cal.save(ev, send);
    saving = false;
    if (ok) {
      created = [];
      for (const id of removed) void api.zoomDeleteMeeting(id).catch(() => {});
      workspace.finish(myId);
    }
  }
  const close = () => { void workspace.requestClose(myId); };
  async function del(notify: boolean) { if (!loadErr && orig && (await cal.deleteEvent(orig.uid, orig.calendarId, notify))) workspace.close(myId); }
  function key(e: KeyboardEvent) {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); e.stopPropagation(); void save(false); }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="ep" role="form" aria-label={isNew ? "New event" : "Edit event"} onkeydown={key}>
  {#if loading && loadErr}
    <div class="lerr" role="alert"><p class="err">Could not load this event: {loadErr}</p>
      <div class="acts"><button class="btn ghost" onclick={close}>Close</button><button class="btn" onclick={retry}>Retry</button></div></div>
  {:else if loading}
    <div class="skeleton" style="height:36px"></div><div class="skeleton" style="height:36px;width:70%"></div><div class="skeleton" style="height:90px"></div>
  {:else}
    {#if loadErr}
      <div class="lerr" role="alert"><p class="err">Could not load the saved event: {loadErr} Your unsent changes are shown below, but they cannot be saved until it loads.</p>
        <div class="acts"><button class="btn ghost" onclick={close}>Close</button><button class="btn" onclick={retry}>Retry</button></div></div>
    {/if}
    <div class="field"><label class="label" for="ev-title-{myId}">Title</label><input id="ev-title-{myId}" bind:value={summary} placeholder="Add a title" autocomplete="off" /></div>

    <label class="chk"><input type="checkbox" bind:checked={allDay} onchange={() => { if (allDay && endDate < startDate) endDate = startDate; }} /> All day</label>

    <div class="grid2">
      <div class="field"><label class="label" for="ev-sd-{myId}">Start</label>
        <div class="pair"><DateTimeSelect mode="date" idPrefix="ev-sd-{myId}" ariaPrefix="Start" from={-60} days={900} value={startDate} onchange={(v) => { startDate = v; startChanged(); }} />{#if !allDay}<DateTimeSelect mode="time" idPrefix="ev-st-{myId}" ariaPrefix="Start" value={startTime} onchange={(v) => { startTime = v; startChanged(); }} />{/if}</div></div>
      <div class="field"><label class="label" for="ev-ed-{myId}">End</label>
        <div class="pair"><DateTimeSelect mode="date" idPrefix="ev-ed-{myId}" ariaPrefix="End" from={-60} days={900} min={startDate} value={endDate} onchange={(v) => { endDate = v; endChanged(); }} />{#if !allDay}<DateTimeSelect mode="time" idPrefix="ev-et-{myId}" ariaPrefix="End" value={endTime} onchange={(v) => { endTime = v; endChanged(); }} />{/if}</div></div>
    </div>

    <div class="grid2">
      {#if !allDay}<div class="field"><label class="label" for="ev-tz-{myId}">Time zone</label>
        <Select id="ev-tz-{myId}" value={tz} options={zones.map((z) => ({ value: z, label: z }))} onchange={(v) => (tz = v)} ariaLabel="Time zone" /></div>{/if}
      <div class="field"><label class="label" for="ev-rep-{myId}">Repeat</label>
        <Select id="ev-rep-{myId}" value={repeat} onchange={(v) => (repeat = v)} ariaLabel="Repeat" options={[
          { value: "none", label: "Does not repeat" }, { value: "daily", label: "Daily" }, { value: "weekdays", label: "Every weekday" },
          { value: "weekly", label: "Weekly" }, { value: "monthly", label: "Monthly" }, { value: "yearly", label: "Yearly" }, { value: "custom", label: "Custom (RRULE)…" },
        ]} /></div>
    </div>
    {#if repeat === "custom"}
      <div class="field"><label class="label" for="ev-rr-{myId}">RRULE</label>
        <input id="ev-rr-{myId}" bind:value={customRule} placeholder="FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10" spellcheck="false" aria-invalid={!!ruleErr} aria-describedby="rr-h-{myId}" />
        <span id="rr-h-{myId}" class="hint" class:bad={customRule && ruleErr}>{customRule && ruleErr ? ruleErr : "FREQ, INTERVAL, COUNT, UNTIL, BYDAY, BYMONTHDAY, BYMONTH, BYSETPOS"}</span></div>
    {/if}
    {#if !isNew && orig?.rrule}<p class="note">Changes apply to every occurrence of this repeating event.</p>{/if}

    <div class="field"><label class="label" for="ev-loc-{myId}">Location</label><input id="ev-loc-{myId}" bind:value={location} placeholder="Add a place or link" autocomplete="off" /></div>

    <div class="zoomrow">
      {#if zoom}
        <span class="zbadge mono">ZOOM</span><span class="mono ell" title={zoom.joinUrl}>{zoom.meetingId}</span>
        <span class="grow"></span>
        <button class="btn ghost sm" onclick={removeZoom}>Remove Zoom meeting</button>
      {:else}
        <button class="btn ghost sm" disabled={zoomBusy} onclick={addZoom}>{zoomBusy ? "Creating meeting…" : "Add Zoom meeting"}</button>
        <span class="hint">{cal.zoom?.connected ? "Creates a meeting on your Zoom account." : "Zoom is not connected yet; this opens Zoom settings."}</span>
      {/if}
    </div>

    <div class="field"><label class="label" for="ev-desc-{myId}">Description</label><textarea id="ev-desc-{myId}" rows="4" bind:value={description}></textarea></div>

    <div class="field"><label class="label" for="ev-att-{myId}">Attendees</label>
      <div class="chips">
        {#each attendees as a (a.email)}
          <span class="chip mono">{a.name || a.email}<button class="x" aria-label="Remove {a.email}" onclick={() => (attendees = attendees.filter((x) => x.email !== a.email))}>×</button></span>
        {/each}
        <input id="ev-att-{myId}" class="ai" bind:value={attInput} onkeydown={attKey} onblur={commitAtt} placeholder={attendees.length ? "" : "name@example.com, press Enter"} aria-invalid={!!attErr} aria-describedby="att-e-{myId}" autocomplete="off" />
      </div>
      <span id="att-e-{myId}" class="hint bad" role={attErr ? "alert" : undefined}>{attErr}</span></div>

    <div class="grid2">
      <div class="field"><label class="label" for="ev-cal-{myId}">Calendar</label>
        <Select id="ev-cal-{myId}" value={calendarId} options={writable.map((c) => ({ value: c.id, label: c.name }))} onchange={(v) => (calendarId = v)} ariaLabel="Calendar" /></div>
      <div class="field"><label class="label" for="ev-rem-{myId}">Reminder</label>
        <Select id="ev-rem-{myId}" value={reminder === null ? "none" : String(reminder)} options={REMINDERS.map(([v, l]) => ({ value: v === null ? "none" : String(v), label: l }))} onchange={(v) => (reminder = v === "none" ? null : Number(v))} ariaLabel="Reminder" /></div>
    </div>

    {#if err}<p class="err" role="alert">{err}</p>{/if}
    {#if confirmDel}
      <DeleteBar hasAttendees={attendees.length > 0} onconfirm={del} oncancel={() => (confirmDel = false)} />
    {:else}
      <div class="acts">
        {#if !isNew}<button class="btn danger" onclick={() => (confirmDel = true)}>Delete</button>{/if}
        <span class="grow"></span>
        <button class="btn ghost" onclick={close}>Cancel</button>
        <button class="btn ghost" disabled={saving || !!loadErr || attendees.length === 0} title={attendees.length ? "" : "Add attendees to send invitations"} onclick={() => save(true)}>Save &amp; send invites</button>
        <button class="btn" disabled={saving || !!loadErr} onclick={() => save(false)}>{saving ? "Saving…" : "Save"}</button>
      </div>
      <p class="kbd mono">⌘/Ctrl+Enter saves</p>
    {/if}
  {/if}
</div>

<style>
  .ep { flex: 1; min-height: 0; overflow-y: auto; padding: 16px; display: grid; gap: 14px; align-content: start; background: var(--surface); }
  .grid2 { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 12px; }
  .pair { display: flex; gap: 6px; align-items: flex-start; } .pair > :global(.dts:first-child) { flex: 1; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 14px; }
  .hint { font-size: 12px; color: var(--muted); min-height: 1em; } .hint.bad { color: var(--danger); }
  .note { margin: 0; font-size: 12px; color: var(--muted); }
  .lerr { display: grid; gap: 8px; }
  .lerr .acts { display: flex; justify-content: flex-end; gap: 8px; }
  .err { margin: 0; color: var(--danger); font-size: 13px; }
  .zoomrow { display: flex; align-items: center; gap: 10px; border: 1px dashed var(--line); border-radius: var(--radius); padding: 8px 10px; flex-wrap: wrap; }
  .zbadge { color: var(--accent); border: 1px solid var(--accent); border-radius: 2px; padding: 0 5px; font-weight: 600; font-size: 11px; }
  .grow { flex: 1; }
  .chips { display: flex; flex-wrap: wrap; gap: 6px; border: 1px solid var(--line); border-radius: var(--radius); padding: 6px; }
  .chips:focus-within { outline: 2px solid var(--accent); border-color: transparent; }
  .chip { display: inline-flex; align-items: center; gap: 4px; background: var(--line); border-radius: 2px; padding: 2px 4px 2px 8px; color: var(--ink); }
  .x { background: none; border: 0; color: var(--muted); padding: 0 4px; font-size: 14px; line-height: 1; } .x:hover { color: var(--danger); }
  .field .ai { flex: 1; min-width: 160px; border: 0; padding: 4px; outline: none !important; }
  .acts { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .kbd { color: var(--muted); font-size: 10.5px; margin: 0; text-align: right; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; max-width: 220px; }
  .field textarea { resize: vertical; font-family: var(--font-mono); }
</style>
