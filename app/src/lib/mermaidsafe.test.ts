// @ts-nocheck -- run with: node --experimental-strip-types --test src/lib/mermaidsafe.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { classifyMermaidError, cleanMermaidSource, DIAGRAM_FONT_STACK, diagramAlt, diagramImgStyle, embedSvgFont, flowchartOrientations, isDrawableMermaid, MAX_MERMAID_SRC, MERMAID_LAYOUT, mermaidErrorReason, mermaidTooLarge, ORIENT_MARGIN, pickOrientation, sizeSvg, svgDataUrl, svgViewBoxSize, themeVariables, withTimeout } from "./mermaidsafe.ts";
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

test("flowchartOrientations gives both drawings and the stated direction", () => {
  assert.deepEqual(flowchartOrientations("graph TD\nA-->B"), { stated: "TD", td: "graph TD\nA-->B", lr: "graph LR\nA-->B" });
  assert.deepEqual(flowchartOrientations("flowchart LR\nA-->B"), { stated: "LR", td: "flowchart TD\nA-->B", lr: "flowchart LR\nA-->B" });
  assert.deepEqual(flowchartOrientations("graph TB;A-->B"), { stated: "TD", td: "graph TD;A-->B", lr: "graph LR;A-->B" });
  assert.deepEqual(flowchartOrientations("graph BT\nA-->B"), { stated: "TD", td: "graph BT\nA-->B", lr: "graph LR\nA-->B" });
  assert.deepEqual(flowchartOrientations("graph RL\nA-->B"), { stated: "LR", td: "graph TD\nA-->B", lr: "graph RL\nA-->B" });
  assert.deepEqual(flowchartOrientations("flowchart\nA-->B"), { stated: null, td: "flowchart TD\nA-->B", lr: "flowchart LR\nA-->B" });
  assert.deepEqual(flowchartOrientations("flowchart-elk LR\nA-->B"), { stated: "LR", td: "flowchart TD\nA-->B", lr: "flowchart LR\nA-->B" });
  const fm = flowchartOrientations('---\nconfig:\n  x: 1\n---\n%%{init: {}}%%\ngraph LR\nA-->B\nclick A call f()');
  assert.deepEqual(fm, { stated: "LR", td: "graph TD\nA-->B", lr: "graph LR\nA-->B" }, "cleaned like cleanMermaidSource");
  assert.equal(flowchartOrientations("graph TD\nsubgraph S\ndirection LR\nA-->B\nend").lr, "graph LR\nsubgraph S\ndirection LR\nA-->B\nend", "subgraph direction untouched");
  assert.equal(flowchartOrientations("sequenceDiagram\nA->>B: graph LR"), null);
  assert.equal(flowchartOrientations("graphical TD\nA-->B"), null);
  assert.equal(flowchartOrientations("stateDiagram-v2\n[*]-->A"), null);
});
test("pickOrientation keeps the narrower drawing; a stated direction needs a clear margin to lose", () => {
  assert.equal(ORIENT_MARGIN, 0.8);
  assert.equal(pickOrientation(null, 300, 200), "LR");
  assert.equal(pickOrientation(null, 200, 300), "TD");
  assert.equal(pickOrientation(null, 200, 200), "TD", "tie goes to top-down");
  assert.equal(pickOrientation("TD", 1000, 790), "LR");
  assert.equal(pickOrientation("TD", 1000, 810), "TD");
  assert.equal(pickOrientation("LR", 790, 1000), "TD");
  assert.equal(pickOrientation("LR", 810, 1000), "LR");
  assert.equal(pickOrientation("LR", 800, 1000), "LR", "exactly at the margin keeps the stated one");
});
test("svgViewBoxSize reads the root viewBox only", () => {
  assert.deepEqual(svgViewBoxSize('<svg id="m" viewBox="-8 -8 120.5 80"><svg viewBox="0 0 1 1"/></svg>'), { w: 120.5, h: 80 });
  assert.deepEqual(svgViewBoxSize('<svg viewBox="0,0,10,20"/>'), { w: 10, h: 20 });
  assert.equal(svgViewBoxSize("<svg><g/></svg>"), null);
  assert.equal(svgViewBoxSize("<p>no svg</p>"), null);
});
test("embedSvgFont puts an @font-face first in the root svg, and only for a woff2 data: URL", () => {
  const font = "data:font/woff2;base64,d09GMgABAAA+/w==";
  const out = embedSvgFont('<svg width="1" viewBox="0 0 1 1"><style>#m{}</style><g/></svg>', font);
  assert.equal(out, '<svg width="1" viewBox="0 0 1 1"><style>@font-face{font-family:"Oswald";src:url(' + font + ') format("woff2");font-weight:200 700;font-style:normal}</style><style>#m{}</style><g/></svg>');
  const svg = '<svg viewBox="0 0 1 1"><g/></svg>';
  assert.equal(embedSvgFont(svg, "https://x.test/f.woff2"), svg);
  assert.equal(embedSvgFont(svg, "data:font/woff2;base64,AAAA) } * { x: url(https://x.test/"), svg);
  assert.equal(embedSvgFont(svg, "data:font/woff;base64,AAAA"), svg);
  assert.equal(embedSvgFont("<p>x</p>", font), "<p>x</p>");
});
test("diagram text uses the UI font first, then local fallbacks", () => {
  const v = themeVariables({ bg: "#0a0a0a", text: "#e6e6e6", line: "#8f8f8f", accent: "#2bd4bf", surface: "#191919" });
  assert.equal(v.fontFamily, DIAGRAM_FONT_STACK);
  assert.match(DIAGRAM_FONT_STACK, /^"Oswald", .*sans-serif$/);
});
test("diagramImgStyle gives the drawing's own size in px, never pre-multiplied by the chat scale", () => {
  const svg = sizeSvg('<svg id="m" width="100%" viewBox="0 0 370.5 393" style="max-width: 370px;"><g/></svg>');
  assert.equal(diagramImgStyle(svgViewBoxSize(svg)), "width:370.5px;height:393px");
  // Same numbers as the width/height sizeSvg writes, so the explicit size and the image's intrinsic size agree.
  assert.match(svg, /^<svg width="370\.5" height="393"/);
  assert.equal(diagramImgStyle(null), "");
  assert.equal(diagramImgStyle({ w: 0, h: 10 }), "");
  assert.equal(diagramImgStyle({ w: 10, h: -1 }), "");
  assert.equal(diagramImgStyle({ w: Number.NaN, h: 10 }), "");
  assert.equal(diagramImgStyle({ w: Infinity, h: 10 }), "");
});
