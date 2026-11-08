// Pure builders for the composer's ready-made blocks: callout boxes, bulletproof buttons, dividers, checklists.
// Email-safe markup only (inline styles + attribute fallbacks, no classes, no <style>, no external resources), and
// every colour pair is set explicitly (text AND background) so dark-mode mail apps cannot make it unreadable.
// No runtime imports: loadable by `node --experimental-strip-types --test`.

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

export type CalloutKind = "info" | "success" | "warning" | "note";
export const CALLOUTS: Record<CalloutKind, { label: string; bg: string; bar: string; fg: string }> = {
  info: { label: "Info", bg: "#E8F1FD", bar: "#2F6FDE", fg: "#1B2A41" },
  success: { label: "Success", bg: "#E6F6EC", bar: "#1E9E52", fg: "#14301F" },
  warning: { label: "Warning", bg: "#FFF4DB", bar: "#E0A100", fg: "#3D2E00" },
  note: { label: "Note", bg: "#F1F1F4", bar: "#6B7280", fg: "#26262B" },
};

/** A one-cell table with a coloured left border and a tinted background; the bold label is editable like any text. */
export function calloutHtml(kind: CalloutKind): string {
  const c = Object.hasOwn(CALLOUTS, kind) ? CALLOUTS[kind] : CALLOUTS.note;
  return `<table width="100%" cellpadding="0" cellspacing="0" border="0" style="border-collapse:collapse;width:100%"><tr><td bgcolor="${c.bg}" style="background-color:${c.bg};border-left:4px solid ${c.bar};padding:10px 14px;color:${c.fg}"><b>${c.label}:</b>&nbsp;</td></tr></table><div><br></div>`;
}

export const BUTTON_COLORS = [
  { id: "blue", label: "Blue", bg: "#2F6FDE", fg: "#FFFFFF" },
  { id: "green", label: "Green", bg: "#187A42", fg: "#FFFFFF" },
  { id: "red", label: "Red", bg: "#C0392B", fg: "#FFFFFF" },
  { id: "dark", label: "Dark", bg: "#26262B", fg: "#FFFFFF" },
] as const;
export type ButtonColor = (typeof BUTTON_COLORS)[number]["id"];
export const MAX_BUTTON_LABEL = 60;

export const MAX_URL_LENGTH = 2000;
const MAILTO_PARAMS = new Set(["subject", "body", "cc", "bcc"]);

/** A mailto: link: one plain address (or a few), and only the subject, body, cc and bcc parameters; no control characters or line breaks (raw or %-encoded) anywhere. */
function safeMailto(t: string): string | null {
  if (/[\u0000-\u001f\u007f-\u009f\u2028\u2029]/.test(t) || /%(0[0-9a-f]|1[0-9a-f]|7f|8[0-9a-f]|9[0-9a-f])/i.test(t)) return null;
  const m = /^mailto:([^?]*)(?:\?(.*))?$/i.exec(t);
  if (!m) return null;
  const addrs = decodeURIComponentSafe(m[1]);
  if (addrs === null || !addrs.split(",").every((a) => /^[^\s@,<>"]+@[^\s@,<>"]+$/.test(a.trim()))) return null;
  for (const pair of (m[2] ?? "").split("&")) {
    if (!pair) continue;
    const k = pair.split("=")[0].toLowerCase();
    if (!MAILTO_PARAMS.has(k)) return null;
    if (decodeURIComponentSafe(pair.slice(k.length + 1)) === null) return null;
  }
  return t;
}
function decodeURIComponentSafe(s: string): string | null {
  try { const d = decodeURIComponent(s); return /[\u0000-\u001f\u007f-\u009f\u2028\u2029]/.test(d) ? null : d; } catch { return null; }
}

/** https:, http: and mailto: only. A bare address becomes mailto:, a bare host https://. Everything else (javascript:, data:, file: ...) is refused; mailto: is limited (see safeMailto) and every URL is capped at MAX_URL_LENGTH. */
export function safeButtonUrl(input: string): string | null {
  const t = input.trim();
  if (!t || t.length > MAX_URL_LENGTH || /[\u0000-\u001f\s<>"]/.test(t)) return null;
  if (/^mailto:/i.test(t)) return safeMailto(t);
  if (/^https?:\/\/[^\s/?#]+/i.test(t)) return t;
  if (/^[a-z][a-z0-9+.-]*:/i.test(t) && !/^[^\s@/:]+:\d+([/?#]|$)/.test(t)) return null;
  if (/^[^\s@/]+@[^\s@/]+\.[^\s@/]+$/.test(t)) return safeMailto(`mailto:${t}`);
  return /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+(:\d+)?([/?#]\S*)?$/i.test(t) ? `https://${t}` : null;
}

/** The table-cell + <a> technique: the cell carries the colour and padding (Outlook ignores both on a link), the link carries the text. Null when label or URL is unusable. */
export function buttonHtml(label: string, url: string, color: ButtonColor = "blue"): string | null {
  const text = label.replace(/\s+/g, " ").trim().slice(0, MAX_BUTTON_LABEL);
  const href = safeButtonUrl(url);
  if (!text || !href) return null;
  const c = BUTTON_COLORS.find((x) => x.id === color) ?? BUTTON_COLORS[0];
  return `<table cellpadding="0" cellspacing="0" border="0" style="border-collapse:separate"><tr><td align="center" bgcolor="${c.bg}" style="background-color:${c.bg};border-radius:6px;padding:10px 22px"><a href="${esc(href)}" style="color:${c.fg};text-decoration:none;font-weight:bold;display:inline-block;font-family:Helvetica, Arial, sans-serif;font-size:15px;line-height:20px">${esc(text)}</a></td></tr></table><div><br></div>`;
}

export const dividerHtml = () => '<hr style="border:0;border-top:1px solid #C9CDD3;margin:12px 0"><div><br></div>';

export const CHECK_OFF = "☐", CHECK_ON = "☑";
/** `n` unticked lines (plain characters, not form inputs, so every client and the text part show them). */
export function checklistHtml(n = 3): string {
  return Array.from({ length: Math.min(Math.max(1, n), 20) }, () => `<div>${CHECK_OFF}&nbsp;</div>`).join("");
}
/** Flip a leading checkbox character; text without one is returned unchanged. */
export function toggleCheck(line: string): string {
  if (line.startsWith(CHECK_OFF)) return CHECK_ON + line.slice(1);
  if (line.startsWith(CHECK_ON)) return CHECK_OFF + line.slice(1);
  return line;
}
