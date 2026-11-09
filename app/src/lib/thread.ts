// Pure helpers for the conversation view of the message list: grouping, collapse / expand, keyboard navigation,
// sender addresses. No DOM, no stores (tested in thread.test.ts). The thread KEYS come from the backend
// (`thread_map`, mailcore::threading); this file only arranges rows.

export interface ThreadMsg { uid: number; date: number | null; flags: { seen: boolean } }

export interface ListRow<T extends ThreadMsg = ThreadMsg> {
  /** The message this row shows (for a collapsed conversation: its newest message). */
  m: T;
  /** 0 = top level, 1 = a message under an expanded conversation header. */
  depth: 0 | 1;
  /** First row of a conversation with two or more messages in this list. */
  head: boolean;
  /** Messages in the conversation (1 for plain rows). */
  count: number;
  /** Unread messages in the conversation. */
  unread: number;
  /** Conversation key (head and children only). */
  key?: string;
  expanded: boolean;
  /** Every uid of the conversation in list order, header first (just [uid] for plain rows). */
  uids: number[];
  pinned: boolean;
}

const plain = <T extends ThreadMsg>(m: T, pinned: boolean): ListRow<T> => ({ m, depth: 0, head: false, count: 1, unread: m.flags.seen ? 0 : 1, expanded: false, uids: [m.uid], pinned });

/**
 * Arrange `msgs` (already newest first) for display. Pinned messages come first, each as its own row.
 * With `threaded`, messages sharing a key (and still two or more once pinned ones are taken out) form a
 * conversation at the position of its newest message; it shows children when its key is in `expanded` or when
 * the selected message is one of its children (so the selection is never hidden).
 */
export function buildRows<T extends ThreadMsg>(
  msgs: readonly T[],
  keyOf: (uid: number) => string | undefined,
  o: { threaded: boolean; expanded: ReadonlySet<string>; pinned: ReadonlySet<number>; selectedUid: number | null },
): ListRow<T>[] {
  const out: ListRow<T>[] = [];
  const rest: T[] = [];
  for (const m of msgs) {
    if (o.pinned.has(m.uid)) out.push(plain(m, true)); else rest.push(m);
  }
  if (!o.threaded) {
    for (const m of rest) out.push(plain(m, false));
    return out;
  }
  const groups = new Map<string, T[]>();
  for (const m of rest) {
    const k = keyOf(m.uid);
    if (k !== undefined) { const g = groups.get(k); if (g) g.push(m); else groups.set(k, [m]); }
  }
  const done = new Set<string>();
  for (const m of rest) {
    const k = keyOf(m.uid);
    const g = k === undefined ? undefined : groups.get(k);
    if (k === undefined || !g || g.length < 2) { out.push(plain(m, false)); continue; }
    if (done.has(k)) continue;
    done.add(k);
    const uids = g.map((x) => x.uid);
    const head = g[0];
    const expanded = o.expanded.has(k) || (o.selectedUid != null && o.selectedUid !== head.uid && uids.includes(o.selectedUid));
    out.push({ m: head, depth: 0, head: true, count: g.length, unread: g.reduce((n, x) => n + (x.flags.seen ? 0 : 1), 0), key: k, expanded, uids, pinned: false });
    if (expanded) for (const c of g.slice(1)) out.push({ m: c, depth: 1, head: false, count: g.length, unread: c.flags.seen ? 0 : 1, key: k, expanded: false, uids: [c.uid], pinned: false });
  }
  return out;
}

/** All uids in display order including those hidden under collapsed conversations (for range selection). */
export function orderUids(rows: readonly ListRow[]): number[] {
  return rows.flatMap((r) => (r.head && !r.expanded ? r.uids : [r.m.uid]));
}

/** The uids an action on `uid` covers: the whole conversation when `uid` heads a collapsed one, else just `uid`. */
export function threadUids(rows: readonly ListRow[], uid: number): number[] {
  const r = rows.find((x) => x.m.uid === uid);
  return r && r.head && !r.expanded ? r.uids : [uid];
}

/** Uid of the row `delta` steps from the selected one among the visible rows (clamped); null when nothing is listed. */
export function navTarget(rows: readonly ListRow[], selectedUid: number | null, delta: 1 | -1): number | null {
  if (!rows.length) return null;
  const i = rows.findIndex((r) => r.m.uid === selectedUid);
  const n = Math.min(rows.length - 1, Math.max(0, i < 0 ? 0 : i + delta));
  return rows[n].m.uid;
}

export type KeyAct =
  | { act: "none" }
  | { act: "expand"; key: string }
  | { act: "collapse"; key: string; select?: number }
  | { act: "select"; uid: number };

/** Left / Right on the selected row: collapse, expand or hop between a header and its first child. */
export function keyNav(rows: readonly ListRow[], selectedUid: number | null, dir: "left" | "right"): KeyAct {
  const i = rows.findIndex((r) => r.m.uid === selectedUid);
  if (i < 0) return { act: "none" };
  const r = rows[i];
  if (r.head && r.key) {
    if (dir === "right") return r.expanded ? (rows[i + 1]?.depth === 1 ? { act: "select", uid: rows[i + 1].m.uid } : { act: "none" }) : { act: "expand", key: r.key };
    return r.expanded ? { act: "collapse", key: r.key } : { act: "none" };
  }
  if (r.depth === 1 && r.key && dir === "left") {
    const h = rows.slice(0, i).reverse().find((x) => x.head && x.key === r.key);
    return h ? { act: "collapse", key: r.key, select: h.m.uid } : { act: "none" };
  }
  return { act: "none" };
}

/** Toggle a key in an immutable set. */
export function toggled(set: ReadonlySet<string>, key: string, on?: boolean): Set<string> {
  const n = new Set(set);
  const want = on ?? !n.has(key);
  if (want) n.add(key); else n.delete(key);
  return n;
}

/** Lower-case address of a From value (an Address object or a bare string); "" when unknown. */
export function senderAddress(from: { address?: string | null } | string | null | undefined): string {
  const a = typeof from === "string" ? from : from?.address ?? "";
  return a.trim().toLowerCase();
}

/** Messages of the thread strictly before the selected one (oldest first), for the Reader's collapsed list. */
export function earlierMembers<T extends { uid: number; folder: string; date: number | null }>(members: readonly T[], folder: string, uid: number): T[] {
  const sorted = [...members].sort((a, b) => (a.date ?? 0) - (b.date ?? 0) || a.uid - b.uid);
  const i = sorted.findIndex((m) => m.uid === uid && m.folder === folder);
  return i < 0 ? [] : sorted.slice(0, i);
}

/** Subject without reply / forward prefixes, lower-cased, whitespace collapsed ("Re: Fwd:  Hello" -> "hello"). */
export function normSubject(subject: string | null | undefined): string {
  let t = (subject ?? "").trim();
  for (let i = 0; i < 8; i++) {
    const n = t.replace(/^(re|fwd?|aw|sv|wg)\s*(?:\[\d+\]|\(\d+\))?\s*:\s*/i, "");
    if (n === t) break;
    t = n;
  }
  return t.replace(/\s+/g, " ").trim().toLowerCase();
}

export interface ConfirmMember { subject: string | null; from?: { name?: string | null; address?: string | null } | string | null }

/**
 * Text for the confirmation before a bulk action on a collapsed conversation. Always the member count; when the
 * members' normalised subjects differ (the grouping may have joined unrelated mail) also each sender and subject,
 * so the user sees exactly what the action will touch.
 */
export function threadConfirm(members: readonly ConfirmMember[], verb: string): { title: string; body: string; mixed: boolean } {
  const n = members.length;
  const mixed = new Set(members.map((m) => normSubject(m.subject))).size > 1;
  const title = `${verb} ${n} messages?`;
  let body = `This conversation has ${n} messages and all of them will be moved (${verb.toLowerCase()}).`;
  if (mixed) {
    const who = (f: ConfirmMember["from"]) => (typeof f === "string" ? f : f?.name || f?.address || "unknown sender");
    const lines = members.slice(0, 8).map((m) => `${who(m.from)}: ${(m.subject || "(no subject)").slice(0, 80)}`);
    body += ` Their subjects differ:\n${lines.join("\n")}${n > 8 ? `\nand ${n - 8} more` : ""}`;
  }
  return { title, body, mixed };
}
