<script lang="ts">
  import Select from "./Select.svelte";
  import { onMount } from "svelte";
  import { api, onCalAddProgress, openExternal, errMsg } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { cal, CAL_COLORS } from "../lib/cal.svelte";
  import { ZOHO_REGIONS, type AddCalError, type CalPreset, type ReusableAccount } from "../lib/types";
  import Modal from "./Modal.svelte";

  let kind = $state<"local" | "caldav">(cal.calendars.some((c) => c.kind === "caldav") ? "local" : "caldav");
  let name = $state("");
  let color = $state(CAL_COLORS[cal.calendars.length % CAL_COLORS.length]);
  let preset = $state<CalPreset>("zoho");
  let region = $state("com");
  let url = $state("");
  let username = $state("");
  let password = $state("");
  let busy = $state(false);
  let err = $state("");
  let diag = $state<string[]>([]);
  let steps = $state<string[]>([]);
  let copied = $state(false);
  let reusable = $state<ReusableAccount[]>([]);
  let manual = $state(false); // "Enter a different login"
  const showReuse = $derived(kind === "caldav" && reusable.length > 0 && !manual);

  // Offer the mail login only when a saved mail account is at this provider (the backend decides).
  $effect(() => {
    const p = preset;
    reusable = [];
    void api.calReusableAccounts(p).then((r) => { if (p === preset) reusable = r; }).catch(() => {});
  });

  const REGION_LABEL: Record<string, string> = { com: ".com (US / global)", in: ".in (India)", eu: ".eu (Europe)", "com.au": ".com.au (Australia)", jp: ".jp (Japan)", ca: ".ca (Canada)", sa: ".sa (Saudi Arabia)" };
  const PRESETS: { id: CalPreset; label: string }[] = [{ id: "zoho", label: "Zoho" }, { id: "icloud", label: "iCloud" }, { id: "fastmail", label: "Fastmail" }, { id: "custom", label: "Other" }];
  const DOCS: Partial<Record<CalPreset, string>> = { zoho: "https://www.zoho.com/calendar/help/setup-caldav-sync.html", icloud: "https://appleid.apple.com" };

  onMount(() => {
    let off = () => {};
    void onCalAddProgress((s) => { if (busy) steps = [...steps, s]; }).then((f) => (off = f));
    void api.calSuggestCaldav().then((s) => {
      if (!s) return;
      if (!username) username = s.username;
      if (s.preset !== "custom") { preset = s.preset; if (s.region) region = s.region; }
    }).catch(() => {});
    return () => off();
  });

  const close = () => { password = ""; cal.addOpen = false; };

  async function openDoc(u: string) {
    const ok = await app.ask({ title: "Open link?", body: "This will open the address below in your default browser.", detail: u, ok: "Open" });
    if (!ok) return;
    try { await openExternal(u); } catch (e) { app.fail(e); }
  }
  async function copyDetails() {
    const text = [err, ...diag].join("\n");
    try { await navigator.clipboard.writeText(text); copied = true; setTimeout(() => (copied = false), 2000); } catch { /* clipboard unavailable */ }
  }

  async function submit(e?: Event, reuse?: string) {
    e?.preventDefault();
    if (busy) return;
    if (!reuse && showReuse) return;
    err = ""; diag = []; steps = []; copied = false;
    try {
      busy = true;
      if (kind === "local") {
        if (!name.trim()) { err = "Give the calendar a name."; busy = false; return; }
        await api.calCreateLocal(name.trim(), color);
      } else if (reuse) {
        const r = await api.calAddCaldavReuse(preset, reuse, preset === "zoho" ? region : undefined);
        app.notify(`Added ${r.length} calendar${r.length === 1 ? "" : "s"}`);
      } else {
        if (!username.trim() || !password) { err = "Enter your username and password."; busy = false; return; }
        if (preset === "custom" && !/^https:\/\//i.test(url.trim())) { err = "The server address must start with https://. Passwords are never sent over plain HTTP."; busy = false; return; }
        const r = await api.calAddCaldav(preset, preset === "custom" ? url.trim() : undefined, username.trim(), password, preset === "zoho" ? region : undefined);
        app.notify(`Added ${r.length} calendar${r.length === 1 ? "" : "s"}`);
      }
      password = "";
      await cal.refresh();
      cal.addOpen = false;
    } catch (ex) {
      const x = ex as Partial<AddCalError> | string;
      if (x && typeof x === "object" && typeof x.message === "string") { err = x.message; diag = Array.isArray(x.diagnostics) ? x.diagnostics : []; }
      else err = typeof x === "string" ? x : errMsg(x) || "Could not add the calendar.";
    }
    password = ""; // never keep the secret around after submit
    if (err && reuse) manual = true; // keep the manual form usable after a rejected mail login
    busy = false;
  }
</script>

<Modal title="Add calendar" onclose={close} width={500}>
  <form onsubmit={submit} class="f">
    <div class="seg" role="group" aria-label="Calendar type">
      <button type="button" class:on={kind === "local"} aria-pressed={kind === "local"} onclick={() => (kind = "local")}>Local</button>
      <button type="button" class:on={kind === "caldav"} aria-pressed={kind === "caldav"} onclick={() => (kind = "caldav")}>CalDAV</button>
    </div>
    {#if kind === "local"}
      <div class="field"><label class="label" for="ac-n">Name</label><input id="ac-n" data-autofocus bind:value={name} autocomplete="off" placeholder="Family" /></div>
      <div class="field"><span class="label" id="ac-c">Color</span>
        <div class="sw" role="radiogroup" aria-labelledby="ac-c">
          {#each CAL_COLORS as c (c)}<button type="button" role="radio" aria-checked={color === c} aria-label={c} class:on={color === c} style="--c:{c}" onclick={() => (color = c)}></button>{/each}
        </div></div>
      <p class="note">Stored only on this Mac.</p>
    {:else}
      <div class="field"><span class="label" id="ac-pl">Provider</span>
        <div class="seg pre" role="group" aria-labelledby="ac-pl">
          {#each PRESETS as p (p.id)}<button type="button" class:on={preset === p.id} aria-pressed={preset === p.id} onclick={() => (preset = p.id)}>{p.label}</button>{/each}
        </div></div>
      {#if preset === "zoho"}
        <div class="field"><label class="label" for="ac-r">Zoho data centre</label>
          <Select id="ac-r" value={region} options={ZOHO_REGIONS.map((r) => ({ value: r, label: REGION_LABEL[r] }))} onchange={(v) => (region = v)} ariaLabel="Zoho data centre" />
          <span class="hint">The region you sign in to Zoho with. Only .com, .eu and .com.au are named in Zoho's help page; the others are best guesses.</span></div>
      {/if}
      {#if preset === "custom"}<div class="field"><label class="label" for="ac-u">Server URL</label><input id="ac-u" bind:value={url} placeholder="https://dav.example.org/" spellcheck="false" autocomplete="off" /></div>{/if}
      {#if showReuse}
        <div class="reuse">
          {#each reusable as a (a.email)}
            <button type="button" class="btn" disabled={busy} onclick={() => submit(undefined, a.email)}>{busy ? "Connecting…" : `Use my ${a.email} mail login`}</button>
          {/each}
          <p class="note">Your password stays in the macOS Keychain and is only reused on this Mac, for the {PRESETS.find((p) => p.id === preset)?.label} calendar server.</p>
          <button type="button" class="ln" onclick={() => (manual = true)}>Enter a different login</button>
        </div>
      {:else}
      {#if reusable.length}<button type="button" class="ln" onclick={() => (manual = false)}>Use my mail login instead</button>{/if}
      <div class="field"><label class="label" for="ac-un">{preset === "zoho" ? "Zoho email address" : "Username"}</label><input id="ac-un" bind:value={username} autocomplete="off" spellcheck="false" /></div>
      <div class="field"><label class="label" for="ac-pw">Password</label><input id="ac-pw" type="password" bind:value={password} autocomplete="off" /></div>
      <div class="note">
        {#if preset === "zoho"}
          <p>Use your normal Zoho account password. Only if you turned on two-factor authentication, create an application-specific password in your Zoho account security settings and use that instead.</p>
          <p>Then, in Zoho Calendar, open Settings &gt; Synchronize &gt; CalDAV and tick each calendar you want here. Your primary calendar is on by default.</p>
        {:else if preset === "icloud"}
          <p>Use your Apple ID email and an app-specific password (Sign-In and Security at appleid.apple.com).</p>
        {:else if preset === "fastmail"}
          <p>Use your Fastmail address and an app password that allows calendars (CalDAV).</p>
        {:else}
          <p>Enter the full https:// CalDAV address of your server or calendar.</p>
        {/if}
        {#if DOCS[preset]}<p class="lnk">More: <button type="button" class="ln" onclick={() => openDoc(DOCS[preset]!)}>{DOCS[preset]}</button></p>{/if}
        <p>Your password is checked against the server, then stored only in the macOS Keychain. MiraMail contacts nothing but this server.</p>
      </div>
      {/if}
    {/if}
    {#if busy && kind === "caldav"}
      <ul class="steps" aria-live="polite">{#each steps as s, i (i)}<li class:cur={i === steps.length - 1}>{s}{i === steps.length - 1 && !s.startsWith("Found") ? "…" : ""}</li>{/each}</ul>
    {/if}
    {#if err}
      <div class="errbox" role="alert">
        <p class="err">{err}</p>
        {#if diag.length}
          <details><summary>Details</summary><pre>{diag.join("\n")}</pre></details>
          <button type="button" class="btn ghost sm" onclick={copyDetails}>{copied ? "Copied" : "Copy details"}</button>
        {/if}
      </div>
    {/if}
    <div class="acts"><button type="button" class="btn ghost" onclick={close}>Cancel</button>{#if !showReuse}<button class="btn" disabled={busy}>{busy ? (kind === "caldav" ? "Connecting…" : "Adding…") : "Add calendar"}</button>{/if}</div>
  </form>
</Modal>

<style>
  .f { display: grid; gap: 14px; }
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; justify-self: start; }
  .seg button { background: none; border: 0; padding: 6px 16px; font-size: 11px; text-transform: uppercase; letter-spacing: .12em; color: var(--muted); border-bottom: 2px solid transparent; }
  .seg button.on { color: var(--ink); border-bottom-color: var(--accent); }
  .sw { display: flex; gap: 8px; } .sw button { width: 24px; height: 24px; border-radius: 2px; border: 2px solid transparent; background: var(--c); padding: 0; }
  .sw button.on { outline: 2px solid var(--ink); outline-offset: 2px; }
  .note { margin: 0; font-size: 12px; color: var(--muted); display: grid; gap: 6px; } .note p { margin: 0; }
  .hint { font-size: 11px; color: var(--muted); }
  .ln { background: none; border: 0; padding: 0; color: var(--ink); text-decoration: underline; font: inherit; word-break: break-all; text-align: left; cursor: pointer; }
  .steps { margin: 0; padding: 0; list-style: none; font-size: 12px; color: var(--muted); display: grid; gap: 2px; } .steps li.cur { color: var(--ink); }
  .errbox { display: grid; gap: 8px; justify-items: start; } .err { margin: 0; color: var(--danger); font-size: 13px; }
  details summary { font-size: 12px; color: var(--muted); cursor: pointer; } pre { margin: 6px 0 0; font-size: 11px; white-space: pre-wrap; word-break: break-word; max-height: 140px; overflow: auto; }
  .reuse { display: grid; gap: 10px; justify-items: start; }
  .acts { display: flex; gap: 8px; justify-content: flex-end; }
</style>
