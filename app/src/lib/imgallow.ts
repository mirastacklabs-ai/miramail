// Per-sender "always load images" allow-list. Default stays blocked; the tracker warning and tracker skipping stay on.
export const ALLOW_KEY = "miramail.imgallow.v1";
export const MAX_ALLOWED = 2000;

/** Lower-cased bare address, or null if it does not look like one. */
export function normAddr(a: string | null | undefined): string | null {
  const s = (a ?? "").trim().toLowerCase();
  return s.length > 0 && s.length <= 254 && /^[^\s@<>,;"]+@[^\s@<>,;"]+\.[^\s@<>,;"]+$/.test(s) ? s : null;
}

export function parseAllow(raw: string | null | undefined): string[] {
  if (!raw) return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    const out = new Set<string>();
    for (const x of v) { const n = typeof x === "string" ? normAddr(x) : null; if (n) out.add(n); if (out.size >= MAX_ALLOWED) break; }
    return [...out];
  } catch { return []; }
}
export const isAllowed = (list: readonly string[], addr: string | null | undefined): boolean => { const n = normAddr(addr); return !!n && list.includes(n); };
export function addAllowed(list: readonly string[], addr: string | null | undefined): string[] {
  const n = normAddr(addr);
  if (!n || list.includes(n)) return [...list];
  return [...list, n].slice(-MAX_ALLOWED);
}
export function removeAllowed(list: readonly string[], addr: string | null | undefined): string[] {
  const n = normAddr(addr);
  return list.filter((x) => x !== n);
}
export function loadAllowed(): string[] { try { return parseAllow(localStorage.getItem(ALLOW_KEY)); } catch { return []; } }
export function storeAllowed(l: readonly string[]) { try { localStorage.setItem(ALLOW_KEY, JSON.stringify(l)); } catch { /* session only */ } }
