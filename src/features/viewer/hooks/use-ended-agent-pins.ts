// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useMemo } from "react";
import {
  useViewerStore,
  selectHiddenTerminalTabIndex,
  selectRunningTerminalTabIndex,
  selectMinimizedTerminalTabIndex,
  findTabLocationBySessionId,
} from "@/shared/stores/viewer-store";
import { useAgentListProjectionStore, useAgentListTrees } from "../lib/agent-list-projection";
import { listedAgentsOf, endedSessionPins } from "../lib/agent-list-view";
import { agentRowKey, type AgentPin } from "@/shared/lib/agent-pins";
import { useRemoteLayoutsLoading } from "./use-agent-layout-loading";

/** A single app-root owner prunes pins only after every placement source is complete. */
export function useEndedAgentPins(): void {
  const { sessions, references, partial, error } = useAgentListProjectionStore();
  const trees = useAgentListTrees();
  const paneTeams = useMemo(
    () => [...trees].map(([teamKey, nodes]) => ({ teamKey, nodes })),
    [trees],
  );
  const pins = useViewerStore((s) => s.agentPins);
  const pinsWithTeam = useMemo(
    () =>
      Object.entries(pins ?? {}).flatMap(([teamKey, list]) =>
        list.map((pin) => ({ teamKey, pin })),
      ),
    [pins],
  );
  const unpinAgents = useViewerStore((s) => s.unpinAgents);
  const tabIndex = useViewerStore(selectRunningTerminalTabIndex);
  const minimizedTabs = useViewerStore(selectMinimizedTerminalTabIndex);
  const allHiddenTabs = useViewerStore(selectHiddenTerminalTabIndex);
  const tabsLoaded = useViewerStore(
    (s) => s.terminalLayout.hydrated === true && s.persistedStateRestored === true,
  );
  const { loading: remoteLayoutsLoading } = useRemoteLayoutsLoading();
  const rosterLoaded = sessions !== null && error === null && !partial;
  const placementOf = useCallback(
    (row: NonNullable<typeof sessions>[number]) =>
      references.find((r) => r.sessionRef === (row.id ?? row.sessionId ?? row.handle))
        ?.placementTeamRef ?? null,
    [references],
  );
  // A pinned session goes when it ends: no listed row runs it, no live roster row in any team
  // runs its session or handle, and no tab holds it. Read only on a full picture: the roster has
  // loaded, and so have the tabs and the workspaces the trees place rows by, and every remote
  // Machine's layout (a session there has its tab only once it is read); before that a running
  // session can be missing from the list, and its pin would go.
  useEffect(() => {
    if (!rosterLoaded || !tabsLoaded || remoteLayoutsLoading || pinsWithTeam.length === 0) return;
    const hasTab = (sid: string) =>
      Boolean(
        findTabLocationBySessionId(tabIndex, sid) ??
        findTabLocationBySessionId(minimizedTabs, sid) ??
        findTabLocationBySessionId(allHiddenTabs, sid),
      );
    const byTeam = new Map<string, AgentPin[]>();
    for (const { teamKey: tk, pin } of pinsWithTeam) {
      byTeam.set(tk, [...(byTeam.get(tk) ?? []), pin]);
    }
    const listed = listedAgentsOf(paneTeams);
    const live = (sessions ?? []).filter((r) => r.alive);
    // By team and handle: a twin handle that runs placed in another team (by its tab or story)
    // does not keep this team's pin. A live row nothing places yet could be any team's, so its
    // handle keeps a pin of that handle in every team.
    const liveKeys = new Set<string>();
    const unplacedHandles = new Set<string>();
    for (const r of live) {
      const placed = placementOf(r);
      if (placed === null) unplacedHandles.add(r.handle);
      else liveKeys.add(agentRowKey(placed, r.handle));
    }
    const liveSessionIds = new Set(live.flatMap((r) => (r.sessionId ? [r.sessionId] : [])));
    for (const [tk, list] of byTeam) {
      const unlisted = list.filter(
        (pin) =>
          !liveKeys.has(agentRowKey(pin.teamKey, pin.handle)) &&
          !unplacedHandles.has(pin.handle) &&
          !(pin.sessionId && liveSessionIds.has(pin.sessionId)),
      );
      const ended = endedSessionPins(unlisted, listed, hasTab);
      if (ended.length > 0) unpinAgents(tk, ended);
    }
  }, [
    rosterLoaded,
    tabsLoaded,
    remoteLayoutsLoading,
    sessions,
    placementOf,
    paneTeams,
    pinsWithTeam,
    tabIndex,
    minimizedTabs,
    allHiddenTabs,
    unpinAgents,
  ]);
}
