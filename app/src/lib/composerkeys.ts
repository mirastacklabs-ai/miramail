// Key handling for the MIRA message box. Pure: the component applies the result.
export type KeyLike = {
  key: string; shiftKey: boolean; altKey: boolean; metaKey: boolean; ctrlKey: boolean;
  isComposing?: boolean; keyCode?: number;
};
/** send: submit. newline: insert "\n" ourselves (Alt+Enter). default: let the textarea handle it (Shift+Enter). none: swallow the key. pass: not ours. */
export type KeyAction = "send" | "newline" | "default" | "none" | "pass";

export function composerKeyAction(e: KeyLike, canSend: boolean): KeyAction {
  if (e.key !== "Enter") return "pass";
  // IME: Enter confirms a candidate, never sends (keyCode 229 covers Safari firing keydown after compositionend).
  if (e.isComposing || e.keyCode === 229) return "pass";
  if (e.shiftKey) return "default";
  if (e.altKey) return "newline";
  return canSend ? "send" : "none"; // plain Enter or Cmd/Ctrl+Enter; empty text or streaming sends nothing
}

/** Insert "\n" at the selection; returns the new text and caret. */
export function insertNewline(text: string, start: number, end: number): { text: string; caret: number } {
  const a = Math.max(0, Math.min(start, end)), b = Math.min(text.length, Math.max(start, end));
  return { text: text.slice(0, a) + "\n" + text.slice(b), caret: a + 1 };
}
