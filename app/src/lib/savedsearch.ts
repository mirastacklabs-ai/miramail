// Saved searches (sidebar). Pure parse/serialize helpers + localStorage wrapper; nothing here touches the network.
import type { SearchScope } from "./orgtypes";

export interface Saved { id: string; name: string; query: string; scope: SearchScope }
export const SAVED_KEY = "miramail.savedsearches.v1";
export const MAX_SAVED = 50;
const SCOPES: SearchScope[] = ["folder", "account", "all"];

export function parseSaved(raw: string | null | undefined): Saved[] {
  if (!raw) return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    const out: Saved[] = [];
    for (const x of v) {
      if (!x || typeof x !== "object") continue;
      const { id, name, query, scope } = x as Record<string, unknown>;
      if (typeof id !== "string" || typeof name !== "string" || typeof query !== "string" || !name.trim() || !query.trim()) continue;
      out.push({ id: id.slice(0, 40), name: name.trim().slice(0, 60), query: query.slice(0, 500), scope: SCOPES.includes(scope as SearchScope) ? (scope as SearchScope) : "all" });
      if (out.length >= MAX_SAVED) break;
    }
    return out;
  } catch { return []; }
}

export const serializeSaved = (l: Saved[]): string => JSON.stringify(l.slice(0, MAX_SAVED));

/** Add (or replace the one with the same name). Returns the new list; empty name/query returns the list unchanged. */
export function addSaved(list: Saved[], name: string, query: string, scope: SearchScope, id = Math.random().toString(36).slice(2, 10)): Saved[] {
  const n = name.trim().slice(0, 60), q = query.trim().slice(0, 500);
  if (!n || !q) return list;
  const rest = list.filter((s) => s.name.toLowerCase() !== n.toLowerCase());
  return [...rest, { id, name: n, query: q, scope }].slice(-MAX_SAVED);
}

export const removeSaved = (list: Saved[], id: string): Saved[] => list.filter((s) => s.id !== id);

export function loadSaved(): Saved[] { try { return parseSaved(localStorage.getItem(SAVED_KEY)); } catch { return []; } }
export function storeSaved(l: Saved[]) { try { localStorage.setItem(SAVED_KEY, serializeSaved(l)); } catch { /* session only */ } }

/** Suggested name for a query: the query itself, trimmed. */
export const defaultName = (query: string): string => query.trim().replace(/\s+/g, " ").slice(0, 40);
