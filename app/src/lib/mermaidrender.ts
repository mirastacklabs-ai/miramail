// Lazy mermaid renderer. `mermaid` is imported only when a finished ```mermaid block is shown, so it lives in its own chunk.
// Resolves to an SVG data: URL or a short failure reason (never throws). Renders are serialised because mermaid config is global.
import {
  classifyMermaidError, cleanMermaidSource, DIAGRAM_FONT, embedSvgFont, flowchartOrientations, MAX_MERMAID_SRC, MERMAID_LAYOUT,
  MERMAID_TIMEOUT_MS, mermaidErrorReason, mermaidTooLarge, pickOrientation, sizeSvg, svgDataUrl, svgViewBoxSize, themeVariables,
  withTimeout, type MermaidResult, type ThemeColors,
} from "./mermaidsafe";

let chain: Promise<unknown> = Promise.resolve();
let seq = 0;

export function renderMermaid(src: string, colors: ThemeColors): Promise<MermaidResult> {
  if (mermaidTooLarge(src)) return Promise.resolve({ ok: false, kind: "syntax", reason: `The diagram is longer than ${MAX_MERMAID_SRC} characters.` });
  const job = chain.then(() => draw(src, colors), () => draw(src, colors));
  chain = job.catch(() => null);
  return job;
}

// The UI font as a woff2 data: URL, loaded once and only here (its own small chunk, next to mermaid). Every draw also
// makes sure the page has the font loaded (cheap once it is), because mermaid sizes the node boxes by measuring the
// text in the page. A font failure only costs the font: the diagram is still drawn with the fallback stack.
let fontUrl: Promise<string | null> | null = null;
async function diagramFont(): Promise<string | null> {
  fontUrl ??= import("../assets/fonts/oswald-latin-wght-normal.woff2?inline").then((m) => m.default, () => { fontUrl = null; return null; });
  const [url] = await Promise.all([fontUrl, document.fonts?.load(`18px "${DIAGRAM_FONT}"`).catch(() => null)]);
  return url;
}

async function draw(src: string, colors: ThemeColors): Promise<MermaidResult> {
  let phase: "load" | "parse" | "render" = "load";
  const ids: string[] = [];
  try {
    const code = cleanMermaidSource(src);
    if (!code) return { ok: false, kind: "syntax", reason: "The diagram is empty." };
    const start = Date.now();
    return await withTimeout((async (): Promise<MermaidResult> => {
      const [mermaid, font] = await Promise.all([import("mermaid").then((m) => m.default), diagramFont()]);
      mermaid.initialize({
        startOnLoad: false,
        securityLevel: "strict",
        theme: "base",
        themeVariables: themeVariables(colors),
        layout: MERMAID_LAYOUT,
        flowchart: { htmlLabels: false },
        htmlLabels: false,
        deterministicIds: true,
        maxTextSize: 20_000,
        logLevel: 5,
      });
      phase = "parse"; // parse first so a source error can be told apart from a drawing failure
      await mermaid.parse(code);
      phase = "render";
      const render = async (c: string) => { const id = `mmd-${++seq}`; ids.push(id); return sizeSvg((await mermaid.render(id, c)).svg); };
      const flow = flowchartOrientations(src);
      let svg: string;
      if (!flow) svg = await render(code);
      else {
        // A flowchart is drawn both ways and the narrower drawing is kept (rule: pickOrientation). The source's own
        // direction is drawn first; the second drawing is optional and only gets the time that is left.
        const first: "TD" | "LR" = flow.stated ?? "TD";
        const other = first === "TD" ? "LR" : "TD";
        svg = await render(flow[first === "TD" ? "td" : "lr"]);
        const left = MERMAID_TIMEOUT_MS - (Date.now() - start) - 250;
        const alt = left > 0 ? await withTimeout(render(flow[other === "TD" ? "td" : "lr"]), left).catch(() => null) : null;
        const a = svgViewBoxSize(svg), b = alt ? svgViewBoxSize(alt) : null;
        if (alt && a && b) {
          const [td, lr] = first === "TD" ? [a.w, b.w] : [b.w, a.w];
          if (pickOrientation(flow.stated, td, lr) === other) svg = alt;
        }
      }
      return { ok: true, url: svgDataUrl(font ? embedSvgFont(svg, font) : svg), size: svgViewBoxSize(svg) };
    })(), MERMAID_TIMEOUT_MS);
  } catch (e) {
    return { ok: false, kind: classifyMermaidError(phase, e), reason: mermaidErrorReason(e) };
  } finally {
    for (const id of ids) document.getElementById(`d${id}`)?.remove(); // mermaid leaves its scratch node behind on render errors
  }
}
