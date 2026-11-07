// SPDX-License-Identifier: MIT
/**
 * The one rule for what becomes of the agents pane's arrow filter (one workspace shown alone)
 * while the things that place its agents load: the agents pane decides with it what its empty
 * list says and when the filter goes, and the app drops a stale filter by it even while the pane
 * is not mounted.
 */

import { machineInTeams } from "@/features/machines/lib/machine-offline";
import { memberKey, type TerminalLayoutProjection } from "@/shared/lib/terminal-layout-projection";
import { NO_WORKSPACE_ID } from "@/shared/lib/workspace-key";
import { isRemoteWorkspace, type WorkspaceLocation } from "@/shared/lib/workspace-location";
import type { MachineSummary } from "@/tauri/commands";

/** What becomes of the filter now. */
export type ArrowFilterFate =
  /** It stays: its workspace holds agents, or what it holds can't be settled (the roster's
   *  last read missed a source, or this caller doesn't read agents). */
  | { fate: "keep" }
  /** Its workspace or its agents are still on their way: an empty list says it is loading. */
  | { fate: "loading" }
  /** Its workspace runs on a remote machine whose layout isn't read (the machine is offline,
   *  unreachable or not connected yet): it stays until the machine's layout comes. */
  | { fate: "waiting"; machineId: string }
  /** Nothing can bring it back: it goes. */
  | { fate: "drop" };

/** Where a load stands: done, still on its way, or stalled: past its wait, or its read failed
 *  or missed one of its sources. */
export type LoadPhase = "done" | "loading" | "stalled";

/** What {@link arrowFilterFate} reads. */
export interface ArrowFilterFacts {
  filter: { teamKey: string; workspaceId: string; machineId?: string };
  /** The filtered workspace's record (open or minimized), when it is here. */
  record: { location?: WorkspaceLocation } | undefined;
  /** The filter's team is hidden from the pane. */
  teamHidden: boolean;
  /** This Mac's saved workspaces and its own terminal layout. */
  local: LoadPhase;
  /** The roster of agents; null for a caller that doesn't read agents. */
  roster: LoadPhase | null;
  /** Some remote machine's layout is still on its way, within its bounded wait: its tabs may
   *  place agents in any workspace. */
  remoteLoading: boolean;
  /** Whether `machineId`'s own layout is still on its way, within its bounded wait. */
  machineLoading: (machineId: string) => boolean;
  /** Whether `machineId`'s layout is held, read live or as it last was. */
  layoutHeld: (machineId: string) => boolean;
  /** Whether `machineId` is known to be gone (removed, or no loaded machine list names it). */
  machineGone: (machineId: string) => boolean;
  /** Whether the filtered workspace holds an agent; null for a caller that doesn't read
   *  agents. */
  holdsAgent: boolean | null;
}

const KEEP: ArrowFilterFate = { fate: "keep" };
const LOADING: ArrowFilterFate = { fate: "loading" };
const DROP: ArrowFilterFate = { fate: "drop" };

/**
 * What becomes of the arrow filter. It goes when its team is hidden, when its workspace's record
 * is gone and nothing could bring it (a local one once This Mac's state is known, a remote one
 * once its machine's layout is held, or its machine is gone), and when its workspace holds no
 * agent on a roster read whole. It waits for a remote machine whose layout isn't read, however
 * long that machine stays away. A load that stalls never drops it, and never reads as loading.
 */
export function arrowFilterFate(f: ArrowFilterFacts): ArrowFilterFate {
  if (f.teamHidden) return DROP;
  if (f.local === "loading") return LOADING;
  if (f.local === "stalled") return KEEP;
  const bucket = f.filter.workspaceId === NO_WORKSPACE_ID;
  const machineId = bucket
    ? undefined
    : f.record
      ? isRemoteWorkspace(f.record)
        ? f.record.location.machineId
        : undefined
      : f.filter.machineId;
  if (machineId !== undefined && !f.layoutHeld(machineId)) {
    if (!f.machineGone(machineId)) {
      return f.machineLoading(machineId) ? LOADING : { fate: "waiting", machineId };
    }
    if (!f.record) return DROP;
  }
  if (!bucket && !f.record) return DROP;
  if (f.roster === null || f.holdsAgent === null) return KEEP;
  if (f.roster === "loading" || f.remoteLoading) return LOADING;
  if (f.roster === "stalled") return KEEP;
  return f.holdsAgent ? KEEP : DROP;
}

/** {@link ArrowFilterFacts}' machine facts for a filter of `teamKey`: a machine's layout is held
 *  once the projection has its remote member, and a machine is gone once its admission says it
 *  was removed, or the team's machine list has loaded and no list names it. */
export function machineFactsOf(
  layout: Pick<TerminalLayoutProjection, "members" | "admission">,
  machines: {
    byTeam: Readonly<Record<string, readonly MachineSummary[]>>;
    failedTeams: readonly string[];
  },
  teamKey: string,
): Pick<ArrowFilterFacts, "layoutHeld" | "machineGone"> {
  const keyOf = (machineId: string) => memberKey({ machineId, slot: "remote" });
  return {
    layoutHeld: (machineId) => layout.members[keyOf(machineId)] !== undefined,
    machineGone: (machineId) =>
      layout.admission[keyOf(machineId)]?.state === "removed" ||
      (machines.byTeam[teamKey] !== undefined &&
        !machines.failedTeams.includes(teamKey) &&
        machineInTeams(machines.byTeam, machineId) === undefined),
  };
}
