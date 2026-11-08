// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/mermaidsafe.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { classifyMermaidError, cleanMermaidSource, diagramAlt, isDrawableMermaid, MAX_MERMAID_SRC, MERMAID_LAYOUT, mermaidErrorReason, mermaidTooLarge, sizeSvg, svgDataUrl, themeVariables, withTimeout } from "./mermaidsafe.ts";
import { parseMarkdown } from "./aimd.ts";

test("cleanMermaidSource draws left-right flowcharts top-down", () => {
  assert.equal(cleanMermaidSource("flowchart LR\nA-->B"), "flowchart TD\nA-->B");
  assert.equal(cleanMermaidSource("graph RL\nA-->B"), "graph TD\nA-->B");
  assert.equal(cleanMermaidSource("graph TD\nA-->B"), "graph TD\nA-->B");
  assert.equal(cleanMermaidSource("sequenceDiagram\nA->>B: LR"), "sequenceDiagram\nA->>B: LR");
});

test("only closed mermaid fences are drawable", () => {
  const done = parseMarkdown("```mermaid\ngraph TD\nA-->B\n```");
  const open = parseMarkdown("```mermaid\ngraph TD\nA-->");
  assert.equal(isDrawableMermaid(done[0] as never), true);
  assert.equal(isDrawableMermaid(open[0] as never), false);
  assert.equal(isDrawableMermaid(parseMarkdown("```js\nx\n```")[0] as never), false);
  assert.equal(isDrawableMermaid(parseMarkdown("```Mermaid\ngraph TD\n```")[0] as never), true);
});
test("tilde fence and text after the block", () => {
  const b = parseMarkdown("~~~mermaid\npie\n\"a\":1\n~~~\nafter");
  assert.equal(isDrawableMermaid(b[0] as never), true);
  assert.equal(b.length, 2);
});
test("size cap", () => {
  assert.equal(mermaidTooLarge("x".repeat(MAX_MERMAID_SRC)), false);
  assert.equal(mermaidTooLarge("x".repeat(MAX_MERMAID_SRC + 1)), true);
});
test("directives, frontmatter and click lines are stripped", () => {
  const s = '---\nconfig:\n  securityLevel: loose\n---\n%%{init: {"securityLevel":"loose"}}%%\ngraph TD\nA-->B\nclick A href "https://x.test"\n  click B call f()';
  assert.equal(cleanMermaidSource(s), "graph TD\nA-->B");
});
test("alt text uses only a recognised diagram kind", () => {
  assert.equal(diagramAlt("flowchart LR\nA-->B"), "Diagram: flowchart");
  assert.equal(diagramAlt("%% c\n\nsequenceDiagram\nA->>B: hi"), "Diagram: sequence diagram");
  assert.equal(diagramAlt('<img src=x onerror=alert(1)>'), "Diagram");
  assert.equal(diagramAlt(""), "Diagram");
});
test("sizeSvg sets concrete size from viewBox and drops max-width style", () => {
  const out = sizeSvg('<svg id="m" width="100%" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120.5 80" style="max-width: 120px;"><g/></svg>');
  assert.match(out, /^<svg width="120\.5" height="80" id="m" xmlns=/);
  assert.ok(!out.includes("max-width"));
  assert.equal(sizeSvg("<p>no svg</p>"), "<p>no svg</p>");
});
test("svgDataUrl round-trips UTF-8", () => {
  const svg = '<svg>é€ 日本</svg>';
  const u = svgDataUrl(svg);
  assert.ok(u.startsWith("data:image/svg+xml;base64,"));
  assert.equal(Buffer.from(u.split(",")[1], "base64").toString("utf8"), svg);
});
test("themeVariables carry the app colours", () => {
  const v = themeVariables({ bg: "#0a0a0a", text: "#e6e6e6", line: "#8f8f8f", accent: "#2bd4bf", surface: "#191919" });
  assert.equal(v.lineColor, "#8f8f8f"); assert.equal(v.primaryBorderColor, "#2bd4bf"); assert.equal(v.textColor, "#e6e6e6");
});
test("withTimeout", async () => {
  await assert.rejects(withTimeout(new Promise(() => {}), 10), /timeout/);
  assert.equal(await withTimeout(Promise.resolve(3), 50), 3);
});

test("repairs the dotted-link-with-text mistake models make", () => {
  assert.equal(cleanMermaidSource("graph TD\nX[T<br/>0] -->.starting point for drafts.--> V"), "graph TD\nX[T<br/>0] -. starting point for drafts .-> V");
  assert.equal(cleanMermaidSource("graph TD\nA -->|a. b.| B"), "graph TD\nA -->|a. b.| B", "valid links untouched");
});

test("layout is dagre, never the stubbed-out elk (mermaid 12 defaults to elk)", () => {
  assert.equal(MERMAID_LAYOUT, "dagre");
  assert.equal(cleanMermaidSource("flowchart-elk TD\nA-->B"), "flowchart TD\nA-->B");
  assert.equal(cleanMermaidSource("graph TD\nflowchart-elk-->B"), "graph TD\nflowchart-elk-->B", "only the header is rewritten");
});
test("error reason: first line, jison Expecting line, no source text, capped", () => {
  const jison = "Parse error on line 4:\n...A-->B[secret label\n-----------------^\nExpecting 'SQE', 'PE', got 'EOF'";
  assert.equal(mermaidErrorReason(new Error(jison)), "Parse error on line 4: Expecting 'SQE', 'PE', got 'EOF'");
  assert.equal(mermaidErrorReason(new Error("Lexical error on line 2. Unrecognized text.\n...secret\n---^")), "Lexical error on line 2. Unrecognized text.");
  assert.equal(mermaidErrorReason(new Error("No diagram type detected matching given configuration for text: hello secret")), "No diagram type detected matching given configuration");
  const lex = mermaidErrorReason(new Error("Parsing failed: unexpected character: ->secret<- at offset: 3. Expecting token of type 'X' but found `secret`."));
  assert.ok(!lex.includes("secret"), lex);
  assert.equal(mermaidErrorReason(new Error("ELK layout is not included")), "ELK layout is not included");
  const long = mermaidErrorReason(new Error("x".repeat(500)));
  assert.equal(long.length, 160); assert.ok(long.endsWith("…"));
  assert.equal(mermaidErrorReason(new Error("a\u0007b\t c")), "a b c");
  assert.equal(mermaidErrorReason("plain string"), "plain string");
  assert.equal(mermaidErrorReason({ message: "obj" }), "obj");
  assert.equal(mermaidErrorReason(undefined), "Unknown error");
  assert.equal(mermaidErrorReason(new Error("")), "Unknown error");
});
test("only parse-step source errors are blamed on the diagram", () => {
  assert.equal(classifyMermaidError("parse", new Error("Parse error on line 1:")), "syntax");
  assert.equal(classifyMermaidError("parse", new Error("No diagram type detected matching given configuration")), "syntax");
  assert.equal(classifyMermaidError("render", new Error("Parse error on line 1:")), "app");
  assert.equal(classifyMermaidError("render", new Error("ELK layout is not included")), "app");
  assert.equal(classifyMermaidError("load", new Error("x")), "app");
  assert.equal(classifyMermaidError("parse", new TypeError("Importing a module script failed.")), "app");
  assert.equal(classifyMermaidError("parse", new Error("Failed to fetch dynamically imported module: x.js")), "app");
  assert.equal(classifyMermaidError("parse", new Error("timeout after 5 s")), "app");
  assert.equal(classifyMermaidError("parse", new TypeError("undefined is not an object")), "app");
});
