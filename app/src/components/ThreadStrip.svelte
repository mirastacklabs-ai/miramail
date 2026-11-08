<script lang="ts">
  // Earlier messages of the open message's conversation, collapsed above the body (local: reads the message cache).
  import { app } from "../lib/state.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import { localApi } from "../lib/localapi";
  import { earlierMembers } from "../lib/thread";
  import { addrLabel, listDate } from "../lib/util";
  import type { MessageSummary } from "../lib/types";

  let { account, folder, uid }: { account: string; folder: string; uid: number } = $props();
  let earlier = $state<MessageSummary[]>([]);
  let openList = $state(false);
  let tok = 0;
  $effect(() => {
    const [a, f, u, on] = [account, folder, uid, prefs.v.threading];
    openList = false;
    if (!on) { earlier = []; return; }
    const t = ++tok;
    localApi.threadMembers(a, f, u).then((m) => { if (t === tok) earlier = earlierMembers(m, f, u); }).catch(() => { if (t === tok) earlier = []; });
  });
  async function go(m: MessageSummary) {
    if (m.folder !== app.folder) await app.selectFolder(m.folder);
    if (app.messages.some((x) => x.uid === m.uid)) await app.open(m.uid);
    else app.notify("That message is older than what is loaded in its folder; load older messages to open it.");
  }
</script>

{#if earlier.length}
  <div class="strip">
    <button class="head" aria-expanded={openList} onclick={() => { openList = !openList; }}>
      <svg viewBox="0 0 24 24" class:open={openList}><path d="M9 6l6 6-6 6"/></svg>
      {earlier.length} earlier message{earlier.length === 1 ? "" : "s"} in this conversation
    </button>
    {#if openList}
      <ul>
        {#each earlier as m (m.folder + "\u0000" + m.uid)}
          <li><button onclick={() => go(m)}>
            <span class="who ell">{addrLabel(m.from)}</span>
            <span class="what ell">{m.subject || "(no subject)"}{m.folder !== folder ? ` · ${m.folder}` : ""}</span>
            <span class="mono when">{listDate(m.date, prefs.v.dateFormat)}</span>
          </button></li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .strip { margin: 0 0 8px; border-bottom: 1px solid var(--line); }
  .head { display: flex; align-items: center; gap: 6px; width: 100%; background: none; border: 0; padding: 8px 16px; color: var(--muted); font: 500 0.7667rem var(--font-ui); letter-spacing: .06em; cursor: pointer; text-align: left; }
  .head:hover { color: var(--ink); }
  .head:focus-visible, li button:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; transition: transform .12s; flex: none; }
  svg.open { transform: rotate(90deg); }
  ul { list-style: none; margin: 0; padding: 0 0 6px; }
  li button { display: grid; grid-template-columns: minmax(80px, 160px) 1fr auto; gap: 10px; width: 100%; background: none; border: 0; padding: 5px 16px 5px 30px; color: var(--text); font-size: 0.8667rem; cursor: pointer; text-align: left; }
  li button:hover { background: color-mix(in srgb, var(--line) 40%, transparent); }
  .who { color: var(--ink); }
  .what, .when { color: var(--muted); }
  .when { font-size: 0.7333rem; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>
