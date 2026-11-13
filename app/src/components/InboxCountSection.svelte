<script lang="ts">
  import { deviceWord, thisDevice } from "../lib/device";
  import { onMount } from "svelte";
  import { app } from "../lib/state.svelte";
  import Select from "./Select.svelte";
  import { platformPrefs } from "../lib/platformprefs.svelte";
  import type { TrayStyle } from "../lib/platformapi";

  const STYLES: { v: TrayStyle; label: string }[] = [{ v: "unread", label: "Unread" }, { v: "total", label: "Total in Inbox" }, { v: "hideZero", label: "Hide when 0" }];
  const p = $derived(platformPrefs.v);
  onMount(() => {
    void platformPrefs.load();
    let off: (() => void) | undefined;
    void platformPrefs.watch().then((f) => (off = f));
    return () => off?.();
  });
</script>

<section aria-labelledby="ic-h">
  <h3 id="ic-h">Inbox count</h3>
  <p class="note">The number of unread messages in your Inbox, shown outside the app. It is counted on {thisDevice()} from mail MiraMail already downloaded; nothing extra is fetched and nothing leaves the {deviceWord()}.</p>

  <label class="chk"><input type="checkbox" checked={p.dockBadge} disabled={!platformPrefs.loaded} onchange={(e) => platformPrefs.patch({ dockBadge: e.currentTarget.checked })} /> Show the unread count on the Dock icon</label>

  <label class="chk"><input type="checkbox" checked={p.trayShow} disabled={!platformPrefs.loaded} onchange={(e) => platformPrefs.patch({ trayShow: e.currentTarget.checked })} /> Show a menu-bar item</label>
  {#if p.trayShow}
    <div class="row"><span class="label" id="ic-style">Menu-bar count</span>
      <div class="seg" role="radiogroup" aria-labelledby="ic-style">
        {#each STYLES as s (s.v)}<button role="radio" aria-checked={p.trayStyle === s.v} class:on={p.trayStyle === s.v} onclick={() => platformPrefs.patch({ trayStyle: s.v })}>{s.label}</button>{/each}
      </div></div>
  {/if}

  <label class="chk"><input type="checkbox" checked={p.widgetShow} disabled={!platformPrefs.loaded} onchange={(e) => platformPrefs.patch({ widgetShow: e.currentTarget.checked })} /> Show a small floating widget on the desktop</label>
  {#if p.widgetShow}
    <label class="chk sub"><input type="checkbox" checked={p.widgetPreviews} onchange={(e) => platformPrefs.patch({ widgetPreviews: e.currentTarget.checked })} /> Show message subjects in the widget (senders are always shown)</label>
    {#if app.accounts.length > 1}
      <div class="field sub"><label class="label" for="ic-acct">Widget shows</label>
        <Select id="ic-acct" value={p.widgetAccount ?? ""} options={[{ value: "", label: "All accounts" }, ...app.accounts.map((a) => ({ value: a.email, label: a.email }))]} onchange={(v) => platformPrefs.patch({ widgetAccount: v || null })} /></div>
    {/if}
    <p class="note sub">Drag the widget to move it; right-click it to choose an account, refresh or hide it.</p>
  {/if}
</section>

<style>
  section { display: grid; gap: 10px; }
  h3 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; text-transform: uppercase; color: var(--muted); }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.9333rem; }
  .chk input { accent-color: var(--accent); }
  .sub { margin-left: 24px; }
  .row { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; margin-left: 24px; }
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  .seg button { background: none; border: 0; border-bottom: 2px solid transparent; padding: 5px 10px; font-size: 0.8rem; color: var(--muted); }
  .seg button.on { color: var(--ink); border-bottom-color: var(--accent); background: color-mix(in srgb, var(--line) 40%, transparent); }
</style>
