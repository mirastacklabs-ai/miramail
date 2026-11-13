<script lang="ts">
  import { thisDevice } from "../lib/device";
  import { prefs } from "../lib/prefs.svelte";
  import { UNDO_SEND_OPTIONS } from "../lib/prefs-model";
  const p = $derived(prefs.v);
</script>

<div class="ct">
  <h3>Composing</h3>
  <p class="note">The editor offers Plain and Rich modes (the choice is remembered). Signatures and sender identities are under Signatures; spell check is a switch in the composer.</p>

  <h3>Sending</h3>
  <div class="row"><span class="label" id="l-undo">Undo send</span>
    <div class="seg" role="radiogroup" aria-labelledby="l-undo">
      {#each UNDO_SEND_OPTIONS as n (n)}
        <button role="radio" aria-checked={p.undoSendSecs === n} class:on={p.undoSendSecs === n} onclick={() => prefs.set("undoSendSecs", n)}>{n === 0 ? "Off" : `${n} s`}</button>
      {/each}
    </div></div>
  <p class="note">After you press Send, the message waits in the Outbox for this long so you can take it back. If MiraMail is closed in that time, it asks what to do with the message the next time you open it; it never sends or drops it by itself. If sending fails, the message stays in the Outbox with Retry, Edit and Discard.</p>

  <h3>Tone</h3>
  <label class="chk"><input type="checkbox" checked={p.toneMeter} onchange={(e) => prefs.set("toneMeter", e.currentTarget.checked)} /> Show a quick tone hint under the message while I write</label>
  <p class="note">A small row under the message says whether your wording reads warm, neutral, firm or tense, and why (English only). It runs on {thisDevice()} while you type, ignores the quoted message and your signature, and sends nothing anywhere. A separate “Check tone” button can ask your AI provider for a second opinion; that only happens when you press it and confirm, and then only the text you wrote is sent.</p>

  <h3>Drafts</h3>
  <label class="chk"><input type="checkbox" checked={p.mirrorDrafts} onchange={(e) => prefs.set("mirrorDrafts", e.currentTarget.checked)} /> Also save my drafts to the Drafts folder on my mail server</label>
  <p class="note">After about 20 seconds without typing, a message you are writing is copied to your account's Drafts folder, so you can continue it from another mail app. It replaces its own earlier copy only, and is removed when you send or discard the message. Attachments are not included. This only talks to your own mail server, and only if the server supports UIDPLUS. Turn it off to keep unfinished drafts on {thisDevice()} only.</p>
</div>

<style>
  .ct { display: grid; gap: 14px; }
  h3 { font-size: 0.8rem; letter-spacing: .12em; color: var(--muted); }
  .row { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  .seg button { background: none; border: 0; padding: 4px 12px; color: var(--muted); border-bottom: 2px solid transparent; }
  .seg button.on { color: var(--ink); border-bottom-color: var(--signal); }
  .chk { display: flex; align-items: center; gap: 8px; }
  .note { margin: 0; color: var(--muted); font-size: 0.85rem; }
</style>
