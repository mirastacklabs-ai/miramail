<script lang="ts">
  // Recipients input with autocomplete from the local address book (cached mail only; no network).
  import { api } from "../../lib/api";
  import { formatMailbox, lastToken, replaceLastToken } from "../../lib/composetext";
  import { contactsApi } from "../../lib/contactsapi";
  import { mailbox } from "../../lib/contactsmodel";
  import { mergeSuggestions, type Pick } from "../../lib/contactsmodel";
  import { splitAddrs, validAddr } from "../../lib/util";
  import { chipParts } from "../../lib/phonemodel";
  import { untrack } from "svelte";

  let { id, value = $bindable(""), placeholder = "", el = $bindable(), account = "", chips = false }: { id: string; value: string; placeholder?: string; el?: HTMLInputElement; account?: string; /** phone: finished recipients become removable chips */ chips?: boolean } = $props();
  // Chips mode: `value` stays the comma-separated string the rest of Compose reads; `done` are the finished chips, `draft` the text being typed.
  let done = $state<string[]>(untrack(() => (chips ? splitAddrs(value) : [])));
  let draft = $state("");
  let foc = $state(false);
  $effect(() => {
    if (!chips) return;
    const v = [...done, draft.trim()].filter(Boolean).join(", ");
    untrack(() => { if (v !== value) value = v; });
  });
  // `value` changed from outside (a restored draft, a Cc/Bcc fill-in): rebuild the chips from it. Only `value` is tracked here.
  $effect(() => {
    if (!chips) return;
    const v = value;
    untrack(() => {
      if (v === [...done, draft.trim()].filter(Boolean).join(", ")) return;
      done = splitAddrs(v); draft = "";
    });
  });
  // Chips always show the real address (a display name can say anything); tap a chip to see all of it.
  let shown = $state(-1);
  function commit() { const t = draft.trim(); if (t) done = [...done, ...splitAddrs(t)]; draft = ""; close(); }
  function dropChip(i: number) { done = done.filter((_, j) => j !== i); el?.focus(); }
  function draftInput() {
    if (/[,;\n]$/.test(draft) || (/\s$/.test(draft) && validAddr(draft.trim()))) { commit(); return; }
    changed();
  }
  function chipKey(e: KeyboardEvent) {
    if (e.key === "Backspace" && !draft && done.length) { e.preventDefault(); done = done.slice(0, -1); return; }
    if (e.key === "Enter" && !open && draft.trim()) { e.preventDefault(); commit(); return; }
    key(e);
  }
  let list = $state<Pick[]>([]);
  let idx = $state(-1);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let seq = 0;
  const open = $derived(list.length > 0);

  function changed() {
    clearTimeout(timer);
    const tok = chips ? draft.trim() : lastToken(value);
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
      } catch { list = []; }
    }, 120);
  }
  function close() { seq++; clearTimeout(timer); list = []; idx = -1; }
  // A group fills in every member (first address of each), a person fills in one mailbox.
  function pick(s: Pick) {
    if (chips) { done = [...done, ...(s.group ? (s.members ?? []).map((m) => mailbox(m.name, m.address)) : [formatMailbox(s.name, s.address)])]; draft = ""; close(); el?.focus(); return; }
    value = replaceLastToken(value, s.group ? (s.members ?? []).map((m) => mailbox(m.name, m.address)).join(", ") : formatMailbox(s.name, s.address)); close(); el?.focus(); }
  function key(e: KeyboardEvent) {
    if (!open) return;
    if (e.key === "ArrowDown") { e.preventDefault(); idx = (idx + 1) % list.length; }
    else if (e.key === "ArrowUp") { e.preventDefault(); idx = (idx + list.length - 1) % list.length; }
    else if ((e.key === "Enter" || e.key === "Tab") && idx >= 0 && !e.metaKey && !e.ctrlKey) { e.preventDefault(); e.stopPropagation(); pick(list[idx]); }
    else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(); }
  }
</script>

{#if chips}
<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="af ch" class:foc onclick={() => el?.focus()}>
  {#each done as c, i (`${i}:${c}`)}
    {@const p = chipParts(c)}
    <span class="chip" class:bad={!validAddr(c)} class:open={shown === i}><button type="button" class="cl" title={p.addr} aria-label={p.name ? `${p.name}, ${p.addr}` : p.addr} aria-expanded={shown === i} onclick={(e) => { e.stopPropagation(); shown = shown === i ? -1 : i; }}><span class="ca mono">{p.addr}</span>{#if p.name}<span class="cn">{p.name}</span>{/if}</button><button type="button" class="cx" aria-label="Remove {c}" onclick={(e) => { e.stopPropagation(); dropChip(i); }}>×</button></span>
  {/each}
  <input {id} bind:this={el} bind:value={draft} placeholder={done.length ? "" : placeholder} spellcheck="false" autocomplete="off" autocapitalize="off" inputmode="email" role="combobox" aria-expanded={open} aria-controls="{id}-list" aria-autocomplete="list"
    oninput={draftInput} onkeydown={chipKey} onfocus={() => (foc = true)} onblur={() => { foc = false; commit(); }} />
  {#if open}
    <ul class="dd" id="{id}-list" role="listbox">
      {#each list as s, i (s.group ? `g:${s.group}` : s.address)}
        <li role="option" aria-selected={i === idx} class:on={i === idx}>
          <button type="button" tabindex="-1" onpointerdown={(e) => e.preventDefault()} onmousedown={(e) => e.preventDefault()} onclick={() => pick(s)}>
            {#if s.group}<span class="nm">{s.group}</span><span class="ad mono">group, {s.members?.length ?? 0} people</span>{:else}{#if s.name}<span class="nm">{s.name}</span>{/if}<span class="ad mono">{s.address}</span>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>
{:else}
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
{/if}

<style>
  .af { position: relative; flex: 1; min-width: 0; display: flex; }
  .af input { flex: 1; background: none; border: 0; color: var(--ink); font: 400 13px var(--font-mono); padding: 11px 0; outline-offset: 4px; min-width: 0; }
  .dd { position: absolute; top: 100%; left: 0; right: 0; z-index: 20; margin: 0; padding: 4px; list-style: none; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); box-shadow: 0 6px 24px rgba(0, 0, 0, .25); max-height: 240px; overflow: auto; }
  .dd button { display: flex; gap: 10px; align-items: baseline; width: 100%; text-align: left; background: none; border: 0; color: var(--text); padding: 6px 8px; border-radius: var(--radius); }
  .dd li.on button, .dd button:hover { background: color-mix(in srgb, var(--accent) 16%, transparent); }
  .af.ch { flex-wrap: nowrap; align-items: center; gap: 6px; min-height: 44px; overflow: hidden; }
  .af.ch.foc { flex-wrap: wrap; overflow: visible; padding: 4px 0; }
  .af.ch input { flex: 1 0 96px; font: 400 17px var(--font-mono); padding: 0; min-height: 44px; }
  .chip { flex: none; max-width: 70%; display: inline-flex; align-items: center; border: 1px solid var(--line); border-radius: 999px; padding: 0 0 0 12px; height: 32px; font: 400 15px var(--font-mono); color: var(--ink); }
  .chip.bad { border-color: var(--danger); color: var(--danger); }
  .chip .cl { min-width: 0; display: inline-flex; gap: 6px; align-items: baseline; overflow: hidden; background: none; border: 0; padding: 0; color: inherit; font: inherit; text-align: left; height: 100%; align-items: center; }
  .chip .ca { flex: 0 1 auto; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chip .cn { flex: 1 1 0; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--muted); font-size: 0.85em; }
  .chip.open { max-width: 100%; height: auto; min-height: 32px; }
  .chip.open .cl { height: auto; overflow: visible; flex-wrap: wrap; padding: 4px 0; }
  .chip.open .ca, .chip.open .cn { overflow: visible; white-space: normal; overflow-wrap: anywhere; }
  .chip .cx { flex: none; width: 40px; height: 44px; margin: -6px 0; background: none; border: 0; color: var(--muted); font-size: 1.1rem; }
  .nm { color: var(--ink); } .ad { color: var(--muted); font-size: 12px; overflow: hidden; text-overflow: ellipsis; }
</style>
