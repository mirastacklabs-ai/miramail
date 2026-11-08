// Which attachments get an inline preview or an "Open" button. Mirrors the allow-list in organise.rs (`openable`).
const OPEN_OK = new Set(["pdf", "png", "jpg", "jpeg", "gif", "webp", "heic", "tiff", "bmp", "txt", "csv", "rtf", "md", "docx", "xlsx", "pptx", "pages", "numbers", "key", "ics", "vcf"]);
const IMG_MIME: Record<string, string> = { "image/png": "image/png", "image/jpeg": "image/jpeg", "image/jpg": "image/jpeg", "image/gif": "image/gif", "image/webp": "image/webp" };
export const MAX_PREVIEW_IMAGE = 10 * 1024 * 1024;
export const MAX_PREVIEW_PDF = 25 * 1024 * 1024;

/** Types the system's default app may open directly. Anything that can run code is not listed: save those instead. */
export function openable(name: string): boolean {
  const i = name.lastIndexOf(".");
  return i > 0 && OPEN_OK.has(name.slice(i + 1).toLowerCase());
}

/** Inline preview kind for a declared mime type and size. SVG and anything unlisted get none. */
export function previewKind(mime: string, size: number): { kind: "image"; mime: string } | { kind: "pdf" } | null {
  const m = mime.toLowerCase().trim();
  if (IMG_MIME[m] && size <= MAX_PREVIEW_IMAGE) return { kind: "image", mime: IMG_MIME[m] };
  if (m === "application/pdf" && size <= MAX_PREVIEW_PDF) return { kind: "pdf" };
  return null;
}
