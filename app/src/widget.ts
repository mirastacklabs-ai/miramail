import "./app.css";
import { mount } from "svelte";
import Widget from "./Widget.svelte";

// Same theme choice as the main window (shared localStorage); "system" follows the OS.
try {
  const t = localStorage.getItem("miramail.theme");
  if (t === "dark" || t === "light" || t === "contrast") document.documentElement.setAttribute("data-theme", t);
} catch { /* storage unavailable: follow the system theme */ }

export default mount(Widget, { target: document.getElementById("app")! });
