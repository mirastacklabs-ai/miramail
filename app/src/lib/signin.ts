// Pure helpers for the sign-in form (components/Onboarding.svelte). No DOM, so they run under node --test.

/** Invisible characters that sneak in when an address is pasted from a web page or a PDF: zero-width space/joiners,
 *  LRM/RLM, bidi embeddings/overrides, word joiner, BOM and the no-break space. */
const INVISIBLE = /[​-‏‪-‮⁠﻿ ]/g;

/** The address exactly as it is sent to `add_account`: invisible characters removed, then surrounding whitespace. */
export function cleanAddress(raw: string): string {
  return raw.replace(INVISIBLE, "").trim();
}
