<script lang="ts">
  import Modal from "./Modal.svelte";
  import { platform } from "../lib/platform.svelte";
  import { SHORTCUTS } from "../lib/shortcuts";
  // Tabs layout (Preferences > Appearance > Layout). In Tabs, ⌘ 1 ... 9 pick tabs instead of Mail / Calendar.
  const GROUPS = [...SHORTCUTS, { group: "Tabs layout", rows: [["⇧ ⌘ T", "Switch between Tiles and Tabs"], ["⌘ T", "New message in a new tab"], ["⌘ 1 ... 8 / ⌘ 9", "Jump to that tab / the last tab"], ["⌃ Tab / ⇧ ⌃ Tab", "Next / previous tab"], ["⌘ W", "Close the tab (Mail stays open)"], ["← / → on a tab", "Move between tabs, Enter opens, Delete closes"]] as [string, string][] }];
</script>

<Modal title="Keyboard shortcuts" onclose={() => (platform.showCheat = false)} width={680}>
  <div class="sheet">
    {#each GROUPS as g (g.group)}
      <section><div class="label">{g.group}</div>
        <dl>{#each g.rows as [k, d] (k + d)}<div><dt class="mono">{k}</dt><dd>{d}</dd></div>{/each}</dl>
      </section>
    {/each}
  </div>
</Modal>

<style>
  .sheet { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 18px 28px; align-items: start; }
  section { display: grid; gap: 6px; }
  dl { margin: 0; display: grid; gap: 2px; }
  dl > div { display: flex; gap: 14px; align-items: baseline; padding: 3px 0; border-bottom: 1px solid var(--line); }
  dt { min-width: 104px; color: var(--ink); }
  dd { margin: 0; font-size: 0.9333rem; }
  @media (max-width: 640px) { .sheet { grid-template-columns: 1fr; } }
</style>
