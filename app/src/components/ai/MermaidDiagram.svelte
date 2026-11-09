<script lang="ts">
  // Draws one finished mermaid block. The SVG is shown through <img src="data:..."> so nothing in it can execute.
  import { onMount } from "svelte";
  import { renderMermaid } from "../../lib/mermaidrender";
  import { ai } from "../../lib/ai.svelte";
  import { prefs } from "../../lib/prefs.svelte";
  import { diagramAlt, diagramImgStyle, type MermaidResult, type ThemeColors } from "../../lib/mermaidsafe";

  let { src }: { src: string } = $props();
  let res = $state<MermaidResult | null>(null);
  const url = $derived(res?.ok ? res.url : null);
  const imgStyle = $derived(res?.ok ? diagramImgStyle(res.size) : "");
  const fail = $derived(res && !res.ok ? res : null);
  let showSrc = $state(false);
  let themeTick = $state(0);
  const alt = $derived(diagramAlt(src));

  function colors(): ThemeColors {
    const cs = getComputedStyle(document.documentElement);
    const v = (n: string, d: string) => cs.getPropertyValue(n).trim() || d;
    return { bg: v("--bg", "#0a0a0a"), text: v("--text", "#e6e6e6"), line: v("--muted", "#8f8f8f"), accent: v("--accent", "#2bd4bf"), surface: v("--surface", "#191919") };
  }

  onMount(() => {
    const bump = () => { themeTick++; };
    const mo = new MutationObserver(bump);
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const mq = window.matchMedia("(prefers-color-scheme: light)");
    mq.addEventListener("change", bump);
    return () => { mo.disconnect(); mq.removeEventListener("change", bump); };
  });

  $effect(() => {
    const s = src; themeTick;
    let live = true;
    renderMermaid(s, colors()).then((r) => { if (live) res = r; });
    return () => { live = false; };
  });
</script>

<figure class="dg">
  {#if fail}
    <p class="note">Could not draw this diagram.
      {#if fail.kind === "syntax"}
        <button type="button" class="tog" disabled={ai.streaming} onclick={() => void ai.send(`That diagram could not be drawn because its mermaid syntax was invalid (${fail.reason}). Please send it again with simpler, valid mermaid syntax.`)}>Ask MIRA to fix it</button>
      {/if}</p>
    <p class="note">Reason: {fail.reason}</p>
    {#if fail.kind === "app"}<p class="note">This is a MiraMail drawing problem, not your diagram.</p>{/if}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <pre class="mono selectable" tabindex="0" aria-label="Diagram source"><code>{src}</code></pre>
  {:else}
    {#if url}
      <!-- A-/A+ (prefs.chatScale) scales the diagram through the chat's zoom, like the text. The <img> has an explicit
           px size and is recreated at each step so the new size is laid out and painted fresh. -->
      <div class="pan">{#key prefs.v.chatScale}<img src={url} {alt} style={imgStyle} />{/key}</div>
    {:else}
      <p class="note" role="status">Drawing diagram…</p>
    {/if}
    <button type="button" class="tog" aria-expanded={showSrc} onclick={() => (showSrc = !showSrc)}>{showSrc ? "Hide source" : "Show source"}</button>
    {#if showSrc}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <pre class="mono selectable" tabindex="0" aria-label="Diagram source"><code>{src}</code></pre>
    {/if}
  {/if}
</figure>

<style>
  .dg { margin: 0; display: grid; gap: 6px; justify-items: start; min-width: 0; }
  /* Wide diagrams keep their readable size and scroll sideways instead of shrinking to fit the column. */
  .pan { max-width: 100%; overflow-x: auto; border: 1px solid var(--line); border-radius: var(--radius); background: var(--bg); }
  img { display: block; box-sizing: content-box; max-width: none; height: auto; padding: 8px; user-select: none; }
  .note { margin: 0; color: var(--muted); font-size: 0.8667rem; }
  .tog { font-size: 0.6667rem; text-transform: uppercase; letter-spacing: .1em; padding: 1px 6px; background: none; border: 1px solid var(--line); border-radius: var(--radius); color: var(--muted); }
  .tog:hover { color: var(--ink); border-color: var(--muted); }
  pre { margin: 0; padding: 10px 12px; width: 100%; background: color-mix(in srgb, var(--line) 40%, transparent); border: 1px solid var(--line); border-radius: var(--radius); overflow-x: auto; white-space: pre; }
  code { font-family: var(--font-mono); font-size: 0.85em; }
</style>
