<script lang="ts">
  // Preferences > AI: providers, budgets, redaction defaults, retention and purge.
  import { onMount } from "svelte";
  import Select from "../Select.svelte";
  import AutoReplyEditor from "./AutoReplyEditor.svelte";
  import { ai } from "../../lib/ai.svelte";
  import { aiApi } from "../../lib/aiapi";
  import { app } from "../../lib/state.svelte";
  import { errMsg } from "../../lib/api";
  import {
    CONV_OPTIONS, EGRESS_OPTIONS, PRESETS, clientWarnings, hostOf, isLoopbackHost, keyReentryNeeded, mergeWarnings,
    purgeLines, validateBudgets, validateProfileForm,
  } from "../../lib/aivm";
  import type { AiSettings, Profile, ProfileKind, Redaction, TestResult } from "../../lib/aitypes";

  onMount(() => { void ai.load(); });

  // ---------- provider form ----------
  let editing = $state<"new" | string | null>(null);
  let presetId = $state("");
  let name = $state(""), kind = $state<ProfileKind>("openai_chat"), baseUrl = $state(""), model = $state(""), maxOut = $state(1024);
  let apiKey = $state(""), clearKey = $state(false), customModel = $state(false);
  let result = $state<TestResult | null>(null), testing = $state(false), saving = $state(false);
  let formId = "";
  const original = $derived(editing && editing !== "new" ? ai.profiles.find((p) => p.id === editing) ?? null : null);

  function openForm(p: Profile | null) {
    editing = p ? p.id : "new";
    formId = p?.id ?? `p-${crypto.randomUUID().replace(/-/g, "").slice(0, 10)}`;
    presetId = "";
    name = p?.name ?? ""; kind = p?.kind ?? "openai_chat"; baseUrl = p?.baseUrl ?? ""; model = p?.model ?? ""; maxOut = p?.maxOutputTokens ?? 1024;
    apiKey = ""; clearKey = false; result = null; customModel = false;
  }
  function closeForm() { editing = null; apiKey = ""; result = null; }
  /** Presets only fill the form; nothing is contacted until Test or Save. */
  function applyPreset(id: string) {
    presetId = id;
    const pr = PRESETS.find((x) => x.id === id);
    if (!pr || id === "custom") return;
    if (!name.trim() || PRESETS.some((x) => x.label === name)) name = pr.label;
    kind = pr.kind; baseUrl = pr.baseUrl; model = pr.model; maxOut = pr.maxOutputTokens; result = null; customModel = false;
  }
  const reenter = $derived(keyReentryNeeded(original, baseUrl));
  const warnings = $derived.by(() => {
    const local = clientWarnings(baseUrl);
    const same = original && !keyReentryNeeded({ ...original, needsKeyReentry: false }, baseUrl);
    return same ? mergeWarnings(original!.warnings, local) : local;
  });
  const models = $derived(result?.ok ? result.models : []);
  const modelOpts = $derived.by(() => {
    const o = models.map((m) => ({ value: m, label: m }));
    if (model && !models.includes(model)) o.unshift({ value: model, label: model });
    o.push({ value: "__custom", label: "Other (type a name)…" });
    return o;
  });
  function draft(): Profile {
    return {
      id: formId, name: name.trim(), kind, baseUrl: baseUrl.trim(), model: model.trim(), maxOutputTokens: Math.round(+maxOut),
      anthropicVersion: original?.anthropicVersion ?? null, hasKey: original?.hasKey ?? false, keyOrigin: original?.keyOrigin ?? null,
      needsKeyReentry: false, warnings: [], testedAt: original?.testedAt ?? null, testOk: original?.testOk ?? null,
    };
  }
  /** undefined-style semantics from the contract: null keeps the saved key, "" clears it. */
  const keyArg = () => (clearKey ? "" : apiKey.trim() ? apiKey.trim() : null);

  async function testIt() {
    const bad = validateProfileForm({ name: name || "x", kind, baseUrl, model: model || "x", maxOutputTokens: Math.round(+maxOut) });
    if (bad && !bad.startsWith("Give")) { app.notify(bad, "error"); return; }
    testing = true; result = null;
    try {
      const r = await ai.test(draft(), apiKey.trim() ? apiKey.trim() : null);
      result = r;
      if (r.ok && !model.trim() && r.models.length) model = r.models[0];
    } catch (e) { result = { ok: false, latencyMs: 0, models: [], supportsTools: false, message: errMsg(e) }; }
    testing = false;
  }
  /** Entering a key fetches this endpoint's model list: one GET /models via the backend, never a chat request. */
  async function detectModels() {
    if (testing || kind === "anthropic" || !apiKey.trim() || !baseUrl.trim()) return;
    if (validateProfileForm({ name: name || "x", kind, baseUrl, model: model || "x", maxOutputTokens: Math.round(+maxOut) })) return;
    testing = true; result = null;
    try {
      const r = await ai.listModels(draft(), apiKey.trim());
      result = r;
      if (r.ok && !model.trim() && r.models.length) model = r.models[0];
    } catch (e) { result = { ok: false, latencyMs: 0, models: [], supportsTools: false, message: errMsg(e) }; }
    testing = false;
  }
  async function saveIt() {
    const bad = validateProfileForm({ name, kind, baseUrl, model, maxOutputTokens: Math.round(+maxOut) });
    if (bad) { app.notify(bad, "error"); return; }
    saving = true;
    try {
      await ai.saveProfile(draft(), keyArg());
      app.notify("Provider saved");
      closeForm();
    } catch (e) { app.notify(errMsg(e), "error"); }
    apiKey = "";
    saving = false;
  }
  async function removeIt(p: Profile) {
    const ok = await app.ask({ title: `Remove ${p.name}?`, body: "The provider and its saved key are removed from this Mac. Existing conversations stay readable.", ok: "Remove", danger: true });
    if (!ok) return;
    try { await ai.deleteProfile(p.id); if (editing === p.id) closeForm(); } catch (e) { app.fail(e); }
  }

  // ---------- budgets / redaction / retention ----------
  // ai.settings is a $state proxy: structuredClone() throws DataCloneError on it (that aborted this whole tab).
  // $state.snapshot gives a plain copy.
  let s = $state<AiSettings>($state.snapshot(ai.settings));
  let base = $state(JSON.stringify(ai.settings));
  let busy = $state(false);
  let purgeNote = $state<string[]>([]);
  $effect(() => { if (ai.loaded) { s = $state.snapshot(ai.settings); base = JSON.stringify(ai.settings); } });
  const dirty = $derived(JSON.stringify(s) !== base);
  const key = (v: number | null) => (v === null ? "never" : String(v));
  const unkey = (k: string) => (k === "never" ? null : Number(k));

  const REDACT: { k: keyof Redaction; label: string }[] = [
    { k: "stripQuotes", label: "Remove quoted earlier replies and signatures" },
    { k: "shortenLinks", label: "Shorten links to their domain" },
    { k: "maskAddresses", label: "Mask email addresses" },
  ];
  async function saveSettings() {
    const bad = validateBudgets(s);
    if (bad) { app.notify(bad, "error"); return; }
    busy = true;
    try { await ai.saveSettings(s); app.notify("AI settings saved"); } catch (e) { app.notify(errMsg(e), "error"); }
    busy = false;
  }
  async function purge() {
    const ok = await app.ask({
      title: "Purge now?",
      body: "This deletes the log of what was sent to AI providers and any conversations older than your auto-delete setting, using your current retention choices. This cannot be undone.",
      ok: "Purge now", danger: true,
    });
    if (!ok) return;
    try { purgeNote = purgeLines(await aiApi.purgeNow()); app.notify("Purge finished"); } catch (e) { app.notify(errMsg(e), "error"); }
  }
</script>

<div class="ai">
  <h3>AI providers</h3>
  <p class="note">Bring your own provider. Messages go from this Mac straight to the endpoint you choose; MiraMail has no AI service of its own. The AI never sends or permanently deletes mail.</p>

  {#if !ai.loaded && ai.loadError}
    <div class="warn" role="alert"><strong>AI settings could not be loaded.</strong> <span class="mono">{ai.loadError}</span>
      <div><button class="btn ghost sm" disabled={ai.loading} onclick={() => ai.load()}>{ai.loading ? "Retrying…" : "Retry"}</button></div></div>
  {:else if !ai.loaded}
    <div class="skeleton" style="height:48px" aria-busy="true"></div>
  {:else}
    {#if ai.profiles.length === 0 && !editing}
      <p class="empty">No provider yet.</p>
    {/if}
    <ul class="plist">
      {#each ai.profiles as p (p.id)}
        <li class="card">
          <div class="top">
            <div class="nm"><strong>{p.name}</strong> <span class="mono dim">{hostOf(p.baseUrl) ?? p.baseUrl} · {p.model}{isLoopbackHost(hostOf(p.baseUrl) ?? "") ? " (local)" : ""}</span></div>
            <div class="rw">
              <button class="btn ghost sm" onclick={() => openForm(p)}>Edit</button>
              <button class="btn danger sm" onclick={() => removeIt(p)} aria-label="Remove {p.name}">Remove</button>
            </div>
          </div>
          <div class="meta">
            <span>{p.hasKey ? "Key saved in Keychain" : "No key"}</span>
            {#if p.testedAt}<span>{p.testOk ? "Tested OK" : "Last test failed"}</span>{/if}
          </div>
          {#if p.needsKeyReentry}<p class="warn" role="alert"><strong>Base URL changed — re-enter the key.</strong> Edit this provider and type the key again.</p>{/if}
          {#each p.warnings as w (w.kind)}
            <p class="warn" role="note"><strong>{w.kind === "cleartextRemote" ? "Not encrypted." : "Local network."}</strong> {w.kind === "cleartextRemote" ? "API key and mail content are sent unencrypted" : w.message}</p>
          {/each}
        </li>
      {/each}
    </ul>
    {#if !editing}<div><button class="btn ghost sm" onclick={() => openForm(null)}>Add provider</button></div>{/if}

    {#if editing}
      <form class="form" onsubmit={(e) => { e.preventDefault(); void saveIt(); }} aria-label={editing === "new" ? "Add provider" : "Edit provider"}>
        <h4>{editing === "new" ? "Add provider" : "Edit provider"}</h4>
        {#if editing === "new"}
          <div class="field"><span class="label" id="pf-pre">Start from</span>
            <Select value={presetId} placeholder="Choose a provider…" labelledby="pf-pre" options={PRESETS.map((x) => ({ value: x.id, label: x.label, hint: x.baseUrl }))} onchange={applyPreset} />
            <span class="help">Presets only fill in the form. Nothing is contacted until you press Test or Save, or type an API key (which fetches the model list with one GET /models request and nothing else).</span></div>
        {/if}
        <div class="field"><label class="label" for="pf-n">Name</label><input id="pf-n" bind:value={name} autocomplete="off" spellcheck="false" /></div>
        <div class="field"><span class="label" id="pf-k">API type</span>
          <Select value={kind} labelledby="pf-k" options={[{ value: "openai_chat", label: "OpenAI-compatible", hint: "chat completions" }, { value: "anthropic", label: "Anthropic", hint: "messages API" }]} onchange={(v) => (kind = v as ProfileKind)} /></div>
        <div class="field"><label class="label" for="pf-u">Base URL</label>
          <input id="pf-u" bind:value={baseUrl} autocomplete="off" spellcheck="false" placeholder="https://api.example.com/v1" aria-describedby="pf-u-h" />
          <span class="help" id="pf-u-h">http or https; localhost and local-network servers are allowed.</span></div>

        {#each warnings as w (w.kind)}
          <p class="warn" role={w.kind === "cleartextRemote" ? "alert" : "note"}><strong>{w.kind === "cleartextRemote" ? "Not encrypted." : "Local network."}</strong>
            {w.kind === "cleartextRemote" ? "API key and mail content are sent unencrypted" : w.message}</p>
        {/each}

        <div class="field"><label class="label" for="pf-key">API key {#if original?.hasKey && !reenter}<span class="dim">(saved, leave blank to keep)</span>{:else}<span class="dim">(optional for local models)</span>{/if}</label>
          <input id="pf-key" type="password" bind:value={apiKey} onchange={detectModels} autocomplete="off" spellcheck="false" placeholder={original?.hasKey && !reenter ? "••••••••" : ""} />
          {#if reenter}<span class="warn inline" role="alert">Base URL changed — re-enter the key. A saved key is never sent to a different address.</span>{/if}
          {#if original?.hasKey && !reenter}<label class="chk"><input type="checkbox" bind:checked={clearKey} /> Remove the saved key</label>{/if}
          <span class="help">Stored only in the macOS Keychain. Never written to files or browser storage.</span></div>

        <div class="field"><span class="label" id="pf-m">Model</span>
          {#if models.length && !customModel}
            <Select value={model} labelledby="pf-m" options={modelOpts} mono onchange={(v) => { if (v === "__custom") { customModel = true; } else model = v; }} />
          {:else}
            <input id="pf-m" aria-labelledby="pf-m" bind:value={model} autocomplete="off" spellcheck="false" placeholder={kind === "anthropic" ? "claude-opus-5-5" : "model name"} />
            {#if models.length}<button type="button" class="lnk" onclick={() => (customModel = false)}>Pick from the list</button>{/if}
          {/if}
          {#if !models.length}<span class="help">Typing an API key fetches the model list (one GET /models request, nothing else); or press Test connection, or type a name.</span>{/if}</div>
        <div class="field"><label class="label" for="pf-t">Max output tokens</label><input id="pf-t" type="number" min="16" max="200000" step="1" bind:value={maxOut} /></div>

        <div class="acts">
          <button type="button" class="btn ghost sm" onclick={testIt} disabled={testing || !baseUrl.trim()}>{testing ? "Testing…" : "Test connection"}</button>
          <span class="sp"></span>
          <button type="button" class="btn ghost sm" onclick={closeForm}>Cancel</button>
          <button type="submit" class="btn sm" disabled={saving}>{saving ? "Saving…" : "Save"}</button>
        </div>
        <div aria-live="polite">
          {#if result}
            <p class="res" class:bad={!result.ok}>
              <strong>{result.ok ? "Connected" : "Test failed"}</strong>{result.ok ? ` in ${result.latencyMs} ms` : ""}. {result.message}
              {#if result.ok}<br /><span class="dim">{result.models.length} model{result.models.length === 1 ? "" : "s"} found. Tool use: {result.supportsTools ? "supported" : "not detected"}.</span>{/if}
            </p>
          {/if}
        </div>
      </form>
    {/if}
  {/if}

  <h3>Token budgets</h3>
  <p class="note">Hard limits per request, conversation and day. The assistant stops when one is reached.</p>
  <div class="grid3">
    <div class="field"><label class="label" for="b-t">Per turn</label><input id="b-t" type="number" min="100" step="100" bind:value={s.perTurnTokens} /></div>
    <div class="field"><label class="label" for="b-c">Per conversation</label><input id="b-c" type="number" min="100" step="1000" bind:value={s.perConversationTokens} /></div>
    <div class="field"><label class="label" for="b-d">Per day</label><input id="b-d" type="number" min="100" step="1000" bind:value={s.perDayTokens} /></div>
  </div>

  <h3>Default redaction</h3>
  <p class="note">Before any mail text leaves this Mac it can be reduced: quoted earlier replies and signatures are cut out, links are shortened to their domain, and email addresses are replaced with placeholders. These are the defaults for new conversations; you can change them per conversation.</p>
  {#each REDACT as r (r.k)}
    <label class="chk"><input type="checkbox" bind:checked={s.defaultRedaction[r.k]} /> {r.label}</label>
  {/each}

  <h3>Retention</h3>
  <div class="field"><span class="label" id="r-e">Log of what was sent</span>
    <Select value={key(s.purge.egressRetentionDays)} labelledby="r-e" options={EGRESS_OPTIONS.map((o) => ({ value: key(o.v), label: o.label }))} onchange={(v) => (s.purge.egressRetentionDays = unkey(v))} /></div>
  <div class="field"><span class="label" id="r-c">Conversations</span>
    <Select value={key(s.purge.conversationRetentionDays)} labelledby="r-c" options={CONV_OPTIONS.map((o) => ({ value: key(o.v), label: o.label }))} onchange={(v) => (s.purge.conversationRetentionDays = unkey(v))} /></div>
  <label class="chk"><input type="checkbox" bind:checked={s.purge.purgeOnAccountRemoval} /> Delete AI logs and conversations when I remove an account</label>

  <div class="acts">
    <button class="btn ghost sm" onclick={purge}>Purge now</button>
    <span class="sp"></span>
    <button class="btn sm" onclick={saveSettings} disabled={!dirty || busy}>{busy ? "Saving…" : "Save settings"}</button>
  </div>
  {#if purgeNote.length}<ul class="note pn" aria-live="polite">{#each purgeNote as l (l)}<li>{l}</li>{/each}</ul>{/if}

  <p class="note">Local rules have their own tab: Preferences > Rules.</p>
  <AutoReplyEditor />
</div>

<style>
  .ai { display: grid; gap: 12px; min-width: 0; }
  h3 { font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); margin-top: 6px; }
  h4 { font-size: 0.8667rem; letter-spacing: .08em; }
  .note, .help, .empty { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .help { font-size: 0.8rem; }
  .dim { color: var(--muted); font-weight: 400; text-transform: none; letter-spacing: 0; }
  .plist { list-style: none; margin: 0; padding: 0; display: grid; gap: 8px; }
  .card { border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; display: grid; gap: 6px; }
  .top { display: flex; justify-content: space-between; gap: 8px; align-items: center; flex-wrap: wrap; }
  .nm { min-width: 0; overflow-wrap: anywhere; }
  .rw { display: flex; gap: 6px; }
  .meta { display: flex; gap: 14px; font-size: 0.8rem; color: var(--muted); }
  .warn { margin: 0; font-size: 0.8667rem; color: var(--text); border: 1px solid var(--line); border-left: 3px solid var(--signal); border-radius: var(--radius); padding: 6px 10px; line-height: 1.45; }
  .warn.inline { display: block; }
  .form { border: 1px solid var(--line); border-radius: var(--radius); padding: 14px; display: grid; gap: 12px; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.8667rem; }
  input[type="checkbox"] { accent-color: var(--accent); }
  .acts { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .sp { flex: 1; }
  .grid3 { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 10px; }
  .res { margin: 0; font-size: 0.8667rem; border: 1px solid var(--line); border-left: 3px solid var(--accent); border-radius: var(--radius); padding: 6px 10px; }
  .res.bad { border-left-color: var(--signal); }
  .lnk { justify-self: start; background: none; border: 0; padding: 0; color: var(--accent); font-size: 0.8rem; text-decoration: underline; }
  .pn { padding-left: 18px; }
  @media (max-width: 640px) { .grid3 { grid-template-columns: 1fr; } }
</style>
