<script lang="ts">
  // Per-conversation consent: where messages go, what the assistant may see (later phases), and how mail is redacted.
  import { onMount, untrack } from "svelte";
  import { api } from "../../lib/api";
  import Modal from "../Modal.svelte";
  import Select from "../Select.svelte";
  import { ai } from "../../lib/ai.svelte";
  import { app } from "../../lib/state.svelte";
  import { buildConsent, withDefaultTicks, defaultScopes, hostOf, isLoopbackHost, consentHeading, editHeading, prefillScopes, prefillCalendars, consentChangeSummary, EDIT_KEEPS_READ_NOTE } from "../../lib/aivm";
  import type { Consent, ConsentScope, Redaction } from "../../lib/aitypes";

  // `edit` = "Edit permissions" for the open conversation: prefilled with its CURRENT consent; Save is the only thing that changes it.
  let { onstart, oncancel, edit = null, onsave }: {
    onstart?: (profileId: string, c: ReturnType<typeof buildConsent>) => void; oncancel: () => void;
    edit?: Consent | null; onsave?: (c: Consent) => void | Promise<void>;
  } = $props();
  // The dialog is mounted fresh for each edit session, so the starting consent is read once.
  const initial = untrack(() => edit);
  const editing = !!initial;

  const profiles = $derived(ai.usable);
  let profileId = $state(ai.activeProfile?.id ?? ai.usable[0]?.id ?? "");
  const profile = $derived(editing ? null : profiles.find((p) => p.id === profileId) ?? profiles[0] ?? null);
  const cur = $derived(ai.current);
  // Edit mode shows the conversation's own endpoint (the profile may have been renamed or removed since).
  const host = $derived(editing ? cur?.endpointHost ?? "" : profile ? hostOf(profile.baseUrl) ?? profile.baseUrl : "");
  const provName = $derived(editing ? ai.activeProfile?.name ?? host : profile?.name ?? "");
  const model = $derived(editing ? cur?.model ?? "" : profile?.model ?? "");
  const local = $derived(host ? isLoopbackHost(host) : false);
  let scopes = $state<ConsentScope[]>(initial ? initial.scopes.map((s) => ({ ...s })) : defaultScopes(app.account, app.folders));
  let redaction = $state<Redaction>(initial ? { ...initial.redaction } : { ...ai.settings.defaultRedaction });
  let tools = $state(initial ? initial.tools : true);
  let suggest = $state(initial ? initial.suggest : true);
  let calOn = $state(initial ? !!initial.calendar?.enabled : true);
  let cals = $state<{ id: string; name: string; color: string; readOnly: boolean; allowed: boolean }[]>([]);
  let saving = $state(false);
  onMount(async () => {
    try {
      const live = (await api.calListCalendars()).map((c) => ({ id: c.id, name: c.name, color: c.color, readOnly: c.readOnly }));
      cals = initial ? prefillCalendars(initial.calendar, live) : withDefaultTicks(live);
    } catch { cals = []; }
    if (initial) {
      // Folders that exist now in the conversation's accounts but were never offered appear unticked.
      const accounts = [...new Set(initial.scopes.filter((s) => s.allowed).map((s) => s.account))];
      const live: { account: string; name: string; selectable: boolean }[] = [];
      await Promise.all(accounts.map(async (a) => {
        try { for (const f of await api.listFolders(a)) live.push({ account: a, name: f.name, selectable: f.selectable }); } catch { /* keep the saved list for this account */ }
      }));
      scopes = prefillScopes(initial.scopes, live);
    }
  });
  const consentNow = $derived(buildConsent(scopes, redaction, tools, suggest, { enabled: calOn, calendars: cals.map((c) => ({ id: c.id, allowed: c.allowed })) }));
  const change = $derived(initial ? consentChangeSummary(initial, consentNow) : null);
  async function save() {
    if (!onsave || saving) return;
    saving = true;
    try { await onsave(consentNow); } finally { saving = false; }
  }

  const REDACT: { key: keyof Redaction; label: string; help: string }[] = [
    { key: "stripQuotes", label: "Remove quoted replies and signatures", help: "Earlier messages in a thread and sign-offs are cut out before anything is sent." },
    { key: "shortenLinks", label: "Shorten links to their domain", help: "https://example.org/a/long/tracking?id=… becomes example.org." },
    { key: "maskAddresses", label: "Mask email addresses", help: "Addresses are replaced with placeholders like person-1 so the model never sees them." },
  ];
</script>

<Modal title={editing ? "Edit permissions" : "New conversation"} onclose={oncancel} width={560}>
  {#if editing || profile}
    <div class="to" class:local>
      {#if editing}
        <h3>{local ? "Change what this chat may see with your local model?" : editHeading(provName, host, model)}</h3>
        <p>You are changing what this existing chat may see. Nothing is sent to the AI endpoint when you open this dialog, edit it or press Cancel, and nothing changes until you press Save permissions. After saving, {tools ? "the mail the assistant reads" : "only what you type"} {local ? "stays on this Mac" : `goes to ${provName} at `}{#if !local}<span class="mono">{host}</span> under its own terms{/if}. Earlier messages and suggestions are not changed; narrowing applies at once, including to suggestions still waiting for your approval. {EDIT_KEEPS_READ_NOTE}</p>
      {:else if local}
        <h3>Start a local conversation?</h3>
        <p>Local model at <span class="mono">{host}</span>. Nothing leaves this Mac.</p>
      {:else if profile}
        <h3>{consentHeading(profile.name, host, profile.model)}</h3>
        <p>{tools ? "What you type and the mail the assistant reads are" : "What you type here is"} sent to {profile.name} at <span class="mono">{host}</span> and processed by that provider under its own terms.</p>
      {/if}
      {#each (profile?.warnings ?? ai.activeProfile?.warnings ?? []) as w (w.kind)}
        <p class="warn" role="note"><strong>{w.kind === "cleartextRemote" ? "Not encrypted." : "Local network."}</strong> {w.message}</p>
      {/each}
    </div>

    {#if !editing && profiles.length > 1}
      <div class="field"><span class="label" id="ac-p">Provider</span>
        <Select value={profileId} labelledby="ac-p" options={profiles.map((p) => ({ value: p.id, label: p.name, hint: p.model }))} onchange={(v) => (profileId = v)} /></div>
    {/if}

    <fieldset>
      <legend class="label">What the assistant may read</legend>
      <label class="chk col"><span><input type="checkbox" bind:checked={tools} /> Let the assistant search and read my mail</span>
        <span class="help">Read-only. It looks things up in the folders ticked below (and folders it creates for you here after you approve); the messages it reads are sent to {local ? "the local model" : host}. Off = plain chat, no mail is sent. With this on, each request also carries the current date, time zone and your work hours.</span></label>
      {#if tools}
        <label class="chk col"><span><input type="checkbox" bind:checked={suggest} /> Assistant may suggest changes (you approve each one)</span>
          <span class="help">It can propose moving, archiving, trashing or flagging messages and creating a folder. Each suggestion shows exactly which messages it touches; nothing happens until you press Approve, and approved changes can be undone. It can never send mail or delete it for good.</span></label>
        <p class="note">{#if editing}Folders MIRA created in this chat are ticked. Untick a folder to take it away from this chat; new folders appear unticked.{:else}Junk and Trash start off. The assistant only sees ticked folders, plus any folder it creates for you here after you approve it.{/if}</p>
      {/if}
      {#if !tools}
        <p class="note">No mail is sent to the model in this conversation; only what you type.</p>
      {:else if scopes.length === 0}
        <p class="note">Folders appear here once an account is open.</p>
      {:else}
        <div class="scopes" aria-label="Folders the assistant may read">
          {#each scopes as s (s.account + s.folder)}
            <label class="chk"><input type="checkbox" bind:checked={s.allowed} /> <span class="mono">{s.folder}</span></label>
          {/each}
        </div>
      {/if}
    </fieldset>

    {#if tools && cals.length}
      <fieldset>
        <legend class="label">Calendar</legend>
        <label class="chk col"><span><input type="checkbox" bind:checked={calOn} /> Let MIRA see my calendar</span>
          <span class="help">{#if suggest}It can look things up and suggest events, changes and answers; nothing changes until you press Approve.{:else}Read-only.{/if} Event titles, times and attendees from the ticked calendars are sent to {local ? "the local model" : host} when it looks something up. All calendars are ticked; untick any you want kept out.</span></label>
        {#if calOn}
          <div class="scopes" aria-label="Calendars the assistant may read">
            {#each cals as c (c.id)}
              <label class="chk"><input type="checkbox" bind:checked={c.allowed} /> <span class="dotc" style:background={c.color} aria-hidden="true"></span> <span>{c.name}{c.readOnly ? " (read-only)" : ""}</span></label>
            {/each}
          </div>
        {/if}
      </fieldset>
    {/if}

    <fieldset>
      <legend class="label">Redaction</legend>
      <p class="note">Applies to any mail text before it leaves this Mac.</p>
      {#each REDACT as r (r.key)}
        <label class="chk col"><span><input type="checkbox" bind:checked={redaction[r.key]} /> {r.label}</span><span class="help">{r.help}</span></label>
      {/each}
    </fieldset>

    <p class="note">{#if tools && suggest}The assistant cannot send mail or delete it for good. Any change it suggests happens only after you approve it.{:else}The assistant cannot change your mailbox: it cannot send, move or delete mail.{/if}</p>
    {#if editing}
      <p class="note" role="status">{change ? change.replace("Permissions changed by you: ", "Will change: ") : "No changes yet."}</p>
      <div class="acts">
        <button class="btn ghost" onclick={oncancel}>Cancel</button>
        <button class="btn" data-autofocus disabled={!change || saving} onclick={save}>{saving ? "Saving…" : "Save permissions"}</button>
      </div>
    {:else if profile}
      <div class="acts">
        <button class="btn ghost" onclick={oncancel}>Cancel</button>
        <button class="btn" data-autofocus onclick={() => onstart?.(profile.id, consentNow)}>Start</button>
      </div>
    {/if}
  {:else}
    <p>Connect an AI provider first.</p>
    <div class="acts"><button class="btn ghost" onclick={oncancel}>Close</button></div>
  {/if}
</Modal>

<style>
  h3 { font-size: 0.9333rem; letter-spacing: .05em; text-transform: none; }
  .to { display: grid; gap: 6px; }
  .to p, .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .to.local { border-left: 2px solid var(--accent); padding-left: 10px; }
  .warn { color: var(--text); border: 1px solid var(--line); border-left: 2px solid var(--signal); border-radius: var(--radius); padding: 6px 10px; }
  fieldset { border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; display: grid; gap: 8px; margin: 0; min-width: 0; }
  legend { padding: 0 6px; }
  .scopes { display: grid; grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); gap: 4px 12px; max-height: 150px; overflow-y: auto; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.8667rem; }
  .chk.col { display: grid; gap: 2px; }
  .dotc { width: 10px; height: 10px; border-radius: 50%; flex: none; }
  .help { color: var(--muted); font-size: 0.8rem; padding-left: 22px; }
  input[type="checkbox"] { accent-color: var(--accent); }
  .acts { display: flex; justify-content: flex-end; gap: 8px; }
</style>
