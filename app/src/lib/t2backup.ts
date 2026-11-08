// Settings backup: gather the sections the UI can read, and apply a validated backup file. IO lives here so the pure
// pieces (lib/t2model.ts) stay testable. No passwords, tokens or API keys are ever read: the sections are
// preferences, signatures, identities, rules, saved searches and the platform/sync settings, and the backend
// (backup.rs) refuses any document with secret-looking fields.
import { api } from "./api";
import { aiApi } from "./aiapi";
import { platformApi } from "./platformapi";
import { prefs } from "./prefs.svelte";
import { DEFAULTS, validate } from "./prefs-model";
import { loadSaved, parseSaved, storeSaved } from "./savedsearch";
import { org } from "./org.svelte";
import { t2, type BackupPreview } from "./t2api";
import { applicable, ruleKey } from "./t2model";
import { describeIdentities, planDefaults, planIdentities, planSignatures } from "./t2restore";
import type { Identity, SigDefaults, Signature } from "./types";

const PLATFORM_KEYS = ["dockBadge", "trayShow", "trayStyle", "widgetShow", "widgetPreviews", "widgetAccount", "notify", "notifyPreview", "notifySound", "notifyOff"] as const;

/** Read every section. A section that cannot be read is left out (the backup is still useful without it). */
export async function gather(): Promise<Record<string, unknown>> {
  const out: Record<string, unknown> = { prefs: $state.snapshot(prefs.v), savedSearches: loadSaved() };
  try {
    const p = await platformApi.getPrefs() as unknown as Record<string, unknown>;
    out.platform = Object.fromEntries(PLATFORM_KEYS.map((k) => [k, p[k]])); // window positions stay out
  } catch { /* skip */ }
  try { out.sync = await api.getSyncSettings(); } catch { /* skip */ }
  try {
    const c = await api.composerData();
    out.signatures = c.signatures; out.signatureDefaults = c.defaults; out.identities = c.identities;
  } catch { /* skip */ }
  try {
    // `valid` and the description are derived by the app; only the definition travels.
    out.rules = (await aiApi.rulesList()).filter((r) => r.valid && r.spec).map((r) => ({ account: r.account, name: r.name, enabled: r.enabled, spec: r.spec }));
  } catch { /* skip */ }
  return out;
}

export async function exportBackup(): Promise<string> {
  return t2.backupExport(await gather());
}

/** Apply the ticked sections of a validated backup. Returns one line per section for the result note. */
export async function apply(p: BackupPreview, keys: string[], accounts: string[], opts: { replyTo?: boolean } = {}): Promise<string[]> {
  const d = p.data as Record<string, any>;
  const done: string[] = [];
  const on = (k: string) => keys.includes(k) && d[k] != null;
  if (on("prefs")) {
    const v = validate(d.prefs);
    for (const k of Object.keys(DEFAULTS) as (keyof typeof DEFAULTS)[]) prefs.set(k, v[k] as never);
    done.push("Preferences restored.");
  }
  if (on("platform")) {
    const cur = await platformApi.getPrefs();
    const next: Record<string, unknown> = { ...cur };
    for (const k of PLATFORM_KEYS) if (k in d.platform) next[k] = d.platform[k];
    if (next.widgetAccount && !accounts.includes(next.widgetAccount as string)) next.widgetAccount = null;
    next.notifyOff = (Array.isArray(next.notifyOff) ? next.notifyOff : []).filter((a: unknown) => typeof a === "string" && accounts.includes(a));
    await platformApi.setPrefs(next as never);
    done.push("Dock, menu bar, widget and notification settings restored.");
  }
  if (on("sync")) { await api.setSyncSettings(d.sync); done.push("Sync settings restored."); }
  if (on("savedSearches")) {
    const incoming = parseSaved(JSON.stringify(d.savedSearches));
    const have = new Set(org.saved.map((s) => s.name.toLowerCase()));
    const add = incoming.filter((s) => !have.has(s.name.toLowerCase()));
    org.saved = [...org.saved, ...add].slice(0, 50); storeSaved(org.saved);
    done.push(`${add.length} saved search${add.length === 1 ? "" : "es"} added.`);
  }
  // Signature ids in the backup may collide with ids here: never overwrite, remap instead.
  const sigMap = new Map<string, string>();
  const needSigs = on("signatures") || on("signatureDefaults") || on("identities");
  if (needSigs) {
    const cur = await api.composerData();
    for (const s of cur.signatures) sigMap.set(s.id, s.id); // ids that already exist here stay valid
    if (on("signatures")) {
      let n = 0;
      for (const a of planSignatures(d.signatures as Signature[], cur.signatures)) {
        if (a.kind === "skip") continue;
        try { const saved = await api.saveSignature(a.sig); sigMap.set(a.origId, saved.id); n++; } catch { /* skip one bad item */ }
      }
      done.push(`${n} signature${n === 1 ? "" : "s"} restored (existing ones were not changed).`);
    }
    if (on("signatureDefaults")) {
      let n = 0;
      for (const s of planDefaults(d.signatureDefaults as SigDefaults[], accounts, sigMap)) { try { await api.setSignatureDefaults(s); n++; } catch { /* skip */ } }
      done.push(`Default signatures set for ${n} account${n === 1 ? "" : "s"}.`);
    }
    if (on("identities")) {
      let n = 0;
      for (const i of planIdentities(d.identities as Identity[], cur.identities.map((x) => x.id), accounts, sigMap, !!opts.replyTo)) {
        try { await api.saveIdentity(i); n++; }
        catch { try { await api.saveIdentity({ ...i, signatureId: null }); n++; } catch { /* skip */ } }
      }
      done.push(`${n} identit${n === 1 ? "y" : "ies"} restored${opts.replyTo ? "" : " (reply-to addresses left empty)"}.`);
    }
  }
  if (on("rules")) {
    const existing = new Set((await aiApi.rulesList()).map(ruleKey));
    let n = 0;
    for (const r of d.rules as { account: string; name: string; spec: never }[]) {
      if (!accounts.includes(r.account) || existing.has(ruleKey(r))) continue;
      try { await aiApi.ruleSave({ id: null, account: r.account, name: r.name, spec: r.spec }); n++; } catch { /* skip */ }
    }
    done.push(`${n} rule${n === 1 ? "" : "s"} added (switched off: review and enable them under AI > Rules).`);
  }
  return done;
}

export { applicable };
