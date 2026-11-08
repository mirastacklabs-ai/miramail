<script lang="ts">
  // Renders model Markdown as Svelte nodes only (no raw-HTML injection). Links are text plus an Open button behind a confirm.
  import { parseMarkdown, type Block, type Inline } from "../../lib/aimd";
  import { isDrawableMermaid } from "../../lib/mermaidsafe";
  import MermaidDiagram from "./MermaidDiagram.svelte";
  import { app } from "../../lib/state.svelte";
  import { openExternal } from "../../lib/api";

  let { text }: { text: string } = $props();
  const blocks = $derived(parseMarkdown(text));

  async function open(href: string) {
    const ok = await app.ask({
      title: "Open this link in your browser?",
      body: "The link was written by the AI model. Check the full address before you open it.",
      detail: href, ok: "Open",
    });
    if (ok) void openExternal(href).catch((e) => app.fail(e));
  }
</script>

{#snippet inl(nodes: Inline[])}
  {#each nodes as n, i (i)}
    {#if n.t === "text"}{n.v}
    {:else if n.t === "strong"}<strong>{@render inl(n.c)}</strong>
    {:else if n.t === "em"}<em>{@render inl(n.c)}</em>
    {:else if n.t === "code"}<code>{n.v}</code>
    {:else if n.t === "link"}<span class="lk">{@render inl(n.c)}</span> <button type="button" class="open" title={n.href} aria-label="Open link {n.href}" onclick={() => open(n.href)}>Open</button>
    {/if}
  {/each}
{/snippet}

{#snippet blks(list: Block[])}
  {#each list as b, i (i)}
    {#if b.t === "p"}<p>{@render inl(b.c)}</p>
    {:else if b.t === "h"}<p class="h"><strong>{@render inl(b.c)}</strong></p>
    {:else if b.t === "code" && isDrawableMermaid(b)}
      <MermaidDiagram src={b.v} />
    {:else if b.t === "code"}
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <pre class="mono selectable" tabindex="0" aria-label="Code{b.lang ? ` (${b.lang})` : ''}"><code>{b.v}</code></pre>
    {:else if b.t === "list"}
      {#if b.ordered}
        <ol>{#each b.items as it, j (j)}<li style="margin-left:{it.depth * 18}px">{@render inl(it.c)}</li>{/each}</ol>
      {:else}
        <ul>{#each b.items as it, j (j)}<li style="margin-left:{it.depth * 18}px">{@render inl(it.c)}</li>{/each}</ul>
      {/if}
    {:else if b.t === "table"}
      <div class="tw"><table>
        <thead><tr>{#each b.head as c, j (j)}<th>{@render inl(c)}</th>{/each}</tr></thead>
        <tbody>{#each b.rows as r, j (j)}<tr>{#each r as c, k (k)}<td>{@render inl(c)}</td>{/each}</tr>{/each}</tbody>
      </table></div>
    {:else if b.t === "quote"}<blockquote>{@render blks(b.c)}</blockquote>
    {:else if b.t === "hr"}<hr />
    {/if}
  {/each}
{/snippet}

<div class="md selectable">{@render blks(blocks)}</div>

<style>
  .md { display: grid; gap: 8px; font-size: 0.9333rem; line-height: 1.5; overflow-wrap: anywhere; min-width: 0; }
  p { margin: 0; white-space: pre-wrap; }
  .h { color: var(--ink); }
  ul, ol { margin: 0; padding-left: 20px; display: grid; gap: 2px; }
  code { font-family: var(--font-mono); font-size: 0.85em; background: color-mix(in srgb, var(--line) 55%, transparent); padding: 1px 4px; border-radius: 2px; }
  pre { margin: 0; padding: 10px 12px; background: color-mix(in srgb, var(--line) 40%, transparent); border: 1px solid var(--line); border-radius: var(--radius); overflow-x: auto; white-space: pre; }
  pre code { background: none; padding: 0; font-size: inherit; }
  blockquote { margin: 0; padding-left: 10px; border-left: 2px solid var(--line); color: var(--muted); display: grid; gap: 6px; }
  hr { border: 0; border-top: 1px solid var(--line); width: 100%; }
  .tw { overflow-x: auto; }
  table { border-collapse: collapse; font-size: 0.8667rem; }
  th, td { border: 1px solid var(--line); padding: 4px 8px; text-align: left; vertical-align: top; }
  th { color: var(--ink); background: color-mix(in srgb, var(--line) 40%, transparent); }
  .lk { color: var(--accent); text-decoration: underline; text-underline-offset: 2px; }
  .open { font-size: 0.6667rem; text-transform: uppercase; letter-spacing: .1em; padding: 1px 6px; background: none; border: 1px solid var(--line); border-radius: var(--radius); color: var(--muted); vertical-align: baseline; }
  .open:hover { color: var(--ink); border-color: var(--muted); }
</style>
