<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { app } from "../../lib/state.svelte";
  import { phone } from "../../lib/phone.svelte";
  import { workspace } from "../../lib/workspace.svelte";
  import { cal } from "../../lib/cal.svelte";
  import { leaf } from "../../lib/wstree";
  import { showTabBar } from "../../lib/phonemodel";
  import { edgeSwipe } from "../../lib/phonegestures";
  import Pane from "../Pane.svelte";
  import AiPane from "../ai/AiPane.svelte";
  import CalendarView from "../CalendarView.svelte";
  import CalPanel from "../CalPanel.svelte";
  import BottomSheet from "./BottomSheet.svelte";
  import PhoneTabBar from "./PhoneTabBar.svelte";
  import PhoneBoxes from "./PhoneBoxes.svelte";
  import PhoneList from "./PhoneList.svelte";
  import PhoneReader from "./PhoneReader.svelte";
  import PhoneSettings from "./PhoneSettings.svelte";

  const nav = $derived(phone.nav);
  const sheetId = $derived(phone.sheetPaneId);
  let calsOpen = $state(false);
  let seenMira = $state(false), seenCal = $state(false);
  $effect(() => { if (nav.tab === "mira") seenMira = true; });
  $effect(() => { if (nav.tab === "calendar" && !seenCal) { seenCal = true; cal.setMode("agenda"); } });
  $effect(() => { if (app.folders.length) phone.restoreFolder(); });
  $effect(() => { const a = app.account; untrack(() => phone.accountChanged(a)); });
  onMount(() => edgeSwipe(() => { if (!sheetId && phone.canBack) phone.back(); }));
</script>

<div class="proot">
  <main class="stage">
    <div class="pg" hidden={nav.tab !== "mail"}>
      {#if nav.mail === "boxes"}<PhoneBoxes />{:else if nav.mail === "list"}<PhoneList />{:else}<PhoneReader />{/if}
    </div>
    {#if seenMira}
      <div class="pg" hidden={nav.tab !== "mira"}>
        <header class="bar"><h1>MIRA</h1></header>
        <AiPane />
      </div>
    {/if}
    {#if seenCal}
      <div class="pg" hidden={nav.tab !== "calendar"}>
        <header class="bar"><h1>Calendar</h1>
          <span class="tools"><button class="plus cals" aria-label="Calendars" aria-haspopup="dialog" onclick={() => (calsOpen = true)}><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M4 12h16M4 17h10"/></svg></button><button class="plus" aria-label="New event" onclick={() => cal.newEvent()}>+</button></span></header>
        <CalendarView />
      </div>
    {/if}
    <div class="pg" hidden={nav.tab !== "settings"}><PhoneSettings /></div>
  </main>
  {#if showTabBar(nav) && !phone.kbOpen}<PhoneTabBar />{/if}

  {#if calsOpen && nav.tab === "calendar" && !sheetId}
    <BottomSheet title="Calendars" tall onclose={() => (calsOpen = false)}>
      <CalPanel sheet onpick={() => (calsOpen = false)} />
      <button class="btn addcal" onclick={() => { calsOpen = false; cal.addOpen = true; }}>+ Add calendar</button>
    </BottomSheet>
  {/if}

  {#if sheetId}
    <div class="sheet" style="padding-bottom:{phone.kbOpen ? 0 : 'env(safe-area-inset-bottom)'}" role="dialog" aria-modal="true" aria-label={workspace.ws.panes[sheetId]?.title ?? "Message"}>
      {#key sheetId}<Pane leaf={leaf(sheetId)} bare />{/key}
    </div>
  {/if}
</div>

<style>
  .proot { position: fixed; left: 0; right: 0; top: var(--vv-top, 0px); height: var(--vv-h, 100%); display: flex; flex-direction: column; background: var(--bg); padding-top: env(safe-area-inset-top); padding-left: env(safe-area-inset-left); padding-right: env(safe-area-inset-right); overflow: hidden; }
  .stage { flex: 1; min-height: 0; display: flex; flex-direction: column; position: relative; }
  .pg { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .pg[hidden] { display: none; }
  .bar { flex: none; display: flex; align-items: center; justify-content: space-between; padding: 6px 12px; min-height: 52px; border-bottom: 1px solid var(--line); background: var(--bg-band); }
  h1 { font-size: 1.05rem; letter-spacing: .08em; }
  .plus { width: 44px; height: 44px; background: none; border: 0; color: var(--accent); font-size: 1.8rem; line-height: 1; }
  .tools { display: flex; align-items: center; }
  .cals svg { width: 24px; height: 24px; fill: none; stroke: currentColor; stroke-width: 1.8; stroke-linecap: round; }
  .addcal { width: 100%; margin-top: 12px; min-height: 44px; }
  .sheet { position: absolute; inset: 0; z-index: 40; background: var(--surface); display: flex; flex-direction: column; padding-top: env(safe-area-inset-top); padding-bottom: env(safe-area-inset-bottom); padding-left: env(safe-area-inset-left); padding-right: env(safe-area-inset-right); }
</style>
