// SPDX-License-Identifier: MIT
import { questionFor } from "@/shared/lib/managed-session";
export { questionFor } from "@/shared/lib/managed-session";
import { appPopoverBounds } from "@/shared/lib/popover-bounds";
import { activityLine } from "../lib/agent-list-search";
export { searchTextOf, activityLine, type SearchIdentity } from "../lib/agent-list-search";
import { customRailKey } from "../lib/workspace-groups";
import { RowMenu } from "./agent-row-menu";
import { Identity, teamNameOf, type IdentityContext } from "./agent-row-identity-context";
export { Identity, teamNameOf, type IdentityContext } from "./agent-row-identity-context";
import {
  useAgentListProjectionStore,
  useAgentStatusOrder,
  projectedStatusNodes,
} from "@/features/viewer/lib/agent-list-projection";

import {
  MachineMark,
  type RemoteMachineIdentity,
} from "@/features/machines/components/machine-location-badge";
import { ContextRepoMark } from "@/features/shell/components/context-repo-mark";
import { InboxChatPopover } from "@/features/viewer/components/agents-inbox-chat";
import { rowInView } from "@/features/viewer/components/agents-popover";
import { QuickSendPopover } from "@/features/viewer/components/agents-quick-send";
import { useWorkspaceCardDrag } from "@/features/viewer/hooks/use-workspace-card-drag";
import {
  CARD_MOVE_REFUSED,
  teamSlotCards,
  type RowBox,
} from "@/features/viewer/lib/agents-pane-drag";
import {
  buildPaneRows,
  paneRowPlaceWords,
  paneRowSemantics,
  type PaneRow,
  type PaneRowPlace,
  type PaneRowSemantics,
} from "@/features/viewer/lib/agents-pane-rows";
import { useRunningAgent } from "@/features/viewer/lib/hand-agents";
import {
  stepSlot,
  teamWorkspaceOrder,
  type OrderPlace,
} from "@/features/viewer/lib/workspace-order";
import { getManagedTreeAccent } from "@/shared/lib/pipeline-graph/epic-colors";
import { useIsLightViewerTheme } from "@/shared/lib/viewer-theme";
import {
  managerFoldKey,
  teamFoldKey,
  workspaceFoldKey,
  workspaceGroupKey,
} from "@/shared/lib/workspace-key";
import { useOrgStore } from "@/shared/stores/org-store";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  memo,
  useCallback,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { toast } from "sonner";
import { useShallow } from "zustand/react/shallow";
import { railInfo } from "../lib/group-workspaces-by-epic";
import { SessionRoleChips } from "./session-role-chips";

import {
  AlarmClock,
  ChevronDown,
  EyeOff,
  Filter,
  GripVertical,
  Layers,
  ListFilter,
  MessageSquare,
  Pin,
  PinOff,
  ScrollText,
  Search,
  Terminal,
  WifiOff,
  X,
} from "lucide-react";

import { ContextMenu, ContextMenuContent, ContextMenuTrigger } from "@/components/ui/context-menu";
import {
  flattenNodes,
  NO_WORKSPACE_ID,
  type AgentListArrange,
  type WorkspaceSessionGroup,
} from "@/features/agents/lib/agent-list-layout";
import { clockTime, queuedWakeFor } from "@/features/agents/lib/agent-routines";
import { openLaunchSheet } from "@/features/agents/lib/launch-sheet";
import { useRoutinesStore, watchRoutineRows } from "@/features/routines/stores/routines-store";
import {
  AgentAvatar,
  CrewStack,
  useArrivedLate,
} from "@/features/viewer/components/agent-identity";
import { SubagentMark } from "@/features/viewer/components/subagent-mark";
import type { RoleHolding } from "@/features/viewer/hooks/use-role-holdings";
import {
  agentDetailLine,
  agentRowLine,
  countNodes,
  flattenRows,
  PINNED_VISIBLE_MAX,
  resolveAgentFace,
  resolveAgentRole,
  rowMarkers,
  THIS_MAC,
  type AgentStatusFilter,
  type AgentTreeNode,
  type PinnedEntry,
  type RowMarker,
} from "@/features/viewer/lib/agent-list-view";
import {
  agentRowLabel,
  agentRowNames,
  formatElapsed,
  isHeadlessSession,
} from "@/features/viewer/lib/agent-row-presentation";
import type {
  PanePlan,
  PaneRailFilter,
  PaneTeam,
  PaneWorkspaceFilter,
  PlannedTeam,
} from "@/features/viewer/lib/agents-pane-plan";
import { agentSessionModel } from "@/features/viewer/lib/agent-session-model";
import { useSessionProgress } from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import {
  cliLabel,
  handAgentSubtitle,
  type ListedAgentRow,
} from "@/features/viewer/lib/hand-agents";
import { SessionNotices } from "@/features/viewer/components/session-notices";
import {
  isTypedByHand,
  releaseHint,
  releaseWarning,
  typedByHandHint,
} from "@/features/viewer/lib/session-states";
import type { SessionStory } from "@/features/viewer/lib/session-story";
import { cn } from "@/lib/utils";
import { RepoGlyph } from "@/shared/components/repo-glyph";
import { StageRail } from "@/shared/components/stage-rail";
import { StatusMark } from "@/shared/components/status-mark";
import { StatusSelectorPopover } from "@/shared/components/status-selector-popover";
import { activityQualifier, withActivityQualifiers } from "@/shared/lib/agent-activity-words";
import { type AgentIdentityChoice } from "@/shared/lib/agent-identity-choice";
import { agentRowKey } from "@/shared/lib/agent-pins";
import {
  AGENT_STATUS_WORDS,
  isNeedsYouStatus,
  type AgentStatus,
} from "@/shared/lib/agent-status-config";
import { type FaceSource } from "@/shared/lib/avatar-packs";
import { isMac } from "@/shared/lib/shortcuts/keystroke";
import {
  countMarks,
  MARK_WORDS,
  markOfAgentStatus,
  markWithAvailability,
  rollupWords,
  topMark,
  type StatusMarkKind,
} from "@/shared/lib/status-mark";
import { NO_TEAM_SENTINEL, parseTeamKey } from "@/shared/lib/team-key";
import {
  agentStatusWord,
  agentStatusWords,
  shownSubagents,
  SUBAGENTS_WORKING_WORD,
} from "@/shared/lib/terminal-dot-words";
import { useAgentStateStore, useSubagentsOpen } from "@/shared/stores/agent-state-store";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import type { SessionProgress } from "@/shared/stores/viewer-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { type AgentSessionRow, type RoutineRow } from "@/tauri/commands";

/** Left inset of a row. */
const ROW_INSET_PX = 2;
/** Horizontal step per launch level: a worker sits under its launcher's chevron. */
const TREE_INDENT_PX = 18;
/** An agent in a workspace card sits this far in, so it reads as part of that card. */
const WORKSPACE_INDENT_PX = 10;
/** The avatar size of a row. */
const AVATAR_PX = 26;
/** The ink a role piece wears in the pane: neutral, since the pane's colours mean status and
 *  machine. */
const NEUTRAL_PIECE = "var(--ag-piece-ink)";
/** A row's role badge: smaller than the face's 45%, so the three corners it shares with the
 *  number tag and the status dot each keep the face between them. */
const ROW_PIECE_PX = 11;
/** The in-avatar status dot. The CSS inflates its visual halo on `running` so the pulse reads
 *  from across the pane. */
const STATUS_DOT_PX = 10;
const HIDDEN_TAB_HINT = "Tab hidden, still running. Click to show it.";
const HEADLESS_HINT = "Headless: no terminal. Click to open its run output.";
/** The modifier key by the name its keyboard prints: Option on macOS, Alt elsewhere. */
function altKeyName(): string {
  return isMac() ? "Option" : "Alt";
}
/** What a row says it does on each gesture. */
function rowGesturesHint(): string {
  return `Click for its terminal · Enter, Space or Q for its quick view · M for its inbox · ${altKeyName()}-click for the quick view too · Right-click for more`;
}
/** An offline row has no quick view: Enter and Space show the agent, and Q and Option-click do
 *  nothing. */
const OFFLINE_ROW_GESTURES_HINT =
  "Click, Enter or Space to show it · M for its inbox · Right-click for more";
/** A launched agent indents one step per level below its launcher, up to this many levels; one
 *  deeper lines up with that level. Only the indent stops there: the pane's launch tree nests any
 *  depth (the launch sheet's manager picker caps its own indent the same way). */
const MAX_INDENT_LEVELS = 4;

interface RowActions {
  /** Show the agent: its tab, restored or adopted into one, its team switched to, its run
   *  output, or a stated reason. Row click, Enter and the menu's first item. */
  onReveal: (session: AgentSessionRow) => void;
  /** Hide the agent's tab (it keeps running; its row stays, marked hidden). */
  onHideTab?: (session: AgentSessionRow) => void;
  onOpenStory: (story: SessionStory) => void;
  onStop: (session: AgentSessionRow) => void;
  /** Open the launch sheet in the agent's own team, with this agent as the new one's manager. */
  onLaunchUnder?: (session: AgentSessionRow, teamKey: string) => void;
  /** Open this agent's per-tab inbox rail with the composer focused. */
  onMessage?: (session: AgentSessionRow) => void;
  /** Open this agent's per-tab inbox rail on messages (no composer focus). */
  onOpenInbox?: (session: AgentSessionRow) => void;
  /** Open the quick-send popup for this row's agent (of `teamKey`), anchored to the row's button. */
  onQuickSend?: (
    session: AgentSessionRow,
    teamKey: string,
    anchor: HTMLElement,
    ask?: string,
  ) => void;
  /** Open the inbox chat popup for this row's agent (of `teamKey`), anchored to the row's button. */
  onChat?: (session: AgentSessionRow, teamKey: string, anchor: HTMLElement, ask?: string) => void;
}

/** What an agent whose activity is unknown says on hover. */
export const UNKNOWN_ACTIVITY_HINT =
  "Activity unknown: its hooks are not connected, so CodeSense can't tell whether it is working.";

/** Why the arrow filter's workspace shows no agents yet: they are still loading (`bucket`: the
 *  filter is the team's agents with no workspace), or its machine's layout isn't read
 *  (`offline`: the machine reads offline by the one offline rule). */
export type WorkspaceWait =
  | { kind: "loading"; bucket: boolean }
  | { kind: "machine"; machine: string; offline: boolean };

/** The empty list's note while the arrow filter's workspace has not shown its agents. */
function WorkspaceWaitNote({
  wait,
  onClear,
}: {
  wait: WorkspaceWait;
  onClear: () => void;
}): React.JSX.Element {
  if (wait.kind === "loading") {
    return (
      <div
        className="px-[15px] py-3"
        data-testid="agents-workspace-pending"
        role="status"
        aria-busy="true"
      >
        <p className="text-[12px] font-medium text-[var(--ag-text)]">
          {wait.bucket ? "Loading agents" : "Loading this workspace"}
        </p>
        <p className="mt-1 text-[11px] leading-[15px] text-[var(--ag-faint)]">
          {wait.bucket ? "They show" : "Its agents show"} once they have loaded.
        </p>
      </div>
    );
  }
  return (
    <div className="px-[15px] py-3" data-testid="agents-workspace-waiting" role="status">
      <p className="text-[12px] font-medium text-[var(--ag-text)]">Waiting for {wait.machine}</p>
      <p className="mt-1 text-[11px] leading-[15px] text-[var(--ag-faint)]">
        {wait.offline
          ? `This workspace runs on ${wait.machine}, which is offline. Its agents show once it reconnects.`
          : `This workspace runs on ${wait.machine}. Its agents show once its terminals can be read.`}
      </p>
      <button type="button" className="ag-link mt-1.5" onClick={onClear}>
        Clear the workspace filter
      </button>
    </div>
  );
}

/**
 * The agents pane's list: the filter field, then every visible team as team › workspace
 * › agent, or by status. `planAgentsPane` decides what shows; this view draws it and owns only
 * row selection, explicit workspace reveal and the list's arrow-key movement.
 */
export interface AgentsTreeViewProps extends RowActions {
  /** The team a node that names none belongs to (the pane's home team): the default place a row's
   *  team is read from. */
  teamKey: string;
  /** The team the empty state's Launch agent opens the sheet in (the viewer's); defaults to
   *  `teamKey`. */
  launchTeamKey?: string;
  /** Every team with live agents, in the pane's fixed team order, hidden ones too: the whole,
   *  unfiltered trees (the plan says what shows). */
  teams: readonly PaneTeam[];
  /** Every team the user has open (for the Teams checklist, which lists them all). */
  openTeamKeys: readonly string[];
  /** What shows, from `planAgentsPane`. */
  plan: PanePlan;
  hiddenTeams: readonly string[];
  onSetTeamHidden: (teamKey: string, hidden: boolean) => void;
  onShowAllTeams: () => void;
  /** Team headers offer a funnel (more than one team can show). */
  teamFunnel?: boolean;
  /** The team the hide list narrows the pane to, if it leaves only one showing. */
  focusedTeam?: string | null;
  /** A team header's funnel: show only that team, or (when it is the one shown) put back what was
   *  hidden before. */
  onToggleTeamFocus?: (teamKey: string) => void;
  query: string;
  onQueryChange: (query: string) => void;
  status: AgentStatusFilter;
  onStatusChange: (status: AgentStatusFilter) => void;
  workspace: PaneWorkspaceFilter | null;
  /** The filtered workspace's name, from its record. */
  workspaceLabel: string | null;
  /** The filtered workspace has not shown its agents yet: they are still loading, or it runs on
   *  a machine whose layout isn't read. An empty list says so rather than that nothing matches. */
  workspaceWait?: WorkspaceWait | null;
  onWorkspaceChange: (workspace: PaneWorkspaceFilter | null) => void;
  /** Clears the search, the Show filter, the workspace arrow filter and the agent chip, and shows
   *  every team. */
  onClearAll: () => void;
  arrange: AgentListArrange;
  arrangeAnchor?: { key: string; within: number; offset: number } | null;
  onArrangeAnchorConsumed?: () => void;
  railScope?: PaneRailFilter | null;
  onRailScopeChange?: (scope: PaneRailFilter | null) => void;
  onArrangeChange: (arrange: AgentListArrange) => void;
  statusOf: (row: AgentSessionRow) => AgentStatus;
  /** The agents that need a person, by `agentRowKey` (one handle can run in two teams). */
  needs: ReadonlySet<string>;
  /** Folded teams and workspaces, by fold key. */
  collapsed: Readonly<Record<string, boolean>> | undefined;
  onToggleChildren: (key: string, collapse: boolean) => void;
  nowMs: number;
  /** A named agent the list is narrowed to from elsewhere, if any. */
  agentFilterName: string | null;
  onClearAgentFilter: () => void;
  /** The Pinned group, in pin order: each pinned session that runs. */
  pinned?: readonly PinnedEntry[];
  workspaceReveal: { teamKey: string; workspaceId: string; nonce: number } | null;
  onWorkspaceRevealDone: () => void;
}

/** How long a via chip's reveal or an arrow key's focus waits for its row to be drawn. */
const ROW_REQUEST_MS = 1000;
const ROW_REQUEST_RETRY_MS = 16;

// "All" is not a status, so it wears no mark; its slot stays, so the labels keep one column.
const SHOW_OPTIONS: readonly {
  id: AgentStatusFilter;
  label: string;
  mark: StatusMarkKind | null;
}[] = [
  { id: "all", label: "All", mark: null },
  { id: "needs", label: "Needs you", mark: "question" },
  { id: "working", label: "Working", mark: "thinking" },
  { id: "idle", label: "Ready", mark: "ready" },
];

/** What an empty list says when only the Show filter narrows it. */
const SHOW_EMPTY_WORDS: Record<Exclude<AgentStatusFilter, "all">, string> = {
  needs: "No agents need you",
  working: "No agents working",
  idle: "No agents ready",
};

const ARRANGE_OPTIONS: readonly { id: AgentListArrange; label: string }[] = [
  { id: "teams", label: "By team" },
  { id: "status", label: "By status" },
  { id: "manager", label: "By manager" },
];

export function AgentsTreeView({
  teamKey,
  launchTeamKey,
  teams,
  openTeamKeys,
  plan,
  hiddenTeams,
  onSetTeamHidden,
  onShowAllTeams,
  teamFunnel = false,
  focusedTeam = null,
  onToggleTeamFocus,
  query,
  onQueryChange,
  status,
  onStatusChange,
  workspace,
  workspaceLabel,
  workspaceWait = null,
  onWorkspaceChange,
  onClearAll,
  arrange,
  arrangeAnchor,
  onArrangeAnchorConsumed,
  railScope,
  onRailScopeChange,
  onArrangeChange,
  statusOf,
  needs,
  collapsed,
  onToggleChildren,
  nowMs,
  agentFilterName,
  onClearAgentFilter,
  pinned = [],
  workspaceReveal,
  onWorkspaceRevealDone,
  ...actions
}: AgentsTreeViewProps): React.JSX.Element {
  "use no memo";
  const scrollRef = useRef<HTMLDivElement>(null);
  const [savedViewport] = useState(
    () => useViewerStore.getState().agentsPaneViewport ?? { offset: 0, measurements: [] },
  );
  // Routines' queued wakes: the rows' `due` markers.
  useEffect(() => watchRoutineRows(), []);

  // The pane's rows, top to bottom, under the plan. The list virtualizes over them.
  const pinnedExpandedState = useState(false);
  const [pinnedExpanded, setPinnedExpanded] = pinnedExpandedState;
  // Memoized: the pinned list feeds the rows memo, so a new array each render would rebuild every row.
  // By team and handle: one handle can run in two teams, and only the one in view shows.
  const inView = useMemo(
    () =>
      new Set(
        plan.teams.flatMap((t) =>
          t.rows.map((n) => agentRowKey(n.teamKey ?? t.teamKey, n.row.handle)),
        ),
      ),
    [plan.teams],
  );
  // A pinned agent shows while the plan keeps it in view. A home-team node names no team.
  const shownPinned = useMemo(
    () =>
      pinned.flatMap((entry): PinnedEntry[] => {
        const nodesShown = entry.nodes.filter((n) =>
          inView.has(agentRowKey(n.teamKey ?? teamKey, n.row.handle)),
        );
        return nodesShown.length > 0 ? [{ ...entry, nodes: nodesShown }] : [];
      }),
    [pinned, inView, teamKey],
  );
  // Each status row names its own team: the sections span teams, and one handle can run in two.
  const statusReferences = useAgentStatusOrder(arrange === "status");
  const statusRows = useMemo(
    () =>
      arrange === "status"
        ? projectedStatusNodes(
            statusReferences,
            plan.teams.flatMap((team) =>
              team.rows.map((node) => ({ ...node, teamKey: node.teamKey ?? team.teamKey })),
            ),
          )
        : [],
    [arrange, statusReferences, plan.teams],
  );
  // The team's one shared workspace order: the tray's pills, with the person's drags. The pane's
  // cards follow it; a card with no pill keeps its own place.
  const storedOrders = useViewerStore((st) => st.workspaceOrderByTeam);
  const baseOrders = useViewerStore((st) => st.trayBaseOrderByTeam);
  const allWorkspaces = useViewerStore((st) => st.workspaces);
  const softClosed = useViewerStore((st) => st.softClosedWorkspaces);
  const reorderWorkspace = useViewerStore((st) => st.reorderWorkspace);
  // The tray's epic groups, so a newly opened workspace lands beside its own epic as it does there.
  const railKeys = useViewerStore((st) => st.railKeyByWorkspaceId);
  const customGroups = useViewerStore((st) => st.customGroupsByTeam);
  const trayModes = useViewerStore((st) => st.trayMode);
  const orderIdsByTeam = useMemo(() => {
    const out: Record<string, string[]> = {};
    for (const t of plan.teams) {
      out[t.teamKey] = teamWorkspaceOrder({
        base: baseOrders?.[t.teamKey],
        workspaces: allWorkspaces?.[t.teamKey],
        softClosed: softClosed?.[t.teamKey],
        stored: storedOrders?.[t.teamKey],
        groups: railKeys?.[t.teamKey],
      });
    }
    return out;
  }, [plan.teams, baseOrders, allWorkspaces, softClosed, storedOrders, railKeys]);
  // Cards move only in the team arrangement with nothing narrowing the list: a search, Show
  // filter, workspace chip or agent chip hides cards, which would make a drop's place ambiguous.
  const canReorder = arrange === "teams" && !plan.filtering;
  const rowsRef = useRef<readonly PaneRow[]>([]);
  const boxesRef = useRef<() => readonly (RowBox | undefined)[]>(() => []);
  /** A change to animate once the list draws it: each row's start before it, the card it is about,
   *  and whether the move came from a keyboard. */
  const flip = useRef<{
    starts: Map<string, number>;
    movedKey: string;
    keyboard: boolean;
  } | null>(null);
  const recordFlip = useCallback(
    (teamKey: string, workspaceId: string, keyboard: boolean, fromTop?: number) => {
      const rows = rowsRef.current;
      const boxes = boxesRef.current();
      const starts = new Map<string, number>();
      rows.forEach((r, i) => {
        const b = boxes[i];
        if (b) starts.set(r.key, b.start);
      });
      const movedKey = `ws:${workspaceGroupKey(teamKey, workspaceId)}`;
      // A dropped card glides from where it was let go, not from its old place.
      if (fromTop !== undefined) starts.set(movedKey, fromTop - WS_GAP_PX);
      flip.current = { starts, movedKey, keyboard };
    },
    [],
  );
  const [announce, setAnnounce] = useState("");
  const moveCard = useCallback(
    (
      teamKey: string,
      workspaceId: string,
      place: OrderPlace,
      keyboard = false,
      fromTop?: number,
    ) => {
      const rows = rowsRef.current;
      recordFlip(teamKey, workspaceId, keyboard, fromTop);
      if (!reorderWorkspace(teamKey, workspaceId, place)) {
        flip.current = null;
        return;
      }
      const labelOf = (id: string) => {
        const hit = rows.find(
          (r): r is Extract<PaneRow, { kind: "ws" }> =>
            r.kind === "ws" && r.teamKey === teamKey && r.group.workspaceId === id,
        );
        return hit?.group.label ?? id;
      };
      setAnnounce(
        "before" in place
          ? `Moved ${labelOf(workspaceId)} above ${labelOf(place.before)}`
          : `Moved ${labelOf(workspaceId)} below ${labelOf(place.after)}`,
      );
    },
    [reorderWorkspace, recordFlip],
  );
  const { drag: cardDrag, begin: beginCardDrag } = useWorkspaceCardDrag({
    scrollRef,
    rowsRef,
    boxesRef,
    orderOf: (teamKey) => orderIdsByTeam[teamKey] ?? [],
    onDrop: (teamKey, workspaceId, place, previewTop) =>
      moveCard(teamKey, workspaceId, place, false, previewTop),
    onReflow: (teamKey, workspaceId) => recordFlip(teamKey, workspaceId, false),
    // A card that can't move stays a click; dragged past the threshold, it says why.
    refusal: (teamKey, workspaceId) =>
      arrange !== "teams"
        ? CARD_MOVE_REFUSED.byStatus
        : plan.filtering
          ? CARD_MOVE_REFUSED.filtered
          : !(orderIdsByTeam[teamKey] ?? []).includes(workspaceId)
            ? CARD_MOVE_REFUSED.noPill
            : null,
    onRefused: setAnnounce,
  });
  const draggingCard = cardDrag
    ? { teamKey: cardDrag.teamKey, workspaceId: cardDrag.workspaceId }
    : null;
  const draggingTeam = draggingCard?.teamKey ?? null;
  const draggingId = draggingCard?.workspaceId ?? null;
  // A folded team's count says how many of its agents need you, whatever a Show filter, search or
  // chip leaves in view: it is read from the team's whole tree.
  const teamNeedCounts = useMemo(
    () =>
      Object.fromEntries(
        teams.map((t) => [
          t.teamKey,
          flattenNodes(t.nodes).filter((n) =>
            needs.has(agentRowKey(n.teamKey ?? t.teamKey, n.row.handle)),
          ).length,
        ]),
      ),
    [teams, needs],
  );
  const [revealUnfolded, setRevealUnfolded] = useState<string[]>([]);
  const effectiveCollapsed = useMemo(() => {
    const folds = { ...collapsed };
    for (const key of revealUnfolded) delete folds[key];
    return folds;
  }, [collapsed, revealUnfolded]);
  const toggleFold = (key: string, fold: boolean) => {
    setRevealUnfolded((keys) => keys.filter((entry) => entry !== key));
    onToggleChildren(key, fold);
  };
  const paneRows = useMemo(
    () =>
      buildPaneRows({
        pinned: shownPinned,
        pinnedExpanded,
        pinnedVisibleMax: PINNED_VISIBLE_MAX,
        teams: plan.teams,
        arrange,
        statusRows,
        collapsed: effectiveCollapsed,
        needs,
        teamNeedCounts,
        arrowFilter: workspace,
        filtering: plan.freezesFolds,
        workspaceOrder: orderIdsByTeam,
        workspaceRails: railKeys,
        customGroups,
        groupedTeams: trayModes,
        dragging:
          draggingTeam !== null && draggingId !== null
            ? { teamKey: draggingTeam, workspaceId: draggingId }
            : null,
      }),
    [
      shownPinned,
      pinnedExpanded,
      plan,
      arrange,
      statusRows,
      effectiveCollapsed,
      needs,
      teamNeedCounts,
      workspace,
      orderIdsByTeam,
      railKeys,
      trayModes,
      customGroups,
      draggingTeam,
      draggingId,
    ],
  );
  // What each row says to a screen reader (its place in the list, its heading level, an agent's
  // team and workspace), read off the rows so a row the list has not drawn has it too.
  const paneSemantics = useMemo(() => paneRowSemantics(paneRows), [paneRows]);
  // The virtualizer (below) keeps its own mutable state and schedules its own re-renders, so its
  // reads (each row's start, the total size) must not be memoised: the "use no memo" directive at
  // the top of this component opts it out of React Compiler memoization.
  const virtualizer = useVirtualizer({
    count: paneRows.length,
    getScrollElement: () => scrollRef.current,
    initialOffset: savedViewport.offset,
    initialMeasurementsCache: savedViewport.measurements,
    estimateSize: (index) => ROW_ESTIMATE_PX[paneRows[index]?.kind ?? "agent"],
    overscan: 10,
    getItemKey: (index) => paneRows[index]?.key ?? `row:${index}`,
    measureElement: (el) => el.getBoundingClientRect().height,
  });
  useLayoutEffect(() => {
    const root = scrollRef.current;
    if (!root) return;
    root.scrollTop = savedViewport.offset;
    return () => {
      useViewerStore
        .getState()
        .rememberAgentsPaneViewport?.(root.scrollTop, virtualizer.takeSnapshot());
    };
    // The mutable virtualizer is snapshotted only when this list unmounts.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useLayoutEffect(() => {
    if (!arrangeAnchor) return;
    const index = paneRows.findIndex((row) => row.key === arrangeAnchor.key);
    const restore = () => {
      const start = index >= 0 ? virtualizer.measurementsCache[index]?.start : undefined;
      virtualizer.scrollToOffset(
        start === undefined ? arrangeAnchor.offset : Math.max(0, start - arrangeAnchor.within),
      );
    };
    restore();
    // Newly mounted headings and rows settle their measurements in this frame.
    const frame = requestAnimationFrame(() => {
      restore();
      onArrangeAnchorConsumed?.();
    });
    return () => {
      cancelAnimationFrame(frame);
      onArrangeAnchorConsumed?.();
    };
    // Only an explicit arrangement change restores the anchor.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [arrangeAnchor]);

  // The drag reads the rows and their places as last drawn; every row has a place, drawn or not.
  useLayoutEffect(() => {
    rowsRef.current = paneRows;
    boxesRef.current = () => virtualizer.measurementsCache;
  });

  // A moved card settles into place: each drawn row that moved slides from where it was (FLIP),
  // the moved card's agents (folded while it moved) fade in under it.
  // Rows that are not drawn are not animated.
  useLayoutEffect(() => {
    const f = flip.current;
    const root = scrollRef.current;
    if (!f || !root) return;
    flip.current = null;
    // The card left the list (a filter or fold took it): nothing to settle.
    const movedAt = paneRows.findIndex((r) => r.key === f.movedKey);
    if (movedAt < 0) return;
    const reduce =
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const cache = virtualizer.measurementsCache;
    const movedRow = paneRows[movedAt];
    const movedTeam = movedRow?.kind === "ws" ? movedRow.teamKey : null;
    const movedWs = movedRow?.kind === "ws" ? movedRow.group.workspaceId : null;
    for (const el of root.querySelectorAll<HTMLElement>("[data-row-key]")) {
      const index = Number(el.dataset.index);
      const now = cache[index]?.start;
      const key = el.dataset.rowKey!;
      if (now === undefined || typeof el.animate !== "function") continue;
      const was = f.starts.get(key);
      const row = paneRows[index];
      const ownAgent =
        row?.kind === "agent" && row.teamKey === movedTeam && row.workspaceId === movedWs;
      if (reduce) continue;
      if (was === undefined || ownAgent) {
        // Unfolding under the moved card: in from just above, as the card lands.
        el.animate(
          [
            { transform: `translateY(${now - 6}px)`, opacity: 0 },
            { transform: `translateY(${now}px)`, opacity: 1 },
          ],
          { duration: 220, delay: 60, easing: "cubic-bezier(0.16, 1, 0.3, 1)", fill: "backwards" },
        );
      } else if (was !== now) {
        el.animate([{ transform: `translateY(${was}px)` }, { transform: `translateY(${now}px)` }], {
          duration: 240,
          easing: "cubic-bezier(0.16, 1, 0.3, 1)",
        });
      }
    }
    if (f.keyboard) {
      const range = virtualizer.range;
      if (!range || movedAt < range.startIndex || movedAt > range.endIndex) {
        virtualizer.scrollToIndex(movedAt, { align: "auto" });
      }
    }
  }, [paneRows, virtualizer]);

  // The props the rows read are kept stable across renders (each calls the latest handler), so a
  // memoised row re-renders only when its own agent changes.
  const latest = useRef({ actions, statusOf, planTeams: plan.teams });
  useLayoutEffect(() => {
    latest.current = { actions, statusOf, planTeams: plan.teams };
  });
  // A via chip selects its manager, in the manager's own team (the row's: a launcher is of its
  // own team). A fold that hides the manager opens first; the list then scrolls to it once its row
  // is drawn. A filter that hides it says so.
  const revealReq = useRowRequest<
    | { kind: "agent"; teamKey: string; handle: string }
    | { kind: "workspace"; teamKey: string; workspaceId: string }
  >();
  const workspaceNonce = workspaceReveal?.nonce;
  useLayoutEffect(() => {
    if (!workspaceReveal) return;
    if (arrange === "manager") {
      onWorkspaceRevealDone();
      return;
    }
    setRevealUnfolded([
      teamFoldKey(workspaceReveal.teamKey),
      workspaceFoldKey(workspaceReveal.teamKey, workspaceReveal.workspaceId),
    ]);
    revealReq.request({
      kind: "workspace",
      teamKey: workspaceReveal.teamKey,
      workspaceId: workspaceReveal.workspaceId,
    });
    onWorkspaceRevealDone();
    // A workspace click is consumed once, independent of roster and layout updates.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workspaceNonce]);
  const selectHandle = (handle: string, managerTeam: string) => {
    const hit = plan.teams
      .filter((t) => t.teamKey === managerTeam)
      .flatMap((t) => t.rows.map((node) => ({ teamKey: t.teamKey, node })))
      .find((h) => h.node.row.handle === handle);
    if (!hit) {
      const filtering =
        query.trim() !== "" || status !== "all" || workspace !== null || agentFilterName !== null;
      const name = agentNameOf(teams, managerTeam, handle);
      if (filtering) {
        toast(`${name} is hidden by the current search or filter.`, {
          action: { label: "Clear search, filters and teams", onClick: onClearAll },
        });
      } else if (hiddenTeams.includes(managerTeam)) {
        toast(`${name} is in a team the pane hides.`, {
          action: { label: "Show team", onClick: () => onSetTeamHidden(managerTeam, false) },
        });
      } else {
        toast(`${name} is not listed right now.`);
      }
      return;
    }
    const ancestors: string[] = [];
    const launchers = plan.teams.find((team) => team.teamKey === hit.teamKey)?.launchers ?? {};
    for (let parent = launchers[handle]; parent; parent = launchers[parent])
      ancestors.push(managerFoldKey(hit.teamKey, parent));
    setRevealUnfolded([
      ...ancestors,
      teamFoldKey(hit.teamKey),
      workspaceFoldKey(hit.teamKey, hit.node.workspace?.id ?? NO_WORKSPACE_ID),
    ]);
    // The chip picks the manager the way a row click does: its CLI comes up.
    actions.onReveal(hit.node.row);
    revealReq.request({ kind: "agent", teamKey: hit.teamKey, handle });
  };
  const selectHandleRef = useRef(selectHandle);
  useLayoutEffect(() => {
    selectHandleRef.current = selectHandle;
  });
  // Not listed yet (a fold is still opening), the request waits for the rows; listed off screen,
  // the list scrolls to it and looks again a frame later, so a scroll that changed nothing gets
  // another go. The request lapses after ROW_REQUEST_MS.
  const revealWant = revealReq.want;
  const revealDone = revealReq.done;
  const revealLookAgain = revealReq.lookAgain;
  useEffect(() => {
    if (revealWant === null) return;
    const index = paneRows.findIndex(
      (r) =>
        r.kind !== "pinned" &&
        r.kind !== "status-head" &&
        r.kind !== "manager-head" &&
        r.kind !== "rail-head" &&
        r.teamKey === revealWant.teamKey &&
        (revealWant.kind === "workspace"
          ? r.kind === "ws" && r.group.workspaceId === revealWant.workspaceId
          : (r.kind === "agent" || r.kind === "status-agent") &&
            r.node.row.handle === revealWant.handle),
    );
    if (index < 0) {
      if (revealWant.kind === "workspace") {
        revealDone();
        toast("This workspace has no agents in the current view.");
      }
      return;
    }
    const range = virtualizer.range;
    if (range && index >= range.startIndex && index <= range.endIndex) {
      revealDone();
      return;
    }
    virtualizer.scrollToIndex(index, { align: "auto", behavior: "auto" });
    if (revealWant.kind === "workspace") revealDone();
    else return revealLookAgain();
  }, [revealWant, revealReq.look, paneRows, virtualizer, revealDone, revealLookAgain]);

  // Arrow Up and Down move focus between the agent rows. The Pinned group is one row with several
  // stops, so the arrows walk its entries before they leave it. A row that is not drawn yet is
  // scrolled to, and focus follows once it draws (a few tries, then the request is dropped). The
  // request names the row by its key, so a list that reorders or shrinks meanwhile never moves
  // focus to another agent; it lapses after ROW_REQUEST_MS.
  const focusReq = useRowRequest<{ key: string; last: boolean }>();
  const focusWant = focusReq.want;
  const focusDone = focusReq.done;
  const focusLookAgain = focusReq.lookAgain;
  useEffect(() => {
    if (!focusWant) return;
    const row = rowElByKey(scrollRef.current, focusWant.key);
    const target = row ? rowStops(row).at(focusWant.last ? -1 : 0) : undefined;
    if (target) {
      focusDone();
      target.focus({ preventScroll: true });
      return;
    }
    const index = paneRows.findIndex((r) => r.key === focusWant.key);
    if (index < 0) {
      focusDone();
      return;
    }
    virtualizer.scrollToIndex(index, { align: "auto" });
    return focusLookAgain();
  }, [focusWant, focusReq.look, paneRows, virtualizer, focusDone, focusLookAgain]);
  const onListKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (
      (e.key === "ArrowLeft" || e.key === "ArrowRight") &&
      !e.altKey &&
      !e.metaKey &&
      !e.ctrlKey &&
      !e.shiftKey
    ) {
      const focused = (document.activeElement as HTMLElement | null)?.closest<HTMLElement>(
        "[data-row-index]",
      );
      const row = focused ? paneRows[Number(focused.dataset.rowIndex)] : undefined;
      const key =
        row?.kind === "team"
          ? teamFoldKey(row.teamKey)
          : row?.kind === "ws"
            ? workspaceFoldKey(row.teamKey, row.group.workspaceId)
            : row?.kind === "agent" && row.managerCanFold
              ? managerFoldKey(row.teamKey, row.node.row.handle)
              : null;
      if (key && !plan.freezesFolds) {
        e.preventDefault();
        if (!e.repeat) toggleFold(key, e.key === "ArrowLeft");
      }
      return;
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    // Alt+Up/Down on a workspace card's name moves the card one place among its team's cards.
    if (e.altKey && !e.metaKey && !e.ctrlKey && !e.shiftKey) {
      const name = (document.activeElement as HTMLElement | null)?.closest<HTMLElement>(
        ".ag-ws-name",
      );
      const at = Number(name?.closest<HTMLElement>("[data-row-index]")?.dataset.rowIndex);
      const head = Number.isFinite(at) ? paneRows[at] : undefined;
      if (name && head?.kind === "ws") {
        e.preventDefault();
        // Said, not swallowed, as a drag says it: a search or filter hides cards (a move has no
        // clear place), a card with no pill has no place in the order, and one with no pill card
        // that way has nowhere to go.
        if (!canReorder) {
          setAnnounce(CARD_MOVE_REFUSED.filtered);
          return;
        }
        const order = orderIdsByTeam[head.teamKey] ?? [];
        if (!order.includes(head.group.workspaceId)) {
          setAnnounce(CARD_MOVE_REFUSED.noPill);
          return;
        }
        const step = stepSlot(
          teamSlotCards(paneRows, head.teamKey, order),
          head.group.workspaceId,
          e.key === "ArrowUp" ? "up" : "down",
        );
        if (step) moveCard(head.teamKey, head.group.workspaceId, step.place, true);
        else setAnnounce(CARD_MOVE_REFUSED.nowhere);
        return;
      }
    }
    const active = document.activeElement as HTMLElement | null;
    const focused = active?.closest<HTMLElement>("[data-row-index]");
    if (!active || !focused) return;
    const step = e.key === "ArrowDown" ? 1 : -1;
    e.preventDefault();
    if (focused.dataset.rowKind === "pinned") {
      const stops = rowStops(focused);
      // A row's own controls (its hover actions) step from that row's stop; from a control of
      // the group outside its rows (its "+n more" link), the arrows leave the group as from any
      // row.
      const own = active.closest<HTMLElement>(".ag-row")?.querySelector<HTMLElement>(".ag-main");
      const from = stops.indexOf(own ?? active);
      const nextStop = from < 0 ? undefined : stops[from + step];
      if (nextStop) {
        nextStop.focus({ preventScroll: true });
        nextStop.scrollIntoView?.({ block: "nearest" });
        return;
      }
    }
    const at = Number(focused.dataset.rowIndex);
    let next = at + step;
    while (next >= 0 && next < paneRows.length && !isAgentRow(paneRows[next]!)) next += step;
    if (next < 0 || next >= paneRows.length) return;
    const row = scrollRef.current?.querySelector<HTMLElement>(`[data-row-index="${next}"]`);
    const target = row ? rowStops(row).at(step < 0 ? -1 : 0) : undefined;
    if (target) target.focus({ preventScroll: true });
    else focusReq.request({ key: paneRows[next]!.key, last: step < 0 });
    virtualizer.scrollToIndex(next, { align: "auto" });
  };

  const identityNow = useContext(Identity);
  const identityRef = useRef(identityNow);
  useLayoutEffect(() => {
    identityRef.current = identityNow;
  });
  // One row popup at a time: the quick view (the agent's CLI) or the inbox chat.
  const [quickSend, setQuickSend] = useState<{
    kind: "cli" | "chat";
    row: AgentSessionRow;
    /** The agent's own team: one handle can run in two. */
    teamKey: string;
    ask: string | null;
    label: string;
    status: AgentStatus;
    workspace: string | null;
    anchor: HTMLElement;
    /** The list row the anchor sat in, so focus can find the row again once it is redrawn. */
    rowKey: string | null;
  } | null>(null);
  // Which optional actions the rows offer: fixed by the caller, so the stable set below is too.
  const offers = {
    hideTab: actions.onHideTab !== undefined,
    launchUnder: actions.onLaunchUnder !== undefined,
    message: actions.onMessage !== undefined,
    openInbox: actions.onOpenInbox !== undefined,
  };
  const rowActions = useMemo((): RowActions => {
    const now = () => latest.current.actions;
    const openPopup = (
      kind: "cli" | "chat",
      row: AgentSessionRow,
      rowTeam: string,
      anchor: HTMLElement,
      ask?: string,
    ) => {
      const { planTeams, statusOf: statusNow } = latest.current;
      const node = planNode(planTeams, rowTeam, row.handle);
      setQuickSend({
        kind,
        row,
        teamKey: rowTeam,
        ask: ask ?? null,
        label: agentRowLabel(row),
        status: statusNow(row),
        workspace: node?.workspace?.label ?? null,
        anchor,
        rowKey: anchor.closest<HTMLElement>("[data-row-key]")?.dataset.rowKey ?? null,
      });
    };
    return {
      onReveal: (row) => now().onReveal(row),
      onOpenStory: (story) => now().onOpenStory(story),
      onStop: (row) => now().onStop(row),
      onHideTab: offers.hideTab ? (row) => now().onHideTab?.(row) : undefined,
      onLaunchUnder: offers.launchUnder
        ? (row, rowTeam) => now().onLaunchUnder?.(row, rowTeam)
        : undefined,
      onMessage: offers.message ? (row) => now().onMessage?.(row) : undefined,
      onOpenInbox: offers.openInbox ? (row) => now().onOpenInbox?.(row) : undefined,
      onQuickSend: (row, rowTeam, anchor, ask) => openPopup("cli", row, rowTeam, anchor, ask),
      onChat: (row, rowTeam, anchor, ask) => openPopup("chat", row, rowTeam, anchor, ask),
    };
  }, [offers.hideTab, offers.launchUnder, offers.message, offers.openInbox]);
  const stableSelectHandle = useCallback(
    (handle: string, managerTeam: string) => selectHandleRef.current(handle, managerTeam),
    [],
  );
  const togglePin = useCallback(
    (rowTeam: string, row: AgentSessionRow, label: string) =>
      identityRef.current.togglePin(rowTeam, row, label),
    [],
  );
  // Opening the agent's tab in the CLI while its popup is open: the popup closes, so its terminal
  // is in one place only.
  const quickSendSession = quickSend?.row.sessionId ?? null;
  const quickSendTeam = quickSend?.teamKey ?? null;
  const quickSendHandle = quickSend?.row.handle ?? null;
  // The popup reads the agent's live row, so its status and session follow the list. It is read
  // from the whole trees, not the plan: a fold, a drag, a Show filter or a search that hides its
  // row leaves the agent running, so none of them closes the popup.
  const quickNode = quickSend ? teamNode(teams, quickSend.teamKey, quickSend.row.handle) : null;
  const quickRow = quickNode?.row ?? quickSend?.row;
  // The popup's header says the status as the row does (the same read), with the subagent mark.
  const quickStatus: AgentStatus = quickRow ? statusOf(quickRow) : "unknown";
  const quickSubagents = shownSubagents(quickStatus, useSubagentsOpen(quickRow?.sessionId));
  // Only the agent leaving its team's tree (it ended, or moved team) closes it.
  useEffect(() => {
    if (
      quickSendHandle !== null &&
      quickSendTeam !== null &&
      teamNode(teams, quickSendTeam, quickSendHandle) === null
    ) {
      setQuickSend(null);
    }
  }, [teams, quickSendTeam, quickSendHandle]);
  // Only a change to the agent's tab closes the popup: a tab already on screen when the popup opens
  // stays, and the popup shows its link.
  const previousCurrent = useRef(identityNow.currentSessionId);
  useEffect(() => {
    const before = previousCurrent.current;
    previousCurrent.current = identityNow.currentSessionId;
    if (
      quickSendSession !== null &&
      identityNow.currentSessionId === quickSendSession &&
      before !== quickSendSession
    ) {
      setQuickSend(null);
    }
  }, [identityNow.currentSessionId, quickSendSession]);
  // The open popup as last drawn, so closing it can give focus back to its row outside a state
  // update (an updater must stay free of side effects).
  const quickSendRef = useRef(quickSend);
  useLayoutEffect(() => {
    quickSendRef.current = quickSend;
  });
  const closeQuickSend = useCallback((restoreFocus: boolean) => {
    const open = quickSendRef.current;
    quickSendRef.current = null;
    setQuickSend(null);
    if (!open || !restoreFocus) return;
    // The list draws only the rows in view: the anchor may have been unmounted while the popup
    // was open. Focus goes to the agent's row as drawn now, else to an agent row in view (so the
    // arrow keys carry on from there), else to the list, never a detached node.
    const root = scrollRef.current;
    const target = open.anchor.isConnected
      ? open.anchor
      : (agentButtonIn(rowElByKey(root, open.rowKey ?? ""), open.row.handle) ??
        (root ? rowInView(root) : null) ??
        root);
    target?.focus({ preventScroll: true });
  }, []);
  const agentNames = useMemo(() => {
    const names = new Map<string, string>();
    for (const team of teams) {
      for (const node of flattenNodes(team.nodes)) {
        const name = agentRowLabel(node.row);
        names.set(`${node.teamKey ?? team.teamKey}|${node.row.handle}`, name);
        if (!names.has(`|${node.row.handle}`)) names.set(`|${node.row.handle}`, name);
      }
    }
    return names;
  }, [teams]);
  const tree: TreeProps = {
    nowMs,
    needs,
    statusOf,
    actions: rowActions,
    onSelectHandle: stableSelectHandle,
    onTogglePin: togglePin,
    nameOf: (rowTeam, handle) =>
      agentNames.get(`${rowTeam ?? ""}|${handle}`) ?? agentNames.get(`|${handle}`),
    multiTeam: teams.length > 1,
  };

  const anyAgents = teams.some((t) => t.nodes.length > 0);
  // The agent chip's agent has nothing running in the teams in view, and no other filter is on:
  // the list says so, as it does with no agents at all, or that it runs only in a hidden team.
  const chipEmpty =
    agentFilterName !== null &&
    plan.visible === 0 &&
    query.trim() === "" &&
    status === "all" &&
    workspace === null &&
    !railScope;
  // Every team the Teams checklist lists is unchecked. An empty open team still checked shows
  // the pane's usual empty state instead.
  const checklistTeams = [...new Set([...openTeamKeys, ...teams.map((t) => t.teamKey)])].filter(
    (k) => k !== NO_TEAM_SENTINEL,
  );
  const everyTeamHidden =
    checklistTeams.length > 0 && checklistTeams.every((k) => hiddenTeams.includes(k));
  // Nothing in view and nothing narrows it: the teams shown have no agents (theirs run in teams
  // the checklist hides).
  const noAgentsShown = plan.visible === 0 && !plan.filtering && shownPinned.length === 0;
  // The hidden teams that have agents: with none shown, the empty state offers to show them.
  const hiddenTeamsWithAgents = noAgentsShown
    ? teams
        .filter((t) => t.nodes.length > 0 && hiddenTeams.includes(t.teamKey))
        .map((t) => t.teamKey)
    : [];
  // Only the Show filter narrows the list: an empty list says which bucket is empty.
  const showFilterOnly =
    status !== "all" &&
    query.trim() === "" &&
    workspace === null &&
    agentFilterName === null &&
    !railScope;
  // The chip's agent runs only in hidden teams: its note and its Clear agent filter win over
  // "Every team is hidden", which offers neither.
  const chipInHiddenTeams = chipEmpty && plan.chipHiddenTeams.length > 0;

  return (
    <>
      {onRailScopeChange && (
        <label className="ag-group-scope">
          <span>Group</span>
          <select
            aria-label="Workspace group scope"
            value={railScope ? JSON.stringify([railScope.teamKey, railScope.railKey]) : ""}
            onChange={(e) => {
              if (!e.target.value) onRailScopeChange(null);
              else {
                const [teamKey, railKey] = JSON.parse(e.target.value) as [string, string];
                onRailScopeChange({ teamKey, railKey });
              }
            }}
          >
            <option value="">All workspace groups</option>
            {railScope &&
              (!checklistTeams.includes(railScope.teamKey) ||
                ![
                  ...Object.values(railKeys?.[railScope.teamKey] ?? {}),
                  ...(customGroups?.[railScope.teamKey] ?? []).map((g) => customRailKey(g.id)),
                ].includes(railScope.railKey)) && (
                <option value={JSON.stringify([railScope.teamKey, railScope.railKey])}>
                  {identityNow.teamLabelOf(railScope.teamKey)} ·{" "}
                  {
                    railInfo(
                      railScope.teamKey,
                      railScope.railKey,
                      customGroups?.[railScope.teamKey],
                    ).title
                  }{" "}
                  (no workspaces)
                </option>
              )}
            {checklistTeams.flatMap((teamKey) =>
              [
                ...new Set([
                  ...Object.values(railKeys?.[teamKey] ?? {}),
                  ...(customGroups?.[teamKey] ?? []).map((g) => customRailKey(g.id)),
                ]),
              ].map((key) => {
                const info = railInfo(teamKey, key, customGroups?.[teamKey]);
                return (
                  <option
                    key={JSON.stringify([teamKey, key])}
                    value={JSON.stringify([teamKey, key])}
                  >
                    {identityNow.teamLabelOf(teamKey)} · {info.title}
                    {info.kind === "custom" ? " (custom group)" : ""}
                  </option>
                );
              }),
            )}
          </select>
        </label>
      )}
      <FilterField
        query={query}
        onQueryChange={onQueryChange}
        status={status}
        onStatusChange={onStatusChange}
        plan={plan}
        teams={teams}
        openTeamKeys={openTeamKeys}
        hiddenTeams={hiddenTeams}
        onSetTeamHidden={onSetTeamHidden}
        onShowAllTeams={onShowAllTeams}
        focusedTeam={focusedTeam}
        onToggleTeamFocus={onToggleTeamFocus}
        workspace={workspace}
        workspaceLabel={workspaceLabel}
        onWorkspaceChange={onWorkspaceChange}
        arrange={arrange}
        onArrangeChange={onArrangeChange}
        agentFilterName={agentFilterName}
        onClearAgentFilter={onClearAgentFilter}
      />
      <div
        ref={scrollRef}
        style={{ scrollBehavior: "auto" }}
        className="ag-scroll flex-1 min-h-0 overflow-y-auto pb-3"
        // Focus can land on the list itself when the row it was given back to is not drawn.
        tabIndex={-1}
        onKeyDown={onListKeyDown}
      >
        {plan.visible === 0 &&
        shownPinned.length === 0 &&
        workspaceWait &&
        query.trim() === "" &&
        status === "all" &&
        agentFilterName === null ? (
          <WorkspaceWaitNote wait={workspaceWait} onClear={() => onWorkspaceChange(null)} />
        ) : ((!anyAgents && !railScope) || chipEmpty || noAgentsShown) &&
          (!everyTeamHidden || chipInHiddenTeams) ? (
          <>
            {paneRows.map((row, index) =>
              row.kind === "pinned" ? (
                // The arrow keys find a row by its index and kind, as in the list below.
                <div
                  key={row.key}
                  data-row-index={index}
                  data-row-kind={row.kind}
                  data-row-key={row.key}
                >
                  <PaneRowView
                    row={row}
                    semantics={paneSemantics[index]}
                    tree={tree}
                    needs={needs}
                    filtering={plan.freezesFolds}
                    onWorkspaceChange={onWorkspaceChange}
                    onToggleChildren={toggleFold}
                    onTogglePinnedExpanded={() => setPinnedExpanded((open) => !open)}
                  />
                </div>
              ) : null,
            )}
            <EmptyState
              filtered={chipEmpty ? agentFilterName : null}
              hiddenTeams={chipEmpty ? plan.chipHiddenTeams : hiddenTeamsWithAgents}
              onShowTeam={(team) => onSetTeamHidden(team, false)}
              onClearAgentFilter={onClearAgentFilter}
              onLaunch={() => openLaunchSheet({ teamKey: launchTeamKey ?? teamKey })}
            />
          </>
        ) : plan.visible === 0 && everyTeamHidden ? (
          <div className="px-[15px] py-3" data-testid="agents-all-hidden">
            <p className="text-[12px] font-medium text-[var(--ag-text)]">Every team is hidden</p>
            <p className="mt-1 text-[11px] leading-[15px] text-[var(--ag-faint)]">
              Show a team from the Teams checklist.
            </p>
            <button type="button" className="ag-link mt-1.5" onClick={onShowAllTeams}>
              Show all teams
            </button>
          </div>
        ) : plan.visible === 0 && shownPinned.length === 0 && showFilterOnly ? (
          <div className="px-[15px] py-3" data-testid="agents-show-empty">
            <p className="text-[12px] font-medium text-[var(--ag-text)]">
              {SHOW_EMPTY_WORDS[status as Exclude<AgentStatusFilter, "all">]}
            </p>
            <p className="mt-1 text-[11px] leading-[15px] text-[var(--ag-faint)]">
              The Show filter lists only these agents.
            </p>
            <button type="button" className="ag-link mt-1.5" onClick={() => onStatusChange("all")}>
              Show all agents
            </button>
          </div>
        ) : plan.visible === 0 && shownPinned.length === 0 ? (
          <div className="px-[15px] py-3" data-testid="agents-no-match">
            <p className="text-[12px] font-medium text-[var(--ag-text)]">Nothing matches</p>
            <p className="mt-1 text-[11px] leading-[15px] text-[var(--ag-faint)]">
              No agent in these teams matches the current search and filters.
            </p>
            <button type="button" className="ag-link mt-1.5" onClick={onClearAll}>
              Clear search, filters and teams
            </button>
          </div>
        ) : (
          <>
            <div
              role="list"
              aria-label="Agents"
              style={{ height: virtualizer.getTotalSize(), position: "relative", width: "100%" }}
            >
              {cardDrag && cardDrag.lineY !== null && (
                <div
                  className="ag-drop-line"
                  data-testid="agents-drop-line"
                  aria-hidden
                  style={{ top: cardDrag.lineY }}
                />
              )}
              {cardDrag && (
                <CardDragPreview
                  row={paneRows.find(
                    (r): r is Extract<PaneRow, { kind: "ws" }> =>
                      r.kind === "ws" &&
                      r.teamKey === cardDrag.teamKey &&
                      r.group.workspaceId === cardDrag.workspaceId,
                  )}
                  top={cardDrag.previewTop}
                  refused={cardDrag.outside}
                  statusOf={statusOf}
                />
              )}
              {virtualizer.getVirtualItems().map((virtualRow) => {
                const row = paneRows[virtualRow.index];
                const semantics = paneSemantics[virtualRow.index];
                if (!row || !semantics) return null;
                return (
                  <div
                    key={virtualRow.key}
                    role="listitem"
                    // Only the drawn rows are in the DOM: each says where it sits in the whole list.
                    aria-setsize={semantics.setSize}
                    aria-posinset={semantics.posInSet}
                    data-index={virtualRow.index}
                    data-row-index={virtualRow.index}
                    data-row-kind={row.kind}
                    data-row-key={row.key}
                    data-ws-key={
                      row.kind === "agent"
                        ? workspaceGroupKey(row.teamKey, row.workspaceId ?? "")
                        : undefined
                    }
                    data-status-section={row.kind === "status-agent" ? row.section : undefined}
                    ref={virtualizer.measureElement}
                    style={{
                      position: "absolute",
                      top: 0,
                      left: 0,
                      width: "100%",
                      transform: `translateY(${virtualRow.start}px)`,
                      // The list measures this element, so the card's air above and below lives
                      // here: a padding inside the measured box is counted, a child's is not.
                      ...(row.kind === "ws"
                        ? { paddingBlock: `${WS_GAP_PX}px ${WS_GAP_BELOW_PX}px` }
                        : null),
                      // A team header gets air above it, so it does not sit against the team above.
                      // The first header sits just under the find field.
                      ...(row.kind === "team"
                        ? {
                            paddingTop:
                              virtualRow.index === 0
                                ? FIRST_TEAM_GAP_PX
                                : paneRows[virtualRow.index - 1]?.kind === "pinned"
                                  ? PINNED_TEAM_GAP_PX
                                  : paneRows[virtualRow.index - 1]?.kind === "team"
                                    ? FOLDED_TEAM_GAP_PX
                                    : TEAM_GAP_PX,
                          }
                        : null),
                      // A status section after the first opens with air, like a card does.
                      ...(row.kind === "status-head" && virtualRow.index > 0
                        ? { paddingTop: STATUS_GAP_PX }
                        : null),
                    }}
                  >
                    <PaneRowView
                      row={row}
                      semantics={semantics}
                      tree={tree}
                      needs={needs}
                      teamFocus={
                        teamFunnel && onToggleTeamFocus
                          ? { focused: focusedTeam, onToggle: onToggleTeamFocus }
                          : undefined
                      }
                      filtering={plan.freezesFolds}
                      reorder={
                        row.kind === "ws" && row.group.workspaceId !== NO_WORKSPACE_ID
                          ? beginCardDrag
                          : undefined
                      }
                      movable={
                        row.kind === "ws" &&
                        canReorder &&
                        (orderIdsByTeam[row.teamKey] ?? []).includes(row.group.workspaceId)
                      }
                      onWorkspaceChange={onWorkspaceChange}
                      onToggleChildren={toggleFold}
                      onTogglePinnedExpanded={() => setPinnedExpanded((open) => !open)}
                    />
                  </div>
                );
              })}
            </div>
          </>
        )}
      </div>
      <div className="sr-only" role="status" aria-live="polite" data-testid="agents-move-status">
        {announce}
      </div>
      {quickSend?.kind === "chat" && (
        <InboxChatPopover
          // The pair as JSON: a team key or a handle can hold the separator a plain join uses.
          key={`chat:${JSON.stringify([quickSend.teamKey, quickSend.row.handle])}`}
          name={quickSend.label}
          statusWord={agentStatusWords(quickStatus, quickSubagents)}
          ask={quickSend.ask}
          workspace={quickNode?.workspace?.label ?? quickSend.workspace}
          handle={quickSend.row.handle}
          teamKey={quickSend.teamKey}
          offlineMachine={identityNow.offlineMachineOf(quickRow ?? quickSend.row)}
          anchor={quickSend.anchor}
          onClose={closeQuickSend}
          onOpenInCli={
            (quickRow?.sessionId ?? quickSend.row.sessionId)
              ? () => {
                  const row = quickRow ?? quickSend.row;
                  closeQuickSend(false);
                  // The CLI already shows this agent: only its inbox opens.
                  if (row.sessionId !== identityNow.currentSessionId) actions.onReveal(row);
                  actions.onOpenInbox?.(row);
                }
              : null
          }
        />
      )}
      {quickSend?.kind === "cli" && (
        <QuickSendPopover
          key={`cli:${JSON.stringify([quickSend.teamKey, quickSend.row.handle])}`}
          name={quickSend.label}
          face={resolveAgentFace(quickRow ?? quickSend.row, {
            name: quickSend.label,
            choice: identityNow.choicesOf(quickSend.teamKey)[quickSend.row.handle],
            faces: identityNow.facesOf(quickSend.teamKey),
          })}
          mark={
            identityNow.offlineMachineOf(quickRow ?? quickSend.row)
              ? "offline"
              : markOfAgentStatus(quickStatus)
          }
          bounds={appPopoverBounds}
          statusWord={agentStatusWord(quickStatus, quickSubagents)}
          nativeSubagents={quickSubagents}
          ask={quickSend.ask}
          workspace={quickNode?.workspace?.label ?? quickSend.workspace}
          offlineMachine={identityNow.offlineMachineOf(quickRow ?? quickSend.row)}
          // An agent on an offline machine has no live terminal to type into.
          sessionId={
            identityNow.offlineMachineOf(quickRow ?? quickSend.row)
              ? null
              : (quickRow?.sessionId ?? quickSend.row.sessionId)
          }
          anchor={quickSend.anchor}
          onClose={closeQuickSend}
          onShowInCli={() => {
            // Close the popup first, so its terminal is given back before the CLI takes the tab.
            closeQuickSend(false);
            // The live row: the session the popup opened on may have moved on.
            actions.onReveal(quickRow ?? quickSend.row);
          }}
        />
      )}
    </>
  );
}

/** The name an agent of `teamKey` shows, found among the teams' nodes; the handle when it is not
 *  listed at all. */
function agentNameOf(teams: readonly PaneTeam[], teamKey: string, handle: string): string {
  for (const team of teams) {
    for (const node of flattenNodes(team.nodes)) {
      if ((node.teamKey ?? team.teamKey) === teamKey && node.row.handle === handle) {
        return agentRowLabel(node.row);
      }
    }
  }
  return handle;
}

/** The node of `teamKey`'s agent `handle` in the whole trees, whatever the filters leave in view
 *  (one handle can run in two teams). */
function teamNode(
  teams: readonly PaneTeam[],
  teamKey: string,
  handle: string,
): AgentTreeNode | null {
  for (const team of teams) {
    for (const node of flattenNodes(team.nodes)) {
      if ((node.teamKey ?? team.teamKey) === teamKey && node.row.handle === handle) return node;
    }
  }
  return null;
}

/** The node of `teamKey`'s agent `handle` the plan keeps in view, drawn or not (one handle can
 *  run in two teams): a folded card, a folded team or a card being dragged still holds it. */
function planNode(
  teams: readonly PlannedTeam[],
  teamKey: string,
  handle: string,
): AgentTreeNode | null {
  for (const team of teams) {
    for (const node of team.rows) {
      if ((node.teamKey ?? team.teamKey) === teamKey && node.row.handle === handle) return node;
    }
  }
  return null;
}

/** The controls a row's arrow stops land on: an agent's row button (a Pinned group has one per
 *  pinned agent). */
function rowStops(row: HTMLElement): HTMLElement[] {
  return Array.from(row.querySelectorAll<HTMLElement>(".ag-row .ag-main"));
}

/** The drawn workspace group with this key, if any. */

/** An agent's own row button inside a drawn list row (a Pinned row holds several). */
function agentButtonIn(row: HTMLElement | undefined, handle: string): HTMLElement | undefined {
  return (
    Array.from(row?.querySelectorAll<HTMLElement>(".ag-row") ?? [])
      .find((el) => el.dataset.agentHandle === handle)
      ?.querySelector<HTMLElement>(".ag-main") ?? undefined
  );
}

/** A request to act on a list row once it is drawn. `lookAgain` asks for another look a frame
 *  later (it returns the timer's cleanup); the request lapses after {@link ROW_REQUEST_MS}. */
function useRowRequest<T extends object>(): {
  want: T | null;
  look: number;
  request: (want: T) => void;
  done: () => void;
  lookAgain: () => () => void;
} {
  const [want, setWant] = useState<T | null>(null);
  const [look, setLook] = useState(0);
  const request = useCallback((next: T) => setWant({ ...next }), []);
  const done = useCallback(() => setWant(null), []);
  const lookAgain = useCallback(() => {
    const timer = window.setTimeout(() => setLook((n) => n + 1), ROW_REQUEST_RETRY_MS);
    return () => window.clearTimeout(timer);
  }, []);
  useEffect(() => {
    if (want === null) return;
    const timer = window.setTimeout(
      () => setWant((now) => (now === want ? null : now)),
      ROW_REQUEST_MS,
    );
    return () => window.clearTimeout(timer);
  }, [want]);
  return { want, look, request, done, lookAgain };
}

/** The drawn row with this key, if any. */
function rowElByKey(root: HTMLElement | null, key: string): HTMLElement | undefined {
  if (!root) return undefined;
  return Array.from(root.querySelectorAll<HTMLElement>("[data-row-key]")).find(
    (el) => el.dataset.rowKey === key,
  );
}

/** The rows an arrow key can land on: an agent, pinned or in the tree, or a status section's agent. */
function isAgentRow(row: PaneRow): boolean {
  return row.kind === "agent" || row.kind === "status-agent" || row.kind === "pinned";
}

/** The name of the agent that launched a node's agent, when that agent is listed. */
function launcherNameOf(tree: TreeProps, node: AgentTreeNode): string | undefined {
  const by = node.row.launchedBy;
  return by ? tree.nameOf(node.teamKey, by) : undefined;
}

/** Each row kind's height before it is measured, in px. */
const ROW_ESTIMATE_PX: Record<PaneRow["kind"], number> = {
  pinned: 120,
  team: 30,
  ws: 34,
  agent: 52,
  "manager-head": 26,
  "rail-head": 26,
  "status-head": 26,
  "status-agent": 52,
};

/** The air above and below a workspace card, inside the row the list measures. */
const WS_GAP_PX = 10;
/** The air below a workspace card, a little less than above it. */
const WS_GAP_BELOW_PX = 6;
/** The air above a team header: more than above a card, so teams read as the larger break. */
const TEAM_GAP_PX = 18;
/** The air above the list's first team header, under the find field. */
const FIRST_TEAM_GAP_PX = 4;
/** The air above the first team header under the Pinned group, whose rows already end in air. */
const PINNED_TEAM_GAP_PX = 12;
/** The air above a team header right under a folded one: folded teams stack as a short list. */
const FOLDED_TEAM_GAP_PX = 4;
/** The air above a status section's heading, after the first. */
const STATUS_GAP_PX = 8;

/** A header row's heading, at the level the row plan gives it: the pane's groups are h3, a
 *  workspace card under its team h4. */
function PaneHeading({
  level,
  ...props
}: { level: 3 | 4 | null | undefined } & React.HTMLAttributes<HTMLHeadingElement> & {
    "data-status"?: string;
  }): React.JSX.Element {
  return level === 4 ? <h4 {...props} /> : <h3 {...props} />;
}

/** One row of the pane: a header, a pinned agent, or an agent in the tree or a status section. */
function PaneRowView({
  row,
  semantics,
  tree,
  needs,
  filtering,
  reorder,
  movable,
  onWorkspaceChange,
  onToggleChildren,
  onTogglePinnedExpanded,
  teamFocus,
}: {
  row: PaneRow;
  /** The row's heading level and its agents' places, from {@link paneRowSemantics}. */
  semantics: PaneRowSemantics | undefined;
  tree: TreeProps;
  needs: ReadonlySet<string>;
  /** Team headers' funnel, when the pane offers one. */
  teamFocus?: { focused: string | null; onToggle: (teamKey: string) => void };
  /** Folds are frozen (a search or the workspace chip): headers cannot fold now. */
  filtering: boolean;
  /** A workspace card that can be dragged to a new place: starts the press on its head row. */
  reorder?: (e: React.PointerEvent<HTMLElement>, teamKey: string, workspaceId: string) => void;
  /** The card can move now, so it wears the drag hints (grip, title, Alt+arrow shortcut). */
  movable?: boolean;
  onWorkspaceChange: (workspace: PaneWorkspaceFilter | null) => void;
  onToggleChildren: (key: string, collapse: boolean) => void;
  onTogglePinnedExpanded: () => void;
}): React.JSX.Element | null {
  const { nowMs, statusOf } = tree;
  const identity = useContext(Identity);
  const pinnedLabelId = useId();
  const isLight = useIsLightViewerTheme();
  const level = semantics?.headingLevel;
  // An agent row's place in words: its team, workspace and launcher, which the flat list's
  // nesting cannot say.
  const whereOf = (place: PaneRowPlace | undefined): string | undefined =>
    place &&
    paneRowPlaceWords(place, {
      teamLabel: identity.teamLabelOf(place.teamKey),
      underName: place.underHandle ? tree.nameOf(place.teamKey, place.underHandle) : null,
    });
  switch (row.kind) {
    case "pinned":
      return (
        <div className="ag-group pt-1" data-testid="agents-pinned">
          <PaneHeading level={level} className="ag-group-label" id={pinnedLabelId}>
            Pinned
          </PaneHeading>
          <ul aria-labelledby={pinnedLabelId}>
            {row.entries.map(({ key, node }, i) => (
              <li key={key}>
                <AgentListRow
                  where={whereOf(semantics?.places[i])}
                  node={node}
                  facts={rowFactsOf(identity, node)}
                  status={statusOf(node.row)}
                  depth={0}
                  elapsed={formatElapsed(node.row.startedAtMs, nowMs)}
                  nowMs={statusOf(node.row) === "stalled" ? nowMs : 0}
                  inPinned
                  onSelectHandle={tree.onSelectHandle}
                  onTogglePin={tree.onTogglePin}
                  {...tree.actions}
                />
              </li>
            ))}
          </ul>
          {(row.more > 0 || row.expanded) && (
            <button
              type="button"
              className="ag-link px-[15px] py-1"
              onClick={onTogglePinnedExpanded}
            >
              {row.expanded ? "Show fewer pinned" : `+${row.more} more pinned`}
            </button>
          )}
        </div>
      );
    case "team":
      return (
        <TeamHead
          headingLevel={level}
          teamKey={row.teamKey}
          needCount={row.needCount}
          folded={row.folded}
          frozen={filtering}
          onToggleFold={(folded) => onToggleChildren(teamFoldKey(row.teamKey), folded)}
          funnelOn={teamFocus?.focused === row.teamKey}
          onFunnel={teamFocus ? () => teamFocus.onToggle(row.teamKey) : undefined}
        />
      );
    case "ws":
      return (
        <WorkspaceHead
          headingLevel={level}
          teamKey={row.teamKey}
          group={row.group}
          folded={row.folded}
          frozen={filtering}
          arrowOn={row.arrowOn}
          dragging={row.dragging}
          onReorderPress={
            reorder ? (e) => reorder(e, row.teamKey, row.group.workspaceId) : undefined
          }
          movable={!!movable}
          onToggleFold={(folded) =>
            onToggleChildren(workspaceFoldKey(row.teamKey, row.group.workspaceId), folded)
          }
          onArrow={() => {
            onWorkspaceChange(
              row.arrowOn ? null : { teamKey: row.teamKey, workspaceId: row.group.workspaceId },
            );
          }}
          tree={tree}
          needs={needs}
        />
      );
    case "agent":
      return (
        <AgentListRow
          where={whereOf(semantics?.places[0])}
          node={row.node}
          facts={rowFactsOf(identity, row.node, row.teamKey)}
          status={statusOf(row.node.row)}
          depth={row.depth}
          inWorkspace={row.managerCanFold === undefined && row.workspaceId !== null}
          managerView={row.managerCanFold !== undefined}
          treeAccent={
            row.treeHandle
              ? getManagedTreeAccent(`${row.teamKey}|${row.treeHandle}`, isLight)
              : undefined
          }
          managerFolded={row.managerFolded}
          foldedNeeds={
            flattenRows(row.node.children).filter((child) =>
              needs.has(agentRowKey(row.teamKey, child.handle)),
            ).length
          }
          onFoldManager={
            row.managerCanFold
              ? () =>
                  onToggleChildren(
                    managerFoldKey(row.teamKey, row.node.row.handle),
                    !row.managerFolded,
                  )
              : undefined
          }
          managerFoldsFrozen={filtering}
          chosenManagerName={
            row.managerCanFold !== undefined &&
            row.node.parentSource === "chosen" &&
            row.parentHandle
              ? (tree.nameOf(row.teamKey, row.parentHandle) ?? row.parentHandle)
              : undefined
          }
          elapsed={formatElapsed(row.node.row.startedAtMs, nowMs)}
          nowMs={statusOf(row.node.row) === "stalled" ? nowMs : 0}
          // A row drawn under the agent that launched it needs no "via": the indent says it.
          showManagerPill={
            row.managerCanFold !== undefined || row.node.row.launchedBy !== row.parentHandle
          }
          launcherName={launcherNameOf(tree, row.node)}
          onSelectHandle={tree.onSelectHandle}
          onTogglePin={tree.onTogglePin}
          {...tree.actions}
        />
      );
    case "rail-head":
      return (
        <PaneHeading
          level={level}
          className="ag-rail-head"
          data-group-kind={row.railKind}
          aria-label={row.railKind === "custom" ? `Custom group ${row.title}` : undefined}
        >
          {row.title}
        </PaneHeading>
      );
    case "manager-head":
      return (
        <PaneHeading level={level} className="ag-rail-head">
          {row.label}
        </PaneHeading>
      );
    case "status-head":
      return (
        <PaneHeading level={level} className="ag-status-head" data-status={row.id}>
          {row.label}
          <span className="ag-status-n">{row.count}</span>
        </PaneHeading>
      );
    case "status-agent":
      return (
        <AgentListRow
          where={whereOf(semantics?.places[0])}
          node={row.node}
          facts={rowFactsOf(identity, row.node, row.teamKey)}
          status={statusOf(row.node.row)}
          showTeam={tree.multiTeam}
          depth={0}
          elapsed={formatElapsed(row.node.row.startedAtMs, nowMs)}
          nowMs={statusOf(row.node.row) === "stalled" ? nowMs : 0}
          showManagerPill
          launcherName={launcherNameOf(tree, row.node)}
          onSelectHandle={tree.onSelectHandle}
          onTogglePin={tree.onTogglePin}
          {...tree.actions}
        />
      );
  }
}

/** The find field: search, the chips that name each active filter, and the filter menu. */
function FilterField({
  openTeamKeys,
  query,
  onQueryChange,
  status,
  onStatusChange,
  plan,
  teams,
  hiddenTeams,
  onSetTeamHidden,
  onShowAllTeams,
  focusedTeam,
  onToggleTeamFocus,
  workspace,
  workspaceLabel,
  onWorkspaceChange,
  arrange,
  onArrangeChange,
  agentFilterName,
  onClearAgentFilter,
}: {
  query: string;
  onQueryChange: (query: string) => void;
  status: AgentStatusFilter;
  onStatusChange: (status: AgentStatusFilter) => void;
  plan: PanePlan;
  teams: readonly PaneTeam[];
  /** Every team the user has open, so the checklist lists them even with no agents yet. */
  openTeamKeys: readonly string[];
  hiddenTeams: readonly string[];
  onSetTeamHidden: (teamKey: string, hidden: boolean) => void;
  onShowAllTeams: () => void;
  /** The team a header's funnel (or the checklist) narrows the pane to: its chip names it. */
  focusedTeam: string | null;
  onToggleTeamFocus?: (teamKey: string) => void;
  workspace: PaneWorkspaceFilter | null;
  workspaceLabel: string | null;
  onWorkspaceChange: (workspace: PaneWorkspaceFilter | null) => void;
  arrange: AgentListArrange;
  onArrangeChange: (arrange: AgentListArrange) => void;
  agentFilterName: string | null;
  onClearAgentFilter: () => void;
}): React.JSX.Element {
  const [menuOpen, setMenuOpen] = useState(false);
  const { teamLabelOf } = useContext(Identity);
  const fieldRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuId = useId();

  // Opening the menu moves focus into it, onto its first control.
  useEffect(() => {
    if (!menuOpen) return;
    menuRef.current?.querySelector<HTMLElement>("input:not(:disabled), button")?.focus();
  }, [menuOpen]);

  // Escape (focus back on the button), a click outside, Tab out of it, or the button again closes
  // the menu.
  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (fieldRef.current?.contains(t)) return;
      setMenuOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [menuOpen]);

  // Hidden teams that still have agents: a hidden team with none left is not on the pane.
  const pickedHidden = hiddenTeams.filter((key) => teams.some((t) => t.teamKey === key));
  const wsLabel = workspace ? (workspaceLabel ?? "No workspace") : "";
  const showOption = SHOW_OPTIONS.find((o) => o.id === status);
  const chips: { key: string; label: string; clearLabel: string; onClear: () => void }[] = [];
  if (focusedTeam && onToggleTeamFocus) {
    // Only one team shows: the chip names it, as the menu does, and its × shows the others again.
    const focusName = teamLabelOf(focusedTeam);
    chips.push({
      key: "team",
      label: `Team: ${focusName}`,
      clearLabel: "Show the other teams",
      onClear: () => onToggleTeamFocus(focusedTeam),
    });
  } else if (pickedHidden.length > 0) {
    chips.push({
      key: "teams",
      // The chip names what is hidden (its × shows them again), not what is shown.
      label:
        pickedHidden.length === 1
          ? `${teamLabelOf(pickedHidden[0]!)} hidden`
          : `${pickedHidden.length} teams hidden`,
      clearLabel: pickedHidden.length === 1 ? "Show the hidden team" : "Show the hidden teams",
      onClear: onShowAllTeams,
    });
  }
  if (showOption && status !== "all") {
    chips.push({
      key: "show",
      label: showOption.label,
      clearLabel: `Clear the ${showOption.label} filter`,
      onClear: () => onStatusChange("all"),
    });
  }
  if (arrange === "status") {
    // The Arrange choice shows as a chip too, so a list that is not drawn by team says so.
    chips.push({
      key: "arrange",
      label: "By status",
      clearLabel: "Arrange by team",
      onClear: () => onArrangeChange("teams"),
    });
  }
  if (workspace) {
    chips.push({
      key: "ws",
      label: wsLabel,
      clearLabel: `Clear the ${wsLabel} filter`,
      onClear: () => onWorkspaceChange(null),
    });
  }
  if (agentFilterName) {
    chips.push({
      key: "agent",
      label: `Agent: ${agentFilterName}`,
      clearLabel: "Clear agent filter",
      onClear: onClearAgentFilter,
    });
  }

  // Teams checklist: every open team, grouped by organisation, in the pane's order. A team with
  // no agents yet is listed with 0, so opening a context repo is never invisible here.
  const orgNames = useOrgStore((s) => s.orgs);
  const orgNameOf = useMemo(
    () => new Map(orgNames.map((o) => [o.slug, o.display_name])),
    [orgNames],
  );
  const orgs = new Map<string, { teamKey: string; count: number }[]>();
  const countOf = new Map(teams.map((t) => [t.teamKey, countNodes(t.nodes)]));
  const projectedTeamOrder = useAgentListProjectionStore((state) => state.teamOrder);
  const checklistKeys = [...new Set([...openTeamKeys, ...teams.map((team) => team.teamKey)])]
    .filter((key) => key !== NO_TEAM_SENTINEL)
    .sort((a, b) => projectedTeamOrder.indexOf(a) - projectedTeamOrder.indexOf(b))
    .map((teamKey) => ({ teamKey }));
  // The pane always keeps one team in view: the last team the checklist shows checked cannot be
  // unchecked. Every listed team counts, one with no agents yet included.
  const lastShown = (teamKey: string) =>
    !hiddenTeams.includes(teamKey) &&
    !checklistKeys.some((t) => t.teamKey !== teamKey && !hiddenTeams.includes(t.teamKey));
  for (const { teamKey } of checklistKeys) {
    const { orgSlug } = parseTeamKey(teamKey);
    const list = orgs.get(orgSlug) ?? [];
    list.push({ teamKey, count: countOf.get(teamKey) ?? 0 });
    orgs.set(orgSlug, list);
  }

  return (
    <div
      className="shrink-0 px-1.5 pb-2"
      onKeyDown={(e) => {
        if (e.key === "Escape" && menuOpen) {
          e.stopPropagation();
          setMenuOpen(false);
          buttonRef.current?.focus();
        }
      }}
    >
      <div ref={fieldRef} className="ag-find" data-testid="agents-filter-field">
        <Search className="size-3.5 shrink-0" aria-hidden />
        {chips.length > 0 && (
          <span className="ag-chips">
            {chips.map((chip) => (
              <span key={chip.key} className="ag-chip" data-chip={chip.key}>
                <span className="truncate">{chip.label}</span>
                <button
                  type="button"
                  aria-label={chip.clearLabel}
                  className="rounded-full opacity-70 hover:opacity-100"
                  onClick={chip.onClear}
                >
                  <X className="size-3" />
                </button>
              </span>
            ))}
          </span>
        )}
        <input
          type="search"
          placeholder={chips.length > 0 ? "" : "Find agents…"}
          aria-label="Find agents"
          value={query}
          onChange={(e) => onQueryChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && query) {
              e.stopPropagation();
              onQueryChange("");
            }
          }}
        />
        {query && (
          <button
            type="button"
            className="ag-search-clear"
            aria-label="Clear search"
            onClick={() => onQueryChange("")}
          >
            <X className="size-3" />
          </button>
        )}
        <button
          ref={buttonRef}
          type="button"
          className="ag-find-filter"
          aria-label="Filter and arrange"
          aria-expanded={menuOpen}
          aria-controls={menuOpen ? menuId : undefined}
          title="Filter and arrange"
          onClick={() => setMenuOpen((open) => !open)}
        >
          <ListFilter className="size-3.5" aria-hidden />
        </button>
        {menuOpen && (
          <div
            ref={menuRef}
            id={menuId}
            role="dialog"
            aria-label="Filter and arrange agents"
            className="ag-fpop"
            // A click on the menu's own surface keeps focus in it, so only Tab out closes it.
            tabIndex={-1}
            onBlur={(e) => {
              const next = e.relatedTarget as Node | null;
              if (!next || menuRef.current?.contains(next) || buttonRef.current === next) return;
              setMenuOpen(false);
            }}
          >
            <div className="ag-fm-sec">Teams</div>
            {[...orgs.entries()].map(([orgSlug, list]) => (
              <div key={orgSlug} className="ag-fm-org">
                <div className="ag-fm-orgname">{orgNameOf.get(orgSlug) || orgSlug}</div>
                {list.map((t) => (
                  <label
                    key={t.teamKey}
                    className="ag-fm-team"
                    title={
                      lastShown(t.teamKey) ? "The pane always shows at least one team" : undefined
                    }
                  >
                    <input
                      type="checkbox"
                      checked={!hiddenTeams.includes(t.teamKey)}
                      disabled={lastShown(t.teamKey)}
                      onChange={(e) => onSetTeamHidden(t.teamKey, !e.target.checked)}
                    />
                    <span className="truncate">{teamNameOf(t.teamKey)}</span>
                    <span className="ag-fm-n">{t.count}</span>
                  </label>
                ))}
              </div>
            ))}
            <div className="ag-fm-sec" id={`${menuId}-show`}>
              Show
            </div>
            {/* One choice of several: a radio group, its arrow keys moving the choice. */}
            <div
              role="radiogroup"
              aria-labelledby={`${menuId}-show`}
              onKeyDown={(e) => radioGroupKeys(e, SHOW_OPTIONS, status, onStatusChange)}
            >
              {SHOW_OPTIONS.map((o) => (
                <button
                  key={o.id}
                  type="button"
                  role="radio"
                  className="ag-fm-item"
                  aria-checked={status === o.id}
                  tabIndex={status === o.id ? 0 : -1}
                  onClick={() => onStatusChange(o.id)}
                >
                  {/* A menu's marks are examples: they hold still. */}
                  <span className="cs-mark-still" aria-hidden>
                    {o.mark && <StatusMark mark={o.mark} size={8} />}
                  </span>
                  <span>{o.label}</span>
                  <span className="ag-fm-n">{plan.counts[o.id]}</span>
                </button>
              ))}
            </div>
            <div className="ag-fm-sec" aria-hidden>
              Arrange
            </div>
            <div
              role="radiogroup"
              aria-label="Arrange"
              className="ag-fp-seg"
              onKeyDown={(e) => radioGroupKeys(e, ARRANGE_OPTIONS, arrange, onArrangeChange)}
            >
              {ARRANGE_OPTIONS.map((o) => (
                <button
                  key={o.id}
                  type="button"
                  role="radio"
                  aria-checked={arrange === o.id}
                  tabIndex={arrange === o.id ? 0 : -1}
                  onClick={() => onArrangeChange(o.id)}
                >
                  {o.label}
                </button>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

/** A radio group's arrow keys: the next or previous choice is chosen and takes focus, wrapping
 *  at the ends, as a native radio group does. */
function radioGroupKeys<T extends string>(
  e: React.KeyboardEvent<HTMLElement>,
  options: readonly { id: T }[],
  current: T,
  choose: (id: T) => void,
): void {
  const step =
    e.key === "ArrowDown" || e.key === "ArrowRight"
      ? 1
      : e.key === "ArrowUp" || e.key === "ArrowLeft"
        ? -1
        : 0;
  if (step === 0 || e.metaKey || e.ctrlKey || e.altKey) return;
  e.preventDefault();
  const at = options.findIndex((o) => o.id === current);
  // A value none of the options holds steps onto the first (down) or the last (up).
  const next =
    at < 0 ? (step > 0 ? 0 : options.length - 1) : (at + step + options.length) % options.length;
  choose(options[next]!.id);
  e.currentTarget.querySelectorAll<HTMLElement>('[role="radio"]')[next]?.focus();
}

/** One team's header (`org › team`). It folds on click; its funnel (shown on hover or focus, and
 *  while on) shows only this team. The agents under it are separate rows of the list. */
function TeamHead({
  headingLevel = 3,
  teamKey,
  needCount,
  folded,
  frozen,
  onToggleFold,
  funnelOn = false,
  onFunnel,
}: {
  /** The header's heading level in the list. */
  headingLevel?: 3 | 4 | null;
  teamKey: string;
  needCount: number;
  folded: boolean;
  /** A search or the workspace chip is on: every group is open, so the fold cannot change now. */
  frozen: boolean;
  onToggleFold: (folded: boolean) => void;
  /** Only this team shows, by the funnel or the Teams checklist. */
  funnelOn?: boolean;
  /** The funnel's click; no funnel when the pane has one team to show. */
  onFunnel?: () => void;
}): React.JSX.Element {
  const { orgSlug } = parseTeamKey(teamKey);
  // The org's saved name ("Codesense"), not its context repo's path.
  const orgName = useOrgStore((s) => s.orgs.find((o) => o.slug === orgSlug)?.display_name);
  // A team reads by its name and its rule, in the pane's own ink: colour in the pane means status
  // and machine, so a team does not take a hue of its own. Its controls name it as "org › team"
  // where another open team shares the name.
  const name = teamNameOf(teamKey);
  const label = useContext(Identity).teamLabelOf(teamKey);
  return (
    <div
      className="ag-team"
      data-testid="agents-team-group"
      data-team-key={teamKey}
      data-funnel={onFunnel ? (funnelOn ? "on" : "off") : undefined}
    >
      {/* The heading holds only the fold button, so the funnel beside it stays out of its name. */}
      <PaneHeading level={headingLevel ?? 3} className="ag-team-title">
        <button
          type="button"
          className="ag-team-head"
          aria-expanded={!folded}
          aria-disabled={frozen || undefined}
          title={
            frozen
              ? "Clear the search or the workspace chip to fold this team"
              : folded
                ? `Show ${label}`
                : `Fold ${label}`
          }
          onClick={() => {
            if (!frozen) onToggleFold(!folded);
          }}
        >
          <span className="ag-team-org" title={orgSlug}>
            {orgName || orgSlug}
          </span>
          <span className="ag-team-sep" aria-hidden>
            ›
          </span>
          <span className="ag-team-name truncate">{name}</span>
          {folded && needCount > 0 && (
            <span role="img" className="ag-team-need" aria-label={`${needCount} need you`}>
              <StatusMark mark="question" size={6} />
              {needCount}
            </span>
          )}
          <ChevronDown
            className={cn("ag-team-chevron size-3 shrink-0", folded && "-rotate-90")}
            aria-hidden
          />
        </button>
      </PaneHeading>
      {onFunnel && (
        // Beside the header, not inside it (no nested buttons): it takes the chevron's place on
        // hover or focus, as a workspace card's filter does, and stays while it is on.
        <button
          type="button"
          className="ag-team-funnel"
          data-testid="agents-team-funnel"
          aria-label={`Filter to ${label}`}
          aria-pressed={funnelOn}
          title={funnelOn ? "Show the other teams again" : `Show only ${label}`}
          onClick={onFunnel}
        >
          <Filter className="size-3" aria-hidden />
        </button>
      )}
    </div>
  );
}

/** One workspace's header: its name folds it, its arrow filters the list to it (shown on hover or
 *  focus), and its stage, repo, count and crew sit beside the name. Its agents are separate rows. */
function WorkspaceHead({
  headingLevel = 4,
  teamKey,
  group,
  folded,
  frozen,
  arrowOn,
  dragging = false,
  onReorderPress,
  movable,
  onToggleFold,
  onArrow,
  tree,
  needs,
}: {
  /** The card's heading level, under its team. */
  headingLevel?: 3 | 4 | null;
  teamKey: string;
  group: WorkspaceSessionGroup;
  folded: boolean;
  /** A search or the workspace chip is on: every group is open, so the fold cannot change now. */
  frozen: boolean;
  arrowOn: boolean;
  /** This card is being dragged: it stays as a placeholder while its preview moves. */
  dragging?: boolean;
  /** Set when the card can move: a press on its head row that may become a drag. */
  onReorderPress?: (e: React.PointerEvent<HTMLElement>) => void;
  /** The card can move now: show the grip and the drag hints. A card that can't still takes the
   *  press, which stays a click or says why it can't move. */
  movable: boolean;
  onToggleFold: (folded: boolean) => void;
  onArrow: () => void;
  tree: TreeProps;
  needs: ReadonlySet<string>;
}): React.JSX.Element {
  const identity = useContext(Identity);
  const changeStoryStatus = useViewerStore((st) => st.changeStoryStatus);
  const [stageOpen, setStageOpen] = useState(false);
  const { statusOf } = tree;
  // The story's stage and the source repo's glyph read as the tray's chips do; a crew is the
  // lead's face and the others in the workspace.
  const repoGlyph = group.sourceRepoSlug
    ? identity.repoGlyphOf(teamKey, group.sourceRepoSlug)
    : undefined;
  // A workspace on a remote machine shows that machine's mark beside its name.
  const cardMachine = group.machineId ? identity.remoteMachineFor(teamKey, group.machineId) : null;
  const crew = identity.crewOf(teamKey, group.workspaceId);
  const marks = group.nodes.map(
    (n): StatusMarkKind =>
      markWithAvailability(markOfAgentStatus(statusOf(n.row)), !!identity.offlineMachineOf(n.row)),
  );
  const mark = topMark(marks) ?? "ready";
  const counts = countMarks(marks);
  // Each member's activity qualifier ("last observation may be stale", "recovering") joins the
  // roll-up words, read from the same activity store the row's own dot reads.
  // A shallow compare on the strings keeps the card from re-rendering on unrelated store updates.
  const qualifiers = useAgentStateStore(
    useShallow((st) =>
      group.nodes.map(({ row }) =>
        activityQualifier(
          st.activityBySession[row.sessionId ?? ""],
          markWithAvailability(markOfAgentStatus(statusOf(row)), !!identity.offlineMachineOf(row)),
        ),
      ),
    ),
  );
  const summary = withActivityQualifiers(rollupWords(counts, ", ", true), qualifiers, ", ");
  const hasNeeds = group.nodes.some((n) =>
    needs.has(agentRowKey(n.teamKey ?? teamKey, n.row.handle)),
  );
  const name = group.label;
  // A story card shows its story id; its title reads in the card's tooltip.
  const fullName = group.storyTitle ? `${name} · ${group.storyTitle}` : name;
  // A story card whose story has a stage shows its rail and the stage picker.
  const stage =
    group.kind === "story" && isStoryStage(group.storyStatus) ? group.storyStatus : null;
  return (
    <div
      className="ag-ws-group"
      data-testid="agents-workspace-group"
      data-group-key={workspaceGroupKey(teamKey, group.workspaceId)}
      data-workspace-id={group.workspaceId}
      data-collapsed={folded || undefined}
      data-arrow-on={arrowOn || undefined}
      data-reorderable={movable ? "" : undefined}
      data-dragging={dragging || undefined}
    >
      <div
        className="ag-ws-head-row"
        onPointerDown={(e) => {
          // A press on a control inside the card (the stage, the filter) stays that control's, and
          // so does one in the stage popover, which portals out but bubbles here through React.
          const target = e.target as HTMLElement;
          if (!e.currentTarget.contains(target) || target.closest(".ag-ws-stage, .ag-ws-arrow")) {
            return;
          }
          onReorderPress?.(e);
        }}
        // The card body folds the workspace; the name, the arrow and the status inside it are
        // their own controls, so their clicks reach this handler only for the body.
        onClick={() => {
          if (!frozen) onToggleFold(!folded);
        }}
      >
        {movable && (
          // The grip: a quiet hint, on hover, that the card moves. The whole head row drags.
          <span className="ag-ws-grip" aria-hidden>
            <GripVertical className="size-3" />
          </span>
        )}
        {stage !== null && (
          // The popover's options portal out, but their clicks still bubble through React to this
          // row, so the wrapper stops them: choosing a stage never folds the card.
          <span className="ag-ws-stage" onClick={(e) => e.stopPropagation()}>
            <StatusSelectorPopover
              currentStatus={stage}
              triggerAriaLabel={`Change status for ${name}`}
              // The store finds the story by its workspace record, and says when a write fails.
              onSelect={(next) => void changeStoryStatus(teamKey, group.workspaceId, next)}
              open={stageOpen}
              onOpenChange={setStageOpen}
            >
              <StageRail status={stage} size={12} />
            </StatusSelectorPopover>
          </span>
        )}
        {/* The heading holds only the name, the card's fold button: the card's other controls
            and marks stay out of its name. */}
        <PaneHeading level={headingLevel ?? 4} className="ag-ws-title">
          <button
            type="button"
            className="ag-ws-name truncate"
            aria-expanded={!folded}
            aria-disabled={frozen || undefined}
            aria-keyshortcuts={movable ? "Alt+ArrowUp Alt+ArrowDown" : undefined}
            title={
              frozen
                ? "Clear the search or the workspace chip to fold this card"
                : movable
                  ? `${folded ? "Expand" : "Collapse"} ${fullName} · drag to move, or ${altKeyName()}+↑ / ${altKeyName()}+↓`
                  : `${folded ? "Expand" : "Collapse"} ${fullName}`
            }
            data-kind={group.workspaceId === NO_WORKSPACE_ID ? "none" : group.kind}
          >
            {/* A story card's stage rail already leads it, so the stack glyph would only repeat it. */}
            {stage === null && <WorkspaceGlyph group={group} />}
            <span className="truncate">{name}</span>
          </button>
        </PaneHeading>
        {cardMachine && (
          <span title={cardMachine.words} className="inline-flex">
            <MachineMark
              identity={cardMachine}
              size={10}
              testId={`agents-workspace-${group.workspaceId}-machine`}
            />
          </span>
        )}
        {repoGlyph && (
          <RepoGlyph
            glyph={repoGlyph}
            glyphColor="var(--ag-muted)"
            testId={`agents-workspace-${group.workspaceId}-repo`}
          />
        )}
        <span
          role="img"
          className="ag-ws-count"
          data-has-needs={hasNeeds || undefined}
          aria-label={`${group.nodes.length} agents: ${summary}`}
          title={summary}
        >
          {/* The count is the one picture: its mark inside is decoration. */}
          <StatusMark mark={mark} size={8} />
          {counts[mark]}
        </span>
        {crew && (
          <span
            role="img"
            className="ag-ws-crew"
            data-testid={`agents-workspace-${group.workspaceId}-crew`}
            // The faces are one picture: its name, and its hover, say who the crew is and what it
            // is doing.
            aria-label={crew.summary}
            title={crew.summary}
          >
            <CrewStack
              lead={{
                face: crew.leadFace,
                piece: crew.leadPiece,
                label: crew.leadLabel,
                hue: crew.hue,
                tag: crew.leadTag,
              }}
              crew={crew.crew}
              more={crew.more}
              size={15}
              tone={NEUTRAL_PIECE}
            />
          </span>
        )}
        {/* One slot at the card's end: at rest the fold chevron (as on a team header); on hover or
            focus the filter button takes its place; while the filter is on, the filter button stays. */}
        <span className="ag-ws-end">
          <ChevronDown className={cn("ag-ws-chevron size-3", folded && "-rotate-90")} aria-hidden />
          <button
            type="button"
            className="ag-ws-arrow"
            aria-label={`Filter to ${name}`}
            aria-pressed={arrowOn}
            title={arrowOn ? "Show all workspaces" : `Show only ${name}`}
            onClick={(e) => {
              e.stopPropagation();
              onArrow();
            }}
          >
            <Filter className="size-3" aria-hidden />
          </button>
        </span>
      </div>
    </div>
  );
}

/** A workspace card's glyph, on its header and its drag preview alike: the stack for a story
 *  and for the no-workspace bucket, else the context mark (its repo's tile, as the switcher
 *  shows it), in the pane's neutral ink: colour here means status and machine. */
function WorkspaceGlyph({ group }: { group: WorkspaceSessionGroup }): React.JSX.Element {
  return group.kind === "story" || group.workspaceId === NO_WORKSPACE_ID ? (
    <Layers className="size-3 shrink-0" aria-hidden />
  ) : (
    <ContextRepoMark
      slug={group.sourceRepoSlug ?? group.label}
      name={group.label}
      size={14}
      className="ag-ws-repo-mark"
    />
  );
}

/** The card a drag carries: its name, its kind and how many agents move with it, lifted over the
 *  list at the pointer. It turns away (dims, no line) while the pointer is outside its team. */
function CardDragPreview({
  row,
  top,
  refused,
  statusOf,
}: {
  row: Extract<PaneRow, { kind: "ws" }> | undefined;
  top: number;
  refused: boolean;
  statusOf: (row: AgentSessionRow) => AgentStatus;
}): React.JSX.Element | null {
  const identity = useContext(Identity);
  if (!row) return null;
  const { group } = row;
  const count = group.nodes.length;
  // The card's own roll-up: its most urgent mark, as the card's count shows it (an agent on an
  // offline machine reads offline there too).
  const mark =
    topMark(
      group.nodes.map((n) =>
        markWithAvailability(
          markOfAgentStatus(statusOf(n.row)),
          !!identity.offlineMachineOf(n.row),
        ),
      ),
    ) ?? "ready";
  return (
    <div
      className="ag-ws-drag-preview"
      data-testid="agents-drag-preview"
      data-refused={refused || undefined}
      aria-hidden
      style={{ top }}
    >
      <GripVertical className="ag-ws-drag-preview-grip size-3" />
      <WorkspaceGlyph group={group} />
      <span className="ag-ws-drag-preview-name truncate">{group.label}</span>
      <span className="ag-ws-drag-preview-count">
        <StatusMark mark={mark} size={8} />
        {count} {count === 1 ? "agent" : "agents"}
      </span>
    </div>
  );
}

interface TreeProps {
  nowMs: number;
  needs: ReadonlySet<string>;
  /** Each agent's status: the one read its chip, its filter bucket and its tab dot share. */
  statusOf: (row: AgentSessionRow) => AgentStatus;
  /** The rows' actions: the same functions every render, so a memoised row keeps its render. */
  actions: RowActions;
  /** A via chip selects its manager row, in that row's team (scrolling only if it is off screen).
   *  Stable. */
  onSelectHandle?: (handle: string, teamKey: string) => void;
  /** Pin a row's session, or unpin it. Stable. */
  onTogglePin: (teamKey: string, row: AgentSessionRow, label: string) => void;
  /** A listed agent's name by team and handle, for the via chip. */
  nameOf: (teamKey: string | null | undefined, handle: string) => string | undefined;
  /** The list spans more than one team, so a Status row names its team. */
  multiTeam: boolean;
}

/** What an agent is doing, for the end of its first line. `markApart`: a subagent mark beside the
 *  status word carries the subagents' count, so the words leave it out ("Working", not
 *  "Working · 2 subagents"); the tooltip reads the full words. */
/** The word a parked row wears when its CLI release is one CodeSense is not qualified on. */
const LIMITED_WORD = "Limited";

/** Whether `row` reads Limited rather than Ready: only an idle row can read Ready, so only an
 *  idle row's word changes. Its notice line says the release in every live state. */
function limitedWord(row: AgentSessionRow, status: AgentStatus): boolean {
  return status === "idle" && releaseWarning(row) !== null;
}

function rowTask(
  row: AgentSessionRow,
  status: AgentStatus,
  subagents: number,
  markApart = false,
): string {
  const detected = (row as ListedAgentRow).detected;
  // An agent nothing reports on says no status word: its hollow dot and tooltip say it, and the
  // name keeps the room. One typed by hand that can't be woken says it in words, since its
  // notice line follows.
  const word =
    status === "unknown"
      ? isTypedByHand(row)
        ? AGENT_STATUS_WORDS.unknown
        : ""
      : limitedWord(row, status)
        ? LIMITED_WORD
        : markApart
          ? agentStatusWord(status, subagents)
          : agentStatusWords(status, subagents);
  // A launch prompt goes after the status word.
  if (row.startedWith) return word ? `${word} · ${row.startedWith}` : row.startedWith;
  if (detected) {
    // Line two already names a detected agent's CLI, so line one keeps only what it is doing.
    if (status === "unknown") return word;
    if (limitedWord(row, status)) return LIMITED_WORD;
    if (markApart && shownSubagents(status, subagents)) return word;
    const subtitle = handAgentSubtitle(detected, status, subagents);
    const cli = `${cliLabel(detected.cli)} · `;
    return detected.cli && subtitle.startsWith(cli) ? subtitle.slice(cli.length) : subtitle;
  }
  return word;
}

/**
 * A blocked row's waiting line: "Blocked: <note>" when the agent's latest
 * `signal_progress {phase:"blocked"}` carried a note, else "Blocked" on its own. It never reads
 * "Waiting for your approval": a block the agent reports is not a permission request, and the
 * note is the reason the agent gave.
 */
export function blockedLine(progress: SessionProgress | undefined): string {
  const note = progress?.phase === "blocked" ? progress.note?.trim() : undefined;
  return note ? `Blocked: ${note}` : "Blocked";
}

/** Where a row says it works: the routine that started it, a headless run's output, its
 *  workspace's name, its story, or Background for a live session no tab
 *  shows. */
export function rowWorkspace(
  node: AgentTreeNode,
  opts: { routine?: string | null; tabless?: boolean } = {},
): string | null {
  if (opts.routine) return `Routine · ${opts.routine}`;
  if (isHeadlessSession(node.row)) return "Headless · run output";
  if (node.workspace) return node.workspace.label;
  if (node.story) return node.story.title ?? node.story.storyId;
  return !node.row.sessionId || opts.tabless ? "Background" : null;
}

/** Whether a workspace's story status names a stage to draw: any non-empty status (one outside
 *  the team's taxonomy draws as an outline). */
function isStoryStage(status: string | undefined): status is string {
  return typeof status === "string" && status.trim() !== "";
}

/** The agent's model, read once per row from its launch and its live record. */
/** The CLI a row's session runs, by the model label's sources: its launch, its hooks, or what
 *  the roster detected. */
function useRowCli(row: AgentSessionRow): string | null {
  const sid = row.sessionId ?? "";
  const detectedCli = (row as ListedAgentRow).detected?.cli ?? null;
  return useAgentStateStore(
    (s) => s.launchBySession[sid]?.agent ?? s.bySession[sid]?.agent ?? detectedCli,
  );
}

export function useRowModel(row: AgentSessionRow): string | null {
  const sid = row.sessionId;
  const detectedCli = (row as ListedAgentRow).detected?.cli ?? null;
  return useAgentStateStore((s) => agentSessionModel(s, sid, detectedCli));
}

/** The status mark on a row's avatar is the same component every other surface draws (the
 *  workspace pill, the terminal tab chip, the header badge), from {@link markOfAgentStatus} plus
 *  an offline-machine override, so every surface agrees on glyph, motion and colour family. Each
 *  state has its own shape, so it reads without its colour. */
type AvatarStatusGroup = "working" | "needs" | "idle";

/** Which colour family a mark belongs to — the row-level selectors key off this
 *  (not the fine-grained mark) so the ag-row chrome keeps its two-colour palette
 *  (busy / needs / idle) while the mark carries the distinction by shape. */
function avatarStatusGroup(mark: StatusMarkKind): AvatarStatusGroup {
  switch (mark) {
    case "thinking":
    case "tool":
      return "working";
    case "question":
    case "permission":
    case "blocked":
    case "error":
    case "crashed":
      return "needs";
    case "ready":
    case "stalled":
    case "ended":
    case "unknown":
    case "offline":
      return "idle";
  }
}

function AvatarStatusDot({
  mark,
  sizePx,
  label,
}: {
  mark: StatusMarkKind;
  sizePx: number;
  label: string;
}): React.JSX.Element {
  const group = avatarStatusGroup(mark);
  // The row's labelled image: the SHARED StatusMark inside a pane-ring wrapper.
  // The outer span carries the accessible name so component tests that already
  // reach for `needs-you-badge` or query by role="img" keep finding it; the
  // inner <StatusMark> stays decoration (`aria-hidden`) so the row speaks with
  // one voice.
  return (
    <span
      className={cn("ag-row-dot ag-avatar-dot", `ag-avatar-dot-${group}`)}
      role="img"
      aria-label={label}
      data-testid={group === "needs" ? "needs-you-badge" : undefined}
      data-mark={mark}
      style={{ width: sizePx, height: sizePx }}
    >
      <StatusMark mark={mark} size={sizePx} />
    </span>
  );
}

/** The via chip's glyph: a routing curve and an arrowhead, drawn inline to match the approved
 *  mock exactly (Lucide's `CornerDownRight` is close, but not the same). */
function ViaGlyph(): React.JSX.Element {
  return (
    <svg className="ag-via-glyph" viewBox="0 0 12 12" fill="none" aria-hidden>
      <path
        d="M3 3v3.25A1.75 1.75 0 0 0 4.75 8H10"
        stroke="currentColor"
        strokeWidth="1.25"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M8 5.5 10.5 8 8 10.5"
        stroke="currentColor"
        strokeWidth="1.25"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** What a row reads of the pane's identity, worked out per row by its parent. The memoised row
 *  takes these as props instead of the Identity context, so a change to one agent (its status, its
 *  tab, its machine) re-renders that agent's row and no other. */
export interface RowFacts {
  /** The agent's own team: its node's, else the list's home team. */
  rowTeam: string;
  /** That team in words, as the pane names it (a Status row says it). */
  teamLabel: string;
  /** How many listed agents it launched (its role reads it). */
  launched: number;
  choice: AgentIdentityChoice | undefined;
  faces: FaceSource;
  faceTag: number | undefined;
  current: boolean;
  workspaceCurrent?: boolean;
  hidden: boolean;
  tabless: boolean;
  /** A tab on one of its team's tiles shows it (Hide tab and Go to terminal act on it). */
  hasTab: boolean;
  /** The name of its machine while that machine is offline, or its tab is held as it last was. */
  offlineMachine: string | null;
  remoteMachine: RemoteMachineIdentity | null;
  pinned: boolean;
  machineId: string;
  machine: string;
  routine: string | null;
  holds: readonly RoleHolding[];
  narrow: boolean;
  settled: boolean;
  /** The agent that launched it runs on an offline machine. */
  managerOffline: boolean;
}

/** A row's facts, read from the pane's identity. `teamKey` names the row's team when its node
 *  carries none (a home-team node). */
export function rowFactsOf(
  identity: IdentityContext,
  node: AgentTreeNode,
  teamKey?: string,
): RowFacts {
  const { row } = node;
  const rowTeam = node.teamKey ?? teamKey ?? identity.homeTeamKey;
  const hidden = identity.isHidden(row);
  return {
    rowTeam,
    teamLabel: identity.teamLabelOf(rowTeam),
    launched: identity.launched.get(agentRowKey(rowTeam, row.handle)) ?? 0,
    choice: identity.choicesOf(rowTeam)[row.handle],
    faces: identity.facesOf(rowTeam),
    faceTag: identity.faceTagsOf(rowTeam).get(row.handle),
    current: row.sessionId !== null && row.sessionId === identity.currentSessionId,
    workspaceCurrent:
      !!node.workspace &&
      useViewerStore.getState().activeWorkspace?.[rowTeam] === node.workspace.id,
    hidden,
    tabless: !isHeadlessSession(row) && identity.isTabless(row),
    hasTab: !hidden && identity.tabOf(row) !== null,
    offlineMachine: identity.offlineMachineOf(row),
    remoteMachine: identity.remoteMachineOf(row),
    pinned: identity.isPinned(rowTeam, row),
    machineId: identity.machineIdOf(row),
    machine: identity.machineOf(row),
    routine: identity.routineOf(row),
    holds: identity.holdsOf(rowTeam, row),
    narrow: identity.narrow,
    settled: identity.settled,
    managerOffline:
      !!row.launchedBy && identity.offlineHandles.has(agentRowKey(rowTeam, row.launchedBy)),
  };
}

/** Two plain objects with the same own values. */
function sameFields(a: object | null | undefined, b: object | null | undefined): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  const ka = Object.keys(a);
  if (ka.length !== Object.keys(b).length) return false;
  return ka.every((k) => (a as Record<string, unknown>)[k] === (b as Record<string, unknown>)[k]);
}

/** The same agent, drawn the same way: a plan rebuilds its nodes (and a tab change its workspace
 *  refs), so a node is compared by what it holds. */
function sameNode(a: AgentTreeNode, b: AgentTreeNode): boolean {
  return (
    a === b ||
    (a.row === b.row &&
      a.teamKey === b.teamKey &&
      a.parentSource === b.parentSource &&
      a.scopeContext === b.scopeContext &&
      sameFields(a.workspace, b.workspace) &&
      sameFields(a.story, b.story))
  );
}

function sameFacts(a: RowFacts, b: RowFacts): boolean {
  if (a === b) return true;
  for (const key of Object.keys(a) as (keyof RowFacts)[]) {
    if (key === "remoteMachine") {
      if (!sameFields(a.remoteMachine, b.remoteMachine)) return false;
    } else if (key === "holds") {
      // A role holding is rebuilt on every roles poll: compared by what it says.
      if (a.holds.length !== b.holds.length || a.holds.some((h, i) => !sameFields(h, b.holds[i]))) {
        return false;
      }
    } else if (a[key] !== b[key]) {
      return false;
    }
  }
  return true;
}

/** A row's props are unchanged: its node and facts by value, everything else by identity. */
function sameRowProps(a: AgentListRowProps, b: AgentListRowProps): boolean {
  const keys = new Set([...Object.keys(a), ...Object.keys(b)]) as Set<keyof AgentListRowProps>;
  for (const key of keys) {
    if (key === "node") {
      if (!sameNode(a.node, b.node)) return false;
    } else if (key === "facts") {
      if (!sameFacts(a.facts, b.facts)) return false;
    } else if (a[key] !== b[key]) {
      return false;
    }
  }
  return true;
}

type AgentListRowProps = {
  /** The row's place (its team, workspace and launcher) in words: its description leads with it,
   *  since the flat list cannot nest the row under its team and card. */
  where?: string;
  node: AgentTreeNode;
  /** What the row reads of the pane's identity (see {@link RowFacts}). */
  facts: RowFacts;
  /** The agent's status, as the list's filters count it. */
  status: AgentStatus;
  /** The agent's depth in its launch tree: a worker indents under its launcher. */
  depth: number;
  /** Whether the agent sits under a workspace card (indented to show it belongs to that card). */
  inWorkspace?: boolean;
  managerView?: boolean;
  treeAccent?: string;
  managerFolded?: boolean;
  foldedNeeds?: number;
  managerFoldsFrozen?: boolean;
  onFoldManager?: () => void;
  chosenManagerName?: string;
  elapsed: string;
  nowMs: number;
  /** The row is the Pinned group's copy, not its place in the workspace list. */
  inPinned?: boolean;
  /** The row names its team on line two (Status view, where the sections span teams). */
  showTeam?: boolean;
  /** A worker row names the agent that launched it on line two. */
  showManagerPill?: boolean;
  /** That launcher's name, when it is listed. */
  launcherName?: string;
  /** A click on the "via <launcher>" chip selects that manager row. */
  onSelectHandle?: (handle: string, teamKey: string) => void;
  /** Pin the row's session, or unpin it. */
  onTogglePin: (teamKey: string, row: AgentSessionRow, label: string) => void;
} & RowActions;

// Non-stalled rows keep a stable clock prop and render on elapsed-string changes.
// Stalled rows also read the list clock for their last-activity age.
const AgentListRow = memo(function AgentListRow({
  where,
  node,
  facts,
  status,
  depth,
  inWorkspace = false,
  managerView = false,
  treeAccent,
  managerFolded,
  foldedNeeds = 0,
  managerFoldsFrozen,
  onFoldManager,
  chosenManagerName,
  elapsed,
  nowMs,
  onReveal,
  onHideTab,
  onOpenStory,
  onStop,
  onLaunchUnder,
  onMessage,
  onOpenInbox,
  onQuickSend,
  onChat,
  inPinned = false,
  showTeam = false,
  showManagerPill = false,
  launcherName,
  onSelectHandle,
  onTogglePin,
}: AgentListRowProps): React.JSX.Element {
  const { row, story } = node;
  const inboxEligible = useRunningAgent(row.sessionId ?? "");
  const { rowTeam, offlineMachine, hidden, pinned } = facts;
  const arrivedLate = useArrivedLate(facts.settled);
  const question = useManagedSessionStore((s) =>
    row.sessionId ? questionFor(s.pendingAsks, row.sessionId) : undefined,
  );
  const model = useRowModel(row);
  const needsYou = isNeedsYouStatus(status);
  const names = agentRowNames(row);
  const label = names.primary;
  const role = resolveAgentRole(row, { launched: facts.launched, choice: facts.choice });
  const face = resolveAgentFace(row, { name: label, choice: facts.choice, faces: facts.faces });
  const quickViewHint = `Quick view: ${label}'s terminal in a popup (Q)`;
  const chatHint = `Messages with ${label}: its inbox, without leaving your tab (M)`;
  const rowEl = useRef<HTMLDivElement>(null);
  const ctxAnchor = (): HTMLElement | null =>
    rowEl.current?.querySelector<HTMLElement>(".ag-main") ?? rowEl.current;
  const current = facts.current;
  const headless = isHeadlessSession(row);
  // An agent on an offline machine has no live terminal, so it offers no quick view.
  const quickSendFn = offlineMachine ? undefined : onQuickSend;
  const rowMark = markWithAvailability(markOfAgentStatus(status), !!offlineMachine);
  // A pinned agent stays in its place in its workspace, dimmed.
  const dimmed = pinned && !inPinned;
  const pinLabel = pinned ? "Unpin" : "Pin";
  const pinHint = pinned ? `Unpin ${label}` : `Pin ${label}: it stays on top until it ends`;
  // A routine's wake that waits for this session to be idle.
  const dueWake = useRoutinesStore((s) => queuedWakeFor(s.rows, row.sessionId, facts.machineId));
  const holds = facts.holds;
  const holdsText = holds.length > 0 ? `Holds: ${holds.map((h) => h.label).join(", ")}` : null;
  const markers = rowMarkers(
    {
      hidden,
      headless,
      // In the Pinned group the group's own label says it.
      pinned: pinned && !inPinned,
      due: dueWake !== null,
    },
    facts.narrow,
  );
  const showActions = !facts.narrow;
  // The hover actions drawn: Quick view (not while the machine is offline), Messages (when the
  // list offers the chat) and Pin.
  const actionCount = showActions
    ? (quickSendFn ? 1 : 0) + (inboxEligible && onChat ? 1 : 0) + 1
    : 0;
  const subagents = useSubagentsOpen(row.sessionId);
  // An unreachable machine's row hides its status word; the prompt it was started with stays.
  const task = offlineMachine ? (row.startedWith ?? "") : rowTask(row, status, subagents);
  // While its subagents run, line one says "Working" and a small mark beside the word counts
  // them; the tooltip and the mark's own name say it in full.
  const subagentsShown = offlineMachine ? 0 : shownSubagents(status, subagents);
  const lineTask = subagentsShown ? rowTask(row, status, subagents, true) : task;
  const viaShown = showManagerPill && !!row.launchedBy;
  const viaName = launcherName ?? row.launchedBy ?? "";
  const place = rowWorkspace(node, { routine: facts.routine, tabless: facts.tabless });
  // Line two keeps the role and workspace even when the machine is offline; the Offline tag and
  // the hint say the rest. A Status row names the team first, since its sections span teams.
  const machine = facts.machine;
  const roleWord = role.label.split(" · ")[0] ?? "";
  const teamWord = showTeam ? facts.teamLabel : null;
  const detail = [teamWord, agentDetailLine({ role: roleWord, workspace: place, machine, model })]
    .filter(Boolean)
    .join(" · ");
  // Line two leaves out what goes without saying; the tooltip keeps the full line. The piece says
  // a manager's or a worker's role, an agent on This Mac says nothing about its machine (a remote
  // one names its machine and shows its mark), and under its workspace card the row leaves the
  // workspace out: the card names it. What runs it and for how long take the room instead; an
  // ended run's clock stops counting, so it shows none (an offline machine's agent runs on).
  const limited = limitedWord(row, status);
  const stoppedWord =
    rowMark === "ended" || rowMark === "crashed" ? AGENT_STATUS_WORDS[status] : null;
  const { lead: detailHead, tail: detailTail } = agentRowLine({
    team: teamWord,
    role: names.secondary ?? roleWord,
    pieceShowsRole: !names.secondary && role.piece !== "custom",
    workspace:
      managerView || (inWorkspace && node.workspace && place === node.workspace.label)
        ? null
        : place,
    machine: machine === THIS_MAC ? null : machine,
    model,
    elapsed: stoppedWord ? null : elapsed,
    name: label,
    hasVia: viaShown,
    stopped: stoppedWord,
  });
  const shownDetail = [detailHead, detailTail].filter(Boolean).join(" · ");
  // How CodeSense reaches the session, in every live state: typed by hand (its mail waits
  // unwoken) or a CLI release outside the qualified range. Settings lists this Mac's CLIs only.
  const rowCli = useRowCli(row);
  const liveRow = !stoppedWord;
  const handTyped = liveRow && isTypedByHand(row);
  const release = liveRow ? releaseWarning(row) : null;
  const unread = row.unread ?? 0;
  const onThisMac = machine === THIS_MAC;
  // A remote agent's machine shows its colour and icon: the server in its colour, or the amber
  // disconnected glyph while it reads offline (by the same rule as the Offline tag).
  const remoteMachine = facts.remoteMachine;
  const offlineHint = offlineMachine
    ? `${offlineMachine} is offline. CodeSense is reconnecting${rowMark === "offline" ? "; the agent keeps running there" : ""}. Enter shows it, and its inbox still takes messages; only the quick view needs the machine online.`
    : null;
  // The agent's own latest `signal_progress` note (or a step-of-total hint), read for this
  // session only, so another session's progress tick leaves the row alone. It also carries the
  // note of a block the agent reports; on the other waiting rows their own ask line wins.
  const progress = useSessionProgress(row.sessionId ?? "");
  const waiting =
    status === "awaiting-answer"
      ? (question ?? "Waiting for your answer")
      : status === "paused"
        ? "Waiting for your approval"
        : status === "blocked"
          ? blockedLine(progress)
          : null;
  // Line one drops the bare status word only where something else on the row already says it:
  // an ask line that says the word itself ("Blocked: …"), or a stopped run whose line two says
  // Ended. An ask line that shows only the question leaves "Needs answer" on line one.
  const statusWord = limited ? LIMITED_WORD : AGENT_STATUS_WORDS[status];
  const askSaysWord = waiting !== null && askLeadsWithWord(waiting, statusWord);
  const lineOneTask =
    (askSaysWord || (stoppedWord && detailHead === stoppedWord && !detailTail)) &&
    (lineTask === statusWord || lineTask.startsWith(`${statusWord} · `))
      ? lineTask.slice(statusWord.length + 3)
      : lineTask;
  // Line one's status word, which never truncates, and the launch prompt after it, which does.
  // A line with no status word (an offline machine's row, or nothing reporting) is the prompt
  // alone.
  const prompt = row.startedWith;
  const lineOnePrompt =
    prompt && (lineOneTask === prompt || lineOneTask.endsWith(` · ${prompt}`)) ? prompt : "";
  const lineOneWord = lineOnePrompt
    ? lineOneTask.slice(0, lineOneTask.length - lineOnePrompt.length).replace(/ · $/, "")
    : lineOneTask;
  // The subagent mark goes right after the status word, before a launch prompt.
  const subsAfter =
    subagentsShown &&
    (lineOneTask === SUBAGENTS_WORKING_WORD ||
      lineOneTask.startsWith(`${SUBAGENTS_WORKING_WORD} · `))
      ? SUBAGENTS_WORKING_WORD
      : null;
  const activityTick = row.activityAtMs;
  const activity =
    status === "stalled"
      ? activityTick === undefined
        ? "Stalled"
        : `Last activity ${Math.floor(Math.max(0, nowMs - activityTick) / 60_000)}m ago`
      : waiting || progress?.phase === "blocked"
        ? null
        : activityLine(progress, names.source === "progress");
  // Shared availability precedence retains known Ended/Crashed outcomes;
  // otherwise a disconnected host replaces the last live mark with Offline.
  const qualifier = useAgentStateStore((s) =>
    activityQualifier(s.activityBySession[row.sessionId ?? ""], rowMark),
  );
  const tooltip = [
    `${names.full}${task ? ` · ${task}` : ""}`,
    detail,
    viaShown && `Launched by ${viaName}`,
    chosenManagerName && `Chosen manager: ${chosenManagerName}`,
    qualifier,
    handTyped && typedByHandHint(rowCli, unread),
    release && releaseHint(release, onThisMac),
    activity && (status === "stalled" ? activity : `Doing: ${activity}`),
    elapsed && `Running ${elapsed}`,
    holdsText,
    // An offline agent reads unknown for its machine, not for its hooks: its own hint says why.
    status === "unknown" && !offlineMachine && UNKNOWN_ACTIVITY_HINT,
    offlineHint,
    hidden && HIDDEN_TAB_HINT,
    headless && HEADLESS_HINT,
    quickSendFn ? rowGesturesHint() : OFFLINE_ROW_GESTURES_HINT,
  ]
    .filter(Boolean)
    .join("\n");

  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        <div
          ref={rowEl}
          className={cn("ag-row", arrivedLate && "ag-enter")}
          data-needs={needsYou || undefined}
          data-current={current || undefined}
          data-offline={rowMark === "offline" || undefined}
          data-dead={rowMark === "ended" || rowMark === "crashed" || undefined}
          data-pinned={dimmed ? "dim" : pinned ? "true" : undefined}
          data-agent-handle={row.handle}
          data-context={node.scopeContext || undefined}
          data-parent-source={node.parentSource}
          data-manager-view={managerView || undefined}
          data-actions={actionCount || undefined}
          style={
            {
              ...(treeAccent && {
                "--ag-guide": treeAccent,
                "--ag-elbow-width": `${16 + Math.min(depth, MAX_INDENT_LEVELS) * TREE_INDENT_PX}px`,
              }),
              paddingLeft:
                ROW_INSET_PX +
                (managerView ? 16 : 0) +
                (inWorkspace ? WORKSPACE_INDENT_PX : 0) +
                Math.min(depth, MAX_INDENT_LEVELS) * TREE_INDENT_PX,
              // Line one gives this much room to the hover actions, so its text truncates
              // before them instead of running underneath.
              ...(actionCount > 0 && { "--ag-row-actions-n": actionCount }),
            } as CSSProperties
          }
        >
          {treeAccent && (
            <span
              className="ag-tree-guide"
              data-testid="agent-tree-guide"
              aria-hidden
              style={{ left: ROW_INSET_PX + 7 }}
            />
          )}
          {onFoldManager && (
            <button
              type="button"
              className="ag-manager-fold"
              aria-label={`${managerFolded ? "Expand" : "Collapse"} ${label}'s crew`}
              aria-expanded={!managerFolded}
              disabled={managerFoldsFrozen}
              onClick={onFoldManager}
              style={{ left: 2 + Math.min(depth, MAX_INDENT_LEVELS) * TREE_INDENT_PX }}
            >
              <ChevronDown className="size-3" data-folded={managerFolded || undefined} />
            </button>
          )}
          <button
            type="button"
            className="ag-main flex min-w-0 flex-1 items-start gap-1.5 py-[6px] pl-1 pr-2 text-left"
            title={tooltip}
            // The description leads with the row's place, then the tooltip's words (a description
            // set here replaces the one the title would give).
            aria-description={where ? `${where}\n${tooltip}` : undefined}
            // The agent's own row is the current one; its Pinned copy only repeats it.
            aria-current={(current && !inPinned) || undefined}
            onClick={(e) => {
              // Option-click opens the quick-send popup; a plain click shows the agent. On an
              // offline agent Option-click does nothing (no popup, no show).
              if (e.altKey) {
                quickSendFn?.(row, rowTeam, e.currentTarget, waiting ?? undefined);
                return;
              }
              onReveal(row);
            }}
            onKeyDown={(e) => {
              const plainKey = !e.metaKey && !e.ctrlKey && !e.altKey;
              // M opens the inbox chat (offline agents too: their inbox keeps the mail).
              if (inboxEligible && onChat && plainKey && /^m$/i.test(e.key)) {
                e.preventDefault();
                if (!e.repeat) onChat(row, rowTeam, e.currentTarget, waiting ?? undefined);
                return;
              }
              // Enter, Space and Q open the quick view popup: the keyboard's way to the terminal
              // without leaving the person's own tab.
              const opens = e.key === "Enter" || e.key === " " || (/^q$/i.test(e.key) && plainKey);
              if (quickSendFn && opens) {
                e.preventDefault();
                if (!e.repeat) quickSendFn(row, rowTeam, e.currentTarget, waiting ?? undefined);
                return;
              }
              // An agent with no quick view (its machine is offline): Enter and Space show it.
              if ((e.key === "Enter" || e.key === " ") && !e.repeat) {
                e.preventDefault();
                onReveal(row);
              }
            }}
          >
            {/* The status dot sits on the avatar's bottom-right, inside its box, so its ring
                reads against the face rather than the row. The trailing markers say the rest. */}
            <span className="ag-avatar-wrap mt-px" data-tone={avatarStatusGroup(rowMark)}>
              <AgentAvatar
                face={face}
                role={role}
                pieceTone={NEUTRAL_PIECE}
                size={AVATAR_PX}
                pieceSize={ROW_PIECE_PX}
                tag={facts.faceTag}
              />
              <AvatarStatusDot
                mark={rowMark}
                sizePx={STATUS_DOT_PX}
                label={withActivityQualifiers(
                  rowMark === "offline"
                    ? MARK_WORDS.offline
                    : limited
                      ? LIMITED_WORD
                      : agentStatusWords(status, subagents),
                  [qualifier],
                )}
              />
            </span>
            <span className="ml-1 flex min-w-0 flex-1 flex-col">
              <span className="ag-line1 flex min-w-0 items-baseline">
                {subsAfter ? (
                  // While subagents run, the title splits so the mark never truncates away: the
                  // launch prompt gives way first, then the name.
                  <span className="ag-title ag-title-split min-w-0 flex-1">
                    <span className="ag-title-lead">
                      <span className="ag-name" title={names.full} aria-label={names.full}>
                        {label}
                      </span>
                      {remoteMachine && (
                        <MachineMark
                          identity={remoteMachine}
                          size={10}
                          testId="agent-row-machine"
                        />
                      )}
                      <span className="ag-task" data-tone={avatarStatusGroup(rowMark)}>
                        {` · ${subsAfter}`}
                      </span>
                    </span>
                    <span className="ag-task ag-subs-slot" data-tone={avatarStatusGroup(rowMark)}>
                      <span
                        onClick={
                          managerView && quickSendFn
                            ? (e) => {
                                e.stopPropagation();
                                quickSendFn(row, rowTeam, e.currentTarget, waiting ?? undefined);
                              }
                            : undefined
                        }
                      >
                        <SubagentMark count={subagentsShown} />
                      </span>
                    </span>
                    {lineOneTask.length > subsAfter.length && (
                      <span
                        className="ag-task ag-title-rest"
                        data-tone={avatarStatusGroup(rowMark)}
                      >
                        {lineOneTask.slice(subsAfter.length)}
                      </span>
                    )}
                  </span>
                ) : (
                  // The name gives way before the status word, so the hover actions never cut
                  // the word; a launch prompt after the word truncates.
                  <span className="ag-title ag-title-line min-w-0 flex-1">
                    <span
                      className="ag-name ag-title-name"
                      title={names.full}
                      aria-label={names.full}
                    >
                      {label}
                    </span>
                    {remoteMachine && (
                      <MachineMark identity={remoteMachine} size={10} testId="agent-row-machine" />
                    )}
                    {lineOneTask && (
                      <span
                        className="ag-task ag-title-task"
                        data-tone={avatarStatusGroup(rowMark)}
                      >
                        {lineOneWord && (
                          <span className="ag-title-word">{` · ${lineOneWord}`}</span>
                        )}
                        {lineOnePrompt && (
                          <span className="ag-title-more">
                            <span className="ag-title-sep"> · </span>
                            {lineOnePrompt}
                          </span>
                        )}
                      </span>
                    )}
                  </span>
                )}
                {offlineMachine && (
                  <span className="ag-offline-tag" title={`${offlineMachine} can't be reached`}>
                    <WifiOff className="mr-0.5 inline size-2.5" aria-hidden />
                    Offline
                  </span>
                )}
                {markers.map((marker) => (
                  <RowMarkerView key={marker} marker={marker} dueWake={dueWake} agentName={label} />
                ))}
                {managerView && onFoldManager && managerFolded && !managerFoldsFrozen && (
                  <span
                    className="ag-folded-count"
                    data-needs={foldedNeeds > 0 || undefined}
                    role="img"
                    title={foldedCrewLabel(countNodes(node.children), foldedNeeds)}
                    aria-label={foldedCrewLabel(countNodes(node.children), foldedNeeds)}
                  >
                    {foldedNeeds > 0 && <StatusMark mark="question" size={6} />}+
                    {countNodes(node.children)}
                  </span>
                )}
              </span>
              {node.scopeContext && <span className="ag-chosen-manager">Manager context</span>}
              {chosenManagerName && (
                <span className="ag-chosen-manager" title={`Chosen manager: ${chosenManagerName}`}>
                  Chosen manager: {chosenManagerName}
                </span>
              )}
              <span className="ag-sub flex min-w-0" data-testid="agent-detail">
                {viaShown ? (
                  // Who launched it follows the role, its name giving way before the role does.
                  // The rest of line two follows when it fits whole, and wraps out of sight when
                  // it does not, so the line never ends on a stray "· …".
                  <span className="ag-via-line">
                    <span className="ag-via-lead">
                      {detailHead && <span className="ag-via-head">{`${detailHead} · `}</span>}
                      {/* Inside the row's own <button>, so not a control of its own: a click
                          selects the launcher, and the row's menu offers the same for keys. */}
                      <span
                        className="ag-via-pill"
                        data-testid="agent-manager-pill"
                        data-launcher={row.launchedBy}
                        data-selects={onSelectHandle ? "true" : undefined}
                        data-manager-offline={facts.managerOffline ? "true" : undefined}
                        title={
                          onSelectHandle
                            ? `Launched by ${viaName}: click to go to it`
                            : `Launched by ${viaName}`
                        }
                        onClick={(e) => {
                          if (!onSelectHandle) return;
                          e.stopPropagation();
                          onSelectHandle(row.launchedBy!, rowTeam);
                        }}
                      >
                        <ViaGlyph />
                        <span className="truncate">{viaName}</span>
                      </span>
                    </span>
                    {detailTail && (
                      <span className="ag-via-rest" style={{ whiteSpace: "pre" }}>
                        {` · ${detailTail}`}
                      </span>
                    )}
                  </span>
                ) : (
                  <span className="min-w-0 truncate">{shownDetail}</span>
                )}
                {managerView && (
                  <span
                    className="ag-workspace-badge"
                    data-current-workspace={facts.workspaceCurrent || undefined}
                    title={
                      node.workspace?.storyTitle
                        ? `${node.workspace.label} · ${node.workspace.storyTitle}`
                        : (node.workspace?.label ?? "No workspace")
                    }
                  >
                    {node.workspace?.label ?? "No workspace"}
                  </span>
                )}
                {row.roleJoinError && !holds.some((h) => h.role === row.queueRole) && (
                  <span
                    className="rounded border border-dashed border-amber-500 px-1 text-xs text-amber-700 dark:text-amber-400"
                    title={row.roleJoinError}
                  >
                    Not joined: {row.queueRole}
                  </span>
                )}
              </span>
              {activity && (
                // Up to two lines, so a status note reads whole; the hover shows anything longer.
                <span
                  className="ag-activity line-clamp-2 break-words"
                  title={activity}
                  data-testid="agent-activity"
                  data-settled={progress?.settled || undefined}
                >
                  {activity}
                </span>
              )}
              {waiting && <span className="ag-ask truncate">{waiting}</span>}
            </span>
          </button>
          {/* Its own wrapping lines under the row's button, outside line two's fixed-height,
              clipped via line, so their actions are buttons of their own and a launcher's name
              or a workspace badge can never squeeze them out. */}
          <SessionNotices
            sessionId={row.sessionId}
            cli={rowCli}
            typedByHand={handTyped}
            unread={unread}
            release={release !== null}
            canCheckVersion={onThisMac}
            onReveal={() => onReveal(row)}
          />
          {managerView && subagentsShown > 0 && (
            <button
              type="button"
              className="ag-native-detail"
              disabled={!quickSendFn}
              onClick={(e) => quickSendFn?.(row, rowTeam, e.currentTarget, waiting ?? undefined)}
              aria-label={`${subagentsShown} native subagents working · Quick view of ${label}`}
            >
              {subagentsShown} native subagents working
            </button>
          )}
          <SessionRoleChips holds={holds} />
          {/* Narrow, these live in the row's menu only. */}
          {showActions && (
            <span className="ag-row-actions">
              {quickSendFn && (
                <span className="ag-row-action" title={quickViewHint}>
                  <button
                    type="button"
                    aria-label="Quick view"
                    aria-description={quickViewHint}
                    data-testid="agent-row-cli"
                    onClick={(e) => {
                      // The popup only: the CLI stays on the user's current tab, and no inbox
                      // opens (the chat icon is the inbox's way in).
                      quickSendFn(row, rowTeam, e.currentTarget, waiting ?? undefined);
                    }}
                  >
                    <Terminal className="size-3.5" aria-hidden />
                  </button>
                </span>
              )}
              {inboxEligible && onChat && (
                <span className="ag-row-action" title={chatHint}>
                  <button
                    type="button"
                    aria-label="Messages"
                    aria-description={chatHint}
                    data-testid="agent-row-message"
                    onClick={(e) => {
                      // The chat popup only: the CLI stays on the user's current tab.
                      onChat(row, rowTeam, e.currentTarget, waiting ?? undefined);
                    }}
                  >
                    <MessageSquare className="size-3.5" aria-hidden />
                  </button>
                </span>
              )}
              <span className="ag-row-action" title={pinHint}>
                <button
                  type="button"
                  aria-label={pinLabel}
                  aria-description={pinHint}
                  aria-pressed={pinned}
                  data-testid="agent-row-pin"
                  onClick={() => onTogglePin(rowTeam, row, label)}
                >
                  {pinned ? (
                    <PinOff className="size-3.5" aria-hidden />
                  ) : (
                    <Pin className="size-3.5" aria-hidden />
                  )}
                </button>
              </span>
            </span>
          )}
        </div>
      </ContextMenuTrigger>
      <ContextMenuContent>
        {/* Mounted only while the menu is open, so only an open menu reads the pane's identity. */}
        <RowMenu
          node={node}
          rowTeam={rowTeam}
          label={label}
          choice={facts.choice}
          holds={holds}
          hasTab={facts.hasTab}
          headless={headless}
          offlineMachine={offlineMachine}
          viaName={viaName}
          pinLabel={pinLabel}
          pinHint={pinHint}
          onQuickView={
            quickSendFn
              ? () => {
                  // The row can have left the drawn window while its menu was open.
                  const anchor = ctxAnchor();
                  if (anchor?.isConnected) quickSendFn(row, rowTeam, anchor, waiting ?? undefined);
                }
              : undefined
          }
          onReveal={onReveal}
          onSelectHandle={onSelectHandle}
          onHideTab={onHideTab}
          onTogglePin={onTogglePin}
          onMessage={onMessage}
          onOpenInbox={onOpenInbox}
          onOpenStory={story ? () => onOpenStory(story) : undefined}
          onLaunchUnder={onLaunchUnder}
          onStop={onStop}
        />
      </ContextMenuContent>
    </ContextMenu>
  );
}, sameRowProps);

/** One trailing marker on a row's first line. */
function RowMarkerView({
  marker,
  dueWake,
  agentName,
}: {
  marker: RowMarker;
  dueWake?: RoutineRow | null;
  agentName?: string;
}): React.JSX.Element {
  switch (marker) {
    case "due": {
      const until = dueWake?.queued?.deadlineMs
        ? ` (until ${clockTime(dueWake.queued.deadlineMs)})`
        : "";
      const name = dueWake?.def.name ?? "A routine";
      return (
        <span
          className="ag-count ml-1 inline-flex shrink-0 items-center gap-0.5 text-[var(--ag-muted)]"
          role="img"
          aria-label={`Wake queued: ${name}`}
          title={`${name} goes in when ${agentName ?? "the agent"} is idle${until}`}
          data-testid="agent-due-mark"
        >
          <AlarmClock className="size-2.5" aria-hidden />
          due
        </span>
      );
    }
    case "hidden":
      return (
        <span
          className="ag-hidden-mark ml-1 shrink-0 self-center"
          role="img"
          aria-label="Tab hidden"
          title={HIDDEN_TAB_HINT}
        >
          <EyeOff className="size-3" aria-hidden />
        </span>
      );
    case "headless":
      return (
        <span
          className="ag-hidden-mark ml-1 shrink-0 self-center"
          role="img"
          aria-label="Headless"
          title={HEADLESS_HINT}
          data-testid="agent-headless-mark"
        >
          <ScrollText className="size-3" aria-hidden />
        </span>
      );
    case "pin":
      return (
        <span
          className="ag-hidden-mark ag-pin-mark ml-1 shrink-0 self-center"
          role="img"
          aria-label="Pinned"
          title="Pinned"
          data-testid="agent-pin-mark"
        >
          <Pin className="size-3" aria-hidden />
        </span>
      );
  }
}

/** Whether an ask line starts with the status word as a word of its own, so line one need not
 *  say the word again: the word (ignoring case and the ask's leading space), then the end of the
 *  ask, a space, one of `:` `;` `,` `.` `!` `?` `–` `—`, or a hyphen with a space after it
 *  ("Blocked", "Blocked: …", "blocked — …", "Blocked - …", "Blocked. …"). "Blockers …" does not
 *  lead with "Blocked", and nor does "Blocked-by-review", whose hyphen joins one word. An empty or
 *  missing word never matches. */
export function askLeadsWithWord(ask: string, word: string | null | undefined): boolean {
  const w = word?.trim().toLowerCase();
  if (!w) return false;
  const a = ask.trim().toLowerCase();
  if (!a.startsWith(w)) return false;
  return /^(?:$|\s|[:;,.!?–—]|-(?:\s|$))/.test(a.slice(w.length));
}

/** Nothing running yet: say what will appear here, and where agents come from. Under the agent
 *  chip, say its agent has nothing running, or that it runs only in a team the pane hides.
 *  Without the chip, `hiddenTeams` are the hidden teams whose agents are all there is: say so,
 *  and offer to show each. */
function EmptyState({
  filtered,
  hiddenTeams = [],
  onShowTeam,
  onClearAgentFilter,
  onLaunch,
}: {
  filtered: string | null;
  /** The hidden teams the chip's agent runs in or, with no chip, the hidden teams with agents. */
  hiddenTeams?: readonly string[];
  onShowTeam?: (teamKey: string) => void;
  onClearAgentFilter?: () => void;
  onLaunch: () => void;
}): React.JSX.Element {
  // A name two open teams share reads as "org › team".
  const { teamLabelOf } = useContext(Identity);
  const names = hiddenTeams.map(teamLabelOf);
  if (!filtered && hiddenTeams.length > 0) {
    return (
      <div className="px-[15px] py-3" data-testid="agents-hidden-teams">
        <p className="text-[12px] font-medium text-[var(--ag-text)]">Agents run in hidden teams</p>
        <p className="mt-1 text-[11px] leading-[15px] text-[var(--ag-faint)]">
          The teams shown have no agents running.
        </p>
        {onShowTeam && (
          <div className="mt-1.5 flex flex-wrap gap-x-3">
            {hiddenTeams.map((team, i) => (
              <button key={team} type="button" className="ag-link" onClick={() => onShowTeam(team)}>
                Show {names[i]}
              </button>
            ))}
          </div>
        )}
      </div>
    );
  }
  if (filtered && hiddenTeams.length > 0) {
    return (
      <div className="px-[15px] py-3" data-testid="agents-chip-hidden">
        <p className="text-[12px] font-medium text-[var(--ag-text)]">
          {hiddenTeams.length === 1
            ? `${filtered} runs in ${names[0]}, a hidden team`
            : `${filtered} runs only in hidden teams`}
        </p>
        <div className="mt-1.5 flex flex-wrap gap-x-3">
          {onShowTeam &&
            hiddenTeams.map((team, i) => (
              <button key={team} type="button" className="ag-link" onClick={() => onShowTeam(team)}>
                Show {names[i]}
              </button>
            ))}
          {onClearAgentFilter && (
            <button type="button" className="ag-link" onClick={onClearAgentFilter}>
              Clear agent filter
            </button>
          )}
        </div>
      </div>
    );
  }
  return (
    <div className="px-[15px] py-3">
      <p className="text-[12px] font-medium text-[var(--ag-text)]">
        {filtered ? `No ${filtered} sessions running` : "No agents running"}
      </p>
      {!filtered && (
        <p className="mt-1 text-[11px] leading-[15px] text-[var(--ag-faint)]">
          Agents you or other agents launch show here, grouped by team and workspace.
        </p>
      )}
      {!filtered && (
        <button type="button" className="ag-link mt-1.5" onClick={onLaunch}>
          Launch agent
        </button>
      )}
    </div>
  );
}

export function foldedCrewLabel(count: number, needs: number): string {
  return `${count} ${count === 1 ? "agent" : "agents"} folded${needs ? `, ${needs} ${needs === 1 ? "needs" : "need"} you` : ""}`;
}
