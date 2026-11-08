// Resizable pane widths (sidebar | message list | reader). Persisted per user in localStorage.
function lsGet(k: string): string | null { try { return localStorage.getItem(k); } catch { return null; } }
function lsSet(k: string, v: string) { try { localStorage.setItem(k, v); } catch { /* ignore */ } }

import { DEFAULTS, LIMITS, clampWidth } from "./layout-math";
export { DEFAULTS, LIMITS, READER_MIN, clampWidth } from "./layout-math";

class Layout {
  sidebar = $state<number>(DEFAULTS.sidebar);
  list = $state<number>(DEFAULTS.list);
  dragging = $state(false);

  constructor() {
    const s = Number(lsGet("mm.w.sidebar")), l = Number(lsGet("mm.w.list"));
    if (Number.isFinite(s) && s > 0) this.sidebar = Math.min(Math.max(s, LIMITS.sidebar[0]), LIMITS.sidebar[1]);
    if (Number.isFinite(l) && l > 0) this.list = Math.min(Math.max(l, LIMITS.list[0]), LIMITS.list[1]);
  }
  vw() { return typeof window === "undefined" ? 1400 : window.innerWidth; }
  set(which: "sidebar" | "list", w: number) {
    const other = which === "sidebar" ? this.list : this.sidebar;
    this[which] = clampWidth(which, w, other, this.vw());
  }
  nudge(which: "sidebar" | "list", delta: number) { this.set(which, this[which] + delta); this.save(); }
  reset(which: "sidebar" | "list") { this.set(which, DEFAULTS[which]); this.save(); }
  save() { lsSet("mm.w.sidebar", String(this.sidebar)); lsSet("mm.w.list", String(this.list)); }
  /** Re-clamp after the window shrinks so the reader never gets squeezed. */
  fit() { this.set("list", this.list); this.set("sidebar", this.sidebar); }
}
export const layout = new Layout();
