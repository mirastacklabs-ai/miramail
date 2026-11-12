// Reactive copy of the platform preferences (Dock badge, menu-bar item, widget, notifications) for the Preferences UI.
import { app } from "./state.svelte";
import { errMsg } from "./api";
import { DEFAULT_PREFS, onPrefsChanged, platformApi, type PlatformPrefs } from "./platformapi";

class PlatformPrefsStore {
  v = $state<PlatformPrefs>({ ...DEFAULT_PREFS });
  loaded = $state(false);
  /** False where the backend has no notification support (iPhone until phase 3): the Notifications UI disables its toggle. */
  notificationsAvailable = $state(true);

  async load() {
    try { this.notificationsAvailable = (await platformApi.platformInfo()).notifications; } catch { /* keep the default */ }
    try { this.v = await platformApi.getPrefs(); this.loaded = true; } catch (e) { app.notify(errMsg(e), "error"); }
  }
  /** Save a change; the backend sanitises and applies it (Dock, tray, widget window) and returns the stored value. */
  async patch(change: Partial<PlatformPrefs>) {
    // Always send the full, current record: the backend also owns window geometry, so refresh it first.
    const prev = this.v;
    this.v = { ...prev, ...change };
    try {
      const fresh = await platformApi.getPrefs();
      this.v = await platformApi.setPrefs({ ...fresh, ...change });
    } catch (e) { this.v = prev; app.notify(errMsg(e), "error"); }
  }
  /** Start listening for backend-side changes (widget "Hide" / account choice). */
  async watch(): Promise<() => void> {
    return onPrefsChanged(() => { void this.load(); });
  }
}
export const platformPrefs = new PlatformPrefsStore();
