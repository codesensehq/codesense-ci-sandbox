// SPDX-License-Identifier: MIT
import { useEffect, useMemo } from "react";
import {
  useViewerStore,
  selectHiddenTerminalTabIndex,
  selectRunningTerminalTabIndex,
  selectMinimizedTerminalTabIndex,
  findTabLocationBySessionId,
} from "@/shared/stores/viewer-store";
import { useAgentListProjectionStore, useAgentListTrees } from "../lib/agent-list-projection";
import { useTeamAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import {
  useRoleConfigsStore,
  reloadRoleConfigs,
  watchRoleConfigs,
} from "@/shared/hooks/use-role-configs";
import { rolePresentation } from "@/shared/lib/role-presentation";
import {
  launchedCountsOf,
  pinnedEntries,
  pinOfRow,
  routineKeyOf,
  routineLabel,
  agentMachineLabel,
  resolveAgentFace,
  resolveAgentRole,
} from "../lib/agent-list-view";
import { agentKeyOfRevision } from "../lib/agent-rows";
import { agentRowLabel, isHeadlessSession } from "../lib/agent-row-presentation";
import { faceTagsFor } from "./use-face-tags";
import { useRoleHoldings } from "./use-role-holdings";
import { useRoutinesStore, watchRoutineRows } from "@/features/routines/stores/routines-store";
import { queuedWakeFor } from "@/features/agents/lib/agent-routines";
import { BUILTIN_FACE_SOURCE } from "@/shared/lib/avatar-packs";
import { usePaneWorkspaceWait } from "./use-pane-workspace-wait";
import { useAgentListFacts } from "../lib/agent-list-facts";

import { useEndedAgentPins } from "./use-ended-agent-pins";
import { activityLine, searchTextOf, type SearchIdentity } from "../lib/agent-list-search";
import { flattenNodes } from "@/features/agents/lib/agent-list-layout";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import {
  frozenTabMachineIn,
  isRemoteMachineId,
  machineInTeams,
} from "@/features/machines/lib/machine-offline";
const EMPTY: never[] = [];

/** The HTML list reads the same identity helpers and role owner as the built-in list. */
export function useAgentListFactsMirror(): void {
  useEndedAgentPins();
  useEffect(watchRoleConfigs, []);
  useEffect(watchRoutineRows, []);
  const rows = useAgentListProjectionStore((s) => s.sessions) ?? EMPTY;
  const refs = useAgentListProjectionStore((s) => s.references);
  const teams = useMemo(() => [...new Set(refs.map((r) => r.teamKey))], [refs]);
  const avatarsOf = useTeamAvatarPacks(teams);
  const choices = useViewerStore((s) => s.agentIdentity);
  const hidden = useViewerStore(selectHiddenTerminalTabIndex);
  const running = useViewerStore(selectRunningTerminalTabIndex);
  const minimized = useViewerStore(selectMinimizedTerminalTabIndex);
  const routines = useRoutinesStore((s) => s.rows);
  const holds = useRoleHoldings(teams, true);
  const configs = useRoleConfigsStore((s) => s.byTeam);
  const workspace = usePaneWorkspaceWait();
  const trees = useAgentListTrees();
  const pins = useViewerStore((s) => s.agentPins);
  const machines = useMachinesStore((s) => s.byTeam);
  const progress = useViewerStore((s) => s.progressBySession);
  const paneTeams = useMemo(
    () => [...trees].map(([teamKey, nodes]) => ({ teamKey, nodes })),
    [trees],
  );
  const teamPins = useMemo(
    () =>
      Object.entries(pins ?? {}).flatMap(([teamKey, list]) =>
        list.map((pin) => ({ teamKey, pin })),
      ),
    [pins],
  );
  const nodes = useMemo(() => paneTeams.flatMap((t) => flattenNodes(t.nodes)), [paneTeams]);
  const pinnedRefs = pinnedEntries(teamPins, paneTeams).flatMap((entry) =>
    entry.nodes.map((n) => n.projectedId),
  );
  const searchIdentity: SearchIdentity = {
    launched: launchedCountsOf(paneTeams),
    choicesOf: (team) => choices[team] ?? {},
    homeTeamKey: teams[0] ?? "",
    machineOf: (row) => {
      const at = machineAt(row);
      const m = at && machineInTeams(machines, at.origin.machineId, at.teamKey);
      return agentMachineLabel(at?.origin.machineId, m ? [m] : null);
    },
    machineIdOf: (row) => machineAt(row)?.origin.machineId ?? "local",
    offlineMachineOf: (row) => {
      const at = machineAt(row);
      return frozenTabMachineIn(machines, at?.origin, at?.teamKey);
    },
    routineOf: (row) => {
      const key = routineKeyOf(row);
      return key ? routineLabel(key, routines) : null;
    },
    isHidden: (row) => !!row.sessionId && !!findTabLocationBySessionId(hidden, row.sessionId),
    activityOf: (row) => (row.sessionId ? activityLine(progress[row.sessionId]) : null),
  };
  function machineAt(row: (typeof rows)[number]) {
    const loc =
      row.sessionId &&
      (findTabLocationBySessionId(running, row.sessionId) ??
        findTabLocationBySessionId(minimized, row.sessionId) ??
        findTabLocationBySessionId(hidden, row.sessionId));
    if (loc) return loc.tab.origin ? { origin: loc.tab.origin, teamKey: loc.teamKey } : null;
    return isRemoteMachineId(row.machineId)
      ? { origin: { machineId: row.machineId }, teamKey: row.launchTeamKey ?? undefined }
      : null;
  }
  const stateLoaded = useViewerStore((s) => s.stateLoaded);
  const roleTeams = teams.join("\n");
  useEffect(() => {
    for (const team of roleTeams.split("\n").filter(Boolean)) void reloadRoleConfigs(team);
  }, [roleTeams]);
  const tags = useMemo(
    () =>
      new Map(
        teams.map((team) => [
          team,
          faceTagsFor(team, {
            sessions: rows,
            choices: choices[team] ?? {},
            faces: avatarsOf(team).faces,
            running,
            minimized,
          }),
        ]),
      ),
    [teams, rows, choices, avatarsOf, running, minimized],
  );
  // Pick only source facts: mirrored fields returning on sessions must not start a write loop.
  const facts = Object.fromEntries(
    rows.map((row) => {
      const ref = refs.find((r) => r.sessionRef === (row.id ?? row.sessionId ?? row.handle));
      const team = ref?.teamKey ?? "";
      const name = agentRowLabel(row);
      const choice = choices[team]?.[row.handle];
      const faces = avatarsOf(team).faces;
      const tabHidden = !!row.sessionId && !!findTabLocationBySessionId(hidden, row.sessionId);
      const due = queuedWakeFor(routines, row.sessionId, row.machineId ?? "local");
      const face = resolveAgentFace(row, { name, choice, faces });
      const piece = resolveAgentRole(row, {
        choice,
        launched: refs.filter((r) => r.parentId === ref?.id).length,
      });
      const pieceRef = `builtin-chess:${piece.piece === "manager" ? "queen" : piece.piece === "worker" ? "pawn" : "bishop"}`;
      return [
        row.id ?? row.sessionId ?? row.handle,
        {
          searchText: nodes
            .filter(
              (n) =>
                (n.row.id ?? n.row.sessionId ?? n.row.handle) ===
                (row.id ?? row.sessionId ?? row.handle),
            )
            .map((n) => searchTextOf(n, searchIdentity))
            .join(" "),
          pinned: !!pinOfRow(teamPins, team, row),
          tabHidden,
          headless: isHeadlessSession(row),
          dueWake: due ? { name: due.def.name, deadlineMs: due.queued?.deadlineMs } : null,
          face: face.kind === "image" ? { ...face, src: paneImage(face.ref, face.src) } : face,
          faceTag: tags.get(team)?.get(row.handle) ?? null,
          rolePiece: {
            ...piece,
            color: "neutral",
            src: paneImage(pieceRef, BUILTIN_FACE_SOURCE.resolve(pieceRef)!),
          },
          roleHoldings: holds.holdsOf(team, row).map((h) => {
            const role = rolePresentation(h.role, configs[team] ?? [], faces);
            return { ...h, ...role, src: paneImage(role.ref, role.src) };
          }),
          agentKey: agentKeyOfRevision(row.agentRevision),
        },
      ];
    }),
  );
  const signature = JSON.stringify({ sessions: facts, pane: { ...workspace, pinnedRefs } });
  useEffect(() => {
    if (stateLoaded && signature !== JSON.stringify(useAgentListFacts.getState()))
      useAgentListFacts.setState(JSON.parse(signature));
  }, [signature, stateLoaded]);
}

/** Bundled image URLs belong to the app origin; panes read the kit's matching asset route. */
function paneImage(ref: string, src: string): string {
  const split = ref.indexOf(":");
  return ref.startsWith("builtin-") && split > 0
    ? `/kit/v1/avatars/${ref.slice(0, split)}/${ref.slice(split + 1)}.svg`
    : src;
}
