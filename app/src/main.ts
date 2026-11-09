import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";
import PaneWindow from "./PaneWindow.svelte";
import { prefs } from "./lib/prefs.svelte";
import { app } from "./lib/state.svelte";
import { parseEntry } from "./lib/detach";
import { installErrorSurface } from "./lib/errors";
import { onMailLink } from "./lib/api";
import { handleMailLink } from "./lib/links";

// Uncaught exceptions / rejections (including ones thrown while Svelte renders) show as an error toast.
installErrorSurface(window, (msg) => app.notify(msg, "error"));
prefs.init();
// Cmd+, opens Preferences (App.svelte's key handler ignores modified keys).
window.addEventListener("keydown", (e) => {
  if (e.metaKey && !e.altKey && !e.ctrlKey && e.key === ",") { e.preventDefault(); if (app.accounts.length) app.showSettings = true; }
});

// Link clicks in message frames arrive here (once per window, however many Readers are mounted).
void onMailLink(handleMailLink);

// `#pane=<id>` (or the id injected by the backend) = a detached window hosting a single pane; never the full app.
const entry = parseEntry(window.location.hash, (window as unknown as { __MIRAMAIL_PANE__?: unknown }).__MIRAMAIL_PANE__);
const target = document.getElementById("app")!;
export default entry.mode === "pane" ? mount(PaneWindow, { target, props: { id: entry.id } })
  : entry.mode === "invalid" ? (target.textContent = "Invalid window.", null)
  : mount(App, { target });
