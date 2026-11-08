<script lang="ts">
  // Main window only: the "Snooze…" dialog and the list of snoozed messages (docs/ESSENTIALS_SPEC.md, Tier 2).
  import { snoozeUi } from "../lib/t2.svelte";
  import { checkTime, fromLocalInput, presets, toLocalInput, whenLabel } from "../lib/snoozetimes";
  import Modal from "./Modal.svelte";
  import DateTimeSelect from "./DateTimeSelect.svelte";

  let now = $state(new Date());
  let custom = $state("");
  let customErr = $state("");
  $effect(() => {
    if (snoozeUi.target) {
      now = new Date();
      const d = new Date(now.getTime() + 3_600_000);
      d.setSeconds(0, 0);
      custom = toLocalInput(d);
      customErr = "";
    }
  });
  const list = $derived(presets(now));

  function pickCustom() {
    const d = fromLocalInput(custom);
    const why = checkTime(d, new Date());
    if (why || !d) { customErr = why ?? "Enter a date and time."; return; }
    void snoozeUi.snooze(d);
  }
  function stateLine(s: { state: string; error: string | null; mode: string }): string {
    if (s.state === "failed") return `Could not come back: ${s.error ?? "unknown error"}`;
    return s.mode === "moved" ? "Waiting in your Snoozed folder" : "Hidden here, still in its folder on the server";
  }
</script>

{#if snoozeUi.target}
  <Modal title="Snooze until…" onclose={() => (snoozeUi.target = null)} width={380}>
    <div class="col">
      {#each list as p (p.id)}
        <button class="btn ghost row" disabled={snoozeUi.busy} onclick={() => snoozeUi.snooze(p.at)}><span>{p.label}</span><span class="mono when">{whenLabel(p.at.getTime(), now)}</span></button>
      {/each}
      <div class="cust"><span class="label" id="sn-cust-l">Pick date and time</span>
        <DateTimeSelect idPrefix="sn-cust" days={366} value={custom} onchange={(v) => { custom = v; customErr = ""; }} />
      </div>
      {#if customErr}<div class="err" role="alert">{customErr}</div>{/if}
      <button class="btn" disabled={snoozeUi.busy} onclick={pickCustom}>Snooze</button>
      <p class="note">The message comes back to your Inbox as unread, with a notification. MiraMail has to be open at that time; if it is closed, the message returns when you next open it.</p>
    </div>
  </Modal>
{/if}

{#if snoozeUi.listOpen && snoozeUi.items.length}
  <Modal title="Snoozed" onclose={() => (snoozeUi.listOpen = false)} width={620}>
    <ul class="list">
      {#each snoozeUi.items as s (s.id)}
        <li class="it" class:bad={s.state === "failed"}>
          <div class="who"><span class="to">{s.from || "(unknown sender)"}</span><span class="sub">{s.subject || "(no subject)"}</span></div>
          <div class="st">Back {whenLabel(s.wakeAt, new Date())} · {s.account}</div>
          <div class="st dim">{stateLine(s)}</div>
          <div class="acts"><button class="btn sm" onclick={() => snoozeUi.unsnooze(s.id)}>{s.state === "failed" ? "Try again" : "Unsnooze now"}</button><button class="btn ghost sm" title="Remove this entry only; the message is not touched" onclick={() => snoozeUi.forget(s.id)}>Forget this snooze</button></div>
        </li>
      {/each}
    </ul>
    <p class="note">Snoozed messages return only while MiraMail is open. Nothing is deleted: they are either in your Snoozed folder or still in their original folder.</p>
  </Modal>
{/if}

<style>
  .col { display: grid; gap: 8px; }
  .row { display: flex; justify-content: space-between; align-items: center; text-align: left; }
  .when { color: var(--muted); font-size: 0.8rem; }
  .cust { display: grid; gap: 4px; margin-top: 6px; }
  .err { color: var(--danger); font-size: 0.85rem; }
  .note { margin: 6px 0 0; color: var(--muted); font-size: 0.8rem; line-height: 1.5; }
  .list { list-style: none; margin: 0; padding: 0; display: grid; gap: 10px; }
  .it { border: 1px solid var(--line); border-radius: var(--radius); padding: 10px 12px; display: grid; gap: 4px; }
  .it.bad { border-left: 3px solid var(--danger); }
  .who { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .to { color: var(--ink); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sub { color: var(--muted); font-size: 0.85rem; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .st { font-size: 0.85rem; }
  .dim { color: var(--muted); }
  .acts { display: flex; gap: 8px; }
</style>
