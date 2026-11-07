// SPDX-License-Identifier: MIT
import { DEFAULT_APP_THEME, type AppTheme } from "@/themes";
import { warmScopes } from "@/shared/lib/warm-scopes";
import { migrateThemeSettings } from "@/themes/settings";
import { hasLiveFrames, retireAfterFlush } from "@/features/dashboard/lib/frame-lifecycle";
import {
  customRailKey,
  customGroupId,
  sanitizeWorkspaceGroups,
  validateGroupEdit,
  type WorkspaceGroup,
  type GroupEdit,
  type GroupResult,
  type GroupSnapshot,
} from "@/features/viewer/lib/workspace-groups";
import { agentListPushFields } from "@/features/viewer/lib/agent-list-inputs";
import type { VirtualItem } from "@tanstack/react-virtual";
import {
  beginContextNavigation,
  currentContextNavigationRevision,
} from "@/shared/lib/context-navigation";
import { serializeAppStateWrite } from "@/shared/lib/app-state-write";
import { fieldPatch, applyFieldPatch } from "@/shared/lib/field-patch";
import { readPrefsState, usePrefsStore } from "@/features/dashboard/lib/prefs-client";
import type { TeamFocusRestore } from "@/features/viewer/lib/agents-pane-team-focus";
import { DEFAULT_NAV_LENS, normalizeNavLens, type NavLensState } from "@/shared/lib/nav-lens";
import { useMemo } from "react";
import { create } from "zustand";
import type { PaneType } from "@/shared/lib/focus-utils";
import {
  removalDiscardMessage,
  uncommittedCodeDiscardMessage,
} from "@/shared/lib/worktree-unpublished";
import type { TerminalAiStatus, TerminalWireStatus } from "@/shared/lib/terminal-ai-status";
import {
  applyIdentityPatch,
  type AgentIdentityChoice,
  type AgentIdentityPatch,
} from "@/shared/lib/agent-identity-choice";
import { agentPinId, sanitizeAgentPinMap, type AgentPin } from "@/shared/lib/agent-pins";
import { clearTrayMove } from "@/shared/lib/tray-move-announcement";
import {
  pruneAgentSessions,
  retireAgentSession,
  snapshotAgentSession,
} from "@/shared/stores/agent-state-store";
import { closeAgentInboxRail } from "@/features/viewer/stores/agent-inbox-rail-store";
import {
  useTeamSurfaceStore,
  publishTeamTree,
  clearTeamTree,
  renameTeamTree,
} from "@/features/viewer/lib/team-surface-store";
import {
  getKindCapabilities,
  WORKSPACE_KIND_CAPABILITIES,
  type WorkspaceKind,
} from "@/shared/lib/workspace-kind-capabilities";
import {
  saveAppState,
  listNotes,
  readNote,
  createNote as createNoteCmd,
  writeNote,
  updateNoteMeta,
  deleteNote as deleteNoteCmd,
  reapEmptyScratchNotes,
  type NoteMeta,
  type NoteActor,
  pushViewerState,
  workspaceGroupEdit,
  checkPathExists,
  dashboardRegisterSession,
  LOCAL_MACHINE_ID,
  type ResolveAgentAccountInput,
  type SubjectLifecycle,
} from "@/tauri/commands";
import { logger } from "@/shared/lib/logger";
import {
  NO_WORKSPACE_ID,
  parseWorkspaceKey,
  teamFoldKey,
  teamOfFoldKey,
  workspaceFoldKey,
  managerFoldKey,
  workspaceKey,
} from "@/shared/lib/workspace-key";
import { scrollbackLines } from "@/shared/terminal/terminal-constants";
import {
  holdForTerminalServer,
  isTerminalServerRefusal,
} from "@/features/shell/lib/terminal-server";
import { acceptsPrivateAuthority } from "@/shared/lib/private-terminal-authority";
import {
  applyLayoutInput,
  EMPTY_TERMINAL_LAYOUT,
  memberOfSession,
  type LayoutApplyOutcome,
  type LayoutInput,
  type ProjectedWorkspace,
  type TerminalLayoutProjection,
  USER_HIDDEN_TAG,
  heldRemotely,
  holdersOfSession,
  sessionTarget,
  type ViewerGrid,
} from "@/shared/lib/terminal-layout-projection";
import {
  forgetFreshOpen,
  noteFreshOpen,
  terminalSpawnSize,
  type TerminalSize,
} from "@/shared/lib/terminal-open";
import {
  terminalCloseTab as closeTabIn,
  terminalMoveTab as moveTabIn,
  terminalOpenTab,
  terminalRespawnTab,
  terminalPlaceTab as placeTabIn,
  terminalRenameTab as renameTabIn,
  terminalSetActive as setActiveIn,
  terminalSetGrid as setGridIn,
  terminalSetTags as setTagsIn,
  type RuntimeSessionTarget,
  type ScopedExecution,
  type TerminalCloseMode,
  type TerminalGridPreset,
  type TerminalGridState,
  type TerminalPlacement,
  type TerminalRespawnScope,
  type TerminalTabSpawn,
  type TerminalWorkspaceDescriptor,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-layout-source";
import {
  currentNotesScope as notesScope,
  notesHaveProjection,
  notesScopeKey,
  type NotesScope,
} from "@/shared/lib/notes-scope";
import { summarize } from "@/features/notes/lib/note-summary";
import type { Phase } from "@/shared/lib/roles";
import { startOpenTimer } from "@/shared/lib/open-path-timing";
import { createDebouncedAction } from "@/shared/lib/debounced-action";
import { fireWorkspaceOpened, fireWorkspaceClosed } from "@/shared/lib/workspace-watch-hooks";
import { VIEWER_PERF } from "@/shared/lib/viewer-perf-config";
import { fileViewKey, pruneViewStateForTeam, type ViewSnapshot } from "@/shared/lib/view-snapshot";
import {
  VIEWER_SCHEMA_VERSION,
  runViewerSchemaMigrations,
  sanitizeAgentContextSelections,
} from "./viewer-schema-migrations";
import type { AgentContextSelection } from "@/features/agent-context/lib/agent-context-model";
import type { RepoState } from "./config-store";
import { useConfigStore } from "./config-store";
import { useOrgStore } from "./org-store";
import {
  DEFAULT_TERMINAL_SPLIT_RATIO,
  applyCliChromePatch,
  sanitizeCliChrome,
  seedCliChrome,
  type CliChrome,
  type LayoutMode,
} from "./cli-chrome";
import { rewriteLegacyOrgSlug } from "@/shared/lib/migrate-org-slug-keys";
import {
  buildTeamKey,
  getActiveTeamKey,
  parseTeamKey,
  NO_TEAM_SENTINEL,
  teamKeyHasTeam,
  firstRootIdForTeam,
  adjacentWorkspaceIdAfterClose,
  activeWorkspaceIdForTeam,
  sortWorkspacesByCanonicalOrder,
} from "@/shared/lib/team-key";
import {
  buildSyntheticRepoPathFromPrompt,
  parseSyntheticRepoKey,
  workspaceReposForTeam,
} from "@/shared/lib/build-synthetic-repo";
import { resolveContentSource } from "@/shared/lib/resolve-content-source";
import { resolveFileSource } from "@/shared/lib/file-source";
import {
  resolveTeamContentContext,
  teamContentNavigationPath,
} from "@/shared/lib/team-content-context";
import { scanTeamPipelineTree } from "@/shared/lib/scan-team-pipeline-tree";
import { storyFilePathFromTree } from "@/shared/lib/pipeline-graph/resolve-story-file-path";
import {
  resolveSourceRepoPath,
  resolveRealRepoPath,
  resolveOrgRoot,
  resolveOrgRootForTeam,
} from "@/shared/lib/resolve-source-repo-path";
import { normalizeScopePath } from "@/shared/lib/pipeline-graph/normalize";
import { computePipelineScopeUpdate } from "@/shared/lib/pipeline-graph/compute-pipeline-scope-update";
import { isPipelineFileName } from "@/shared/lib/pipeline-graph/pipeline-file-filter";
import { stripViewerExtension } from "@/shared/lib/viewer-file-type";
import {
  type EditorLineWrap,
  DEFAULT_EDITOR_LINE_WRAP,
} from "@/shared/components/code-editor/line-wrap";
import {
  type EditorFolding,
  type EditorTabSize,
  DEFAULT_EDITOR_FOLDING,
  DEFAULT_EDITOR_TAB_SIZE,
  coerceTabSize,
} from "@/shared/components/code-editor/editor-affordances";
import { isVirtualTabPath, resolveVirtualTabTitle } from "@/shared/lib/virtual-viewer-registry";
import { migrateScratchNoteRatio } from "@/features/viewer/views/drag-resize-utils";
import {
  defaultRightPaneLayout,
  migrateLegacyRightPaneState,
  reconcileLayoutToBuiltins,
  revealPanel,
  removePanel,
  type RightPaneLayout,
} from "@/features/shell/right-pane/right-pane-layout";
import {
  foregroundTilesFor,
  hasForegroundTile,
  epicForegroundTiles,
  scopedForegroundTiles,
  isSplitActive,
} from "@/features/viewer/lib/foreground-tiles";
import {
  buildWorkspaceGrouping,
  buildEpicDocPathIndex,
  resolveAdjacentWorkspaceInScope,
  resolveSelectedRailKey,
} from "@/features/viewer/lib/group-workspaces-by-epic";
import {
  moveInOrder,
  teamWorkspaceOrder,
  type OrderPlace,
  type TeamOrderSource,
} from "@/features/viewer/lib/workspace-order";
import type { Heading } from "@/features/viewer/lib/note-headings";
import {
  type MosaicNode,
  type SavedPresetTree,
  type TilePlacementTarget,
  parseMosaicNode,
  seedTreeFromFlat,
  buildBalancedGrid,
  reconcileMosaic,
  leafWorkspaceIds,
  pruneLeaf,
  setNodeSizes,
  splitAt,
  moveLeaf,
  replaceLeafWorkspaceId,
} from "@/features/viewer/lib/mosaic-tree";
import type {
  PipelineStatus,
  DerivedPipelineTree,
  StoryRef,
} from "@/shared/lib/pipeline-graph/types";
import { normalizeStatus } from "@/shared/lib/pipeline-graph/pipeline-utils";
import { writePipelineStatus } from "@/shared/lib/pipeline-status-writer";
import { useFileRefreshStore } from "./file-refresh-store";
import {
  findWorktreeOnDisk,
  normalizeStoryFilePath,
  parseSourceRepoFromFrontmatter,
  parseStoryIdFromFrontmatter,
  parseStoryTitle,
  branchRefMatches,
  resolveWorktreeParams,
  resolveWorktreePath,
  setupWorktree,
  createWorktree,
  runWorktreeSetupPhase,
  storyIdFromPath,
  type WorktreeSetupError,
} from "@/shared/lib/worktree";
// SLW-RI-6 (AC-2/AC-6): imported from the submodule (not the barrel) so the
// many `vi.mock("@/shared/lib/worktree", …)` characterization suites — which
// don't stub this gate — run the real (transparent-for-one-open) limiter
// instead of throwing on a missing mock export.
import { withCreationSlot } from "@/shared/lib/worktree/creation-fanout";
// RF-perf-14 (decouple): the low-cap background-setup gate — same submodule-import
// rationale as `withCreationSlot` (characterization suites don't stub it).
import {
  MAX_BACKGROUND_SETUP_CAP,
  withBackgroundSetupSlot,
} from "@/shared/lib/worktree/background-setup-fanout";
import { findWorkspaceRepoBySlug } from "@/shared/lib/build-synthetic-repo";
import { trackWorktreeEstablish } from "@/shared/lib/worktree/pending-establish";
import {
  checkBranchExists,
  checkWorktreeDirty,
  getWorktreeUnpublishedContext,
  establishWorktreeStoryLink,
  getWorktreeConfig,
  listWorktrees,
  probeWorktreeConfig,
  readFile,
  removeWorktree,
  storyDefaultStartPoint,
  switchWorktreeBranch,
} from "@/tauri/commands";
import { toast } from "sonner";
import { getErrorMessage } from "@/shared/lib/error-message";
import {
  applyPrepare,
  clearRun,
  failRun,
  type AgentRun,
  step1StateFromLocalPhase,
  type WorkspaceSetupState,
} from "@/features/viewer/lib/workspace-setup-state";
import type { AgentPrepareState } from "@/features/agents/lib/agent-prepare-progress";
import {
  DEFAULT_WORKSPACE_BAR_SIZE,
  resolveWorkspaceBarSize,
  type WorkspaceBarSize,
} from "@/shared/lib/workspace-bar-size";
import {
  isRemoteWorkspace,
  remoteWorkspaceDriver,
  type MachineLocation,
  type WorkspaceLocation,
  type WorkspaceLocationChoice,
} from "@/shared/lib/workspace-location";

export interface TabState {
  path: string;
  name: string;
  /** undefined / false = pinned. At most one tab per repo has preview=true. */
  preview?: boolean;
}

// The terminal-AI status vocabulary lives in `@/shared/lib/terminal-ai-status`
// (framework-free, mock-safe). Re-exported here so existing `TerminalAiStatus`
// importers keep their path.
export type { TerminalAiStatus, TerminalWireStatus };

/** SW-AM-4: the retained per-session live-progress slice value — the field set of
 *  a `signal_progress` delta (`dashboard-stream/types.ts`), plus a `settled` flag
 *  the terminal `signal_done` write sets so the reader shows the summary (not a
 *  stale in-progress note) and a raw `pty-exit` leaves it alone. `phase` is the
 *  fixed lifecycle stage (see `@/shared/lib/roles`); `step`/`total` drive an
 *  optional N-of-M bar; `note` is "what it's working on" (untrusted — render as
 *  text, never HTML). */
export type SessionProgress = {
  phase: Phase;
  step?: number;
  total?: number;
  note?: string;
  settled?: boolean;
};

/** SW-GS-2a (AC-4): the agent-session role a codesense-managed tab declares at
 *  spawn. Agent-neutral (not a Claude concept). SW-GS-2b (AC-1/3) WIDENED this
 *  to a free string tag so agent surfaces (story/epic) bind via the same
 *  resolver — which matches the *requested* role — WITHOUT editing a central
 *  union. Absent on ordinary terminals. */
export type TerminalRole = string;

/** SW-GW-8 (AC-5): provenance value stamped on a session the CodeSense nav/UI
 *  opened (a story worktree, an epic terminal) — as opposed to a dashboard
 *  `docPath`. The single source of truth for this string: the store STAMPS it via
 *  `dashboardRegisterSession`, and the panel's `formatLauncher` HUMANIZES it — both
 *  import this constant so the producer and consumer can't drift. (Rust only stores
 *  whatever the FE sends; it emits no launcher literal of its own.) */
export const CODESENSE_LAUNCHER = "codesense";

/** One terminal tab as rendered — a projection of a host layout entry. Its id
 *  is its PTY session id: there is no second handle. */
export interface TerminalTab {
  id: string;
  name: string;
  sessionId: string;
  /** SW-GS-2a (AC-4): set on a managed agent tab at spawn. The agent surface
   *  binds to the tab carrying the requested role — never by scanning/adopting
   *  a user's untagged session. */
  role?: TerminalRole;
  /** Set once the tab's process exited in-session: the tab stays, read-only,
   *  with its final output and the real exit status, until it is closed. */
  exit?: { code: number | null; signal: string | null };
  /** Set on a tab whose session runs on a remote Machine: which one, and the
   *  handle its pane attaches with. */
  origin?: TerminalTabOrigin;
}

/** Where a remote tab's session runs. `offlineSince` is set while the Machine
 *  is not reachable: the tab stays, frozen, until it is live again. */
export interface TerminalTabOrigin {
  machineId: string;
  /** The runtime member holding it (`memberKey`). */
  member: string;
  handle: string;
  requestId?: string;
  offlineSince?: number;
}

/** A tab to open: its session id (which becomes the tab id) plus, optionally,
 *  its name — the host names it `Terminal N` otherwise — and its role. */
export interface NewTerminalTab {
  sessionId: string;
  name?: string;
  role?: TerminalRole;
}

/** Mint a tab to open, with a collision-proof session id: a `Date.now()`
 *  scheme aliases two tabs minted in the same millisecond onto one PTY, so
 *  `crypto.randomUUID()` removes the collision class entirely.
 *
 *  SW-GW-6: an optional `title` names the tab (e.g. "Architecture Review" for
 *  a named story session) instead of the host's `Terminal N` default. The name
 *  stays inline-editable via `renameTerminalTab` afterward. */
export function createTerminalTab(title?: string): NewTerminalTab {
  return { sessionId: `shell-${crypto.randomUUID()}`, ...(title ? { name: title } : {}) };
}

/** The terminal-tab read model: teamKey → workspaceId → tabs in strip order.
 *  Every reader goes through the selectors below. */
export type TerminalTabIndex = Record<string, Record<string, TerminalTab[]>>;

const EMPTY_PANED_IDS = Object.freeze([]) as unknown as string[];

/** The frozen empty tab list: one identity for "no tabs", so a selector that
 *  finds none never hands a subscriber a fresh `[]`. */
export const EMPTY_TERMINAL_TABS = Object.freeze([]) as unknown as TerminalTab[];

/** The workspace record behind `(teamKey, workspaceId)` — open, or minimized
 *  (its record is its shadow's). */
function workspaceRecord(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): Workspace | undefined {
  return (
    readTeamKeyed(s.workspaces, teamKey, []).find((w) => w.id === workspaceId) ??
    s.softClosedWorkspaces[teamKey]?.[workspaceId]?.workspace
  );
}

/** SW-AGR-6: the directory `(teamKey, workspaceId)` runs in (on its machine,
 *  for a remote workspace), or null. */
export function selectWorkspaceCwd(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): string | null {
  return workspaceRecord(s, teamKey, workspaceId)?.cwd ?? null;
}

/** The terminal host's key for `(teamKey, workspaceId)`. A workspace whose
 *  record is gone (an agent hidden without one) keys by its id. */
export function selectWorkspaceKey(s: ViewerState, teamKey: string, workspaceId: string): string {
  return workspaceKey(teamKey, workspaceRecord(s, teamKey, workspaceId) ?? { id: workspaceId });
}

/** The teamKey that owns `workspaceId` (first team whose live or shadow bucket
 *  holds it), or `null` for a workspaceId known to no team. Used by sibling-
 *  overlay closers that are driven at the workspace level but need to resolve
 *  the active tab's sessionId. O(teams) worst case; the map is small. */
export function teamKeyOfWorkspace(s: ViewerState, workspaceId: string): string | null {
  for (const [teamKey, list] of Object.entries(s.workspaces)) {
    if (list.some((w) => w.id === workspaceId)) return teamKey;
  }
  for (const [teamKey, bucket] of Object.entries(s.softClosedWorkspaces)) {
    if (bucket[workspaceId]) return teamKey;
  }
  return null;
}

/** The rendered host layout of `(teamKey, workspaceId)`, if any host holds it. */
function projectedWorkspace(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): ProjectedWorkspace | undefined {
  return s.terminalLayout.byKey[selectWorkspaceKey(s, teamKey, workspaceId)];
}

interface TerminalTabIndexes {
  open: TerminalTabIndex;
  running: TerminalTabIndex;
  minimized: TerminalTabIndex;
  hidden: TerminalTabIndex;
}

let tabIndexMemo: {
  byKey: TerminalLayoutProjection["byKey"];
  workspaces: ViewerState["workspaces"];
  softClosed: ViewerState["softClosedWorkspaces"];
  value: TerminalTabIndexes;
} | null = null;

/** Every host-held workspace's tabs, split into open and minimized
 *  workspaces and keyed back to `(teamKey, workspaceId)`: an open tile's shown
 *  tabs (`open`), and every one of its running tabs — shown, then Hidden
 *  (`running`); a minimized tile's tabs, Hidden ones included (`minimized`). A
 *  key with no workspace record (a stale layout key, a session opened for a
 *  story not open here) reads as open under its own workspace form; such
 *  record-less openTabs buckets are consumer-skipped (orphan handling, no
 *  hidden-agent surface). Memoized on the three slices it reads, so each
 *  index keeps its identity across unrelated store updates. */
function terminalTabIndexes(s: ViewerState): TerminalTabIndexes {
  const memo = tabIndexMemo;
  if (
    memo &&
    memo.byKey === s.terminalLayout.byKey &&
    memo.workspaces === s.workspaces &&
    memo.softClosed === s.softClosedWorkspaces
  ) {
    return memo.value;
  }
  const open: TerminalTabIndex = {};
  const running: TerminalTabIndex = {};
  const minimized: TerminalTabIndex = {};
  const hidden: TerminalTabIndex = {};
  for (const [key, layout] of Object.entries(s.terminalLayout.byKey)) {
    const parsed = parseWorkspaceKey(key);
    if (!parsed) continue;
    const { teamKey, form } = parsed;
    const record = readTeamKeyed(s.workspaces, teamKey, []).find(
      (w) => workspaceKey(teamKey, w) === key,
    );
    const shadow = record
      ? undefined
      : Object.values(s.softClosedWorkspaces[teamKey] ?? {}).find(
          (sc) => workspaceKey(teamKey, sc.workspace) === key,
        );
    const id = record?.id ?? shadow?.workspace.id ?? form;
    if (layout.hiddenTabs.length > 0) (hidden[teamKey] ??= {})[id] = layout.hiddenTabs;
    const all =
      layout.hiddenTabs.length === 0 ? layout.tabs : [...layout.tabs, ...layout.hiddenTabs];
    if (shadow) {
      (minimized[teamKey] ??= {})[id] = all;
    } else {
      (open[teamKey] ??= {})[id] = layout.tabs;
      (running[teamKey] ??= {})[id] = all;
    }
  }
  const value = { open, running, minimized, hidden };
  tabIndexMemo = {
    byKey: s.terminalLayout.byKey,
    workspaces: s.workspaces,
    softClosed: s.softClosedWorkspaces,
    value,
  };
  return value;
}

/** The tabs of every OPEN workspace, by team and workspace. Identity-stable
 *  across unrelated store updates. */
export function selectTerminalTabIndex(s: ViewerState): TerminalTabIndex {
  return terminalTabIndexes(s).open;
}

/** Every RUNNING tab of the open workspaces, by team and workspace: the tabs
 *  shown on each tile followed by the ones Hidden from it (listed under its
 *  tray chip, still running). The index every "which sessions exist / run
 *  here" scan reads — never the render index. Identity-stable across
 *  unrelated store updates. */
export function selectRunningTerminalTabIndex(s: ViewerState): TerminalTabIndex {
  return terminalTabIndexes(s).running;
}

/** The tabs Hidden from a tile — open or minimized — by team and workspace:
 *  running Background entries the user marked hidden, listed under the tray
 *  chip. Identity-stable across unrelated store updates. */
export function selectHiddenTerminalTabIndex(s: ViewerState): TerminalTabIndex {
  return terminalTabIndexes(s).hidden;
}

/** The tabs of every MINIMIZED (in the tray, still running) workspace — Hidden
 *  ones included — by team and workspace. Identity-stable across unrelated
 *  store updates. */
export function selectMinimizedTerminalTabIndex(s: ViewerState): TerminalTabIndex {
  return terminalTabIndexes(s).minimized;
}

/** One workspace's tabs in strip order — open or minimized — or the frozen
 *  empty list. */
export function selectWorkspaceTabs(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): TerminalTab[] {
  return projectedWorkspace(s, teamKey, workspaceId)?.tabs ?? EMPTY_TERMINAL_TABS;
}

/** The tabs Hidden from one workspace's tile — its running Background
 *  entries, listed under its tray chip — or the frozen empty list. */
export function selectHiddenTerminalTabs(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): TerminalTab[] {
  return projectedWorkspace(s, teamKey, workspaceId)?.hiddenTabs ?? EMPTY_TERMINAL_TABS;
}

/** One workspace's focused tab id — open or minimized. */
export function selectActiveTerminalTab(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): string | undefined {
  return projectedWorkspace(s, teamKey, workspaceId)?.active;
}

/** The session the CLI shows for a team: its focused workspace's active tab, when running. */
export function selectFocusedSession(s: ViewerState, teamKey: string): string | undefined {
  const workspaceId = s.activeWorkspace?.[teamKey];
  if (!workspaceId) return undefined;
  const active = selectActiveTerminalTab(s, teamKey, workspaceId);
  return selectTerminalTabIndex(s)[teamKey]?.[workspaceId]?.find((tab) => tab.sessionId === active)
    ?.sessionId;
}

/** The tab bound to `sessionId` anywhere — shown, Hidden, or minimized (each
 *  is still running and addressable). */
export function selectTabBySessionId(s: ViewerState, sessionId: string): TerminalTab | undefined {
  return selectSessionHome(s, sessionId)?.tab;
}

/** Where the OPEN tab bound to `sessionId` lives. A minimized workspace's tab
 *  reads as tabless, so its agent takes the foreground/adopt path rather than a
 *  focus that could not reveal it. */
export function selectOpenTabLocation(
  s: ViewerState,
  sessionId: string,
): { teamKey: string; workspaceId: string; tab: TerminalTab } | undefined {
  return findTabLocationBySessionId(selectTerminalTabIndex(s), sessionId);
}

/** Where the Hidden tab bound to `sessionId` is listed — in any team, its tile
 *  open or in the tray. */
export function selectHiddenTabLocation(
  s: ViewerState,
  sessionId: string,
): { teamKey: string; workspaceId: string; tab: TerminalTab } | undefined {
  return findTabLocationBySessionId(terminalTabIndexes(s).hidden, sessionId);
}

/** Where the tab bound to `sessionId` lives in `index`. Pure. */
export function findTabLocationBySessionId(
  index: TerminalTabIndex,
  sessionId: string,
): { teamKey: string; workspaceId: string; tab: TerminalTab } | undefined {
  for (const [teamKey, byWorkspace] of Object.entries(index)) {
    for (const [workspaceId, tabs] of Object.entries(byWorkspace)) {
      const tab = tabs.find((t) => t.sessionId === sessionId);
      if (tab) return { teamKey, workspaceId, tab };
    }
  }
  return undefined;
}

/** Where the tab bound to `sessionId` lives — shown, Hidden, or minimized —
 *  with its workspace record (a minimized workspace's record is its shadow's;
 *  an orphan tab with no live record has none). */
export function selectSessionHome(
  s: ViewerState,
  sessionId: string,
):
  | {
      teamKey: string;
      workspaceId: string;
      tab: TerminalTab;
      workspace: Workspace | undefined;
      minimized: boolean;
    }
  | undefined {
  const open = findTabLocationBySessionId(selectRunningTerminalTabIndex(s), sessionId);
  if (open) {
    const workspace = s.workspaces[open.teamKey]?.find((w) => w.id === open.workspaceId);
    return { ...open, workspace, minimized: false };
  }
  const shadowed = findTabLocationBySessionId(selectMinimizedTerminalTabIndex(s), sessionId);
  if (shadowed) {
    const workspace = s.softClosedWorkspaces[shadowed.teamKey]?.[shadowed.workspaceId]?.workspace;
    return { ...shadowed, workspace, minimized: true };
  }
  return undefined;
}

/** SW-GS-2a (AC-4): "the bound session" of `role` in a team — the running tab
 *  carrying that role, shown, Hidden or minimized (an exited one, kept
 *  read-only, is not the agent). Matches on the role PARAMETER, never on a
 *  specific role literal, and never adopts an untagged user session. */
export function selectRoleTab(
  s: ViewerState,
  teamKey: string,
  role: TerminalRole,
): { workspaceId: string; tab: TerminalTab } | undefined {
  for (const index of [selectRunningTerminalTabIndex(s), selectMinimizedTerminalTabIndex(s)]) {
    for (const [workspaceId, tabs] of Object.entries(index[teamKey] ?? {})) {
      const tab = tabs.find((t) => t.role === role && t.exit === undefined);
      if (tab) return { workspaceId, tab };
    }
  }
  return undefined;
}

/** How many of `(teamKey, workspaceId)`'s new terminals are held for a
 *  terminal-server restart. */
export function selectHeldTerminalOpens(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): number {
  return s.heldTerminalOpens[selectWorkspaceKey(s, teamKey, workspaceId)] ?? 0;
}

/** SW-EW-46: the workspace's paned tab ids (the grid cells), in pane order.
 *  Empty ⇒ tab mode. Identity-stable across unrelated updates (a frozen
 *  sentinel when empty), so a component can subscribe to it or `useMemo` a
 *  `Set` from it without churn. */
export function workspacePanedTabIds(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): string[] {
  return projectedWorkspace(s, teamKey, workspaceId)?.grid.paned ?? EMPTY_PANED_IDS;
}

/** SW-EW-46: whether the workspace's grid is collapsed to tabs (its paned set
 *  kept warm). */
export function selectTabGridCollapsed(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): boolean {
  return projectedWorkspace(s, teamKey, workspaceId)?.grid.collapsed ?? false;
}

/** SW-EW-46: the workspace's tab-grid preset, defaulting to `columns`. */
export function workspaceTabGridPreset(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
): LayoutPreset {
  return projectedWorkspace(s, teamKey, workspaceId)?.grid.preset ?? "columns";
}

/** How to open a terminal tab beyond its session, name and role. */
export interface OpenTerminalTabOptions {
  commandArgv?: ScopedExecution["argv"];
  nativeLaunch?: ScopedExecution["nativeLaunch"];
  /** The directory the shell starts in — default: the workspace's. Required
   *  for a workspace with no record (an agent hidden without one). */
  cwd?: string;
  /** The kind of a workspace with no record. Default: `generic`. */
  kind?: WorkspaceKind;
  /** A background entry runs out of sight (a headless agent). Default: a tab. */
  placement?: TerminalPlacement;
  /** Focus the new tab. Default: true. */
  activate?: boolean;
  /** SW-GS-2a (AC-7): env var names the shell must not inherit. */
  envRemove?: string[];
  /** Account-selection intent; trusted Rust resolves it again at spawn. */
  account?: ResolveAgentAccountInput;
  /** Opaque one-use backend binding for an MCP-scoped environment. */
  environmentBinding?: string;
}

/** How a respawn's new process is started: the shell rules of the launch that
 *  first started it, and a scoped agent launch's MCP scope to project afresh. */
export interface RespawnTerminalTabOptions {
  cwd?: string;
  envRemove?: string[];
  account?: ResolveAgentAccountInput;
  scope?: TerminalRespawnScope;
}

/** Sessions with a close in flight — so "close all" still knows which close
 *  empties the workspace before the host announces any of them. */
const closingSessions = new Set<string>();

/** Hides sent to a host and not settled yet, by session: the workspace it
 *  leaves and the hide itself. Until its host answers, a hiding tab still shows
 *  on the strip — these count it gone already, and a fold waits on it. */
const hidingSessions = new Map<string, { key: string; done: Promise<void> }>();

/** Log a refused layout verb. The host's next announcement stays the truth. */
function logLayoutVerb(verb: string): (e: unknown) => void {
  return (e) => logger.warn("terminal", `${verb} failed: ${getErrorMessage(e)}`);
}

/** The viewer-held grids in saved app state, keeping only well-formed ones. */
function savedViewerGrids(saved: unknown): Record<string, ViewerGrid> {
  if (!saved || typeof saved !== "object") return {};
  const grids: Record<string, ViewerGrid> = {};
  for (const [key, value] of Object.entries(saved as Record<string, unknown>)) {
    if (!value || typeof value !== "object") continue;
    const { paned, preset, collapsed } = value as Record<string, unknown>;
    if (
      !Array.isArray(paned) ||
      (preset !== "columns" && preset !== "rows" && preset !== "grid") ||
      typeof collapsed !== "boolean"
    )
      continue;
    grids[key] = {
      paned: paned.flatMap((p: unknown) =>
        p &&
        typeof p === "object" &&
        typeof (p as { session?: unknown }).session === "string" &&
        typeof (p as { member?: unknown }).member === "string"
          ? [
              {
                session: (p as { session: string }).session,
                member: (p as { member: string }).member,
              },
            ]
          : [],
      ),
      preset,
      collapsed,
    };
  }
  return grids;
}

/** The saved tab-grid sizes in app state, keeping only well-formed ones. */
function savedTabGrids(saved: unknown): Record<string, SavedPresetTree> {
  if (!saved || typeof saved !== "object") return {};
  const grids: Record<string, SavedPresetTree> = {};
  for (const [key, value] of Object.entries(saved as Record<string, unknown>)) {
    if (!value || typeof value !== "object") continue;
    const { preset, root } = value as Record<string, unknown>;
    if (preset !== "columns" && preset !== "rows" && preset !== "grid") continue;
    const node = parseMosaicNode(root);
    if (node) grids[key] = { preset, root: node };
  }
  return grids;
}

/** The saved tab-grid sizes whose workspace is still open or minimized. */
function tabGridsOfLiveWorkspaces(s: ViewerState): Record<string, SavedPresetTree> {
  const live = new Set<string>();
  for (const [teamKey, list] of Object.entries(s.workspaces)) {
    for (const ws of list) live.add(workspaceKey(teamKey, ws));
  }
  for (const [teamKey, shadows] of Object.entries(s.softClosedWorkspaces)) {
    for (const shadow of Object.values(shadows)) live.add(workspaceKey(teamKey, shadow.workspace));
  }
  const kept = Object.entries(s.tabGridLayout).filter(([key]) => live.has(key));
  return kept.length === Object.keys(s.tabGridLayout).length
    ? s.tabGridLayout
    : Object.fromEntries(kept);
}

/** `state`'s tab-grid sizes without `tabId`'s cell in `(teamKey, workspaceId)`:
 *  the others keep their proportions; one left is no arrangement to keep. */
function withoutTabGridCell(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
  tabId: string,
): Record<string, SavedPresetTree> {
  const key = selectWorkspaceKey(s, teamKey, workspaceId);
  const saved = s.tabGridLayout[key];
  if (!saved || !leafWorkspaceIds(saved.root).includes(tabId)) return s.tabGridLayout;
  const { [key]: _pruned, ...rest } = s.tabGridLayout;
  const root = pruneLeaf(saved.root, tabId);
  return root?.kind === "split" ? { ...rest, [key]: { preset: saved.preset, root } } : rest;
}

/** `input` without what a retired private authority sealed: a snapshot read
 *  (or a change, an admission) that was in flight when the private owner
 *  changed never shows that owner's remote tabs again. `null` when nothing of
 *  it is still current. */
function withoutRetiredAuthority(input: LayoutInput): LayoutInput | null {
  if (input.kind === "snapshot") {
    const replies = input.replies.filter(
      (reply) =>
        reply.mode === "unavailable" || acceptsPrivateAuthority(reply.privateAuthorityEpoch),
    );
    return replies.length === input.replies.length ? input : { ...input, replies };
  }
  if (input.kind === "change")
    return acceptsPrivateAuthority(input.privateAuthorityEpoch) ? input : null;
  if (input.kind === "admission")
    return acceptsPrivateAuthority(input.row.privateAuthorityEpoch) ? input : null;
  return input;
}

/** A remote tab's exact admitted target — `null` while its Machine is not
 *  live, or when its id is held by more than one member — or `undefined` for a
 *  tab of This Mac, which its id reaches. */
function remoteSessionTarget(session: string): RuntimeSessionTarget | null | undefined {
  const layout = useViewerStore.getState().terminalLayout;
  if (!heldRemotely(layout, session)) return undefined;
  return sessionTarget(layout, session) ?? null;
}

/** Run a layout verb about `session` on the member that holds it: a remote tab
 *  by its admitted target, refused while its Machine is offline. */
function inSessionMember<T>(
  session: string,
  verb: (reference: RuntimeSessionTarget | string) => Promise<T>,
): Promise<T> {
  const remote = remoteSessionTarget(session);
  if (remote === null) return Promise.reject(new Error("this terminal's machine is offline"));
  return verb(remote ?? session);
}

const terminalCloseTab = (session: string, mode: TerminalCloseMode) =>
  inSessionMember(session, (ref) => closeTabIn(ref, mode));
const terminalMoveTab = (session: string, toIndex: number) =>
  inSessionMember(session, (ref) => moveTabIn(ref, toIndex));
const terminalRenameTab = (session: string, name: string) =>
  inSessionMember(session, (ref) => renameTabIn(ref, name));
const terminalSetTags = (session: string, patch: Record<string, string | null>) =>
  inSessionMember(session, (ref) => setTagsIn(ref, patch));
const terminalPlaceTab = (
  session: string,
  workspace: string,
  placement: TerminalPlacement,
  index?: number,
  descriptor?: TerminalWorkspaceDescriptor,
) => inSessionMember(session, (ref) => placeTabIn(ref, workspace, placement, index, descriptor));

// A newer request, including one that fails, retires older ACKs for this workspace.
const terminalFocusRequests = new Map<string, object>();

/** Focus `session` in the workspace keyed `key`. A remote tab is focused on its
 *  Machine and held as this viewer's focus over This Mac's own; a local one
 *  gives that focus back. `null` clears the focus in This Mac's hosts. */
async function terminalSetActive(key: string, session: string | null): Promise<void> {
  const request = {};
  terminalFocusRequests.set(key, request);
  const revision = currentContextNavigationRevision();
  try {
    const remote = session === null ? undefined : remoteSessionTarget(session);
    if (remote === null) throw new Error("this terminal's machine is offline");
    if (!remote) await setActiveIn(key, session);
    else {
      const { session: _session, ...member } = remote;
      await setActiveIn({ ...member, workspace: key }, remote);
    }
    if (
      terminalFocusRequests.get(key) !== request ||
      revision !== currentContextNavigationRevision()
    )
      return;
    useViewerStore
      .getState()
      .applyLayout({ kind: "focused", key, session: remote ? session : null });
    useViewerStore.getState().persistState();
  } finally {
    if (terminalFocusRequests.get(key) === request) terminalFocusRequests.delete(key);
  }
}

/** Arrange a workspace's one grid. This Mac's host holds its own paned tabs
 *  and the shape; the paned remote tabs (which no host of This Mac can hold)
 *  are this viewer's, kept in the projection and saved with the app's state. A
 *  Machine's own grid is never sent. */
function terminalSetGrid(key: string, grid: TerminalGridState): Promise<void> {
  const store = useViewerStore.getState();
  const layout = store.terminalLayout;
  const remote: ViewerGrid["paned"] = [];
  const local: string[] = [];
  for (const id of grid.paned) {
    const holders = holdersOfSession(layout, id);
    const member = holders.length === 1 ? layout.members[holders[0]] : undefined;
    if (member?.target.member.slot === "remote") remote.push({ session: id, member: holders[0] });
    else if (!heldRemotely(layout, id)) local.push(id);
  }
  const heldLocally = Object.values(layout.members).some(
    (member) => member.target.member.slot !== "remote" && member.workspaces[key],
  );
  const heldRemote = Object.values(layout.members).some(
    (member) => member.target.member.slot === "remote" && member.workspaces[key],
  );
  // The viewer's part: its remote cells, and the shape while This Mac holds
  // no tab of the workspace.
  const arranged = store.applyLayout({
    kind: "viewer-grid",
    key,
    grid:
      remote.length > 0 || (heldRemote && !heldLocally)
        ? { paned: remote, preset: grid.preset, collapsed: grid.collapsed }
        : null,
  });
  if (arranged === "applied") store.persistState();
  if (heldRemote && !heldLocally) return Promise.resolve();
  return setGridIn(key, { ...grid, paned: local });
}

/** Dispose a session's display. The terminal manager is loaded lazily: it
 *  reaches the machines store, which subscribes to this store as it loads, so a
 *  static import would be a cycle. Fire-and-forget, so a close's own
 *  bookkeeping (the replacement tab, the closing guard) never waits on it. */
function disposeTerminalDisplay(sessionId: string, verb: string): void {
  // A remote tab's display is keyed by its origin, not its id.
  const layout = useViewerStore.getState().terminalLayout;
  const found = memberOfSession(layout, sessionId);
  const member = found && layout.members[found.key];
  const reference =
    member && member.target.member.slot === "remote" && member.privateAuthorityEpoch
      ? {
          target: { ...member.target, session: sessionId },
          teamKey: "",
          workspaceKey: found.workspace,
          privateAuthorityEpoch: member.privateAuthorityEpoch,
        }
      : sessionId;
  import("@/shared/terminal/terminal-manager")
    .then(({ terminalManager }) => terminalManager.dispose(reference))
    .catch(logLayoutVerb(verb));
}

/** End `tabs` in their host, and their displays: nothing will show these
 *  sessions again. Only the tabs: a workspace's background entries (an agent
 *  detached to the Agents panel, a headless launch) keep running. A remote
 *  Machine's tab is only ever ended by the user's own confirmed End
 *  (`endRemote`): every other teardown detaches it — its display goes, its
 *  session keeps running there and comes back with its workspace. */
function killTerminalTabs(
  tabs: readonly TerminalTab[],
  verb: string,
  opts: { endRemote?: boolean } = {},
): void {
  for (const tab of tabs) {
    if (tab.origin && !opts.endRemote) {
      disposeTerminalDisplay(tab.sessionId, verb);
      continue;
    }
    terminalCloseTab(tab.sessionId, "kill")
      .then(() => disposeTerminalDisplay(tab.sessionId, verb))
      .catch(logLayoutVerb(verb));
  }
}

/** What another client needs to render a workspace it did not open. */
function workspaceDescriptor(teamKey: string, ws: Workspace): TerminalWorkspaceDescriptor {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  return {
    displayName: ws.storyTitle ?? ws.storyId ?? ws.id,
    kind: ws.kind,
    org: orgSlug,
    team: teamName,
    storyId: ws.kind === "story" ? (ws.storyId ?? null) : null,
    repo: ws.sourceRepoSlug ?? null,
    worktreePath: ws.cwd || null,
  };
}

/** The spawn behind a new tab in `ws`: a warm-pool shell at its directory, or
 *  a cold one when the env must be stripped or an account bound (pre-booted
 *  pool shells inherit the full parent env and can't honor either). */
function tabSpawn(
  s: ViewerState,
  ws: Workspace,
  size: TerminalSize,
  opts: OpenTerminalTabOptions,
  cold = false,
): TerminalTabSpawn {
  const starshipPreset = s.settings.starshipPreset;
  const shellIntegration = s.settings.shellIntegration ?? true;
  const meta = {
    workspaceId: ws.id,
    storyId: ws.kind === "story" ? (ws.storyId ?? ws.id) : undefined,
    scrollback: scrollbackLines(s.settings.terminalScrollback),
  };
  if (
    cold ||
    opts.commandArgv ||
    (opts.envRemove?.length ?? 0) > 0 ||
    opts.account ||
    opts.environmentBinding
  ) {
    return {
      kind: "cold",
      command: "",
      cwd: ws.cwd,
      size,
      interactive: true,
      starshipPreset,
      shellIntegration,
      ...meta,
      envRemove: opts.envRemove,
      account: opts.account,
      environmentBinding: opts.environmentBinding,
      commandArgv: opts.commandArgv,
      nativeLaunch: opts.nativeLaunch,
    };
  }
  return { kind: "warm", targetCwd: ws.cwd, size, starshipPreset, shellIntegration, ...meta };
}

/** Open `tab` in `ws` through its terminal host: spawned and placed at once,
 *  at the last measured pane size. Resolves once the host placed it. SW-RT-6d:
 *  a terminal server of another version refuses it — the restart prompt is the
 *  whole answer, and the tab opens once the server has been restarted. */
function openLayoutTab(
  s: ViewerState,
  teamKey: string,
  ws: Workspace,
  tab: NewTerminalTab,
  opts: OpenTerminalTabOptions,
): Promise<void> {
  const size = terminalSpawnSize();
  noteFreshOpen(tab.sessionId);
  const key = workspaceKey(teamKey, ws);
  const open = () =>
    terminalOpenTab({
      machineId: LOCAL_MACHINE_ID,
      workspace: key,
      session: tab.sessionId,
      descriptor: workspaceDescriptor(teamKey, ws),
      name: tab.name,
      role: tab.role,
      placement: opts.placement ?? "tab",
      activate: opts.activate ?? true,
      spawn: tabSpawn(s, ws, size, opts),
    });
  return open()
    .catch((e: unknown) => {
      if (!isTerminalServerRefusal(e)) throw e;
      // Held: no tab is placed, so no pane mounts and nothing typed meanwhile
      // can reach it — the terminal starts clean once the server restarts. A
      // workspace closed while held drops the open.
      adjustHeldOpens(key, 1);
      return new Promise<unknown>((resolve, reject) => {
        holdForTerminalServer(() => {
          adjustHeldOpens(key, -1);
          if (!workspaceRecord(useViewerStore.getState(), teamKey, ws.id)) {
            reject(new Error(`workspace ${ws.id} closed while its terminal was held`));
            return;
          }
          void open().then(resolve, reject);
        });
      });
    })
    .then(
      () => undefined,
      (e: unknown) => {
        forgetFreshOpen(tab.sessionId);
        throw e;
      },
    );
}

/** Count a held terminal open in (or out of) the workspace keyed `key`. */
function adjustHeldOpens(key: string, delta: number): void {
  useViewerStore.setState((st) => {
    const count = (st.heldTerminalOpens[key] ?? 0) + delta;
    const { [key]: _dropped, ...rest } = st.heldTerminalOpens;
    return { heldTerminalOpens: count > 0 ? { ...rest, [key]: count } : rest };
  });
}

/** Put a tile's Hidden tabs back on it (its host moves each Background entry
 *  to a tab): a tile leaving the foreground — hidden or displaced into the
 *  tray, or ended — takes them along. A no-op when it has none. */
function foldHiddenTabsIntoTile(s: ViewerState, teamKey: string, workspaceId: string): void {
  const key = selectWorkspaceKey(s, teamKey, workspaceId);
  for (const tab of projectedWorkspace(s, teamKey, workspaceId)?.hiddenTabs ?? []) {
    unhideTerminal(tab.sessionId, key);
  }
  // A hide still on its way to the host folds back once it lands.
  for (const [session, hide] of hidingSessions) {
    if (hide.key === key) void hide.done.then(() => unhideTerminal(session, key));
  }
}

/** Put a Hidden entry back on its tile's strip. Its Hidden mark stays: it
 *  only counts on a Background entry, and every Hide stamps it again — so a
 *  failed or raced move can never leave a running tab unlisted. */
function unhideTerminal(session: string, key: string): void {
  terminalPlaceTab(session, key, "tab").catch(logLayoutVerb("fold hidden tab"));
}

/** Send `(teamKey, workspaceId)`'s grid, with `patch` applied, to its host. */
function setWorkspaceGrid(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
  patch: Partial<TerminalGridState>,
): void {
  const grid: TerminalGridState = {
    paned: workspacePanedTabIds(s, teamKey, workspaceId),
    preset: workspaceTabGridPreset(s, teamKey, workspaceId) as TerminalGridPreset,
    collapsed: selectTabGridCollapsed(s, teamKey, workspaceId),
    ...patch,
  };
  terminalSetGrid(selectWorkspaceKey(s, teamKey, workspaceId), grid).catch(
    logLayoutVerb("arrange terminal grid"),
  );
}

/** Move every terminal of the workspace keyed `fromKey` — its tabs and its
 *  background entries, in order and keeping each placement — to `toKey` in its
 *  host, describing the target with `describe` applied to the source's
 *  descriptor; its tab-grid sizes move with it. For a workspace whose key changed: a renamed team, an agent
 *  following the active team, a story whose frontmatter id arrived. */
function moveWorkspaceTerminals(
  s: ViewerState,
  fromKey: string,
  toKey: string,
  describe: (d: TerminalWorkspaceDescriptor) => TerminalWorkspaceDescriptor = (d) => d,
): void {
  if (fromKey === toKey) return;
  const gridSizes = s.tabGridLayout[fromKey];
  if (gridSizes) {
    useViewerStore.setState((st) => {
      const { [fromKey]: _moved, ...rest } = st.tabGridLayout;
      return { tabGridLayout: { ...rest, [toKey]: gridSizes } };
    });
  }
  for (const member of Object.values(s.terminalLayout.members)) {
    if (member.target.member.machineId !== "local") continue;
    const layout = member.workspaces[fromKey];
    if (!layout) continue;
    const descriptor = describe(layout.descriptor);
    void layout.tabs
      .reduce(
        (moved, entry) =>
          moved.then(() =>
            terminalPlaceTab(entry.session, toKey, entry.placement, undefined, descriptor),
          ),
        Promise.resolve(),
      )
      .catch(logLayoutVerb("move workspace terminals"));
  }
}

/** One commit's change to a file, as a History tab shows it. */
export interface CommitDiff {
  sha: string;
  shortSha: string;
  subject: string;
  pr?: number;
  /** Who made it: an agent's name or the author. */
  who: string;
  /** Unix seconds. */
  committedAt: number;
  /** A rename: the path before (repo-relative). */
  oldPath?: string;
  added?: number;
  removed?: number;
  /** The tab flipped to File: it keeps its commit so Changes can come back to it. */
  showFile?: boolean;
}

/** What a tab on its Changes compares: the file against GitHub, or one commit's change. */
export type TabDiff = true | CommitDiff;

/** Is a tab with this entry showing its Changes (rather than the file)? */
export function isShowingDiff(diff: TabDiff | undefined | null): boolean {
  return diff === true || (!!diff && !diff.showFile);
}

/** SW-GW-1: id prefix for a generic (open-any-dir) workspace. */
export const GENERIC_WORKSPACE_ID_PREFIX = "generic:";

/** SW-GW-1: strip trailing slash(es) (keep root `/`) so the same directory always
 *  normalizes to one id/cwd — `/foo/bar/` and `/foo/bar` dedupe (AC-5). The native
 *  folder picker returns a clean absolute path, so only trailing slashes are
 *  normalized here (interior `//` isn't produced by the picker). */
export function normalizeWorkspaceDir(dirPath: string): string {
  return dirPath.replace(/\/+$/, "") || "/";
}

/** SW-GW-1: mint a stable, per-directory id for a generic workspace. An absolute
 *  path is injective, so the normalized path alone dedupes re-opens of the same
 *  dir — no hash needed. Team-namespacing is automatic (records live under
 *  `workspaces[teamKey]`), so two teams opening the same dir get two records. */
export function genericWorkspaceId(dirPath: string): string {
  return `${GENERIC_WORKSPACE_ID_PREFIX}${normalizeWorkspaceDir(dirPath)}`;
}

/**
 * RF-perf-14 (decouple) — state of a worktree's `setup:` commands running IN THE
 * BACKGROUND, after the workspace already flipped `"ready"` (terminal mounted +
 * usable). Drives a NON-blocking badge that overlays the live terminal — it
 * never hides the shell. Runtime-only (stripped from persistence, like
 * `statusError`): the setup process is gone after a restart.
 */
/** SW-WT-12 (S2): one row of the create-setup checklist. Seeded `queued` from the
 *  configured `setup:` list, then driven live by the run's step events. */
export interface WorktreeSetupStep {
  index: number;
  command: string;
  status: "queued" | "running" | "done" | "failed";
  exitCode?: number | null;
}

interface BackgroundSetupQueuedState {
  /** RF-perf-14d: `queued` = waiting behind the background-setup concurrency gate
   *  (a slot isn't free yet); `running` = actively installing (slot acquired);
   *  `failed` = the setup errored (retryable badge). A queued run reads as
   *  "Queued…" (no spinner) so a waiting worktree doesn't masquerade as installing. */
  status: "queued";
  /** Badge sub-label while queued/running (the setup phase, e.g. "Running setup…"). */
  label?: string;
  /** SW-WT-12 (S2): per-command checklist rows (empty for a no-config worktree). */
  steps?: WorktreeSetupStep[];
}

interface BackgroundSetupRunningState {
  status: "running";
  label?: string;
  /** SW-WT-12 (S2): per-command checklist rows, updated live from step events. */
  steps?: WorktreeSetupStep[];
}

interface BackgroundSetupFailedState {
  status: "failed";
  /** Human-readable fallback for legacy or unstructured failures. */
  error: string;
  diagnostics?: WorktreeSetupError["diagnostics"];
}

export type BackgroundSetupState =
  | BackgroundSetupQueuedState
  | BackgroundSetupRunningState
  | BackgroundSetupFailedState;

export interface Workspace {
  /** Identity invariant — unique per repoPath, never changes after creation.
   *  Path-derived via `storyIdFromPath` for story workspaces. Used by every
   *  lookup/dedup/key site (`switchWorkspace`, `closeWorkspace`, terminal
   *  panel keying, etc.). Display surfaces use `storyId` instead. */
  id: string;
  /** Workspace kind — see `workspace-kind-capabilities.ts` (the registry + the union
   *  source of truth): `root` (retired/dormant, dropped on load), `story`
   *  (worktree), `epic` (the live epic terminal workspace — renamed from the
   *  former `integration`), `generic` (a plain CLI at any user-chosen directory). */
  kind: WorkspaceKind;
  contextWorktreeId?: string;
  contextWorktreeName?: string;
  contextWorktreeBranch?: string;
  cwd: string;
  /** SW-EW-1: epic-workspace dir name (the pipeline-tree `dirName`, e.g.
   *  "epic-workspaces"). Set for `kind:"epic"` and `kind:"integration"`. Keyed
   *  identically to the pipeline graph so an epic's panel accent matches its
   *  graph color (AC-9). */
  epicDirPath?: string;
  /** Parent epic dirName for a story workspace, reconciled from the pipeline tree.
   *  Tray grouping resolves the live story path; this field is a cached hint. */
  parentEpic?: string;
  /** SW-EW-5: absolute on-disk path of the registered epic workspace dir
   *  (`{codesense_home}/users/{uid}/_epic/{org_slug}/{epic-id}/`, relocated
   *  outside the context-repo clone by SW-CP-15 Task 10). Set alongside
   *  `epicRegistered`. */
  epicWorkspaceDir?: string;
  /** SW-EW-15: full path of the epic's source doc, threaded onto the record at
   *  open time. The source of truth for this slug-less integration workspace's
   *  epic doc — `resolveEpicDocPath` reads it FIRST so repo read/write works
   *  without a nav-tree entry (the tree never scans for an empty repoPath). */
  epicDocPath?: string;
  storyFilePath?: string;
  /** R-18: display label sourced from story `story_id` frontmatter.
   *  Falls back to the path-derived `id` when frontmatter is absent.
   *  Reconciled on every tree update by `syncWorkspaceFrontmatter`. */
  storyId?: string;
  storyTitle?: string;
  status: "ready" | "creating" | "error";
  statusError?: string;
  /** SW-XS-10 (SPEC §8.1): the workspace's setup as one typed state — Step 1's
   *  phase and counts, the wait for the first agent, that agent's Step 2, Ready
   *  (only after Step 2, X5), or a failure with its reason. The tile caption,
   *  the tray pill and the screen-reader announcement all render from it.
   *  Replaces the free-text `setupLabel`. Runtime-only (never persisted). */
  setupState?: WorkspaceSetupState;
  /** SW-XS-10 (SPEC §11.2): files the last agent preparation kept because they
   *  were edited here — the persistent "N files kept (modified)" note with
   *  "Discard my edits". Runtime-only (never persisted). */
  keptModified?: KeptModified;
  /** SW-XS-10: each agent's Step 2 in this workspace, by agent (two agents
   *  preparing at once never overwrite each other). The line shows the
   *  busiest; Ready waits for all. Runtime-only (never persisted). */
  agentRuns?: Record<string, AgentRun>;
  /** SLW-CO-6 (AC-3): true when this workspace's in-flight create is cancellable
   *  — the inline `WorkspaceInitOverlay` shows a Cancel button only then. A
   *  story's `setupWorktree` is abortable (set true); an epic's fast dir-ensure
   *  is not (left unset). Data-driven so the overlay needs no per-kind branch. */
  setupCancellable?: boolean;
  /** RF-perf-14 (decouple): non-null while the worktree's `setup:` commands run
   *  (or failed) in the BACKGROUND after the workspace flipped `"ready"`. Drives
   *  a non-blocking badge / retryable warning that overlays the live terminal;
   *  the shell stays mounted throughout. Runtime-only (never persisted). */
  backgroundSetup?: BackgroundSetupState;
  /** SLW-CO-7 (AC-1/AC-5): true when this workspace fires a materialization wave
   *  after its dir is created — so the inline `WorkspaceInitOverlay` OWNS the
   *  whole sync (create → materialize), staying up until the surface wave settles
   *  (not just until the dir exists). Set ONLY on the two materialize-firing
   *  create paths (story `ensureWorktree`, epic open); any on-disk re-hydrate
   *  (already materialized) leaves it unset → no overlay (the
   *  AC-5 opt-out). The overlay-visibility gate reads the per-surface wave state
   *  (`surface-sync-store`) as the "materialized yet?" signal; this latch
   *  disambiguates the pre-wave gap (wave not started yet) from an
   *  already-materialized surface (both `undefined` in the store). Not persisted
   *  — `workspaces` is session state rebuilt from disk, so a fresh session's
   *  rehydrated records are born ready without it. */
  needsSyncOverlay?: boolean;
  /** Pipeline status from story frontmatter (e.g. "in-progress", "review"). */
  storyStatus?: PipelineStatus;
  /** SW-ID-6: assignee login from story frontmatter (`assigned_to`), reconciled
   *  from the pipeline scan alongside `storyStatus`. Drives the story HUD avatar. */
  storyAssignedTo?: string | null;
  /** M.2: resolved on-disk source repo path for cross-repo worktrees.
   *  Used by cleanup/done flows to call `removeWorktree` against the correct
   *  repo. Absent for legacy workspaces created before M.2. */
  sourceRepoPath?: string;
  /** M.3: owner/repo slug from story frontmatter `source_repo` field.
   *  Used by workspace-bar repo badge and status-bar context display.
   *  Absent when no `source_repo` is declared or for legacy workspaces.
   *  R-18: reconciled on every tree update by `syncWorkspaceFrontmatter`. */
  sourceRepoSlug?: string;
  /** Where the workspace lives, when that is a remote machine (set once, at
   *  create). Such a workspace's `cwd` stays empty: its directory is on the
   *  machine (`location.cwd`), never a path on this Mac. */
  location?: WorkspaceLocation;
}

/** Entry in the on-disk worktrees map: worktree path + optional source repo
 *  for cross-repo cleanup (M.2). */
export type OnDiskWorktreeEntry = { path: string; sourceRepoPath?: string };

/**
 * PT-10 — a closed-but-running ("close, keep running") workspace's shadow.
 *
 * Captured at workspace-× close time so the EW panel can render a dimmed row
 * (status dot still lit) and ANY reopen path can restore the workspace + its
 * exact tab list. The PTYs survive in Rust (PT-1) because the workspace-×
 * gesture detaches viewports instead of killing — remount reattaches via
 * `subscribe_terminal` + snapshot replay.
 *
 * Stores the FULL `Workspace` record (not just the tab list) so reopen is
 * generic across `story` / `epic` / `integration` kinds (AC-20) and fully
 * synchronous — no frontmatter round-trip, nothing blocking the UI. `storyId`
 * and `storyFilePath` (AC-2) live on `workspace`.
 *
 * Runtime-only — never persisted (same lifetime as `onDiskWorktrees`).
 * Orthogonal to a dismissed-workspace allow-list: this means "PTYs alive for
 * this workspace this session," not "the user dismissed it."
 */
/** SLW-DV-9: a worktree editor's preserved unsaved-edit draft — the repo-relative
 *  file it belongs to, the edited `buffer`, and the on-disk `lastSaved` baseline
 *  it diverged from. The canonical shape for the `worktreeEditorDraft` slice and
 *  its setter; the editor component imports it (its `buffer`/`lastSaved` are also
 *  structurally the hook's `FileDraft`). */
export interface WorktreeEditorDraft {
  relPath: string;
  buffer: string;
  lastSaved: string;
}

/** A minimized workspace; its terminals keep running in their host. Persisted so a
 * workspace minimized at quit comes back minimized. Global chrome is never saved here. */
export interface SoftClosedWorkspace {
  workspace: Workspace;
}

/**
 * SLW-CO-6 (AC-1/AC-7): parameters for the unified `openWorkspace` primitive —
 * the single sync open path every non-story CLI-workspace kind routes through
 * (epic, generic, and born-minimized shadows). It owns the
 * shared spine: restore-if-soft-closed → focus-if-exists (updating a resolved
 * cwd) → else add the record → switch/expand/spawn; or, when `minimized`, the
 * born-minimized shadow create. The record's `status` is derived from `cwd`
 * (`cwd` present → `"ready"`, empty → `"creating"` so the inline overlay shows
 * until a later open resolves the dir — the epic pre-dir case).
 *
 * The STORY path is intentionally NOT modeled here: its create lifecycle stays
 * in `ensureWorktree` (its hardened async create driver — dedup/abort/epoch
 * guards live there per the SLW-CO-6 validation), and it reuses only the shared
 * `finishForegroundOpen` tail.
 */
export interface OpenWorkspaceParams {
  /** False when a host-owned session already exists; reveal must never spawn a second shell. */
  spawn?: boolean;
  teamKey: string;
  workspaceId: string;
  kind: WorkspaceKind;
  /** Resolved working dir. Empty string => the record is born `"creating"`
   *  (overlay shown) and no shell is spawned until a later open resolves it. */
  cwd: string;
  /** Kind-specific fields merged onto the created/updated record (e.g.
   *  `epicDirPath`, `epicDocPath`, `sourceRepoSlug`). Never overrides
   *  `id`/`kind`/`cwd`/`status`. */
  recordFields?: Partial<Omit<Workspace, "id" | "kind" | "cwd" | "status">>;
  /** Born-minimized: create directly in the soft-close shadow (no xterm mount,
   *  no switch/expand/spawn) — folds the two `ensure*Minimized` variants. */
  minimized?: boolean;
  /** Restore the workspace from its soft-close shadow if minimized (keeps the
   *  running session). Git-sync opts in; epic preserves its no-restore behavior. */
  restoreSoftClosed?: boolean;
  /** SW-FX-69: opt into stealing focus. Unset/false ⇒ additive open — the opened
   *  workspace appears as a NON-focused tile and the current tile keeps focus.
   *  `true` ⇒ jump to it (the pre-SW-FX-69 behavior). An empty mosaic focuses the
   *  opened workspace regardless (empty-mosaic guard). */
  focus?: boolean;
}

export type ContentWidth = "narrow" | "normal" | "wide" | "full";
export type FontSize = "small" | "normal" | "large" | "xl";
export type TreeFontSize = "small" | "normal" | "large";
export type TerminalFontSize = "small" | "normal" | "large" | "xl";
export type TerminalTheme =
  | "match-app"
  | "github-dark"
  | "dracula"
  | "nord"
  | "catppuccin"
  | "solarized-dark";
export type StarshipPreset = "compact" | "minimal" | "full" | "twoline" | "plain";
// SW-GS-17: publish target for the node Publish button. `direct` (default) commits
// the slice and pushes straight to `origin/main` (SW-GS-8); `pr` commits to a `gs/*`
// branch and opens a PR against main instead. Off-by-default — the direct path is
// unchanged when this is `direct`.
export type PublishMode = "direct" | "pr";

// SLW-WD-7: default layout for a freshly-opened worktree source diff — `inline`
// (unified single-column) or `split` (side-by-side). Seeds the per-tile `inline`
// view-option fallback; an explicit per-tile toggle always wins.
export type SourceDiffLayout = "inline" | "split";

export interface ViewerSettings {
  contentWidth: ContentWidth;
  sidebarWidth: number;
  appTheme: AppTheme;
  viewerFontSize: FontSize;
  // Optional custom pixel override for the viewer/scratch font size.
  // 0 or undefined means "use the preset from viewerFontSize".
  viewerFontSizePx?: number;
  treeFontSize: TreeFontSize;
  // Same override pattern for the nav tree font size.
  treeFontSizePx?: number;
  // The bottom workspace bar's size: one factor scales its pills, avatars,
  // badges, controls and height together. "normal" = the original, larger bar.
  workspaceBarSize?: WorkspaceBarSize;
  terminal: string;
  terminalCustomCmd: string;
  editor: string;
  editorCustomCmd: string;
  terminalFontSize: TerminalFontSize;
  // Same override pattern for the xterm font size.
  terminalFontSizePx?: number;
  // SS-14: shared <CodeEditor> font size (D-13). Separate from `editor`/
  // `editorCustomCmd` which configure the EXTERNAL CLI editor (vim/code).
  codeEditorFontSize: FontSize;
  // Same override pattern for the in-app code editor font size.
  codeEditorFontSizePx?: number;

  // SW-VE-8: editor soft line-wrapping policy. "auto" wraps prose (markdown/
  // text) but not code; "on"/"off" force it. The one net-new editor setting.
  editorLineWrap: EditorLineWrap;
  // SW-VE-11: code folding. "auto" folds code / not prose (per-type, inverse of
  // line-wrap's polarity); "on"/"off" force it. Default "auto".
  editorFolding: EditorFolding;
  // SW-VE-11: render tabs/spaces as glyphs in the editor. Default false.
  editorShowWhitespace: boolean;
  // SW-VE-11: indent width (2/4/8) — drives indentUnit + hard-tab render width.
  // Default 2 (codesense content is predominantly 2-space).
  editorTabSize: EditorTabSize;
  // SW-VE-11: use a proportional reading font for PROSE editing (code stays
  // monospace). Default false. iA-Writer-style opt-in.
  editorProseFont: boolean;
  terminalTheme: TerminalTheme;
  // Scrollback buffer lines. 0 = "Maximum" (bounded to MAX_SCROLLBACK, NOT
  // infinite — SW-FX-64). Written only when the user picks a value; undefined
  // means "use the default" (DEFAULT_SCROLLBACK). Resolved via `scrollbackLines`.
  terminalScrollback?: number;
  starshipPreset: StarshipPreset;
  // SW-LN-35: shell override for new terminals (absolute path to zsh/bash/fish).
  // Optional — undefined/empty means "use the detected login shell".
  terminalShell?: string;
  // PT-5: shell integration (OSC 7 cwd capture + OSC 133 prompt markers +
  // inline prompt). Optional — undefined means "use default (on)".
  shellIntegration?: boolean;
  // PT-8: number of pre-spawned shells kept warm so new terminals open
  // instantly. Range 0–5; 0 disables. Optional — undefined means default (2).
  terminalWarmPoolSize?: number;
  // SW-AU-34: when an agent should be able to keep driving codesense with
  // no window on screen, closing the window hides the app (macOS dock-resident) and
  // keeps the MCP host + engine alive instead of quitting. Turn this on to make a
  // window close quit the process instead. Pushed to Rust via `useQuitOnCloseSetting`
  // → `setQuitOnClose`, read on the window-close path. Optional — undefined reads as
  // `false` (background-on-close, the shipped default).
  quitOnClose?: boolean;
  // SV-8: show this instance's terminal server in the macOS menu bar (status,
  // Open, Restart, Stop/Start, Quit). Pushed to Rust via
  // `useTerminalServerMenuBarSetting` → `setTerminalServerMenuBar`. Optional —
  // undefined reads as `false` (off, the shipped default).
  terminalServerMenuBar?: boolean;
  // OS notifications when a terminal agent needs you (a permission prompt, a
  // question, a crash) while the window is in the background. The dock badge
  // counts those tabs either way. Optional — undefined reads as `true` (on).
  osNotifications?: boolean;
  // An OS notification when an agent finishes a turn of 30s or more that the user
  // was away for (window in the background, or its tab never in view). Optional —
  // undefined reads as `true` (on).
  notifyOnFinish?: boolean;
  // Whether OS notifications play the system notification sound: "needs-you" for
  // questions, permission prompts, crashes and errors; "all" also for finished
  // agents. Optional — undefined reads as "off".
  notificationSound?: "off" | "needs-you" | "all";
  // SLW-TL-4: OS/push notification when a NEW Inbox item (an agent/CLI/trigger post)
  // arrives while the window is in the background. OPT-IN — undefined reads as
  // `false` (off). The nav badge + the reveal are the always-on floor; this is the
  // escape for an away-from-desk human, so it never fires unless explicitly turned on.
  inboxNotifications?: boolean;
  // SW-AU-34: one-time "the app is still running in the background" explainer gate.
  // Flipped to `true` the first time a close is handled as background so the toast
  // shows exactly once. Optional — undefined reads as "not yet seen".
  hasSeenBackgroundExplainer?: boolean;
  // SW-EW-32: ordered list of section headings the summary extraction keys off
  // (first match wins), applied to the nav story cards and the story HUD. A story doc (no `## Purpose`)
  // falls through to `## Story`; an epic hits `## Purpose` first — one list
  // covers both. Optional — undefined means default (`["Purpose", "Story"]`).
  summaryHeadings?: string[];
  // SW-GS-17: node Publish target. `direct` (default) commits + pushes straight to
  // origin/main (SW-GS-8); `pr` commits to a `gs/*` branch and opens a PR against
  // main. Optional — undefined means default (`direct`).
  publishMode?: PublishMode;
  // SLW-WD-7: default layout for a freshly-opened source diff. Seeds the
  // per-tile `inline` view-option fallback (an explicit per-tile toggle still
  // wins). Optional — undefined means default (`inline`).
  defaultSourceDiffLayout?: SourceDiffLayout;
  // SW-SD-2: override for the account/CLI-agnostic agent runner behind the
  // `analyze` bridge verb. RESERVED surface — the field + read path exist so a
  // user can retune model/timeout without a rebuild, but the backend consumes the
  // built-in Claude defaults today (wiring this through to Rust is a follow-up).
  // Optional — undefined means the built-in default (claude-opus-4-6, 60s).
  agentRunner?: { model?: string; timeoutSecs?: number };
  // RF-perf-14d → RF-perf-23: how many worktree `setup:` runs install in parallel
  // in the BACKGROUND after a worktree opens. `"auto"` (default) sizes to the
  // machine's core count via `resolveBackgroundSetupCap`; `"off"` = no cap (all at
  // once); an explicit number is a manual override (any integer ≥ 1 — presets or a
  // custom value). Optional — undefined/corrupted reads as `"auto"` (the resolver
  // fails safe). Applied live by `use-background-setup-cap`.
  worktreeSetupParallelism?: "auto" | "off" | number;
  // SW-DX-17d (AC-3): the user-owned ceiling on UNATTENDED background agents
  // (foreground/adopted don't count). `"auto"` (default) = an attention-based
  // constant (NOT core-sized — this is about how many agents a person can watch);
  // an explicit number is a manual override. Optional — undefined/corrupted reads
  // as `"auto"`. Pushed to Rust live by `use-background-agent-limit`.
  backgroundAgentLimit?: "auto" | number;
  // SW-CW-5: when ON, activating an epic/story workspace focuses that workspace's
  // own doc (epic README / story file) in the viewer. Default OFF — opt-in, since
  // it changes what the viewer shows on every workspace switch. Optional —
  // undefined reads as `false`. Consumed by `useFocusActiveWorkspaceDoc`.
  focusWorkspaceDocOnActivation?: boolean;
}

/**
 * SLW-WD-7: resolve the effective "is inline?" default for a source-diff tile
 * with no explicit per-tile choice. `inline` is the shipped default; a user who
 * sets the layout to `split` in Settings flips every unset tile to side-by-side.
 * One definition shared by the overlay read and the toggle reconcile (RD-1/RD-3)
 * so "what the setting means as a boolean" lives in exactly one place.
 *
 * Fail-safe to the documented default: only an explicit `"split"` yields
 * side-by-side; unset OR any unrecognized/corrupted value falls to `inline`
 * (a corrupted persisted blob must not silently open every diff side-by-side).
 */
export function defaultSourceDiffInline(
  settings: Pick<ViewerSettings, "defaultSourceDiffLayout">,
): boolean {
  return settings.defaultSourceDiffLayout !== "split";
}

/**
 * The per-tile source-diff VIEW options — the small set of boolean toggles a
 * user flips per workspace from the diff's view-options surface. One keyed
 * record backs all of them so the state/setter/teardown/default plumbing stays
 * one authoritative shape and can't drift out of sync. Adding an option means
 * extending this union (+ one `resolveSourceDiffOption` arm if its default isn't
 * OFF, + wiring the read/toggle in the overlay) — never a new slice quartet.
 *
 * Per-option origin + absent-value rule (owned by `resolveSourceDiffOption`, an
 * explicit per-tile boolean always wins):
 * - `inline`     — SLW-WD-6/WD-7: tri-state, settings-coupled (absent = follow
 *                  the global `defaultSourceDiffLayout` via `defaultSourceDiffInline`).
 * - `autoFollow` — SLW-DV-5 (AC-4): defaults ON (absent = follow-the-edit).
 * - `wordWrap`   — SLW-DV-6 (AC-6): defaults OFF.
 * - `expandAll`  — SLW-DV-6 (AC-4): defaults OFF.
 *
 * NOTE: ignore-whitespace is NOT a peer option here — SLW-DV-8b (RD-3) models it
 * as one value of the mutually-exclusive `sourceDiffMode` enum (line /
 * ignoreWhitespace / structural), since "ignore-whitespace AND structural" is a
 * contradiction (structural already ignores whitespace).
 */
/** `fileTree` groups the rail by folder; unset means the rail picks by the
 *  change set's shape (see `prefersTree`), so callers read it raw. */
export type SourceDiffViewOption = "inline" | "wordWrap" | "expandAll" | "autoFollow" | "fileTree";
export type SourceDiffViewOptions = Partial<Record<SourceDiffViewOption, boolean>>;

export function resolveSourceDiffOption(
  opts: SourceDiffViewOptions | undefined,
  key: SourceDiffViewOption,
  settings: Pick<ViewerSettings, "defaultSourceDiffLayout">,
): boolean {
  const explicit = opts?.[key];
  if (explicit !== undefined) return explicit;
  switch (key) {
    case "inline":
      return defaultSourceDiffInline(settings);
    case "autoFollow":
      return true;
    default:
      return false; // wordWrap, expandAll, fileTree
  }
}

/**
 * SW-EW-32: default summary-section headings, tried in order (first match
 * wins). Exported as a module constant so consumers can use it as a stable
 * fallback (`settings.summaryHeadings ?? DEFAULT_SUMMARY_HEADINGS`) without
 * minting a new array reference per render. Mirrors the Rust
 * `DEFAULT_SUMMARY_HEADINGS` in `pipeline_scanner.rs`.
 */
export const DEFAULT_SUMMARY_HEADINGS: readonly string[] = ["Purpose", "Story"];

const DEFAULT_SETTINGS: ViewerSettings = {
  contentWidth: "normal",
  sidebarWidth: 240,
  appTheme: DEFAULT_APP_THEME,
  viewerFontSize: "large",
  viewerFontSizePx: 0,
  treeFontSize: "large",
  treeFontSizePx: 0,
  workspaceBarSize: DEFAULT_WORKSPACE_BAR_SIZE,
  terminal: "",
  terminalCustomCmd: "",
  editor: "",
  editorCustomCmd: "",
  terminalFontSize: "large",
  terminalFontSizePx: 0,
  codeEditorFontSize: "normal",
  codeEditorFontSizePx: 0,

  editorLineWrap: DEFAULT_EDITOR_LINE_WRAP,
  editorFolding: DEFAULT_EDITOR_FOLDING,
  editorShowWhitespace: false,
  editorTabSize: DEFAULT_EDITOR_TAB_SIZE,
  editorProseFont: false,
  terminalTheme: "match-app",
  // SW-LN-35: new installs keep the user's own prompt; the Starship presets are
  // opt-in. Existing installs keep their saved preset (the persisted settings
  // are merged over these defaults).
  starshipPreset: "plain",
  shellIntegration: true,
  // SW-EW-46: warm MORE by default so gridding/opening several terminals at once (a
  // 4–8 cell grid, or a multi-workspace cold boot) serves them all from the pool
  // (~0ms) instead of cold-booting some (~750ms) one-by-one. 8 idle members is a modest
  // footprint and refills async off the request path.
  terminalWarmPoolSize: 8,
  quitOnClose: false,
  terminalServerMenuBar: false,
  hasSeenBackgroundExplainer: false,
  summaryHeadings: [...DEFAULT_SUMMARY_HEADINGS],
  publishMode: "direct",
  defaultSourceDiffLayout: "inline",
  worktreeSetupParallelism: "auto",
  backgroundAgentLimit: "auto",
  focusWorkspaceDocOnActivation: false,
};

export type { LayoutMode } from "./cli-chrome";
export { DEFAULT_TERMINAL_SPLIT_RATIO };

/** SW-EW-3: the split-terminal tiling arrangement for one team's foreground.
 *  Membership (which workspaces are tiled) is the open-workspace set — derived,
 *  not stored (D-1); the focused tile is `activeWorkspace` — reused, not stored
 *  (D-3). Only the *arrangement* lives here so it survives a restart (AC-9). */
export interface ForegroundLayout {
  /** `react-resizable-panels` percent sizes, parallel to the rendered tile
   *  order. Empty = even split (the read site reconciles length to the live
   *  tile count, so add/remove never strands a stale size — R-3). */
  sizes: number[];
  /** Tiling axis. Flat 1-D for SW-EW-3 (`row` = side-by-side columns,
   *  `column` = stacked rows); nesting both axes is a later story. */
  direction: "row" | "column";
  /** SW-EW-4 AC-2: layout-switcher preset. `columns`/`rows` map to `direction`
   *  (row/column) for the flat group; `grid` nests the group (a column of
   *  row-groups). SW-EW-17: `custom` = a user-arranged 2D mosaic (set by a
   *  directional drag-split; `root` is then authoritative). Additive — absent →
   *  derived from `direction`. */
  preset: "columns" | "rows" | "grid" | "custom";
  /** SW-EW-17: the 2D mosaic arrangement tree, or null to DERIVE a depth-1
   *  arrangement from `preset`/`direction`/`sizes` at render (columns/rows/grid).
   *  Once the user directionally drag-splits, this holds the authoritative custom
   *  mosaic. Persisted; reconciled to the live tile set on read. */
  root: MosaicNode | null;
}

/** Fill a (possibly absent) persisted `ForegroundLayout` with defaults so the
 *  setters can patch a single field without clobbering siblings or assuming the
 *  team has an entry yet. */
export function defaultForegroundLayout(partial?: Partial<ForegroundLayout>): ForegroundLayout {
  const direction = partial?.direction ?? "row";
  return {
    sizes: partial?.sizes ?? [],
    direction,
    // Keep preset/direction consistent for legacy entries that predate `preset`.
    preset: partial?.preset ?? (direction === "column" ? "rows" : "columns"),
    // SW-EW-17: absent → null (derive depth-1 from preset/direction at render).
    root: partial?.root ?? null,
  };
}

/** SW-CW-7: the pickable-preset ring the hot-key cycles through, in order. The
 *  `as const` element type is the exact `setForegroundPreset` argument union, so
 *  `cycleForegroundPreset` stays type-safe without a second copy of the literals.
 *  `custom` is intentionally absent — it is not a pickable preset, so it falls
 *  off the ring (indexOf → -1) and the next step restarts at `columns`. */
const PRESET_RING = ["columns", "rows", "grid"] as const;

/** SW-EW-46: the pickable-preset union shared by the workspace mosaic and the
 *  tab-grid, sourced from `PRESET_RING` so there is exactly one layout vocabulary
 *  (no second enum). Mirrors `setForegroundPreset`'s inline argument type. */
export type LayoutPreset = (typeof PRESET_RING)[number];

/** SW-EW-46: step a preset one place around `PRESET_RING`. An ABSENT slot reads as
 *  `columns` (→ `rows`), matching the pre-refactor `cycleForegroundPreset`; `custom`
 *  falls off the ring (indexOf -1) and restarts at `columns`. Shared by
 *  `cycleForegroundPreset` (mosaic) and `cycleTabGridPreset` (tab-grid) so the ring
 *  math lives in one place. */
function nextPresetInRing(current: LayoutPreset | "custom" | undefined): LayoutPreset {
  const i = PRESET_RING.indexOf((current ?? "columns") as LayoutPreset);
  return PRESET_RING[(i + 1) % PRESET_RING.length];
}

/** SW-EW-17: the single source of the effective mosaic tree for one team —
 *  derives a depth-1 arrangement from the preset (columns/rows → flat axis,
 *  grid → balanced ⌈√N⌉) when no custom `root` is stored, then reconciles to the
 *  live tile set. Both the store (placement) and the renderer use this so they
 *  can never disagree on the arrangement. */
export function effectiveMosaicRoot(
  layout: ForegroundLayout | undefined,
  orderedWorkspaceIds: string[],
): MosaicNode | null {
  const base =
    layout?.root != null
      ? layout.root
      : layout?.preset === "grid"
        ? buildBalancedGrid(orderedWorkspaceIds)
        : seedTreeFromFlat(orderedWorkspaceIds, layout?.direction ?? "row", layout?.sizes);
  return reconcileMosaic(base, orderedWorkspaceIds);
}

// ---------------------------------------------------------------------------
// SW-EW-40: per-epic foreground "slot" resolution.
//
// The foreground mosaic is per-team in flat mode (`foregroundLayout[teamKey]`)
// and per-(team, rail) in grouped mode (`foregroundLayoutByRail[teamKey][rail]`).
// These helpers are the ONE place that branches on tray mode, so every setter
// and reader (mosaic, CLI chrome, empty-state gate, zoom shortcut) shares one
// resolution and flat math stays byte-identical. Membership is DERIVED, never
// stored: flat → `foregroundTilesFor`; grouped → `epicForegroundTiles` filtered
// through the store-cached `railKeyByWorkspaceId` (the SAME grouping the tray
// uses), so the mosaic and tray can never disagree.
// ---------------------------------------------------------------------------

/** The structural subset of state the foreground-slot resolvers read (so they
 *  are unit-testable with a partial state and cheap in selectors). */
type ForegroundSlotState = Pick<
  ViewerState,
  | "workspaces"
  | "trayMode"
  | "selectedEpicDirName"
  | "railKeyByWorkspaceId"
  | "foregroundLayout"
  | "foregroundLayoutByRail"
  | "trayBaseOrderByTeam"
  | "softClosedWorkspaces"
  | "workspaceOrderByTeam"
>;

/** Per team, the moves made before the team's computed order was known (its tree never synced
 *  and no base was saved), with the stored order they started from. Each was applied to the
 *  tree-less order the person saw; once the tree syncs they are made again over the real base,
 *  so the move holds and every other workspace keeps its epic's place. Runtime-only: a restart
 *  (or a profile switch, which reloads the window) before the sync keeps the stored order as the
 *  person last saw it, and loading the saved state drops what was queued. */
const provisionalMoves = new Map<
  string,
  { from: string[] | undefined; moves: { workspaceId: string; place: OrderPlace }[] }
>();

/** Per team, the last order `teamTileOrder` computed and the slices it came from. A team's entry
 *  goes with the team (`closeTeam`); every entry goes when the saved state loads. */

const tileOrderCache = new Map<string, { source: TeamOrderSource; order: readonly string[] }>();

/** The team's shared workspace order (its pills and Agents-pane cards): the
 *  foreground tiles follow it, so the store's placement and the mosaic agree.
 *  Read by per-render selectors, so it is computed again only when one of the
 *  team's slices it reads changes; the array is shared, never to be mutated. */
export function teamTileOrder(state: ForegroundSlotState, teamKey: string): readonly string[] {
  const source: TeamOrderSource = {
    base: state.trayBaseOrderByTeam[teamKey],
    workspaces: state.workspaces[teamKey],
    softClosed: state.softClosedWorkspaces[teamKey],
    stored: state.workspaceOrderByTeam[teamKey],
    groups: state.railKeyByWorkspaceId[teamKey],
  };
  const hit = tileOrderCache.get(teamKey);
  if (
    hit &&
    hit.source.base === source.base &&
    hit.source.workspaces === source.workspaces &&
    hit.source.softClosed === source.softClosed &&
    hit.source.stored === source.stored &&
    hit.source.groups === source.groups
  ) {
    return hit.order;
  }
  const order = teamWorkspaceOrder(source);
  tileOrderCache.set(teamKey, { source, order });
  return order;
}

/** One resolved foreground slot: the layout to read, the ordered live tile ids to
 *  reconcile against, and the write-back patch for the current mode+rail. */
export interface ForegroundSlot {
  layout: ForegroundLayout | undefined;
  tileIds: string[];
  write(next: ForegroundLayout): Partial<ViewerState>;
}

/** The active rail key for the current mode: `null` in flat mode; else the
 *  selected epic rail key (`selectedEpicDirName`, defaulting to "" = Ungrouped).
 *  The tray's follow-focus keeps `selectedEpicDirName` resolved (its AC-8 first-
 *  pill fallback is written back), so readers just consume it — one source. */
export function activeRailKey(state: ForegroundSlotState, teamKey: string): string | null {
  return state.trayMode[teamKey] === "grouped"
    ? readTeamKeyed(state.selectedEpicDirName, teamKey, "")
    : null;
}

/** The mode-aware foreground tile workspaces for a team: flat → all foreground
 *  tiles; grouped → the selected rail's tiles. Shared by the mosaic read-site,
 *  the empty-state gate, the CLI chrome, and the zoom shortcut. */
export function activeForegroundTiles(state: ForegroundSlotState, teamKey: string): Workspace[] {
  const workspaces = readTeamKeyed(state.workspaces, teamKey, []);
  const railKey = activeRailKey(state, teamKey);
  return scopedForegroundTiles(
    workspaces,
    railKey !== null,
    state.railKeyByWorkspaceId[teamKey],
    railKey ?? "",
    teamTileOrder(state, teamKey),
  );
}

/** The mode-aware foreground layout for a team: flat → `foregroundLayout`;
 *  grouped → the selected rail's `foregroundLayoutByRail` slot. */
export function activeForegroundLayout(
  state: ForegroundSlotState,
  teamKey: string,
): ForegroundLayout | undefined {
  const railKey = activeRailKey(state, teamKey);
  return railKey === null
    ? state.foregroundLayout[teamKey]
    : state.foregroundLayoutByRail[teamKey]?.[railKey];
}

/** The foreground slot for an EXPLICIT rail (`null` = the flat per-team slot).
 *  Used by the setters (active rail) and the close path (the closing workspace's
 *  rail). `write` closes over `state`, so call it within the same `set((s) => …)`
 *  that produced `state === s`. */
function foregroundSlotForRail(
  state: ForegroundSlotState,
  teamKey: string,
  railKey: string | null,
): ForegroundSlot {
  const workspaces = readTeamKeyed(state.workspaces, teamKey, []);
  if (railKey === null) {
    return {
      layout: state.foregroundLayout[teamKey],
      tileIds: foregroundTilesFor(workspaces, teamTileOrder(state, teamKey)).map((w) => w.id),
      write: (next) => ({
        foregroundLayout: updateTeamKeyed(state.foregroundLayout, teamKey, next),
      }),
    };
  }
  return {
    layout: state.foregroundLayoutByRail[teamKey]?.[railKey],
    tileIds: epicForegroundTiles(
      workspaces,
      state.railKeyByWorkspaceId[teamKey],
      railKey,
      teamTileOrder(state, teamKey),
    ).map((w) => w.id),
    write: (next) => ({
      foregroundLayoutByRail: {
        ...state.foregroundLayoutByRail,
        [teamKey]: { ...readTeamKeyed(state.foregroundLayoutByRail, teamKey, {}), [railKey]: next },
      },
    }),
  };
}

/** The active foreground slot for the current mode + selected rail — the seam
 *  every foreground setter routes through. */
export function activeForegroundSlot(state: ForegroundSlotState, teamKey: string): ForegroundSlot {
  return foregroundSlotForRail(state, teamKey, activeRailKey(state, teamKey));
}

/** SW-EW-40: rebuild the per-team rail-key index from the SAME grouping the tray
 *  uses (`trayWorkspaceOrder`+`railKeyOf`), covering the open ∪ soft-closed union
 *  of foreground-tile kinds exactly like the tray. Pure — the store caches the
 *  result in `railKeyByWorkspaceId` on every tree change. */
export function buildRailKeyIndex(
  workspaces: readonly Workspace[],
  softClosed: Readonly<Record<string, SoftClosedWorkspace>>,
  tree: DerivedPipelineTree | null,
  groups: readonly WorkspaceGroup[] = [],
): Record<string, string> {
  return buildWorkspaceGrouping(workspaces, softClosed, tree, groups).railKeyIndex;
}

/** Shallow key/value equality for two string→string records (index no-op guard
 *  so the index rebuild on every tree tick doesn't churn subscribers). */
function sameStringRecord(
  a: Record<string, string> | undefined,
  b: Record<string, string>,
): boolean {
  if (a === undefined) return false;
  const ak = Object.keys(a);
  if (ak.length !== Object.keys(b).length) return false;
  return ak.every((k) => a[k] === b[k]);
}

/** SLW-DV-7 (AC-2): the working-diff comparison basis — `"branchBase"` (the
 *  default: merge-base against the fork parent, i.e. all work on the branch) or
 *  `"head"` (baseRef `"HEAD"`, i.e. uncommitted work only). Both are tree-ish
 *  bases the shipped `source_worktree_changeset` already supports — no new git. */
export type SourceDiffBaseMode = "branchBase" | "head";

/** SLW-DV-8b (RD-3): the code-diff comparison mode for a tile — one discriminant
 *  instead of stacked booleans that could contradict each other (structural
 *  already ignores whitespace). `"line"` is the default universal diff;
 *  `"ignoreWhitespace"` suppresses whitespace-only churn (DV-8's synchronous
 *  override); `"structural"` diffs the parse tree off-thread (DV-8b, supported
 *  languages only, else it degrades to the line diff). Runtime-only, per tile. */
export type SourceDiffMode = "line" | "ignoreWhitespace" | "structural";

/** The agents pane's search text and Show filter. */
export interface AgentsPaneSearch {
  query: string;
  status: "all" | "working" | "needs" | "idle";
}

/** A place in the team and workspace history: a team and the workspace focused in it then. */
export interface ContextEntry {
  teamKey: string;
  workspaceId: string | null;
}

/** The team and workspace history ⌘[ and ⌘] walk: the places left by a switch (`back`, the
 *  latest last) and the places left by going back (`forward`, the latest last). */
export interface ContextHistory {
  back: readonly ContextEntry[];
  forward: readonly ContextEntry[];
}

export const EMPTY_CONTEXT_HISTORY: ContextHistory = { back: [], forward: [] };

export type FileHashRequest = { path: string; hash: string };

export interface ViewerState {
  /** T.4: per-team open file tabs. Outer key = `teamKey`. */
  openTabs: Record<string, TabState[]>;
  /** T.4: per-team active file tab path. Outer key = `teamKey`. */
  activeTab: Record<string, string>;
  /** SW-VE-3: paths with an UNSAVED deliberate edit, per team. Transient (never
   *  persisted). `closeTab` reads it so closing the tab of a dirty edit
   *  re-activates it (surfacing the editor) rather than silently dropping the
   *  buffer — "never drop edits" (README §Guardrails). Multiple tabs can be
   *  edited concurrently, so this is a set (per team).
   *  NB: deliberately NOT in `TEAM_KEYED_SLICES` — it is cleared by the
   *  `TeamWorkspaceView` unmount effect, not the `closeTeam` sweep, so a
   *  non-empty entry can never outlive its mounted view. RF-perf-10 relies on
   *  this to gate both the render-gate unmount and F-19 eviction ("never drop
   *  edits" on ANY teardown path); keep the clear-on-unmount, don't fold it
   *  into `TEAM_KEYED_SLICES`. */
  editingDirtyPaths: Record<string, string[]>;
  /** SW-VE-8: a request (from the `viewer.edit-file` shortcut / File menu) to
   *  enter edit on the active file of `teamKey`. The registry `run()` drives
   *  STORE state (it can't reach the view's component ref), and the team view
   *  reacts — bumping `nonce` re-fires even for the same file. Transient. */
  editFileRequest: { nonce: number; teamKey: string } | null;
  /** Open tabs showing their Changes instead of the file: against GitHub (`true`), or one
   *  commit's change to it (opened from History). Outer key = `teamKey`, inner = the tab's
   *  path. A tab opened from the Changes or History view starts on Changes; the tab's
   *  File | Changes switch flips it. In-memory only. */
  tabDiff: Record<string, Record<string, TabDiff>>;
  /** RF-perf-12: session-scoped view-POSITION snapshots (scroll + editor
   *  cursor/selection), keyed by `${teamKey}::nav` (tree scroll) or
   *  `${teamKey}::${filePath}` (markdown / editor). Captured on component
   *  UNMOUNT, restored on remount, so RF-perf-10's team-unmount / file-eviction
   *  no longer resets scroll & cursor to the top.
   *
   *  IN-MEMORY ONLY — deliberately NOT in `TEAM_KEYED_SLICES` and NOT written
   *  into `persistState()`'s `existing.viewer` literal: it restores parity with
   *  the pre-unmount MOUNTED experience without disk-write amplification on
   *  every background (position already dies on app restart today). Stores PLAIN
   *  NUMBERS only (never a CmEditorState / doc text / undo history) so it cannot
   *  re-pin the renderer memory RF-perf-10 unmounts to reclaim (AC-8). GC'd via
   *  `pruneViewStateForTeam` on `closeTeam` and per-file on `closeTab` (AC-5). */
  viewStateByKey: Record<string, ViewSnapshot>;
  /** T.4: per-team nav drill root. Outer key = `teamKey`. */
  drillPaths: Record<string, string>;
  /** SLW-FX-14: per-team cold-open pipeline-key hint. Outer key = `teamKey`,
   *  value = the org-derived team-pipeline synthetic key
   *  (`resolveTeamPipelineRepoPath`, SW-FX-37's `source_repos[0]` shape).
   *  Written through on every successful resolve; read on cold open (before
   *  the unpersisted `orgs`/`workspaceRepos` populate) so the epic's nav/tree
   *  key is stable across org-discovery — no re-skeleton flash when orgs land.
   *  Self-healing: a stale/missing value degrades to today's post-discovery
   *  behavior. Persisted + pruned + rename-rekeyed like its teamKey siblings. */
  teamPipelineRepoPath: Record<string, string>;
  /** Whether the CLI pane is on screen. D39: the CLI chrome (this, `layoutMode`,
   *  `lastTerminalMode`, `terminalSplitRatio`, `cliExpanded` and the Agents dock's
   *  `workspacePanelCollapsed` and `workspacePanelWidth`) is one setting for every team,
   *  saved as one `cliChrome` value (see `cli-chrome.ts`). */
  terminalVisible: boolean;
  /** Expanded: the focused workspace's tile fills the CLI (D68). One setting for every team;
   *  the tile it zooms is always the active team's focused workspace
   *  (`selectExpandedWorkspaceId`). */
  cliExpanded: boolean;
  /** SW-FX-8: per-team left-nav collapsed state. Outer key = `teamKey`.
   *  Absent/false = expanded. */
  navCollapsed: Record<string, boolean>;
  /** The Agents dock beside the CLI is folded away (one setting for every team). */
  workspacePanelCollapsed: boolean;
  /** The agents pane's folded team and workspace headers, by fold key (`teamFoldKey`,
   *  `workspaceFoldKey`, which name their team). One map for the whole pane, which lists every
   *  team: a fold survives a team switch. Only folds are stored; absent = open. A mirror of
   *  `ui:dock.folds`, which owns and saves them (agent-list-inputs). */
  agentsPaneFolds: Record<string, boolean>;
  /** Teams the agents pane hides (Teams checklist). Persisted; empty = every team shows. */
  agentsPaneHiddenTeams: string[];
  /** What the pane hid before a team header's funnel narrowed it to that team, so the funnel
   *  turns off to exactly that. Not persisted: without it, turning the funnel off shows every team. */
  agentsPaneTeamFocusRestore: TeamFocusRestore | null;
  /** The pane's search and Show filter. One value for every team's list, so a team switch keeps it. */
  agentsPaneSearch: AgentsPaneSearch;
  setAgentsPaneSearch: (patch: Partial<AgentsPaneSearch>) => void;
  /** The global, persisted arrangement: workspace cards, manager trees, or status sections. */
  agentsPaneArrange: "teams" | "status" | "manager";
  setAgentsPaneArrange: (arrange: "teams" | "status" | "manager") => void;
  /** The arrow filter: one workspace the agents pane shows alone. Persisted; it never
   *  changes which workspace is focused. `machineId` is the remote machine the workspace
   *  runs on, stamped when the filter is set, so a filter whose workspace has not arrived
   *  yet knows which machine's layout can still bring it. */
  agentsWorkspaceFilter: AgentsWorkspaceFilter | null;
  /** One explicit workspace reveal in the open pane; never persisted. */
  agentsPaneViewport: { offset: number; measurements: VirtualItem[] };
  rememberAgentsPaneViewport: (offset: number, measurements: VirtualItem[]) => void;
  agentsWorkspaceReveal: { teamKey: string; workspaceId: string; nonce: number } | null;
  /** The team and workspace history (not persisted; see `context-switch.ts`). */
  contextHistory: ContextHistory;
  /** Per-team agent looks a person chose: `teamKey` → (session handle → role and face).
   *  Display only; absent = Auto. */
  agentIdentity: Record<string, Record<string, AgentIdentityChoice>>;
  /** Per-team agents a person pinned to the top of the agents list, in the order pinned.
   *  Per person (never team-shared); absent = none. */
  agentPins: Record<string, AgentPin[]>;
  /** The Agents dock's dragged width in px (clamped by its divider); null for its default.
   *  One setting for every team. */
  workspacePanelWidth: number | null;
  /** SW-EW-3 AC-9: per-team split-terminal tiling ARRANGEMENT (the
   *  `react-resizable-panels` layout). Foreground *membership* is the open-
   *  workspace set (derived, not stored — D-1) and the *focused* tile reuses
   *  `activeWorkspace` (D-3); this slice persists only the arrangement so it
   *  restores on reopen. New additive field — absent → {} (no migration),
   *  mirroring `workspacePanelCollapsed`. Outer key = `teamKey`. */
  foregroundLayout: Record<string, ForegroundLayout>;
  /** SW-EW-40: per-team, PER-RAIL split-terminal arrangement — used ONLY in
   *  grouped tray mode so each epic (and roots / Ungrouped) is its own foreground
   *  context. Outer key = `teamKey`, inner key = rail key (`ROOTS_RAIL_KEY`, an
   *  epic `dirName`, or "" for Ungrouped). Membership per rail is DERIVED
   *  (`epicForegroundTiles`), reconciled to the live set on read like the flat
   *  slice; only the arrangement persists. Additive parallel slice — flat's
   *  `foregroundLayout` is left byte-identical (no nested-unify migration).
   *  Absent → {} (no migration). Outer key = `teamKey`. */
  foregroundLayoutByRail: Record<string, Record<string, ForegroundLayout>>;
  /** SW-EW-18 AC-3 (SW-EW-15): epic-repo-config overlay open state, keyed by
   *  workspace id. Lifted out of the team-view hook into the store so the
   *  bottom-bar terminal zone (global `status-bar.tsx`) can toggle it for the
   *  active epic-integration workspace. Transient — not persisted. */
  epicReposOpen: Record<string, boolean>;
  /** The CLI's console/shared/viewer layout (D39: global); null until the first team
   *  view settles it (`settleInitialLayout`). */
  layoutMode: LayoutMode | null;
  /** The CLI's last non-viewer layout, which a reopen restores (D39: global). */
  lastTerminalMode: "console" | "shared";
  /** SW-SN-1: the ACTIVE note's markdown content (plural notes model). Bodies
   *  live in per-note files (`notes/{id}.md`); this mirrors the active one and
   *  is NOT persisted to appstate — it's re-derived from the file on load. */
  scratchNoteContent: string;
  /** SW-SN-1: metadata for all notes (bodies excluded) — the source for the
   *  rail list + switcher. Refreshed by `loadNotes` and the CRUD actions. */
  notes: NoteMeta[];
  /** SW-SN-1: id of the note shown in the editor, or null before the first
   *  `loadNotes`. Body writes/saves target this id. */
  activeNoteId: string | null;
  /** SW-SN-6: the active note's live heading list, PUBLISHED by the scratch
   *  panel from its `useNoteHeadings` result (SN-5's single source — never
   *  re-enumerated). A read-model so the go-to-heading modal (mounted in the team
   *  view, without the editor) can list headings without reaching into the panel.
   *  Ephemeral (NOT persisted); empty before the panel first mounts. */
  activeNoteHeadings: Heading[];
  /** SW-SN-6: one-shot signal for a go-to-heading jump. The modal (no editor)
   *  sets it via `requestHeadingJump`; the panel (has the editor) consumes it,
   *  scrolls, and clears it. `noteId` binds the jump to the note it was requested
   *  for, so a concurrent note switch discards a stale jump instead of scrolling
   *  the wrong note (or re-firing on a colliding slug). `nonce` lets a repeat jump
   *  to the same slug re-fire. Ephemeral. */
  pendingHeadingJump: { slug: string; noteId: string | null; nonce: number } | null;
  /** Global right-pane visibility — the pane hosting the Inbox, notes and
   *  custom panes. Persisted (migrated from `scratchNoteOpen`). */
  rightPaneOpen: boolean;
  /** The bottom agents list shows only the sessions of this named agent (any revision)
   *  while set: the Agents tab's "N running" link sets it, the list's chip clears it.
   *  Ephemeral (never persisted). */
  agentsListFilter: { agentKey: string; name: string } | null;
  /** Global right-pane width ratio (0-1). Persisted (migrated from the legacy
   *  `scratchNoteWidth`/`scratchNoteRatio`). */
  rightPaneRatio: number;
  /** SW-FX-66 (D2.1): monotonic counter bumped every time a worktree
   *  story-link is (re)established. The workspace-sync hook watches it to arm a
   *  bounded post-establish "verify beat" — establish enqueues the derive and
   *  returns, so if that wave is lost/superseded before it materializes AND the
   *  user never focuses/opens the worktree (a burst-created, unopened sibling),
   *  a single stuck-sweep fires ~one watchdog window later to heal it without a
   *  restart. NOT persisted — a session-only heal signal. */
  worktreeEstablishTick: number;
  /** R5B-P2: tracks the outcome of `loadScratchNote`. Used by
   *  `toggleRightPane` to retry the notes load on first open when the startup
   *  load failed transiently. NOT persisted — only meaningful within a session. */
  scratchNoteLoadStatus: "pending" | "loaded" | "error";
  /** SW-SN-13 AC-4: gate so the one-shot empty-duplicate-"Scratch" reap runs at
   *  most ONCE per session (on the first `loadNotes`), never on an SN-7 hot-reload
   *  refresh — which would otherwise police a note the user just emptied. NOT
   *  persisted — session-internal. */
  emptyScratchReapDone: boolean;
  /** SLW-PT-19: the scope (profile + org) the loaded notes belong to. Every write
   *  to a loaded note uses it — never the CURRENT org — so an edit typed after an
   *  org switch but before the reload lands saves into the note's own org. NOT
   *  persisted — session-internal. */
  notesLoadedScope: NotesScope | null;
  /** T.4: per-team right-pane drag ratio. Outer key = `teamKey`. */
  rightPaneSplitRatio: Record<string, number>;
  /** SW-RP-1: the structured right-pane layout — ordered sections → tab-groups,
   *  global (matches today's right-pane scalars). Supersedes the
   *  `rightPaneSplitRatio` encoding (migrated in on load);
   *  those legacy fields stay for back-compat read. Persisted. */
  rightPaneLayout: RightPaneLayout;
  /** When the active note's last edit reached disk (epoch ms); null until a save
   *  lands this session. Not persisted. */
  noteSavedAt: number | null;
  /** The viewer's share of the column beside the CLI (the CLI holds the rest), as the
   *  split divider leaves it (D39: global). */
  terminalSplitRatio: number;
  /** T.4: per-team list of expanded sub-epic absolute paths in the status nav. Outer key = `teamKey`. */
  statusNavExpandedPaths: Record<string, string[]>;
  /** SLW-NC-7: per-team PERSONAL nav pins — repo-relative paths (dirs or files)
   *  this user pinned to the top of the left nav, layered additively on top of the
   *  git-shared TEAM pins (which live in `designations.yml`, not here). Local-only,
   *  never committed; mirrors `statusNavExpandedPaths`. Outer key = `teamKey`. */
  statusNavPinnedPaths: Record<string, string[]>;
  /** Per-team left-nav lens (which stories show, how they are arranged). Session-only,
   *  not persisted. Outer key = `teamKey`. */
  navLens: Record<string, NavLensState>;
  /** N.6: ephemeral counter bumped by revealOpenedFile. PipelineStatusNav
   *  watches it to trigger scroll-into-view without reacting to manual
   *  folder expand/collapse. NOT persisted or snapshotted. T.4: outer key = `teamKey`. */
  revealCounter: Record<string, number>;
  settings: ViewerSettings;
  /**
   * Per-team list of workspaces. Each team has at least one root
   * workspace per attached source repo (minted by `ensureRootWorkspaces`
   * on team activation). Story workspaces are appended when a pipeline
   * node is clicked. Source of truth for the workspace bar.
   *
   * `teamKey = buildTeamKey(orgSlug, teamName)` (see `team-key.ts`).
   */
  workspaces: Record<string, Workspace[]>;
  /** Per-team active workspace ID (T.1: re-keyed from repoPath to teamKey). */
  activeWorkspace: Record<string, string>;
  /**
   * SW-WL-3 AC-1: per-team set of "open" (surface-active) workspace ids that
   * drive the surface-bound file-watcher. Workspace-level, NOT pane-level.
   * The value is always `1` — membership semantics, since each workspace has
   * exactly one logical open-holder (creation for story workspaces, first
   * activation for roots). This makes `openWorkspaceWatch` idempotent so the
   * activation call sites can fire freely without leaking a refcount that
   * never drains (roots never close via `closeWorkspace`).
   *
   * Runtime-only: NOT persisted. Rebuilt at F.18 restore (T1.3) from the
   * restored workspaces, and cleared per-team on `closeTeam` teardown. The
   * 0→1 / 1→0 transitions invoke the attach/detach hooks in
   * `workspace-watch-hooks` (T2 wires the Rust `lazy_attach_workspace` /
   * `lazy_detach_workspace` commands; F-2 — no event substrate).
   */
  workspaceOpenSet: Record<string, Record<string, number>>;
  /** The terminal hosts' layouts, projected for rendering: which workspaces
   *  hold which tabs, their names, order, focus, grid and live/exited state.
   *  Runtime-only (the hosts own it); written ONLY by `applyLayout`. Read it
   *  through the terminal selectors (`selectWorkspaceTabs`, …). */
  terminalLayout: TerminalLayoutProjection;
  /** Per workspace key, the sizes this viewer dragged its tab grid to, stamped with
   *  the preset they were dragged under (another preset voids them). This
   *  viewer's presentation only — never sent to a host. Persisted. */
  tabGridLayout: Record<string, SavedPresetTree>;
  /** SW-RT-6d: per workspace key, how many new terminals an incompatible
   *  terminal server refused and are held — no tab, no pane, nothing typed —
   *  until it restarts, when they open. Runtime-only; drives the pane's held
   *  notice. */
  heldTerminalOpens: Record<string, number>;
  /** How many times each session's tab started a new process in place
   *  (`respawnTerminalTab`). Its pane is keyed by it, so it remounts on the new
   *  process. Runtime-only; dropped with the tab. */
  terminalLives: Record<string, number>;
  /** Whether the terminal hosts could not be asked for their layout yet (a
   *  terminal server that runs but does not answer): until one answers, the
   *  terminal area says so instead of showing nothing. Runtime-only. */
  terminalHostsUnreachable: boolean;
  /** SW-FX-52: agent status keyed by PTY `sessionId` — a FLAT slice decoupled
   *  from the tab arrays. A `terminal-status-update` writes ONE entry in O(1)
   *  with no scan, and — because status no longer lives on the tab — a status
   *  tick never churns the terminal layout's identity, so nothing that
   *  renders tabs re-renders on it; only the leaf status dots (which subscribe
   *  per session/workspace) do. Session ids are globally unique, so one flat map
   *  serves live AND soft-closed (minimized) tabs uniformly — no shadow branch.
   *  Ephemeral (runtime-only, never persisted): PTYs die on quit, so a restored
   *  session has no live status. Cleared on `pty-exit`/SessionEnd. Holds the
   *  coarse wire status; dots read it through `selectSessionAiStatus`, which
   *  prefers the session's agent-state record when it has one. */
  aiStatusBySession: Record<string, TerminalWireStatus>;
  /** SW-AM-4: retained live `signal_progress` per session (latest note + optional
   *  N-of-M + phase), keyed by the globally-unique sessionId — the exact mirror
   *  of `aiStatusBySession`. The Agents-panel row subscribes to its OWN session
   *  via `useSessionProgress`, so a progress tick re-renders only that row.
   *  Ephemeral (runtime-only, never persisted); fed by the `mcp:agent-progress` /
   *  `mcp:agent-done` listeners, pruned alongside `aiStatusBySession`. */
  progressBySession: Record<string, SessionProgress>;
  /** feat-idle-agent-status: epoch-ms of the last `signal_progress` tick seen for
   *  each session — stamped by every {@link setSessionProgress} call, including
   *  the ones the field-equality guard would otherwise skip. The idle derivation
   *  reads it to tell a running agent that's quietly ticking from one that has
   *  gone silent (`IDLE_STALLED_MS`). Ephemeral, pruned with `progressBySession`. */
  progressTickBySession: Record<string, number>;
  /** SW-EW-28: per-workspace HUD visibility — `true` = the story overlay is open
   *  for that tile (the chrome toggle / ⌘⇧H drive it), absent/`false` = hidden
   *  (just the chrome toggle shows). Keyed by workspace id. Runtime-only — each
   *  tile starts hidden on launch. */
  storyHudShown: Record<string, boolean>;
  /** SW-EW-28: per-workspace HUD size — `true` = expanded (the full scrollable
   *  story / epic README), absent/`false` = compact card. Keyed by workspace id.
   *  Runtime-only — NOT persisted (each tile starts compact on launch). */
  storyHudExpanded: Record<string, boolean>;
  /** Per-tab source-diff overlay visibility — `true` = the source-diff review
   *  overlay is open for the terminal session with this id. **Keyed by terminal
   *  `sessionId`**, so different tabs of the same workspace are independent, and
   *  switching tabs within a tile re-reads the current tab's state. Runtime-only;
   *  each tab starts hidden on launch. The instance-wide `sourceSlot` guarantees
   *  at most one entry is `true` across the whole app. */
  sourceDiffShownBySession: Record<string, boolean>;
  // SW-TP D1: `sourceShownBySession` + its selectors/actions are retired. The
  // Editor overlay covers all folder-rooted tabs (CAP-1); the standalone
  // source browser was redundant with it. The TileOverlay union's "source"
  // member and the matching branch in `closeSiblingOverlays` go with it.
  /** SLW-WD-2: per-workspace source-diff size — `true` = expanded (the inset
   *  redline reader), absent/`false` = compact glanceable file list. Keyed by
   *  workspace id. Runtime-only — NOT persisted (mirrors `storyHudExpanded`). */
  /** SLW-WD-2: last-selected file path in the source-diff overlay (the file
   *  whose redline the expanded reader shows), or `null` when none is selected.
   *  **Keyed by terminal `sessionId`** (SW-TP CAP-3): sibling tabs on the same
   *  workspace carry independent selected-file state. Runtime-only. */
  sourceDiffSelectedFile: Record<string, string | null>;
  /** SLW-WD-9 (AC-1/AC-1c): per-workspace source-diff RAIL mode — `true` =
   *  History (commit graph), `false` = Working (file list). ABSENT = not yet
   *  picked: the overlay adaptively leads with History on a clean tree / Working
   *  when there are changes, ONCE the change-set first resolves (RD-12). Once
   *  present (adaptive pick or an explicit toggle) it sticks — a nonce refresh
   *  never re-adapts. **Keyed by terminal `sessionId`** (SW-TP CAP-3): sibling
   *  tabs on the same workspace carry independent history-mode selection.
   *  Runtime-only — reset on hide/close. */
  sourceDiffHistoryMode: Record<string, boolean>;
  /** SLW-WD-9 (AC-2/RD-11): the selected commit in History mode — a commit
   *  `sha`, or `null`/absent = the pinned Working-tree node (the default). Keyed
   *  by SHA so the selection (and its lazily-fetched redline) survives paging /
   *  virtualization / a nonce refresh. **Keyed by terminal `sessionId`** (SW-TP
   *  CAP-3). Runtime-only — reset on hide/close. */
  sourceDiffSelectedSha: Record<string, string | null>;
  /** SLW-DV-7 (AC-2): working-diff comparison basis. Absent = `"branchBase"`
   *  (the default). Session memory — persists across a hide like
   *  `sourceDiffHistoryMode`, cleared only on minimize/close. **Keyed by
   *  terminal `sessionId`** (SW-TP CAP-3). */
  sourceDiffBaseMode: Record<string, SourceDiffBaseMode>;
  /** The source-diff VIEW options (inline / wordWrap / expandAll / autoFollow /
   *  ignoreWhitespace). One record backs the whole set — `resolveSourceDiffOption`
   *  owns each option's absent-value rule (inline follows the Settings default,
   *  autoFollow defaults ON, the rest OFF); an absent key is a genuine "unset",
   *  so inline's tri-state survives. **Keyed by terminal `sessionId`** (SW-TP
   *  CAP-3). Runtime-only — survives re-render AND hide (session memory) and
   *  carries to the next file; pruned on tab close. */
  sourceDiffViewOptions: Record<string, SourceDiffViewOptions>;
  /** Per-tab worktree code-editor visibility — `true` = the editable code view is
   *  open on this terminal session. **Keyed by terminal `sessionId`**, so a tile
   *  with N tabs tracks each tab's editor independently. Runtime-only; each tab
   *  starts closed. Mutually exclusive with `sourceDiffShownBySession` on the
   *  SAME session (opening one closes the other), and instance-wide via
   *  `sourceSlot` (opening on one tab closes the slot's prior occupant). */
  worktreeEditorOpenBySession: Record<string, boolean>;
  /** SLW-DV-3 (session memory): per-workspace remembered editor file (repo-
   *  relative), so re-opening the editor resumes the last file rather than the
   *  tree root. Absent/`null` = open at the tree root. Runtime-only. */
  worktreeEditorFile: Record<string, string | null>;
  /** A one-shot "open at this line" for the editor's next mount, keyed by
   *  workspace id — set by the source diff's "Open in code editor" so the
   *  editor lands on the change being read. Consumed (cleared) by the editor
   *  surface once it mounts. Runtime-only. */
  worktreeEditorRevealLine: Record<string, { path: string; line: number }>;
  /** SLW-DV-9: per-workspace UNSAVED-edit draft, so losing tile focus (a split
   *  unfocus or a 1-up workspace switch, both of which unmount the editor inset)
   *  never silently drops the buffer. Written only while the buffer is DIRTY —
   *  `relPath` is the repo-relative file it belongs to, `buffer` the edited text,
   *  `lastSaved` the on-disk baseline the edit diverged from (kept so a restore can
   *  re-derive dirtiness AND re-arm the stale guard against fresh disk, WDE-3/4).
   *  Seeded into the editor on remount and cleared on save/discard/clean. Keyed by
   *  workspace id, then by the file's repo-relative path (one per open tab).
   *  Runtime-only — NOT persisted. */
  worktreeEditorDraft: Record<string, Record<string, WorktreeEditorDraft>>;
  /** Per-workspace open editor tabs (repo-relative paths, in tab order), so a
   *  re-open — or a remount after an unfocus — brings back the same set. The
   *  active one is `worktreeEditorFile`. Runtime-only. */
  worktreeEditorTabs: Record<string, string[]>;
  /** SLW-DV-8b (RD-3): code-diff mode (line / ignore-whitespace / structural).
   *  Absent = `"line"`. Runtime-only, a sibling of the consolidated
   *  `sourceDiffViewOptions` record — kept separate because it is a 3-value
   *  enum, not a boolean. **Keyed by terminal `sessionId`** (SW-TP CAP-3). */
  sourceDiffMode: Record<string, SourceDiffMode>;
  /** SLW-WDE-3: per-workspace external-change signal for the open editor inset.
   *  The App-level `worktree-file-change` listener bumps this on ANY change the
   *  worktree watcher reports under `ws.cwd`. `nonce` is the trigger — it bumps
   *  every event so a repeat change to the same file still registers, and
   *  `useWorktreeFileEditor` reconciles the OPEN file against disk on each bump
   *  (path-agnostic, so a multi-file agent turn can't overwrite the open file's
   *  own signal in this single slot). `path` is the last-changed file's path
   *  RELATIVE to `ws.cwd` — retained for diagnostics only; the reconcile does not
   *  read it. `resumed` counts the bumps that mean "a hidden team is shown again" rather than
   *  a file changed, so a consumer behind a debounce can catch up at once. Keyed by workspace
   *  id. Runtime-only — NOT persisted. */
  worktreeChangedPath: Record<string, { path: string; nonce: number; resumed?: number }>;
  /**
   * F-6 AC-8: when non-null, the no-local-clone-attached modal is open
   * for this synthetic workspace's source repo. Set by entry-point
   * actions (worktree icon, terminal cwd, open-in-editor, story-open)
   * when `resolveSourceRepoPath(repo)` returns `null`. Cleared on
   * dismiss or after the user attaches a clone (via the
   * `attachmentsVersion` re-trigger).
   *
   * T.4 AC-3: reshape from `string | null` (repoPath) to
   * `{ teamKey, sourceRepoSlug } | null`.
   */
  noClonePromptRepo: { teamKey: string; sourceRepoSlug: string } | null;
  /**
   * F-6 review patch: callback to re-fire the original action that
   * triggered the no-clone modal. After a successful attach, an observer
   * waits for the synthetic's `localPath` to populate (workspace-sync
   * soft refresh) then runs this callback so the user doesn't have to
   * re-click their original action. Cleared after fire or on explicit
   * dismiss.
   */
  noClonePromptRetry: (() => void) | null;
  /**
   * SW-WT-9: pending team-sync setup approval gate. Non-null while the
   *  preview-and-approve modal is open; the user's choice resolves the
   *  in-flight `setupWorktree` prompt. `steps` are the verbatim setup
   *  command-lines (AC-2); `repoLabel` is for display only. */
  worktreeSetupApproval: {
    steps: string[];
    configHash: string;
    repoLabel: string;
    teamKey: string;
  } | null;
  stateLoaded: boolean;
  /** `loadPersistedState` has finished (restored, found nothing saved, or failed).
   *  Unlike `stateLoaded`, the boot watchdog never forces it, so code that must
   *  not act before saved tabs are back (the team landing page) waits on this.
   *  Runtime-only, never persisted. */
  persistedStateRestored: boolean;
  /**
   * Per-team pipeline scope — the directory (relative to pipelineRoot) the
   * story views are currently rooted at. Persisted. Empty string means
   * "scoped at pipelineRoot itself". T.4: outer key = `teamKey`. See
   * 30.7 AC-32/33.
   */
  pipelineScope: Record<string, string>;
  /**
   * SW-XS-15 — per-team left-nav context pivot: whose context the nav shows.
   * Absent = **Team (everything)** (the raw context repo); otherwise
   * **Context for ‹Agent›**, `repoSlug` (`owner/repo`) adding **· in ‹Repo›**.
   * Persisted, team-keyed. A stale agent or repo (archived / removed) reads as
   * Team (everything) / the team workspace at read time (`useAgentContextPivot`),
   * so this slot never needs a reconciling write. Replaces the retired
   * install-tier pivot (`selectedSourceRepo` / `selectedContextId`, v14).
   */
  selectedAgentContext: Record<string, AgentContextSelection>;
  /**
   * True after the single-IPC ancestor-walk scope recovery has run at
   * startup. Runtime-only; not persisted. See 30.7 AC-19/34.
   */
  scopeRecoveryComplete: boolean;
  /** T.6: list of teams the user has visited this session (or restored
   *  from persisted state). Each entry gets a `<TeamWorkspaceView>` mount
   *  in the shell's workbench; only the active team is `display:block`, the rest stay
   *  alive at `display:none` (F.16/F.18/F.19/F.20). Pushed by `openTeam`
   *  on team-switch; spliced by `closeTeam`. */
  openedTeams: string[];
  /** T.4: per-team nav breadcrumb history. Outer key = `teamKey`. */
  navHistory: Record<string, string[]>;
  /** Runtime-only file fragments, retained until the target team/tab consumes them. */
  fileHashRequests: Record<string, FileHashRequest>;
  consumeFileHash: (teamKey: string, request: FileHashRequest) => void;
  /** T.4: per-team nav history pointer. Outer key = `teamKey`. */
  navIndex: Record<string, number>;
  /**
   * Per-team LRU of recently-viewed file paths. Keeps the last N
   * `MarkdownViewer` instances mounted (active + hidden via `display:none`)
   * so tab switches skip re-parse / re-highlight / re-mermaid. Runtime-
   * only; not persisted. T.4: outer key = `teamKey`. See `touchViewerPath`
   * and `VIEWER_PERF.keepAliveMaxViewers`.
   */
  openedViewerPaths: Record<string, string[]>;
  /**
   * Per-team map of on-disk worktree entries. Outer key = `teamKey`;
   * inner Map key = composite `${sourceRepoSlug}::${storyFilePath}`
   * (T.4 D-3). Populated by the prefetch loop via `setOnDiskWorktrees`
   * — tracks which stories have a worktree directory on disk,
   * independently of workspace entries. **Runtime-only — not
   * persisted.** Re-derived from disk on each prefetch cycle.
   */
  onDiskWorktrees: Record<string, Map<string, OnDiskWorktreeEntry>>;
  /**
   * PT-10: minimized ("close, keep running") workspaces, keyed
   * `[teamKey][workspaceId]`. Populated by `closeWorkspace` when a workspace is
   * closed with ≥1 terminal; read by the EW panel to render dimmed rows and by
   * every reopen path to restore the workspace. Only the record and its modes
   * (see `SoftClosedWorkspace`), persisted: its terminals are its host's.
   * Distinct from a dismissed-workspace allow-list: presence here means
   * "minimized," not "user dismissed it" — both can be true independently.
   */
  softClosedWorkspaces: Record<string, Record<string, SoftClosedWorkspace>>;
  /**
   * SW-EW-33: per-team most-recently-used workspace stack (most-recent first,
   * de-duped). Drives the `Cmd+Alt+;` flip-to-last gesture. Runtime-only — recency
   * is session-scoped and not worth persisting.
   */
  mruWorkspaceStack: Record<string, string[]>;
  /**
   * SW-EW-33: the tray's CURRENTLY-RENDERED chip order (workspace ids), published
   * by `WorkspaceTray` so the `Cmd+1..9` positional jump indexes the exact array
   * the user sees (single source of truth). Runtime-only.
   */
  trayOrderByTeam: Record<string, string[]>;
  /**
   * SW-CW-4: the FULL on-screen strip order (all rails), published by
   * `WorkspaceTray` alongside `trayOrderByTeam`. In grouped mode `trayOrderByTeam`
   * holds only the selected rail's row-2 chips, so `closeWorkspace` reads this
   * cross-rail order to pick the adjacent tab (`adjacentWorkspaceIdAfterClose`).
   * Runtime-only — re-published on mount, never persisted.
   */
  trayFullOrderByTeam: Record<string, string[]>;
  /**
   * The person's workspace order per team (workspace ids), shared by the tray's pills and the
   * Agents pane's cards. Written only by a move (`reorderWorkspace`); absent → the computed tray
   * order. A workspace closed for good leaves it. PERSISTED (additive slice, absent → {}; on load
   * de-duplicated and kept to the workspaces that came back, open or minimized).
   */
  workspaceGroupVersionsByTeam: Record<string, number>;
  customGroupsByTeam: Record<string, WorkspaceGroup[]>;
  applyWorkspaceGroups: (snapshot: GroupSnapshot) => void;
  editWorkspaceGroup: (teamKey: string, edit: GroupEdit) => Promise<GroupResult>;
  createWorkspaceGroup: (
    teamKey: string,
    name: string,
    workspaceIds?: string[],
  ) => Promise<GroupResult>;
  renameWorkspaceGroup: (teamKey: string, groupId: string, name: string) => Promise<GroupResult>;
  setWorkspaceGroupMembers: (
    teamKey: string,
    groupId: string,
    workspaceIds: string[],
    pruneEmpty?: boolean,
  ) => Promise<GroupResult>;
  deleteWorkspaceGroup: (teamKey: string, groupId: string) => Promise<GroupResult>;
  workspaceOrderByTeam: Record<string, string[]>;
  /**
   * The tray's computed order per team (`trayWorkspaceOrder` over the team's pipeline tree),
   * cached by `syncWorkspaceFrontmatter` next to `railKeyByWorkspaceId` so the Agents pane, the
   * tiles and `reorderWorkspace` read the order the tray draws without the tree. PERSISTED as the
   * last order the tree gave, so after a restart the cards and tiles keep it until the tree syncs
   * again (absent → {}; sanitized on load as `workspaceOrderByTeam` is).
   */
  trayBaseOrderByTeam: Record<string, string[]>;
  /**
   * SW-EW-38: per-team tray lens. "flat" (default) = today's single-row tray
   * (one chip per workspace); "grouped" = the two-row epic-rail mode. PERSISTED
   * (additive slice, absent → {}, no migration — mirrors SW-FX-8 `navCollapsed`).
   */
  trayMode: Record<string, "flat" | "grouped">;
  /**
   * SW-EW-38: per-team selected epic-rail key in grouped mode — a real epic
   * `dirName`, "" for the Ungrouped pill, or `ROOTS_RAIL_KEY` for the roots pill
   * (the one discriminator that disambiguates roots-"" from Ungrouped-""). R3:
   * RUNTIME-ONLY — follow-focus reconstructs it on entry/focus, so it is in
   * `TEAM_KEYED_SLICES` for `closeTeam` cleanup but is never persisted.
   */
  selectedEpicDirName: Record<string, string>;
  /**
   * SW-EW-40: per-team workspace-id → rail key index, rebuilt on every tree /
   * workspace change by `syncWorkspaceFrontmatter` from the SAME
   * `trayWorkspaceOrder`+`railKeyOf` the tray uses — so the mosaic's grouped
   * membership and the tray can never disagree. RUNTIME-ONLY (like
   * `selectedEpicDirName`): in `TEAM_KEYED_SLICES` for `closeTeam` cleanup, never
   * persisted (it is a pure derivation of the tree). Outer key = `teamKey`, inner
   * key = workspace id.
   */
  railKeyByWorkspaceId: Record<string, Record<string, string>>;
  /**
   * Honest lifecycle for an OPEN file, keyed by normalized absolute path. The viewer that
   * renders a file publishes its own `readFile` outcome here (the bit that drives
   * `FileDeletedPlaceholder`): a missing read → `"deleted"`, a transient read failure →
   * `"temporarily_unavailable"`. Holds ONLY non-`active` paths (a successful read deletes the
   * entry). Runtime-only — deliberately NOT in the `debouncedPersist` allowlist and NOT
   * team-keyed (absolute paths are globally unique); bounded to viewed files by the viewer's
   * unmount clear.
   */
  openFileLifecycleByPath: Record<string, SubjectLifecycle>;
  /**
   * SW-EW-33: whether the one-time bottom-tray coachmark ("click to switch · drag
   * to split") has been dismissed. Global scalar (the gesture is learned once),
   * persisted additively — no schemaVersion bump.
   */
  workspaceTraySeenCoachmark: boolean;
  /**
   * OC-17: whether the one-time CLI empty-state hint pointing at the keyboard
   * shortcuts reference has been dismissed. Global scalar (the entry point is
   * learned once), persisted additively — no schemaVersion bump (mirrors
   * `workspaceTraySeenCoachmark`).
   */
  cliShortcutsHintSeen: boolean;
  /**
   * SW-AGR-19: the out-of-date terminal servers the user dismissed the notice
   * for, as `machineId@build` entries. The notice returns only for a server or
   * build not in this list. Persisted additively (no schemaVersion bump).
   */
  dismissedTerminalServers: string[];
  /**
   * SW-SO-1: per-team set of workspace ids whose open is in its PRE-RECORD
   * "opening" window — the instant between the click and `addStoryWorkspace`
   * landing a `status:"creating"` record. Drives the decoupled opening overlay
   * (team-workspace-view) that shows the "Setting up worktree…" affordance while
   * the git/config probe chain runs, BEFORE any workspace record (and thus the
   * in-tile overlay) exists. Set by `ensureWorktree` right after the fast-path +
   * dedup early-returns (so a ready-reopen / double-click never flashes it) and
   * cleared in the same promise's `finally`. Runtime-only — never persisted;
   * swept on `closeTeam` via TEAM_KEYED_SLICES.
   */
  openingWorkspaces: Record<string, Set<string>>;

  /** T.6: push `teamKey` to `openedTeams` if absent. Called from
   *  `use-workspace-viewer-sync` whenever a team becomes the active team
   *  so its mount stays alive across subsequent switches. */
  openTeam: (teamKey: string) => void;
  /** Open a file as a pinned tab. An open for a team on screen steps out of full-screen
   *  CLI, because the file needs the pane; `keepLayout` is for an open the user did not ask
   *  for (a restored tab's redirect, a launch landing), which never leaves full screen. */
  openFile: (
    teamKey: string,
    repoPath: string,
    filePath: string,
    opts?: { keepLayout?: boolean; hash?: string },
  ) => void;
  openFilePreview: (
    teamKey: string,
    repoPath: string,
    filePath: string,
    opts?: { keepLayout?: boolean; hash?: string },
  ) => void;
  /**
   * SW-AM-2: open a story doc when the caller knows only its owning `teamKey`
   * (not a `repoPath`) — the Agents panel and the CLI workspace tab both do.
   * Resolves the team's content anchor via `firstRepoPathForTeam` and delegates
   * to `openFile`, so callers express intent without duplicating the repoPath
   * plumbing. `openFile` / `revealOpenedFile` anchor on the given team, so a
   * cross-team doc opens under its own team. `opts` as in `openFile`.
   */
  openStoryDocForTeam: (
    teamKey: string,
    storyFilePath: string,
    opts?: { keepLayout?: boolean },
  ) => void;
  /**
   * SW-WG-9: which panel is maximized to the in-app full-screen overlay, keyed by
   * the panel's OWN team key (`Record<teamKey, panelId>`) — one generic slice that
   * replaces the former per-panel `*FullscreenTeamKey` scalars. Own-identity keying (a background team can never
   * maximize the foreground, since every mounted view resolves the same globally-active
   * team key) + the foreground gate live in `useFullscreenOverlay` / `useMaximized`.
   * At most ONE maximized panel per team (only one right-pane panel is ever visible at
   * a time). An absent key = nothing maximized for that team. Runtime-only: deliberately
   * omitted from the `debouncedPersist` allowlist so maximize is ephemeral (never
   * survives a reload), yet a MEMBER of `TEAM_KEYED_SLICES` so team rename/close sweep
   * it like its siblings (no bespoke per-scalar rename/remove handling).
   */
  maximizedPanel: Record<string, string>;
  /** SW-WG-9: set (`panelId`) or clear (`null`) the maximized panel for a team. */
  setMaximized: (teamKey: string, panelId: string | null) => void;
  /** SW-WG-9: toggle a panel's maximize for a team — maximize if not already it, else exit. */
  toggleMaximized: (teamKey: string, panelId: string) => void;
  pinTab: (teamKey: string, filePath: string) => void;
  /** RF-perf-12: write a lightweight view-position snapshot into
   *  `viewStateByKey` (O(1), reference-stable, NEVER persisted). Called from a
   *  view's unmount cleanup — see `navViewKey` / `fileViewKey`. */
  captureViewState: (viewKey: string, snap: ViewSnapshot) => void;
  /** RF-perf-12: read a snapshot back on remount (undefined = none saved). A
   *  plain getter — restore timing is the caller's (post-async-render). */
  readViewState: (viewKey: string) => ViewSnapshot | undefined;
  closeTab: (
    teamKey: string,
    repoPath: string,
    filePath: string,
    opts?: { user?: boolean },
  ) => void;
  closeOtherTabs: (teamKey: string, repoPath: string, filePath: string) => void;
  closeAllTabs: (teamKey: string) => void;
  /** SW-VE-3: set the paths with an unsaved deliberate edit for a team, so
   *  `closeTab` won't silently drop the buffer. */
  setEditingDirtyPaths: (teamKey: string, filePaths: string[]) => void;
  /** SW-VE-8: request entering edit on `teamKey`'s active file (from the
   *  `viewer.edit-file` shortcut / File menu). Bumps `editFileRequest.nonce`;
   *  the team view's effect honors it through the same edit-entry gate the ⌘E
   *  keydown used (no-op on read-only / inherited / non-editable / already
   *  editing). */
  requestEditActiveFile: (teamKey: string) => void;
  /**
   * SW-VE-6: follow a renamed/moved file. Repoint any open tab (+ the active
   * pointer + nav history) from `oldPath` to `newPath`, recomputing the tab
   * name. A no-op when no tab references `oldPath` (moving a file that isn't
   * open must not spawn a tab). The caller uses `closeTab` instead when the
   * file moved out of the current view scope.
   */
  remapTabPath: (teamKey: string, oldPath: string, newPath: string) => void;
  /**
   * `repoPath` is nullable so cross-team callers can pass `null` to skip
   * the ancestor-reveal (e.g. when the team has no active workspace with
   * a resolvable `sourceRepoPath`). Tab state still updates; only the
   * reveal-into-current-repo's nav is gated.
   */
  setActiveTab: (teamKey: string, repoPath: string | null, filePath: string) => void;
  reorderTab: (teamKey: string, fromIdx: number, toIdx: number) => void;
  setDrillPath: (teamKey: string, drillPath: string) => void;
  /** SLW-FX-14: write through the resolved team-pipeline key for `teamKey` so
   *  it survives to the next cold open. No-op when the value is unchanged
   *  (avoids a redundant persist on every render). */
  setTeamPipelineRepoPath: (teamKey: string, repoKey: string) => void;
  /**
   * Drill the nav to a target path AND reconcile the pipeline scope in
   * one step. Single source of truth for the drill+scope sync — both
   * the nav drill chevron and the pipeline-canvas drill button route
   * through this so they can never disagree.
   *
   * T.4: first arg is `teamKey` for slice writes; contextual paths
   * (`contextRepoPath`/`teamContentPath`) come from the caller's resolved
   * repo (D-6). SW-FX-56: the trailing intentionally-ignored `pipelineRoot`
   * param was removed (scan scope retired by R-14/PTS-2).
   */
  syncDrillPath: (
    teamKey: string,
    targetAbsolutePath: string,
    contextRepoPath: string,
    teamContentPath: string,
  ) => void;
  setTerminalVisible: (visible: boolean) => void;
  /** SW-FX-8: set the left-nav collapsed state for a team. */
  setNavCollapsed: (teamKey: string, collapsed: boolean) => void;
  /** Open a changed file on its Changes: a preview tab (single click) or a pinned one. */
  openChangeTab: (teamKey: string, repoPath: string, filePath: string, pin: boolean) => void;
  /** Open a file on one commit's change to it, in an ordinary tab named `file @ sha`. */
  openCommitChangeTab: (
    teamKey: string,
    repoPath: string,
    filePath: string,
    commit: CommitDiff,
    pin: boolean,
  ) => void;
  /** Show an open tab's Changes (`true`) or the file itself. */
  /** Show a tab's Changes (`true`: against GitHub; a commit: that commit's change) or,
   *  with `false`, the file. */
  setTabDiff: (teamKey: string, filePath: string, on: TabDiff | false) => void;
  /** Fold or open the Agents dock beside the CLI (one setting for every team). */
  setWorkspacePanelCollapsed: (collapsed: boolean) => void;
  /** Fold or open one team or workspace header in the agents pane, by fold key. */
  setAgentsPaneFold: (key: string, folded: boolean) => void;
  /** Show or hide one team in the agents pane (the Teams checklist, a team chip, a row's team). */
  setAgentsPaneTeamHidden: (teamKey: string, hidden: boolean) => void;
  /** Set the pane's whole hide list at once, with the team funnel's restore point. */
  setAgentsPaneTeamScope: (hidden: string[], focusRestore: TeamFocusRestore | null) => void;
  /** Set or clear the arrow filter: the pane shows only this workspace. */
  setAgentsWorkspaceFilter: (filter: AgentsWorkspaceFilter | null) => void;
  revealAgentsWorkspace: (teamKey: string, workspaceId: string) => void;
  clearAgentsWorkspaceReveal: () => void;
  /** Replace the team and workspace history. */
  setContextHistory: (history: ContextHistory) => void;
  /** Choose (or, with `null`, return to Auto) an agent's role piece or face in a team. */
  setAgentIdentity: (teamKey: string, handle: string, patch: AgentIdentityPatch) => void;
  /** Drop a team's chosen looks for sessions not in `keep`. */
  pruneAgentIdentity: (teamKey: string, keep: ReadonlySet<string>) => void;
  /** Pin an agent to the top of a team's agents list (after the pins already there). A pin
   *  with the same id is updated in place: it keeps its position. */
  pinAgent: (teamKey: string, pin: AgentPin) => void;
  /** Drop pins from a team's agents list, by {@link agentPinId}. */
  unpinAgents: (teamKey: string, pinIds: readonly string[]) => void;
  /** Set the Agents dock's dragged width in px (one setting for every team). */
  setWorkspacePanelWidth: (width: number) => void;
  /** SW-EW-3 AC-4/9: commit the split-terminal pane sizes (on drag-release,
   *  not per-frame). Thin alias over `setForegroundNodeSizes` at the root. */
  setForegroundPanelSizes: (teamKey: string, sizes: number[]) => void;
  /** SW-EW-17 AC-4: commit sizes for one split node addressed by index `path`
   *  (`[]` = root). At depth-1 with no custom mosaic this writes the flat `sizes`
   *  (unchanged columns/rows behavior); for a nested mosaic it writes `root`. */
  setForegroundNodeSizes: (teamKey: string, path: number[], sizes: number[]) => void;
  /** SW-EW-17 AC-1/2: place a workspace into the mosaic at a directional edge
   *  target (split) or focus it (center). Used by directional drag-split and by
   *  drag-restore (via `reopenSoftClosedWorkspace`). */
  placeWorkspaceTile: (
    teamKey: string,
    workspaceId: string,
    target: TilePlacementTarget | { center: true },
    opts?: { user?: boolean },
  ) => void;
  /** SW-EW-33: click-to-replace — swap the workspace occupying the focused mosaic
   *  leaf for `incomingWorkspaceId` (a minimized workspace) IN PLACE. The
   *  displaced workspace soft-closes (PTYs kept), the incoming reopens into that
   *  same leaf → pane count + arrangement unchanged. No focused leaf (empty
   *  mosaic) → opens the incoming into the center instead. */
  replaceFocusedLeafWorkspace: (
    teamKey: string,
    incomingWorkspaceId: string,
    opts?: { keepLayout?: boolean },
  ) => void;
  /** SW-EW-33: the shared tray state-rule used by chip click, `Cmd+1..9`, and
   *  flip-to-last — minimized target → replace the focused leaf; on-screen target
   *  → focus; already-focused → no-op. Returns `true` when it switched or
   *  replaced, `false` on the already-focused / unknown-id no-op (so callers can
   *  surface the blocked-nav cue). */
  activateTrayWorkspace: (
    teamKey: string,
    workspaceId: string,
    opts?: { keepLayout?: boolean },
  ) => boolean;
  /** SW-EW-33: `Cmd+Alt+;` — activate the most-recently-used OTHER workspace
   *  (2-way flip). Returns `true` when it flipped, `false` when there's no
   *  distinct prior workspace to flip to. */
  flipLastWorkspace: (teamKey: string) => boolean;
  /** SW-EW-33: publish the tray's rendered chip order (called by `WorkspaceTray`);
   *  no-ops when unchanged so it can run in an effect without a render loop. */
  setTrayOrder: (teamKey: string, workspaceIds: string[]) => void;
  /** SW-CW-4: publish the tray's FULL on-screen strip order (all rails) so
   *  `closeWorkspace` can pick the adjacent tab across epic boundaries. Same
   *  no-op-when-unchanged guard as `setTrayOrder`. */
  setTrayFullOrder: (teamKey: string, workspaceIds: string[]) => void;
  /** SW-EW-33: `Cmd+1..9` — activate the Nth (0-based) chip in the rendered tray
   *  order via the shared state-rule. Out-of-range or already-active → `false`
   *  (no move); `true` on activation. */
  jumpToTrayIndex: (teamKey: string, index: number) => boolean;
  /** SW-EW-33: dismiss the one-time bottom-tray coachmark (persisted). */
  markWorkspaceTrayCoachmarkSeen: () => void;
  /** OC-17: dismiss the one-time CLI shortcuts hint (persisted). */
  markCliShortcutsHintSeen: () => void;
  /** SW-AGR-19: dismiss the out-of-date notice for these `machineId@build`
   *  entries (persisted). */
  dismissTerminalServers: (entries: string[]) => void;
  /** SW-EW-38: flip the per-team tray lens (flat ↔ grouped); persisted. */
  setTrayMode: (teamKey: string, mode: "flat" | "grouped") => void;
  /** SW-EW-38: set the selected epic-rail key in grouped mode. Takes an already-
   *  resolved rail key (epic→key resolution lives in the FE, near the tree).
   *  Runtime-only (R3) — does NOT persist. */
  setSelectedEpicGroup: (teamKey: string, railKey: string) => void;
  /** SW-EW-3 AC-4: set the tiling axis (`row` columns / `column` rows). */
  setForegroundDirection: (teamKey: string, direction: "row" | "column") => void;
  /** Expanded on or off (D68): one setting for every team, always on the focused workspace. */
  setCliExpanded: (expanded: boolean) => void;
  toggleCliExpanded: () => void;
  /** Expand `workspaceId` of `teamKey`: focus it (when it is not the focused one), then
   *  expand. The tile chrome's zoom and a split route here. */
  expandWorkspace: (teamKey: string, workspaceId: string) => void;
  /** SW-EW-4 AC-2: set the layout-switcher preset (syncs `direction`, leaves expanded). */
  setForegroundPreset: (teamKey: string, preset: LayoutPreset) => void;
  /** SW-CW-7: advance the active foreground slot one step around the preset ring
   *  (columns → rows → grid → columns). Thin alias over `setForegroundPreset` — the
   *  direction-sync / custom-root reset / zoom-clear / persist logic stays in one
   *  place. `custom` or an absent slot starts the ring at `columns`. */
  cycleForegroundPreset: (teamKey: string) => void;
  /** SW-EW-46 (D2/D8): the tab-grid dial for one workspace. `paneOut` promotes a
   *  single terminal to a live grid cell (and focuses it); `demote` returns a cell
   *  to the tab bar; `gridAll`/`collapseTabGridToTabs` snap to all-cells / all-tabs;
   *  `cycleTabGridPreset` steps the tab-grid preset around `PRESET_RING`. All write
   *  the workspace's grid in its terminal host. */
  paneOutTerminalTab: (teamKey: string, workspaceId: string, tabId: string) => void;
  demoteTerminalPane: (teamKey: string, workspaceId: string, tabId: string) => void;
  gridAllTerminalTabs: (teamKey: string, workspaceId: string) => void;
  collapseTabGridToTabs: (teamKey: string, workspaceId: string) => void;
  cycleTabGridPreset: (teamKey: string, workspaceId: string) => void;
  /** Lay the workspace's tab grid out as `preset`, evenly: the sizes dragged under
   *  any preset are dropped. */
  setTabGridPreset: (teamKey: string, workspaceId: string, preset: LayoutPreset) => void;
  /** A tab-grid divider was released: the split at `path` of the shown `root` (laid
   *  out as `preset`) now has `sizes`. */
  setTabGridSizes: (
    teamKey: string,
    workspaceId: string,
    preset: LayoutPreset,
    root: MosaicNode,
    path: number[],
    sizes: number[],
  ) => void;
  /** SW-UX-CLI-10 (Split): place the just-opened `newTabId` beside `anchorTabId`
   *  (next in the strip, so next in the grid) and show both side by side — joining
   *  the cells already shown, or as a fresh two-cell grid when the workspace was
   *  on tabs — with the new one focused. False when either tab isn't there. */
  /** Show exactly `paned` (those still tabs of the workspace, in strip order)
   *  side by side — e.g. the cells a zoomed cell was taken from. False when none
   *  of them is left. */
  showTerminalCells: (teamKey: string, workspaceId: string, paned: readonly string[]) => boolean;
  splitTerminalBeside: (
    teamKey: string,
    workspaceId: string,
    anchorTabId: string,
    newTabId: string,
  ) => boolean;
  /** SW-EW-18 AC-3 (SW-EW-15): open/close/toggle the epic-repo-config overlay
   *  for a workspace id. `setEpicReposOpen` is idempotent; `toggleEpicReposOpen`
   *  flips it. */
  setEpicReposOpen: (workspaceId: string, open: boolean) => void;
  toggleEpicReposOpen: (workspaceId: string) => void;
  setLayoutMode: (mode: LayoutMode) => void;
  toggleConsoleShared: () => void;
  /** SW-FX-35: the single "a workspace just opened/restored → bring the terminal
   *  forward" intent. Routes through `setLayoutMode` (the canonical pair-setter)
   *  so `layoutMode` + `terminalVisible` always move together, keeping full-screen
   *  CLI or restoring the user's last terminal mode. Every open/restore path
   *  MUST use this rather than a bare `setTerminalVisible(true)`, which leaves
   *  `layoutMode` "viewer" and the cli-bottom-chrome gate hides the tray. */
  expandTerminalOnOpen: (teamKey?: string) => void;
  /** SW-LN-B38: show the terminal for a just-launched agent WITHOUT changing the
   *  user's layout mode. `shared`/`console` stay as they are; a collapsed pane
   *  (`viewer`) expands to the bottom pane (`shared`) — never jumps to console. */
  revealTerminalInPlace: (teamKey?: string) => void;
  /** SW-LN-B37: settle the layout the first time a team's view becomes active.
   *  A layout already set (saved, or settled by another team's view) is kept as it is,
   *  visibility and last terminal mode too. With none, the terminal pane
   *  starts collapsed (`viewer`) unless it already has a foreground tile (the
   *  same predicate as the pane's empty state),
   *  so a first run never spends the screen on an empty terminal pane. Opening a
   *  workspace or launching an agent expands it later. */
  settleInitialLayout: (teamKey: string) => void;
  // SW-SN-1: plural scratch-notes actions. `setScratchNoteContent` writes the
  // active note; `setActiveNote` is the single programmatic switch entry point
  // (SN-2 selection, SN-3 Enter, SN-9 link) — flushes the outgoing save, loads
  // the target, bumps recency. CRUD actions update `notes` in-memory (AC-3b).
  setScratchNoteContent: (content: string) => void;
  loadNotes: () => Promise<void>;
  /** The notes `scope`'s records projection now holds (`rows`), and the delta that
   *  changed them when one did. Ignored unless `scope` is the loaded notes' scope. */
  applyNotesProjection: (
    scope: NotesScope,
    rows: readonly ProjectedNoteRow[],
    change?: ProjectedNotesChange,
  ) => void;
  setActiveNote: (id: string) => Promise<void>;
  /** SW-SN-10: `initialBody` seeds the note's markdown at creation, so "New note
   *  about this" can pre-link the open doc. Omitted → a blank body. */
  createNote: (title?: string, initialBody?: string) => Promise<string>;
  renameNote: (id: string, title: string) => Promise<void>;
  deleteNote: (id: string) => Promise<void>;
  setNotePinned: (id: string, pinned: boolean) => Promise<void>;
  // Right-pane visibility + width (hosts the Inbox, notes and panes).
  toggleRightPane: () => void;
  setRightPaneRatio: (ratio: number) => void;
  setRightPaneSplitRatio: (teamKey: string, ratio: number) => void;
  /** SW-RP-1: replace the whole structured right-pane layout (sections sent
   *  whole — the layout is a single value, like foregroundLayout). */
  setRightPaneLayout: (layout: RightPaneLayout) => void;
  /** Reveal the active team's bottom agents list (terminal column shown, list
   *  expanded) filtered to one named agent's sessions. */
  revealAgentsListFor: (agent: { agentKey: string; name: string }) => void;
  /** Drop the agents list's agent filter. */
  clearAgentsListFilter: () => void;
  /** SW-WG-10: the generic reveal seam — open the pane and place/activate `panelId`
   *  (as an expanded, active top section via `revealPanel`), then persist. The named
   *  reveal actions delegate here. Synchronous — never blocks the UI. */
  revealPanelById: (panelId: string) => void;
  /** SW-SN-6: publish the active note's live headings (called by the panel from
   *  its `useNoteHeadings` result). Signature-gated upstream, so this only fires
   *  on a real heading-list change. */
  setActiveNoteHeadings: (headings: Heading[]) => void;
  /** SW-SN-6: request a go-to-heading jump by slug (the modal's dispatch). Sets
   *  the `pendingHeadingJump` signal the panel consumes. Synchronous — never
   *  blocks. Pair with `revealSlot("right-pane", {surface: "notes"})` so the jump lands on a visible
   *  editor even from a closed pane. */
  requestHeadingJump: (slug: string) => void;
  /** SW-SN-6: clear the pending jump (the panel calls this once it has scrolled). */
  clearHeadingJump: () => void;
  setTerminalSplitRatio: (ratio: number) => void;
  toggleStatusNavExpanded: (teamKey: string, absolutePath: string) => void;
  /** SLW-NC-7: toggle a PERSONAL nav pin (local-only) for `path` under `teamKey`. */
  toggleStatusNavPinPersonal: (teamKey: string, path: string) => void;
  /** Change part of a team's left-nav lens. */
  setNavLens: (teamKey: string, patch: Partial<NavLensState>) => void;
  updateSettings: (partial: Partial<ViewerSettings>) => void;
  /** Apply what the viewer learned about the terminal hosts' layouts — the ONE
   *  writer of `terminalLayout`. Reports whether it applied, was stale, or the
   *  viewer must re-fetch the snapshot. */
  applyLayout: (input: LayoutInput) => LayoutApplyOutcome;
  /** The terminal hosts could not be asked for their layout (hydrate keeps
   *  asking); the next snapshot that lands clears it. */
  noteTerminalHostsUnreachable: () => void;
  /** Open a terminal tab through the workspace's terminal host (spawned and
   *  placed at once). The tab shows when the host announces it. Resolves once
   *  placed; rejects with the host's refusal (already logged). SW-EW-3: the
   *  optional `workspaceId` targets one tile's workspace; absent → active. */
  addTerminalTab: (
    teamKey: string,
    tab: NewTerminalTab,
    workspaceId?: string,
    opts?: OpenTerminalTabOptions,
  ) => Promise<void>;
  /** Start a new process in the ended tab `sessionId` of `workspaceId`, under
   *  the same id and in the same place (its host's `terminal_respawn_tab`):
   *  Restart and Relaunch on an ended pane. The shell's rules (`opts`) are a
   *  launch's own; `scope` re-projects a scoped agent launch's MCP config. The
   *  tab's pane remounts on the new process. Resolves with the fresh scope's
   *  launch arguments; rejects with the host's refusal. */
  respawnTerminalTab: (
    teamKey: string,
    sessionId: string,
    workspaceId: string,
    opts?: RespawnTerminalTabOptions,
  ) => Promise<{ extraArgs: string[]; removeArgs?: string[]; execution?: ScopedExecution }>;
  /** SW-GW-2: open a new terminal tab in an EXISTING workspace and RETURN its
   *  `sessionId` right away (the addressable handle); `role` optionally tags the
   *  tab (e.g. a generic-agent tab). Returns `""` when no such workspace is open
   *  (so no orphan tab is opened). */
  openTabInWorkspace: (
    teamKey: string,
    workspaceId: string,
    opts?: { role?: string; name?: string },
  ) => string;
  /** Close a terminal tab through its host: an idle shell (or an exited tab)
   *  goes, a busy one keeps running out of sight (`kill` ends it regardless).
   *  Closing the last ordinary tab opens a fresh one unless `replace: false`.
   *  Resolves once the host answered and this viewer no longer shows it (a
   *  refusal is logged, never thrown). */
  removeTerminalTab: (
    teamKey: string,
    tabId: string,
    workspaceId?: string,
    opts?: { kill?: boolean; replace?: boolean; throwOnError?: boolean },
  ) => Promise<void>;
  /** SW-DX-17a: bring a running session into a workspace as its focused tab —
   *  its host moves the entry there (from out of sight, or another workspace).
   *  Absent `workspaceId` → the active workspace. */
  adoptTerminalSession: (
    teamKey: string,
    sessionId: string,
    workspaceId?: string,
    opts?: { isCurrent?: () => boolean },
  ) => Promise<void>;
  /** Hide a tab: its host moves it off its tile to the workspace's Background
   *  entries — its Hidden tabs, listed under the tray chip — still running (no
   *  kill, no replacement tab, its status kept). Returns false — and does
   *  nothing — when it is the tile's only tab (hide the tile instead). */
  hideTerminalTab: (teamKey: string, tabId: string, workspaceId: string) => boolean;
  /** Put a Hidden tab back on its tile (restoring the tile from the tray first
   *  when it is hidden too), active and reattached to the same session. */
  restoreHiddenTerminalTab: (teamKey: string, workspaceId: string, tabId: string) => void;
  /** SW-GW-5: close the tab bound to `sessionId` wherever it lives — open,
   *  minimized, or Hidden — for callers that hold only a session id.
   *  Rejects if the host close fails, so SDK callers can report the real result. */
  removeTerminalTabBySession: (sessionId: string, opts?: { kill?: boolean }) => Promise<void>;
  setActiveTerminalTab: (teamKey: string, tabId: string, workspaceId?: string) => void;
  /** Cycle the active workspace's terminal tabs by one, with wrap. ≤1 tab is a
   *  no-op. Keyboard sibling of the tab-bar click (`setActiveTerminalTab`).
   *  Returns `true` when it moved, `false` on the ≤1-tab no-op. */
  cycleTerminalTab: (teamKey: string, direction: "next" | "prev") => boolean;
  /** SW-EW-28: show/hide a workspace's story HUD. `shown` omitted → toggle;
   *  provided → set explicitly. Hiding also resets the tile back to compact. */
  toggleStoryHudShown: (workspaceId: string, shown?: boolean) => void;
  /** SW-EW-28: flip a workspace's HUD between compact card and expanded full
   *  story. `expanded` omitted → toggle; provided → set explicitly. */
  toggleStoryHudSize: (workspaceId: string, expanded?: boolean) => void;
  /** Show/hide a terminal tab's source-diff overlay. Keyed by `sessionId` — a
   *  tile with multiple tabs tracks each independently. `workspaceId` + `teamKey`
   *  carry context: the sibling-overlay-closer (StoryHud is still per-workspace)
   *  needs the wsId, and the single-source slot stores the triple so an eviction
   *  can tear down the previous occupant's state without a lookup. `shown`
   *  omitted → toggle; provided → set explicitly. */
  toggleSourceDiffShown: (opts: {
    sessionId: string;
    workspaceId: string;
    teamKey: string;
    shown?: boolean;
  }) => void;
  /** SLW-WD-2 + SW-TP CAP-3: set the source-diff overlay's selected file for a
   *  tab (the file whose redline the expanded reader shows), or `null` to clear.
   *  Keyed by `sessionId` so siblings on the same workspace carry independent
   *  selections. */
  setSourceDiffSelectedFile: (sessionId: string, filePath: string | null) => void;
  /** SLW-WD-9 + SW-TP CAP-3: set the source-diff rail mode for a tab —
   *  `true` = History, `false` = Working. Both the adaptive pick and an explicit
   *  toggle call this; once set it sticks (a nonce refresh never re-adapts) and
   *  is reset only on hide/close. Change-only. */
  setSourceDiffHistoryMode: (sessionId: string, historyMode: boolean) => void;
  /** SLW-WD-9 + SW-TP CAP-3: select a commit in History mode by `sha`, or
   *  `null` for the pinned Working-tree node. Keyed per-session. Change-only. */
  setSourceDiffSelectedSha: (sessionId: string, sha: string | null) => void;
  setSourceDiffBaseMode: (sessionId: string, mode: SourceDiffBaseMode) => void;
  /** SW-TP CAP-3: flip one source-diff view option (inline / wordWrap /
   *  expandAll / autoFollow / ignoreWhitespace) for a tab. Pass `value` to set
   *  explicitly, or omit to toggle relative to the option's resolved current
   *  value (so the first flip of an unset tab moves off its default). Keyed
   *  per-session. Change-only. */
  toggleSourceDiffOption: (sessionId: string, key: SourceDiffViewOption, value?: boolean) => void;
  /** Open/close a terminal tab's worktree code editor. Keyed by `sessionId`;
   *  takes `workspaceId` + `teamKey` for the same reasons as
   *  {@link toggleSourceDiffShown}. The editor resumes its remembered file
   *  (`worktreeEditorFile[workspaceId]`) on open — session memory keyed by the
   *  owning workspace, not by tab (two tabs of one workspace share the resume
   *  file). `open` omitted → toggle; provided → set explicitly. Change-only. */
  toggleWorktreeEditorOpen: (opts: {
    sessionId: string;
    workspaceId: string;
    teamKey: string;
    open?: boolean;
  }) => void;
  /** SLW-DV-3 (session memory): the worktree editor's remembered file (repo-
   *  relative), so re-opening resumes where the user was. `null` = the tree root.
   *  Set as the user browses the inset tree, and by the diff's "Open code editor"
   *  button (seeds from the diff selection). Change-only. */
  setWorktreeEditorFile: (workspaceId: string, filePath: string | null) => void;
  /** Arm (or, with `null`, clear) the editor's one-shot open-at-line for a
   *  workspace. Change-only. */
  setWorktreeEditorRevealLine: (
    workspaceId: string,
    reveal: { path: string; line: number } | null,
  ) => void;
  /** SLW-DV-9: mirror (or clear) the open editor's unsaved-edit draft for a
   *  workspace. `draft` = the dirty `{ relPath, buffer, lastSaved }` to preserve
   *  across an unfocus unmount; `null` clears it (buffer saved/discarded/clean).
   *  Change-only (a no-op clear on an already-absent key never re-renders). */
  setWorktreeEditorDraft: (
    workspaceId: string,
    relPath: string,
    draft: WorktreeEditorDraft | null,
  ) => void;
  /** Remember the editor's open tabs for a workspace (change-only). */
  setWorktreeEditorTabs: (workspaceId: string, tabs: string[]) => void;
  /** SLW-DV-8b (RD-3): set a workspace's code-diff mode (line / ignore-whitespace /
   *  structural). Change-only. */
  setSourceDiffMode: (sessionId: string, mode: SourceDiffMode) => void;
  /** SLW-WDE-3: signal an external worktree change for a workspace's open editor
   *  inset (bumps `worktreeChangedPath[workspaceId]` with a fresh nonce). Driven
   *  by the App-level `worktree-file-change` listener. */
  bumpWorktreeChangedPath: (workspaceId: string, path: string, resumed?: boolean) => void;
  /** Publish the open-file lifecycle for `path` (normalized). A non-`active` value records it;
   *  `null`/`"active"` deletes the entry (absence ≡ active). Called by the rendering viewer
   *  from its existing `readFile` outcome — no new IPC. */
  setOpenFileLifecycle: (path: string, lifecycle: SubjectLifecycle | null) => void;
  /** SW-FX-52: O(1) agent-status write — set (or clear, when `status` is
   *  `undefined`) the status for one PTY session in the flat `aiStatusBySession`
   *  slice. No workspace/shadow scan; team-agnostic (session ids are global).
   *  No-op when the value is unchanged, so the slice identity is stable across
   *  redundant events. */
  setSessionAiStatus: (sessionId: string, status: TerminalWireStatus | undefined) => void;
  /** SW-AM-4: O(1) live-progress write — set (or clear, when `progress` is
   *  `undefined`) the retained progress for one PTY session in the flat
   *  `progressBySession` slice. Mirrors `setSessionAiStatus`: no workspace/shadow
   *  scan, team-agnostic, no-op when unchanged so the slice identity stays stable
   *  across redundant ticks (only the subscribed row re-renders). */
  setSessionProgress: (sessionId: string, progress: SessionProgress | undefined) => void;
  switchWorkspace: (teamKey: string, workspaceId: string) => void;
  /** T.1: first arg re-keyed `repoPath` → `teamKey`. Closes the workspace
   *  identified by `workspaceId` in `workspaces[teamKey]`. T.2 D-11:
   *  `bypassRootGuard` is set only by `ensureRootWorkspaces`' remove
   *  branch — user-facing paths never set it, so roots stay non-closable
   *  through the dialog/button gates. See `closeTeam` (T.1 AC-11) for
   *  team-level cleanup. */
  closeWorkspace: (
    teamKey: string,
    workspaceId: string,
    opts?: {
      bypassRootGuard?: boolean;
      killTerminals?: boolean;
      /** With `killTerminals`: the user confirmed End, so a remote Machine's
       *  tabs end too. Every other close only detaches them. */
      endRemote?: boolean;
    },
  ) => void;
  /** SW-SO-1: mark a workspace's open as in its pre-record "opening" window
   *  (drives the decoupled opening overlay). Idempotent add. */
  beginWorkspaceOpen: (teamKey: string, workspaceId: string) => void;
  /** SW-SO-1: clear the pre-record "opening" window flag. Idempotent — no-op if
   *  absent; drops the per-team set when it empties. */
  endWorkspaceOpen: (teamKey: string, workspaceId: string) => void;
  /** SW-SO-1 (AC-2): cancel an in-flight pre-record open — aborts the probe
   *  chain via the shared `storyWorkspaceAborts` handle (the same one
   *  `closeWorkspace` uses) AND drops the opening overlay immediately so Cancel
   *  feels instant even while a slow `findWorktreeOnDisk` git-spawn resolves. */
  cancelWorkspaceOpen: (teamKey: string, workspaceId: string) => void;
  /** SW-WL-3 AC-1: mark a workspace surface open. Idempotent — fires the
   *  attach hook only on the 0→1 transition. */
  openWorkspaceWatch: (teamKey: string, workspaceId: string) => void;
  /** SW-WL-3 AC-1: release a workspace surface. No-op if not open — fires the
   *  detach hook only on the 1→0 transition. */
  closeWorkspaceWatch: (teamKey: string, workspaceId: string) => void;
  /** SW-WL-3: open a surface's watch only when it resolves to something
   *  watchable (non-empty `cwd`). The activation-gated root paths (mint +
   *  close-reassign) use this so an unresolvable root (synthetic source repo
   *  with no local clone yet, cwd "") does not latch open-set membership
   *  before it can attach — otherwise the later creating→ready open is a
   *  no-op and the surface renders ready-but-unwatched. */
  openActiveSurfaceWatch: (teamKey: string, workspaceId: string) => void;
  /** Tear down all team-keyed viewer state. Terminal sessions are ended by
   *  default; callers evicting only renderer state can preserve them. `null`
   *  is a no-op (cold-start branch). */
  closeTeam: (teamKey: string | null, opts?: { preserveTerminalSessions?: boolean }) => void;
  /** SW-HB-8: a team was renamed `oldName → newName` within `orgSlug`. Move
   *  every team-keyed viewer slice from the old teamKey to the new one (and
   *  rewrite `openedTeams`), so open tabs / terminals / layout survive the
   *  rename in place with no unmount. Applied AFTER the backend confirms. */
  rekeyTeam: (orgSlug: string, oldName: string, newName: string) => void;
  cleanupStoryWorktree: (repoPath: string, storyFilePath: string) => Promise<void>;
  /** T.4 D-3: replace the on-disk worktree entries for a single source
   *  repo within a team. The setter preserves entries for OTHER source
   *  repos in the same team bucket (composite-key prefix scan). Input
   *  `entries` is keyed by raw `storyFilePath`; the setter composes
   *  `${sourceRepoSlug}::${storyFilePath}` composite keys internally. */
  setOnDiskWorktrees: (
    teamKey: string,
    sourceRepoSlug: string,
    entries: Map<string, OnDiskWorktreeEntry>,
  ) => void;
  /** T.4 D-3: remove a single composite-keyed entry from the on-disk
   *  map (e.g. after cleanup). */
  removeOnDiskWorktree: (teamKey: string, sourceRepoSlug: string, storyFilePath: string) => void;
  /** Cycle the active workspace within its scope (flat order, or the selected
   *  rail's slice in grouped mode), with wrap. Returns `true` when it moved,
   *  `false` on the ≤1-scope / already-current no-op. */
  cycleWorkspace: (teamKey: string, direction: "next" | "prev") => boolean;
  /** Jump to the team's first root workspace. Returns `true` when it moved,
   *  `false` when already on that root. */
  jumpToRootWorkspace: (teamKey: string) => boolean;
  /**
   * T.5: ensures (or focuses) the story workspace under `teamKey`. The
   * story's source repo is read from frontmatter via the M.2 chain, so the
   * `repoPath` argument is gone — callers compute `teamKey` once at the
   * call site (active org/team or the panel's owning team) and let the
   * action resolve the source repo from `storyFilePath`. */
  openOrFocusStoryWorkspace: (
    teamKey: string,
    storyFilePath: string,
    expectedSlug?: string,
    /** SW-FX-41: optional start-point forwarded to `ensureWorktree` (its 3rd
     *  arg). The normal open path omits it (dep-chain resolution). The
     *  "Create from default" escape hatch passes the default branch so it
     *  reuses this one focus+spawn path instead of a divergent create-only
     *  path that never opened the worktree. Resolves to the worktree `cwd`. */
    overrideStartPoint?: string,
    /** SW-FX-69: opt into stealing focus (default no-steal). Applied at BOTH the
     *  `onCreating` reveal and the `finishForegroundOpen` tail via the shared
     *  focus policy, so a nav/dashboard story-open is additive unless it asks. */
    focus?: boolean,
    /** SW-AM-7: opt OUT of auto-spawning the workspace's first shell (default
     *  spawns — every existing caller is unchanged). The re-home path passes
     *  `false` so it can attach the ALREADY-RUNNING agent PTY as the sole tab,
     *  instead of a redundant fresh shell that would also hijack the storyId
     *  nav-registration (`registerNavWorkspaceSession` self-gates on empty tabs). */
    spawnFirstTerminal?: boolean,
    /** Where a NEW workspace lives, decided by the caller (an MCP open on a
     *  machine) instead of asking. An existing workspace opens where it is. */
    location?: WorkspaceLocationChoice,
    /** Superseded user navigation may finish creating in memory, but never select/reveal. */
    isCurrent?: () => boolean,
  ) => Promise<string>;
  /** SW-AM-7: ensure a story's CLI workspace is showing, keyed by its bare
   *  `story_id` — the reusable "open (or restore) the workspace this story lives
   *  in" primitive. Resolves the story's file path in-memory first (a live tile or
   *  soft-closed shadow), then falls back to a DURABLE disk scan
   *  (`scanTeamPipelineTree` → {@link storyFilePathFromTree}) so it works even
   *  after the tile was fully closed. Restores a soft-closed shadow / focuses a live
   *  tile via `activateTrayWorkspace`, else cold-opens via
   *  `openOrFocusStoryWorkspace`. Returns the target `workspaceId`, or `null` when
   *  the story can't be resolved (caller falls back to today's behavior). Async +
   *  decoupled: the open path never blocks the UI thread. `focus: false` shows it
   *  without taking focus: an already-showing record is left as it is. */
  ensureStoryWorkspaceOpen: (
    teamKey: string,
    storyId: string,
    /** `location`: a cold open creates the workspace there (and opens its
     *  first terminal there). */
    opts?: { focus?: boolean; location?: WorkspaceLocationChoice; isCurrent?: () => boolean },
  ) => Promise<string | null>;
  /** SLW-CO-6 (AC-1): the unified sync open primitive. Every non-story CLI
   *  workspace kind (epic, generic, born-minimized shadows) routes through
   *  this — see {@link OpenWorkspaceParams}. Owns the shared
   *  restore/focus/add/switch/expand/spawn spine + the born-minimized shadow
   *  path, so each former open action is a thin kind-specific wrapper. */
  openWorkspace: (params: OpenWorkspaceParams) => void;
  /** SLW-CO-7 (AC-1 review): clear a workspace's `needsSyncOverlay` latch. Called
   *  once the surface materializes (`synced` → one-shot, so a LATER unrelated wave
   *  on the same `cwd` — reverse-sync/reconcile — can't re-cover a live terminal)
   *  and by the materialize-stall watchdog (no wave / stuck wave → drop the
   *  overlay so the ready workspace isn't stranded behind it with no Cancel). */
  clearSyncOverlay: (teamKey: string, workspaceId: string) => void;
  /** SW-XS-10: fold a launch's Step 2 state (no operation id) into the setup
   *  line of the workspace it names (its directory, or its machine handle). */
  applyAgentPrepareProgress: (state: AgentPrepareState) => void;
  /** SW-XS-10: replace a workspace's setup line (a Retry restoring the line it
   *  had, a dismissed failure). Runtime-only. */
  setWorkspaceSetupState: (
    teamKey: string,
    workspaceId: string,
    state: WorkspaceSetupState | undefined,
  ) => void;
  /** SW-XS-10: re-run Step 1 for a workspace whose creation failed — a machine
   *  replays its request (same id, base and branch); This Mac creates it again. */
  retryWorkspaceCreate: (teamKey: string, workspaceId: string) => Promise<void>;
  /** SW-XS-10: `agent`'s Step 2 failed without an event (its launch refused
   *  before preparing); `code` is the backend's typed reason. */
  failAgentRun: (
    teamKey: string,
    workspaceId: string,
    /** The agent, or every name its launch knows it by (the run its Step 2
     *  events started is updated, whichever spelling it carries). */
    agent: string | readonly string[],
    reason: string,
    code?: string,
  ) => void;
  /** SW-XS-10: drop `agent`'s Step 2 run (dismissed, or retried); every other
   *  agent's run is kept. */
  clearAgentRun: (teamKey: string, workspaceId: string, agent: string) => void;
  /** SW-XS-10: replace (or clear) a workspace's kept-modified note. */
  setWorkspaceKeptModified: (
    teamKey: string,
    workspaceId: string,
    kept: KeptModified | undefined,
  ) => void;
  /** SW-EW-1c/EW-6: open (or focus) an epic's terminal workspace. Activates it
   *  and launches a terminal tab if none. The default (no `sourceRepoSlug`) is
   *  ONE epic-level terminal rooted at `cwd` = the epic workspace dir that holds
   *  every configured repo's worktree (the planning vantage point). Passing a
   *  `sourceRepoSlug` opens a per-repo terminal instead (id includes the slug;
   *  cwd falls back to that repo's clone root when no explicit `cwd` is given). */
  openOrFocusEpicWorkspace: (
    teamKey: string,
    epicDirName: string,
    sourceRepoSlug?: string,
    cwd?: string,
    epicDocPath?: string,
  ) => void;
  /** SW-GW-1: open (or focus) a generic CLI workspace rooted at `dirPath` — a
   *  plain terminal at any user-chosen directory, deduped by normalized path. */
  openOrFocusGenericWorkspace: (teamKey: string, dirPath: string) => void;
  /** SW-WV-A6: open (or focus) the team's ever-present team-folder workspace —
   *  a `kind:"generic"` CLI rooted at the team's context-repo content dir
   *  (`RepoState.teamContentPath`). Thin wrapper around
   *  `openOrFocusGenericWorkspace`; resolves the cwd from the first `RepoState`
   *  of the team and no-ops when that path is empty (an un-initialized team falls
   *  through to `TeamWorkspaceEmptyState`). The one integration point for the
   *  dispatch UI (SW-WV-F1/F2) and future MCP flows so nobody re-invents the
   *  "always a CLI" guarantee. */
  openOrFocusTeamFolderWorkspace: (teamKey: string) => void;
  /** R-18: Reconcile workspace frontmatter-derived fields (`storyStatus`,
   *  `storyId`, `sourceRepoSlug`) from the latest pipeline tree scan. Wired
   *  into the `repo-viewer-view.tsx` `useEffect` so any edit to a story's
   *  frontmatter propagates to every workspace surface within one watcher
   *  tick. `storyId` falls back to `storyIdFromPath(storyFilePath)` when
   *  `ref.storyId` is undefined (consistent with creation-time invariant). */
  syncWorkspaceFrontmatter: (teamKey: string, tree: DerivedPipelineTree) => void;
  /** Change a story workspace's pipeline status: optimistic update + Tauri write + rescan. */
  changeStoryStatus: (
    teamKey: string,
    workspaceId: string,
    newStatus: PipelineStatus,
  ) => Promise<void>;
  /**
   * Ensure a worktree exists for a story and return its `cwd`. Checks the
   * workspace store first, then on-disk detection via `findWorktreeOnDisk`,
   * then creates one (with progress popup) if needed. Rejects when config
   * is missing (shows setup-required modal) or setup fails/is cancelled.
   * Story 34.9 AC-4.
   *
   * T.5: re-signed to take `teamKey` (no `repoPath`). The source repo is
   * resolved from the story's frontmatter `source_repo` against the team's
   * `workspaceRepos` slice (D-6). When the story's source repo isn't tracked
   * by `teamKey`, the action throws a `WorktreeTeamMismatchError` (D-1) and
   * the caller's `.catch` surfaces a typed toast.
   */
  ensureWorktree: (
    teamKey: string,
    storyFilePath: string,
    overrideStartPoint?: string,
    expectedSlug?: string,
    /** SLW-CO-5: open-path hook. Invoked the moment the workspace record is
     *  inserted as `status:"creating"` (before `setupWorktree` runs) so the
     *  caller can switch to it and mount the terminal panel + inline overlay
     *  immediately, instead of blocking on setup. Both the normal open path and
     *  (SW-FX-41) the "Create from default" hatch pass it — the latter delegates
     *  through `openOrFocusStoryWorkspace`. Non-open callers (retry) omit it —
     *  the record still appears + flips to ready. */
    opts?: {
      onCreating?: () => void;
      /** RF-perf-14 (decouple): force the BLOCKING behavior even for an
       *  interactive (`onCreating`) open — resolve only after `setup:` finishes,
       *  rolling back on failure. Default (undefined/false) for `onCreating`
       *  opens is the DECOUPLED path: flip ready + mount the instant the dir
       *  exists, run setup in the background. Non-`onCreating` callers (agent
       *  launch, retry) are always blocking regardless — they need a fully
       *  prepared worktree on resolve, so the terminal isn't the discriminator. */
      awaitSetup?: boolean;
      /** SW-XS-10: a cancel leaves the record for the caller (a create
       *  retry restores its failure line) instead of closing the workspace. */
      keepRecordOnCancel?: boolean;
      /** SW-AM-7: opt OUT of auto-spawning the first shell when the worktree
       *  materializes (default spawns). The re-home cold-open passes `false` so it
       *  can attach the agent's ALREADY-RUNNING PTY as the sole tab instead of a
       *  redundant fresh shell. */
      spawnFirstTerminal?: boolean;
      /** Where a new workspace lives, already decided (no Where question). */
      location?: WorkspaceLocationChoice;
    },
  ) => Promise<string>;
  /** RF-perf-14 (decouple): re-run a FAILED background `setup:` for a workspace
   *  whose retry badge the user clicked. No-op when there is no pending retry
   *  (e.g. setup already succeeded or the workspace was closed). */
  retryWorktreeSetup: (teamKey: string, workspaceId: string) => void;
  /**
   * SW-WB-1: reconcile a story's worktree onto its intended branch after the
   * user edits `branch_name` (the editable INTENT). Fetch-aware, never-clobber:
   *  - clean tree → **recreate**: close the old workspace, remove the old
   *    worktree, and re-establish a fresh one at the new branch-derived path
   *    (reuses `ensureWorktree` — no duplicated establish/derive machinery);
   *  - dirty tree → **migrate in place**: `switchWorktreeBranch` keeps the same
   *    path + running session, carrying WIP across (throws a typed error the
   *    caller surfaces + offers recreate if the switch would clobber);
   *  - already aligned / no worktree → **noop** (or a plain create).
   * Non-blocking: all git work runs through async commands behind the existing
   * open overlay; never called on a render path.
   */
  reconcileWorktreeBranch: (
    teamKey: string,
    storyFilePath: string,
    /** AC-7: `forceRecreate` runs the recreate path even on a dirty tree —
     *  wired to the "Recreate (discards local changes)" affordance the caller
     *  offers after a blocked migrate. Never silently discards on its own. */
    opts?: { forceRecreate?: boolean },
  ) => Promise<{ path: string; mode: "recreate" | "migrate" | "noop" }>;
  /** F-6 AC-8: set/clear the no-local-clone-attached prompt. The optional
   *  `retry` callback fires automatically once the user attaches a clone
   *  (see `noClonePromptRetry` for details).
   *
   *  T.4 AC-3: signature reshape — `(teamKey, sourceRepoSlug, retry?)`. */
  showNoClonePrompt: (teamKey: string, sourceRepoSlug: string, retry?: () => void) => void;
  dismissNoClonePrompt: () => void;
  /** Internal — fires the registered retry callback once the synthetic's
   *  `localPath` populates after a successful attach. */
  consumeNoClonePromptRetry: () => void;
  /**
   * PT-10: Hide a workspace into the tray — keep running. Thin pass-through to
   * `closeWorkspace` (no confirm dialog): the viewport panels detach and the PTYs
   * survive in the tray shadow. No-op for the root workspace. `hideWorkspace`
   * (the – button, the tray menu's Hide, ⌘⇧W) routes here.
   */
  requestCloseWorkspace: (teamKey: string, workspaceId: string) => void;
  /**
   * PT-10: reopen a closed-but-running workspace. Re-adds its shadowed
   * `Workspace` record to the bar, restores its tab list (the survived
   * terminals' displays move back into its slots), focuses it, and clears the
   * shadow. No-op if no shadow exists. Shared by every reopen entry point
   * (panel row, close toast, `openOrFocusStoryWorkspace`).
   */
  reopenSoftClosedWorkspace: (
    teamKey: string,
    workspaceId: string,
    target?: TilePlacementTarget | { center: true },
    opts?: { keepLayout?: boolean },
  ) => void;
  /**
   * PT-10: end every terminal of a minimized (tray) workspace — its tabs and
   * its Hidden tabs — then drop its shadow. Backs End on a hidden tray chip.
   * No-op if no shadow exists.
   */
  killTerminalsForSoftClosedWorkspace: (
    teamKey: string,
    workspaceId: string,
    opts?: { endRemote?: boolean },
  ) => void;
  /** SW-WT-9: resolve the open setup-approval gate (true = run, false = skip). */
  resolveWorktreeSetupApproval: (approved: boolean) => void;
  cycleTab: (teamKey: string, tabType: PaneType, direction: 1 | -1) => void;
  reorderTerminalTab: (
    teamKey: string,
    fromIdx: number,
    toIdx: number,
    workspaceId?: string,
  ) => void;
  /** Move a workspace anywhere in its team's shared order (pills, pane cards and tiles). Before
   *  a team's computed order is known (its tree never synced, no base saved) the move applies to
   *  the tree-less order shown, and is made again over the real base once the tree syncs.
   *  `base` is the computed order the caller drew (the tray's, from the tree it holds), so the
   *  move lands where its line showed even before the store's saved base catches up.
   *  Returns whether the order changed. */
  reorderWorkspace: (
    teamKey: string,
    workspaceId: string,
    place: OrderPlace,
    base?: readonly string[],
  ) => boolean;
  renameTerminalTab: (
    teamKey: string,
    tabId: string,
    name: string,
    workspaceId?: string,
    automatic?: boolean,
  ) => void;
  navigateBack: (teamKey: string, repoPath: string) => void;
  navigateForward: (teamKey: string, repoPath: string) => void;
  setPipelineScope: (teamKey: string, scopePath: string) => void;
  /** SW-XS-15 — set the per-team context pivot (`null` = Team (everything)). */
  setSelectedAgentContext: (teamKey: string, selection: AgentContextSelection | null) => void;
  setScopeRecoveryComplete: (done: boolean) => void;
  /**
   * Record that `filePath` is the active file for the active team. Moves
   * the path to the front of the per-team keep-alive LRU, deduping and
   * capping at `VIEWER_PERF.keepAliveMaxViewers`. Called from
   * `RepoViewerView` whenever `activeTabPath` changes — the sole
   * driver of keep-alive state. Idempotent. T.4: keyed by `teamKey`.
   */
  touchViewerPath: (teamKey: string, filePath: string) => void;
  persistState: () => void;
  flushPersist: () => Promise<void>;
  // SW-SN-12: flush the pending scratch-note body write at teardown boundaries
  // (app quit, window blur, panel unmount). Separate from `flushPersist` (which
  // flushes layout/config) so note-save timing isn't coupled to layout events.
  flushNotes: () => Promise<void>;
  loadPersistedState: () => Promise<void>;
}

/** Derive a tab's display name from its absolute file path. */
function tabNameFromPath(filePath: string): string {
  // A synthetic "virtual" tab (`virtual://` scheme) shows its registered
  // label, not a path-derived basename. Falls through for real files and for
  // virtual paths registered without a title.
  const virtualTitle = resolveVirtualTabTitle(filePath);
  if (virtualTitle) return virtualTitle;
  return stripViewerExtension(filePath.split("/").pop() ?? filePath);
}

/** A committed (non-preview) tab built from a file path — the shape shared by
 *  `openFile`'s append, history re-add (`commitNavTarget`), `setActiveTab`'s
 *  ensure-tab guard, and the SLW-FX-12 reconcile. Centralised so the four
 *  re-add sites can't drift on tab shape or name derivation. */
function committedTab(path: string): TabState {
  return { path, name: tabNameFromPath(path) };
}

// --- RF-4: shared pure tab-array mutators ---
// The array logic of the tab actions, kept pure; side-effects (nav history,
// persistState, revealOpenedFile, dirty-edit guards) stay in the action bodies.

/**
 * RF-4: pin an open preview tab in place (commit). Returns the SAME array
 * reference when the path is not open as a preview (no-op), so an unrelated pin
 * never churns slice identity.
 */
export function pinPreviewInTabs(tabs: TabState[], filePath: string): TabState[] {
  const idx = tabs.findIndex((t) => t.path === filePath);
  if (idx < 0 || !tabs[idx].preview) return tabs;
  const next = [...tabs];
  next[idx] = { ...next[idx], preview: false };
  return next;
}

/**
 * RF-4: place `newPreview` into the single preview slot — replace the current
 * preview tab in place (no flicker as the user arrow-keys through a folder), or
 * append when none is open. The caller builds `newPreview`.
 */
export function replacePreviewSlot(tabs: TabState[], newPreview: TabState): TabState[] {
  const previewIdx = tabs.findIndex((t) => t.preview);
  if (previewIdx >= 0) {
    const next = [...tabs];
    next[previewIdx] = newPreview;
    return next;
  }
  return [...tabs, newPreview];
}

/** `tabDiff` without one closed tab's entry (the same record when it had none). */
function withoutTabDiff(
  tabDiff: Record<string, Record<string, TabDiff>>,
  teamKey: string,
  filePath: string,
): Record<string, Record<string, TabDiff>> {
  const cur = tabDiff[teamKey];
  if (!cur?.[filePath]) return tabDiff;
  const { [filePath]: _closed, ...rest } = cur;
  return { ...tabDiff, [teamKey]: rest };
}

/**
 * RF-4: after removing the tab that was at `removedIdx` in the original array,
 * pick the neighbor path to activate — same index, clamped to the new end; ""
 * when nothing remains.
 */
export function neighborPathAfterClose(remaining: TabState[], removedIdx: number): string {
  return remaining[Math.min(removedIdx, remaining.length - 1)]?.path ?? "";
}

/**
 * SLW-FX-12: enforce the invariant that every `activeTab[tk]` references a
 * member of `openTabs[tk]`. The viewer body renders off `activeTab[tk]` while
 * the title-bar tab strip gates off `openTabs[tk]` (`title-bar.tsx` `hasTabs`),
 * so a divergence — an `activeTab[tk]` with no backing tab — shows a doc with no
 * tab (the "file open, no tab" cold-launch symptom). Converge toward the user's
 * intent: an unbacked REAL path is healed by RECONSTRUCTING its tab (keep both
 * the doc AND a visible tab); a virtual/empty pointer is DROPPED (SW-GS-2b's
 * "lone virtual leaves no active pointer" precedent — a virtual path can't be
 * persisted as a real tab). Pure; returns the SAME references when nothing
 * changed so Zustand's Object.is comparison stays cheap.
 */
export function reconcileActiveTabs(
  openTabs: Record<string, TabState[]>,
  activeTab: Record<string, string>,
): { openTabs: Record<string, TabState[]>; activeTab: Record<string, string> } {
  let nextOpen = openTabs;
  let nextActive = activeTab;
  for (const [tk, path] of Object.entries(activeTab)) {
    const tabs = nextOpen[tk] ?? [];
    if (tabs.some((t) => t.path === path)) continue; // already backed — OK
    if (!path || isVirtualTabPath(path)) {
      // Unrecoverable pointer → drop it (this team has no active tab).
      if (nextActive === activeTab) nextActive = { ...activeTab };
      delete nextActive[tk];
      continue;
    }
    // Real, unbacked path → reconstruct its tab so the user keeps doc + tab.
    if (nextOpen === openTabs) nextOpen = { ...openTabs };
    nextOpen[tk] = [...tabs, committedTab(path)];
  }
  return { openTabs: nextOpen, activeTab: nextActive };
}

/**
 * T.4: `getActiveTeamKey` lives in `@/shared/lib/team-key` (alongside
 * `buildTeamKey` / `useActiveTeamKey`); re-exported here for backwards
 * compatibility with tests importing from `./viewer-store`.
 */
export { getActiveTeamKey };

/**
 * T.4 D-3: derive a canonical `sourceRepoSlug` from a synthetic
 * workspace repoPath (`workspace:org:team:owner/repo`). Falls back to
 * the verbatim `repoPath` for legacy real-path repos (those go away in
 * T.6). Always lowercased so composite-key lookups match emit-time
 * casing.
 */
export function deriveSourceRepoSlug(repoPath: string): string {
  const parsed = parseSyntheticRepoKey(repoPath);
  if (parsed) return `${parsed.owner}/${parsed.repo}`.toLowerCase();
  // T.4 Phase 8 review (Cluster H / D3): legacy real-path fallback retires
  // in T.6. `composeWorktreeKey` uses `::` as a separator and does not
  // escape it; a real-path containing `::` (rare but possible) would
  // produce composite-key collisions. Surface a warning during the
  // transition window so we have telemetry if it ever happens in the wild.
  if (repoPath.includes("::")) {
    logger.warn(
      "viewer",
      `deriveSourceRepoSlug: legacy real-path contains '::' — composite-key collision risk: ${repoPath}`,
    );
  }
  return repoPath;
}

/**
 * T.4 AC-3: extract `(teamKey, sourceRepoSlug)` from a synthetic
 * repoPath for matching against `noClonePromptRepo`. Returns `null` for
 * legacy non-synthetic repos —
 * those can never match a prompt (prompts are only raised for
 * synthetic workspace repos that lack a local clone).
 *
 * Exported so UI consumers (settings, send-to-terminal) can derive the
 * new `showNoClonePrompt(teamKey, slug, retry?)` arguments from a
 * `RepoState.repoPath` they have in hand.
 */
export function promptIdentityFromRepoPath(
  repoPath: string,
): { teamKey: string; sourceRepoSlug: string } | null {
  const parsed = parseSyntheticRepoKey(repoPath);
  if (!parsed) return null;
  return {
    teamKey: buildTeamKey(parsed.orgSlug, parsed.team),
    sourceRepoSlug:
      `${parsed.host ? `${parsed.host}/` : ""}${parsed.owner}/${parsed.repo}`.toLowerCase(),
  };
}

/**
 * T.4 D-3: compose the canonical inner Map key for `onDiskWorktrees`.
 * The composite disambiguates stories
 * that share a file path across multiple source repos in the same team
 * — e.g. `_team/default/content/in-progress/foo/F-1.md` hosted in both
 * `acme/web` and `acme/api`. Story file path is NFC-normalised first so
 * the key aligns with the rest of the worktree pipeline (P5).
 */
export function composeWorktreeKey(sourceRepoSlug: string, storyFilePath: string): string {
  return `${sourceRepoSlug.toLowerCase()}::${normalizeStoryFilePath(storyFilePath)}`;
}

/**
 * T.4 D-3: compose a team-prefixed worktree key for module-global Sets
 * (`cleanupInFlight`). Teamkey-prefixed because the Set is module-global, not
 * per-team; `closeTeam` sweeps entries with the closing team's prefix.
 */
export function composeTeamWorktreeKey(
  teamKey: string,
  sourceRepoSlug: string,
  storyFilePath: string,
): string {
  return `${teamKey}::${composeWorktreeKey(sourceRepoSlug, storyFilePath)}`;
}

/**
 * T.4 D-8: DRY helper for `closeTeam`'s team-level teardown. Given a list
 * of slice names whose outer key is `teamKey`, returns a `Partial<ViewerState>`
 * with `{[teamKey]: _, ...rest}` for each named slice. Replaces the giant
 * destructure block that would otherwise accumulate in `closeTeam` as
 * more team-keyed slices come online.
 *
 * Caller is responsible for ensuring every `sliceName` references a slice
 * whose value type is `Record<string, unknown>` — TS narrows via the
 * generic constraint but the helper does not validate runtime shape.
 */
export function deleteFromKeyedSlices<K extends keyof ViewerState>(
  state: ViewerState,
  teamKey: string,
  sliceNames: readonly K[],
): Partial<ViewerState> {
  const partial: Partial<ViewerState> = {};
  for (const name of sliceNames) {
    const current = state[name];
    if (current && typeof current === "object" && !Array.isArray(current)) {
      const { [teamKey]: _drop, ...rest } = current as Record<string, unknown>;
      (partial as Record<string, unknown>)[name as string] = rest;
    }
  }
  return partial;
}

function replaceTeamBucket<T>(buckets: Record<string, T>, teamKey: string, value: T | undefined) {
  return value === undefined
    ? removeTeamKeyed(buckets, teamKey)
    : updateTeamKeyed(buckets, teamKey, value);
}

/**
 * SW-HB-8: sibling of {@link deleteFromKeyedSlices} — for a team rename, MOVE
 * each keyed slice's `oldKey` entry to `newKey` (delete old, set new) so open
 * tabs / terminals / layout survive the rename in place. Only slices that
 * actually hold `oldKey` are emitted, so untouched slices stay `Object.is`-
 * stable. Every `TEAM_KEYED_SLICES` member is keyed by the teamKey at the outer
 * level (composite-inner-key slices still have a teamKey OUTER key), so the outer-key move is uniform.
 */
export function rekeyKeyedSlices<K extends keyof ViewerState>(
  state: ViewerState,
  oldKey: string,
  newKey: string,
  sliceNames: readonly K[],
): Partial<ViewerState> {
  if (oldKey === newKey) return {};
  const partial: Partial<ViewerState> = {};
  for (const name of sliceNames) {
    const current = state[name];
    if (current && typeof current === "object" && !Array.isArray(current)) {
      const rec = current as Record<string, unknown>;
      if (!(oldKey in rec)) continue;
      const { [oldKey]: moved, ...rest } = rec;
      (partial as Record<string, unknown>)[name as string] = { ...rest, [newKey]: moved };
    }
  }
  return partial;
}

/**
 * SW-HB-8 / SLW-IN-7: after {@link rekeyKeyedSlices} relocates a team's slices
 * to `newKey`, rewrite the flat `/{old}/` team segment → `/{new}/` inside the
 * ABSOLUTE content paths those slices carry (`{org_root}/{team}/content/...`),
 * so open story tabs / drill root / expanded nav re-point at the moved dir
 * instead of rendering a "file deleted" placeholder. Operates ONLY on the
 * `newKey` entry of the path-bearing slices present in `moved`. The segment is
 * anchored on the following layer marker (`content`/…) so a coincidental
 * `/{oldName}/` elsewhere in the path isn't rewritten; source-repo file tabs
 * carry no such team-segment, so the swap is a safe no-op for them. Team names
 * are slug-safe (no regex metacharacters). Returns only the slices it actually
 * changed (Object.is-stable).
 */
export function rewriteTeamContentPaths(
  moved: Partial<ViewerState>,
  newKey: string,
  oldName: string,
  newName: string,
): Partial<ViewerState> {
  const fromRe = new RegExp(`/${oldName}/(content|install|repos|libraries)(/|$)`);
  const fix = (p: string): string => p.replace(fromRe, `/${newName}/$1$2`);
  const fixTab = (t: TabState): TabState => {
    const path = fix(t.path);
    return path === t.path ? t : { ...t, path };
  };
  const out: Partial<ViewerState> = {};

  for (const name of ["openTabs"] as const) {
    const rec = moved[name];
    const tabs = rec?.[newKey];
    if (tabs) out[name] = { ...rec, [newKey]: tabs.map(fixTab) };
  }
  for (const name of ["activeTab", "drillPaths"] as const) {
    const rec = moved[name];
    const val = rec?.[newKey];
    if (typeof val === "string") out[name] = { ...rec, [newKey]: fix(val) };
  }
  // Path-string arrays: `statusNavExpandedPaths` (expanded nav dirs) and
  // `navHistory` (back/forward stack — nav-back would otherwise re-open a
  // moved-away path as a fresh "file deleted" tab, re-polluting the tab slices
  // this fn just corrected).
  for (const name of ["statusNavExpandedPaths", "navHistory"] as const) {
    const rec = moved[name];
    const arr = rec?.[newKey];
    if (arr) out[name] = { ...rec, [newKey]: arr.map(fix) };
  }

  return out;
}

/** PT-10: immutably drop one `[teamKey][workspaceId]` entry from the
 *  closed-but-running shadow. Shared by the reopen + kill actions. */
function clearSoftClosedEntry(
  state: ViewerState,
  teamKey: string,
  workspaceId: string,
): ViewerState["softClosedWorkspaces"] {
  const teamShadow = { ...readTeamKeyed(state.softClosedWorkspaces, teamKey, {}) };
  delete teamShadow[workspaceId];
  return updateTeamKeyed(state.softClosedWorkspaces, teamKey, teamShadow);
}

/** SW-EW-33: push a workspace to the top of a team's MRU stack (most-recent
 *  first), de-duping any prior occurrence. Pure. */
function pushMruStack(
  stacks: Record<string, string[]>,
  teamKey: string,
  workspaceId: string,
): Record<string, string[]> {
  const prev = stacks[teamKey] ?? [];
  if (prev[0] === workspaceId) return stacks; // already on top — no churn
  return { ...stacks, [teamKey]: [workspaceId, ...prev.filter((id) => id !== workspaceId)] };
}

/** The state slices a soft-close (minimize) clears for one workspace + the
 *  closed-but-running shadow it stashes. */
type SoftCloseSlices = Pick<
  ViewerState,
  | "storyHudShown"
  | "storyHudExpanded"
  | "sourceDiffShownBySession"
  | "sourceDiffSelectedFile"
  | "sourceDiffHistoryMode"
  | "sourceDiffSelectedSha"
  | "sourceDiffBaseMode"
  | "sourceDiffViewOptions"
  | "worktreeEditorOpenBySession"
  | "worktreeEditorFile"
  | "worktreeEditorRevealLine"
  | "worktreeEditorDraft"
  | "worktreeEditorTabs"
  | "sourceDiffMode"
  | "worktreeChangedPath"
  | "softClosedWorkspaces"
>;

/** The per-tile FLOATING work-surface overlays that are MUTUALLY EXCLUSIVE — a
 *  workspace tile shows at most one at a time (story/epic HUD, source diff, code
 *  editor). A new per-tile overlay added
 *  here also needs its own expanded-reset branch in `closeSiblingOverlays`. */
// SW-TP D1: the "source" overlay member retires with the SourceFolderOverlay.
type TileOverlay = "storyHud" | "sourceDiff" | "worktreeEditor";

/** Opening one tile overlay closes the other two for that workspace. Returns a
 *  change-only patch (only the currently-open siblings are touched) that turns
 *  them off, resetting a hidden HUD to compact (its own hide
 *  semantics). The source diff keeps its selected file/mode and the code editor
 *  keeps its remembered file + unsaved draft (SLW-DV-9) — closing them this way is
 *  a silent switch, not a reset or discard. Enforced in the store so EVERY open
 *  path (dropdown, hotkeys, the diff's "Open code editor" button) upholds it. */
function closeSiblingOverlays(
  s: ViewerState,
  workspaceId: string,
  sessionId: string | null,
  keep: TileOverlay,
): Partial<ViewerState> {
  const patch: Partial<ViewerState> = {};
  if (keep !== "storyHud" && (s.storyHudShown[workspaceId] ?? false)) {
    patch.storyHudShown = { ...s.storyHudShown, [workspaceId]: false };
    if (s.storyHudExpanded[workspaceId]) {
      patch.storyHudExpanded = { ...s.storyHudExpanded, [workspaceId]: false };
    }
  }
  if (sessionId && keep !== "sourceDiff" && (s.sourceDiffShownBySession[sessionId] ?? false)) {
    patch.sourceDiffShownBySession = {
      ...s.sourceDiffShownBySession,
      [sessionId]: false,
    };
  }
  if (
    sessionId &&
    keep !== "worktreeEditor" &&
    (s.worktreeEditorOpenBySession[sessionId] ?? false)
  ) {
    patch.worktreeEditorOpenBySession = {
      ...s.worktreeEditorOpenBySession,
      [sessionId]: false,
    };
  }
  return patch;
}

/** SW-GS-2d: immutably set one workspace's soft-close shadow, building the nested
 *  `softClosedWorkspaces[teamKey][workspaceId]` map. The born-minimized agent
 *  create + its shadow-tab add/remove all route through this (one spread, not three). */
function patchSoftClosed(
  softClosedWorkspaces: Record<string, Record<string, SoftClosedWorkspace>>,
  teamKey: string,
  workspaceId: string,
  nextShadow: SoftClosedWorkspace,
): Record<string, Record<string, SoftClosedWorkspace>> {
  return {
    ...softClosedWorkspaces,
    [teamKey]: {
      ...(softClosedWorkspaces[teamKey] ?? {}),
      [workspaceId]: nextShadow,
    },
  };
}

/** SW-EW-33: the shared soft-close transform — clears a workspace's per-workspace
 *  overlay slices and (when `keepRunning`) stashes its minimized shadow: the
 *  record plus the CLI's layout mode. Its terminals stay in their host's
 *  layout either way. Factored out of `closeWorkspace` so
 *  `replaceFocusedLeafWorkspace` captures the displaced workspace byte-identically
 *  (no leaked HUD/progress state). Pure. */
function captureSoftCloseShadow(
  s: ViewerState,
  teamKey: string,
  workspaceId: string,
  workspace: Workspace,
  keepRunning: boolean,
): SoftCloseSlices {
  const storyHudShown = { ...s.storyHudShown };
  delete storyHudShown[workspaceId];
  const storyHudExpanded = { ...s.storyHudExpanded };
  delete storyHudExpanded[workspaceId];
  // Session-keyed overlay visibility: a workspace soft-close drops every
  // SESSION entry belonging to this workspace's tabs. Deriving the sid set
  // from the terminal projection keeps this safe even for a workspace with
  // multiple tabs or an un-rendered one.
  const sidsForWs = selectWorkspaceTabs(s, teamKey, workspaceId).map((t) => t.sessionId);
  const sourceDiffShownBySession = sidsForWs.reduce((acc, sid) => {
    if (sid in acc) {
      acc = { ...acc };
      delete acc[sid];
    }
    return acc;
  }, s.sourceDiffShownBySession);
  // SW-TP CAP-3: the diff content state is now sessionId-keyed, so soft-close
  // iterates the workspace's sids and drops each entry (mirroring the
  // sourceDiffShownBySession/worktreeEditorOpenBySession reducers above).
  const dropBySid = <T>(record: Record<string, T>): Record<string, T> =>
    sidsForWs.reduce((acc, sid) => {
      if (sid in acc) {
        acc = { ...acc };
        delete acc[sid];
      }
      return acc;
    }, record);
  const sourceDiffSelectedFile = dropBySid(s.sourceDiffSelectedFile);
  // SLW-WD-9 (RD-9): tear down History-mode + selected-sha so a re-open re-runs
  // the adaptive lead fresh (never a stale sticky mode).
  const sourceDiffHistoryMode = dropBySid(s.sourceDiffHistoryMode);
  const sourceDiffSelectedSha = dropBySid(s.sourceDiffSelectedSha);
  // SLW-DV-7 (AC-2): tear down the comparison basis alongside the mode so a
  // re-opened tab starts from the default (branch-base).
  const sourceDiffBaseMode = dropBySid(s.sourceDiffBaseMode);
  const sourceDiffViewOptions = dropBySid(s.sourceDiffViewOptions);
  const worktreeEditorOpenBySession = sidsForWs.reduce((acc, sid) => {
    if (sid in acc) {
      acc = { ...acc };
      delete acc[sid];
    }
    return acc;
  }, s.worktreeEditorOpenBySession);
  const worktreeEditorFile = { ...s.worktreeEditorFile };
  delete worktreeEditorFile[workspaceId];
  const worktreeEditorRevealLine = { ...s.worktreeEditorRevealLine };
  delete worktreeEditorRevealLine[workspaceId];
  const worktreeEditorDraft = { ...s.worktreeEditorDraft };
  delete worktreeEditorDraft[workspaceId];
  const worktreeEditorTabs = { ...s.worktreeEditorTabs };
  delete worktreeEditorTabs[workspaceId];
  const sourceDiffMode = dropBySid(s.sourceDiffMode);
  const worktreeChangedPath = { ...s.worktreeChangedPath };
  delete worktreeChangedPath[workspaceId];
  const softClosedWorkspaces = keepRunning
    ? {
        ...s.softClosedWorkspaces,
        [teamKey]: {
          ...readTeamKeyed(s.softClosedWorkspaces, teamKey, {}),
          [workspaceId]: {
            workspace,
          },
        },
      }
    : s.softClosedWorkspaces;
  return {
    storyHudShown,
    storyHudExpanded,
    sourceDiffShownBySession,
    sourceDiffSelectedFile,
    sourceDiffHistoryMode,
    sourceDiffSelectedSha,
    sourceDiffBaseMode,
    sourceDiffViewOptions,
    worktreeEditorOpenBySession,
    worktreeEditorFile,
    worktreeEditorRevealLine,
    worktreeEditorDraft,
    worktreeEditorTabs,
    sourceDiffMode,
    worktreeChangedPath,
    softClosedWorkspaces,
  };
}

/**
 * T.4 AC-7 / D-8: canonical list of all team-keyed `Record<teamKey, X>`
 * slices on `ViewerState`. Source of truth for `closeTeam`'s team-level
 * teardown — adding a new team-keyed slice means adding it here once and
 * the closeTeam clear inherits it automatically.
 *
 * Two cohorts:
 * - **T.1 slices** (already team-keyed at runtime): the workspace bar +
 *   per-workspace terminal slices.
 * - **T.4 slices** (signature still `repoPath` until Phase 5 lands the
 *   flip + UI rewire; bucketing flips to teamKey alongside): the 18
 *   per-team viewer-state slices.
 *
 * NOT in this list:
 * - Non-Record scalars (`noClonePromptRepo`,
 *   `workspaceCloseConfirm`, `noClonePromptRetry`) — Phase 5 reshapes
 *   them per AC-3 with their own match-and-clear logic.
 */
const TEAM_KEYED_SLICES: ReadonlyArray<keyof ViewerState> = [
  // SW-WG-9: the generic maximize slice — runtime-only (NOT persisted) but swept on
  // rename/close here so a reopened/renamed team's maximize is cleared/relocated like
  // its siblings, with no bespoke per-scalar handling.
  "maximizedPanel",
  // T.1 cohort
  "workspaces",
  "activeWorkspace",
  // T.4 cohort (per-repo today, per-team after Phase 5)
  "openTabs",
  "activeTab",
  "tabDiff",
  "drillPaths",
  // SLW-FX-14: persisted, teamKey-keyed cold-open pipeline-key hint — must ride
  // a rename re-key and be cleared on closeTeam like its siblings.
  "teamPipelineRepoPath",
  "navCollapsed",
  "agentIdentity",
  "agentPins",
  // SW-WC-6: per-team Inbox list/content split ratio — closeTeam cleanup.
  "foregroundLayout",
  // SW-EW-40: per-rail foreground arrangement (persisted) + the runtime-only
  // rail-key index (here only for closeTeam cleanup, like selectedEpicDirName).
  "foregroundLayoutByRail",
  "railKeyByWorkspaceId",
  "rightPaneSplitRatio",
  "statusNavExpandedPaths",
  "statusNavPinnedPaths",
  "navLens",
  "revealCounter",
  "pipelineScope",
  "selectedAgentContext",
  "navHistory",
  "navIndex",
  "openedViewerPaths",
  "onDiskWorktrees",
  // PT-10: closed-but-running shadows are team-keyed; `closeTeam` kills their
  // PTYs (below) then this sweep drops the entries.
  "softClosedWorkspaces",
  // SW-EW-33: per-team tray recency stack + published render order.
  "mruWorkspaceStack",
  "trayOrderByTeam",
  // SW-CW-4: full-strip order published alongside trayOrderByTeam (closeTeam cleanup).
  "trayFullOrderByTeam",
  // The shared workspace order and the computed tray order it applies over (both persisted,
  // so a restart keeps the order before the tree syncs).
  "workspaceOrderByTeam",
  "customGroupsByTeam",
  "workspaceGroupVersionsByTeam",
  "trayBaseOrderByTeam",
  // SW-EW-38: tray lens + selected epic-rail key (the latter runtime-only — here
  // only for closeTeam cleanup).
  "trayMode",
  "selectedEpicDirName",
  // T.4 Phase 4 — composite-inner-key worktree slices
  // SW-SO-1: pre-record "opening" flags — runtime-only, here only so closeTeam
  // sweeps a team's in-flight opening overlays on teardown (AC-6).
  "openingWorkspaces",
] as const;

/**
 * Shared "open a file tab" state commit used by both `openFile` (pin-
 * immediately) and `openFilePreview` (preview-aware). Centralises the
 * tab-array update, activeTab flip, and navHistory push so the two
 * entry points can't drift on side-effects.
 *
 * `tabsMutator` describes the caller's tab-array update:
 *   - `openFile` — find-or-append-pinned, pin-if-existing-preview.
 *   - `openFilePreview` — replace-existing-preview-or-append-preview,
 *     leave-alone-if-already-open.
 *
 * When the mutator returns the same array reference (no change) the
 * openTabs partial is omitted to preserve pre-refactor zustand update
 * semantics (avoids gratuitous subscriber fan-out).
 */
/**
 * SW-LN-B38: console layout hides the main pane, so an explicit open there
 * (Settings, Workspaces, Source repos, a file, a dashboard) would land behind the
 * terminals. Step out of console to the shared layout, which keeps the terminal
 * on screen. `lastTerminalMode` is left alone: the user's chosen terminal mode
 * stands. Preview-opens (`openFilePreview`) don't call this — browsing never
 * changes the layout. The layout is one for every team (D39), so only an open for
 * the team on screen (the active one) steps out of it.
 */
function revealMainPaneFromConsole(s: ViewerState, teamKey: string): Partial<ViewerState> {
  if (s.layoutMode !== "console") return {};
  if (teamKey !== getActiveTeamKey()) return {};
  return { layoutMode: "shared", terminalVisible: true };
}

function fileHashPatch(
  state: ViewerState,
  teamKey: string,
  path: string,
  hash?: string,
): Partial<ViewerState> {
  if (hash) return { fileHashRequests: { ...state.fileHashRequests, [teamKey]: { path, hash } } };
  if (!state.fileHashRequests?.[teamKey]) return {};
  const next = { ...state.fileHashRequests };
  delete next[teamKey];
  return { fileHashRequests: next };
}

function commitTabOpen(
  s: ViewerState,
  teamKey: string,
  filePath: string,
  tabsMutator: (tabs: TabState[]) => TabState[],
): Partial<ViewerState> {
  const currentTabs = readTeamKeyed(s.openTabs, teamKey, []);
  const nextTabs = tabsMutator(currentTabs);
  const partial: Partial<ViewerState> = {
    activeTab: updateTeamKeyed(s.activeTab, teamKey, filePath),
  };
  if (nextTabs !== currentTabs) {
    partial.openTabs = updateTeamKeyed(s.openTabs, teamKey, nextTabs);
  }

  // navHistory push with dedup + truncate-forward + 50-cap.
  const history = readTeamKeyed(s.navHistory, teamKey, []);
  const index = readTeamKeyed(s.navIndex, teamKey, -1);
  const currentEntry = index >= 0 && index < history.length ? history[index] : null;
  if (currentEntry === filePath) return partial;

  const truncated = history.slice(0, index + 1);
  truncated.push(filePath);
  const capped = truncated.length > 50 ? truncated.slice(truncated.length - 50) : truncated;

  return {
    ...partial,
    navHistory: updateTeamKeyed(s.navHistory, teamKey, capped),
    navIndex: updateTeamKeyed(s.navIndex, teamKey, capped.length - 1),
  };
}

/**
 * Shared "navigate to a history target" state commit used by
 * `navigateBack` and `navigateForward`. History navigation is a commit
 * action (AC-P6): if the target tab is already open as a preview, pin
 * it; if it's not open at all, re-add it pinned (preview flag omitted).
 */
function commitNavTarget(
  s: ViewerState,
  teamKey: string,
  targetPath: string,
  newIndex: number,
): Partial<ViewerState> {
  const tabs = readTeamKeyed(s.openTabs, teamKey, []);
  const idx = tabs.findIndex((t) => t.path === targetPath);
  let nextTabs: TabState[];
  if (idx < 0) {
    // Tab was closed — re-add as pinned.
    nextTabs = [...tabs, committedTab(targetPath)];
  } else if (tabs[idx].preview) {
    // Tab is open as preview — pin it in place (history nav is a commit).
    nextTabs = [...tabs];
    nextTabs[idx] = { ...nextTabs[idx], preview: false };
  } else {
    nextTabs = tabs;
  }

  const partial: Partial<ViewerState> = {
    activeTab: updateTeamKeyed(s.activeTab, teamKey, targetPath),
    navIndex: updateTeamKeyed(s.navIndex, teamKey, newIndex),
  };
  if (nextTabs !== tabs) {
    partial.openTabs = updateTeamKeyed(s.openTabs, teamKey, nextTabs);
  }
  return partial;
}

/**
 * Side-effect helper invoked after `openFile` has committed the new tab.
 *
 * 1. Expand status-nav sub-epic ancestors so the file's row is visible.
 * 2. If the file is outside the current drill path, reset drill to the
 *    viewer root first. Uses direct `drillPaths` mutation (not the
 *    sync callback in repo-viewer-view.tsx) so the pipeline scope is
 *    NOT re-synced from the drill change — we handle scope separately.
 * 3. Pipeline-scope rules (post-R-14: viewerRoot IS the scope base):
 *    - File outside viewerRoot → if drill was reset to root, also
 *      clear pipeline scope so the two slices stay in sync. Otherwise
 *      isDrilledIn becomes false while the scoped tree still shows a
 *      sub-epic, leaking viewerRootEntries into the nav.
 *    - File inside viewerRoot but NOT a story file (README, non-md,
 *      etc.) AND pipeline is currently drilled → reset pipeline scope
 *      to "" so the user sees broader context.
 *    - File inside viewerRoot and IS a story file → do nothing to
 *      scope; `AutoCenterOnActiveTab` handles highlighting/centering.
 */
function revealOpenedFile(
  repoPath: string,
  teamKey: string,
  filePath: string,
  set: (partial: Partial<ViewerState> | ((s: ViewerState) => Partial<ViewerState>)) => void,
  get: () => ViewerState,
): void {
  // A private file carries its own root; teamKey still owns the nav slices.
  const source = resolveFileSource(filePath, resolveContentSource(repoPath));
  const state = get();
  const navSource = normalizeNavLens(
    readTeamKeyed(state.navLens, teamKey, DEFAULT_NAV_LENS),
  ).source;
  if (source.source.kind !== navSource.kind) return;
  if (!source.orgRoot) return;

  // PTS-2: tree root is the Rust-resolved per-team content dir.
  const treeRoot = source.contentRoot;
  if (!treeRoot) return;
  const owner = (node: DerivedPipelineTree): string | null =>
    node.docs?.find((doc) => doc.filePath === filePath)?.sourceTeam ??
    node.subEpics.map(owner).find((team) => team !== null) ??
    null;
  const tree = useTeamSurfaceStore.getState().trees[teamKey];
  const sourceTeam = tree ? owner(tree) : null;
  const navigationPath = filePath.startsWith(`${treeRoot}/`)
    ? filePath
    : teamContentNavigationPath(teamKey, filePath, useOrgStore.getState().orgs, sourceTeam);
  if (!navigationPath) return;

  const currentDrill = readTeamKeyed(state.drillPaths, teamKey, treeRoot);
  const effectiveDrill = navigationPath.startsWith(`${currentDrill}/`) ? currentDrill : treeRoot;

  // Build the partial update so it applies atomically.
  const partial: Partial<ViewerState> = {};
  const currentScope = readTeamKeyed(state.pipelineScope, teamKey, "");

  if (effectiveDrill !== currentDrill) {
    partial.drillPaths = updateTeamKeyed(state.drillPaths, teamKey, effectiveDrill);
    // When drill resets to root, also clear pipeline scope so isDrilledIn
    // and the scoped tree stay in sync. Without this, opening a file outside
    // the pipeline root (e.g. docs/) while drilled in leaves scope stale,
    // causing viewerRootEntries to leak into the scoped nav.
    if (effectiveDrill === treeRoot && currentScope !== "") {
      partial.pipelineScope = updateTeamKeyed(state.pipelineScope, teamKey, "");
    }
  }

  // Walk ancestor dirs between the file and treeRoot, expanding
  // sub-epic folders in statusNavExpandedPaths so PipelineStatusNav reveals
  // the file. Files outside the content root produce zero ancestors.
  const ancestors: string[] = [];
  let parent = navigationPath.slice(0, navigationPath.lastIndexOf("/"));
  while (parent.length > treeRoot.length && parent.startsWith(`${treeRoot}/`)) {
    ancestors.push(parent);
    const nextSlash = parent.lastIndexOf("/");
    if (nextSlash < 0) break;
    parent = parent.slice(0, nextSlash);
  }
  if (ancestors.length > 0) {
    const existing = readTeamKeyed(state.statusNavExpandedPaths, teamKey, []);
    const merged = Array.from(new Set([...existing, ...ancestors]));
    if (merged.length !== existing.length) {
      partial.statusNavExpandedPaths = updateTeamKeyed(
        state.statusNavExpandedPaths,
        teamKey,
        merged,
      );
    }
  }
  const insideViewerRoot = navigationPath.startsWith(`${treeRoot}/`);
  const fileName = filePath.slice(filePath.lastIndexOf("/") + 1);

  // Only reset pipeline scope if the opened file is outside the current
  // scope. Opening a non-story file (e.g. README.md) INSIDE the current
  // scope should not disrupt the drill/scope context.
  if (insideViewerRoot && !isPipelineFileName(fileName) && currentScope !== "") {
    const scopeAbs = `${treeRoot}/${currentScope}`;
    const fileInsideScope =
      navigationPath.startsWith(`${scopeAbs}/`) || navigationPath === scopeAbs;
    if (!fileInsideScope) {
      partial.pipelineScope = updateTeamKeyed(state.pipelineScope, teamKey, "");
    }
  }

  // N.6: bump reveal counter so PipelineStatusNav scrolls the file into
  // view. Unconditional — fires even when no ancestors were added (the
  // file may already be visible but re-selected after a manual collapse).
  partial.revealCounter = updateTeamKeyed(
    state.revealCounter,
    teamKey,
    readTeamKeyed(state.revealCounter, teamKey, 0) + 1,
  );

  if (Object.keys(partial).length > 0) {
    set(partial);
  }
}

/**
 * Per-story-workspace abort controllers. Keyed by `${teamKey}::${workspaceId}`
 * (T.1: bucket re-keyed; structure unchanged). Lives outside Zustand state
 * because AbortController is not serializable and the lifecycle is strictly
 * tied to a single in-flight `setupWorktree` call. Cleared in the
 * orchestrator's `finally`; cancel button reads from here and calls
 * `.abort()`.
 */
const storyWorkspaceAborts = new Map<string, AbortController>();

/**
 * RF-perf-14 (decouple): per-workspace handle for a background `setup:` run,
 * keyed by `workspaceAbortKey(teamKey, workspaceId)`. ONE entry owns both the
 * lifecycle handles that are always created + cleared together (a prior
 * two-parallel-maps design let `closeTeam` sweep one but not the other):
 * - `abort` — the in-flight run's controller. The open's own controller resolves
 *   at Phase A, so the background lane owns its own; closing the workspace/team
 *   aborts it (killing the setup subprocess) via `abortInFlightWorktreeOpen`. Its
 *   identity is also the OWNERSHIP token — a superseded attempt (close→reopen
 *   while setup ran) whose `abort` is no longer the registered one skips its
 *   badge write (mirrors the blocking path's `storyWorkspaceAborts` guard).
 * - `retry` — set when the run FAILS (nothing then in flight, but the workspace
 *   stays "ready" + shows a retry badge); invoked by `retryWorktreeSetup`. `null`
 *   while a run is in flight.
 * Cleared on success/cancel and on close (`abortInFlightWorktreeOpen`).
 */
interface BackgroundSetupHandle {
  abort: AbortController;
  retry: (() => void) | null;
}
const backgroundSetupHandles = new Map<string, BackgroundSetupHandle>();

/** SW-WT-12 (S2): NON-reactive per-step setup output buffer, keyed
 *  `${workspaceId}:${stepIndex}` → bounded tail of streamed lines. Populated by
 *  the background-setup `onProgress`; read on demand when a checklist row is
 *  expanded (a snapshot), so a chatty `npm install` never re-renders the badge
 *  (the RF-perf-14c no-churn guard stays intact — this Map is never in store
 *  state). Cleared when the run (re)starts and when it settles. */
const backgroundSetupStepOutput = new Map<string, string>();
const MAX_STEP_OUTPUT_CHARS = 8 * 1024;
function appendBackgroundSetupOutput(workspaceId: string, stepIndex: number, line: string): void {
  const key = `${workspaceId}:${stepIndex}`;
  const next = `${backgroundSetupStepOutput.get(key) ?? ""}${line}\n`;
  backgroundSetupStepOutput.set(
    key,
    next.length > MAX_STEP_OUTPUT_CHARS ? next.slice(next.length - MAX_STEP_OUTPUT_CHARS) : next,
  );
}
function clearBackgroundSetupOutput(workspaceId: string): void {
  const prefix = `${workspaceId}:`;
  for (const key of backgroundSetupStepOutput.keys()) {
    if (key.startsWith(prefix)) backgroundSetupStepOutput.delete(key);
  }
}
/** SW-WT-12 (S2): the buffered output for one checklist step (snapshot). The
 *  checklist row reads this when expanded — output-on-demand without live churn. */
export function getBackgroundSetupStepOutput(
  workspaceId: string,
  stepIndex: number,
): string | undefined {
  return backgroundSetupStepOutput.get(`${workspaceId}:${stepIndex}`);
}

/** SW-WT-9: resolver for the in-flight setup-approval gate. Module-level
 *  (like `storyWorkspaceAborts`) because the promise is created inside
 *  `setupWorktree`'s `prompt` hook and resolved by a store action when the
 *  user clicks the modal. Cleared on resolve to avoid a stale double-call. */
let pendingSetupApprovalResolve: ((approved: boolean) => void) | null = null;

function workspaceAbortKey(teamKey: string, workspaceId: string): string {
  return `${teamKey}::${workspaceId}`;
}

/**
 * Build a plain `Error` tagged with a `failedStep` — the discriminator the open
 * orchestrator reads against `HANDLED_FAILED_STEPS` to decide swallow-vs-rethrow.
 * One factory so every worktree-open throw site tags consistently (was three
 * hand-rolled `new Error(...) as ... .failedStep = ...` blocks with divergent
 * casts).
 */
function failedStepError(message: string, step: string): Error & { failedStep: string } {
  const err = new Error(message) as Error & { failedStep: string };
  err.failedStep = step;
  return err;
}

/**
 * SW-SO-1: resolve which (if any) workspace id in a team is currently in its
 * pre-record "opening" window — i.e. flagged in `openingWorkspaces` but with NO
 * live `workspaces` record yet. The record's appearance IS the handoff: once
 * `addStoryWorkspace` lands the `status:"creating"` record, this returns
 * `undefined` and the in-tile overlay takes over (no double overlay, no gap).
 * Pure so the handoff logic is unit-testable without React (AC-7).
 */
export function resolveOpeningWorkspaceId(
  opening: ReadonlySet<string> | undefined,
  records: readonly Workspace[],
): string | undefined {
  if (!opening || opening.size === 0) return undefined;
  for (const id of opening) {
    if (!records.some((w) => w.id === id)) return id;
  }
  return undefined;
}

/** SW-FX-69: the open-time focus policy. Focus the opened workspace only when the
 *  caller explicitly asked (`requested === true`) OR there is no currently-focused
 *  tile to preserve (empty-mosaic guard — a mosaic must never render with zero
 *  focused tiles, so the "nothing to preserve" case wins over the flag). Otherwise
 *  the open is additive and the user's current tile keeps focus. Pure so the truth
 *  table is unit-tested without a store. The parentheses are load-bearing —
 *  `requested ?? false || !hasActiveTileToPreserve` is a JS SyntaxError (`??` cannot
 *  mix with `||` unparenthesized). */
export const shouldFocusOnOpen = (
  requested: boolean | undefined,
  hasActiveTileToPreserve: boolean,
): boolean => !hasActiveTileToPreserve || (requested ?? false);

/** SW-FX-69: apply {@link shouldFocusOnOpen} on an OPEN path. "The tile the user is
 *  working in" is the team's ACTIVE workspace — a team-global signal, so this is
 *  correct in both flat and grouped/rail mode (a rail-scoped foreground count would
 *  mis-read "other tiles" when the opened workspace lands in a different rail than
 *  the one on screen). We preserve focus only when that active id is a DIFFERENT,
 *  still-live foreground tile; an unset/stale/self active id means there is nothing
 *  to steal, so the opened workspace takes focus. The single seam both open focus
 *  sites (`finishForegroundOpen` and the story-open `onCreating` hook) delegate to —
 *  call this instead of a bare `switchWorkspace` on any open path. */
function applyOpenFocus(
  get: () => ViewerState,
  teamKey: string,
  workspaceId: string,
  requested?: boolean,
): void {
  const s = get();
  const activeId = s.activeWorkspace[teamKey];
  const hasActiveTileToPreserve =
    activeId !== undefined &&
    activeId !== workspaceId &&
    foregroundTilesFor(readTeamKeyed(s.workspaces, teamKey, []), teamTileOrder(s, teamKey)).some(
      (w) => w.id === activeId,
    );
  if (shouldFocusOnOpen(requested, hasActiveTileToPreserve)) {
    get().switchWorkspace(teamKey, workspaceId);
  }
}

/** Guards concurrent cleanupStoryWorktree calls for the same story. */
const cleanupInFlight = new Set<string>();

/** T.4 Phase 11 (Round 3 MED — mid-flight guard race fix). Per-team
 *  close epoch. `closeTeam(teamKey)` increments the counter for that
 *  team; long-running async operations (`ensureWorktree`) capture the
 *  epoch at start and check at write time. If the value increased
 *  during the await, the team was closed mid-flight and the write must
 *  skip to avoid resurrecting a torn-down bucket as orphan state. */
const teamCloseEpoch = new Map<string, number>();

/** Invalidates asynchronous team reads across close and rename. */
export function teamLifecycleVersion(teamKey: string): number {
  return teamCloseEpoch.get(teamKey) ?? 0;
}

/** Show the no-clone-attached modal and throw with `failedStep: "no-clone"`.
 *  Shared by the source_repo and fallback branches in `ensureWorktree`.
 *
 *  T.4 AC-3: prompt identity is `(teamKey, sourceRepoSlug)`. Derived from
 *  `promptRepoPath` (synthetic) — non-synthetic repos can't reach this
 *  branch in practice, so the identity is always parseable.
 *
 *  T.5: the retry callback re-invokes `ensureWorktree(teamKey, storyFilePath)`
 *  — the third arg is the `teamKey` the original call landed in, not the
 *  legacy `contextRepoPath`. */
function throwNoClone(
  get: () => ViewerState,
  promptRepoPath: string,
  teamKey: string,
  storyFilePath: string,
): never {
  const identity = promptIdentityFromRepoPath(promptRepoPath);
  if (identity === null) {
    // Defensive: a non-synthetic repoPath reaching this branch is a
    // configuration bug. Skip the prompt rather than crash; the original
    // error still surfaces via the throw below.
    logger.warn(
      "viewer",
      `throwNoClone: non-synthetic repoPath '${promptRepoPath}' — skipping prompt`,
    );
  } else {
    get().showNoClonePrompt(identity.teamKey, identity.sourceRepoSlug, () => {
      get()
        .ensureWorktree(teamKey, storyFilePath)
        .catch((e) => {
          logger.warn("viewer", `ensureWorktree retry failed: ${e}`);
          // T.5 (iter-2 #F-iter2-B): suppress the catch-side toast when
          // ensureWorktree already toasted at its throw site (F1).
          // HANDLED_FAILED_STEPS now includes `team-mismatch`, `no-clone`,
          // `untracked`, `setup`, `create`, `cancelled`, `orphan`,
          // `blocked-dep` — every step whose throw already produced UI
          // feedback. Without this gate, a retry that hit a different
          // failure mode (e.g. the slug got `RemoveSourceRepoFromTeam`'d
          // while the no-clone modal was open) produced a double-toast:
          // the action's throw-site toast + the catch's generic toast.
          const failedStep = (e as { failedStep?: string }).failedStep;
          if (failedStep && (HANDLED_FAILED_STEPS as readonly string[]).includes(failedStep)) {
            return;
          }
          toast.error(`Couldn't open worktree: ${e instanceof Error ? e.message : String(e)}`);
        });
    });
  }
  throw failedStepError("No local clone attached for this workspace repo", "no-clone");
}

/** T.5 D-1: typed error thrown by `ensureWorktree` when the resolved
 *  story-to-team mapping breaks. Callers (`pipeline-viewer-panel.tsx`,
 *  `team-workspace-view.tsx`) catch this and emit a reason-specific toast
 *  instead of the generic worktree-failure toast.
 *
 *  Differentiated reasons (D-1), selected by the precedence ladder in
 *  `ensureWorktree`'s lookup-failure branch — most-specific first:
 *  - `frontmatter-rewritten`: the story file's `source_repo` was edited
 *    between the pipeline scan and the click. Detected when the caller
 *    passes an `expectedSlug` (the scan-time slug) that differs from the
 *    current frontmatter slug. Implemented in T-13.
 *  - `slug-stale-after-removal`: the slug *was* in the team but the user
 *    removed it (Settings-side) after the workspace was created.
 *    Detected via the `recentlyRemovedRepos` set in org-store. Implemented
 *    in T-13.
 *  - `slug-not-in-team`: the story's `source_repo` slug isn't in the
 *    team's `workspaceRepos` slice and matches neither reason above.
 *    The most common shape — usually cross-team browsing.
 *  - `missing-source-repo`: the story has no `source_repo` frontmatter
 *    AND the team has zero source repos to fall back to (separate branch,
 *    before the ladder). */
export type WorktreeTeamMismatchReason =
  | "slug-not-in-team"
  | "slug-stale-after-removal"
  | "frontmatter-rewritten"
  | "missing-source-repo";

export class WorktreeTeamMismatchError extends Error {
  readonly failedStep = "team-mismatch" as const;
  constructor(
    readonly reason: WorktreeTeamMismatchReason,
    readonly slug: string | null,
    readonly teamKey: string,
    message?: string,
  ) {
    super(
      message ?? `Worktree team mismatch (${reason}): slug=${slug ?? "<none>"} team=${teamKey}`,
    );
    this.name = "WorktreeTeamMismatchError";
  }
}

/** `failedStep` values whose errors are already surfaced via toast or modal
 *  and should NOT produce a second toast in caller `.catch` blocks. Shared
 *  between `openOrFocusStoryWorkspace` and `isHandledWorktreeError`.
 *
 *  T.5: `team-mismatch` is included so the catch-path in
 *  `openOrFocusStoryWorkspace` swallows the error after the caller surfaces
 *  its own reason-specific toast. */
export const HANDLED_FAILED_STEPS = [
  "create",
  "setup",
  "orphan",
  "cancelled",
  "untracked",
  "no-clone",
  "blocked-dep",
  "team-mismatch",
  // SW-WT-1: an invalid worktree.yaml surfaces its error via toast at the
  // throw site, then is swallowed here so callers don't double-toast.
  "config",
] as const;

/**
 * Deduplicates concurrent `ensureWorktree` calls for the same story.
 * If a second caller arrives while the first is in-flight, it piggybacks
 * on the existing promise instead of starting a parallel setup.
 */
const ensureWorktreeInFlight = new Map<string, Promise<string>>();

/**
 * SW-SO-1: tear down an in-flight story-workspace open by its abort key. Aborts
 * + drops the `AbortController`, AND evicts the dedup entry so an immediate
 * reopen of the same story starts FRESH instead of piggybacking the now-doomed
 * promise (which would reject `cancelled`, get swallowed by the orchestrator, and
 * no-op the reopen — a dead click).
 *
 * This is the ONE teardown every abort site must funnel through — `closeWorkspace`
 * (in-tile Cancel), `cancelWorkspaceOpen` (pre-record Cancel), and `closeTeam`
 * (mid-flight team switch). Centralised because three review rounds each found the
 * same class of bug from ONE of these paths forgetting the dedup eviction; a
 * single helper makes a fourth omission impossible. The doomed promise's own
 * ownership-guarded `finally` (`=== promise`/`=== controller`) then no-ops rather
 * than clobbering any fresh open that reused the key.
 */
function abortInFlightWorktreeOpen(abortKey: string): void {
  const controller = storyWorkspaceAborts.get(abortKey);
  if (controller) {
    controller.abort();
    storyWorkspaceAborts.delete(abortKey);
  }
  ensureWorktreeInFlight.delete(abortKey);
  // RF-perf-14 (decouple): also cancel a BACKGROUND `setup:` run for this
  // workspace. The open's controller above no longer covers it (the open
  // resolved at Phase A), so without this a close/cancel would leave `npm ci`
  // running orphaned. Aborting the signal makes runWorktreeSetupPhase kill the
  // setup subprocess; deleting the handle also flips the ownership token so the
  // (now-cancelled) attempt's badge write is skipped AND drops any retry thunk
  // (the badge it would retry from is gone with the workspace).
  backgroundSetupHandles.get(abortKey)?.abort.abort();
  backgroundSetupHandles.delete(abortKey);
}

// --- RF-4: generic team-keyed record helpers ---
// `viewer-store.ts` reads team-keyed maps as `state.field[teamKey] ?? default`
// (~90 sites) and writes them as `{ ...state.field, [teamKey]: next }` (~96
// sites). These free functions collapse that boilerplate to one place. They take
// the MAP first (not `state` + a field name) so TypeScript infers the value type
// with zero gymnastics: pure, returning the value / next map and leaving `set()`
// to the caller.

/**
 * RF-4: read a team-keyed entry with a caller-supplied fallback. The fallback
 * stays caller-supplied on purpose — the right default is contextual (e.g.
 * `drillPaths` falls back to the tree root, `layoutMode` to "shared"), so a
 * field→default registry would inject the wrong value.
 */
export function readTeamKeyed<T>(map: Record<string, T>, teamKey: string, fallback: T): T {
  return map[teamKey] ?? fallback;
}

/**
 * RF-4: write one team-keyed entry, returning the SAME map reference when the
 * value is unchanged so we never churn slice identity — no gratuitous zustand
 * subscriber fan-out / re-renders (the "don't block the UI" invariant, and the
 * exact behavior several hot call sites already hand-rolled as
 * `next === cur ? map : { ...map, [teamKey]: next }`).
 */
export function updateTeamKeyed<T>(
  map: Record<string, T>,
  teamKey: string,
  next: T,
): Record<string, T> {
  if (map[teamKey] === next) return map;
  return { ...map, [teamKey]: next };
}

/** SW-CW-4: shallow element-wise equality for a published tray-order slice.
 *  Shared by `setTrayOrder` / `setTrayFullOrder` so the "unchanged → skip the
 *  set" guard (which lets the publish run in a render effect without looping)
 *  lives in one place. */
export function sameStringOrder(
  prev: readonly string[] | undefined,
  next: readonly string[],
): boolean {
  return prev != null && prev.length === next.length && prev.every((id, i) => id === next[i]);
}

/**
 * RF-4: drop a team-keyed entry, returning the SAME map reference when the key
 * is already absent. Replaces the `const { [teamKey]: _, ...rest } = map` idiom
 * (and its unused-var lint noise) at the tab-teardown sites.
 */
export function removeTeamKeyed<T>(map: Record<string, T>, teamKey: string): Record<string, T> {
  if (!(teamKey in map)) return map;
  const { [teamKey]: _drop, ...rest } = map;
  return rest;
}

// --- SW-EW-46: tab-grid selectors ---
// Pure reads over the projected grids. Kept as free functions (like
// `activeForegroundLayout`) so the chord/arrow-nav dispatch and the render gate
// share ONE definition of "grid mode" and "which workspace is expanded".

/** The workspace expanded (D68) in `teamKey`'s CLI: its focused workspace while the global
 *  `cliExpanded` is on and the CLI shows at least two tiles to zoom over, else null. With one
 *  tile, expanded stays set but shows nothing until a second tile opens. */
export function selectExpandedWorkspaceId(s: ViewerState, teamKey: string): string | null {
  if (!s.cliExpanded) return null;
  const tiles = activeForegroundTiles(s, teamKey);
  if (!isSplitActive(tiles.length)) return null;
  const focused = s.activeWorkspace[teamKey];
  return focused !== undefined && tiles.some((w) => w.id === focused) ? focused : null;
}

/** SW-EW-46 (D1): the workspace whose tile currently FILLS the CLI area — the
 *  expanded tile, or the lone open workspace when nothing is expanded.
 *  Null when several tiles share the mosaic (no single expanded workspace). This
 *  is the D1 boundary: only an expanded workspace may show its terminals gridded,
 *  so both the render gate and the `Cmd+Shift+L` branch resolve grid mode through
 *  it — never from a bare paned-set scan (a collapsed workspace keeps its remembered
 *  panes but must render as tabs). */
export function expandedWorkspaceId(s: ViewerState, teamKey: string): string | null {
  const zoomed = selectExpandedWorkspaceId(s, teamKey);
  if (zoomed) return zoomed;
  const open = activeForegroundTiles(s, teamKey);
  return open.length === 1 ? open[0].id : null;
}

/** SW-EW-46: the team's workspace that is expanded AND currently gridded (a
 *  non-empty paned set), or null. The ONE definition of "grid mode is active
 *  right now" — the body render, the `Cmd+Shift+L` branch, and directional
 *  arrow-nav all resolve through it, so they can never disagree. */
export function expandedGridWorkspaceId(s: ViewerState, teamKey: string): string | null {
  const wsId = expandedWorkspaceId(s, teamKey);
  if (!wsId || workspacePanedTabIds(s, teamKey, wsId).length === 0) return null;
  // SW-EW-46: a COLLAPSED grid keeps its (warm) paned set but renders as tabs — so
  // toggling grid⇄tabs flips this flag, never the paned membership (which would
  // dispose+respawn the cell terminals). `showGrid` is false while collapsed.
  return selectTabGridCollapsed(s, teamKey, wsId) ? null : wsId;
}

/** SW-EW-46: true when `workspaceId` is the expanded, currently-gridded workspace —
 *  i.e. its terminals should render as a live grid right now. */
export function shouldRenderTabGrid(s: ViewerState, teamKey: string, workspaceId: string): boolean {
  return expandedGridWorkspaceId(s, teamKey) === workspaceId;
}

/**
 * SW-FX-52 / SW-AM-4: drop entries for sessions that are going away from a flat
 * `Record<sessionId, T>` slice. `pty-exit` is the only event-driven clear — but a
 * tab closed with its PTY KEPT RUNNING (the common `removeTerminalTab` /
 * close-keep-running path) never emits `pty-exit`, and a kill can silently
 * fail. So the removal/reap paths prune deterministically here. Returns the SAME
 * ref when nothing matched, so an unrelated close never churns the slice identity
 * (no needless re-renders). Generic over the value type so it serves both the
 * `aiStatusBySession` and `progressBySession` slices from one implementation.
 */
/** SW-AM-4: field equality for a retained progress value — the no-op-on-equal
 *  guard for `setSessionProgress` (agents re-emit identical ticks). */
function progressEqual(a: SessionProgress | undefined, b: SessionProgress): boolean {
  return (
    a !== undefined &&
    a.phase === b.phase &&
    a.step === b.step &&
    a.total === b.total &&
    a.note === b.note &&
    a.settled === b.settled
  );
}

function pruneSessionMap<T>(
  map: Record<string, T>,
  sessionIds: Iterable<string>,
): Record<string, T> {
  let next: Record<string, T> | null = null;
  for (const id of sessionIds) {
    if (id in map) {
      next ??= { ...map };
      delete next[id];
    }
  }
  return next ?? map;
}

/** T.1 D-4: shape of a single slice migration outcome. */
interface MigratedSlices {
  workspaces: Record<string, Workspace[]>;
  activeWorkspace: Record<string, string>;
  /** Per-outer-key map of resolved teamKeys. Used to ensure cross-slice
   *  consistency when migrating siblings whose value types lack
   *  `sourceRepoSlug` (activeWorkspace). */
  repoPathToTeamKey: Map<string, string>;
  /** Aggregated list of repoPaths that fell through to the sentinel
   *  bucket. Used for a single `console.warn` at the end of migration. */
  sentinelOuterKeys: string[];
}

/**
 * T.1 D-4: Migrate the T.1-scope viewer-store slices (`workspaces`,
 * `activeWorkspace`) from per-repoPath to per-teamKey keying.
 *
 * Algorithm:
 * 1. Walk `workspaces` first (it carries `sourceRepoSlug` per-workspace,
 *    which is the only recoverable signal for legacy real-path outer keys).
 *    For each outer key:
 *    - **Pass 1 (primary, no await):** `parseSyntheticRepoKey(outerKey)`.
 *      On success → teamKey = `buildTeamKey(parsed.orgSlug, parsed.team)`.
 *    - **Pass 2 (fallback):** look at each workspace's `sourceRepoSlug`,
 *      resolve via `workspaceRepos` (caller pre-awaits org-store readiness
 *      and threads `workspaceRepos` here). All workspaces under one outer
 *      key must resolve to the SAME teamKey for the map to record it;
 *      otherwise the outer key is ambiguous and siblings go to sentinel.
 *    - **Pass 3 (sentinel):** anything still unresolved lands in
 *      `workspaces[NO_TEAM_SENTINEL]` and the outer key is appended to the
 *      sentinel accumulator.
 * 2. Walk the sibling slices using the shared `repoPathToTeamKey` map.
 *    Siblings whose outer key isn't in the map fall back to a primary
 *    parse attempt and finally sentinel.
 *
 * Lifetime: the map and accumulator are local to a single migration run;
 * they are not kept past the function return.
 */
/** Shared readiness-wait against `useOrgStore` (SW-FX-75: DRY'd from
 *  `waitForOrgStoreReady` + `waitForWorkspaceReposReady`, which were the same
 *  subscribe+timeout+finish skeleton). Resolves immediately if `predicate` is
 *  already satisfied; otherwise resolves on the first store change that
 *  satisfies it, or after `timeoutMs` (logging `onTimeoutMessage`). ALWAYS
 *  resolves — never rejects, never hangs — so a boot step awaiting it can't
 *  wedge (the whole point of the SW-FX-75 fix). */
function waitForStoreCondition(
  predicate: (state: ReturnType<typeof useOrgStore.getState>) => boolean,
  timeoutMs: number,
  onTimeoutMessage: string,
): Promise<void> {
  if (predicate(useOrgStore.getState())) return Promise.resolve();
  return new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      unsubscribe();
      clearTimeout(timer);
      resolve();
    };
    const unsubscribe = useOrgStore.subscribe((state) => {
      if (predicate(state)) finish();
    });
    const timer = setTimeout(() => {
      logger.warn("viewer", onTimeoutMessage);
      finish();
    }, timeoutMs);
  });
}

/** T.1 D-4 / AC-7 await semantic for the migration's Pass 2 fallback.
 *  Resolves immediately when `useOrgStore.orgs` is already populated;
 *  otherwise subscribes for the next change and resolves when orgs
 *  becomes non-empty, with a short timeout so a misconfigured/empty-org
 *  install doesn't hang the load forever. */
function waitForOrgStoreReady(timeoutMs = 5000): Promise<void> {
  return waitForStoreCondition(
    (state) => state.orgs.length > 0,
    timeoutMs,
    `[migration] org-store readiness timeout after ${timeoutMs}ms — Pass 2 may sentinel legacy entries`,
  );
}

/** T.4 Phase 8 review (Cluster B / D1): wait until `workspaceRepos` is
 *  populated (or a short timeout) before `pruneTeamKeyed` runs. Without
 *  this, a schemaVersion=2 blob on cold start can race: viewer load fires
 *  before `useWorkspaceViewerSync` populates `workspaceRepos`, so
 *  `knownTeamKeys = {NO_TEAM_SENTINEL}` and every persisted team's
 *  `openTabs`/`drillPaths`/etc. is dropped.
 *
 *  Guards: skip the wait when there's no active org/team to populate
 *  `workspaceRepos` from (cold-start without team selection, picker mode,
 *  test fixtures that don't activate a team). In that state the user has
 *  no team-keyed data to lose either, so proceeding is safe. */
function waitForWorkspaceReposReady(timeoutMs = 1000): Promise<void> {
  const state = useOrgStore.getState();
  // No active team → nothing populating `workspaceRepos`; proceed. (The
  // populated case is handled by the predicate check inside the shared wait.)
  if (Object.keys(state.workspaceRepos).length === 0 && state.activeOrgSlug === null) {
    return Promise.resolve();
  }
  return waitForStoreCondition(
    (s) => Object.keys(s.workspaceRepos).length > 0,
    timeoutMs,
    `[migration] workspaceRepos readiness timeout after ${timeoutMs}ms — pruneTeamKeyed will preserve all persisted entries (Phase 10 MED #4: identity-prune on empty workspaceRepos)`,
  );
}

export function migrateT1Slices(input: {
  workspaces: Record<string, Workspace[]>;
  activeWorkspace: Record<string, string>;
  /** Pre-resolved view of `useOrgStore.workspaceRepos`. Empty `Record` is
   *  acceptable; entries without `sourceRepoSlug` resolution fall through
   *  to sentinel. */
  workspaceRepos: Record<string, { repoPath: string }>;
}): MigratedSlices {
  const repoPathToTeamKey = new Map<string, string>();
  const sentinelOuterKeys: string[] = [];

  // Build exact host-aware identities first. A legacy hostless alias is added
  // for a non-GitHub row only when no GitHub twin exists and exactly one
  // host-qualified identity owns that slug, so migration never guesses.
  const parsedRepos = Object.values(input.workspaceRepos).flatMap((repo) => {
    const parsed = parseSyntheticRepoKey(repo.repoPath);
    return parsed ? [{ parsed, teamKey: buildTeamKey(parsed.orgSlug, parsed.team) }] : [];
  });
  const slugToTeamKeys = new Map<string, Set<string>>();
  const addSlugTeam = (slug: string, teamKey: string) => {
    const teams = slugToTeamKeys.get(slug) ?? new Set<string>();
    teams.add(teamKey);
    slugToTeamKeys.set(slug, teams);
  };
  const hostlessIdentities = new Map<string, { github: boolean; nonGithub: Set<string> }>();
  for (const { parsed } of parsedRepos) {
    const hostless = `${parsed.owner}/${parsed.repo}`.toLowerCase();
    const identities = hostlessIdentities.get(hostless) ?? {
      github: false,
      nonGithub: new Set<string>(),
    };
    if (parsed.host) identities.nonGithub.add(parsed.host.toLowerCase());
    else identities.github = true;
    hostlessIdentities.set(hostless, identities);
  }
  for (const { parsed, teamKey } of parsedRepos) {
    const hostless = `${parsed.owner}/${parsed.repo}`.toLowerCase();
    const exact = `${parsed.host ? `${parsed.host}/` : ""}${hostless}`.toLowerCase();
    addSlugTeam(exact, teamKey);
    const identities = hostlessIdentities.get(hostless)!;
    if (parsed.host && !identities.github && identities.nonGithub.size === 1) {
      addSlugTeam(hostless, teamKey);
    }
  }

  function resolveOuterKey(outerKey: string, sourceRepoSlugs: string[]): string | null {
    // Pass 1: parse the outer key as a synthetic repoPath.
    const parsed = parseSyntheticRepoKey(outerKey);
    if (parsed) return buildTeamKey(parsed.orgSlug, parsed.team);

    // Pass 2: walk per-workspace `sourceRepoSlug` against workspaceRepos.
    // All workspaces under this outer key must agree on a single teamKey
    // for the resolution to count — otherwise the outer key is ambiguous.
    const candidates = new Set<string>();
    for (const slug of sourceRepoSlugs) {
      for (const teamKey of slugToTeamKeys.get(slug.toLowerCase()) ?? []) {
        candidates.add(teamKey);
      }
    }
    if (candidates.size === 1) return candidates.values().next().value as string;

    return null;
  }

  // ===== Pass A: migrate workspaces (drives the shared map) =====
  const migratedWorkspaces: Record<string, Workspace[]> = {};
  for (const [outerKey, wsList] of Object.entries(input.workspaces)) {
    const slugs = (wsList ?? [])
      .map((w) => w.sourceRepoSlug)
      .filter((s): s is string => typeof s === "string" && s.length > 0);
    const resolvedTeamKey = resolveOuterKey(outerKey, slugs);
    const targetKey = resolvedTeamKey ?? NO_TEAM_SENTINEL;
    if (resolvedTeamKey !== null) {
      repoPathToTeamKey.set(outerKey, resolvedTeamKey);
    } else {
      sentinelOuterKeys.push(outerKey);
    }
    // Multiple outer keys may collapse onto the same teamKey (e.g. two
    // synthetic keys for different repos in the same team). Concatenate
    // workspace lists but dedup by `id` (last-write-wins) so duplicate
    // `"root"` entries across siblings don't violate AC-4 within
    // `workspaces[teamKey]`. T.2 generalises the root constant to per-
    // source-repo IDs; until then a single root per teamKey is correct.
    const prev = migratedWorkspaces[targetKey] ?? [];
    const merged = new Map<string, Workspace>();
    for (const w of prev) merged.set(w.id, w);
    for (const w of wsList) merged.set(w.id, w);
    migratedWorkspaces[targetKey] = Array.from(merged.values());
  }

  // ===== Pass B: migrate sibling slices via shared map + parse fallback =====
  function migrateSibling<T>(obj: Record<string, T>, sliceName: string): Record<string, T> {
    const out: Record<string, T> = {};
    for (const [outerKey, value] of Object.entries(obj)) {
      let teamKey = repoPathToTeamKey.get(outerKey);
      if (teamKey === undefined) {
        const parsed = parseSyntheticRepoKey(outerKey);
        if (parsed) {
          teamKey = buildTeamKey(parsed.orgSlug, parsed.team);
          repoPathToTeamKey.set(outerKey, teamKey);
        }
      }
      if (teamKey === undefined) {
        teamKey = NO_TEAM_SENTINEL;
        if (!sentinelOuterKeys.includes(outerKey)) {
          sentinelOuterKeys.push(outerKey);
        }
        logger.debug("viewer", `[migration] ${sliceName}[${outerKey}] → sentinel (unresolvable)`);
      }
      // Last-write-wins on a collision (rare; benign).
      out[teamKey] = value;
    }
    return out;
  }

  const result: MigratedSlices = {
    workspaces: migratedWorkspaces,
    activeWorkspace: migrateSibling(input.activeWorkspace, "activeWorkspace"),
    repoPathToTeamKey,
    sentinelOuterKeys,
  };

  if (sentinelOuterKeys.length > 0) {
    logger.warn(
      "viewer",
      `[migration] ${sentinelOuterKeys.length} unresolvable workspace path(s) bucketed into sentinel: ${sentinelOuterKeys.join(", ")}`,
    );
  }

  return result;
}

/** The store's flat chrome fields, as `cliChrome` saves them. */
export function cliChromeOf(
  s: Pick<
    ViewerState,
    | "terminalVisible"
    | "layoutMode"
    | "lastTerminalMode"
    | "terminalSplitRatio"
    | "cliExpanded"
    | "workspacePanelCollapsed"
    | "workspacePanelWidth"
  >,
): CliChrome {
  return {
    visible: s.terminalVisible,
    layoutMode: s.layoutMode,
    lastTerminalMode: s.lastTerminalMode,
    splitRatio: s.terminalSplitRatio,
    expanded: s.cliExpanded,
    agentsDockCollapsed: s.workspacePanelCollapsed,
    agentsDockWidth: s.workspacePanelWidth,
  };
}

/** A `cliChrome` value as the store's flat chrome fields. */
export function cliChromeState(c: CliChrome) {
  return {
    terminalVisible: c.visible,
    layoutMode: c.layoutMode,
    lastTerminalMode: c.lastTerminalMode,
    terminalSplitRatio: c.splitRatio,
    cliExpanded: c.expanded,
    workspacePanelCollapsed: c.agentsDockCollapsed,
    workspacePanelWidth: c.agentsDockWidth,
  } satisfies Partial<ViewerState>;
}

/** The team the saved org selection names (the one open when the app last closed), or null.
 *  A legacy `owner--repo` slug is read as `owner/repo`, as the viewer keys are. */
function savedOrgTeamKey(org: unknown): string | null {
  if (!org || typeof org !== "object") return null;
  const { activeOrgSlug, activeTeamName } = org as Record<string, unknown>;
  return typeof activeOrgSlug === "string" && typeof activeTeamName === "string"
    ? buildTeamKey(rewriteLegacyOrgSlug(activeOrgSlug), activeTeamName)
    : null;
}

/**
 * T.4 D-4: schemaVersion 1 → 2 migration.
 *
 * Re-buckets the persisted T.4 slices (`openTabs`, `activeTab`, `drillPaths`,
 * `rightPaneSplitRatio`, `statusNavExpandedPaths`,
 * `pipelineScope`) from legacy `Record<repoPath, …>` to team-keyed
 * `Record<teamKey, …>` shape. The CLI chrome T.4 also re-bucketed is global
 * since D39; the raw-blob v15 step seeds it before this runs.
 *
 * Outer-key resolution reuses T.1's three-pass strategy:
 *   1. `parseSyntheticRepoKey(outerKey)` — for synthetic workspace keys
 *   2. `repoPathToTeamKey` map carried over from T.1's migration (if it
 *      just ran in the same call) — handles legacy real-path keys that
 *      already got resolved via `sourceRepoSlug → workspaceRepos`
 *   3. NO_TEAM_SENTINEL fallback — unresolvable keys
 *
 * Per-slice merge rules apply when multiple repoPath buckets collapse
 * onto the same teamKey:
 *   - Arrays: concat + dedup. `openTabs` dedups by `path`;
 *     `statusNavExpandedPaths` dedups by string value.
 *   - Scalars: last-write-wins (any value yields a sensible first render;
 *     subsequent user interaction overwrites).
 *
 * T.6: `workspaceViewerStates` retired per D-9 — the snapshot/restore
 * mechanism is gone and the migration no longer carries its outer keys
 * forward.
 */
function migrateT4Slices(input: {
  openTabs: Record<string, TabState[]>;
  activeTab: Record<string, string>;
  drillPaths: Record<string, string>;
  rightPaneSplitRatio: Record<string, number>;
  statusNavExpandedPaths: Record<string, string[]>;
  pipelineScope: Record<string, string>;
  /** Lookup map carried over from T.1's migration. Empty when T.1 didn't
   *  need to run (schemaVersion was already 1). */
  repoPathToTeamKey: Map<string, string>;
}): {
  openTabs: Record<string, TabState[]>;
  activeTab: Record<string, string>;
  drillPaths: Record<string, string>;
  rightPaneSplitRatio: Record<string, number>;
  statusNavExpandedPaths: Record<string, string[]>;
  pipelineScope: Record<string, string>;
  sentinelOuterKeys: string[];
} {
  const sentinelOuterKeys: string[] = [];

  function resolveOuterKey(outerKey: string): string {
    // Pass 1: shared map from T.1 migration.
    const cached = input.repoPathToTeamKey.get(outerKey);
    if (cached) return cached;
    // Pass 2: parse outer key as synthetic repoPath.
    const parsed = parseSyntheticRepoKey(outerKey);
    if (parsed) {
      const tk = buildTeamKey(parsed.orgSlug, parsed.team);
      input.repoPathToTeamKey.set(outerKey, tk);
      return tk;
    }
    // Pass 3: sentinel.
    if (!sentinelOuterKeys.includes(outerKey)) sentinelOuterKeys.push(outerKey);
    return NO_TEAM_SENTINEL;
  }

  function migrateScalar<T>(obj: Record<string, T> | undefined): Record<string, T> {
    if (!obj) return {};
    const out: Record<string, T> = {};
    // Last-write-wins: iterate in insertion order; later entries overwrite.
    for (const [outerKey, value] of Object.entries(obj)) {
      out[resolveOuterKey(outerKey)] = value;
    }
    return out;
  }

  function migrateArrayDedup<T>(
    obj: Record<string, T[]> | undefined,
    getKey: (item: T) => string,
  ): Record<string, T[]> {
    if (!obj) return {};
    const out: Record<string, T[]> = {};
    for (const [outerKey, arr] of Object.entries(obj)) {
      const teamKey = resolveOuterKey(outerKey);
      const existing = out[teamKey] ?? [];
      const seen = new Set(existing.map(getKey));
      const merged = [...existing];
      for (const item of arr) {
        const k = getKey(item);
        if (!seen.has(k)) {
          merged.push(item);
          seen.add(k);
        }
      }
      out[teamKey] = merged;
    }
    return out;
  }

  return {
    openTabs: migrateArrayDedup(input.openTabs, (t) => t.path),
    activeTab: migrateScalar(input.activeTab),
    drillPaths: migrateScalar(input.drillPaths),
    rightPaneSplitRatio: migrateScalar(input.rightPaneSplitRatio),
    statusNavExpandedPaths: migrateArrayDedup(input.statusNavExpandedPaths, (s) => s),
    pipelineScope: migrateScalar(input.pipelineScope),
    sentinelOuterKeys,
  };
}

/** SLW-PT-19: the scope of the notes the store holds (set when a load lands), else
 *  the current one. Writes to a loaded note use it, so they never follow an org
 *  switch that has not reloaded the notes yet. */
function loadedNotesScope(): NotesScope {
  return useViewerStore.getState().notesLoadedScope ?? notesScope();
}

/** Whether the store holds `scope`'s notes. */
function holdsNotesOf(scope: NotesScope): boolean {
  const loaded = useViewerStore.getState().notesLoadedScope;
  return loaded !== null && notesScopeKey(loaded) === notesScopeKey(scope);
}

/** The body each note's editor content was last based on, as it was on disk: what
 *  the store loaded, or what it last saved. A save sends it so the backend can tell
 *  whether someone else changed the note in between. */
const noteBases = new Map<string, string>();

function noteBaseKey(scope: NotesScope, id: string): string {
  return `${notesScopeKey(scope)}:${id}`;
}

function rememberNoteBase(scope: NotesScope, id: string, body: string): void {
  noteBases.set(noteBaseKey(scope, id), body);
}

/** Request ids of the editor's own saves whose projection delta hasn't arrived: the
 *  delta brings nothing the editor lacks, so it never re-reads the body. */
const ownNoteSaves = new Set<string>();

/** The list preview of `body`, for notes no projection previews: those kept on this
 *  Mac with no org. Null for an org's notes, whose rows bring their own. */
function localPreview(scope: NotesScope, body: string): Pick<NoteMeta, "preview" | "refs"> | null {
  if (notesHaveProjection(scope)) return null;
  const { snippet, refs } = summarize(body);
  return { preview: snippet, refs };
}

/** Save a note's body from the editor, tagged as the person's own save. A note with
 *  no projection takes its new preview from the body saved. */
async function saveNoteBody(scope: NotesScope, id: string, content: string, base?: string) {
  const requestId = `note-save:${crypto.randomUUID()}`;
  ownNoteSaves.add(requestId);
  if (ownNoteSaves.size > 64) ownNoteSaves.delete(ownNoteSaves.values().next().value!);
  const written = await writeNote(scope, id, content, base, requestId);
  const preview = typeof written === "string" ? localPreview(scope, written) : null;
  if (preview && holdsNotesOf(scope)) {
    useViewerStore.setState((s) => ({
      notes: s.notes.map((n) => (n.id === id ? { ...n, ...preview } : n)),
    }));
  }
  return written;
}

/** Whether the person typed in note `id` since the editor held `typedFrom`. A body
 *  read meanwhile must not replace their edit; their next save merges with it. */
function typedSince(id: string, typedFrom: string): boolean {
  const s = useViewerStore.getState();
  return s.activeNoteId === id && s.scratchNoteContent !== typedFrom;
}

/** The editor state that shows `content`, note `id`'s body as read from disk, which
 *  becomes the base the next save starts from. */
function adoptedNoteBody(scope: NotesScope, id: string, content: string) {
  rememberNoteBase(scope, id, content);
  return { activeNoteId: id, scratchNoteContent: content };
}

/** Someone else changed the open note: show their body, unless the person switched
 *  notes or typed while it was read. */
async function adoptActiveNoteChange(scope: NotesScope, id: string): Promise<void> {
  await debouncedScratchSave.flush();
  const typedFrom = useViewerStore.getState().scratchNoteContent;
  const gen = activeSwitchGeneration;
  try {
    const note = await readNote(scope, id);
    const s = useViewerStore.getState();
    if (
      gen !== activeSwitchGeneration ||
      s.activeNoteId !== id ||
      !holdsNotesOf(scope) ||
      typedSince(id, typedFrom)
    )
      return;
    useViewerStore.setState(adoptedNoteBody(scope, id, note.content));
  } catch (e) {
    logger.error("viewer", `adoptActiveNoteChange(${id}) failed: ${e}`);
  }
}

/** A records projection row of the `notes` collection. */
type ProjectedNoteRow = { id: string } & Record<string, unknown>;
/** The projection delta that changed the rows. */
type ProjectedNotesChange = {
  upserts: readonly { id: string }[];
  kind?: string;
  requestId?: string;
};

/** Changes that leave a note's body as it was. */
const BODY_KEEPING_CHANGES = new Set(["commit", "meta"]);

/** A projected note as the list holds it; recency stays this Mac's own, from `known`. */
function noteFromRow(row: ProjectedNoteRow, known: NoteMeta | undefined): NoteMeta {
  const fields = (row.fields ?? {}) as { title?: unknown; pinned?: unknown };
  const updatedAt = typeof row.updatedAt === "number" ? row.updatedAt : undefined;
  return {
    id: row.id,
    title: String(row.title ?? fields.title ?? ""),
    pinned: fields.pinned === true,
    lastOpenedAt: known?.lastOpenedAt ?? updatedAt ?? 0,
    ...(updatedAt === undefined ? {} : { updatedAt }),
    ...(row.updatedBy ? { updatedBy: row.updatedBy as NoteActor } : {}),
    preview: typeof row.preview === "string" ? row.preview : "",
    refs: Array.isArray(row.refs) ? (row.refs as NoteMeta["refs"]) : [],
    ...(typeof row.headCommit === "string" ? { headCommit: row.headCommit } : {}),
  };
}

/** A save came back merged with someone else's change: show the merged body, unless
 *  the person typed again since (their next save merges, and lands here again). */
function adoptMergedNote(scope: NotesScope, id: string, sent: string, merged: string): void {
  const s = useViewerStore.getState();
  if (s.activeNoteId !== id || !holdsNotesOf(scope) || s.scratchNoteContent !== sent) return;
  rememberNoteBase(scope, id, merged);
  useViewerStore.setState({ scratchNoteContent: merged });
}

const debouncedScratchSave = createDebouncedAction(
  // SW-SN-1: save the active note's body. Args = [scope, noteId, content]. BOTH the
  // scope (profile + org, SLW-PT-19) and the id are captured per-trigger: a deferred
  // flush that lands after an account OR org switch must write to the store the edit
  // was MADE against — resolving the scope at fire time instead would target whatever
  // profile/org is active when the timer fires, so the edit misses its store.
  async (...args: unknown[]) => {
    const [scope, id, content] = args as [NotesScope, string, string];
    const key = noteBaseKey(scope, id);
    const base = noteBases.get(key);
    // Send the body this edit started from, so a change an agent saved meanwhile
    // is merged with the person's edit instead of overwritten by it.
    const written = await saveNoteBody(scope, id, content, base);
    noteBases.set(key, content);
    if (typeof written === "string" && written !== content)
      adoptMergedNote(scope, id, content, written);
    useViewerStore.setState({ noteSavedAt: Date.now() });
  },
  1000,
  "writeNote",
  (scope, noteId) => `${notesScopeKey(scope as NotesScope)}:${String(noteId)}`,
);

// SW-SN-12 AC-3: buffer for keystrokes that arrive with no active note (the
// effectively-unreachable empty-notes branch — SN-1 migration/reseed guarantees
// >=1 note). Rather than drop the edit, hold the latest content and lazily
// create a note to receive it; a single in-flight create absorbs a burst.
// `lazyCreateInFlight` is the whole create→persist chain, exposed so `flushNotes`
// can drain it at a teardown boundary (the buffered content isn't in the
// debounce until the create resolves).
let noActiveNoteContent: string | null = null;
let creatingNoteForBuffer = false;
let lazyCreateInFlight: Promise<void> | null = null;

// SW-SN-13: the default title given to a lazily/deterministically created note.
// MUST match Rust's `DEFAULT_SCRATCH_TITLE` (scratch.rs) — the reaper only reaps
// empty notes still bearing this title, so a drift here would leave FE-minted
// empties un-reapable.
const DEFAULT_SCRATCH_TITLE = "Note";

/** SW-SN-1: the most-recently-opened note (highest `lastOpenedAt`), or
 *  undefined when the list is empty. The active-note pick on load + after a
 *  delete. Copies before sorting so the caller's array isn't mutated. */
function mostRecentNote(notes: NoteMeta[]): NoteMeta | undefined {
  return notes.slice().sort((a, b) => b.lastOpenedAt - a.lastOpenedAt)[0];
}

/** SW-SN-1: monotonic token for active-note ownership. EXPLICIT active-note
 *  changers (`setActiveNote`, `createNote`, `deleteNote`) `++` it before their
 *  first await and gate their terminal set on it, so the last-invoked explicit
 *  action wins regardless of IO resolution order. `loadNotes` is a PASSIVE
 *  refresh — it captures the token WITHOUT bumping and defers to any explicit
 *  change that lands mid-refresh (a refresh must never yank the user off a note
 *  they just switched to). */
let activeSwitchGeneration = 0;

/** SW-SN-1: merge a fresh (disk) note list over the current one, keeping any
 *  current-only ids the stale snapshot lacks (e.g. a note created concurrently
 *  with a refresh) so a deferred `loadNotes` never drops it from the list. */
function mergeNotesById(current: NoteMeta[], fresh: NoteMeta[]): NoteMeta[] {
  const freshIds = new Set(fresh.map((n) => n.id));
  return [...fresh, ...current.filter((n) => !freshIds.has(n.id))];
}

/** `note` in the list once: merged over the row already there (the projection may
 *  have delivered it before the command that made it returned), else appended. */
function upsertNote(notes: NoteMeta[], note: NoteMeta): NoteMeta[] {
  return notes.some((n) => n.id === note.id)
    ? notes.map((n) => (n.id === note.id ? { ...n, ...note } : n))
    : [...notes, note];
}

/** A persisted workspace record as it restores, or `null` when its kind no
 *  longer restores. Registry-driven load policy: `drop` (root, unknown legacy)
 *  → gone. `keep` (story, generic) → kept. `keep-if-valid` (epic) needs an epic
 *  dir + (sourceRepoSlug | epicDocPath) — which ALSO drops the
 *  legacy terminal-less zombie `epic` records (no epicDirPath) and the pre-EW-15
 *  slug-less rows, so reopening re-creates a clean doc-path-keyed row. Code-review
 *  P5: a legacy raw `storyFilePath` is normalised so the fast-path lookups match
 *  the scanner emit; every restored record is `ready`. */
function restoreWorkspaceRecord(saved: Workspace): Workspace | null {
  // SW-GS-2a rename migration: persisted `integration` records become `epic`
  // (the kind was renamed). Raw-string compare because the new `WorkspaceKind`
  // union no longer contains `"integration"`.
  const w = (saved.kind as string) === "integration" ? { ...saved, kind: "epic" as const } : saved;
  const caps = WORKSPACE_KIND_CAPABILITIES[w.kind];
  if (!caps || caps.loadMigration === "drop") return null;
  if (
    caps.loadMigration === "keep-if-valid" &&
    !(w.kind === "context"
      ? w.contextWorktreeId
      : w.epicDirPath && (w.sourceRepoSlug || w.epicDocPath))
  ) {
    return null;
  }
  // A create on a machine that a quit interrupted is not ready: opening the
  // story again reconciles it under the same request.
  if (isRemoteWorkspace(w) && !(w.location.handle && w.location.cwd)) {
    return {
      ...w,
      cwd: "",
      storyFilePath: w.storyFilePath ? normalizeStoryFilePath(w.storyFilePath) : undefined,
      status: "error",
      statusError:
        "Creating this workspace on its machine was interrupted. Open the story again to finish.",
    };
  }
  return {
    ...w,
    ...(isRemoteWorkspace(w) ? { cwd: "" } : {}),
    storyFilePath: w.storyFilePath ? normalizeStoryFilePath(w.storyFilePath) : undefined,
    status: "ready",
  };
}

/** SW-FU-B2: a `persistState` call arrived before the saved state loaded. */
let persistHeldBeforeLoad = false;

/** SW-FU-B2: the saved `openedTeams` (string entries only) followed by any
 *  live entry it lacks. Saved order wins; nothing live is dropped. */
export function mergeOpenedTeams(saved: unknown, live: readonly string[]): string[] {
  const merged = Array.isArray(saved)
    ? (saved as unknown[]).filter((tk): tk is string => typeof tk === "string")
    : [];
  const seen = new Set(merged);
  for (const tk of live) {
    if (!seen.has(tk)) {
      seen.add(tk);
      merged.push(tk);
    }
  }
  return merged;
}

/** A list of teams the agents pane hides: each team key once, in order. Only keys that name a
 *  team survive (not the no-team sentinel, not an empty or colon-less string). A team the
 *  checklist no longer lists is kept, as the funnel keeps it (`toggleTeamFocus`). */
function cleanHiddenTeams(raw: unknown): string[] {
  if (!Array.isArray(raw)) return [];
  const out: string[] = [];
  for (const t of raw) {
    if (typeof t !== "string" || !t.includes(":") || t === NO_TEAM_SENTINEL) continue;
    if (!out.includes(t)) out.push(t);
  }
  return out;
}

/** The agents pane's arrow filter: one workspace of one team (`NO_WORKSPACE_ID` for the team's
 *  agents with no workspace), and the remote machine the workspace runs on, when it does. */
export interface AgentsWorkspaceFilter {
  teamKey: string;
  workspaceId: string;
  machineId?: string;
}

/** `filter` with the remote machine its workspace runs on, read from the workspace's record
 *  (open or minimized); a filter whose record is not here keeps the machine it carries. */
function stampFilterMachine(
  s: {
    workspaces: Readonly<Record<string, readonly Workspace[]>>;
    softClosedWorkspaces: Readonly<Record<string, Readonly<Record<string, SoftClosedWorkspace>>>>;
  },
  filter: AgentsWorkspaceFilter,
): AgentsWorkspaceFilter {
  const { teamKey, workspaceId } = filter;
  const record =
    s.workspaces[teamKey]?.find((w) => w.id === workspaceId) ??
    s.softClosedWorkspaces[teamKey]?.[workspaceId]?.workspace;
  const machineId = record
    ? isRemoteWorkspace(record)
      ? record.location.machineId
      : undefined
    : filter.machineId;
  return machineId ? { teamKey, workspaceId, machineId } : { teamKey, workspaceId };
}

/** The agents pane's arrow filter, from a saved state: one team and workspace, or none. A
 *  filter on a team that is gone is dropped. One on a workspace not among the team's restored
 *  workspaces (open or minimized) is kept here: it is settled once the layouts that could bring
 *  it are read, by the one rule of `arrowFilterFate` (a local workspace's record is gone for
 *  good; a remote one can still come with its machine's layout). `teamKnown` says whether a
 *  team with no restored workspace still exists. */
function sanitizeWorkspaceFilter(
  raw: unknown,
  workspaces: Record<string, readonly Workspace[]>,
  softClosed: Record<string, Record<string, SoftClosedWorkspace>>,
  teamKnown: (teamKey: string) => boolean,
): AgentsWorkspaceFilter | null {
  if (!raw || typeof raw !== "object") return null;
  const { teamKey, workspaceId, machineId } = raw as Record<string, unknown>;
  if (typeof teamKey !== "string" || typeof workspaceId !== "string") return null;
  const exists = teamKey in workspaces || teamKey in softClosed || teamKnown(teamKey);
  if (!exists) return null;
  return stampFilterMachine(
    { workspaces, softClosedWorkspaces: softClosed },
    typeof machineId === "string" && machineId
      ? { teamKey, workspaceId, machineId }
      : { teamKey, workspaceId },
  );
}

/** Saved per-team workspace orders: each team's string ids, de-duplicated, kept to the team's
 *  restored workspaces (open or minimized). A team left with no id has no entry. */
export function sanitizeWorkspaceOrders(
  raw: unknown,
  workspaces: Record<string, readonly Workspace[]>,
  softClosed: Record<string, Record<string, SoftClosedWorkspace>>,
): Record<string, string[]> {
  if (!raw || typeof raw !== "object") return {};
  const out: Record<string, string[]> = {};
  for (const [teamKey, ids] of Object.entries(raw as Record<string, unknown>)) {
    if (!Array.isArray(ids)) continue;
    const live = new Set([
      ...(workspaces[teamKey] ?? []).map((w) => w.id),
      ...Object.keys(softClosed[teamKey] ?? {}),
    ]);
    const kept = [
      ...new Set(ids.filter((id): id is string => typeof id === "string" && live.has(id))),
    ];
    if (kept.length > 0) out[teamKey] = kept;
  }
  return out;
}

/** The stored orders with `teamKey`'s made again from `rebase.from` over `base`: each early move
 *  replayed (one whose workspace or anchor is gone is skipped). No move left → the order it
 *  started from. */
function rebasedWorkspaceOrders(
  s: Pick<
    ViewerState,
    "workspaces" | "softClosedWorkspaces" | "workspaceOrderByTeam" | "railKeyByWorkspaceId"
  >,
  teamKey: string,
  base: readonly string[],
  rebase: { from: string[] | undefined; moves: { workspaceId: string; place: OrderPlace }[] },
): Record<string, string[]> {
  let order = teamWorkspaceOrder({
    base,
    workspaces: s.workspaces[teamKey],
    softClosed: s.softClosedWorkspaces[teamKey],
    stored: rebase.from,
    groups: s.railKeyByWorkspaceId[teamKey],
  });
  let moved = false;
  for (const m of rebase.moves) {
    const next = moveInOrder(order, m.workspaceId, m.place);
    if (next) {
      order = next;
      moved = true;
    }
  }
  if (moved) return updateTeamKeyed(s.workspaceOrderByTeam, teamKey, order);
  if (rebase.from) return updateTeamKeyed(s.workspaceOrderByTeam, teamKey, rebase.from);
  const { [teamKey]: _dropped, ...rest } = s.workspaceOrderByTeam;
  return rest;
}

/** Whether a team's tray order depends on its pipeline tree: it has a story or epic workspace
 *  (open or minimized), which the tree places in its epic and orders by pipeline position. */
function needsTreeForOrder(
  s: Pick<ViewerState, "workspaces" | "softClosedWorkspaces">,
  teamKey: string,
): boolean {
  const records = [
    ...(s.workspaces[teamKey] ?? []),
    ...Object.values(s.softClosedWorkspaces[teamKey] ?? {}).map((shadow) => shadow.workspace),
  ];
  return records.some((w) => getKindCapabilities(w.kind)?.trayGroup === "epic-member");
}

/** A team's stored workspace order without `workspaceId` (closed for good), or the slice as it
 *  was when the order does not hold it. */
function orderWithoutWorkspace(
  orders: Record<string, string[]>,
  teamKey: string,
  workspaceId: string,
): Record<string, string[]> {
  const order = orders[teamKey];
  if (!order?.includes(workspaceId)) return orders;
  return updateTeamKeyed(
    orders,
    teamKey,
    order.filter((id) => id !== workspaceId),
  );
}

/** What closing `teamKey` drops from the agents pane's own state: the team's folds, its place
 *  among the hidden teams (and in a funnel's restore point), an arrow filter on one of its
 *  workspaces, a pending workspace reveal, and its places in the team and
 *  workspace history. */
function agentsPaneWithoutTeam(
  s: Pick<
    ViewerState,
    | "agentsPaneFolds"
    | "agentsPaneHiddenTeams"
    | "agentsWorkspaceFilter"
    | "agentsPaneTeamFocusRestore"
    | "agentsWorkspaceReveal"
    | "contextHistory"
  >,
  teamKey: string,
): Partial<ViewerState> {
  const patch: Partial<ViewerState> = {};
  const folds = Object.keys(s.agentsPaneFolds);
  if (folds.some((key) => teamOfFoldKey(key) === teamKey)) {
    patch.agentsPaneFolds = Object.fromEntries(
      Object.entries(s.agentsPaneFolds).filter(([key]) => teamOfFoldKey(key) !== teamKey),
    );
  }
  if (s.agentsPaneHiddenTeams.includes(teamKey)) {
    patch.agentsPaneHiddenTeams = s.agentsPaneHiddenTeams.filter((t) => t !== teamKey);
  }
  if (s.agentsWorkspaceFilter?.teamKey === teamKey) patch.agentsWorkspaceFilter = null;
  const restore = s.agentsPaneTeamFocusRestore;
  if (restore?.teamKey === teamKey) patch.agentsPaneTeamFocusRestore = null;
  else if (restore?.hidden.includes(teamKey)) {
    patch.agentsPaneTeamFocusRestore = {
      ...restore,
      hidden: restore.hidden.filter((t) => t !== teamKey),
    };
  }
  if (s.agentsWorkspaceReveal?.teamKey === teamKey) patch.agentsWorkspaceReveal = null;
  const history = s.contextHistory;
  if ([...history.back, ...history.forward].some((e) => e.teamKey === teamKey)) {
    const keep = (e: ContextEntry) => e.teamKey !== teamKey;
    patch.contextHistory = {
      back: history.back.filter(keep),
      forward: history.forward.filter(keep),
    };
  }
  return patch;
}

/** A fold key of `oldKey`'s, as it reads once the team is `newKey` (a rename); any other key as
 *  it is. A team header's fold, its no-workspace bucket's and each workspace card's follow it. */
function rekeyedFoldKey(key: string, oldKey: string, newKey: string): string {
  if (teamOfFoldKey(key) !== oldKey) return key;
  if (key === teamFoldKey(oldKey)) return teamFoldKey(newKey);
  if (key === workspaceFoldKey(oldKey, NO_WORKSPACE_ID)) {
    return workspaceFoldKey(newKey, NO_WORKSPACE_ID);
  }
  const managerPrefix = managerFoldKey(oldKey, "");
  if (key.startsWith(managerPrefix)) return managerFoldKey(newKey, key.slice(managerPrefix.length));
  // A workspace card's: the team's prefix, then the workspace id.
  const probe = "x";
  const oldPrefix = workspaceFoldKey(oldKey, probe).slice(0, -probe.length);
  if (!key.startsWith(oldPrefix)) return key;
  return workspaceFoldKey(newKey, key.slice(oldPrefix.length));
}

/** What renaming `oldKey` to `newKey` carries over in the agents pane's own state: the team's
 *  folds, its place among the hidden teams (and in a funnel's restore point), an arrow filter on
 *  one of its workspaces, and its places in the team and workspace history. A pending workspace reveal is dropped (it is momentary). */
function agentsPaneRekeyed(
  s: Pick<
    ViewerState,
    | "agentsPaneFolds"
    | "agentsPaneHiddenTeams"
    | "agentsWorkspaceFilter"
    | "agentsPaneTeamFocusRestore"
    | "agentsWorkspaceReveal"
    | "contextHistory"
  >,
  oldKey: string,
  newKey: string,
): Partial<ViewerState> {
  const patch: Partial<ViewerState> = {};
  const rename = (tk: string) => (tk === oldKey ? newKey : tk);
  const renameAll = (list: readonly string[]) => [...new Set(list.map(rename))];
  if (Object.keys(s.agentsPaneFolds).some((key) => teamOfFoldKey(key) === oldKey)) {
    patch.agentsPaneFolds = Object.fromEntries(
      Object.entries(s.agentsPaneFolds).map(([key, v]) => [rekeyedFoldKey(key, oldKey, newKey), v]),
    );
  }
  if (s.agentsPaneHiddenTeams.includes(oldKey)) {
    patch.agentsPaneHiddenTeams = renameAll(s.agentsPaneHiddenTeams);
  }
  if (s.agentsWorkspaceFilter?.teamKey === oldKey) {
    patch.agentsWorkspaceFilter = { ...s.agentsWorkspaceFilter, teamKey: newKey };
  }
  const restore = s.agentsPaneTeamFocusRestore;
  if (restore && (restore.teamKey === oldKey || restore.hidden.includes(oldKey))) {
    patch.agentsPaneTeamFocusRestore = {
      teamKey: rename(restore.teamKey),
      hidden: renameAll(restore.hidden),
    };
  }
  if (s.agentsWorkspaceReveal?.teamKey === oldKey) patch.agentsWorkspaceReveal = null;
  const history = s.contextHistory;
  if ([...history.back, ...history.forward].some((e) => e.teamKey === oldKey)) {
    const renamed = (e: ContextEntry) => (e.teamKey === oldKey ? { ...e, teamKey: newKey } : e);
    patch.contextHistory = {
      back: history.back.map(renamed),
      forward: history.forward.map(renamed),
    };
  }
  return patch;
}

function serializedViewerPrefs(state: ViewerState): Record<string, unknown> {
  const {
    openTabs,
    activeTab,
    drillPaths,
    teamPipelineRepoPath,
    navCollapsed,
    navLens,
    agentsPaneHiddenTeams,
    agentsWorkspaceFilter,
    foregroundLayout,
    foregroundLayoutByRail,
    trayMode,
    workspaceOrderByTeam,
    customGroupsByTeam,
    workspaceGroupVersionsByTeam,
    trayBaseOrderByTeam,
    rightPaneOpen,
    rightPaneRatio,
    workspaceTraySeenCoachmark,
    cliShortcutsHintSeen,
    dismissedTerminalServers,
    rightPaneSplitRatio,
    rightPaneLayout,
    statusNavExpandedPaths,
    settings,
    softClosedWorkspaces,
    pipelineScope,
    selectedAgentContext,
    workspaces,
    activeWorkspace,
    openedTeams,
  } = state;
  // SW-GS-2b (AC-11a): synthetic "virtual" tabs (`virtual://` scheme) are
  // ephemeral UI surfaces with no disk backing — they MUST NOT persist. A
  // restored sentinel tab would route through the extension dispatch to
  // the markdown viewer and fail `readFile` → a broken error tab on every
  // launch. Strip them before serialization (mirrors the role-tagged
  // terminal-tab not-persisted precedent).
  const persistedOpenTabs: Record<string, TabState[]> = {};
  for (const [tk, tabs] of Object.entries(openTabs)) {
    const kept = tabs.filter((t) => !isVirtualTabPath(t.path));
    if (kept.length > 0) persistedOpenTabs[tk] = kept;
  }
  const persistedActiveTab: Record<string, string> = {};
  for (const [tk, path] of Object.entries(activeTab)) {
    if (!isVirtualTabPath(path)) {
      persistedActiveTab[tk] = path;
    } else {
      // The active tab was synthetic (not persisted) — fall back to the
      // first surviving real tab so the team doesn't reopen with a blank
      // viewer (rather than dropping the active pointer entirely).
      const firstReal = persistedOpenTabs[tk]?.[0]?.path;
      if (firstReal) persistedActiveTab[tk] = firstReal;
    }
  }
  // SLW-FX-12: never persist a divergent (openTabs, activeTab). A real
  // active path with no surviving tab is reconstructed into its tab so the
  // blob restores as doc + tab (not a bare pointer → "file open, no tab").
  // With the setActiveTab guard this is usually a no-op — it's the
  // belt-and-suspenders against any direct openTabs mutation.
  const reconciledPersist = reconcileActiveTabs(persistedOpenTabs, persistedActiveTab);
  // F-18: serialize open workspaces, stripping runtime-only statusError.
  // T.1: outer keys are teamKeys (post-re-keying); loop variable renamed
  // `rp → tk` for honesty.
  // SLW-CO-5: don't persist a NON-ready story workspace. Both transient
  // states SLW-CO-5 introduces carry `cwd = worktreePath` for a worktree that
  // is either not yet created (`"creating"`, mid-setup) or was never
  // successfully created (`"error"`, setup failed). Since load coerces every
  // restored workspace to `status:"ready"`, persisting either would restore a
  // "ready" tile whose shell chdir's into a missing / half-provisioned dir,
  // and the ready fast-path would never re-run setup to heal it. Drop them —
  // they're transient; the user re-opens (which re-runs setup). (Epic
  // "creating" is a different, dir-resolving state with its own restore path,
  // so only `kind:"story"` is filtered.)
  // RF-perf-14: `backgroundSetup` is runtime-only too (a live setup process /
  // retry latch) — restoring it as "still running" after a restart would be
  // a lie, so strip it alongside `statusError`.
  const serializedWorkspaces: Record<
    string,
    Array<
      Omit<
        Workspace,
        "statusError" | "backgroundSetup" | "setupState" | "keptModified" | "agentRuns"
      >
    >
  > = {};
  for (const [tk, ws] of Object.entries(workspaces)) {
    // A workspace being created on a machine is kept: the machine prepares
    // only a workspace the pushed viewer state holds, and its request id
    // must survive a relaunch so the create reconciles, never repeats.
    serializedWorkspaces[tk] = ws
      .filter((w) => !(w.kind === "story" && w.status !== "ready" && !isRemoteWorkspace(w)))
      .map(
        ({
          statusError: _,
          backgroundSetup: _bg,
          setupState: _ss,
          keptModified: _km,
          agentRuns: _ar,
          ...rest
        }) => rest,
      );
  }
  // Minimized workspaces round-trip as their record + the CLI's layout
  // mode at close (their terminals live in the host), so a workspace
  // minimized at quit comes back minimized. Same record filter/strip as the
  // open workspaces above.
  const serializedSoftClosed: Record<
    string,
    Record<string, Omit<SoftClosedWorkspace, "workspace"> & { workspace: object }>
  > = {};
  for (const [tk, byWorkspace] of Object.entries(softClosedWorkspaces)) {
    for (const [wsId, shadow] of Object.entries(byWorkspace)) {
      const w = shadow.workspace;
      if (w.kind === "story" && w.status !== "ready" && !isRemoteWorkspace(w)) continue;
      const {
        statusError: _,
        backgroundSetup: _bg,
        setupState: _ss,
        keptModified: _km,
        agentRuns: _ar,
        ...record
      } = w;
      (serializedSoftClosed[tk] ??= {})[wsId] = {
        workspace: record,
      };
    }
  }
  return {
    // T.2 AC-10: schemaVersion 2 marks the per-source-repo root shape
    // (terminal slices nested by root id under each team).
    // T.4 D-11: schemaVersion 3 marks the post-T.4 team-keyed shape
    // (17 Record slices + AC-3 scalar reshapes + D-5 narrowed
    // workspaceViewerStates) layered on top of T.2's nested terminals.
    // T.6 AC-7.4: schemaVersion 4 drops `selectedRepoPath`, `viewMode`,
    // `openedRepos`, and `workspaceViewerStates`; introduces
    // `openedTeams`. SS-FU-12: v5 rewrote the org-slug prefix of every
    // team key `owner--repo` → `owner/repo`; v6 layered on the deep
    // absolute-path rewrite v5 missed; v7 covered teamKey strings stored
    // in the `openedTeams` array; v8 dedupes openedTeams after the
    // rewrite (v7 could leave duplicates when a fresh `owner/repo:team`
    // entry coexisted with a stale `owner--repo:team`). Chain on load:
    // missing → T.1 → T.2 → T.4 → T.6 → v8; 1 → T.2 → T.4 → T.6 → v8;
    // 2 → T.4 → T.6 → v8; 3 → T.6 → v8; 4..7 → v8; 8 → v9;
    // 9 → v10 (SLW-IN-8 `_team/` flatten path backfill); 10 → v11 (SW-XS-3
    // drops the forward-sync settings); 11 → pass-through. The raw-blob steps
    // are the ordered chain in `viewer-schema-migrations.ts`.
    // SW-EW-2: the load restore SWEEPS (drops) any persisted `kind:"root"`,
    // `kind:"epic"`, and epic-less `kind:"integration"` record — codesense
    // is story-organized, so these have no home in the flat panel. The sweep
    // is idempotent and runs unconditionally on load regardless of stored
    // version, so it needs no schemaVersion bump.
    schemaVersion: VIEWER_SCHEMA_VERSION,
    // SLW-FX-12: write the reconciled slices so `activeTab` is always backed.
    openTabs: reconciledPersist.openTabs,
    activeTab: reconciledPersist.activeTab,
    drillPaths,
    // SLW-FX-14: additive teamKey-keyed record; missing → {} on load, so
    // no schemaVersion bump needed.
    teamPipelineRepoPath,
    // D39: the CLI's chrome, one value for every team.
    cliChrome: cliChromeOf(state),
    navCollapsed,
    navLens,
    agentsPaneHiddenTeams,
    agentsWorkspaceFilter,
    foregroundLayout,
    // SW-EW-40: persist the per-rail foreground arrangement (railKeyByWorkspaceId
    // is runtime-only — a pure tree derivation — so it is NOT serialized here,
    // like selectedEpicDirName).
    foregroundLayoutByRail,
    // SW-EW-38: persist the per-team tray lens (selectedEpicDirName is
    // runtime-only — R3 — so it is deliberately NOT serialized here).
    trayMode,
    // The person's shared workspace order (pills and pane cards), per team, and the last
    // computed tray order it applies over (so a restart keeps it before the tree syncs).
    // Kept to the workspaces saved beside them: one dropped by any path leaves both.
    customGroupsByTeam: sanitizeWorkspaceGroups(customGroupsByTeam),
    workspaceGroupVersionsByTeam,
    workspaceOrderByTeam: sanitizeWorkspaceOrders(
      workspaceOrderByTeam,
      workspaces,
      softClosedWorkspaces,
    ),
    trayBaseOrderByTeam: sanitizeWorkspaceOrders(
      trayBaseOrderByTeam,
      workspaces,
      softClosedWorkspaces,
    ),
    rightPaneOpen,
    rightPaneRatio,
    // SW-EW-33 (AC-8): additive global scalar — survives restart so the
    // one-time tray coachmark never reappears after dismissal.
    workspaceTraySeenCoachmark,
    // OC-17: additive global scalar — the one-time CLI shortcuts hint stays
    // dismissed across restarts.
    cliShortcutsHintSeen,
    dismissedTerminalServers,
    rightPaneSplitRatio,
    // SW-RP-1: persist the structured layout alongside the legacy scalars
    // (kept for back-compat read on older builds).
    rightPaneLayout,
    // The paned remote tabs of each workspace's grid (This Mac's host
    // holds the rest), so the grid comes back whole after a relaunch.
    terminalViewerGrid: state.terminalLayout.grids ?? {},
    // Remote tab focus belongs to this viewer; local focus comes from its host.
    terminalViewerFocus: state.terminalLayout.focus ?? {},
    // The sizes each workspace's tab grid was dragged to (this viewer's own).
    terminalTabGridSizes: state.tabGridLayout,
    statusNavExpandedPaths,
    settings,
    pipelineScope,
    selectedAgentContext,
    workspaces: serializedWorkspaces,
    softClosedWorkspaces: serializedSoftClosed,
    activeWorkspace,
    openedTeams,
  };
}

let pendingSettings: Partial<ViewerSettings> = {};
let persistedViewer: Record<string, unknown> | undefined;
const ownedViewerKeys = new Set<string>();

const debouncedPersist = createDebouncedAction(
  () =>
    serializeAppStateWrite(async () => {
      try {
        const existing: { viewer: Record<string, unknown> } = { viewer: {} };
        const settingsPatch = { ...pendingSettings };
        const candidate = serializedViewerPrefs(useViewerStore.getState());
        const previous = Object.fromEntries(
          Object.keys(candidate)
            .filter((key) => key !== "settings")
            .map((key) => [key, persistedViewer?.[key]]),
        );
        const next = { ...candidate };
        delete next.settings;
        existing.viewer = (fieldPatch(previous, next) ?? {}) as Record<string, unknown>;
        if (Object.keys(settingsPatch).length)
          existing.viewer.settings = Object.fromEntries(
            Object.entries(settingsPatch).map(([key, value]) => [
              key,
              value === undefined ? null : value,
            ]),
          );
        const baseline = persistedViewer;
        await saveAppState(JSON.stringify(existing), true);
        if (persistedViewer === baseline) persistedViewer = structuredClone(candidate);
        for (const key of Object.keys(settingsPatch) as Array<keyof ViewerSettings>) {
          if (pendingSettings[key] === settingsPatch[key]) delete pendingSettings[key];
        }
        // Flush also awaits the native mirror of the latest visible state.
        await syncLiveViewerMirror();
        logger.info("viewer", "persistState: success");
      } catch (e) {
        logger.error("viewer", `persistState: failed — ${e}`);
        throw e;
      }
    }),
  300,
  "persistState",
);

/**
 * Add a story workspace to the store if it doesn't already exist.
 * Shared by `ensureWorktree` (on-disk detection + creation paths).
 * Returns true if a new workspace was added, false if it already existed.
 */
interface AddStoryWorkspaceParams {
  /** T.1: bucket key — `teamKey` post-re-keying. Was `repoPath` pre-T.1. */
  teamKey: string;
  /** R-18: identity key — path-derived via `storyIdFromPath`. Used as `Workspace.id`. */
  workspaceId: string;
  /** R-18: display label from frontmatter `story_id`. Falls back to `workspaceId` when absent. */
  storyId?: string;
  cwd: string;
  storyFilePath: string;
  storyTitle?: string;
  sourceRepoPath?: string;
  sourceRepoSlug?: string;
  /** SLW-CO-5: initial lifecycle state. The open path adds the record as
   *  `"creating"` BEFORE `setupWorktree` resolves (so the terminal panel +
   *  inline overlay mount immediately), then flips it to `"ready"` on
   *  success. Every other caller (on-disk detection) omits it → `"ready"`. */
  status?: "ready" | "creating";
  /** SW-XS-10: the initial setup state (only meaningful with `status: "creating"`). */
  setupState?: WorkspaceSetupState;
  /** SLW-CO-7 (AC-1): mark that this workspace materializes after create, so the
   *  overlay owns the whole sync. Set by the `ensureWorktree` create path; the
   *  on-disk detection callers omit it (already materialized → no overlay). */
  needsSyncOverlay?: boolean;
  /** Where it lives, for a workspace created on a machine. */
  location?: WorkspaceLocation;
}

/** T.3 D-9: derive the team's `source_repos` slug order for canonical-sort
 *  callers. Reads `useOrgStore.workspaceRepos` (insertion-ordered in
 *  `teams.yml.source_repos` order via `use-workspace-viewer-sync`) and
 *  parses each synthetic key to recover `${owner}/${repo}`.
 *
 *  Pure helper kept inside viewer-store because it touches an imperative
 *  org-store read — keeping it out of `team-key.ts` preserves that
 *  module's "no store imports beyond org-store hooks" discipline. */
function teamSourceRepoSlugsInOrder(teamKey: string): string[] {
  const repos = workspaceReposForTeam(useOrgStore.getState().workspaceRepos, teamKey);
  const slugs: string[] = [];
  for (const r of repos) {
    const parsed = parseSyntheticRepoKey(r.repoPath);
    if (parsed) slugs.push(`${parsed.owner}/${parsed.repo}`);
  }
  return slugs;
}

function addStoryWorkspace(
  set: (fn: (s: ViewerState) => Partial<ViewerState> | ViewerState) => void,
  params: AddStoryWorkspaceParams,
): boolean {
  let added = false;
  // R-18 Phase B (AC #28): normalise at write time so every workspace's
  // persisted `storyFilePath` is byte-comparable to the scanner's emitted
  // `StoryRef.filePath` after `byAbsPath` build.
  const normalizedStoryFilePath = normalizeStoryFilePath(params.storyFilePath);
  // T.3 D-9: capture slug order BEFORE the set() callback so the org-store
  // read is observed-once and consistent with the sort applied inside.
  const slugsInOrder = teamSourceRepoSlugsInOrder(params.teamKey);
  set((s) => {
    if ((s.workspaces[params.teamKey] ?? []).some((w) => w.id === params.workspaceId)) return s;
    added = true;
    const newWorkspace: Workspace = {
      id: params.workspaceId,
      kind: "story",
      cwd: params.cwd,
      storyFilePath: normalizedStoryFilePath,
      storyId: params.storyId ?? params.workspaceId,
      storyTitle: params.storyTitle,
      status: params.status ?? "ready",
      ...(params.setupState ? { setupState: params.setupState } : {}),
      // SLW-CO-6 (AC-3): a story worktree create is cancellable while "creating"
      // (its `setupWorktree` is abortable) — the overlay shows Cancel only then.
      // A machine's preparation runs to its end there; it is not cancellable.
      setupCancellable: params.status === "creating" && !params.location ? true : undefined,
      // SLW-CO-7 (AC-1): a materialize-firing create (the ensureWorktree open
      // path) marks the record so the overlay owns create → materialize. On-disk
      // detection callers omit it → born ready, no overlay (AC-5 opt-out).
      needsSyncOverlay: params.needsSyncOverlay,
      sourceRepoPath: params.sourceRepoPath,
      sourceRepoSlug: params.sourceRepoSlug,
      ...(params.location ? { location: params.location } : {}),
    };
    // T.3 D-9: append then re-sort into canonical order so the bucket
    // grouping (roots-first by source_repo, then stories) is maintained.
    const appended = [...(s.workspaces[params.teamKey] ?? []), newWorkspace];
    const sorted = sortWorkspacesByCanonicalOrder(appended, slugsInOrder);
    return {
      workspaces: {
        ...s.workspaces,
        [params.teamKey]: sorted,
      },
    };
  });
  return added;
}

/**
 * SLW-CO-5 → SLW-CO-6: transition a workspace's setup lifecycle in place.
 * Kind-agnostic (touches only base-`Workspace` fields, guards on
 * `status:"creating"`) — renamed from `updateStoryWorkspaceSetup` in SLW-CO-6
 * so the unified `openWorkspace` core can drive every kind through it.
 *
 * The record add (`addStoryWorkspace`) no-ops on an existing record (it only ADDS), so the
 * `"creating" → "ready"/"error"` flip and the live sub-label updates are a
 * separate in-place mutation. Guarded on the record still being in
 * `status: "creating"`, so a late settle for a superseded / already-torn-down
 * workspace (e.g. cancel removed it, or a re-open already flipped it) is a
 * no-op — the same "only mutate the live op's slot" discipline the old toast
 * singleton enforced via its `state.id` guard.
 *
 * Field semantics:
 *   - `setupState` present → update the setup line (stays "creating").
 *   - `status: "ready"`    → reveal the terminal; the setup line takes the
 *                            patch's `setupState` (the wait for an expected
 *                            agent) or clears — Step 1 alone never says Ready.
 *   - `status: "error"`    → stamp `statusError` (the frame swaps to the
 *                            shared `WorkspaceErrorPlaceholder`).
 */
type WorkspaceSetupPatch = {
  status?: "ready" | "error";
  statusError?: string;
  setupState?: WorkspaceSetupState;
};

/** What a preparation kept because it was modified (the first paths, and the
 *  full count). */
export interface KeptModified {
  agent: string;
  paths: string[];
  count: number;
}

/** Value equality for setup states (a same-phase progress line resolves to an
 *  equal state, so it must not churn subscribers). */
export function setupStateEqual(
  a: WorkspaceSetupState | undefined,
  b: WorkspaceSetupState | undefined,
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * Pure core of {@link updateWorkspaceSetup}: apply a setup patch to the matching
 * `"creating"` workspace in a team's list, returning the SAME array reference
 * when the patch is a no-op (every resolved field already equals the current
 * value).
 *
 * RF-perf-14c Prong 3 — kill the `onProgress` set()-storm. The worktree-setup
 * stream fires `onProgress` per output LINE (npm/cargo can emit hundreds), and
 * each event lands in a Tauri `listen()` callback (`lib/worktree/setup.ts`) that
 * ESCAPES React 18 auto-batching, so every write is its own render. But
 * `setupState` is phase-keyed (`step1StateFromLocalPhase`), so same-phase lines
 * resolve to an EQUAL patch. Returning the same array ref on a no-op
 * lets the caller return `s` unchanged → Zustand's `Object.is(next, prev)`
 * short-circuit fires → zero subscriber notifications. Net: N same-phase lines
 * collapse to exactly one re-render per real phase transition, with no new
 * timer/scheduler/debounce (NB-4). Real changes (phase transition, `→ready`
 * label-clear, `→error`) still allocate and notify as before.
 */
export function patchWorkspaceSetupList(
  list: Workspace[] | undefined,
  workspaceId: string,
  patch: WorkspaceSetupPatch,
): Workspace[] | undefined {
  if (!list) return list;
  let changed = false;
  const next = list.map((w) => {
    if (w.id !== workspaceId || w.status !== "creating") return w;
    const nextStatus = patch.status ?? w.status;
    // Flip to "ready" takes the patch's line (or none); an error keeps the
    // failure line a Step 1 failure already wrote; an update keeps "creating".
    const nextSetupState =
      patch.status === "ready"
        ? patch.setupState
        : patch.status === "error" && !patch.setupState
          ? w.setupState?.kind === "failed"
            ? w.setupState
            : {
                kind: "failed" as const,
                step: 1 as const,
                phase: setupFailedPhase(w.setupState),
                reason: patch.statusError ?? "the workspace couldn't be created",
              }
          : (patch.setupState ?? w.setupState);
    const nextStatusError = patch.status === "error" ? patch.statusError : w.statusError;
    // Only allocate a new workspace object on a REAL field change: a same-value
    // write (a same-phase progress line) leaves `w` untouched so the whole
    // reducer can return the input list and the caller can no-op the `set()`.
    if (
      nextStatus === w.status &&
      setupStateEqual(nextSetupState, w.setupState) &&
      nextStatusError === w.statusError
    ) {
      return w;
    }
    changed = true;
    const { setupState: _prev, ...rest } = w;
    return {
      ...rest,
      status: nextStatus,
      statusError: nextStatusError,
      ...(nextSetupState ? { setupState: nextSetupState } : {}),
    };
  });
  return changed ? next : list;
}

/** The Step 1 phase a failure stopped in: the one the line last showed. */
function setupFailedPhase(state: WorkspaceSetupState | undefined): string {
  return state?.kind === "step1" ? state.phase : "starting";
}

/** SW-XS-10: agent launches expected to follow a workspace's Step 1, by
 *  `${teamKey}\0${workspaceId}`. When Step 1 finishes for one of them the line
 *  waits for that agent's Step 2 ("Preparing agent context"), never Ready. */
const expectedAgentLaunches = new Set<string>();

/** Say an agent launch follows this workspace's creation (the launch paths call
 *  it before they create the workspace). */
export function expectAgentLaunch(teamKey: string, workspaceId: string): void {
  expectedAgentLaunches.add(`${teamKey}\0${workspaceId}`);
}

/** The launch that was expected will not come (it failed before Step 1
 *  finished): a later create of this workspace waits for no agent. */
export function forgetAgentLaunch(teamKey: string, workspaceId: string): void {
  expectedAgentLaunches.delete(`${teamKey}\0${workspaceId}`);
}

/** Tests only: whether a launch is still expected for this workspace. */
export function agentLaunchExpected(teamKey: string, workspaceId: string): boolean {
  return expectedAgentLaunches.has(`${teamKey}\0${workspaceId}`);
}

/** Replace a workspace's setup slots (base line + agent runs). */
function withSlots(
  w: Workspace,
  slots: { base?: WorkspaceSetupState; runs?: Record<string, AgentRun> },
): Workspace {
  const { setupState: _s, agentRuns: _r, ...rest } = w;
  return {
    ...rest,
    ...(slots.base ? { setupState: slots.base } : {}),
    ...(slots.runs && Object.keys(slots.runs).length ? { agentRuns: slots.runs } : {}),
  };
}

const sameSlots = (w: Workspace, next: Workspace) =>
  setupStateEqual(w.setupState, next.setupState) &&
  JSON.stringify(w.agentRuns) === JSON.stringify(next.agentRuns);

/** The setup line once Step 1 finished: the wait for an expected agent, or
 *  nothing (the terminal itself is the signal; Ready needs a Step 2). */
function setupAfterStep1(teamKey: string, workspaceId: string): WorkspaceSetupState | undefined {
  const key = `${teamKey}\0${workspaceId}`;
  if (!expectedAgentLaunches.delete(key)) return undefined;
  return { kind: "awaitingAgent", since: Date.now() };
}

/** Whether `ws` is the workspace a Step 2 event names: its local directory,
 *  or its handle on a machine. */
function workspaceMatchesIdentity(ws: Workspace, identity: string): boolean {
  const norm = (p: string) => p.replace(/\/+$/, "").replace(/^\/private(?=\/)/, "");
  const id = norm(identity);
  if (isRemoteWorkspace(ws)) return !!ws.location.handle && ws.location.handle === identity;
  return !!ws.cwd && norm(ws.cwd) === id;
}

function updateWorkspaceSetup(
  set: (fn: (s: ViewerState) => Partial<ViewerState> | ViewerState) => void,
  teamKey: string,
  workspaceId: string,
  patch: WorkspaceSetupPatch,
): void {
  set((s) => {
    const list = s.workspaces[teamKey];
    const next = patchWorkspaceSetupList(list, workspaceId, patch);
    // `next === list` ⇒ no real change ⇒ return `s` so Zustand skips the notify.
    return next === list ? s : { workspaces: updateTeamKeyed(s.workspaces, teamKey, next!) };
  });
}

/** SW-WT-12 (S2): content-wise checklist compare so a step transition re-renders
 *  while an unchanged step list (e.g. a per-line `onProgress` tick) still dedups
 *  (preserving the RF-perf-14c no-churn guard). */
function stepsEqual(a?: WorktreeSetupStep[], b?: WorktreeSetupStep[]): boolean {
  if (a === b) return true;
  if (!a || !b || a.length !== b.length) return false;
  return a.every((s, i) => {
    const t = b[i];
    return (
      s.index === t.index &&
      s.command === t.command &&
      s.status === t.status &&
      s.exitCode === t.exitCode
    );
  });
}

function backgroundSetupEqual(
  a: BackgroundSetupState | undefined,
  b: BackgroundSetupState | undefined,
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  if (a.status !== b.status) return false;
  if (a.status === "failed" && b.status === "failed") {
    return a.error === b.error && a.diagnostics === b.diagnostics;
  }
  if (a.status === "failed" || b.status === "failed") return false;
  return a.label === b.label && stepsEqual(a.steps, b.steps);
}

/**
 * RF-perf-14 (decouple) — pure reducer for a workspace's BACKGROUND-setup badge
 * state. Sets/clears `backgroundSetup` on the matching workspace, returning the
 * SAME array reference on a no-op (the same value-equality guard as
 * {@link patchWorkspaceSetupList}: the background `onProgress` stream fires per
 * output line, so same-phase updates must not churn subscribers). Unlike
 * `patchWorkspaceSetupList` this targets a workspace in ANY status — the setup
 * badge outlives the `"creating"` → `"ready"` flip — keyed only by id.
 */
export function setWorkspaceBackgroundSetup(
  list: Workspace[] | undefined,
  workspaceId: string,
  next: BackgroundSetupState | undefined,
): Workspace[] | undefined {
  if (!list) return list;
  let changed = false;
  const out = list.map((w) => {
    if (w.id !== workspaceId) return w;
    if (backgroundSetupEqual(w.backgroundSetup, next)) return w;
    changed = true;
    return { ...w, backgroundSetup: next };
  });
  return changed ? out : list;
}

/** Store-facing wrapper: apply {@link setWorkspaceBackgroundSetup} under `set`,
 *  no-op'ing the notification when nothing changed (same contract as
 *  {@link updateWorkspaceSetup}). */
function updateWorkspaceBackgroundSetup(
  set: (fn: (s: ViewerState) => Partial<ViewerState> | ViewerState) => void,
  teamKey: string,
  workspaceId: string,
  next: BackgroundSetupState | undefined,
): void {
  set((s) => {
    const list = s.workspaces[teamKey];
    const updated = setWorkspaceBackgroundSetup(list, workspaceId, next);
    return updated === list ? s : { workspaces: updateTeamKeyed(s.workspaces, teamKey, updated!) };
  });
}

/** Create (or finish creating) a story workspace on a machine: the record goes
 *  in as `creating` at once (the creating overlay shows the machine's progress),
 *  the viewer state is pushed (the backend prepares only a workspace it holds),
 *  then the machine prepares the worktree under the record's one request id.
 *  Resolves the worktree on the machine; the workspace's terminals open there. */
async function openStoryWorkspaceOnMachine(input: {
  get: () => ViewerState;
  set: (fn: (s: ViewerState) => Partial<ViewerState> | ViewerState) => void;
  teamKey: string;
  workspaceId: string;
  storyFilePath: string;
  machineId: string;
  prior: (Workspace & { location: MachineLocation }) | undefined;
  storyId?: string;
  storyTitle?: string;
  sourceRepoPath: string;
  sourceRepoSlug: string;
  baseRef: string;
  branch: string;
  onCreating?: () => void;
  spawnFirstTerminal: boolean;
  stillOpen: () => boolean;
  throwIfCancelled: () => void;
}): Promise<string> {
  const { get, set, teamKey, workspaceId } = input;
  const driver = remoteWorkspaceDriver();
  if (!driver) {
    const message = "This build can't create workspaces on machines";
    toast.error(message);
    throw failedStepError(message, "create");
  }
  if (!get().stateLoaded) {
    // The machine prepares only a workspace the pushed viewer state holds.
    throw failedStepError("CodeSense is still loading. Open the story again in a moment.", "setup");
  }
  // A repeat replays the request as it was made: the same id, base and branch.
  const prior = input.prior?.location;
  const location: MachineLocation = {
    kind: "machine",
    machineId: input.machineId,
    requestId: prior?.requestId ?? crypto.randomUUID(),
    baseRef: prior?.baseRef ?? input.baseRef,
    branch: prior?.branch ?? input.branch,
  };
  const preparing: WorkspaceSetupState = { kind: "step1", phase: "starting" };
  if (input.prior) {
    set((s) => ({
      workspaces: updateTeamKeyed(
        s.workspaces,
        teamKey,
        readTeamKeyed(s.workspaces, teamKey, []).map((w) =>
          w.id === workspaceId
            ? {
                ...w,
                cwd: "",
                status: "creating" as const,
                statusError: undefined,
                setupState: preparing,
                setupCancellable: undefined,
                location,
              }
            : w,
        ),
      ),
    }));
  } else {
    addStoryWorkspace(set, {
      teamKey,
      workspaceId,
      storyId: input.storyId,
      cwd: "",
      storyFilePath: input.storyFilePath,
      storyTitle: input.storyTitle,
      sourceRepoPath: input.sourceRepoPath,
      sourceRepoSlug: input.sourceRepoSlug,
      status: "creating",
      setupState: preparing,
      needsSyncOverlay: false,
      location,
    });
  }
  input.onCreating?.();
  const stillMine = () =>
    input.stillOpen() &&
    readTeamKeyed(get().workspaces, teamKey, []).some(
      (w) => w.id === workspaceId && w.location?.requestId === location.requestId,
    );
  try {
    await get().flushPersist();
    input.throwIfCancelled();
    const prepared = await driver.prepare({
      teamKey,
      workspaceId,
      storyId: input.storyId ?? workspaceId,
      machineId: location.machineId,
      requestId: location.requestId,
      sourceRepo: input.sourceRepoSlug,
      baseRef: location.baseRef ?? input.baseRef,
      branch: location.branch ?? input.branch,
      onProgress: (state) => updateWorkspaceSetup(set, teamKey, workspaceId, { setupState: state }),
    });
    if (!stillMine()) throw failedStepError("Worktree open cancelled", "cancelled");
    // SW-XS-10: Step 1 is done; Ready waits for Step 2 (X5). The terminal opens
    // now (a shell needs no agent context); the line waits for an expected agent.
    const afterStep1 = setupAfterStep1(teamKey, workspaceId);
    // The machine may have replayed an earlier preparation of this worktree:
    // keep the identity it was made under.
    const ready: MachineLocation = {
      ...location,
      requestId: prepared.requestId,
      baseRef: prepared.baseRef,
      branch: prepared.branch,
      handle: prepared.handle,
      cwd: prepared.cwd,
    };
    set((s) => ({
      workspaces: updateTeamKeyed(
        s.workspaces,
        teamKey,
        readTeamKeyed(s.workspaces, teamKey, []).map((w) =>
          w.id === workspaceId
            ? (() => {
                const { setupState: _s, ...rest } = w;
                return {
                  ...rest,
                  status: "ready" as const,
                  location: ready,
                  ...(afterStep1 ? { setupState: afterStep1 } : {}),
                };
              })()
            : w,
        ),
      ),
    }));
    void get().flushPersist();
    if (input.onCreating && input.spawnFirstTerminal)
      ensureFirstTerminalTab(get, teamKey, workspaceId);
    return prepared.cwd;
  } catch (error) {
    if ((error as { failedStep?: string }).failedStep === "cancelled") throw error;
    const message = getErrorMessage(error);
    expectedAgentLaunches.delete(`${teamKey}\0${workspaceId}`);
    if (stillMine())
      updateWorkspaceSetup(set, teamKey, workspaceId, { status: "error", statusError: message });
    // Shown on the workspace itself (its error placeholder), not as a toast.
    throw failedStepError(message, "setup");
  }
}

/** Where a workspace (open or minimized) lives: `"local"`, a machine id, or
 *  `null` when there is no such workspace. */
function whereWorkspaceLives(s: ViewerState, teamKey: string, workspaceId: string): string | null {
  const ws =
    readTeamKeyed(s.workspaces, teamKey, []).find((w) => w.id === workspaceId) ??
    s.softClosedWorkspaces[teamKey]?.[workspaceId]?.workspace;
  if (!ws) return null;
  return isRemoteWorkspace(ws) ? ws.location.machineId : "local";
}

/** SW-WS-1: optional `movingTo` swaps the "lives on X" wording for "being moved to Y;
 *  try again shortly" so an in-flight move's open attempts get an actionable
 *  message instead of looking like a hard refusal. The predicate itself is
 *  unchanged. The renderer uses the raw machine id (R1 nit: resolving the
 *  human label here would close the viewer ↔ machines store import cycle
 *  for a small UX win — deferred). */
function locationConflictMessage(label: string, at: string, movingTo?: string): string {
  if (movingTo) {
    const target = movingTo === "local" ? "This Mac" : `machine ${movingTo}`;
    return `${label}'s workspace is being moved to ${target}; try again shortly`;
  }
  return `${label}'s workspace already lives on ${at === "local" ? "This Mac" : `machine ${at}`}; a workspace stays where it was created`;
}

/** SW-WS-1: resolve the "move in progress" target for soft-refusal messaging.
 *  Returns the target machine id (or `"local"`) when a move is in flight for
 *  this workspace, else `undefined`. Reads from the FE's `setupState`
 *  projection of the Rust-side `MoveIntent` (rehydrated at init via
 *  `list_move_intents` or on event). */
function movingTargetFor(s: ViewerState, teamKey: string, workspaceId: string): string | undefined {
  const ws = readTeamKeyed(s.workspaces, teamKey, []).find((w) => w.id === workspaceId);
  const setup = ws?.setupState as
    | { phase?: string; to?: { machineId?: string | null } }
    | undefined;
  if (setup?.phase !== "moving") return undefined;
  const to = setup.to?.machineId;
  return to == null ? "local" : to;
}

/** First terminals being opened by a user's open of their workspace, by
 *  `${teamKey}\0${workspaceId}` — so the nav registration that follows the open
 *  can register the session before its host announces it. */
const firstTabOpens = new Map<string, { sessionId: string; opened: Promise<void> }>();

/** Open a workspace's first shell tab iff it has none — the single "a user's
 *  open of a workspace gives it a terminal" idiom shared by the story-open,
 *  epic, and generic open paths. Never while the host layout is still unknown:
 *  until the first snapshot lands, "no tabs" means "not known yet" (restore
 *  never spawns). */
function ensureFirstTerminalTab(
  get: () => ViewerState,
  teamKey: string,
  workspaceId: string,
): void {
  const s = get();
  if (!s.terminalLayout.hydrated) {
    // Until the hosts' layout is known, "no tabs" is not "empty": decide once
    // it is (the workspace may already hold terminals).
    const unsubscribe = useViewerStore.subscribe((next) => {
      if (!next.terminalLayout.hydrated) return;
      unsubscribe();
      ensureFirstTerminalTab(get, teamKey, workspaceId);
    });
    return;
  }
  if (selectWorkspaceTabs(s, teamKey, workspaceId).length > 0) return;
  const record = workspaceRecord(s, teamKey, workspaceId);
  if (isRemoteWorkspace(record)) {
    // On its machine, at its worktree; never a local shell in its stead.
    if (record.status === "ready")
      remoteWorkspaceDriver()?.openTerminal({
        teamKey,
        workspaceId,
        location: record.location,
        first: true,
      });
    return;
  }
  const key = `${teamKey}\0${workspaceId}`;
  if (firstTabOpens.has(key)) return;
  const tab = createTerminalTab();
  const opened = get().addTerminalTab(teamKey, tab, workspaceId);
  firstTabOpens.set(key, { sessionId: tab.sessionId, opened });
  void opened.catch(() => {}).finally(() => firstTabOpens.delete(key));
}

/** SLW-CO-6: the shared "finish a foreground open" tail — switch to the
 *  workspace, reveal the terminal, and (unless suppressed) auto-spawn its first
 *  shell. The single idiom the unified `openWorkspace` core AND the story-open
 *  async tail both run once the record exists + is usable. `spawn:false` gates
 *  the epic pre-dir case (cwd "" → don't `chdir("")`; the shell launches on a
 *  later open once the dir resolves). */
function finishForegroundOpen(
  get: () => ViewerState,
  teamKey: string,
  workspaceId: string,
  opts?: { spawn?: boolean; focus?: boolean },
): void {
  applyOpenFocus(get, teamKey, workspaceId, opts?.focus); // SW-FX-69: additive by default
  get().expandTerminalOnOpen(teamKey); // SW-FX-35: pair-safe "show the terminal"
  if (opts?.spawn !== false) ensureFirstTerminalTab(get, teamKey, workspaceId);
}

/** SW-GW-8: pick the MAIN-session id to register under a workspace's BARE handle.
 *  Prefer the main agent tab — a role tag WITHOUT a `#<label>` suffix — else the
 *  bare shell (no role). A named SW-GW-6 session (role carries `#<label>`) is a
 *  DISTINCT agent already registered under its own `handle#label` by the
 *  dashboard, so it is NEVER folded under the bare handle here (that would steal
 *  its `by_session` reverse-index binding and break `sendToSession(handle#label)`).
 *  A workspace with ONLY named sessions registers nothing → `undefined`. Pure +
 *  exported so the selection is unit-tested without a store. */
export function pickNavRegisterSessionId(tabs: TerminalTab[]): string | undefined {
  const mainTab = tabs.find((t) => t.role && !t.role.includes("#")) ?? tabs.find((t) => !t.role);
  return mainTab?.sessionId;
}

/** SW-GW-8 (AC-1): fold a CodeSense-nav-opened MANAGED WORKSPACE session (a story
 *  worktree or a generic dir) into the trusted launch registry (CAP-FREE) so it
 *  appears in the Background agents panel and is addressable/adoptable/killable —
 *  closing the SW-GW-3 split where a nav-opened session was invisible to the
 *  registry (the panel read only the dashboard side). The handle matches what a
 *  dashboard uses for the SAME target so a nav-open and a dashboard-open COLLAPSE
 *  to one row (idempotent rebind): a story → its `storyId` (the dashboard
 *  main-session key, `dashboard-launch.ts`), a generic dir → its `cwd` (the
 *  dashboard `dirHandle`, which is the dir path itself). Registers only the MAIN
 *  session (see `pickNavRegisterSessionId`) — never an SW-GW-6 named session.
 *  Epic terminals register separately under their `epicDocPath` in the epic-open
 *  path; `root` is not a dashboard-addressable target. Best-effort — a failure
 *  never breaks the open. */
function registerNavWorkspaceSession(
  get: () => ViewerState,
  teamKey: string,
  workspaceId: string,
): void {
  const state = get();
  const ws = readTeamKeyed(state.workspaces, teamKey, []).find((w) => w.id === workspaceId);
  if (!ws) return;
  const handle = ws.kind === "story" ? ws.storyId : ws.kind === "generic" ? ws.cwd : undefined;
  if (handle) registerWorkspaceSession(get, teamKey, workspaceId, handle);
}

/** Register a user-opened workspace's MAIN session (see
 *  `pickNavRegisterSessionId`) under `handle` in the trusted launch registry,
 *  CAP-FREE with "codesense" provenance — or, when the open just started the
 *  workspace's first terminal, register that one once its host placed it.
 *  Best-effort: a registration failure never breaks the open. */
function registerWorkspaceSession(
  get: () => ViewerState,
  teamKey: string,
  workspaceId: string,
  handle: string,
): void {
  const register = (sessionId: string) =>
    void dashboardRegisterSession(handle, sessionId, CODESENSE_LAUNCHER, teamKey).catch(() => {});
  const sessionId = pickNavRegisterSessionId(selectWorkspaceTabs(get(), teamKey, workspaceId));
  if (sessionId) {
    register(sessionId);
    return;
  }
  const first = firstTabOpens.get(`${teamKey}\0${workspaceId}`);
  if (first)
    void first.opened.then(
      () => register(first.sessionId),
      () => {},
    );
}

/** SLW-CO-5: map a `setupWorktree` progress phase to the inline overlay's
 *  sub-label. Relocated from the retired worktree-setup toast's `PHASE_LABELS`
 *  (the toast keeps only its epic-flow copy). */
const WORKTREE_SETUP_PHASE_LABELS: Record<string, string> = {
  copy: "Copying files…",
  setup: "Running setup…",
  validate: "Validating…",
  cleanup: "Cleaning up…",
};
function worktreeSetupPhaseLabel(phase: string): string {
  return WORKTREE_SETUP_PHASE_LABELS[phase] ?? `${phase}…`;
}

export const MAXIMIZED_SPLIT_RATIO = 0.95;
export const IS_MAXIMIZED_THRESHOLD = 0.93;

const pendingPanelCloses = new Set<string>();
let paneLayoutRevision = 0;

export const useViewerStore = create<ViewerState>((set, get) => ({
  openTabs: {},
  activeTab: {},
  editingDirtyPaths: {},
  editFileRequest: null,
  tabDiff: {},
  viewStateByKey: {},
  drillPaths: {},
  teamPipelineRepoPath: {},
  terminalVisible: false,
  cliExpanded: false,
  navCollapsed: {},
  workspacePanelCollapsed: false,
  agentsPaneFolds: {},
  agentsPaneHiddenTeams: [],
  agentsPaneTeamFocusRestore: null,
  agentsPaneSearch: { query: "", status: "all" },
  agentsPaneArrange: "teams",
  agentsWorkspaceFilter: null,
  agentsPaneViewport: { offset: 0, measurements: [] },
  rememberAgentsPaneViewport: (offset, measurements) =>
    set({ agentsPaneViewport: { offset, measurements } }),
  agentsWorkspaceReveal: null,
  contextHistory: EMPTY_CONTEXT_HISTORY,
  agentIdentity: {},
  agentPins: {},
  workspacePanelWidth: null,
  // SW-WC-6: per-team Inbox list/content split ratio. Absent → default.
  foregroundLayout: {},
  foregroundLayoutByRail: {},
  epicReposOpen: {},
  layoutMode: null,
  lastTerminalMode: "shared",
  scratchNoteContent: "",
  notes: [],
  activeNoteId: null,
  activeNoteHeadings: [],
  pendingHeadingJump: null,
  rightPaneOpen: false,
  agentsListFilter: null,
  rightPaneRatio: 0.33,
  worktreeEstablishTick: 0,
  scratchNoteLoadStatus: "pending",
  emptyScratchReapDone: false,
  notesLoadedScope: null,
  rightPaneSplitRatio: {},
  rightPaneLayout: defaultRightPaneLayout(),
  noteSavedAt: null,
  terminalSplitRatio: DEFAULT_TERMINAL_SPLIT_RATIO,
  statusNavExpandedPaths: {},
  statusNavPinnedPaths: {},
  navLens: {},
  revealCounter: {},
  workspaces: {},
  activeWorkspace: {},
  workspaceOpenSet: {},
  terminalLayout: EMPTY_TERMINAL_LAYOUT,
  tabGridLayout: {},
  heldTerminalOpens: {},
  terminalLives: {},
  terminalHostsUnreachable: false,
  aiStatusBySession: {},
  progressBySession: {},
  progressTickBySession: {},
  storyHudShown: {},
  storyHudExpanded: {},
  sourceDiffShownBySession: {},
  sourceDiffSelectedFile: {},
  sourceDiffHistoryMode: {},
  sourceDiffSelectedSha: {},
  sourceDiffBaseMode: {},
  sourceDiffViewOptions: {},
  worktreeEditorOpenBySession: {},
  worktreeEditorFile: {},
  worktreeEditorRevealLine: {},
  worktreeEditorDraft: {},
  worktreeEditorTabs: {},
  sourceDiffMode: {},
  worktreeChangedPath: {},
  noClonePromptRepo: null,
  noClonePromptRetry: null,
  worktreeSetupApproval: null,
  stateLoaded: false,
  persistedStateRestored: false,
  pipelineScope: {},
  selectedAgentContext: {},
  scopeRecoveryComplete: false,
  openedTeams: [] as string[],
  navHistory: {},
  fileHashRequests: {},
  consumeFileHash: (teamKey, request) =>
    set((state) => {
      if (state.fileHashRequests?.[teamKey] !== request) return state;
      const next = { ...state.fileHashRequests };
      delete next[teamKey];
      return { fileHashRequests: next };
    }),
  navIndex: {},
  openedViewerPaths: {},
  onDiskWorktrees: {},
  softClosedWorkspaces: {},
  mruWorkspaceStack: {},
  trayOrderByTeam: {},
  trayFullOrderByTeam: {},
  workspaceGroupVersionsByTeam: {},
  customGroupsByTeam: {},
  applyWorkspaceGroups: ({ teamKey, groups, revision }) => {
    if (revision <= (get().workspaceGroupVersionsByTeam[teamKey] ?? -1)) return;
    set((s) => {
      const previous = s.customGroupsByTeam[teamKey] ?? [];
      const tree = useTeamSurfaceStore.getState().trees[teamKey] ?? null;
      const { railKeyIndex: index, baseOrder } = buildWorkspaceGrouping(
        s.workspaces[teamKey] ?? [],
        s.softClosedWorkspaces[teamKey] ?? {},
        tree,
        groups,
      );
      const layouts = { ...s.foregroundLayoutByRail[teamKey] };
      for (const group of previous)
        if (!groups.some((g) => g.id === group.id)) delete layouts[customRailKey(group.id)];
      const focused = s.activeWorkspace[teamKey];
      const selected = s.selectedEpicDirName[teamKey];
      const follows = focused && index[focused] !== s.railKeyByWorkspaceId[teamKey]?.[focused];
      const deletedSelected =
        selected &&
        customGroupId(selected) !== null &&
        !groups.some((g) => customRailKey(g.id) === selected);
      return {
        customGroupsByTeam: updateTeamKeyed(s.customGroupsByTeam, teamKey, groups),
        workspaceGroupVersionsByTeam: updateTeamKeyed(
          s.workspaceGroupVersionsByTeam,
          teamKey,
          revision,
        ),
        railKeyByWorkspaceId: updateTeamKeyed(s.railKeyByWorkspaceId, teamKey, index),
        trayBaseOrderByTeam: updateTeamKeyed(s.trayBaseOrderByTeam, teamKey, baseOrder),
        foregroundLayoutByRail: updateTeamKeyed(s.foregroundLayoutByRail, teamKey, layouts),
        ...(s.trayMode[teamKey] === "grouped" && (follows || deletedSelected)
          ? {
              selectedEpicDirName: updateTeamKeyed(
                s.selectedEpicDirName,
                teamKey,
                index[focused ?? ""] ?? "",
              ),
            }
          : {}),
      };
    });
    get().persistState();
  },
  editWorkspaceGroup: async (teamKey, edit) => {
    const s = get();
    const all = [
      ...(s.workspaces[teamKey] ?? []),
      ...Object.values(s.softClosedWorkspaces[teamKey] ?? {}).map((v) => v.workspace),
    ];
    const problem = validateGroupEdit(s.customGroupsByTeam[teamKey] ?? [], edit, all);
    if (problem) return { status: "validation", message: problem };
    try {
      const result = await workspaceGroupEdit(teamKey, edit);
      if (result.status === "ok") get().applyWorkspaceGroups(result);
      return result;
    } catch (error) {
      return { status: "validation", message: String(error) };
    }
  },
  createWorkspaceGroup: (teamKey, name, workspaceIds = []) =>
    get().editWorkspaceGroup(teamKey, { action: "create", name, workspaceIds }),
  renameWorkspaceGroup: (teamKey, groupId, name) =>
    get().editWorkspaceGroup(teamKey, { action: "rename", groupId, name }),
  setWorkspaceGroupMembers: (teamKey, groupId, workspaceIds, pruneEmpty = false) =>
    get().editWorkspaceGroup(teamKey, { action: "set_members", groupId, workspaceIds, pruneEmpty }),
  deleteWorkspaceGroup: (teamKey, groupId) =>
    get().editWorkspaceGroup(teamKey, { action: "delete", groupId }),
  workspaceOrderByTeam: {},
  trayBaseOrderByTeam: {},
  trayMode: {},
  selectedEpicDirName: {},
  railKeyByWorkspaceId: {},
  openFileLifecycleByPath: {},
  workspaceTraySeenCoachmark: false,
  cliShortcutsHintSeen: false,
  dismissedTerminalServers: [],
  openingWorkspaces: {},
  maximizedPanel: {},
  settings: { ...DEFAULT_SETTINGS },

  openTeam: (teamKey) => {
    let added = false;
    set((s) => {
      if (s.openedTeams.includes(teamKey)) return s;
      added = true;
      return { openedTeams: [...s.openedTeams, teamKey] };
    });
    if (added) get().persistState();
  },

  openFile: (teamKey, repoPath, filePath, opts) => {
    if (!opts?.keepLayout && teamKey === getActiveTeamKey()) beginContextNavigation();
    logger.debug("viewer", `openFile: ${filePath} in ${repoPath} (team ${teamKey})`);
    set((s) => ({
      ...fileHashPatch(s, teamKey, filePath, opts?.hash),
      ...commitTabOpen(s, teamKey, filePath, (tabs) => {
        const idx = tabs.findIndex((t) => t.path === filePath);
        if (idx >= 0) {
          // Tab exists — "commit" semantics: pin it if it was a preview.
          // Leaves the position untouched; callers hit this path via
          // navigateBack/Forward, QuickOpen, and tree-row dblclick.
          if (tabs[idx].preview) {
            const next = [...tabs];
            next[idx] = { ...next[idx], preview: false };
            return next;
          }
          return tabs;
        }
        return [...tabs, committedTab(filePath)];
      }),
      ...(opts?.keepLayout ? {} : revealMainPaneFromConsole(s, teamKey)),
    }));
    // Reveal the opened file in the status nav and reconcile pipeline scope.
    // Runs after the tab state has been committed so selection/auto-center
    // downstream sees the latest activeTab value.
    revealOpenedFile(repoPath, teamKey, filePath, set, get);
    get().persistState();
  },

  openStoryDocForTeam: (teamKey, storyFilePath, opts) => {
    const state = useOrgStore.getState();
    const content = resolveTeamContentContext(teamKey, state.orgs, state.discoveryComplete);
    if (content.kind === "ready") {
      get().openFile(teamKey, content.source.repoPath, storyFilePath, opts);
    }
  },

  setMaximized: (teamKey, panelId) =>
    set((s) => {
      if (panelId === null) {
        if (!(teamKey in s.maximizedPanel)) return s; // Object.is-stable no-op
        const { [teamKey]: _drop, ...rest } = s.maximizedPanel;
        return { maximizedPanel: rest };
      }
      if (s.maximizedPanel[teamKey] === panelId) return s;
      return { maximizedPanel: { ...s.maximizedPanel, [teamKey]: panelId } };
    }),

  toggleMaximized: (teamKey, panelId) =>
    // Delegate to setMaximized so the set/clear + no-op semantics live in one place.
    get().setMaximized(teamKey, get().maximizedPanel[teamKey] === panelId ? null : panelId),

  openFilePreview: (teamKey, repoPath, filePath, opts) => {
    if (!opts?.keepLayout && teamKey === getActiveTeamKey()) beginContextNavigation();
    logger.debug("viewer", `openFilePreview: ${filePath} in ${repoPath} (team ${teamKey})`);
    set((s) => ({
      ...fileHashPatch(s, teamKey, filePath, opts?.hash),
      ...commitTabOpen(s, teamKey, filePath, (tabs) => {
        // Already open (preview or pinned) — tabs untouched, caller still
        // flips activeTab via commitTabOpen. Preview flag preserved.
        if (tabs.some((t) => t.path === filePath)) return tabs;

        const newPreview: TabState = {
          path: filePath,
          name: tabNameFromPath(filePath),
          preview: true,
        };
        // Replace the existing preview tab in place to avoid flicker as the
        // user arrow-keys through a folder; append when none is open.
        return replacePreviewSlot(tabs, newPreview);
      }),
    }));
    revealOpenedFile(repoPath, teamKey, filePath, set, get);
    get().persistState();
  },

  pinTab: (teamKey, filePath) => {
    set((s) => {
      const tabs = readTeamKeyed(s.openTabs, teamKey, [] as TabState[]);
      const next = pinPreviewInTabs(tabs, filePath);
      if (next === tabs) return s;
      return { openTabs: updateTeamKeyed(s.openTabs, teamKey, next) };
    });
    get().persistState();
  },

  setEditingDirtyPaths: (teamKey, filePaths) => {
    if (sameStringOrder(readTeamKeyed(get().editingDirtyPaths, teamKey, []), filePaths)) return;
    set((s) => ({ editingDirtyPaths: updateTeamKeyed(s.editingDirtyPaths, teamKey, filePaths) }));
  },

  requestEditActiveFile: (teamKey) => {
    // SW-VE-8: bump the nonce so re-firing on the SAME file still triggers the
    // view's effect. The view applies the edit-entry gate (canEnter) — this is
    // just an intent signal, never a direct mutation of edit state. Transient,
    // not persisted.
    const prev = get().editFileRequest;
    set({ editFileRequest: { nonce: (prev?.nonce ?? 0) + 1, teamKey } });
  },

  // RF-perf-12: session-scoped view-position snapshot actions. `captureViewState`
  // writes on component unmount (a fresh snapshot each time, so identity changes
  // — correct for a real write); `readViewState` reads on remount. A composed
  // viewKey is a flat string key. NEVER persisted — see the `viewStateByKey`
  // field doc (AC-4/AC-8). GC is inline in closeTab/closeTeam (reference-stable).
  captureViewState: (viewKey, snap) => {
    set((s) => ({ viewStateByKey: updateTeamKeyed(s.viewStateByKey, viewKey, snap) }));
  },
  readViewState: (viewKey) => get().viewStateByKey[viewKey],

  closeTab: (teamKey, repoPath, filePath, opts) => {
    if (opts?.user && teamKey === getActiveTeamKey()) beginContextNavigation();
    logger.debug("viewer", `closeTab: ${filePath} in team ${teamKey}`);
    // SW-VE-3: never silently drop an unsaved deliberate edit. Closing the tab
    // of a dirty edit re-activates it (surfacing the editor) so the user must
    // Save or discard (Done · Esc) first — non-blocking, no data loss.
    if (readTeamKeyed(get().editingDirtyPaths, teamKey, []).includes(filePath)) {
      if (opts?.user) get().setActiveTab(teamKey, repoPath, filePath);
      toast.info("Finish or discard your edit first (Done · Esc)");
      return;
    }
    let activatedPath = "";
    set((s) => {
      const tabs = readTeamKeyed(s.openTabs, teamKey, [] as TabState[]);
      const newTabs = tabs.filter((t) => t.path !== filePath);
      const active = s.activeTab[teamKey];

      if (newTabs.length === 0) {
        // T.4 Round 4 (Edge Case Hunter #8 / Blind Hunter #19): closing the
        // last tab converges to the same shape as closeAllTabs — clear
        // navHistory/navIndex/openedViewerPaths too. Pre-patch, the closed
        // file's entries lingered in navHistory[teamKey], so navigateBack
        // would re-open the just-closed tab; the keep-alive LRU kept a
        // hidden MarkdownViewer mounted for the closed file.
        return {
          openTabs: updateTeamKeyed(s.openTabs, teamKey, newTabs),
          activeTab: removeTeamKeyed(s.activeTab, teamKey),
          navHistory: removeTeamKeyed(s.navHistory, teamKey),
          navIndex: removeTeamKeyed(s.navIndex, teamKey),
          openedViewerPaths: removeTeamKeyed(s.openedViewerPaths, teamKey),
          // RF-perf-12: the closed file will never remount — drop its snapshot.
          viewStateByKey: removeTeamKeyed(s.viewStateByKey, fileViewKey(teamKey, filePath)),
          tabDiff: removeTeamKeyed(s.tabDiff, teamKey),
        };
      }

      let newActive = active;
      if (active === filePath) {
        const idx = tabs.findIndex((t) => t.path === filePath);
        newActive = neighborPathAfterClose(newTabs, idx);
        activatedPath = newActive;
      }

      // Reconcile navIndex: if the closed file was at the current history position,
      // update navIndex to point at the new active tab's position in history
      const history = readTeamKeyed(s.navHistory, teamKey, [] as string[]);
      const navIdx = readTeamKeyed(s.navIndex, teamKey, -1);
      let newNavIndex = navIdx;
      if (navIdx >= 0 && navIdx < history.length && history[navIdx] === filePath) {
        const activeInHistory = history.indexOf(newActive);
        if (activeInHistory >= 0) {
          newNavIndex = activeInHistory;
        }
      }

      return {
        openTabs: updateTeamKeyed(s.openTabs, teamKey, newTabs),
        activeTab: updateTeamKeyed(s.activeTab, teamKey, newActive),
        navIndex: updateTeamKeyed(s.navIndex, teamKey, newNavIndex),
        // RF-perf-12: the closed file will never remount — drop its snapshot.
        viewStateByKey: removeTeamKeyed(s.viewStateByKey, fileViewKey(teamKey, filePath)),
        tabDiff: withoutTabDiff(s.tabDiff, teamKey, filePath),
      };
    });
    if (activatedPath) revealOpenedFile(repoPath, teamKey, activatedPath, set, get);
    get().persistState();
  },

  remapTabPath: (teamKey, oldPath, newPath) => {
    if (oldPath === newPath) return;
    let changed = false;
    set((s) => {
      const patch: Partial<ViewerState> = {};
      // Each surface is repointed independently: a renamed/moved file may be an
      // open tab, only in nav history, and/or only in the keep-alive MRU — clean
      // whichever hold the stale path (mirrors closeTab's openedViewerPaths
      // hygiene) so nothing resurrects or lingers.
      const tabs = readTeamKeyed(s.openTabs, teamKey, []);
      if (tabs.some((t) => t.path === oldPath)) {
        patch.openTabs = updateTeamKeyed(
          s.openTabs,
          teamKey,
          tabs.map((t) =>
            t.path === oldPath ? { ...t, path: newPath, name: tabNameFromPath(newPath) } : t,
          ),
        );
        if (s.activeTab[teamKey] === oldPath) {
          patch.activeTab = updateTeamKeyed(s.activeTab, teamKey, newPath);
        }
      }
      const history = s.navHistory[teamKey];
      if (history?.some((p) => p === oldPath)) {
        patch.navHistory = updateTeamKeyed(
          s.navHistory,
          teamKey,
          history.map((p) => (p === oldPath ? newPath : p)),
        );
      }
      const opened = s.openedViewerPaths[teamKey];
      if (opened?.some((p) => p === oldPath)) {
        patch.openedViewerPaths = updateTeamKeyed(
          s.openedViewerPaths,
          teamKey,
          opened.map((p) => (p === oldPath ? newPath : p)),
        );
      }
      if (Object.keys(patch).length === 0) return s; // nothing referenced it
      changed = true;
      return patch;
    });
    if (changed) get().persistState();
  },

  reorderTab: (teamKey, fromIdx, toIdx) => {
    set((s) => {
      const tabs = [...readTeamKeyed(s.openTabs, teamKey, [])];
      const [moved] = tabs.splice(fromIdx, 1);
      if (!moved) return s;
      // Drag is a commit action — a move (fromIdx !== toIdx) pins a
      // preview tab. A no-op drag back to the same slot leaves the
      // preview flag alone.
      const pinned = fromIdx !== toIdx && moved.preview ? { ...moved, preview: false } : moved;
      tabs.splice(toIdx, 0, pinned);
      return { openTabs: updateTeamKeyed(s.openTabs, teamKey, tabs) };
    });
    get().persistState();
  },

  setActiveTab: (teamKey, repoPath, filePath) => {
    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    set((s) => {
      // SLW-FX-12: ensure the target is an OPEN tab before activating it, so
      // `activeTab[teamKey]` can never orphan (a doc with no tab). Most callers
      // (`viewer-tabs` click, `cycleTab`) pass an already-open path and hit the
      // fast activate-only path; the one caller that can pass a not-yet-open
      // file is `useEnterTeam` (enter-team.ts — resume onto the exact file
      // after `openTeam`, which doesn't add tabs). Append a pinned tab in that
      // case (idempotent — the guard above skips when it's already open),
      // reusing `committedTab` so shape/naming matches the other re-add sites.
      const tabs = readTeamKeyed(s.openTabs, teamKey, []);
      if (filePath && !tabs.some((t) => t.path === filePath)) {
        return {
          openTabs: {
            ...s.openTabs,
            [teamKey]: [...tabs, committedTab(filePath)],
          },
          activeTab: updateTeamKeyed(s.activeTab, teamKey, filePath),
        };
      }
      return { activeTab: updateTeamKeyed(s.activeTab, teamKey, filePath) };
    });
    // Re-expand ancestors so a tab whose folder was collapsed comes
    // back into view. Same call openFile makes — keeps every path
    // that activates a tab (click, Cmd+[ cycle, history nav) consistent.
    // `repoPath` may be null when the caller can't resolve a repo for
    // this team (e.g. `cycleTab` when the active workspace has no
    // resolvable `sourceRepoPath`); skip the reveal in that case — tab
    // state still updates correctly.
    if (filePath && repoPath) revealOpenedFile(repoPath, teamKey, filePath, set, get);
    get().persistState();
  },

  closeOtherTabs: (teamKey, repoPath, filePath) => {
    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    // SW-VE-3: never silently drop an unsaved deliberate edit. If any OTHER tab
    // holds a dirty edit, refuse the bulk close (non-blocking toast) — the user
    // must Save/discard (Done · Esc) first. (Closing a dirty edit that IS the
    // kept tab is fine.)
    if (readTeamKeyed(get().editingDirtyPaths, teamKey, []).some((p) => p !== filePath)) {
      toast.info("Finish or discard your edits first (Done · Esc)");
      return;
    }
    // T.4 Round 5 (Blind Hunter HIGH H3 / Edge Case Hunter MED M2+M3):
    // sample first so the post-set reveal call can gate on the same `tab`
    // the set callback saw. Avoids both (a) the !tab branch leaving stale
    // navHistory/navIndex/openedViewerPaths that navigateBack could resurrect
    // (matches the Phase 12 closeTab last-tab + closeAllTabs 4-slice clear),
    // and (b) firing revealOpenedFile for a file that no longer has any tab.
    const hadTab = readTeamKeyed(get().openTabs, teamKey, []).some((t) => t.path === filePath);
    set((s) => {
      const tab = readTeamKeyed(s.openTabs, teamKey, []).find((t) => t.path === filePath);
      if (!tab) {
        // No matching tab → same end-state as closeAllTabs (zero tabs,
        // no active tab). Mirror the 4-slice delete so navigateBack can't
        // re-open a tab that no longer exists, and the keep-alive LRU drops
        // hidden MarkdownViewers for vanished files.
        return {
          openTabs: updateTeamKeyed(s.openTabs, teamKey, []),
          activeTab: removeTeamKeyed(s.activeTab, teamKey),
          navHistory: removeTeamKeyed(s.navHistory, teamKey),
          navIndex: removeTeamKeyed(s.navIndex, teamKey),
          openedViewerPaths: removeTeamKeyed(s.openedViewerPaths, teamKey),
        };
      }
      return {
        openTabs: updateTeamKeyed(s.openTabs, teamKey, [tab]),
        activeTab: updateTeamKeyed(s.activeTab, teamKey, filePath),
      };
    });
    if (hadTab && filePath) revealOpenedFile(repoPath, teamKey, filePath, set, get);
    get().persistState();
  },

  closeAllTabs: (teamKey) => {
    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    // SW-VE-3: never silently drop an unsaved deliberate edit — refuse the bulk
    // close while any tab holds a dirty edit (non-blocking toast).
    if (readTeamKeyed(get().editingDirtyPaths, teamKey, []).length > 0) {
      toast.info("Finish or discard your edits first (Done · Esc)");
      return;
    }
    set((s) => ({
      openTabs: updateTeamKeyed(s.openTabs, teamKey, []),
      activeTab: removeTeamKeyed(s.activeTab, teamKey),
      navHistory: removeTeamKeyed(s.navHistory, teamKey),
      navIndex: removeTeamKeyed(s.navIndex, teamKey),
      // "Clear everything" intent extends to keep-alive: otherwise up to
      // `keepAliveMaxViewers` hidden MarkdownViewers remain mounted for
      // files that are no longer in any tab.
      openedViewerPaths: removeTeamKeyed(s.openedViewerPaths, teamKey),
    }));
    get().persistState();
  },

  setDrillPath: (teamKey, drillPath) => {
    set((s) => ({
      drillPaths: updateTeamKeyed(s.drillPaths, teamKey, drillPath),
    }));
    get().persistState();
  },

  setTeamPipelineRepoPath: (teamKey, repoKey) => {
    // SLW-FX-14: write-through cache. Skip the set+persist when unchanged so
    // the resolver's per-render effect doesn't thrash the debounced persist.
    if (get().teamPipelineRepoPath[teamKey] === repoKey) return;
    set((s) => ({
      teamPipelineRepoPath: updateTeamKeyed(s.teamPipelineRepoPath, teamKey, repoKey),
    }));
    get().persistState();
  },

  syncDrillPath: (teamKey, targetAbsolutePath, contextRepoPath, teamContentPath) => {
    set((s) => ({
      drillPaths: updateTeamKeyed(s.drillPaths, teamKey, targetAbsolutePath),
    }));
    // Pipeline scope reconciliation runs through the same helper the
    // nav drill chevron uses, so the two surfaces stay in lock-
    // step. `null` means "pin scope" — used when the target is outside
    // the context repo entirely (different repo, symlink out).
    // PTS-2: helper anchors on `teamContentPath` (was the viewerRoot
    // composition). SW-FX-56: the retired scan-scope prefix is no longer
    // even passed in.
    const next = computePipelineScopeUpdate(targetAbsolutePath, contextRepoPath, teamContentPath);
    if (next !== null) {
      const normalized = normalizeScopePath(next);
      set((s) => ({
        pipelineScope: updateTeamKeyed(s.pipelineScope, teamKey, normalized),
      }));
    }
    get().persistState();
  },

  setPipelineScope: (teamKey, scopePath) => {
    const normalized = normalizeScopePath(scopePath);
    set((s) => ({
      pipelineScope: updateTeamKeyed(s.pipelineScope, teamKey, normalized),
    }));
    get().persistState();
  },

  setSelectedAgentContext: (teamKey, selection) => {
    set((s) => {
      const current = s.selectedAgentContext[teamKey];
      if (!selection) {
        if (!current) return s;
        const { [teamKey]: _dropped, ...rest } = s.selectedAgentContext;
        return { selectedAgentContext: rest };
      }
      if (current?.agentKey === selection.agentKey && current?.repoSlug === selection.repoSlug) {
        return s;
      }
      return {
        selectedAgentContext: { ...s.selectedAgentContext, [teamKey]: { ...selection } },
      };
    });
    get().persistState();
  },

  setScopeRecoveryComplete: (done) => set({ scopeRecoveryComplete: done }),

  touchViewerPath: (teamKey, filePath) => {
    if (!filePath) return;
    set((s) => {
      const prev = readTeamKeyed(s.openedViewerPaths, teamKey, []);
      // If this path is already at the front, nothing changes — skip the
      // partial update so we don't fan out subscribers for a no-op.
      if (prev[0] === filePath) return s;
      const deduped = prev.filter((p) => p !== filePath);
      const next = [filePath, ...deduped].slice(0, VIEWER_PERF.keepAliveMaxViewers);
      return { openedViewerPaths: updateTeamKeyed(s.openedViewerPaths, teamKey, next) };
    });
  },

  setTerminalVisible: (visible) => {
    beginContextNavigation();
    set(cliChromeState(applyCliChromePatch(cliChromeOf(get()), { visible })));
    get().persistState();
  },

  setNavCollapsed: (teamKey, collapsed) => {
    set((s) => ({
      navCollapsed: updateTeamKeyed(s.navCollapsed, teamKey, collapsed),
    }));
    get().persistState();
  },

  openChangeTab: (teamKey, repoPath, filePath, pin) => {
    if (pin) get().openFile(teamKey, repoPath, filePath);
    else get().openFilePreview(teamKey, repoPath, filePath);
    get().setTabDiff(teamKey, filePath, true);
  },

  openCommitChangeTab: (teamKey, repoPath, filePath, commit, pin) => {
    if (pin) get().openFile(teamKey, repoPath, filePath);
    else get().openFilePreview(teamKey, repoPath, filePath);
    get().setTabDiff(teamKey, filePath, commit);
  },

  setTabDiff: (teamKey, filePath, on) => {
    set((s) => {
      const cur = s.tabDiff[teamKey] ?? {};
      const want: TabDiff | null = on === false ? null : on;
      if ((cur[filePath] ?? null) === want) return s;
      const next = { ...cur };
      if (want) next[filePath] = want;
      else delete next[filePath];
      // A tab on one commit's change says which commit; back on the file it is the file.
      const tabs = s.openTabs[teamKey];
      const name =
        want && want !== true && !want.showFile
          ? `${tabNameFromPath(filePath)} @ ${want.shortSha}`
          : tabNameFromPath(filePath);
      const renamed = tabs?.some((t) => t.path === filePath && t.name !== name)
        ? updateTeamKeyed(
            s.openTabs,
            teamKey,
            tabs.map((t) => (t.path === filePath ? { ...t, name } : t)),
          )
        : s.openTabs;
      return { tabDiff: updateTeamKeyed(s.tabDiff, teamKey, next), openTabs: renamed };
    });
  },

  setWorkspacePanelCollapsed: (collapsed) => {
    set({
      workspacePanelCollapsed: collapsed,
      ...(collapsed ? { agentsWorkspaceReveal: null } : {}),
    });
    get().persistState();
  },

  setAgentsPaneSearch: (patch) =>
    set((s) => ({ agentsPaneSearch: { ...s.agentsPaneSearch, ...patch } })),
  setAgentsPaneArrange: (agentsPaneArrange) => {
    set({ agentsPaneArrange });
  },
  setAgentsPaneTeamHidden: (teamKey, hidden) => {
    set((s) => {
      const without = s.agentsPaneHiddenTeams.filter((t) => t !== teamKey);
      if (!hidden) return { agentsPaneHiddenTeams: without };
      return { agentsPaneHiddenTeams: [...without, teamKey] };
    });
    get().persistState();
  },

  setAgentsPaneTeamScope: (hidden, focusRestore) => {
    set({
      agentsPaneHiddenTeams: cleanHiddenTeams(hidden),
      agentsPaneTeamFocusRestore: focusRestore && {
        ...focusRestore,
        hidden: cleanHiddenTeams(focusRestore.hidden),
      },
    });
    get().persistState();
  },

  setAgentsWorkspaceFilter: (filter) => {
    set((s) => ({ agentsWorkspaceFilter: filter && stampFilterMachine(s, filter) }));
    get().persistState();
  },

  revealAgentsWorkspace: (teamKey, workspaceId) => {
    const state = get();
    if (state.workspacePanelCollapsed || !state.terminalVisible) return;
    set({
      agentsWorkspaceReveal: {
        teamKey,
        workspaceId,
        nonce: (state.agentsWorkspaceReveal?.nonce ?? 0) + 1,
      },
    });
  },
  clearAgentsWorkspaceReveal: () => set({ agentsWorkspaceReveal: null }),

  setContextHistory: (history) => {
    set({ contextHistory: history });
  },

  // Folds are the dock's shared view state: `ui:dock` owns and saves them (agent-list-inputs).
  setAgentsPaneFold: (key, folded) => {
    set((s) => {
      const { [key]: _was, ...rest } = s.agentsPaneFolds;
      return { agentsPaneFolds: folded ? { ...rest, [key]: true } : rest };
    });
  },

  setAgentIdentity: (teamKey, handle, patch) => {
    set((s) => {
      const { [handle]: current, ...rest } = s.agentIdentity[teamKey] ?? {};
      const chosen = applyIdentityPatch(current, patch);
      const next = chosen ? { ...rest, [handle]: chosen } : rest;
      return { agentIdentity: updateTeamKeyed(s.agentIdentity, teamKey, next) };
    });
  },

  pruneAgentIdentity: (teamKey, keep) => {
    const current = get().agentIdentity[teamKey];
    if (!current) return;
    const kept = Object.fromEntries(Object.entries(current).filter(([handle]) => keep.has(handle)));
    if (Object.keys(kept).length === Object.keys(current).length) return;
    set((s) => ({ agentIdentity: updateTeamKeyed(s.agentIdentity, teamKey, kept) }));
  },

  pinAgent: (teamKey, pin) => {
    set((s) => {
      const current = s.agentPins[teamKey] ?? [];
      const id = agentPinId(pin);
      const at = current.findIndex((p) => agentPinId(p) === id);
      if (at >= 0 && JSON.stringify(current[at]) === JSON.stringify(pin)) return {};
      const next = at >= 0 ? current.map((p, i) => (i === at ? pin : p)) : [...current, pin];
      return { agentPins: updateTeamKeyed(s.agentPins, teamKey, next) };
    });
  },

  unpinAgents: (teamKey, pinIds) => {
    const current = get().agentPins[teamKey];
    if (!current) return;
    const drop = new Set(pinIds);
    const kept = current.filter((p) => !drop.has(agentPinId(p)));
    if (kept.length === current.length) return;
    set((s) => ({ agentPins: updateTeamKeyed(s.agentPins, teamKey, kept) }));
  },

  setWorkspacePanelWidth: (width) => {
    set({ workspacePanelWidth: width });
    get().persistState();
  },

  setForegroundPanelSizes: (teamKey, sizes) => {
    get().setForegroundNodeSizes(teamKey, [], sizes);
  },

  setForegroundNodeSizes: (teamKey, path, sizes) => {
    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    set((s) => {
      // SW-EW-40: read/write the active foreground slot (flat per-team, or the
      // selected rail's slot in grouped mode). Flat output is byte-identical.
      const slot = activeForegroundSlot(s, teamKey);
      const base = defaultForegroundLayout(slot.layout);
      // Depth-1 flat fast path: columns/rows persist their root-level sizes
      // verbatim on `sizes` (seedTreeFromFlat honors them) — identical to the
      // pre-EW-17 behavior. `grid` is excluded because buildBalancedGrid ignores
      // `sizes`, so a grid resize must materialize a concrete tree instead.
      if (base.root == null && path.length === 0 && base.preset !== "grid") {
        return slot.write({ ...base, sizes });
      }
      // Custom mosaic, or a resize of a DERIVED arrangement (grid / any nested
      // pane): materialize the current effective tree so the commit sticks, then
      // write the node sizes. A grid that's been resized becomes a concrete root
      // (still flagged `grid`; re-clicking Grid re-balances via root: null).
      const root = base.root ?? effectiveMosaicRoot(base, slot.tileIds);
      if (!root) return {};
      const sized = setNodeSizes(root, path, sizes);
      // A no-op commit (bad path / length mismatch) must not freeze a still-derived
      // grid into a concrete root — only materialize when the sizes actually changed.
      if (base.root == null && sized === root) return {};
      return slot.write({ ...base, root: sized });
    });
    get().persistState();
  },

  placeWorkspaceTile: (teamKey, workspaceId, target, opts) => {
    if (opts?.user && teamKey === getActiveTeamKey()) beginContextNavigation();
    if ("center" in target) {
      // Center drop = focus only, no structural change (resolved open question).
      get().switchWorkspace(teamKey, workspaceId);
      return;
    }
    set((s) => {
      // SW-EW-40: operate on the active foreground slot (flat, or the selected
      // rail's slot in grouped mode) over its mode-aware tile set.
      const slot = activeForegroundSlot(s, teamKey);
      const base = defaultForegroundLayout(slot.layout);
      // Start from the CURRENT effective arrangement (derives the grid/flat tree
      // when no custom root is stored yet) so a split off a preset preserves
      // what's on screen; reconcile has already appended a just-restored ws.
      const current = effectiveMosaicRoot(base, slot.tileIds);
      if (!current) return {};
      const onScreen = leafWorkspaceIds(current).includes(workspaceId);
      const nextRoot = onScreen
        ? moveLeaf(current, workspaceId, target)
        : splitAt(current, target.targetWorkspaceId, target.axis, target.side, workspaceId);
      // No structural change (e.g. a tile dropped on its OWN edge) → don't flip
      // the preset to `custom` or leave expanded for a no-op (the pure tree ops
      // return the same reference unchanged).
      if (nextRoot === current) return {};
      // A directional split makes the arrangement a user-owned custom mosaic, shown as the
      // grid: it leaves expanded (a layout change, like a preset change).
      return {
        ...slot.write({ ...base, root: nextRoot, preset: "custom" }),
        ...(teamKey === getActiveTeamKey() ? { cliExpanded: false } : {}),
      };
    });
    get().persistState();
  },

  replaceFocusedLeafWorkspace: (teamKey, incomingWorkspaceId, opts) => {
    const state = get();
    // Only a minimized (soft-closed) workspace replaces — guard on its shadow
    // (Seam B / D2). On-screen chips focus, they never replace.
    const shadow = state.softClosedWorkspaces[teamKey]?.[incomingWorkspaceId];
    if (!shadow) return;

    const focusedId = state.activeWorkspace[teamKey];
    // SW-EW-40: the active foreground slot (flat, or the selected rail's slot in
    // grouped mode) — its layout, its mode-aware tiles, and its write-back.
    const slot = activeForegroundSlot(state, teamKey);
    const base = defaultForegroundLayout(slot.layout);
    // Materialize the tree FIRST (root is null on the default preset — derives
    // from preset/direction) and BEFORE any membership swap, so `F` is still in
    // it. This is the placeWorkspaceTile idiom.
    const current = effectiveMosaicRoot(base, slot.tileIds);
    const focusedWs = state.workspaces[teamKey]?.find((w) => w.id === focusedId);
    const canReplace =
      !!focusedId &&
      focusedWs !== undefined &&
      getKindCapabilities(focusedWs.kind).canSoftClose &&
      !!current &&
      leafWorkspaceIds(current).includes(focusedId);

    // No replaceable focused leaf (empty mosaic, or the focused tile is a root):
    // surface the incoming via the existing center-reopen path rather than
    // displacing nothing or breaking the root invariant (Seam C "no focused leaf").
    if (!canReplace) {
      get().reopenSoftClosedWorkspace(teamKey, incomingWorkspaceId, { center: true }, opts);
      return;
    }

    const F = focusedId as string;
    const displacedWs = focusedWs!;
    // The displaced tile takes its Hidden tabs into the tray with it (as
    // `closeWorkspace` does), so they are never stranded under its chip.
    foldHiddenTabsIntoTile(state, teamKey, F);
    const nextRoot = replaceLeafWorkspaceId(current!, F, incomingWorkspaceId);

    set((s) => {
      // ---- Displace F: clear its per-workspace slices + stash its
      // closed-but-running shadow so F stays a tray chip. Unlike closeWorkspace
      // (where 0 terminals → hard close), a REPLACE is a swap, not a close — the
      // user didn't ask to discard F, so always retain it (keepRunning=true) even
      // with no live terminals, or it would silently vanish from the tray. ----
      const displaced = captureSoftCloseShadow(s, teamKey, F, displacedWs, true);

      // ---- Surface incoming: drop it from the shadow map (its tabs stay in
      // their host's layout, mirroring reopenSoftClosedWorkspace). ----
      const teamSoft = { ...readTeamKeyed(displaced.softClosedWorkspaces, teamKey, {}) };
      delete teamSoft[incomingWorkspaceId];

      // Membership swap: F out, incoming in.
      const ws = readTeamKeyed(s.workspaces, teamKey, []);
      const withoutF = ws.filter((w) => w.id !== F);
      const nextWs = withoutF.some((w) => w.id === incomingWorkspaceId)
        ? withoutF
        : [...withoutF, shadow.workspace];

      return {
        ...displaced,
        workspaces: updateTeamKeyed(s.workspaces, teamKey, nextWs),
        activeWorkspace: updateTeamKeyed(s.activeWorkspace, teamKey, incomingWorkspaceId),
        softClosedWorkspaces: updateTeamKeyed(displaced.softClosedWorkspaces, teamKey, teamSoft),
        // Pin the swapped leaf in place → reconcile is a no-op, pane count + slot
        // preserved (preset→custom). Expanded stands: it now zooms the incoming tile, the
        // focused one. SW-EW-40: write the active slot (flat, or the selected rail in grouped).
        ...slot.write({ ...base, root: nextRoot, preset: "custom" }),
      };
    });

    // Watcher lifecycle: release F's surface watch, (re)open incoming's — mirrors
    // the close + reopen paths. F's terminals keep running.
    get().closeWorkspaceWatch(teamKey, F);
    get().openActiveSurfaceWatch(teamKey, incomingWorkspaceId);
    if (!opts?.keepLayout) get().expandTerminalOnOpen(teamKey); // SW-FX-35: pair-safe "show the terminal"
    // Record recency (replace sets activeWorkspace directly, bypassing
    // switchWorkspace's MRU push): push the displaced F first, then the incoming
    // on top — so `Cmd+Alt+;` flips back to the workspace just swapped away.
    set((s) => ({
      mruWorkspaceStack: pushMruStack(
        pushMruStack(s.mruWorkspaceStack, teamKey, F),
        teamKey,
        incomingWorkspaceId,
      ),
    }));
    get().flushPersist();
  },

  activateTrayWorkspace: (teamKey, workspaceId, opts) => {
    const s = get();
    // Shared tray state-rule (chip click, Cmd+1..9, flip-to-last all route here).
    if (s.softClosedWorkspaces[teamKey]?.[workspaceId]) {
      get().replaceFocusedLeafWorkspace(teamKey, workspaceId, opts); // minimized → replace
      return true;
    }
    if (
      s.workspaces[teamKey]?.some((w) => w.id === workspaceId) &&
      s.activeWorkspace[teamKey] !== workspaceId
    ) {
      get().switchWorkspace(teamKey, workspaceId); // on-screen, not focused → focus
      return true;
    }
    return false; // already-focused (or unknown id) → no-op
  },

  flipLastWorkspace: (teamKey) => {
    const s = get();
    const current = s.activeWorkspace[teamKey];
    const exists = (id: string): boolean =>
      (s.workspaces[teamKey]?.some((w) => w.id === id) ?? false) ||
      s.softClosedWorkspaces[teamKey]?.[id] !== undefined;
    // "last" = the most-recent entry that isn't the current workspace and still
    // exists (prune hard-closed ids lazily).
    const last = readTeamKeyed(s.mruWorkspaceStack, teamKey, []).find(
      (id) => id !== current && exists(id),
    );
    return last ? get().activateTrayWorkspace(teamKey, last) : false;
  },

  setTrayOrder: (teamKey, workspaceIds) => {
    // unchanged — avoid a render loop when called from an effect
    if (sameStringOrder(get().trayOrderByTeam[teamKey], workspaceIds)) return;
    set((s) => ({ trayOrderByTeam: updateTeamKeyed(s.trayOrderByTeam, teamKey, workspaceIds) }));
  },

  setTrayFullOrder: (teamKey, workspaceIds) => {
    if (sameStringOrder(get().trayFullOrderByTeam[teamKey], workspaceIds)) return;
    set((s) => ({
      trayFullOrderByTeam: updateTeamKeyed(s.trayFullOrderByTeam, teamKey, workspaceIds),
    }));
  },

  jumpToTrayIndex: (teamKey, index) => {
    const id = get().trayOrderByTeam[teamKey]?.[index];
    return id ? get().activateTrayWorkspace(teamKey, id) : false;
  },

  markWorkspaceTrayCoachmarkSeen: () => {
    if (get().workspaceTraySeenCoachmark) return;
    set({ workspaceTraySeenCoachmark: true });
    get().persistState();
  },

  dismissTerminalServers: (entries: string[]) => {
    const prior = get().dismissedTerminalServers;
    const added = entries.filter((entry) => !prior.includes(entry));
    if (added.length === 0) return;
    // Newest last, bounded: an entry for a server long since updated ages out;
    // the set being dismissed now always survives the trim.
    const kept = prior.filter((entry) => !entries.includes(entry));
    set({ dismissedTerminalServers: [...kept, ...entries].slice(-Math.max(64, entries.length)) });
    get().persistState();
  },

  markCliShortcutsHintSeen: () => {
    if (get().cliShortcutsHintSeen) return;
    set({ cliShortcutsHintSeen: true });
    get().persistState();
  },

  setForegroundDirection: (teamKey, direction) => {
    // EW-3 axis seam, now a thin alias over the EW-4 preset setter so the
    // direction↔preset sync + zoom-clear logic lives in exactly one place.
    get().setForegroundPreset(teamKey, direction === "column" ? "rows" : "columns");
  },

  cycleForegroundPreset: (teamKey) => {
    // SW-CW-7: same pattern as setForegroundDirection — read the active slot's
    // current preset, step once around PRESET_RING, and delegate. `custom` / absent
    // → indexOf -1 → next lands on columns. All slot-targeting, direction sync,
    // custom-root reset, zoom clear and persistence are inherited from the setter.
    const current = activeForegroundLayout(get(), teamKey)?.preset;
    get().setForegroundPreset(teamKey, nextPresetInRing(current));
  },

  // SW-EW-46 (D2/D8): the tab-grid dial. Each mutator sends the workspace's
  // next grid to its host; cells stay in tab-strip order.
  paneOutTerminalTab: (teamKey, workspaceId, tabId) => {
    const s = get();
    const tabs = selectWorkspaceTabs(s, teamKey, workspaceId);
    if (!tabs.some((t) => t.id === tabId)) return;
    const paned = workspacePanedTabIds(s, teamKey, workspaceId);
    if (!paned.includes(tabId)) {
      // Paning a tab out shows the grid — clear any remembered collapse.
      setWorkspaceGrid(s, teamKey, workspaceId, {
        paned: tabs.filter((t) => paned.includes(t.id) || t.id === tabId).map((t) => t.id),
        collapsed: false,
      });
    }
    // The paned cell becomes the focused pane (they unify in grid mode, D6).
    get().setActiveTerminalTab(teamKey, tabId, workspaceId);
  },

  demoteTerminalPane: (teamKey, workspaceId, tabId) => {
    const s = get();
    const paned = workspacePanedTabIds(s, teamKey, workspaceId);
    if (!paned.includes(tabId)) return;
    setWorkspaceGrid(s, teamKey, workspaceId, { paned: paned.filter((id) => id !== tabId) });
    // A demoted focused cell hands focus to a surviving cell so the grid never
    // loses its keyboard owner.
    const survivor = paned.find((id) => id !== tabId);
    if (survivor && selectActiveTerminalTab(s, teamKey, workspaceId) === tabId) {
      get().setActiveTerminalTab(teamKey, survivor, workspaceId);
    }
  },

  gridAllTerminalTabs: (teamKey, workspaceId) => {
    const s = get();
    const tabs = selectWorkspaceTabs(s, teamKey, workspaceId);
    if (tabs.length === 0) return;
    // Showing the grid clears any remembered collapse.
    setWorkspaceGrid(s, teamKey, workspaceId, { paned: tabs.map((t) => t.id), collapsed: false });
  },

  collapseTabGridToTabs: (teamKey, workspaceId) => {
    // SW-EW-46: KEEP the paned set and just flag the grid collapsed → it renders
    // one tab at a time. Toggling back shows the same cells again: their
    // sessions keep running, and each pane re-attaches its display.
    setWorkspaceGrid(get(), teamKey, workspaceId, { collapsed: true });
  },

  showTerminalCells: (teamKey, workspaceId, paned) => {
    const s = get();
    const wanted = new Set(paned);
    const cells = selectWorkspaceTabs(s, teamKey, workspaceId)
      .map((t) => t.id)
      .filter((id) => wanted.has(id));
    if (cells.length === 0) return false;
    setWorkspaceGrid(s, teamKey, workspaceId, { paned: cells, collapsed: false });
    return true;
  },

  splitTerminalBeside: (teamKey, workspaceId, anchorTabId, newTabId) => {
    const s = get();
    const tabs = selectWorkspaceTabs(s, teamKey, workspaceId);
    const anchorIdx = tabs.findIndex((t) => t.id === anchorTabId);
    const newIdx = tabs.findIndex((t) => t.id === newTabId);
    if (anchorIdx === -1 || newIdx === -1 || anchorTabId === newTabId) return false;
    // Beside = straight after the anchor in the strip; cells follow strip order.
    const order = tabs.map((t) => t.id).filter((id) => id !== newTabId);
    order.splice(order.indexOf(anchorTabId) + 1, 0, newTabId);
    const target = order.indexOf(newTabId);
    if (target !== newIdx) get().reorderTerminalTab(teamKey, newIdx, target, workspaceId);
    // Already side by side: the new cell joins the shown ones. On tabs: just the two.
    const shown = shouldRenderTabGrid(s, teamKey, workspaceId)
      ? workspacePanedTabIds(s, teamKey, workspaceId)
      : [];
    const paned = new Set([...shown, anchorTabId, newTabId]);
    setWorkspaceGrid(s, teamKey, workspaceId, {
      paned: order.filter((id) => paned.has(id)),
      collapsed: false,
    });
    get().setActiveTerminalTab(teamKey, newTabId, workspaceId);
    return true;
  },

  cycleTabGridPreset: (teamKey, workspaceId) => {
    const s = get();
    get().setTabGridPreset(
      teamKey,
      workspaceId,
      nextPresetInRing(workspaceTabGridPreset(s, teamKey, workspaceId)),
    );
  },

  setTabGridPreset: (teamKey, workspaceId, preset) => {
    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    const s = get();
    setWorkspaceGrid(s, teamKey, workspaceId, { preset });
    const key = selectWorkspaceKey(s, teamKey, workspaceId);
    if (!(key in s.tabGridLayout)) return;
    const { [key]: _evened, ...rest } = s.tabGridLayout;
    set({ tabGridLayout: rest });
    get().persistState();
  },

  setTabGridSizes: (teamKey, workspaceId, preset, root, path, sizes) => {
    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    set((s) => ({
      tabGridLayout: {
        ...s.tabGridLayout,
        [selectWorkspaceKey(s, teamKey, workspaceId)]: {
          preset,
          root: setNodeSizes(root, path, sizes),
        },
      },
    }));
    get().persistState();
  },

  setCliExpanded: (expanded) => {
    beginContextNavigation();
    if (get().cliExpanded === expanded) return;
    set({ cliExpanded: expanded });
    get().persistState();
  },

  toggleCliExpanded: () => get().setCliExpanded(!get().cliExpanded),

  expandWorkspace: (teamKey, workspaceId) => {
    if (get().activeWorkspace[teamKey] !== workspaceId) get().switchWorkspace(teamKey, workspaceId);
    if (
      teamKey === getActiveTeamKey() &&
      get().activeWorkspace[teamKey] === workspaceId &&
      get().workspaces[teamKey]?.some((workspace) => workspace.id === workspaceId)
    )
      get().setCliExpanded(true);
  },

  setForegroundPreset: (teamKey, preset) => {
    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    set((s) => {
      // SW-EW-40: preset applies to the active slot — in grouped mode it rebuilds
      // only the selected epic's arrangement (per-rail), flat unchanged.
      const slot = activeForegroundSlot(s, teamKey);
      const base = defaultForegroundLayout(slot.layout);
      // columns→row, rows→column drive the flat 1-D group; grid keeps the axis
      // (the nested grid renderer derives its own). A preset change shows the grid, so it
      // leaves expanded (locked rule 2).
      const direction =
        preset === "columns" ? "row" : preset === "rows" ? "column" : base.direction;
      // SW-EW-17: choosing a preset RESETS any custom mosaic — the arrangement is
      // re-derived from the preset at render (root null → flat/grid).
      // Its sizes go too, so a preset always lays out evenly.
      return {
        ...slot.write({ ...base, preset, direction, root: null, sizes: [] }),
        ...(teamKey === getActiveTeamKey() ? { cliExpanded: false } : {}),
      };
    });
    get().persistState();
  },

  setEpicReposOpen: (workspaceId, open) => {
    set((s) => {
      if ((s.epicReposOpen[workspaceId] ?? false) === open) return s;
      return { epicReposOpen: { ...s.epicReposOpen, [workspaceId]: open } };
    });
  },

  toggleEpicReposOpen: (workspaceId) => {
    set((s) => ({
      epicReposOpen: {
        ...s.epicReposOpen,
        [workspaceId]: !(s.epicReposOpen[workspaceId] ?? false),
      },
    }));
  },

  setLayoutMode: (mode) => {
    beginContextNavigation();
    set({
      layoutMode: mode,
      terminalVisible: mode !== "viewer",
      ...(mode === "console" || mode === "shared" ? { lastTerminalMode: mode } : {}),
    });
    get().persistState();
  },

  toggleConsoleShared: () => {
    const current = get().layoutMode ?? "shared";
    if (current === "viewer") return; // no-op in viewer mode
    get().setLayoutMode(current === "console" ? "shared" : "console");
  },

  // SW-EW-38: tray lens flip — persisted (AC-2 reload restores it).
  setTrayMode: (teamKey, mode) => {
    set((s) => ({ trayMode: updateTeamKeyed(s.trayMode, teamKey, mode) }));
    get().persistState();
  },

  // SW-EW-38: selected epic-rail key — runtime-only (R3), no persistState().
  // Follow-focus reconstructs it on grouped-mode entry / focus change.
  setSelectedEpicGroup: (teamKey, railKey) => {
    set((s) => ({ selectedEpicDirName: updateTeamKeyed(s.selectedEpicDirName, teamKey, railKey) }));
  },

  expandTerminalOnOpen: (teamKey = getActiveTeamKey()) => {
    if (teamKey !== getActiveTeamKey()) return;
    // Already-visible chrome, including its saved reopen mode, survives unrelated opens.
    const s = get();
    if (s.layoutMode === "console" || (s.layoutMode === "shared" && s.terminalVisible)) return;
    // An automatic reveal is part of the pending open, not a newer user navigation.
    set({ layoutMode: s.lastTerminalMode, terminalVisible: true });
    get().persistState();
  },

  revealTerminalInPlace: (teamKey = getActiveTeamKey()) => {
    if (teamKey !== getActiveTeamKey()) return;
    const s = get();
    const mode = s.layoutMode ?? "shared";
    if (mode !== "viewer" && s.terminalVisible) return;
    // Written directly (not via `setLayoutMode`) so the user's saved
    // `lastTerminalMode` is not overwritten by a launch.
    set({ layoutMode: mode === "console" ? "console" : "shared", terminalVisible: true });
    get().persistState();
  },

  settleInitialLayout: (teamKey) => {
    const s = get();
    if (s.layoutMode !== null) return;
    const mode = activeForegroundTiles(s, teamKey).length > 0 ? "shared" : "viewer";
    set({
      layoutMode: mode,
      terminalVisible: mode !== "viewer",
      ...(mode === "shared" ? { lastTerminalMode: mode } : {}),
    });
    get().persistState();
  },

  // SW-SN-1: `setScratchNoteContent` (from the editor's onUpdate) writes the
  // ACTIVE note; the id is captured in the debounce args so a mid-flight switch
  // lands edits on the right file.
  setScratchNoteContent: (content) => {
    const id = get().activeNoteId;
    set({ scratchNoteContent: content });
    if (id) {
      debouncedScratchSave.trigger(loadedNotesScope(), id, content);
      return;
    }
    // SW-SN-12 AC-3 / SW-SN-13 AC-1: no active note (defensive — the panel gate
    // keeps the editor read-only until the model resolves AND an active note
    // exists, so this branch is genuinely rare). Buffer the latest content, but
    // only lazily CREATE a note when there is MEANINGFUL content to receive. An
    // empty/whitespace transaction — a blank pad, a no-op editor update, or the
    // 3s grace-timeout escape hatch firing on an empty editor — must NEVER mint a
    // note: that is exactly the duplicate-empty-"Scratch" regression (SW-SN-13).
    // A real (non-empty) keystroke still lands, preserving SW-SN-12's guarantee.
    noActiveNoteContent = content;
    if (content.trim() === "") return; // AC-1: nothing meaningful → create nothing
    if (creatingNoteForBuffer) return;
    creatingNoteForBuffer = true;
    lazyCreateInFlight = (async () => {
      try {
        const newId = await get().createNote(DEFAULT_SCRATCH_TITLE);
        const latest = noActiveNoteContent;
        // Re-check after the await: reject a create failure (newId falsy), a null
        // buffer, OR a buffer that went empty while we created (the edit was
        // cleared mid-flight). Leave the buffer intact so the next keystroke
        // retries — never null it before it's committed somewhere. (AC-1)
        if (!newId || latest == null || latest.trim() === "") return;
        if (get().activeNoteId === newId) {
          // Still on the note we created — reflect it and persist via the normal
          // debounce.
          noActiveNoteContent = null;
          set({ scratchNoteContent: latest });
          debouncedScratchSave.trigger(loadedNotesScope(), newId, latest);
        } else {
          // A concurrent switch/load stole focus mid-create. Persist the buffered
          // content STRAIGHT to the note we created FOR it — so it's neither lost
          // nor left as an empty orphan — without fighting the now-active note's
          // debounce (which coalesces on a single pending slot and would drop
          // this write). Awaited within the chain so a teardown `flushNotes` that
          // drains this promise sees the write land; the buffer is nulled only
          // after it resolves.
          await saveNoteBody(loadedNotesScope(), newId, latest);
          noActiveNoteContent = null;
        }
      } catch (e) {
        logger.error("viewer", `setScratchNoteContent lazy-create failed: ${e}`);
      } finally {
        creatingNoteForBuffer = false;
        lazyCreateInFlight = null;
      }
    })();
  },

  // SW-SN-1: boot/refresh the plural notes model. `listNotes` triggers the
  // Rust-side legacy→notes/ migration (and corrupt-index recovery) on its first
  // call. Also the re-sync primitive SN-7's external (agent) hot-reload path
  // calls after a watcher event — so it flushes any pending save first and
  // PRESERVES the active note when it still exists (only falling back to
  // most-recent on a cold boot), rather than yanking the user off their note.
  loadNotes: async () => {
    // Land any pending edit before re-reading disk — safe as a hot-reload primitive.
    await debouncedScratchSave.flush();
    // What the editor holds now: if the person types while this reads, their edit
    // (and the body it started from) wins over the re-read; its save merges.
    const typedFrom = get().scratchNoteContent;
    // Capture WITHOUT bumping: a passive refresh must defer to any explicit
    // switch/create that lands while we read, never supersede it.
    const gen = activeSwitchGeneration;
    // SLW-PT-19: the scope this load reads; it becomes the loaded notes' scope.
    const scope = notesScope();
    const sameScope = holdsNotesOf(scope);
    try {
      let notes: NoteMeta[] = await listNotes(scope);
      // SW-SN-13 AC-4: one-shot, guarded reap of the empty duplicate "Scratch"
      // notes the SN-12 lazy-create minted. Runs ONCE per session (not on an
      // SN-7 hot-reload refresh) and off the first-paint path (the panel shows a
      // loading affordance until "loaded"). Idempotent + best-effort: a reap
      // failure must never break the load. Preserves the active note; the Rust
      // reaper never drops the sole survivor.
      if (!get().emptyScratchReapDone) {
        // Latch BEFORE the await so a re-entrant loadNotes (hot-reload) can't
        // double-run the reap; reset on failure so a transient IPC error retries
        // on the next load rather than forfeiting the cleanup for the session.
        set({ emptyScratchReapDone: true });
        try {
          // `notes` here is the boot snapshot (from listNotes above) — pass its ids
          // as the reap-eligible set so a note created concurrently with the reap
          // (e.g. via the panel's 3s editable hatch) is structurally immune.
          const reaped = await reapEmptyScratchNotes(
            scope,
            get().activeNoteId ?? undefined,
            notes.map((n) => n.id),
          );
          if (reaped) {
            // The reap returns bare metadata: keep each survivor's listed preview.
            const listed = new Map(notes.map((n) => [n.id, n]));
            notes = reaped.map((n) => ({ ...listed.get(n.id), ...n }));
          }
        } catch (e) {
          set({ emptyScratchReapDone: false });
          logger.error("viewer", `reapEmptyScratchNotes failed: ${e}`);
        }
      }
      // Another org's notes are another set: keep the active note only on a refresh
      // of the SAME scope.
      const current = sameScope ? get().activeNoteId : null;
      const target = (current && notes.find((n) => n.id === current)) || mostRecentNote(notes);
      const note = target ? await readNote(scope, target.id) : null;
      // An explicit active-note change superseded this refresh — keep ITS active
      // note; only merge the fresh list metadata (without dropping a note it may
      // have just created).
      if (gen !== activeSwitchGeneration) {
        set((s) => ({ notes: mergeNotesById(s.notes, notes) }));
        return;
      }
      if (target && note) {
        if (sameScope && target.id === current && typedSince(current, typedFrom)) {
          set({ notes, notesLoadedScope: scope, scratchNoteLoadStatus: "loaded" });
          return;
        }
        set({
          notes,
          notesLoadedScope: scope,
          ...adoptedNoteBody(scope, target.id, note.content),
          scratchNoteLoadStatus: "loaded",
        });
      } else {
        // SW-SN-13 AC-3 (suspenders): migration guarantees >=1 note, so this is
        // effectively unreachable — but NEVER settle a null active id under a
        // "loaded" status (that leaves the editor editable with no active note,
        // the exact window this story closes). Deterministically create ONE
        // canonical note to land on. `createNote` bumps + gates the generation
        // token itself, so a concurrent explicit switch still wins.
        set({ notes, notesLoadedScope: scope });
        try {
          await get().createNote(DEFAULT_SCRATCH_TITLE);
        } catch (e) {
          logger.error("viewer", `loadNotes empty-branch createNote failed: ${e}`);
          set({ activeNoteId: null, scratchNoteContent: "", scratchNoteLoadStatus: "error" });
        }
      }
    } catch (e) {
      logger.error("viewer", `loadNotes failed: ${e}`);
      set({ scratchNoteLoadStatus: "error" });
    }
  },

  applyNotesProjection: (scope, rows, change) => {
    if (!holdsNotesOf(scope)) return;
    const known = new Map(get().notes.map((n) => [n.id, n]));
    const notes = rows.map((row) => noteFromRow(row, known.get(row.id)));
    const id = get().activeNoteId;
    const before = id ? known.get(id) : undefined;
    const after = id ? notes.find((n) => n.id === id) : undefined;
    set({ notes });
    if (!id || !before) return;
    // The open note is gone (deleted from a pane): land on another, as a reload does.
    if (!after) {
      void get().loadNotes();
      return;
    }
    const own = change?.requestId !== undefined && ownNoteSaves.delete(change.requestId);
    const bodyChanged = change
      ? !own &&
        !BODY_KEEPING_CHANGES.has(change.kind ?? "") &&
        change.upserts.some((row) => row.id === id)
      : after.updatedAt !== before.updatedAt;
    if (bodyChanged) void adoptActiveNoteChange(scope, id);
  },

  // SW-SN-1 AC-5: the single programmatic entry point for switching notes.
  // Flushes the outgoing note's pending save FIRST (else its last edits are
  // lost), loads the target body, and bumps `lastOpenedAt` (recency). A
  // generation token drops a stale result so fast A->B->C switching always
  // lands on the last-selected note regardless of IO resolution order.
  setActiveNote: async (id) => {
    if (get().activeNoteId === id) return;
    const gen = ++activeSwitchGeneration;
    await debouncedScratchSave.flush();
    try {
      const note = await readNote(loadedNotesScope(), id);
      if (gen !== activeSwitchGeneration) return; // superseded by a newer switch
      rememberNoteBase(loadedNotesScope(), id, note.content);
      const now = Date.now();
      set((s) => ({
        activeNoteId: id,
        scratchNoteContent: note.content,
        scratchNoteLoadStatus: "loaded",
        notes: s.notes.map((m) => (m.id === id ? { ...m, lastOpenedAt: now } : m)),
      }));
      updateNoteMeta(loadedNotesScope(), id, { lastOpenedAt: now }).catch((e) =>
        logger.error("viewer", `updateNoteMeta(lastOpenedAt) failed: ${e}`),
      );
    } catch (e) {
      logger.error("viewer", `setActiveNote(${id}) failed: ${e}`);
    }
  },

  // SW-SN-1: create -> activate. Flush the outgoing save so the new blank note
  // can't inherit a late write from the previous one. Rethrows so a caller can
  // surface the failure — but logs it too.
  createNote: async (title = "Untitled", initialBody) => {
    // Bump BEFORE the flush await (like setActiveNote) so invocation order ==
    // generation order — a switch invoked after this create still wins.
    const gen = ++activeSwitchGeneration;
    await debouncedScratchSave.flush();
    try {
      const note = await createNoteCmd(
        loadedNotesScope(),
        title,
        initialBody === undefined ? {} : { content: initialBody },
      );
      rememberNoteBase(loadedNotesScope(), note.id, note.content);
      const meta = {
        id: note.id,
        title: note.title,
        pinned: note.pinned,
        lastOpenedAt: note.lastOpenedAt,
        ...localPreview(loadedNotesScope(), note.content),
      };
      // Always add the new note to the list; only steal focus if no newer
      // active-note change (a switch/load) superseded us mid-create.
      if (gen !== activeSwitchGeneration) {
        set((s) => ({ notes: upsertNote(s.notes, meta) }));
        return note.id;
      }
      set((s) => ({
        notes: upsertNote(s.notes, meta),
        activeNoteId: note.id,
        // Reflect the seeded body from the create result — never re-reading via
        // setActiveNote (which no-ops on the already-active new id).
        scratchNoteContent: note.content,
        scratchNoteLoadStatus: "loaded",
      }));
      return note.id;
    } catch (e) {
      logger.error("viewer", `createNote failed: ${e}`);
      throw e;
    }
  },

  renameNote: async (id, title) => {
    try {
      const meta = await updateNoteMeta(loadedNotesScope(), id, { title });
      set((s) => ({ notes: s.notes.map((m) => (m.id === id ? { ...m, title: meta.title } : m)) }));
    } catch (e) {
      logger.error("viewer", `renameNote(${id}) failed: ${e}`);
    }
  },

  // SW-SN-1 AC-4: the command reseeds a fresh note if this was the last one and
  // returns the resulting list. If the active note was deleted, switch to the
  // most-recent survivor. Land any pending or in-flight save first, so the deleted
  // note keeps every keystroke for a restore and no late write follows the delete.
  deleteNote: async (id) => {
    const wasActive = get().activeNoteId === id;
    await debouncedScratchSave.flush();
    try {
      const list = await deleteNoteCmd(loadedNotesScope(), id);
      set({ notes: list });
      if (wasActive) {
        const next = mostRecentNote(list);
        if (next) {
          // setActiveNote bumps the generation itself.
          await get().setActiveNote(next.id);
        } else {
          ++activeSwitchGeneration;
          set({ activeNoteId: null, scratchNoteContent: "" });
        }
      }
    } catch (e) {
      logger.error("viewer", `deleteNote(${id}) failed: ${e}`);
    }
  },

  setNotePinned: async (id, pinned) => {
    try {
      const meta = await updateNoteMeta(loadedNotesScope(), id, { pinned });
      set((s) => ({
        notes: s.notes.map((m) => (m.id === id ? { ...m, pinned: meta.pinned } : m)),
      }));
    } catch (e) {
      logger.error("viewer", `setNotePinned(${id}) failed: ${e}`);
    }
  },

  toggleRightPane: () => {
    beginContextNavigation();
    const isOpen = get().rightPaneOpen;
    set({ rightPaneOpen: !isOpen });
    // R5B-P2: if startup load never completed successfully AND the user
    // hasn't typed anything yet, retry on first open. Recovers from a
    // transient IPC error during `loadPersistedState`. The content-empty
    // guard prevents clobbering anything the user has already written
    // (a user with `scratchNoteContent !== ""` has either successfully
    // loaded their content OR typed during the boot-load gap — either way,
    // we don't want to re-fire the load and replace what's in memory).
    const status = get().scratchNoteLoadStatus;
    if (!isOpen && status !== "loaded" && get().scratchNoteContent === "") {
      void get().loadNotes();
    }
    get().persistState();
  },

  setRightPaneRatio: (ratio) => {
    set({ rightPaneRatio: ratio });
    get().persistState();
  },

  setRightPaneSplitRatio: (teamKey, ratio) => {
    set((s) => ({
      rightPaneSplitRatio: updateTeamKeyed(s.rightPaneSplitRatio, teamKey, ratio),
    }));
    get().persistState();
  },

  // Retire outgoing frames before removing the DOM that owns their document.
  // Other layout changes stay synchronous.
  setRightPaneLayout: (layout) => {
    const revision = ++paneLayoutRevision;
    const requestedPanels = new Set(layout.sections.flatMap((s) => s.group.panelIds));
    for (const section of get().rightPaneLayout.sections) {
      for (const panelId of section.group.panelIds)
        if (!requestedPanels.has(panelId)) pendingPanelCloses.add(panelId);
    }
    for (const panelId of pendingPanelCloses) layout = removePanel(layout, panelId);
    const present = new Set(layout.sections.flatMap((s) => s.group.panelIds));
    const leaving = (f: import("@/features/dashboard/lib/frame-lifecycle").LiveFrame) =>
      !!f.panelId && f.slotId === "right-pane" && !present.has(f.panelId);
    const apply = () => {
      if (revision !== paneLayoutRevision) return;
      pendingPanelCloses.clear();
      set({ rightPaneLayout: layout });
      get().persistState();
    };
    if (hasLiveFrames(leaving)) void retireAfterFlush(leaving, apply);
    else apply();
  },

  // SW-SN-3: reveal the notes surface for a jump-from-outside-the-pane. Reuses
  // the right-pane-layout pure ops (`setActivePanel` + `expandSection`) so the
  // scratch panel becomes the active, expanded tab of whatever section holds it
  // — then opens the pane. If the panel is already the active/expanded tab this
  // is idempotent (same layout content). Synchronous `set()`; never blocks.
  // SW-WG-10: the ONE reveal seam — `revealPanel` adds `panelId` as an expanded,
  // active top section when the layout lacks it, else activates+expands it; then open
  // the pane and persist. Synchronous; never blocks. The named reveal actions below
  // delegate here.
  revealPanelById: (panelId) => {
    const nextLayout = revealPanel(get().rightPaneLayout, panelId);
    set({ rightPaneOpen: true });
    get().setRightPaneLayout(nextLayout);
  },

  revealAgentsListFor: (agent) => {
    set({ agentsListFilter: agent });
    const teamKey = getActiveTeamKey();
    if (!teamKeyHasTeam(teamKey)) return;
    get().setWorkspacePanelCollapsed(false);
    get().revealTerminalInPlace();
  },

  clearAgentsListFilter: () => set({ agentsListFilter: null }),

  // SW-SN-6: read-model publish + jump signal. All ephemeral (no persistState) —
  // derived view state, not user data.
  setActiveNoteHeadings: (headings) => set({ activeNoteHeadings: headings }),
  requestHeadingJump: (slug) =>
    set((s) => ({
      // Capture the note this jump targets — the panel discards it if the active
      // note has changed by the time it's consumed.
      pendingHeadingJump: {
        slug,
        noteId: s.activeNoteId,
        nonce: (s.pendingHeadingJump?.nonce ?? 0) + 1,
      },
    })),
  clearHeadingJump: () => set({ pendingHeadingJump: null }),

  toggleStatusNavExpanded: (teamKey: string, absolutePath: string) => {
    set((s) => {
      const current = readTeamKeyed(s.statusNavExpandedPaths, teamKey, []);
      const next = current.includes(absolutePath)
        ? current.filter((p) => p !== absolutePath)
        : [...current, absolutePath];
      return {
        statusNavExpandedPaths: {
          ...s.statusNavExpandedPaths,
          [teamKey]: next,
        },
      };
    });
    get().persistState();
  },

  // SLW-NC-7: personal nav pins are local-only (never committed) — the additive
  // layer on top of the git-shared team pins. Same toggle shape as
  // `toggleStatusNavExpanded`; persisted so a personal pin survives a restart.
  toggleStatusNavPinPersonal: (teamKey: string, path: string) => {
    set((s) => {
      const current = readTeamKeyed(s.statusNavPinnedPaths, teamKey, []);
      const next = current.includes(path) ? current.filter((p) => p !== path) : [...current, path];
      return {
        statusNavPinnedPaths: {
          ...s.statusNavPinnedPaths,
          [teamKey]: next,
        },
      };
    });
    get().persistState();
  },

  setNavLens: (teamKey, patch) => {
    set((s) => {
      const previous = normalizeNavLens(readTeamKeyed(s.navLens, teamKey, DEFAULT_NAV_LENS));
      const sourceChanged =
        patch.source && JSON.stringify(patch.source) !== JSON.stringify(previous.source);
      return {
        ...(sourceChanged
          ? {
              pipelineScope: updateTeamKeyed(s.pipelineScope, teamKey, ""),
              drillPaths: updateTeamKeyed(s.drillPaths, teamKey, ""),
            }
          : {}),
        navLens: {
          ...s.navLens,
          [teamKey]: {
            ...previous,
            ...patch,
            ...(sourceChanged ? { historyBranches: { kind: "all" as const } } : {}),
          },
        },
      };
    });
    get().persistState();
  },

  setTerminalSplitRatio: (ratio) => {
    if (!Number.isFinite(ratio)) return;
    beginContextNavigation();
    set({ terminalSplitRatio: Math.min(0.9, Math.max(0.1, ratio)) });
    get().persistState();
  },

  updateSettings: (partial) => {
    Object.assign(pendingSettings, partial);
    set((s) => ({
      settings: { ...s.settings, ...partial },
    }));
    get().persistState();
  },

  applyLayout: (raw) => {
    let outcome: LayoutApplyOutcome = "stale";
    let focusChanged = false;
    const input = withoutRetiredAuthority(raw);
    if (!input) return outcome;
    set((s) => {
      const applied = applyLayoutInput(s.terminalLayout, input);
      outcome = applied.outcome;
      focusChanged = applied.next.focus !== s.terminalLayout.focus;
      const answered = input.kind === "snapshot" && s.terminalHostsUnreachable;
      if (applied.next === s.terminalLayout && !answered) return s;
      return {
        terminalLayout: applied.next,
        ...(answered ? { terminalHostsUnreachable: false } : {}),
      };
    });
    // A host push can return focus to This Mac without a viewer selection verb.
    // Save that clearing too, so restart cannot resurrect the old remote choice.
    if (input.kind === "change" && focusChanged && get().stateLoaded) get().persistState();
    return outcome;
  },

  noteTerminalHostsUnreachable: () => {
    if (!get().terminalHostsUnreachable) set({ terminalHostsUnreachable: true });
  },

  addTerminalTab: (teamKey, tab, workspaceId, opts = {}) => {
    const s = get();
    const wsId = workspaceId ?? activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
    // SW-DX-17a: idempotent on the session — opening a session this workspace
    // already shows focuses it instead.
    if (selectWorkspaceTabs(s, teamKey, wsId).some((t) => t.sessionId === tab.sessionId)) {
      get().setActiveTerminalTab(teamKey, tab.sessionId, wsId);
      return Promise.resolve();
    }
    const record = workspaceRecord(s, teamKey, wsId);
    if (isRemoteWorkspace(record)) {
      // Its terminals open on its machine (remoteWorkspaceDriver), never here.
      toast(
        "This workspace lives on a machine: open its terminals with ⌘T or the new-terminal menu",
      );
      const refused = Promise.reject(
        new Error(`workspace ${wsId} lives on a remote machine; open its terminals there`),
      );
      refused.catch(logLayoutVerb("open terminal tab"));
      return refused;
    }
    const cwd = opts.cwd || record?.cwd;
    const ws: Workspace | undefined =
      record || cwd
        ? {
            ...(record ?? { id: wsId, kind: opts.kind ?? "generic", status: "ready" }),
            cwd: cwd ?? "",
          }
        : undefined;
    const opened = ws?.cwd
      ? openLayoutTab(s, teamKey, ws, tab, opts)
      : Promise.reject(new Error(`no workspace ${wsId} with a directory to open a terminal in`));
    opened.catch(logLayoutVerb("open terminal tab"));
    return opened;
  },

  respawnTerminalTab: async (teamKey, sessionId, workspaceId, opts = {}) => {
    if (remoteSessionTarget(sessionId) !== undefined) {
      throw new Error("a remote terminal cannot be restarted in place");
    }
    const s = get();
    const record = workspaceRecord(s, teamKey, workspaceId);
    const cwd = opts.cwd || record?.cwd;
    if (!cwd) throw new Error(`no workspace ${workspaceId} with a directory to restart in`);
    const ws: Workspace = {
      ...(record ?? { id: workspaceId, kind: "generic", status: "ready" }),
      cwd,
    };
    const size = terminalSpawnSize();
    // The ended process's exit, agent record and dot stay on screen until the
    // host has started the new one (a refused restart changes nothing); then
    // they go — but not whatever the new process already reported.
    const ended = snapshotAgentSession(sessionId);
    const endedStatus = s.aiStatusBySession[sessionId];
    // Always a fresh shell spawned for this tab (never a warm-pool one): the
    // host starts its ring with the ended session's scrollback, which the new
    // pane replays on attach — an agent tab included.
    const respawned = await terminalRespawnTab(
      LOCAL_MACHINE_ID,
      sessionId,
      tabSpawn(s, ws, size, { envRemove: opts.envRemove, account: opts.account }, true),
      opts.scope,
    );
    retireAgentSession(sessionId, ended);
    // The live entry shows at once (no stale ended bar on the new pane), and
    // the pane remounts on the new process — in the same step.
    get().applyLayout({ kind: "respawned", entry: respawned.entry });
    set((st) => ({
      aiStatusBySession:
        st.aiStatusBySession[sessionId] === endedStatus
          ? pruneSessionMap(st.aiStatusBySession, [sessionId])
          : st.aiStatusBySession,
      terminalLives: { ...st.terminalLives, [sessionId]: (st.terminalLives[sessionId] ?? 0) + 1 },
    }));
    return {
      extraArgs: respawned.extraArgs,
      removeArgs: respawned.removeArgs,
      execution: respawned.execution,
    };
  },

  // SW-GW-2: the addressable "+ new tab" — mint a tab and RETURN its session id
  // so a dashboard can drive it. Non-blocking: the tab appears when the host
  // announces it. Guards on the workspace existing so no tab is opened under a
  // dead id.
  openTabInWorkspace: (teamKey, workspaceId, opts) => {
    const record = readTeamKeyed(get().workspaces, teamKey, []).find((w) => w.id === workspaceId);
    if (!record) return "";
    if (isRemoteWorkspace(record)) {
      if (record.status === "ready")
        remoteWorkspaceDriver()?.openTerminal({ teamKey, workspaceId, location: record.location });
      return "";
    }
    // SW-WA-F6 (AC-1): an optional `name` reads as what the tab is FOR; omitted →
    // the host's `Terminal N`.
    const tab = { ...createTerminalTab(opts?.name), ...(opts?.role ? { role: opts.role } : {}) };
    void get()
      .addTerminalTab(teamKey, tab, workspaceId)
      .catch(() => {});
    return tab.sessionId;
  },

  removeTerminalTab: (teamKey, tabId, workspaceId, opts = {}) => {
    const s = get();
    const wsId = workspaceId ?? activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
    const tabs = selectWorkspaceTabs(s, teamKey, wsId);
    const shown = tabs.find((t) => t.id === tabId);
    const removed = shown ?? selectHiddenTerminalTabs(s, teamKey, wsId).find((t) => t.id === tabId);
    if (!removed || closingSessions.has(tabId)) {
      return opts.throwOnError
        ? Promise.reject(
            new Error("Terminal close is already in progress or the tab is no longer shown"),
          )
        : Promise.resolve();
    }
    closingSessions.add(tabId);
    // Its cell leaves the saved tab-grid sizes; the others keep theirs.
    const tabGridLayout = withoutTabGridCell(s, teamKey, wsId, tabId);
    if (tabGridLayout !== s.tabGridLayout) set({ tabGridLayout });
    // The live-mode "never leave the area empty" rule, for an in-session close
    // only: closing the last ordinary tab opens a fresh one. Never for a
    // role-tagged agent tab (a roleless replacement would break the role
    // binding) and never when the caller asked not to (a detach or a rollback).
    const lastTab =
      shown !== undefined &&
      opts.replace !== false &&
      !removed.role &&
      tabs.every((t) => t.id === tabId || closingSessions.has(t.id));

    // SW-FX-52: prune the closed tab's status/progress (a tab closed with its
    // process kept running never emits `pty-exit`), so these runtime maps stay
    // bounded.
    set((st) => ({
      aiStatusBySession: pruneSessionMap(st.aiStatusBySession, [removed.sessionId]),
      progressBySession: pruneSessionMap(st.progressBySession, [removed.sessionId]),
      progressTickBySession: pruneSessionMap(st.progressTickBySession, [removed.sessionId]),
      // SW-TP CAP-3: the six diff content-state slices are session-keyed now;
      // clear the closed tab's entries so a reopened session doesn't inherit
      // stale selection / mode / SHA / view-options from the departed tab.
      sourceDiffSelectedFile: pruneSessionMap(st.sourceDiffSelectedFile, [removed.sessionId]),
      sourceDiffHistoryMode: pruneSessionMap(st.sourceDiffHistoryMode, [removed.sessionId]),
      sourceDiffSelectedSha: pruneSessionMap(st.sourceDiffSelectedSha, [removed.sessionId]),
      sourceDiffBaseMode: pruneSessionMap(st.sourceDiffBaseMode, [removed.sessionId]),
      sourceDiffViewOptions: pruneSessionMap(st.sourceDiffViewOptions, [removed.sessionId]),
      sourceDiffMode: pruneSessionMap(st.sourceDiffMode, [removed.sessionId]),
    }));
    pruneAgentSessions([removed.sessionId]);
    // SW-TP CAP-9 polish: the per-tab inbox rail's open/closed state lives in
    // its own store (agent-inbox-rail-store). Close it alongside the rest of
    // the per-session prune so a reopened session can't inherit a ghost
    // open-state from the removed tab.
    closeAgentInboxRail(removed.sessionId);

    // PT-2 state-driven close, decided by the host where the process lives: an
    // idle shell (or an exited tab) goes, a busy one keeps running out of sight.
    return terminalCloseTab(removed.sessionId, opts.kill ? "kill" : "keepIfBusy")
      .then((outcome) => {
        // Gone from this viewer at once, ahead of the host's announcement, and
        // its display with it. A tab kept running out of sight keeps both.
        if (outcome !== "backgrounded") {
          get().applyLayout({ kind: "dismissed", session: removed.sessionId });
          set((st) => ({ terminalLives: pruneSessionMap(st.terminalLives, [removed.sessionId]) }));
          disposeTerminalDisplay(removed.sessionId, "close terminal tab");
        }
        if (lastTab) {
          const record = workspaceRecord(get(), teamKey, wsId);
          if (isRemoteWorkspace(record))
            remoteWorkspaceDriver()?.openTerminal({
              teamKey,
              workspaceId: wsId,
              location: record.location,
            });
          else
            void get()
              .addTerminalTab(teamKey, createTerminalTab(), wsId)
              .catch(() => {});
        }
      })
      .catch((error) => {
        logLayoutVerb("close terminal tab")(error);
        if (opts.throwOnError) throw error;
      })
      .finally(() => closingSessions.delete(tabId));
  },

  adoptTerminalSession: (teamKey, sessionId, workspaceId, opts) => {
    const s = get();
    const wsId = workspaceId ?? activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
    const record = workspaceRecord(s, teamKey, wsId);
    const key = selectWorkspaceKey(s, teamKey, wsId);
    return terminalPlaceTab(
      sessionId,
      key,
      "tab",
      undefined,
      record ? workspaceDescriptor(teamKey, record) : undefined,
    )
      .then(() => {
        if (opts?.isCurrent?.() !== false) return terminalSetActive(key, sessionId);
      })
      .catch(logLayoutVerb("bring terminal to the foreground"));
  },

  setActiveTerminalTab: (teamKey, tabId, workspaceId) => {
    const s = get();
    const wsId = workspaceId ?? activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
    const key = selectWorkspaceKey(s, teamKey, wsId);
    // Selecting the displayed tab still retires any older pending ACK.
    const override = s.terminalLayout.focus?.[key];
    if (selectActiveTerminalTab(s, teamKey, wsId) === tabId && (!override || override === tabId)) {
      terminalFocusRequests.delete(key);
      return;
    }
    terminalSetActive(key, tabId).catch(logLayoutVerb("focus terminal tab"));
  },

  cycleTerminalTab: (teamKey, direction) => {
    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    const s = get();
    const wsId = activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
    const tabs = selectWorkspaceTabs(s, teamKey, wsId);
    if (tabs.length <= 1) return false;
    const delta = direction === "next" ? 1 : -1;
    const idx = tabs.findIndex((t) => t.id === selectActiveTerminalTab(s, teamKey, wsId));
    // No active tab yet: next lands on the first, prev on the last.
    const base = idx === -1 ? (direction === "next" ? -1 : 0) : idx;
    const nextId = tabs[(((base + delta) % tabs.length) + tabs.length) % tabs.length].id;
    get().setActiveTerminalTab(teamKey, nextId, wsId);
    return true;
  },

  toggleStoryHudShown: (workspaceId, shown) => {
    set((s) => {
      const next = shown ?? !s.storyHudShown[workspaceId];
      if ((s.storyHudShown[workspaceId] ?? false) === next) return s; // change-only
      const storyHudShown = { ...s.storyHudShown, [workspaceId]: next };
      // Hiding resets the tile back to compact so a re-open always starts small.
      if (!next && s.storyHudExpanded[workspaceId]) {
        return {
          storyHudShown,
          storyHudExpanded: { ...s.storyHudExpanded, [workspaceId]: false },
        };
      }
      // Opening closes the sibling overlays on the ACTIVE TAB of this
      // workspace (per-tab source/editor are mutually exclusive with the HUD).
      // When the workspace has no live tab the sibling-closer is a no-op.
      if (next) {
        const teamKey = teamKeyOfWorkspace(s, workspaceId);
        // The TerminalTab's id IS its sessionId (see TerminalTab contract) — the
        // active-tab selector returns that id directly.
        const activeSid = teamKey
          ? (selectActiveTerminalTab(s, teamKey, workspaceId) ?? null)
          : null;
        return {
          storyHudShown,
          ...closeSiblingOverlays(s, workspaceId, activeSid, "storyHud"),
        };
      }
      return { storyHudShown };
    });
    // Runtime-only — each tile starts hidden on launch.
  },

  toggleStoryHudSize: (workspaceId, expanded) => {
    set((s) => {
      const next = expanded ?? !s.storyHudExpanded[workspaceId];
      if ((s.storyHudExpanded[workspaceId] ?? false) === next) return s; // change-only
      return { storyHudExpanded: { ...s.storyHudExpanded, [workspaceId]: next } };
    });
    // Runtime-only — each tile starts compact on launch.
  },

  toggleSourceDiffShown: ({ sessionId, workspaceId, shown }) => {
    set((s) => {
      const next = shown ?? !s.sourceDiffShownBySession[sessionId];
      if ((s.sourceDiffShownBySession[sessionId] ?? false) === next) return s; // change-only
      const sourceDiffShownBySession = {
        ...s.sourceDiffShownBySession,
        [sessionId]: next,
      };
      // Opening closes the other overlays on the SAME tab (sibling mutual
      // exclusion). StoryHud is still per-workspace. There is no instance-
      // wide cap: the data-source seam dedupes fetches across tabs, so N
      // source views cost one fetch per (cwd, baseRef) per edit burst.
      if (next) {
        return {
          sourceDiffShownBySession,
          ...closeSiblingOverlays(s, workspaceId, sessionId, "sourceDiff"),
        };
      }
      return { sourceDiffShownBySession };
    });
    // Runtime-only — each tab starts hidden on launch; selected file / mode /
    // commit are kept per-workspace (session memory), so re-opening resumes.
  },

  // SW-TP CAP-3: the four content-state setters below moved from `workspaceId`
  // to `sessionId` so siblings on the same workspace carry independent diff
  // selection / mode / SHA / base. Guards against the (defensive) empty-string
  // id so an un-keyed call never writes a '' bucket.
  setSourceDiffSelectedFile: (sessionId, filePath) => {
    if (!sessionId) return;
    set((s) => {
      if ((s.sourceDiffSelectedFile[sessionId] ?? null) === filePath) return s; // change-only
      return { sourceDiffSelectedFile: { ...s.sourceDiffSelectedFile, [sessionId]: filePath } };
    });
  },

  setSourceDiffHistoryMode: (sessionId, historyMode) => {
    if (!sessionId) return;
    set((s) => {
      if ((s.sourceDiffHistoryMode[sessionId] ?? undefined) === historyMode) return s; // change-only
      return { sourceDiffHistoryMode: { ...s.sourceDiffHistoryMode, [sessionId]: historyMode } };
    });
    // Runtime-only (RD-9): both the adaptive pick and an explicit toggle set this;
    // once set it sticks (a nonce refresh never re-adapts), reset only on hide/close.
  },

  setSourceDiffSelectedSha: (sessionId, sha) => {
    if (!sessionId) return;
    set((s) => {
      if ((s.sourceDiffSelectedSha[sessionId] ?? null) === sha) return s; // change-only
      return { sourceDiffSelectedSha: { ...s.sourceDiffSelectedSha, [sessionId]: sha } };
    });
    // Runtime-only — `null` selects the pinned Working-tree node (RD-11).
  },

  setSourceDiffBaseMode: (sessionId, mode) => {
    if (!sessionId) return;
    set((s) => {
      if ((s.sourceDiffBaseMode[sessionId] ?? "branchBase") === mode) return s; // change-only
      return { sourceDiffBaseMode: { ...s.sourceDiffBaseMode, [sessionId]: mode } };
    });
    // Session memory (AC-2) — persists across a hide, cleared on tab close.
  },

  toggleSourceDiffOption: (sessionId, key, value) => {
    if (!sessionId) return;
    set((s) => {
      const opts = s.sourceDiffViewOptions[sessionId];
      // Resolve the effective current value first (inline follows the Settings
      // default, autoFollow defaults ON) so an omitted `value` flips relative to
      // that default and the change-only guard compares against the same baseline.
      // fileTree's effective value also depends on the change set (the overlay
      // auto-picks tree mode for large sets), which the store cannot see: an
      // explicit value compares against what is stored, or a flip away from an
      // auto-chosen tree would be dropped as "no change".
      const current =
        key === "fileTree" && value !== undefined
          ? opts?.fileTree
          : resolveSourceDiffOption(opts, key, s.settings);
      const next = value ?? !current;
      if (current === next) return s; // change-only
      return {
        sourceDiffViewOptions: {
          ...s.sourceDiffViewOptions,
          [sessionId]: { ...opts, [key]: next },
        },
      };
    });
    // Runtime-only — each tab seeds view options from defaults on launch. (SW-TP CAP-3)
  },

  setSourceDiffMode: (sessionId, mode) => {
    if (!sessionId) return;
    set((s) => {
      const current = s.sourceDiffMode[sessionId] ?? "line";
      if (current === mode) return s; // change-only
      return { sourceDiffMode: { ...s.sourceDiffMode, [sessionId]: mode } };
    });
  },

  toggleWorktreeEditorOpen: ({ sessionId, workspaceId, open }) => {
    set((s) => {
      const next = open ?? !s.worktreeEditorOpenBySession[sessionId];
      if ((s.worktreeEditorOpenBySession[sessionId] ?? false) === next) return s; // change-only
      const worktreeEditorOpenBySession = {
        ...s.worktreeEditorOpenBySession,
        [sessionId]: next,
      };
      // Opening closes sibling overlays on the SAME tab (editor ↔ diff mutual
      // exclusion within one tab; HUD per-workspace). No instance-wide cap.
      if (next) {
        return {
          worktreeEditorOpenBySession,
          ...closeSiblingOverlays(s, workspaceId, sessionId, "worktreeEditor"),
        };
      }
      return { worktreeEditorOpenBySession };
    });
    // Runtime-only — each tab starts closed; remembered file + unsaved draft
    // (SLW-DV-9) are kept per-workspace, untouched by close.
  },

  setWorktreeEditorRevealLine: (workspaceId, reveal) => {
    set((s) => {
      const prev = s.worktreeEditorRevealLine[workspaceId];
      if (reveal == null) {
        if (prev === undefined) return s; // change-only
        const next = { ...s.worktreeEditorRevealLine };
        delete next[workspaceId];
        return { worktreeEditorRevealLine: next };
      }
      if (prev?.path === reveal.path && prev.line === reveal.line) return s;
      return {
        worktreeEditorRevealLine: { ...s.worktreeEditorRevealLine, [workspaceId]: reveal },
      };
    });
  },

  setWorktreeEditorFile: (workspaceId, filePath) => {
    set((s) => {
      if ((s.worktreeEditorFile[workspaceId] ?? null) === filePath) return s; // change-only
      return { worktreeEditorFile: { ...s.worktreeEditorFile, [workspaceId]: filePath } };
    });
    // Runtime-only — the editor resumes this file on its next open.
  },

  setWorktreeEditorDraft: (workspaceId, relPath, draft) => {
    set((s) => {
      const forWs = s.worktreeEditorDraft[workspaceId] ?? {};
      if (draft == null) {
        if (forWs[relPath] === undefined) return s; // change-only
        const nextForWs = { ...forWs };
        delete nextForWs[relPath];
        const next = { ...s.worktreeEditorDraft };
        if (Object.keys(nextForWs).length === 0) delete next[workspaceId];
        else next[workspaceId] = nextForWs;
        return { worktreeEditorDraft: next };
      }
      return {
        worktreeEditorDraft: {
          ...s.worktreeEditorDraft,
          [workspaceId]: { ...forWs, [relPath]: draft },
        },
      };
    });
    // Runtime-only — seeded back into the editor on its next mount.
  },

  setWorktreeEditorTabs: (workspaceId, tabs) => {
    set((s) => {
      const prior = s.worktreeEditorTabs[workspaceId] ?? [];
      if (prior.length === tabs.length && prior.every((t, i) => t === tabs[i])) return s;
      return { worktreeEditorTabs: { ...s.worktreeEditorTabs, [workspaceId]: tabs } };
    });
  },

  bumpWorktreeChangedPath: (workspaceId, path, resumed = false) => {
    set((s) => {
      const prev = s.worktreeChangedPath[workspaceId];
      return {
        worktreeChangedPath: {
          ...s.worktreeChangedPath,
          // Fresh nonce each time so a repeat change to the SAME path still
          // registers as a new signal for the inset's effect (SLW-WDE-3).
          [workspaceId]: {
            path,
            nonce: (prev?.nonce ?? 0) + 1,
            ...(prev?.resumed || resumed
              ? { resumed: (prev?.resumed ?? 0) + (resumed ? 1 : 0) }
              : {}),
          },
        },
      };
    });
  },

  setOpenFileLifecycle: (path, lifecycle) => {
    const key = normalizeStoryFilePath(path);
    set((s) => {
      const current = s.openFileLifecycleByPath[key];
      // The slice holds ONLY non-active paths, so `null`/`"active"` means delete.
      if (lifecycle === null || lifecycle === "active") {
        if (current === undefined) return s; // stable reference — no churn
        const { [key]: _drop, ...rest } = s.openFileLifecycleByPath;
        return { openFileLifecycleByPath: rest };
      }
      if (current === lifecycle) return s; // Object.is-stable no-op
      return { openFileLifecycleByPath: { ...s.openFileLifecycleByPath, [key]: lifecycle } };
    });
  },

  setSessionAiStatus: (sessionId, status) => {
    set((s) => {
      // SW-FX-52: O(1) write into the flat slice keyed by sessionId — no scan of
      // live workspaces or soft-closed shadows (the old O(N) double-scan). Skip
      // when unchanged so the slice identity is stable across redundant events
      // (mirrors the prior no-op-on-equal behavior). A `sessionId` is globally
      // unique, so this serves live and minimized (shadow) tabs alike — the
      // PT-4 minimized-card status now reads the same slice, no shadow branch.
      const current = s.aiStatusBySession[sessionId];
      if (current === status) return s;
      const aiStatusBySession = { ...s.aiStatusBySession };
      if (status === undefined) delete aiStatusBySession[sessionId];
      else aiStatusBySession[sessionId] = status;
      return { aiStatusBySession };
    });
  },

  setSessionProgress: (sessionId, progress) => {
    set((s) => {
      // SW-AM-4: O(1) write into the flat `progressBySession` slice, mirroring
      // `setSessionAiStatus`. Skip when field-equal so a redundant tick (agents
      // re-emit the same phase/note) doesn't churn slice identity and re-render
      // the subscribed row for nothing.
      const current = s.progressBySession[sessionId];
      const unchanged =
        progress === undefined ? current === undefined : progressEqual(current, progress);
      // feat-idle-agent-status: a tick is a tick even when its fields are
      // identical to the previous one — stall detection reads this timestamp,
      // so an unchanged repeat must still freshen it. A clear (progress ===
      // undefined) drops the timestamp alongside the entry.
      const progressTickBySession = { ...s.progressTickBySession };
      if (progress === undefined) delete progressTickBySession[sessionId];
      else progressTickBySession[sessionId] = Date.now();
      const nextTick =
        progress === undefined
          ? !(sessionId in s.progressTickBySession)
            ? s.progressTickBySession
            : progressTickBySession
          : progressTickBySession;
      if (unchanged) return nextTick === s.progressTickBySession ? s : { progressTickBySession };
      const progressBySession = { ...s.progressBySession };
      if (progress === undefined) delete progressBySession[sessionId];
      else progressBySession[sessionId] = progress;
      return { progressBySession, progressTickBySession: nextTick };
    });
  },

  switchWorkspace: (teamKey, workspaceId) => {
    let activated = false;
    set((s) => {
      const ws = readTeamKeyed(s.workspaces, teamKey, []);
      if (!ws.some((w) => w.id === workspaceId)) return s;
      activated = true;
      const newState: Partial<ViewerState> = {
        activeWorkspace: updateTeamKeyed(s.activeWorkspace, teamKey, workspaceId),
      };
      // Expanded follows the focus by definition: it always zooms the focused workspace.
      return newState;
    });
    // SW-WL-3 F-5: roots attach their watcher on first ACTIVATION, not on
    // `ensureRootWorkspaces` mint — otherwise every attached repo's root
    // would attach at team activation and defeat AC-12 scaling. Idempotent,
    // so re-activating the same workspace does not re-attach. Gated on a
    // resolvable cwd (like the mint path): activating a not-yet-cloned root
    // (cwd "") must not latch membership before it can attach, or the later
    // creating→ready open is a no-op and the surface renders unwatched.
    if (activated) {
      get().openActiveSurfaceWatch(teamKey, workspaceId);
      // SW-EW-33: record recency for the flip-to-last gesture. This is the central
      // focus path (cycle, jump-root, click-focus, reopen all route here).
      set((s) => ({ mruWorkspaceStack: pushMruStack(s.mruWorkspaceStack, teamKey, workspaceId) }));
    }
  },

  // SLW-CO-6 (AC-1/AC-7): the unified sync open primitive. See
  // `OpenWorkspaceParams`. Owns the shared spine so epic/generic/minimized are
  // thin wrappers below. The STORY path routes its create through `ensureWorktree`
  // (hardened async driver) and only reuses `finishForegroundOpen`, so it is NOT
  // funnelled through here — foreground vs minimized is the `minimized` flag.
  applyAgentPrepareProgress: (state) => {
    const now = Date.now();
    set((s) => {
      let changed = false;
      const workspaces = { ...s.workspaces };
      for (const [teamKey, list] of Object.entries(s.workspaces)) {
        let touched = false;
        const next = list.map((w) => {
          if (!workspaceMatchesIdentity(w, state.workspace)) return w;
          // The expected agent has reported: a later re-create waits for none.
          expectedAgentLaunches.delete(`${teamKey}\0${w.id}`);
          const slots = applyPrepare({ base: w.setupState, runs: w.agentRuns }, state, now);
          // A settled run says what it kept; the next settled run replaces it.
          const keptModified: KeptModified | undefined =
            state.settled && state.summary
              ? state.summary.keptModifiedCount > 0
                ? {
                    agent: state.agent,
                    paths: state.summary.keptModified,
                    count: state.summary.keptModifiedCount,
                  }
                : undefined
              : w.keptModified;
          const { keptModified: _kept, ...without } = withSlots(w, slots);
          const updated = keptModified ? { ...without, keptModified } : without;
          if (
            sameSlots(w, updated) &&
            JSON.stringify(keptModified) === JSON.stringify(w.keptModified)
          )
            return w;
          touched = true;
          return updated;
        });
        if (touched) {
          changed = true;
          workspaces[teamKey] = next;
        }
      }
      return changed ? { workspaces } : s;
    });
  },

  setWorkspaceSetupState: (teamKey, workspaceId, state) => {
    set((s) => {
      const list = s.workspaces[teamKey];
      if (!list) return s;
      let changed = false;
      const next = list.map((w) => {
        if (w.id !== workspaceId || setupStateEqual(w.setupState, state)) return w;
        changed = true;
        const { setupState: _prev, ...rest } = w;
        return state ? { ...rest, setupState: state } : rest;
      });
      return changed ? { workspaces: updateTeamKeyed(s.workspaces, teamKey, next) } : s;
    });
  },

  failAgentRun: (teamKey, workspaceId, agent, reason, code) => {
    set((s) => {
      const list = s.workspaces[teamKey];
      if (!list) return s;
      let changed = false;
      const next = list.map((w) => {
        if (w.id !== workspaceId) return w;
        const updated = withSlots(
          w,
          failRun({ base: w.setupState, runs: w.agentRuns }, agent, reason, code),
        );
        if (sameSlots(w, updated)) return w;
        changed = true;
        return updated;
      });
      return changed ? { workspaces: updateTeamKeyed(s.workspaces, teamKey, next) } : s;
    });
  },

  clearAgentRun: (teamKey, workspaceId, agent) => {
    const now = Date.now();
    set((s) => {
      const list = s.workspaces[teamKey];
      if (!list) return s;
      let changed = false;
      const next = list.map((w) => {
        if (w.id !== workspaceId) return w;
        const updated = withSlots(
          w,
          clearRun({ base: w.setupState, runs: w.agentRuns }, agent, now),
        );
        if (sameSlots(w, updated)) return w;
        changed = true;
        return updated;
      });
      return changed ? { workspaces: updateTeamKeyed(s.workspaces, teamKey, next) } : s;
    });
  },

  setWorkspaceKeptModified: (teamKey, workspaceId, kept) => {
    set((s) => {
      const list = s.workspaces[teamKey];
      if (!list) return s;
      let changed = false;
      const next = list.map((w) => {
        if (w.id !== workspaceId || JSON.stringify(w.keptModified) === JSON.stringify(kept))
          return w;
        changed = true;
        const { keptModified: _prev, ...rest } = w;
        return kept ? { ...rest, keptModified: kept } : rest;
      });
      return changed ? { workspaces: updateTeamKeyed(s.workspaces, teamKey, next) } : s;
    });
  },

  retryWorkspaceCreate: async (teamKey, workspaceId) => {
    const ws = readTeamKeyed(get().workspaces, teamKey, []).find((w) => w.id === workspaceId);
    if (!ws?.storyFilePath) return;
    const storyFilePath = ws.storyFilePath;
    const prior = ws;
    if (!isRemoteWorkspace(ws)) {
      // A failed local create left a record that is not a worktree: put it back
      // to "creating" in place, so the open runs again into it (and its line
      // shows the attempt) instead of the workspace vanishing. A machine's
      // record carries its request id and is replayed by the open itself.
      set((s) => ({
        workspaces: updateTeamKeyed(
          s.workspaces,
          teamKey,
          readTeamKeyed(s.workspaces, teamKey, []).map((w) =>
            w.id === workspaceId
              ? withSlots(
                  { ...w, status: "creating" as const, statusError: undefined },
                  { base: { kind: "step1", phase: "starting" } },
                )
              : w,
          ),
        ),
      }));
    }
    try {
      await get().ensureWorktree(teamKey, storyFilePath, undefined, undefined, {
        onCreating: () => get().switchWorkspace(teamKey, workspaceId),
        // A cancel the person didn't make (they would close it) must not
        // tear the record down: it goes back to its failure line below.
        keepRecordOnCancel: true,
      });
    } catch (e) {
      logger.warn("viewer", `retryWorkspaceCreate(${workspaceId}) failed: ${e}`);
      // Never a silent vanish: a failure the open did not already put on the
      // workspace (one before it reached the record) lands on its line here.
      const failedStep = (e as { failedStep?: string }).failedStep;
      const still = readTeamKeyed(get().workspaces, teamKey, []).find((w) => w.id === workspaceId);
      if (failedStep === "cancelled") {
        // A cancelled retry leaves the workspace as it was before (its failure
        // line, with Retry) — never stuck "creating". A closed one (no record)
        // stays closed.
        if (still && still.status === "creating")
          set((s) => ({
            workspaces: updateTeamKeyed(
              s.workspaces,
              teamKey,
              readTeamKeyed(s.workspaces, teamKey, []).map((w) =>
                w.id === workspaceId ? prior : w,
              ),
            ),
          }));
        return;
      }
      if (still && still.status === "creating")
        updateWorkspaceSetup(set, teamKey, workspaceId, {
          status: "error",
          statusError: getErrorMessage(e),
        });
    }
  },

  clearSyncOverlay: (teamKey, workspaceId) => {
    set((s) => {
      const list = s.workspaces[teamKey];
      if (!list) return s;
      let changed = false;
      const next = list.map((w) => {
        if (w.id !== workspaceId || !w.needsSyncOverlay) return w;
        changed = true;
        const { needsSyncOverlay: _drop, ...rest } = w;
        return rest;
      });
      return changed ? { workspaces: updateTeamKeyed(s.workspaces, teamKey, next) } : s;
    });
  },

  openWorkspace: (params) => {
    const {
      teamKey,
      workspaceId,
      kind,
      cwd,
      recordFields,
      minimized,
      restoreSoftClosed,
      focus,
      spawn,
    } = params;

    if (minimized) {
      // BORN-MINIMIZED shadow spine. If it's already LIVE (the user opened it from
      // the tray), leave it — just keep the cwd current. Don't yank an open
      // workspace back to minimized.
      const liveList = readTeamKeyed(get().workspaces, teamKey, []);
      if (liveList.some((w) => w.id === workspaceId)) {
        set((s) => {
          const list = readTeamKeyed(s.workspaces, teamKey, []);
          const next = list.map((w) => (w.id === workspaceId && w.cwd !== cwd ? { ...w, cwd } : w));
          return { workspaces: updateTeamKeyed(s.workspaces, teamKey, next) };
        });
        return;
      }
      // Create the workspace directly in the soft-close shadow (a tray chip),
      // never as a foreground tile — so the xterm never mounts. The launcher
      // opens the owned agent tab in it (SW-GS-2d / SW-SN-7).
      set((s) => {
        const existing = s.softClosedWorkspaces[teamKey]?.[workspaceId];
        const workspace: Workspace = {
          id: workspaceId,
          kind,
          cwd,
          status: "ready",
          ...recordFields,
        };
        return {
          softClosedWorkspaces: patchSoftClosed(s.softClosedWorkspaces, teamKey, workspaceId, {
            ...existing,
            workspace,
          }),
        };
      });
      return;
    }

    // FOREGROUND spine: restore-if-soft-closed → focus-if-exists (stamping a
    // now-resolved cwd) → else add → switch/expand/spawn.
    // SW-FX-69: re-opening an ALREADY-LIVE tile is the "focus" half of the
    // openOrFocus contract (nav re-click, tray re-open) — always bring it to
    // front, even under the additive default. Only a brand-new tile is additive.
    const wasAlreadyOpen = readTeamKeyed(get().workspaces, teamKey, []).some(
      (w) => w.id === workspaceId,
    );
    if (restoreSoftClosed && get().softClosedWorkspaces[teamKey]?.[workspaceId]) {
      get().reopenSoftClosedWorkspace(teamKey, workspaceId);
    }
    set((s) => {
      const list = readTeamKeyed(s.workspaces, teamKey, []);
      const existing = list.find((w) => w.id === workspaceId);
      if (existing) {
        // SW-EW-7 (review F1, re-open path): a workspace created before its dir
        // resolved carries cwd "". Now that the caller supplies it, stamp it
        // (creating → ready) so the terminal spawns at the real dir, not the
        // stale "" (`ShellTerminalPanel` reads the stored cwd, and `cwd ?? repoPath`
        // doesn't fall back on a non-nullish empty string).
        if (cwd && existing.cwd !== cwd) {
          const next = list.map((w) =>
            w.id === workspaceId ? { ...w, cwd, status: "ready" as const, ...recordFields } : w,
          );
          return { workspaces: updateTeamKeyed(s.workspaces, teamKey, next) };
        }
        return s;
      }
      const ws: Workspace = {
        id: workspaceId,
        kind,
        cwd,
        // A resolved cwd is born "ready"; an empty cwd is born "creating" so the
        // inline overlay shows until a later open supplies the dir (epic pre-dir).
        status: cwd ? "ready" : "creating",
        ...recordFields,
      };
      return { workspaces: updateTeamKeyed(s.workspaces, teamKey, [...list, ws]) };
    });
    // SW-EW-7 (review F1): only spawn once the cwd is resolved — an empty cwd
    // stays "creating" and the shell launches on a later open once it arrives.
    // SW-FX-69: an EXPLICIT `focus` (true/false) always wins; only the unset
    // default becomes "focus a re-opened tile" (the openOrFocus contract).
    finishForegroundOpen(get, teamKey, workspaceId, {
      spawn: cwd !== "" && spawn !== false,
      focus: focus ?? wasAlreadyOpen,
    });
  },

  openOrFocusEpicWorkspace: (teamKey, epicDirName, sourceRepoSlug, cwd, epicDocPath) => {
    if (!epicDirName) return;
    // SW-EW-2: an epic-level integration terminal (no `sourceRepoSlug`) is ONE
    // row per epic; a slug-scoped variant (id includes the slug) is still
    // supported for a per-repo terminal.
    // SW-EW-15: the epic-level id is keyed on the epic DOC PATH (unique by
    // construction), not the dir leaf — two epics with the same leaf no longer
    // collide on the FE id ("open one, focus the other"). `epicDirPath` stays
    // the leaf for display/color/tree-matching. Falls back to the leaf only for
    // legacy/test callers that don't supply a doc path.
    const id = sourceRepoSlug
      ? `integration:${epicDirName}:${sourceRepoSlug}`
      : `integration:${epicDocPath ?? epicDirName}`;
    // SW-EW-15: the epic dir is AUTHORITATIVE from the backend handle — the FE
    // never derives `{org_root}/_epic/{leaf}` (it can't know the parent-
    // disambiguated `{parent}__{leaf}` id). The caller passes the real dir as
    // `cwd`; absent it, the row stays "creating" until a later open supplies it.
    const epicDir = cwd ?? "";
    // SLW-CO-8 (LD-E): the epic overlay now rides the WHOLE parallel multi-repo
    // batch (inheritance + worktree create + dev-env install), keyed off the
    // epic-scoped `epic-assembly-store` aggregate — NOT a single wave. Set the
    // `needsSyncOverlay` latch on a fresh open ONLY: `openWorkspace`'s
    // focus-existing branch re-applies `recordFields`, so latching
    // unconditionally would re-cover a live epic terminal on a plain re-focus.
    // (`openEpicTerminal` already guards re-open with `isAlreadyOpen → focus`, so
    // this is defense in depth for any other caller.)
    const alreadyOpen = readTeamKeyed(get().workspaces, teamKey, []).some((w) => w.id === id);
    get().openWorkspace({
      teamKey,
      workspaceId: id,
      kind: "epic",
      cwd: epicDir,
      recordFields: {
        epicDirPath: epicDirName,
        epicWorkspaceDir: epicDir || undefined,
        // SW-EW-15: the slug-less integration workspace's own epic doc path —
        // `resolveEpicDocPath` reads this first so repo read/write works without
        // a nav-tree entry.
        epicDocPath,
        sourceRepoSlug,
        // SLW-CO-8: overlay owns the epic's whole multi-repo assembly. The
        // aggregate batch state (`useEpicBatchStatus`) — not a single
        // `EpicInheritance` wave — is what the overlay-visibility gate reads; the
        // one-shot clear + start-watchdog handle the two-wave ordering + the
        // no-wave build-error path CO-7 deferred here.
        ...(alreadyOpen ? {} : { needsSyncOverlay: true as const }),
        // NB: `sourceRepoPath` is intentionally NOT set on an integration
        // workspace (D3) — it is the story-cleanup field (`removeWorktree`),
        // which doesn't apply here; integration teardown is `reconcileEpicWorktrees`.
      },
      // Epic preserves its no-soft-close-restore behavior (AC-8).
      // SW-FX-69: an epic reveal is a "go-to this workspace" gesture — focus it
      // (only the additive story-worktree open defaults to no-steal).
      focus: true,
    });
    // SW-DX-11 §2: register the epic terminal's session under its `epicDocPath`
    // handle in the trusted launch registry (CAP-FREE — the user opened this, not
    // a dashboard). This makes a dashboard hosted INSIDE the epic able to address
    // the already-running agent via `sendToSession(target: epicDocPath)` — the
    // doc supplies only the opaque handle; the trusted side owns the map, and the
    // generalized `resolve()` finds it through the SAME lookup as a story. SW-GW-8
    // (AC-5): the user opened this epic terminal from the CodeSense nav, not a
    // dashboard → provenance is "codesense".
    if (epicDocPath) registerWorkspaceSession(get, teamKey, id, epicDocPath);
  },

  // SW-GW-1: open (or focus) a generic CLI workspace at `dirPath` — the "open a
  // terminal anywhere" kind. NOT a singleton: the id is minted per normalized
  // dir, so re-opening the same dir focuses the existing record (AC-5) while a
  // different dir gets its own. No provision — the dir already exists;
  // `openWorkspace` is synchronous so the tile paints before the first spawn
  // (no UI block). `restoreSoftClosed` reopens it from a soft-close shadow.
  openOrFocusGenericWorkspace: (teamKey, dirPath) => {
    if (!dirPath) return;
    const cwd = normalizeWorkspaceDir(dirPath);
    const workspaceId = genericWorkspaceId(cwd);
    get().openWorkspace({
      teamKey,
      workspaceId,
      kind: "generic",
      cwd,
      restoreSoftClosed: true,
      focus: true, // SW-FX-69: opening a terminal at a chosen dir jumps to it
    });
    // SW-GW-8 (AC-1): fold this nav-opened generic dir session into the trusted
    // registry (under its `cwd` handle — the dashboard `dirHandle`) so it shows in
    // the Background agents panel, same as a nav-opened story.
    registerNavWorkspaceSession(get, teamKey, workspaceId);
  },

  // The canonical team folder exists independently of source repositories.
  // The generic open path preserves directory identity, deduplication and
  // soft-close restoration. Resolution only reads discovered configuration;
  // it performs no filesystem work, worktree provisioning or process launch.
  openOrFocusTeamFolderWorkspace: (teamKey) => {
    const state = useOrgStore.getState();
    const content = resolveTeamContentContext(teamKey, state.orgs, state.discoveryComplete);
    if (content.kind !== "ready") return;
    get().openOrFocusGenericWorkspace(teamKey, content.source.teamContentPath);
  },

  removeTerminalTabBySession: (sessionId, opts) => {
    const home = selectSessionHome(get(), sessionId);
    if (home) {
      return get().removeTerminalTab(home.teamKey, home.tab.id, home.workspaceId, {
        kill: opts?.kill,
        replace: false,
        throwOnError: true,
      });
    }
    // Not shown (yet): its host still knows the session — a tab whose open has
    // not been announced, or an entry out of sight.
    return terminalCloseTab(sessionId, opts?.kill ? "kill" : "keepIfBusy").then(() => undefined);
  },

  hideTerminalTab: (teamKey, tabId, workspaceId) => {
    if (hidingSessions.has(tabId)) return true;
    const s = get();
    // Tabs already on their way out (hidden or closed) are gone for this count,
    // so two quick hides never leave the tile with an empty strip.
    const tabs = selectWorkspaceTabs(s, teamKey, workspaceId).filter(
      (t) => !hidingSessions.has(t.id) && !closingSessions.has(t.id),
    );
    if (!tabs.some((t) => t.id === tabId) || tabs.length < 2) return false;
    const key = selectWorkspaceKey(s, teamKey, workspaceId);
    // Marked as the user's Hidden tab (never a headless launch's entry), then
    // its host moves it to the workspace's Background entries (off the strip
    // and out of the grid, the tile's focus moving on) — still running.
    const done = terminalSetTags(tabId, { [USER_HIDDEN_TAG]: "tab" })
      .then(() => terminalPlaceTab(tabId, key, "background"))
      .catch(logLayoutVerb("hide terminal tab"))
      .finally(() => hidingSessions.delete(tabId));
    hidingSessions.set(tabId, { key, done });
    return true;
  },

  restoreHiddenTerminalTab: (teamKey, workspaceId, tabId) => {
    if (!selectHiddenTerminalTabs(get(), teamKey, workspaceId).some((t) => t.id === tabId)) return;
    // The tile itself may be hidden in the tray — bring it back first.
    if (get().softClosedWorkspaces[teamKey]?.[workspaceId]) {
      get().reopenSoftClosedWorkspace(teamKey, workspaceId);
    }
    if (!get().workspaces[teamKey]?.some((w) => w.id === workspaceId)) return;
    void get().adoptTerminalSession(teamKey, tabId, workspaceId);
    get().switchWorkspace(teamKey, workspaceId);
    get().expandTerminalOnOpen(teamKey);
  },

  closeWorkspace: (teamKey, workspaceId, opts) => {
    // T.2 D-11: root non-closability is now `kind`-based (was a literal
    // id match against ROOT_WORKSPACE_ID). The bypass flag is only set by
    // `ensureRootWorkspaces` when removing a root whose source repo dropped
    // from `workspaceRepos`; user-facing callers never set it.
    const wsHandle = get().workspaces[teamKey]?.find((w) => w.id === workspaceId);
    if (wsHandle && !getKindCapabilities(wsHandle.kind).closableByUser && !opts?.bypassRootGuard) {
      logger.debug("viewer", "closeWorkspace: ignored (kind is not user-closable)");
      return;
    }
    // Abort any in-flight worktree setup for this workspace BEFORE
    // tearing down state — otherwise the success branch races against
    // the deletion and writes phantom terminal tabs to a workspace
    // that no longer exists.
    // SW-SO-1: abort + evict any in-flight open for this id (one shared teardown,
    // see `abortInFlightWorktreeOpen`) so an immediate reopen after an in-tile
    // Cancel of a `creating` record runs fresh instead of piggybacking the doomed
    // promise (dead click).
    abortInFlightWorktreeOpen(workspaceAbortKey(teamKey, workspaceId));
    // SW-SO-1 (review r1): a record teardown also clears any lingering pre-record
    // "opening" flag for this id. The teardown above dropped the abort handle, so
    // the aborted open's ownership-guarded `finally` skips its own flag clear —
    // without this the flag would outlive the record and `resolveOpeningWorkspaceId`
    // would resurrect the overlay over a workspace that no longer exists.
    // Idempotent (no-op when unset).
    get().endWorkspaceOpen(teamKey, workspaceId);
    // A closing tile takes its Hidden tabs with it, treated like its other tabs
    // below — back on the tile in the tray (Hide) or killed (End).
    const closingHidden = selectHiddenTerminalTabs(get(), teamKey, workspaceId);
    foldHiddenTabsIntoTile(get(), teamKey, workspaceId);
    // PT-10: × = "close, keep running" (overrides SW-EW-1 AC-8). Do NOT kill —
    // the viewport panels unmount → detach (PT-1 unsubscribe) and the terminals
    // keep running in their host, whose layout keeps their tabs. Stash the
    // workspace record as a minimized shadow so the panel renders a dimmed row
    // and any reopen path can restore + reattach. EXCEPTION: an internal root
    // teardown (`bypassRootGuard` — source repo dropped) has no shadow home, so
    // its terminals are closed rather than leaked.
    const wsForShadow = get().workspaces[teamKey]?.find((w) => w.id === workspaceId);
    const dyingTabs = [...selectWorkspaceTabs(get(), teamKey, workspaceId), ...closingHidden];
    const dyingSessionIds = dyingTabs.map((t) => t.sessionId);
    // `killTerminals` (End) and the internal root teardown both opt OUT of
    // keep-running: they reap the PTYs and skip the shadow/toast.
    const keepRunning =
      dyingTabs.length > 0 &&
      !opts?.bypassRootGuard &&
      !opts?.killTerminals &&
      wsForShadow !== undefined;
    if (!keepRunning && dyingTabs.length > 0) {
      killTerminalTabs(dyingTabs, "close workspace terminals", {
        endRemote: opts?.killTerminals === true && opts.endRemote === true,
      });
    }
    // Its tab-grid sizes go with its terminals; a minimized tile keeps them.
    if (!keepRunning) {
      const key = selectWorkspaceKey(get(), teamKey, workspaceId);
      if (key in get().tabGridLayout) {
        const { [key]: _closed, ...rest } = get().tabGridLayout;
        set({ tabGridLayout: rest });
      }
    }
    let reassignedActiveId: string | undefined;
    set((s) => {
      const ws = readTeamKeyed(s.workspaces, teamKey, []);
      const idx = ws.findIndex((w) => w.id === workspaceId);
      if (idx === -1) return s;

      const nextWs = ws.filter((w) => w.id !== workspaceId);
      const wasActive = s.activeWorkspace[teamKey] === workspaceId;
      const nextWorkspaces = updateTeamKeyed(s.workspaces, teamKey, nextWs);
      // SW-CW-4: closing the active tab lands on the tab to its RIGHT in the full
      // on-screen strip (falling left when it was the rightmost — browser
      // convention), not an arbitrary first-root jump. Eligible neighbors are the
      // surviving live workspaces (`nextWs`) —
      // that both skips parked/minimized tiles (they're not in `nextWs`) and
      // guarantees the picked id still backs a live workspace even if the
      // published order is stale (a prior close not yet re-published). Falls back
      // to the first root when no eligible neighbor remains or the order isn't
      // published yet (headless/test).
      const survivingIds = new Set(nextWs.map((w) => w.id));
      const nextActive = wasActive
        ? updateTeamKeyed(
            s.activeWorkspace,
            teamKey,
            adjacentWorkspaceIdAfterClose(
              s.trayFullOrderByTeam[teamKey],
              workspaceId,
              (id) => survivingIds.has(id),
              firstRootIdForTeam(teamKey, nextWorkspaces),
            ),
          )
        : s.activeWorkspace;
      if (wasActive) reassignedActiveId = nextActive[teamKey];

      // PT-10 / SW-EW-28: clear this workspace's per-workspace slices and (when
      // keepRunning) stash the closed-but-running shadow — shared with
      // replaceFocusedLeafWorkspace so the two stay byte-identical.
      const softClose = captureSoftCloseShadow(
        s,
        teamKey,
        workspaceId,
        wsForShadow ?? ws[idx],
        keepRunning,
      );

      // Expanded follows the focus a close moves to (`nextActive`, the deterministic
      // neighbour): it always zooms the focused workspace, so nothing is patched here.

      // Full-screen CLI is never left on the user's behalf: a close that empties the CLI
      // keeps it, on the empty state (whose Agents panel and New terminal start work again).
      // Only a user action leaves full screen (its toggle, collapsing the CLI, or opening
      // a file that needs the pane).
      return {
        workspaces: nextWorkspaces,
        activeWorkspace: nextActive,
        ...softClose,
        // Closed for good (no minimized shadow): it leaves the team's order, and the base order
        // the tray last saw, too.
        ...(keepRunning
          ? {}
          : {
              workspaceOrderByTeam: orderWithoutWorkspace(
                s.workspaceOrderByTeam,
                teamKey,
                workspaceId,
              ),
              trayBaseOrderByTeam: orderWithoutWorkspace(
                s.trayBaseOrderByTeam,
                teamKey,
                workspaceId,
              ),
            }),
        // SW-FX-52: on the REAP path (kill), prune the killed tabs' status
        // entries — don't rely solely on `pty-exit`. keepRunning KEEPS them (the
        // shadow card shows live status).
        ...(keepRunning
          ? {}
          : {
              aiStatusBySession: pruneSessionMap(s.aiStatusBySession, dyingSessionIds),
              // SW-AM-4: same reap → prune the progress slice in lockstep.
              progressBySession: pruneSessionMap(s.progressBySession, dyingSessionIds),
              progressTickBySession: pruneSessionMap(s.progressTickBySession, dyingSessionIds),
            }),
      };
    });
    if (!keepRunning) pruneAgentSessions(dyingSessionIds);
    // Hiding a workspace with live terminals is silent: the workspace leaves the
    // tiles and appears as a dimmed tray chip (SW-EW-11), the durable "still
    // running" indicator. The chip's click restores it and its menu's End covers
    // what the old PT-10 Model-C toast offered — so there is no toast.
    // Flush immediately so workspace closure reaches disk before a
    // potential app-close; the 300ms debounce is too risky for this state.
    get().flushPersist();
    // SW-WL-3 AC-1: release this workspace's surface-bound watcher (no-op if
    // it was never opened, e.g. a root that was minted but never activated).
    get().closeWorkspaceWatch(teamKey, workspaceId);
    // SW-WL-3: closing the active surface reassigns selection to the left
    // neighbor (SW-CW-4), or the first root as the fallback. That surface may not
    // have its watcher attached yet — a root minted-but-never-activated (F-5), or
    // a neighbor whose watch was released — so open it (gated on a resolvable cwd)
    // so the surface the viewer now renders is watched. Idempotent if already open.
    if (reassignedActiveId) get().openActiveSurfaceWatch(teamKey, reassignedActiveId);
  },

  openWorkspaceWatch: (teamKey, workspaceId) => {
    // Membership semantics: already-open is a no-op so the activation call
    // sites (switchWorkspace, ensureRootWorkspaces, F.18 restore) can fire
    // freely without leaking an undrained refcount.
    if ((get().workspaceOpenSet[teamKey]?.[workspaceId] ?? 0) > 0) return;
    set((s) => ({
      workspaceOpenSet: {
        ...s.workspaceOpenSet,
        [teamKey]: { ...readTeamKeyed(s.workspaceOpenSet, teamKey, {}), [workspaceId]: 1 },
      },
    }));
    fireWorkspaceOpened(teamKey, workspaceId); // 0→1
  },

  openActiveSurfaceWatch: (teamKey, workspaceId) => {
    const ws = readTeamKeyed(get().workspaces, teamKey, []).find((w) => w.id === workspaceId);
    if (ws?.cwd) get().openWorkspaceWatch(teamKey, workspaceId);
  },

  closeWorkspaceWatch: (teamKey, workspaceId) => {
    if ((get().workspaceOpenSet[teamKey]?.[workspaceId] ?? 0) === 0) return;
    set((s) => {
      const inner = { ...readTeamKeyed(s.workspaceOpenSet, teamKey, {}) };
      delete inner[workspaceId];
      const next = { ...s.workspaceOpenSet };
      if (Object.keys(inner).length === 0) delete next[teamKey];
      else next[teamKey] = inner;
      return { workspaceOpenSet: next };
    });
    fireWorkspaceClosed(teamKey, workspaceId); // 1→0
  },

  closeTeam: (teamKey, opts = {}) => {
    // Tear down ALL team-keyed slices + PTYs + abort controllers for a
    // team in a single atomic set(). Without this expansion, switching
    // teams (or evicting suspended ones) would leak ~24 buckets of stale
    // state per old team into memory forever.
    if (teamKey === null) return; // cold-start: nothing to tear down

    // A move made from the team's tray is not repeated when it opens again.
    clearTrayMove(teamKey);

    // SW-WL-3 AC-1 / F-5: team-switch detaches every open watch for the team
    // we're leaving — "cold launch / team-switch are free until I open
    // something" only holds if the prior team's surfaces release here.
    for (const wsId of Object.keys(readTeamKeyed(get().workspaceOpenSet, teamKey, {}))) {
      get().closeWorkspaceWatch(teamKey, wsId);
    }

    // 1. Abort in-flight worktree setups for this team. The abort key
    //    shape is `${teamKey}::${workspaceId}` post-T.1 (workspaceAbortKey).
    //    SW-SO-1 (review r3): route through the shared teardown so the dedup
    //    entry is evicted too — else a reopen after a mid-probe team switch
    //    piggybacks the doomed promise (dead click), the same class r1/r2 fixed
    //    for the Cancel paths. (`openingWorkspaces` is swept separately below via
    //    TEAM_KEYED_SLICES.)
    for (const key of Array.from(storyWorkspaceAborts.keys())) {
      if (key.startsWith(`${teamKey}::`)) {
        abortInFlightWorktreeOpen(key);
      }
    }
    // T.4 Phase 11 (Round 3 MED — mid-flight guard fix): bump the team's
    // close epoch so any in-flight ensureWorktree captures-then-checks
    // the value, skipping its workspace write if closeTeam fired while
    // setupWorktree was running. Pairs with the capture+check in
    // `ensureWorktree` (see comments near the addStoryWorkspace site).
    teamCloseEpoch.set(teamKey, (teamCloseEpoch.get(teamKey) ?? 0) + 1);

    // 2. End every terminal of this team's tiles — open, minimized into the
    //    tray, or Hidden under a chip — on explicit close. Age eviction keeps
    //    the sessions and only retains workspace metadata for sessions that
    //    can be reattached when the team is opened again.
    const teamSessionIds: string[] = [];
    const s0 = get();
    const tileKeys = new Set(
      [
        ...readTeamKeyed(s0.workspaces, teamKey, []),
        ...Object.values(s0.softClosedWorkspaces[teamKey] ?? {}).map((shadow) => shadow.workspace),
      ].map((ws) => workspaceKey(teamKey, ws)),
    );
    for (const [key, layout] of Object.entries(s0.terminalLayout.byKey)) {
      if (parseWorkspaceKey(key)?.teamKey !== teamKey) continue;
      const ending = tileKeys.has(key) ? [...layout.tabs, ...layout.hiddenTabs] : layout.tabs;
      teamSessionIds.push(...ending.map((t) => t.sessionId));
      if (!opts.preserveTerminalSessions) {
        killTerminalTabs(ending, "close team terminals");
      }
    }
    // These flat slices are not team-keyed. Clear them only when the sessions
    // are ended; an age eviction must preserve status and progress for live PTYs.
    if (!opts.preserveTerminalSessions) pruneAgentSessions(teamSessionIds);

    const workspaceKeysWithSessions = opts.preserveTerminalSessions
      ? new Set(
          Object.entries(s0.terminalLayout.byKey)
            .filter(
              ([key, layout]) =>
                parseWorkspaceKey(key)?.teamKey === teamKey &&
                (layout.tabs.length > 0 || layout.hiddenTabs.length > 0),
            )
            .map(([key]) => key),
        )
      : null;
    const retainedWorkspaces = workspaceKeysWithSessions
      ? (s0.workspaces[teamKey] ?? []).filter((workspace) =>
          workspaceKeysWithSessions.has(workspaceKey(teamKey, workspace)),
        )
      : undefined;
    const retainedSoftClosed = workspaceKeysWithSessions
      ? Object.fromEntries(
          Object.entries(s0.softClosedWorkspaces[teamKey] ?? {}).filter(([, shadow]) =>
            workspaceKeysWithSessions.has(workspaceKey(teamKey, shadow.workspace)),
          ),
        )
      : undefined;
    const retainedWorkspaceIds = new Set([
      ...(retainedWorkspaces ?? []).map((workspace) => workspace.id),
      ...Object.keys(retainedSoftClosed ?? {}),
    ]);
    // RF-perf-14 (decouple): a background `setup:` handle owns both the
    // running job and a failed badge's retry thunk. Keep that owner when its
    // workspace is retained with a live terminal; abort only jobs whose
    // workspace is being dropped. Explicit close still drops every handle.
    for (const key of Array.from(backgroundSetupHandles.keys())) {
      if (!key.startsWith(`${teamKey}::`)) continue;
      const workspaceId = key.slice(teamKey.length + 2);
      if (!opts.preserveTerminalSessions || !retainedWorkspaceIds.has(workspaceId)) {
        abortInFlightWorktreeOpen(key);
      }
    }
    const retainedActiveWorkspace = retainedWorkspaceIds.has(s0.activeWorkspace[teamKey] ?? "")
      ? s0.activeWorkspace[teamKey]
      : undefined;

    // 3. T.4 Phase 10 review (Round 2 LOW): sweep `cleanupInFlight` for
    //    entries with the closing team's `${teamKey}::` prefix so in-flight
    //    cleanups whose team got closed don't leak Set entries forever.
    const teamPrefix = `${teamKey}::`;
    for (const key of Array.from(cleanupInFlight)) {
      if (key.startsWith(teamPrefix)) cleanupInFlight.delete(key);
    }

    // 4. Delete every team-keyed slice entry for this teamKey in a
    //    single set() so observers don't see an intermediate state.
    //    T.4 AC-3/D-8: also clear identity scalars when their teamKey
    //    matches the closing team. T.4 Phase 10 review: compute the
    //    scalar-clear booleans INSIDE the set callback (read from `s`,
    //    not stale `get()`) so a concurrent dismiss/show between get
    //    and set can't desync the decision.
    //    Phase 10: noClonePromptRetry is also cleared if it survives
    //    without a noClonePromptRepo — the retry is per-team, so when
    //    the team teardown clears the repo it must clear the retry too
    //    (defense-in-depth against an orphan-retry window).
    // SW-WT-9: if a setup-approval gate is open for the closing team, resolve
    // its awaiting `setupWorktree` as declined (skip) before the modal unmounts
    // with the team view — otherwise the promise orphans and the setup flow
    // hangs forever. The state itself is cleared in the set() below.
    if (get().worktreeSetupApproval?.teamKey === teamKey) {
      pendingSetupApprovalResolve?.(false);
      pendingSetupApprovalResolve = null;
    }

    tileOrderCache.delete(teamKey);
    provisionalMoves.delete(teamKey);
    // The team's published pipeline tree goes with it.
    clearTeamTree(teamKey);
    set((s) => {
      const clearSetupApproval = s.worktreeSetupApproval?.teamKey === teamKey;
      const clearNoClonePrompt = s.noClonePromptRepo?.teamKey === teamKey;
      // Conservative: if the prompt repo is null but a retry callback
      // lingers, clear it on team teardown — the retry was for a
      // specific team's no-clone flow and can never fire correctly
      // post-teardown anyway.
      const clearOrphanRetry = s.noClonePromptRepo === null && s.noClonePromptRetry !== null;
      // T.6: remove the team from `openedTeams` so the workbench's mount loop
      // no longer renders a `<TeamWorkspaceView>` for it.
      const openedTeams = s.openedTeams.filter((tk) => tk !== teamKey);
      return {
        ...deleteFromKeyedSlices(s, teamKey, TEAM_KEYED_SLICES),
        ...(opts.preserveTerminalSessions
          ? {
              workspaces: replaceTeamBucket(s.workspaces, teamKey, retainedWorkspaces),
              activeWorkspace: replaceTeamBucket(
                s.activeWorkspace,
                teamKey,
                retainedActiveWorkspace,
              ),
              softClosedWorkspaces: replaceTeamBucket(
                s.softClosedWorkspaces,
                teamKey,
                retainedSoftClosed,
              ),
            }
          : {}),
        ...agentsPaneWithoutTeam(s, teamKey),
        ...(!opts.preserveTerminalSessions
          ? { aiStatusBySession: pruneSessionMap(s.aiStatusBySession, teamSessionIds) }
          : {}),
        // Progress and its idle-detection tick are retained with live sessions.
        ...(!opts.preserveTerminalSessions
          ? {
              progressBySession: pruneSessionMap(s.progressBySession, teamSessionIds),
              progressTickBySession: pruneSessionMap(s.progressTickBySession, teamSessionIds),
            }
          : {}),
        // RF-perf-12: `viewStateByKey` is a FLAT map (composed `${teamKey}::…`
        // keys), so it can't ride the `TEAM_KEYED_SLICES` sweep — prune its
        // nav + per-file snapshots for the closing team here (reference-stable).
        viewStateByKey: pruneViewStateForTeam(s.viewStateByKey, teamKey),
        ...(openedTeams.length !== s.openedTeams.length ? { openedTeams } : {}),
        // SW-WG-9: `maximizedPanel` is team-keyed and rides `TEAM_KEYED_SLICES`, so
        // the `deleteFromKeyedSlices` sweep above already clears the closing team's
        // maximize — no bespoke handling needed (was four per-scalar clears).
        ...(clearSetupApproval ? { worktreeSetupApproval: null } : {}),
        ...(clearNoClonePrompt
          ? { noClonePromptRepo: null, noClonePromptRetry: null }
          : clearOrphanRetry
            ? { noClonePromptRetry: null }
            : {}),
      };
    });

    // 5. Flush persistence immediately — team teardown is decisive state
    //    and the 300ms debounce is too risky for app-close races.
    get().flushPersist();
  },

  rekeyTeam: (orgSlug, oldName, newName) => {
    if (oldName === newName) return;
    const oldKey = buildTeamKey(orgSlug, oldName);
    const newKey = buildTeamKey(orgSlug, newName);
    teamCloseEpoch.set(oldKey, teamLifecycleVersion(oldKey) + 1);
    teamCloseEpoch.set(newKey, teamLifecycleVersion(newKey) + 1);
    // The team's terminals follow it: every host key carries the team.
    const before = get();
    for (const key of Object.keys(before.terminalLayout.byKey)) {
      const parsed = parseWorkspaceKey(key);
      if (parsed?.teamKey !== oldKey) continue;
      moveWorkspaceTerminals(before, key, workspaceKey(newKey, { id: parsed.form }), (d) => ({
        ...d,
        team: newName,
      }));
    }
    // The module's own per-team state: the order cache is rebuilt under the new key; moves queued
    // before the team's tree synced wait for it under the new key.
    tileOrderCache.delete(oldKey);
    tileOrderCache.delete(newKey);
    const queued = provisionalMoves.get(oldKey);
    provisionalMoves.delete(oldKey);
    if (queued) provisionalMoves.set(newKey, queued);
    // The published pipeline tree follows the team until its view publishes again.
    renameTeamTree(oldKey, newKey);
    set((s) => {
      const moved = rekeyKeyedSlices(s, oldKey, newKey, TEAM_KEYED_SLICES);
      // `openedTeams` is a plain teamKey array (the workbench's mount loop) — rewrite
      // the entry so the renamed team stays mounted under its new key.
      const openedTeams = s.openedTeams.includes(oldKey)
        ? s.openedTeams.map((tk) => (tk === oldKey ? newKey : tk))
        : s.openedTeams;

      // The outer-key move relocates the slices, but path-bearing ones hold
      // ABSOLUTE content paths (`{org_root}/{team}/content/...`, flat per
      // SLW-IN-7), so an open story tab would still point at the moved-away OLD
      // dir and render a "file deleted" placeholder. Rewrite the flat `/{old}/`
      // team segment → new so open tabs / drill / nav genuinely survive the
      // rename (AC-7). Anchored on the following layer marker; source-repo file
      // tabs carry no such segment, so the swap is safe and a no-op for them.
      const pathFixes = rewriteTeamContentPaths(moved, newKey, oldName, newName);

      return {
        ...moved,
        ...pathFixes,
        ...(openedTeams !== s.openedTeams ? { openedTeams } : {}),
        // The pane's folds, hidden teams, arrow filter and funnel restore point follow the team.
        ...agentsPaneRekeyed(s, oldKey, newKey),
        // SW-WG-9: `maximizedPanel` rides `TEAM_KEYED_SLICES`, so the
        // `rekeyKeyedSlices` move above already relocates a renamed team's maximize
        // to the new key — no bespoke rewrite needed (was four per-scalar rewrites).
      };
    });
    // Decisive state (like closeTeam) — don't leave the re-key to the debounce.
    get().flushPersist();
  },

  cleanupStoryWorktree: async (repoPath, storyFilePath) => {
    // Code-review P5: normalise at entry so both `w.storyFilePath` (already
    // normalised at write time by `addStoryWorkspace`) and `onDiskWorktrees`
    // Map composite keys (composed via normalised paths post-Phase-4) compare cleanly.
    storyFilePath = normalizeStoryFilePath(storyFilePath);
    // T.1: `workspaces` is team-keyed. T.4 Phase 4: `onDiskWorktrees` is
    // team-keyed with composite inner keys. T.4 Phase 8 review: derive
    // teamKey from the `repoPath` arg (not the active team) so cleanups
    // for stories that belong to a non-active team still land in the
    // right bucket. Legacy non-synthetic repos fall back to the active
    // team via `getActiveTeamKey()`. T.5 generalizes the function
    // signature to take teamKey directly.
    const parsed = parseSyntheticRepoKey(repoPath);
    const teamKey = parsed ? buildTeamKey(parsed.orgSlug, parsed.team) : getActiveTeamKey();
    const ws = readTeamKeyed(get().workspaces, teamKey, []).find(
      (w) => w.storyFilePath === storyFilePath && w.status === "ready",
    );
    // T.4 Phase 8 review: prefer the workspace entry's stored sourceRepoSlug
    // (M.3 field — written by `ensureWorktree` from frontmatter `source_repo`)
    // so the composite key matches what was originally written. Falls back to
    // deriving from `repoPath` for entries without the field (pre-M.3) and
    // for on-disk-only cleanups where no workspace entry exists.
    const sourceRepoSlug = ws?.sourceRepoSlug ?? deriveSourceRepoSlug(repoPath);
    const composite = composeWorktreeKey(sourceRepoSlug, storyFilePath);
    // T.4 Phase 8 review: cleanupInFlight key prefixes with teamKey (like
    // `composeTeamWorktreeKey`) so two teams sharing the same `sourceRepoSlug` +
    // `storyFilePath` don't block each other's cleanups.
    const inFlightKey = composeTeamWorktreeKey(teamKey, sourceRepoSlug, storyFilePath);
    if (cleanupInFlight.has(inFlightKey)) return;
    // Fall back to the on-disk map when no workspace entry exists
    // (e.g. workspace tab was closed but worktree still on disk).
    const onDiskEntry = get().onDiskWorktrees[teamKey]?.get(composite);
    const cwd = ws?.cwd ?? onDiskEntry?.path;
    if (!cwd) return;
    // M.2: prefer stored sourceRepoPath (cross-repo aware), fall back to
    // on-disk entry's sourceRepoPath, then context-repo resolution for
    // legacy entries without the field.
    const realPath =
      ws?.sourceRepoPath ?? onDiskEntry?.sourceRepoPath ?? resolveRealRepoPath(repoPath);
    if (realPath === null) {
      toast.error("No local clone attached — cannot remove worktree");
      return;
    }
    const remove = async (consent: { discardContext: boolean; discardCode: boolean }) => {
      if (cleanupInFlight.has(inFlightKey)) return;
      cleanupInFlight.add(inFlightKey);
      const toastId = toast.loading("Removing worktree...");
      try {
        // Routine cleanup preserves dirty code and ignored context files. "Remove
        // anyway" consents to discarding the context edits only; uncommitted code
        // needs its own, separate confirmation (SW-XS-4).
        await removeWorktree(realPath, cwd, consent.discardCode, consent.discardContext);
        // Worktree is gone from disk — clear the on-disk map entry.
        get().removeOnDiskWorktree(teamKey, sourceRepoSlug, storyFilePath);
        if (ws) {
          get().closeWorkspace(teamKey, ws.id);
        }
        toast.success("Worktree removed", { id: toastId });
      } catch (err) {
        toast.error(`Couldn't remove worktree: ${getErrorMessage(err)}`, { id: toastId });
      } finally {
        cleanupInFlight.delete(inFlightKey);
      }
    };
    // SW-XS-4 (M4): nothing publishes worktree context edits automatically any
    // more, so a removal that would discard unpublished ones says so (with the
    // count) and only proceeds on the user's explicit "Remove anyway".
    let unpublished: { count: number | null; error: string | null };
    try {
      const found = await getWorktreeUnpublishedContext(cwd);
      unpublished = { count: found.count, error: null };
    } catch (err) {
      unpublished = { count: null, error: getErrorMessage(err) };
    }
    if (unpublished.count !== 0) {
      // "Remove anyway" discards the context edits. If the worktree also has
      // uncommitted CODE changes, that is a second, explicit confirmation.
      const removeAnyway = async () => {
        let dirty: boolean | null;
        try {
          dirty = (await checkWorktreeDirty(cwd)).dirty;
        } catch {
          dirty = null;
        }
        if (dirty === false) {
          await remove({ discardContext: true, discardCode: false });
          return;
        }
        toast.warning(uncommittedCodeDiscardMessage(dirty), {
          id: `worktree-remove-uncommitted:${cwd}`,
          action: {
            label: "Discard code changes too",
            onClick: () => void remove({ discardContext: true, discardCode: true }),
          },
        });
      };
      toast.warning(removalDiscardMessage(unpublished.count, unpublished.error), {
        id: `worktree-remove-unpublished:${cwd}`,
        action: { label: "Remove anyway", onClick: () => void removeAnyway() },
      });
      return;
    }
    await remove({ discardContext: false, discardCode: false });
  },

  setOnDiskWorktrees: (teamKey, sourceRepoSlug, entries) => {
    // T.4 D-3: compose composite inner keys and preserve entries for OTHER
    // source repos in the same team bucket. The prefix scan is critical —
    // without it, scanning repo X would clobber repo Y's worktrees inside
    // the shared `[teamKey]` bucket.
    //
    // Code-review round-2 R3: incoming `entries` keys are raw scanner paths;
    // `composeWorktreeKey` normalises them as part of composition so
    // read-side normalised lookups in `cleanupStoryWorktree` etc. hit cleanly.
    //
    // Cleanup-race filter: `cleanupInFlight` is teamKey-prefixed composite
    // (Phase 8 review fix for cross-team slug collisions), so check by the
    // same key shape that `cleanupStoryWorktree` adds while in flight.
    const slug = sourceRepoSlug.toLowerCase();
    const prefix = `${slug}::`;
    set((s) => {
      const existing = s.onDiskWorktrees[teamKey];
      const next = new Map<string, OnDiskWorktreeEntry>();
      // Preserve entries for OTHER source repos in this team.
      if (existing) {
        for (const [k, v] of existing) {
          if (!k.startsWith(prefix)) next.set(k, v);
        }
      }
      // Compose new entries for THIS source repo.
      for (const [storyFilePath, entry] of entries) {
        const composite = composeWorktreeKey(slug, storyFilePath);
        if (cleanupInFlight.has(composeTeamWorktreeKey(teamKey, slug, storyFilePath))) continue;
        next.set(composite, entry);
      }
      return { onDiskWorktrees: updateTeamKeyed(s.onDiskWorktrees, teamKey, next) };
    });
  },

  removeOnDiskWorktree: (teamKey, sourceRepoSlug, storyFilePath) => {
    // T.4 D-3: composite-key delete from the team's bucket. No-op if the
    // teamKey bucket is absent or the composite key isn't present.
    const composite = composeWorktreeKey(sourceRepoSlug, storyFilePath);
    set((s) => {
      const prev = s.onDiskWorktrees[teamKey];
      if (!prev?.has(composite)) return s;
      const next = new Map(prev);
      next.delete(composite);
      return { onDiskWorktrees: updateTeamKeyed(s.onDiskWorktrees, teamKey, next) };
    });
  },

  cycleWorkspace: (teamKey, direction) => {
    const s = get();
    // Traverse the visible tray order (open ∪ minimized), so a minimized member is
    // reachable and restored via activateTrayWorkspace. In grouped mode the scope
    // narrows to the selected rail's slice — prev/next stays WITHIN the rail, while
    // moving BETWEEN rails is the explicit epic-nav command.
    //
    // Fall back to the raw open-workspace order when the tray hasn't published its
    // order yet (`trayFullOrderByTeam` is populated by the tray on tree change): a
    // user cycling before that first publish must still move, never silently no-op
    // with multiple workspaces open. Grouped scoping needs the published
    // railKeyByWorkspaceId, so the fallback is inherently the flat order.
    const published = readTeamKeyed(s.trayFullOrderByTeam, teamKey, []);
    const order =
      published.length > 0 ? published : readTeamKeyed(s.workspaces, teamKey, []).map((w) => w.id);
    let scope = order;
    if (published.length > 0 && s.trayMode[teamKey] === "grouped") {
      const railKeyById = readTeamKeyed(s.railKeyByWorkspaceId, teamKey, {});
      const selectedRailKey = resolveSelectedRailKey(
        order.map((id) => railKeyById[id] ?? ""),
        s.selectedEpicDirName[teamKey] ?? "",
      );
      scope = order.filter((id) => (railKeyById[id] ?? "") === selectedRailKey);
    }
    const activeId = activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
    const target = resolveAdjacentWorkspaceInScope(scope, activeId, direction);
    if (!target || target === activeId) return false;
    // Activate here (handles minimized→replace, on-screen→switch); the FOCUS half of
    // the tuple is applied by the caller (registry run() → focusActiveWorkspaceTerminal),
    // NOT here, because the store cannot import focus-cascade without a cycle
    // (focus-cascade imports the store). Same activate-then-focus split OC-5 established.
    return s.activateTrayWorkspace(teamKey, target);
  },

  jumpToRootWorkspace: (teamKey) => {
    // T.2 D-10: with N roots per team, "jump to root" lands on the first
    // root (workspaceRepos-order, which follows teams.yml.source_repos).
    const s = get();
    const rootId = firstRootIdForTeam(teamKey, s.workspaces);
    // switchWorkspace is not idempotent (it always re-renders), so detect the
    // already-at-root no-op here to report it as blocked.
    if (s.activeWorkspace[teamKey] === rootId) return false;
    s.switchWorkspace(teamKey, rootId);
    return true;
  },

  openOrFocusStoryWorkspace: async (
    teamKey,
    storyFilePath,
    expectedSlug?,
    overrideStartPoint?,
    focus?,
    spawnFirstTerminal = true,
    location?,
    isCurrent?,
  ) => {
    // T.5: signature re-keyed `repoPath` → `teamKey`. Source repo
    // resolution happens inside `ensureWorktree` from the story's
    // frontmatter; this entry point only needs the team bucket.

    // Code-review P5: normalise at entry so the workspaceId derivation +
    // downstream comparisons in switchWorkspace / terminal-tab indexing
    // all see the same byte representation.
    storyFilePath = normalizeStoryFilePath(storyFilePath);
    // R-18: identity key only — display label is resolved from frontmatter
    // inside `ensureWorktree` and applied to `Workspace.storyId`.
    const workspaceId = storyIdFromPath(storyFilePath);
    // SW-FX-69: capture BEFORE `ensureWorktree` (which may create the record) —
    // re-opening an already-live story tile is a "focus" gesture (nav re-click),
    // so the fast-path/on-disk tail must bring it to front. Only the fresh-create
    // path (which fires `onCreating`) is additive.
    const wasAlreadyOpen = readTeamKeyed(get().workspaces, teamKey, []).some(
      (w) => w.id === workspaceId,
    );
    // SW-FX-41: the resolved worktree cwd, returned so the create-from-default
    // delegate keeps its `Promise<string>` contract. It is a reliable "did we
    // actually open?" signal: "" whenever nothing was opened — a handled setup
    // error (swallow path) OR a concurrent-close ghost (tile torn down mid-
    // flight) — and the real cwd on a genuine open (incl. the soft-close reopen,
    // which DID re-open). The create-from-default handler keys its "Started
    // from …" success toast on a non-empty result.
    let resolvedCwd = "";

    // ensureWorktree handles store-hit (with symlink resync), on-disk
    // detection, config check, and creation.
    //
    // SLW-CO-5: the `onCreating` hook fires the moment the workspace record is
    // inserted as `status:"creating"` (before setup runs), so we switch to it +
    // reveal the terminal HERE — the panel and inline `WorkspaceInitOverlay`
    // appear immediately. `await` below does NOT visually block: the mount
    // already happened via the hook; the await just lets the creation-path spawn
    // the shell once the worktree dir exists (status → "ready"). The fast-path
    // (already-ready) and on-disk exits don't fire `onCreating`, so the
    // post-await switch below covers them.
    try {
      resolvedCwd = await get().ensureWorktree(
        teamKey,
        storyFilePath,
        overrideStartPoint,
        expectedSlug,
        {
          onCreating: () => {
            if (isCurrent?.() === false) return;
            applyOpenFocus(get, teamKey, workspaceId, focus); // SW-FX-69: additive by default
            get().expandTerminalOnOpen(teamKey); // SW-FX-35: pair-safe "show the terminal"
          },
          // SW-AM-7: forward the spawn opt so the fresh-CREATE path (spawn lives in
          // `ensureWorktree`, not just the `finishForegroundOpen` tail) also honors it.
          spawnFirstTerminal,
          location,
        },
      );
    } catch (err) {
      // Post-toast errors are already displayed via toast/modal — swallow
      // them so callers' .catch doesn't double-toast. Pre-toast errors
      // (plain Error from config probe/load) still throw.
      const setupErr = err as { failedStep?: string };
      if (
        setupErr.failedStep &&
        (HANDLED_FAILED_STEPS as readonly string[]).includes(setupErr.failedStep)
      ) {
        return resolvedCwd;
      }
      throw err;
    }
    get().flushPersist();
    if (isCurrent?.() === false) return resolvedCwd;

    // PT-10: reopening a closed-but-running workspace restores its shadowed tab
    // list (the survived PTYs reattach on remount via PT-1) instead of spawning
    // a fresh shell. `reopenSoftClosedWorkspace` re-adds the record (idempotent —
    // `ensureWorktree` already recreated it above), focuses it, and clears the
    // shadow.
    if (get().softClosedWorkspaces[teamKey]?.[workspaceId]) {
      get().reopenSoftClosedWorkspace(teamKey, workspaceId);
      return resolvedCwd;
    }

    // SLW-CO-5: if a concurrent close removed the workspace while setup was in
    // flight (the abort-lost-the-race window — setup resolved despite the Cancel),
    // don't switch/expand/spawn onto a ghost id: that would leave an orphan
    // terminal tab + PTY with no owning tile. `ensureWorktree` already skipped its
    // own success side-effects for the same reason; this covers the open path's.
    // SW-FX-41 (review r2): return "" — nothing was opened (the tile is gone), so
    // the create-from-default handler suppresses its success toast rather than
    // announcing "Started from …" over a torn-down workspace.
    if (!readTeamKeyed(get().workspaces, teamKey, []).some((w) => w.id === workspaceId)) {
      return "";
    }

    // Focus + expand + auto-launch a terminal tab if none (covers the
    // fast-path/on-disk/deduped exits where the creation path didn't spawn one).
    // SLW-CO-6: the same shared finish tail the unified `openWorkspace` core runs.
    // SW-FX-69: an explicit `focus` wins; the unset default focuses a re-opened
    // (fast-path/on-disk) tile — the openOrFocus contract. A fresh create already
    // applied the additive policy in `onCreating`.
    finishForegroundOpen(get, teamKey, workspaceId, {
      focus: focus ?? wasAlreadyOpen,
      spawn: spawnFirstTerminal,
    });

    // SW-GW-8 (AC-1/AC-5): a story worktree opened from the CodeSense nav must
    // join the SAME trusted session registry a dashboard launch does — otherwise
    // it is invisible to the Background agents panel (the SW-GW-3 split). Register
    // CAP-FREE (the user opened this, not a dashboard) under the storyId handle —
    // the SAME key a dashboard `openWorkspace({storyId})` main session binds
    // (`dashboard-launch.ts` handle = bare storyId), so the two COLLAPSE to one
    // row instead of double-listing. Provenance "codesense". Prefer the role-
    // tagged agent tab over a bare shell (mirrors `findOpenStoryWorkspace`).
    // Best-effort: a registration failure never breaks the open.
    registerNavWorkspaceSession(get, teamKey, workspaceId);
    return resolvedCwd;
  },

  ensureStoryWorkspaceOpen: async (teamKey, storyId, opts) => {
    const focus = opts?.focus ?? true;
    // In-memory first: a live tile or a soft-closed (minimized) shadow already
    // carries the story's file path + workspaceId, so there's no need to hit
    // disk. A live record wins a same-story tie over a shadow.
    const s = get();
    const fromRecord =
      readTeamKeyed(s.workspaces, teamKey, []).find(
        (w) => w.storyId === storyId && w.storyFilePath,
      ) ??
      Object.values(s.softClosedWorkspaces[teamKey] ?? {})
        .map((shadow) => shadow.workspace)
        .find((w) => w.storyId === storyId && w.storyFilePath);
    // On an in-memory hit the record's OWN id is the canonical key (it was derived
    // from a normalized path at open time) — prefer it over re-deriving.
    let storyFilePath = fromRecord?.storyFilePath;
    let workspaceId = fromRecord?.id;
    // Durable fallback: a fully-closed workspace has no record left, so resolve
    // the story's file path off disk via the SAME team-content scan the dashboard
    // uses (`use-dashboard-stream`). Async → off the UI thread; only the rare
    // cold-foreground gesture pays it (the in-memory hit above covers the
    // live/minimized cases). A failed/unresolvable scan is a home MISS, not an
    // error — the caller falls back to adopt-into-active. Normalize so the derived
    // id matches the one `openOrFocusStoryWorkspace` will assign to the record.
    if (!storyFilePath) {
      const tree = await scanTeamPipelineTree(teamKey);
      if (opts?.isCurrent?.() === false) return null;
      const scanned = tree ? storyFilePathFromTree(tree, storyId) : null;
      if (!scanned) return null;
      storyFilePath = normalizeStoryFilePath(scanned);
      workspaceId = storyIdFromPath(storyFilePath);
    }
    if (!storyFilePath || !workspaceId || opts?.isCurrent?.() === false) return null;
    // Reveal an existing record (restore a minimized shadow / focus a live tile),
    // else cold-open the worktree. Both are decoupled — no UI-thread block.
    const hasRecord =
      readTeamKeyed(get().workspaces, teamKey, []).some((w) => w.id === workspaceId) ||
      get().softClosedWorkspaces[teamKey]?.[workspaceId] !== undefined;
    if (hasRecord) {
      if (opts?.location) {
        const at = whereWorkspaceLives(get(), teamKey, workspaceId);
        const want = opts.location.kind === "machine" ? opts.location.machineId : "local";
        if (at !== null && at !== want) {
          // SW-WS-1: a move_workspace in flight re-skins the refusal as a soft
          // "being moved" message; the predicate itself is unchanged.
          const movingTo = movingTargetFor(get(), teamKey, workspaceId);
          throw new Error(locationConflictMessage(storyId, at, movingTo));
        }
      }
      if (focus) get().activateTrayWorkspace(teamKey, workspaceId);
      return workspaceId;
    }
    // Cold-open. `focus:true` — an explicit "bring to foreground" gesture wants the
    // tile up. `spawnFirstTerminal:false` — the caller attaches the agent's
    // ALREADY-RUNNING PTY as the workspace's tab, so a redundant fresh shell (which
    // would also hijack the storyId nav-registration) must not be spawned. A handled
    // setup error / concurrent-close ghost returns "" with NO live record — don't
    // hand back a workspaceId pointing at a nonexistent tile (the caller would orphan
    // the agent's tab there); degrade to the adopt-into-active fallback (AC-4).
    // A workspace created on a machine opens its own first terminal there: no
    // local agent PTY is waiting to be attached to it.
    const onMachine = opts?.location?.kind === "machine";
    const openOptions: [WorkspaceLocationChoice?, (() => boolean)?] = opts?.isCurrent
      ? [opts.location, opts.isCurrent]
      : opts?.location
        ? [opts.location]
        : [];
    const cwd = await get().openOrFocusStoryWorkspace(
      teamKey,
      storyFilePath,
      undefined,
      undefined,
      focus,
      onMachine,
      ...openOptions,
    );
    return cwd ? workspaceId : null;
  },

  syncWorkspaceFrontmatter: (teamKey, tree) => {
    publishTeamTree(teamKey, tree);
    const workspaces = get().workspaces[teamKey];
    // SW-FX-30: the minimized (soft-closed) shadow is reconciled alongside the
    // live array below (its frozen `Workspace` copies are what the minimized
    // tray renders), so read it once here and share it with the rail index.
    const teamShadow = get().softClosedWorkspaces[teamKey];

    // SW-EW-40: rebuild the per-team rail-key index from the tray's OWN grouping
    // (`trayWorkspaceOrder`+`railKeyOf`) so grouped-mode mosaic membership and the
    // tray share ONE workspace→epic map. This action already fires on every
    // tree/workspace change with the tree in hand (team-workspace-view), and the
    // store otherwise has no tree — so this is the seam. Covers the open ∪
    // soft-closed union (built before the empty-open early-return below). No-op
    // guard so a tick that doesn't change the mapping doesn't churn subscribers.
    const groups = get().customGroupsByTeam[teamKey] ?? [];
    const { railKeyIndex: nextRailIndex, baseOrder: nextBaseOrder } = buildWorkspaceGrouping(
      workspaces ?? [],
      teamShadow ?? {},
      tree,
      groups,
    );
    // The base order and the rail index land in ONE set(), so no subscriber sees one without the
    // other.
    const baseChanged = !sameStringOrder(get().trayBaseOrderByTeam[teamKey], nextBaseOrder);
    const railChanged = !sameStringRecord(get().railKeyByWorkspaceId[teamKey], nextRailIndex);
    // Moves made before this team had a base are made again over it, in the same set(). The
    // first sync takes them whether or not it changes the base (one restored from a save may
    // already match it), so a queued move never outlives the base it waited for.
    const rebase = provisionalMoves.get(teamKey);
    if (rebase) provisionalMoves.delete(teamKey);
    // A workspace that left the team by any path (not only a close) leaves its stored order too,
    // so one that comes back later takes its computed place, not a stale slot.
    const storedOrder = get().workspaceOrderByTeam[teamKey];
    const liveIds = new Set(nextBaseOrder);
    const storedStale =
      (workspaces !== undefined || teamShadow !== undefined) &&
      !!storedOrder?.some((id) => !liveIds.has(id));
    if (baseChanged || railChanged || rebase || storedStale) {
      set((prev) => {
        const patch: Partial<ViewerState> = baseChanged
          ? {
              trayBaseOrderByTeam: updateTeamKeyed(
                prev.trayBaseOrderByTeam,
                teamKey,
                nextBaseOrder,
              ),
            }
          : {};
        if (!rebase && storedStale) {
          const kept = (prev.workspaceOrderByTeam[teamKey] ?? []).filter((id) => liveIds.has(id));
          if (kept.length > 0) {
            patch.workspaceOrderByTeam = updateTeamKeyed(prev.workspaceOrderByTeam, teamKey, kept);
          } else {
            const { [teamKey]: _gone, ...rest } = prev.workspaceOrderByTeam;
            patch.workspaceOrderByTeam = rest;
          }
        }
        if (rebase) {
          // Over the groups this sync computes, not the last ones.
          const groups = updateTeamKeyed(prev.railKeyByWorkspaceId, teamKey, nextRailIndex);
          patch.workspaceOrderByTeam = rebasedWorkspaceOrders(
            { ...prev, railKeyByWorkspaceId: groups },
            teamKey,
            nextBaseOrder,
            rebase,
          );
        }
        if (!railChanged) return patch;
        patch.railKeyByWorkspaceId = updateTeamKeyed(
          patch.railKeyByWorkspaceId ?? prev.railKeyByWorkspaceId,
          teamKey,
          nextRailIndex,
        );
        return patch;
      });
      // The base is saved, so a restart keeps this order until the tree syncs again.
      if (baseChanged || rebase) get().persistState();
    }

    // SW-FX-30: proceed when EITHER the live array OR the minimized shadow has
    // something to reconcile. Previously this bailed on an empty live array,
    // which froze every minimized card's status once ALL workspaces were
    // minimized (AC-3). The rail-index build above stays ahead of this guard.
    const shadowEntries = teamShadow ? Object.entries(teamShadow) : [];
    if ((!workspaces || workspaces.length === 0) && shadowEntries.length === 0) return;

    // Collect all stories from the tree (recursively including subEpics).
    const allStories: StoryRef[] = [];
    function collect(t: DerivedPipelineTree) {
      for (const s of t.stories) allStories.push(s);
      for (const sub of t.subEpics) collect(sub);
    }
    collect(tree);

    // Build lookup: normalised absolute path → StoryRef.
    // StoryRef.filePath is already absolute (set by the Rust scanner from
    // `path.display().to_string()`), but R-18 Phase B (AC #28) flows both
    // sides through `normalizeStoryFilePath` so NFC/NFD or trailing-slash
    // drift between the Rust scanner emit and the TS-persisted workspace
    // path can't silently break the lookup.
    const byAbsPath = new Map<string, StoryRef>();
    for (const s of allStories) {
      byAbsPath.set(normalizeStoryFilePath(s.filePath), s);
    }

    // SW-FX-44: secondary match indexes so a persisted-path vs scan-path
    // divergence no longer silently skips the workspace (see the exact `ref`
    // lookup below). Two DIFFERENT key spaces, kept separate on purpose:
    //  - `byFrontmatterId` keys on `StoryRef.storyId` (the `story_id`
    //    frontmatter field, e.g. "SW-FX-44") — stable across a filename
    //    change, so it covers a moved/renamed story file.
    //  - `byPathStem` keys on `storyIdFromPath(filePath)` (the filename
    //    stem, e.g. "SW-FX-44-workspace-chrome-…") — identical between a
    //    context-repo source copy and its projected `_bmad-output/` copy
    //    (same basename, different parent dir), so it covers the
    //    source-vs-projected divergence that is the origin case here.
    // Conflating the two into one map would mismatch the key spaces. Keys
    // that resolve to >1 distinct file are AMBIGUOUS and dropped, so a
    // collision degrades to "no update" (safe) rather than a wrong glyph.
    const buildUniqueIndex = (
      keyOf: (s: StoryRef) => string | undefined,
    ): Map<string, StoryRef> => {
      const index = new Map<string, StoryRef>();
      const ambiguous = new Set<string>();
      for (const s of allStories) {
        const key = keyOf(s);
        if (!key) continue;
        const existing = index.get(key);
        if (existing) {
          if (normalizeStoryFilePath(existing.filePath) !== normalizeStoryFilePath(s.filePath)) {
            ambiguous.add(key);
          }
          continue;
        }
        index.set(key, s);
      }
      for (const key of ambiguous) index.delete(key);
      return index;
    };
    // Built lazily (memoised) — in the steady state every workspace matches
    // `byAbsPath` exactly, so these secondary indexes are only ever needed
    // on a path-drift miss. Avoids two O(n) passes on the reconcile hot path
    // (this fires on every tree refresh). `|| undefined` (not `??`) so an
    // empty-string `storyId` is skipped, not indexed under "".
    let byFrontmatterId: Map<string, StoryRef> | undefined;
    let byPathStem: Map<string, StoryRef> | undefined;
    const frontmatterIndex = () =>
      (byFrontmatterId ??= buildUniqueIndex((s) => s.storyId || undefined));
    const pathStemIndex = () =>
      (byPathStem ??= buildUniqueIndex((s) => storyIdFromPath(s.filePath)));
    // SLW-IN-9 AC-4: `epicDirPath` → current epic main-doc path, from the live
    // tree. Lazily built (only epic workspaces need it), keyed the same as the
    // tray grouping (`dirName`), so an epic's double-click doc stays live.
    let epicDocByDir: Map<string, string> | undefined;
    const epicDocIndex = () => (epicDocByDir ??= buildEpicDocPathIndex(tree));

    // R-18: reconcile every frontmatter-derived field the tree carries
    // (`storyStatus`, `storyId`, `sourceRepoSlug`). When `ref.storyId` is
    // absent, fall back to the path-derived form so the invariant
    // "`Workspace.storyId` always reflects the current authoritative value"
    // holds for the no-frontmatter case too.
    //
    // SW-FX-30: the whole per-workspace body lives in one closure so BOTH the
    // live array and the minimized (soft-closed) shadow reconcile through the
    // SAME resolver — the SW-FX-44 multi-index fallbacks and the
    // `frontmatterReconcile` gate can't drift between the two passes. Returns
    // the updated `Workspace`, or `null` when nothing changed (or the kind is
    // not frontmatter-reconciled — which excludes epic/integration shadows for
    // free, so no non-story record ever gets a `storyStatus`).
    const reconcileWorkspaceFromTree = (ws: Workspace): Workspace | null => {
      // SLW-IN-9 AC-4: an epic workspace carries no story frontmatter, but its
      // doc path (`epicDocPath`, the README snapshot) 404s after the epic's main
      // doc moves. Repoint it live from the tree by epic identity so the title
      // double-click opens the current doc. Kept before the story guard because
      // epic records have no `storyFilePath`.
      if (getKindCapabilities(ws.kind).docPathSource === "epicDocPath") {
        if (!ws.epicDirPath) return null;
        const liveDoc = epicDocIndex().get(ws.epicDirPath);
        if (!liveDoc) return null;
        const normalized = normalizeStoryFilePath(liveDoc);
        if (ws.epicDocPath && normalizeStoryFilePath(ws.epicDocPath) === normalized) return null;
        return { ...ws, epicDocPath: normalized };
      }
      if (!getKindCapabilities(ws.kind).frontmatterReconcile || !ws.storyFilePath) return null;
      // SW-FX-44: exact absolute-path match first (fast, unambiguous); then
      // the frontmatter-id fallback (survives a rename — `ws.storyId` holds
      // the frontmatter id after any prior reconcile); then the path-stem
      // fallback (survives source-vs-projected divergence — same basename).
      const stem = storyIdFromPath(ws.storyFilePath);
      const normalizedWsPath = normalizeStoryFilePath(ws.storyFilePath);
      // SLW-IN-9 AC-3-FE: track WHETHER the match was by durable story_id at a
      // path that drifted — that (and only that) is a genuine move whose path
      // hint we repoint below. `byAbsPath` is already current; `byPathStem`
      // matches a distinct physical copy (source vs projection) we must not
      // rewrite to. `buildUniqueIndex` drops ambiguous ids, so a `byFrontmatterId`
      // hit is unambiguously THIS story's current location.
      let matchedByStableId = false;
      let ref = byAbsPath.get(normalizedWsPath);
      if (!ref && ws.storyId) {
        ref = frontmatterIndex().get(ws.storyId);
        if (ref) matchedByStableId = true;
      }
      if (!ref) {
        // Path-stem fallback, accepted only on POSITIVE corroboration. The
        // dup guard only catches collisions *present together* in the scan —
        // it can't see the case where THIS workspace's own file has left the
        // scan (deleted/moved/other team) and a lone UNRELATED story happens
        // to share its filename stem. Adopting it would cross-wire a wrong
        // status (and `sourceRepoSlug`, which routes later writes) — trading
        // safe staleness for a wrong glyph. So accept the stem candidate only
        // when EITHER:
        //  - the workspace carries no real frontmatter-id signal (its
        //    `storyId` is still just the path stem — the pre-reconcile
        //    source-vs-projected case, where the stem is all we have), OR
        //  - the candidate's frontmatter id positively AGREES with the
        //    workspace's.
        // An *absent* candidate `storyId` is not corroboration — a lone
        // same-stem impostor with no `story_id` is still rejected.
        const candidate = pathStemIndex().get(stem);
        if (candidate) {
          const wsHasFrontmatterId = !!ws.storyId && ws.storyId !== stem;
          if (!wsHasFrontmatterId || candidate.storyId === ws.storyId) {
            ref = candidate;
          }
        }
      }
      if (!ref) return null;

      const newStatus = normalizeStatus(ref.status);
      // Code-review P3: `||` (not `??`) so an empty-string `ref.storyId`
      // also falls back — defence-in-depth in case the cache/scanner ever
      // forwards `""` past the Rust-side `.filter(!is_empty)` guard.
      // Empty-string `ref.sourceRepo` likewise collapses to undefined here
      // rather than overwriting a good slug with `""`.
      const newStoryId = ref.storyId || stem;
      const newSourceRepoSlug = ref.sourceRepo || undefined;
      // SW-ID-6: reconcile the assignee login alongside the other scan-derived
      // fields so the story HUD avatar follows a re-assignment. `|| undefined`
      // collapses an empty-string assignee to unassigned.
      const newAssignedTo = ref.assignedTo || undefined;
      // SLW-IN-9 AC-3-FE: repoint the path hint when the story moved (matched by
      // durable story_id at a different path). Demotes `storyFilePath` to a
      // re-resolvable hint: the existing `ensureWorktree` fast-path, the tray's
      // grouping, and the doc-open gesture all key on it, so one
      // repoint self-heals every path-keyed linkage without a manual reopen.
      const movedPath =
        matchedByStableId && normalizeStoryFilePath(ref.filePath) !== normalizedWsPath
          ? normalizeStoryFilePath(ref.filePath)
          : undefined;
      if (
        ws.storyStatus === newStatus &&
        ws.storyId === newStoryId &&
        ws.sourceRepoSlug === newSourceRepoSlug &&
        ws.storyAssignedTo === newAssignedTo &&
        !movedPath
      ) {
        return null;
      }
      return {
        ...ws,
        storyStatus: newStatus,
        storyId: newStoryId,
        sourceRepoSlug: newSourceRepoSlug,
        storyAssignedTo: newAssignedTo,
        ...(movedPath ? { storyFilePath: movedPath } : {}),
      };
    };

    // Live pass.
    let changed = false;
    const updated = (workspaces ?? []).map((ws) => {
      const next = reconcileWorkspaceFromTree(ws);
      if (next) changed = true;
      return next ?? ws;
    });

    // SW-FX-30: shadow pass. Rebuild only the changed `sc.workspace` copies so
    // the minimized tray's frozen records track status the same way PT-4 keeps
    // their `aiStatus` live. O(minimized count) over indexes already built for
    // the live pass — no new tree walk, no async.
    let shadowChanged = false;
    let nextShadow = teamShadow;
    if (teamShadow) {
      const rebuilt: Record<string, SoftClosedWorkspace> = {};
      for (const [wsId, sc] of shadowEntries) {
        const next = reconcileWorkspaceFromTree(sc.workspace);
        if (next) {
          shadowChanged = true;
          rebuilt[wsId] = { ...sc, workspace: next };
        } else {
          rebuilt[wsId] = sc;
        }
      }
      if (shadowChanged) nextShadow = rebuilt;
    }

    if (changed || shadowChanged) {
      // A story keys its terminals by its frontmatter `story_id`: one that arrived
      // or changed moves the workspace's terminals to its new key.
      const before = get();
      const rekeyed: Array<[Workspace, Workspace]> = [
        ...(workspaces ?? []).map((ws, i): [Workspace, Workspace] => [ws, updated[i]]),
        ...shadowEntries.map(([wsId, sc]): [Workspace, Workspace] => [
          sc.workspace,
          nextShadow?.[wsId]?.workspace ?? sc.workspace,
        ]),
      ];
      for (const [prev, next] of rekeyed) {
        moveWorkspaceTerminals(before, workspaceKey(teamKey, prev), workspaceKey(teamKey, next));
      }
      set((s) => ({
        ...(changed ? { workspaces: updateTeamKeyed(s.workspaces, teamKey, updated) } : {}),
        ...(shadowChanged
          ? { softClosedWorkspaces: updateTeamKeyed(s.softClosedWorkspaces, teamKey, nextShadow!) }
          : {}),
      }));
    }
  },

  changeStoryStatus: async (teamKey, workspaceId, newStatus) => {
    // SW-FX-30: a minimized workspace lives in the soft-closed shadow, not the
    // live array, so resolve from BOTH — otherwise the card's own status
    // popover silently no-ops on a minimized card (the disk is never written).
    const liveWs = get().workspaces[teamKey]?.find((w) => w.id === workspaceId);
    const shadowWs = liveWs
      ? undefined
      : get().softClosedWorkspaces[teamKey]?.[workspaceId]?.workspace;
    const ws = liveWs ?? shadowWs;
    if (!ws?.storyFilePath) return;

    const previousStatus = ws.storyStatus;

    // Write `storyStatus` to whichever slice CURRENTLY owns the workspace,
    // re-resolved on every call. A story can migrate between the live array and
    // the soft-closed shadow (minimize/restore) during the `await` below, so
    // pinning the slice at entry would strand the failure-revert on a slice the
    // record has left. `expected` (optional) makes the write conditional: apply
    // only when the current status still equals it — so the revert never
    // clobbers a newer value a concurrent reconcile wrote mid-flight.
    const writeStatus = (status: PipelineStatus | undefined, expected?: PipelineStatus) =>
      set((s) => {
        const live = s.workspaces[teamKey];
        const liveIdx = live ? live.findIndex((w) => w.id === workspaceId) : -1;
        if (liveIdx >= 0) {
          if (expected !== undefined && live![liveIdx].storyStatus !== expected) return {};
          const nextLive = live!.slice();
          nextLive[liveIdx] = { ...nextLive[liveIdx], storyStatus: status };
          return { workspaces: updateTeamKeyed(s.workspaces, teamKey, nextLive) };
        }
        const teamShadow = s.softClosedWorkspaces[teamKey];
        const sc = teamShadow?.[workspaceId];
        if (!sc) return {};
        if (expected !== undefined && sc.workspace.storyStatus !== expected) return {};
        return {
          softClosedWorkspaces: {
            ...s.softClosedWorkspaces,
            [teamKey]: {
              ...teamShadow,
              [workspaceId]: { ...sc, workspace: { ...sc.workspace, storyStatus: status } },
            },
          },
        };
      });

    // Optimistic update.
    writeStatus(newStatus);

    try {
      // Serialized per file so a rapid second change (here or from the board /
      // nav) can't lose to this write completing later — the last intent wins.
      await writePipelineStatus(ws.storyFilePath, newStatus);
      // T.1: bumpFileRefresh still needs a per-repo signal. Resolve the
      // workspace's owning repo via its `sourceRepoSlug` against the
      // workspaceRepos map; fall back to all team repos if the slug is
      // missing (e.g. legacy workspaces pre-M.3).
      const targetRepoPath = ws.sourceRepoSlug
        ? findWorkspaceRepoBySlug(ws.sourceRepoSlug, parseTeamKey(teamKey))?.repoPath
        : undefined;
      if (targetRepoPath) {
        useFileRefreshStore.getState().bumpFileRefresh(targetRepoPath);
      } else {
        for (const repo of Object.values(useOrgStore.getState().workspaceRepos)) {
          useFileRefreshStore.getState().bumpFileRefresh(repo.repoPath);
        }
      }
    } catch (err) {
      // Revert — re-resolve the owning slice, and only if our optimistic value
      // is still the one present (don't clobber a concurrent reconcile).
      writeStatus(previousStatus, newStatus);
      toast.error(`Failed to update status: ${getErrorMessage(err)}`);
    }
  },

  beginWorkspaceOpen: (teamKey, workspaceId) =>
    set((s) => {
      const prev = s.openingWorkspaces[teamKey];
      if (prev?.has(workspaceId)) return s;
      const next = new Set(prev ?? []);
      next.add(workspaceId);
      return { openingWorkspaces: updateTeamKeyed(s.openingWorkspaces, teamKey, next) };
    }),

  endWorkspaceOpen: (teamKey, workspaceId) =>
    set((s) => {
      const prev = s.openingWorkspaces[teamKey];
      if (!prev?.has(workspaceId)) return s;
      const next = new Set(prev);
      next.delete(workspaceId);
      const nextOuter = { ...s.openingWorkspaces };
      if (next.size === 0) delete nextOuter[teamKey];
      else nextOuter[teamKey] = next;
      return { openingWorkspaces: nextOuter };
    }),

  cancelWorkspaceOpen: (teamKey, workspaceId) => {
    // AC-2: abort + evict the in-flight open via the ONE shared teardown
    // (`abortInFlightWorktreeOpen`) — the same handle `closeWorkspace`/`closeTeam`
    // use. The probe chain's `throwIfCancelled` checkpoints bail before any
    // worktree/record is created; `setupWorktree` (if reached) honors the same
    // signal natively. Evicting the dedup entry lets an immediate reopen of the
    // same story run fresh instead of piggybacking this doomed promise (dead
    // click); the doomed promise's ownership-guarded `finally` no-ops rather than
    // clobbering the reopen.
    abortInFlightWorktreeOpen(workspaceAbortKey(teamKey, workspaceId));
    // Drop the overlay immediately so Cancel is instant even while a slow
    // `findWorktreeOnDisk` git-spawn is still resolving (it bails at its next
    // checkpoint). The probe promise's `finally` also clears this — idempotent.
    get().endWorkspaceOpen(teamKey, workspaceId);
  },

  ensureWorktree: async (teamKey, storyFilePath, overrideStartPoint?, expectedSlug?, opts?) => {
    // T.5: signature re-keyed `repoPath` → `teamKey`. All workspace-slice
    // reads/writes bucket by the input `teamKey`; the source repo is
    // resolved internally from the story's frontmatter via the M.2 chain
    // (D-6, AC-3). The pre-T.5 `parseSyntheticRepoKey(repoPath)` dance is
    // gone — callers compute `teamKey` once at the call site (D-2).
    //
    // T.4 Phase 11 (Round 3 MED — mid-flight guard fix): capture the
    // team's close epoch at function entry. closeTeam bumps the epoch
    // for its team; the post-await check below skips the workspace write
    // if the epoch increased — i.e., closeTeam fired during setupWorktree.
    // T.4 Round 4 (Edge Case Hunter #1/#2/#3 + Blind Hunter #13): the
    // original Phase-11 guard only protected the success-path
    // addStoryWorkspace call. The on-disk branch + three probe-failure
    // exits + the pre-flight blocked-dep write all run after awaits and
    // need the same guard, otherwise a cross-team `closeTeam` racing with
    // any of them resurrects torn-down team state. Helper centralises the
    // check; every post-await write goes through it.
    const teamCloseEpochAtEntry = teamCloseEpoch.get(teamKey) ?? 0;
    const isMidFlightClose = (siteLabel: string): boolean => {
      const teamCloseEpochNow = teamCloseEpoch.get(teamKey) ?? 0;
      if (teamCloseEpochNow > teamCloseEpochAtEntry) {
        logger.warn(
          "viewer",
          `ensureWorktree: closeTeam(${teamKey}) fired mid-flight (epoch ${teamCloseEpochAtEntry} → ${teamCloseEpochNow}) — skipping ${siteLabel} for ${storyFilePath}`,
        );
        return true;
      }
      return false;
    };

    // Code-review P5: normalise once at entry; thread the normalised
    // value through every downstream comparison + persist site so the
    // Rust scanner's `path.display()` output, the persisted workspace
    // path, and incoming probe paths all match byte-for-byte.
    storyFilePath = normalizeStoryFilePath(storyFilePath);

    // R-18: `workspaceId` is the identity key (path-derived, used for dedup,
    // abort, toast keying). The display label `storyId` is parsed from
    // frontmatter below and threaded into `addStoryWorkspace` separately.
    const workspaceId = storyIdFromPath(storyFilePath);

    // A workspace on a machine opens there: its record says where. Only an
    // open of the workspace (`onCreating`) can; a caller that needs a local
    // directory is told, never handed a remote path or a local stand-in.
    // A workspace stays where it was created: asked to open somewhere else,
    // say where it lives instead of opening it there.
    if (opts?.location) {
      const at = whereWorkspaceLives(get(), teamKey, workspaceId);
      const want = opts.location.kind === "machine" ? opts.location.machineId : "local";
      if (at !== null && at !== want) {
        // SW-WS-1: soft-refuse during an in-flight move.
        const movingTo = movingTargetFor(get(), teamKey, workspaceId);
        throw new Error(locationConflictMessage(workspaceId, at, movingTo));
      }
    }
    const remoteRecord = readTeamKeyed(get().workspaces, teamKey, []).find(
      (w): w is Workspace & { location: MachineLocation } =>
        w.id === workspaceId && isRemoteWorkspace(w),
    );
    if (remoteRecord) {
      if (!opts?.onCreating) {
        throw new Error(
          `${remoteRecord.storyId ?? workspaceId} lives on a remote machine; open it there`,
        );
      }
      if (
        remoteRecord.status === "ready" &&
        remoteRecord.location.handle &&
        remoteRecord.location.cwd
      ) {
        return remoteRecord.location.cwd;
      }
    }

    // P4: fast path FIRST — a cached ready worktree can resolve even if
    // the attachment was removed after adoption (worktree path is what
    // matters, not the source-repo path).
    const existing = readTeamKeyed(get().workspaces, teamKey, []).find(
      (w) => w.storyFilePath === storyFilePath && w.status === "ready" && !isRemoteWorkspace(w),
    );

    // Persisted ready state may outlive an interrupted setup. Reconcile the
    // durable registration before trusting it; a live setup keeps its own gate.
    if (existing?.backgroundSetup) return existing.cwd;
    if (existing?.sourceRepoSlug) {
      const registration = establishWorktreeStoryLink(
        existing.cwd,
        storyFilePath,
        existing.storyId ?? "",
        existing.sourceRepoSlug,
      );
      trackWorktreeEstablish(existing.cwd, registration);
      const stillCurrent = () =>
        !isMidFlightClose("ready workspace reconciliation") &&
        readTeamKeyed(get().workspaces, teamKey, []).some((w) => w === existing);
      try {
        const complete = await registration;
        if (!stillCurrent()) throw failedStepError("Worktree open cancelled", "cancelled");
        if (complete !== false) return existing.cwd;
      } catch (error) {
        if (!stillCurrent()) throw error;
        set((s) => ({
          workspaces: updateTeamKeyed(
            s.workspaces,
            teamKey,
            readTeamKeyed(s.workspaces, teamKey, []).map((w) =>
              w.id === existing.id
                ? { ...w, status: "error" as const, statusError: getErrorMessage(error) }
                : w,
            ),
          ),
        }));
        throw error;
      }
      set((s) => ({
        workspaces: updateTeamKeyed(
          s.workspaces,
          teamKey,
          readTeamKeyed(s.workspaces, teamKey, []).map((w) =>
            w.id === existing.id ? { ...w, status: "creating" as const } : w,
          ),
        ),
      }));
    }

    // SLW-CO-5: a prior FAILED open leaves a `status:"error"` record in the store
    // (the frame shows `WorkspaceErrorPlaceholder`). Everything downstream is
    // add-only (`addStoryWorkspace` no-ops on an existing id) or `"creating"`-
    // guarded (`updateWorkspaceSetup`), so a retry — re-clicking the story,
    // which is exactly how AC-6's "retry" is expected to work — would no-op
    // against that stale error record: the UI stays stuck on the error, and a
    // silently-successful retry would even spawn a hidden PTY the host layer
    // refuses to render. Clear the errored record here so the retry re-creates
    // the workspace fresh (covers both the on-disk and creation paths below).
    // A machine's errored record is kept: its retry reconciles the same request.
    const staleErrored = readTeamKeyed(get().workspaces, teamKey, []).find(
      (w) => w.id === workspaceId && w.status === "error" && !isRemoteWorkspace(w),
    );
    if (staleErrored) {
      get().closeWorkspace(teamKey, workspaceId);
    }

    // Dedup: piggyback on an in-flight ensureWorktree for the same story.
    // Checked BEFORE any async work so concurrent callers (e.g. double-click)
    // don't duplicate readFile, source-repo resolution, or error toasts.
    // T.1: abort key uses teamKey (matches `workspaceAbortKey`'s post-T.1
    // contract — `${teamKey}::${workspaceId}`).
    const dedupKey = workspaceAbortKey(teamKey, workspaceId);
    const pending = ensureWorktreeInFlight.get(dedupKey);
    if (pending) return pending;

    // SW-SO-1: hoist the abort handle + the pre-record "opening" overlay flag to
    // HERE — AFTER the fast-path + dedup early-returns above (so a ready-reopen
    // or piggybacked double-click never raises them) and BEFORE the probe
    // promise. Both live in `ensureWorktree`'s scope (not the IIFE's) so the
    // OUTER `finally` below can reconcile them on EVERY exit — including the
    // pre-flight throws (config/blocked-dep/team-mismatch/no-clone) that bypass
    // the IIFE's inner try/finally.
    //
    // (AC-2) The abort handle used to be created only AFTER the whole probe
    // chain, so a Cancel during the multi-second pre-flight window had nothing to
    // abort. It is registered under the SAME key `closeWorkspace` and the opening
    // overlay's Cancel use — one abort path (AC-8).
    const controller = new AbortController();
    storyWorkspaceAborts.set(dedupKey, controller);
    // Bail the pre-flight awaits at their next checkpoint. A git-spawn
    // `findWorktreeOnDisk` can't be killed mid-spawn, but checking after each
    // await guarantees we never create a worktree/record after an abort.
    const throwIfCancelled = (): void => {
      if (!controller.signal.aborted) return;
      throw failedStepError("Worktree open cancelled", "cancelled");
    };
    // (AC-1/AC-3/AC-4) Raise the opening overlay the INSTANT a genuine open
    // begins. This runs synchronously (before the first `await` in the probe
    // promise), within the click's task, so the team view paints the "Setting up
    // worktree…" affordance BEFORE the git/config probe chain runs — no dead,
    // unclickable, spinner-less window. Foreground opens only (`onCreating`);
    // hands off to the in-tile `creating` overlay the moment `addStoryWorkspace`
    // lands a record (see `resolveOpeningWorkspaceId`).
    if (opts?.onCreating && !existing) {
      get().beginWorkspaceOpen(teamKey, workspaceId);
      // SW-SO-1 (review r1 — AC-1): reveal the terminal region NOW, at intent
      // time, not at the post-probe `onCreating`. The opening overlay is hosted
      // inside the terminal flex region, which collapses to `0 0 0px` while
      // `terminalVisible` is false (viewer/collapsed mode) — so without this the
      // overlay would render into a zero-height box and stay invisible during the
      // exact pre-flight stall AC-1 targets. `expandTerminalOnOpen` is the same
      // canonical reveal `onCreating` fires (moves `layoutMode`+`terminalVisible`
      // together; "shared" keeps the viewer visible), just hoisted ahead of the
      // probe chain. Idempotent — `onCreating` re-asserts it post-probe.
      get().expandTerminalOnOpen(teamKey);
    }

    const promise = (async (): Promise<string> => {
      // SW-SO-2 (① — AC-1): dev-flag-gated per-hop timing of the pre-flight probe
      // chain (null + no-op unless `localStorage["codesense:open-timing"]==="1"`).
      const openTimer = startOpenTimer(storyFilePath);
      // M.2: read story file once for source_repo resolution + reuse by
      // parseStoryTitle and resolveWorktreeParams (DRY — AC-8).
      let storyContent: string | undefined;
      try {
        storyContent = await readFile(storyFilePath);
      } catch {
        // File unreadable — treat as "no source_repo", downstream consumers
        // that need content will read from disk themselves (backward compat).
        storyContent = undefined;
      }
      openTimer?.mark("readFile");
      throwIfCancelled(); // SW-SO-1 (AC-2)

      // M.2 / T.5 D-6: resolve source repo from story frontmatter against
      // the team's tracked repos. Two shapes:
      //
      //   (1) story declares `source_repo` → look up in team. Throw
      //       `WorktreeTeamMismatchError("slug-not-in-team")` when the
      //       team doesn't track that slug (AC-4, D-1).
      //   (2) story has no `source_repo` frontmatter → fall back to the
      //       team's first tracked source repo (matches T.2 root-workspace
      //       behavior). Throw `WorktreeTeamMismatchError("missing-source-repo")`
      //       when the team has zero source repos.
      //
      // R-18: also parse `story_id` against the same pre-read content so the
      // workspace's display label sources from frontmatter at creation time.
      const sourceRepoSlug = storyContent ? parseSourceRepoFromFrontmatter(storyContent) : null;
      const frontmatterStoryId = storyContent ? parseStoryIdFromFrontmatter(storyContent) : null;

      let storyRepo: RepoState | undefined;
      if (sourceRepoSlug) {
        // AC-3 / D-6 step 2: story declares a source repo — look it up in
        // the team's tracked repos.
        // T.4 Round 5 (Edge Case Hunter HIGH → patch P3): scope the lookup
        // to THIS team. Teams independently track the same source slug, so
        // an unscoped lookup could return a foreign team's RepoState whose
        // synthetic repoPath parses to the wrong teamKey — stamping the
        // no-clone prompt / setup modal with a teamKey the modal-render
        // gates then reject.
        storyRepo = findWorkspaceRepoBySlug(sourceRepoSlug, parseTeamKey(teamKey));
        if (!storyRepo) {
          // T.5 D-1 (iter-1 #F1): emit a reason-specific toast HERE before
          // throwing. With `team-mismatch` in HANDLED_FAILED_STEPS, every
          // caller's `.catch` treats the error as "already surfaced by the
          // action," so the toast MUST come from this site or no user
          // feedback appears at all.
          //
          // T-13 D-2: disambiguate WHY the lookup failed, most-specific
          // first. All three reasons share this single throw site — `mismatch`
          // emits the reason's toast (which MUST originate here, see above)
          // and builds the typed error; the caller throws it.
          const mismatch = (
            reason: WorktreeTeamMismatchReason,
            message: string,
            action?: { label: string; onClick: () => void },
          ): WorktreeTeamMismatchError => {
            if (action) toast.error(message, { action });
            else toast.error(message);
            return new WorktreeTeamMismatchError(reason, sourceRepoSlug, teamKey, message);
          };

          const normalizedSlug = sourceRepoSlug.toLowerCase();
          const wasRemoved = useOrgStore
            .getState()
            .recentlyRemovedRepos[teamKey]?.has(normalizedSlug);

          // Normalize both sides: callers derive `expectedSlug` from the
          // scan-time slug (already lowercased today), but normalizing here
          // keeps the rewrite check case-insensitive regardless of caller.
          if (expectedSlug != null && expectedSlug.toLowerCase() !== normalizedSlug) {
            // The frontmatter `source_repo` changed between the pipeline scan
            // (which produced `expectedSlug`) and this click. The view is
            // stale; reloading the pipeline picks up the new mapping. The
            // bump-all action is robust to which pipeline surfaced the node.
            throw mismatch(
              "frontmatter-rewritten",
              "Story's source_repo was rewritten. Reload the pipeline to pick up the new mapping.",
              {
                label: "Reload pipeline",
                onClick: () => useFileRefreshStore.getState().bumpFileRefresh(),
              },
            );
          }

          if (wasRemoved) {
            // The repo was tracked by this team but the user removed it after
            // this story workspace was created — an orphan.
            throw mismatch(
              "slug-stale-after-removal",
              `Repo ${sourceRepoSlug} was removed from this team. ` +
                `Re-add it in Settings → Workspace, or close the orphan workspace.`,
            );
          }

          throw mismatch(
            "slug-not-in-team",
            `Story references repo ${sourceRepoSlug} which isn't tracked by this team.`,
          );
        }
      } else {
        // T.5 D-6 fallback: no source_repo frontmatter — use the team's
        // first tracked source repo (matches T.2's `ensureRootWorkspaces`
        // order, which follows `teams.yml.source_repos`). This is a
        // behavior shift from pre-T.5 (which fell back to the panel's
        // repoPath); the no-frontmatter fallback is effectively dead
        // code post-PTS-2 since every team-content story carries
        // source_repo, but we keep it defensively for hand-edited /
        // legacy stories.
        const teamRepos = workspaceReposForTeam(useOrgStore.getState().workspaceRepos, teamKey);
        if (teamRepos.length === 0) {
          // T.5 D-1 (iter-1 #F1): toast at throw site — same reasoning
          // as the slug-not-in-team branch above.
          const message =
            `This team has no source repos configured. ` +
            `Add one in Settings → Workspace before opening worktrees.`;
          toast.error(message);
          throw new WorktreeTeamMismatchError("missing-source-repo", null, teamKey, message);
        }
        storyRepo = teamRepos[0];
      }

      // AC-4: tracked but no local clone attached → no-clone modal.
      const sourceRepoPath = resolveSourceRepoPath(storyRepo);
      if (sourceRepoPath === null) {
        throwNoClone(get, storyRepo.repoPath, teamKey, storyFilePath);
      }

      const resolvedSourceRepoPath = sourceRepoPath!; // narrowed by throwNoClone
      const resolvedContent = storyContent;
      // T.5: canonical slug for setup-modal identity, composite keys, and
      // toast routing. Derived from the resolved `storyRepo.repoPath` so
      // it carries the same case-preserved shape `syntheticRepoKey`
      // produced — every downstream consumer sees the same bytes.
      const resolvedSlug = deriveSourceRepoSlug(storyRepo.repoPath);

      // Register the durable story owner before watchers, setup, or agent launch.
      const establishStoryWorktree = (cwd: string) => {
        const establishment = establishWorktreeStoryLink(
          cwd,
          storyFilePath,
          frontmatterStoryId ?? "",
          resolvedSlug,
        );
        trackWorktreeEstablish(cwd, establishment);
        return establishment;
      };

      // Legacy persisted rows lack the canonical slug. Resolve it through the
      // tracked repository above before checking their durable registration.
      if (existing && !existing.sourceRepoSlug) {
        const complete = await establishStoryWorktree(existing.cwd);
        throwIfCancelled();
        if (
          isMidFlightClose("legacy ready reconciliation") ||
          !readTeamKeyed(get().workspaces, teamKey, []).some((w) => w === existing)
        ) {
          throw failedStepError("Worktree open cancelled", "cancelled");
        }
        if (complete !== false) return existing.cwd;
        set((s) => ({
          workspaces: updateTeamKeyed(
            s.workspaces,
            teamKey,
            readTeamKeyed(s.workspaces, teamKey, []).map((w) =>
              w.id === existing.id ? { ...w, status: "creating" as const } : w,
            ),
          ),
        }));
      }

      const orgRoot = resolveOrgRoot(storyRepo.repoPath) || resolveOrgRootForTeam(teamKey);
      // SW-WT-9: gate hooks — config-store owns the approval memory, the store
      // state drives the preview-and-approve modal. Shared by the blocking +
      // background setup paths (RF-perf-14). Only fires for team-synced setup.
      const setupApproval = {
        isApproved: (rp: string, h: string) => useConfigStore.getState().isSetupApproved(rp, h),
        record: (rp: string, h: string) => useConfigStore.getState().recordSetupApproval(rp, h),
        prompt: ({ steps, configHash }: { steps: string[]; configHash: string }) =>
          new Promise<boolean>((resolve) => {
            // Concurrent setups share the single modal slot. Decline any
            // still-pending gate before taking it over, so its awaiter resolves
            // (skips) instead of orphaning forever.
            pendingSetupApprovalResolve?.(false);
            pendingSetupApprovalResolve = resolve;
            set({
              worktreeSetupApproval: { steps, configHash, repoLabel: storyRepo.repoName, teamKey },
            });
          }),
      };

      // A workspace on a machine never adopts a local worktree instead.
      const requestedMachine =
        remoteRecord?.location.machineId ??
        (opts?.location?.kind === "machine" ? opts.location.machineId : null);

      // On-disk detection: worktree exists in git but not in our store.
      const onDisk = requestedMachine
        ? null
        : await findWorktreeOnDisk(resolvedSourceRepoPath, storyFilePath);
      openTimer?.mark("findWorktreeOnDisk");
      throwIfCancelled(); // SW-SO-1 (AC-2): bail before adopting an on-disk worktree
      if (onDisk) {
        // T.4 Round 4: guard against mid-flight closeTeam (Edge Case Hunter #2).
        if (isMidFlightClose("on-disk write + addStoryWorkspace")) {
          return onDisk.path;
        }
        // Track on-disk worktree for F-17 cleanup button.
        // T.4 D-3: outer bucket is teamKey; inner key is composite
        // `${slug}::${storyFilePath}`. T.5: slug from the resolved repo
        // (frontmatter-aware via the lookup above).
        const slugForKey = resolvedSlug.toLowerCase();
        const composite = composeWorktreeKey(slugForKey, storyFilePath);
        set((s) => {
          const prev = s.onDiskWorktrees[teamKey] ?? new Map<string, OnDiskWorktreeEntry>();
          if (prev.get(composite)?.path === onDisk.path) return s;
          const next = new Map(prev);
          next.set(composite, {
            path: onDisk.path,
            sourceRepoPath: resolvedSourceRepoPath,
          });
          return { onDiskWorktrees: updateTeamKeyed(s.onDiskWorktrees, teamKey, next) };
        });
        // Best-effort story title from pre-read content (DRY — AC-8).
        const storyTitle = resolvedContent
          ? (parseStoryTitle(resolvedContent) ?? undefined)
          : undefined;
        addStoryWorkspace(set, {
          teamKey,
          workspaceId,
          storyId: frontmatterStoryId ?? undefined,
          cwd: onDisk.path,
          storyFilePath,
          storyTitle,
          sourceRepoPath: resolvedSourceRepoPath,
          sourceRepoSlug: resolvedSlug,
          status: "creating",
        });
        try {
          const complete = await establishStoryWorktree(onDisk.path);
          throwIfCancelled();
          if (complete === false) {
            await runWorktreeSetupPhase({
              repoPath: resolvedSourceRepoPath,
              orgRoot,
              branchName: onDisk.branch ?? "",
              worktreePath: onDisk.path,
              // Setup only: the worktree already exists, so no base is resolved.
              startPoint: null,
              runId: `wt-${workspaceId}-${Date.now()}`,
              signal: controller.signal,
              keepWorktreeOnFailure: true,
              setupApproval,
              onProgress: (event) =>
                updateWorkspaceSetup(set, teamKey, workspaceId, {
                  setupState: step1StateFromLocalPhase(event.phase),
                }),
            });
          }
          throwIfCancelled();
          updateWorkspaceSetup(set, teamKey, workspaceId, {
            status: "ready",
            setupState: setupAfterStep1(teamKey, workspaceId),
          });
        } catch (error) {
          if (storyWorkspaceAborts.get(dedupKey) === controller) {
            updateWorkspaceSetup(set, teamKey, workspaceId, {
              status: "error",
              statusError: getErrorMessage(error),
            });
          }
          throw error;
        }
        get().openWorkspaceWatch(teamKey, workspaceId);
        openTimer?.report(); // ① on-disk adopt path — probe chain ends here
        return onDisk.path;
      }

      // Where a new workspace lives: asked once, at create, and only when the
      // team has machines (the driver answers This Mac at once otherwise).
      let machineId = requestedMachine;
      if (!machineId && !existing && opts?.onCreating && !opts.location) {
        const label =
          (resolvedContent ? parseStoryTitle(resolvedContent) : null) ??
          frontmatterStoryId ??
          workspaceId;
        const driver = remoteWorkspaceDriver();
        const choice: WorkspaceLocationChoice | null = driver
          ? await driver.chooseLocation(teamKey, label)
          : { kind: "local" };
        if (choice === null) throw failedStepError("Worktree open cancelled", "cancelled");
        throwIfCancelled();
        if (choice.kind === "machine") machineId = choice.machineId;
      }

      // SS.8: orgRoot threads to the backend so it can resolve the yaml from
      // the new `_codesense/` path before falling back to legacy. SS-11b: fall
      // back to the org-store `org_root` when the repo's contextRepoPath isn't
      // hydrated yet — the same canonical root capture uses, always present for
      // a loaded org.

      // SW-WT-1: worktree.yaml is optional. SW-SO-2 (③): ONE probe returns both
      // the calm/loud status AND the parsed config in a single backend read —
      // collapsing the former `hasWorktreeConfig` + `getWorktreeConfig` double
      // read. Split a calm `missing` (proceed bare with defaults — no modal, no
      // teardown) from a loud `invalid` (surface the error — the user expressed
      // intent and it's broken). A probe *transport* failure degrades to `missing`
      // so a transient backend hiccup never blocks creation (AC-6); this is the
      // former `hasWorktreeConfig` catch semantics, now applied to the single call
      // — it retires the old has/get asymmetry (a transient failure on the second
      // `getWorktreeConfig` read used to hard-error) in favour of one consistent
      // "never block creation on a hiccup" policy. Behaviour on a *malformed*
      // config is unchanged: the backend returns `invalid` and we throw below.
      let probe: Awaited<ReturnType<typeof probeWorktreeConfig>>;
      try {
        probe = await probeWorktreeConfig(resolvedSourceRepoPath, orgRoot);
      } catch (e) {
        logger.warn("viewer", `ensureWorktree: config probe failed, proceeding bare — ${e}`);
        probe = { status: { kind: "missing" }, config: null };
      }
      openTimer?.mark("probeWorktreeConfig");
      if (probe.status.kind === "invalid") {
        const message = `Worktree config is invalid: ${probe.status.error}`;
        toast.error(message);
        throw failedStepError(message, "config");
      }

      // `missing` and `present` both proceed with the config the SAME probe read
      // returned — null when absent → resolve.ts null-defaults branch_prefix /
      // start_point / worktree_dir (AC-6: never throw on a *missing* config). No
      // second round-trip.
      const config = probe.config;
      // M.2: pass pre-read content to avoid re-reading the file (AC-8).
      // No configured base (`null`): the backend starts a new branch from the repo's
      // default branch and refuses, naming every ref it tried, when none resolves.
      const defaultStartPoint = config?.default_start_point || null;
      const resolved = await resolveWorktreeParams(storyFilePath, config, 0, resolvedContent);
      openTimer?.mark("resolveWorktreeParams");
      const branchName = resolved.branchName;
      // A base from worktree.yaml goes to the backend as `defaultStartPoint`, which resolves
      // it as the MCP open does (a branch name prefers its cached `origin/` ref); a
      // dependency branch or an override is sent verbatim as `startPoint`.
      const fromConfig =
        !overrideStartPoint &&
        resolved.startPoint !== null &&
        resolved.startPoint === defaultStartPoint;
      // F-29: use override start point when provided (e.g. "Create from default branch").
      const startPoint = overrideStartPoint ?? (fromConfig ? null : resolved.startPoint);

      // F-29: pre-flight branch guard — when the start point came from a
      // dependency chain (not the default fallback), verify the branch
      // exists before attempting `git worktree add`. Cheap local ref lookup.
      // Skip the check when the caller provided an override (they know what they're doing).
      if (!overrideStartPoint && startPoint !== null) {
        const exists = await checkBranchExists(resolvedSourceRepoPath, startPoint);
        openTimer?.mark("checkBranchExists");
        if (!exists) {
          throw failedStepError(
            `Blocked — dependency branch "${startPoint}" hasn't been created yet`,
            "blocked-dep",
          );
        }
      }

      if (machineId) {
        // A machine is sent a named base: the configured or default branch, resolved
        // against this clone's local refs (a repeat keeps the base it was first prepared
        // from).
        let baseRef = startPoint ?? remoteRecord?.location.baseRef;
        if (!baseRef) {
          try {
            baseRef = await storyDefaultStartPoint(resolvedSourceRepoPath, defaultStartPoint);
          } catch (e) {
            const message = getErrorMessage(e);
            toast.error(message);
            throw failedStepError(message, "create");
          }
        }
        return openStoryWorkspaceOnMachine({
          get,
          set,
          teamKey,
          workspaceId,
          storyFilePath,
          machineId,
          prior: remoteRecord,
          storyId: frontmatterStoryId ?? undefined,
          storyTitle: resolvedContent ? (parseStoryTitle(resolvedContent) ?? undefined) : undefined,
          sourceRepoPath: resolvedSourceRepoPath,
          sourceRepoSlug: resolvedSlug,
          baseRef,
          branch: branchName,
          onCreating: opts?.onCreating,
          spawnFirstTerminal: opts?.spawnFirstTerminal !== false,
          stillOpen: () => !isMidFlightClose(`remote workspace ${workspaceId}`),
          throwIfCancelled,
        });
      }

      const worktreePath = resolveWorktreePath(config, resolvedSourceRepoPath, branchName);
      const runId = `wt-${workspaceId}-${Date.now()}`;

      // SW-SO-1 (AC-2): the `controller` + `storyWorkspaceAborts` registration
      // were hoisted to the top of this promise (so a pre-flight Cancel has
      // something to abort). Final gate before we create the `creating` record:
      // an abort anywhere in the probe chain bails HERE, so a cancelled open
      // never lands a workspace record or spawns setup.
      throwIfCancelled();

      // SLW-CO-5: mount the terminal FIRST. Add the workspace as "creating" —
      // with the worktree dir as its cwd (already known) — BEFORE running setup,
      // then let the open path's `onCreating` switch to it so the terminal panel
      // + inline `WorkspaceInitOverlay` appear immediately, instead of blocking on
      // setup behind a detached toast. `setupWorktree` runs below (still awaited
      // INSIDE this promise for `ensureWorktree`'s cwd contract, but the mount
      // already happened via `onCreating`, so the open path is not visually
      // blocked). The record flips "creating" → "ready" on success. Mirrors the
      // epic precedent's switch-then-defer-shell shape (see ~4200).
      // Best-effort story title from pre-read content (DRY — AC-8).
      const storyTitle = resolvedContent
        ? (parseStoryTitle(resolvedContent) ?? undefined)
        : undefined;
      addStoryWorkspace(set, {
        teamKey,
        workspaceId,
        storyId: frontmatterStoryId ?? undefined,
        cwd: worktreePath,
        storyFilePath,
        storyTitle,
        sourceRepoPath: resolvedSourceRepoPath,
        sourceRepoSlug: resolvedSlug,
        status: "creating",
        setupState: { kind: "step1", phase: "creatingWorktree" },
        // Registration and setup determine readiness; no inheritance wave runs.
        needsSyncOverlay: false,
      });
      opts?.onCreating?.();
      // ① create path — the pre-flight probe chain is complete; setupWorktree's
      // git time (inherent + offloaded) is measured separately from these hops.
      openTimer?.report();

      // Both setup modes share one awaited registration attempt.
      let establishment: Promise<boolean> | undefined;
      const establishOnce = (): Promise<boolean> => {
        establishment ??= establishStoryWorktree(worktreePath);
        return establishment;
      };

      // Mount after registration, retaining the existing cancellation guards.
      const flipReadyAndMount = (): boolean => {
        if (isMidFlightClose(`worktree ready ${workspaceId}`)) return false;
        updateWorkspaceSetup(set, teamKey, workspaceId, {
          status: "ready",
          setupState: setupAfterStep1(teamKey, workspaceId),
        });
        const readyInStore = readTeamKeyed(get().workspaces, teamKey, []).some(
          (w) => w.id === workspaceId && w.status === "ready",
        );
        if (!readyInStore) return false;
        // SW-WL-3 AC-1: open the just-created story surface (idempotent).
        get().openWorkspaceWatch(teamKey, workspaceId);
        // SLW-CO-5: spawn the shell now the dir exists — open path only. SW-AM-7:
        // the re-home cold-open opts out (`spawnFirstTerminal:false`) so the agent's
        // own PTY becomes the sole tab, not a redundant fresh shell.
        if (opts?.onCreating && opts?.spawnFirstTerminal !== false) {
          ensureFirstTerminalTab(get, teamKey, workspaceId);
        }
        return true;
      };

      // RF-perf-14 (decouple): run the worktree's `setup:` commands in the
      // BACKGROUND (Phase B) after the workspace is already "ready" + mounted.
      // Non-blocking, under the low-cap background gate, non-destructive on
      // failure — progress + failure surface as a badge on the (still-usable)
      // terminal, never hiding the shell. Registered as a retry thunk so the
      // badge's Retry re-runs it.
      const retryKey = workspaceAbortKey(teamKey, workspaceId);
      const startBackgroundSetup = (): void => {
        // Own controller: closing the workspace/team aborts THIS run (kills the
        // setup subprocess) via `abortInFlightWorktreeOpen`; being the currently
        // registered handle is also the ownership token (a close→reopen replaces
        // it, so a superseded attempt's settle below skips writing state).
        const bgController = new AbortController();
        backgroundSetupHandles.set(retryKey, { abort: bgController, retry: null });
        // Only the CURRENT attempt may write badge/retry state — a stale attempt
        // (its handle replaced by a reopen, or deleted by a close) must not stamp
        // the reopened/closed workspace.
        const stillOwns = () => backgroundSetupHandles.get(retryKey)?.abort === bgController;
        const setupLabel = worktreeSetupPhaseLabel("setup");
        // SW-WT-12 (S2): seed the per-command checklist from the probed `setup:`
        // list (all queued). Step events flip each row live; an empty list (no
        // config) yields no checklist. Every running-state write below re-includes
        // `steps` because `updateWorkspaceBackgroundSetup` replaces the whole state.
        const steps: WorktreeSetupStep[] = (config?.setup ?? []).map((command, index) => ({
          index,
          command,
          status: "queued" as const,
        }));
        // SW-WT-12 (S2): reset any prior per-step output (a retry re-runs).
        clearBackgroundSetupOutput(workspaceId);
        const writeRunning = (label: string) => {
          if (!stillOwns()) return;
          updateWorkspaceBackgroundSetup(set, teamKey, workspaceId, {
            status: "running",
            label,
            steps: steps.length > 0 ? [...steps] : undefined,
          });
        };
        // RF-perf-14d: mark QUEUED before we try for a slot — with a low cap, a
        // batch open leaves the over-cap worktrees waiting, and a waiting setup
        // must read as "Queued…" not "Setting up…". Flip to `running` the moment
        // the slot is acquired (first line inside the slotted fn, below).
        updateWorkspaceBackgroundSetup(set, teamKey, workspaceId, {
          status: "queued",
          label: setupLabel,
          steps: steps.length > 0 ? [...steps] : undefined,
        });
        void withBackgroundSetupSlot(() => {
          // Slot acquired: report setup progress.
          writeRunning(setupLabel);
          return runWorktreeSetupPhase({
            repoPath: resolvedSourceRepoPath,
            orgRoot,
            branchName,
            worktreePath,
            startPoint,
            runId,
            signal: bgController.signal,
            onProgress: (event) => {
              // SW-WT-12 (S2): buffer the line for on-demand per-step output
              // (non-reactive — see `backgroundSetupStepOutput`). No re-render.
              appendBackgroundSetupOutput(workspaceId, event.stepIndex, event.line);
              writeRunning(worktreeSetupPhaseLabel(event.phase));
            },
            onStep: (event) => {
              const i = event.stepIndex;
              if (i >= 0 && i < steps.length) {
                steps[i] =
                  event.kind === "started"
                    ? { ...steps[i], status: "running", command: event.command ?? steps[i].command }
                    : {
                        ...steps[i],
                        status: event.passed ? "done" : "failed",
                        exitCode: event.exitCode ?? null,
                      };
              }
              writeRunning(worktreeSetupPhaseLabel("setup"));
            },
            setupApproval,
            keepWorktreeOnFailure: true,
          });
        })
          .then(() => {
            if (!stillOwns()) return;
            // Successful setup (including no-step completion) clears retry state.
            backgroundSetupHandles.delete(retryKey);
            clearBackgroundSetupOutput(workspaceId);
            updateWorkspaceBackgroundSetup(set, teamKey, workspaceId, undefined);
          })
          .catch((e) => {
            if (!stillOwns()) return;
            const err = e as WorktreeSetupError;
            if (err.failedStep === "cancelled") {
              // Backgrounded setup cancelled (e.g. workspace/team closed) → keep
              // the usable worktree, clear the badge + handle + step output.
              backgroundSetupHandles.delete(retryKey);
              clearBackgroundSetupOutput(workspaceId);
              updateWorkspaceBackgroundSetup(set, teamKey, workspaceId, undefined);
              return;
            }
            // Failure: a RETRYABLE badge over the LIVE terminal (a failed `npm ci`
            // must not hide the shell). The worktree is kept (keepWorktreeOnFailure).
            // Nothing in flight now, but keep the handle to carry the retry thunk
            // (its `abort` stays the ownership token until a retry/close replaces it).
            backgroundSetupHandles.set(retryKey, {
              abort: bgController,
              retry: startBackgroundSetup,
            });
            updateWorkspaceBackgroundSetup(set, teamKey, workspaceId, {
              status: "failed",
              error: err.message,
              diagnostics: err.diagnostics,
            });
          });
      };

      // RF-perf-14 (decouple): an INTERACTIVE open (`onCreating`, no explicit
      // `awaitSetup`) completes as soon as `git worktree add` does, then runs the
      // user's `setup:` in the background — the terminal is usable in ~1–2s
      // regardless of what `setup:` contains. Every other caller (agent launch,
      // retry, non-`onCreating`) stays on the blocking path: it needs a fully
      // prepared worktree on resolve.
      const backgroundDecouple = !!opts?.onCreating && !opts?.awaitSetup;

      try {
        if (backgroundDecouple) {
          // ── DECOUPLED path — Phase A (create) blocks; Phase B (setup) doesn't ──
          // SLW-RI-6: bound the git-create burst (the derive/materialize wave it
          // triggers stays unslotted). Phase B runs under its OWN low-cap gate so
          // a batch never launches a pile of heavy `setup:` subprocesses at once.
          await withCreationSlot(() =>
            createWorktree({
              repoPath: resolvedSourceRepoPath,
              branchName,
              worktreePath,
              startPoint,
              defaultStartPoint,
              signal: controller.signal,
            }),
          );
          await establishOnce();
          throwIfCancelled();
          // Registration is durable before exposing a usable story workspace.
          if (flipReadyAndMount()) {
            startBackgroundSetup();
          }
          return worktreePath;
        }

        // ── BLOCKING path (agent launch / retry / explicit awaitSetup) ────────
        // Await registration, then setup, before resolving the agent launch path.
        await withCreationSlot(() =>
          setupWorktree({
            repoPath: resolvedSourceRepoPath,
            orgRoot,
            branchName,
            worktreePath,
            startPoint,
            defaultStartPoint,
            runId,
            signal: controller.signal,
            onCreated: establishOnce,
            onProgress: (event) => {
              // SLW-CO-5 / SW-XS-10: drive the setup line (was the toast).
              updateWorkspaceSetup(set, teamKey, workspaceId, {
                setupState: step1StateFromLocalPhase(event.phase),
              });
            },
            setupApproval,
          }),
        );
        if (isMidFlightClose("worktree setup completed")) return worktreePath;
        await establishOnce();
        throwIfCancelled();
        flipReadyAndMount();
        return worktreePath;
      } catch (e) {
        const err = e as WorktreeSetupError;
        if (err.failedStep === "cancelled") {
          // SLW-CO-5 / AC-8: tear down the just-mounted "creating" workspace so a
          // cancel leaves nothing orphaned. When the cancel came from the overlay's
          // Cancel (`closeWorkspace`), the record is already gone and this is a
          // no-op; for any other abort source it removes the orphan + reassigns the
          // active workspace. (Pre-SLW-CO-5 there was no phantom because the record
          // was only added AFTER setup resolved — now it's added before, so cancel
          // must clean it up.)
          //
          // SW-SO-1 (review r2): ownership-scope this teardown. If a reopen of the
          // SAME story has superseded this (now-aborted) attempt, `storyWorkspaceAborts`
          // holds the REOPEN's controller — a blind `closeWorkspace` here would
          // destroy the live reopen's record. Only tear down while THIS attempt
          // still owns the open. (When the cancel came via `closeWorkspace` it
          // already deleted the controller + record, so this correctly skips.)
          if (storyWorkspaceAborts.get(dedupKey) === controller && !opts?.keepRecordOnCancel) {
            get().closeWorkspace(teamKey, workspaceId);
          }
          throw err;
        }
        // SLW-CO-5: surface the failure on the record itself — the frame swaps to
        // the shared `WorkspaceErrorPlaceholder` (statusError + Close). Was a toast.
        updateWorkspaceSetup(set, teamKey, workspaceId, {
          status: "error",
          statusError: err.message,
        });
        throw err;
      }
    })();

    ensureWorktreeInFlight.set(dedupKey, promise);
    try {
      return await promise;
    } finally {
      // SW-SO-1: reconcile the hoisted abort handle + opening overlay on EVERY
      // exit — success, on-disk adopt, pre-flight throw, setup error, and abort
      // all funnel through here (ONE reconcile, no per-exit teardown). This
      // wraps the WHOLE probe promise, so the pre-flight throws that bypass the
      // IIFE's inner try/catch are covered too.
      //
      // ALL of this is OWNERSHIP-SCOPED so a superseded attempt can't clobber the
      // current one (review r1 — the cancel-then-reopen-same-story race): a reopen
      // registers a fresh `promise`/`controller` under the same key, so this
      // doomed attempt must touch NOTHING that the fresh one now owns. `stillOwns`
      // is true only while this attempt's `controller` is still the registered
      // one — a reopen (overwrites it) or a `closeWorkspace` teardown (deletes it,
      // and clears the flag itself) flips it false.
      const stillOwns = storyWorkspaceAborts.get(dedupKey) === controller;
      if (ensureWorktreeInFlight.get(dedupKey) === promise) {
        ensureWorktreeInFlight.delete(dedupKey);
      }
      if (stillOwns) {
        storyWorkspaceAborts.delete(dedupKey);
        // On success the overlay already handed off to the in-tile record overlay,
        // so this clear is invisible; gated on `onCreating` to mirror the set.
        if (opts?.onCreating) {
          get().endWorkspaceOpen(teamKey, workspaceId);
        }
      }
    }
  },

  retryWorktreeSetup: (teamKey, workspaceId) => {
    // RF-perf-14 (decouple): re-run a failed background `setup:` from the retry
    // badge. The thunk re-registers a fresh handle (`retry: null`) as it starts,
    // so a double-click naturally no-ops the second call (no re-entry guard
    // needed) and a re-failure re-arms the thunk.
    const retry = backgroundSetupHandles.get(workspaceAbortKey(teamKey, workspaceId))?.retry;
    retry?.();
  },

  reconcileWorktreeBranch: async (teamKey, storyFilePath, opts) => {
    // Normalise once so the workspace lookup (keyed by `storyIdFromPath`) and
    // the downstream resolve/link calls all see the same canonical path.
    const normalized = normalizeStoryFilePath(storyFilePath);
    const workspaceId = storyIdFromPath(normalized);

    // The story's known worktree record carries the source repo + old path. The
    // mismatch chip only offers this action when a worktree exists, so a record
    // is the common case; without one there is nothing to reconcile in place —
    // fall back to a normal (fresh) create.
    const ws = readTeamKeyed(get().workspaces, teamKey, []).find((w) => w.id === workspaceId);
    const sourceRepoPath = ws?.sourceRepoPath;
    if (!sourceRepoPath || !ws?.cwd) {
      const path = await get().ensureWorktree(teamKey, normalized);
      return { path, mode: "recreate" };
    }
    const oldPath = ws.cwd;

    // Resolve the INTENDED branch from the (freshly edited) frontmatter. This is
    // the same resolver `ensureWorktree` uses — `branch_name` drives it. Read the
    // content once and reuse for the story_id (so the re-link carries the real
    // label, never clobbering the record with an empty id).
    const orgRoot = resolveOrgRoot(sourceRepoPath) || resolveOrgRootForTeam(teamKey);
    const config = await getWorktreeConfig(sourceRepoPath, orgRoot);
    const content = await readFile(normalized).catch(() => undefined);
    const resolved = await resolveWorktreeParams(normalized, config, 0, content);
    const intent = resolved.branchName;
    const storyId = (content ? parseStoryIdFromFrontmatter(content) : null) ?? ws.storyId ?? "";
    const slug = ws.sourceRepoSlug ?? deriveSourceRepoSlug(sourceRepoPath);

    // Re-assert the worktree→story link, which re-stamps the canonical `branch`
    // frontmatter (SW-OR-1b; formerly `dev_branch`) to the worktree's live branch.
    // AWAITED (not fire-and-forget) so the caller's model reload sees the fresh
    // `branch` and the mismatch chip clears deterministically (the chip is
    // suppressed once `branch` is present) instead of racing an in-flight write.
    const reStamp = (cwd: string) =>
      establishWorktreeStoryLink(cwd, normalized, storyId, slug).catch((e: unknown) => {
        logger.warn("viewer", `reconcile re-link failed (non-fatal): ${getErrorMessage(e)}`);
      });

    const samePath = (a: string, b: string) => a.replace(/\/+$/, "") === b.replace(/\/+$/, "");
    const worktrees = await listWorktrees(sourceRepoPath);
    const entry = worktrees.find((w) => samePath(w.path, oldPath));

    // Recreate: close the old workspace (kill PTYs — no WIP to preserve here),
    // prune the old worktree if it still exists, re-establish fresh via
    // `ensureWorktree` (single source of truth for create + link + derive + open),
    // then re-stamp so `dev_branch` reflects the new branch. Closing FIRST is
    // load-bearing: it evicts the "ready" record so `ensureWorktree` can't fast-
    // path-return the stale cwd.
    const recreate = async (): Promise<{ path: string; mode: "recreate" }> => {
      if (entry) await removeWorktree(sourceRepoPath, oldPath, false);
      get().closeWorkspace(teamKey, workspaceId, { killTerminals: true });
      const path = await get().ensureWorktree(teamKey, normalized);
      await reStamp(path);
      return { path, mode: "recreate" };
    };

    // Forced recreate (AC-7): the user chose "discard local changes" after a
    // blocked/dirty migrate. Skip the dirty→migrate branch entirely.
    if (opts?.forceRecreate) return recreate();

    // Stale cwd (worktree gone) → recreate fresh.
    if (!entry) return recreate();

    // Already on the intended branch → the frontmatter `dev_branch` may still be
    // stale (e.g. a prior out-of-band `git switch`). Re-stamp it so the chip
    // clears, then noop.
    if (branchRefMatches(entry.branch, intent)) {
      await reStamp(oldPath);
      return { path: oldPath, mode: "noop" };
    }

    // DIRTY → migrate in place: preserve the running session + uncommitted WIP.
    // `switchWorktreeBranch` throws a typed error rather than clobber; the caller
    // catches it and offers recreate (which discards).
    const { dirty } = await checkWorktreeDirty(oldPath);
    if (dirty) {
      const res = await switchWorktreeBranch(sourceRepoPath, oldPath, intent);
      await reStamp(res.worktree_path);
      set((s) => ({ worktreeEstablishTick: s.worktreeEstablishTick + 1 }));
      return { path: res.worktree_path, mode: "migrate" };
    }

    // CLEAN → recreate at the branch-derived path + clean up the orphan.
    return recreate();
  },

  showNoClonePrompt: (teamKey: string, sourceRepoSlug: string, retry?: () => void) => {
    set({
      noClonePromptRepo: { teamKey, sourceRepoSlug },
      noClonePromptRetry: retry ?? null,
    });
  },

  dismissNoClonePrompt: () => {
    // Clear both — explicit dismiss means "don't auto-retry". The retry
    // path is `consumeNoClonePromptRetry`.
    set({ noClonePromptRepo: null, noClonePromptRetry: null });
  },

  consumeNoClonePromptRetry: () => {
    const retry = get().noClonePromptRetry;
    set({ noClonePromptRepo: null, noClonePromptRetry: null });
    if (retry) {
      try {
        retry();
      } catch (e) {
        logger.warn("viewer", `noClonePromptRetry threw: ${e}`);
      }
    }
  },

  requestCloseWorkspace: (teamKey, workspaceId) => {
    // T.2 D-11: root non-closability is kind-based; use the same predicate
    // as the close-button gate.
    const ws = get().workspaces[teamKey]?.find((w) => w.id === workspaceId);
    if (ws && !getKindCapabilities(ws.kind).closableByUser) return;
    // PT-10: × = "close, keep running". No confirm dialog — `closeWorkspace`
    // detaches (PTYs survive) and emits the Model C toast when ≥1 is kept alive.
    get().closeWorkspace(teamKey, workspaceId);
    // SW-WV-A6 (AC-2): if that close drained the team to zero foreground tiles,
    // ensure the ever-present team-folder workspace so the operator always has a
    // CLI — never left at `ROOT_NO_REPOS_SENTINEL`. `closeWorkspace` and
    // `openOrFocusTeamFolderWorkspace` are both synchronous, so the team-folder
    // record is present before any subscriber observes the drained state (AC-5,
    // structural — no await/setTimeout/microtask). The ensure no-ops when the
    // team has no `teamContentPath`, leaving the empty-state branch intact (AC-7).
    if (!hasForegroundTile(get().workspaces[teamKey] ?? [])) {
      get().openOrFocusTeamFolderWorkspace(teamKey);
    }
  },

  reopenSoftClosedWorkspace: (teamKey, workspaceId, target, opts) => {
    const shadow = get().softClosedWorkspaces[teamKey]?.[workspaceId];
    if (!shadow) return;
    // Its tabs never left their host's layout; their displays come back as they were.
    set((s) => {
      const ws = readTeamKeyed(s.workspaces, teamKey, []);
      // Re-add the record only if it isn't already back — a story reopen path
      // recreates it via `ensureWorktree` first, so this stays idempotent.
      const nextWs = ws.some((w) => w.id === workspaceId) ? ws : [...ws, shadow.workspace];
      return {
        workspaces: updateTeamKeyed(s.workspaces, teamKey, nextWs),
        softClosedWorkspaces: clearSoftClosedEntry(s, teamKey, workspaceId),
      };
    });
    // Focus + (re)attach the surface watcher via the shared open path. The
    // remounted ShellTerminalPanels reattach to the survived PTYs (PT-1).
    get().switchWorkspace(teamKey, workspaceId);
    if (!opts?.keepLayout) get().expandTerminalOnOpen(teamKey); // SW-FX-35: pair-safe "show the terminal"
    // SW-EW-17: a drag-restore onto a tile edge places the restored workspace at
    // the chosen position; a plain click-restore (no target) leaves it appended
    // by the render-time reconcile (the unchanged PT-13 path).
    if (target) get().placeWorkspaceTile(teamKey, workspaceId, target);
    get().flushPersist();
  },

  killTerminalsForSoftClosedWorkspace: (teamKey, workspaceId, opts) => {
    const s = get();
    if (!s.softClosedWorkspaces[teamKey]?.[workspaceId]) return;
    // Its tabs, and the tabs Hidden from it.
    const tabs = [
      ...selectWorkspaceTabs(s, teamKey, workspaceId),
      ...selectHiddenTerminalTabs(s, teamKey, workspaceId),
    ];
    const sessionIds = tabs.map((t) => t.sessionId);
    killTerminalTabs(tabs, "close minimized workspace terminals", opts);
    pruneAgentSessions(sessionIds);
    // SW-FX-52: this ends a MINIMIZED card's terminals — the one place a live
    // `running`/`needs-permission` status is most likely on display — so prune
    // the flat status/progress slices here rather than rely on `pty-exit`.
    set((st) => ({
      softClosedWorkspaces: clearSoftClosedEntry(st, teamKey, workspaceId),
      // Ended for good: it leaves the team's order, and the base order the tray last saw, too.
      workspaceOrderByTeam: orderWithoutWorkspace(st.workspaceOrderByTeam, teamKey, workspaceId),
      trayBaseOrderByTeam: orderWithoutWorkspace(st.trayBaseOrderByTeam, teamKey, workspaceId),
      aiStatusBySession: pruneSessionMap(st.aiStatusBySession, sessionIds),
      progressBySession: pruneSessionMap(st.progressBySession, sessionIds),
      progressTickBySession: pruneSessionMap(st.progressTickBySession, sessionIds),
    }));
  },

  resolveWorktreeSetupApproval: (approved) => {
    const resolve = pendingSetupApprovalResolve;
    pendingSetupApprovalResolve = null;
    set({ worktreeSetupApproval: null });
    resolve?.(approved);
  },

  cycleTab: (teamKey, tabType, direction) => {
    const state = get();
    if (tabType === "terminal") {
      state.cycleTerminalTab(teamKey, direction === 1 ? "next" : "prev");
    } else {
      // T.4: viewer/markdown tab strip is team-keyed (Phase 5 flip).
      // `setActiveTab` needs a `repoPath` for `revealOpenedFile`'s
      // `resolveRepo` lookup. T.6: derive the **synthetic** workspace
      // key (NOT `workspace.sourceRepoPath`, the M.2 on-disk path which
      // misses every `workspaceRepos[...]` entry — same class of bug
      // Checkpoint 7 fixed for 8 consumer sites). Pass `null` when the
      // workspace has no resolvable slug so `setActiveTab` skips the
      // reveal (tab state still updates).
      const tabs = readTeamKeyed(state.openTabs, teamKey, []);
      if (tabs.length <= 1) return;
      const activePath = readTeamKeyed(state.activeTab, teamKey, "");
      const idx = tabs.findIndex((t) => t.path === activePath);
      const next = (idx + direction + tabs.length) % tabs.length;
      const teamWorkspaces = readTeamKeyed(state.workspaces, teamKey, []);
      const activeWsId = activeWorkspaceIdForTeam(teamKey, state.activeWorkspace, state.workspaces);
      const activeWs = teamWorkspaces.find((w) => w.id === activeWsId);
      const repoPathForTeam = activeWs?.sourceRepoSlug
        ? buildSyntheticRepoPathFromPrompt({
            teamKey,
            sourceRepoSlug: activeWs.sourceRepoSlug,
          })
        : null;
      state.setActiveTab(teamKey, repoPathForTeam, tabs[next].path);
    }
  },

  reorderTerminalTab: (teamKey, fromIdx, toIdx, workspaceId) => {
    const s = get();
    const wsId = workspaceId ?? activeWorkspaceIdForTeam(teamKey, s.activeWorkspace, s.workspaces);
    const tabs = selectWorkspaceTabs(s, teamKey, wsId);
    const moved = tabs[fromIdx];
    const onto = tabs[toIdx];
    if (!moved || !onto || fromIdx === toIdx) return;
    // The host orders every entry of the workspace (background ones too): move
    // to the strip position of the tab being displaced.
    const home = memberOfSession(s.terminalLayout, onto.sessionId);
    // Each member orders its own tabs: a tab moves only among its own host's.
    if (home?.key !== memberOfSession(s.terminalLayout, moved.sessionId)?.key) return;
    const toEntry = home?.layout.tabs.findIndex((t) => t.session === onto.sessionId) ?? -1;
    if (toEntry < 0) return;
    terminalMoveTab(moved.sessionId, toEntry).catch(logLayoutVerb("reorder terminal tab"));
  },

  reorderWorkspace: (teamKey, workspaceId, place, drawnBase) => {
    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    const s = get();
    // Before the team's computed order is known (its tree never synced, no base was saved), a
    // team with epic members has only the tree-less order on hand. The move is applied to it, as
    // the person saw it, and kept to be made again over the real base when the tree syncs
    // (`provisionalMoves`). Roots and folders need no tree.
    // A base the caller drew from the tree is the real one: the move is final.
    const base = drawnBase ?? s.trayBaseOrderByTeam[teamKey];
    const provisional = base === undefined && needsTreeForOrder(s, teamKey);
    const order = teamWorkspaceOrder({
      base,
      workspaces: s.workspaces[teamKey],
      softClosed: s.softClosedWorkspaces[teamKey],
      stored: s.workspaceOrderByTeam[teamKey],
      groups: s.railKeyByWorkspaceId[teamKey],
    });
    const next = moveInOrder(order, workspaceId, place);
    if (!next) return false;
    // Over a real base the stored order is whole: earlier queued moves are already in it.
    if (!provisional) provisionalMoves.delete(teamKey);
    else {
      const pending = provisionalMoves.get(teamKey) ?? {
        from: s.workspaceOrderByTeam[teamKey],
        moves: [],
      };
      pending.moves.push({ workspaceId, place });
      provisionalMoves.set(teamKey, pending);
    }
    set((st) => ({
      workspaceOrderByTeam: updateTeamKeyed(st.workspaceOrderByTeam, teamKey, next),
    }));
    get().persistState();
    return true;
  },

  renameTerminalTab: (teamKey, tabId, name, _workspaceId, automatic = false) => {
    if (!automatic) get().setAgentIdentity(teamKey, tabId, { name });
    terminalRenameTab(tabId, name).catch(logLayoutVerb("rename terminal tab"));
  },

  navigateBack: (teamKey, repoPath) => {
    const s = get();
    const index = readTeamKeyed(s.navIndex, teamKey, -1);
    if (index <= 0) return;

    const history = readTeamKeyed(s.navHistory, teamKey, []);
    const newIndex = index - 1;
    const targetPath = history[newIndex];
    if (!targetPath) return;

    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    set((prev) => commitNavTarget(prev, teamKey, targetPath, newIndex));
    revealOpenedFile(repoPath, teamKey, targetPath, set, get);
    get().persistState();
  },

  navigateForward: (teamKey, repoPath) => {
    const s = get();
    const history = readTeamKeyed(s.navHistory, teamKey, []);
    const index = readTeamKeyed(s.navIndex, teamKey, -1);
    if (index >= history.length - 1) return;

    const newIndex = index + 1;
    const targetPath = history[newIndex];
    if (!targetPath) return;

    if (teamKey === getActiveTeamKey()) beginContextNavigation();
    set((prev) => commitNavTarget(prev, teamKey, targetPath, newIndex));
    revealOpenedFile(repoPath, teamKey, targetPath, set, get);
    get().persistState();
  },

  persistState: () => {
    // SW-FU-B2: no write before the saved state has loaded. A pre-load setter
    // (the workspace sync opening a team) would otherwise schedule a save of
    // in-memory defaults over the saved viewer. The load persists afterwards
    // when a write was held back.
    if (!get().stateLoaded) {
      persistHeldBeforeLoad = true;
      return;
    }
    debouncedPersist.trigger();
  },

  flushPersist: () => {
    if (!get().stateLoaded) {
      persistHeldBeforeLoad = true;
      return Promise.resolve();
    }
    // trigger() ensures a pending write exists, flush() cancels the
    // debounce timer and invokes the persist fn synchronously.
    debouncedPersist.trigger();
    return debouncedPersist.flush();
  },

  // SW-SN-12 AC-1/AC-2: land any pending note-body edit NOW. The steady-state
  // 1s debounce is durable while the app runs, but the window before a teardown
  // (quit / window blur / panel unmount) would otherwise drop up to one debounce
  // interval of edits. `debouncedScratchSave.flush()` awaits the actual disk
  // write, so a caller that `await`s this is guaranteed the edit landed before
  // teardown proceeds. Unlike `flushPersist`, do NOT `trigger()` first — there's
  // nothing to persist unless an edit is already pending, and a spurious write
  // to a possibly-null active note is exactly what we don't want.
  // Drain an in-flight null-active-note lazy-create FIRST: its buffered content
  // isn't in the debounce until the create resolves, so without this a teardown
  // landing mid-create would miss it.
  flushNotes: async () => {
    if (lazyCreateInFlight) await lazyCreateInFlight;
    await debouncedScratchSave.flush();
  },

  loadPersistedState: async () => {
    logger.info("viewer", "loadPersistedState: loading");
    // The saved orders replace the live ones: nothing computed or queued over them survives.
    tileOrderCache.clear();
    provisionalMoves.clear();
    try {
      const data = JSON.parse(JSON.stringify(await readPrefsState()));
      const prefsOwner = usePrefsStore.getState().principal;
      persistedViewer = structuredClone(data.viewer ?? {});
      if (data.viewer) {
        const savedGrids = savedViewerGrids(
          (data.viewer as { terminalViewerGrid?: unknown }).terminalViewerGrid,
        );
        if (Object.keys(savedGrids).length > 0) {
          get().applyLayout({ kind: "viewer-grids", grids: savedGrids });
        }
        const savedFocus = (data.viewer as { terminalViewerFocus?: unknown }).terminalViewerFocus;
        if (savedFocus && typeof savedFocus === "object" && !Array.isArray(savedFocus)) {
          for (const [key, session] of Object.entries(savedFocus)) {
            if (typeof session === "string" && session.length > 0)
              get().applyLayout({ kind: "focused", key, session });
          }
        }
        set({
          tabGridLayout: savedTabGrids(
            (data.viewer as { terminalTabGridSizes?: unknown }).terminalTabGridSizes,
          ),
        });
        // SS-FU-12 v4→v6: rewrite the org-slug prefix (`owner--repo` →
        // `owner/repo`) on TWO levels:
        //   (a) outer teamKey of every persisted team-keyed slice, so the
        //       T.1/T.4 chain + prune below match against the now-`owner/repo`
        //       backend slugs and don't drop every persisted team as
        //       unrecognised;
        //   (b) absolute paths embedded inside slice values
        //       (`statusNavExpandedPaths`, `workspaces.storyFilePath`,
        //       `openedTeams`, etc.) — v5 missed these and left stale
        //       `/orgs/{owner}--{repo}/...` strings pointing at the renamed
        //       org root, producing "file removed from disk" on restore. v6
        //       adds the deep path rewrite.
        // v6 also missed teamKey strings stored as ARRAY values (only
        // `openedTeams` does this — the other slices key by teamKey). v7
        // extended `migrateViewerOrgSlugKeys` to handle that case but didn't
        // dedupe, so a v6 session that added a fresh `owner/repo:team`
        // entry alongside a stale `owner--repo:team` ended up with both,
        // collapsing into duplicate teamKeys after the rewrite and rendering
        // two panels for the same team. v8 adds dedupe.
        // Gated on `< 8`; every rewrite is narrow + idempotent regardless.
        // The raw-blob passes (v8 org-slug rewrite, v10 `_team/` flatten, v11
        // SW-XS-3 forward-sync settings drop, v12 SW-XS-4 reverse-sync settings drop,
        // v13 SW-XS-17 worktree cap / idle reaper / merged auto-remove drop, v14 SW-XS-15 agent
        // context pivot) run as one ordered chain keyed on the saved
        // version — see `viewer-schema-migrations.ts`.
        const hadCliChrome =
          data.viewer.cliChrome !== null &&
          typeof data.viewer.cliChrome === "object" &&
          !Array.isArray(data.viewer.cliChrome);
        runViewerSchemaMigrations(
          data.viewer as Record<string, unknown>,
          (data.viewer as { schemaVersion?: number }).schemaVersion,
          { lastTeamKey: savedOrgTeamKey(data.org), deferCliChromeSeed: true },
        );
        let seededCliChrome = false;
        const {
          schemaVersion: savedSchemaVersion,
          openTabs: savedOpenTabs,
          activeTab: savedActiveTab,
          drillPaths: savedDrillPaths,
          teamPipelineRepoPath: savedTeamPipelineRepoPath,
          navCollapsed: savedNavCollapsed,
          navLens: savedNavLens,
          agentsPaneArrange: savedAgentsPaneArrange,
          agentIdentity: savedAgentIdentity,
          agentPins: savedAgentPins,
          agentsPaneHiddenTeams: savedAgentsPaneHiddenTeams,
          agentsWorkspaceFilter: savedAgentsWorkspaceFilter,
          foregroundLayout: savedForegroundLayout,
          foregroundLayoutByRail: savedForegroundLayoutByRail,
          trayMode: savedTrayMode,
          workspaceOrderByTeam: savedWorkspaceOrderByTeam,
          customGroupsByTeam: savedCustomGroupsByTeam,
          workspaceGroupVersionsByTeam: savedGroupVersions,
          trayBaseOrderByTeam: savedTrayBaseOrderByTeam,
          // Right-pane open/width: prefer the new keys, fall back to the legacy
          // `scratchNoteOpen` / `scratchNoteWidth` so existing installs keep
          // their pane state across the rename.
          rightPaneOpen: savedRightPaneOpen,
          scratchNoteOpen: savedScratchNoteOpen,
          workspaceTraySeenCoachmark: savedWorkspaceTraySeenCoachmark,
          cliShortcutsHintSeen: savedCliShortcutsHintSeen,
          dismissedTerminalServers: savedDismissedTerminalServers,
          rightPaneRatio: savedRightPaneRatio,
          scratchNoteWidth: savedScratchNoteWidth,
          rightPaneSplitRatio: savedRightPaneSplitRatio,
          rightPaneLayout: savedRightPaneLayout,
          statusNavExpandedPaths: savedStatusNavExpandedPaths,
          statusNavPinnedPaths: savedStatusNavPinnedPaths,
          settings,
          softClosedWorkspaces: savedSoftClosedWorkspaces,
          pipelineScope: savedPipelineScope,
          selectedAgentContext: savedSelectedAgentContext,
          workspaces: savedWorkspaces,
          activeWorkspace: savedActiveWorkspace,
          openedTeams: savedOpenedTeams,
        } = data.viewer;
        const { loadSavedInboxPanelIds } =
          await import("@/features/shell/right-pane/inbox-layout-migration");
        const savedInboxPanelIds = await loadSavedInboxPanelIds(
          savedRightPaneLayout,
          savedOrgTeamKey(data.org),
        );

        // T.1 AC-7 / D-4: migrate the six T.1-scope slices to team-keyed
        // shape when `schemaVersion` is missing. After migration, all
        // restore logic below operates on teamKey-shaped maps.
        const needsT1Migration = typeof savedSchemaVersion !== "number" || savedSchemaVersion < 1;
        // T.4 D-11: migrate the persisted T.4 Record slices (post-T.1 +
        // post-T.2) to team-keyed shape when schemaVersion < 3. T.4 sits
        // in the middle of the chain (T.1 → T.2 → T.4 → T.6) so v1 and
        // v2 blobs both still get T.4's re-bucketing of viewer Record
        // slices + AC-3 scalar reshapes. Reuses T.1's `repoPathToTeamKey`
        // map when both migrations run.
        const needsT4Migration = typeof savedSchemaVersion !== "number" || savedSchemaVersion < 3;

        let migratedWorkspaces: Record<string, Workspace[]>;
        let migratedActiveWorkspace: Record<string, string>;
        // T.4 D-11: shared lookup map carried from T.1 → T.4 migration so
        // legacy real-path outer keys resolved via sourceRepoSlug in T.1
        // don't need a second org-store resolution pass in T.4.
        let migrationRepoPathToTeamKey: Map<string, string> = new Map();

        if (needsT1Migration) {
          // T.1 D-4 / AC-7: Pass 2 of the migration resolves legacy
          // real-path outer keys via `sourceRepoSlug → workspaceRepos`.
          // Spec mandates awaiting org-store readiness before that
          // fallback, otherwise cold-start legacy users end up in the
          // sentinel bucket. The await is only meaningful when there are
          // non-synthetic outer keys to recover; an all-synthetic blob
          // resolves in Pass 1 with no store dependency.
          const persistedOuterKeys = [
            ...Object.keys(savedWorkspaces ?? {}),
            ...Object.keys(savedActiveWorkspace ?? {}),
          ];
          const hasLegacyOuterKey = persistedOuterKeys.some(
            (k) => parseSyntheticRepoKey(k) === null,
          );
          if (hasLegacyOuterKey) {
            await waitForOrgStoreReady();
          }
          const result = migrateT1Slices({
            workspaces: (savedWorkspaces ?? {}) as Record<string, Workspace[]>,
            activeWorkspace: (savedActiveWorkspace ?? {}) as Record<string, string>,
            workspaceRepos: useOrgStore.getState().workspaceRepos,
          });
          migratedWorkspaces = result.workspaces;
          migratedActiveWorkspace = result.activeWorkspace;
          // T.4 D-11: capture T.1's repoPath → teamKey resolution so T.4's
          // re-bucketing of the 12 Record slices can short-circuit the
          // second-pass org-store lookup for the same legacy outer keys.
          migrationRepoPathToTeamKey = result.repoPathToTeamKey;
          logger.info(
            "viewer",
            `[migration] T.1 slices migrated to schemaVersion 1${
              result.sentinelOuterKeys.length > 0
                ? ` (${result.sentinelOuterKeys.length} sentinel)`
                : ""
            }`,
          );
        } else {
          // schemaVersion 1+: workspace + activeWorkspace are already team-keyed.
          migratedWorkspaces = (savedWorkspaces ?? {}) as Record<string, Workspace[]>;
          migratedActiveWorkspace = (savedActiveWorkspace ?? {}) as Record<string, string>;
        }

        // T.4 D-11 migration: re-bucket the 11 persisted T.4 slices from
        // repoPath → teamKey. T.6 drops the no-longer-migrated
        // `workspaceViewerStates` outer-key collection.
        let openTabs: Record<string, TabState[]>;
        let activeTab: Record<string, string>;
        let drillPaths: Record<string, string>;
        let migratedRightPaneSplitRatio: Record<string, number>;
        let migratedStatusNavExpandedPaths: Record<string, string[]>;
        let migratedPipelineScope: Record<string, string>;

        if (needsT4Migration) {
          const t4Result = migrateT4Slices({
            openTabs: (savedOpenTabs ?? {}) as Record<string, TabState[]>,
            activeTab: (savedActiveTab ?? {}) as Record<string, string>,
            drillPaths: (savedDrillPaths ?? {}) as Record<string, string>,
            rightPaneSplitRatio: (savedRightPaneSplitRatio ?? {}) as Record<string, number>,
            statusNavExpandedPaths: (savedStatusNavExpandedPaths ?? {}) as Record<string, string[]>,
            pipelineScope: (savedPipelineScope ?? {}) as Record<string, string>,
            repoPathToTeamKey: migrationRepoPathToTeamKey,
          });
          openTabs = t4Result.openTabs;
          activeTab = t4Result.activeTab;
          drillPaths = t4Result.drillPaths;
          migratedRightPaneSplitRatio = t4Result.rightPaneSplitRatio;
          migratedStatusNavExpandedPaths = t4Result.statusNavExpandedPaths;
          migratedPipelineScope = t4Result.pipelineScope;
          // T.4 Round 4 (Edge Case Hunter #5 → Decision→Patch): when legacy
          // real-path outer keys collapse to NO_TEAM_SENTINEL (path that fires
          // when `needsT1Migration === false` — T.1 already ran in a prior
          // session, so the `repoPathToTeamKey` lookup is empty and Pass 2's
          // `parseSyntheticRepoKey` rejects legacy real-paths), surface a
          // warning with a sample of dropped keys so the silent collapse is
          // observable in the field. Bounded to migration time — runs once
          // per app launch when schemaVersion < 3.
          if (t4Result.sentinelOuterKeys.length > 0) {
            const sample = t4Result.sentinelOuterKeys.slice(0, 5).join(", ");
            const more =
              t4Result.sentinelOuterKeys.length > 5
                ? ` (+${t4Result.sentinelOuterKeys.length - 5} more)`
                : "";
            logger.warn(
              "viewer",
              `[migration] T.4 schemaVersion 3: ${t4Result.sentinelOuterKeys.length} legacy outer key(s) collapsed to NO_TEAM_SENTINEL — sample: ${sample}${more}`,
            );
          } else {
            logger.info("viewer", "[migration] T.4 slices migrated to schemaVersion 3");
          }
        } else {
          // Already team-keyed; pass through.
          openTabs = (savedOpenTabs ?? {}) as Record<string, TabState[]>;
          activeTab = (savedActiveTab ?? {}) as Record<string, string>;
          drillPaths = (savedDrillPaths ?? {}) as Record<string, string>;
          migratedRightPaneSplitRatio = (savedRightPaneSplitRatio ?? {}) as Record<string, number>;
          migratedStatusNavExpandedPaths = (savedStatusNavExpandedPaths ?? {}) as Record<
            string,
            string[]
          >;
          migratedPipelineScope = (savedPipelineScope ?? {}) as Record<string, string>;
        }

        // Resolve legacy repo buckets before choosing the last team's global chrome.
        // The seed is idempotent for partially upgraded current-version saves.
        seedCliChrome(
          data.viewer as Record<string, unknown>,
          savedOrgTeamKey(data.org),
          (key) => migrationRepoPathToTeamKey.get(key) ?? null,
        );
        seededCliChrome = !hadCliChrome && data.viewer.cliChrome !== undefined;

        // T.4 AC-11: `pruneTeamKeyed` validates outer keys against the
        // org-store's known teamKeys plus the sentinel. The 17 newly
        // team-keyed slices route through this helper (replacing T.1's
        // dropped `prune()` for the same set — see git history for the
        // pre-T.4 path). The original `prune()` is retired entirely: every
        // restore-time slice in the viewer store is team-keyed post-T.4.
        //
        // T.4 Phase 8 review (Cluster B): wait for `workspaceRepos` to
        // populate before pruning. On a cold start with a schemaVersion=2
        // blob, the wait above only fires when there are legacy outer keys
        // to migrate. Without this extra wait, every persisted team's
        // state could be dropped because `workspaceRepos` hasn't been
        // populated by `useWorkspaceViewerSync` yet.
        //
        // T.4 Phase 10 review (Round 2 MED #4): if `workspaceRepos` is
        // STILL empty after the wait (active team has zero source repos,
        // or the wait timed out), skip the prune entirely. Better to keep
        // possibly-stale entries than to wipe every persisted team's
        // state. Stale entries will be cleaned up next time the user
        // visits their teams; data loss is irreversible.
        await waitForWorkspaceReposReady();
        const workspaceRepos = useOrgStore.getState().workspaceRepos;
        const workspaceReposIsEmpty = Object.keys(workspaceRepos).length === 0;
        const knownTeamKeys = new Set<string>([NO_TEAM_SENTINEL]);
        for (const repo of Object.values(workspaceRepos)) {
          const parsed = parseSyntheticRepoKey(repo.repoPath);
          if (parsed) knownTeamKeys.add(buildTeamKey(parsed.orgSlug, parsed.team));
        }
        if (workspaceReposIsEmpty) {
          logger.warn(
            "viewer",
            "[migration] pruneTeamKeyed skipped — workspaceRepos still empty after wait; keeping persisted team-keyed state to avoid silent data loss",
          );
        }
        // When workspaceRepos is empty, identity-prune (preserve everything).
        const pruneTeamKeyed = <T>(obj: Record<string, T>): Record<string, T> =>
          workspaceReposIsEmpty
            ? obj
            : Object.fromEntries(Object.entries(obj).filter(([k]) => knownTeamKeys.has(k)));

        // SW-GS-2b (AC-11a, belt-and-suspenders): drop any synthetic virtual
        // tab a pre-fix build may have persisted, so it never resurrects as a
        // broken `readFile` tab on load. The persist side already strips these
        // going forward — this guards already-saved state.
        const dropVirtualTabs = (obj: Record<string, TabState[]>): Record<string, TabState[]> =>
          Object.fromEntries(
            Object.entries(obj)
              .map(([k, tabs]) => [k, tabs.filter((t) => !isVirtualTabPath(t.path))] as const)
              .filter(([, tabs]) => tabs.length > 0),
          );
        const dropVirtualActive = (obj: Record<string, string>): Record<string, string> =>
          Object.fromEntries(Object.entries(obj).filter(([, p]) => !isVirtualTabPath(p)));

        // F-18: Restore persisted workspace tabs (now team-keyed).
        // T.1 Task 5.5.b: loop variable renamed `rp → tk`; no auto-root
        // seeding here (a teamKey is not a valid cwd). T.2's
        // `ensureRootWorkspaces(teamKey)` will populate per-source-repo
        // roots on next activation.
        const restoredWorkspaces: Record<string, Workspace[]> = {};
        const restoredActiveWorkspace: Record<string, string> = {};

        for (const [tk, wsList] of Object.entries(migratedWorkspaces)) {
          // Code-review P5: legacy (pre-Phase-B) workspaces have raw
          // `storyFilePath` values. Normalise on restore so the
          // `byAbsPath`/fast-path lookups match the scanner emit.
          // SW-EW-2: codesense is organized by STORY, not by repo. Drop every
          // workspace that no longer has a home in the story-level panel:
          //  - `kind:"epic"` — epic-as-workspace is retired (terminal-less).
          //  - `kind:"root"` — the per-repo "repository workspace" is retired
          //    (run repo-root commands from any terminal via `cd`).
          //  - `kind:"integration"` with no epic — a previously re-homed root
          //    (the transitional Ungrouped repo terminal). Real epic integration
          //    terminals (epicDirPath set) are KEPT — that's the planning surface.
          // Dropping a root/integration kills its PTY, which is intended (these
          // can't be re-created in the story model). Filtered BEFORE the
          // `restoredActiveWorkspace` resolution below so a persisted active id
          // pointing at a dropped workspace falls back to `firstRootIdForTeam`.
          const restored: Workspace[] = (wsList ?? []).flatMap((w) => {
            const kept = restoreWorkspaceRecord(w);
            return kept ? [kept] : [];
          });
          restoredWorkspaces[tk] = restored;
          // T.2: restore activeWorkspace with fallback to firstRootIdForTeam.
          // When no roots are restored yet (typical post-T.1 cold start),
          // the sentinel "root:_no_repos" is written; ensureRootWorkspaces
          // overwrites it on activation. Sentinel is never user-visible
          // because the bar doesn't render until activation populates roots.
          const savedActive = migratedActiveWorkspace[tk];
          restoredActiveWorkspace[tk] =
            savedActive && restored.some((w) => w.id === savedActive)
              ? savedActive
              : firstRootIdForTeam(tk, { [tk]: restored });
        }

        // Minimized workspaces come back minimized (their terminals are the
        // host's), under the same record policy as the open ones.
        const restoredSoftClosed: Record<string, Record<string, SoftClosedWorkspace>> = {};
        for (const [tk, byWorkspace] of Object.entries(
          (savedSoftClosedWorkspaces ?? {}) as Record<string, Record<string, SoftClosedWorkspace>>,
        )) {
          for (const [wsId, shadow] of Object.entries(byWorkspace)) {
            const workspace = shadow?.workspace && restoreWorkspaceRecord(shadow.workspace);
            if (!workspace) continue;
            (restoredSoftClosed[tk] ??= {})[wsId] = {
              workspace,
            };
          }
        }

        // SW-RP-1: the structured layout is global, but the legacy split was
        // team-keyed — warn once when collapsing >1 team's divergent values into a
        // single layout (only when there is no already-migrated layout).
        if (
          savedRightPaneLayout === undefined &&
          Object.keys((savedRightPaneSplitRatio ?? {}) as object).length > 1
        ) {
          logger.warn(
            "viewer",
            "SW-RP-1: collapsing team-keyed right-pane split into a single global layout — keeping the first team's values",
          );
        }

        // SLW-FX-12: heal an already-persisted orphan on load — a real
        // `activeTab[tk]` whose `openTabs[tk]` bucket was dropped (empty/all-
        // virtual, or `dropVirtualActive` kept a real path after
        // `dropVirtualTabs` removed its bucket) is reconstructed into its tab,
        // so the team restores as doc + tab (recovered, not discarded). Runs
        // AFTER `dropVirtual*`/`pruneTeamKeyed` and does NOT depend on
        // `pruneTeamKeyed` (a cold-start pass-through when `workspaceRepos` is
        // still empty).
        if (usePrefsStore.getState().principal !== prefsOwner) return;
        const reconciledLoad = reconcileActiveTabs(
          pruneTeamKeyed(dropVirtualTabs(openTabs)),
          pruneTeamKeyed(dropVirtualActive(activeTab)),
        );
        set({
          stateLoaded: true,
          persistedStateRestored: true,
          // T.6: `openedTeams` replaces `openedRepos`; coerce non-array legacy
          // state to an empty list. SW-FU-B2: MERGE with the live list, never
          // replace it — the workspace sync may already have opened the active
          // team while this load awaited, and dropping it leaves that team with
          // no pane (a blank content area on upgrade).
          openedTeams: mergeOpenedTeams(savedOpenedTeams, get().openedTeams),
          // T.4 AC-11: route every team-keyed slice through pruneTeamKeyed.
          openTabs: reconciledLoad.openTabs,
          activeTab: reconciledLoad.activeTab,
          drillPaths: pruneTeamKeyed(drillPaths),
          // SLW-FX-14: new field (never pre-T.4), so no migration — read straight
          // through, pruned against known teamKeys like its siblings.
          teamPipelineRepoPath: pruneTeamKeyed(
            (savedTeamPipelineRepoPath ?? {}) as Record<string, string>,
          ),
          // D39: the CLI chrome is one value for every team, not team-keyed, so it skips the
          // team-key prune. A field of the wrong type loads as its default.
          ...cliChromeState(sanitizeCliChrome(data.viewer.cliChrome)),
          // SW-FX-8: new field — no legacy migration, absent → {}.
          navCollapsed: pruneTeamKeyed((savedNavCollapsed ?? {}) as Record<string, boolean>),
          navLens: pruneTeamKeyed(
            Object.fromEntries(
              Object.entries((savedNavLens ?? {}) as Record<string, Partial<NavLensState>>).map(
                ([key, lens]) => [key, normalizeNavLens(lens)],
              ),
            ),
          ),
          agentsPaneArrange:
            savedAgentsPaneArrange === "manager" || savedAgentsPaneArrange === "status"
              ? savedAgentsPaneArrange
              : "teams",
          agentIdentity: pruneTeamKeyed(
            (savedAgentIdentity ?? {}) as Record<string, Record<string, AgentIdentityChoice>>,
          ),
          agentPins: sanitizeAgentPinMap(
            pruneTeamKeyed((savedAgentPins ?? {}) as Record<string, unknown>),
          ),
          agentsPaneHiddenTeams: cleanHiddenTeams(savedAgentsPaneHiddenTeams),
          agentsWorkspaceFilter: sanitizeWorkspaceFilter(
            savedAgentsWorkspaceFilter,
            restoredWorkspaces,
            restoredSoftClosed,
            (tk) => workspaceReposIsEmpty || knownTeamKeys.has(tk),
          ),
          // SW-EW-3: new field — no migration, absent → {} (backward-compatible).
          // `preset` is normalized for pre-EW-4 entries (defaultForegroundLayout
          // derives it from `direction`). Expanded is the global chrome's.
          foregroundLayout: pruneTeamKeyed(
            Object.fromEntries(
              Object.entries(
                (savedForegroundLayout ?? {}) as Record<string, Partial<ForegroundLayout>>,
              ).map(([k, v]) => [k, defaultForegroundLayout(v)]),
            ),
          ),
          // SW-EW-40: new persisted nested slice — no migration, absent → {}
          // (mirrors foregroundLayout), each rail's layout restored the same way.
          // railKeyByWorkspaceId is runtime-only (R3) so it has no restore line —
          // it starts {} and syncWorkspaceFrontmatter fills it on the first tree.
          foregroundLayoutByRail: pruneTeamKeyed(
            Object.fromEntries(
              Object.entries(
                (savedForegroundLayoutByRail ?? {}) as Record<
                  string,
                  Record<string, Partial<ForegroundLayout>>
                >,
              ).map(([tk, byRail]) => [
                tk,
                Object.fromEntries(
                  Object.entries(byRail ?? {}).map(([rk, v]) => [rk, defaultForegroundLayout(v)]),
                ),
              ]),
            ),
          ),
          // SW-EW-38: new persisted slice — no migration, absent → {}
          // (mirrors SW-FX-8 navCollapsed). selectedEpicDirName is runtime-only
          // (R3) so it has no restore line — it starts {} and follow-focus fills it.
          trayMode: pruneTeamKeyed((savedTrayMode ?? {}) as Record<string, "flat" | "grouped">),
          // Additive slices, absent → {}: each team's ids de-duplicated and kept to the
          // workspaces that came back (open or minimized; a remote one is restored as a record).
          customGroupsByTeam: pruneTeamKeyed(sanitizeWorkspaceGroups(savedCustomGroupsByTeam)),
          workspaceGroupVersionsByTeam: pruneTeamKeyed(
            (savedGroupVersions ?? {}) as Record<string, number>,
          ),
          workspaceOrderByTeam: pruneTeamKeyed(
            sanitizeWorkspaceOrders(
              savedWorkspaceOrderByTeam,
              restoredWorkspaces,
              restoredSoftClosed,
            ),
          ),
          trayBaseOrderByTeam: pruneTeamKeyed(
            sanitizeWorkspaceOrders(
              savedTrayBaseOrderByTeam,
              restoredWorkspaces,
              restoredSoftClosed,
            ),
          ),
          // Right-pane open/ratio are scalars. Tolerate legacy persisted
          // Record-shaped values by coercing to a default. Prefer the new
          // `rightPaneOpen`/`rightPaneRatio` keys; fall back to the legacy
          // `scratchNoteOpen`/`scratchNoteWidth`/`scratchNoteRatio` so the
          // rename preserves existing pane state.
          // R-5: migrate legacy pixel width to ratio (0-1). Values > 1 are
          // legacy pixel widths; values in [0,1] are already ratios.
          rightPaneOpen:
            typeof savedRightPaneOpen === "boolean"
              ? savedRightPaneOpen
              : typeof savedScratchNoteOpen === "boolean"
                ? savedScratchNoteOpen
                : false,
          workspaceTraySeenCoachmark:
            typeof savedWorkspaceTraySeenCoachmark === "boolean"
              ? savedWorkspaceTraySeenCoachmark
              : false,
          cliShortcutsHintSeen:
            typeof savedCliShortcutsHintSeen === "boolean" ? savedCliShortcutsHintSeen : false,
          dismissedTerminalServers: Array.isArray(savedDismissedTerminalServers)
            ? savedDismissedTerminalServers.filter(
                (entry): entry is string => typeof entry === "string",
              )
            : [],
          rightPaneRatio: migrateScratchNoteRatio(
            savedRightPaneRatio ?? savedScratchNoteWidth ?? data.viewer?.scratchNoteRatio,
            typeof window !== "undefined" ? window.innerWidth : 1200,
          ),
          rightPaneSplitRatio: pruneTeamKeyed(migratedRightPaneSplitRatio),
          // SW-RP-1: prefer a valid persisted layout; else fold the legacy
          // team-keyed split into the default. Migration runs before
          // team activation, so migrateLegacyRightPaneState takes a deterministic
          // first-entry of the team-keyed records (documented one-time loss for
          // multi-team divergence). Malformed → default (coerced inside).
          // SW-AW-5: prune any persisted DYNAMIC-panel section (e.g. the agent
          // module's) — those re-add themselves on feature re-engage; restoring
          // them would render a dead, unregistered section on boot.
          rightPaneLayout: reconcileLayoutToBuiltins(
            migrateLegacyRightPaneState({
              rightPaneLayout: savedRightPaneLayout,
              inboxPanelIds: savedInboxPanelIds,
              rightPaneSplitRatio: savedRightPaneSplitRatio as Record<string, number> | undefined,
            }),
          ),
          statusNavExpandedPaths: pruneTeamKeyed(migratedStatusNavExpandedPaths),
          // SLW-NC-7: personal pins are net-new (no legacy repoPath shape) → no T4
          // migration; restore the saved team-keyed map directly, pruned to live teams.
          statusNavPinnedPaths: pruneTeamKeyed(
            (savedStatusNavPinnedPaths ?? {}) as Record<string, string[]>,
          ),
          settings: normalizeViewerSettings(settings),
          pipelineScope: pruneTeamKeyed(migratedPipelineScope),
          // SW-XS-15: born post-T.4 → always team-keyed; v14 seeds it from the
          // retired install-tier pivot (`viewer-schema-migrations.ts`).
          selectedAgentContext: pruneTeamKeyed(
            sanitizeAgentContextSelections(savedSelectedAgentContext),
          ),
          workspaces: restoredWorkspaces,
          activeWorkspace: restoredActiveWorkspace,
          softClosedWorkspaces: pruneTeamKeyed(restoredSoftClosed),
        });
        // Tab-grid sizes only for workspaces that came back, open or minimized.
        set((st) => ({ tabGridLayout: tabGridsOfLiveWorkspaces(st) }));

        // SW-WL-3 T1.3 / F-7: rebuild the (runtime-only) open-set from the
        // restored surfaces. Story workspaces were explicit user surfaces, so
        // each re-opens; for roots only the restored ACTIVE one is viewed, so
        // it opens while the other minted roots stay closed (F-5). Reconcile-
        // on-attach is non-blocking (AC-10) — these fire after the optimistic
        // paint above. Gated on a resolvable cwd (`openActiveSurfaceWatch`):
        // a restored root with an empty/stale cwd must not latch membership
        // before it can attach, matching the mint/switch paths.
        for (const [tk, wsList] of Object.entries(restoredWorkspaces)) {
          for (const w of wsList) {
            if (getKindCapabilities(w.kind).surfaceWatchOnLoad)
              get().openActiveSurfaceWatch(tk, w.id);
          }
          const activeId = restoredActiveWorkspace[tk];
          if (activeId) get().openActiveSurfaceWatch(tk, activeId);
        }

        // F-18: post-load validation — check that restored story workspace
        // worktrees still exist on disk. Non-blocking: the UI shows them as
        // "ready" immediately; stale ones flip to "error" once validation
        // resolves. Fire-and-forget — validation errors are logged, not thrown.
        void (async () => {
          // T.1: workspaces is team-keyed; outer-key variable named `teamKey`.
          const entries: Array<{ teamKey: string; wsId: string; cwd: string }> = [];
          for (const [tk, wsList] of Object.entries(restoredWorkspaces)) {
            for (const ws of wsList) {
              // A machine's worktree is not on this disk to check.
              if (getKindCapabilities(ws.kind).worktreeStalenessCheck && !isRemoteWorkspace(ws))
                entries.push({ teamKey: tk, wsId: ws.id, cwd: ws.cwd });
            }
          }
          if (entries.length === 0) return;
          const results = await Promise.all(
            entries.map(async (e) => ({
              ...e,
              exists: await checkPathExists(e.cwd).catch(() => false),
            })),
          );
          const stale = results.filter((r) => !r.exists);
          if (stale.length === 0) return;
          set((s) => {
            const next = { ...s.workspaces };
            for (const { teamKey, wsId } of stale) {
              next[teamKey] = (next[teamKey] ?? []).map((w) =>
                w.id === wsId
                  ? {
                      ...w,
                      status: "error" as const,
                      statusError: "Worktree no longer exists on disk",
                    }
                  : w,
              );
            }
            return { workspaces: next };
          });
          logger.info(
            "viewer",
            `loadPersistedState: ${stale.length} stale workspace(s) marked as error`,
          );
        })();

        // SW-SN-1: load the plural notes model on startup. `listNotes` triggers
        // the Rust-side legacy → notes/ migration on its first call, then opens
        // the most-recently-opened note.
        void get().loadNotes();

        // Reveal each restored active tab so the status nav expands
        // ancestor sub-epics on app start. Without this, a file whose
        // ancestors aren't already in the persisted statusNavExpandedPaths
        // set (e.g. because a debounced persist dropped the update before
        // quit) would sit hidden under collapsed parents on next launch.
        //
        // T.4 Phase 5: `activeTab` is keyed by teamKey. To resolve the
        // `teamContentPath` needed by revealOpenedFile, derive a
        // representative repoPath per team from `workspaceRepos`.
        // T.4 Round 4 (Blind Hunter #3): subscribe to `workspaceRepos` for a
        // bounded window so teams whose repos arrive after the first
        // `waitForWorkspaceReposReady` notification still get their ancestors
        // expanded. Without this retry, a late-populating team's tab opens
        // hidden under a collapsed parent on next launch.
        const restoredActiveTab = get().activeTab;
        const unrevealedTeams = new Map<string, string>();
        for (const [teamKey, filePath] of Object.entries(restoredActiveTab)) {
          if (filePath) unrevealedTeams.set(teamKey, filePath);
        }
        const tryReveal = () => {
          if (unrevealedTeams.size === 0) return;
          const allRepos = Object.values(useOrgStore.getState().workspaceRepos);
          for (const [teamKey, filePath] of Array.from(unrevealedTeams.entries())) {
            const repForTeam = allRepos.find((r) => {
              const parsed = parseSyntheticRepoKey(r.repoPath);
              if (!parsed) return false;
              return buildTeamKey(parsed.orgSlug, parsed.team) === teamKey;
            });
            if (repForTeam) {
              revealOpenedFile(repForTeam.repoPath, teamKey, filePath, set, get);
              unrevealedTeams.delete(teamKey);
            }
          }
        };
        tryReveal();
        if (unrevealedTeams.size > 0) {
          // T.4 Round 5 (Blind Hunter HIGH H2 / Edge Case Hunter MED M1):
          // the subscriber's `clearTimeout` must read the timer through a
          // binding that's safe to reference at subscribe-register time —
          // not a `const` declared after the subscribe (which would sit
          // in TDZ). Today's zustand `subscribe` fires async on the next
          // state change so a const-after-subscribe closure resolves the
          // binding after the setTimeout assignment runs and works in
          // practice. But a future synchronous-fire path (zustand upgrade,
          // middleware, sync useOrgStore.setState between subscribe-register
          // and setTimeout) would throw ReferenceError. Wrapper object
          // gets us a stable const binding while allowing late assignment
          // of the timer handle, with no `let` + prefer-const friction.
          const timerHandle: { current: ReturnType<typeof setTimeout> | undefined } = {
            current: undefined,
          };
          const unsubReveal = useOrgStore.subscribe(() => {
            tryReveal();
            if (unrevealedTeams.size === 0) {
              unsubReveal();
              if (timerHandle.current !== undefined) clearTimeout(timerHandle.current);
            }
          });
          timerHandle.current = setTimeout(() => {
            unsubReveal();
            if (unrevealedTeams.size > 0) {
              logger.warn(
                "viewer",
                `loadPersistedState: ${unrevealedTeams.size} team(s) never resolved a repo for revealOpenedFile within 2000ms`,
              );
            }
          }, 2000);
        }
        const latestPrefs = usePrefsStore.getState();
        if (latestPrefs.principal !== prefsOwner) return;
        if (latestPrefs.globalReady) applyGlobalViewerPrefs(latestPrefs.global.viewer?.settings);
        if (latestPrefs.ownedReady) applyOwnedViewerPrefs(latestPrefs.owned.viewer ?? {});
        // All restored slices are installed and stateLoaded is true before the durable seed save.
        const knownSchema =
          typeof savedSchemaVersion !== "number" || savedSchemaVersion <= VIEWER_SCHEMA_VERSION;
        if (knownSchema && (savedSchemaVersion !== VIEWER_SCHEMA_VERSION || seededCliChrome)) {
          await get().flushPersist();
        }
        // Push to Rust memory for MCP tools on startup. It is the last push of the launch and
        // replaces the mirror, so it carries every field (MCP `open_terminal_tab` resolves an
        // open workspace from the workspaces among them).
        const state = get();
        pushViewerState({ ...viewerMirrorFields(state), ...agentListPushFields(state) }).catch(
          (e) => logger.warn("viewer", `pushViewerState failed: ${e}`),
        );
      } else {
        set({ stateLoaded: true, persistedStateRestored: true });
      }
    } catch (e) {
      logger.error("viewer", `loadPersistedState: failed — ${e}`);
      set({ stateLoaded: true, persistedStateRestored: true });
      // The saved state was never read: a held write would put defaults over
      // it. Drop it; the next real change persists normally.
      persistHeldBeforeLoad = false;
    }
    // SW-FU-B2: land the write held back while loading (e.g. a team opened
    // mid-load, now merged into `openedTeams`).
    if (persistHeldBeforeLoad && get().stateLoaded) {
      persistHeldBeforeLoad = false;
      get().persistState();
    }
  },
}));

/**
 * Resolve an open file's honest `lifecycle` from the store-published `openFileLifecycleByPath`
 * slice (written by the rendering viewer's `readFile` outcome). Returns a primitive
 * `SubjectLifecycle` or `undefined` (≡ `active`), so a subscriber re-renders only when THIS
 * path's lifecycle changes. The normalize runs per `path` change, not on every store emission.
 */
export function useOpenFileLifecycle(
  path: string | null | undefined,
): SubjectLifecycle | undefined {
  const normalized = useMemo(() => (path ? normalizeStoryFilePath(path) : undefined), [path]);
  return useViewerStore((s) => (normalized ? s.openFileLifecycleByPath[normalized] : undefined));
}

/** Save note bodies before their selection/layout; propagate any durability failure. */
export async function flushViewerForProfileSwitch(): Promise<void> {
  if (lazyCreateInFlight) await lazyCreateInFlight;
  if (noActiveNoteContent?.trim())
    throw new Error("The new note has not been saved. Retry saving it before switching profiles.");
  await debouncedScratchSave.flushStrict();
  if (!useViewerStore.getState().stateLoaded) return;
  debouncedPersist.trigger();
  await debouncedPersist.flushStrict();
}

function normalizeViewerSettings(settings: unknown): ViewerSettings {
  const merged = {
    ...DEFAULT_SETTINGS,
    ...migrateThemeSettings((settings ?? {}) as Partial<ViewerSettings>),
  } as ViewerSettings;
  // Clear editor values that are no longer supported
  const validEditors = ["cursor", "custom", ""];
  if (!validEditors.includes(merged.editor)) {
    merged.editor = "";
  }
  // SW-VE-8: coerce an unknown/legacy editorLineWrap back to the
  // default so the Settings radio always has a valid selection (the
  // `resolveEditorLineWrap` resolver fails safe on its own; this keeps
  // the STORED value a real enum member).
  const validLineWrap: EditorLineWrap[] = ["auto", "on", "off"];
  if (!validLineWrap.includes(merged.editorLineWrap)) {
    merged.editorLineWrap = DEFAULT_EDITOR_LINE_WRAP;
  }
  // SW-VE-11: coerce the editor power-config settings back to valid
  // members so each Settings control always has one selection and the
  // CM6 builders never see a garbage value (mirrors editorLineWrap).
  const validFolding: EditorFolding[] = ["auto", "on", "off"];
  if (!validFolding.includes(merged.editorFolding)) {
    merged.editorFolding = DEFAULT_EDITOR_FOLDING;
  }
  if (typeof merged.editorShowWhitespace !== "boolean") {
    merged.editorShowWhitespace = false;
  }
  merged.editorTabSize = coerceTabSize(merged.editorTabSize);
  merged.workspaceBarSize = resolveWorkspaceBarSize(merged.workspaceBarSize);
  if (typeof merged.editorProseFont !== "boolean") {
    merged.editorProseFont = false;
  }
  // SW-CW-5: normalize a corrupted/non-boolean persisted
  // focusWorkspaceDocOnActivation to a clean boolean (default-OFF), so
  // the settings radio always has one selection (mirrors the other
  // FE-only boolean hygiene arms; the `?? false` reads fail safe too).
  if (typeof merged.focusWorkspaceDocOnActivation !== "boolean") {
    merged.focusWorkspaceDocOnActivation = false;
  }
  // SW-AU-34: normalize a corrupted/non-boolean persisted quitOnClose to a
  // clean boolean (default-OFF = background-on-close). Pushed to a strictly-
  // typed Rust `bool` command (`set_quit_on_close`) — a stray non-bool would
  // fail arg-deserialization and never reach the close-path gate.
  if (typeof merged.quitOnClose !== "boolean") {
    merged.quitOnClose = false;
  }
  // SV-8: same for the menu-bar item (a strictly-typed Rust `bool` push).
  if (typeof merged.terminalServerMenuBar !== "boolean") {
    merged.terminalServerMenuBar = false;
  }
  // SW-AU-34: keep hasSeenBackgroundExplainer a real boolean so a stray
  // truthy persisted value can't permanently suppress the one-time explainer
  // (the gate is `!hasSeen`). Not Rust-pushed, but coerced for the same reason.
  if (typeof merged.hasSeenBackgroundExplainer !== "boolean") {
    merged.hasSeenBackgroundExplainer = false;
  }
  // RF-perf-14d → RF-perf-23: coerce a corrupted/legacy
  // worktreeSetupParallelism back to a valid member so the Settings row
  // always has one selection. The valid persisted domain is "auto",
  // "off", or an integer in [1, MAX_BACKGROUND_SETUP_CAP] (a preset OR a
  // custom value — RF-perf-23 removed the old 1–4 clamp now that setup is
  // a cheap clone, not `npm ci`; a large custom value like 999 is
  // legitimate and the row renders it via its "Custom…" option). A
  // non-integer / < 1 / above-MAX / non-numeric value (0, -1, 2.9, 1e21,
  // garbage, a stray boolean) coerces to "auto": otherwise it would leave
  // the radiogroup with no checked option AND no keyboard tab-stop (an
  // above-MAX value would also render as sci-notation in the number
  // input). "off" is the unbounded escape hatch, not a giant number.
  const wsp = merged.worktreeSetupParallelism;
  const validOverride =
    typeof wsp === "number" && Number.isInteger(wsp) && wsp >= 1 && wsp <= MAX_BACKGROUND_SETUP_CAP;
  if (wsp !== "auto" && wsp !== "off" && !validOverride) {
    merged.worktreeSetupParallelism = "auto";
  }
  // SW-DX-17d: same coercion for the background-agent limit — its radio
  // offers exactly "auto" or one of {2,4,6,8}, so a finite-but-out-of-
  // domain persisted value (0, 5, 999, 2.5) must coerce to "auto" or the
  // group would render with no checked button and no keyboard tab-stop.
  const bal = merged.backgroundAgentLimit;
  const validLimit = typeof bal === "number" && [2, 4, 6, 8].includes(bal);
  if (bal !== "auto" && !validLimit) {
    merged.backgroundAgentLimit = "auto";
  }
  return merged;
}

/** Apply committed global values without scheduling persistence or replacing a pending edit. */
export function applyGlobalViewerPrefs(settings: unknown) {
  if (!useViewerStore.getState().stateLoaded) return;
  useViewerStore.setState({
    settings: {
      ...normalizeViewerSettings(settings),
      ...pendingSettings,
    } as ViewerSettings,
  });
  syncLiveViewerMirror();
}

/** Workspace records belong to the principal; transient host state never comes from prefs. */
export function applyOwnedViewerPrefs(viewer: Record<string, unknown>, reset = false) {
  if (reset) {
    const initial = useViewerStore.getInitialState() as unknown as Record<string, unknown>;
    const cleared = Object.fromEntries(
      [...ownedViewerKeys].filter((key) => key in initial).map((key) => [key, initial[key]]),
    );
    const live = useViewerStore.getState();
    if (ownedViewerKeys.has("cliChrome"))
      Object.assign(cleared, cliChromeState(sanitizeCliChrome(undefined)));
    if (ownedViewerKeys.has("terminalViewerGrid"))
      live.applyLayout({ kind: "viewer-grids", grids: {} });
    if (ownedViewerKeys.has("terminalViewerFocus"))
      for (const key of Object.keys(live.terminalLayout.focus ?? {}))
        live.applyLayout({ kind: "focused", key, session: null });
    if (ownedViewerKeys.has("terminalTabGridSizes"))
      cleared.tabGridLayout = useViewerStore.getInitialState().tabGridLayout;
    ownedViewerKeys.clear();
    resetViewerPrefsPersistence();
    useViewerStore.setState(cleared);
    return;
  }
  for (const key of Object.keys(viewer)) ownedViewerKeys.add(key);
  if (!useViewerStore.getState().stateLoaded) return;
  const viewerState = useViewerStore.getState();
  const state = viewerState as unknown as Record<string, unknown>;
  const current = serializedViewerPrefs(viewerState);
  const defaults = serializedViewerPrefs(useViewerStore.getInitialState());
  const patch: Record<string, unknown> = {};
  for (const key of new Set([...ownedViewerKeys, ...Object.keys(viewer)])) {
    if (!(key in current) || key === "settings" || key === "schemaVersion") continue;
    const before = persistedViewer?.[key] ?? defaults[key];
    const committed = viewer[key] ?? defaults[key];
    const pending = fieldPatch(before, current[key]);
    const value = pending === undefined ? committed : applyFieldPatch(committed, pending);
    if (JSON.stringify(current[key]) === JSON.stringify(value)) continue;
    if (key === "cliChrome") Object.assign(patch, cliChromeState(sanitizeCliChrome(value)));
    else if (key === "terminalViewerGrid")
      viewerState.applyLayout({ kind: "viewer-grids", grids: savedViewerGrids(value) });
    else if (key === "terminalViewerFocus") {
      const focus = (value ?? {}) as Record<string, string>;
      for (const id of new Set([
        ...Object.keys(viewerState.terminalLayout.focus ?? {}),
        ...Object.keys(focus),
      ]))
        viewerState.applyLayout({ kind: "focused", key: id, session: focus[id] ?? null });
    } else if (key === "terminalTabGridSizes") patch.tabGridLayout = savedTabGrids(value);
    else if (key === "workspaces") {
      patch.workspaces = Object.fromEntries(
        [
          ...new Set([...Object.keys(value as object), ...Object.keys(viewerState.workspaces)]),
        ].flatMap((team) => {
          const records = (value as Record<string, Workspace[]>)[team] ?? [];
          const restored = [
            ...records.flatMap((record) => {
              const restored = restoreWorkspaceRecord(record);
              if (!restored) return [];
              const live = viewerState.workspaces[team]?.find(
                (workspace) => workspace.id === record.id,
              );
              const runtime = live
                ? {
                    status: live.status,
                    statusError: live.statusError,
                    backgroundSetup: live.backgroundSetup,
                    setupState: live.setupState,
                    keptModified: live.keptModified,
                    agentRuns: live.agentRuns,
                  }
                : {};
              return [{ ...restored, ...runtime }];
            }),
            ...(viewerState.workspaces[team] ?? []).filter(
              (workspace) =>
                !((current.workspaces as Record<string, Workspace[]>)[team] ?? []).some(
                  (saved) => saved.id === workspace.id,
                ),
            ),
          ];
          return team in (value as object) || restored.length ? [[team, restored]] : [];
        }),
      );
    } else if (key in state) patch[key] = value;
  }
  if ("openTabs" in patch || "activeTab" in patch) {
    const tabs = (patch.openTabs ?? viewerState.openTabs) as Record<string, TabState[]>;
    const active = { ...((patch.activeTab ?? viewerState.activeTab) as Record<string, string>) };
    const next = { ...tabs };
    for (const [team, live] of Object.entries(viewerState.openTabs)) {
      const virtual = live.filter((tab) => isVirtualTabPath(tab.path));
      if (virtual.length)
        next[team] = [
          ...(next[team] ?? []).filter((tab) => !isVirtualTabPath(tab.path)),
          ...virtual,
        ];
      if (isVirtualTabPath(viewerState.activeTab[team] ?? ""))
        active[team] = viewerState.activeTab[team];
    }
    const reconciled = reconcileActiveTabs(next, active);
    patch.openTabs = reconciled.openTabs;
    patch.activeTab = reconciled.activeTab;
  }
  const nextBaseline = { ...persistedViewer };
  for (const key of ownedViewerKeys) delete nextBaseline[key];
  Object.assign(nextBaseline, structuredClone(viewer));
  persistedViewer = nextBaseline;
  ownedViewerKeys.clear();
  for (const key of Object.keys(viewer)) ownedViewerKeys.add(key);
  useViewerStore.setState(patch);
  syncLiveViewerMirror();
}

export function resetViewerPrefsPersistence() {
  debouncedPersist.cancel();
  persistedViewer = undefined;
  pendingSettings = {};
}

/**
 * The viewer's fields of the native mirror MCP reads (the agent list adds its own). A push
 * replaces the whole mirror, so every push carries all of them: one that left some out would
 * drop them from `get_viewer_state` until the next full push.
 */
export function viewerMirrorFields(state: ViewerState): Record<string, unknown> {
  return {
    ...serializedViewerPrefs(state),
    workspaces: state.workspaces,
    softClosedWorkspaces: state.softClosedWorkspaces,
    statusNavPinnedPaths: state.statusNavPinnedPaths,
  };
}

/** Live commits refresh the existing native mirror without scheduling a disk save. */
function syncLiveViewerMirror(): Promise<void> {
  const state = useViewerStore.getState();
  if (!state.stateLoaded) return Promise.resolve();
  return pushViewerState({
    ...viewerMirrorFields(state),
    ...agentListPushFields(state),
  }).catch((error) => logger.warn("viewer", `pushViewerState failed: ${error}`));
}

// The warm-scope owner opens a team in the same tick it makes it active (see org-store), so no
// commit sees an active team that is not in `openedTeams`.
warmScopes.setOpener((teamKey) => useViewerStore.getState().openTeam(teamKey));
