// SPDX-License-Identifier: MIT
import type { Workspace } from "@/shared/stores/viewer-store";
import { getKindCapabilities } from "@/shared/lib/workspace-kind-capabilities";

/**
 * The "open this workspace's primary doc" gesture, shared by every surface that
 * shows a workspace title: the console-bar `WorkspacePill` and the split-tile
 * `TerminalTileChrome` (SW-EW-12 AC-11). Defining the guard + the open call once
 * keeps them from drifting. Event semantics stay at the call site — the pill
 * gates on `e.detail === 2` inside its `onClick`; the tile uses a real
 * `onDoubleClick` (no detail gate needed).
 *
 * SW-EW-35: extended from story-only to cover EPIC (integration) workspaces too,
 * so double-click opens an epic's doc (`epicDocPath`, the README) exactly the
 * way it opens a story's (`storyFilePath`) — no UX divergence between the two
 * workspace kinds. The same `onOpenStoryDoc` handler (`openFile`) works
 * for either path: it opens the file in the viewer.
 */
export type WorkspaceDocTarget = Pick<Workspace, "kind" | "storyFilePath" | "epicDocPath">;

/** The doc a workspace's title-double-click opens: a story file for a story
 *  tile, the epic README for an epic (integration) tile. `undefined` for a root
 *  (or a record missing its path) — those have no doc to open. */
export function workspaceDocPath(ws: WorkspaceDocTarget): string | undefined {
  const src = getKindCapabilities(ws.kind).docPathSource;
  if (src === "epicDocPath") return ws.epicDocPath;
  if (src === "storyFilePath") return ws.storyFilePath;
  return undefined;
}

/** True when this workspace can open a doc with the given handler. */
export function canOpenWorkspaceDoc(
  ws: WorkspaceDocTarget,
  onOpenStoryDoc: ((filePath: string) => void) | undefined,
): boolean {
  return !!workspaceDocPath(ws) && !!onOpenStoryDoc;
}

/**
 * Open the workspace's doc (via `openFile`) when it's openable. Returns `true` if it fired, so
 * callers can branch (the pill suppresses select; the tile stops the dblclick
 * from also toggling zoom).
 */
export function openWorkspaceDoc(
  ws: WorkspaceDocTarget,
  onOpenStoryDoc: ((filePath: string) => void) | undefined,
): boolean {
  const docPath = workspaceDocPath(ws);
  if (docPath && onOpenStoryDoc) {
    onOpenStoryDoc(docPath);
    return true;
  }
  return false;
}
