<script lang="ts">
  import { onMount, tick } from "svelte";
  import { ai } from "../../lib/ai.svelte";
  import { composerKeyAction, insertNewline } from "../../lib/composerkeys";
  import { AI_FOCUS_EVENT } from "../../lib/aiui.svelte";

  let text = $state("");
  let el = $state<HTMLTextAreaElement>();
  const canSend = $derived(text.trim().length > 0 && !ai.streaming);

  export function focus() { el?.focus(); }
  onMount(() => {
    el?.focus();
    const h = () => el?.focus();
    window.addEventListener(AI_FOCUS_EVENT, h);
    return () => window.removeEventListener(AI_FOCUS_EVENT, h);
  });
  $effect(() => { text; if (el) { el.style.height = "auto"; el.style.height = Math.min(el.scrollHeight, 160) + "px"; } });

  function send() {
    if (!canSend) return;
    const t = text;
    text = "";
    void ai.send(t);
  }
  function key(e: KeyboardEvent) {
    const act = composerKeyAction(e, canSend);
    if (act === "send") { e.preventDefault(); send(); return; }
    if (act === "none") { e.preventDefault(); return; }
    if (act === "newline") {
      e.preventDefault();
      if (el) { const r = insertNewline(text, el.selectionStart, el.selectionEnd); text = r.text; const c = r.caret; void tick().then(() => el?.setSelectionRange(c, c)); }
      return;
    }
    if (act === "default") return;
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === "." && e.metaKey) { e.preventDefault(); void ai.stop(); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); el?.blur(); }
  }
</script>

<div class="cmp">
  <label class="sr-only" for="ai-in">Message to the assistant</label>
  <textarea id="ai-in" bind:this={el} bind:value={text} rows="2" placeholder="Ask MIRA…" onkeydown={key} spellcheck="true"></textarea>
  <div class="row">
    <span class="mono hint">↵ send · ⇧↵ new line · ⌘. stop · Esc leave</span>
    {#if ai.streaming}
      <button class="btn ghost sm" onclick={() => ai.stop()} disabled={ai.stopping}>Stop</button>
    {:else}
      <button class="btn sm" onclick={send} disabled={!canSend}>Send</button>
    {/if}
  </div>
</div>

<style>
  .cmp { flex: none; border-top: 1px solid var(--line); padding: 10px 14px; display: grid; gap: 8px; background: var(--bg); }
  textarea { width: 100%; resize: none; background: transparent; color: var(--ink); border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; font: 400 0.9333rem var(--font-ui); user-select: text; }
  textarea:focus { outline: 2px solid var(--accent); border-color: transparent; }
  .row { display: flex; justify-content: space-between; align-items: center; gap: 8px; }
  .hint { color: var(--muted); font-size: 0.7rem; }
</style>
