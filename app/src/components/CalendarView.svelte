<script lang="ts">
  import { onMount } from "svelte";
  import { cal } from "../lib/cal.svelte";
  import { daysIn, type CalMode } from "../lib/caldate";
  import TimeGrid from "./TimeGrid.svelte";
  import MonthGrid from "./MonthGrid.svelte";
  import Agenda from "./Agenda.svelte";
  import EventPopover from "./EventPopover.svelte";
  import { api, errMsg } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { phone } from "../lib/phone.svelte";
  import type { AddCalError, CalPreset, ReusableAccount } from "../lib/types";

  onMount(() => { void cal.start(); void cal.load(); });
  const ALL_MODES: { m: CalMode; label: string; k: string }[] = [{ m: "day", label: "Day", k: "1" }, { m: "week", label: "Week", k: "2" }, { m: "month", label: "Month", k: "3" }, { m: "agenda", label: "Agenda", k: "4" }];
  const MODES = phone.on ? ALL_MODES.filter((x) => x.m !== "week") : ALL_MODES; // day / month / agenda fit a phone
  const days = $derived(daysIn(cal.range));
  const noRemote = $derived(cal.loaded && !cal.calendars.some((c) => c.kind === "caldav"));
  // One-click connect when a saved mail account is at a known calendar provider.
  let reuse = $state<{ preset: CalPreset; region: string | null; acc: ReusableAccount } | null>(null);
  let connecting = $state(false);
  let cerr = $state("");
  $effect(() => {
    if (!noRemote || reuse) return;
    void (async () => {
      try {
        const s = await api.calSuggestCaldav();
        if (!s || s.preset === "custom") return;
        const a = (await api.calReusableAccounts(s.preset))[0];
        if (a) reuse = { preset: s.preset, region: s.region, acc: a };
      } catch { /* the plain button remains */ }
    })();
  });
  async function connectReuse() {
    if (!reuse || connecting) return;
    connecting = true; cerr = "";
    try {
      const r = await api.calAddCaldavReuse(reuse.preset, reuse.acc.email, reuse.preset === "zoho" ? reuse.region ?? undefined : undefined);
      app.notify(`Added ${r.length} calendar${r.length === 1 ? "" : "s"}`);
      await cal.refresh();
    } catch (ex) {
      const x = ex as Partial<AddCalError> | string;
      cerr = x && typeof x === "object" && typeof x.message === "string" ? x.message : typeof x === "string" ? x : errMsg(x) || "Could not add the calendar.";
    }
    connecting = false;
  }
  const unit = $derived(cal.mode === "agenda" ? "30 days" : cal.mode);
</script>

<section class="cv" aria-label="Calendar">
  <div class="tb" role="toolbar" aria-label="Calendar navigation">
    <button class="btn ghost sm" onclick={() => cal.today()} title="Today (t)">Today</button>
    <button class="iconbtn" aria-label="Previous {unit}" title="Previous (←)" onclick={() => cal.step(-1)}><svg viewBox="0 0 24 24"><path d="M15 6l-6 6 6 6"/></svg></button>
    <button class="iconbtn" aria-label="Next {unit}" title="Next (→)" onclick={() => cal.step(1)}><svg viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></button>
    <h2 aria-live="polite">{cal.title}</h2>
    <span class="grow"></span>
    <div class="seg" role="group" aria-label="Calendar view">
      {#each MODES as x (x.m)}
        <button class:on={cal.mode === x.m} aria-pressed={cal.mode === x.m} title="{x.label} ({x.k})" onclick={() => cal.setMode(x.m)}>{x.label}</button>
      {/each}
    </div>
  </div>

  {#if noRemote}
    <div class="empty" role="region" aria-label="Connect your calendar">
      <div class="et">
        <strong>Connect your calendar</strong>
        <span>Zoho, iCloud, Fastmail or any CalDAV server. Until then only the local calendar on this Mac is shown.</span>
        {#if reuse}<span>Your password stays in the macOS Keychain and is only reused on this Mac, for your {reuse.preset} calendar server.</span>{/if}
        {#if cerr}<span class="cerr" role="alert">{cerr}</span>{/if}
      </div>
      {#if reuse}
        <div class="ea">
          <button class="btn" disabled={connecting} onclick={connectReuse}>{connecting ? "Connecting…" : `Use my ${reuse.acc.email} mail login`}</button>
          <button class="btn ghost sm" onclick={() => (cal.addOpen = true)}>Enter a different login</button>
        </div>
      {:else}
        <button class="btn" onclick={() => (cal.addOpen = true)}>Add calendar</button>
      {/if}
    </div>
  {/if}

  {#if cal.mode === "day" || cal.mode === "week"}
    <TimeGrid {days} />
  {:else if cal.mode === "month"}
    <MonthGrid />
  {:else}
    <Agenda />
  {/if}
</section>

{#if cal.popover}
  {#key cal.popover.occ.uid + cal.popover.occ.recurrenceStart}<EventPopover occ={cal.popover.occ} x={cal.popover.x} y={cal.popover.y} />{/key}
{/if}

<style>
  .cv { flex: 1; min-width: 0; display: flex; flex-direction: column; min-height: 0; background: var(--bg); }
  .tb { display: flex; align-items: center; gap: 6px; padding: 10px 20px; border-bottom: 1px solid var(--line); flex: none; }
  h2 { font-size: 17px; letter-spacing: .06em; margin-left: 10px; }
  .grow { flex: 1; }
  .empty { display: flex; align-items: center; gap: 16px; margin: 12px 20px 0; padding: 14px 16px; border: 1px solid var(--line); border-left: 3px solid var(--accent); border-radius: var(--radius); flex: none; }
  .ea { display: grid; gap: 6px; justify-items: end; } .cerr { color: var(--danger) !important; }
  .et { display: grid; gap: 2px; flex: 1; min-width: 0; } .et strong { font-size: 14px; } .et span { font-size: 12px; color: var(--muted); }
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  .seg button { background: none; border: 0; padding: 5px 12px; font-size: 11px; text-transform: uppercase; letter-spacing: .12em; color: var(--muted); border-bottom: 2px solid transparent; }
  .seg button:hover { color: var(--ink); }
  .seg button.on { color: var(--ink); border-bottom-color: var(--accent); }
</style>
