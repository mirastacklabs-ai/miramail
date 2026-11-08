<script lang="ts">
  // A rule the assistant proposed. It is already saved, but OFF. The description is written by MiraMail; the
  // model's own words appear only as "AI's reason". The preview shows existing mail the rule would match, and
  // nothing has been changed. "Turn on" is the user's decision; the rule then acts only on mail arriving later.
  import { onMount } from "svelte";
  import { auto } from "../../lib/aiauto.svelte";
  import { AUTOMATION_NOTICE, dryRunText } from "../../lib/airules";
  import { app } from "../../lib/state.svelte";

  let { id }: { id: number } = $props();
  const card = $derived(auto.cards[id]);
  const rule = $derived(card?.rule ?? auto.rules.find((r) => r.id === id));
  let gone = $state(false);
  onMount(async () => { if (!(await auto.card(id, true))) gone = true; });

  async function enable() {
    const ok = await app.ask({ title: `Turn on "${rule?.name ?? "this rule"}"?`, body: `${rule?.description ?? ""}\n\n${dryRunText(card?.dryRun ?? null)}\n\n${AUTOMATION_NOTICE}`, ok: "Turn on" });
    if (ok) await auto.setEnabled(id, true);
  }
  async function discard() {
    const ok = await app.ask({ title: "Delete this proposed rule?", body: "The rule is removed. Nothing was ever run.", ok: "Delete", danger: true });
    if (ok) { await auto.remove(id); gone = true; }
  }
</script>

{#if rule && !gone}
  <section class="card" class:live={!rule.enabled} aria-label="Proposed rule: {rule.name}">
    <div class="top">
      <span class="label">Proposed rule</span>
      <span class="state" class:on={rule.enabled}>{rule.enabled ? "On" : "Saved, but off"}</span>
    </div>
    <h4 class="title selectable">{rule.name}</h4>
    <p class="desc selectable">{rule.description}</p>
    {#if card}
      <div class="dry" aria-label="Preview on existing mail">
        <span class="label">Preview on existing mail</span>
        <p>{dryRunText(card.dryRun)}</p>
        {#if card.dryRun?.sample.length}
          <ul>{#each card.dryRun.sample as s, i (i)}<li><span class="mono">{s.folder}</span> {s.from} <span class="dim">· {s.subject}</span></li>{/each}</ul>
        {/if}
      </div>
    {/if}
    {#if rule.reason}<p class="reason"><span class="label">AI's reason:</span> <span class="selectable">{rule.reason}</span></p>{/if}
    <div class="foot">
      {#if !rule.enabled}
        <button class="btn" disabled={!!auto.busy[id] || !rule.valid} onclick={enable}>Enable now…</button>
        <button class="btn ghost" disabled={!!auto.busy[id]} onclick={discard}>Delete</button>
        <span class="note" role="status">Nothing runs until you enable it. You can also edit it under Preferences &gt; AI &gt; Rules.</span>
      {:else}
        <button class="btn ghost" disabled={!!auto.busy[id]} onclick={() => auto.setEnabled(id, false)}>Turn off</button>
        <span class="note" role="status">It acts on mail that arrives from now on, only while MiraMail is open.</span>
      {/if}
    </div>
  </section>
{/if}

<style>
  .card { display: grid; gap: 6px; border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; background: var(--surface); min-width: 0; }
  .card.live { border-left: 3px solid var(--accent); }
  .top { display: flex; justify-content: space-between; gap: 8px; align-items: baseline; }
  .state { font-size: 0.72rem; color: var(--muted); }
  .state.on { color: var(--accent); font-weight: 600; }
  .title { margin: 0; font-size: 0.9333rem; overflow-wrap: anywhere; }
  .desc { margin: 0; font-size: 0.8667rem; line-height: 1.5; overflow-wrap: anywhere; }
  .dry { border: 1px solid var(--line); border-radius: var(--radius); padding: 6px 10px; display: grid; gap: 3px; background: color-mix(in srgb, var(--line) 12%, transparent); }
  .dry p { margin: 0; font-size: 0.8333rem; }
  .dry ul { margin: 2px 0 0; padding-left: 16px; display: grid; gap: 2px; font-size: 0.78rem; overflow-wrap: anywhere; }
  .dim { color: var(--muted); }
  .reason, .note { margin: 0; font-size: 0.8rem; color: var(--muted); overflow-wrap: anywhere; }
  .foot { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
</style>
