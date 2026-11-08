# Frontend dependencies (all exact-pinned)

| Package | Why |
|---|---|
| `svelte` | UI runtime (Svelte 5, small compiled output). |
| `@tauri-apps/api` | `invoke` / `listen` bridge to the Rust backend. |
| `@tauri-apps/plugin-opener` | Open confirmed mail links in the default browser. |
| `vite` | Dev server + bundler. |
| `@sveltejs/vite-plugin-svelte` | Compiles `.svelte` files in Vite. |
| `typescript` | Type checking (5.x; svelte-check support). |
| `svelte-check` | `npm run check` for Svelte + TS diagnostics. |
| `@tauri-apps/cli` | `npm run tauri` (dev/build of the desktop shell). |

No UI framework, CSS framework, icon pack, or runtime network access. Fonts are bundled locally.

Calendar UI (v0.2): no new packages. Date/time logic uses `Intl` and `Date` only (`src/lib/caldate.ts`, tested with `node --test src/lib/caldate.test.ts`).
