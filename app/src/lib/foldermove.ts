// Pure rules for moving a folder (and its subfolders) under another folder or to the top level.
// Mirrors `foldermgmt::moved` in crates/mailcore (the backend re-checks everything); here it only decides
// which destinations / drop targets to offer, so invalid ones are never shown.
import type { Folder } from "./types";

const delim = (f: Folder) => (f.delimiter ? f.delimiter : null);

/** INBOX-prefixed namespace (e.g. Dovecot `INBOX.`): every other folder starts with `INBOX<delim>`. */
export function personalPrefix(folders: Folder[]): string | null {
  const inbox = folders.find((f) => f.name.toUpperCase() === "INBOX");
  const d = inbox && delim(inbox);
  if (!inbox || !d) return null;
  const prefix = inbox.name + d;
  const others = folders.filter((f) => f.name.toUpperCase() !== "INBOX");
  return others.length > 0 && others.every((f) => f.name.startsWith(prefix)) ? prefix : null;
}

/** Can this folder be moved at all (not INBOX / a role folder, and the server has a hierarchy delimiter)? */
export function isMovable(folders: Folder[], f: Folder): boolean {
  if (f.role !== "other" || f.name.toUpperCase() === "INBOX") return false;
  const d = delim(f);
  if (!d) return false;
  return !folders.some((o) => o.name.startsWith(f.name + d) && (o.role !== "other" || o.name.toUpperCase() === "INBOX"));
}

/** New full name for `f` under `parent` (`null` = top level), or null when that move is not allowed. */
export function movedName(folders: Folder[], f: Folder, parent: string | null): string | null {
  if (!isMovable(folders, f)) return null;
  const d = delim(f)!;
  const leaf = f.name.includes(d) ? f.name.slice(f.name.lastIndexOf(d) + d.length) : f.name;
  let name: string;
  if (parent === null) {
    name = (personalPrefix(folders) ?? "") + leaf;
  } else {
    const pf = folders.find((x) => x.name === parent);
    const pd = pf && delim(pf);
    if (!pf || !pd) return null;
    if (parent === f.name || parent.startsWith(f.name + d)) return null;
    name = parent + pd + leaf;
  }
  if (name === f.name) return null;
  const lower = name.toLowerCase(), sub = (name + d).toLowerCase();
  if (folders.some((x) => x.name.toLowerCase() === lower || x.name.toLowerCase().startsWith(sub))) return null;
  return name;
}

export interface MoveTargets { top: boolean; folders: Folder[] }

/** Valid destinations for moving `f`: the top level, and every folder that is not itself, a descendant, the current parent, or a name clash. */
export function moveTargets(folders: Folder[], f: Folder): MoveTargets {
  return { top: movedName(folders, f, null) !== null, folders: folders.filter((x) => movedName(folders, f, x.name) !== null) };
}

/** May folder `f` be dropped on `target` (a folder name, or null for the top level)? */
export const canDropFolder = (folders: Folder[], f: Folder, target: string | null): boolean => movedName(folders, f, target) !== null;
