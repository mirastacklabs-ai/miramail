<script lang="ts">
  import { deviceWord, keychainName, thisDevice } from "../lib/device";
  // Preferences > Privacy > Backup: settings to a JSON file (never passwords, tokens or API keys), restore with a preview.
  import { app } from "../lib/state.svelte";
  import { t2, type BackupPreview } from "../lib/t2api";
  import { applicable } from "../lib/t2model";
  import { apply, exportBackup } from "../lib/t2backup";
  import { describeIdentities } from "../lib/t2restore";
  import { errMsg } from "../lib/api";

  let busy = $state(false);
  let preview = $state<BackupPreview | null>(null);
  let ticked = $state<string[]>([]);
  let result = $state<string[]>([]);
  let replyTo = $state(false);
  const idv = $derived(preview ? describeIdentities(preview.data["identities"]) : { lines: [], hasReplyTo: false });
  let fileEl = $state<HTMLInputElement>();
  const accounts = $derived(app.accounts.map((a) => a.email));
  const NEEDS_ACCOUNT = ["signatureDefaults", "identities", "rules"];

  async function backup() {
    busy = true; result = [];
    try { app.notify(`Backup saved to ${await exportBackup()}`); } catch (e) { app.fail(e); }
    finally { busy = false; }
  }
  async function picked(e: Event) {
    const f = (e.currentTarget as HTMLInputElement).files?.[0];
    (e.currentTarget as HTMLInputElement).value = "";
    if (!f) return;
    result = []; preview = null; replyTo = false;
    if (f.size > 5 * 1024 * 1024) { app.notify("That file is too large to be a MiraMail backup.", "error"); return; }
    try { preview = await t2.backupInspect(await f.text()); ticked = preview.sections.map((s) => s.key); }
    catch (er) { app.notify(errMsg(er), "error"); }
  }
  function note(key: string, count: number): string {
    if (!preview || !NEEDS_ACCOUNT.includes(key)) return "";
    const a = applicable(preview.data[key], accounts);
    return a.skipped ? ` (${a.applicable} for your accounts, ${a.skipped} skipped: account not added here)` : "";
  }
  async function restore() {
    if (!preview) return;
    const ok = await app.ask({ title: "Restore these settings?", body: `The ticked sections replace or add to the settings on ${thisDevice()}. Passwords are not part of a backup, so accounts keep their own. Rules come back switched off.`, ok: "Restore" });
    if (!ok) return;
    busy = true;
    try { result = await apply(preview, ticked, accounts, { replyTo }); preview = null; } catch (e) { app.fail(e); }
    finally { busy = false; }
  }
</script>

<section class="bk" aria-labelledby="bk-h">
  <h4 id="bk-h">Backup and restore settings</h4>
  <p class="note">Saves preferences, signatures, sender identities, rules and saved searches to a file in Downloads. It never contains passwords, tokens or API keys: those stay in the {keychainName()}, so add your accounts again on a new {deviceWord()} and then restore.</p>
  <div class="row">
    <button class="btn" onclick={backup} disabled={busy}>Back up settings</button>
    <button class="btn ghost" onclick={() => fileEl?.click()} disabled={busy}>Restore from file…</button>
    <input bind:this={fileEl} type="file" accept="application/json,.json" hidden onchange={picked} />
  </div>
  {#if preview}
    <div class="pv" role="group" aria-label="Restore preview">
      <div class="dim">Backup made {preview.createdAt ? new Date(preview.createdAt).toLocaleString() : "at an unknown time"}. Tick what to restore:</div>
      {#each preview.sections as s (s.key)}
        <label><input type="checkbox" checked={ticked.includes(s.key)} onchange={(e) => (ticked = e.currentTarget.checked ? [...ticked, s.key] : ticked.filter((k) => k !== s.key))} /> {s.label}{s.count > 1 || s.key === "rules" ? ` · ${s.count}` : ""}<span class="dim">{note(s.key, s.count)}</span></label>
      {/each}
      {#if ticked.includes("identities") && idv.lines.length}
        <div class="dim">Sender identities in this backup:</div>
        <ul class="res">{#each idv.lines as l}<li>{l}</li>{/each}</ul>
        {#if idv.hasReplyTo}
          <label><input type="checkbox" bind:checked={replyTo} /> Restore reply-to addresses (replies would go to the addresses listed above)</label>
        {/if}
      {/if}
      <div class="row"><button class="btn" onclick={restore} disabled={busy || !ticked.length}>Restore</button><button class="btn ghost" onclick={() => (preview = null)} disabled={busy}>Cancel</button></div>
    </div>
  {/if}
  {#if result.length}<ul class="res" role="status">{#each result as r}<li>{r}</li>{/each}</ul>{/if}
</section>

<style>
  .bk { display: grid; gap: 10px; }
  h4 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); text-transform: uppercase; }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .row { display: flex; gap: 8px; flex-wrap: wrap; }
  .pv { border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; display: grid; gap: 6px; }
  .pv label { display: flex; gap: 8px; align-items: baseline; font-size: 0.9rem; }
  .dim { color: var(--muted); font-size: 0.85rem; }
  .res { margin: 0; padding-left: 18px; font-size: 0.9rem; }
</style>
