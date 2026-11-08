// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/tonemeter.test.ts
import test from "node:test";
import assert from "node:assert/strict";
import { analyseTone, splitDraft, ownText, MAX_CHARS, DISCLAIMER, summaryLine, replaceBodyText, replaceBodyHtml, planRich, richOwnText, restoreTokens, draftChanged, MAX_HTML, createAnnouncer } from "./tonemeter.ts";
const QUOTE_OPEN = '<div data-mm-quote="1">';

const ids = (m) => m.signals.map((s) => s.id);
const has = (t, id) => ids(analyseTone(t)).includes(id);

test("disclaimer text is fixed", () => assert.equal(DISCLAIMER, "Quick check on your wording, not a judgement"));
test("empty or tiny drafts give no verdict", () => {
  for (const t of ["", "ok", "  \n "]) { const m = analyseTone(t); assert.equal(m.empty, true); assert.equal(m.level, "neutral"); assert.deepEqual(m.signals, []); }
  assert.match(summaryLine(analyseTone("")), /Write a few words/);
});
test("ALL-CAPS shouting, but not acronyms or a single name", () => {
  assert.ok(has("Hi, I NEED this FIXED today", "caps"));
  assert.ok(has("Hi, this is URGENT please look", "caps"));
  assert.ok(!has("Hi, please send the PDF to the CEO by EOD, thanks", "caps"));
  assert.ok(!has("Hi Maria, the invoice from ACME and NASA is ready", "caps"));
  assert.ok(!has("Dear Mr. SMITH, thanks for your note yesterday", "caps"));
  assert.ok(has("THIS IS NOT WHAT WE AGREED ON\nplease fix", "caps"));
});
test("repeated punctuation", () => {
  assert.ok(has("Where is my order?! I asked yesterday", "punct"));
  assert.ok(has("Please reply!! It is important", "punct"));
  assert.ok(!has("Thanks so much! Great to hear. Really?", "punct"));
});
test("sharp and passive-aggressive phrases, one signal per rule with examples", () => {
  for (const p of ["As per my last email, the file was attached", "Per my previous message the date is Friday", "As I already said, it ships Monday", "Obviously this was missed", "Clearly you did not read it", "Once again the report is late", "I am disappointed with this", "This is unacceptable", "Whatever, do as you like"]) {
    const m = analyseTone(p + " and that is all I have to say now."); assert.ok(m.signals.some((s) => s.kind === "warn" && (s.id === "pa" || s.id === "harsh")), p);
  }
  const m = analyseTone("Hi Sam, as per my last email and once again, this is late.");
  assert.equal(m.signals.filter((s) => s.id === "pa").length, 1);
  assert.match(m.signals.find((s) => s.id === "pa").label, /as per my last email/i);
  assert.ok(m.signals[0].why.length > 10);
});
test("harsh words push to tense", () => {
  assert.equal(analyseTone("This is unacceptable and ridiculous. You failed again.").level, "tense");
});
test("blunt openers and missing greeting", () => {
  assert.ok(has("Send me the report by tonight and copy the whole team on it", "blunt"));
  assert.ok(!has("Hi Sam, send me the report by tonight and copy the whole team on it", "blunt"));
  assert.ok(has("The report is attached and the numbers look fine to me overall", "nogreet"));
  assert.ok(!has("Hello Sam, the report is attached and the numbers look fine to me", "nogreet"));
  assert.ok(!has("Please send me the report by tonight and copy the whole team on it", "blunt"));
});
test("absolutes aimed at the reader only", () => {
  assert.ok(has("Hi, you always forget the attachment and you never reply", "absolute"));
  assert.ok(has("Hi, you are never on time for these", "absolute"));
  assert.ok(!has("Hi, we always ship on Monday and never on Sunday, thanks", "absolute"));
});
test("very long sentences", () => {
  const s = Array.from({ length: 40 }, (_, i) => "word" + i).join(" ") + ".";
  assert.ok(has("Hi Sam. " + s + " Thanks.", "longsent"));
  assert.ok(!has("Hi Sam. " + s.split(" ").slice(0, 30).join(" ") + ". Thanks.", "longsent"));
});
test("long message and missing sign-off", () => {
  const long = "Hi Sam, " + Array.from({ length: 22 }, () => "We reviewed the numbers for the quarter and everything matches the plan we discussed before. ").join("");
  const m = analyseTone(long);
  assert.ok(ids(m).includes("longmsg") && ids(m).includes("signoff"));
  assert.ok(!has(long + "\nThanks,\nAnn", "signoff"));
  assert.ok(!has("Hi, quick note: the numbers match the plan we discussed. Thanks", "signoff"));
});
test("politeness markers and warm level", () => {
  const m = analyseTone("Hi Sam, could you please send the report when you have a moment? I really appreciate it. Thanks!");
  assert.ok(ids(m).includes("polite"));
  assert.equal(m.level, "warm");
  assert.equal(m.signals.at(-1).kind, "good");
});
test("hedging overload only when dense", () => {
  assert.ok(has("Hi, maybe I think perhaps we could possibly just move it, I guess, if that is fine", "hedge"));
  assert.ok(!has("Hi Sam, I think the plan is fine and we can ship on Monday as planned. Thanks for the quick review of the numbers and the notes.", "hedge"));
});
test("levels: firm vs tense vs neutral", () => {
  assert.equal(analyseTone("The invoice is attached. The totals are final and the due date is Friday.").level, "neutral");
  assert.ok(["firm", "tense"].includes(analyseTone("Send me the report today. You never reply to my questions!!").level));
  assert.equal(analyseTone("Fix this NOW!! This is unacceptable, you always do this and I am disappointed.").level, "tense");
});
test("a long, polite email raises no warnings", () => {
  const body = [
    "Dear Priya,", "",
    "Thank you for the detailed notes from yesterday; I really appreciate the time you put in. I went through the proposal with the finance team this morning, and overall we are happy with the direction. There are two points where I think we should align before we sign: the payment schedule for the second phase, and who owns the integration testing in the weeks before launch.",
    "On the payment schedule, we would prefer to split the second phase into two equal instalments, one at kickoff and one at delivery, which is how we handled the first phase. If that works for you, could you please update the draft and send it back? I am happy to jump on a call if it is easier to talk it through.",
    "On testing, our engineers are glad to help, but we would like the final sign-off to stay with your team, since you know the system best. Perhaps we can agree a short checklist together next week.",
    "Thanks again for your patience. Looking forward to hearing from you.", "", "Best regards,", "Ann",
  ].join("\n");
  const m = analyseTone(body);
  assert.ok(m.words > 150);
  assert.deepEqual(m.signals.filter((s) => s.kind === "warn").map((s) => s.id), []);
  assert.equal(m.level, "warm");
});
test("quoted original, forwarded banner and signature are not analysed", () => {
  const quoted = "On Mon, 3 Nov 2026, Bob wrote:\n> THIS IS UNACCEPTABLE!! You never listen. As per my last email, once again ridiculous.\n";
  const own = "Hi Bob,\n\nThanks for flagging this, I will look into it today and report back. Please send the logs if you can.\n\nBest,\nAnn\n";
  const m = analyseTone(own + "\n" + quoted);
  assert.deepEqual(m.signals.filter((s) => s.kind === "warn"), []);
  assert.equal(m.level, "warm");
  assert.equal(analyseTone(own + "\n-- \nANN SMITH, ACME OBVIOUSLY INC\nUNACCEPTABLE STREET 5").signals.filter((s) => s.kind === "warn").length, 0);
  assert.equal(analyseTone(own + "\n---------- Forwarded message ----------\nFrom: x\nTHIS IS RIDICULOUS!!\n").signals.filter((s) => s.kind === "warn").length, 0);
  assert.equal(analyseTone("> UNACCEPTABLE!!\nHi Bob, thanks for the note, I will look into it today. Please send the logs.").signals.filter((s) => s.kind === "warn").length, 0);
  assert.equal(analyseTone("Hi\n\n-----Original Message-----\nUNACCEPTABLE!!").empty, true);
});
test("splitDraft keeps the tail exactly for Apply", () => {
  const t = "Hello there my friend\n\nOn Mon, 3 Nov 2026, Bob wrote:\n> hi\n";
  const { body, tail } = splitDraft(t);
  assert.equal(body + tail, t);
  assert.ok(tail.startsWith("On Mon"));
  assert.equal(ownText(t), "Hello there my friend");
  assert.deepEqual(splitDraft("plain"), { body: "plain", tail: "" });
  const sig = splitDraft("Body here\n\n-- \nSig\n\nOn Mon, 3 Nov 2026, Bob wrote:\n> q");
  assert.ok(sig.tail.startsWith("-- \nSig"));
});
test("names and ordinary words do not trigger phrase rules", () => {
  assert.ok(!has("Hi Clarity Obviousky and Whatever Inc, thanks for the update on the order today.", "pa"));
  assert.ok(!has("Hi, our Lazy Susan arrived and the box is fine, thanks a lot for that.", "harsh"));
  assert.ok(has("Hi. Whatever, do it your way then.", "pa"));
  assert.ok(has("Thanks. Obviously it was missed, though.", "pa"));
});
test("cap: huge drafts are cut and analysed fast", () => {
  const big = "Hi Sam, thanks for the note. ".repeat(20000);
  const t0 = performance.now();
  const m = analyseTone(big);
  const ms = performance.now() - t0;
  assert.equal(m.truncated, true);
  assert.ok(m.words < MAX_CHARS / 3);
  assert.ok(ms < 150, `took ${ms} ms`);
  const t1 = performance.now();
  for (let i = 0; i < 20; i++) analyseTone("Hi Sam, could you please send the report? Thanks, as per my last email. ".repeat(250));
  assert.ok((performance.now() - t1) / 20 < 50);
});
test("an extremely long single line with no spaces cannot hang", () => {
  const t0 = performance.now();
  analyseTone("a".repeat(MAX_CHARS * 3) + "!".repeat(5000));
  assert.ok(performance.now() - t0 < 300);
});

test("apply (plain): quote and signature survive byte for byte", () => {
  const t = "Old words\n\n-- \nAnn\n\nOn Mon, 3 Nov 2026, Bob wrote:\n> hi\n";
  const r = replaceBodyText(t, "New words\n\nWith two paragraphs.\n");
  assert.ok(r.startsWith("New words\n\nWith two paragraphs.\n\n-- \nAnn"));
  assert.ok(r.endsWith("On Mon, 3 Nov 2026, Bob wrote:\n> hi\n"));
  assert.equal(replaceBodyText("Old", "New  \n"), "New");
});
test("apply (rich): only the part before the signature / quote is replaced; text is escaped", () => {
  const html = `<div>Old</div><div><br></div><div data-mm-sig="s1"><div><br></div><div>-- </div><div>Ann</div></div>${QUOTE_OPEN}<div>On x wrote:</div><blockquote>q</blockquote></div>`;
  const r = replaceBodyHtml(html, "Hi <b>x</b> & co\n\nBye");
  assert.ok(r.startsWith("<div>Hi &lt;b&gt;x&lt;/b&gt; &amp; co</div><div><br></div><div>Bye</div>"));
  assert.ok(!r.includes("<b>") && !r.includes("Old"));
  assert.ok(r.includes('<div data-mm-sig="s1">') && r.includes(QUOTE_OPEN) && r.includes("<blockquote>q</blockquote>"));
  assert.equal(replaceBodyHtml("<div>Old</div>", "New"), "<div>New</div>");
  assert.equal(replaceBodyHtml(`<div>Old</div>${QUOTE_OPEN}<blockquote>q</blockquote></div>`, "New"), `<div>New</div>${QUOTE_OPEN}<blockquote>q</blockquote></div>`);
});

const TABLE = '<table border="1"><tr><td>Qty</td><td>5</td></tr></table>';
const CALLOUT = '<div data-mm-callout="info" style="border:1px solid"><div>Heads up</div></div>';
const BUTTON = '<table data-mm-button="1"><tr><td><a href="https://x.example/pay">Pay now</a></td></tr></table>';
const SIG = '<div data-mm-sig="s1"><div><br></div><div>-- </div><div>Ann</div></div>';
const QUOTE = QUOTE_OPEN + "<div>On Mon, Bob wrote:</div><blockquote>q text</blockquote></div>";

test("Q01 rich apply keeps tables, callouts and buttons byte for byte; only the text run changes", () => {
  const html = `${TABLE}${CALLOUT}${BUTTON}<div>Old words</div><div>more old</div>${SIG}${QUOTE}`;
  const plan = planRich(html);
  assert.equal(plan.canApply, true);
  assert.equal(plan.text, "Old words\nmore old");
  const r = replaceBodyHtml(html, "New one\n\nNew two");
  assert.equal(r, `${TABLE}${CALLOUT}${BUTTON}<div>New one</div><div><br></div><div>New two</div>${SIG}${QUOTE}`);
});
test("Q01 text before a kept block: blocks around it stay", () => {
  const html = `<div>Hello</div><div><br></div>${TABLE}`;
  assert.equal(replaceBodyHtml(html, "Hi there"), `<div>Hi there</div><div><br></div>${TABLE}`);
});
test("Q01 formatted / linked / list / image paragraphs are kept, not rewritten", () => {
  for (const b of ["<div>a <b>bold</b> word</div>", '<div>see <a href="https://a.b">here</a></div>', "<ul><li>x</li></ul>", '<div><img src="cid:1"></div>', "<ol><li>x</li></ol>"]) {
    const p = planRich(b);
    assert.equal(p.runs, 0, b);
    assert.equal(p.canApply, false, b);
    assert.equal(p.text, "", b);
    assert.equal(replaceBodyHtml(b, "x"), null, b);
  }
});
test("Q01 no text run: Apply disabled with a reason", () => {
  const p = planRich(`${TABLE}${BUTTON}${SIG}`);
  assert.equal(p.canApply, false);
  assert.match(p.reason, /no plain text/);
  assert.equal(replaceBodyHtml(`${TABLE}${SIG}`, "x"), null);
});
test("Q01 two separate text runs: Apply disabled (Copy only); both are still sent", () => {
  const html = `<div>First part</div>${TABLE}<div>Second part</div>`;
  const p = planRich(html);
  assert.equal(p.runs, 2);
  assert.equal(p.canApply, false);
  assert.match(p.reason, /Copy/);
  assert.equal(p.text, "First part\n\nSecond part");
  assert.equal(replaceBodyHtml(html, "x"), null);
});
test("Q01 sent text excludes table and callout text", () => {
  const p = planRich(`<div>Mine</div>${TABLE}${CALLOUT}`);
  assert.ok(!/Qty|Heads up/.test(p.text));
});

test("Q03 rich own text: no signature (even without a -- line) and no quote", () => {
  const sig = '<div data-mm-sig="s2"><div><br></div><div><b>Ann Lee</b></div><div>CEO, Acme Corp</div></div>';
  const html = `<div>My own words here please.</div>${sig}${QUOTE}`;
  for (const t of [richOwnText(html), planRich(html).text, ownText(richOwnText(html))]) {
    assert.ok(t.includes("My own words"));
    assert.ok(!/Ann Lee|Acme|Bob wrote|q text/.test(t), t);
  }
  const html2 = `<div>Mine again.</div>${QUOTE}${sig}`;
  assert.ok(!/Ann Lee|q text/.test(richOwnText(html2)));
});

const RS = [{ token: "[link-1]", original: "https://x.example/a?b=1" }, { token: "[person-1]", original: "kim@example.org" }];
test("Q02 tokens echoed by the model are replaced with the user's originals", () => {
  const r = restoreTokens("See [link-1] and write to [person-1]. Again [link-1].", RS);
  assert.equal(r.blocked, "");
  assert.equal(r.text, "See https://x.example/a?b=1 and write to kim@example.org. Again https://x.example/a?b=1.");
});
test("Q02 an original containing person-N is not re-scanned", () => {
  const r = restoreTokens("Mail [person-1]", [{ token: "[person-1]", original: "person-9@example.org" }]);
  assert.equal(r.blocked, "");
  assert.equal(r.text, "Mail person-9@example.org");
});
test("Q02 invented or shortened tokens block Apply", () => {
  for (const t of ["Open [link-7] now", "Open [link: example.org] now", "Ask person-3 about it", "Ask [person-4]", "Open [link]"]) {
    const r = restoreTokens(t, RS);
    assert.ok(r.blocked, t);
  }
  assert.ok(restoreTokens("Open [link-1] [link-2]", RS).blocked);
  assert.equal(restoreTokens("Plain text, no tokens.", undefined).blocked, "");
  assert.ok(restoreTokens("Hi [person-1]", undefined).blocked);
});

test("Q10 announcer: trailing announcement when the window ends, cancelled by a newer change", () => {
  let t = 100000; const timers = new Map(); let id = 0; const said = [];
  const a = createAnnouncer((s) => said.push(s), { now: () => t, set: (f, ms) => { timers.set(++id, { f, at: t + ms }); return id; }, clear: (h) => timers.delete(h) });
  const advance = (ms) => { t += ms; for (const [k, v] of [...timers]) if (v.at <= t) { timers.delete(k); v.f(); } };
  a.push("neutral", "Neutral");
  assert.deepEqual(said, ["Tone: Neutral"]);
  advance(1000); a.push("tense", "Tense");
  assert.deepEqual(said, ["Tone: Neutral"]);
  advance(1000); a.push("firm", "Firm");           // newer change replaces the pending one
  advance(3000);
  assert.deepEqual(said, ["Tone: Neutral", "Tone: Firm"]);
  advance(1000); a.push("warm", "Warm"); advance(500); a.push("firm", "Firm"); // back to what was announced: nothing pending
  advance(10000);
  assert.deepEqual(said, ["Tone: Neutral", "Tone: Firm"]);
});

test("user-typed token-like strings are identity; invented ones still block", () => {
  const draft = "See [link-1] and [link to doc] and person-2 please";
  const r = restoreTokens("Please see [link-1], [link to doc] and person-2.", undefined, draft);
  assert.equal(r.blocked, "");
  assert.equal(r.text, "Please see [link-1], [link to doc] and person-2.");
  assert.ok(restoreTokens("See [link-9]", undefined, draft).blocked);
  assert.ok(restoreTokens("See [link-1]", undefined, "").blocked);
});

test("a link or address not in the draft blocks Apply", () => {
  const draft = "Mail bob@example.org, docs at https://example.org/a";
  assert.equal(restoreTokens("Write bob@example.org or see https://example.org/a.", undefined, draft).blocked, "");
  assert.match(restoreTokens("See https://evil.test/x", undefined, draft).blocked, /not in your draft/);
  assert.match(restoreTokens("Mail eve@evil.test", undefined, draft).blocked, /not in your draft/);
});

test("draftChanged: only a changed draft after a check is stale", () => {
  assert.equal(draftChanged(null, "x"), false);
  assert.equal(draftChanged("x", "x"), false);
  assert.equal(draftChanged("x", "x edited"), true);
});

test("an element hiding itself is kept, not replaced", () => {
  for (const hid of ['style="display:none"', 'style="visibility:hidden"', 'style="font-size:0"', 'style="opacity:0"']) {
    const html = `<div>Mine</div><div><span ${hid}>secret</span></div><div>More</div>`;
    assert.equal(planRich(html).canApply, false, hid);
    assert.equal(replaceBodyHtml(html, "x"), null, hid);
  }
  assert.equal(planRich('<div><span style="color:#333">fine</span></div>').canApply, true);
});

test("huge html is not classified", () => {
  const html = "<div>" + "a".repeat(MAX_HTML + 10);
  const t0 = Date.now();
  const p = planRich(html);
  assert.equal(p.canApply, false);
  assert.equal(replaceBodyHtml(html, "x"), null);
  assert.ok(Date.now() - t0 < 2000);
});
