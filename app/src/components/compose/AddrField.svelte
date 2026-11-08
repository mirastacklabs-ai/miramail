<script lang="ts">
  // Recipients input with autocomplete from the local address book (cached mail only; no network).
  import { api } from "../../lib/api";
  import { formatMailbox, lastToken, replaceLastToken } from "../../lib/composetext";
  import { contactsApi } from "../../lib/contactsapi";
  import { mailbox } from "../../lib/contactsmodel";
  import { mergeSuggestions, type Pick } from "../../lib/contactsmodel";

  let { id, value = $bindable(""), placeholder = "", el = $bindable(), account = "" }: { id: string; value: string; placeholder?: string; el?: HTMLInputElement; account?: string } = $props();
  let list = $state<Pick[]>([]);
  let idx = $state(-1);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;
  const open = $derived(list.length > 0);

  function changed() {
    clearTimeout(timer);
    const tok = lastToken(value);
    if (!tok) { seq++; list = []; return; }
    timer = setTimeout(async () => {
      const mine = ++seq;
      try {
        // Address book (contacts + groups) first, then the mail-history suggestions; a failing address book never blocks the rest.
        const [r, book] = await Promise.all([
          api.suggestAddresses(tok, 8, account || undefined),
          contactsApi.suggest(tok, 5).catch(() => ({ contacts: [], groups: [] })),
        ]);
        if (mine !== seq) return;
        list = mergeSuggestions(book, r, 8);
        idx = list.length ? 0 : -1;
        return;
        list = r; idx = r.length ? 0 : -1;
      } catch { list = []; }
    }, 120);
  }
  function close() { seq++; clearTimeout(timer); list = []; idx = -1; }
  // A group fills in every member (first address of each), a person fills in one mailbox.
  function pick(s: Pick) { value = replaceLastToken(value, s.group ? (s.members ?? []).map((m) => mailbox(m.name, m.address)).join(", ") : formatMailbox(s.name, s.address)); close(); el?.focus(); }
  function key(e: KeyboardEvent) {
    if (!open) return;
    if (e.key === "ArrowDown") { e.preventDefault(); idx = (idx + 1) % list.length; }
    else if (e.key === "ArrowUp") { e.preventDefault(); idx = (idx + list.length - 1) % list.length; }
    else if ((e.key === "Enter" || e.key === "Tab") && idx >= 0 && !e.metaKey && !e.ctrlKey) { e.preventDefault(); e.stopPropagation(); pick(list[idx]); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(); }
  }
</script>

<div class="af">
  <input {id} bind:this={el} bind:value {placeholder} spellcheck="false" autocomplete="off" role="combobox" aria-expanded={open} aria-controls="{id}-list" aria-autocomplete="list"
    oninput={changed} onkeydown={key} onblur={() => setTimeout(close, 120)} />
  {#if open}
    <ul class="dd" id="{id}-list" role="listbox">
      {#each list as s, i (s.group ? `g:${s.group}` : s.address)}
        <li role="option" aria-selected={i === idx} class:on={i === idx}>
          <button type="button" tabindex="-1" onmousedown={(e) => e.preventDefault()} onclick={() => pick(s)}>
            {#if s.group}<span class="nm">{s.group}</span><span class="ad mono">group, {s.members?.length ?? 0} people</span>{:else}{#if s.name}<span class="nm">{s.name}</span>{/if}<span class="ad mono">{s.address}</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .af { position: relative; flex: 1; min-width: 0; display: flex; }
  .af input { flex: 1; background: none; border: 0; color: var(--ink); font: 400 13px var(--font-mono); padding: 11px 0; outline-offset: 4px; min-width: 0; }
  .dd { position: absolute; top: 100%; left: 0; right: 0; z-index: 20; margin: 0; padding: 4px; list-style: none; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); box-shadow: 0 6px 24px rgba(0, 0, 0, .25); max-height: 240px; overflow: auto; }
  .dd button { display: flex; gap: 10px; align-items: baseline; width: 100%; text-align: left; background: none; border: 0; color: var(--text); padding: 6px 8px; border-radius: var(--radius); }
  .dd li.on button, .dd button:hover { background: color-mix(in srgb, var(--accent) 16%, transparent); }
  .nm { color: var(--ink); } .ad { color: var(--muted); font-size: 12px; overflow: hidden; text-overflow: ellipsis; }
</style>
