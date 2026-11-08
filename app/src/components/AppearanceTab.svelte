<script lang="ts">
  import { app, type Theme } from "../lib/state.svelte";
  import { t } from "../lib/i18n";
  import { prefs } from "../lib/prefs.svelte";
  import { workspace, type View } from "../lib/workspace.svelte";
  import Select from "./Select.svelte";
  import { BODY_SIZE, LINE_HEIGHT, type Prefs } from "../lib/prefs-model";

  type Opt<T> = { v: T; label: string };
  const THEMES: Opt<Theme>[] = [{ v: "system", label: t("theme.system") }, { v: "dark", label: t("theme.dark") }, { v: "light", label: t("theme.light") }, { v: "contrast", label: t("theme.contrast") }];
  const UI_FONTS: Opt<Prefs["uiFont"]>[] = [{ v: "mira", label: "MiraStack (Oswald)" }, { v: "system", label: "System UI (SF)" }, { v: "inter", label: "Inter-like" }];
  const UI_SIZES: Opt<Prefs["uiSize"]>[] = [{ v: "s", label: "S 14" }, { v: "m", label: "M 15" }, { v: "l", label: "L 16" }];
  const BODY_FONTS: Opt<Prefs["bodyFont"]>[] = [{ v: "system", label: "System UI" }, { v: "serif", label: "Serif (Georgia)" }, { v: "mono", label: "Monospace" }, { v: "oswald", label: "Oswald" }];
  const DENS: Opt<Prefs["density"]>[] = [{ v: "compact", label: "Compact" }, { v: "comfortable", label: "Comfortable" }, { v: "spacious", label: "Spacious" }];
  const SNIP: Opt<Prefs["snippetLines"]>[] = [{ v: 0, label: "0" }, { v: 1, label: "1" }, { v: 2, label: "2" }];
  const DATES: Opt<Prefs["dateFormat"]>[] = [{ v: "relative", label: "Relative" }, { v: "absolute", label: "Absolute" }];
  const MARK: Opt<Prefs["markRead"]>[] = [{ v: "immediately", label: "Immediately" }, { v: "delay", label: "After 2 s" }, { v: "never", label: "Never" }];
  const AFTER: Opt<Prefs["afterRemove"]>[] = [{ v: "next", label: "Next" }, { v: "previous", label: "Previous" }, { v: "none", label: "None" }];
  const LAYOUTS: { value: View; label: string }[] = [{ value: "tiles", label: "Tiles" }, { value: "tabs", label: "Tabs" }];
  const p = $derived(prefs.v);
</script>

{#snippet seg<K extends keyof Prefs>(name: string, key: K, opts: Opt<Prefs[K]>[])}
  <div class="seg" role="radiogroup" aria-label={name}>
    {#each opts as o (o.v)}
      <button role="radio" aria-checked={p[key] === o.v} class:on={p[key] === o.v} onclick={() => prefs.set(key, o.v)}>{o.label}</button>
    {/each}
  </div>
{/snippet}

<div class="ap">
  <div class="top"><h3>Appearance</h3><button class="btn ghost sm" onclick={() => { prefs.reset(); app.setTheme("system"); }}>Reset to defaults</button></div>

  <div class="row"><span class="label" id="l-theme">{t("theme.label")}</span>
    <div class="seg" role="radiogroup" aria-labelledby="l-theme">
      {#each THEMES as o (o.v)}<button role="radio" aria-checked={app.theme === o.v} class:on={app.theme === o.v} onclick={() => app.setTheme(o.v)}>{o.label}</button>{/each}
    </div></div>
  <div class="row"><span class="label">Interface font</span>{@render seg("Interface font", "uiFont", UI_FONTS)}</div>
  <div class="row"><span class="label">Interface text size</span>{@render seg("Interface text size", "uiSize", UI_SIZES)}</div>
  <div class="row"><span class="label" id="l-layout">Layout</span>
    <div class="lay"><Select value={workspace.view} options={LAYOUTS} onchange={(v) => workspace.setView(v)} labelledby="l-layout" block={false} />
      <span class="note">Tabs show one thing at a time, like a browser. Tiles show panes side by side.</span></div></div>
  <div class="row"><span class="label">Density</span>{@render seg("Density", "density", DENS)}</div>

  <h3>Reading</h3>
  <div class="row"><span class="label">Message font</span>{@render seg("Message font", "bodyFont", BODY_FONTS)}</div>
  <div class="row"><label class="label" for="bs">Message size</label>
    <span class="rng"><input id="bs" type="range" min={BODY_SIZE.min} max={BODY_SIZE.max} step="1" value={p.bodySize} oninput={(e) => prefs.set("bodySize", +e.currentTarget.value)} /><span class="mono">{p.bodySize} px</span></span></div>
  <div class="row"><label class="label" for="lh">Line height</label>
    <span class="rng"><input id="lh" type="range" min={LINE_HEIGHT.min} max={LINE_HEIGHT.max} step="0.05" value={p.lineHeight} oninput={(e) => prefs.set("lineHeight", +e.currentTarget.value)} /><span class="mono">{p.lineHeight.toFixed(2)}</span></span></div>
  <label class="chk"><input type="checkbox" checked={p.plainMono} onchange={(e) => prefs.set("plainMono", e.currentTarget.checked)} /> Use monospace for plain text messages</label>

  <h3>Message list</h3>
  <div class="row"><span class="label">Preview lines</span>{@render seg("Preview lines", "snippetLines", SNIP)}</div>
  <div class="row"><span class="label">Date format</span>{@render seg("Date format", "dateFormat", DATES)}</div>
  <label class="chk"><input type="checkbox" checked={p.showDot} onchange={(e) => prefs.set("showDot", e.currentTarget.checked)} /> Show dot on unread messages</label>
  <label class="chk"><input type="checkbox" checked={p.threading} onchange={(e) => prefs.set("threading", e.currentTarget.checked)} /> Group messages into conversations (also the button at the top of the list)</label>

  <h3>Behaviour</h3>
  <div class="row"><span class="label">Mark as read</span>{@render seg("Mark as read", "markRead", MARK)}</div>
  <div class="row"><span class="label">After archive or delete, select</span>{@render seg("After archive or delete", "afterRemove", AFTER)}</div>

  <h3>Preview</h3>
  <div class="pv" aria-label="Preview of the chosen fonts">
    <div class="pvh"><strong>Ada Lovelace</strong><span class="mono">Notes on the Bernoulli program</span></div>
    <div class="html">Formatted mail. The quick brown fox jumps over the lazy dog, then reads the notes again at a pace that suits the eye.</div>
    <div class="plain">Plain text mail. The quick brown fox jumps over the lazy dog, then reads the notes again at a pace that suits the eye.</div>
  </div>
  <p class="note">Formatted mail uses its own styles where it has them. The sandboxed view cannot load the bundled Oswald or JetBrains Mono, so it uses a close local fallback.</p>
</div>

<style>
  .ap { display: grid; gap: 12px; min-width: 0; }
  .top { display: flex; justify-content: space-between; align-items: center; }
  h3 { font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); margin-top: 6px; }
  .top h3 { margin: 0; }
  .row { display: grid; grid-template-columns: 190px minmax(0, 1fr); gap: 10px; align-items: center; }
  .seg { display: inline-flex; flex-wrap: wrap; border: 1px solid var(--line); border-radius: var(--radius); justify-self: start; overflow: hidden; }
  .seg button { background: none; border: 0; border-bottom: 2px solid transparent; padding: 6px 12px; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .1em; color: var(--muted); }
  .seg button.on { color: var(--ink); border-bottom-color: var(--accent); background: color-mix(in srgb, var(--line) 40%, transparent); }
  .lay { display: grid; gap: 6px; justify-items: start; min-width: 0; }
  .rng { display: flex; align-items: center; gap: 12px; }
  .rng input { flex: 1; max-width: 260px; accent-color: var(--accent); }
  .rng .mono { min-width: 48px; color: var(--muted); }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.9333rem; }
  .chk input { accent-color: var(--accent); }
  .note { margin: 0; font-size: 0.8rem; color: var(--muted); line-height: 1.5; }
  .pv { border: 1px solid var(--line); border-radius: var(--radius); padding: 12px 14px; display: grid; gap: 10px; background: var(--surface); }
  .pvh { display: grid; gap: 2px; }
  .pvh strong { color: var(--ink); }
  .pvh .mono { color: var(--muted); }
  .html { font: 400 var(--body-size)/var(--body-lh) var(--html-font); color: var(--text); }
  .plain { font: 400 var(--body-size)/var(--body-lh) var(--body-font); color: var(--text); white-space: pre-wrap; border-top: 1px solid var(--line); padding-top: 10px; }
  @media (max-width: 640px) { .row { grid-template-columns: 1fr; gap: 4px; } }
</style>
