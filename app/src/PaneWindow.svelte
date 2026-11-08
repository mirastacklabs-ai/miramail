<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "./lib/state.svelte";
  import { workspace } from "./lib/workspace.svelte";
  import { cal } from "./lib/cal.svelte";
  import { api, onClosePane, onWsEvent } from "./lib/api";
  import Workspace from "./components/Workspace.svelte";
  import Overlays from "./components/Overlays.svelte";

  let { id }: { id: string } = $props();
  let failed = $state(false);

  onMount(() => {
    let off: (() => void) | undefined, offClose: (() => void) | undefined, dead = false;
    void (async () => {
      await app.initLite();
      const ok = await workspace.initPane(id);
      if (!ok) {
        // Nothing to ask the user about: Cmd+W, the red button and the Close button all just close (and forget) this window.
        failed = true;
        const close = () => { void api.closePaneWindow().catch(() => {}); };
        const a = await onWsEvent("pane://close-request", close), b = await onClosePane(close);
        if (dead) { a(); b(); } else { off = a; offClose = b; }
        return;
      }
      if (workspace.ws.panes[id]?.kind === "event") void cal.start();
      const a = await workspace.bindNative(), b = await onClosePane(() => workspace.closeFocused());
      if (dead) { a(); b(); } else { off = a; offClose = b; }
    })();
    return () => { dead = true; off?.(); offClose?.(); };
  });
  function keydown(e: KeyboardEvent) {
    if (e.defaultPrevented) return;
    workspace.key(e);
  }
</script>

<svelte:window onkeydown={keydown} />
<div class="drag" data-tauri-drag-region></div>

{#if failed}
  <div class="boot">
    <p>This window's pane could not be restored.</p>
    <button type="button" onclick={() => { void api.closePaneWindow().catch(() => {}); }}>Close window</button>
  </div>
{:else if workspace.ready}
  <div class="shell"><Workspace /></div>
{:else}
  <div class="boot"></div>
{/if}
<Overlays />

<style>
  .drag { position: fixed; top: 0; left: 0; right: 0; height: 28px; padding-left: 80px; z-index: 10; }
  .boot { height: 100%; background: var(--bg); display: grid; place-content: center; color: var(--muted); }
  .shell { display: flex; height: 100%; }
</style>
