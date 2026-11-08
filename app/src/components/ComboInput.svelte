<script lang="ts">
  // Branded text input with suggestions (replaces the native datalist, whose popup macOS draws itself).
  // Comma-separated: suggestions complete the last item. Free text is always allowed.
  let { value = $bindable(""), options, id, placeholder, maxlength, ariaLabel }: {
    value: string; options: string[]; id?: string; placeholder?: string; maxlength?: number; ariaLabel?: string;
  } = $props();

  const uid = `cmb-${Math.random().toString(36).slice(2, 9)}`;
  let open = $state(false);
  let active = $state(-1);
  let inp = $state<HTMLInputElement>();
  let pos = $state({ left: 0, top: 0, width: 0 });

  const cut = $derived(value.lastIndexOf(","));
  const token = $derived(value.slice(cut + 1).trim().toLowerCase());
  const taken = $derived(new Set(value.slice(0, cut < 0 ? 0 : cut).split(",").map((s) => s.trim().toLowerCase()).filter(Boolean)));
  const matches = $derived(options.filter((o) => !taken.has(o.toLowerCase()) && o.toLowerCase() !== token && o.toLowerCase().includes(token)).slice(0, 8));
  const shown = $derived(open && matches.length > 0);

  function place() {
    if (!inp) return;
    const r = inp.getBoundingClientRect();
    pos = { left: r.left, top: r.bottom + 2, width: r.width };
  }
  function show() { place(); open = true; }
  function pick(i: number) {
    const o = matches[i];
    if (o === undefined) return;
    const head = cut < 0 ? "" : value.slice(0, cut + 1) + " ";
    value = head + o;
    open = false; active = -1;
    inp?.focus();
  }
  function key(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!shown) { show(); active = 0; } else active = (active + 1) % matches.length;
    } else if (e.key === "ArrowUp" && shown) {
      e.preventDefault(); active = active <= 0 ? matches.length - 1 : active - 1;
    } else if (e.key === "Enter" && shown && active >= 0) {
      e.preventDefault(); pick(active);
    } else if (e.key === "Escape" && shown) {
      e.preventDefault(); e.stopPropagation(); open = false; active = -1;
    } else if (e.key === "Tab") open = false;
  }
</script>

<svelte:window onresize={() => open && place()} />

<input
  bind:this={inp} {id} bind:value {placeholder} {maxlength}
  role="combobox" aria-autocomplete="list" aria-expanded={shown} aria-controls="{uid}-list" aria-label={ariaLabel}
  aria-activedescendant={shown && active >= 0 ? `${uid}-${active}` : undefined}
  autocomplete="off"
  oninput={() => { show(); active = -1; }}
  onfocus={show}
  onblur={() => (open = false)}
  onkeydown={key}
/>
{#if shown}
  <ul id="{uid}-list" class="menu" role="listbox" aria-label={ariaLabel} style="left:{pos.left}px; top:{pos.top}px; width:{pos.width}px">
    {#each matches as o, i (o)}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <li id="{uid}-{i}" role="option" aria-selected={i === active} class:act={i === active} onmousedown={(e) => { e.preventDefault(); pick(i); }} onpointerenter={() => (active = i)}>{o}</li>
    {/each}
  </ul>
{/if}

<style>
  .menu { position: fixed; z-index: 1000; margin: 0; padding: 4px; list-style: none; max-height: 220px; overflow-y: auto; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); box-shadow: 0 10px 30px rgba(0, 0, 0, .45); }
  li { padding: 7px 12px; border-left: 2px solid transparent; border-radius: 0 var(--radius) var(--radius) 0; color: var(--text); cursor: pointer; font-size: 14px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  li.act { background: color-mix(in srgb, var(--line) 70%, transparent); color: var(--ink); border-left-color: var(--accent); }
</style>
