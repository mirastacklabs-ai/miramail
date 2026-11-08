// Pure restore planning for signatures and identities (no IO, no Svelte runtime: node-testable).
// Restoring must never silently overwrite something that is already on this Mac:
//  * a signature whose id exists here is skipped when identical, otherwise saved under a fresh id;
//  * an identity whose id exists here is skipped (never overwritten);
//  * identities come back with an EMPTY reply-to unless the user ticked "restore reply-to addresses".
import type { Identity, SigDefaults, Signature } from "./types";

const sameSig = (a: Signature, b: Signature) =>
  a.name === b.name && a.rich === b.rich && a.text === b.text && a.html === b.html && a.placement === b.placement && JSON.stringify(a.accounts) === JSON.stringify(b.accounts);

export type SigAction = { sig: Signature; kind: "skip" | "add" | "add-fresh"; origId: string };

/** What to do with each incoming signature. `skip` = identical one already here (still maps to its id). */
export function planSignatures(incoming: Signature[], existing: Signature[]): SigAction[] {
  return incoming.map((sig) => {
    const have = existing.find((e) => e.id === sig.id);
    if (!have) return { sig, kind: "add" as const, origId: sig.id };
    return { sig: { ...sig, id: "" }, kind: sameSig(have, sig) ? ("skip" as const) : ("add-fresh" as const), origId: sig.id };
  });
}

/** Identities to save: unknown ids only, signature ids remapped, reply-to cleared unless allowed. */
export function planIdentities(incoming: Identity[], existingIds: string[], accounts: string[], sigMap: Map<string, string>, keepReplyTo: boolean): Identity[] {
  return incoming
    .filter((i) => accounts.includes(i.account) && !existingIds.includes(i.id))
    .map((i) => ({ ...i, replyTo: keepReplyTo ? i.replyTo : "", signatureId: i.signatureId ? (sigMap.get(i.signatureId) ?? null) : null }));
}

/** Default-signature choices with ids remapped; a reference to a signature that is not here becomes null. */
export function planDefaults(incoming: SigDefaults[], accounts: string[], sigMap: Map<string, string>): SigDefaults[] {
  const m = (id: string | null) => (id ? (sigMap.get(id) ?? null) : null);
  return incoming.filter((d) => accounts.includes(d.account)).map((d) => ({ ...d, newId: m(d.newId), replyId: m(d.replyId) }));
}

/** Lines shown in the restore preview so the user sees which sender/reply-to addresses a backup carries. */
export function describeIdentities(items: unknown): { lines: string[]; hasReplyTo: boolean } {
  const lines: string[] = [];
  let hasReplyTo = false;
  if (!Array.isArray(items)) return { lines, hasReplyTo };
  for (const i of items.slice(0, 50)) {
    if (!i || typeof i !== "object") continue;
    const { name, address, replyTo } = i as Record<string, unknown>;
    const rt = typeof replyTo === "string" && replyTo.trim() ? replyTo.trim() : "";
    if (rt) hasReplyTo = true;
    lines.push(`${typeof name === "string" && name ? name + " " : ""}<${String(address ?? "")}>${rt ? ` · replies go to ${rt}` : ""}`);
  }
  return { lines, hasReplyTo };
}
