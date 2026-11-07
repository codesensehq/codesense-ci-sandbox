// SPDX-License-Identifier: MIT
import { isRemoteMachineId } from "@/features/machines/lib/machine-offline";
import type { TerminalLayoutProjection } from "@/shared/lib/terminal-layout-projection";
import { workspaceKey } from "@/shared/lib/workspace-key";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { normalizeWorkspaceDir, type Workspace } from "@/shared/stores/viewer-store";

/** The workspace slices {@link workspaceInDir} reads. */
export interface WorkspaceSlices {
  workspaces: Readonly<Record<string, readonly Workspace[]>>;
  softClosedWorkspaces: Readonly<
    Record<string, Readonly<Record<string, { workspace: Workspace }>>>
  >;
}

/**
 * The team's workspace in `dir` on This Mac, trailing slashes aside: an open one before one
 * minimized into the tray, or undefined. Among two in the same directory and the same place,
 * the first wins: the first open one in the team's workspace order, else the first one minimized
 * (in the order the tray record holds them). A workspace on a remote machine has no directory
 * here and never matches.
 */
export function workspaceInDir(
  s: WorkspaceSlices,
  teamKey: string,
  dir: string,
): Workspace | undefined {
  if (!dir) return undefined;
  const want = normalizeWorkspaceDir(dir);
  const inDir = (w: Workspace) =>
    !isRemoteWorkspace(w) && !!w.cwd && normalizeWorkspaceDir(w.cwd) === want;
  return (
    s.workspaces[teamKey]?.find(inDir) ??
    Object.values(s.softClosedWorkspaces[teamKey] ?? {})
      .map((sc) => sc.workspace)
      .find(inDir)
  );
}

/**
 * The directory of the host workspace a tab-index entry names, from its layout. The host keeps
 * tabs running under keys no workspace record may mint (a directory whose tile is not open, an
 * agent launched into a home of its own, a story not open here), and keeps them across an app
 * restart: the tab index then names that workspace by its key's form (a story's by its story
 * id), so `{ id: workspaceId }` mints the same key back. A workspace with a record is named by
 * the record's id instead, and is found through the record, not here.
 */
export function layoutWorkspaceDir(
  byKey: TerminalLayoutProjection["byKey"],
  teamKey: string,
  workspaceId: string,
): string | null {
  return byKey[workspaceKey(teamKey, { id: workspaceId })]?.descriptor.worktreePath ?? null;
}

/** A tab as the tab indexes place it: its team, its host workspace and where it runs. */
export interface TabInWorkspace {
  teamKey: string;
  workspaceId: string;
  tab: { origin?: { machineId: string } };
}

/** The directory on This Mac of the host workspace holding `loc`'s tab ({@link layoutWorkspaceDir}),
 *  or null: a tab on a remote machine runs in a directory on that machine. */
export function tabLayoutDir(
  byKey: TerminalLayoutProjection["byKey"],
  loc: TabInWorkspace,
): string | null {
  if (isRemoteMachineId(loc.tab.origin?.machineId)) return null;
  return layoutWorkspaceDir(byKey, loc.teamKey, loc.workspaceId);
}

/**
 * The team's workspace in the directory of the host workspace holding `loc`'s tab, or undefined.
 * The one way a tab in a host workspace no record keys is placed in a workspace: the agents pane
 * lists the agent under it, and Show brings its tab into it.
 */
export function tabDirWorkspace(
  s: WorkspaceSlices,
  byKey: TerminalLayoutProjection["byKey"],
  loc: TabInWorkspace,
): Workspace | undefined {
  const dir = tabLayoutDir(byKey, loc);
  return dir ? workspaceInDir(s, loc.teamKey, dir) : undefined;
}
