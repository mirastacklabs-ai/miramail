// Last-resort error surface: an uncaught exception or unhandled rejection becomes a visible toast instead of a
// silently frozen view. Pure helpers (no Svelte, no DOM types beyond EventTarget): safe under node --test.

/** Benign browser noise that must not become a toast. */
const IGNORED = [/ResizeObserver loop/i];

/** One readable line for anything that can be thrown or rejected. */
export function describeError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object") {
    const o = e as { name?: unknown; message?: unknown };
    if (typeof o.message === "string" && o.message) {
      const name = typeof o.name === "string" && o.name && o.name !== "Error" ? `${o.name}: ` : "";
      return name + o.message;
    }
    try { const j = JSON.stringify(e); if (j && j !== "{}") return j; } catch { /* fall through */ }
  }
  return e === undefined || e === null ? "Unknown error" : String(e);
}

export function isIgnored(msg: string): boolean { return IGNORED.some((r) => r.test(msg)); }

/**
 * Report `error` and `unhandledrejection` events through `notify`. The same message is reported at most once
 * per `dedupeMs` so an error thrown on every frame cannot flood the UI. Returns the uninstall function.
 */
export function installErrorSurface(
  target: Pick<EventTarget, "addEventListener" | "removeEventListener">,
  notify: (msg: string) => void,
  opts: { dedupeMs?: number; now?: () => number; log?: (...a: unknown[]) => void } = {},
): () => void {
  const dedupeMs = opts.dedupeMs ?? 5000, now = opts.now ?? Date.now, log = opts.log ?? console.error;
  const seen = new Map<string, number>();
  const report = (raw: unknown) => {
    const msg = describeError(raw);
    if (isIgnored(msg)) return;
    log("[miramail] uncaught:", raw);
    const t = now(), last = seen.get(msg);
    if (last !== undefined && t - last < dedupeMs) return;
    seen.set(msg, t);
    notify(`Unexpected error: ${msg}`);
  };
  const onError = (ev: Event) => { const e = ev as ErrorEvent; report(e.error ?? e.message); };
  const onRejection = (ev: Event) => report((ev as PromiseRejectionEvent).reason);
  target.addEventListener("error", onError);
  target.addEventListener("unhandledrejection", onRejection);
  return () => { target.removeEventListener("error", onError); target.removeEventListener("unhandledrejection", onRejection); };
}
