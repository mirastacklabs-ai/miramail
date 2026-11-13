// Touch gestures for the calendar time grid (phone). Pure state + math; the DOM glue lives in TimeGrid.svelte.
// A finger that stays put for GRID_LONG_MS arms a drag (create on an empty slot, move on an event);
// a finger that travels first is a scroll and is left alone.
import { LONG_PRESS_SLOP } from "./phonemodel.ts";

export const GRID_LONG_MS = 400;
export type LpState = "idle" | "pending" | "armed" | "cancelled";

export const lpStart = (touches: number): LpState => (touches === 1 ? "pending" : "idle");
/** The timer elapsed: only a still-pending press arms. */
export const lpArm = (s: LpState): LpState => (s === "pending" ? "armed" : s);
/** Finger moved by (dx, dy) from where it landed. Travel before arming is a scroll; after arming it drives the drag. */
export const lpMove = (s: LpState, dx: number, dy: number): LpState => (s === "pending" && Math.hypot(dx, dy) > LONG_PRESS_SLOP ? "cancelled" : s);
/** Touch scrolling must be blocked only while a drag is armed. */
export const lpBlocksScroll = (s: LpState): boolean => s === "armed";

/** Minute of day under a finger, snapped; `top` is the column's top edge, `hh` the pixel height of one hour. */
export function minuteAt(clientY: number, top: number, hh: number, step = 15): number {
  const m = Math.round((((clientY - top) / hh) * 60) / step) * step;
  return Math.min(24 * 60, Math.max(0, m));
}
/** Start minute of a dragged event: the pointer minus where inside the event it was grabbed, kept within the day. */
export function moveStartMin(pointerMin: number, grabOffsetMin: number, durMin: number, step = 15): number {
  const s = Math.round((pointerMin - grabOffsetMin) / step) * step;
  return Math.max(0, Math.min(Math.max(0, 24 * 60 - durMin), s));
}
/** Index of the column whose horizontal span contains x (nearest edge when outside). */
export function columnAt(x: number, rects: { left: number; right: number }[]): number {
  if (!rects.length) return -1;
  const i = rects.findIndex((r) => x >= r.left && x < r.right);
  return i >= 0 ? i : x < rects[0].left ? 0 : rects.length - 1;
}
/** Create range from a long press: a bare press (under 15 min of drag) makes a one-hour slot. */
export function createRange(a: number, b: number): { start: number; end: number } {
  const s = Math.min(a, b), e = Math.max(a, b);
  return e - s >= 15 ? { start: s, end: e } : { start: s, end: Math.min(24 * 60, s + 60) };
}
