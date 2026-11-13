import { openExternal } from "./api";
import { app } from "./state.svelte";
import { workspace } from "./workspace.svelte";
import { linkAction } from "./util";
import { phone } from "./phone.svelte";

let asking = false;
/** A mail://link this soon after opening a message may come from the frame that was just replaced. */
export const STALE_LINK_MS = 300;

/** "Open link?" confirm showing the real address, then the system browser. A second request while the dialog is open is ignored (no stacking). */
export async function openLink(url: string): Promise<void> {
  if (phone.on) { app.notify("Opening links isn't available on iPhone yet.", "error"); return; }
  if (asking || app.confirmReq) return; // never replace a dialog that is already showing
  asking = true;
  try {
    const ok = await app.ask({ title: "Open link?", body: "This will open the address below in your default browser.", detail: url, ok: "Open" });
    if (!ok) return;
    await openExternal(url);
  } catch (e) {
    app.fail(e);
  } finally {
    asking = false;
  }
}

/** A link the backend cancelled in the message frame (see linknav.rs): re-validate, then confirm / compose. */
export function handleMailLink(url: string): void {
  // Defence in depth: the frame's links have no href until the shown message's checks finished (lib/linkgate.ts). The
  // event cannot say which frame sent it, so one arriving just after an open (from the previous frame) is ignored too.
  if (!app.readyKey || performance.now() - app.openT0 < STALE_LINK_MS) return;
  const act = linkAction(url);
  if (act.kind === "web") void openLink(act.url);
  else if (act.kind === "mail") workspace.openCompose({ to: act.to, cc: [], bcc: [], subject: act.subject, text: "", inReplyTo: null });
}
