<script lang="ts" generics="T extends string | number">
  // Branded dropdown: replaces the native <select> (whose popup macOS draws itself and cannot be themed).
  // Usage: <Select value={x} options={[{ value: "a", label: "Alpha", hint: "optional" }]} onchange={(v) => (x = v)} ariaLabel="…" />
  import { tick } from "svelte";

  type Opt = { value: T; label: string; hint?: string; disabled?: boolean };
  let {
    value,
    options,
    onchange,
    id,
    ariaLabel,
    labelledby,
    disabled = false,
    placeholder = "Select…",
    block = true,
    mono = false,
    compact = false,
    keepFocus = false,
    title,
  }: {
    value: T | null | undefined;
    options: Opt[];
    onchange: (v: T) => void;
    id?: string;
    ariaLabel?: string;
    labelledby?: string;
    disabled?: boolean;
    placeholder?: string;
    block?: boolean;
    mono?: boolean;
    /** Small inline control for toolbars. */
    compact?: boolean;
    /** Do not take keyboard focus (rich-text toolbar: the editor keeps its selection). */
    keepFocus?: boolean;
    title?: string;
  } = $props();

  const uid = `sel-${Math.random().toString(36).slice(2, 9)}`;
  let open = $state(false);
  let active = $state(-1);
  let btn = $state<HTMLButtonElement>();
  let list = $state<HTMLUListElement>();
  let pos = $state({ left: 0, top: 0, width: 0, maxH: 280, up: false });
  let typed = "";
  let typedTimer: ReturnType<typeof setTimeout> | undefined;

  const selected = $derived(options.find((o) => o.value === value));
  const enabled = $derived(options.map((o, i) => (o.disabled ? -1 : i)).filter((i) => i >= 0));

  function place() {
    if (!btn) return;
    const r = btn.getBoundingClientRect();
    const below = window.innerHeight - r.bottom - 8;
    const above = r.top - 8;
    const up = below < 180 && above > below;
    pos = { left: r.left, width: Math.max(r.width, 160), top: up ? r.top - 4 : r.bottom + 4, maxH: Math.min(320, Math.max(120, up ? above : below)), up };
  }
  async function show() {
    if (disabled || open) return;
    place();
    open = true;
    active = Math.max(0, options.findIndex((o) => o.value === value));
    await tick();
    list?.querySelector<HTMLElement>(`[data-i="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }
  function hide(refocus = true) {
    open = false;
    if (refocus && !keepFocus) btn?.focus();
  }
  function pick(i: number) {
    const o = options[i];
    if (!o || o.disabled) return;
    hide();
    if (o.value !== value) onchange(o.value);
  }
  function move(delta: number) {
    if (!enabled.length) return;
    const pos0 = enabled.indexOf(active);
    const next = enabled[Math.min(enabled.length - 1, Math.max(0, (pos0 < 0 ? 0 : pos0) + delta))];
    active = next;
    list?.querySelector<HTMLElement>(`[data-i="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }
  function key(e: KeyboardEvent) {
    if (disabled) return;
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) { e.preventDefault(); void show(); }
      return;
    }
    switch (e.key) {
      case "Escape": e.preventDefault(); e.stopPropagation(); hide(); break;
      case "Tab": hide(false); break;
      case "ArrowDown": e.preventDefault(); move(1); break;
      case "ArrowUp": e.preventDefault(); move(-1); break;
      case "Home": e.preventDefault(); active = enabled[0] ?? -1; break;
      case "End": e.preventDefault(); active = enabled[enabled.length - 1] ?? -1; break;
      case "Enter": case " ": e.preventDefault(); if (active >= 0) pick(active); break;
      default:
        if (e.key.length === 1 && !e.metaKey && !e.ctrlKey) {
          typed += e.key.toLowerCase();
          clearTimeout(typedTimer);
          typedTimer = setTimeout(() => (typed = ""), 600);
          const i = options.findIndex((o) => !o.disabled && o.label.toLowerCase().startsWith(typed));
          if (i >= 0) { active = i; list?.querySelector<HTMLElement>(`[data-i="${i}"]`)?.scrollIntoView({ block: "nearest" }); }
        }
    }
  }
  function outside(e: PointerEvent) {
    if (!open) return;
    const t = e.target as Node;
    if (btn?.contains(t) || list?.contains(t)) return;
    hide(false);
  }
</script>

<svelte:window onpointerdown={outside} onresize={() => open && place()} onblur={() => (open = false)} />

<button
  bind:this={btn}
  {id}
  type="button"
  class="sel"
  class:block
  class:mono
  class:compact
  class:open
  {title}
  {disabled}
  role="combobox"
  aria-haspopup="listbox"
  aria-expanded={open}
  aria-controls="{uid}-list"
  aria-label={ariaLabel}
  aria-labelledby={labelledby}
  onmousedown={(e) => { if (keepFocus) e.preventDefault(); }}
  onclick={() => (open ? hide() : void show())}
  onkeydown={key}
>
  <span class="val ell" class:ph={!selected}>{selected ? selected.label : placeholder}</span>
  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
</button>

{#if open}
  <ul
    bind:this={list}
    id="{uid}-list"
    class="menu"
    class:up={pos.up}
    role="listbox"
    aria-label={ariaLabel}
    tabindex="-1"
    style="left:{pos.left}px; width:{pos.width}px; max-height:{pos.maxH}px; {pos.up ? `bottom:${window.innerHeight - pos.top}px` : `top:${pos.top}px`}"
  >
    {#each options as o, i (i)}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <li
        role="option"
        data-i={i}
        aria-selected={o.value === value}
        aria-disabled={o.disabled}
        class:on={o.value === value}
        class:act={i === active}
        class:dis={o.disabled}
        onpointerenter={() => !o.disabled && (active = i)}
        onclick={() => pick(i)}
      >
        <span class="ell">{o.label}</span>
        {#if o.hint}<span class="hint mono ell">{o.hint}</span>{/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .sel { display: inline-flex; align-items: center; justify-content: space-between; gap: 8px; min-height: 34px; padding: 6px 10px; background: var(--surface); color: var(--text); border: 1px solid var(--line); border-radius: var(--radius); font: inherit; text-align: left; cursor: pointer; }
  .sel.block { display: flex; width: 100%; }
  .sel.compact { min-height: 0; padding: 3px 6px; gap: 4px; font-size: 0.8rem; width: auto; max-width: 140px; }
  .sel.mono { font-family: var(--font-mono); font-size: .92em; }
  .sel:hover { border-color: var(--muted); }
  .sel:focus-visible, .sel.open { outline: 2px solid var(--accent); outline-offset: 1px; border-color: transparent; }
  .sel:disabled { opacity: .5; cursor: not-allowed; }
  .val { min-width: 0; flex: 1; }
  .val.ph { color: var(--muted); }
  svg { width: 14px; height: 14px; flex: none; fill: none; stroke: var(--muted); stroke-width: 2; transition: transform .12s; }
  .open svg { transform: rotate(180deg); stroke: var(--accent); }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .menu { position: fixed; z-index: 1000; margin: 0; padding: 4px; list-style: none; overflow-y: auto; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); box-shadow: 0 10px 30px rgba(0, 0, 0, .45); scrollbar-width: thin; scrollbar-color: var(--line) transparent; }
  li { display: flex; justify-content: space-between; align-items: center; gap: 10px; padding: 7px 10px 7px 12px; border-left: 2px solid transparent; border-radius: 0 var(--radius) var(--radius) 0; color: var(--text); cursor: pointer; font-size: 14px; }
  li.act { background: color-mix(in srgb, var(--line) 70%, transparent); color: var(--ink); }
  li.on { border-left-color: var(--accent); color: var(--ink); font-weight: 600; }
  li.dis { opacity: .4; cursor: not-allowed; }
  .hint { color: var(--muted); font-size: 11px; flex: none; max-width: 45%; }
  @media (prefers-reduced-motion: reduce) { svg { transition: none; } }
</style>
