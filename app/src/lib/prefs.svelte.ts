import { cssVars, DEFAULTS, parse, serialize, STORAGE_KEY, validate, type Prefs } from "./prefs-model";

function read(): string | null { try { return localStorage.getItem(STORAGE_KEY); } catch { return null; } }
function write(v: string) { try { localStorage.setItem(STORAGE_KEY, v); } catch { /* storage unavailable: prefs last for this session */ } }

/** Appearance and behaviour preferences. Reactive; applied live to :root and persisted. No secrets here. */
class PrefsStore {
  v = $state<Prefs>({ ...DEFAULTS });

  /** Load saved prefs and apply them. Call once at startup. */
  init() { this.v = parse(read()); this.apply(); }
  set<K extends keyof Prefs>(key: K, value: Prefs[K]) { this.v = validate({ ...this.v, [key]: value }); this.apply(); write(serialize(this.v)); }
  reset() { this.v = { ...DEFAULTS }; this.apply(); write(serialize(this.v)); }
  private apply() {
    const st = document.documentElement.style;
    for (const [k, val] of Object.entries(cssVars(this.v))) st.setProperty(k, val);
  }
}
export const prefs = new PrefsStore();
