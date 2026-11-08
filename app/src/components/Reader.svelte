<script lang="ts">
  import { api, openExternal } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { forwardMessage, replyTo } from "../lib/compose";
  import { prefs } from "../lib/prefs.svelte";
  import { bodyBaseCss } from "../lib/prefs-model";
  import { addrFull, addrLabel, buildSrcdoc, extractLinks, fmtSize, fullDate } from "../lib/util";
  import type { Attachment } from "../lib/types";
  import { bannerText, imageStatus, plural, sizeLabel, summarize, warningFor } from "../lib/trackers";
  import InviteCard from "./InviteCard.svelte";
  import ReaderMore from "./ReaderMore.svelte";
  import ContactCard from "./ContactCard.svelte";
  import UnsubBar from "./UnsubBar.svelte";
  import ImageAllow from "./ImageAllow.svelte";
  import Attachments from "./Attachments.svelte";
  import PrintSheet from "./PrintSheet.svelte";
  import ThreadStrip from "./ThreadStrip.svelte";

  const b = $derived(app.body);
  const sel = $derived(app.selected);
  let mode = $state<"html" | "text">("html");
  let lastUid = -1;
  $effect(() => {
    const u = b?.summary.uid ?? -1;
    if (u !== lastUid) { lastUid = u; mode = "html"; }
  });

  const hasHtml = $derived(!!b?.html);
  const showHtml = $derived(hasHtml && mode === "html");
  const srcdoc = $derived(b?.html ? buildSrcdoc(b.html, app.imagesAllowed, bodyBaseCss(prefs.v)) : "");
  const links = $derived(b ? extractLinks(b.html) : []);
  const flagged = $derived(sel?.flags.flagged ?? false);

  /**
   * Size the message frame to its content so the READER pane is the only scroller (a nested scroller inside the
   * frame stopped answering two-finger scrolling after the pane was resized). `allow-same-origin` lets us read the
   * document height; scripts stay off twice over (no `allow-scripts`, and the srcdoc CSP is `default-src 'none'`).
   */
  function fit(node: HTMLIFrameElement) {
    let ro: ResizeObserver | undefined;
    const measure = () => {
      const d = node.contentDocument;
      if (!d?.documentElement) return;
      const h = Math.min(Math.max(d.documentElement.scrollHeight, 80), 40000);
      if (Math.abs(node.offsetHeight - h) > 1) node.style.height = `${h}px`;
    };
    const onload = () => {
      ro?.disconnect();
      const d = node.contentDocument;
      if (!d?.documentElement) return;
      measure();
      ro = new ResizeObserver(measure);
      ro.observe(d.documentElement);
      if (d.body) ro.observe(d.body);
      ro.observe(node);
    };
    node.addEventListener("load", onload);
    return { destroy() { node.removeEventListener("load", onload); ro?.disconnect(); } };
  }

  async function openLink(url: string) {
    const ok = await app.ask({ title: "Open link?", body: "This will open the address below in your default browser.", detail: url, ok: "Open" });
    if (!ok) return;
    try { await openExternal(url); } catch (e) { app.fail(e); }
  }
  async function download(a: Attachment) {
    if (!app.account || !b) return;
    try {
      const p = await api.saveAttachmentToDownloads(app.account, b.summary.folder, b.summary.uid, a.id);
      app.notify(`Saved to ${p}`);
    } catch (e) { app.fail(e); }
  }
  const imgs = $derived(b?.images ?? []);
  const sum = $derived(summarize(imgs));
  const warning = $derived(warningFor(sum));
  const showPrivacy = $derived(hasHtml && mode === "html" && imgs.length > 0);
  const loadSafe = () => b && app.open(b.summary.uid, true, true);
  async function loadAll() {
    if (!b) return;
    if (sum.trackers + sum.suspicious > 0) {
      const ok = await app.ask({
        title: "Load tracking images?",
        body: `The sender will learn you opened this message, when, your IP address and device. ${plural(sum.trackers + sum.suspicious, "image")} look${sum.trackers + sum.suspicious === 1 ? "s" : ""} like trackers or could be tracking. This detection is a heuristic.`,
        ok: "Load anyway", danger: true,
      });
      if (!ok) return;
    }
    void app.open(b.summary.uid, true, false);
  }
</script>

<section class="reader" aria-label="Reading pane">
  {#if app.bodyLoading && !b}
    <div class="pad">
      <div class="skeleton" style="height:22px;width:60%"></div>
      <div class="skeleton" style="height:12px;width:35%;margin-top:14px"></div>
      <div class="skeleton" style="height:12px;width:45%;margin-top:8px"></div>
      <div class="skeleton" style="height:180px;width:100%;margin-top:28px"></div>
    </div>
  {:else if !b}
    <div class="blank">
      <svg viewBox="0 0 24 18" width="48" aria-hidden="true" opacity=".6"><rect width="24" height="18" rx="4" fill="#2bd4bf"/><path d="M3.5 4.5 L12 11 L20.5 4.5" fill="none" stroke="#0a0a0a" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/></svg>
      <strong>No message selected</strong>
      <span>Pick a message from the list. <span class="mono">j</span>/<span class="mono">k</span> to move, <span class="mono">Enter</span> to open, <span class="mono">c</span> to compose.</span>
    </div>
  {:else}
    <div class="bar" role="toolbar" aria-label="Message actions">
      <button class="btn ghost sm" onclick={() => replyTo(false)}>Reply</button>
      <button class="btn ghost sm" onclick={() => replyTo(true)}>Reply all</button>
      <button class="btn ghost sm" onclick={() => replyTo(false, true)}>Forward</button>
      <button class="btn ghost sm" onclick={() => forwardMessage("attach")} title="Forward the whole message as an .eml attachment">Forward as attachment</button>
      <span class="sep"></span>
      <button class="btn ghost sm" onclick={() => app.moveTo("archive")}>Archive</button>
      <button class="btn ghost sm" onclick={() => app.moveTo("trash")}>Trash</button>
      <button class="btn ghost sm" aria-pressed={flagged} onclick={() => app.setFlag("flagged", !flagged)}>{flagged ? "Unflag" : "Flag"}</button>
      <button class="btn ghost sm" onclick={() => app.setFlag("seen", false)}>Mark unread</button>
      <span class="grow"></span>
      <ReaderMore />
      {#if hasHtml}
        <div class="seg" role="group" aria-label="Message view">
          <button class:on={mode === "html"} aria-pressed={mode === "html"} onclick={() => (mode = "html")}>Formatted</button>
          <button class:on={mode === "text"} aria-pressed={mode === "text"} onclick={() => (mode = "text")}>Plain text</button>
        </div>
      {/if}
      <button class="btn danger sm" onclick={() => app.deleteForever()}>Delete forever</button>
    </div>

    <div class="scroll">
      <ThreadStrip account={b.summary.account} folder={b.summary.folder} uid={b.summary.uid} />
      <header class="hd selectable">
        <h1>{b.summary.subject || "(no subject)"}</h1>
        <dl class="mono meta">
          <dt>From</dt><dd>{b.summary.from ? addrFull(b.summary.from) : "Unknown sender"}{#if b.summary.from}<ContactCard name={b.summary.from.name} address={b.summary.from.address} />{/if}</dd>
          <dt>To</dt><dd>{b.summary.to.map(addrFull).join(", ") || "—"}</dd>
          {#if b.cc.length}<dt>Cc</dt><dd>{b.cc.map(addrFull).join(", ")}</dd>{/if}
          <dt>Date</dt><dd>{fullDate(b.summary.date)}</dd>
        </dl>
      </header>

      <UnsubBar />

      {#if b.invite}<InviteCard invite={b.invite} uid={b.summary.uid} folder={b.summary.folder} />{/if}

      {#if showPrivacy}
        <div class="privacy" aria-live="polite">
          {#if warning && !app.imagesAllowed}
            <div class="warn" class:soft={warning.level === "suspicious"} role="status">
              <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3l7 3v5c0 4.5-3 8-7 10-4-2-7-5.500-7-10V6l7-3z"/><path d="M8.500 12s1.500-2.500 3.500-2.500 3.500 2.500 3.500 2.500-1.500 2.500-3.500 2.500S8.500 12 8.500 12z"/><circle cx="12" cy="12" r=".9" fill="currentColor"/></svg>
              <div><strong>{warning.headline}</strong><p>{warning.body}</p></div>
            </div>
          {/if}

          <ImageAllow />

          {#if !app.imagesAllowed}
            {#if b.blockedRemoteCount > 0}
              <div class="banner">
                <span>{bannerText(b.blockedRemoteCount, sum)}</span>
                <span class="acts">
                  <button class="btn sm" onclick={loadSafe}>{sum.trackers > 0 ? "Load safe images" : "Load images"}</button>
                  {#if sum.trackers + sum.suspicious > 0}
                    <button class="btn ghost sm" onclick={loadAll}>Load all, including trackers</button>
                  {/if}
                </span>
              </div>
            {/if}
          {:else if app.skipTrackers && sum.trackers > 0}
            <p class="note"><span class="dot"></span>{plural(sum.trackers, "tracker")} blocked. The other images were loaded.</p>
          {:else if !app.skipTrackers && sum.trackers > 0}
            <p class="note"><span class="dot"></span>Tracking images were loaded, so the sender may know you opened this message.</p>
          {/if}

          {#if sum.trackers + sum.suspicious === 0}
            <p class="calm">No trackers detected (heuristic)</p>
          {/if}

          <details class="imgs">
            <summary class="label">Image details ({imgs.length})</summary>
            <ul>
              {#each imgs as im, n (n)}
                <li class="im">
                  <div class="row1">
                    <span class="chip {im.verdict}">{im.verdict}</span>
                    <span class="mono ell" title={im.url}>{im.host}</span>
                    <span class="mono dim">{sizeLabel(im)}{im.hidden ? ", hidden" : ""}</span>
                    <span class="status">{imageStatus(im, app.imagesAllowed, app.skipTrackers)}</span>
                  </div>
                  <div class="row2">{#if im.tracker}<strong>{im.tracker}. </strong>{/if}{im.reasons.join("; ")}</div>
                </li>
              {/each}
            </ul>
            <p class="dim small">Detection is heuristic: it looks at the image address and size, and cannot see what the sender's server does.</p>
          </details>
          <details class="explain">
            <summary class="label">What is a tracking pixel?</summary>
            <p class="small">A tracking pixel is a tiny or invisible image hosted by the sender or a mailing service.</p>
            <p class="small">When your mail app loads it, the sender's server logs that you opened the message, when, your IP address and device.</p>
            <p class="small">Blocking remote images stops this. Loading only safe images skips the ones that look like trackers.</p>
          </details>
        </div>
      {/if}

      {#if showHtml}
        <!-- Keyed on the content: swapping `srcdoc` on a live frame sometimes left the OLD document showing (images never appeared after "Show images"); a fresh frame per content cannot. -->
        <div class="card">{#key srcdoc}<iframe title="Message content" sandbox="allow-same-origin" use:fit {srcdoc}></iframe>{/key}</div>
      {:else}
        <pre class="plain selectable">{b.text ?? "(This message has no readable text.)"}</pre>
      {/if}

      {#if links.length}
        <details class="links">
          <summary class="label">Links in this message ({links.length})</summary>
          <ul>
            {#each links as l (l.url)}
              <li><button class="lk" onclick={() => openLink(l.url)} title={l.url}><span class="mono ell">{l.url}</span>{#if l.label && l.label !== l.url}<span class="lbl ell">{l.label}</span>{/if}</button></li>
            {/each}
          </ul>
        </details>
      {/if}

      {#if b.attachments.length}<Attachments {b} />{/if}
    </div>
  {/if}
</section>
<PrintSheet plain={mode === "text"} />

<style>
  .reader { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; background: var(--bg); }
  .pad { padding: 32px; }
  .blank { margin: auto; display: grid; gap: 10px; justify-items: center; text-align: center; color: var(--muted); max-width: 320px; font-size: 0.9333rem; }
  .blank strong { color: var(--ink); text-transform: uppercase; letter-spacing: .12em; font-weight: 600; }
  .bar { display: flex; gap: 6px; align-items: center; padding: 10px 20px; border-bottom: 1px solid var(--line); flex-wrap: wrap; }
  .bar .sep { width: 1px; height: 18px; background: var(--line); margin: 0 4px; }
  .grow { flex: 1; }
  .scroll { overflow-y: auto; overflow-x: hidden; flex: 1; min-height: 0; padding: 24px 28px 40px; display: flex; flex-direction: column; gap: 16px; }
  h1 { font-size: 1.3333rem; letter-spacing: .03em; line-height: 1.25; margin-bottom: 12px; overflow-wrap: anywhere; }
  .meta { display: grid; grid-template-columns: max-content 1fr; gap: 4px 14px; margin: 0; color: var(--text); }
  .meta dt { color: var(--muted); text-transform: uppercase; letter-spacing: .1em; font-family: var(--font-ui); font-size: 0.7333rem; padding-top: 1px; }
  .meta dd { margin: 0; overflow-wrap: anywhere; }
  .privacy { display: grid; gap: 10px; }
  .warn { display: flex; gap: 12px; align-items: flex-start; border: 1px solid var(--line); border-left: 4px solid var(--signal); border-radius: var(--radius); padding: 12px 14px; background: var(--surface); }
  .warn svg { flex: none; color: var(--signal); margin-top: 2px; }
  .warn.soft { border-left-color: var(--muted); }
  .warn.soft svg { color: var(--muted); }
  .warn strong { font-size: 1rem; letter-spacing: .02em; }
  .warn p { margin: 4px 0 0; font-size: 0.9rem; line-height: 1.5; color: var(--text); }
  .acts { display: inline-flex; gap: 8px; flex-wrap: wrap; justify-content: flex-end; }
  .note, .calm { margin: 0; font-size: 0.8333rem; color: var(--muted); display: flex; align-items: center; gap: 8px; }
  .note .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--signal); flex: none; }
  .calm { color: var(--accent); }
  .imgs summary, .explain summary { cursor: pointer; margin-bottom: 6px; }
  .im { border: 1px solid var(--line); border-radius: var(--radius); padding: 6px 10px; display: grid; gap: 3px; }
  .row1 { display: flex; gap: 10px; align-items: center; min-width: 0; }
  .row1 .ell { flex: 1; }
  .row2 { font-size: 0.8rem; color: var(--muted); overflow-wrap: anywhere; }
  .row2 strong { color: var(--text); font-weight: 600; }
  .status { font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .08em; color: var(--muted); flex: none; }
  .chip { font-size: 0.6667rem; text-transform: uppercase; letter-spacing: .1em; padding: 1px 7px; border-radius: 999px; border: 1px solid; flex: none; }
  .chip.tracker { color: var(--signal); border-color: var(--signal); background: color-mix(in srgb, var(--signal) 14%, transparent); }
  .chip.suspicious { color: var(--muted); border-color: #d9a21b; }
  .chip.clean { color: var(--accent); border-color: var(--accent); }
  .dim { color: var(--muted); }
  .small { font-size: 0.8333rem; line-height: 1.5; margin: 4px 0; }
  .banner { display: flex; justify-content: space-between; align-items: center; gap: 12px; border: 1px solid var(--line); border-left: 2px solid var(--signal); border-radius: var(--radius); padding: 8px 12px; font-size: 0.9333rem; background: var(--surface); }
  .seg { display: inline-flex; flex: none; white-space: nowrap; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  .seg button { background: none; border: 0; flex: none; line-height: 1.4; padding: 5px 12px; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .12em; color: var(--muted); border-bottom: 2px solid transparent; }
  .seg button.on { color: var(--ink); border-bottom-color: var(--accent); }
  .scroll > * { flex-shrink: 0; }
  .card { background: #fff; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  iframe { display: block; width: 100%; height: 56vh; min-height: 80px; border: 0; background: #fff; }
  .plain { margin: 0; font: 400 var(--body-size)/var(--body-lh) var(--body-font); white-space: pre-wrap; overflow-wrap: anywhere; color: var(--text); }
  .links summary { cursor: pointer; margin-bottom: 8px; }
  ul { list-style: none; margin: 0; padding: 0; display: grid; grid-template-columns: minmax(0, 1fr); gap: 4px; }
  li { min-width: 0; }
  .links { min-width: 0; max-width: 100%; }
  .lk { display: flex; flex-direction: column; gap: 2px; align-items: stretch; min-width: 0; max-width: 100%; background: none; border: 1px solid var(--line); border-radius: var(--radius); padding: 6px 10px; width: 100%; text-align: left; color: var(--accent); }
  .lk:hover, .att:hover { border-color: var(--muted); }
  .lbl { color: var(--muted); font-size: 0.8rem; display: block; max-width: 100%; }
  .lk .mono { display: block; max-width: 100%; }
  .att { display: flex; align-items: center; gap: 10px; border: 1px solid var(--line); background: var(--surface); border-radius: var(--radius); padding: 7px 12px; color: var(--text); max-width: 420px; width: 100%; text-align: left; }
  .sz { color: var(--muted); margin-left: auto; flex: none; }
  .ell { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
</style>
