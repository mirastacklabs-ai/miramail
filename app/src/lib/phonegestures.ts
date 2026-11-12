// Touch gestures for the phone layout (DOM glue; the decisions live in phonemodel.ts and are unit-tested there).
import { isBackSwipe, pull, PULL_TRIGGER, longPressOwns, rowSwipeMayStart, swipedRecently, swipeEnd, swipeOffset, type SwipeEnd } from "./phonemodel";

/** iOS-style edge swipe back, listened for on the window. Returns a cleanup. */
export function edgeSwipe(onBack: () => void): () => void {
  let sx = -1, sy = 0, done = false;
  const start = (e: TouchEvent) => { const t = e.touches[0]; sx = t.clientX; sy = t.clientY; done = false; };
  const move = (e: TouchEvent) => {
    if (sx < 0 || done) return;
    const t = e.touches[0];
    if (isBackSwipe(sx, t.clientX - sx, t.clientY - sy)) { done = true; onBack(); }
  };
  const end = () => { sx = -1; };
  window.addEventListener("touchstart", start, { passive: true });
  window.addEventListener("touchmove", move, { passive: true });
  window.addEventListener("touchend", end, { passive: true });
  window.addEventListener("touchcancel", end, { passive: true });
  return () => { window.removeEventListener("touchstart", start); window.removeEventListener("touchmove", move); window.removeEventListener("touchend", end); window.removeEventListener("touchcancel", end); };
}

export interface PullOpts { /** false while a refresh runs (no re-entry) */ canPull?: () => boolean; onPull: (offset: number, ready: boolean) => void; onRelease: (ready: boolean) => void }
/** Pull-to-refresh on a scroll container (only from scrollTop 0). */
export function pullRefresh(node: HTMLElement, opts: PullOpts) {
  let y0 = -1, ready = false, pulling = false;
  const start = (e: TouchEvent) => { y0 = node.scrollTop <= 0 && (opts.canPull?.() ?? true) ? e.touches[0].clientY : -1; ready = false; pulling = false; };
  const move = (e: TouchEvent) => {
    if (y0 < 0) return;
    const r = pull(e.touches[0].clientY - y0, node.scrollTop);
    pulling = r.offset > 0; ready = r.ready;
    opts.onPull(r.offset, r.ready);
  };
  const end = () => { if (pulling) opts.onRelease(ready); y0 = -1; pulling = false; ready = false; opts.onPull(0, false); };
  node.addEventListener("touchstart", start, { passive: true });
  node.addEventListener("touchmove", move, { passive: true });
  node.addEventListener("touchend", end, { passive: true });
  node.addEventListener("touchcancel", end, { passive: true });
  return { destroy() { node.removeEventListener("touchstart", start); node.removeEventListener("touchmove", move); node.removeEventListener("touchend", end); node.removeEventListener("touchcancel", end); } };
}
export { PULL_TRIGGER };

export interface SwipeOpts { /** false: this row has no swipe actions (e.g. a search result from another folder) */ enabled?: () => boolean; isOpen: () => boolean; onMove: (offset: number) => void; onEnd: (r: SwipeEnd) => void }
/** Horizontal swipe on a list row. Vertical movement is left to the browser (the row has touch-action: pan-y). */
export function swipeRow(node: HTMLElement, opts: SwipeOpts) {
  let x0 = 0, y0 = 0, axis: "" | "x" | "y" = "", startOpen = false, active = false;
  const start = (e: TouchEvent) => {
    const t = e.touches[0];
    // A touch that begins in the left edge zone belongs to the back swipe, never to the row.
    if (!rowSwipeMayStart(t.clientX) || opts.enabled?.() === false) { active = false; return; }
    x0 = t.clientX; y0 = t.clientY; axis = ""; active = true; startOpen = opts.isOpen();
  };
  const move = (e: TouchEvent) => {
    if (!active) return;
    if (longPressOwns(node.dataset.lp)) { active = false; if (axis === "x") opts.onEnd(startOpen ? "open" : "close"); axis = ""; return; }
    const t = e.touches[0], dx = t.clientX - x0, dy = t.clientY - y0;
    if (!axis && Math.hypot(dx, dy) > 8) axis = Math.abs(dx) > Math.abs(dy) * 1.3 ? "x" : "y";
    if (axis !== "x") return;
    e.preventDefault();
    opts.onMove(swipeOffset(dx, startOpen));
  };
  const end = (e: TouchEvent) => {
    if (!active) return;
    active = false;
    if (axis !== "x" || longPressOwns(node.dataset.lp)) return;
    const dx = (e.changedTouches[0]?.clientX ?? x0) - x0;
    node.dataset.swiped = String(Date.now()); // the click that follows a swipe must not open the message
    opts.onEnd(swipeEnd(dx, startOpen));
  };
  // Only an x-axis swipe in progress needs settling; a cancelled vertical scroll must not touch the row.
  const cancel = () => { const was = active && axis === "x"; active = false; if (was) opts.onEnd(startOpen ? "open" : "close"); };
  node.addEventListener("touchstart", start, { passive: true });
  node.addEventListener("touchmove", move, { passive: false });
  node.addEventListener("touchend", end, { passive: true });
  node.addEventListener("touchcancel", cancel, { passive: true });
  return { destroy() { node.removeEventListener("touchstart", start); node.removeEventListener("touchmove", move); node.removeEventListener("touchend", end); node.removeEventListener("touchcancel", cancel); } };
}
/** True right after a swipe ended on this row. `el` may be any element inside the swiped row (the stamp sits on the swiped node). */
export const justSwiped = (el: HTMLElement): boolean => swipedRecently((el.closest("[data-swiped]") as HTMLElement | null)?.dataset.swiped, Date.now());
