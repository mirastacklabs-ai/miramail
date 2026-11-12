<script lang="ts">
  import AiSettings from "./ai/AiSettings.svelte";
  import RulesPanel from "./ai/RulesPanel.svelte";
  import { app } from "../lib/state.svelte";
  import Modal from "./Modal.svelte";
  import SyncSettings from "./SyncSettings.svelte";
  import AppearanceTab from "./AppearanceTab.svelte";
  import AboutContent from "./AboutContent.svelte";
  import SignaturesTab from "./SignaturesTab.svelte";
  import ComposingTab from "./ComposingTab.svelte";
  import { takeSettingsTab } from "../lib/aiui.svelte";
  import RenderError from "./RenderError.svelte";
  import InboxCountSection from "./InboxCountSection.svelte";
  import NotificationsTab from "./NotificationsTab.svelte";
  import SystemSection from "./SystemSection.svelte";
  import StorageSection from "./StorageSection.svelte";
  import ExportSection from "./ExportSection.svelte";
  import BackupSection from "./BackupSection.svelte";
  import SendersTab from "./SendersTab.svelte";
  import { SHORTCUTS } from "../lib/shortcuts";
  import VacationTab from "./VacationTab.svelte";
  import { t } from "../lib/i18n";

  const TABS = [
    { id: "general", label: t("prefs.tab.general") }, { id: "appearance", label: t("prefs.tab.appearance") }, { id: "composing", label: t("prefs.tab.composing") }, { id: "signatures", label: t("prefs.tab.signatures") },
    { id: "vacation", label: t("prefs.tab.vacation") },
    { id: "sync", label: t("prefs.tab.sync") }, { id: "notifications", label: t("prefs.tab.notifications") }, { id: "senders", label: "Senders" }, { id: "privacy", label: t("prefs.tab.privacy") }, { id: "rules", label: "Rules" }, { id: "ai", label: t("prefs.tab.ai") },
    { id: "shortcuts", label: t("prefs.tab.shortcuts") }, { id: "about", label: t("prefs.tab.about") },
  ] as const;
  type Id = (typeof TABS)[number]["id"];
  /** Phone layout: one section shown inline (no modal, no tab column); the section list is PhoneSettings. */
  let { section = null }: { section?: string | null } = $props();
  // svelte-ignore state_referenced_locally
  let tab = $state<Id>(section ? (TABS.find((x) => x.id === section)?.id ?? "general") : takeSettingsTab() === "ai" ? "ai" : "general");
  let tabEls: HTMLButtonElement[] = [];

  function tabKey(e: KeyboardEvent) {
    const i = TABS.findIndex((t) => t.id === tab);
    const n = e.key === "ArrowDown" || e.key === "ArrowRight" ? (i + 1) % TABS.length
      : e.key === "ArrowUp" || e.key === "ArrowLeft" ? (i + TABS.length - 1) % TABS.length
      : e.key === "Home" ? 0 : e.key === "End" ? TABS.length - 1 : -1;
    if (n < 0) return;
    e.preventDefault(); tab = TABS[n].id; tabEls[n]?.focus();
  }

</script>

{#snippet panelBody()}
    <div class="panel" role="tabpanel" id="pp-{tab}" aria-labelledby="pt-{tab}" tabindex="-1">
      <!-- A tab that throws while rendering shows its error here; {#key} gives every tab a fresh boundary. -->
      {#key tab}
      <svelte:boundary onerror={(e) => console.error("[miramail] Preferences tab failed:", e)}>
        {#if tab === "general"}
          <h3>General</h3>
          <p class="note">Accounts are managed from the folder pane. Theme, fonts and density live under Appearance.</p>
          <p class="note">MiraMail keeps these preferences on this Mac only. Nothing is sent anywhere.</p>
          <InboxCountSection />
          <SystemSection />
        {:else if tab === "appearance"}
          <AppearanceTab />
        {:else if tab === "composing"}
          <ComposingTab />
        {:else if tab === "signatures"}
          <SignaturesTab />
        {:else if tab === "vacation"}
          <VacationTab />
        {:else if tab === "sync"}
          <SyncSettings />
        {:else if tab === "notifications"}
          <NotificationsTab />
        {:else if tab === "senders"}
          <SendersTab />
        {:else if tab === "privacy"}
          <h3>Privacy</h3>
          <dl class="ro">
            <dt>Remote images</dt><dd>Blocked by default in every message. You can load them per message from the banner above the message.</dd>
            <dt>Trackers</dt><dd>Images that look like tracking pixels are flagged and skipped by "Load safe images". Detection is heuristic.</dd>
            <dt>HTML mail</dt><dd>Rendered in a sandbox with scripts off. Links open in your browser only after you confirm.</dd>
            <dt>Telemetry</dt><dd>None. No analytics or tracking. MiraMail talks to your mail servers, DNS, and your email domain&rsquo;s standard mail-settings file at sign-in, plus services you set up yourself.</dd>
          </dl>
          <p class="note">These are fixed for now. Per-sender image allow-lists come later.</p>
          <StorageSection />
          <ExportSection />
          <BackupSection />
        {:else if tab === "rules"}
          <RulesPanel />
        {:else if tab === "ai"}
          <AiSettings />
        {:else if tab === "shortcuts"}
          <h3>Keyboard shortcuts</h3>
          {#each SHORTCUTS as g (g.group)}
            <section class="sc"><div class="label">{g.group}</div>
              <dl>{#each g.rows as [k, d] (k)}<div><dt class="mono">{k}</dt><dd>{d}</dd></div>{/each}</dl>
            </section>
          {/each}
        {:else if tab === "about"}
          <AboutContent />
        {/if}
        {#snippet failed(error, reset)}<RenderError {error} {reset} what="This section" />{/snippet}
      </svelte:boundary>
      {/key}
    </div>
{/snippet}

{#if section}
  {@render panelBody()}
{:else}
<Modal title={t("prefs.title")} onclose={() => (app.showSettings = false)} width={760}>
  <div class="prefs">
    <div class="tabs" role="tablist" aria-label={t("prefs.sections")} aria-orientation="vertical" tabindex="-1" onkeydown={tabKey}>
      {#each TABS as t, i (t.id)}
        <button role="tab" id="pt-{t.id}" aria-selected={tab === t.id} aria-controls="pp-{t.id}" tabindex={tab === t.id ? 0 : -1}
          bind:this={tabEls[i]} onclick={() => (tab = t.id)}>{t.label}</button>
      {/each}
    </div>
    {@render panelBody()}
  </div>
</Modal>
{/if}

<style>
  .prefs { display: grid; grid-template-columns: 150px minmax(0, 1fr); gap: 20px; align-items: start; }
  .tabs { display: grid; gap: 2px; position: sticky; top: 0; }
  .tabs button { text-align: left; background: none; border: 0; border-left: 2px solid transparent; padding: 8px 10px; color: var(--muted); text-transform: uppercase; letter-spacing: .1em; font-size: 0.8rem; border-radius: 0 var(--radius) var(--radius) 0; }
  .tabs button:hover { color: var(--ink); }
  .tabs button[aria-selected="true"] { color: var(--ink); border-left-color: var(--accent); background: color-mix(in srgb, var(--line) 45%, transparent); }
  .panel { display: grid; gap: 14px; min-width: 0; outline: none; }
  h3 { font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .ro { display: grid; gap: 10px; margin: 0; }
  .ro dt { color: var(--ink); font-weight: 600; font-size: 0.8667rem; text-transform: uppercase; letter-spacing: .08em; }
  .ro dd { margin: 2px 0 0; font-size: 0.9333rem; line-height: 1.5; }
  .sc { display: grid; gap: 6px; }
  .sc dl { margin: 0; display: grid; gap: 2px; }
  .sc dl > div { display: flex; gap: 14px; align-items: baseline; padding: 3px 0; border-bottom: 1px solid var(--line); }
  .sc dt { min-width: 96px; color: var(--ink); }
  .sc dd { margin: 0; font-size: 0.9333rem; }
  @media (max-width: 640px) { .prefs { grid-template-columns: 1fr; } .tabs { grid-auto-flow: column; overflow-x: auto; position: static; } }
</style>
