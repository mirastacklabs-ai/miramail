// Wire types for the organising commands (camelCase, mirrors app/src-tauri/src/organise.rs).
import type { Folder, FolderRole, MessageSummary } from "./types";

export type SearchScope = "folder" | "account" | "all";
export interface SearchResult { hits: MessageSummary[]; total: number; hints: string[] }
export interface ServerSearchResult { found: number; searched: number; skipped: number; notes: string[] }
export interface MoveItem { uid?: number | null; messageId?: string | null }
export interface Outcome { uid: number | null; ok: boolean; error: string | null; newUid: number | null; messageId: string | null }
export interface BulkResult { outcomes: Outcome[]; destFolder: string | null; destValidity: number | null }
export interface UnsubInfo { mailto: string | null; httpsHost: string | null; httpsUrl: string | null; oneClick: boolean }
export interface SourceView { headers: string; raw: string; size: number; truncated: boolean }
export interface ColourEntry { uid: number; colour: number }
export type { Folder, FolderRole };

/** Colour label names; index = value stored by the backend (0 = none). */
export const COLOURS = ["", "Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Grey"] as const;
export const COLOUR_CSS = ["transparent", "#e5484d", "#f08a24", "#e5c100", "#30a46c", "#3e63dd", "#8e4ec6", "#8b8d98"] as const;
