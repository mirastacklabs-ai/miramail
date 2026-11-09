// Pure helpers for the sign-in form (components/Onboarding.svelte). No DOM, so they run under node --test.

/** Invisible characters that sneak in when an address is pasted from a web page or a PDF: soft hyphen, combining grapheme
 *  joiner, Arabic letter mark, Hangul/Khmer/Mongolian fillers and selectors, zero-width space/joiners, LRM/RLM, bidi
 *  embeddings/overrides/isolates, word joiner and invisible operators, deprecated format controls, the no-break space,
 *  Hangul filler variants, variation selectors (incl. the supplement), BOM and tag characters. */
const INVISIBLE = /[\u00AD\u00A0\u034F\u061C\u115F\u1160\u17B4\u17B5\u180B-\u180F\u200B-\u200F\u202A-\u202E\u2060-\u206F\u3164\uFE00-\uFE0F\uFEFF\uFFA0\u{E0000}-\u{E007F}\u{E0100}-\u{E01EF}]/gu;

/** The address exactly as it is sent to `add_account`: invisible characters removed, then surrounding whitespace. */
export function cleanAddress(raw: string): string {
  return raw.replace(INVISIBLE, "").trim();
}
