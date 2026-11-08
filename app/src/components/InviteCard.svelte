<script lang="ts">
  import { api, openExternal } from "../lib/api";
  import { app } from "../lib/state.svelte";
  import { fmtSpan, localTz, occSpan } from "../lib/caldate";
  import type { Invite, RespondStatus } from "../lib/types";

  let { invite, uid, folder }: { invite: Invite; uid: number; folder: string } = $props();
  let busy = $state<string>("");
  let trust = $state(false);
  const ev = $derived(invite.event);
  const sp = $derived(occSpan(ev));
  const cancelled = $derived(invite.method === "CANCEL" || ev.status === "cancelled");
  const place = $derived(ev.location && !/^https?:\/\//i.test(ev.location) ? ev.location : "");
  const zoomUrl = $derived(ev.zoom?.joinUrl ?? (/^https:\/\/[\w.-]*zoom\.us\//i.test(ev.location) ? ev.location : null));
  const guests = $derived(ev.attendees.length);
  const STATUS: Record<string, string> = { accepted: "Accepted", tentative: "Tentative", declined: "Declined", "needs-action": "Awaiting your reply" };

  function patch(p: Partial<Invite>) { if (app.body?.invite) Object.assign(app.body.invite, p); }
  async function respond(s: RespondStatus) {
    if (!app.account) return;
    busy = s;
    try { await api.calRespondInvite(app.account, folder, uid, invite.attachmentId, s, trust); patch({ myStatus: s, alreadyInCalendar: s !== "declined" || invite.alreadyInCalendar }); app.notify(`Replied: ${STATUS[s]}`); }
    catch (e) { app.fail(e); }
    busy = "";
  }
  async function add() {
    if (!app.account) return;
    busy = "add";
    try { await api.calImportInvite(app.account, folder, uid, invite.attachmentId, trust); patch({ alreadyInCalendar: true }); app.notify("Added to calendar"); }
    catch (e) { app.fail(e); }
    busy = "";
  }
  async function join() {
    if (!zoomUrl || !/^https:\/\//i.test(zoomUrl)) return;
    if (await app.ask({ title: "Join Zoom meeting?", body: "This will open the meeting link in your default browser.", detail: zoomUrl, ok: "Open" })) {
      try { await openExternal(zoomUrl); } catch (e) { app.fail(e); }
    }
  }
</script>

<section class="inv" class:cx={cancelled} aria-label="Calendar invitation">
  {#if cancelled}<div class="banner" role="status">This event was cancelled by the organizer.</div>{/if}
  {#if invite.senderUnverified}
    <div class="banner warn" role="alert">
      <strong>Couldn't verify the sender.</strong> This invitation may not come from who it claims. Updates and cancellations are refused unless you trust the sender.
      <label class="trust"><input type="checkbox" bind:checked={trust} /> I trust this sender</label>
    </div>
  {/if}
  <div class="top"><span class="label">{invite.method === "REPLY" ? "Reply to invitation" : "Invitation"}</span>
    {#if invite.myStatus && !cancelled}<span class="pill mono {invite.myStatus}">{STATUS[invite.myStatus]}</span>{/if}</div>
  <h2 class:strike={cancelled} class="selectable">{ev.summary || "Untitled event"}</h2>
  <dl class="selectable">
    <dt>When</dt><dd>{fmtSpan(sp)}{#if !ev.allDay && ev.tzid && ev.tzid !== localTz()} <span class="mono sub">({ev.tzid})</span>{/if}{#if ev.rrule} <span class="mono sub">· repeats</span>{/if}</dd>
    {#if place}<dt>Where</dt><dd>{place}</dd>{/if}
    {#if zoomUrl && !cancelled}<dt>Zoom</dt><dd><button class="btn sm ghost" onclick={join}>Join Zoom meeting</button></dd>{/if}
    {#if ev.organizer}<dt>From</dt><dd>{ev.organizer.name ? `${ev.organizer.name} <${ev.organizer.email}>` : ev.organizer.email}</dd>{/if}
    {#if guests}<dt>Guests</dt><dd>{guests} {guests === 1 ? "person" : "people"}</dd>{/if}
  </dl>
  {#if !cancelled}
    <div class="acts" role="group" aria-label="Respond to invitation">
      {#if invite.method === "REQUEST"}
        <button class="btn sm" aria-pressed={invite.myStatus === "accepted"} disabled={!!busy} onclick={() => respond("accepted")}>{busy === "accepted" ? "…" : "Accept"}</button>
        <button class="btn sm ghost" aria-pressed={invite.myStatus === "tentative"} disabled={!!busy} onclick={() => respond("tentative")}>Tentative</button>
        <button class="btn sm ghost" aria-pressed={invite.myStatus === "declined"} disabled={!!busy} onclick={() => respond("declined")}>Decline</button>
      {/if}
      <span class="grow"></span>
      {#if invite.alreadyInCalendar}<span class="mono sub">In your calendar</span>
      {:else}<button class="btn sm ghost" disabled={!!busy} onclick={add}>{busy === "add" ? "Adding…" : "Add to calendar"}</button>{/if}
    </div>
  {/if}
</section>

<style>
  .inv { border: 1px solid var(--line); border-left: 3px solid var(--accent); border-radius: var(--radius); background: var(--surface); padding: 14px 16px; display: grid; gap: 10px; }
  .inv.cx { border-left-color: var(--signal); }
  .banner { border: 1px solid var(--line); border-left: 2px solid var(--signal); border-radius: var(--radius); padding: 6px 10px; font-size: 14px; color: var(--ink); }
  .banner.warn { border-left-color: var(--danger); display: grid; gap: 6px; } .trust { display: flex; gap: 8px; align-items: center; font-size: 13px; color: var(--muted); }
  .top { display: flex; justify-content: space-between; align-items: center; }
  .pill { border: 1px solid var(--line); border-radius: 2px; padding: 1px 6px; font-size: 11px; } .pill.accepted { color: var(--accent); border-color: var(--accent); } .pill.declined { color: var(--danger); border-color: var(--danger); } .pill.tentative { color: var(--signal); border-color: var(--signal); }
  h2 { font-size: 17px; letter-spacing: .03em; overflow-wrap: anywhere; } .strike { text-decoration: line-through; color: var(--muted); }
  dl { display: grid; grid-template-columns: 62px 1fr; gap: 4px 10px; margin: 0; font-size: 14px; }
  dt { color: var(--muted); text-transform: uppercase; font-size: 11px; letter-spacing: .1em; padding-top: 2px; } dd { margin: 0; overflow-wrap: anywhere; }
  .sub { color: var(--muted); } .acts { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; } .grow { flex: 1; }
  button[aria-pressed="true"] { outline: 1px solid var(--accent); outline-offset: 1px; }
</style>
