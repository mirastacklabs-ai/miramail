<script lang="ts">
  // Create or edit a local rule by hand. No model is involved. A new rule is saved OFF; you review the preview of
  // the mail it would match and turn it on yourself. There are no regular expressions and only five actions.
  import { onMount } from "svelte";
  import Select from "../Select.svelte";
  import { api, errMsg } from "../../lib/api";
  import { auto } from "../../lib/aiauto.svelte";
  import { app } from "../../lib/state.svelte";
  import { ACTION_CHOICES, COLOUR_NAMES, FIELDS, MAX_ACTIONS, MAX_CONDITIONS, describeSpec, dryRunText, emptyCondition, emptySpec, newAction, opsFor, problems, withField } from "../../lib/airules";
  import type { DryRun, Rule, RuleAction, RuleSpec } from "../../lib/aitypes";

  let { rule = null, account, onsaved, oncancel }: { rule?: Rule | null; account: string; onsaved: (r: Rule) => void; oncancel: () => void } = $props();

  // The parent re-creates this editor for every rule ({#key}), so the initial value is what we want.
  // svelte-ignore state_referenced_locally
  let name = $state(rule?.name ?? "");
  // svelte-ignore state_referenced_locally
  let spec = $state<RuleSpec>(rule?.spec ? $state.snapshot(rule.spec) : emptySpec());
  let folders = $state<string[]>([]);
  let dry = $state<DryRun | null>(null), dryErr = $state(""), checking = $state(false), saving = $state(false);
  const acct = $derived(rule?.account ?? account);

  onMount(async () => {
    try { folders = (await api.listFolders(acct)).filter((f) => f.selectable !== false).map((f) => f.name); } catch { folders = []; }
  });

  const issues = $derived(problems(spec, name));
  const text = $derived(describeSpec(spec));
  const folderOpts = $derived(folders.filter((f) => f.toLowerCase() !== "inbox").map((f) => ({ value: f, label: f })));

  function setField(i: number, f: (typeof FIELDS)[number]["value"]) { spec.conditions[i] = withField(spec.conditions[i], f); dry = null; }
  function addCondition() { if (spec.conditions.length < MAX_CONDITIONS) spec.conditions.push(emptyCondition("subject")); }
  function removeCondition(i: number) { spec.conditions.splice(i, 1); dry = null; }
  function toggleAction(t: RuleAction["type"], on: boolean) {
    if (on && !spec.actions.some((a) => a.type === t) && spec.actions.length < MAX_ACTIONS) {
      // only one of move / archive / trash at a time
      if (t === "moveTo" || t === "archive" || t === "trash") spec.actions = spec.actions.filter((a) => !["moveTo", "archive", "trash"].includes(a.type));
      spec.actions.push(newAction(t));
    } else if (!on) spec.actions = spec.actions.filter((a) => a.type !== t);
    dry = null;
  }
  const has = (t: RuleAction["type"]) => spec.actions.some((a) => a.type === t);
  const moveFolder = $derived((spec.actions.find((a) => a.type === "moveTo") as { folder: string } | undefined)?.folder ?? "");
  function setMoveFolder(f: string) { const a = spec.actions.find((x) => x.type === "moveTo"); if (a && a.type === "moveTo") a.folder = f; }

  const colourValue = $derived((spec.actions.find((a) => a.type === "colour") as { colour: number } | undefined)?.colour ?? 1);
  function setColour(n: number) { const a = spec.actions.find((x) => x.type === "colour"); if (a && a.type === "colour") a.colour = n; }

  async function preview() {
    checking = true; dryErr = ""; dry = null;
    try { dry = await auto.dryRun(acct, $state.snapshot(spec) as RuleSpec); } catch (e) { dryErr = errMsg(e); }
    checking = false;
  }
  async function save() {
    if (issues.length || saving) return;
    saving = true;
    try {
      const r = await auto.save({ id: rule?.id ?? null, account: acct, name: name.trim(), spec: $state.snapshot(spec) as RuleSpec });
      app.notify(rule ? "Rule saved" : "Rule saved. It is off until you turn it on.");
      onsaved(r);
    } catch (e) { app.notify(errMsg(e), "error"); }
    saving = false;
  }
</script>

<form class="ed" onsubmit={(e) => { e.preventDefault(); void save(); }} aria-label={rule ? "Edit rule" : "New rule"}>
  <div class="field"><label class="label" for="rn">Rule name</label><input id="rn" bind:value={name} maxlength="80" placeholder="For example: Receipts" /></div>

  <fieldset class="grp">
    <legend class="label">When a new message arrives and</legend>
    <div class="mode" role="radiogroup" aria-label="How conditions combine">
      <label class="chk"><input type="radio" name="mode" checked={spec.match === "all"} onchange={() => { spec.match = "all"; dry = null; }} /> all of these match</label>
      <label class="chk"><input type="radio" name="mode" checked={spec.match === "any"} onchange={() => { spec.match = "any"; dry = null; }} /> any of these matches</label>
    </div>
    {#each spec.conditions as c, i (i)}
      <div class="cond">
        <Select value={c.field} options={FIELDS} ariaLabel="Condition {i + 1} field" onchange={(v) => setField(i, v)} />
        <Select value={c.op} options={opsFor(c.field)} ariaLabel="Condition {i + 1} test" onchange={(v) => { c.op = v; dry = null; }} />
        {#if c.field === "hasListUnsubscribe" || c.field === "hasAttachment"}
          <Select value={c.value} options={[{ value: "true", label: "yes" }, { value: "false", label: "no" }]} ariaLabel="Condition {i + 1} value" onchange={(v) => { c.value = v; dry = null; }} />
        {:else if c.field === "size"}
          <input inputmode="numeric" maxlength="7" bind:value={c.value} aria-label="Condition {i + 1} size in kilobytes" placeholder="Size in KB, e.g. 5000" oninput={() => (dry = null)} />
        {:else}
          <input bind:value={c.value} maxlength="200" aria-label="Condition {i + 1} value" placeholder={c.op === "domainIs" ? "example.com" : c.op === "glob" ? "e.g. *sale*" : c.field === "folder" ? "Folder name" : c.op === "equals" && c.field !== "subject" ? "name@example.com" : "text"} oninput={() => (dry = null)} />
        {/if}
        <button type="button" class="iconbtn" aria-label="Remove condition {i + 1}" disabled={spec.conditions.length === 1} onclick={() => removeCondition(i)}><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg></button>
      </div>
    {/each}
    <button type="button" class="btn ghost sm" disabled={spec.conditions.length >= MAX_CONDITIONS} onclick={addCondition}>Add a condition</button>
    <p class="help">Patterns use <span class="mono">*</span> (any text) and <span class="mono">?</span> (one character); there are no regular expressions. Without a folder condition the rule looks at the Inbox only.</p>
  </fieldset>

  <fieldset class="grp">
    <legend class="label">Then</legend>
    <label class="chk"><input type="checkbox" checked={has("moveTo")} onchange={(e) => toggleAction("moveTo", e.currentTarget.checked)} /> Move to a folder</label>
    {#if has("moveTo")}
      <div class="ind">
        {#if folderOpts.length}
          <Select value={moveFolder} options={folderOpts} ariaLabel="Destination folder" placeholder="Choose a folder" onchange={setMoveFolder} />
        {:else}
          <input value={moveFolder} aria-label="Destination folder" placeholder="Folder name" oninput={(e) => setMoveFolder(e.currentTarget.value)} />
        {/if}
      </div>
    {/if}
    <label class="chk"><input type="checkbox" checked={has("archive")} onchange={(e) => toggleAction("archive", e.currentTarget.checked)} /> Archive</label>
    <label class="chk"><input type="checkbox" checked={has("trash")} onchange={(e) => toggleAction("trash", e.currentTarget.checked)} /> Move to Trash (not deleted for good)</label>
    <label class="chk"><input type="checkbox" checked={has("markRead")} onchange={(e) => toggleAction("markRead", e.currentTarget.checked)} /> Mark as read</label>
    <label class="chk"><input type="checkbox" checked={has("flag")} onchange={(e) => toggleAction("flag", e.currentTarget.checked)} /> Flag</label>
    <label class="chk"><input type="checkbox" checked={has("colour")} onchange={(e) => toggleAction("colour", e.currentTarget.checked)} /> Give a colour label (kept on this Mac)</label>
    {#if has("colour")}
      <div class="ind"><Select value={String(colourValue)} options={COLOUR_NAMES.slice(1).map((n, i) => ({ value: String(i + 1), label: n }))} ariaLabel="Colour label" onchange={(v) => setColour(Number(v))} /></div>
    {/if}
    <p class="help">Rules can only organise mail. They cannot delete for good, forward, reply or send anything.</p>
  </fieldset>

  <section class="sum" aria-label="What this rule does">
    <span class="label">In plain words</span>
    <p class="selectable">{text}</p>
  </section>

  <div class="acts">
    <button type="button" class="btn ghost sm" onclick={preview} disabled={checking || issues.length > 0}>{checking ? "Checking…" : "Preview on existing mail"}</button>
    <span class="sp"></span>
    <button type="button" class="btn ghost sm" onclick={oncancel}>Cancel</button>
    <button type="submit" class="btn sm" disabled={issues.length > 0 || saving}>{saving ? "Saving…" : rule ? "Save rule" : "Save rule (off)"}</button>
  </div>
  {#if issues.length}
    <ul class="issues" aria-label="To fix before saving">{#each issues as p (p)}<li>{p}</li>{/each}</ul>
  {/if}
  <div aria-live="polite">
    {#if dryErr}<p class="res bad">{dryErr}</p>{/if}
    {#if dry}
      <div class="res">
        <p>{dryRunText(dry)}</p>
        {#if dry.sample.length}
          <ul class="sample">{#each dry.sample as s, i (i)}<li><span class="mono">{s.folder}</span> {s.from} <span class="dim">· {s.subject}</span></li>{/each}</ul>
        {/if}
      </div>
    {/if}
  </div>
</form>

<style>
  .ed { display: grid; gap: 12px; border: 1px solid var(--line); border-radius: var(--radius); padding: 14px; min-width: 0; }
  .grp { border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; display: grid; gap: 8px; margin: 0; min-width: 0; }
  legend { padding: 0 4px; }
  .mode { display: flex; gap: 14px; flex-wrap: wrap; }
  .cond { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.2fr) minmax(0, 1.4fr) auto; gap: 6px; align-items: center; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.8667rem; }
  .ind { padding-left: 24px; }
  input[type="checkbox"], input[type="radio"] { accent-color: var(--accent); }
  .help { margin: 0; font-size: 0.8rem; color: var(--muted); line-height: 1.45; }
  .sum { border: 1px solid var(--line); border-left: 3px solid var(--accent); border-radius: var(--radius); padding: 8px 12px; display: grid; gap: 2px; }
  .sum p { margin: 0; font-size: 0.8667rem; line-height: 1.5; overflow-wrap: anywhere; }
  .acts { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .sp { flex: 1; }
  .issues { margin: 0; padding-left: 18px; font-size: 0.8rem; color: var(--signal); display: grid; gap: 1px; }
  .res { margin: 0; font-size: 0.8667rem; border: 1px solid var(--line); border-left: 3px solid var(--accent); border-radius: var(--radius); padding: 6px 10px; }
  .res p { margin: 0; }
  .res.bad { border-left-color: var(--signal); }
  .sample { margin: 6px 0 0; padding-left: 16px; display: grid; gap: 2px; font-size: 0.8rem; overflow-wrap: anywhere; }
  .dim { color: var(--muted); }
  @media (max-width: 640px) { .cond { grid-template-columns: 1fr 1fr; } }
</style>
