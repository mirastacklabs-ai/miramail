<script lang="ts">
  // Tone row under the message body. Layer 1 (the meter) is local: it runs on this Mac and sends nothing.
  // Layer 2 ("Check tone") asks the configured AI provider ONCE, only after the user confirms the notice below.
  import { onDestroy } from "svelte";
  import Modal from "../Modal.svelte";
  import Select from "../Select.svelte";
  import BottomSheet from "../mobile/BottomSheet.svelte";
  import { phone } from "../../lib/phone.svelte";
  import { ai } from "../../lib/ai.svelte";
  import { aiApi } from "../../lib/aiapi";
  import { errMsg } from "../../lib/api";
  import { hostOf, toneNotice, TONE_SETUP_HINT, isLoopbackHost } from "../../lib/aivm";
  import { analyseTone, ownText, planRich, restoreTokens, draftChanged, createAnnouncer, DISCLAIMER, summaryLine, type ToneMeter } from "../../lib/tonemeter";
  import type { ToneReport } from "../../lib/aitypes";

  let { text, html = null, meterOn, canRestore, onapply, onrestore }: {
    /** The user's own words as plain text (rich mode: from the HTML structure; plain mode: whole body, cut here). */
    text: string;
    /** Rich mode only: the editor HTML, so Apply can keep tables, callouts and buttons. null in plain mode. */
    html?: string | null;
    meterOn: boolean; canRestore: boolean; onapply: (body: string) => boolean | void; onrestore: () => void;
  } = $props();

  // ---- layer 1: local meter, debounced ----
  let meter = $state<ToneMeter | null>(null);
  let open = $state(false);
  $effect(() => {
    const t = text, on = meterOn;
    if (!on) { meter = null; return; }
    const h = setTimeout(() => { meter = analyseTone(t); }, 400);
    return () => clearTimeout(h);
  });
  // Screen readers: announce when the level changes, at most once per 5 s, with one trailing announcement when the window ends.
  let live = $state("");
  const announcer = createAnnouncer((t) => (live = t));
  onDestroy(() => announcer.dispose());
  $effect(() => {
    const m = meter;
    if (!m || m.empty) return;
    announcer.push(m.level, m.label);
  });

  // ---- layer 2: check with the AI provider ----
  type Stage = "idle" | "confirm" | "busy";
  let stage = $state<Stage>("idle");
  let hint = $state("");
  let err = $state("");
  let report = $state<ToneReport | null>(null);
  /** The draft text that was sent for the check; a later edit makes the rewrite stale. */
  let checkedDraft = $state<string | null>(null);
  let applied = $state(false);
  let profileId = $state("");
  let token = 0;
  const profile = $derived(ai.usable.find((p) => p.id === profileId) ?? ai.usable[0] ?? null);
  const host = $derived(profile ? hostOf(profile.baseUrl) ?? profile.baseUrl : "");
  const plan = $derived(html != null ? planRich(html) : null);
  /** What is sent: exactly the user's own text (rich: only the plain-text paragraphs; never tables, signature or quote). */
  const draft = $derived(plan ? plan.text : ownText(text));
  const applyNote = $derived(plan && !plan.canApply ? plan.reason : "");
  const prepared = $derived(report ? report.suggestions.map((s) => restoreTokens(s.text, report?.restore, checkedDraft ?? "")) : []);
  const stale = $derived(report !== null && draftChanged(checkedDraft, draft));
  const STALE_MSG = "Your draft changed since this check: check again to apply a rewrite";
  let copied = $state(-1);
  async function copy(i: number) {
    try { await navigator.clipboard.writeText(prepared[i].text); copied = i; setTimeout(() => { if (copied === i) copied = -1; }, 2500); } catch { err = "Could not copy. Select the text and copy it by hand."; }
  }

  /** Phone: the result lives in a dismissible bottom sheet (no permanent bar). Opens only for a message to show. */
  let sheet = $state(false);
  export function closeSheet() { sheet = false; }
  /** Same entry as the desktop "Check tone" button: asks for confirmation before anything is sent. */
  export async function start() {
    if (phone.on) {
      // Never a second request while one runs; show the sheet instead. After Apply, "Restore my version" must stay
      // reachable, and the last result is shown again while it is still current: neither starts a request.
      if (stage === "busy" || stage === "confirm" || canRestore || (report && !stale)) { sheet = true; return; }
    }
    await begin();
  }
  /** Start a check: confirm notice first (clause (f)); the request itself only happens after the user confirms. */
  async function begin() {
    hint = ""; err = "";
    if (!draft) { hint = "Write something first."; sheet = true; return; }
    await ai.load();
    const pick = ai.activeProfile && !ai.activeProfile.needsKeyReentry ? ai.activeProfile : ai.usable[0];
    if (!pick) { hint = TONE_SETUP_HINT; sheet = true; return; }
    profileId = pick.id;
    stage = "confirm";
  }
  async function send() {
    if (!profile) return;
    const my = ++token, pid = profile.id;
    sheet = true;
    stage = "busy"; err = ""; report = null; checkedDraft = null; applied = false;
    try {
      const sent = draft;
      const r = await aiApi.checkTone(pid, sent);
      if (my === token) { report = r; checkedDraft = sent; }
    } catch (e) {
      if (my === token) err = errMsg(e);
    } finally { if (my === token) stage = "idle"; }
  }
  function cancel() { token++; stage = "idle"; }
  function apply(t: string) { if (stale) { err = STALE_MSG; return; } if (onapply(t) !== false) applied = true; else err = "The text could not be placed back into your message. Use Copy."; }
  function restore() { onrestore(); applied = false; }
  const TONE_ICON: Record<string, string> = { warm: "☺", neutral: "–", firm: "■", tense: "!", angry: "!!" };
  const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
</script>

{#snippet toneBody()}
  <div class="row">
    {#if meterOn}
      {#if meter}
        <span class="chip {meter.level}"><span class="ic" aria-hidden="true">{meter.icon}</span> {meter.empty ? "Tone" : meter.label}</span>
        <span class="sum">{summaryLine(meter)}</span>
        {#if !meter.empty && meter.signals.length}
          <button type="button" class="lnk" aria-expanded={open} onclick={() => (open = !open)}>{open ? "Hide why" : "Why?"}</button>
        {/if}
      {:else}<span class="chip neutral"><span class="ic" aria-hidden="true">–</span> Tone</span>{/if}
      <span class="disc">{DISCLAIMER}</span>
    {/if}
    <span class="grow"></span>
    {#if !phone.on}<button type="button" class="btn ghost sm" onclick={start} disabled={stage === "busy"} title="Ask your AI provider for a second opinion on this draft's tone. You are asked to confirm first.">{stage === "busy" ? "Checking…" : "Check tone"}</button>{/if}
  </div>
  <span class="sr-only" role="status" aria-live="polite">{live}</span>

  {#if meterOn && open && meter && meter.signals.length}
    <ul class="why" aria-label="Why this tone">
      {#each meter.signals as s (s.id)}
        <li class={s.kind}><span class="k" aria-hidden="true">{s.kind === "good" ? "+" : s.kind === "warn" ? "!" : "·"}</span> <b>{s.label}.</b> {s.why}</li>
      {/each}
      {#if meter.truncated}<li class="note"><span class="k" aria-hidden="true">·</span> Only the first 20,000 characters were checked.</li>{/if}
    </ul>
  {/if}

  {#if hint}<p class="msg" role="status">{hint} {#if hint === TONE_SETUP_HINT}<span class="dim">(Preferences &gt; AI)</span>{/if}</p>{/if}
  {#if err}<p class="msg bad" role="alert">{err}</p>{/if}

  {#if report}
    <section class="res" aria-label="Tone check result">
      <p class="head"><span class="chip {report.tone}"><span class="ic" aria-hidden="true">{TONE_ICON[report.tone]}</span> {cap(report.tone)}</span>
        {#if report.clarity != null}<span class="dim">Clarity {report.clarity}/5</span>{/if}
        <span class="dim">· from {profile?.name ?? "your AI provider"}, a second opinion, not a judgement</span>
        <button type="button" class="lnk" onclick={() => { report = null; checkedDraft = null; }}>Close</button></p>
      {#if report.notes.length}<ul>{#each report.notes as n}<li>{n}</li>{/each}</ul>{/if}
      {#each report.suggestions as s, i (i)}
        {@const p = prepared[i]}
        {@const why = stale ? STALE_MSG : p.blocked || applyNote}
        <div class="sug">
          <div class="sh"><b>{s.label}</b>
            <span class="btns">
              <button type="button" class="btn ghost sm" onclick={() => copy(i)}>{copied === i ? "Copied" : "Copy"}</button>
              <button type="button" class="btn sm" disabled={!!why} onclick={() => apply(p.text)} title="Replaces your plain text paragraphs with this version. Tables, buttons, callouts, the quoted message and your signature stay.">Apply</button>
            </span></div>
          {#if why}<p class="msg" role="note">{why}</p>{/if}
          <pre>{p.text}</pre>
        </div>
      {/each}
      {#if !report.suggestions.length}<p class="dim">No rewrite suggested.</p>{/if}
      {#if canRestore}<p class="rs"><button type="button" class="btn ghost sm" onclick={restore}>Restore my version</button> <span class="dim">{applied ? "Applied. " : ""}Puts back what you had before Apply.</span></p>{/if}
    </section>
  {:else if canRestore}
    <p class="rs"><button type="button" class="btn ghost sm" onclick={restore}>Restore my version</button></p>
  {/if}
  {#if phone.on && stage === "idle" && (canRestore || stale)}
    <p class="rs"><button type="button" class="btn ghost sm" onclick={begin}>Check tone again</button></p>
  {/if}
{/snippet}

{#if !phone.on}
<div class="tone" role="group" aria-label="Tone check">
{@render toneBody()}
</div>
{:else if sheet}
<BottomSheet title="Tone" onclose={closeSheet}>
<div class="tone ph" role="group" aria-label="Tone check">
{@render toneBody()}
</div>
</BottomSheet>
{/if}

{#if stage === "confirm" && profile}
  <Modal title="Check tone with {profile.name}" onclose={cancel} width={500}>
    <p class="notice">{toneNotice(profile.name, host)}</p>
    {#if isLoopbackHost(host)}<p class="dim">This provider runs on this Mac.</p>{/if}
    {#each profile.warnings as w (w.kind)}
      <p class="warn" role="note"><strong>{w.kind === "cleartextRemote" ? "Not encrypted." : "Local network."}</strong> {w.message}</p>
    {/each}
    {#if ai.usable.length > 1}
      <span class="dim" id="tone-prov-l">Provider</span>
      <Select value={profile.id} labelledby="tone-prov-l" options={ai.usable.map((p) => ({ value: p.id, label: p.name }))} onchange={(v) => (profileId = v)} />
    {/if}
    <p class="dim">Links in the text are shortened to their domain{ai.settings.defaultRedaction.maskAddresses ? " and email addresses are masked" : ""}, as in your AI redaction settings.</p>
    <div class="acts">
      <button class="btn ghost" onclick={cancel}>Cancel</button>
      <button class="btn" data-autofocus onclick={send}>Send</button>
    </div>
  </Modal>
{/if}

<style>
  .tone { border-top: 1px solid var(--line); padding: 4px 14px; font-size: 0.8rem; color: var(--muted); display: grid; gap: 4px; }
  .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; min-height: 26px; }
  .grow { flex: 1; }
  .chip { display: inline-flex; align-items: center; gap: 5px; border: 1px solid currentColor; border-radius: 999px; padding: 1px 9px; font-weight: 600; font-size: 0.75rem; white-space: nowrap; }
  .chip .ic { font-weight: 700; min-width: 1ch; text-align: center; }
  .chip.warm { color: var(--accent); }
  .chip.neutral { color: var(--muted); }
  .chip.firm { color: var(--signal); }
  .chip.tense, .chip.angry { color: var(--danger); }
  .sum { color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 46ch; }
  .disc { font-size: 0.7rem; opacity: .85; }
  .dim { color: var(--muted); font-size: 0.8rem; }
  .lnk { background: none; border: 0; color: var(--muted); text-decoration: underline; padding: 0 2px; font-size: 0.75rem; }
  .lnk:hover { color: var(--accent); }
  .why, .res ul { margin: 0; padding-left: 18px; display: grid; gap: 3px; }
  .why { list-style: none; padding-left: 0; }
  .why li { color: var(--text); line-height: 1.4; }
  .why .k { display: inline-block; width: 1.2ch; font-weight: 700; color: var(--muted); }
  .why .warn .k { color: var(--danger); } .why .good .k { color: var(--accent); }
  .msg { margin: 0; color: var(--text); } .msg.bad { color: var(--danger); }
  .res { border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 10px; display: grid; gap: 6px; color: var(--text); }
  .res .head { margin: 0; display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .sug { border-top: 1px solid var(--line); padding-top: 6px; display: grid; gap: 4px; }
  .sh { display: flex; justify-content: space-between; align-items: center; }
  pre { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; max-height: 160px; overflow: auto; }
  .rs { margin: 0; }
  .btns { display: inline-flex; gap: 6px; }
  .notice { margin: 0 0 8px; line-height: 1.5; }
  .warn { border: 1px solid var(--line); border-left: 2px solid var(--signal); border-radius: var(--radius); padding: 6px 10px; margin: 0 0 8px; }
  .tone.ph { border-top: 0; padding: 4px 0; font-size: 0.9rem; }
  .tone.ph :global(.btn.sm) { min-height: 44px; }
  .acts { display: flex; justify-content: flex-end; gap: 8px; margin-top: 10px; }
</style>
