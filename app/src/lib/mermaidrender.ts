// Lazy mermaid renderer. `mermaid` is imported only when a finished ```mermaid block is shown, so it lives in its own chunk.
// Resolves to an SVG data: URL or a short failure reason (never throws). Renders are serialised because mermaid config is global.
import {
  classifyMermaidError, cleanMermaidSource, MAX_MERMAID_SRC, MERMAID_LAYOUT, MERMAID_TIMEOUT_MS, mermaidErrorReason, mermaidTooLarge,
  sizeSvg, svgDataUrl, themeVariables, withTimeout, type MermaidResult, type ThemeColors,
} from "./mermaidsafe";

let chain: Promise<unknown> = Promise.resolve();
let seq = 0;

export function renderMermaid(src: string, colors: ThemeColors): Promise<MermaidResult> {
  if (mermaidTooLarge(src)) return Promise.resolve({ ok: false, kind: "syntax", reason: `The diagram is longer than ${MAX_MERMAID_SRC} characters.` });
  const job = chain.then(() => draw(src, colors), () => draw(src, colors));
  chain = job.catch(() => null);
  return job;
}

async function draw(src: string, colors: ThemeColors): Promise<MermaidResult> {
  let phase: "load" | "parse" | "render" = "load";
  try {
    const code = cleanMermaidSource(src);
    if (!code) return { ok: false, kind: "syntax", reason: "The diagram is empty." };
    return await withTimeout((async (): Promise<MermaidResult> => {
      const mermaid = (await import("mermaid")).default;
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
      const { svg } = await mermaid.render(`mmd-${++seq}`, code);
      return { ok: true, url: svgDataUrl(sizeSvg(svg)) };
    })(), MERMAID_TIMEOUT_MS);
  } catch (e) {
    return { ok: false, kind: classifyMermaidError(phase, e), reason: mermaidErrorReason(e) };
  } finally {
    document.getElementById(`dmmd-${seq}`)?.remove(); // mermaid leaves its scratch node behind on render errors
  }
}
