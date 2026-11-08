<script lang="ts">
  // "Sent to model" disclosure for one assistant turn: what left this Mac, to where, and the raw request.
  import type { EgressRow } from "../../lib/aitypes";
  import { fmtBytes, fmtTokens, prettyJson, toMs } from "../../lib/aivm";

  let { row }: { row: EgressRow } = $props();
  let open = $state(false);
  let raw = $state(false);
  const uid = `ci-${Math.random().toString(36).slice(2, 8)}`;
  const when = $derived(new Date(toMs(row.at)).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" }));
</script>

<div class="ci">
  <button type="button" class="tg" aria-expanded={open} aria-controls="{uid}-b" onclick={() => (open = !open)}>
    <span class="car" aria-hidden="true">{open ? "▾" : "▸"}</span> Sent to model
  </button>
  {#if open}
    <div class="b" id="{uid}-b">
      <dl>
        <dt>Endpoint</dt><dd class="mono">{row.host}</dd>
        <dt>Model</dt><dd class="mono">{row.model}</dd>
        <dt>Time</dt><dd class="mono">{when}</dd>
        {#if (row.requests ?? 1) > 1}<dt>Requests</dt><dd class="mono">{row.requests} (the first, then one after each round of mail lookups)</dd>{/if}
        <dt>Size</dt><dd class="mono">{fmtBytes(row.bytes)} · about {fmtTokens(row.estTokens)} tokens{#if (row.requests ?? 1) > 1} (last request){/if}</dd>
        <dt>Mail items</dt>
        <dd>
          {#if row.items.length === 0}<span class="none">None. No mail content was included.</span>
          {:else}<ul>{#each row.items as it (it.ref)}<li>{#if it.ref === "earlier"}{it.subject}{:else}<span class="mono">{it.ref}</span> {it.subject} <span class="from">from {it.from}</span>{/if}</li>{/each}</ul>
            <span class="none">The text of these messages (after redaction) is in the raw request below.</span>{/if}
        </dd>
      </dl>
      <button type="button" class="tg" aria-expanded={raw} aria-controls="{uid}-r" onclick={() => (raw = !raw)}>
        <span class="car" aria-hidden="true">{raw ? "▾" : "▸"}</span> Raw request{(row.requests ?? 1) > 1 ? " (last, includes all tool results)" : ""}
      </button>
      {#if raw}<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <pre class="mono selectable" id="{uid}-r" tabindex="0" aria-label="Raw request JSON">{prettyJson(row.requestJson)}</pre>{/if}
    </div>
  {/if}
</div>

<style>
  .ci { font-size: 0.7667rem; color: var(--muted); }
  .tg { background: none; border: 0; padding: 2px 0; color: var(--muted); text-transform: uppercase; letter-spacing: .1em; font-size: 0.6667rem; font-weight: 600; display: inline-flex; gap: 4px; align-items: center; }
  .tg:hover { color: var(--ink); }
  .car { width: 10px; }
  .b { margin-top: 4px; padding: 8px 10px; border: 1px solid var(--line); border-radius: var(--radius); display: grid; gap: 8px; background: color-mix(in srgb, var(--line) 20%, transparent); }
  dl { margin: 0; display: grid; grid-template-columns: auto 1fr; gap: 3px 12px; }
  dt { text-transform: uppercase; letter-spacing: .08em; font-size: 0.6667rem; padding-top: 2px; }
  dd { margin: 0; color: var(--text); overflow-wrap: anywhere; }
  ul { margin: 0; padding-left: 16px; }
  .from, .none { color: var(--muted); }
  pre { margin: 0; max-height: 220px; overflow: auto; padding: 8px; border: 1px solid var(--line); border-radius: var(--radius); white-space: pre-wrap; overflow-wrap: anywhere; color: var(--text); }
</style>
