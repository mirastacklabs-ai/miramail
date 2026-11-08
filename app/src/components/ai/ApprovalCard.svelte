<script lang="ts">
  // One suggested change. Title, count and warnings are written by MiraMail from the messages it found;
  // the model's own words appear only as "AI's reason". Nothing happens until Approve is pressed.
  import { ai } from "../../lib/ai.svelte";
  import { approveLabel, canApprove, eventHeading, isEventKind, isPending, isRunning, resultText, warningTone } from "../../lib/aivm";
  import BulkPreviewTable from "./BulkPreviewTable.svelte";

  let { id }: { id: number } = $props();
  const p = $derived(ai.proposals.find((x) => x.id === id) ?? ai.activity.find((x) => x.id === id));
  const d = $derived(ai.details[id]);
  const busy = $derived(ai.pbusy[id]);
  let open = $state(false);

  async function toggle() {
    open = !open;
    if (open) await ai.loadDetail(id);
  }
  const items = $derived(d?.items ?? []);
  const ev = $derived(p && isEventKind(p.kind) ? p.event ?? null : null);
  const isEv = $derived(!!p && isEventKind(p.kind));
  const note = $derived(p && p.state === "done" && isEventKind(p.kind) && ai.notes[id] ? ai.notes[id] : "");
  const notifyLines = $derived((p?.warnings ?? []).filter((w) => warningTone(w) === "notify"));
  const otherWarns = $derived((p?.warnings ?? []).filter((w) => warningTone(w) !== "notify"));
  const when = (t: number) => new Date(t * 1000).toLocaleString();
</script>

{#if p}
  <section class="card" class:live={isPending(p.state)} class:bad={p.state === "failed" || p.state === "partially_done"} aria-label="Suggested change: {p.title}">
    <div class="top">
      <span class="label">{isEv ? eventHeading(p.kind) : "Suggested change"}</span>
      <span class="state s-{p.state}">{p.state.replace("_", " ")}</span>
    </div>
    <h4 class="title">{p.title}</h4>
    {#if ev}
      <p class="cal">Calendar: <span class="selectable">{ev.calendarName}</span></p>
      {#if ev.rows.length}
        <table class="rows" aria-label="What changes">
          <tbody>
            {#each ev.rows as r, i (i)}
              <tr>
                <th scope="row">{r.label}</th>
                <td class="selectable">{#if r.before}<s class="was">{r.before}</s> <span aria-label="becomes">&rarr;</span> {/if}{r.after}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {:else if !isEv && p.kind !== "create_folder"}
      <p class="count">{p.counts.total} message{p.counts.total === 1 ? "" : "s"}{#if isPending(p.state) && d} · {items.filter((i) => i.selected).length} selected{/if}</p>
    {/if}
    {#if (isEv ? otherWarns : p.warnings).length}
      <ul class="warns" aria-label="Things to know">
        {#each isEv ? otherWarns : p.warnings as w, i (i)}<li class:caution={warningTone(w) === "caution"}>{w}</li>{/each}
      </ul>
    {/if}
    {#if p.reason}
      <p class="reason"><span class="label">AI's reason:</span> <span class="selectable">{p.reason}</span></p>
    {/if}
    {#if !isEv && p.kind !== "create_folder"}
      <button class="btn ghost sm" aria-expanded={open} onclick={toggle}>{open ? "Hide" : "Show"} messages</button>
      {#if open}
        {#if d}<BulkPreviewTable proposalId={id} {items} editable={isPending(p.state) && !busy} />
        {:else}<p class="note">Loading…</p>{/if}
      {/if}
    {/if}

    {#if isEv && isPending(p.state)}
      {#each notifyLines as w, i (i)}<p class="notify" role="note">{w}</p>{/each}
    {/if}
    <div class="foot">
      {#if isPending(p.state)}
        <button class="btn" disabled={!!busy || !canApprove(p, items)} onclick={() => ai.approve(id)}>{busy === "approving" ? "Working…" : approveLabel(p, items)}</button>
        <button class="btn ghost" disabled={!!busy} onclick={() => ai.reject(id)}>Reject</button>
        <span class="note">Nothing has been changed. Expires {when(p.expiresAt)}.</span>
      {:else}
        <p class="res" role="status">{resultText(p)}{#if note} {note}{/if}</p>
        {#if isEv && p.error && p.state !== "failed"}<p class="notify" role="note">{p.error}</p>{/if}
        {#if isEv && p.undoable && ev?.undoNote && !isRunning(p.state)}<p class="note">{ev.undoNote}</p>{/if}
        {#if p.undoable && !isRunning(p.state)}
          <button class="btn ghost" disabled={!!busy} onclick={() => ai.undo(id)}>{busy === "undoing" ? "Putting back…" : "Undo"}</button>
        {/if}
      {/if}
    </div>
  </section>
{/if}

<style>
  .card { display: grid; gap: 6px; border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; background: var(--surface); min-width: 0; }
  .card.live { border-left: 3px solid var(--accent); }
  .card.bad { border-left: 3px solid var(--signal); }
  .top { display: flex; justify-content: space-between; gap: 8px; align-items: baseline; }
  .state { font-size: 0.72rem; color: var(--muted); text-transform: capitalize; }
  .s-done, .s-undone { color: var(--accent); }
  .s-failed, .s-partially_done { color: var(--signal); }
  .title { margin: 0; font-size: 0.9333rem; letter-spacing: 0; text-transform: none; overflow-wrap: anywhere; }
  .count, .note, .res { margin: 0; color: var(--muted); font-size: 0.8rem; }
  .res { color: var(--ink); }
  .warns { margin: 0; padding-left: 18px; font-size: 0.8rem; color: var(--ink); display: grid; gap: 1px; }
  .cal { margin: 0; font-size: 0.8rem; color: var(--muted); }
  .rows { border-collapse: collapse; font-size: 0.8rem; width: 100%; }
  .rows th { text-align: left; font-weight: 600; color: var(--muted); padding: 2px 10px 2px 0; vertical-align: top; white-space: nowrap; width: 1%; }
  .rows td { padding: 2px 0; overflow-wrap: anywhere; color: var(--ink); }
  .was { color: var(--muted); opacity: .75; }
  .warns li.caution { color: var(--signal); }
  .notify { margin: 0; font-size: 0.8667rem; font-weight: 600; color: var(--ink); border: 1px solid var(--line); border-left: 3px solid var(--signal); border-radius: var(--radius); padding: 6px 10px; overflow-wrap: anywhere; }
  .reason { margin: 0; font-size: 0.8rem; color: var(--muted); overflow-wrap: anywhere; }
  .foot { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
</style>
