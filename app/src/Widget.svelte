<script lang="ts">
  import { onMount } from "svelte";
  import { EMPTY_SUMMARY, onBadge, platformApi, type InboxSummary } from "./lib/platformapi";

  let s = $state<InboxSummary>({ ...EMPTY_SUMMARY });
  const one = $derived(s.widgetAccount);
  const count = $derived(one ? (s.perAccount[one] ?? 0) : s.total);
  const accounts = $derived(one ? [one] : s.accounts);
  const big = $derived(count > 999 ? "999+" : String(count));

  onMount(() => {
    let off: (() => void) | undefined;
    void platformApi.getSummary().then((x) => (s = x)).catch(() => {});
    void onBadge((x) => (s = x)).then((f) => (off = f));
    return () => off?.();
  });
  const openInbox = () => { void platformApi.focusMain(one ?? s.accounts[0]); };
  const openMsg = (r: InboxSummary["recent"][number]) => { void platformApi.focusMain(r.account, r.folder, r.uid); };
  function ctx(e: MouseEvent) { e.preventDefault(); void platformApi.widgetMenu(); }
</script>

<div class="w" data-tauri-drag-region oncontextmenu={ctx} role="group" aria-label="MiraMail inbox count">
  <button class="count" onclick={openInbox} aria-label="{count} unread. Open inbox">
    <span class="n">{big}</span>
    <span class="label">unread</span>
  </button>

  {#if accounts.length > 1}
    <ul class="accts" data-tauri-drag-region>
      {#each accounts as a (a)}<li data-tauri-drag-region><span class="ell">{a}</span><span class="mono">{s.perAccount[a] ?? 0}</span></li>{/each}
    </ul>
  {:else if accounts.length === 1}
    <div class="one ell" data-tauri-drag-region>{accounts[0]}</div>
  {/if}

  {#if s.recent.length}
    <ul class="recent">
      {#each s.recent as r (r.account + r.uid)}
        <li><button onclick={() => openMsg(r)}>
          <span class="from ell">{r.from}</span>
          {#if s.previews && r.subject}<span class="subj ell">{r.subject}</span>{/if}
        </button></li>
      {/each}
    </ul>
  {:else}
    <div class="empty" data-tauri-drag-region>{count === 0 ? "All caught up" : ""}</div>
  {/if}
</div>

<style>
  :global(html), :global(body), :global(#app) { background: var(--bg); }
  .w {
    height: 100%; box-sizing: border-box; padding: 14px; display: flex; flex-direction: column; gap: 10px; overflow: hidden;
    border: 1px solid var(--line); background: var(--bg);
  }
  .count { background: none; border: 0; padding: 0; text-align: left; display: grid; gap: 0; color: var(--ink); }
  .n { font-size: 3.4rem; font-weight: 600; line-height: 1; letter-spacing: .01em; color: var(--accent); }
  .label { font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .12em; color: var(--muted); }
  ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 4px; }
  .accts li { display: flex; justify-content: space-between; gap: 8px; font-size: 0.8rem; color: var(--muted); }
  .one { font-size: 0.8rem; color: var(--muted); }
  .recent { border-top: 1px solid var(--line); padding-top: 8px; min-height: 0; overflow: hidden; }
  .recent button { width: 100%; background: none; border: 0; padding: 3px 4px; text-align: left; display: grid; border-radius: var(--radius); color: var(--text); }
  .recent button:hover { background: color-mix(in srgb, var(--line) 55%, transparent); }
  .from { font-size: 0.8667rem; color: var(--ink); font-weight: 500; }
  .subj { font-size: 0.8rem; color: var(--muted); }
  .empty { font-size: 0.8667rem; color: var(--muted); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>
