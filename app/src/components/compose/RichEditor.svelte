<script lang="ts">
  // Small contenteditable editor (no dependency). Output is untrusted until the backend sanitises it on send;
  // pasted HTML is sanitised by the backend before it is inserted, dropped HTML becomes plain text.
  import { api } from "../../lib/api";
  import { onMount } from "svelte";
  import Select from "../Select.svelte";
  import { safeInitialHtml } from "../../lib/composetext";
  import {
    applyTableOp, tableState, tabTarget, newTable, tableBlock, tableFromClipboard, clampDims, ownRowIndex, TOO_LARGE_NOTE, MAX_EDIT_CELLS, MAX_INSERT_ROWS, MAX_INSERT_COLS, MAX_ROWS, MAX_COLS,
    type TableOp, type TableState,
  } from "../../lib/richtable";
  import { calloutHtml, buttonHtml, BUTTON_COLORS, dividerHtml, checklistHtml, toggleCheck, CHECK_OFF, CHECK_ON, MAX_BUTTON_LABEL, type CalloutKind, type ButtonColor } from "../../lib/richblocks";

  let { html, spell = true, label = "Message body", oninput, onerror }: {
    html: string; spell?: boolean; label?: string; oninput: (html: string) => void; onerror?: (msg: string) => void;
  } = $props();

  let el: HTMLDivElement;
  let linkOpen = $state(false), linkUrl = $state(""), linkEl = $state<HTMLInputElement | null>(null);
  let active = $state({ bold: false, italic: false, underline: false, strikeThrough: false, ul: false, ol: false });
  let saved: Range | null = null;
  // Tables and ready-made blocks.
  let tableOpen = $state(false), tRows = $state(3), tCols = $state(3), tHeader = $state(false);
  let btnOpen = $state(false), btnLabel = $state(""), btnUrl = $state(""), btnColor = $state<ButtonColor>("blue"), btnEl = $state<HTMLInputElement | null>(null);
  let ts = $state<TableState | null>(null); // the table around the caret, for the table tools row
  let widthText = $state("");

  const FONTS: [string, string][] = [["Font", ""], ["Sans", "Helvetica, Arial, sans-serif"], ["Serif", "Georgia, serif"], ["Mono", "Menlo, Courier, monospace"]];
  const SIZES: [string, string][] = [["Size", ""], ["Small", "2"], ["Normal", "3"], ["Large", "5"], ["Huge", "6"]];
  const MAX_IMG = 5 * 1024 * 1024;

  // The starting HTML is sanitised before it reaches innerHTML (onMount); until then getHtml hands back the prop unchanged.
  let ready = false;
  export function getHtml(): string { return ready ? el?.innerHTML ?? "" : html; }
  /** Replace the body. The HTML goes through the backend sanitiser first (plain-text fallback), so this is async. */
  export async function setHtml(h: string): Promise<void> {
    const safe = await safeInitialHtml(h, api.sanitizeCompose);
    if (!el) return;
    el.innerHTML = safe;
    ready = true;
    oninput(el.innerHTML);
  }
  export function focusStart() {
    el?.focus();
    const s = getSelection();
    if (s && el) { const r = document.createRange(); r.setStart(el, 0); r.collapse(true); s.removeAllRanges(); s.addRange(r); }
  }
  export function focusEnd() { el?.focus(); const s = getSelection(); if (s && el) { s.selectAllChildren(el); s.collapseToEnd(); } }

  const inside = () => { const s = getSelection(); return !!s && s.rangeCount > 0 && el.contains(s.anchorNode); };
  const SWATCHES: [string, string | null][] = [
    ["Black", "#000000"], ["Dark grey", "#444444"], ["Grey", "#888888"], ["Light grey", "#cccccc"], ["White", "#ffffff"],
    ["Red", "#d93025"], ["Orange", "#f57c00"], ["Amber", "#f9a825"], ["Green", "#2e7d32"], ["Teal", "#00897b"], ["Blue", "#1a73e8"],
    ["Indigo", "#3f51b5"], ["Purple", "#8e24aa"], ["Pink", "#d81b60"], ["Brown", "#6d4c41"],
  ];
  let colorOpen = $state(false), curColor = $state<string | null>(null);
  let colBtn = $state<HTMLButtonElement | null>(null), palEl = $state<HTMLDivElement | null>(null);
  function toggleColor() {
    remember();
    if (colorOpen) { closeColor(); return; }
    colorOpen = true; tableOpen = false; btnOpen = false; linkOpen = false;
    queueMicrotask(() => palEl?.querySelector<HTMLElement>("button")?.focus());
  }
  function closeColor() { colorOpen = false; el.focus(); }
  function pickColor(hex: string | null) {
    colorOpen = false; curColor = hex;
    restore();
    run("foreColor", hex ?? "inherit");
  }
  function palKey(e: KeyboardEvent) {
    if (e.key === "Escape") { e.stopPropagation(); closeColor(); return; }
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
    const bs = [...(palEl?.querySelectorAll<HTMLElement>("button") ?? [])];
    const i = bs.indexOf(document.activeElement as HTMLElement);
    if (i < 0) return;
    e.preventDefault();
    bs[(i + (e.key === "ArrowRight" ? 1 : bs.length - 1)) % bs.length].focus();
  }
  function outsideColor(e: PointerEvent) {
    if (!colorOpen) return;
    const t = e.target as Node;
    if (palEl?.contains(t) || colBtn?.contains(t)) return;
    colorOpen = false;
  }
  function remember() { const s = getSelection(); if (s && s.rangeCount && el.contains(s.anchorNode)) saved = s.getRangeAt(0).cloneRange(); }
  function restore() { if (saved) { const s = getSelection(); s?.removeAllRanges(); s?.addRange(saved); } }
  function sync() { oninput(el.innerHTML); }

  function run(name: string, value?: string) {
    el.focus();
    if (!inside()) restore();
    try { document.execCommand("styleWithCSS", false, "true"); } catch { /* older engines */ }
    document.execCommand(name, false, value);
    remember(); refresh(); sync();
  }
  function refresh() {
    if (!inside()) { return; }
    refreshTable();
    const q = (c: string) => { try { return document.queryCommandState(c); } catch { return false; } };
    active = { bold: q("bold"), italic: q("italic"), underline: q("underline"), strikeThrough: q("strikeThrough"), ul: q("insertUnorderedList"), ol: q("insertOrderedList") };
  }
  function toggleQuote() {
    let v = ""; try { v = document.queryCommandValue("formatBlock"); } catch { /* ignore */ }
    run("formatBlock", v.toLowerCase() === "blockquote" ? "div" : "blockquote");
  }
  function clearFormat() { run("removeFormat"); run("unlink"); }

  const safeUrl = (u: string): string | null => {
    const t = u.trim();
    if (!t) return null;
    if (/^(https?:|mailto:)/i.test(t)) return t;
    if (/^[a-z][a-z0-9+.-]*:/i.test(t)) return null; // javascript:, data:, file: ...
    return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(t) ? `mailto:${t}` : `https://${t}`;
  };
  const escHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  function openLink() { colorOpen = false; remember(); linkUrl = ""; linkOpen = true; queueMicrotask(() => linkEl?.focus()); }
  function applyLink() {
    const u = safeUrl(linkUrl);
    if (!u) { onerror?.("Enter a web or mail address for the link."); return; }
    linkOpen = false;
    el.focus(); restore();
    const collapsed = !getSelection() || getSelection()!.isCollapsed;
    if (collapsed) run("insertHTML", `<a href="${escHtml(u)}">${escHtml(u.replace(/^mailto:/i, ""))}</a>`);
    else run("createLink", u);
  }

  // ---- tables, callouts, buttons, dividers, checklists ----

  /** The table cell around the caret (innermost), or null. Quoted tables (inside a quote or the quoted original) are never edited. */
  function cellAt(): HTMLTableCellElement | null {
    const s = getSelection();
    if (!s || !s.rangeCount || !el.contains(s.anchorNode)) return null;
    const n = s.anchorNode;
    const e = n instanceof Element ? n : n?.parentElement;
    const c = e?.closest("td,th") as HTMLTableCellElement | null;
    if (!c || !el.contains(c)) return null;
    for (let q: Element | null = c.parentElement; q && q !== el; q = q.parentElement) if (q.matches("blockquote,[data-mm-quote]")) return null;
    return c;
  }
  /** Rows of a table in document order (what the model parses), not the DOM's thead / tbody / tfoot order. */
  const ownRows = (tbl: HTMLTableElement) => Array.from(tbl.querySelectorAll("tr")).filter((tr) => tr.closest("table") === tbl) as HTMLTableRowElement[];
  const posOf = (c: HTMLTableCellElement) => {
    const tbl = c.closest("table") as HTMLTableElement;
    return { tbl, r: ownRowIndex(Array.from(tbl.querySelectorAll("tr")), (tr) => tr.closest("table"), tbl, c.parentElement as HTMLTableRowElement), i: c.cellIndex };
  };
  /** Cheap DOM-side size check, before any HTML is built or parsed. */
  const tooBig = (tbl: HTMLTableElement) => tbl.rows.length > MAX_ROWS || tbl.querySelectorAll("td,th").length > MAX_EDIT_CELLS;
  function refreshTable() {
    const c = cellAt();
    if (!c) { ts = null; return; }
    const { tbl, r, i } = posOf(c);
    if (tooBig(tbl)) { ts = { rows: tbl.rows.length, cols: 0, header: false, zebra: false, border: false, width: null, tooLarge: true }; return; }
    ts = tableState(tbl.outerHTML, r, i);
    widthText = ts?.width != null ? String(ts.width) : "";
  }
  function focusCell(tbl: HTMLTableElement, r: number, i: number, select = false) {
    const cell = ownRows(tbl)[r]?.cells[i];
    const s = getSelection();
    if (!cell || !s) return;
    const rg = document.createRange();
    rg.selectNodeContents(cell);
    if (!select) rg.collapse(false);
    s.removeAllRanges(); s.addRange(rg);
    remember();
  }
  function ensureSel() { el.focus(); if (!inside()) restore(); }
  /** Edit the table around the caret through the pure model; the table is rebuilt from clean markup. */
  function tableOp(op: TableOp, focus?: (res: { r: number; i: number }) => { r: number; i: number }) {
    ensureSel();
    const c = cellAt();
    if (!c) return;
    const { tbl, r, i } = posOf(c);
    if (tooBig(tbl)) { onerror?.(TOO_LARGE_NOTE); return; }
    const res = applyTableOp(tbl.outerHTML, op, r, i);
    if (!res) return;
    if (res.note) { onerror?.(res.note); return; }
    const tpl = document.createElement("template");
    tpl.innerHTML = res.html;
    const nt = tpl.content.firstElementChild as HTMLTableElement | null;
    if (!nt) return;
    tbl.replaceWith(nt);
    const to = focus ? focus(res) : res;
    focusCell(nt, to.r, to.i);
    sync(); refreshTable();
  }
  function deleteTable() {
    ensureSel();
    const c = cellAt();
    if (!c) return;
    const tbl = posOf(c).tbl;
    const next = tbl.nextElementSibling;
    tbl.remove();
    if (next) { const s = getSelection(); const rg = document.createRange(); rg.setStart(next, 0); rg.collapse(true); s?.removeAllRanges(); s?.addRange(rg); remember(); }
    sync(); refreshTable();
  }
  function applyWidth() {
    const t = widthText.trim().replace(/%$/, "");
    if (t === "" || /^auto$/i.test(t)) { tableOp({ type: "width", value: null }); return; }
    const n = Number(t);
    if (!Number.isFinite(n)) { onerror?.("Enter a width between 1 and 100 percent, or leave it empty for automatic."); return; }
    tableOp({ type: "width", value: n });
  }

  /** Insert block markup at the caret (through execCommand, so it is part of the undo history), then place the caret. */
  function insertBlock(html: string, where: "first-cell" | "cell-end" | "none" = "none") {
    ensureSel();
    const before = new Set(Array.from(el.querySelectorAll("table")));
    run("insertHTML", html);
    if (where === "none") return;
    const nt = Array.from(el.querySelectorAll("table")).find((t) => !before.has(t));
    if (nt) { focusCell(nt, 0, 0, where === "first-cell"); refreshTable(); }
  }
  function insertTable() {
    const d = clampDims(Number(tRows), Number(tCols));
    tRows = d.rows; tCols = d.cols;
    tableOpen = false;
    insertBlock(tableBlock(newTable(d.rows, d.cols, { header: tHeader })), "first-cell");
  }
  function insertCallout(kind: CalloutKind) { insertBlock(calloutHtml(kind), "cell-end"); }
  function openButton() { colorOpen = false; remember(); btnOpen = true; tableOpen = false; linkOpen = false; queueMicrotask(() => btnEl?.focus()); }
  function applyButton() {
    const h = buttonHtml(btnLabel, btnUrl, btnColor);
    if (!h) { onerror?.(btnLabel.trim() ? "Enter a web (https or http) or mail (mailto) address for the button." : "Give the button a label."); return; }
    btnOpen = false; btnLabel = ""; btnUrl = "";
    insertBlock(h);
  }
  function toggleCheckHere() {
    ensureSel();
    const s = getSelection();
    const n = s?.anchorNode;
    const e = n instanceof Element ? n : n?.parentElement;
    const block = e?.closest("div,li,p,td,th") as HTMLElement | null;
    if (!block || !el.contains(block)) return;
    const w = document.createTreeWalker(block, NodeFilter.SHOW_TEXT);
    const t = w.nextNode() as Text | null;
    if (!t || !(t.data.startsWith(CHECK_OFF) || t.data.startsWith(CHECK_ON))) return;
    t.data = toggleCheck(t.data);
    sync();
  }

  function insertImage(f: File) {
    if (f.size > MAX_IMG) { onerror?.(`“${f.name || "Image"}” is over 5 MB. Attach it as a file instead.`); return; }
    const r = new FileReader();
    r.onload = () => { if (typeof r.result === "string") run("insertImage", r.result); };
    r.onerror = () => onerror?.("Could not read that picture.");
    r.readAsDataURL(f);
  }

  async function paste(e: ClipboardEvent) {
    const cd = e.clipboardData;
    if (!cd) return;
    const pics = Array.from(cd.files).filter((f) => /^image\/(png|jpeg|gif|webp)$/.test(f.type));
    if (pics.length) { e.preventDefault(); pics.forEach(insertImage); return; }
    const h = cd.getData("text/html"), t = cd.getData("text/plain");
    e.preventDefault();
    // Excel / Sheets / web tables (or tab-separated text) become a clean table. Everything still goes through the
    // backend sanitiser before it is inserted.
    let tbl: ReturnType<typeof tableFromClipboard> = null;
    try { tbl = tableFromClipboard(h, t); } catch { /* never let table detection break paste: use the ordinary path */ }
    if (tbl) {
      try {
        run("insertHTML", await api.sanitizeCompose(tbl.kind === "table" ? tableBlock(tbl.model) : tbl.html));
        if (tbl.kind === "table" && tbl.truncated) onerror?.(`That table is large: only the first ${MAX_ROWS} rows and ${MAX_COLS} columns were pasted.`);
        return;
      } catch { /* fall back to the ordinary paste below */ }
    }
    if (h) {
      try { run("insertHTML", await api.sanitizeCompose(h)); return; } catch { /* fall back to text */ }
    }
    if (t) run("insertText", t);
  }

  // Dropped text/HTML becomes plain text at the drop point; dropped files bubble up and become attachments.
  function drop(e: DragEvent) {
    const dt = e.dataTransfer;
    if (!dt || Array.from(dt.types).includes("Files")) return;
    e.preventDefault();
    const t = dt.getData("text/plain");
    if (!t) return;
    const r = (document as Document & { caretRangeFromPoint?: (x: number, y: number) => Range | null }).caretRangeFromPoint?.(e.clientX, e.clientY);
    if (r) { const s = getSelection(); s?.removeAllRanges(); s?.addRange(r); }
    run("insertText", t);
  }

  function key(e: KeyboardEvent) {
    if (e.key === "Tab" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      const c = cellAt();
      if (c) {
        e.preventDefault();
        const { tbl, r, i } = posOf(c);
        const tgt = tabTarget(ownRows(tbl).map((row) => row.cells.length), r, i, e.shiftKey);
        if (tgt === "addRow") tableOp({ type: "rowBelow" }, (res) => ({ r: res.r, i: 0 }));
        else if (tgt) focusCell(tbl, tgt.r, tgt.i, true);
        return;
      }
    }
    if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "k") { e.preventDefault(); openLink(); }
  }

  onMount(() => {
    let gone = false;
    safeInitialHtml(html, api.sanitizeCompose).then((safe) => { if (!gone && !ready) { el.innerHTML = safe; ready = true; } });
    document.addEventListener("selectionchange", refresh);
    return () => { gone = true; document.removeEventListener("selectionchange", refresh); };
  });

  // Toolbar buttons must not steal the editor's selection.
  const keep = (e: MouseEvent) => e.preventDefault();
</script>

<div class="rich">
  <div class="tb" role="toolbar" aria-label="Formatting">
    <button type="button" class:on={active.bold} aria-pressed={active.bold} aria-label="Bold" title="Bold (⌘B)" onmousedown={keep} onclick={() => run("bold")}><b>B</b></button>
    <button type="button" class:on={active.italic} aria-pressed={active.italic} aria-label="Italic" title="Italic (⌘I)" onmousedown={keep} onclick={() => run("italic")}><i>I</i></button>
    <button type="button" class:on={active.underline} aria-pressed={active.underline} aria-label="Underline" title="Underline (⌘U)" onmousedown={keep} onclick={() => run("underline")}><u>U</u></button>
    <button type="button" class:on={active.strikeThrough} aria-pressed={active.strikeThrough} aria-label="Strikethrough" title="Strikethrough" onmousedown={keep} onclick={() => run("strikeThrough")}><s>S</s></button>
    <span class="sep"></span>
    <button type="button" class:on={active.ul} aria-pressed={active.ul} aria-label="Bulleted list" title="Bulleted list" onmousedown={keep} onclick={() => run("insertUnorderedList")}>• List</button>
    <button type="button" class:on={active.ol} aria-pressed={active.ol} aria-label="Numbered list" title="Numbered list" onmousedown={keep} onclick={() => run("insertOrderedList")}>1. List</button>
    <button type="button" aria-label="Quote" title="Quote" onmousedown={keep} onclick={toggleQuote}>“ Quote</button>
    <button type="button" aria-label="Link" title="Link (⌘K)" onmousedown={keep} onclick={openLink}>Link</button>
    <span class="sep"></span>
    <Select ariaLabel="Font family" value="" placeholder="Font" compact keepFocus block={false} options={FONTS.slice(1).map(([n, v]) => ({ value: v, label: n }))} onchange={(v) => { remember(); restore(); run("fontName", v); }} />
    <Select ariaLabel="Font size" value="" placeholder="Size" compact keepFocus block={false} options={SIZES.slice(1).map(([n, v]) => ({ value: v, label: n }))} onchange={(v) => { remember(); restore(); run("fontSize", v); }} />
    <button type="button" bind:this={colBtn} class="colbtn" class:on={colorOpen} aria-expanded={colorOpen} aria-haspopup="true" aria-label="Text colour" title="Text colour" onmousedown={keep} onclick={toggleColor}><span class="colA" style="border-bottom-color:{curColor ?? 'currentColor'}">A</span></button>
    <button type="button" aria-label="Clear formatting" title="Clear formatting" onmousedown={keep} onclick={clearFormat}>Clear</button>
    <span class="sep"></span>
    <button type="button" class:on={tableOpen} aria-expanded={tableOpen} aria-label="Insert table" title="Insert table" onmousedown={keep} onclick={() => { remember(); colorOpen = false; tableOpen = !tableOpen; btnOpen = false; linkOpen = false; }}>Table</button>
    <Select ariaLabel="Insert callout box" value="" placeholder="Callout" compact keepFocus block={false} options={[{ value: "info", label: "Info" }, { value: "success", label: "Success" }, { value: "warning", label: "Warning" }, { value: "note", label: "Note" }]} onchange={(v) => { remember(); restore(); insertCallout(v as CalloutKind); }} />
    <button type="button" aria-label="Insert button" title="Insert a button that links to a web page or address" onmousedown={keep} onclick={openButton}>Button</button>
    <button type="button" aria-label="Insert divider line" title="Divider line" onmousedown={keep} onclick={() => insertBlock(dividerHtml())}>Line</button>
    <button type="button" aria-label="Insert checklist" title="Checklist" onmousedown={keep} onclick={() => insertBlock(checklistHtml(3))}>{CHECK_OFF} List</button>
    <button type="button" aria-label="Tick or untick this checklist line" title="Tick or untick this line" onmousedown={keep} onclick={toggleCheckHere}>{CHECK_ON}</button>
  </div>
  {#if ts?.tooLarge}
    <div class="tb tt" role="status"><span class="lbl">Table</span> This table is too large to edit here.</div>
  {:else if ts}
    <div class="tb tt" role="toolbar" aria-label="Table tools">
      <span class="lbl">Table</span>
      <button type="button" aria-label="Add row above" title="Add row above" onmousedown={keep} onclick={() => tableOp({ type: "rowAbove" })}>+ Row above</button>
      <button type="button" aria-label="Add row below" title="Add row below (Tab in the last cell does this too)" onmousedown={keep} onclick={() => tableOp({ type: "rowBelow" })}>+ Row below</button>
      <button type="button" aria-label="Delete row" title="Delete row" onmousedown={keep} onclick={() => tableOp({ type: "delRow" })}>- Row</button>
      <span class="sep"></span>
      <button type="button" aria-label="Add column left" title="Add column left" onmousedown={keep} onclick={() => tableOp({ type: "colLeft" })}>+ Col left</button>
      <button type="button" aria-label="Add column right" title="Add column right" onmousedown={keep} onclick={() => tableOp({ type: "colRight" })}>+ Col right</button>
      <button type="button" aria-label="Delete column" title="Delete column" onmousedown={keep} onclick={() => tableOp({ type: "delCol" })}>- Col</button>
      <span class="sep"></span>
      <button type="button" class:on={ts.header} aria-pressed={ts.header} title="First row is a header" onmousedown={keep} onclick={() => tableOp({ type: "header" })}>Header</button>
      <button type="button" class:on={ts.zebra} aria-pressed={ts.zebra} title="Alternate row shading" onmousedown={keep} onclick={() => tableOp({ type: "zebra" })}>Stripes</button>
      <button type="button" class:on={ts.border} aria-pressed={ts.border} title="Cell borders" onmousedown={keep} onclick={() => tableOp({ type: "border" })}>Borders</button>
      <label class="wid" title="Width of this column in percent; empty = automatic">Col width
        <input inputmode="numeric" placeholder="auto" aria-label="Column width in percent (empty for automatic)" bind:value={widthText} onfocus={remember}
          onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); applyWidth(); } }} onchange={applyWidth} /> %</label>
      <span class="sep"></span>
      <button type="button" class="danger" aria-label="Delete table" title="Delete the whole table" onmousedown={keep} onclick={deleteTable}>Delete table</button>
    </div>
  {/if}
  {#if colorOpen}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div class="link pal" bind:this={palEl} role="group" aria-label="Text colour" tabindex="-1" onkeydown={palKey}>
      {#each SWATCHES as [name, hex] (name)}
        <button type="button" class="sw" class:cur={curColor === hex} aria-label={name} title={name} style={hex ? `background:${hex}` : undefined} onmousedown={keep} onclick={() => pickColor(hex)}>{#if hex === null}<span aria-hidden="true">A</span>{/if}</button>
      {/each}
      <button type="button" class="btn ghost sm" onclick={() => closeColor()}>Close</button>
    </div>
  {/if}
  {#if tableOpen}
    <form class="link" onsubmit={(e) => { e.preventDefault(); insertTable(); }}>
      <label for="mm-t-rows">Rows</label>
      <input id="mm-t-rows" class="num" type="number" min="1" max={MAX_INSERT_ROWS} bind:value={tRows} />
      <label for="mm-t-cols">Columns</label>
      <input id="mm-t-cols" class="num" type="number" min="1" max={MAX_INSERT_COLS} bind:value={tCols} />
      <label class="chk"><input type="checkbox" bind:checked={tHeader} /> Header row</label>
      <button type="submit" class="btn sm">Insert</button>
      <button type="button" class="btn ghost sm" onclick={() => { tableOpen = false; el.focus(); }}>Cancel</button>
    </form>
  {/if}
  {#if btnOpen}
    <form class="link" onsubmit={(e) => { e.preventDefault(); applyButton(); }}>
      <label for="mm-b-label">Button text</label>
      <input id="mm-b-label" bind:this={btnEl} bind:value={btnLabel} maxlength={MAX_BUTTON_LABEL} placeholder="Read more" autocomplete="off"
        onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); btnOpen = false; el.focus(); } }} />
      <label for="mm-b-url">Links to</label>
      <input id="mm-b-url" bind:value={btnUrl} placeholder="https://example.org" autocomplete="off" spellcheck="false"
        onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); btnOpen = false; el.focus(); } }} />
      <Select ariaLabel="Button colour" value={btnColor} compact block={false} options={BUTTON_COLORS.map((c) => ({ value: c.id, label: c.label }))} onchange={(v) => (btnColor = v as ButtonColor)} />
      <button type="submit" class="btn sm">Add</button>
      <button type="button" class="btn ghost sm" onclick={() => { btnOpen = false; el.focus(); }}>Cancel</button>
    </form>
  {/if}
  {#if linkOpen}
    <form class="link" onsubmit={(e) => { e.preventDefault(); applyLink(); }}>
      <label for="mm-link">Link to</label>
      <input id="mm-link" bind:this={linkEl} bind:value={linkUrl} placeholder="https://example.org" autocomplete="off" spellcheck="false" onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); linkOpen = false; el.focus(); } }} />
      <button type="submit" class="btn sm">Add</button>
      <button type="button" class="btn ghost sm" onclick={() => { linkOpen = false; el.focus(); }}>Cancel</button>
    </form>
  {/if}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="ed" bind:this={el} contenteditable="true" role="textbox" tabindex="0" aria-multiline="true" aria-label={label} spellcheck={spell}
    oninput={() => { sync(); refresh(); }} onpaste={paste} ondrop={drop} onkeydown={key} onkeyup={remember} onmouseup={remember} onblur={remember}></div>
</div>

<svelte:window onpointerdown={outsideColor} />

<style>
  .rich { display: flex; flex-direction: column; flex: 1; min-height: 0; }
  .tb { display: flex; flex-wrap: wrap; align-items: center; gap: 2px; padding: 4px 10px; border-bottom: 1px solid var(--line); }
  .tb button { background: none; border: 1px solid transparent; color: var(--text); border-radius: var(--radius); padding: 3px 8px; font-size: 0.8rem; min-width: 28px; }
  .tb button:hover { border-color: var(--line); }
  .tb button.on { border-color: var(--accent); color: var(--accent); }
  .tb .sep { width: 1px; height: 16px; background: var(--line); margin: 0 4px; }
  .colA { display: inline-block; font-weight: 700; line-height: 1.1; border-bottom: 3px solid currentColor; }
  .pal { flex-wrap: wrap; gap: 6px; }
  .pal .sw { width: 22px; height: 22px; padding: 0; border: 1px solid var(--line); border-radius: var(--radius); cursor: pointer; display: inline-grid; place-items: center; font-size: 0.7rem; font-weight: 700; color: var(--text); background: var(--surface); }
  .pal .sw:hover { border-color: var(--muted); }
  .pal .sw.cur { outline: 2px solid var(--accent); outline-offset: 1px; }
  .pal .sw:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .link { display: flex; align-items: center; gap: 8px; padding: 6px 14px; border-bottom: 1px solid var(--line); font-size: 0.8rem; }
  .link input { flex: 1; min-width: 0; background: none; border: 1px solid var(--line); border-radius: var(--radius); color: var(--ink); padding: 4px 8px; font: 400 13px var(--font-mono); }
  .ed { flex: 1; min-height: 120px; overflow: auto; padding: 14px; outline-offset: -2px; font: 400 14px/1.5 var(--body-font, -apple-system, system-ui, sans-serif); color: var(--text); overflow-wrap: anywhere; }
  .ed :global(blockquote) { margin: 6px 0; padding-left: 10px; border-left: 2px solid var(--line); color: var(--muted); }
  .ed :global(a) { color: var(--accent); }
  .ed :global(img) { max-width: 100%; height: auto; }
  .tt { background: color-mix(in srgb, var(--surface) 92%, var(--accent)); }
  .tt .lbl { font-size: 0.7333rem; text-transform: uppercase; letter-spacing: .1em; color: var(--muted); margin-right: 4px; }
  .tt .wid { display: inline-flex; align-items: center; gap: 4px; font-size: 0.8rem; color: var(--text); }
  .tt .wid input { width: 52px; background: none; border: 1px solid var(--line); border-radius: var(--radius); color: var(--text); padding: 2px 6px; font: 400 12px var(--font-mono); }
  .tt button.danger:hover { border-color: var(--danger); color: var(--danger); }
  .link .num { flex: 0 0 64px; }
  .link .chk { display: inline-flex; align-items: center; gap: 4px; }
  .link .chk input { flex: none; }
  .ed :global(table) { max-width: 100%; }
  .ed :global(td), .ed :global(th) { min-width: 24px; overflow-wrap: anywhere; }
  .ed :global(hr) { margin: 8px 0; }
  .ed :global(ul), .ed :global(ol) { margin: 4px 0; padding-left: 24px; }
  .ed :global([data-mm-sig]) { margin-top: 4px; }
</style>
