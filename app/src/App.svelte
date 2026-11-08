<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { app } from "./lib/state.svelte";
  import { replyFresh, replyTo } from "./lib/compose";
  import { onClosePane } from "./lib/api";
  import { workspace } from "./lib/workspace.svelte";
  import Workspace from "./components/Workspace.svelte";
  import Onboarding from "./components/Onboarding.svelte";
  import Sidebar from "./components/Sidebar.svelte";
  import Overlays from "./components/Overlays.svelte";
  import OutboxUi from "./components/OutboxUi.svelte";
  import Tier2Ui from "./components/Tier2Ui.svelte";
  import { snoozeUi } from "./lib/t2.svelte";
  import { outbox } from "./lib/outbox.svelte";
  import About from "./components/About.svelte";
  import Divider from "./components/Divider.svelte";
  import { layout } from "./lib/layout.svelte";
  import ZoomSettings from "./components/ZoomSettings.svelte";
  import Settings from "./components/Settings.svelte";
  import AddCalendar from "./components/AddCalendar.svelte";
  import Contacts from "./components/Contacts.svelte";
  import { contactsUi } from "./lib/contacts.svelte";
  import { cal } from "./lib/cal.svelte";
  import { platform } from "./lib/platform.svelte";
  import ShortcutSheet from "./components/ShortcutSheet.svelte";

  onMount(() => {
    void app.init();
    void workspace.init().then(() => outbox.init());
    void snoozeUi.init();
    workspace.onNewEvent = () => { void cal.start().then(() => cal.newEvent()); };
    let offClose: (() => void) | undefined;
    void onClosePane(() => workspace.closeFocused()).then((f) => (offClose = f));
    let offNative: (() => void) | undefined;
    void workspace.bindNative().then((f) => (offNative = f));
    let offPlatform: (() => void) | undefined;
    void platform.start().then((f) => (offPlatform = f));
    const onFocus = () => { if (app.account) { void app.sync(true); void app.refreshUnread(); } };
    const iv = window.setInterval(() => { if (document.hasFocus() && app.account) void app.sync(true); }, 5 * 60 * 1000);
    const iv2 = window.setInterval(() => { if (document.hasFocus() && app.account) void app.refreshUnread(); }, 60 * 1000);
    window.addEventListener("focus", onFocus);
    return () => { window.removeEventListener("focus", onFocus); clearInterval(iv); clearInterval(iv2); offClose?.(); offNative?.(); offPlatform?.(); };
  });

  // Sidebar Mail / Calendar switch <-> workspace panes (idempotent; only a real change of app.view acts).
  let lastView = app.view;
  $effect(() => {
    const v = app.view, ready = workspace.ready;
    untrack(() => { if (ready && v !== lastView) workspace.showView(v); lastView = v; });
  });
  $effect(() => {
    const k = workspace.focused?.kind;
    untrack(() => {
      const v = k === "calendar" ? "calendar" : k === "mailbox" || k === "reader" ? "mail" : null;
      if (v && v !== app.view) { lastView = v; app.view = v; }
    });
  });

  let gPending = 0;
  function keydown(e: KeyboardEvent) {
    if (e.defaultPrevented) return;
    if (workspace.key(e)) return;
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (app.confirmReq || workspace.closeReq || workspace.drag || !app.account || cal.modalOpen) return;
    const fk = workspace.focused?.kind;
    if (fk === "compose" || fk === "event" || fk === "aichat") return;
    const t = e.target as HTMLElement | null;
    if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable)) return;
    // `g c` / `g m` view chords (1.2 s window)
    if (gPending && Date.now() - gPending < 1200) {
      gPending = 0;
      if (e.key === "c") { app.view = "calendar"; e.preventDefault(); return; }
      if (e.key === "m") { app.view = "mail"; e.preventDefault(); return; }
    }
    if (e.key === "g") { gPending = Date.now(); e.preventDefault(); return; }
    if (fk === "calendar") {
      if (t?.tagName === "BUTTON" && (e.key === "Enter" || e.key === " ")) return;
      switch (e.key) {
        case "t": cal.today(); break;
        case "n": cal.newEvent(); break;
        case "ArrowLeft": cal.step(-1); break;
        case "ArrowRight": cal.step(1); break;
        case "1": cal.setMode("day"); break;
        case "2": cal.setMode("week"); break;
        case "3": cal.setMode("month"); break;
        case "4": cal.setMode("agenda"); break;
        case "Escape": if (cal.popover) cal.popover = null; else return; break;
        default: return;
      }
      e.preventDefault();
      return;
    }
    switch (e.key) {
      case "j": app.move(1); break;
      case "k": app.move(-1); break;
      case "Enter": if (t?.tagName === "BUTTON") return; if (app.selectedUid != null) void app.open(app.selectedUid); break;
      case "r": void replyTo(false); break;
      case "a": void app.moveTo("archive"); break;
      case "#": void app.moveTo("trash"); break;
      case "c": replyFresh(); break;
      case "/": document.getElementById("search")?.focus(); break;
      case "?": platform.showCheat = true; break;
      default: return;
    }
    e.preventDefault();
  }
</script>

<svelte:window onkeydown={keydown} onresize={() => layout.fit()} />
<div class="drag" data-tauri-drag-region></div>

{#if !app.ready}
  <div class="boot"></div>
{:else if app.accounts.length === 0 || app.addingAccount}
  <Onboarding cancellable={app.accounts.length > 0} />
{:else}
  <div class="shell">
    <Sidebar />
    <Divider which="sidebar" label="Resize folder pane" />
    {#if workspace.ready}<Workspace />{/if}
  </div>
{/if}

{#if cal.zoomOpen}<ZoomSettings />{/if}
{#if cal.addOpen}<AddCalendar />{/if}
{#if contactsUi.open}<Contacts />{/if}
{#if app.showAbout}<About />{/if}
{#if app.showSettings}<Settings />{/if}
{#if platform.showCheat}<ShortcutSheet />{/if}
<Overlays />
<OutboxUi />
<Tier2Ui />

<style>
  .drag { position: fixed; top: 0; left: 0; right: 0; height: 28px; padding-left: 80px; z-index: 10; }
  .boot { height: 100%; background: var(--bg); }
  .shell { display: flex; height: 100%; }
</style>
