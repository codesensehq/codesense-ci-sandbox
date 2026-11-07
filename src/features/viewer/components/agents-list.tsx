// SPDX-License-Identifier: MIT
import { useRemoteLayoutsLoading } from "../hooks/use-agent-layout-loading";
import {
  useAgentListProjectionStore,
  useAgentListTrees,
} from "@/features/viewer/lib/agent-list-projection";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { flattenNodes, NO_WORKSPACE_ID } from "@/features/agents/lib/agent-list-layout";
import { openLaunchSheet, launchUnderAgent } from "@/features/agents/lib/launch-sheet";
import type { RemoteMachineIdentity } from "@/features/machines/components/machine-location-badge";
import { machineColor } from "@/features/machines/lib/machine-color";
import {
  frozenTabMachineIn,
  isRemoteMachineId,
  machineInTeams,
  machineOfflineWord,
  UNKNOWN_MACHINE_LABEL,
  type TabOrigin,
} from "@/features/machines/lib/machine-offline";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { useRoutinesStore } from "@/features/routines/stores/routines-store";
import { useAgentStop } from "@/features/viewer/components/agent-stop-dialog";
import {
  activityLine,
  AgentsTreeView,
  Identity,
  searchTextOf,
  type IdentityContext,
  type SearchIdentity,
  type WorkspaceWait,
} from "@/features/viewer/components/agents-tree-view";
import { RunsView, useRunHistory } from "@/features/viewer/components/runs-view";
import { useAgentDrill } from "@/features/viewer/hooks/use-agent-drill";
import { useAgentStatusOf } from "@/features/viewer/hooks/use-agent-status-of";
import { useCorrectedAgentRoster } from "@/features/viewer/hooks/use-corrected-agent-roster";
import { faceTagsFor } from "@/features/viewer/hooks/use-face-tags";
import { useNowClock } from "@/features/viewer/hooks/use-now-clock";
import { useRoleHoldings } from "@/features/viewer/hooks/use-role-holdings";
import {
  managerOptions,
  teamAgentIdentitiesOf,
} from "@/features/viewer/hooks/use-team-agent-identities";
import { useNeedsYou } from "@/features/viewer/hooks/use-team-agents";
import {
  crewsFor,
  memberMarksFor,
  qualifiersFor,
  type PillCrew,
} from "@/features/viewer/hooks/use-workspace-crews";
import {
  agentMachineLabel,
  countNodes,
  launchedCountsOf,
  needsYouKeys,
  otherTeamNodes,
  paneTeamLabel,
  pinForRow,
  pinnedEntries,
  pinOfRow,
  routineKeyOf,
  routineLabel,
  type AgentWorkspaceRef,
  type TeamPin,
} from "@/features/viewer/lib/agent-list-view";
import { makePaneWorkspaceSources } from "@/features/viewer/lib/agent-list-workspaces";
import {
  focusAgentsList,
  hideAgentsDock,
  takeAgentsFocus,
} from "@/features/viewer/lib/agents-list-focus";
import { planAgentsPane, type PaneTeam } from "@/features/viewer/lib/agents-pane-plan";
import {
  openPaneRunHistory,
  takePaneRunsRequest,
  usePaneRunsRequest,
} from "@/features/viewer/lib/agents-pane-runs";
import { focusedTeam, toggleTeamFocus } from "@/features/viewer/lib/agents-pane-team-focus";
import { useWorkspaceFilterFate } from "../hooks/use-pane-workspace-wait";
import {
  useMinimizedTerminalTabIndex,
  useRunningTerminalTabIndex,
  useTerminalTabIndex,
  useWorkspaceActiveTab,
} from "@/features/viewer/lib/cli-workspace/store/terminal-selectors";
import { setRunHistoryOpener } from "@/features/viewer/lib/show-agent";
import { hideTab } from "@/features/viewer/lib/terminal-close";
import { openAgentInboxForRow } from "@/features/viewer/lib/agent-inbox-actions";
import { StatusMark } from "@/shared/components/status-mark";
import { useAvatarPacks, useTeamAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import type { AgentIdentityChoice } from "@/shared/lib/agent-identity-choice";
import { agentPinId, agentRowKey, type AgentPin } from "@/shared/lib/agent-pins";
import type { AgentStatus } from "@/shared/lib/agent-status-config";
import { parseSyntheticRepoKey, workspaceReposForTeam } from "@/shared/lib/build-synthetic-repo";
import { markOfAgentStatus } from "@/shared/lib/status-mark";
import { computeTeamGlyphMap, NO_TEAM_SENTINEL } from "@/shared/lib/team-key";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { workspaceDisplayLabel } from "@/shared/lib/worktree";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  findTabLocationBySessionId,
  selectHiddenTerminalTabIndex,
  useViewerStore,
  type Workspace,
} from "@/shared/stores/viewer-store";
import type { AgentSessionRow } from "@/tauri/commands";
import { LOCAL_MACHINE_ID, type MachineSummary } from "@/tauri/commands";
import { History, PanelLeftClose, Plus } from "lucide-react";
import { useShallow } from "zustand/react/shallow";

const NO_PIN_MAP: Readonly<Record<string, readonly AgentPin[]>> = {};
const NO_CHOICES_MAP: Readonly<Record<string, Readonly<Record<string, AgentIdentityChoice>>>> = {};
const NO_CHOICES: Readonly<Record<string, AgentIdentityChoice>> = {};
const NO_FACE_TAGS: ReadonlyMap<string, number> = new Map();
const NO_CREWS: ReadonlyMap<string, PillCrew> = new Map();
const NO_OPEN_TEAMS: readonly string[] = [];
const NO_ACTIVITY: Readonly<Record<string, string | null>> = {};

/** A team's repo glyphs: only a team with two or more source repos marks its repos. */
function repoGlyphsForTeam(
  workspaceRepos: Parameters<typeof workspaceReposForTeam>[0],
  teamKey: string,
): ReturnType<typeof computeTeamGlyphMap> | null {
  const repos = workspaceReposForTeam(workspaceRepos, teamKey);
  const slugs: string[] = [];
  for (const r of repos) {
    const parsed = parseSyntheticRepoKey(r.repoPath);
    if (parsed) slugs.push(`${parsed.owner}/${parsed.repo}`);
  }
  // Counted by the repos that name a slug: one that doesn't parse has no glyph to tell apart.
  return slugs.length < 2 ? null : computeTeamGlyphMap(slugs);
}
/** How long after the list first paints a row counts as having arrived (and eases in). */
const SETTLE_MS = 800;

interface AgentsListProps {
  teamKey: string;
  /** False while the terminal column is collapsed: the list then renders nothing,
   *  so no hidden control takes focus and no clock ticks. */
  visible: boolean;
  /** Narrow layout, decided by the dock that holds the list (`DockSlotHost`). */
  narrow?: boolean;
}

/**
 * The left of the bottom CLI workspace: every visible team's live agents, as team ›
 * workspace › agent, with the header count, the find field and its filter menu, and `+`.
 * Selecting an agent shows its tab in the CLI and focuses its terminal; nothing here starts a
 * process. The dock that holds it (`DockSlotHost`) owns its fold, width and border.
 */
export function AgentsList({
  teamKey,
  visible,
  narrow = false,
}: AgentsListProps): React.JSX.Element | null {
  const collapsed = useViewerStore((s) => s.workspacePanelCollapsed);
  const hiddenTeams = useViewerStore((s) => s.agentsPaneHiddenTeams);
  const workspaceFilter = useViewerStore((s) => s.agentsWorkspaceFilter);
  const setWorkspaceFilter = useViewerStore((s) => s.setAgentsWorkspaceFilter);
  const setTeamHidden = useViewerStore((s) => s.setAgentsPaneTeamHidden);
  const teamFocusRestore = useViewerStore((s) => s.agentsPaneTeamFocusRestore);
  const setTeamScope = useViewerStore((s) => s.setAgentsPaneTeamScope);
  const workspaceReveal = useViewerStore((s) => s.agentsWorkspaceReveal);
  const clearWorkspaceReveal = useViewerStore((s) => s.clearAgentsWorkspaceReveal);
  // The list covers every team, so its search, Show filter, arrangement, arrow filter and folds are
  // the pane's own and survive a team switch: switching the team changes the pills and the CLI, not
  // the list.
  const filter = useViewerStore((s) => s.agentsPaneSearch);
  const setFilter = useViewerStore((s) => s.setAgentsPaneSearch);
  const arrowFilter = workspaceFilter;
  const arrange = useViewerStore((s) => s.agentsPaneArrange);
  const setArrange = useViewerStore((s) => s.setAgentsPaneArrange);
  const [railScope, setRailScope] = useState<
    import("../lib/agents-pane-plan").PaneRailFilter | null
  >(null);
  const railKeys = useViewerStore((s) => s.railKeyByWorkspaceId);
  const [arrangeAnchor, setArrangeAnchor] = useState<{
    key: string;
    within: number;
    offset: number;
  } | null>(null);
  const consumeArrangeAnchor = useCallback(() => setArrangeAnchor(null), []);
  const changeArrange = (next: typeof arrange) => {
    if (next === arrange) return;
    const scroll = navRef.current?.querySelector<HTMLElement>(".ag-scroll");
    if (scroll) {
      const box = scroll.getBoundingClientRect();
      const row = [...scroll.querySelectorAll<HTMLElement>("[data-row-kind='agent']")].find(
        (el) => el.getBoundingClientRect().bottom > box.top,
      );
      setArrangeAnchor(
        row
          ? {
              key: row.dataset.rowKey!,
              within: row.getBoundingClientRect().top - box.top,
              offset: scroll.scrollTop,
            }
          : { key: "", within: 0, offset: scroll.scrollTop },
      );
    }
    setArrange(next);
  };
  const workspaces = useViewerStore((s) => s.workspaces);
  const softClosedWorkspaces = useViewerStore((s) => s.softClosedWorkspaces);
  const allHiddenTabs = useViewerStore(selectHiddenTerminalTabIndex);
  // The "current" agent is the one the CLI shows: the active tab of the team's focused workspace.
  const focusedWorkspaceId = useViewerStore((s) => s.activeWorkspace?.[teamKey] ?? "");
  const currentSessionId = useWorkspaceActiveTab(teamKey, focusedWorkspaceId);
  const folds = useViewerStore((s) => s.agentsPaneFolds);
  const setFold = useViewerStore((s) => s.setAgentsPaneFold);
  const tabIndex = useTerminalTabIndex();
  const runningTabs = useRunningTerminalTabIndex();
  const minimizedTabs = useMinimizedTerminalTabIndex();
  const layoutByKey = useViewerStore((s) => s.terminalLayout.byKey);
  const { sessions: roster, reload } = useCorrectedAgentRoster();
  // Agents started by hand in a terminal tab are listed with the launched ones.
  const sessions = roster;
  // Each team's agents wear that team's choices and face numbers, whichever team is active. The
  // home team reads its own from the same map, so its tree and its Manager menu never differ.
  const allChoices = useViewerStore((s) => s.agentIdentity) ?? NO_CHOICES_MAP;
  const setAgentIdentity = useViewerStore((s) => s.setAgentIdentity);
  const pruneAgentIdentity = useViewerStore((s) => s.pruneAgentIdentity);
  const navRef = useRef<HTMLElement>(null);
  const { onLocate, onOpenStory } = useAgentDrill(teamKey, reload);
  const agentFilter = useViewerStore((s) => s.agentsListFilter);
  const clearAgentFilter = useViewerStore((s) => s.clearAgentsListFilter);
  const { requestStop, dialog: stopDialog } = useAgentStop();
  const open = visible && !collapsed;
  const settled = useSettled(open);
  // The header's Run history button swaps the agents for the finished runs and back. The history
  // is fetched only while it shows.
  const [showRuns, setShowRuns] = useState(false);
  const runs = useRunHistory(open && showRuns);
  // A Show that finds only a finished run opens the history here. The opener goes through the
  // pane's store, so it works while this list is unmounted too, and no list that goes away
  // takes it from another.
  useEffect(() => setRunHistoryOpener(openPaneRunHistory), []);
  // Adjusted while rendering, so the agents never draw once more first; the request is spent once
  // drawn.
  const runsAsked = usePaneRunsRequest((s) => s.pending);
  if (runsAsked && !showRuns) setShowRuns(true);
  useEffect(() => {
    if (runsAsked) takePaneRunsRequest();
  }, [runsAsked]);
  // A pill click asks for its workspace's agents: the agents come back to show it (adjusted
  // while rendering, so the runs never draw once more first).
  const [seenReveal, setSeenReveal] = useState(workspaceReveal);
  if (workspaceReveal !== seenReveal) {
    setSeenReveal(workspaceReveal);
    if (workspaceReveal) setShowRuns(false);
  }
  useEffect(() => {
    if (!open && workspaceReveal) clearWorkspaceReveal();
  }, [open, workspaceReveal, clearWorkspaceReveal]);
  // Pins are kept per team, and the Pinned group shows every team's.
  const allPins = useViewerStore((s) => s.agentPins) ?? NO_PIN_MAP;
  const pinsWithTeam = useMemo(
    () =>
      Object.entries(allPins).flatMap(([tk, list]) =>
        (list ?? []).map((pin): TeamPin => ({ teamKey: tk, pin })),
      ),
    [allPins],
  );
  const pinAgent = useViewerStore((s) => s.pinAgent);
  const unpinAgents = useViewerStore((s) => s.unpinAgents);
  // Whether the tabs and the workspaces are back: until then a running session can have no tab
  // and no story yet, so its pin must not be read as ended.
  const tabsLoaded = useViewerStore(
    (s) => s.terminalLayout.hydrated === true && s.persistedStateRestored === true,
  );
  // The remote Machines being connected: their tabs are not known until their layouts are read.
  const { loading: remoteLayoutsLoading } = useRemoteLayoutsLoading();

  // The agent chip narrows the plan, not the trees: the header counts what it leaves of the
  // whole list, and a child whose launcher it leaves out sits at its card's top level.

  const refOf = useCallback(
    (ws: Workspace): AgentWorkspaceRef => ({
      id: ws.id,
      // A story's workspace is labelled by its story id, short enough for a card, not its title.
      label: (ws.kind === "story" && ws.storyId) || ws.storyTitle || workspaceDisplayLabel(ws),
      kind: ws.kind === "story" ? "story" : "folder",
      storyStatus: ws.kind === "story" ? ws.storyStatus : undefined,
      // The card shows the story id; its title stays searchable and in the card's tooltip.
      storyTitle: ws.kind === "story" ? ws.storyTitle || undefined : undefined,
      sourceRepoSlug: ws.sourceRepoSlug,
      machineId:
        isRemoteWorkspace(ws) && isRemoteMachineId(ws.location.machineId)
          ? ws.location.machineId
          : undefined,
    }),
    [],
  );
  // The arrow filter's workspace record (open or soft-closed); undefined until it has loaded.
  const arrowWorkspace = useMemo((): Workspace | undefined => {
    if (!arrowFilter || arrowFilter.workspaceId === NO_WORKSPACE_ID) return undefined;
    const inTeam = [
      ...(workspaces[arrowFilter.teamKey] ?? []),
      ...Object.values(softClosedWorkspaces[arrowFilter.teamKey] ?? {}).map((sc) => sc.workspace),
    ];
    return inTeam.find((w) => w.id === arrowFilter.workspaceId);
  }, [arrowFilter, workspaces, softClosedWorkspaces]);
  // The arrow chip names the workspace by its record, not by its agents. Until the record has
  // loaded it says only "Workspace".
  const arrowLabel = useMemo((): string | null => {
    if (!arrowFilter) return null;
    if (arrowFilter.workspaceId === NO_WORKSPACE_ID) return "No workspace";
    return arrowWorkspace ? refOf(arrowWorkspace).label : "Workspace";
  }, [arrowFilter, arrowWorkspace, refOf]);

  // Every team the user has open: the filter menu lists them all, even ones with no agents yet.
  const openTeamKeys = useViewerStore((s) => s.openedTeams ?? NO_OPEN_TEAMS);
  // Each org's display name, as the team headers show it.
  const orgList = useOrgStore((s) => s.orgs);
  const orgNameOf = useMemo(() => {
    const names = new Map(orgList.map((o) => [o.slug, o.display_name]));
    return (slug: string) => names.get(slug);
  }, [orgList]);
  // The team a row nothing places (no tab, no story) is listed in: the first open team in the
  // pane's order, never the active one, so switching teams never moves it. With no open team
  // listed, the viewer's.
  const homeTeam = useAgentListProjectionStore((state) => state.homeTeam) ?? teamKey;
  const projectedTeamOrder = useAgentListProjectionStore((state) => state.teamOrder);

  const workspaceSources = useMemo(
    () =>
      makePaneWorkspaceSources(
        {
          workspaces,
          softClosedWorkspaces,
          running: runningTabs,
          minimized: minimizedTabs,
          hidden: allHiddenTabs,
          layoutByKey,
        },
        homeTeam,
      ),
    [
      workspaces,
      softClosedWorkspaces,
      homeTeam,
      runningTabs,
      minimizedTabs,
      allHiddenTabs,
      layoutByKey,
    ],
  );
  const projectedTrees = useAgentListTrees();
  const projectedReferences = useAgentListProjectionStore((state) => state.references);
  const placementOf = useCallback(
    (row: AgentSessionRow) =>
      projectedReferences.find((ref) => ref.sessionRef === (row.id ?? row.sessionId ?? row.handle))
        ?.placementTeamRef ?? null,
    [projectedReferences],
  );
  const tabTeamOf = useCallback(
    (sessionId: string) =>
      (
        findTabLocationBySessionId(tabIndex, sessionId) ??
        findTabLocationBySessionId(minimizedTabs, sessionId) ??
        findTabLocationBySessionId(allHiddenTabs, sessionId)
      )?.teamKey ?? null,
    [tabIndex, minimizedTabs, allHiddenTabs],
  );
  const nodes = projectedTrees.get(homeTeam) ?? NO_PROJECTED_NODES;
  const otherTeams = useMemo(
    () =>
      [...projectedTrees]
        .filter(([key]) => key !== homeTeam)
        .map(([teamKey, nodes]) => ({ teamKey, nodes })),
    [projectedTrees, homeTeam],
  );
  const everyNode = useMemo(() => [...nodes, ...otherTeamNodes(otherTeams)], [nodes, otherTeams]);
  // The pane's teams in name order, whichever one is active: switching teams never reorders the
  // rows, the active team just keeps its place in the list.
  const paneTeams = useMemo((): PaneTeam[] => {
    const mine: PaneTeam[] = nodes.length > 0 ? [{ teamKey: homeTeam, nodes }] : [];
    return [...mine, ...otherTeams.map((g) => ({ teamKey: g.teamKey, nodes: g.nodes }))].sort(
      (a, b) => projectedTeamOrder.indexOf(a.teamKey) - projectedTeamOrder.indexOf(b.teamKey),
    );
  }, [nodes, otherTeams, homeTeam, projectedTeamOrder]);
  // The roster reads as whole only once it has loaded in full (never while it is still loading,
  // or its last read missed one of its sources): the same rule wherever something is dropped
  // for not being listed (looks, pins).
  const rosterLoaded = useAgentListProjectionStore(
    (s) => s.sessions !== null && s.error === null && !s.partial,
  );
  const layoutMembers = useViewerStore((s) => s.terminalLayout.members);
  const filterMachines = useMachinesStore(
    useShallow((s) => ({ byTeam: s.byTeam, failedTeams: s.failedTeams })),
  );
  const { fate: arrowFate } = useWorkspaceFilterFate();
  // What an empty list says while the filter's workspace has not shown its agents yet.
  const arrowWait = useMemo((): WorkspaceWait | null => {
    if (!arrowFilter || !arrowFate) return null;
    if (arrowFate.fate === "loading") {
      return { kind: "loading", bucket: arrowFilter.workspaceId === NO_WORKSPACE_ID };
    }
    if (arrowFate.fate !== "waiting") return null;
    const machine = machineInTeams(filterMachines.byTeam, arrowFate.machineId, arrowFilter.teamKey);
    return {
      kind: "machine",
      machine: machine?.label ?? UNKNOWN_MACHINE_LABEL,
      // The rows' one offline rule: a machine whose status no list has read reads neither.
      offline: !!machine?.status && machineOfflineWord(machine) !== null,
    };
  }, [arrowFilter, arrowFate, filterMachines]);
  // Every listed team's role holdings: a row of any team says what it holds.
  const roleTeams = useMemo(
    () => [...new Set([teamKey, homeTeam, ...paneTeams.map((t) => t.teamKey)])],
    [teamKey, homeTeam, paneTeams],
  );
  const roles = useRoleHoldings(roleTeams, open);
  const agentCount = countNodes(everyNode);
  const pinned = useMemo(() => pinnedEntries(pinsWithTeam, paneTeams), [pinsWithTeam, paneTeams]);
  const { statusOf: liveStatusOf } = useNeedsYou(everyNode);
  const nowMs = useNowClock(1000, open && agentCount > 0);

  // Roles read the whole tree, so a filter never changes what a row wears.
  const avatars = useAvatarPacks();
  // The teams the list can show: their own tables are built up front, not while a row renders.
  const everyTeamKey = useMemo(
    () => [
      ...new Set([
        teamKey,
        homeTeam,
        ...paneTeams.map((t) => t.teamKey),
        ...Object.keys(workspaces),
        ...Object.keys(allChoices),
        // A pin can keep a team that has no agents, workspaces or choices of its own.
        ...pinsWithTeam.map((p) => p.teamKey),
      ]),
    ],
    [teamKey, homeTeam, paneTeams, workspaces, allChoices, pinsWithTeam],
  );
  // Each team's agents wear that team's own packs.
  const avatarsOf = useTeamAvatarPacks(everyTeamKey);
  const facesOf = useCallback((tk: string) => avatarsOf(tk).faces, [avatarsOf]);
  const faceTagsByTeam = useMemo(
    () =>
      new Map(
        everyTeamKey.map((tk) => [
          tk,
          faceTagsFor(tk, {
            sessions,
            choices: allChoices[tk] ?? NO_CHOICES,
            faces: facesOf(tk),
            running: runningTabs,
            minimized: minimizedTabs,
          }),
        ]),
      ),
    [everyTeamKey, sessions, allChoices, facesOf, runningTabs, minimizedTabs],
  );
  const faceTagsOf = useCallback(
    (tk: string) => faceTagsByTeam.get(tk) ?? NO_FACE_TAGS,
    [faceTagsByTeam],
  );
  // Agents whose tab the person Hid, in any team: still running, listed with a quiet marker.
  const hiddenSessionIds = useMemo(
    () =>
      new Set(
        Object.values(allHiddenTabs).flatMap((byWs) =>
          Object.values(byWs).flatMap((tabs) => tabs.map((t) => t.sessionId)),
        ),
      ),
    [allHiddenTabs],
  );
  const routineRows = useRoutinesStore((s) => s.rows);
  // Every team's machines: a row names, and reads the status of, the machine its tab runs on as
  // its tab's team lists it, and any team's machine going offline re-renders the rows there.
  const machinesByTeam = useMachinesStore((s) => s.byTeam);
  // A team with two or more source repos marks each story's repo with a glyph, as the tray does.
  const workspaceRepos = useOrgStore((s) => s.workspaceRepos);
  // Each team's glyphs come from that team's own repos, so any team's workspace can wear one.
  const repoGlyphsByTeam = useMemo(
    () => new Map(everyTeamKey.map((tk) => [tk, repoGlyphsForTeam(workspaceRepos, tk)])),
    [everyTeamKey, workspaceRepos],
  );
  const repoGlyphsOf = useCallback(
    (tk: string) => repoGlyphsByTeam.get(tk) ?? null,
    [repoGlyphsByTeam],
  );
  // The face numbers, crews and their marks read the same corrected roster as the trees, so a
  // card and its rows never disagree on who is there.
  const liveRows = useMemo(() => (sessions ?? []).filter((r) => r.alive), [sessions]);
  const crewStatusOf = useAgentStatusOf(liveRows);
  // Each member's mark, per team: the crews and their qualifier words are built from these.
  const memberMarksByTeam = useMemo(
    () =>
      new Map(
        everyTeamKey.map((tk) => [
          tk,
          memberMarksFor({
            workspaceSources,
            teamKey: tk,
            workspaces: workspaces[tk],
            tabIndex,
            live: liveRows,
            statusOf: crewStatusOf,
            machines: machinesByTeam,
          }),
        ]),
      ),
    [
      everyTeamKey,
      workspaceSources,
      workspaces,
      tabIndex,
      liveRows,
      crewStatusOf,
      machinesByTeam,
      projectedReferences,
    ],
  );
  // The activity words the crews show, by session: compared shallowly, so the list re-renders only
  // when one of those words changes, not on every activity snapshot.
  const qualifiers = useAgentStateStore(
    useShallow((s) => {
      const words: Record<string, string | undefined> = {};
      for (const marks of memberMarksByTeam.values()) {
        Object.assign(words, qualifiersFor(marks, s.activityBySession));
      }
      return words;
    }),
  );
  // Every team's workspace cards get their own crew stack, from that team's agents.
  const crewsByTeam = useMemo(
    () =>
      new Map(
        everyTeamKey.map((tk) => [
          tk,
          crewsFor({
            workspaceSources,
            teamKey: tk,
            workspaces: workspaces[tk],
            tabIndex,
            live: liveRows,
            statusOf: crewStatusOf,
            machines: machinesByTeam,
            choices: allChoices[tk] ?? NO_CHOICES,
            faces: facesOf(tk),
            tags: faceTagsOf(tk),
            memberMarks: memberMarksByTeam.get(tk) ?? {},
            qualifiers,
          }),
        ]),
      ),
    [
      everyTeamKey,
      workspaceSources,
      workspaces,
      tabIndex,
      liveRows,
      crewStatusOf,
      machinesByTeam,
      allChoices,
      facesOf,
      faceTagsOf,
      memberMarksByTeam,
      qualifiers,
    ],
  );
  const crewsOf = useCallback((tk: string) => crewsByTeam.get(tk) ?? NO_CREWS, [crewsByTeam]);
  // Where a row's tab sits, shown, minimized or hidden, in whichever team holds it.
  const tabLocOf = useCallback(
    (row: AgentSessionRow) =>
      row.sessionId
        ? (findTabLocationBySessionId(tabIndex, row.sessionId) ??
          findTabLocationBySessionId(minimizedTabs, row.sessionId) ??
          findTabLocationBySessionId(allHiddenTabs, row.sessionId))
        : undefined,
    [tabIndex, minimizedTabs, allHiddenTabs],
  );
  // Where a row runs: its tab's origin (a tab with none runs on This Mac, whatever the roster
  // says), else, before its tab arrives, the remote machine its launch recorded. Only a tab can
  // be held frozen, so a tabless row reads offline only by its machine's own status. The team
  // whose machine list names the machine is its tab's, else the one its launch was made in.
  const machineAtOf = useCallback(
    (row: AgentSessionRow): { origin: TabOrigin; teamKey?: string } | null => {
      const loc = tabLocOf(row);
      if (loc?.tab.origin) return { origin: loc.tab.origin, teamKey: loc.teamKey };
      if (loc) return null;
      return isRemoteMachineId(row.machineId)
        ? { origin: { machineId: row.machineId }, teamKey: row.launchTeamKey ?? undefined }
        : null;
    },
    [tabLocOf],
  );
  // A tab held as it last was (its machine's layout not read live) reads offline too.
  const offlineMachineOf = useCallback(
    (row: AgentSessionRow) => {
      const at = machineAtOf(row);
      return frozenTabMachineIn(machinesByTeam, at?.origin, at?.teamKey);
    },
    [machineAtOf, machinesByTeam],
  );
  // An agent on an offline machine reads unknown: what it last said (a question, a block) may no
  // longer hold, so it is not counted as needing you, wears no ask line and sits in no needs-you
  // bucket. A known ending (ended, crashed) still reads as itself.
  const statusOf = useCallback(
    (row: AgentSessionRow): AgentStatus => {
      const status = liveStatusOf(row);
      if (offlineMachineOf(row) === null) return status;
      const mark = markOfAgentStatus(status);
      return mark === "ended" || mark === "crashed" ? status : "unknown";
    },
    [liveStatusOf, offlineMachineOf],
  );
  // By team and handle: one handle can run in two teams, and only the one that waits needs you.
  const needs = useMemo(() => needsYouKeys(paneTeams, statusOf), [paneTeams, statusOf]);
  const identity = useMemo((): Omit<IdentityContext, "offlineHandles"> => {
    // Each agent's role reads the agents under it in its own team's tree, as the Manager menu does.
    const launched = launchedCountsOf(paneTeams);
    // A team's name is ambiguous when another open team, or another team on the pane, shares it.
    const labelTeams = [...new Set([...openTeamKeys, ...paneTeams.map((t) => t.teamKey)])];
    return {
      launched,
      homeTeamKey: homeTeam,
      teamLabelOf: (tk) => paneTeamLabel(tk, labelTeams, orgNameOf),
      choicesOf: (tk) => allChoices[tk] ?? NO_CHOICES,
      choose: (rowTeam, row, patch) => setAgentIdentity(rowTeam, row.handle, patch),
      isHidden: (row) => row.sessionId !== null && hiddenSessionIds.has(row.sessionId),
      // Live, but on no tile: no tab at all, or only in a launch workspace no tile shows.
      isTabless: (row) => {
        if (row.sessionId === null || hiddenSessionIds.has(row.sessionId)) return false;
        if (findTabLocationBySessionId(minimizedTabs, row.sessionId)) return false;
        const loc = findTabLocationBySessionId(tabIndex, row.sessionId);
        return !loc || !(workspaces[loc.teamKey] ?? []).some((w) => w.id === loc.workspaceId);
      },
      // In any team: another team's row hides its tab in that team.
      tabOf: (row) => {
        const loc = row.sessionId ? findTabLocationBySessionId(tabIndex, row.sessionId) : null;
        return loc && (workspaces[loc.teamKey] ?? []).some((w) => w.id === loc.workspaceId)
          ? { teamKey: loc.teamKey, workspaceId: loc.workspaceId, tabId: loc.tab.id }
          : null;
      },
      routineOf: (row) => {
        const key = routineKeyOf(row);
        return key ? routineLabel(key, routineRows) : null;
      },
      // A row reads its machine as its tab's own team lists it, as a card does: one machine id
      // can be listed by two teams, each with its own status.
      remoteMachineOf: (row) => {
        const at = machineAtOf(row);
        // The mark reads offline by the Offline tag's own rule: a tab held as it last was reads
        // offline though its machine is connected, and a machine whose status no list has read
        // reads neither.
        return at
          ? remoteMachineIdentity(
              at.origin.machineId,
              frozenTabMachineIn(machinesByTeam, at.origin, at.teamKey),
              machineInTeams(machinesByTeam, at.origin.machineId, at.teamKey),
            )
          : null;
      },
      // The card's own team's machine first, by the same rule (a card has no tab to hold frozen).
      remoteMachineFor: (tk, machineId) =>
        remoteMachineIdentity(
          machineId,
          frozenTabMachineIn(machinesByTeam, { machineId }, tk),
          machineInTeams(machinesByTeam, machineId, tk),
        ),
      offlineMachineOf,
      currentSessionId,
      settled,
      avatars,
      facesOf,
      avatarsOf,
      faceTagsOf,
      repoGlyphOf: (tk, slug) => repoGlyphsOf(tk)?.get(slug),
      crewOf: (tk, workspaceId) => crewsOf(tk).get(workspaceId),
      // Read when a row's menu opens, from that row's own team's tree: built with that team's
      // manager choices, so the agents below the row (left out, as they would make a loop) are
      // the ones the pane draws below it.
      managerOptions: (tk, handle) =>
        managerOptions(
          teamAgentIdentitiesOf(paneTeams.find((t) => t.teamKey === tk)?.nodes ?? [], {
            choices: allChoices[tk] ?? NO_CHOICES,
            faces: facesOf(tk),
            tags: faceTagsOf(tk),
          }),
          handle,
        ),
      machineOf: (row) => {
        const at = machineAtOf(row);
        const machineId = at?.origin.machineId;
        const machine = machineId ? machineInTeams(machinesByTeam, machineId, at.teamKey) : null;
        return agentMachineLabel(machineId, machine ? [machine] : null);
      },
      // A hidden tab's session can have a wake queued for it too.
      machineIdOf: (row) => {
        const machineId = machineAtOf(row)?.origin.machineId;
        return isRemoteMachineId(machineId) ? machineId : LOCAL_MACHINE_ID;
      },
      isPinned: (rowTeam, row) => pinOfRow(pinsWithTeam, rowTeam, row) !== null,
      // A new pin is kept in the row's own team, under an id that names that team. A row a pin
      // already holds (by its session id, too) unpins that pin, in the team it is kept in.
      togglePin: (rowTeam, row, label) => {
        const held = pinOfRow(pinsWithTeam, rowTeam, row);
        if (held) unpinAgents(held.teamKey, [agentPinId(held.pin)]);
        else pinAgent(rowTeam, pinForRow(rowTeam, row, label));
      },
      holdsOf: (tk, row) => roles.holdsOf(tk, row),
      releaseRole: (holding) => void roles.release(holding),
      narrow,
    };
  }, [
    pinsWithTeam,
    pinAgent,
    unpinAgents,
    roles,
    narrow,
    machinesByTeam,
    workspaces,
    routineRows,
    minimizedTabs,
    paneTeams,
    hiddenSessionIds,
    avatars,
    facesOf,
    avatarsOf,
    tabIndex,
    currentSessionId,
    settled,
    setAgentIdentity,
    homeTeam,
    machineAtOf,
    offlineMachineOf,
    faceTagsOf,
    allChoices,
    repoGlyphsOf,
    crewsOf,
    openTeamKeys,
    orgNameOf,
  ]);

  // The agents on an offline machine, by team and handle: a via chip strikes through an offline
  // manager.
  const offlineHandles = useMemo(
    () =>
      new Set(
        flattenNodes(everyNode)
          .filter((n) => offlineMachineOf(n.row) !== null)
          .map((n) => agentRowKey(n.teamKey ?? homeTeam, n.row.handle)),
      ),
    [everyNode, offlineMachineOf, homeTeam],
  );
  const identityWithOffline = useMemo(
    () => ({ ...identity, offlineHandles }),
    [identity, offlineHandles],
  );

  // The sheet opens in the row's own team, where its manager runs.
  const launchUnder = useCallback(
    (row: AgentSessionRow, rowTeam: string) => launchUnderAgent(row, rowTeam),
    [],
  );
  // A row click shows the agent's tab and focuses its terminal, so the user can type straight away.
  const reveal = useCallback(
    (row: AgentSessionRow) => {
      // A row whose team the pane hid shows that team again: the team the pane places it in (by
      // its story, else its tab, as its node is), and the team whose tab Show brings up (a hidden
      // tab's too).
      const tabTeam = row.sessionId ? tabTeamOf(row.sessionId) : null;
      const placed = placementOf(row) ?? homeTeam;
      for (const home of new Set([placed, tabTeam ?? placed])) {
        if (hiddenTeams.includes(home)) setTeamHidden(home, false);
      }
      onLocate(row);
    },
    [onLocate, hiddenTeams, setTeamHidden, placementOf, tabTeamOf, homeTeam],
  );
  const openInbox = useCallback(
    (row: AgentSessionRow) => {
      void openAgentInboxForRow(row, onLocate);
    },
    [onLocate],
  );
  const message = useCallback(
    (row: AgentSessionRow) => {
      void openAgentInboxForRow(row, onLocate, { compose: true });
    },
    [onLocate],
  );

  const hideAgentTab = useCallback(
    (row: AgentSessionRow) => {
      const at = identity.tabOf(row);
      if (at) hideTab(at.teamKey, at.workspaceId, at.tabId);
    },
    [identity],
  );

  // One fold map for the whole pane: fold keys name their team.
  const toggleChildren = useCallback(
    (key: string, folded: boolean) => setFold(key, folded),
    [setFold],
  );
  // The search reads each agent's activity note. Only a typed search subscribes, and only to the
  // listed agents' notes as their rows word them (compared shallowly), so another session's
  // progress tick, or one that leaves the words as they were, does not re-plan the pane.
  const searching = filter.query.trim() !== "";
  const listedSessionIds = useMemo(
    () => flattenNodes(everyNode).flatMap((n) => (n.row.sessionId ? [n.row.sessionId] : [])),
    [everyNode],
  );
  const activityForSearch = useViewerStore(
    useShallow((s) => {
      if (!searching) return NO_ACTIVITY;
      const out: Record<string, string | null> = {};
      for (const sid of listedSessionIds) out[sid] = activityLine(s.progressBySession[sid]);
      return out;
    }),
  );
  // What the search reads, built once per plan.
  const searchIdentity = useMemo(
    (): SearchIdentity => ({
      launched: identity.launched,
      choicesOf: identity.choicesOf,
      homeTeamKey: identity.homeTeamKey,
      machineOf: identity.machineOf,
      machineIdOf: identity.machineIdOf,
      offlineMachineOf: identity.offlineMachineOf,
      routineOf: identity.routineOf,
      isHidden: identity.isHidden,
      activityOf: (row) => (row.sessionId ? (activityForSearch[row.sessionId] ?? null) : null),
    }),
    [identity, activityForSearch],
  );
  // The pane's plan: which teams and agents show, under the search, Show filter, team scope,
  // workspace arrow and agent chip. The view draws exactly this.
  const agentChipKey = agentFilter?.agentKey ?? null;
  const plan = useMemo(
    () =>
      planAgentsPane(
        paneTeams,
        hiddenTeams,
        {
          query: filter.query,
          status: filter.status,
          workspace: arrowFilter,
          agentKey: agentChipKey,
          rail: railScope,
        },
        {
          statusOf,
          textOf: (node) => searchTextOf(node, searchIdentity),
          manager: arrange === "manager",
          railKeyOfWorkspace: (tk, id) => railKeys?.[tk]?.[id],
        },
      ),
    [
      paneTeams,
      hiddenTeams,
      filter.query,
      filter.status,
      arrowFilter,
      agentChipKey,
      railScope,
      railKeys,
      arrange,
      statusOf,
      searchIdentity,
    ],
  );
  // Clears the search, the Show filter, the arrow filter and the agent chip, and shows every team
  // again.
  const clearPaneFilters = useCallback(() => {
    setRailScope(null);
    setFilter({ query: "", status: "all" });
    setWorkspaceFilter(null);
    clearAgentFilter();
    setTeamScope([], null);
  }, [setTeamScope, setFilter, setWorkspaceFilter, clearAgentFilter]);
  // The last team the Teams checklist shows checked cannot be unchecked: the pane always keeps one
  // team in view. The checklist lists every open team and every team with agents.
  const hideTeam = useCallback(
    (team: string, hidden: boolean) => {
      if (hidden) {
        const listed = new Set([...openTeamKeys, ...paneTeams.map((t) => t.teamKey)]);
        listed.delete(NO_TEAM_SENTINEL);
        const left = [...listed].filter((k) => k !== team && !hiddenTeams.includes(k));
        if (left.length === 0) return;
      }
      setTeamHidden(team, hidden);
    },
    [openTeamKeys, paneTeams, hiddenTeams, setTeamHidden],
  );
  const showAllTeams = useCallback(() => setTeamScope([], null), [setTeamScope]);
  // A team header's funnel reads the teams on the pane (whether it is on) and hides every team
  // the Teams checklist lists but its own.
  const funnelTeams = useMemo(
    () => ({
      shownTeams: paneTeams.map((t) => t.teamKey).filter((k) => k !== NO_TEAM_SENTINEL),
      allTeams: openTeamKeys.filter((k) => k !== NO_TEAM_SENTINEL),
    }),
    [openTeamKeys, paneTeams],
  );
  const teamInFocus = focusedTeam(funnelTeams.shownTeams, hiddenTeams);
  const toggleTeamInFocus = useCallback(
    (team: string) => {
      const next = toggleTeamFocus(team, funnelTeams, hiddenTeams, teamFocusRestore);
      setTeamScope(next.hidden, next.restore);
    },
    [funnelTeams, hiddenTeams, teamFocusRestore, setTeamScope],
  );

  // Looks chosen for sessions that are gone are dropped once the roster, the tabs and every remote
  // layout have loaded, once per team that keeps choices (each team's agents wear that team's): a
  // team whose choices arrive later is pruned when they do.
  const [identityHydrated, setIdentityHydrated] = useState(tabsLoaded && !remoteLayoutsLoading);
  useEffect(() => {
    if (identityHydrated || !rosterLoaded || !tabsLoaded || remoteLayoutsLoading) return;
    let current = true;
    void reload().then(() => {
      if (current) setIdentityHydrated(true);
    });
    return () => {
      current = false;
    };
  }, [identityHydrated, rosterLoaded, tabsLoaded, remoteLayoutsLoading, reload]);
  const prunedTeams = useRef(new Set<string>());
  useEffect(() => {
    if (
      !identityHydrated ||
      sessions === null ||
      !rosterLoaded ||
      !tabsLoaded ||
      remoteLayoutsLoading
    )
      return;
    const handles = new Set(sessions.map((r) => r.handle));
    // A layout snapshot starts launch restoration asynchronously. Until the registry catches
    // up, its roster names these tabs by PTY id; the host's live launch tags retain their
    // actual handles (and therefore their saved manager choices) through that gap.
    for (const member of Object.values(layoutMembers)) {
      for (const workspace of Object.values(member.workspaces)) {
        for (const tab of workspace.tabs) {
          if (tab.state.kind === "live" && tab.tags.launchHandle)
            handles.add(tab.tags.launchHandle);
        }
      }
    }
    for (const tk of Object.keys(allChoices)) {
      if (prunedTeams.current.has(tk)) continue;
      prunedTeams.current.add(tk);
      const keep = new Set(handles);
      // A hand-started agent is known by its tab's session: shown, minimized or hidden.
      for (const index of [tabIndex, minimizedTabs, allHiddenTabs]) {
        for (const tabs of Object.values(index[tk] ?? {})) {
          for (const t of tabs) keep.add(t.sessionId);
        }
      }
      pruneAgentIdentity?.(tk, keep);
    }
  }, [
    sessions,
    identityHydrated,
    rosterLoaded,
    tabsLoaded,
    remoteLayoutsLoading,
    tabIndex,
    minimizedTabs,
    allHiddenTabs,
    layoutMembers,
    allChoices,
    pruneAgentIdentity,
  ]);

  // Opened from the tray's Agents control: focus moves into the list.
  useEffect(() => {
    if (open && takeAgentsFocus(teamKey, "list")) focusAgentsList(teamKey);
  }, [open, teamKey]);

  // With the CLI column closed, or the dock folded, there is nothing here at all; the dock
  // keeps the border that brings it back.
  if (!visible || collapsed) return null;

  // A filter or a hidden team leaves some agents out of view: the count says how many show of all.
  const countText =
    plan.filtering || plan.everyTotal > plan.total
      ? `${plan.visible}/${plan.everyTotal}`
      : `${plan.total}`;

  return (
    <Identity.Provider value={identityWithOffline}>
      <div
        ref={navRef as React.RefObject<HTMLDivElement>}
        data-narrow={narrow || undefined}
        className="cs-agents relative flex min-h-0 w-full flex-1 flex-col"
      >
        <div className="ag-head">
          <span className="ag-pane-title">{showRuns ? "Run history" : "Agents"}</span>
          {/* Plain text after the title, so it reads "Agents 3" (or "Agents 2/5"), or how many
              runs the history lists once it has read them. */}
          <span className="ag-pane-count" data-testid="agents-count">
            {showRuns ? (runs ? `${runs.length}` : "") : countText}
          </span>
          {showRuns && needs.size > 0 && (
            // The agents are out of sight: the header still says some wait on you.
            <span
              role="img"
              className="ag-team-need"
              data-testid="agents-runs-needs"
              aria-label={`${needs.size} ${needs.size === 1 ? "agent needs" : "agents need"} you`}
            >
              <StatusMark mark="question" size={6} />
              {needs.size}
            </span>
          )}
          <span className="flex-1" />
          {!showRuns && (
            <div className="ag-view-switch" role="group" aria-label="Agents view">
              <button
                type="button"
                aria-pressed={arrange === "teams"}
                onClick={() => changeArrange("teams")}
              >
                Workspace
              </button>
              <button
                type="button"
                aria-pressed={arrange === "manager"}
                onClick={() => changeArrange("manager")}
              >
                Manager
              </button>
            </div>
          )}
          <button
            type="button"
            aria-label={showRuns ? "Back to agents" : "Run history"}
            aria-pressed={showRuns}
            title={showRuns ? "Back to agents" : "Run history: finished runs and their output"}
            data-testid="agents-run-history"
            className="ag-icon-btn"
            onClick={() => setShowRuns((on) => !on)}
          >
            <History className="size-3.5" />
          </button>
          <button
            type="button"
            aria-label="Launch agent"
            title="Launch agent"
            data-testid="agents-launch"
            className="ag-icon-btn"
            onClick={() => openLaunchSheet({ teamKey })}
          >
            <Plus className="size-3.5" />
          </button>
          <button
            type="button"
            aria-label="Hide agents"
            title="Hide agents"
            className="ag-icon-btn"
            onClick={() => hideAgentsDock(teamKey)}
          >
            <PanelLeftClose className="size-3.5" />
          </button>
        </div>
        {showRuns ? (
          <RunsView teamKey={teamKey} runs={runs} />
        ) : (
          <AgentsTreeView
            teamKey={homeTeam}
            launchTeamKey={teamKey}
            teams={paneTeams}
            openTeamKeys={openTeamKeys}
            plan={plan}
            hiddenTeams={hiddenTeams}
            onSetTeamHidden={hideTeam}
            onShowAllTeams={showAllTeams}
            // Offered while more than one team has agents on the pane.
            teamFunnel={funnelTeams.shownTeams.length > 1}
            focusedTeam={teamInFocus}
            onToggleTeamFocus={toggleTeamInFocus}
            query={filter.query}
            onQueryChange={(query) => setFilter({ query })}
            status={filter.status}
            onStatusChange={(status) => setFilter({ status })}
            workspace={arrowFilter}
            workspaceLabel={arrowLabel}
            workspaceWait={arrowWait}
            onWorkspaceChange={setWorkspaceFilter}
            onClearAll={clearPaneFilters}
            arrange={arrange}
            onArrangeChange={changeArrange}
            arrangeAnchor={arrangeAnchor}
            onArrangeAnchorConsumed={consumeArrangeAnchor}
            railScope={railScope}
            onRailScopeChange={setRailScope}
            statusOf={statusOf}
            needs={needs}
            collapsed={folds}
            onToggleChildren={toggleChildren}
            nowMs={nowMs}
            agentFilterName={agentFilter?.name ?? null}
            onClearAgentFilter={clearAgentFilter}
            onReveal={reveal}
            onHideTab={hideAgentTab}
            onOpenStory={onOpenStory}
            onStop={requestStop}
            onLaunchUnder={launchUnder}
            onMessage={message}
            onOpenInbox={openInbox}
            pinned={pinned}
            workspaceReveal={workspaceReveal}
            onWorkspaceRevealDone={clearWorkspaceReveal}
          />
        )}
        {stopDialog}
      </div>
    </Identity.Provider>
  );
}

/** True once the list has been showing for a moment, so only later arrivals animate in. */
function useSettled(showing: boolean): boolean {
  const [settled, setSettled] = useState(false);
  useEffect(() => {
    if (!showing) return;
    const timer = setTimeout(() => setSettled(true), SETTLE_MS);
    return () => clearTimeout(timer);
  }, [showing]);
  return settled;
}

/** A remote machine's colour and status, as a row or a card shows them; null for This Mac.
 *  `offlineName` is the machine's name when {@link frozenTabMachineIn} reads it offline for the
 *  row's or card's own team (rows and cards share that rule), else null. `machine` is its record
 *  as that team lists it (else as another team does). A machine no list knows is named
 *  {@link UNKNOWN_MACHINE_LABEL}, never by its raw id. */
function remoteMachineIdentity(
  machineId: string,
  offlineName: string | null,
  machine: MachineSummary | undefined,
): RemoteMachineIdentity | null {
  if (!isRemoteMachineId(machineId)) return null;
  const label = machine?.label ?? UNKNOWN_MACHINE_LABEL;
  // "offline since 14:05" when the machine itself says so; a tab held as it last was on a
  // connected machine reads plain "offline".
  const offline = offlineName
    ? ((machine?.status ? machineOfflineWord(machine) : null) ?? "offline")
    : null;
  return {
    machineId,
    label,
    color: machineColor(machineId),
    offline,
    words: offline ? `on ${label} · ${offline}` : `on ${label}`,
  };
}

const NO_PROJECTED_NODES: import("../lib/agent-list-view").AgentTreeNode[] = [];
