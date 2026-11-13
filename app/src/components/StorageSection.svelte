<script lang="ts">
  import { thisDevice } from "../lib/device";
  // Preferences > Privacy > Storage: cache size per account, offline-cache limits, and an explicit "Clear cached bodies".
  import { onMount } from "svelte";
  import { app } from "../lib/state.svelte";
  import { t2, type StorageReport } from "../lib/t2api";
  import { bytesLabel, MAX_DAYS, MAX_MB, parseLimit, usageBytes } from "../lib/t2model";

  let report = $state<StorageReport | null>(null);
  let days = $state<Record<string, string>>({});
  let mb = $state<Record<string, string>>({});
  let err = $state<Record<string, string>>({});
  let busy = $state("");

  async function load() {
    try {
      report = await t2.storageUsage();
      for (const a of report.accounts) {
        days[a.account] = a.policy.keepDays == null ? "" : String(a.policy.keepDays);
        mb[a.account] = a.policy.maxMb == null ? "" : String(a.policy.maxMb);
      }
    } catch (e) { app.fail(e); }
  }
  onMount(load);

  async function save(account: string) {
    const d = parseLimit(days[account] ?? "", MAX_DAYS), m = parseLimit(mb[account] ?? "", MAX_MB);
    if (d === undefined) { err[account] = `Days must be a whole number from 1 to ${MAX_DAYS.toLocaleString("en-US")}, or empty for no limit.`; return; }
    if (m === undefined) { err[account] = `Megabytes must be a whole number from 1 to ${MAX_MB.toLocaleString("en-US")}, or empty for no limit.`; return; }
    err[account] = ""; busy = account;
    try { await t2.storageSetPolicy(account, { keepDays: d, maxMb: m }); app.notify("Saved. New downloads respect these limits; nothing was deleted."); await load(); }
    catch (e) { app.fail(e); }
    finally { busy = ""; }
  }

  async function clear(account: string, size: string) {
    const ok = await app.ask({
      title: `Clear cached bodies for ${account}?`,
      body: `This removes about ${size} of downloaded message text and attachments from ${thisDevice()}. Your messages stay on the mail server and MiraMail downloads them again when you open them. Search in message text only finds messages you open again.`,
      ok: "Clear cached bodies", danger: true,
    });
    if (!ok) return;
    busy = account;
    try { const n = await t2.storageClearBodies(account); app.notify(`Cleared the cached text of ${n.toLocaleString("en-US")} message${n === 1 ? "" : "s"}.`); await load(); }
    catch (e) { app.fail(e); }
    finally { busy = ""; }
  }
</script>

<section class="st" aria-labelledby="stor-h">
  <h4 id="stor-h">Storage</h4>
  <p class="note">MiraMail keeps message lists, and the messages you have opened, in a cache on {thisDevice()} so it works offline. {#if report}The cache database is {bytesLabel(report.dbBytes)} in total.{/if} Limits only stop new downloads from being kept; MiraMail never deletes cached mail by itself.</p>
  {#if report}
    {#each report.accounts as a (a.account)}
      {@const size = bytesLabel(usageBytes(a.usage))}
      <div class="acct">
        <div class="head"><span class="mono ell">{a.account}</span><span class="mono dim">{size}</span></div>
        <div class="dim">{a.usage.messages.toLocaleString("en-US")} messages listed, {a.usage.withBody.toLocaleString("en-US")} downloaded ({bytesLabel(a.usage.bodyBytes)} of mail plus {bytesLabel(a.usage.textBytes)} of search text).</div>
        <div class="limits">
          <label>Keep bodies for <input inputmode="numeric" size="5" placeholder="no limit" bind:value={days[a.account]} oninput={() => (err[a.account] = "")} aria-label="Keep message bodies for how many days" /> days</label>
          <label>Use at most <input inputmode="numeric" size="6" placeholder="no limit" bind:value={mb[a.account]} oninput={() => (err[a.account] = "")} aria-label="Maximum megabytes of cached bodies" /> MB</label>
          <button class="btn sm" disabled={busy === a.account} onclick={() => save(a.account)}>Save limits</button>
        </div>
        {#if err[a.account]}<div class="err" role="alert">{err[a.account]}</div>{/if}
        <div><button class="btn ghost sm" disabled={busy === a.account || a.usage.withBody === 0} onclick={() => clear(a.account, size)}>Clear cached bodies for {a.account}…</button></div>
      </div>
    {:else}
      <p class="note">No accounts yet.</p>
    {/each}
    <p class="note">“Keep bodies for N days” means messages older than that are not stored when you open them (you can still read them; they are fetched each time). Leave a box empty for no limit. Clearing frees space inside the database; the file itself may not shrink right away.</p>
  {:else}
    <p class="note">Reading sizes…</p>
  {/if}
</section>

<style>
  .st { display: grid; gap: 10px; }
  h4 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); text-transform: uppercase; }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .acct { border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; display: grid; gap: 8px; }
  .head { display: flex; justify-content: space-between; gap: 12px; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .dim { color: var(--muted); font-size: 0.85rem; }
  .limits { display: flex; gap: 14px; flex-wrap: wrap; align-items: center; font-size: 0.9rem; }
  .limits input { background: var(--surface); color: var(--text); border: 1px solid var(--line); border-radius: var(--radius); padding: 3px 6px; font: inherit; }
  .err { color: var(--danger); font-size: 0.85rem; }
</style>
