<script lang="ts">
  import { untrack } from "svelte";
  import { app } from "../lib/state.svelte";
  import Select from "./Select.svelte";
  import DateTimeSelect from "./DateTimeSelect.svelte";
  import { errMsg } from "../lib/api";
  import { t } from "../lib/i18n";
  import { vacationApi } from "../lib/vacationapi";
  import { defaultSettings, overrideFor, parseAddresses, validate, type VacationSettings, type VacationStatus } from "../lib/vacation";

  let account = $state(app.accounts[0]?.email ?? "");
  let status = $state<VacationStatus | null>(null);
  let busy = $state(false);
  const first = app.accounts[0]?.email ?? "";
  let form = $state<VacationSettings>(defaultSettings(first));
  let addrText = $state(first);
  let restore = $state(true);
  let err = $state("");
  let srv = $state({ host: "", port: "4190", tls: "starttls" as "starttls" | "implicit" });
  let seq = 0;

  const ov = $derived(overrideFor(srv));
  const ready = $derived(status?.kind === "ready" ? status : null);
  const problem = $derived((() => { const k = validate({ ...form, addresses: parseAddresses(addrText) }); return k ? t(k) : ""; })());

  async function load() {
    if (!account) return;
    const mine = ++seq;
    busy = true; err = ""; status = null;
    try {
      const s = await vacationApi.status(account, ov);
      if (mine !== seq) return;
      status = s;
      if (s.kind === "ready") {
        form = s.settings ? { ...s.settings } : defaultSettings(account);
        addrText = form.addresses.join(", ");
      }
    } catch (e) { if (mine === seq) err = errMsg(e); }
    if (mine === seq) busy = false;
  }
  $effect(() => { void account; untrack(() => { void load(); }); });

  async function save() {
    if (problem || !ready) return;
    const settings: VacationSettings = { ...form, subject: form.subject.trim(), start: form.start || null, end: form.end || null, days: Number(form.days), addresses: parseAddresses(addrText) };
    busy = true; err = "";
    try {
      let r = await vacationApi.enable(account, settings, false, ov);
      if (r.kind === "needs_confirm") {
        const yes = await app.ask({ title: t("vac.confirm.title"), body: t("vac.confirm.body", { name: r.active }), ok: t("vac.confirm.go") });
        if (!yes) { busy = false; return; }
        r = await vacationApi.enable(account, settings, true, ov);
      }
      app.notify(t("vac.saved"));
      await load();
    } catch (e) { err = errMsg(e); busy = false; }
  }

  async function turnOff() {
    busy = true; err = "";
    try {
      const r = await vacationApi.disable(account, restore, ov);
      app.notify(r.restored ? t("vac.restored", { name: r.restored }) : t("vac.disabled"));
      await load();
    } catch (e) { err = errMsg(e); busy = false; }
  }
</script>

<section aria-labelledby="vac-h" aria-busy={busy}>
  <h3 id="vac-h">{t("vac.title")}</h3>
  <p class="note">{t("vac.intro")}</p>

  {#if app.accounts.length > 1}
    <div class="field"><label class="label" for="vac-acct">{t("vac.account")}</label>
      <Select id="vac-acct" value={account} options={app.accounts.map((a) => ({ value: a.email, label: a.email }))} onchange={(v) => (account = v)} /></div>
  {/if}

  <div role="status">
    {#if busy && !status}<p class="note">{t("vac.checking")}</p>{/if}
    {#if status?.kind === "unsupported"}
      <div class="box"><strong>{t("vac.unsupported")}</strong><p class="note selectable">{status.reason}</p></div>
    {/if}
    {#if ready}<p class="state" class:on={ready.active}>{ready.active ? t("vac.status.on", { account }) : t("vac.status.off")}</p>{/if}
  </div>
  {#if err}<p class="err selectable" role="alert">{err}</p>{/if}

  {#if ready}
    <form class="form" onsubmit={(e) => { e.preventDefault(); void save(); }}>
      <div class="field"><label class="label" for="vac-sub">{t("vac.subject")}</label>
        <input id="vac-sub" maxlength="200" placeholder={t("vac.subject.ph")} bind:value={form.subject} /></div>
      <div class="field"><label class="label" for="vac-msg">{t("vac.message")}</label>
        <textarea id="vac-msg" rows="6" maxlength="4000" aria-describedby={problem ? "vac-prob" : undefined} bind:value={form.message}></textarea></div>

      {#if ready.datesSupported}
        <div class="dates">
          <div class="field"><label class="label" for="vac-start">{t("vac.start")}</label><DateTimeSelect mode="date" idPrefix="vac-start" ariaPrefix="Start" allowEmpty days={400} value={form.start ?? ""} onchange={(v) => (form.start = v || null)} /></div>
          <div class="field"><label class="label" for="vac-end">{t("vac.end")}</label><DateTimeSelect mode="date" idPrefix="vac-end" ariaPrefix="End" allowEmpty days={400} value={form.end ?? ""} onchange={(v) => (form.end = v || null)} /></div>
        </div>
        <p class="note">{t("vac.dates.note")}</p>
      {:else}
        <p class="note">{t("vac.dates.unsupported")}</p>
      {/if}

      <div class="field"><label class="label" for="vac-days">{t("vac.days")}</label>
        <input id="vac-days" type="number" min="1" max="30" step="1" bind:value={form.days} style="max-width:100px" /></div>
      <label class="chk"><input type="checkbox" bind:checked={form.onlyToMe} /> {t("vac.onlyToMe")}</label>
      {#if form.onlyToMe}
        <div class="field"><label class="label" for="vac-addr">{t("vac.addresses")}</label><input id="vac-addr" bind:value={addrText} /></div>
      {/if}

      {#if problem}<p class="err" id="vac-prob">{problem}</p>{/if}

      <details class="adv">
        <summary>{t("vac.advanced")}</summary>
        <div class="advrow">
          <div class="field"><label class="label" for="vac-host">{t("vac.host")}</label><input id="vac-host" placeholder={app.accounts.find((a) => a.email === account)?.imap.host ?? ""} bind:value={srv.host} /></div>
          <div class="field"><label class="label" for="vac-port">{t("vac.port")}</label><input id="vac-port" inputmode="numeric" bind:value={srv.port} style="max-width:90px" /></div>
          <div class="field"><label class="label" for="vac-tls">{t("vac.tls")}</label>
            <Select id="vac-tls" value={srv.tls} options={[{ value: "starttls", label: t("vac.tls.starttls") }, { value: "implicit", label: t("vac.tls.implicit") }]} onchange={(v) => (srv.tls = v as typeof srv.tls)} /></div>
        </div>
        <p class="note">{t("vac.sieveNote")}</p>
        <button type="button" class="btn ghost sm" onclick={load} disabled={busy}>{t("vac.recheck")}</button>
      </details>

      <div class="acts">
        <button class="btn" type="submit" disabled={busy || !!problem}>{ready.active ? t("vac.update") : t("vac.on")}</button>
        {#if ready.active}
          <button type="button" class="btn ghost" onclick={turnOff} disabled={busy}>{t("vac.turnOff")}</button>
        {/if}
      </div>
      {#if ready.active}
        <label class="chk"><input type="checkbox" bind:checked={restore} /> {t("vac.restore")}</label>
      {/if}
    </form>
  {:else if status?.kind === "unsupported"}
    <button class="btn ghost sm" onclick={load} disabled={busy}>{t("vac.recheck")}</button>
  {:else if err && !status}
    <!-- The status check failed after sign-in: keep Turn off reachable so a responder that is on can still be stopped. -->
    <div class="acts">
      <button class="btn ghost sm" onclick={load} disabled={busy}>{t("vac.recheck")}</button>
      <button class="btn ghost sm" onclick={turnOff} disabled={busy}>{t("vac.turnOff")}</button>
    </div>
  {/if}
</section>

<style>
  section { display: grid; gap: 12px; }
  h3 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; text-transform: uppercase; color: var(--muted); }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .form { display: grid; gap: 12px; }
  .box { border: 1px solid var(--line); border-radius: var(--radius); padding: 12px 14px; display: grid; gap: 6px; background: var(--surface); }
  .state { margin: 0; font-size: 0.9333rem; color: var(--muted); }
  .state.on { color: var(--accent); font-weight: 600; }
  .err { margin: 0; color: var(--danger); font-size: 0.8667rem; }
  .dates { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.9333rem; }
  .chk input { accent-color: var(--accent); }
  .acts { display: flex; gap: 10px; }
  .adv { border: 1px solid var(--line); border-radius: var(--radius); padding: 8px 12px; }
  .adv summary { cursor: pointer; font-size: 0.8rem; text-transform: uppercase; letter-spacing: .1em; color: var(--muted); }
  .advrow { display: flex; flex-wrap: wrap; gap: 12px; margin: 10px 0; }
  .advrow .field:first-child { flex: 1; min-width: 180px; }
</style>
