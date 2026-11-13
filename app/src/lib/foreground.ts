// The page's side of the iPhone presence rule (see foreground-model.ts and app/src-tauri/src/presence.rs).
// `startForeground()` runs once per window at start: it listens for `app://presence` FIRST and only then asks
// `launch_state`, so a change between the two is never missed (`seq` orders them).
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { errMsg, isTauri } from "./api";
import { ForegroundGate, isDeferredMsg, type Presence } from "./foreground-model";

export const fg = new ForegroundGate();

/** True for an error that only means "refused while the user is away"; callers stay silent and retry when they are back. */
export const isDeferred = (e: unknown): boolean => isDeferredMsg(errMsg(e));

let started = false;
export async function startForeground(): Promise<void> {
  if (started) return;
  started = true;
  if (!isTauri) { fg.apply({ present: true, seq: 0 }); return; } // plain browser (npm run dev)
  try { await listen<Presence>("app://presence", (e) => fg.apply(e.payload)); } catch { /* no event bus: the answer below decides */ }
  // A failed answer cannot happen in practice; if it did, the backend still refuses network work while the user is away.
  try { fg.apply(await invoke<Presence>("launch_state")); } catch { fg.apply({ present: true, seq: 0 }); }
}
