// Pure helpers for the Reader's privacy UI. Detection is heuristic (static analysis in the backend):
// all wording says "look like" / "may" and never claims certainty.
import type { ImageInfo } from "./types";

export interface ImageSummary { total: number; trackers: number; suspicious: number; clean: number; services: string[] }

export function summarize(images: ImageInfo[]): ImageSummary {
  const s: ImageSummary = { total: images.length, trackers: 0, suspicious: 0, clean: 0, services: [] };
  for (const i of images) {
    if (i.verdict === "tracker") {
      s.trackers++;
      if (i.tracker && !s.services.includes(i.tracker)) s.services.push(i.tracker);
    } else if (i.verdict === "suspicious") s.suspicious++;
    else s.clean++;
  }
  return s;
}

export const plural = (n: number, one: string, many = one + "s") => `${n} ${n === 1 ? one : many}`;

export interface Warning { level: "tracker" | "suspicious"; headline: string; body: string }

/** The prominent warning shown before images are loaded; null when nothing looks suspicious. */
export function warningFor(s: ImageSummary): Warning | null {
  const flagged = s.trackers + s.suspicious;
  if (!flagged) return null;
  const leak = "Loading images tells the sender that you opened this email, when, roughly where you are (IP address) and what device you use.";
  if (s.trackers > 0) {
    const names = s.services.length ? ` (${s.services.slice(0, 4).join(", ")})` : "";
    const extra = s.suspicious ? ` ${plural(s.suspicious, "more image")} could be tracking too.` : "";
    return {
      level: "tracker",
      headline: "This message may be tracking you",
      body: `${leak} ${plural(s.trackers, "image")} look${s.trackers === 1 ? "s" : ""} like ${s.trackers === 1 ? "a tracking pixel" : "tracking pixels"}${names}.${extra} MiraMail blocked them.`,
    };
  }
  return {
    level: "suspicious",
    headline: "This message could be tracking you",
    body: `${leak} ${plural(s.suspicious, "image")} could be tracking (long personal-looking addresses). MiraMail has blocked all remote images for now.`,
  };
}

export function bannerText(blocked: number, s: ImageSummary): string {
  const base = `${plural(blocked, "remote image")} blocked`;
  return s.trackers > 0 ? `${base} — ${s.trackers} look${s.trackers === 1 ? "s" : ""} like ${s.trackers === 1 ? "a tracker" : "trackers"}` : base;
}

export type ImageStatus = "Not loaded" | "Loaded" | "Skipped (tracker)";
export function imageStatus(i: ImageInfo, loaded: boolean, skipTrackers: boolean): ImageStatus {
  if (!loaded) return "Not loaded";
  return skipTrackers && i.verdict === "tracker" ? "Skipped (tracker)" : "Loaded";
}

export const sizeLabel = (i: ImageInfo): string =>
  i.width == null && i.height == null ? "size unknown" : `${i.width ?? "?"}×${i.height ?? "?"}`;
