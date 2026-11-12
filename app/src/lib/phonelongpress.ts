// Long-press on a list row (DOM glue; thresholds live in phonemodel.ts). Fires once after LONG_PRESS_MS if the finger has not travelled.
import { LONG_PRESS_MS, longPressMoved } from "./phonemodel";

export interface LongPressOpts { enabled?: () => boolean; onLong: () => void }
export function longPress(node: HTMLElement, opts: LongPressOpts) {
  let timer: ReturnType<typeof setTimeout> | undefined, x0 = 0, y0 = 0, fired = false;
  const clear = () => { clearTimeout(timer); timer = undefined; };
  const start = (e: TouchEvent) => {
    clear(); fired = false; delete node.dataset.lp;
    if (e.touches.length !== 1 || opts.enabled?.() === false) return;
    x0 = e.touches[0].clientX; y0 = e.touches[0].clientY;
    timer = setTimeout(() => { timer = undefined; fired = true; node.dataset.lp = "1"; try { navigator.vibrate?.(8); } catch { /* ignore */ } opts.onLong(); }, LONG_PRESS_MS);
  };
  const move = (e: TouchEvent) => { if (timer && longPressMoved(e.touches[0].clientX - x0, e.touches[0].clientY - y0)) clear(); };
  // The click that ends a long press must not also open the row: stamp it like a swipe (justSwiped reads data-swiped).
  // touchend is non-passive so preventDefault() can suppress the synthetic click iOS would send to whatever the sheet put under the finger.
  const end = (e: TouchEvent) => {
    clear();
    if (fired) { node.dataset.swiped = String(Date.now()); if (e.type === "touchend" && e.cancelable) e.preventDefault(); }
    fired = false; delete node.dataset.lp;
  };
  node.addEventListener("touchstart", start, { passive: true });
  node.addEventListener("touchmove", move, { passive: true });
  node.addEventListener("touchend", end, { passive: false });
  node.addEventListener("touchcancel", end, { passive: true });
  return { destroy() { clear(); node.removeEventListener("touchstart", start); node.removeEventListener("touchmove", move); node.removeEventListener("touchend", end); node.removeEventListener("touchcancel", end); } };
}
