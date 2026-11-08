<script lang="ts">
  // Preferences > Privacy > Export: mailbox to .mbox (new files in Downloads, progress, cancel).
  import { onMount, onDestroy } from "svelte";
  import { api } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import Select from "./Select.svelte";
  import { onExportProgress, t2, type ExportProgress } from "../lib/t2api";
  import { exportLine } from "../lib/t2model";

  let account = $state(app.account ?? app.accounts[0]?.email ?? "");
  let folder = $state(""); // "" = whole account
  let folders = $state<string[]>([]);
  let progress = $state<ExportProgress | null>(null);
  let off: (() => void) | undefined;
  const running = $derived(progress?.state === "running");

  async function loadFolders() {
    folder = "";
    try { folders = account ? (await api.listFolders(account)).filter((f) => f.selectable).map((f) => f.name) : []; } catch { folders = []; }
  }
  onMount(async () => { off = await onExportProgress((p) => (progress = p)); await loadFolders(); });
  onDestroy(() => off?.());

  async function start() {
    progress = { state: "running", done: 0, total: 0, skipped: 0, path: null, error: null };
    try { await t2.mboxExport(account, folder ? [folder] : null); }
    catch (e) { progress = null; app.fail(e); }
  }
</script>

<section class="ex" aria-labelledby="exp-h">
  <h4 id="exp-h">Export mail</h4>
  <p class="note">Saves a copy of your mail as .mbox files (readable by Apple Mail, Thunderbird and most tools). One folder makes one file in Downloads; the whole account makes a new folder in Downloads with one file per mailbox folder. Existing files are never overwritten. It includes the messages MiraMail has synced, and downloads missing message bodies as it goes.</p>
  <div class="row">
    <Select value={account} options={app.accounts.map((a) => ({ value: a.email, label: a.email }))} onchange={(v) => { account = v; void loadFolders(); }} disabled={running} ariaLabel="Account to export" />
    <Select value={folder} options={[{ value: "", label: "Whole account" }, ...folders.map((f) => ({ value: f, label: f }))]} onchange={(v) => (folder = v)} disabled={running} ariaLabel="Folder to export" />
    {#if running}<button class="btn ghost" onclick={() => t2.mboxCancel()}>Cancel</button>
    {:else}<button class="btn" onclick={start} disabled={!account}>Export…</button>{/if}
  </div>
  {#if progress}
    <div class="prog" role="status" aria-live="polite">
      {#if running && progress.total}<progress max={progress.total} value={progress.done}></progress>{/if}
      <div>{exportLine(progress)}</div>
      {#if progress.path && progress.state !== "running"}<div class="mono dim selectable">{progress.path}</div>{/if}
      {#if progress.state === "cancelled"}<div class="dim">Cancelling never deletes anything: the partial file is a complete .mbox of what was written. Delete it in Finder if you do not want it.</div>{/if}
    </div>
  {/if}
</section>

<style>
  .ex { display: grid; gap: 10px; }
  h4 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); text-transform: uppercase; }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .row { display: flex; gap: 8px; flex-wrap: wrap; }
  .prog { display: grid; gap: 4px; font-size: 0.9rem; }
  progress { width: 100%; }
  .dim { color: var(--muted); font-size: 0.8rem; overflow-wrap: anywhere; }
</style>
