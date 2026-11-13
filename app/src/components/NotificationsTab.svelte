<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../lib/state.svelte";
  import { errMsg } from "../lib/api";
  import { platformPrefs } from "../lib/platformprefs.svelte";
  import { phone } from "../lib/phone.svelte";

  const p = $derived(platformPrefs.v);
  let busy = $state(false);
  onMount(() => { void platformPrefs.load(); });

  const accountOn = (email: string) => !p.notifyOff.includes(email);
  function toggleAccount(email: string, on: boolean) {
    const off = new Set(p.notifyOff);
    if (on) off.delete(email); else off.add(email);
    void platformPrefs.patch({ notifyOff: [...off] });
  }
  const vipOnly = (email: string) => p.notifyVipOnly.includes(email);
  function toggleVipOnly(email: string, on: boolean) {
    const v = new Set(p.notifyVipOnly);
    if (on) v.add(email); else v.delete(email);
    void platformPrefs.patch({ notifyVipOnly: [...v] });
  }
  // iPhone: explain what will happen (in the page) before iOS shows its own permission prompt.
  let explain = $state(false);
  function toggleNotify(on: boolean) {
    if (on && phone.on) { explain = true; return; }
    void platformPrefs.patch({ notify: on });
  }
  function allow() { explain = false; void platformPrefs.patch({ notify: true }); }
  async function test() {
    busy = true;
    try { const { platformApi } = await import("../lib/platformapi"); await platformApi.notifyTest(); app.notify(phone.on ? "Test notification sent. If nothing appears, allow MiraMail in the Settings app > Notifications." : "Test notification sent. If nothing appears, allow MiraMail in System Settings > Notifications."); }
    catch (e) { app.notify(errMsg(e), "error"); }
    busy = false;
  }
</script>

<section aria-labelledby="nt-h">
  <h3 id="nt-h">Notifications</h3>
  {#if phone.on}
    <p class="note">MiraMail checks your mail in the background and shows a notification on this iPhone when new mail reaches your Inbox. There is no push service and no server of ours: iOS wakes MiraMail every so often (about every 15 minutes at best, and less often when the battery is low or Background App Refresh is off), and MiraMail asks your own mail server directly. New mail is not announced the moment it arrives. Notifications are not shown while MiraMail is open.</p>
  {:else}
    <p class="note">MiraMail uses the macOS notification center on this Mac. There is no push service; you are notified when new mail reaches your Inbox while MiraMail is running. Notifications are not shown while a MiraMail window is in front.</p>
  {/if}

  {#if !platformPrefs.notificationsAvailable}<p class="note">New-mail notifications aren't available on this system.</p>{/if}
  <label class="chk"><input type="checkbox" checked={p.notify} disabled={!platformPrefs.loaded || !platformPrefs.notificationsAvailable} onchange={(e) => { const on = e.currentTarget.checked; if (on && phone.on) e.currentTarget.checked = false; toggleNotify(on); }} /> Notify me about new mail</label>

  {#if explain}
    <div class="explain" role="group" aria-label="Before you allow notifications">
      <p>iOS will now ask whether MiraMail may send you notifications. If you allow it, MiraMail checks your Inbox in the background and notifies you about new mail: the sender and subject (you can switch that to just "New mail" below), never the message text. Your mail stays between this iPhone and your mail server.</p>
      <div class="acts-row"><button class="btn sm" onclick={allow}>Continue</button><button class="btn ghost sm" onclick={() => (explain = false)}>Not now</button></div>
    </div>
  {/if}

  {#if p.notify}
    <label class="chk sub"><input type="checkbox" checked={p.notifyPreview} onchange={(e) => platformPrefs.patch({ notifyPreview: e.currentTarget.checked })} /> Show sender and subject (off: just "New mail")</label>
    <label class="chk sub"><input type="checkbox" checked={p.notifySound} onchange={(e) => platformPrefs.patch({ notifySound: e.currentTarget.checked })} /> Play a sound</label>
    <div class="sub accts" role="group" aria-label="Accounts to notify for">
      <span class="label">Accounts</span>
      {#each app.accounts as a (a.email)}
        <div class="acct">
          <label class="chk"><input type="checkbox" checked={accountOn(a.email)} onchange={(e) => toggleAccount(a.email, e.currentTarget.checked)} /> {a.email}</label>
          {#if accountOn(a.email)}<label class="chk vo"><input type="checkbox" checked={vipOnly(a.email)} onchange={(e) => toggleVipOnly(a.email, e.currentTarget.checked)} /> VIP only</label>{/if}
        </div>
      {/each}
    </div>
    <div class="acts"><button class="btn ghost sm" onclick={test} disabled={busy}>Send a test notification</button></div>
    <p class="note">“VIP only” notifies just for senders on your VIP list (Preferences > Senders; star a sender from the message list). Muted conversations and blocked senders never notify.</p>
    {#if phone.on}
      <p class="note">Tapping a notification opens that message (with "just New mail" previews it only selects it, so it is not marked read unseen). If notifications don't appear, check the Settings app > Notifications > MiraMail and Settings > General > Background App Refresh.</p>
    {:else}
      <p class="note">Following a notification (clicking it) brings MiraMail forward and opens that message.</p>
    {/if}
  {/if}
</section>

<style>
  section { display: grid; gap: 10px; }
  h3 { margin: 0; font-size: 0.8rem; letter-spacing: .12em; text-transform: uppercase; color: var(--muted); }
  .note { margin: 0; font-size: 0.8667rem; color: var(--muted); line-height: 1.5; }
  .chk { display: flex; gap: 8px; align-items: center; font-size: 0.9333rem; }
  .chk input { accent-color: var(--accent); }
  .sub { margin-left: 24px; }
  .accts { display: grid; gap: 6px; }
  .acts { margin-left: 24px; }
  .explain { border: 1px solid var(--line); border-radius: 10px; padding: 12px; display: grid; gap: 10px; background: var(--bg-band); }
  .explain p { margin: 0; font-size: 0.9rem; line-height: 1.5; }
  .acts-row { display: flex; gap: 10px; flex-wrap: wrap; }
  .acct { display: flex; gap: 16px; align-items: center; flex-wrap: wrap; }
  .vo { font-size: 0.8667rem; color: var(--muted); }
</style>
