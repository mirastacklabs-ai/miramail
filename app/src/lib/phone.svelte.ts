// Phone layout state: when it applies, the navigation stack, the remembered folder, the on-screen keyboard.
// "Phone layout" is decided in one place (phonemodel.isPhoneLayout) by OS + screen size, so Android can reuse it later.
import { app } from "./state.svelte";
import { workspace } from "./workspace.svelte";
import { back, canGoBack, folderKey, isPhoneLayout, navAfterAccountChange, viewForTab, openList, openReader, openSection, selectTab, startFolder, startNav, keyboardInset, type Nav, type Tab } from "./phonemodel";

function lsGet(k: string): string | null { try { return localStorage.getItem(k); } catch { return null; } }
function lsSet(k: string, v: string) { try { localStorage.setItem(k, v); } catch { /* ignore */ } }

function detect(): boolean {
  if (typeof window === "undefined" || typeof navigator === "undefined") return false;
  let force: string | null = null;
  try { force = new URLSearchParams(window.location.search).get("layout"); } catch { /* ignore */ }
  const s = window.screen;
  // ?layout=phone is a browser dev switch: ignored in any shipped build.
  return isPhoneLayout({ ua: navigator.userAgent, maxTouchPoints: navigator.maxTouchPoints ?? 0, shortSide: Math.min(s?.width ?? 0, s?.height ?? 0), force, dev: import.meta.env.DEV });
}

class Phone {
  /** The phone layout is active (decided once at startup). */
  readonly on = detect();
  nav = $state<Nav>(startNav());
  /** Height of the on-screen keyboard in px (0 when hidden). */
  kb = $state(0);
  /** A text field / editor has focus (the keyboard is up or about to be), even if the viewport has not resized yet. */
  typing = $state(false);
  private restored = false;
  private lastAccount: string | null = null;
  get kbOpen(): boolean { return this.kb > 0 || this.typing; }

  constructor() {
    if (!this.on || typeof document === "undefined") return;
    const root = document.documentElement;
    root.classList.add("phone");
    // Edge-to-edge under the notch / home indicator; env(safe-area-inset-*) then keeps content clear of them.
    const meta = document.querySelector('meta[name="viewport"]');
    meta?.setAttribute("content", "width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no, viewport-fit=cover");
    const vv = window.visualViewport;
    const sync = () => {
      const h = vv?.height ?? window.innerHeight, top = vv?.offsetTop ?? 0;
      root.style.setProperty("--vv-h", `${h}px`);
      root.style.setProperty("--vv-top", `${top}px`);
      this.kb = keyboardInset(window.innerHeight, h, top);
    };
    sync();
    vv?.addEventListener("resize", sync);
    vv?.addEventListener("scroll", sync);
    window.addEventListener("orientationchange", sync);
    // The keyboard pans the visual viewport; the layout follows it through --vv-top / --vv-h above. The page itself is
    // position: fixed, so it is not scrolled back to (0,0) here: that fought iOS's own scroll-to-focused-field.
    const editable = (t: EventTarget | null) => t instanceof HTMLElement && (t.isContentEditable || t instanceof HTMLTextAreaElement || (t instanceof HTMLInputElement && !/^(button|checkbox|radio|submit|file|range|color|reset|image)$/.test(t.type)));
    document.addEventListener("focusin", (e) => { this.typing = editable(e.target); });
    document.addEventListener("focusout", () => { setTimeout(() => { this.typing = editable(document.activeElement); }, 0); });
  }

  get canBack(): boolean { return canGoBack(this.nav); }
  back() {
    if (this.nav.tab === "mail" && this.nav.mail === "reader") { app.selectedUid = null; app.body = null; }
    this.nav = back(this.nav);
  }
  tab(t: Tab) {
    app.view = viewForTab(t); // Calendar tab = calendar view, every other tab = mail (never a stale value)
    this.nav = selectTab(this.nav, t);
  }
  section(id: string) { this.nav = openSection(this.nav, id); }

  /** The active account changed (switch / removal): the open list or message belonged to the old one. */
  accountChanged(account: string | null) {
    if (account === this.lastAccount) return;
    const hadAccount = this.lastAccount !== null; // startup (null -> first account) keeps the restored folder / screen
    this.lastAccount = account;
    if (!hadAccount && account !== null) return;
    app.selectedUid = null; app.body = null;
    this.nav = navAfterAccountChange(this.nav);
  }

  async openFolder(name: string) {
    lsSet(folderKey(app.account), name);
    if (app.folder !== name) await app.selectFolder(name);
    this.nav = openList(this.nav);
  }
  async openMessage(uid: number) {
    this.nav = openReader(this.nav);
    await app.open(uid);
  }
  /** After an archive / trash / move from the reader the message is gone: return to the list. */
  async afterRemoval(run: () => Promise<void>) {
    const uid = app.selectedUid;
    await run();
    if (this.nav.tab === "mail" && this.nav.mail === "reader" && !app.messages.some((m) => m.uid === uid)) this.back();
  }

  /** First start: open the folder used last time (or the inbox) once the account's folders are known. */
  restoreFolder() {
    if (this.restored || !app.folders.length) return;
    this.restored = true;
    const saved = lsGet(folderKey(app.account));
    const target = startFolder(app.folders, saved);
    if (saved && target) { this.nav = openList(this.nav); if (target !== app.folder) void app.selectFolder(target); }
  }

  /** A compose or event-editor pane is the focused workspace pane: shown as a full-screen sheet. */
  get sheetPaneId(): string | null {
    const p = workspace.focused;
    return p && (p.kind === "compose" || p.kind === "event") ? p.id : null;
  }
}

export const phone = new Phone();
