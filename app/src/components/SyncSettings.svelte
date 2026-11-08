<script lang="ts">
  import { onMount } from "svelte";
  import { api, errMsg } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import type { SyncSettings } from "../lib/types";
  import Select from "./Select.svelte";

  const DEFAULTS: SyncSettings = { foldersPerCycle: 2, intervalSeconds: 60, maxLiveFolders: 5, initialSyncMessages: 200 };
  const INITIAL_CHOICES = [100, 200, 500, 1000, 2000, 5000];
  let perCycle = $state(DEFAULTS.foldersPerCycle);
  let interval = $state(DEFAULTS.intervalSeconds);
  let maxLive = $state(DEFAULTS.maxLiveFolders);
  let initial = $state(DEFAULTS.initialSyncMessages);
  /** The presets, plus the stored value when it was set outside them (e.g. hand-edited file). */
  const initialOptions = $derived(
    (INITIAL_CHOICES.includes(initial) ? INITIAL_CHOICES : [...INITIAL_CHOICES, initial].sort((a, b) => a - b)).map((n) => ({
      value: n, label: `${n.toLocaleString("en-US")} messages`, hint: n === DEFAULTS.initialSyncMessages ? "default" : undefined,
    })),
  );
  let busy = $state(false);
  let loaded = $state(false);

  onMount(async () => {
    try { const s = await api.getSyncSettings(); perCycle = s.foldersPerCycle; interval = s.intervalSeconds; maxLive = s.maxLiveFolders; initial = s.initialSyncMessages ?? DEFAULTS.initialSyncMessages; }
    catch (e) { app.fail(e); }
    loaded = true;
    if (app.account) void app.loadLive(app.account);
  });

  /** Selectable folders other than INBOX that are not live: these are the ones the round-robin visits. */
  const polled = $derived(app.folders.filter((f) => f.selectable && f.role !== "inbox" && !app.isLive(f.name)).length);
  const human = (s: number) => (s < 90 ? `${Math.round(s)} seconds` : s < 5400 ? `${Math.round(s / 60)} minutes` : `${(s / 3600).toFixed(1)} hours`);
  const explain = $derived.by(() => {
    const p = Math.max(1, Math.floor(perCycle) || 1), i = Math.max(1, Math.floor(interval) || 1);
    if (polled === 0) return "There are no other folders to check. INBOX is always live.";
    const cycles = Math.ceil(polled / p);
    return `With ${polled} folder${polled === 1 ? "" : "s"}, every folder is checked about every ${human(cycles * i)}. INBOX is always live.`;
  });

  function check(): string | null {
    if (!Number.isInteger(perCycle) || perCycle < 1 || perCycle > 20) return "Folders per cycle must be a whole number from 1 to 20.";
    if (!Number.isInteger(interval) || interval < 15 || interval > 3600) return "Cycle interval must be from 15 to 3600 seconds.";
    if (!Number.isInteger(maxLive) || maxLive < 1 || maxLive > 10) return "Live folders must be a whole number from 1 to 10.";
    if (!Number.isInteger(initial) || initial < 50 || initial > 5000) return "Initial messages per folder must be from 50 to 5000.";
    return null;
  }
  async function save() {
    const bad = check();
    if (bad) { app.notify(bad, "error"); return; }
    busy = true;
    try {
      const s = await api.setSyncSettings({ foldersPerCycle: perCycle, intervalSeconds: interval, maxLiveFolders: maxLive, initialSyncMessages: initial });
      perCycle = s.foldersPerCycle; interval = s.intervalSeconds; maxLive = s.maxLiveFolders; initial = s.initialSyncMessages;
      app.notify("Sync settings saved");
    } catch (e) { app.notify(errMsg(e), "error"); }
    busy = false;
  }
  function reset() { perCycle = DEFAULTS.foldersPerCycle; interval = DEFAULTS.intervalSeconds; maxLive = DEFAULTS.maxLiveFolders; initial = DEFAULTS.initialSyncMessages; }

  const liveList = $derived(app.account ? (app.liveFolders[app.account] ?? []) : []);
  const stateText = (f: string) => {
    const s = app.liveStateOf(f);
    return s ? (s.state === "polling" ? "polling" : s.state) : "off";
  };
</script>

<section aria-labelledby="hist-h">
  <h3 id="hist-h">Message history</h3>
  <p class="note">How many of the newest messages are downloaded the first time a folder is synced. More takes longer and uses more disk space. A change applies to folders synced from now on; folders already on this Mac keep what they have, and you can always load older mail with the button at the bottom of a message list.</p>
  <div class="field"><span class="label" id="ss-init-l">Initial messages per folder</span>
    <Select value={initial} options={initialOptions} onchange={(v) => { initial = v; }} labelledby="ss-init-l" disabled={!loaded} /></div>

  <h3 id="bgsync-h">Background sync</h3>
  <p class="note">INBOX is always live. Other folders are checked in turn, a few at a time, over one short connection.</p>
  <div class="field"><label class="label" for="ss-n">Folders checked per cycle</label>
    <input id="ss-n" type="number" min="1" max="20" step="1" bind:value={perCycle} disabled={!loaded} /></div>
  <div class="field"><label class="label" for="ss-i">Cycle interval (seconds)</label>
    <input id="ss-i" type="number" min="15" max="3600" step="1" bind:value={interval} disabled={!loaded} /></div>
  <p class="explain" role="status">{explain}</p>

  <h3 id="live-h">Live folders</h3>
  <p class="note">Each live folder keeps its own connection to your mail server. Providers limit how many connections you can have open at once, so keep this number small.</p>
  <div class="field"><label class="label" for="ss-l">Most live folders per account (1-10)</label>
    <input id="ss-l" type="number" min="1" max="10" step="1" bind:value={maxLive} disabled={!loaded} /></div>
  {#if liveList.length}
    <ul class="live" aria-label="Live folders">
      {#each liveList as f (f)}
        {@const st = app.liveStateOf(f)}
        <li>
          <span class="ld {st?.state ?? 'off'}" aria-hidden="true"></span>
          <span class="ell">{f}</span>
          <span class="mono st">{stateText(f)}</span>
          {#if st?.state === "error" || st?.state === "polling"}<span class="msg selectable">{st.message}</span>{/if}
        </li>
      {/each}
    </ul>
  {:else}<p class="note">No extra live folders. Hover a folder in the sidebar and press the live icon to add one.</p>{/if}

  <div class="acts">
    <button class="btn ghost" onclick={reset} disabled={busy || !loaded}>Reset to defaults</button>
    <span class="grow"></span>
    <button class="btn" onclick={save} disabled={busy || !loaded}>{busy ? "Saving…" : "Save"}</button>
  </div>
</section>

<style>
  section { display: grid; gap: 12px; }
  h3 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; text-transform: uppercase; color: var(--muted); }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); }
  .explain { margin: 0; font-size: 0.8667rem; border: 1px solid var(--line); border-left: 2px solid var(--accent); border-radius: var(--radius); padding: 8px 12px; }
  .live { margin: 0; padding: 0; list-style: none; display: grid; gap: 6px; }
  .live li { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; font-size: 0.8667rem; }
  .ld { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); align-self: center; flex: none; }
  .ld.polling { background: var(--muted); } .ld.connecting, .ld.off { background: transparent; border: 1.5px solid var(--muted); }
  .ld.error { background: transparent; border: 1.5px solid var(--danger); }
  .st { color: var(--muted); } .msg { flex-basis: 100%; color: var(--danger); font-size: 0.8rem; overflow-wrap: anywhere; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .acts { display: flex; gap: 8px; align-items: center; } .grow { flex: 1; }
</style>
