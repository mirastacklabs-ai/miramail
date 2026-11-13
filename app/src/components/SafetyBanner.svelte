<script lang="ts">
  import { thisDevice } from "../lib/device";
  import { app } from "../lib/state.svelte";
  import { addDismissed, allReasons, dismissKey, headline, loadDismissed, needsBanner, NONE_FOUND_TEXT, PRIVACY_NOTE, reasonTitle, removeDismissed, storeDismissed, topReasons } from "../lib/safety";

  // Warning signs found by checks that run on this Mac (no network). Plain text only: every string is rendered as
  // text by Svelte, never as HTML. "This is fine" only hides the notice for this message; it is stored in this
  // app's local storage, never sent anywhere, and never changes the message on the server.
  // The report is fetched by the app state when the message opens (lib/state.svelte.ts `runSafety`), because the
  // Reader's links wait for it too (lib/linkgate.ts).
  const m = $derived(app.body?.summary);
  const key = $derived(m ? dismissKey(m.account, m.folder, m.uidvalidity, m.uid) : "");
  const report = $derived(m ? app.safety : null);
  const pending = $derived(!!m && app.checkSafety === "pending");
  let fine = $state<string[]>(loadDismissed());
  let open = $state(false);
  $effect(() => { void key; open = false; });

  const dismissed = $derived(!!key && fine.includes(key));
  const top = $derived(report ? topReasons(report) : []);
  const rest = $derived(report ? allReasons(report) : []);
  function markFine() { fine = addDismissed(fine, key); storeDismissed(fine); open = false; }
  function undo() { fine = removeDismissed(fine, key); storeDismissed(fine); }
</script>

{#if pending && !report}
  <p class="calm busy" role="status" aria-live="polite"><span class="spin" aria-hidden="true"></span>Checking links and sender…</p>
{:else if report}
  {#if needsBanner(report)}
    {#if dismissed}
      <p class="note" role="status">You marked this message as fine. <button class="lnk" onclick={undo}>Show the warning again</button></p>
    {:else}
      <section class="safety" class:soft={report.level === "caution"} role="status" aria-label="Message safety check">
        <svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3l7 3v5c0 4.5-3 8-7 10-4-2-7-5.5-7-10V6l7-3z"/><path d="M12 8.5v4.2"/><circle cx="12" cy="15.8" r=".9" fill="currentColor"/></svg>
        <div class="body">
          <strong>{headline(report.level)}</strong>
          <ul class="top">
            {#each top as r (r.code)}
              <li><span class="t">{reasonTitle(r)}.</span> {r.detail}</li>
            {/each}
          </ul>
          <div class="acts">
            <button class="btn ghost sm" aria-expanded={open} aria-controls="safety-details" onclick={() => (open = !open)}>{open ? "Hide details" : "Show details"}</button>
            <button class="btn sm" onclick={markFine} title={`Hides this notice for this message. Stored only on ${thisDevice()}.`}>This is fine</button>
          </div>
          {#if open}
            <div id="safety-details" class="details">
              <ul>
                {#each rest as r (r.code)}
                  <li><span class="chip {r.severity}">{r.severity}</span> <span class="t">{reasonTitle(r)}.</span> {r.detail} <span class="code mono">{r.code}</span></li>
                {/each}
              </ul>
              <p class="small dim">{PRIVACY_NOTE} "This is fine" only hides this notice for this message, on {thisDevice()}.</p>
            </div>
          {/if}
        </div>
      </section>
    {/if}
  {:else}
    <p class="calm" role="status">{NONE_FOUND_TEXT}</p>
  {/if}
{/if}

<style>
  .safety { display: flex; gap: 12px; align-items: flex-start; border: 1px solid var(--line); border-left: 4px solid var(--signal); border-radius: var(--radius); padding: 12px 14px; background: var(--surface); }
  .safety > svg { flex: none; color: var(--signal); margin-top: 2px; }
  .safety.soft { border-left-color: var(--muted); }
  .safety.soft > svg { color: var(--muted); }
  .body { min-width: 0; flex: 1; }
  strong { font-size: 1rem; letter-spacing: .02em; }
  ul { list-style: disc; margin: 6px 0 0; padding: 0 0 0 18px; display: grid; gap: 4px; }
  li { font-size: 0.9rem; line-height: 1.5; color: var(--text); overflow-wrap: anywhere; }
  .t { font-weight: 600; }
  .acts { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 10px; }
  .details { margin-top: 10px; border-top: 1px solid var(--line); padding-top: 8px; }
  .chip { font-size: 0.6667rem; text-transform: uppercase; letter-spacing: .1em; padding: 1px 7px; border-radius: 999px; border: 1px solid var(--line); color: var(--muted); }
  .chip.high, .chip.critical { color: var(--signal); border-color: var(--signal); }
  .chip.medium { border-color: #d9a21b; }
  .code { color: var(--muted); font-size: 0.7333rem; margin-left: 4px; }
  .small { font-size: 0.8333rem; line-height: 1.5; margin: 8px 0 0; }
  .dim { color: var(--muted); }
  .calm, .note { margin: 0; font-size: 0.8333rem; color: var(--muted); }
  .busy { display: flex; align-items: center; gap: 8px; }
  .spin { width: 12px; height: 12px; flex: none; border: 2px solid var(--line); border-top-color: var(--accent); border-radius: 50%; animation: rot .8s linear infinite; }
  @keyframes rot { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .spin { animation-duration: 3s; } }
  .lnk { background: none; border: 0; padding: 0; color: var(--accent); text-decoration: underline; cursor: pointer; font: inherit; }
</style>
