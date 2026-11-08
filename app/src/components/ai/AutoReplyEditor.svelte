<script lang="ts">
  // Opt-in template auto-reply, per account. OFF by default. The text is what YOU type: no AI model writes or
  // chooses anything. The "Test" button renders the reply and sends nothing.
  import { onMount } from "svelte";
  import Select from "../Select.svelte";
  import DateTimeSelect from "../DateTimeSelect.svelte";
  import { aiApi } from "../../lib/aiapi";
  import { auto } from "../../lib/aiauto.svelte";
  import { errMsg } from "../../lib/api";
  import { app } from "../../lib/state.svelte";
  import { AUTOMATION_NOTICE, AUTOREPLY_WARNINGS, DEFAULT_REPLY, MAX_REPLY_BODY, fromLocalInput, replyOutcomeText, replyProblems, toLocalInput } from "../../lib/airules";
  import type { AutoReplyConfig, ReplyLogRow, ReplyPreview } from "../../lib/aitypes";

  let account = $state(app.account || app.accounts[0]?.email || "");
  const acctOpts = $derived(app.accounts.map((a) => ({ value: a.email, label: a.email })));
  let cfg = $state<AutoReplyConfig>({ ...DEFAULT_REPLY });
  let saved = $state<AutoReplyConfig>({ ...DEFAULT_REPLY });
  let startIn = $state(""), endIn = $state("");
  let loaded = $state(false), saving = $state(false);
  let preview = $state<ReplyPreview | null>(null), previewErr = $state(""), previewing = $state(false);
  let log = $state<ReplyLogRow[]>([]), logOpen = $state(false);
  let seq = 0;

  const issues = $derived(replyProblems(cfg));
  const dirty = $derived(JSON.stringify(cfg) !== JSON.stringify(saved));
  const when = (t: number) => new Date(t * 1000).toLocaleString();

  async function load() {
    const my = ++seq;
    loaded = false; preview = null; previewErr = "";
    if (!account) { loaded = true; return; }
    try {
      const c = await auto.getReply(account);
      if (my !== seq) return;
      cfg = { ...c }; saved = { ...c }; startIn = toLocalInput(c.start); endIn = toLocalInput(c.end);
      void loadLog();
    } catch (e) { app.fail(e); }
    loaded = true;
  }
  async function loadLog() {
    try { log = await auto.replyLog(account || null); } catch { /* the log just stays empty */ }
  }
  onMount(() => { void auto.load(); void load(); void aiApi.automationState().then((s) => (auto.state = s)).catch(() => {}); });

  function syncWindow() { cfg.start = fromLocalInput(startIn); cfg.end = fromLocalInput(endIn); }

  async function turn(on: boolean) {
    if (on) {
      const ok = await app.ask({
        title: "Turn on automatic replies?",
        body: `${AUTOREPLY_WARNINGS.join("\n\n")}\n\nIt answers only mail that arrives after you turn it on.`,
        ok: "Turn on",
      });
      if (!ok) return;
    }
    cfg.enabled = on;
    await save();
  }
  async function save() {
    syncWindow();
    if (issues.length || saving) return;
    saving = true;
    try {
      const c = await auto.saveReply(account, $state.snapshot(cfg) as AutoReplyConfig);
      cfg = { ...c }; saved = { ...c };
      app.notify(c.enabled ? "Automatic reply is on" : "Automatic reply saved (off)");
    } catch (e) { app.notify(errMsg(e), "error"); cfg.enabled = saved.enabled; }
    saving = false;
  }
  async function test() {
    syncWindow();
    previewing = true; previewErr = ""; preview = null;
    try { preview = await aiApi.autoreplyPreview(account, $state.snapshot(cfg) as AutoReplyConfig, "Your question"); }
    catch (e) { previewErr = errMsg(e); }
    previewing = false;
  }
</script>

<section class="ar" aria-labelledby="ar-h">
  <h3 id="ar-h">Automatic reply</h3>
  <p class="note">An optional out-of-office style reply, written by you. Off by default. No AI model is involved. {AUTOMATION_NOTICE}</p>

  {#if app.accounts.length === 0}
    <p class="empty">Add a mail account first.</p>
  {:else}
    {#if app.accounts.length > 1}
      <div class="field"><span class="label" id="ar-a">Account</span><Select value={account} labelledby="ar-a" options={acctOpts} onchange={(v) => { account = v; void load(); }} /></div>
    {/if}
    {#if !loaded}
      <div class="skeleton" style="height:48px"></div>
    {:else}
      <div class="warn" role="note">
        <strong>Read this before you turn it on</strong>
        <ul>{#each AUTOREPLY_WARNINGS as w (w)}<li>{w}</li>{/each}</ul>
      </div>
      <div class="kill" class:on={auto.state.autoreplyPaused}>
        <label class="chk"><input type="checkbox" checked={auto.state.autoreplyPaused} onchange={(e) => auto.setReplyPaused(e.currentTarget.checked)} /> Pause all automatic replies (every account)</label>
        <span class="dim">{auto.state.autoreplyPaused ? "Paused. Mail that arrives now will not be answered later." : "Not paused."}</span>
      </div>

      <div class="field"><label class="label" for="ar-s">Subject (optional)</label>
        <input id="ar-s" bind:value={cfg.subject} maxlength="200" placeholder="Leave empty for: Automatic reply" />
        <label class="chk"><input type="checkbox" bind:checked={cfg.echoSubject} /> Include the original subject (first 60 characters) where <span class="mono">{"{subject}"}</span> appears</label>
        <span class="help">Off by default: the original subject is not repeated back to whoever wrote.</span></div>
      <div class="field"><label class="label" for="ar-b">Reply text</label>
        <textarea id="ar-b" rows="6" bind:value={cfg.body} maxlength={MAX_REPLY_BODY} placeholder="Thanks for your message. I am away until ..."></textarea>
        <span class="help">Sent exactly as written. The original message is not quoted.</span></div>
      <div class="win">
        <div class="field"><span class="label" id="ar-st-l">Start (optional)</span><DateTimeSelect idPrefix="ar-st" ariaPrefix="Start" allowEmpty days={400} value={startIn} onchange={(v) => { startIn = v; syncWindow(); }} /></div>
        <div class="field"><span class="label" id="ar-en-l">End (optional)</span><DateTimeSelect idPrefix="ar-en" ariaPrefix="End" allowEmpty days={400} value={endIn} onchange={(v) => { endIn = v; syncWindow(); }} /></div>
        <div class="field"><label class="label" for="ar-cap">Most replies per day</label><input id="ar-cap" type="number" min="1" max="200" bind:value={cfg.dailyCap} /></div>
      </div>
      <label class="chk"><input type="checkbox" bind:checked={cfg.requireAuth} /> Only answer mail my provider verified (aligned DKIM or SPF pass). If your provider adds no such result, nothing will be answered with this on.</label>

      {#if issues.length}<ul class="issues" aria-label="To fix">{#each issues as p (p)}<li>{p}</li>{/each}</ul>{/if}

      <div class="acts">
        <button class="btn ghost sm" onclick={test} disabled={previewing || !cfg.body.trim()}>{previewing ? "Rendering…" : "Test: show the reply (nothing is sent)"}</button>
        <span class="sp"></span>
        <button class="btn ghost sm" onclick={save} disabled={!dirty || issues.length > 0 || saving}>Save</button>
        {#if cfg.enabled}
          <button class="btn sm" onclick={() => turn(false)} disabled={saving}>Turn off</button>
        {:else}
          <button class="btn sm" onclick={() => turn(true)} disabled={saving || issues.length > 0 || !cfg.body.trim()}>Turn on…</button>
        {/if}
      </div>
      <p class="state" aria-live="polite">Status: <strong class:on={cfg.enabled}>{cfg.enabled ? "ON" : "OFF"}</strong>{cfg.enabled && auto.state.autoreplyPaused ? " (paused)" : ""}</p>

      <div aria-live="polite">
        {#if previewErr}<p class="res bad">{previewErr}</p>{/if}
        {#if preview}
          <div class="res">
            <p><strong>This is how a reply would look.</strong> Nothing was sent. The address below is made up.</p>
            <pre class="raw selectable">{preview.raw}</pre>
          </div>
        {/if}
      </div>

      <div class="logbar">
        <h4>Audit log</h4>
        <button class="btn ghost sm" onclick={() => { logOpen = !logOpen; if (logOpen) void loadLog(); }} aria-expanded={logOpen}>{logOpen ? "Hide" : "Show"}</button>
      </div>
      {#if logOpen}
        {#if log.length === 0}
          <p class="empty">Nothing yet. Every reply, and every message that was deliberately not answered, is listed here with the reason.</p>
        {:else}
          <ul class="log">
            {#each log as r (r.id)}
              <li class="row {r.outcome}">
                <span class="mono dim">{when(r.ts)}</span>
                <span class="who selectable">{r.sender || "(unknown sender)"}{r.subject ? ` · ${r.subject}` : ""}</span>
                <span class="why">{replyOutcomeText(r)}</span>
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
    {/if}
  {/if}
</section>

<style>
  .ar { display: grid; gap: 10px; min-width: 0; }
  h3 { font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); margin-top: 6px; }
  h4 { font-size: 0.8667rem; letter-spacing: .08em; margin: 0; }
  .note, .empty { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .dim { color: var(--muted); font-size: 0.8rem; }
  .help { font-size: 0.8rem; color: var(--muted); }
  .warn { border: 1px solid var(--line); border-left: 3px solid var(--signal); border-radius: var(--radius); padding: 8px 12px; font-size: 0.8667rem; line-height: 1.45; }
  .warn ul { margin: 4px 0 0; padding-left: 18px; display: grid; gap: 3px; }
  .kill { display: flex; gap: 12px; align-items: center; flex-wrap: wrap; border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 12px; }
  .kill.on { border-left: 3px solid var(--signal); }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.8667rem; }
  input[type="checkbox"] { accent-color: var(--accent); }
  textarea { width: 100%; box-sizing: border-box; resize: vertical; font: inherit; }
  .win { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; }
  .acts { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .sp { flex: 1; }
  .state { margin: 0; font-size: 0.8667rem; }
  .state strong.on { color: var(--accent); }
  .issues { margin: 0; padding-left: 18px; font-size: 0.8rem; color: var(--signal); }
  .res { margin: 0; font-size: 0.8667rem; border: 1px solid var(--line); border-left: 3px solid var(--accent); border-radius: var(--radius); padding: 6px 10px; }
  .res.bad { border-left-color: var(--signal); }
  .res p { margin: 0 0 6px; }
  .raw { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font: 0.75rem/1.45 var(--mono, monospace); max-height: 260px; overflow: auto; background: color-mix(in srgb, var(--line) 18%, transparent); border-radius: var(--radius); padding: 8px 10px; }
  .logbar { display: flex; justify-content: space-between; align-items: center; }
  .log { list-style: none; margin: 0; padding: 0; display: grid; gap: 4px; }
  .row { display: grid; gap: 1px; border: 1px solid var(--line); border-radius: var(--radius); padding: 6px 10px; font-size: 0.8rem; }
  .row.sent { border-left: 3px solid var(--accent); }
  .row.failed { border-left: 3px solid var(--signal); }
  .who { overflow-wrap: anywhere; }
  .why { color: var(--muted); }
  @media (max-width: 640px) { .win { grid-template-columns: 1fr; } }
</style>
