// Keep the typing caret visible inside the phone compose scroll area (DOM glue; the arithmetic is phonemodel.caretScrollDelta).
import { caretScrollDelta } from "./phonemodel";

const MIRROR_PROPS = ["fontFamily", "fontSize", "fontWeight", "lineHeight", "letterSpacing", "paddingTop", "paddingRight", "paddingBottom", "paddingLeft", "textIndent", "tabSize"] as const;

/** Viewport rect of the caret in a textarea, measured with a hidden mirror of its text up to the caret. */
function textareaCaretRect(ta: HTMLTextAreaElement): DOMRect {
  const cs = getComputedStyle(ta), box = ta.getBoundingClientRect();
  const m = document.createElement("div");
  for (const p of MIRROR_PROPS) m.style[p] = cs[p];
  m.style.position = "absolute";
  m.style.visibility = "hidden";
  m.style.whiteSpace = "pre-wrap";
  m.style.overflowWrap = "anywhere";
  m.style.top = "0";
  m.style.left = "-9999px";
  m.style.boxSizing = "border-box";
  m.style.width = `${box.width}px`;
  m.textContent = ta.value.slice(0, ta.selectionEnd ?? ta.value.length);
  const tail = document.createElement("span");
  tail.textContent = "​";
  m.appendChild(tail);
  document.body.appendChild(m);
  const y = tail.offsetTop, h = tail.offsetHeight || parseFloat(cs.lineHeight) || 24;
  m.remove();
  return new DOMRect(box.left, box.top + y - ta.scrollTop, 1, h);
}

function editableCaretRect(): DOMRect | null {
  const s = getSelection();
  if (!s || !s.rangeCount) return null;
  const r = s.getRangeAt(0).cloneRange();
  r.collapse(false);
  const rect = r.getClientRects()[0];
  if (rect && (rect.height > 0 || rect.top > 0)) return rect;
  const n = r.startContainer, e = n instanceof Element ? n : n.parentElement;
  return e ? e.getBoundingClientRect() : null;
}

function measureAndScroll(target: HTMLElement, bottomInset: number): void {
  const box = target.closest<HTMLElement>("[data-caret-scroll]");
  if (!box) return;
  const c = target instanceof HTMLTextAreaElement ? textareaCaretRect(target) : editableCaretRect();
  if (!c) return;
  const v = box.getBoundingClientRect();
  const d = caretScrollDelta(c.top, c.bottom, v.top, v.bottom - bottomInset);
  if (d) box.scrollTop += d;
}

let pending: { target: HTMLElement; bottomInset: number } | null = null;
/** Scroll the nearest [data-caret-scroll] container so the caret clears its edges and `bottomInset` px (the docked format bar).
 *  The measurement (it builds a mirror of the whole text) runs at most once per animation frame, however fast the typing. */
export function keepCaretVisible(target: HTMLElement, bottomInset = 0): void {
  const queued = pending !== null;
  pending = { target, bottomInset };
  if (queued) return;
  requestAnimationFrame(() => {
    const p = pending;
    pending = null;
    if (p && p.target.isConnected) measureAndScroll(p.target, p.bottomInset);
  });
}
