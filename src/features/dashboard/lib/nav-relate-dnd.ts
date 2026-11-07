// SPDX-License-Identifier: MIT
/**
 * SLW-FX-21: the pure predicates behind nav drag-to-link. Kept out of the
 * (large, JSX-heavy) `pipeline-status-nav.tsx` so the drag-source / drop-target /
 * validity rules are unit-testable in isolation and shared by the one `NavRowDnd`
 * wrapper. No React, no I/O — every check is an O(1) string/enum compare, so the
 * hot `dragover` path stays cheap.
 */

import { resolveFileScope } from "./file-scope";
import { itemFilePath, type FlatNavItem } from "./flatten-nav-tree";

/** The private drag payload MIME — a filepath the nav itself produced, so a
 *  foreign OS drag (files, text) is ignored. */
export const CS_FILEPATH_MIME = "application/x-cs-filepath";

/** SW-XS-20: a doc row inherited from a parent team. It is not dragged: a drop
 *  on a folder would move the parent's file into a folder the parent may not
 *  have. (It can still be edited, renamed, moved from the menu, or related to.) */
export function isInheritedRow(item: FlatNavItem, activeTeam: string | null): boolean {
  return item.type === "doc" && !!resolveFileScope(item.sourceTeam, null, activeTeam)?.isInherited;
}

/**
 * The path a row can be dragged FROM — any leaf file except one inherited from a
 * parent team (see {@link isInheritedRow}). A browse-only source doc can still be
 * dragged: only the DROP TARGET must be writable. This is the row's own file
 * path (folders / pinned rows have none), via {@link itemFilePath}.
 */
export function dragSourcePath(item: FlatNavItem, activeTeam: string | null = null): string | null {
  return isInheritedRow(item, activeTeam) ? null : itemFilePath(item);
}

/**
 * The path a row can be a drop TARGET for — a node that owns a `supporting_docs`
 * list in its own frontmatter: a story, or a NON-installed doc / main-doc. An
 * installed (browse-only source) doc, folders, generic files, and pinned rows
 * are never targets (dropping onto them would have nowhere to write).
 */
export function relatableTargetPath(item: FlatNavItem): string | null {
  switch (item.type) {
    case "story":
      return item.story.filePath;
    case "doc":
    case "main-doc":
      return item.installed ? null : item.filePath;
    default:
      return null;
  }
}

/** `path` is under `rootDir` (or equals it). Pure prefix compare. */
export function underRoot(path: string, rootDir: string): boolean {
  return rootDir.length > 0 && (path === rootDir || path.startsWith(`${rootDir}/`));
}

/**
 * A drop is valid when there is a dragged path + a target, they differ (no
 * self-drop), and BOTH live under the same team root — the last check blocks a
 * cross-root relation (e.g. a source-repo doc dragged onto a team doc, which
 * would write a nonsensical `../../other-repo/x` ref). O(1) string checks only,
 * safe to call on every `dragover`.
 */
export function isValidRelateDrop(
  draggedPath: string | null | undefined,
  targetPath: string | null,
  rootDir: string,
): boolean {
  if (!draggedPath || !targetPath) return false;
  if (draggedPath === targetPath) return false;
  return underRoot(draggedPath, rootDir) && underRoot(targetPath, rootDir);
}
