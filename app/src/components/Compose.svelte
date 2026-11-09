<script lang="ts">
  import { app } from "../lib/state.svelte";
  import Select from "./Select.svelte";
  import DateTimeSelect from "./DateTimeSelect.svelte";
  import { api, errMsg } from "../lib/api";
  import { workspace, takeRestoredAtts, getComposeMem, setComposeMem, type ComposePayload } from "../lib/workspace.svelte";
  import { prefs } from "../lib/prefs.svelte";
  import { splitAddrs, validAddr } from "../lib/util";
  import { AI_DRAFT_BANNER } from "../lib/aivm";
  import { composer, defaultMode, rememberMode, rememberSpell, spellOn } from "../lib/composer.svelte";
  import {
    extractInlineImages, htmlForDraft, htmlToText, insertPlainSig, insertRichSig, pickSignature, plainToRichBody, removePlainSig, removeRichSig,
    sigHtmlBlock, sigsForAccount, textToHtml, QUOTE_OPEN,
  } from "../lib/composetext";
  import { buildRestore } from "../lib/outboxmodel";
  import RichEditor from "./compose/RichEditor.svelte";
  import { lintCompose } from "../lib/composelint";
  import ToneBar from "./compose/ToneBar.svelte";
  import { replaceBodyHtml, replaceBodyText, richOwnText } from "../lib/tonemeter";
  import AddrField from "./compose/AddrField.svelte";
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { checkTime, fromLocalInput, presets, toLocalInput, whenLabel } from "../lib/snoozetimes";

  let { id }: { id: string } = $props();
  const init = untrack(() => workspace.getPayload<ComposePayload>(id))!;
  const myId = untrack(() => id);
  // In-memory copy of what the saved draft cannot hold (files, an oversized rich body): survives a remount (Tiles <-> Tabs, tab stacks).
  const mem0 = getComposeMem(myId);
  const initHtml = mem0?.html ?? init.html;
  let to = $state(init.to);
  let cc = $state(init.cc);
  let bcc = $state(init.bcc);
  let subject = $state(init.subject);
  let text = $state(init.text);
  let showCc = $state(init.showCc || !!init.bcc);
  const aiDraft = !!init.aiDraft;
  const aiDraftId = init.aiDraftId, aiWarnings = init.aiWarnings ?? [];
  const replyNotice = init.replyNotice ?? "";
  let sending = $state(false);
  // Send later: a small chooser next to Send (presets + custom time). The message is stored in the Outbox as scheduled.
  let laterOpen = $state(false);
  let laterNow = $state(new Date());
  let laterCustom = $state("");
  let laterErr = $state("");
  function openLater() {
    laterNow = new Date();
    const d = new Date(laterNow.getTime() + 3_600_000); d.setSeconds(0, 0);
    laterCustom = toLocalInput(d); laterErr = ""; laterOpen = !laterOpen;
  }
  function pickLaterCustom() {
    const d = fromLocalInput(laterCustom), why = checkTime(d, new Date());
    if (why || !d) { laterErr = why ?? "Enter a date and time."; return; }
    laterOpen = false; void send(d.getTime());
  }
  let error = $state("");
  let toEl = $state<HTMLInputElement>(), bodyEl = $state<HTMLTextAreaElement>(), fileEl: HTMLInputElement;
  let editor = $state<RichEditor>();

  // ---- editor mode: plain textarea or rich contenteditable (the plain text is always kept in `text`) ----
  const initialMode: "plain" | "rich" = aiDraft ? "plain" : init.mode ?? defaultMode();
  let mode = $state<"plain" | "rich">(initialMode);
  const kind = init.kind ?? (init.inReplyTo ? "reply" : "new");
  // Rich body to start from: restored draft HTML, else a reply / forward quote, else the plain text converted.
  const startHtml = initHtml ?? (init.seedHtml !== undefined ? "<div><br></div><div><br></div>" + init.seedHtml : plainToRichBody(init.text));
  let html = $state(startHtml);
  if (initialMode === "rich" && initHtml == null) text = htmlToText(startHtml);
  let spell = $state(spellOn());
  const wasClean = JSON.stringify([splitAddrs(init.to), splitAddrs(init.cc), init.subject, init.text]) === init.base;
  let base = $state(init.base);
  const snapshotKey = () => JSON.stringify([splitAddrs(to), splitAddrs(cc), subject, text]);
  /** Body content that Compose itself produced (signature, quote conversion) must not make a fresh message look edited:
   *  re-base on the opened header fields plus the current body, unless the user has typed in the body meanwhile. */
  const rebaseIfClean = () => { if (wasClean && !bodyEdited) base = JSON.stringify([splitAddrs(init.to), splitAddrs(init.cc), init.subject, text]); };
  let bodyEdited = false, programmatic = false; // bodyEdited: the user typed in the body (programmatic edits do not count)

  // ---- signature + sender ----
  const account = init.account ?? "";
  let sigId = $state(init.sigId ?? "");
  let sigApplied = $state(init.sigApplied ?? false);
  let from = $state(init.from ?? "");
  const sigOptions = $derived(sigsForAccount(composer.signatures, account || null));
  const curSig = $derived(composer.signatures.find((s) => s.id === sigId) ?? null);
  const fromOptions = $derived(account ? [{ address: account, name: "" }, ...composer.identities.filter((i) => i.account === account && i.address.toLowerCase() !== account.toLowerCase()).map((i) => ({ address: i.address, name: i.name }))] : []);
  const mailboxLabel = (o: { address: string; name: string }) => (o.name ? `${o.name} <${o.address}>` : o.address);

  /** Body swaps go through the editor's sanitiser (async); they must not count as the user typing. */
  async function setEditorHtml(h: string) {
    programmatic = true;
    try { await editor?.setHtml(h); } finally { programmatic = false; }
  }
  function switchSig(newId: string) {
    const old = composer.signatures.find((s) => s.id === sigId) ?? null;
    const nu = newId ? composer.signatures.find((s) => s.id === newId) ?? null : null;
    if (mode === "plain") {
      let t = text;
      if (old) t = removePlainSig(t, old.text);
      if (nu) t = insertPlainSig(t, nu.text, nu.placement);
      text = t;
    } else {
      let h = editor?.getHtml() ?? html;
      h = removeRichSig(h);
      if (nu) h = insertRichSig(h, sigHtmlBlock(nu), nu.placement);
      void setEditorHtml(h);
    }
    sigId = nu ? nu.id : "";
  }
  function pickFrom(addr: string) {
    from = addr.toLowerCase() === account.toLowerCase() ? "" : addr;
    const sg = pickSignature(composer.signatures, composer.defaults, composer.identities, account || null, kind, from || null);
    // An identity with its own signature switches to it; leaving it falls back to the account default.
    if ((sg?.id ?? "") !== sigId) switchSig(sg?.id ?? "");
  }

  function setMode(m: "plain" | "rich") {
    if (m === mode) return;
    const sg = curSig;
    if (m === "rich") {
      let t = text;
      if (sg) t = removePlainSig(t, sg.text);
      let h = plainToRichBody(t);
      if (sg) h = insertRichSig(h, sigHtmlBlock(sg), sg.placement);
      html = h;
      mode = "rich";
    } else {
      let h = editor?.getHtml() ?? html;
      if (sg) h = removeRichSig(h);
      let t = htmlToText(h);
      if (sg) t = insertPlainSig(t, sg.text, sg.placement);
      text = t;
      mode = "plain";
    }
    rememberMode(m);
    bodyEdited = true;
    void tick().then(() => (m === "rich" ? editor?.focusEnd() : bodyEl?.focus()));
  }
  /** Plain-text alternative of rich HTML, built from the compose-sanitised markup so hidden text never reaches it. */
  async function safeText(h: string): Promise<string> {
    try { return htmlToText(await api.sanitizeCompose(h)); } catch { return htmlToText(h); }
  }
  // Pasted pictures cannot ride inline (most clients ignore data: images), so they go out as attachments: say so.
  const pastedPics = $derived(mode === "rich" ? (html.match(/<img\b[^>]*\bsrc\s*=\s*["']?data:image\//gi) ?? []).length : 0);
  // Advisory compatibility check (never blocks Send): what some mail apps may not show.
  const lint = $derived(mode === "rich" ? lintCompose(html) : []);
  function richInput(h: string) { html = h; text = htmlToText(h); }
  // "Check tone" > Apply: replaces only the user's own words (quote and signature stay). One level of "Restore my version".
  let toneBackup = $state<{ mode: "plain" | "rich"; v: string } | null>(null);
  function applyTone(body: string): boolean {
    const cur = mode === "rich" ? editor?.getHtml() ?? html : "";
    const next = mode === "rich" ? replaceBodyHtml(cur, body) : null;
    if (mode === "rich" && next == null) return false; // no single plain-text run to replace: nothing changes
    if (!toneBackup) toneBackup = mode === "rich" ? { mode, v: cur } : { mode, v: text };
    if (mode === "rich") void setEditorHtml(next!);
    else { text = replaceBodyText(text, body); touched(); }
    return true;
  }
  function restoreTone() {
    const b = toneBackup;
    if (!b) return;
    toneBackup = null;
    if (b.mode === "rich" && mode === "rich") void setEditorHtml(b.v);
    else if (b.mode === "plain" && mode === "plain") { text = b.v; touched(); }
  }
  function toggleSpell() { spell = !spell; rememberSpell(spell); }

  // Attachments live in this window only (not in the autosaved draft): a restored draft starts without them.
  const MAX_TOTAL = 20 * 1024 * 1024;
  type Att = { key: number; name: string; mime: string; size: number; dataBase64: string; src?: "fwd-raw" | string };
  // Undo / Edit from the Outbox hands the files back here (in memory, once).
  let nextKey = 1;
  const restored = takeRestoredAtts(untrack(() => id));
  let atts = $state<Att[]>(restored.length
    ? restored.map((a) => ({ key: nextKey++, name: a.name, mime: a.mime, size: Math.floor((a.dataBase64.length * 3) / 4), dataBase64: a.dataBase64 }))
    : (mem0?.atts ?? []).map((a) => ({ key: nextKey++, ...a })));
  const hadFwdAtts = untrack(() => atts.some((a) => a.src)); // forwarded files came back from memory: do not fetch them again
  let over = $state(false), dragDepth = 0;
  const attTotal = $derived(atts.reduce((n, a) => n + a.size, 0));
  const fmtSize = (n: number) => (n < 1024 ? `${n} B` : n < 1048576 ? `${Math.round(n / 1024)} KB` : `${(n / 1048576).toFixed(1)} MB`);
  const readB64 = (f: File) => new Promise<string>((res, rej) => {
    const r = new FileReader();
    r.onload = () => res(String(r.result).split(",", 2)[1] ?? "");
    r.onerror = () => rej(r.error);
    r.readAsDataURL(f);
  });
  async function addFiles(files: Iterable<File>) {
    error = "";
    for (const f of files) {
      if (attTotal + f.size > MAX_TOTAL) { error = `“${f.name}” would take the attachments over 20 MB. Share a link instead.`; continue; }
      try { atts.push({ key: nextKey++, name: f.name, mime: f.type, size: f.size, dataBase64: await readB64(f) }); }
      catch { error = `Could not read “${f.name}”.`; }
    }
  }
  const hasFiles = (e: DragEvent) => !!e.dataTransfer && Array.from(e.dataTransfer.types).includes("Files");
  function dragEnter(e: DragEvent) { if (!hasFiles(e)) return; e.preventDefault(); dragDepth++; over = true; }
  function dragOver(e: DragEvent) { if (!hasFiles(e)) return; e.preventDefault(); if (e.dataTransfer) e.dataTransfer.dropEffect = "copy"; }
  function dragLeave(e: DragEvent) { if (!hasFiles(e)) return; dragDepth = Math.max(0, dragDepth - 1); if (!dragDepth) over = false; }
  function drop(e: DragEvent) {
    if (!hasFiles(e)) return;
    e.preventDefault(); dragDepth = 0; over = false;
    void addFiles(Array.from(e.dataTransfer?.files ?? []));
  }
  function picked() { void addFiles(Array.from(fileEl.files ?? [])); fileEl.value = ""; }

  // ---- forward: re-attach the original's files, or the whole message as .eml (fetched here, never stored in the draft) ----
  let fwd = $state(init.fwd);
  let fwdBusy = $state(false);
  const GONE = "The original message is no longer available, so it was not attached.";
  const isGone = (e: unknown) => errMsg(e).includes("no longer available") || errMsg(e).includes("UIDVALIDITY");
  async function loadForward() {
    const f = fwd;
    if (!f || hadFwdAtts) return;
    // Drafts saved before UIDVALIDITY was stored cannot prove the UID still names the same message: do not guess.
    if (f.uidvalidity == null) { error = GONE; fwd = undefined; return; }
    fwdBusy = true;
    try {
      if (f.as === "attach") {
        const d = await api.getRawMessage(f.account, f.folder, f.uid, f.uidvalidity);
        const size = Math.floor((d.dataBase64.length * 3) / 4);
        if (attTotal + size > MAX_TOTAL) error = "The original message is over 20 MB, too large to forward as an attachment.";
        else atts.push({ key: nextKey++, name: d.name, mime: d.mime, size, dataBase64: d.dataBase64, src: "fwd-raw" });
        return;
      }
      for (const a of [...f.atts]) {
        if (attTotal + a.size > MAX_TOTAL) {
          error = `“${a.name}” was not re-attached: the forward would be over 20 MB.`;
          fwd = fwd && { ...fwd, atts: fwd.atts.filter((x) => x.id !== a.id) };
          continue;
        }
        try {
          const d = await api.getAttachment(f.account, f.folder, f.uid, a.id, f.uidvalidity);
          atts.push({ key: nextKey++, name: d.name || a.name, mime: d.mime || a.mime, size: a.size, dataBase64: d.dataBase64, src: a.id });
        } catch (e) {
          if (isGone(e)) { error = GONE; fwd = undefined; return; }
          error = `Could not re-attach “${a.name}”.`; fwd = fwd && { ...fwd, atts: fwd.atts.filter((x) => x.id !== a.id) };
        }
      }
    } catch (e) {
      if (isGone(e)) { error = GONE; fwd = undefined; }
      else error = typeof e === "string" ? e : "Could not attach the original message.";
    }
    finally { fwdBusy = false; }
  }
  function removeAtt(a: Att) {
    atts = atts.filter((x) => x.key !== a.key);
    if (a.src === "fwd-raw") fwd = undefined;
    else if (a.src && fwd) fwd = { ...fwd, atts: fwd.atts.filter((x) => x.id !== a.src) };
  }

  const dirty = $derived(snapshotKey() !== base || !!bcc.trim() || atts.length > 0);
  // Files the user added live only in this webview: tell the workspace, which then refuses to move the pane to another window
  // (forwarded files are not counted: they are re-fetched from the server after a move). Cleared when the pane goes away.
  $effect(() => {
    const n = atts.filter((a) => !a.src).length;
    untrack(() => workspace.setAttachmentCount(myId, n));
  });
  // Keep files and any rich body the draft copy had to shrink in memory (cleared by the workspace when the pane is closed or sent).
  $effect(() => {
    const files = $state.snapshot(atts).map(({ name, mime, size, dataBase64, src }) => ({ name, mime, size, dataBase64, ...(src ? { src } : {}) }));
    const full = mode === "rich" && htmlForDraft(html) !== html ? html : undefined;
    untrack(() => setComposeMem(myId, files.length || full != null ? { atts: files, ...(full != null ? { html: full } : {}) } : null));
  });
  onDestroy(() => { workspace.setAttachmentCount(myId, 0); workspace.setHold(myId, false); });

  // Mirror this instance's state into the workspace (payload map; draft autosave is debounced there).
  const makePayload = (): ComposePayload => ({
    account: init.account, to, cc, bcc, subject, text, inReplyTo: init.inReplyTo, showCc, base, mode, kind, sigId, sigApplied,
    ...(mode === "rich" ? { html: htmlForDraft(html) } : {}), ...(from ? { from } : {}), ...(fwd ? { fwd: $state.snapshot(fwd) } : {}),
    ...(aiDraft ? { aiDraft: true, ...(aiDraftId != null ? { aiDraftId } : {}), ...(aiWarnings.length ? { aiWarnings } : {}) } : {}),
    ...(replyNotice ? { replyNotice } : {}),
  });
  $effect(() => {
    const payload = makePayload();
    const title = subject.trim() || (init.inReplyTo ? "Reply" : "New message");
    const d = dirty;
    untrack(() => workspace.setPayload(myId, payload, { title, dirty: d }));
  });

  // ---- IMAP Drafts mirror (on by default, can be turned off in Preferences > Composing): after >= 20 s without typing, only with content ----
  const MIRROR_IDLE_MS = 20_000;
  let mirrorMsg = $state("");
  let mirrorTimer: ReturnType<typeof setTimeout> | undefined, lastMirrored = "", mirrorGone = false;
  const hasContent = $derived(!!(to.trim() || cc.trim() || bcc.trim() || subject.trim() || text.trim()));
  async function mirrorNow(sig: string) {
    const acct = init.account;
    if (!acct || mirrorGone || sig === lastMirrored) return;
    const rich = mode === "rich" ? extractInlineImages(editor?.getHtml() ?? html).html : undefined; // the server draft carries no pictures or files
    try {
      const st = await api.draftMirrorSave(myId, acct, {
        to: splitAddrs(to), cc: splitAddrs(cc), bcc: splitAddrs(bcc), subject: subject.trim(), text: rich != null ? await safeText(rich) : text, inReplyTo: init.inReplyTo,
        ...(rich != null ? { html: rich } : {}), ...(from ? { fromAddress: from } : {}),
      });
      lastMirrored = sig;
      mirrorMsg = st === "saved" ? "Draft saved on server" : "";
    } catch (e) { mirrorMsg = "Draft not saved on server"; console.warn("[miramail] drafts mirror:", errMsg(e).slice(0, 120)); }
  }
  $effect(() => {
    const sig = JSON.stringify([to, cc, bcc, subject, text, mode === "rich" ? html : ""]);
    const on = prefs.v.mirrorDrafts && hasContent && !sending;
    untrack(() => {
      clearTimeout(mirrorTimer);
      if (on && sig !== lastMirrored) mirrorTimer = setTimeout(() => void mirrorNow(sig), MIRROR_IDLE_MS);
    });
  });
  onDestroy(() => { mirrorGone = true; clearTimeout(mirrorTimer); });

  // A newly opened (focused) pane takes the cursor; restored panes do not steal focus at launch.
  onMount(() => {
    void (async () => {
      await composer.ensure();
      if (!sigApplied && !aiDraft && wasClean) {
        const sg = pickSignature(composer.signatures, composer.defaults, composer.identities, account || null, kind, from || null);
        if (sg) switchSig(sg.id);
        sigApplied = true;
      }
      rebaseIfClean();
      void loadForward();
    })();
    if (workspace.ws.focus !== myId) return;
    void tick().then(() => {
      if (init.to) {
        if (mode === "rich") editor?.focusStart(); else { bodyEl?.focus(); if (!dirty) bodyEl?.setSelectionRange(0, 0); }
      } else toEl?.focus();
    });
  });

  async function discard() {
    if (sending) return;
    if (dirty) {
      workspace.setHold(myId, true); // the pane must not change windows while the question is open
      let ok = false;
      try { ok = await app.ask({ title: "Discard this message?", body: "Your draft has not been sent and will be lost.", ok: "Discard", danger: true }); }
      finally { workspace.setHold(myId, false); }
      if (!ok) return;
    }
    workspace.close(myId);
  }

  /** The whole send (including its "Send without a subject?" question) holds the pane in this window. */
  async function send(sendAt?: number) {
    if (sending || fwdBusy || workspace.moving(myId)) return;
    workspace.setHold(myId, true);
    try { await sendNow(sendAt); } finally { workspace.setHold(myId, false); }
  }
  async function sendNow(sendAt?: number) {
    if (sending || fwdBusy) return;
    error = "";
    const t = splitAddrs(to), c = splitAddrs(cc), bc = splitAddrs(bcc);
    if (!t.length) { error = "Add at least one recipient."; return; }
    const bad = [...t, ...c, ...bc].find((a) => !validAddr(a));
    if (bad) { error = `“${bad}” doesn't look like an email address.`; return; }
    if (!subject.trim() && !(await app.ask({ title: "Send without a subject?", body: "This message has no subject.", ok: "Send anyway" }))) return;
    // Rich mode: pasted pictures travel as ordinary attachments (data: images are not shown by most mail clients).
    let outHtml: string | undefined;
    let files = atts.map(({ name, mime, dataBase64 }) => ({ name, mime, dataBase64 }));
    let inlinePics = 0;
    if (mode === "rich") {
      const ex = extractInlineImages(editor?.getHtml() ?? html);
      const picSize = ex.images.reduce((n, i) => n + i.size, 0);
      if (attTotal + picSize > MAX_TOTAL) { error = "The pictures in this message would take it over 20 MB. Remove some."; return; }
      outHtml = ex.html;
      text = await safeText(ex.html); // keeps the plain part equal to what the sanitiser lets through
      files = [...files, ...ex.images.map(({ name, mime, dataBase64 }) => ({ name, mime, dataBase64 }))];
      inlinePics = ex.images.length;
    }
    const acct = init.account ?? app.account;
    if (!acct) { error = "Choose an account to send from."; return; }
    // What Undo gives back: the pane exactly as it is now (full body, pictures included when they fit the 1 MB limit).
    // The whole JSON is measured; if it is too big it is reduced (pictures, then rich body), never refused.
    const fullHtml = mode === "rich" ? editor?.getHtml() ?? html : undefined;
    const { restore, inlineCount } = buildRestore({ ...makePayload() }, fullHtml, inlinePics);
    sending = true;
    try {
      // Held in the Outbox for the undo delay (Preferences > Composing), then sent by the backend. The pane closes now;
      // the main window shows "Sending… Undo".
      await api.outboxSubmit(acct, {
        to: t, cc: c, bcc: bc, subject: subject.trim(), text, inReplyTo: init.inReplyTo,
        ...(outHtml != null ? { html: outHtml } : {}), ...(from ? { fromAddress: from } : {}), ...(files.length ? { attachments: files } : {}),
      }, restore, inlineCount, prefs.v.undoSendSecs, sendAt, myId);
      workspace.finish(myId);
      if (sendAt != null) app.notify(`Scheduled for ${whenLabel(sendAt, new Date())}. MiraMail must be open then; it is in the Outbox until it goes out.`);
    } catch (e) { error = errMsg(e); }
    finally { sending = false; }
  }

  function key(e: KeyboardEvent) {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); e.stopPropagation(); void send(); }
  }
  const touched = () => { if (!programmatic) bodyEdited = true; };
</script>


<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="dlg" class:over role="form" aria-label="Message" onkeydown={key} ondragenter={dragEnter} ondragover={dragOver} ondragleave={dragLeave} ondrop={drop}>
    {#if over}<div class="drop" aria-hidden="true">Drop files to attach</div>{/if}
    {#if aiDraft}
      <div class="aibanner" role="note">
        {AI_DRAFT_BANNER}
        {#if aiWarnings.length}<ul class="ainotes">{#each aiWarnings as w}<li>{w}</li>{/each}</ul>{/if}
      </div>
    {/if}
    {#if replyNotice}<div class="aibanner" role="note" data-testid="reply-to-notice">{replyNotice}</div>{/if}
    <div class="form">
      {#if aiDraft}<div class="r"><span class="label">From</span><span class="mono from" data-testid="ai-from">{init.account ?? ""}</span></div>
      {:else if fromOptions.length > 1}
        <div class="r"><label class="label" for="c-from-{myId}">From</label>
          <Select id="c-from-{myId}" value={from || account} options={fromOptions.map((o) => ({ value: o.address, label: mailboxLabel(o) }))} onchange={pickFrom} disabled={sending} /></div>
      {/if}
      <div class="r"><label class="label" for="c-to-{myId}">To</label><AddrField id="c-to-{myId}" {account} bind:el={toEl} bind:value={to} placeholder="name@example.com, …" />
        {#if !showCc}<button class="link" onclick={() => (showCc = true)}>Cc / Bcc</button>{/if}</div>
      {#if showCc}
        <div class="r"><label class="label" for="c-cc-{myId}">Cc</label><AddrField id="c-cc-{myId}" {account} bind:value={cc} /></div>
        <div class="r"><label class="label" for="c-bcc-{myId}">Bcc</label><AddrField id="c-bcc-{myId}" {account} bind:value={bcc} /></div>
      {/if}
      <div class="r"><label class="label" for="c-sub-{myId}">Subject</label><input id="c-sub-{myId}" bind:value={subject} autocomplete="off" spellcheck={spell} /></div>
      {#if !aiDraft}
        <div class="opts" role="group" aria-label="Message options">
          <div class="seg" role="group" aria-label="Format">
            <button type="button" class:on={mode === "plain"} aria-pressed={mode === "plain"} onclick={() => setMode("plain")}>Plain</button>
            <button type="button" class:on={mode === "rich"} aria-pressed={mode === "rich"} onclick={() => setMode("rich")}>Rich</button>
          </div>
          {#if sigOptions.length}
            <span class="sigsel"><span id="c-sig-l-{myId}">Signature</span>
              <Select value={sigId ?? ""} labelledby="c-sig-l-{myId}" block={false} compact options={[{ value: "", label: "None" }, ...sigOptions.map((sg) => ({ value: sg.id, label: sg.name }))]} onchange={(v) => { switchSig(v); touched(); }} disabled={sending} /></span>
          {/if}
          <button type="button" class="tog" class:on={spell} aria-pressed={spell} onclick={toggleSpell}
            title="Spell check. For grammar, use right-click > Spelling and Grammar > Check Grammar With Spelling (the system offers it; the web view cannot switch it from here).">Spelling</button>
        </div>
      {/if}
      {#if mode === "rich"}
        <RichEditor bind:this={editor} {html} {spell} oninput={(h) => { touched(); richInput(h); }} onerror={(m) => (error = m)} label="Message body" />
      {:else}
        <label class="sr-only" for="c-body-{myId}">Message body</label>
        <textarea id="c-body-{myId}" bind:this={bodyEl} bind:value={text} spellcheck={spell} oninput={touched}></textarea>
      {/if}
    </div>
    {#if !aiDraft}<ToneBar text={mode === "rich" ? richOwnText(html) : text} html={mode === "rich" ? html : null} meterOn={prefs.v.toneMeter} canRestore={!!toneBackup && toneBackup.mode === mode} onapply={applyTone} onrestore={restoreTone} />{/if}
    {#if pastedPics}<div class="picnote" role="note">{pastedPics === 1 ? "The picture you pasted" : `The ${pastedPics} pictures you pasted`} will be sent as {pastedPics === 1 ? "an attachment" : "attachments"} after the message, because many mail apps do not show inline pasted images.</div>{/if}
    {#if lint.length}
      <details class="lint">
        <summary>{lint.length === 1 ? "1 thing may look different in some mail apps" : `${lint.length} things may look different in some mail apps`}</summary>
        <ul>{#each lint as w (w.id)}<li><b>{w.message}</b> {w.hint}</li>{/each}</ul>
      </details>
    {/if}
    {#if atts.length}
      <ul class="atts" aria-label="Attachments">
        {#each atts as a (a.key)}
          <li class="chip"><span class="nm" title={a.name}>{a.name}</span><span class="sz mono">{fmtSize(a.size)}</span>
            <button class="x" aria-label="Remove {a.name}" onclick={() => removeAtt(a)} disabled={sending}>×</button></li>
        {/each}
      </ul>
    {/if}
    {#if error}<div class="alert" role="alert">{error}</div>{/if}
    <input bind:this={fileEl} type="file" multiple hidden onchange={picked} />
    <footer>
      <button class="btn ghost" onclick={() => fileEl.click()} disabled={sending} title="Attach files (or drop them here)">Attach{atts.length ? ` (${fmtSize(attTotal)})` : ""}</button>
      <span class="mono hint">⌘↵ send</span>
      {#if mirrorMsg}<span class="mono hint quiet" role="status">{mirrorMsg}</span>{/if}
      <span class="grow"></span>
      <button class="btn ghost" onclick={discard} disabled={sending}>Discard</button>
      <span class="later">
        <button class="btn ghost" onclick={openLater} disabled={sending || fwdBusy} aria-haspopup="menu" aria-expanded={laterOpen} title="Choose a time to send this message">Send later…</button>
        {#if laterOpen}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="laterscrim" onmousedown={() => (laterOpen = false)} onkeydown={(e) => e.key === "Escape" && (laterOpen = false)}></div>
          <div class="latermenu" role="menu" aria-label="Send later">
            {#each presets(laterNow) as p (p.id)}
              <button role="menuitem" onclick={() => { laterOpen = false; void send(p.at.getTime()); }}><span>{p.label}</span><span class="mono dim">{whenLabel(p.at.getTime(), laterNow)}</span></button>
            {/each}
            <div class="lcust"><span class="label" id="c-later-l-{myId}">Pick date and time</span><DateTimeSelect idPrefix="c-later-{myId}" value={laterCustom} onchange={(v) => { laterCustom = v; laterErr = ""; }} /></div>
            {#if laterErr}<div class="lerr" role="alert">{laterErr}</div>{/if}
            <button class="btn sm" onclick={pickLaterCustom}>Schedule</button>
            <p class="lnote">Sent only while MiraMail is open. If it is closed at that time you are asked before anything goes out.</p>
          </div>
        {/if}
      </span>
      <button class="btn" onclick={() => send()} disabled={sending || fwdBusy} title={fwdBusy ? "Attaching the original files…" : ""}>{sending ? "Sending…" : fwdBusy ? "Attaching…" : "Send"}</button>
    </footer>
</div>

<style>
  .later { position: relative; }
  .laterscrim { position: fixed; inset: 0; z-index: 80; }
  .latermenu { position: absolute; right: 0; bottom: calc(100% + 6px); z-index: 81; min-width: 260px; display: grid; gap: 4px; background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); padding: 6px; }
  .latermenu > button[role="menuitem"] { display: flex; justify-content: space-between; gap: 12px; background: none; border: 0; padding: 6px 8px; color: var(--ink); font-size: 0.8667rem; text-align: left; }
  .latermenu > button[role="menuitem"]:hover { background: color-mix(in srgb, var(--line) 50%, transparent); }
  .dim { color: var(--muted); font-size: 0.8rem; }
  .lcust { display: grid; gap: 3px; padding: 4px 8px 0; }
  .lerr { color: var(--danger); font-size: 0.8rem; padding: 0 8px; }
  .lnote { margin: 2px 8px 2px; color: var(--muted); font-size: 0.75rem; line-height: 1.4; }
  .dlg { position: relative; flex: 1; min-height: 0; min-width: 0; background: var(--surface); display: flex; flex-direction: column; }
  footer { display: flex; align-items: center; gap: 8px; padding: 10px 14px; border-top: 1px solid var(--line); flex-wrap: wrap; }
  .aibanner { padding: 6px 14px; font-size: 12px; color: var(--ink); background: color-mix(in srgb, var(--accent) 14%, transparent); border-bottom: 1px solid var(--line); border-left: 3px solid var(--accent); }
  .ainotes { margin: 4px 0 0; padding-left: 18px; color: var(--danger); }
  .from { flex: 1; font-size: 13px; padding: 11px 0; color: var(--muted); user-select: text; }
  .form { display: flex; flex-direction: column; flex: 1; min-height: 0; }
  .r { display: flex; align-items: center; gap: 10px; border-bottom: 1px solid var(--line); padding: 0 14px; }
  .r .label { width: 56px; flex: none; }
  .r > input { flex: 1; background: none; border: 0; color: var(--ink); font: 400 13px var(--font-mono); padding: 11px 0; outline-offset: 4px; min-width: 0; }
  textarea { flex: 1; resize: none; background: none; border: 0; color: var(--text); font: 400 13px/1.6 var(--font-mono); padding: 14px; outline-offset: -2px; }
  .opts { display: flex; align-items: center; gap: 12px; padding: 5px 14px; border-bottom: 1px solid var(--line); flex-wrap: wrap; font-size: 0.8rem; color: var(--muted); }
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; }
  .seg button { background: none; border: 0; padding: 3px 10px; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .1em; color: var(--muted); border-bottom: 2px solid transparent; }
  .seg button.on { color: var(--ink); border-bottom-color: var(--signal); }
  .sigsel { display: inline-flex; align-items: center; gap: 6px; }
  .tog { background: none; border: 1px solid var(--line); border-radius: var(--radius); color: var(--muted); padding: 3px 10px; font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .1em; margin-left: auto; }
  .tog.on { color: var(--accent); border-color: var(--accent); }
  .link { background: none; border: 0; color: var(--muted); font-size: 11px; text-transform: uppercase; letter-spacing: .12em; }
  .link:hover { color: var(--accent); }
  .alert { margin: 0 14px 10px; border: 1px solid var(--danger); border-left-width: 3px; color: var(--danger); border-radius: var(--radius); padding: 8px 12px; font-size: 14px; }
  .drop { position: absolute; inset: 6px; z-index: 5; display: grid; place-items: center; pointer-events: none; border: 2px dashed var(--accent); border-radius: var(--radius); background: color-mix(in srgb, var(--surface) 88%, var(--accent)); color: var(--accent); font-size: 15px; }
  .atts { list-style: none; margin: 0; padding: 8px 14px 0; display: flex; flex-wrap: wrap; gap: 6px; }
  .chip { display: inline-flex; align-items: center; gap: 8px; max-width: 100%; border: 1px solid var(--line); border-radius: var(--radius); padding: 3px 4px 3px 10px; font-size: 12px; }
  .chip .nm { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 260px; }
  .chip .sz { color: var(--muted); }
  .chip .x { background: none; border: 0; color: var(--muted); font-size: 16px; line-height: 1; padding: 0 6px; }
  .chip .x:hover { color: var(--danger); }
  .hint { color: var(--muted); } .grow { flex: 1; }
  .picnote { margin: 0 14px 6px; font-size: 12px; color: var(--muted); }
  .lint { margin: 0 14px 6px; font-size: 12px; color: var(--muted); border: 1px solid var(--line); border-radius: var(--radius); padding: 4px 10px; }
  .lint summary { cursor: pointer; }
  .lint ul { margin: 4px 0 2px; padding-left: 18px; }
  .lint li { margin: 2px 0; } .lint b { color: var(--text); font-weight: 600; }
  .quiet { font-size: 11px; opacity: .8; }
</style>
