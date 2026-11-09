// Pure helpers for drawing ```mermaid blocks from model output. The model text is UNTRUSTED:
// source is size-capped and stripped of config directives, and the SVG mermaid returns is only ever shown
// inside an <img data:> (no script, HTML or event handler can run there), never via {@html}.

export const MAX_MERMAID_SRC = 20_000;
export const MERMAID_TIMEOUT_MS = 5_000;
/** Layout engine for every diagram. mermaid 12's default config says "elk", which this build replaces with a
 *  stub that always rejects (src/lib/elkstub.ts), so without this every flowchart failed. */
export const MERMAID_LAYOUT = "dagre";

/** "syntax": the diagram source is the problem (MIRA can resend it). "app": loading or drawing failed in MiraMail. */
export type MermaidFailKind = "syntax" | "app";
/** `size`: the drawing's own size in CSS px (its viewBox), or null when the SVG has none. */
export type MermaidResult = { ok: true; url: string; size: { w: number; h: number } | null } | { ok: false; kind: MermaidFailKind; reason: string };

/** A finished (closed-fence) mermaid code block. Open fences stay plain code while the reply streams. */
export function isDrawableMermaid(b: { t: string; lang?: string; closed?: boolean }): boolean {
  return b.t === "code" && (b.lang ?? "").toLowerCase() === "mermaid" && b.closed === true;
}

export function mermaidTooLarge(src: string): boolean { return src.length > MAX_MERMAID_SRC; }

/** Drop frontmatter, %%{init}%% directives and click/call/href interaction lines: the app, not the model, decides config. */
function stripMermaidSource(src: string): string {
  let s = src.replace(/\r\n?/g, "\n");
  s = s.replace(/^\s*---\n[\s\S]*?\n---\s*(\n|$)/, "");
  s = s.replace(/%%\{[\s\S]*?\}%%/g, "");
  s = s.split("\n").filter((l) => !/^\s*(click|call|href|callback|link)\s/i.test(l)).join("\n");
  // Models often write a dotted link with text as `A -->.text.--> B`; the valid form is `A -. text .-> B`.
  s = s.replace(/(?:-->|--)[ \t]*\.[ \t]*([^.\n|<>]+?)[ \t]*\.[ \t]*-->/g, "-. $1 .->");
  // `flowchart-elk` forces the elk layout, which is not bundled; draw it with the default layout instead.
  return s.trim().replace(/^flowchart-elk\b/i, "flowchart");
}

/** The source as it is parsed. A left-right / right-left flowchart is written top-down here; the renderer then draws
 *  every flowchart both ways and keeps the narrower one (see flowchartOrientations / pickOrientation). */
export function cleanMermaidSource(src: string): string {
  return stripMermaidSource(src).replace(/^(graph|flowchart)([ \t]+)(LR|RL)\b/i, "$1$2TD");
}

export type FlowDir = "TD" | "LR";
/** For a flowchart/graph source: the cleaned source drawn top-down (`td`) and left-right (`lr`), and which of the two
 *  the source asked for (`stated`, null when it gives no direction). Any other diagram type: null. BT and RL are kept
 *  as the vertical / horizontal form when the source uses them; `direction` lines inside subgraphs are not touched. */
export function flowchartOrientations(src: string): { stated: FlowDir | null; td: string; lr: string } | null {
  const s = stripMermaidSource(src);
  const m = /^(graph|flowchart)(?:[ \t]+(TB|TD|BT|LR|RL))?(?=[\s;]|$)/i.exec(s);
  if (!m) return null;
  const d = m[2]?.toUpperCase();
  const rest = s.slice(m[0].length);
  const stated: FlowDir | null = !d ? null : d === "LR" || d === "RL" ? "LR" : "TD";
  return { stated, td: `${m[1]} ${d === "BT" ? "BT" : "TD"}${rest}`, lr: `${m[1]} ${d === "RL" ? "RL" : "LR"}${rest}` };
}

/** How much narrower the other orientation must be to override the direction the source states. */
export const ORIENT_MARGIN = 0.8;
/** Which drawing of a flowchart to keep. The chat column is narrow and scrolls vertically, so the narrower one wins
 *  (a tie goes to top-down). A direction the source states is kept unless the other drawing is at least 20% narrower
 *  (width < ORIENT_MARGIN x the stated one's width). */
export function pickOrientation(stated: FlowDir | null, tdWidth: number, lrWidth: number): FlowDir {
  if (stated === "TD") return lrWidth < tdWidth * ORIENT_MARGIN ? "LR" : "TD";
  if (stated === "LR") return tdWidth < lrWidth * ORIENT_MARGIN ? "TD" : "LR";
  return lrWidth < tdWidth ? "LR" : "TD";
}

const KINDS: [RegExp, string][] = [
  [/^(graph|flowchart)\b/i, "flowchart"], [/^sequenceDiagram\b/i, "sequence diagram"], [/^classDiagram/i, "class diagram"],
  [/^stateDiagram/i, "state diagram"], [/^erDiagram\b/i, "entity relationship diagram"], [/^gantt\b/i, "Gantt chart"],
  [/^pie\b/i, "pie chart"], [/^journey\b/i, "user journey"], [/^gitGraph\b/i, "git graph"], [/^mindmap\b/i, "mind map"],
  [/^timeline\b/i, "timeline"], [/^quadrantChart\b/i, "quadrant chart"],
];
/** Short accessible label: "Diagram: flowchart". Never includes raw model text beyond a recognised keyword. */
export function diagramAlt(src: string): string {
  const first = cleanMermaidSource(src).split("\n").find((l) => l.trim() && !l.trim().startsWith("%%"))?.trim() ?? "";
  for (const [re, name] of KINDS) if (re.test(first)) return `Diagram: ${name}`;
  return "Diagram";
}

const VIEWBOX = /viewBox="\s*[-\d.]+[ ,]+[-\d.]+[ ,]+([\d.]+)[ ,]+([\d.]+)\s*"/;
/** Width and height from the root <svg>'s viewBox, or null. */
export function svgViewBoxSize(svg: string): { w: number; h: number } | null {
  const m = /<svg\b[^>]*>/.exec(svg);
  const vb = m && VIEWBOX.exec(m[0]);
  return vb ? { w: Number(vb[1]), h: Number(vb[2]) } : null;
}

/** Inline style for the diagram <img>: its own size as explicit CSS px (content box, the CSS adds the padding), so the
 *  chat's A-/A+ zoom scales it the same way it scales text and borders, not through the SVG image's intrinsic size.
 *  The drawing is never multiplied by the chat scale here (the zoom already does that). "" = the image sizes itself. */
export function diagramImgStyle(size: { w: number; h: number } | null): string {
  if (!size || !Number.isFinite(size.w) || !Number.isFinite(size.h) || size.w <= 0 || size.h <= 0) return "";
  return `width:${size.w}px;height:${size.h}px`;
}

/** Give the root <svg> concrete width/height from its viewBox (an <img> has no container to resolve width="100%"). */
export function sizeSvg(svg: string): string {
  const m = /<svg\b[^>]*>/.exec(svg);
  if (!m) return svg;
  const vb = VIEWBOX.exec(m[0]);
  if (!vb) return svg;
  let tag = m[0].replace(/\s(width|height)="[^"]*"/g, "").replace(/\sstyle="[^"]*"/, "");
  if (!/xmlns=/.test(tag)) tag = tag.replace("<svg", '<svg xmlns="http://www.w3.org/2000/svg"');
  tag = tag.replace("<svg", `<svg width="${vb[1]}" height="${vb[2]}"`);
  return svg.replace(m[0], tag);
}

/** The UI font. The diagram is an <img>, which cannot load the page's fonts, so the woff2 is embedded in the SVG as a
 *  data: URI (embedSvgFont); the rest of the stack is the fallback if that ever fails. */
export const DIAGRAM_FONT = "Oswald";
export const DIAGRAM_FONT_STACK = `"${DIAGRAM_FONT}", "Arial Narrow", "Helvetica Neue", -apple-system, sans-serif`;
const WOFF2_DATA_URL = /^data:font\/woff2;base64,[A-Za-z0-9+/]+={0,2}$/;
/** Put an @font-face for DIAGRAM_FONT (a base64 woff2 data: URL) first inside the root <svg>. Anything that is not
 *  such a data: URL is ignored, so no other text can reach the <style>. */
export function embedSvgFont(svg: string, woff2DataUrl: string): string {
  if (!WOFF2_DATA_URL.test(woff2DataUrl)) return svg;
  const m = /<svg\b[^>]*>/.exec(svg);
  if (!m) return svg;
  const style = `<style>@font-face{font-family:"${DIAGRAM_FONT}";src:url(${woff2DataUrl}) format("woff2");font-weight:200 700;font-style:normal}</style>`;
  return svg.slice(0, m.index + m[0].length) + style + svg.slice(m.index + m[0].length);
}

/** data: URL for <img src>. Base64 of the UTF-8 bytes. */
export function svgDataUrl(svg: string): string {
  const bytes = new TextEncoder().encode(svg);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return "data:image/svg+xml;base64," + btoa(bin);
}

export type ThemeColors = { bg: string; text: string; line: string; accent: string; surface: string };
/** mermaid 'base' themeVariables from app colours. */
export function themeVariables(c: ThemeColors): Record<string, string> {
  return {
    background: c.bg, primaryColor: c.surface, primaryTextColor: c.text, primaryBorderColor: c.accent,
    secondaryColor: c.surface, tertiaryColor: c.bg, lineColor: c.line, textColor: c.text,
    mainBkg: c.surface, nodeBorder: c.accent, clusterBkg: c.bg, clusterBorder: c.line,
    edgeLabelBackground: c.bg, noteBkgColor: c.surface, noteTextColor: c.text, noteBorderColor: c.line,
    actorBkg: c.surface, actorBorder: c.accent, actorTextColor: c.text, actorLineColor: c.line, signalColor: c.line, signalTextColor: c.text,
    labelBoxBkgColor: c.surface, labelTextColor: c.text, loopTextColor: c.text, titleColor: c.text,
    fontFamily: DIAGRAM_FONT_STACK, fontSize: "18px",
  };
}

/** Race a promise against a timeout; the timer is always cleared. */
export function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  let t: ReturnType<typeof setTimeout>;
  const to = new Promise<never>((_, rej) => { t = setTimeout(() => rej(new Error(`timeout after ${ms / 1000} s`)), ms); });
  return Promise.race([p, to]).finally(() => clearTimeout(t));
}

const MAX_REASON = 160;
/** A short, single-line reason for the UI: the error's first line, with the places where mermaid quotes the
 *  diagram source removed, at most 160 characters. */
export function mermaidErrorReason(e: unknown): string {
  const msg = e instanceof Error ? e.message : typeof e === "string" ? e
    : e && typeof e === "object" && "message" in e ? String((e as { message: unknown }).message) : "";
  const lines = msg.split(/\r?\n/);
  let s = lines.find((l) => l.trim()) ?? "";
  // jison: "Parse error on line N:" / source excerpt / caret / "Expecting 'A', 'B', got 'C'" (grammar token names only).
  const expecting = /:\s*$/.test(s) ? lines.find((l) => /^Expecting /.test(l)) : undefined;
  if (expecting) s = `${s} ${expecting}`;
  s = s.replace(/[\u0000-\u001f\u007f]/g, " ");
  s = s.replace(/\s*for text:[\s\S]*$/i, ""); // UnknownDiagramError appends the source text
  s = s.replace(/->[\s\S]*?<-/g, "…").replace(/`[^`]*`/g, "…"); // lexer/parser errors quote the offending text
  s = s.replace(/\s+/g, " ").trim() || "Unknown error";
  return s.length > MAX_REASON ? s.slice(0, MAX_REASON - 1).trimEnd() + "…" : s;
}

const APP_FAILURE = /dynamically imported module|importing a module|module script|failed to fetch|load failed|timeout after|layout|getBBox/i;
/** Only a parse-step error that is not an environment failure counts as a problem with the diagram source. */
export function classifyMermaidError(phase: "load" | "parse" | "render", e: unknown): MermaidFailKind {
  if (phase !== "parse") return "app";
  if (e instanceof TypeError || e instanceof ReferenceError || e instanceof RangeError) return "app";
  return APP_FAILURE.test(e instanceof Error ? e.message : String(e)) ? "app" : "syntax";
}
