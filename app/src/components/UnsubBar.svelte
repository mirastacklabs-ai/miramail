<script lang="ts">
  import { app } from "../lib/state.svelte";
  import { org } from "../lib/org.svelte";
  import { openExternal } from "../lib/api";

  const m = $derived(app.body?.summary);
  const key = $derived(m ? `${m.account}\u0000${m.folder}\u0000${m.uid}` : "");
  $effect(() => { void key; void org.loadUnsub(m); });
  const u = $derived(org.unsub);

  async function openPage() {
    if (!u?.httpsUrl) return;
    const ok = await app.ask({ title: "Open the unsubscribe page?", body: "This opens the sender's unsubscribe page in your default browser. MiraMail sends nothing itself.", detail: u.httpsUrl, ok: "Open" });
    if (!ok) return;
    try { await openExternal(u.httpsUrl); } catch (e) { app.fail(e); }
  }
</script>

{#if u && (u.mailto || u.httpsHost)}
  <div class="unsub" role="region" aria-label="Unsubscribe">
    <span class="t">This looks like a mailing list.</span>
    <span class="acts">
      {#if u.oneClick}<button class="btn sm" disabled={org.unsubBusy} onclick={() => org.unsubscribe("oneClick")} title="One request to {u.httpsHost}">Unsubscribe</button>{/if}
      {#if u.mailto}<button class="btn sm" class:ghost={u.oneClick} disabled={org.unsubBusy} onclick={() => org.unsubscribe("mailto")} title="Sends an email to {u.mailto}">{u.oneClick ? "Unsubscribe by email" : "Unsubscribe"}</button>{/if}
      {#if u.httpsUrl && !u.oneClick}<button class="btn ghost sm" onclick={openPage} title={u.httpsHost ?? ""}>Open unsubscribe page</button>{/if}
    </span>
  </div>
{/if}

<style>
  .unsub { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; border: 1px solid var(--line); border-left: 2px solid var(--muted); border-radius: var(--radius); padding: 6px 12px; font-size: 0.8667rem; color: var(--muted); }
  .acts { display: inline-flex; gap: 8px; flex-wrap: wrap; }
</style>
