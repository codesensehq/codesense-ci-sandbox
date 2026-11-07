// SPDX-License-Identifier: MIT
import { commitTextEditOnBlur } from "@/shared/lib/text-menu-focus";
import { useRoleQueues } from "@/shared/hooks/use-role-queues";
import { useRoleConfigs } from "@/shared/hooks/use-role-configs";
import { QueueRole } from "@/shared/components/queue-role";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  Check,
  ChevronDown,
  CircleAlert,
  FolderOpen,
  GitBranch,
  GitFork,
  KeyRound,
  Minus,
  Rocket,
  Search,
  TriangleAlert,
  Wrench,
} from "lucide-react";
import { toast } from "sonner";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  LaunchProgress,
  LaunchTargetPicker,
  useLaunchTarget,
} from "@/shared/components/launch-target-picker";
import {
  isLaunchable,
  runLaunchWithProgress,
  type LaunchOpen,
  type LaunchPhase,
} from "@/shared/lib/launch-target";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { PortaledSelect } from "@/components/ui/portaled-select";
import { Segmented } from "@/components/ui/segmented";
import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { AgentRowFace } from "@/features/notes/components/agent-row-face";
import { MachineIcon } from "@/features/machines/components/machine-status-label";
import { workspaceMachineChoices } from "@/features/machines/lib/machine-choices";
import { useTeamMachines } from "@/features/machines/stores/machines-store";
import { agentLaunchNames, rememberAgentLaunch } from "@/features/viewer/lib/agent-launch-retry";
import { AgentAvatar } from "@/features/viewer/components/agent-identity";
import {
  useTeamAgentIdentities,
  type TeamAgentIdentity,
} from "@/features/viewer/hooks/use-team-agent-identities";
import {
  paneWorkspaceOf,
  makePaneWorkspaceSources,
} from "@/features/viewer/lib/agent-list-workspaces";
import {
  resolveAgentFace,
  resolveAgentRole,
  THIS_MAC,
  type AgentFace,
} from "@/features/viewer/lib/agent-list-view";
import { useWorktreeRosterStore } from "@/features/viewer/stores/worktree-roster-store";
import { useAgentListProjectionStore } from "@/features/viewer/lib/agent-list-projection";
import { useAvatarPacks } from "@/shared/hooks/use-avatar-packs";
import { useNow } from "@/shared/hooks/use-now";
import { useTeamWorkspaceContext } from "@/shared/hooks/use-active-workspace-context";
import { userFacingErrorMessage } from "@/shared/lib/error-message";
import { logger } from "@/shared/lib/logger";
import { fuzzyIdTitleScore } from "@/shared/lib/fuzzy-match";
import { selectedIndexById } from "@/shared/lib/picker-selection";
import { storyFilePathFromTree } from "@/shared/lib/pipeline-graph/resolve-story-file-path";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { usePipelineTree } from "@/shared/lib/pipeline-graph/use-pipeline-tree";
import { normalizeStoryFilePath, storyIdFromPath } from "@/shared/lib/worktree/resolve";
import { isRemoteWorkspace } from "@/shared/lib/workspace-location";
import { NO_WORKSPACE_ID, teamFoldKey, workspaceFoldKey } from "@/shared/lib/workspace-key";
import {
  findTabLocationBySessionId,
  selectHiddenTerminalTabIndex,
  selectMinimizedTerminalTabIndex,
  selectRunningTerminalTabIndex,
  useViewerStore,
  type ViewerState,
  type Workspace,
} from "@/shared/stores/viewer-store";
import {
  agentDetail,
  agentLaunchOpen,
  type AgentDefinition,
  type AgentLaunchWarning,
  type ContextWorktreeInfo,
  LOCAL_MACHINE_ID,
  type AgentRow,
  type AgentSessionRow,
  type MachineSummary,
} from "@/tauri/commands";

import { useLaunchStories } from "../lib/use-launch-stories";
import { groupAgents, placeLabel, runtimeLabel, accountLine } from "../lib/agent-display";
import { AGENTS_LAUNCHER, toAgentOpenWorkspaceArg } from "../lib/agent-launch";
import {
  modelWord,
  NEEDS_MODEL_ENDPOINT,
  needsModelEndpoint,
  shortAccount,
} from "../lib/agent-row-state";
import { useNewBranchBase } from "../lib/use-new-branch-base";
import { openAgentAccount, openAgentEditSection } from "../agents-virtual-tab";
import { PreviewBadge, previewNoteRef } from "@/shared/components/preview-badge";
import { PREVIEW_RUNTIME_NOTE } from "@/shared/lib/agent-runtimes";
import {
  openLaunchSheet,
  defaultAgentKey,
  launchHandle,
  launchMachineLock,
  suggestName,
  toLaunchArgs,
  validateLaunch,
  type LaunchField,
  type MachineLock,
  type LaunchSheetForm,
  type LaunchSheetRequest,
  type LaunchWhere,
} from "../lib/launch-sheet";
import { useAgentsStore, watchAgentList } from "../stores/agents-store";
import { draftFromDefinition, emptyDraft, formFromDraft } from "../lib/agent-form";
import { useAgentPreview } from "../lib/use-agent-preview";
import { FacePicker } from "./face-picker";

const LOCAL_MACHINE = LOCAL_MACHINE_ID;

/** The first-prompt box's placeholder, naming the agent's own default prompt when it has one. */
function taskPlaceholder(agent: AgentRow | undefined): string {
  const own = agent?.default_prompt.trim();
  return own
    ? `Ask it something, or leave empty to start with ${own}`
    : "Ask it something, or describe the job.";
}

/** The card of the agents pane a launched agent lands in: the workspace the pane places its
 *  session in (`placed`, once the roster lists it), else, by where it was launched, the
 *  no-workspace card for its team folder (its own folder has no card), or its story's workspace,
 *  open or minimized, or the one its story will open as. Null: a story whose card can't be named
 *  yet. */
export function launchLandsIn(
  where: LaunchWhere,
  card: {
    placed: string | null;
    storyWorkspaceId: string | null;
    storyWorkspaceIdToBe: string | null;
  },
): string | null {
  if (card.placed !== null) return card.placed;
  switch (where) {
    case "team-workspace":
      return NO_WORKSPACE_ID;
    case "story":
      return card.storyWorkspaceId ?? card.storyWorkspaceIdToBe;
    default:
      where satisfies never;
      return null;
  }
}

/** The roster row of the agent just launched as `handle` into `teamKey`: one handle can run in two
 *  teams, so a row whose tab is in another team is not it. A row whose tab is in `teamKey` comes
 *  first, then one whose session has no tab anywhere yet. */
export function launchedRowIn(
  rows: readonly AgentSessionRow[] | null | undefined,
  handle: string,
  teamKey: string,
  tabTeamOf: (sessionId: string) => string | null,
): AgentSessionRow | undefined {
  const named = (rows ?? []).filter((r) => r.handle === handle);
  const teamOf = (r: AgentSessionRow) => (r.sessionId ? tabTeamOf(r.sessionId) : null);
  return named.find((r) => teamOf(r) === teamKey) ?? named.find((r) => teamOf(r) === null);
}

/** How long a launched agent is waited for in the roster, when the read that followed its launch
 *  did not list it (a newer read superseded it), before its reveal is given up. */
const LAUNCHED_ROW_WAIT_MS = 30_000;

/** The team a session's tab is in, open, minimized or hidden. */
function tabTeamIn(viewer: ViewerState, sessionId: string): string | null {
  return (
    (
      findTabLocationBySessionId(selectRunningTerminalTabIndex(viewer), sessionId) ??
      findTabLocationBySessionId(selectMinimizedTerminalTabIndex(viewer), sessionId) ??
      findTabLocationBySessionId(selectHiddenTerminalTabIndex(viewer), sessionId)
    )?.teamKey ?? null
  );
}

/** Whether the pane's workspace arrow still shows a launched agent that lands in `landsIn` of
 *  `teamKey`: only an arrow on that very card does. */
export function launchKeepsArrow(
  arrow: { teamKey: string; workspaceId: string },
  teamKey: string,
  landsIn: string | null,
): boolean {
  return arrow.teamKey === teamKey && landsIn !== null && arrow.workspaceId === landsIn;
}

/** The workspace the agents pane lists `row` of `teamKey` under, read as the pane reads it. */
function paneWorkspaceIn(
  viewer: ViewerState,
  teamKey: string,
  row: Pick<AgentSessionRow, "handle" | "sessionId">,
): Workspace | undefined {
  const slices = {
    workspaces: viewer.workspaces,
    softClosedWorkspaces: viewer.softClosedWorkspaces,
  };
  return paneWorkspaceOf(
    row,
    makePaneWorkspaceSources(
      {
        ...slices,
        running: selectRunningTerminalTabIndex(viewer),
        minimized: selectMinimizedTerminalTabIndex(viewer),
        hidden: selectHiddenTerminalTabIndex(viewer),
        layoutByKey: viewer.terminalLayout.byKey,
      },
      teamKey,
    ),
  );
}

/** `teamKey`'s workspace of story `storyId`, open before minimized. */
function storyWorkspaceIn(
  viewer: Pick<ViewerState, "workspaces" | "softClosedWorkspaces">,
  teamKey: string,
  storyId: string,
): Workspace | undefined {
  if (!storyId) return undefined;
  const ofStory = (w: Workspace) => w.kind === "story" && w.storyId === storyId;
  return (
    (viewer.workspaces[teamKey] ?? []).find(ofStory) ??
    Object.values(viewer.softClosedWorkspaces[teamKey] ?? {})
      .map((shadow) => shadow.workspace)
      .find(ofStory)
  );
}

/** The id a story's workspace opens with, from its file in the pipeline: what its card is keyed
 *  by before it opens. Null when the pipeline does not list the story. */
function storyWorkspaceIdToBe(tree: DerivedPipelineTree | null, storyId: string): string | null {
  const path = tree && storyId ? storyFilePathFromTree(tree, storyId) : null;
  return path ? storyIdFromPath(normalizeStoryFilePath(path)) : null;
}

/** Stands in for the form while no agent is picked; never sent. */
const NO_FORM = formFromDraft(emptyDraft(), { placement: { scope: "personal" } });

/** CLI, selected-worktree and usage-recording warnings shown before Launch on this Mac. */
const SHEET_WARNING_CODES: readonly string[] = [
  "cli_version_unqualified",
  "shadowing_root_document",
  "usage_not_recorded",
];

/**
 * What the saved agent `agentKey` would warn about if launched now, from the same preview the
 * editor reads. Only the warnings about the CLI itself show here: the rest are editor advice.
 * The preview reports the CLI installed on this Mac, so a launch somewhere else shows none, and
 * nothing shows while the preview of the picked agent is loading or refused.
 */
function useLaunchPreview(
  agentKey: string,
  teamKey: string,
  onThisMac: boolean,
  cwd?: string,
  task = "",
): { warnings: AgentLaunchWarning[]; admission?: import("@/tauri/commands").AgentLaunchAdmission } {
  // The saved revision, so a change to the agent (its runtime, say) fetches its definition again.
  const revision = useAgentsStore((s) => s.rows?.find((r) => r.key === agentKey)?.revision);
  const [detail, setDetail] = useState<{
    key: string;
    revision: number | undefined;
    definition: AgentDefinition;
  } | null>(null);
  useEffect(() => {
    if (!agentKey) return;
    let current = true;
    agentDetail(agentKey).then(
      (view) => current && setDetail({ key: agentKey, revision, definition: view.definition }),
      (e: unknown) => logger.debug("agents", `launch preview detail: ${String(e)}`),
    );
    return () => {
      current = false;
    };
  }, [agentKey, revision]);
  const [org, team] = teamKey.split(":");
  const form = useMemo(
    () =>
      detail?.key === agentKey && detail.revision === revision
        ? formFromDraft(
            {
              ...draftFromDefinition(detail.definition),
              ...(task.trim() ? { defaultPrompt: task } : {}),
            },
            { key: agentKey },
          )
        : null,
    [detail, agentKey, revision, task],
  );
  const enabled = form !== null && onThisMac;
  const preview = useAgentPreview(
    form ?? NO_FORM,
    enabled ? { org, team } : undefined,
    enabled,
    cwd,
  );
  if (!enabled || preview.status !== "resolved") return { warnings: [] };
  return {
    warnings: preview.resolved.warnings.filter((w) => SHEET_WARNING_CODES.includes(w.code)),
    admission: preview.resolved.launch_admission,
  };
}

/** Under the first-prompt box: what the agent does with it. */
const FIRST_MESSAGE_HINT = "Passed to the agent as its starting prompt — a /command works too";

/**
 * The launch sheet: pick the agent, its manager, where it works, its name, role and face, and
 * write what it should do. Launch opens it as a tab in that workspace, focused, nested under
 * its manager wearing the chosen face. ⌘↵ launches from anywhere in the sheet.
 */
export function AgentLaunchSheet({
  request,
  onClose,
}: {
  request: LaunchSheetRequest;
  onClose: () => void;
}): React.JSX.Element {
  const rows = useAgentsStore((s) => s.rows);
  useEffect(() => watchAgentList(false), []);
  const agents = useMemo(() => (rows ?? []).filter((r) => !r.archived), [rows]);

  const identities = useTeamAgentIdentities(request.teamKey);
  const avatars = useAvatarPacks();
  const machines = useTeamMachines(request.teamKey);
  const {
    stories,
    loading: storiesLoading,
    error: storiesError,
  } = useLaunchStories(request.teamKey);
  const { repoPath } = useTeamWorkspaceContext(request.teamKey);
  const { tree } = usePipelineTree(repoPath ?? "", "");
  const workspaces = useViewerStore((s) => s.workspaces[request.teamKey]);
  const minimized = useViewerStore((s) => s.softClosedWorkspaces[request.teamKey]);
  const setPaneFold = useViewerStore((s) => s.setAgentsPaneFold);

  const managerRow = identities.find((i) => i.row.handle === request.managerHandle);
  const [form, setForm] = useState<LaunchSheetForm>(() => ({
    agentKey: request.agentKey ?? "",
    manager: request.managerHandle ?? null,
    where:
      request.where ??
      (request.contextWorktreeId
        ? "team-workspace"
        : request.storyId || managerRow?.row.story
          ? "story"
          : "team-workspace"),
    storyId: request.storyId ?? managerRow?.row.story ?? "",
    name: request.name ?? "",
    role: request.role,
    queueRole: request.queueRole,
    face: request.face ?? null,
    task: request.task ?? "",
  }));
  const [nameTouched, setNameTouched] = useState(Boolean(request.name));
  const worktreeRows = useWorktreeRosterStore((s) => s.rows);
  const worktreesError = useWorktreeRosterStore((s) => s.error);
  const reloadWorktrees = useWorktreeRosterStore((s) => s.reload);
  const worktreesLoading = worktreeRows === null && worktreesError === null;
  const [worktreeTouched, setWorktreeTouched] = useState(false);
  const [org, team] = request.teamKey.split(":");
  useEffect(() => {
    void reloadWorktrees();
  }, [reloadWorktrees, request.teamKey]);
  const contextWorktrees = useMemo(
    () =>
      (worktreeRows ?? [])
        .filter(
          (r): r is ContextWorktreeInfo =>
            r.kind === "context" && r.orgSlug === org && r.team === team,
        )
        .sort((a, b) => b.lastUsedAt - a.lastUsedAt),
    [worktreeRows, org, team],
  );
  const [machineId, setMachineId] = useState(LOCAL_MACHINE);
  // Branch or worktree opens the agent through the open-workspace controller, with its own
  // target picker; the other two places go through the targeted agent launch.
  const [branchMode, setBranchMode] = useState(false);
  const branch = useLaunchTarget("repoBranch");
  const [phase, setPhase] = useState<LaunchPhase>("idle");
  const [branchWarnings, setBranchWarnings] = useState<AgentLaunchWarning[]>([]);
  const newBranchBase = useNewBranchBase(branch.kind === "newWorktree" ? branch.fields.repo : "");
  const [showErrors, setShowErrors] = useState(false);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const taskRef = useRef<HTMLTextAreaElement>(null);
  const set = <K extends keyof LaunchSheetForm>(key: K, value: LaunchSheetForm[K]) => {
    setFailure(null);
    setForm((f) => ({ ...f, [key]: value }));
  };

  // The agent defaults once the list lands; the name follows the agent until it is edited.
  const agentKey = form.agentKey || defaultAgentKey(agents);
  const agent = agents.find((a) => a.key === agentKey);
  const { roles: queueRoles } = useRoleConfigs(request.teamKey);
  const effectiveQueue = form.queueRole === undefined ? agent?.role : form.queueRole;
  const { queues } = useRoleQueues(request.teamKey);
  const queueState = queues.find((q) => q.role === effectiveQueue) ?? null;
  const takenNames = identities.map((i) => i.label);
  const name = nameTouched ? form.name : suggestName(agent?.name ?? "", takenNames);
  const preselected =
    contextWorktrees.find((r) => r.id === request.contextWorktreeId) ?? contextWorktrees[0];
  const worktree =
    form.worktree ??
    (preselected
      ? { kind: "existing" as const, id: preselected.id }
      : { kind: "new" as const, name: "" });
  const effective: LaunchSheetForm = {
    ...form,
    agentKey,
    name,
    worktree:
      worktree.kind === "new" && !worktreeTouched
        ? {
            kind: "new",
            name: suggestName(
              name,
              contextWorktrees.map((r) => r.name),
              "-",
            ),
          }
        : worktree,
  };
  const selectedWorktree =
    effective.worktree?.kind === "existing"
      ? contextWorktrees.find(
          (r) => r.id === (effective.worktree as { kind: "existing"; id: string }).id,
        )
      : undefined;
  const worktreeName =
    effective.worktree?.kind === "new" ? effective.worktree.name : (selectedWorktree?.name ?? "");
  const worktreeTaken =
    effective.worktree?.kind === "new" &&
    contextWorktrees.some((r) => r.name.toLowerCase() === worktreeName.trim().toLowerCase());

  const manager = identities.find((i) => i.row.handle === effective.manager) ?? null;
  const handle = launchHandle(effective);
  const nameRunning =
    effective.name.trim() !== "" &&
    identities.some((i) =>
      effective.where === "story" ? i.row.handle === handle : i.row.handle.endsWith(`/${handle}`),
    );
  const errors: Partial<Record<LaunchField, string>> = {
    ...validateLaunch(effective),
    ...(effective.where === "team-workspace" &&
    (worktreesLoading || worktreesError || worktreeTaken)
      ? {
          worktree: worktreesLoading
            ? "Loading worktrees…"
            : (worktreesError ??
              `${worktreeName.trim()} already exists. Pick it from the list, or use another name.`),
        }
      : {}),
    ...(nameRunning ? { name: "One with this name is already running there. Pick another." } : {}),
  };
  const valid = branchMode
    ? agentKey !== "" && isLaunchable(branch.target)
    : Object.keys(errors).length === 0;
  const shownError = (field: LaunchField) => (showErrors ? errors[field] : undefined);

  const openStoryWorkspaces = useMemo(
    () => (workspaces ?? []).filter((ws) => ws.kind === "story" && ws.storyId),
    [workspaces],
  );

  // A story's workspace stays where it was created: one that exists holds the Machine to
  // where it lives; with none, the Machine picks where it is created.
  //
  // SW-WS-1: the one authorized way to mutate "where it lives" is the
  // `move_workspace` MCP tool, surfaced via the workspace header dialog
  // (`MoveWorkspaceDialog`). The Machine control here remains read-only on
  // existing workspaces; a user who wants to re-home opens the dialog.
  const storyId = effective.storyId.trim();
  const storyWorkspace = storyWorkspaceIn(
    {
      workspaces: workspaces ? { [request.teamKey]: workspaces } : {},
      softClosedWorkspaces: minimized ? { [request.teamKey]: minimized } : {},
    },
    request.teamKey,
    storyId,
  );
  const place = !storyWorkspace
    ? null
    : isRemoteWorkspace(storyWorkspace)
      ? storyWorkspace.location.machineId
      : LOCAL_MACHINE;
  const labelOf = (id: string) =>
    id === LOCAL_MACHINE
      ? THIS_MAC
      : ((machines ?? []).find((m) => m.machineId === id)?.label ?? id);
  const lock = launchMachineLock(effective.where, storyId, place, labelOf);
  const launchMachine = lock?.machineId ?? machineId;
  // Branch targets run locally; only an existing worktree has a cwd to inspect before launch.
  // Resolve both location facts together so hidden team/story selections cannot leak into preview.
  const activeTarget = branchMode
    ? {
        machineId: LOCAL_MACHINE,
        cwd:
          branch.target.kind === "existingWorktree"
            ? branch.target.path.trim() || undefined
            : undefined,
      }
    : {
        machineId: launchMachine,
        cwd: effective.where === "story" ? storyWorkspace?.cwd : selectedWorktree?.launchCwd,
      };
  const { warnings, admission: launchAdmission } = useLaunchPreview(
    agentKey,
    request.teamKey,
    activeTarget.machineId === LOCAL_MACHINE,
    activeTarget.cwd,
    form.task,
  );

  // The team's stories, with the open workspaces' ones first-class even before a scan lands.
  const storyChoices = useMemo(() => {
    const known = new Set(stories.map((s) => s.storyId));
    const open = openStoryWorkspaces
      .filter((ws) => ws.storyId && !known.has(ws.storyId))
      .map((ws) => ({ storyId: ws.storyId ?? "", title: ws.storyTitle ?? "" }));
    return [...open, ...stories];
  }, [stories, openStoryWorkspaces]);

  // How the new agent will look in the list: the face Auto resolves to, the role piece.
  const stamp = agentKey ? `${agentKey}@0` : null;
  const previewName = effective.name.trim() || agent?.name || "New agent";
  const autoFace = resolveAgentFace(
    { avatar: null, agentRevision: stamp, handle, displayName: previewName },
    { name: previewName, faces: avatars.faces },
  );
  const previewFace: AgentFace = effective.face
    ? resolveAgentFace(
        { avatar: effective.face, agentRevision: stamp, handle, displayName: previewName },
        { name: previewName, faces: avatars.faces },
      )
    : autoFace;
  const previewRole = resolveAgentRole({ role: effective.role ?? null }, { launched: 0 });

  // The new agent shows in the Agents pane: its team shows again if the pane hides it, and its
  // team unfolds with the card it lands in ({@link launchLandsIn}), as a pill click unfolds them.
  // Read once the roster lists it, so the card is the one the pane places it in. A story whose
  // card can't be named yet has none to unfold: its new card is drawn unfolded, as every card
  // with no saved fold is. Under a manager, the manager's card unfolds too. An agent chip naming
  // another agent, or a workspace arrow on another card, would hide it, so either one clears;
  // the search and the Show filter stay as they are, so a launch never undoes them.
  // Says whether the roster listed the launched agent, so its card was the one the pane places it in.
  const revealLaunchInPane = (launchedHandle: string | null): boolean => {
    const team = request.teamKey;
    const viewer = useViewerStore.getState();
    if (viewer.agentsPaneHiddenTeams.includes(team)) viewer.setAgentsPaneTeamHidden(team, false);
    if (viewer.agentsListFilter && viewer.agentsListFilter.agentKey !== agentKey) {
      viewer.clearAgentsListFilter();
    }
    setPaneFold(teamFoldKey(team), false);
    const projection = useAgentListProjectionStore.getState();
    const launched = launchedHandle
      ? launchedRowIn(projection.sessions, launchedHandle, team, (sid) => tabTeamIn(viewer, sid))
      : undefined;
    // Sessions arrive before their placement references; wait before changing the card/filter.
    if (
      launchedHandle &&
      (!launched ||
        !projection.references.some(
          (ref) => ref.sessionRef === (launched.id ?? launched.sessionId ?? launched.handle),
        ))
    )
      return false;
    const landsIn = launchLandsIn(effective.where, {
      placed: launched ? (paneWorkspaceIn(viewer, team, launched)?.id ?? null) : null,
      storyWorkspaceId: storyWorkspaceIn(viewer, team, storyId)?.id ?? null,
      storyWorkspaceIdToBe: storyWorkspaceIdToBe(tree, storyId),
    });
    const arrow = viewer.agentsWorkspaceFilter;
    if (arrow && !launchKeepsArrow(arrow, team, landsIn)) viewer.setAgentsWorkspaceFilter(null);
    if (landsIn !== null) setPaneFold(workspaceFoldKey(team, landsIn), false);
    if (manager) {
      const managerWs = paneWorkspaceIn(viewer, team, manager.row);
      setPaneFold(workspaceFoldKey(team, managerWs?.id ?? NO_WORKSPACE_ID), false);
    }
    return launched !== undefined;
  };

  // The read that follows a launch can be superseded by a newer one that settles first and may
  // not list the agent yet: reveal it again once the roster does, for a while.
  const revealWhenListed = (launchedHandle: string) => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    const stop = useAgentListProjectionStore.subscribe((st, prev) => {
      if (st.sessions === prev.sessions && st.references === prev.references) return;
      if (!revealLaunchInPane(launchedHandle)) return;
      stop();
      if (timer !== null) clearTimeout(timer);
    });
    timer = setTimeout(stop, LAUNCHED_ROW_WAIT_MS);
  };

  const launch = () => {
    if (launchAdmission?.allowed === false) {
      setFailure(launchAdmission.reason);
      return;
    }
    if (busy || phase === "hydrating" || phase === "starting") return;
    if (!valid) {
      setShowErrors(true);
      return;
    }
    if (branchMode) {
      setFailure(null);
      setBranchWarnings([]);
      // Lazy, so the sheet never statically pulls in the launch controller.
      const open: LaunchOpen = async (arg, hooks) =>
        (await import("@/features/dashboard/components/launch-open-real")).makeRealLaunchOpen(
          AGENTS_LAUNCHER,
        )(arg, hooks);
      void runLaunchWithProgress(
        toAgentOpenWorkspaceArg(branch.target, agentKey, form.task),
        setPhase,
        open,
      )
        .then((result) => setBranchWarnings(result.warnings ?? []))
        .catch((err: unknown) => setFailure(userFacingErrorMessage(err)));
      return;
    }
    const rememberContextRetry = (id: string) => {
      rememberAgentLaunch(
        request.teamKey,
        id,
        agentLaunchNames(agentKey, agent?.name),
        async () => {
          openLaunchSheet({
            teamKey: request.teamKey,
            where: "team-workspace",
            contextWorktreeId: id,
            agentKey,
            name: effective.name,
            role: effective.role,
            queueRole: effective.queueRole,
            managerHandle: effective.manager ?? undefined,
            task: effective.task,
            face: effective.face ?? undefined,
          });
        },
      );
    };
    if (effective.where === "team-workspace" && effective.worktree?.kind === "existing")
      rememberContextRetry(effective.worktree.id);
    setBusy(true);
    setFailure(null);
    agentLaunchOpen({
      ...toLaunchArgs(effective, launchMachine),
      ...(effective.where === "team-workspace" ? { team, org } : {}),
    })
      .then((result) => {
        if (result.status !== "opened") {
          setFailure(result.message);
          if (effective.where === "team-workspace" && result.status === "invalid")
            void reloadWorktrees();
          return;
        }
        if (result.worktree) rememberContextRetry(result.worktree.id);
        const launchedHandle = result.handle;
        void useAgentListProjectionStore
          .getState()
          .reload()
          .catch((e: unknown) => logger.debug("agents", `roster read after launch: ${String(e)}`))
          .finally(() => {
            if (!revealLaunchInPane(launchedHandle) && launchedHandle) {
              revealWhenListed(launchedHandle);
            }
          });
        const where =
          effective.where === "story"
            ? launchMachine === LOCAL_MACHINE
              ? effective.storyId.trim()
              : `${effective.storyId.trim()} on ${labelOf(launchMachine)}`
            : `${result.worktree?.created ? "new worktree " : ""}${result.worktree?.name ?? worktreeName}`;
        toast.success(`${result.name ?? previewName} is starting in ${where}`, {
          description: manager ? `Under ${manager.label}` : undefined,
        });
        for (const w of result.warnings ?? []) toast.warning(w.message);
        onClose();
      })
      .catch((err: unknown) => setFailure(userFacingErrorMessage(err)))
      .finally(() => setBusy(false));
  };

  const branchBusy = phase === "hydrating" || phase === "starting";
  // One pending condition for the whole sheet: while a launch is in flight nothing resets,
  // repeats or dismisses it.
  const pending = busy || branchBusy;
  const account = agent ? accountLine(agent.account, agent.runtime) : null;
  // A launch on another machine uses that machine's own CLI login, not this Mac's account: the
  // line names whose it is, and this Mac's account preview (and its Fix) doesn't apply.
  const remoteMachine =
    activeTarget.machineId !== LOCAL_MACHINE ? labelOf(activeTarget.machineId) : null;
  const fixAccount = () => {
    onClose();
    openAgentAccount(agentKey);
  };
  // A CLI that runs only on a configured model endpoint, and the agent names none.
  const needsEndpoint = !!agent && needsModelEndpoint(agent);
  const fixEndpoint = () => {
    onClose();
    openAgentEditSection(agentKey, "runs-on");
  };

  return (
    <Dialog
      open
      onOpenChange={(next) => {
        if (!next && !pending) onClose();
      }}
    >
      <DialogContent
        onEscapeKeyDown={(event) => {
          if (
            event.target instanceof HTMLElement &&
            event.target.dataset.testid === "launch-worktree-name"
          )
            event.preventDefault();
        }}
        data-testid="agent-launch-sheet"
        className="cs-agent-identity ls-sheet gap-0 overflow-hidden p-0 sm:max-w-[920px]"
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          taskRef.current?.focus();
        }}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && !e.nativeEvent.isComposing) {
            e.preventDefault();
            launch();
          }
        }}
      >
        <DialogHeader className="ls-head">
          {/* Who is being launched, the saved agent as the list and the profile header draw it. The
              Face picker below is what the new session wears; this is the agent behind it. */}
          <div data-testid="launch-header" className="flex items-center gap-3">
            {agent && (
              <span data-testid="launch-header-face" className="shrink-0">
                <AgentRowFace agent={agent} size={44} />
              </span>
            )}
            {/* pr-10 keeps the title clear of the Close button in the corner. */}
            <div className="min-w-0 pr-10">
              <DialogTitle className="truncate text-xl font-semibold tracking-[-0.01em]">
                {agent ? `Launch ${agent.name}` : "Launch an agent"}
              </DialogTitle>
              {agent && (
                <p className="truncate text-[13px] text-muted-foreground">
                  {agent.description || `${placeLabel(agent)} · ${runtimeLabel(agent.runtime)}`}
                </p>
              )}
              <DialogDescription className="mt-1 text-[13px]">
                {branchMode
                  ? "It opens in a workspace of its own, on the branch or worktree you choose."
                  : "It opens as a tab where you choose, under its manager, wearing the face you pick."}
              </DialogDescription>
            </div>
          </div>
        </DialogHeader>

        <div
          className="ls-body"
          style={branchMode ? { gridTemplateColumns: "minmax(0, 1fr)", height: "auto" } : undefined}
        >
          <div className="ls-scroll ls-form">
            {!branchMode && (
              <Preview
                manager={manager}
                face={previewFace}
                role={previewRole}
                name={previewName}
                task={effective.task.trim()}
                where={
                  effective.where === "story"
                    ? effective.storyId.trim() || "Pick a story"
                    : worktreeName
                }
                machine={labelOf(launchMachine)}
              />
            )}

            <div className="grid grid-cols-2 gap-3">
              <Field label="Agent" error={shownError("agentKey")}>
                <AgentPicker
                  agents={agents}
                  value={agentKey}
                  loading={rows === null}
                  onChange={(key) => set("agentKey", key)}
                />
              </Field>
              {!branchMode && (
                <Field label="Manager">
                  <ManagerPicker
                    identities={identities}
                    value={manager}
                    onChange={(h) => set("manager", h)}
                  />
                </Field>
              )}
            </div>

            {account && (
              <div
                data-testid="launch-account"
                data-ok={!needsEndpoint && (remoteMachine !== null || account.kind === "ok")}
                data-remote={remoteMachine !== null || undefined}
                className={cn(
                  "flex min-h-9 flex-wrap items-center gap-x-2 gap-y-1.5 rounded-lg border px-3 py-1 text-[13px]",
                  !needsEndpoint && (remoteMachine !== null || account.kind === "ok")
                    ? "border-border/70 bg-muted/35"
                    : "border-warning bg-warning-wash",
                )}
              >
                {needsEndpoint ? (
                  // Static text, not a choice: the endpoint is set in Edit → Runs on. The button
                  // wraps under the words when the sheet is narrow.
                  <>
                    <TriangleAlert
                      aria-hidden="true"
                      className="size-3.5 shrink-0 text-warning-fg"
                    />
                    <span className="font-medium text-warning-ink">{NEEDS_MODEL_ENDPOINT}</span>
                    <Button
                      type="button"
                      size="xs"
                      variant="outline"
                      className="ml-auto"
                      disabled={pending}
                      onClick={fixEndpoint}
                    >
                      <Wrench />
                      Fix in Edit → Runs on
                    </Button>
                  </>
                ) : remoteMachine !== null ? (
                  <>
                    <KeyRound aria-hidden="true" className="size-3.5 text-muted-foreground" />
                    <span>
                      Uses the {agent && runtimeLabel(agent.runtime)} login on{" "}
                      <b className="font-semibold">{remoteMachine}</b>
                    </span>
                    <span className="text-muted-foreground">· {agent && modelWord(agent)}</span>
                  </>
                ) : agent?.account.provider === null ? (
                  <>
                    <KeyRound aria-hidden="true" className="size-3.5 text-muted-foreground" />
                    <span>No CodeSense login</span>
                    <span className="text-muted-foreground">
                      · Model settings are in Edit → Runs on
                    </span>
                  </>
                ) : account.kind === "ok" ? (
                  <>
                    <KeyRound aria-hidden="true" className="size-3.5 text-muted-foreground" />
                    <span>
                      Charges{" "}
                      <b className="font-semibold">
                        {agent && shortAccount(agent.account, agent.runtime).replace(/^on /, "")}
                      </b>
                    </span>
                    {agent?.account.source === "binding" && <Badge tone="quiet">pinned</Badge>}
                    <span className="text-muted-foreground">
                      · {agent && runtimeLabel(agent.runtime)} · {agent && modelWord(agent)}
                      {branchMode ? "" : ` · ${labelOf(launchMachine)}`}
                    </span>
                    <span className="ml-auto text-xs text-muted-foreground">
                      Change it on Overview
                    </span>
                  </>
                ) : (
                  <>
                    <TriangleAlert
                      aria-hidden="true"
                      className="size-3.5 shrink-0 text-warning-fg"
                    />
                    <span className="font-medium text-warning-ink">{account.text}</span>
                    <Button
                      type="button"
                      size="xs"
                      variant="outline"
                      className="ml-auto"
                      aria-label={`Fix the account for ${agent?.name ?? "this agent"}`}
                      disabled={pending}
                      onClick={fixAccount}
                    >
                      Fix
                    </Button>
                  </>
                )}
              </div>
            )}

            {warnings.length > 0 && (
              <ul
                className="ls-warnings"
                data-testid="launch-warnings"
                aria-label="Launch warnings"
              >
                {warnings.map((w) => (
                  <li key={w.code} className="ls-warning" data-code={w.code}>
                    <TriangleAlert className="mt-px size-3.5 shrink-0" aria-hidden />
                    <span className="min-w-0">{w.message}</span>
                  </li>
                ))}
              </ul>
            )}

            <Field label="First prompt" htmlFor="ls-task" hint={FIRST_MESSAGE_HINT}>
              <textarea
                ref={taskRef}
                id="ls-task"
                aria-describedby="ls-task-hint"
                data-testid="launch-task"
                value={form.task}
                rows={4}
                placeholder={taskPlaceholder(agent)}
                onChange={(e) => set("task", e.target.value)}
                className="ls-input ls-textarea"
              />
            </Field>

            <div className="grid grid-cols-[1fr_auto] items-start gap-3">
              <Field label="Where">
                <Segmented
                  aria-label="Where it works"
                  value={branchMode ? "branch" : effective.where}
                  onValueChange={(id) => {
                    setBranchMode(id === "branch");
                    setPhase("idle");
                    setFailure(null);
                    if (id !== "branch") set("where", id as LaunchWhere);
                  }}
                  options={(
                    [
                      { id: "story", label: "Story workspace", Icon: GitBranch },
                      { id: "team-workspace", label: "Team folder", Icon: FolderOpen },
                      { id: "branch", label: "Branch or worktree", Icon: GitFork },
                    ] as const
                  ).map((opt) => ({
                    value: opt.id,
                    testId: `launch-where-${opt.id}`,
                    disabled: pending,
                    label: (
                      <>
                        <opt.Icon className="size-3.5" aria-hidden />
                        {opt.label}
                      </>
                    ),
                  }))}
                />
              </Field>
              {!branchMode && (
                <Field label="Machine">
                  <MachinePicker
                    machines={machines}
                    value={launchMachine}
                    lock={lock}
                    onChange={setMachineId}
                  />
                </Field>
              )}
            </div>

            {branchMode ? (
              <div className="space-y-2">
                <LaunchTargetPicker
                  kinds={["repoBranch", "newWorktree", "existingWorktree"]}
                  kind={branch.kind}
                  fields={branch.fields}
                  onKindChange={(kind) => {
                    branch.setKind(kind);
                    setPhase("idle");
                    setFailure(null);
                    setBranchWarnings([]);
                  }}
                  onFieldChange={branch.setField}
                  disabled={branchBusy}
                  newBranchBase={newBranchBase}
                />
                {showErrors && !isLaunchable(branch.target) && (
                  <p className="ls-field-error" role="alert">
                    Say which repo and branch, or which worktree, it should work in.
                  </p>
                )}
                <p className="ls-note">
                  A branch or worktree launch opens the agent in a workspace of its own, with the
                  agent&apos;s own name, face and queue role.
                </p>
              </div>
            ) : effective.where === "story" ? (
              <div className="space-y-2">
                <StoryInput
                  value={form.storyId}
                  stories={storyChoices}
                  loading={storiesLoading}
                  loadError={storiesError}
                  error={shownError("storyId")}
                  onChange={(v) => set("storyId", v)}
                />
                {openStoryWorkspaces.length > 0 && (
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="text-[12px] text-muted-foreground">Open now</span>
                    {openStoryWorkspaces.map((ws) => (
                      <button
                        key={ws.id}
                        type="button"
                        className="ls-chip"
                        aria-pressed={form.storyId.trim() === ws.storyId}
                        title={ws.storyTitle ?? ws.storyId}
                        onClick={() => set("storyId", ws.storyId ?? "")}
                      >
                        <span className="font-medium">{ws.storyId}</span>
                        {ws.storyTitle && (
                          <span className="max-w-40 truncate text-muted-foreground">
                            {ws.storyTitle}
                          </span>
                        )}
                      </button>
                    ))}
                  </div>
                )}
                <p className="ls-note" data-testid="launch-machine-note">
                  {lock
                    ? `Opens as a new tab in the story's workspace, on its worktree. ${lock.reason}`
                    : launchMachine === LOCAL_MACHINE
                      ? "Opens as a new tab in the story's workspace, on its worktree."
                      : `Creates the story's workspace on ${labelOf(launchMachine)} (its worktree is copied there), then opens it there as a tab.`}
                </p>
              </div>
            ) : (
              <div className="space-y-2">
                <ContextWorktreePicker
                  rows={contextWorktrees}
                  value={effective.worktree!}
                  loading={worktreesLoading}
                  error={shownError("worktree")}
                  onChange={(value, edited) => {
                    if (value.kind === "new") setWorktreeTouched(Boolean(edited));
                    set("worktree", value);
                  }}
                />
                <p className="ls-note">
                  {request.contextWorktreeId &&
                  !contextWorktrees.some((r) => r.id === request.contextWorktreeId) &&
                  !worktreesLoading &&
                  preselected
                    ? `The previous worktree no longer exists, so ${preselected.name}, the most recent, is selected. `
                    : ""}
                  {selectedWorktree
                    ? `Opens as a new tab in ${selectedWorktree.name}, in ${team}/content on branch ${selectedWorktree.branch}. ${selectedWorktree.agents.length} agents already work there.`
                    : `Creates branch ${team}/${launchHandle({ where: "team-workspace", storyId: "", name: worktreeName })} from your clone's main and opens in ${team}/content. Edits stay on that branch until an agent commits or syncs them.`}
                </p>
              </div>
            )}

            {!branchMode && (
              <>
                <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3">
                  <Field label="Name" htmlFor="ls-name" error={shownError("name")}>
                    <input
                      id="ls-name"
                      data-testid="launch-name"
                      value={name}
                      maxLength={80}
                      aria-invalid={Boolean(shownError("name")) || undefined}
                      onChange={(e) => {
                        setNameTouched(true);
                        set("name", e.target.value);
                      }}
                      className="ls-input"
                    />
                  </Field>
                  <Field label="Queue role">
                    <div role="radiogroup" aria-label="Queue role" className="ls-seg">
                      <button
                        type="button"
                        role="radio"
                        aria-checked={form.queueRole === undefined}
                        onClick={() => set("queueRole", undefined)}
                      >
                        {agent?.role && (
                          <QueueRole role={agent.role} teamKey={request.teamKey} iconOnly />
                        )}
                        Saved default (
                        {agent?.role
                          ? (queueRoles.find((r) => r.id === agent.role)?.name ?? agent.role)
                          : "none"}
                        )
                      </button>
                      {(agent?.role || form.queueRole === null) && (
                        <button
                          type="button"
                          role="radio"
                          aria-checked={form.queueRole === null}
                          onClick={() => set("queueRole", null)}
                        >
                          No role
                        </button>
                      )}
                      <PortaledSelect
                        ariaLabel="Other queue role"
                        triggerClassName="h-7 w-auto min-w-24 text-[13px]"
                        value={form.queueRole ?? ""}
                        onValueChange={(v) => {
                          if (v) set("queueRole", v);
                        }}
                        options={[
                          { value: "", label: "Other…" },
                          ...queueRoles.map((r) => ({ value: r.id, label: r.name })),
                        ]}
                      />
                    </div>
                  </Field>
                </div>
                <p className="ls-note">
                  {effectiveQueue ? (
                    <>
                      <QueueRole role={effectiveQueue} teamKey={request.teamKey} /> · Joins this
                      shared queue
                      {queueState
                        ? ` with ${queueState.registrations.filter((r) => r.live).length} other holders; ${queueState.counts.pending} messages are waiting`
                        : " (queue counts unavailable)"}
                      {form.queueRole !== undefined
                        ? ` for this launch only. ${agent?.name ?? "This agent"} still defaults to ${agent?.role ? (queueRoles.find((r) => r.id === agent.role)?.name ?? agent.role) : "no role"}`
                        : ""}
                      .
                    </>
                  ) : (
                    "Joins no queue."
                  )}
                </p>
              </>
            )}
            {branchMode && branchWarnings.length > 0 && (
              <ul data-testid="launch-run-warnings" className="space-y-1">
                {branchWarnings.map((w) => (
                  <li
                    key={`${w.code}:${w.message}`}
                    className="flex items-start gap-1.5 rounded-md bg-warning-wash px-2 py-1 text-xs text-warning-ink"
                  >
                    <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
                    <span>{w.message}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>

          {!branchMode && (
            <aside className="ls-faces" aria-label="Face">
              <div className="flex items-baseline justify-between px-4 pt-4 pb-2">
                <span className="ls-label">Face</span>
                <span className="text-[11px] text-muted-foreground">
                  {form.face ? "Picked" : "Auto"}
                </span>
              </div>
              <FacePicker
                packs={avatars.packs}
                value={form.face}
                autoFace={autoFace}
                onChange={(f) => set("face", f)}
              />
            </aside>
          )}
        </div>

        <DialogFooter className="ls-foot">
          <div className="mr-auto flex min-w-0 items-center gap-2" aria-live="polite">
            {failure || launchAdmission?.reason ? (
              <p data-testid="launch-error" className="ls-error">
                <CircleAlert className="size-3.5 shrink-0" aria-hidden />
                <span className="min-w-0">{failure ?? launchAdmission?.reason}</span>
              </p>
            ) : branchMode && phase !== "idle" ? (
              <LaunchProgress phase={phase} />
            ) : showErrors && !valid ? (
              <p className="ls-error">
                <CircleAlert className="size-3.5 shrink-0" aria-hidden />
                Fix the highlighted fields to launch.
              </p>
            ) : (
              <span className="text-[12px] text-muted-foreground">
                <kbd className="ls-kbd">⌘</kbd>
                <kbd className="ls-kbd">↵</kbd> to launch
              </span>
            )}
          </div>
          <Button type="button" variant="outline" size="sm" disabled={pending} onClick={onClose}>
            Cancel
          </Button>
          <Button
            type="button"
            size="sm"
            data-testid="launch-submit"
            disabled={pending || launchAdmission?.allowed === false}
            onClick={launch}
          >
            <Rocket aria-hidden />
            {busy || branchBusy
              ? "Launching…"
              : branchMode && phase === "running"
                ? "Launch again"
                : `Launch ${previewName}`}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function Field({
  label,
  htmlFor,
  hint,
  error,
  children,
}: {
  label: string;
  htmlFor?: string;
  /** A line under the field saying what it is for; `${htmlFor}-hint` describes it. */
  hint?: string;
  error?: string;
  children: React.ReactNode;
}): React.JSX.Element {
  return (
    <div className="min-w-0 space-y-1.5">
      {htmlFor ? (
        <label htmlFor={htmlFor} className="ls-label">
          {label}
        </label>
      ) : (
        <span className="ls-label">{label}</span>
      )}
      {children}
      {hint && (
        <p id={htmlFor ? `${htmlFor}-hint` : undefined} className="ls-note">
          {hint}
        </p>
      )}
      {error && (
        <p id={htmlFor ? `${htmlFor}-error` : undefined} className="ls-field-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** Where the agent will sit in the agents list, drawn the way the list draws it. */
function Preview({
  manager,
  face,
  role,
  name,
  task,
  where,
  machine,
}: {
  manager: TeamAgentIdentity | null;
  face: AgentFace;
  role: ReturnType<typeof resolveAgentRole>;
  name: string;
  task: string;
  where: string;
  machine: string;
}): React.JSX.Element {
  return (
    <div className="ls-preview" data-testid="launch-preview" aria-label="How it shows in Agents">
      {manager && (
        <div className="ls-preview-parent">
          <AgentAvatar face={manager.face} role={manager.role} pieceTone={null} size={18} />
          <span className="truncate">{manager.label}</span>
        </div>
      )}
      <div className={cn("ls-preview-row", manager && "ls-preview-child")}>
        <AgentAvatar
          face={face}
          role={role}
          pieceTone={null}
          size={30}
          className="ls-preview-face"
        />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-[13px] leading-[18px]">
            <span className="font-semibold">{name}</span>
            {task && <span className="text-muted-foreground"> · {task}</span>}
          </span>
          <span className="block truncate text-[12px] leading-4 text-muted-foreground">
            {[role.label, where, machine].join(" · ")}
          </span>
        </span>
      </div>
    </div>
  );
}

const TRIGGER = "ls-input ls-trigger";

function AgentPicker({
  agents,
  value,
  loading,
  onChange,
}: {
  agents: readonly AgentRow[];
  value: string;
  loading: boolean;
  onChange: (key: string) => void;
}): React.JSX.Element {
  const picked = agents.find((a) => a.key === value);
  const noteId = useId();
  const { mine, teams, builtin } = groupAgents(agents);
  // One-off launches still reach the templates, grouped at the bottom under "Templates".
  // A saved agent (Mine / Team) is always what you'd pick first.
  const groups = [
    { id: "mine", label: "Mine", rows: mine },
    ...teams.map((t) => ({ id: t.id, label: t.label, rows: t.rows })),
    { id: "builtin", label: "Templates", rows: builtin },
  ].filter((g) => g.rows.length > 0);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className={TRIGGER}
          data-testid="launch-agent"
          aria-label="Agent"
          aria-describedby={previewNoteRef(noteId, picked?.runtime)}
        >
          <span className="min-w-0 flex-1 truncate text-left">
            {picked ? picked.name : loading ? "Loading agents…" : "Pick an agent"}
          </span>
          {picked && runtimeLabel(picked.runtime) !== picked.name && (
            <span className="shrink-0 text-[11px] text-muted-foreground">
              {runtimeLabel(picked.runtime)}
            </span>
          )}
          <PreviewBadge cli={picked?.runtime} />
          <ChevronDown className="size-3.5 shrink-0 opacity-60" aria-hidden />
        </button>
      </DropdownMenuTrigger>
      <span id={noteId} hidden>
        {PREVIEW_RUNTIME_NOTE}
      </span>
      <DropdownMenuContent align="start" className="max-h-80 w-80 overflow-y-auto">
        {groups.map((g, i) => (
          <div key={g.id} role="group" aria-label={g.label}>
            {i > 0 && <DropdownMenuSeparator />}
            <div className="px-2 pt-1.5 pb-1 text-[11px] font-medium text-muted-foreground">
              {g.label}
            </div>
            {g.rows.map((a) => (
              <DropdownMenuItem
                key={a.key}
                className="items-start gap-2 py-1.5"
                aria-describedby={previewNoteRef(noteId, a.runtime)}
                onSelect={() => onChange(a.key)}
              >
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="text-[13px] font-medium">{a.name}</span>
                  {needsModelEndpoint(a) ? (
                    // Still selectable: the sheet then says why it can't launch, with its Fix.
                    <span className="line-clamp-1 text-[11px] font-medium text-warning-ink">
                      {NEEDS_MODEL_ENDPOINT}
                    </span>
                  ) : (
                    a.description && (
                      <span className="line-clamp-1 text-[11px] text-muted-foreground">
                        {a.description}
                      </span>
                    )
                  )}
                </span>
                <span className="mt-px flex shrink-0 items-center gap-1.5 text-[11px] text-muted-foreground">
                  {runtimeLabel(a.runtime)}
                  <PreviewBadge cli={a.runtime} className="h-4 px-[5px] text-[10.5px]" />
                </span>
                <Check
                  className={cn("mt-0.5 size-3.5 shrink-0", a.key !== value && "invisible")}
                  aria-hidden
                />
              </DropdownMenuItem>
            ))}
          </div>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function ManagerPicker({
  identities,
  value,
  onChange,
}: {
  identities: readonly TeamAgentIdentity[];
  value: TeamAgentIdentity | null;
  onChange: (handle: string | null) => void;
}): React.JSX.Element {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button type="button" className={TRIGGER} data-testid="launch-manager" aria-label="Manager">
          {value ? (
            <>
              <AgentAvatar face={value.face} role={value.role} pieceTone={null} size={18} />
              <span className="min-w-0 flex-1 truncate text-left">{value.label}</span>
            </>
          ) : (
            <>
              <span className="ls-none-face" aria-hidden>
                <Minus className="size-3" />
              </span>
              <span className="min-w-0 flex-1 truncate text-left text-muted-foreground">
                No manager
              </span>
            </>
          )}
          <ChevronDown className="size-3.5 shrink-0 opacity-60" aria-hidden />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="max-h-80 w-72 overflow-y-auto">
        <DropdownMenuItem className="gap-2" onSelect={() => onChange(null)}>
          <span className="ls-none-face" aria-hidden>
            <Minus className="size-3" />
          </span>
          <span className="flex-1">
            No manager
            <span className="ml-1 text-[11px] text-muted-foreground">· top level</span>
          </span>
          <Check className={cn("size-3.5", value && "invisible")} aria-hidden />
        </DropdownMenuItem>
        {identities.length > 0 && <DropdownMenuSeparator />}
        {identities.map((i) => (
          <DropdownMenuItem
            key={i.row.handle}
            className="gap-2"
            style={{ paddingLeft: 8 + Math.min(i.depth, 4) * 14 }}
            onSelect={() => onChange(i.row.handle)}
          >
            <AgentAvatar face={i.face} role={i.role} pieceTone={null} size={20} tag={i.tag} />
            <span className="min-w-0 flex-1 truncate">
              {i.label}
              <span className="ml-1 text-[11px] text-muted-foreground">
                · {i.role.label.split(" · ")[0]}
              </span>
            </span>
            <Check
              className={cn("size-3.5 shrink-0", value?.row.handle !== i.row.handle && "invisible")}
              aria-hidden
            />
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** This Mac and the team's machines: where a story's new workspace is created and its agent
 *  runs. A machine that can't take one now is listed disabled, with why. Held (`lock`) to
 *  where the story's workspace already lives, or to This Mac for a team folder. */
function MachinePicker({
  machines,
  value,
  lock,
  onChange,
}: {
  machines: readonly MachineSummary[] | null;
  value: string;
  lock: MachineLock | null;
  onChange: (id: string) => void;
}): React.JSX.Element {
  const visible = (machines ?? []).filter((m) => m.kind === "local" || !m.binding?.hidden);
  const picked = visible.find((m) => m.machineId === value);
  const local = visible.find((m) => m.kind === "local");
  const now = useNow(true, 30_000);
  const remote = workspaceMachineChoices(machines, now);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild disabled={lock !== null}>
        <button
          type="button"
          className={cn(TRIGGER, "w-40")}
          data-testid="launch-machine"
          aria-label="Machine"
          title={lock?.reason}
        >
          {picked ? <MachineIcon machine={picked} /> : null}
          <span className="min-w-0 flex-1 truncate text-left">
            {picked?.label ?? (value === LOCAL_MACHINE ? THIS_MAC : value)}
          </span>
          <ChevronDown className="size-3.5 shrink-0 opacity-60" aria-hidden />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" side="bottom" className="max-h-80 w-72 overflow-y-auto">
        <DropdownMenuItem
          className="gap-2"
          data-testid={`launch-machine-${LOCAL_MACHINE}`}
          onSelect={() => onChange(LOCAL_MACHINE)}
        >
          {local ? <MachineIcon machine={local} /> : null}
          <span className="flex-1">{local?.label ?? THIS_MAC}</span>
          <Check className={cn("size-3.5", value !== LOCAL_MACHINE && "invisible")} aria-hidden />
        </DropdownMenuItem>
        {remote.length > 0 && (
          <>
            <DropdownMenuSeparator />
            {remote.map((choice) => {
              const m = visible.find((v) => v.machineId === choice.machineId);
              return (
                <DropdownMenuItem
                  key={choice.machineId}
                  disabled={choice.blocked !== null}
                  title={choice.blocked ?? undefined}
                  className="gap-2"
                  data-testid={`launch-machine-${choice.machineId}`}
                  onSelect={() => onChange(choice.machineId)}
                >
                  {m ? <MachineIcon machine={m} /> : null}
                  <span className="min-w-0 flex-1 truncate">{choice.label}</span>
                  {choice.note && (
                    <span className="shrink-0 text-[11px] text-muted-foreground">
                      {choice.note}
                    </span>
                  )}
                  <Check
                    className={cn("size-3.5", value !== choice.machineId && "invisible")}
                    aria-hidden
                  />
                </DropdownMenuItem>
              );
            })}
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** The story box: type an id, or pick one of the team's stories as you type. */
export function StoryInput({
  value,
  stories,
  loading = false,
  loadError,
  error,
  onChange,
}: {
  value: string;
  stories: readonly { storyId: string; title: string }[];
  loading?: boolean;
  loadError?: string;
  error?: string;
  onChange: (storyId: string) => void;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const q = value.trim().toLowerCase();
  const matches = stories
    .map((story) => ({ story, score: fuzzyIdTitleScore(q, story.storyId, story.title) }))
    .filter((item) => item.score > 0)
    .sort((a, b) => b.score - a.score)
    .slice(0, 8)
    .map((item) => item.story);
  const active = selectedIndexById(matches, selectedId, (story) => story.storyId);
  const show = open && (loading || !!loadError || matches.length > 0);
  const pick = (id: string) => {
    onChange(id);
    setOpen(false);
  };
  const title = stories.find((s) => s.storyId.toLowerCase() === q)?.title;
  return (
    <div className="space-y-1.5">
      <Popover open={show} onOpenChange={setOpen}>
        <PopoverAnchor asChild>
          <label className={cn("ls-input ls-trigger cursor-text", error && "ls-invalid")}>
            <Search className="size-3.5 shrink-0 opacity-60" aria-hidden />
            <input
              data-testid="launch-story"
              aria-label="Story"
              role="combobox"
              aria-expanded={show}
              aria-invalid={Boolean(error) || undefined}
              value={value}
              placeholder="Story id or title, e.g. BUG-142"
              className="min-w-0 flex-1 bg-transparent outline-none"
              onFocus={() => setOpen(true)}
              onBlur={(event) =>
                commitTextEditOnBlur(event.currentTarget, () => {
                  setTimeout(() => setOpen(false), 120);
                })
              }
              onChange={(e) => {
                onChange(e.target.value);
                setSelectedId(null);
                setOpen(true);
              }}
              onKeyDown={(e) => {
                if (!show) return;
                if (e.key === "Escape") {
                  e.stopPropagation();
                  setOpen(false);
                } else if (loading || loadError || !matches.length) return;
                else if (e.key === "ArrowDown")
                  setSelectedId(matches[Math.min(matches.length - 1, active + 1)].storyId);
                else if (e.key === "ArrowUp")
                  setSelectedId(matches[Math.max(0, active - 1)].storyId);
                else if (e.key === "Enter" && !e.metaKey && !e.ctrlKey)
                  pick(matches[active].storyId);
                else return;
                e.preventDefault();
              }}
            />
            {title && (
              <span className="max-w-[45%] truncate text-[12px] text-muted-foreground">
                {title}
              </span>
            )}
          </label>
        </PopoverAnchor>
        <PopoverContent
          align="start"
          className="w-(--radix-popover-trigger-width) gap-0 p-1"
          onOpenAutoFocus={(e) => e.preventDefault()}
        >
          {loading ? (
            <p role="status" className="px-2 py-1.5 text-[13px]">
              Loading stories…
            </p>
          ) : loadError ? (
            <p role="alert" className="px-2 py-1.5 text-[13px]">
              Couldn’t load stories. {loadError}
            </p>
          ) : (
            <ul role="listbox" aria-label="Stories">
              {matches.map((s, i) => (
                <li key={s.storyId} role="option" aria-selected={i === active}>
                  <button
                    type="button"
                    tabIndex={-1}
                    className={cn(
                      "flex w-full items-baseline gap-2 rounded-md px-2 py-1.5 text-left text-[13px]",
                      i === active ? "bg-muted" : "hover:bg-muted/60",
                    )}
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => pick(s.storyId)}
                  >
                    <span className="shrink-0 font-medium">{s.storyId}</span>
                    <span className="truncate text-muted-foreground">{s.title}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </PopoverContent>
      </Popover>
      {error && (
        <p className="ls-field-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function ContextWorktreePicker({
  rows,
  value,
  loading,
  error,
  onChange,
}: {
  rows: ContextWorktreeInfo[];
  value: NonNullable<LaunchSheetForm["worktree"]>;
  loading: boolean;
  error?: string;
  onChange: (value: NonNullable<LaunchSheetForm["worktree"]>, edited?: boolean) => void;
}): React.JSX.Element {
  const trigger = useRef<HTMLButtonElement>(null);
  const [query, setQuery] = useState("");
  const [menuRows, setMenuRows] = useState<ContextWorktreeInfo[] | null>(null);
  const listed = menuRows ?? rows;
  const [previous, setPrevious] = useState<string | null>(
    value.kind === "existing" ? value.id : null,
  );
  const cancel = () => {
    if (previous || rows[0]) onChange({ kind: "existing", id: previous ?? rows[0].id });
    requestAnimationFrame(() => trigger.current?.focus());
  };
  const selected = value.kind === "existing" ? rows.find((r) => r.id === value.id) : undefined;
  return (
    <Field label="Worktree" htmlFor="ls-worktree" error={error}>
      {value.kind === "new" ? (
        <div className="flex gap-2">
          <input
            id="ls-worktree"
            data-testid="launch-worktree-name"
            autoFocus={previous !== null}
            value={value.name}
            disabled={loading}
            className="ls-input"
            aria-invalid={Boolean(error) || undefined}
            aria-describedby={error ? "ls-worktree-error" : undefined}
            onChange={(e) => onChange({ kind: "new", name: e.target.value }, true)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                cancel();
              }
            }}
          />
          <Button variant="ghost" onClick={cancel} disabled={!rows.length}>
            Cancel
          </Button>
        </div>
      ) : (
        <DropdownMenu onOpenChange={(open) => setMenuRows(open ? rows : null)}>
          <DropdownMenuTrigger asChild>
            <button
              ref={trigger}
              type="button"
              className="ls-input ls-trigger"
              data-testid="launch-worktree"
              disabled={loading}
            >
              <GitBranch className="size-3.5" aria-hidden />
              <span className="flex-1 text-left">{selected?.name ?? "Pick a worktree"}</span>
              <span className="text-muted-foreground">
                {selected?.agents.length || "no"} agents
              </span>
              <ChevronDown className="size-3" aria-hidden />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent className="max-h-80 min-w-72 overflow-y-auto">
            {listed.length > 8 && (
              <input
                aria-label="Find a worktree"
                className="ls-input"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => e.stopPropagation()}
              />
            )}
            {listed
              .filter((r) => r.name.toLowerCase().includes(query.toLowerCase()))
              .map((r) => (
                <DropdownMenuItem
                  key={r.id}
                  onSelect={() => {
                    setPrevious(r.id);
                    onChange({ kind: "existing", id: r.id });
                  }}
                >
                  <div className="min-w-0 flex-1">
                    <span className="block truncate">{r.name}</span>
                    <span className="text-xs text-muted-foreground">
                      {r.branch} · {r.agents.length || "no"} agents ·{" "}
                      {new Date(r.lastUsedAt).toLocaleDateString()}
                    </span>
                  </div>
                  {r.id === selected?.id && <Check className="size-3" />}
                </DropdownMenuItem>
              ))}
            <DropdownMenuSeparator />
            <DropdownMenuItem
              onSelect={() => {
                setPrevious(selected?.id ?? null);
                onChange({ kind: "new", name: "" });
              }}
            >
              New worktree…
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </Field>
  );
}
