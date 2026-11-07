// SPDX-License-Identifier: MIT
import { dirname } from "@/shared/lib/basename";
import { dropPositionInRect, type DragPosition } from "@/shared/lib/drag-drop-hit-test";
import type { FlatNavItem } from "./flatten-nav-tree";

// ---------------------------------------------------------------------------
// SW-VE-7 — pure targeting for drag-drop file ingest. The nav marks each
// directory row with the scope a dropped file would land in; the OS-drop hook
// (Tauri `onDragDropEvent`) and the internal-drag move both resolve their target
// through these helpers. Kept pure (no DOM, no React) so the hit-test is
// unit-testable — mirrors the `file-scope.ts` / `structural-ops.ts` split.
// ---------------------------------------------------------------------------

/** The absolute path of a nav row that is a writable DIRECTORY drop target
 *  (folder-header / generic-dir), or `null` for a file/story/placeholder row or
 *  a browse-only `installed://` source tree. Directory rows are the drop
 *  targets; a drop over any other row (or empty space) routes to the nav root
 *  zone. Used both to ring the destination directory row and to gate the
 *  internal-drag move target. */
export function navDropDirPath(item: FlatNavItem): string | null {
  if (item.type === "folder-header") {
    const p = item.node.absoluteDirPath;
    return p.startsWith("installed://") ? null : p;
  }
  if (item.type === "generic-dir") {
    const p = item.entry.path;
    return p.startsWith("installed://") ? null : p;
  }
  return null;
}

/** True when an INTERNAL file drag (`draggedPath`, or `null` for an OS drag —
 *  which never sets the nav drag ref) is a valid MOVE into directory `toDir`: a
 *  real path that isn't `toDir` itself and isn't already directly inside it. The
 *  guard behind the internal-drag move drop. Pure. */
export function isInternalMoveToDir(draggedPath: string | null, toDir: string | null): boolean {
  return !!toDir && !!draggedPath && draggedPath !== toDir && dirname(draggedPath) !== toDir;
}

/** A drop candidate row: the directory a drop on `rect` targets. */
export type DropCandidate = { dir: string; rect: DOMRect };

/**
 * Resolve a drop position to the directory whose row rect contains it. Nav rows
 * are flat, full-width, non-overlapping siblings, so at most one contains the
 * y — first hit wins. Returns `null` when the position is over no directory row
 * (a file row / empty nav space → the caller routes to the root zone). Half-open
 * edges via {@link dropPositionInRect}. Pure (rects are passed in).
 */
export function pickDropDir(position: DragPosition, candidates: DropCandidate[]): string | null {
  for (const c of candidates) {
    if (dropPositionInRect(position, c.rect)) return c.dir;
  }
  return null;
}
