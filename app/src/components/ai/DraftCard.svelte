<script lang="ts">
  // A message the assistant drafted. Recipients, subject and warnings are written by MiraMail from the original
  // message / your mailbox; the body is the model's text; the model's own words appear only as "AI's reason".
  // There is deliberately NO Send button: you open the draft in Compose, read it, and send it from there.
  import { ai } from "../../lib/ai.svelte";
  import { canDismissDraft, canOpenDraft, draftHeading, draftPreview, draftRecipients, draftStatusText } from "../../lib/aivm";

  let { id }: { id: number } = $props();
  const d = $derived(ai.drafts.find((x) => x.id === id));
  const busy = $derived(ai.dbusy[id]);
  let all = $state(false);
  const pv = $derived(d ? draftPreview(d.body) : { text: "", cut: false });
  const rec = $derived(d ? draftRecipients(d) : []);
</script>

{#if d}
  <section class="card" class:live={d.state === "proposed"} aria-label="{draftHeading(d)}: {d.subject}">
    <div class="top">
      <span class="label">{draftHeading(d)}</span>
      <span class="state s-{d.state}">{d.state === "proposed" ? "waiting" : d.state}</span>
    </div>
    <dl class="hdr">
      <dt>From</dt><dd class="mono selectable">{d.account}</dd>
      {#each rec as r (r.label)}
        <dt>{r.label}</dt>
        <dd class="mono selectable">
          {#each r.addresses as a, i (a.address)}{#if i}, {/if}<span class:isnew={a.isNew}>{a.address}</span>{#if a.isNew} <span class="tag">new address</span>{/if}{/each}
        </dd>
      {/each}
      <dt>Subject</dt><dd class="selectable">{d.subject}</dd>
    </dl>
    {#if d.warnings.length}
      <ul class="warns" aria-label="Things to check">
        {#each d.warnings as w, i (i)}<li>{w}</li>{/each}
      </ul>
    {/if}
    <pre class="body selectable">{all ? d.body : pv.text}{#if pv.cut && !all}…{/if}</pre>
    {#if pv.cut}<button class="btn ghost sm" aria-expanded={all} onclick={() => (all = !all)}>{all ? "Show less" : "Show all"}</button>{/if}
    {#if d.quote}<p class="note">The original message will be quoted below your text in Compose.</p>{/if}
    {#if d.reason}
      <p class="reason"><span class="label">AI's reason:</span> <span class="selectable">{d.reason}</span></p>
    {/if}
    <div class="foot">
      {#if canOpenDraft(d)}
        <button class="btn" disabled={!!busy} onclick={() => ai.openDraft(id)}>{busy === "opening" ? "Opening…" : d.state === "opened" ? "Open again" : "Open in Compose"}</button>
      {/if}
      {#if canDismissDraft(d)}
        <button class="btn ghost" disabled={!!busy} onclick={() => ai.dismissDraft(id)}>Dismiss</button>
      {/if}
      <span class="note" role="status">{draftStatusText(d)}</span>
    </div>
  </section>
{/if}

<style>
  .card { display: grid; gap: 6px; border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; background: var(--surface); min-width: 0; }
  .card.live { border-left: 3px solid var(--accent); }
  .top { display: flex; justify-content: space-between; gap: 8px; align-items: baseline; }
  .state { font-size: 0.72rem; color: var(--muted); text-transform: capitalize; }
  .s-opened { color: var(--accent); }
  .hdr { display: grid; grid-template-columns: auto 1fr; gap: 2px 10px; margin: 0; font-size: 0.8rem; }
  .hdr dt { color: var(--muted); }
  .hdr dd { margin: 0; overflow-wrap: anywhere; min-width: 0; }
  .isnew { color: var(--signal); font-weight: 600; }
  .tag { font-size: 0.68rem; color: var(--signal); border: 1px solid var(--signal); border-radius: var(--radius); padding: 0 4px; }
  .warns { margin: 0; padding-left: 18px; font-size: 0.8rem; color: var(--ink); display: grid; gap: 1px; }
  .body { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; font-size: 0.8667rem; color: var(--ink); background: color-mix(in srgb, var(--line) 18%, transparent); border-radius: var(--radius); padding: 8px 10px; max-height: 320px; overflow: auto; }
  .note { margin: 0; color: var(--muted); font-size: 0.8rem; }
  .reason { margin: 0; font-size: 0.8rem; color: var(--muted); overflow-wrap: anywhere; }
  .foot { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
</style>
