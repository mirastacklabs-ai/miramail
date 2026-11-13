// Name of the device in user-facing text ("removed from this iPhone"). Mac wording is the default.
// iPadOS reports a Mac user agent; real Macs report no touch points, so touch support marks the iPad.
import { isMobileOs } from "./phonemodel.ts";

export const thisDevice = (
  ua: string = typeof navigator === "undefined" ? "" : navigator.userAgent,
  maxTouchPoints: number = typeof navigator === "undefined" ? 0 : navigator.maxTouchPoints,
): string => (/iPhone|iPod/.test(ua) ? "this iPhone" : /iPad/.test(ua) || (/Macintosh/.test(ua) && maxTouchPoints > 1) ? "this iPad" : isMobileOs(ua, maxTouchPoints) ? "this device" : "this Mac");
