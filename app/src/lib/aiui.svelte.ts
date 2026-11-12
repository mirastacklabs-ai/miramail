// Tiny cross-component hooks for the AI feature (kept separate so Settings / Workspace pull in almost nothing at idle).
import { app } from "./state.svelte";
import { phone } from "./phone.svelte";

export const aiUi = $state({ settingsTab: null as string | null });

/** Open Preferences on the AI tab. */
export function openAiSettings() {
  // Phone: the Settings tab's MIRA section (an inline screen), not the Mac modal.
  if (phone.on) { phone.section("ai"); return; }
  aiUi.settingsTab = "ai";
  app.showSettings = true;
}
/** Take (and clear) the tab Preferences should start on. */
export function takeSettingsTab(): string | null {
  const t = aiUi.settingsTab;
  aiUi.settingsTab = null;
  return t;
}
export const AI_FOCUS_EVENT = "miramail:ai-focus";
