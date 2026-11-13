<script lang="ts">
  import { keychainName } from "../lib/device";
  import { api } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { cal } from "../lib/cal.svelte";
  import Modal from "./Modal.svelte";

  let clientId = $state(cal.zoom?.clientId ?? "");
  let secret = $state("");
  let busy = $state<"" | "save" | "connect" | "disconnect">("");
  let err = $state("");
  const z = $derived(cal.zoom);
  const redirect = $derived(z?.redirectUri ?? "http://127.0.0.1:53682/callback");
  const SCOPES = "meeting:write:meeting user:read:user";

  async function copy(text: string, what: string) {
    try { await navigator.clipboard.writeText(text); app.notify(`${what} copied`); } catch { app.notify("Copy failed. Select the text and copy it manually.", "error"); }
  }
  async function saveClient(): Promise<boolean> {
    if (!clientId.trim()) { err = "Enter the Client ID from your Zoom app."; return false; }
    try { await api.zoomSetClient(clientId.trim(), secret.trim() || undefined); secret = ""; await cal.refreshZoom(); return true; }
    catch (e) { err = typeof e === "string" ? e : "Could not save the Zoom client."; return false; }
  }
  async function save() { err = ""; busy = "save"; if (await saveClient()) app.notify("Zoom client saved"); busy = ""; }
  async function connect() {
    err = ""; busy = "connect";
    if (await saveClient()) {
      try { cal.zoom = await api.zoomConnect(); app.notify("Zoom connected"); }
      catch (e) { err = typeof e === "string" ? e : "Zoom connection failed."; await cal.refreshZoom(); }
    }
    busy = "";
  }
  async function disconnect() {
    err = ""; busy = "disconnect";
    try { await api.zoomDisconnect(); await cal.refreshZoom(); app.notify("Zoom disconnected"); } catch (e) { err = typeof e === "string" ? e : "Could not disconnect."; }
    busy = "";
  }
  const close = () => { secret = ""; cal.zoomOpen = false; };
</script>

<Modal title="Zoom settings" onclose={close} width={600}>
  <p class="priv"><strong>Zoom is contacted only after you press Connect.</strong> Until then MiraMail makes no request to zoom.us. Tokens are kept in the {keychainName()}; MiraMail has no server of its own.</p>

  <div class="status" role="status">
    <span class="zd" class:on={z?.connected}></span>
    <span class="mono">{z?.connected ? `Connected${z.account ? " as " + z.account : ""}` : z?.configured ? "Client saved, not connected" : "Not set up"}</span>
  </div>

  <ol class="steps">
    <li>Open <span class="mono">marketplace.zoom.us</span> in your browser, sign in, then choose <em>Develop &gt; Build App</em>.</li>
    <li>Create a <strong>General App</strong> and set the type to <strong>User-managed</strong>. Keep <em>Use PKCE</em> (OAuth with PKCE) on.</li>
    <li>Under <em>OAuth Redirect URL</em> and <em>Allow list</em>, add this exact address:
      <div class="copy"><code class="mono selectable">{redirect}</code><button class="btn ghost sm" onclick={() => copy(redirect, "Redirect URI")}>Copy</button></div></li>
    <li>Under <em>Scopes</em>, add:
      <div class="copy"><code class="mono selectable">{SCOPES}</code><button class="btn ghost sm" onclick={() => copy(SCOPES, "Scopes")}>Copy</button></div></li>
    <li>Copy the <strong>Client ID</strong> (and the Client Secret if Zoom shows one) below, then press Connect. Your browser opens Zoom to approve access; you have 3 minutes.</li>
  </ol>

  <div class="field"><label class="label" for="z-id">Client ID</label><input id="z-id" data-autofocus bind:value={clientId} spellcheck="false" autocomplete="off" /></div>
  <div class="field"><label class="label" for="z-s">Client Secret <span class="opt">(optional with PKCE)</span></label>
    <input id="z-s" type="password" bind:value={secret} autocomplete="off" placeholder={z?.configured ? "Saved. Leave blank to keep it." : ""} /></div>

  {#if err}<p class="err selectable" role="alert">{err}</p>{/if}
  <div class="acts">
    {#if z?.connected}<button class="btn danger" disabled={!!busy} onclick={disconnect}>{busy === "disconnect" ? "Disconnecting…" : "Disconnect"}</button>{/if}
    <span class="grow"></span>
    <button class="btn ghost" disabled={!!busy} onclick={save}>{busy === "save" ? "Saving…" : "Save"}</button>
    <button class="btn" disabled={!!busy} onclick={connect}>{busy === "connect" ? "Waiting for Zoom…" : z?.connected ? "Reconnect" : "Connect"}</button>
  </div>
</Modal>

<style>
  .priv { margin: 0; font-size: 13px; border: 1px solid var(--line); border-left: 2px solid var(--accent); border-radius: var(--radius); padding: 8px 12px; }
  .status { display: flex; align-items: center; gap: 8px; }
  .zd { width: 8px; height: 8px; border-radius: 50%; border: 1.5px solid var(--muted); } .zd.on { background: var(--accent); border-color: var(--accent); }
  .steps { margin: 0; padding-left: 20px; display: grid; gap: 8px; font-size: 14px; }
  .copy { display: flex; gap: 8px; align-items: center; margin-top: 6px; } code { flex: 1; border: 1px solid var(--line); border-radius: var(--radius); padding: 6px 8px; color: var(--accent); overflow-wrap: anywhere; }
  .opt { text-transform: none; letter-spacing: 0; }
  .err { margin: 0; color: var(--danger); font-size: 13px; overflow-wrap: anywhere; }
  .acts { display: flex; gap: 8px; align-items: center; } .grow { flex: 1; }
</style>
