<script lang="ts">
  import { keychainName } from "../lib/device";
  import Select from "./Select.svelte";
  import { api, errMsg } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import type { Advanced, Tls } from "../lib/types";
  import { cleanAddress } from "../lib/signin";
  import Logo from "./Logo.svelte";

  let { cancellable = false }: { cancellable?: boolean } = $props();

  let email = $state("");
  let passEl: HTMLInputElement;
  let advanced = $state(false);
  let phase = $state<"idle" | "lookup" | "verify">("idle");
  let error = $state("");
  let username = $state("");
  let imapHost = $state(""), imapPort = $state(993), imapTls = $state<Tls>("implicit");
  let smtpHost = $state(""), smtpPort = $state(465), smtpTls = $state<Tls>("implicit");

  const busy = $derived(phase !== "idle");
  const progress = $derived(phase === "lookup" ? "Looking up your provider…" : phase === "verify" ? "Verifying…" : "");

  function onTls(which: "imap" | "smtp", t: Tls) {
    if (which === "imap") { imapTls = t; imapPort = t === "implicit" ? 993 : 143; }
    else { smtpTls = t; smtpPort = t === "implicit" ? 465 : 587; }
  }

  async function submit(e: Event) {
    e.preventDefault();
    if (busy) return;
    error = "";
    const addr = cleanAddress(email);
    let password = passEl.value;
    if (!addr || !password) { error = "Enter your email address and password."; return; }
    let adv: Advanced | undefined;
    if (advanced && imapHost.trim() && smtpHost.trim()) {
      adv = {
        username: username.trim() || addr,
        imap: { host: imapHost.trim(), port: Number(imapPort), tls: imapTls },
        smtp: { host: smtpHost.trim(), port: Number(smtpPort), tls: smtpTls },
      };
    } else if (advanced && (imapHost.trim() || smtpHost.trim())) {
      error = "Fill in both IMAP and SMTP hosts, or leave them empty to auto-detect."; return;
    }
    phase = adv ? "verify" : "lookup";
    const timer = adv ? 0 : window.setTimeout(() => { if (phase === "lookup") phase = "verify"; }, 1500);
    try {
      const info = await api.addAccount(addr, password, adv);
      passEl.value = "";
      await app.accountAdded(info);
    } catch (err) {
      error = errMsg(err);
    } finally {
      clearTimeout(timer);
      password = "";
      phase = "idle";
    }
    if (error) passEl.focus();
  }
</script>

<main class="wrap">
  <form class="card" onsubmit={submit} aria-busy={busy} novalidate>
    <div class="head">
      <Logo size={22} />
      <p class="tag">Type your email. Enter your password. Done.</p>
    </div>

    <div class="field">
      <label class="label" for="ob-email">Email</label>
      <input id="ob-email" type="email" autocomplete="username" autocapitalize="off" spellcheck="false" bind:value={email} disabled={busy} />
    </div>
    <div class="field">
      <label class="label" for="ob-pass">Password</label>
      <input id="ob-pass" type="password" autocomplete="current-password" bind:this={passEl} disabled={busy} />
    </div>

    {#if error}
      <div class="alert" role="alert">{error}</div>
    {/if}
    {#if busy}
      <div class="progress" role="status"><span class="bar"></span>{progress}</div>
    {/if}

    <button class="btn" type="submit" disabled={busy}>Sign in</button>

    <div class="foot">
      <button type="button" class="link" aria-expanded={advanced} aria-controls="ob-adv" onclick={() => (advanced = !advanced)}>
        Advanced {advanced ? "−" : "+"}
      </button>
      {#if cancellable}<button type="button" class="link" onclick={() => (app.addingAccount = false)}>Cancel</button>{/if}
    </div>

    {#if advanced}
      <fieldset id="ob-adv" class="adv" disabled={busy}>
        <legend class="label">Manual server settings</legend>
        <div class="field"><label class="label" for="ob-user">Username (if not your email)</label><input id="ob-user" bind:value={username} autocomplete="off" spellcheck="false" /></div>
        <div class="row">
          <div class="field grow"><label class="label" for="ob-ih">IMAP host</label><input id="ob-ih" bind:value={imapHost} placeholder="imap.example.com" spellcheck="false" /></div>
          <div class="field port"><label class="label" for="ob-ip">Port</label><input id="ob-ip" type="number" min="1" max="65535" bind:value={imapPort} /></div>
          <div class="field sec"><label class="label" for="ob-is">Security</label>
            <Select id="ob-is" value={imapTls} options={[{ value: "implicit", label: "SSL/TLS" }, { value: "starttls", label: "STARTTLS" }]} onchange={(v) => onTls("imap", v as Tls)} ariaLabel="IMAP security" /></div>
        </div>
        <div class="row">
          <div class="field grow"><label class="label" for="ob-sh">SMTP host</label><input id="ob-sh" bind:value={smtpHost} placeholder="smtp.example.com" spellcheck="false" /></div>
          <div class="field port"><label class="label" for="ob-sp">Port</label><input id="ob-sp" type="number" min="1" max="65535" bind:value={smtpPort} /></div>
          <div class="field sec"><label class="label" for="ob-ss">Security</label>
            <Select id="ob-ss" value={smtpTls} options={[{ value: "implicit", label: "SSL/TLS" }, { value: "starttls", label: "STARTTLS" }]} onchange={(v) => onTls("smtp", v as Tls)} ariaLabel="SMTP security" /></div>
        </div>
      </fieldset>
    {/if}
    <p class="note">Your password stays in the {keychainName()}. MiraMail talks to your mail servers, and may look up your email domain&rsquo;s standard mail-settings file over HTTPS to find them.</p>
  </form>
  <p class="attr">© MIRASTACK LABS PRIVATE LIMITED · Visual identity by MIRASTACK LABS</p>
</main>

<style>
  .wrap { height: 100%; display: grid; place-items: center; padding: 40px 20px 20px; background: var(--bg-band); overflow: auto; }
  .card { width: 100%; max-width: 400px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); padding: 32px; display: grid; gap: 18px; }
  .head { display: grid; gap: 10px; margin-bottom: 4px; }
  .tag { margin: 0; color: var(--muted); font-size: 14px; letter-spacing: .02em; }
  .alert { border: 1px solid var(--danger); border-left-width: 3px; color: var(--danger); border-radius: var(--radius); padding: 10px 12px; font-size: 14px; user-select: text; }
  .progress { display: flex; align-items: center; gap: 10px; color: var(--muted); font-size: 13px; letter-spacing: .06em; text-transform: uppercase; }
  .bar { width: 28px; height: 2px; background: var(--line); position: relative; overflow: hidden; }
  .bar::after { content: ""; position: absolute; inset: 0; width: 40%; background: var(--accent); animation: slide 1s ease-in-out infinite; }
  @keyframes slide { from { transform: translateX(-100%); } to { transform: translateX(250%); } }
  .foot { display: flex; justify-content: space-between; }
  .attr { margin: 18px 0 0; font: 400 11px var(--font-mono); letter-spacing: .08em; color: var(--muted); text-align: center; }
  .link { background: none; border: 0; padding: 0; color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: .12em; }
  .link:hover { color: var(--accent); }
  .adv { border: 1px solid var(--line); border-radius: var(--radius); padding: 14px; margin: 0; display: grid; gap: 12px; min-width: 0; }
  .adv legend { padding: 0 6px; }
  .row { display: flex; gap: 8px; align-items: end; }
  .grow { flex: 1; min-width: 0; } .port { width: 78px; } .sec { width: 112px; }
  .note { margin: 0; font-size: 12px; color: var(--muted); }
</style>
