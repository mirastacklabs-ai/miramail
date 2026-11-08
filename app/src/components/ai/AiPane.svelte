<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { workspace } from "../../lib/workspace.svelte";
  import Select from "../Select.svelte";
  import AiThread from "./AiThread.svelte";
  import AiComposer from "./AiComposer.svelte";
  import AiConsent from "./AiConsent.svelte";
  import ActivityLog from "./ActivityLog.svelte";
  import { ai } from "../../lib/ai.svelte";
  import { app } from "../../lib/state.svelte";
  import { prefs } from "../../lib/prefs.svelte";
  import { CHAT_SCALE } from "../../lib/prefs-model";
  import { openAiSettings } from "../../lib/aiui.svelte";
  import { badgeText } from "../../lib/aivm";
  import { errMsg } from "../../lib/api";
  import type { Consent } from "../../lib/aitypes";

  // Detached window: reopen the conversation this pane was showing (payload = conversation id), then keep the payload current.
  const wanted = untrack(() => workspace.getPayload<{ conversationId?: string | null }>("aichat")?.conversationId ?? null);
  let restored = $state(!wanted);
  onMount(() => {
    void ai.load().then(async () => {
      if (wanted && ai.current?.id !== wanted && ai.conversations.some((c) => c.id === wanted)) await ai.open(wanted);
      restored = true;
    });
  });
  $effect(() => {
    const cid = ai.current?.id ?? (restored ? null : wanted);
    if (!restored) return;
    untrack(() => workspace.setPayload("aichat", { conversationId: cid }, { title: "MIRA", dirty: false }));
  });

  const cur = $derived(ai.current);
  const badge = $derived(cur ? badgeText(cur.endpointHost, cur.model) : "");
  const meter = $derived(ai.meter);
  const options = $derived(ai.conversations.map((c) => ({ value: c.id, label: c.title || "Untitled", hint: new Date(c.updatedAt * 1000).toLocaleDateString() })));

  async function begin(profileId: string, consent: Consent) {
    ai.consentOpen = false;
    try { await ai.start(profileId, consent); } catch (e) { app.notify(errMsg(e), "error"); }
  }
  async function openEdit() {
    try { ai.editConsent = await ai.currentConsent(); } catch (e) { app.fail(e); }
  }
  async function savePerms(c: Consent) {
    try { await ai.updateConsent(c); ai.editConsent = null; app.notify("Permissions saved"); }
    catch (e) { app.notify(errMsg(e), "error"); } // the dialog stays open with the user's ticks
  }
  async function del() {
    if (!cur) return;
    const ok = await app.ask({ title: "Delete this conversation?", body: "The conversation is removed from this Mac. Its log of what was sent is kept until it expires under Preferences > AI.", ok: "Delete", danger: true });
    if (ok) { try { await ai.remove(cur.id); } catch (e) { app.fail(e); } }
  }
</script>

<div class="ai">
  {#if !ai.loaded && ai.loadError}
    <div class="empty" role="alert">
      <h3>AI chat could not load</h3>
      <p class="mono">{ai.loadError}</p>
      <button class="btn" disabled={ai.loading} onclick={() => ai.load()}>{ai.loading ? "Retrying…" : "Retry"}</button>
    </div>
  {:else if !ai.loaded}
    <div class="skel" aria-busy="true"><div class="skeleton" style="height:14px;width:60%"></div><div class="skeleton" style="height:14px;width:40%"></div></div>
  {:else if ai.usable.length === 0}
    <div class="empty">
      <h3>Connect an AI provider</h3>
      <p>{ai.profiles.length ? "Your provider needs its key entered again before it can be used." : "Add OpenAI, Anthropic, OpenRouter or a model running on this Mac. Nothing is contacted until you start a conversation."}</p>
      <button class="btn" onclick={openAiSettings}>Open AI settings</button>
    </div>
  {:else}
    <div class="bar">
      <div class="sw">
        {#if options.length}
          <Select value={cur?.id ?? null} {options} ariaLabel="Conversation" placeholder="Choose a conversation" disabled={ai.streaming}
            onchange={(v) => ai.open(v)} />
        {:else}<span class="none">No conversations yet</span>{/if}
      </div>
      <button class="iconbtn" aria-label="New conversation" title="New conversation" disabled={ai.streaming} onclick={() => (ai.consentOpen = true)}><svg viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></svg></button>
      <button class="iconbtn" aria-label="Edit permissions" title="Edit permissions: what this chat may see" disabled={!cur || ai.streaming} onclick={openEdit}><svg viewBox="0 0 24 24"><path d="M12 3l7 3v5c0 4.5-3 8-7 10-4-2-7-5.5-7-10V6z"/><path d="M9 12l2 2 4-4"/></svg></button>
      <button class="iconbtn" aria-label="Delete conversation" title="Delete conversation" disabled={!cur || ai.streaming} onclick={del}><svg viewBox="0 0 24 24"><path d="M5 7h14M10 7V4h4v3M7 7l1 13h8l1-13"/></svg></button>
      <button class="iconbtn" aria-label="Activity log" aria-pressed={ai.activityOpen} title="Activity: changes you approved" onclick={() => (ai.activityOpen = !ai.activityOpen)}><svg viewBox="0 0 24 24"><path d="M4 6h16M4 12h16M4 18h10"/></svg></button>
      <button class="iconbtn txt" aria-label="Smaller chat text" title="Smaller chat text" disabled={prefs.v.chatScale <= CHAT_SCALE.min} onclick={() => prefs.set("chatScale", prefs.v.chatScale - CHAT_SCALE.step)}>A−</button>
      <button class="iconbtn txt" aria-label="Larger chat text" title="Larger chat text" disabled={prefs.v.chatScale >= CHAT_SCALE.max} onclick={() => prefs.set("chatScale", prefs.v.chatScale + CHAT_SCALE.step)}>A+</button>
      <button class="iconbtn" aria-label="AI settings" title="AI settings" onclick={openAiSettings}><svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="3"/><path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M18.4 5.6l-2.1 2.1M7.7 16.3l-2.1 2.1"/></svg></button>
    </div>
    {#if cur}
      <div class="sub">
        <span class="badge mono" title={cur.endpointHost + " · " + cur.model}>{badge}</span>
        <div class="meter {meter.level}" role="meter" aria-label="Tokens used in this conversation" aria-valuemin="0" aria-valuemax="100" aria-valuenow={meter.pct} aria-valuetext="{meter.label} tokens">
          <span class="trk" aria-hidden="true"><span class="fill" style="width:{meter.pct}%"></span></span>
          <span class="mono lbl">{meter.label}</span>
        </div>
      </div>
      {#if ai.activityOpen}
        <ActivityLog />
      {:else}
        <AiThread />
        <AiComposer />
      {/if}
    {:else}
      <div class="empty">
        <h3>Start a conversation</h3>
        <p>You choose what MIRA may see, and you can inspect exactly what was sent after each reply.</p>
        <button class="btn" onclick={() => (ai.consentOpen = true)}>New conversation</button>
      </div>
    {/if}
  {/if}
</div>

{#if ai.consentOpen}<AiConsent onstart={begin} oncancel={() => (ai.consentOpen = false)} />{/if}
{#if ai.editConsent}<AiConsent edit={ai.editConsent} onsave={savePerms} oncancel={() => (ai.editConsent = null)} />{/if}

<style>
  .ai { flex: 1; min-height: 0; min-width: 0; display: flex; flex-direction: column; }
  .skel { padding: 16px; display: grid; gap: 10px; }
  .iconbtn.txt { font-size: 0.7333rem; font-weight: 600; letter-spacing: 0; width: auto; min-width: 28px; padding: 0 4px; }
  .bar { flex: none; display: flex; align-items: center; gap: 4px; padding: 6px 8px; border-bottom: 1px solid var(--line); }
  .sw { flex: 1; min-width: 0; }
  .none { color: var(--muted); font-size: 0.8667rem; padding-left: 4px; }
  .sub { flex: none; display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 5px 12px; border-bottom: 1px solid var(--line); background: var(--bg-band); }
  .badge { color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .meter { display: flex; align-items: center; gap: 6px; flex: none; }
  .trk { width: 54px; height: 4px; background: var(--line); border-radius: 2px; overflow: hidden; }
  .fill { display: block; height: 100%; background: var(--accent); transition: width .2s; }
  .meter.warn .fill { background: var(--signal); }
  .meter.full .fill { background: var(--danger); }
  .lbl { color: var(--muted); font-size: 0.7rem; }
  .empty { margin: auto; padding: 24px; display: grid; gap: 10px; justify-items: center; text-align: center; max-width: 340px; }
  .empty h3 { font-size: 0.9333rem; letter-spacing: .08em; }
  .empty p { margin: 0; color: var(--muted); font-size: 0.8667rem; line-height: 1.5; }
</style>
