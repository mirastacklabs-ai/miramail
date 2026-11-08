// Pure layout math (no Svelte runes) so it can be unit-tested with node --test.
export const DEFAULTS = { sidebar: 232, list: 340 } as const;
export const LIMITS = { sidebar: [180, 420], list: [260, 700] } as const;
export const READER_MIN = 360;

export function clampWidth(which: "sidebar" | "list", w: number, other: number, viewport: number): number {
  const [lo, hi] = LIMITS[which];
  const room = Math.max(lo, viewport - other - READER_MIN - 2);
  return Math.round(Math.min(Math.max(w, lo), Math.min(hi, room)));
}

