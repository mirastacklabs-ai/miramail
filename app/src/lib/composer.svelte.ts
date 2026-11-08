// Composer data shared by Compose and Preferences: signatures, signature defaults, From identities.
import { api } from "./api";
import type { Identity, SigDefaults, Signature } from "./types";

class ComposerStore {
  signatures = $state<Signature[]>([]);
  defaults = $state<SigDefaults[]>([]);
  identities = $state<Identity[]>([]);
  loaded = $state(false);
  private p: Promise<void> | null = null;

  /** Load once (Compose waits for this before choosing a signature). A failure leaves empty lists and allows a retry. */
  ensure(): Promise<void> {
    if (!this.p) this.p = this.reload().catch(() => { this.p = null; });
    return this.p;
  }
  async reload() {
    const d = await api.composerData();
    this.signatures = d.signatures; this.defaults = d.defaults; this.identities = d.identities;
    this.loaded = true;
  }
}
export const composer = new ComposerStore();

// Per-viewer conveniences (never required for correctness).
const MODE_KEY = "miramail.compose.mode", SPELL_KEY = "miramail.compose.spell";
export function defaultMode(): "plain" | "rich" {
  // Rich is the default (owner decision 2026-11-07); someone who explicitly switched to plain keeps plain.
  try { return localStorage.getItem(MODE_KEY) === "plain" ? "plain" : "rich"; } catch { return "rich"; }
}
export function rememberMode(m: "plain" | "rich") { try { localStorage.setItem(MODE_KEY, m); } catch { /* private window */ } }
export function spellOn(): boolean {
  try { return localStorage.getItem(SPELL_KEY) !== "off"; } catch { return true; }
}
export function rememberSpell(on: boolean) { try { localStorage.setItem(SPELL_KEY, on ? "on" : "off"); } catch { /* private window */ } }
