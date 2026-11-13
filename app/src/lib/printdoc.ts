// The printout the iPhone sends to the system print sheet (UIPrintInteractionController): a header block like the Mac's
// PrintSheet plus the same sanitised body the Reader shows. Links are inert (held, as in the Reader), the document
// carries the same CSP, and remote references are stripped unless the user already allowed remote images for it.
import { addrFull, buildSrcdoc, fullDate } from "./util.ts";
import { holdHtml } from "./linkgate.ts";
import type { MessageBody } from "./types";

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

const PRINT_CSS =
  "body{margin:0;padding:0;font:13px/1.45 -apple-system,system-ui,sans-serif;overflow-wrap:anywhere}" +
  "h1.mm{font-size:18px;margin:0 0 8px}table.mm{border-collapse:collapse;font-size:12px}table.mm th{text-align:left;color:#555;padding:1px 12px 1px 0;vertical-align:top;font-weight:600}" +
  "table.mm td{padding:1px 0;overflow-wrap:anywhere}hr.mm{border:0;border-top:1px solid #bbb;margin:10px 0}pre.mm{white-space:pre-wrap;font:12px/1.45 ui-monospace,Menlo,monospace;margin:0}";

/** Drop anything that would fetch a remote resource (defence in depth next to the CSP): src / srcset / background /
 *  poster values other than data: and cid:, CSS url(http...) and @import. */
export function stripRemote(html: string): string {
  return html
    .replace(/(\s)(src|srcset|background|poster)\s*=\s*("[^"]*"|'[^']*'|[^\s>]*)/gi, (m, ws: string, _n: string, v: string) => {
      const val = v.replace(/^["']|["']$/g, "").trim().toLowerCase();
      return val.startsWith("data:") || val.startsWith("cid:") ? m : ws;
    })
    .replace(/url\(\s*(['"]?)\s*(?:https?:)?\/\/[^)]*\)/gi, "url()")
    .replace(/@import[^;]*;?/gi, "");
}

function header(b: MessageBody): string {
  const s = b.summary;
  const row = (k: string, v: string) => `<tr><th>${k}</th><td>${esc(v)}</td></tr>`;
  return (
    `<h1 class="mm">${esc(s.subject || "(no subject)")}</h1><table class="mm"><tbody>` +
    row("From", s.from ? addrFull(s.from) : "Unknown sender") +
    row("To", s.to.map(addrFull).join(", ") || "-") +
    (b.cc.length ? row("Cc", b.cc.map(addrFull).join(", ")) : "") +
    row("Date", fullDate(s.date)) +
    (b.attachments.length ? row("Attached", b.attachments.map((a) => a.name).join(", ")) : "") +
    `</tbody></table><hr class="mm">`
  );
}

/** The document to print. Printing never loads remote content, whatever the Reader allows (a tracker pixel would reveal
 *  that the message was printed); the Swift side enforces the same with its own CSP and a block rule. */
export function buildPrintHtml(b: MessageBody, plain = false): string {
  const allowRemote = false;
  const useHtml = !!b.html && !plain;
  const body = useHtml ? (b.html as string) : `<pre class="mm">${esc(b.text ?? "(This message has no readable text.)")}</pre>`;
  const safe = useHtml && !allowRemote ? stripRemote(body) : body;
  return buildSrcdoc(safe, allowRemote, PRINT_CSS, header(b), useHtml ? (h) => holdHtml(h).html : undefined);
}
