<script lang="ts">
  // The dry run of a suggestion: one row per message, with a tick per row while the suggestion is still waiting.
  // Everything is rendered as text (mail subjects and senders are untrusted).
  import { ai } from "../../lib/ai.svelte";
  import { PREVIEW_PAGE, selectedCount } from "../../lib/aivm";
  import type { ProposalItem } from "../../lib/aitypes";

  let { proposalId, items, editable }: { proposalId: number; items: ProposalItem[]; editable: boolean } = $props();

  let shown = $state(PREVIEW_PAGE);
  const rows = $derived(items.slice(0, shown));
  const sel = $derived(selectedCount(items));
  const all = $derived(sel === items.length);
  const some = $derived(sel > 0 && sel < items.length);
  const hasStatus = $derived(items.some((i) => i.status !== "pending"));

  const STATUS: Record<string, string> = {
    pending: "", done: "done", failed: "failed", skipped: "skipped", noop: "already in place", undone: "put back", undo_failed: "could not put back", unknown: "not confirmed",
  };
  const statusText = (i: ProposalItem) => (i.selected || i.status !== "pending" ? STATUS[i.status] ?? i.status : "left out");
</script>

<div class="wrap">
  <table>
    <caption class="sr-only">Messages in this suggestion</caption>
    <thead>
      <tr>
        {#if editable}
          <th class="ck"><input type="checkbox" checked={all} indeterminate={some} aria-label="Select all messages" onchange={(e) => ai.setAllSelected(proposalId, e.currentTarget.checked)} /></th>
        {/if}
        <th>From</th><th>Subject</th><th>Folder</th>{#if hasStatus}<th>Result</th>{/if}
      </tr>
    </thead>
    <tbody>
      {#each rows as it (it.idx)}
        <tr class:off={!it.selected} class:bad={it.status === "failed" || it.status === "skipped" || it.status === "undo_failed" || it.status === "unknown"}>
          {#if editable}
            <td class="ck"><input type="checkbox" checked={it.selected} aria-label="Include {it.subject || 'message'}" onchange={(e) => ai.setSelected(proposalId, it.idx, e.currentTarget.checked)} /></td>
          {/if}
          <td class="from" title={it.sender}>{it.sender || "(unknown)"}</td>
          <td class="subj" class:unread={!it.seen} title={it.subject}>{it.subject || "(no subject)"}</td>
          <td class="fol"><span class="mono">{it.folder}</span>{#if it.toFolder} <span aria-label="to">→</span> <span class="mono">{it.toFolder}</span>{/if}</td>
          {#if hasStatus}<td class="st" title={it.error}>{statusText(it)}{#if it.error} <span class="why">{it.error}</span>{/if}</td>{/if}
        </tr>
      {/each}
    </tbody>
  </table>
  {#if items.length > shown}
    <button class="btn ghost sm more" onclick={() => (shown += PREVIEW_PAGE)}>Show {Math.min(PREVIEW_PAGE, items.length - shown)} more ({items.length - shown} not shown)</button>
  {/if}
  {#if editable}<p class="sum" aria-live="polite">{sel} of {items.length} selected</p>{/if}
</div>

<style>
  .wrap { max-height: 280px; overflow: auto; border: 1px solid var(--line); border-radius: var(--radius); }
  table { width: 100%; border-collapse: collapse; font-size: 0.78rem; table-layout: fixed; }
  th { text-align: left; font-weight: 600; color: var(--muted); padding: 4px 6px; position: sticky; top: 0; background: var(--surface); border-bottom: 1px solid var(--line); }
  td { padding: 3px 6px; border-bottom: 1px solid color-mix(in srgb, var(--line) 50%, transparent); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ck { width: 26px; text-align: center; padding: 3px 2px; }
  .from { width: 26%; } .fol { width: 28%; } .st { width: 24%; white-space: normal; }
  .unread { font-weight: 600; }
  tr.off td { opacity: 0.5; text-decoration: line-through; }
  tr.bad .st { color: var(--signal); }
  .why { color: var(--muted); }
  .sum { margin: 0; padding: 4px 8px; color: var(--muted); font-size: 0.75rem; position: sticky; bottom: 0; background: var(--surface); }
  .more { margin: 4px 6px; }
  input[type="checkbox"] { accent-color: var(--accent); }
</style>
