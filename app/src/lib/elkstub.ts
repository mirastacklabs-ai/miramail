// Replaces elkjs (EPL-2.0, ~1.4 MB) in the bundle: mermaid only loads it for the optional "elk" layout,
// which MIRA diagrams do not use. mermaid 12 makes "elk" the default layout, so mermaidrender.ts must set
// layout: "dagre" (MERMAID_LAYOUT), or every flowchart lands here. If a diagram still reaches it, layout() rejects.
export default class ELK {
  layout(): Promise<never> { return Promise.reject(new Error("ELK layout is not included")); }
  knownLayoutAlgorithms(): Promise<never[]> { return Promise.resolve([]); }
  knownLayoutOptions(): Promise<never[]> { return Promise.resolve([]); }
  knownLayoutCategories(): Promise<never[]> { return Promise.resolve([]); }
  terminateWorker(): void {}
}
