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
export type MermaidResult = { ok: true; url: string } | { ok: false; kind: MermaidFailKind; reason: string };

/** A finished (closed-fence) mermaid code block. Open fences stay plain code while the reply streams. */
export function isDrawableMermaid(b: { t: string; lang?: string; closed?: boolean }): boolean {
  return b.t === "code" && (b.lang ?? "").toLowerCase() === "mermaid" && b.closed === true;
}

export function mermaidTooLarge(src: string): boolean { return src.length > MAX_MERMAID_SRC; }

/** Drop frontmatter, %%{init}%% directives and click/call/href interaction lines: the app, not the model, decides config. */
export function cleanMermaidSource(src: string): string {
  let s = src.replace(/\r\n?/g, "\n");
  s = s.replace(/^\s*---\n[\s\S]*?\n---\s*(\n|$)/, "");
  s = s.replace(/%%\{[\s\S]*?\}%%/g, "");
  s = s.split("\n").filter((l) => !/^\s*(click|call|href|callback|link)\s/i.test(l)).join("\n");
  // Models often write a dotted link with text as `A -->.text.--> B`; the valid form is `A -. text .-> B`.
  s = s.replace(/(?:-->|--)[ \t]*\.[ \t]*([^.\n|<>]+?)[ \t]*\.[ \t]*-->/g, "-. $1 .->");
  // `flowchart-elk` forces the elk layout, which is not bundled; draw it with the default layout instead.
  // Top-down fits the narrow chat column; a left-right / right-left flowchart is drawn top-down.
  return s.trim().replace(/^flowchart-elk\b/i, "flowchart").replace(/^(graph|flowchart)([ \t]+)(LR|RL)\b/i, "$1$2TD");
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

/** Give the root <svg> concrete width/height from its viewBox (an <img> has no container to resolve width="100%"). */
export function sizeSvg(svg: string): string {
  const m = /<svg\b[^>]*>/.exec(svg);
  if (!m) return svg;
  const vb = /viewBox="\s*[-\d.]+[ ,]+[-\d.]+[ ,]+([\d.]+)[ ,]+([\d.]+)\s*"/.exec(m[0]);
  if (!vb) return svg;
  let tag = m[0].replace(/\s(width|height)="[^"]*"/g, "").replace(/\sstyle="[^"]*"/, "");
  if (!/xmlns=/.test(tag)) tag = tag.replace("<svg", '<svg xmlns="http://www.w3.org/2000/svg"');
  tag = tag.replace("<svg", `<svg width="${vb[1]}" height="${vb[2]}"`);
  return svg.replace(m[0], tag);
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
    // The diagram is shown as an <img>, which cannot load the bundled Oswald; use its closest local match.
    fontFamily: '"Arial Narrow", "Helvetica Neue", -apple-system, sans-serif', fontSize: "18px",
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
