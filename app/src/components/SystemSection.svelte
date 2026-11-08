<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/state.svelte";
  import { errMsg } from "../lib/api";
  import { platformApi } from "../lib/platformapi";

  let login = $state(false);
  let ready = $state(false);
  let loginSupported = $state(false);
  onMount(async () => {
    try { loginSupported = (await platformApi.platformInfo()).launchAtLogin; } catch { loginSupported = false; }
    if (loginSupported) { try { login = await platformApi.getLaunchAtLogin(); } catch { /* not available in this build */ } }
    ready = true;
  });
  async function setLogin(on: boolean) {
    try { login = await platformApi.setLaunchAtLogin(on); } catch (e) { login = !on; app.notify(errMsg(e), "error"); }
  }
</script>

<section aria-labelledby="sy-h">
  <h3 id="sy-h">System</h3>
  {#if loginSupported}
    <label class="chk"><input type="checkbox" checked={login} disabled={!ready} onchange={(e) => setLogin(e.currentTarget.checked)} /> Open MiraMail when I log in</label>
  {/if}

  <h3>Default mail app</h3>
  <p class="note">MiraMail can open <span class="mono">mailto:</span> links (the "email" links on web pages and in other apps) in a new, filled-in message. It never changes your Mac's default by itself. To make it the default: open the Mail app, choose Mail > Settings > General, and pick MiraMail under "Default email reader". (If Mail isn't installed, use the default email setting in the app you use for it.)</p>
</section>

<style>
  section { display: grid; gap: 10px; }
  h3 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; text-transform: uppercase; color: var(--muted); }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.9333rem; }
  .chk input { accent-color: var(--accent); }
</style>
