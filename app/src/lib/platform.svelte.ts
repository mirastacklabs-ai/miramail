// Native menu bar, mailto: links, tray / widget / notification navigation. Main window only (App.svelte calls start()).
import { app } from "./state.svelte";
import { workspace } from "./workspace.svelte";
import { cal } from "./cal.svelte";
import { replyFresh, replyTo } from "./compose";
import { onMailto, onMenuAction, onNotice, onOpenInbox, onOpenMessage, platformApi, type MailtoDraft } from "./platformapi";
import type { FolderRole } from "./types";

const GO: Record<string, FolderRole> = {
  "go-inbox": "inbox", "go-drafts": "drafts", "go-sent": "sent", "go-archive": "archive", "go-trash": "trash", "go-junk": "junk",
};

class Platform {
  showCheat = $state(false);

  constructor() {
    // Tab shortcuts (Ctrl+Tab, Cmd+3..9, Cmd+[ ]) stay quiet while the shortcut sheet or a calendar popover / modal is open.
    workspace.keyBlock = () => this.showCheat || cal.modalOpen || !!cal.popover;
  }

  /** Open the folder with this role in the current account. */
  async goRole(role: FolderRole) {
    const f = app.folders.find((x) => x.role === role && x.selectable);
    if (!f) { app.notify(`This account has no ${role} folder.`); return; }
    app.view = "mail";
    if (app.folder !== f.name) await app.selectFolder(f.name);
  }

  /** Same functions the keyboard shortcuts call. */
  async run(action: string) {
    if (GO[action]) { await this.goRole(GO[action]); return; }
    if (!app.accounts.length && action !== "preferences" && action !== "about" && action !== "shortcuts") return;
    switch (action) {
      case "preferences": if (app.accounts.length) app.showSettings = true; break;
      case "about": app.showAbout = true; break;
      case "shortcuts": this.showCheat = !this.showCheat; break;
      case "new-message": replyFresh(); break;
      case "check-mail": void app.sync(); void app.refreshUnread(); break;
      // Tabs layout: Cmd+1 / Cmd+2 are the first / second tab (the keydown handler leaves these two to the menu so it never fires twice).
      case "view-mail": if (workspace.tabsOn) { if (!workspace.keyBlock?.()) { workspace.jumpTab(1); workspace.tabFocus(); } } else app.view = "mail"; break;
      case "view-calendar": if (workspace.tabsOn) { if (!workspace.keyBlock?.()) { workspace.jumpTab(2); workspace.tabFocus(); } } else app.view = "calendar"; break;
      case "toggle-view": workspace.toggleView(); break;
      case "toggle-ai": workspace.toggleAi(); break;
      case "cycle-theme": app.cycleTheme(); break;
      case "reply": void replyTo(false); break;
      case "reply-all": void replyTo(true); break;
      case "forward": void replyTo(false, true); break;
      case "archive": void app.moveTo("archive"); break;
      case "trash": void app.moveTo("trash"); break;
      case "mark-read": void app.setFlag("seen", true); break;
      case "mark-unread": void app.setFlag("seen", false); break;
      case "toggle-flag": { const m = app.selected; if (m) void app.setFlag("flagged", !m.flags.flagged); break; }
    }
  }

  async openInbox(account: string) {
    if (!app.accounts.some((a) => a.email === account)) return;
    app.view = "mail";
    if (app.account !== account) await app.selectAccount(account);
    else await this.goRole("inbox");
  }

  /** Select a message in the main window (tray / widget / notification click). */
  async openMessage(account: string, folder: string, uid: number, selectOnly = false) {
    if (!app.accounts.some((a) => a.email === account)) return;
    app.view = "mail";
    if (app.account !== account) await app.selectAccount(account);
    if (app.folder !== folder) await app.selectFolder(folder);
    if (!app.messages.some((m) => m.uid === uid)) await app.loadMessages();
    app.selectedUid = uid;
    if (!selectOnly) await app.open(uid);
  }

  openMailto(d: MailtoDraft) {
    const acc = app.account;
    if (!acc) { app.notify("Add an account first, then open the mailto link again."); return; }
    workspace.openCompose({ to: d.to, cc: d.cc, bcc: d.bcc, subject: d.subject, text: d.text, inReplyTo: null }, acc);
  }

  async drainMailto() {
    try { for (const d of await platformApi.takePendingMailto()) this.openMailto(d); } catch (e) { app.fail(e); }
  }

  /** Subscribe to native events; returns the unsubscribe function. */
  async start(): Promise<() => void> {
    const offs = await Promise.all([
      onMenuAction((a) => { void this.run(a); }),
      onMailto(() => { void this.drainMailto(); }),
      onOpenInbox((a) => { void this.openInbox(a); }),
      onOpenMessage((m) => { void this.openMessage(m.account, m.folder, m.uid, m.selectOnly === true); }),
      onNotice((msg) => app.notify(msg, "error")),
    ]);
    // A cold launch from a mailto: link queued it before the window existed.
    const t = window.setTimeout(() => { void this.drainMailto(); }, 600);
    return () => { clearTimeout(t); offs.forEach((f) => f()); };
  }
}

export const platform = new Platform();
