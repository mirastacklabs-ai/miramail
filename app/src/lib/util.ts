import type { Address, MessageBody } from "./types";

export const addrLabel = (a: Address | null | undefined) => (a ? a.name?.trim() || a.address : "Unknown sender");
export const addrFull = (a: Address) => (a.name ? `${a.name} <${a.address}>` : a.address);

export function relDate(ts: number | null): string {
  if (ts == null) return "";
  const d = new Date(ts * 1000), n = new Date();
  const diff = (n.getTime() - d.getTime()) / 1000;
  if (diff < 60) return "now";
  if (d.toDateString() === n.toDateString()) return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const y = new Date(n); y.setDate(n.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return "Yesterday";
  if (diff < 6 * 86400) return d.toLocaleDateString([], { weekday: "short" });
  if (d.getFullYear() === n.getFullYear()) return d.toLocaleDateString([], { month: "short", day: "numeric" });
  return d.toLocaleDateString([], { year: "2-digit", month: "short", day: "numeric" });
}
/** Message-list date per the Preferences "date format" setting. */
export function listDate(ts: number | null, mode: "relative" | "absolute"): string {
  if (ts == null) return "";
  if (mode === "relative") return relDate(ts);
  const d = new Date(ts * 1000);
  const same = d.getFullYear() === new Date().getFullYear();
  return d.toLocaleString([], same ? { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" } : { year: "numeric", month: "short", day: "numeric" });
}
export const fullDate = (ts: number | null) => (ts == null ? "" : new Date(ts * 1000).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }));

export function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1048576).toFixed(1)} MB`;
}

/** Extract hrefs from sanitised HTML without rendering it. */
export function extractLinks(html: string | null): { url: string; label: string }[] {
  if (!html) return [];
  const doc = new DOMParser().parseFromString(html, "text/html");
  const seen = new Set<string>();
  const out: { url: string; label: string }[] = [];
  for (const a of Array.from(doc.querySelectorAll("a[href]"))) {
    const url = (a.getAttribute("href") ?? "").trim();
    if (!/^(https?:|mailto:)/i.test(url) || seen.has(url)) continue;
    seen.add(url);
    out.push({ url, label: (a.textContent ?? "").trim().replace(/\s+/g, " ").slice(0, 80) });
  }
  return out;
}

/** Wrap sanitised HTML in the CSP prefix mandated by docs/APP_API.md. */
export function buildSrcdoc(html: string, allowRemote: boolean, baseCss = ""): string {
  const csp = `default-src 'none'; img-src data:${allowRemote ? " https:" : ""}; style-src 'unsafe-inline'`;
  return `<meta http-equiv="Content-Security-Policy" content="${csp}"><style>html{background:#fff;color:#1c1c1c}${baseCss || "body{margin:0;padding:16px;font:14px/1.5 -apple-system,system-ui,sans-serif;overflow-wrap:anywhere}"}img{max-width:100%;height:auto}</style>${html}`;
}

export const reSubject = (s: string) => (/^\s*re\s*(?:\[\d+\]|\(\d+\))?\s*:/i.test(s) ? s.trim() : `Re: ${s.trim()}`.trim());

/** `aliases` = the account's saved sender identities: mail from them is mine too. */
export function replyDraft(b: MessageBody, all: boolean, me: string, aliases: readonly string[] = []) {
  const s = b.summary;
  const from = s.from;
  const key = (a: string) => a.trim().toLowerCase();
  const self = key(me);
  const mineSet = new Set([self, ...aliases.map(key)].filter(Boolean));
  // Replying to a message I sent (e.g. from Sent) goes to the people I wrote to, not back to me.
  const mine = !!from && mineSet.has(key(from.address));
  const to = new Map<string, string>();
  const cc = new Map<string, string>();
  const add = (m: Map<string, string>, a: string) => { const k = key(a); if (k && !mineSet.has(k) && !to.has(k) && !cc.has(k)) m.set(k, a); };
  // Reply-To (usable addresses only; garbage is ignored) is where a reply to someone else's message goes.
  const replyTo = (b.replyTo ?? []).filter((a) => validAddr(a.address));
  let notice: string | null = null;
  if (mine) {
    for (const a of s.to) add(to, a.address);
    if (!to.size) to.set(self, from!.address); // a note to myself: answer myself
  } else {
    for (const a of replyTo) add(to, a.address);
    if (!to.size && from) to.set(key(from.address), from.address); // no usable Reply-To (or it is only me): the sender
    // Same rule as the MIRA draft path (aichat drafts.rs): flag a Reply-To that is not the sender.
    if (replyTo.length && [...to.keys()].some((k) => !from || k !== key(from.address))) {
      notice = `Replies go to ${[...to.values()].join(", ")}, not the sender (${from ? from.address : "unknown"}).`;
    }
  }
  if (all) {
    for (const a of [...s.to, ...b.cc]) add(cc, a.address);
  }
  const who = from ? addrLabel(from) : "someone";
  const when = s.date != null ? new Date(s.date * 1000).toLocaleString([], { dateStyle: "long", timeStyle: "short" }) : "an earlier date";
  const orig = (b.text ?? "").replace(/\r\n/g, "\n").trimEnd();
  const quoted = orig.split("\n").map((l) => (l.startsWith(">") ? `>${l}` : `> ${l}`)).join("\n");
  return {
    to: [...to.values()], cc: [...cc.values()], bcc: [] as string[],
    subject: reSubject(s.subject),
    text: `\n\nOn ${when}, ${who} wrote:\n${quoted}\n`,
    inReplyTo: s.messageId ? { messageId: s.messageId, references: b.references } : null,
    /** Set when Reply-To sends the answer somewhere other than the sender (Compose shows it as a notice). */
    notice,
  };
}

export function forwardDraft(b: MessageBody) {
  const s = b.summary;
  const when = s.date != null ? new Date(s.date * 1000).toLocaleString([], { dateStyle: "long", timeStyle: "short" }) : "";
  const head = ["---------- Forwarded message ----------", `From: ${s.from ? addrFull(s.from) : "Unknown sender"}`, when && `Date: ${when}`, `Subject: ${s.subject}`, s.to.length ? `To: ${s.to.map(addrFull).join(", ")}` : ""].filter(Boolean).join("\n");
  return { to: [] as string[], cc: [] as string[], bcc: [] as string[], subject: /^\s*fwd?\s*(?:\[\d+\]|\(\d+\))?\s*:/i.test(s.subject) ? s.subject.trim() : `Fwd: ${s.subject.trim()}`.trim(), text: `\n\n${head}\n\n${(b.text ?? "").replace(/\r\n/g, "\n").trimEnd()}\n`, inReplyTo: null };
}

/** Split a recipients field on , ; or newline, but not inside a quoted display name ("Doe, John" <j@x.org>). */
export function splitAddrs(v: string): string[] {
  const out: string[] = [];
  let cur = "", q = false;
  for (const ch of v) {
    if (ch === '"') q = !q;
    if (!q && (ch === "," || ch === ";" || ch === "\n")) { out.push(cur); cur = ""; } else cur += ch;
  }
  out.push(cur);
  return out.map((x) => x.trim()).filter(Boolean);
}
export const validAddr = (v: string) => /^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/.test(v.replace(/^.*<(.+)>$/, "$1"));

/** What a click on a link inside a message should do. Only web and mailto links ever leave the sandboxed frame; "#anchor" stays in the message; everything else is ignored. */
export type LinkAction = { kind: "web"; url: string } | { kind: "mail"; to: string[]; subject: string } | { kind: "anchor" } | { kind: "ignore" };
export function linkAction(href: string | null | undefined): LinkAction {
  const h = (href ?? "").trim();
  if (!h) return { kind: "ignore" };
  if (h.startsWith("#")) return { kind: "anchor" };
  if (h.length > 8192 || /[\u0000-\u001f\u007f\s]/.test(h)) return { kind: "ignore" };
  let u: URL;
  try { u = new URL(h); } catch { return { kind: "ignore" }; }
  if (u.protocol === "http:" || u.protocol === "https:") return u.hostname && !u.username ? { kind: "web", url: u.href } : { kind: "ignore" };
  if (u.protocol === "mailto:") {
    let to: string[] = [];
    try { to = splitAddrs(decodeURIComponent(u.pathname)).filter(validAddr); } catch { return { kind: "ignore" }; }
    if (!to.length) return { kind: "ignore" };
    const subject = (u.searchParams.get("subject") ?? "").replace(/[\u0000-\u001f\u007f]/g, " ").slice(0, 300);
    return { kind: "mail", to, subject };
  }
  return { kind: "ignore" };
}
