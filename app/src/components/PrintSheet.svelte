<script lang="ts">
  // Print-only copy of the open message: header block plus the sanitised body. It lives directly under <body>,
  // off-screen, and the @media print rules below hide the rest of the app. Remote images are never loaded here
  // (the iframe's CSP is built with remote images off, so print never contacts a sender).
  import { tick } from "svelte";
  import { app } from "../lib/state.svelte";
  import { org } from "../lib/org.svelte";
  import { orgApi } from "../lib/orgapi";
  import { isTauri } from "../lib/api";
  import { addrFull, buildSrcdoc, fullDate } from "../lib/util";

  let { plain = false }: { plain?: boolean } = $props();
  let active = $state(false);
  let seen = 0;
  let frame = $state<HTMLIFrameElement>();
  let height = $state(0);
  const b = $derived(app.body);
  const useHtml = $derived(!!b?.html && !plain);
  const srcdoc = $derived(b?.html ? buildSrcdoc(b.html, false, "body{margin:0;padding:0;font:13px/1.45 -apple-system,system-ui,sans-serif;overflow-wrap:anywhere}") : "");

  $effect(() => {
    const n = org.printing;
    if (n === seen) return;
    seen = n;
    if (n > 0 && app.body) { height = 0; active = true; if (!(!!app.body.html && !plain)) void tick().then(go); }
  });
  // A different message replaces the sheet.
  $effect(() => { void b?.summary.uid; active = false; });

  function measure() {
    const d = frame?.contentDocument;
    height = d ? Math.max(d.documentElement.scrollHeight, d.body?.scrollHeight ?? 0) + 8 : 600;
    void tick().then(() => requestAnimationFrame(() => requestAnimationFrame(go)));
  }
  async function go() {
    try { if (isTauri) await orgApi.printPage(); else window.print(); }
    catch { try { window.print(); } catch (e) { app.fail(e); } }
  }
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy() { node.remove(); } };
  }
</script>

{#if active && b}
  <div class="print-root" use:portal aria-hidden="true">
    <h1>{b.summary.subject || "(no subject)"}</h1>
    <table>
      <tbody>
        <tr><th>From</th><td>{b.summary.from ? addrFull(b.summary.from) : "Unknown sender"}</td></tr>
        <tr><th>To</th><td>{b.summary.to.map(addrFull).join(", ") || "-"}</td></tr>
        {#if b.cc.length}<tr><th>Cc</th><td>{b.cc.map(addrFull).join(", ")}</td></tr>{/if}
        <tr><th>Date</th><td>{fullDate(b.summary.date)}</td></tr>
        {#if b.attachments.length}<tr><th>Attached</th><td>{b.attachments.map((a) => a.name).join(", ")}</td></tr>{/if}
      </tbody>
    </table>
    <hr />
    {#if useHtml}
      <iframe bind:this={frame} title="Message for printing" sandbox="allow-same-origin" {srcdoc} style="height:{height || 600}px" onload={measure}></iframe>
    {:else}
      <pre>{b.text ?? "(This message has no readable text.)"}</pre>
    {/if}
  </div>
{/if}

<style>
  :global(.print-root) { position: fixed; left: -10000px; top: 0; width: 720px; background: #fff; color: #000; font: 13px/1.4 -apple-system, system-ui, sans-serif; z-index: -1; }
  :global(.print-root h1) { font-size: 18px; margin: 0 0 8px; }
  :global(.print-root table) { border-collapse: collapse; font-size: 12px; }
  :global(.print-root th) { text-align: left; color: #555; padding: 1px 12px 1px 0; vertical-align: top; font-weight: 600; }
  :global(.print-root td) { padding: 1px 0; overflow-wrap: anywhere; }
  :global(.print-root hr) { border: 0; border-top: 1px solid #bbb; margin: 10px 0; }
  :global(.print-root iframe) { display: block; width: 100%; border: 0; background: #fff; }
  :global(.print-root pre) { white-space: pre-wrap; overflow-wrap: anywhere; font: 12px/1.45 ui-monospace, Menlo, monospace; margin: 0; }
  @media print {
    :global(body > *:not(.print-root)) { display: none !important; }
    :global(html), :global(body) { background: #fff !important; height: auto !important; overflow: visible !important; }
    :global(.print-root) { position: static !important; left: auto !important; width: auto !important; z-index: auto !important; }
  }
</style>
