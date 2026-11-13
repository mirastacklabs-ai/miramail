// Name of the device in user-facing text ("removed from this iPhone"). Mac wording is the default.
// iPadOS reports a Mac user agent; real Macs report no touch points, so touch support marks the iPad.
import { isMobileOs } from "./phonemodel.ts";

export const thisDevice = (
  ua: string = typeof navigator === "undefined" ? "" : navigator.userAgent,
  maxTouchPoints: number = typeof navigator === "undefined" ? 0 : navigator.maxTouchPoints,
): string => (/iPhone|iPod/.test(ua) ? "this iPhone" : /iPad/.test(ua) || (/Macintosh/.test(ua) && maxTouchPoints > 1) ? "this iPad" : isMobileOs(ua, maxTouchPoints) ? "this device" : "this Mac");

// Wording helpers for text shown on both the Mac and the phone. Mac text stays exactly as before.
const deviceUa = (ua?: string, tp?: number) => thisDevice(ua, tp);
/** "Mac" | "iPhone" | "iPad" | "device": for "the Mac", "a new Mac", "Mac's". */
export const deviceWord = (ua?: string, tp?: number): string => deviceUa(ua, tp).replace(/^this /, "");
const onMac = (ua?: string, tp?: number) => deviceWord(ua, tp) === "Mac";
/** "macOS Keychain" on a Mac, plain "Keychain" on iOS. */
export const keychainName = (ua?: string, tp?: number): string => (onMac(ua, tp) ? "macOS Keychain" : "Keychain");
/** "macOS" on a Mac, "iOS" / "iPadOS" on the phone / tablet. */
export const osName = (ua?: string, tp?: number): string => { const w = deviceWord(ua, tp); return w === "Mac" ? "macOS" : w === "iPad" ? "iPadOS" : w === "iPhone" ? "iOS" : "your device"; };

/** Backend messages say "on this Mac" (they are Mac text there); name the actual device instead. Mac text is unchanged. */
export const deviceMsg = (msg: string, ua?: string, tp?: number): string => msg.replace(/\b(Saved|Deleted) on this Mac\b/g, (_, v) => `${v} on ${thisDevice(ua, tp)}`);
