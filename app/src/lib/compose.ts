import { app } from "./state.svelte";
import { api } from "./api";
import { forwardDraft, replyDraft, addrFull, addrLabel } from "./util";
import { workspace, type ComposePayload } from "./workspace.svelte";
import { defaultMode } from "./composer.svelte";
import { forwardQuoteHtml, replyQuoteHtml } from "./composetext";
import type { MessageBody } from "./types";

export function replyFresh() {
  workspace.newCompose();
}

const longDate = (ts: number | null) => (ts != null ? new Date(ts * 1000).toLocaleString([], { dateStyle: "long", timeStyle: "short" }) : "");

/** Original message as sanitised HTML with remote images blocked (so the editor never loads anything), or null. */
async function quotedHtml(acc: string, b: MessageBody): Promise<string | null> {
  if (!b.html) return null;
  try {
    const fresh = await api.getMessage(acc, b.summary.folder, b.summary.uid, false, true, false);
    // Same policy the send path applies, BEFORE the editor sees it: what the user sees in the quote is what is sent
    // (table presentation attributes are limited to the composer's allow-list, so the editor shows what is sent).
    return fresh.html ? await api.sanitizeCompose(fresh.html) : null;
  } catch { return null; }
}

async function bodyOf(acc: string): Promise<MessageBody | null> {
  let b = app.body;
  if (!b || b.summary.uid !== app.selectedUid) {
    if (app.selectedUid == null) return null;
    try { b = await api.getMessage(acc, app.folder, app.selectedUid, false); } catch (e) { app.fail(e); return null; }
  }
  return b;
}

export async function replyTo(all: boolean, forward = false) {
  const acc = app.account;
  if (!acc || app.selectedUid == null) return;
  const b = await bodyOf(acc);
  if (!b) return;
  if (forward) return forwardMessage("inline", b, acc);
  const draft = replyDraft(b, all, acc);
  const mode = defaultMode();
  const extra: Partial<ComposePayload> = { kind: "reply", mode };
  if (mode === "rich") {
    const s = b.summary;
    const when = longDate(s.date) || "an earlier date";
    const who = s.from ? addrLabel(s.from) : "someone";
    extra.seedHtml = replyQuoteHtml(`On ${when}, ${who} wrote:`, await quotedHtml(acc, b), b.text);
  }
  workspace.openCompose(draft, acc, { extra });
}

/** Forward the open message: quoted inline (re-attaching its files) or as a message/rfc822 attachment. */
export async function forwardMessage(as: "inline" | "attach", given?: MessageBody, account?: string) {
  const acc = account ?? app.account;
  if (!acc || app.selectedUid == null) return;
  const b = given ?? (await bodyOf(acc));
  if (!b) return;
  const s = b.summary;
  const mode = defaultMode();
  const fwd = { account: acc, folder: s.folder, uid: s.uid, ...(s.uidvalidity != null ? { uidvalidity: s.uidvalidity } : {}), as, atts: as === "inline" ? b.attachments.map(({ id, name, mime, size }) => ({ id, name, mime, size })) : [] };
  const base = forwardDraft(b);
  const draft = as === "inline" ? base : { ...base, text: "" };
  const extra: Partial<ComposePayload> = { kind: "reply", mode, fwd };
  if (mode === "rich") {
    extra.seedHtml = as === "inline"
      ? forwardQuoteHtml([["From", s.from ? addrFull(s.from) : "Unknown sender"], ["Date", longDate(s.date)], ["Subject", s.subject], ["To", s.to.map(addrFull).join(", ")], ["Cc", b.cc.map(addrFull).join(", ")]], await quotedHtml(acc, b), b.text)
      : "";
  }
  workspace.openCompose(draft, acc, { extra });
}
