<script lang="ts">
  import { app } from "../../lib/state.svelte";
  import { phone } from "../../lib/phone.svelte";
  import { forwardMessage, replyTo } from "../../lib/compose";
  import { snoozeUi } from "../../lib/t2.svelte";
  import { org } from "../../lib/org.svelte";
  import { readerView } from "../../lib/readerview.svelte";
  import { orgApi } from "../../lib/orgapi";
  import { buildPrintHtml } from "../../lib/printdoc";
  import Reader from "../Reader.svelte";
  let more = $state(false);
  const sel = $derived(app.selected);
  const role = $derived(app.folders.find((f) => f.name === app.folder)?.role);
  const label = $derived(role && role !== "other" ? role[0].toUpperCase() + role.slice(1) : "Back");
  const remove = (r: "archive" | "trash") => phone.afterRemoval(() => app.moveTo(r));
  const sender = $derived(app.body?.summary.from?.address);
  const hasHtml = $derived(!!app.body?.html);
  async function printIt() {
    const b = app.body; if (!b) return;
    try { await orgApi.printMessage(buildPrintHtml(b, readerView.mode !== "html"), b.summary.subject || ""); } catch (e) { app.fail(e); }
  }
  async function shareEml() {
    const m = app.body?.summary; if (!m) return;
    try { await orgApi.shareEml(m.account, m.folder, m.uid); } catch (e) { app.fail(e); }
  }
  const run = (f: () => void | Promise<void>) => { more = false; void f(); };
</script>
<section class="screen">
  <header class="bar">
    <button class="back" onclick={() => phone.back()} aria-label="Back to {label}"><svg viewBox="0 0 24 24"><path d="M15 6l-6 6 6 6"/></svg><span>{label}</span></button>
  </header>
  <div class="body"><Reader /></div>
  {#if app.body}
  <div class="tools" role="toolbar" aria-label="Message actions">
    <button onclick={() => replyTo(false)}><svg viewBox="0 0 24 24"><path d="M10 7L4 12l6 5M4 12h9a7 7 0 017 7"/></svg><span>Reply</span></button>
    <button onclick={() => replyTo(true)}><svg viewBox="0 0 24 24"><path d="M8 7l-5 5 5 5M13 7l-5 5 5 5M8 12h6a6 6 0 016 6"/></svg><span>All</span></button>
    <button onclick={() => replyTo(false, true)}><svg viewBox="0 0 24 24"><path d="M14 7l6 5-6 5M20 12h-9a7 7 0 00-7 7"/></svg><span>Forward</span></button>
    <button onclick={() => remove("archive")}><svg viewBox="0 0 24 24"><path d="M3 5h18v4H3zM5 9v10h14V9M10 13h4"/></svg><span>Archive</span></button>
    <button onclick={() => remove("trash")}><svg viewBox="0 0 24 24"><path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/></svg><span>Trash</span></button>
    <button aria-haspopup="menu" aria-expanded={more} onclick={() => (more = !more)}><svg viewBox="0 0 24 24"><circle cx="6" cy="12" r="1.4"/><circle cx="12" cy="12" r="1.4"/><circle cx="18" cy="12" r="1.4"/></svg><span>More</span></button>
  </div>
  {/if}
  {#if more && app.body}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="scrim" onclick={() => (more = false)}></div>
    <div class="sheet" role="menu">
      <button role="menuitem" onclick={() => run(() => void app.setFlag("flagged", !(sel?.flags.flagged ?? false)))}>{sel?.flags.flagged ? "Unflag" : "Flag"}</button>
      <button role="menuitem" onclick={() => run(async () => { await app.setFlag("seen", false); phone.back(); })}>Mark unread</button>
      {#if role !== "trash"}<button role="menuitem" onclick={() => run(() => snoozeUi.open(app.body!.summary.uid))}>Snooze…</button>{/if}
      <button role="menuitem" onclick={() => run(() => void forwardMessage("attach"))}>Forward as attachment</button>
      {#if role === "junk"}<button role="menuitem" onclick={() => run(() => void phone.afterRemoval(() => app.moveTo("inbox")))}>Not junk</button>
      {:else if role !== "trash"}<button role="menuitem" onclick={() => run(() => void phone.afterRemoval(() => app.moveTo("junk")))}>Mark as junk</button>{/if}
      {#if hasHtml}<button role="menuitem" onclick={() => run(() => { readerView.mode = readerView.mode === "html" ? "text" : "html"; })}>{readerView.mode === "html" ? "Show plain text" : "Show formatted"}</button>{/if}
      {#if sender}
        {#if org.senderAllowed(sender)}<button role="menuitem" onclick={() => run(() => org.disallowSender(sender))}>Stop auto-loading images from {sender}</button>
        {:else}<button role="menuitem" onclick={() => run(() => { org.allowSender(sender); void app.open(app.body!.summary.uid, true, true); })}>Always load images from {sender}</button>{/if}
      {/if}
      <button role="menuitem" onclick={() => run(() => void printIt())}>Print…</button>
      <button role="menuitem" onclick={() => run(() => void shareEml())}>Share as .eml…</button>
      <button role="menuitem" onclick={() => run(() => void org.viewSource("headers"))}>View full headers</button>
      <button role="menuitem" onclick={() => run(() => void org.viewSource("raw"))}>View raw source</button>
      <button role="menuitem" class="danger" onclick={() => run(() => phone.afterRemoval(() => app.deleteForever()))}>Delete forever</button>
      <button role="menuitem" class="cancel" onclick={() => (more = false)}>Cancel</button>
    </div>
  {/if}
</section>
<style>
  .screen { flex: 1; min-height: 0; display: flex; flex-direction: column; position: relative; }
  .bar { flex: none; display: flex; align-items: center; padding: 6px 4px; min-height: 52px; border-bottom: 1px solid var(--line); background: var(--bg-band); }
  .back { min-height: 44px; min-width: 44px; display: inline-flex; align-items: center; background: none; border: 0; color: var(--accent); font-size: 0.95rem; padding: 0 8px 0 2px; }
  .back svg { width: 22px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .body { flex: 1; min-height: 0; display: flex; }
  .tools { flex: none; display: grid; grid-template-columns: repeat(6, 1fr); border-top: 1px solid var(--line); background: var(--bg-band); padding-bottom: env(safe-area-inset-bottom); }
  .tools button { min-height: 52px; background: none; border: 0; color: var(--text); display: grid; place-items: center; align-content: center; gap: 2px; padding: 4px 0; }
  .tools svg { width: 22px; height: 22px; fill: none; stroke: currentColor; stroke-width: 1.7; stroke-linecap: round; stroke-linejoin: round; }
  .tools span { font-size: 0.65rem; text-transform: uppercase; letter-spacing: .06em; color: var(--muted); }
  .scrim { position: absolute; inset: 0; z-index: 30; background: color-mix(in srgb, #000 45%, transparent); }
  .sheet { max-height: 80%; overflow-y: auto; overscroll-behavior: contain; position: absolute; left: 8px; right: 8px; bottom: calc(env(safe-area-inset-bottom) + 8px); z-index: 31; background: var(--surface); border: 1px solid var(--line); border-radius: 10px; display: grid; align-content: start; }
  .sheet button { min-height: 52px; background: none; border: 0; border-bottom: 1px solid var(--line); color: var(--ink); font-size: 1rem; text-align: center; }
  .sheet button { overflow-wrap: anywhere; padding: 8px 14px; }
  .sheet button.danger { color: var(--danger); }
  .sheet button.cancel { color: var(--accent); border-bottom: 0; font-weight: 600; }
</style>
