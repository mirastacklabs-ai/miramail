// iPhone owner rule (app/src-tauri/src/presence.rs): while the user is not in front of MiraMail (iOS launched it in the
// background for a refresh, or woke it suspended), the page may only read local data. Anything that talks to a mail
// server or changes mail waits until the user is present. Pure state, no Tauri imports, so it is unit-tested in node.
// On a Mac the first report is always "present" and nothing ever changes.

/** Prefix of the error a command returns when it was refused because the user is away (Rust `presence::DEFERRED`). */
export const DEFERRED_PREFIX = "deferred:";
export const isDeferredMsg = (msg: string): boolean => msg.startsWith(DEFERRED_PREFIX);

/** The `launch_state` answer and the `app://presence` event payload. `seq` counts changes in the native process. */
export interface Presence { present: boolean; seq: number }

export class ForegroundGate {
  private cur: Presence | null = null;
  private readyWaiters: Array<() => void> = [];
  private presentWaiters: Array<() => void> = [];
  private enter = new Set<() => void>();

  /** A report (`launch_state` answer or `app://presence` event). One older than the last applied one is ignored, so the
   * order in which the answer and an event arrive does not matter. */
  apply(p: Presence): void {
    if (this.cur && p.seq < this.cur.seq) return;
    const was = this.cur;
    this.cur = { present: p.present, seq: p.seq };
    if (!was) for (const f of this.readyWaiters.splice(0)) f();
    if (p.present) {
      for (const f of this.presentWaiters.splice(0)) f();
      if (was && !was.present) for (const f of [...this.enter]) f();
    }
  }

  /** The first report arrived. */
  get known(): boolean { return this.cur !== null; }
  /** The user is in front of the app (false until the first report). */
  get present(): boolean { return this.cur?.present === true; }

  /** Resolves once the first report arrived (then `present` is meaningful). */
  ready(): Promise<void> {
    return this.cur ? Promise.resolve() : new Promise((r) => this.readyWaiters.push(r));
  }
  /** Resolves when the user is (or becomes) present. Use for one-shot work, not for timers (they would pile up). */
  whenPresent(): Promise<void> {
    return this.present ? Promise.resolve() : new Promise((r) => this.presentWaiters.push(r));
  }
  /** Called each time the user arrives after being away (not for a launch that starts with the user present). */
  onEnter(cb: () => void): () => void {
    this.enter.add(cb);
    return () => { this.enter.delete(cb); };
  }
}
