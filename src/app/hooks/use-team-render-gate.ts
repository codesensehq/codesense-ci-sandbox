// SPDX-License-Identifier: MIT
import { useRetiringFrames } from "@/features/dashboard/components/use-retiring-frames";
import { useMountedScopes, warmScopes } from "@/shared/lib/warm-scopes";
import { useEffect, useRef } from "react";

export interface UseTeamRenderGateInput {
  /** All teams the user has opened this session (the mount candidates). */
  openedTeams: readonly string[];
  /** Teams with unsaved (dirty) editor buffers: pinned warm, since the live buffer only exists in
   *  the mounted subtree. */
  dirtyTeamKeys: ReadonlySet<string>;
}

/**
 * The render gate. Returns the set of team keys whose `<TeamWorkspaceView>` should be mounted: the
 * warm set the owner keeps (the active team plus the most recent others, across orgs), intersected
 * with `openedTeams` so a closed team is never resurrected. A team that stops being active stays
 * mounted, hidden by its wrapper, until enough other teams have been visited to push it out, so
 * ping-pong switching never tears down and rebuilds a team tree.
 *
 * Terminals are not the gate's concern: a terminal's display belongs to the terminal manager.
 */
export function useTeamRenderGate(input: UseTeamRenderGateInput): Set<string> {
  const { openedTeams, dirtyTeamKeys } = input;

  // One pin per dirty team, reconciled against the set: a team that stays dirty keeps its pin
  // (never released and retaken, which would let it age out in between).
  const pins = useRef(new Map<string, () => void>());
  useEffect(() => {
    for (const tk of dirtyTeamKeys) {
      if (!pins.current.has(tk)) pins.current.set(tk, warmScopes.pin(tk, "unsaved-edits"));
    }
    for (const [tk, release] of [...pins.current]) {
      if (dirtyTeamKeys.has(tk)) continue;
      pins.current.delete(tk);
      release();
    }
  }, [dirtyTeamKeys]);
  useEffect(() => {
    const held = pins.current;
    return () => {
      for (const release of held.values()) release();
      held.clear();
    };
  }, []);

  return new Set(
    useRetiringFrames(
      useMountedScopes(openedTeams),
      (key) => key,
      (f, key) => f.teamKey === key,
    ),
  );
}
