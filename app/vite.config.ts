import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import pkg from "./package.json";

export default defineConfig({
  plugins: [svelte()],
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  // elkjs (EPL-2.0) is swapped for a stub: mermaid only needs it for the optional elk layout (see src/lib/elkstub.ts).
  resolve: { alias: { "elkjs/lib/elk.bundled.js": decodeURIComponent(new URL("./src/lib/elkstub.ts", import.meta.url).pathname) } },
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  // Two entries: the app (index.html) and the tiny floating inbox-count widget (widget.html).
  build: { target: "safari15", outDir: "dist", emptyOutDir: true, rollupOptions: { input: { main: "index.html", widget: "widget.html" } } },
});
