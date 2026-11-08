<script lang="ts">
  import { tick } from "svelte";
  import { ai } from "../../lib/ai.svelte";
  import { app } from "../../lib/state.svelte";
  import { alignEgress, chipDetail, chipText, placeDrafts, placeProposals } from "../../lib/aivm";
  import { plainText } from "../../lib/aimd";
  import { openAiSettings } from "../../lib/aiui.svelte";
  import AiMarkdown from "./AiMarkdown.svelte";
  import ContextInspector from "./ContextInspector.svelte";
  import ApprovalCard from "./ApprovalCard.svelte";
  import DraftCard from "./DraftCard.svelte";
  import RuleCard from "./RuleCard.svelte";

  let box = $state<HTMLDivElement>();
  let stick = true;
  const rows = $derived(alignEgress(ai.messages, ai.egress));
  const live = $derived(Object.fromEntries(ai.messages.filter((m) => m.proposalIds?.length).map((m) => [m.id, m.proposalIds!])));
  const cards = $derived(placeProposals(ai.messages, ai.proposals, live));
  const liveDrafts = $derived(Object.fromEntries(ai.messages.filter((m) => m.draftIds?.length).map((m) => [m.id, m.draftIds!])));
  const dcards = $derived(placeDrafts(ai.messages, ai.drafts, liveDrafts));

  function onScroll() { if (box) stick = box.scrollHeight - box.scrollTop - box.clientHeight < 40; }
  $effect(() => {
    // Re-run on every message / text change; follow the stream only while the reader is at the bottom.
    ai.messages.length; ai.messages[ai.messages.length - 1]?.text; ai.proposals.length; ai.drafts.length;
    if (stick) void tick().then(() => { if (box) box.scrollTop = box.scrollHeight; });
  });

  async function copy(text: string) {
    try { await navigator.clipboard.writeText(plainText(text)); app.notify("Copied"); }
    catch { app.notify("Could not copy to the clipboard.", "error"); }
  }
</script>

<div class="th" bind:this={box} onscroll={onScroll} role="log" aria-label="Conversation" aria-live="polite" aria-busy={ai.streaming}>
  <div class="zm">
  {#if ai.messages.length === 0}
    {#if ai.current?.toolsEnabled}
      <p class="hint">Ask about your mail. The assistant can search and read messages in the folders you allowed and sends the parts it reads to {ai.current.endpointHost}. See "Sent to model" under each reply.{#if ai.current.calendarEnabled} It can also look at the calendars you ticked.{/if}{#if ai.current.suggestEnabled} It may suggest changes (move, archive, trash, flag, new folder), write drafts and propose rules (saved off){#if ai.current.calendarEnabled} and suggest calendar events{/if}; nothing happens until you approve, and nothing is ever sent unless you press Send in Compose.{/if}</p>
    {:else}
      <p class="hint">Ask a question. No mail is sent to the model in this conversation; only what you type here.</p>
    {/if}
  {/if}
  {#each ai.messages as m (m.id)}
    {#if m.role === "notice"}
      <p class="notice" role="note">{m.text}</p>
    {:else}
    <article class="msg {m.role}" aria-label={m.role === "user" ? "You" : "MIRA"}>
      <div class="who label">{m.role === "user" ? "You" : "MIRA"}</div>
      {#if m.role === "user"}
        <p class="u selectable">{plainText(m.text)}</p>
      {:else}
        {#if m.tools?.length}
          <ul class="chips" aria-label="Mail lookups">
            {#each m.tools as t (t.id)}
              <li>
                <details class="chip" class:bad={!t.ok && !t.running}>
                  <summary><span class="dot" aria-hidden="true">{t.running ? "…" : t.ok ? "✓" : "!"}</span> <span class="ct">{chipText(t)}</span></summary>
                  <p class="cd">{chipDetail(t)}</p>
                </details>
              </li>
            {/each}
          </ul>
        {/if}
        {#if m.text}<AiMarkdown text={m.text} />{:else if m.streaming}<p class="wait">{m.tools?.length ? "Reading your mail…" : "Thinking…"}</p>{/if}
        {#each cards.get(m.id) ?? [] as pid (pid)}<ApprovalCard id={pid} />{/each}
        {#each dcards.get(m.id) ?? [] as did (did)}<DraftCard id={did} />{/each}
        {#each m.ruleIds ?? [] as rid (rid)}<RuleCard id={rid} />{/each}
        {#if m.toolLimit}<p class="note">Stopped after too many lookups for one question. Ask again more narrowly.</p>{/if}
        {#if m.stopped}<p class="note">Stopped.</p>{/if}
        {#if m.error}
          <div class="err" role="alert">
            <span class="selectable">{m.error.message}</span>
            <span class="acts">
              {#if m === ai.messages[ai.messages.length - 1]}<button class="btn ghost sm" onclick={() => ai.retry()} disabled={ai.streaming}>Retry</button>{/if}
              <button class="btn ghost sm" onclick={openAiSettings}>AI settings</button>
            </span>
          </div>
        {/if}
        {#if !m.streaming}
          <div class="foot">
            {#if m.text}<button class="btn ghost sm" onclick={() => copy(m.text)} aria-label="Copy reply">Copy</button>{/if}
            {#if rows.get(m.id)}<ContextInspector row={rows.get(m.id)!} />{/if}
          </div>
        {/if}
      {/if}
    </article>
    {/if}
  {/each}
  {#if ai.streaming}
    <div class="stopbar">
      <button class="btn ghost sm" onclick={() => ai.stop()} disabled={ai.stopping}>{ai.stopping ? "Stopping…" : "Stop"} <span class="mono kbd">⌘.</span></button>
    </div>
    <span class="sr-only" role="status">The assistant is responding.</span>
  {/if}
  </div>
</div>

<style>
  .th { flex: 1; min-height: 0; overflow-y: auto; padding: 12px 14px; }
  /* Text size control (A- / A+ in the chat bar): zoom scales text, cards and diagrams together. */
  .zm { display: grid; gap: 14px; align-content: start; zoom: var(--chat-zoom, 1); }
  .hint { margin: 0; color: var(--muted); font-size: 0.8667rem; }
  .notice { margin: 0; text-align: center; color: var(--muted); font-size: 0.8rem; padding: 2px 0; border-top: 1px dashed var(--line); border-bottom: 1px dashed var(--line); }
  .msg { display: grid; gap: 4px; min-width: 0; }
  .msg.user { padding-left: 10px; border-left: 2px solid var(--accent); }
  .msg.assistant { padding-left: 10px; border-left: 2px solid var(--line); }
  .who { color: var(--muted); }
  .u { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font-size: 0.9333rem; color: var(--ink); }
  .wait, .note { margin: 0; color: var(--muted); font-size: 0.8667rem; }
  .err { display: flex; gap: 10px; align-items: center; justify-content: space-between; flex-wrap: wrap; border: 1px solid var(--line); border-left: 2px solid var(--signal); border-radius: var(--radius); padding: 8px 10px; font-size: 0.8667rem; }
  .acts { display: flex; gap: 6px; }
  .foot { display: flex; gap: 10px; align-items: flex-start; flex-wrap: wrap; margin-top: 2px; }
  .chips { list-style: none; margin: 0; padding: 0; display: grid; gap: 3px; }
  .chip { border: 1px solid var(--line); border-radius: var(--radius); font-size: 0.8rem; color: var(--muted); background: color-mix(in srgb, var(--line) 15%, transparent); }
  .chip summary { cursor: pointer; padding: 3px 8px; display: flex; gap: 6px; align-items: baseline; list-style: none; }
  .chip summary::-webkit-details-marker { display: none; }
  .chip summary:hover { color: var(--ink); }
  .chip .dot { width: 12px; text-align: center; color: var(--accent); flex: none; }
  .chip.bad .dot { color: var(--signal); }
  .chip .ct { overflow-wrap: anywhere; min-width: 0; }
  .chip .cd { margin: 0; padding: 2px 8px 6px 26px; font-size: 0.7667rem; }
  .stopbar { position: sticky; bottom: 0; display: flex; justify-content: center; }
  .stopbar .btn { background: var(--surface); }
  .kbd { color: var(--muted); margin-left: 4px; }
</style>
