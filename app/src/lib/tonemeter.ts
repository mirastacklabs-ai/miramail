// Local tone meter for Compose (owner-approved 2026-11-07). Pure and offline: nothing here sends anything anywhere.
// English only. It looks at the user's OWN words (never the quoted original or the signature), counts a few
// well-known wording signals and says why. It never claims certainty: the UI always shows DISCLAIMER.

export const DISCLAIMER = "Quick check on your wording, not a judgement";
/** Only this many characters of the draft are looked at (typing must never lag). */
export const MAX_CHARS = 20_000;
export const LONG_SENTENCE_WORDS = 35;
export const LONG_MESSAGE_WORDS = 300;

export type ToneLevel = "warm" | "neutral" | "firm" | "tense";
export type SignalKind = "good" | "note" | "warn";
export interface ToneSignal { id: string; kind: SignalKind; label: string; why: string; examples: string[] }
export interface ToneMeter { level: ToneLevel; label: string; icon: string; signals: ToneSignal[]; words: number; truncated: boolean; empty: boolean }

export const LEVELS: Record<ToneLevel, { label: string; icon: string }> = {
  warm: { label: "Calm and warm", icon: "☺" },
  neutral: { label: "Neutral", icon: "–" },
  firm: { label: "Firm", icon: "■" },
  tense: { label: "Tense", icon: "!" },
};

// ---- the data tables (extend here; everything below is generic) ----

interface PhraseRule { id: string; label: string; why: string; weight: number; phrases: string[] }
/** Sharp or passive-aggressive phrases. `phrases` are matched case-insensitively on word boundaries; `*` = any one word. */
export const SHARP_PHRASES: PhraseRule[] = [
  { id: "pa", label: "Passive-aggressive phrase", why: "Phrases like this often read as a rebuke, even when meant neutrally.", weight: 2, phrases: [
    "as per my last email", "per my last email", "per my previous", "as per my previous", "as i already said", "as i already mentioned", "as i already stated", "as i said", "as i mentioned before",
    "as previously stated", "once again", "once more", "as you should know", "as you know", "i shouldn't have to", "i should not have to", "obviously", "clearly you", "clearly, you",
    "with all due respect", "i am disappointed", "i'm disappointed", "i was disappointed", "whatever", "no offense", "no offence", "do you even", "how many times",
  ] },
  { id: "harsh", label: "Harsh wording", why: "Strong words like these are likely to feel like an attack.", weight: 3, phrases: [
    "unacceptable", "ridiculous", "pathetic", "incompetent", "useless", "stupid", "idiot*", "your fault", "for the last time", "waste of my time", "waste of time", "fed up", "sick of", "sick and tired",
    "how dare you", "shut up", "not my problem", "you failed", "you have failed", "you messed up", "you screwed up", "lazy", "disgraceful", "outrageous",
  ] },
];

/** Phrases that signal care. Each distinct one counts once (max 4). */
export const POLITE_PHRASES: string[] = [
  "please", "thanks", "thank you", "thankful", "appreciate", "appreciated", "happy to", "glad to", "would you mind", "could you", "would you be able", "if you don't mind", "if possible",
  "sorry", "apologies", "apologize", "apologise", "hope you", "hope this helps", "looking forward", "great to hear", "good to hear", "no worries", "welcome",
];

/** Softeners. Many of them in a short text can read as unsure. */
export const HEDGES: string[] = [
  "maybe", "perhaps", "possibly", "probably", "i think", "i guess", "i suppose", "sort of", "kind of", "a bit", "a little", "somewhat", "might", "i was wondering", "not sure if", "just", "i feel like", "i'm sorry to bother",
];

export const GREETINGS = /^(hi|hello|hey|dear|good\s+(morning|afternoon|evening|day)|greetings|hiya|howdy|hi there|hello there|yo|namaste|hola|bonjour)\b/i;
/** First words that make a message start like an order. */
export const IMPERATIVES: string[] = [
  "send", "give", "fix", "do", "tell", "stop", "answer", "reply", "call", "explain", "change", "make", "get", "let", "don't", "do not", "just", "listen", "look", "read", "check", "forward", "provide", "confirm", "respond", "update", "remove", "delete", "move", "bring", "show", "come",
];
export const SIGNOFFS = /\b(thanks|thank you|regards|best|cheers|sincerely|cordially|warmly|yours|take care|talk soon|many thanks|appreciate|speak soon|have a (good|great|nice))\b/i;
/** Capital words that are normal (acronyms, units, currencies, file types). Never counted as shouting. */
export const CAPS_OK = new Set((
  "PDF URL API USA USD EUR GBP INR AUD CAD EU UK UAE CEO CFO CTO COO FAQ IBAN VAT GST PO ID RE FYI EOD ETA IMAP SMTP HTTP HTTPS DNS SQL HTML CSS NDA KYC SLA SSL TLS VPN USB ISP SEO CRM ERP QA PR CV DOB TBD TBA PTO LLC INC LTD " +
  "JPG PNG GIF ZIP DOC DOCX XLS XLSX CSV JSON XML PPT PPTX TXT RSVP OK AM PM CET UTC GMT IST PST EST PDT EDT NASA UN WHO NATO ASAP"
).split(" "));
/** Capital words that ARE shouting even once. */
export const SHOUT_WORDS = new Set("NOW URGENT NEVER ALWAYS STOP MUST IMMEDIATELY UNACCEPTABLE WHY WHAT HOW WHERE LISTEN READ NO DON'T DO NOT YOU YOUR ENOUGH WRONG".split(" "));

// ---- compile once ----

const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
function phraseRe(list: string[]): RegExp {
  const alts = list.map((p) => {
    const star = p.endsWith("*");
    const body = esc(star ? p.slice(0, -1) : p).replace(/ /g, "\\s+");
    return star ? `\\b${body}\\w*` : `\\b${body}\\b`;
  });
  return new RegExp(alts.join("|"), "gi");
}
const SHARP_RE = SHARP_PHRASES.map((r) => ({ rule: r, re: phraseRe(r.phrases) }));
const POLITE_RE = POLITE_PHRASES.map((p) => ({ p, re: new RegExp(`\\b${esc(p).replace(/ /g, "\\s+")}\\b`, "i") }));
const HEDGE_RE = phraseRe(HEDGES);
const IMPERATIVE_RE = new RegExp(`^(${IMPERATIVES.map((p) => esc(p).replace(/ /g, "\\s+")).join("|")})\\b`, "i");
const ABSOLUTE_RE = /\byou(?:'re|\s+are|\s+have|'ve)?\s+(?:always|never)\b|\byou\s+(?:don't|do\s+not|didn't|did\s+not)\s+ever\b/gi;

// ---- splitting the draft: what the user wrote vs. the quote / signature ----

const ATTRIBUTION = /(^|\n)(On [^\n]{1,200} wrote:[ \t]*(?:\n|$)|-{3,}\s*Original Message\s*-{3,}[ \t]*(?:\n|$)|-{10} Forwarded message -{10}[ \t]*(?:\n|$)|Begin forwarded message:[ \t]*(?:\n|$))/i;
const SIG_LINE = /(^|\n)-- ?(?:\n|$)/;
const SENT_FROM = /(^|\n)(Sent from my [^\n]{1,40}|Get Outlook for [^\n]{1,20})[ \t]*(?:\n|$)/i;

/** Where the user's own words end: the quote, a forwarded banner, the `-- ` signature delimiter. -1 = nothing to cut. */
export function cutIndex(text: string): number {
  let cut = -1;
  for (const re of [ATTRIBUTION, SIG_LINE, SENT_FROM]) {
    const m = re.exec(text);
    if (m) { const i = m.index + m[1].length; if (cut < 0 || i < cut) cut = i; }
  }
  return cut;
}
/** `body` = the user's own text, `tail` = quote and signature from the cut on (kept as typed, for Apply). */
export function splitDraft(text: string): { body: string; tail: string } {
  const t = text.replace(/\r\n?/g, "\n");
  const c = cutIndex(t);
  return c < 0 ? { body: t, tail: "" } : { body: t.slice(0, c), tail: t.slice(c) };
}
/** Own words only: cut at the quote / signature and drop stray `>` lines. */
export function ownText(text: string): string {
  return splitDraft(text).body.split("\n").filter((l) => !/^\s*>/.test(l)).join("\n").trim();
}

// ---- analysis ----

const sig = (id: string, kind: SignalKind, label: string, why: string, examples: string[] = []): ToneSignal => ({ id, kind, label, why, examples: examples.slice(0, 3) });
const plural = (n: number, one: string, many = one + "s") => `${n} ${n === 1 ? one : many}`;

function matches(re: RegExp, s: string, skipNames = false): string[] {
  re.lastIndex = 0;
  const out: string[] = [];
  let m: RegExpExecArray | null;
  while ((m = re.exec(s)) && out.length < 50) {
    if (m[0] === "") { re.lastIndex++; continue; }
    if (!(skipNames && isNameLike(s, m.index, m[0]))) out.push(m[0].replace(/\s+/g, " "));
  }
  return out;
}
/** A Capitalised match in the middle of a sentence is probably a name ("Whatever Inc", "Mr Lazy"): not a phrase. */
function isNameLike(s: string, i: number, hit: string): boolean {
  if (!/^[A-Z][a-z]/.test(hit)) return false;
  const before = s.slice(Math.max(0, i - 3), i).replace(/[ \t]+$/, "");
  return before !== "" && !/[.!?:\n"“(\-–—]$/.test(before) && !/\n$/.test(s.slice(Math.max(0, i - 3), i));
}
const words = (s: string) => s.match(/[\p{L}\p{N}'’-]+/gu) ?? [];

export function analyseTone(draft: string): ToneMeter {
  const truncated = draft.length > MAX_CHARS;
  // Cut BEFORE the split so a huge quote never costs time; the limit is on the raw draft.
  const own = ownText(truncated ? draft.slice(0, MAX_CHARS) : draft).replace(/[’‘]/g, "'");
  const w = words(own);
  const n = w.length;
  const base = { truncated, words: n };
  if (n < 3) return { level: "neutral", ...LEVELS.neutral, signals: [], empty: true, ...base };

  const signals: ToneSignal[] = [];
  let neg = 0;

  // sharp phrases
  for (const { rule, re } of SHARP_RE) {
    const hits = matches(re, own, true);
    if (!hits.length) continue;
    const uniq = [...new Set(hits.map((h) => h.toLowerCase()))];
    neg += rule.weight * Math.min(uniq.length, 2);
    signals.push(sig(rule.id, "warn", `${rule.label}: “${uniq[0]}”${uniq.length > 1 ? ` (+${uniq.length - 1})` : ""}`, rule.why, uniq));
  }

  // shouting
  const shouts = own.match(/[A-Z][A-Z']{2,}/g)?.filter((x) => !CAPS_OK.has(x) && !CAPS_OK.has(x.replace(/'S$/, "")) && !/\d/.test(x)) ?? [];
  const shoutLines = own.split("\n").filter((l) => { const lw = words(l); return lw.length >= 3 && l === l.toUpperCase() && /[A-Z]/.test(l) && lw.filter((x) => !CAPS_OK.has(x)).length >= 3; }).length;
  const strongShout = shouts.some((x) => SHOUT_WORDS.has(x));
  if (shouts.length >= 2 || strongShout || shoutLines) {
    neg += shouts.length >= 3 || shoutLines ? 2.5 : 1.5;
    signals.push(sig("caps", "warn", `ALL-CAPS: ${[...new Set(shouts)].slice(0, 3).join(", ") || "whole line"}`, "Capital letters read as shouting. Names and acronyms are ignored.", [...new Set(shouts)]));
  }

  // !! and ?!
  const punct = own.match(/[!?]{2,}/g) ?? [];
  if (punct.length) { neg += punct.some((p) => p.length >= 3 || /!.*\?|\?.*!/.test(p)) ? 2 : 1.5; signals.push(sig("punct", "warn", `Repeated punctuation: ${punct[0]}`, "Strings like !! or ?! can read as irritation.", punct)); }

  // absolutes aimed at the reader
  const abs = matches(ABSOLUTE_RE, own);
  if (abs.length) { neg += 2; signals.push(sig("absolute", "warn", `Absolute about the reader: “${abs[0].toLowerCase()}”`, "“You always / you never” tends to feel like blame; a specific example lands better.", abs)); }

  // opener
  const firstLine = own.split("\n").find((l) => l.trim())?.trim() ?? "";
  const greeted = GREETINGS.test(firstLine);
  if (!greeted && n >= 12) {
    if (IMPERATIVE_RE.test(firstLine)) { neg += 1.2; signals.push(sig("blunt", "note", "Blunt opener", "The message starts with an order and no greeting; a short “Hi …” softens it.", [firstLine.split(/\s+/).slice(0, 4).join(" ")])); }
    else { neg += 0.4; signals.push(sig("nogreet", "note", "No greeting", "A short greeting makes a message feel less abrupt.")); }
  } else if (!greeted && n >= 3 && IMPERATIVE_RE.test(firstLine) && !/^please\b/i.test(firstLine) && n <= 11) {
    neg += 0.8; signals.push(sig("blunt", "note", "Blunt opener", "Very short and starts with an order; “please” or a greeting helps.", [firstLine]));
  }

  // long sentences
  const sentences = own.split(/(?<=[.!?])\s+|\n+/);
  const longS = sentences.filter((s) => words(s).length > LONG_SENTENCE_WORDS);
  if (longS.length) { neg += Math.min(0.5 * longS.length, 1.5); signals.push(sig("longsent", "note", `${plural(longS.length, "very long sentence")} (over ${LONG_SENTENCE_WORDS} words)`, "Long sentences are harder to follow and can bury the point.", [longS[0].slice(0, 60) + "…"])); }

  // long message
  if (n > LONG_MESSAGE_WORDS) { neg += 0.3; signals.push(sig("longmsg", "note", `Long message (${n} words)`, "Readers skim long emails; the key ask may be missed. Consider leading with it.")); }

  // sign-off
  const lastLines = own.split("\n").filter((l) => l.trim()).slice(-3).join(" ");
  if (n >= 25 && !SIGNOFFS.test(lastLines)) { neg += 0.4; signals.push(sig("signoff", "note", "No sign-off or thanks", "A closing line such as “Thanks” or “Best regards” leaves a warmer last impression.")); }

  // hedging
  const hedges = matches(HEDGE_RE, own);
  if (hedges.length >= 4 && (hedges.length * 100) / n >= 4) signals.push(sig("hedge", "note", `Lots of softeners (${hedges.length})`, "Many “maybe / I think / just” can make the message sound unsure of itself.", [...new Set(hedges.map((h) => h.toLowerCase()))]));

  // politeness
  const polite = POLITE_RE.filter((x) => x.re.test(own)).map((x) => x.p);
  if (polite.length) signals.push(sig("polite", "good", `Polite wording: ${polite.slice(0, 3).join(", ")}`, "Courtesy words help the message land well.", polite));
  if (greeted) signals.push(sig("greeting", "good", "Starts with a greeting", "A greeting sets a friendly tone."));

  const politeScore = Math.min(polite.length, 4) + (greeted ? 0.5 : 0);
  const net = neg - 0.5 * Math.min(politeScore, 3);
  const hardHit = signals.some((s) => s.id === "harsh");
  let level: ToneLevel;
  if (net >= 3 || (hardHit && net >= 2)) level = "tense";
  else if (net >= 1.2) level = "firm";
  else if (polite.length >= 2 && neg < 1.2) level = "warm";
  else level = "neutral";

  const order: Record<SignalKind, number> = { warn: 0, note: 1, good: 2 };
  signals.sort((a, b) => order[a.kind] - order[b.kind]);
  return { level, ...LEVELS[level], signals, empty: false, ...base };
}

/** Short one-line summary for the collapsed row (no jargon). */
export function summaryLine(m: ToneMeter): string {
  if (m.empty) return "Write a few words and a tone hint appears here.";
  const warns = m.signals.filter((s) => s.kind !== "good");
  if (!warns.length) return m.signals.length ? m.signals[0].label : "Nothing stands out.";
  return warns.length === 1 ? warns[0].label : `${warns[0].label} and ${warns.length - 1} more`;
}

// ---- Apply (Check tone): replace only the user's own plain paragraphs, keep everything else untouched ----

import { htmlToText, removeRichSig, QUOTE_OPEN } from "./composetext.ts";
import { attrsHide } from "./csshide.ts";

/** Above this many characters of editor HTML nothing is classified (no Apply, Copy only): an unclosed quote must not go quadratic. */
export const MAX_HTML = 200_000;

const SIG_OPEN = '<div data-mm-sig="';
const escHtml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const paragraphs = (s: string) => s.replace(/\r\n?/g, "\n").replace(/\s+$/, "").split("\n").map((l) => (l === "" ? "<div><br></div>" : `<div>${escHtml(l)}</div>`)).join("");

/** Plain editor: the new body, then the old quote / signature unchanged. */
export function replaceBodyText(text: string, body: string): string {
  const { tail } = splitDraft(text);
  const b = body.replace(/\r\n?/g, "\n").replace(/\s+$/, "");
  return tail ? `${b}\n\n${tail}` : b;
}

/** Rich editor: the html in front of the signature / quote block (whichever comes first). */
function richHead(html: string): { head: string; tail: string } {
  const cuts = [html.indexOf(SIG_OPEN), html.indexOf(QUOTE_OPEN)].filter((i) => i >= 0);
  const cut = cuts.length ? Math.min(...cuts) : -1;
  return cut < 0 ? { head: html, tail: "" } : { head: html.slice(0, cut), tail: html.slice(cut) };
}

/** The user's own words in a rich draft, from the HTML structure: no signature (even without a `-- ` line), no quote. */
export function richOwnText(html: string): string {
  const noSig = removeRichSig(html);
  const q = noSig.indexOf(QUOTE_OPEN);
  return htmlToText(q >= 0 ? noSig.slice(0, q) : noSig);
}

type BlockKind = "text" | "ws" | "keep";
interface Block { start: number; end: number; kind: BlockKind }
const VOID = new Set(["br", "img", "hr", "input", "meta", "link", "wbr", "col", "area", "base", "source", "embed", "param", "track"]);
const TEXT_TAGS = new Set(["div", "p", "br", "span"]);
const TAG_RE = /<!--[\s\S]*?-->|<(\/?)([a-zA-Z][a-zA-Z0-9:-]*)((?:[^>"']|"[^"]*"|'[^']*')*)>/g;

/** Top-level blocks of `head`. 'text' = plain paragraphs only (div / p / br / span, no data-mm-* or hidden); 'ws' = blank; 'keep' = anything else. */
function classify(head: string): Block[] {
  const out: Block[] = [];
  let pos = 0;
  while (pos < head.length) {
    if (head[pos] !== "<") {
      let e = head.indexOf("<", pos);
      if (e < 0) e = head.length;
      out.push({ start: pos, end: e, kind: head.slice(pos, e).trim() === "" ? "ws" : "text" });
      pos = e;
      continue;
    }
    TAG_RE.lastIndex = pos;
    const m = TAG_RE.exec(head);
    if (!m || m.index !== pos) { out.push({ start: pos, end: head.length, kind: "keep" }); break; }
    const tag = (m[2] ?? "").toLowerCase();
    if (!m[2] || m[1] || (VOID.has(tag) && tag !== "br")) { out.push({ start: pos, end: m.index + m[0].length, kind: "keep" }); pos = m.index + m[0].length; continue; }
    if (tag === "br") { out.push({ start: pos, end: m.index + m[0].length, kind: "ws" }); pos = m.index + m[0].length; continue; }
    // balanced scan of this element
    let depth = 1, ok = tag === "div" || tag === "p", end = -1;
    const selfClose = /\/\s*$/.test(m[3] ?? "");
    const bad = (t: string, a: string) => !TEXT_TAGS.has(t) || /data-mm|\bhidden\b/i.test(a) || attrsHide(a, true);
    if (bad(tag, m[3] ?? "")) ok = false;
    if (selfClose) { depth = 0; end = m.index + m[0].length; }
    while (depth > 0) {
      const n = TAG_RE.exec(head);
      if (!n) break;
      if (!n[2]) continue;
      const t = n[2].toLowerCase();
      if (n[1]) { depth--; if (depth === 0) end = n.index + n[0].length; continue; }
      if (bad(t, n[3] ?? "")) ok = false;
      if (!VOID.has(t) && !/\/\s*$/.test(n[3] ?? "")) depth++;
    }
    if (end < 0) { out.push({ start: pos, end: head.length, kind: "keep" }); break; }
    const inner = head.slice(pos, end);
    out.push({ start: pos, end, kind: !ok ? "keep" : htmlToText(inner).trim() === "" ? "ws" : "text" });
    pos = end;
  }
  return out;
}

export interface RichPlan {
  /** The text runs' own words (what Check tone sends); "" when there is none. */
  text: string;
  runs: number;
  /** Apply can splice the rewrite back in place. */
  canApply: boolean;
  /** Short reason shown when canApply is false. */
  reason: string;
}
interface Run { start: number; end: number }
function runsOf(head: string): Run[] {
  const runs: Run[] = [];
  let cur: Run | null = null;
  for (const b of classify(head)) {
    if (b.kind === "keep") { cur = null; continue; }
    if (b.kind === "text") { if (cur) cur.end = b.end; else { cur = { start: b.start, end: b.end }; runs.push(cur); } }
  }
  return runs;
}

export function planRich(html: string): RichPlan {
  const head = richHead(html).head;
  if (head.length > MAX_HTML) {
    return { text: htmlToText(head.slice(0, MAX_HTML)).trim().slice(0, MAX_CHARS), runs: 0, canApply: false, reason: "This message is too large to place a rewrite back safely. Use Copy." };
  }
  const runs = runsOf(head);
  const text = runs.map((r) => htmlToText(head.slice(r.start, r.end)).trim()).join("\n\n");
  if (!runs.length) return { text: "", runs: 0, canApply: false, reason: "There is no plain text to rewrite here (only tables, buttons or other blocks)." };
  if (runs.length > 1) return { text, runs: runs.length, canApply: false, reason: "Your text is split around tables, buttons or other blocks, so a rewrite cannot be placed back. Use Copy." };
  return { text, runs: 1, canApply: true, reason: "" };
}

/** Rich editor: swap the single plain-text run for the new paragraphs; every other byte stays. null = cannot be placed. */
export function replaceBodyHtml(html: string, body: string): string | null {
  const { head, tail } = richHead(html);
  if (head.length > MAX_HTML) return null;
  const runs = runsOf(head);
  if (runs.length !== 1) return null;
  const r = runs[0];
  return head.slice(0, r.start) + paragraphs(body) + head.slice(r.end) + tail;
}

// ---- placeholders the backend puts in what it sends ([link-1], [person-1]) ----

export interface ToneRestore { token: string; original: string }
const TOKEN_RE = /\[link\b[^\]\n]*\]?|\[person-\d+\]|\bperson-\d+\b/gi;
const URL_RE = /\bhttps?:\/\/[^\s<>"'\])}]+/gi;
const EMAIL_RE = /[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+/g;
const trimEnd = (u: string) => u.replace(/[.,;:!?]+$/, "").toLowerCase();
/** True when the draft changed since the check: an Apply would overwrite edits with a rewrite of the OLD text. */
export const draftChanged = (checked: string | null, current: string): boolean => checked !== null && checked !== current;

/**
 * Put the user's own links / addresses back for the tokens in a suggestion. Single pass (inserted originals are never re-scanned).
 * Token-like strings the user typed themselves (in `draft`) are identity. Unknown tokens, and http(s) links or addresses that are
 * not in the draft (a model-invented link), block.
 */
export function restoreTokens(text: string, restore: ToneRestore[] | undefined, draft = ""): { text: string; blocked: string } {
  const map = new Map((restore ?? []).map((r) => [r.token, r.original]));
  const typed = new Set((draft.match(TOKEN_RE) ?? []).map((t) => t.toLowerCase()));
  const bad: string[] = [];
  const out = text.replace(TOKEN_RE, (t) => {
    const o = map.get(t);
    if (o !== undefined) return o;
    if (!typed.has(t.toLowerCase())) bad.push(t);
    return t;
  });
  if (bad.length) return { text: out, blocked: `This suggestion contains a placeholder (${bad[0]}) that could not be matched to your own text, so it was not applied. Use Copy and fix it by hand.` };
  const dl = (draft + "\n" + (restore ?? []).map((r) => r.original).join("\n")).toLowerCase();
  const known = (x: string) => dl.includes(trimEnd(x));
  const fresh = [...(out.match(URL_RE) ?? []), ...(out.match(EMAIL_RE) ?? [])].find((x) => !known(x));
  if (fresh) return { text: out, blocked: `This suggestion contains a link or address (${fresh.slice(0, 60)}) that is not in your draft, so it was not applied. Use Copy and check it by hand.` };
  return { text: out, blocked: "" };
}

// ---- screen-reader announcements: at most one per `gap`, plus one trailing one when the window ends ----

export interface Announcer { push(level: string, label: string): void; dispose(): void }
export function createAnnouncer(emit: (s: string) => void, o: { gap?: number; now?: () => number; set?: (f: () => void, ms: number) => unknown; clear?: (h: unknown) => void } = {}): Announcer {
  const gap = o.gap ?? 5000, now = o.now ?? Date.now;
  const set = o.set ?? ((f, ms) => setTimeout(f, ms)), clear = o.clear ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
  let last = "", lastAt = -Infinity, timer: unknown = null;
  const stop = () => { if (timer !== null) { clear(timer); timer = null; } };
  const fire = (level: string, label: string) => { timer = null; last = level; lastAt = now(); emit(`Tone: ${label}`); };
  return {
    push(level, label) {
      stop();
      if (level === last) return;
      const wait = gap - (now() - lastAt);
      if (wait <= 0) fire(level, label); else timer = set(() => fire(level, label), wait);
    },
    dispose: stop,
  };
}
