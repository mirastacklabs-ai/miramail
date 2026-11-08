<script lang="ts">
  // The Rules section of AI Preferences. Works without any AI provider: rules are local and run without a model.
  import { onMount } from "svelte";
  import RuleEditor from "./RuleEditor.svelte";
  import Select from "../Select.svelte";
  import { auto } from "../../lib/aiauto.svelte";
  import { app } from "../../lib/state.svelte";
  import { AUTOMATION_NOTICE, dryRunText, runStateText } from "../../lib/airules";
  import type { Rule } from "../../lib/aitypes";

  onMount(() => { void auto.load(true); });

  let editing = $state<"new" | number | null>(null);
  let account = $state(app.account || app.accounts[0]?.email || "");
  const acctOpts = $derived(app.accounts.map((a) => ({ value: a.email, label: a.email })));
  const editRule = $derived(typeof editing === "number" ? auto.rules.find((r) => r.id === editing) ?? null : null);
  const when = (t: number | null) => (t ? new Date(t * 1000).toLocaleString() : "never");

  async function toggle(r: Rule) {
    if (r.enabled) { await auto.setEnabled(r.id, false); return; }
    // Show the preview first: turning a rule on is the decision, so make it an informed one.
    const c = await auto.card(r.id, true);
    const ok = await app.ask({
      title: `Turn on "${r.name}"?`,
      body: `${r.description}\n\n${dryRunText(c?.dryRun ?? null)}\n\nThe rule acts only on mail that arrives from now on, and every run can be undone from this page. ${AUTOMATION_NOTICE}`,
      ok: "Turn on",
    });
    if (ok) await auto.setEnabled(r.id, true);
  }
  // "Apply to existing messages": show how many cached messages would change, then run once. Undo is per run.
  async function applyNow(r: Rule) {
    if (auto.state.rulesPaused) { app.notify("Rules are paused. Turn off \"Pause all rules\" first.", "error"); return; }
    let n: Awaited<ReturnType<typeof auto.previewExisting>>;
    try { n = await auto.previewExisting(r.id); } catch (e) { app.fail(e); return; }
    if (n.matched === 0) { app.notify(n.scanned ? `None of the ${n.scanned} cached messages match this rule.` : "There is no cached mail in the folders this rule covers."); return; }
    const ok = await app.ask({
      title: `Apply "${r.name}" to existing messages?`,
      body: `${r.description}\n\n${n.matched} of the ${n.scanned} newest cached messages in these folders match and will be changed. This is a one-time run: it is listed under Recent runs and can be undone there. Nothing is ever deleted for good.`,
      ok: `Apply to ${n.matched}`,
    });
    if (ok) await auto.applyExisting(r.id);
  }
  async function del(r: Rule) {
    const ok = await app.ask({ title: `Delete "${r.name}"?`, body: "The rule is removed. Mail it already moved stays where it is; the log of its runs is kept for 90 days.", ok: "Delete", danger: true });
    if (ok) await auto.remove(r.id);
  }
</script>

<section class="rules" aria-labelledby="rules-h">
  <h3 id="rules-h">Rules</h3>
  <p class="note">Local rules sort new mail: move, archive, trash, mark read, flag or colour it, by sender, recipient, subject, mailing list, attachment or size. They run on this Mac without any AI model, and never delete for good or send anything. {AUTOMATION_NOTICE}</p>
  <div class="kill" class:on={auto.state.rulesPaused}>
    <label class="chk"><input type="checkbox" checked={auto.state.rulesPaused} onchange={(e) => auto.setRulesPaused(e.currentTarget.checked)} /> Pause all rules</label>
    <span class="dim">{auto.state.rulesPaused ? "Rules are paused. Mail that arrives now will not be sorted later." : "Rules are active."}</span>
  </div>

  {#if !auto.loaded}
    <div class="skeleton" style="height:40px"></div>
  {:else}
    {#if auto.rules.length === 0 && editing === null}<p class="empty">No rules yet.</p>{/if}
    <ul class="list">
      {#each auto.rules as r (r.id)}
        <li class="card" class:off={!r.enabled}>
          <div class="top">
            <strong class="nm">{r.name}</strong>
            <span class="badges">
              {#if r.origin === "ai"}<span class="tag">suggested by AI</span>{/if}
              <span class="state" class:on={r.enabled}>{r.enabled ? "On" : "Off"}</span>
            </span>
          </div>
          <p class="desc selectable">{r.description}</p>
          {#if r.reason}<p class="dim"><span class="label">AI's reason:</span> <span class="selectable">{r.reason}</span></p>{/if}
          <p class="dim mono">{r.account} · last run {when(r.lastRunAt)}</p>
          <div class="rw">
            <button class="btn sm" disabled={!r.valid || !!auto.busy[r.id]} onclick={() => toggle(r)}>{r.enabled ? "Turn off" : "Turn on…"}</button>
            <button class="btn ghost sm" disabled={!r.valid || !!auto.busy[r.id]} onclick={() => applyNow(r)}>Apply to existing…</button>
            <button class="btn ghost sm" disabled={!!auto.busy[r.id]} onclick={() => (editing = r.id)}>Edit</button>
            <button class="btn ghost sm" disabled={!!auto.busy[r.id]} onclick={() => del(r)}>Delete</button>
          </div>
        </li>
      {/each}
    </ul>

    {#if editing !== null}
      {#key editing}
        <RuleEditor rule={editRule} {account} onsaved={() => (editing = null)} oncancel={() => (editing = null)} />
      {/key}
    {:else}
      <div class="rw">
        {#if app.accounts.length > 1}<div class="sel"><Select value={account} options={acctOpts} ariaLabel="Account for the new rule" onchange={(v) => (account = v)} /></div>{/if}
        <button class="btn sm" disabled={!account} onclick={() => (editing = "new")}>New rule</button>
      </div>
    {/if}

    <h4>Recent runs</h4>
    {#if auto.runs.length === 0}
      <p class="empty">Nothing has run yet. When a rule changes mail, the run is listed here and can be undone.</p>
    {:else}
      <ul class="list">
        {#each auto.runs as r (r.id)}
          <li class="card">
            <div class="top"><strong class="nm">{r.ruleName}</strong><span class="dim mono">{when(r.ts)}</span></div>
            <p class="desc">{r.matched} message{r.matched === 1 ? "" : "s"} in {r.folder}: {r.summary}</p>
            <div class="rw">
              <span class="dim">{runStateText(r)}</span>
              {#if r.undoable && r.state !== "undone"}
                <button class="btn ghost sm" disabled={!!auto.runBusy[r.id]} onclick={() => auto.undoRun(r.id)}>{auto.runBusy[r.id] ? "Putting back…" : "Undo this run"}</button>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .rules { display: grid; gap: 10px; min-width: 0; }
  h3 { font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); margin-top: 6px; }
  h4 { font-size: 0.8667rem; letter-spacing: .08em; margin-top: 4px; }
  .note, .empty { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .dim { margin: 0; color: var(--muted); font-size: 0.8rem; overflow-wrap: anywhere; }
  .kill { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 12px; }
  .kill.on { border-left: 3px solid var(--signal); }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.8667rem; }
  input[type="checkbox"] { accent-color: var(--accent); }
  .list { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
  .card { border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; display: grid; gap: 6px; min-width: 0; }
  .card.off { background: color-mix(in srgb, var(--line) 10%, transparent); }
  .top { display: flex; justify-content: space-between; gap: 8px; align-items: baseline; flex-wrap: wrap; }
  .nm { min-width: 0; overflow-wrap: anywhere; }
  .badges { display: flex; gap: 6px; align-items: center; }
  .tag { font-size: 0.68rem; color: var(--muted); border: 1px solid var(--line); border-radius: var(--radius); padding: 0 4px; }
  .state { font-size: 0.72rem; color: var(--muted); }
  .state.on { color: var(--accent); font-weight: 600; }
  .desc { margin: 0; font-size: 0.8667rem; line-height: 1.5; overflow-wrap: anywhere; }
  .rw { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .sel { min-width: 200px; }
</style>
