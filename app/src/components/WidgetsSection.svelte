<script lang="ts">
  import { onMount } from "svelte";
  import { widgetApi } from "../lib/widgetapi";
  import { app } from "../lib/state.svelte";

  let loaded = $state(false);
  let details = $state(false);
  onMount(() => { void widgetApi.getPrefs().then((p) => { details = p.showDetails; loaded = true; }).catch(() => { loaded = true; }); });
  async function toggle(on: boolean) {
    const before = details;
    details = on;
    try { details = (await widgetApi.setShowDetails(on)).showDetails; } catch (e) { details = before; app.fail(e); }
  }
</script>

<section aria-labelledby="wd-h">
  <h3 id="wd-h">Widgets</h3>
  <p class="note">Add a MiraMail widget from the Home Screen (touch and hold, then the + button). Widgets show a snapshot from the last time MiraMail synced, refreshed now and then; they never connect to your mail servers. Tap a widget to open MiraMail.</p>
  <label class="chk"><input type="checkbox" checked={details} disabled={!loaded} onchange={(e) => toggle(e.currentTarget.checked)} /> Show sender names and subjects in widgets</label>
  <p class="note">Off by default: the Mail widget then shows only the unread count and when mail arrived, and the Calendar widget shows "Busy" instead of event titles. Widgets can be seen on the Home Screen without unlocking your phone.</p>
</section>

<style>
  section { display: grid; gap: 10px; }
  h3 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; text-transform: uppercase; color: var(--muted); }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.9333rem; }
  .chk input { accent-color: var(--accent); }
</style>
