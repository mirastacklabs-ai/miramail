// Name of the device in user-facing text ("removed from this iPhone"). Mac wording is the default.
export const thisDevice = (ua: string = typeof navigator === "undefined" ? "" : navigator.userAgent): string => (/iPhone|iPod/.test(ua) ? "this iPhone" : "this Mac");
