// SPDX-License-Identifier: MIT
import { openStoryTrail } from "@/features/dashboard/lib/story-trail";
import { storyIdFromPath } from "@/shared/lib/worktree/resolve";
import { toast } from "sonner";
import { getRepoColor } from "@/shared/lib/pipeline-graph/epic-colors";
import { useNavModelProjection } from "../lib/use-nav-model-projection";
import { fileSurfaceCall } from "@/tauri/commands";
import { dirname } from "@/shared/lib/basename";
import { useAgentRows } from "@/features/agents/lib/agent-names";
import { liveHub } from "@/features/dashboard/lib/live-hub";
import { registerNativeMenu, type NativeMenuRequest } from "@/features/shell/native-menu-openers";
import { dashboardNavTarget } from "./status-nav/dashboard-nav-target";
import { ContextWorktreeCrew } from "@/features/git-sync/components/context-worktree-crew";
import { useContextSourceOptions } from "@/features/git-sync/lib/use-context-worktrees";
import { useScratchItemActions } from "@/features/scratch-space/components/scratch-item-actions";
import { PositionedMenuItem } from "@/shared/components/positioned-context-menu";
import { PrivateCommitButton } from "@/features/git-sync/components/git-status-button";
import { useMemo, useCallback, useState, useEffect, useRef, useDeferredValue } from "react";
import { ArrowLeft, ChevronRight, Link2, MoreHorizontal } from "lucide-react";
import { useUiStore } from "@/shared/stores/ui-store";
import { VirtualList, type VirtualListHandle } from "@/shared/components/virtual-list";
import { useMapToggle } from "@/shared/hooks/use-map-toggle";
import { pruneMapByKeys } from "@/shared/lib/prune-map-by-keys";
import { useScrollSnapshot } from "@/shared/hooks/use-scroll-snapshot";
import { navViewKey } from "@/shared/lib/view-snapshot";
import { navDropDirPath } from "@/features/dashboard/lib/nav-file-drop";
import { useNavFileDrop } from "@/features/dashboard/hooks/use-nav-file-drop";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { useFileRefreshStore } from "@/shared/stores/file-refresh-store";
import { useActiveTeamKey } from "@/shared/lib/team-key";
import { useNavSurface } from "@/features/dashboard/lib/use-nav-surface";
import { useMergeMarks, type MergeMarks } from "@/features/dashboard/lib/use-merge-marks";
import { canMergeMark } from "@/features/dashboard/lib/merge-mark";
import { AppButton } from "@/shared/components/app-button";
import type {
  DerivedPipelineTree,
  PipelineStatus,
  StoryRef,
} from "@/shared/lib/pipeline-graph/types";
import { buildSprintTargetOptions } from "@/shared/lib/pipeline-graph/sprint-targets";
import type { SprintRegistry } from "@/tauri/commands";
import { applySprintChange, bulkTagStories } from "@/shared/lib/pipeline-graph/sprint-tag-actions";
import { collectAllStories } from "@/shared/lib/pipeline-graph/collect-all-stories";
import { findScopedSubtree, normalizeStatus } from "@/shared/lib/pipeline-graph/pipeline-utils";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { ProgressChipRow } from "@/features/viewer/components/progress-chip-row";
import { sendTextToTerminal } from "@/shared/lib/send-to-terminal";
import { type PipelineGraph } from "@/shared/lib/pipeline-graph/pipeline-graph";
import { itemFilePath, type FlatNavItem } from "@/features/dashboard/lib/flatten-nav-tree";
import { normalizePinPath, type PinnedNavItem } from "@/features/dashboard/lib/nav-pins";
import {
  buildPipelineBreadcrumbs,
  collapsePipelineBreadcrumbs,
  type PipelineCrumb,
} from "@/shared/lib/pipeline-graph/breadcrumb-model";
// SW-VE-5: the provenance-badge family (NavProvenance + SourceRepoTint) lives in
// `scope-badge`; RF-12 moved the row-level consumers into `status-nav/rows`, so the
// orchestrator now only needs the `NavProvenance` type for its props/interaction model.
import { type NavProvenance } from "./scope-badge";
import { useOrgStore } from "@/shared/stores/org-store";
// RF-12: the row layer (dispatch + row variants + drag-to-link wrapper) lives in
// its own module now; the orchestrator just wires the interaction model into it.
import { itemStableKey, renderRow, NavRowDnd, type RowInteractionModel } from "./status-nav/rows";
import { NavContextMenu, type NavSprintMenu } from "./status-nav/nav-context-menu";
import { useActiveTeamStarterPage } from "@/features/viewer/hooks/use-team-starter-page";
import { isStarterPagePath } from "@/features/viewer/lib/team-starter-page";
import { NavDialogs } from "./status-nav/nav-dialogs";
import { useNavFileActions } from "./status-nav/use-nav-file-actions";
import { useNavItems } from "./status-nav/use-nav-items";
import { useStoryAuthoringActions } from "@/features/story-authoring/components/story-authoring-actions";
import type { NavContextMenuState } from "./status-nav/types";
import type { NavAgentContext } from "@/features/agent-context/lib/nav-agent-context";
import { withAgentContext } from "@/features/agent-context/components/agent-context-row";
import { NavFilterField } from "./status-nav/nav-filter-field";
import { PaneNavFilterMenu, PaneNavStatusPicker } from "./status-nav/nav-pane-menus";
import {
  DEFAULT_NAV_LENS,
  NAV_LENSES,
  type HistoryBranchFilter,
  type NavLensState,
} from "@/shared/lib/nav-lens";
import { useContextGit } from "@/features/git-sync/store/use-context-git-store";
import { scopeChanges } from "@/features/git-sync/lib/nav-changes";
import { useTeamContentPrefix } from "@/features/git-sync/lib/scope-publish-set";
import { ContextHistoryView } from "@/features/git-sync/components/context-history-view";
import { deriveSyncNodes, nodeOwnedPaths } from "@/features/git-sync/lib/group-changes-by-node";
import {
  AttachToNodePicker,
  buildAttachTargets,
} from "@/features/git-sync/components/attach-to-node-picker";
import { useAttachDocToNode } from "@/features/dashboard/lib/use-attach-doc-to-node";
import { getViewerFileType } from "@/shared/lib/viewer-file-type";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

const EMPTY_EXPANDED: string[] = [];
const NO_MERGE_MARKS: MergeMarks = {
  paths: new Set(),
  ready: true,
  isMarked: () => false,
  toggle: async () => {},
};
const ROW_HEIGHT = 24;
const INDENT_PX = 12;

/** SW-SB-15: find a single story by its file path in the tree — an early-exit
 *  walk used to resolve the right-clicked row's current sprint. Cheaper than
 *  indexing every story into a map when only one lookup is ever needed. */
function findStoryByPath(node: DerivedPipelineTree, path: string): StoryRef | null {
  for (const s of node.stories) {
    if (s.filePath === path) return s;
  }
  for (const sub of node.subEpics) {
    const found = findStoryByPath(sub, path);
    if (found) return found;
  }
  return null;
}

function paneStoryStatus(tree: DerivedPipelineTree, path: string) {
  const status = findStoryByPath(tree, path)?.status ?? null;
  return status?.trim() || normalizeStatus(status);
}

export interface PipelineStatusNavProps {
  /** The workspace that owns this nav, including while it is retained offscreen. */
  teamKey?: string;
  scopeTree?: DerivedPipelineTree;
  /** Scoped display subtree when drilled in; the full tree otherwise. */
  tree: DerivedPipelineTree;
  /** Full-graph readiness seam, built once from the full tree. Resolves
   *  cross-epic deps against the full index regardless of `tree`'s drill
   *  scope, so readiness can't flip on drill (SW-FX-1). Production always
   *  supplies this; when omitted (standalone render / tests) it falls back to
   *  a graph built from `tree`. */
  graph?: PipelineGraph;
  repoPath: string;
  /** Currently selected file path (highlighted in the nav). */
  selectedPath: string | null;
  /** Called when user clicks a story/doc row to open the file (preview). */
  onFileSelect: (filePath: string, options?: { openInNewTab?: boolean; hash?: string }) => void;
  /** Called when user double-clicks a file to permanently open it. */
  onFileDoubleClick?: (filePath: string) => void;
  /** Called when user clicks the drill button on a sub-epic. */
  onDrillPathChange?: (absolutePath: string) => void;
  /** Called when a story's status is changed via the inline popover. */
  onStatusChange?: (filePath: string, newStatus: PipelineStatus) => void;
  /** Repo display name for the toolbar. */
  repoName?: string;
  /** Root-level display name (e.g. org display name for synthetic repos).
   *  When provided and not drilled in, shown instead of repoName. */
  rootDisplayName?: string;
  /** Whether the nav is showing a drilled-in sub-scope (for toolbar label). */
  isDrilledIn?: boolean;
  /** Root path — drill-reset target and the breadcrumb root crumb. */
  rootPath?: string;
  /** Context menu: open story workspace. R-18: storyId is resolved from
   *  frontmatter inside `ensureWorktree`; callers pass only the file path. */
  onOpenStoryWorkspace?: (filePath: string) => void;
  /** The story row's "Open CLI" item, when the host offers a choice of where (see
   *  `OpenCliMenuItem`); absent, the item opens the story's workspace the plain way. */
  renderOpenCli?: (filePath: string, onClose: () => void) => React.ReactNode;
  /** SW-EW-6: context menu — open an epic's integration terminal(s). Assembles
   *  a writable worktree per epic repo (`assembleEpicWorktrees`) then opens an
   *  integration row at each. `epicDirName` is the epic folder leaf (the graph
   *  color key + workspace id); `epicFilePath` is the epic doc the backend reads. */
  onOpenEpicTerminal?: (epicDirName: string, epicFilePath: string) => void;
  /** Context menu: predicate for whether a path is a story file. */
  isStoryFilePath?: (filePath: string) => boolean;
  /** Viewer root path — for listing non-pipeline content (docs, planning-artifacts, etc.). */
  viewerRootPath?: string;
  /** Bumped by the filesystem watcher — triggers re-fetch of non-pipeline content. */
  refreshKey?: number;
  /** Callback for "Export to PDF" context-menu action (R.4). */
  onExportPdf?: (filePath: string) => void;
  /** SLW-NC-1 Phase 3: provenance inputs for the source-repo color tint on
   *  merged source rows (folders + docs). Bundled so the glyph map and its theme
   *  can't be half-supplied (which would silently mis-color the tint). Omitted →
   *  no tint. */
  provenance?: NavProvenance;
  fileProjectionTree?: DerivedPipelineTree;
  /** SLW-FX-21: relate `draggedFilePath` INTO `targetFilePath`'s supporting_docs
   *  (drag one nav file onto another). Omitted → drag-to-link is off (no row is
   *  draggable and no row accepts a drop). The host wires this to the shared
   *  `relateDoc` orchestration. */
  onRelateDrop?: (targetFilePath: string, draggedFilePath: string) => void;
  /** The sprint registry (loaded by the parent). The nav no longer filters by
   *  sprint — it stays a static folder tree — but it still needs the registry for
   *  the SW-SB-15 "add to sprint" right-click menu. */
  sprintRegistry?: SprintRegistry | null;
  /** SLW-PT-18: the per-file ⋯ move/copy/remove affordance for a content file
   *  row. The host (TeamWorkspaceView) supplies it wired to the active pivot
   *  context + app-shell seams; omitted ⇒ mobility not mounted here. */
  privateFiles?: ReadonlyMap<string, import("@/tauri/commands").ScratchOverlayFile>;
  moveActionSlot?: (filePath: string, fileName: string) => React.ReactNode;
  /** SW-XS-15: the selected agent's context ("Context for ‹Agent› · in ‹Repo›"): rows in its
   *  want set are emphasized with a quiet source tag, the rest muted, and right-click offers
   *  "Add to / Remove from ‹Agent›'s context". Omitted (Team (everything)) → no overlay. */
  agentContext?: NavAgentContext;
}

// SW-VE-5: `NavProvenance` moved to `./scope-badge` (the provenance-badge home);
// re-exported here so existing `import { NavProvenance } from "…/pipeline-status-nav"`
// keeps resolving.
export type { NavProvenance } from "./scope-badge";

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

export function NavFileActionsOwner({
  teamKey: owningTeamKey,
  tree,
  scopeTree: fullTeamTree,
  fileProjectionTree = fullTeamTree ?? tree,
  graph: graphProp,
  repoPath,
  selectedPath,
  onFileSelect,
  onFileDoubleClick,
  onDrillPathChange,
  onStatusChange,
  repoName,
  rootDisplayName,
  isDrilledIn,
  rootPath,
  onOpenStoryWorkspace,
  renderOpenCli,
  onOpenEpicTerminal,
  isStoryFilePath,
  viewerRootPath,
  refreshKey,
  onExportPdf,
  provenance,
  onRelateDrop,
  sprintRegistry: sprintRegistryProp,
  moveActionSlot,
  privateFiles,
  agentContext,
  renderNative = (body) => body,
}: PipelineStatusNavProps & { renderNative?: (body: React.ReactNode) => React.ReactNode }) {
  const scopeTree = fullTeamTree ?? tree;
  // T.4 Phase 5: statusNavExpandedPaths / revealCounter are team-keyed.
  const activeTeamKey = useActiveTeamKey();
  const teamKey = owningTeamKey ?? activeTeamKey;
  // The nav's surface (source repo, org root, team) for the git-shared team reads.
  const navSurface = useNavSurface(repoPath, teamKey);
  const { rows: scopeAgents } = useAgentRows();
  const openTabs = useViewerStore((state) => state.navHistory?.[teamKey]);
  const sources = useMemo(() => {
    const all = collectAllStories(scopeTree);
    const recent = (openTabs ?? [])
      .slice()
      .reverse()
      .map((path) => all.find((story) => story.filePath === path))
      .filter((story): story is StoryRef => !!story);
    const selected = all.find((story) => story.filePath === selectedPath);
    const stories = [
      ...new Map(
        [...(selected ? [selected] : []), ...recent].map((story) => [
          story.storyId ?? story.filePath,
          { id: story.storyId ?? story.filePath, name: story.storyId ?? story.filePath },
        ]),
      ).values(),
    ].slice(0, 9);
    const agents = (scopeAgents ?? [])
      .filter(
        (agent) =>
          agent.scope === "team" && `${agent.org}:${agent.team}` === teamKey && !agent.archived,
      )
      .map((agent) => ({
        id: agent.key.split("/").pop()!,
        name: agent.name ?? agent.key.split("/").pop()!,
        avatar: agent.avatar,
      }));
    return { stories, agents };
  }, [scopeTree, selectedPath, openTabs, scopeAgents, teamKey]);
  useEffect(() => {
    const at = teamKey.lastIndexOf(":");
    void liveHub
      .writeUi("ui:file-nav", `team:${teamKey.slice(0, at)}/${teamKey.slice(at + 1)}`, { sources })
      .catch(console.error);
  }, [teamKey, sources]);
  // SW-CP-27: the team's merge-marked content paths (CP-24) + the optimistic
  // set/clear toggle — drives the in-tree merge badge and the Mark/Unmark item.
  // Keyed by the owning clone/team, not the displayed source: the projection below is team-wide.
  const owningSurface = useNavSurface(repoPath, teamKey, true);
  const teamMergeMarks = useMergeMarks(owningSurface.orgRoot, owningSurface.team);
  const mergeMarks = navSurface.team === owningSurface.team ? teamMergeMarks : NO_MERGE_MARKS;
  // The active team's starter page → the house marker on its row.
  const starterPage = useActiveTeamStarterPage(owningTeamKey);
  const starterEffectivePath = starterPage?.effectivePath ?? null;
  const starterContentRoot = starterPage?.contentRoot ?? null;
  const isStarterPage = useMemo(() => {
    if (!starterEffectivePath || !starterContentRoot) return undefined;
    return (abs: string) => isStarterPagePath(abs, starterEffectivePath, starterContentRoot);
  }, [starterEffectivePath, starterContentRoot]);

  const projectedFiles = useMemo(
    () => ({
      tree: fileProjectionTree,
      starterPage:
        starterEffectivePath && starterContentRoot
          ? { path: starterEffectivePath, root: starterContentRoot }
          : null,
      mergePaths: [...(teamMergeMarks.paths ?? [])],
      repos: Object.fromEntries(
        [...(provenance?.glyphMap ?? new Map<string, string>())].map(([slug, glyph]) => [
          slug,
          { glyph, tint: getRepoColor(slug, provenance?.isLightTheme ?? false) },
        ]),
      ),
    }),
    [
      fileProjectionTree,
      starterEffectivePath,
      starterContentRoot,
      teamMergeMarks.paths,
      provenance,
    ],
  );
  // A loading placeholder is never published over the team's real marks.
  useNavModelProjection(
    teamKey,
    fullTeamTree ?? null,
    teamMergeMarks.ready ? projectedFiles : null,
  );

  // SW-SB-15: sprint-tag context-menu wiring. The registry drives the concrete
  // target list; `refreshAfterSprintWrite` is the shared post-write rescan
  // (watcher-suppressed, off the UI thread — a menu click never needs the drag
  // path's optimistic overlay). Bulk epic add is confirmed (AC-2/AC-3) via the
  // AlertDialog below. The registry rides in on the `sprintRegistry` prop (loaded
  // by the parent) — no store access when the nav is rendered without it (tests,
  // standalone). The right-clicked row's story/epic context is resolved lazily in
  // the `sprintMenu` memo below, so no tree is walked while the menu is closed.
  const sprintRegistry = sprintRegistryProp ?? null;
  const sprintTargets = useMemo(() => buildSprintTargetOptions(sprintRegistry), [sprintRegistry]);
  const activeSprintId = sprintRegistry?.activeSprint ?? null;
  const refreshAfterSprintWrite = useCallback(() => {
    if (repoPath) useFileRefreshStore.getState().bumpFileRefresh(repoPath);
  }, [repoPath]);
  // The pending epic bulk-add confirm (null = closed). `target` is captured at
  // open time so the dialog can't drift to a null active sprint mid-confirm; the
  // partitions are its unassigned stories vs those in a DIFFERENT sprint (ones
  // already in `target` need no action and are in neither bucket).
  const [sprintBulkConfirm, setSprintBulkConfirm] = useState<{
    target: string;
    unassigned: StoryRef[];
    sprinted: StoryRef[];
  } | null>(null);

  // Expanded sub-epic paths (for expand/collapse, not drill). Persisted in
  // viewer store per-team so state survives drill/back navigation.
  const expandedPathsArr = useViewerStore(
    (s) => s.statusNavExpandedPaths[teamKey] ?? EMPTY_EXPANDED,
  );
  const expandedPaths = useMemo(() => new Set(expandedPathsArr), [expandedPathsArr]);
  const toggleExpand = useCallback(
    (absolutePath: string) => {
      useViewerStore.getState().toggleStatusNavExpanded(teamKey, absolutePath);
    },
    [teamKey],
  );

  const genericExpandedPaths = expandedPaths;
  const toggleGenericExpand = useCallback(
    (absolutePath: string) => {
      useViewerStore.setState((state) => {
        const previous = state.statusNavExpandedPaths[teamKey] ?? [];
        const expanded = previous.includes(absolutePath)
          ? previous.filter((p) => p !== absolutePath && !p.startsWith(`${absolutePath}/`))
          : [...previous, absolutePath];
        return { statusNavExpandedPaths: { ...state.statusNavExpandedPaths, [teamKey]: expanded } };
      });
      useViewerStore.getState().persistState();
    },
    [teamKey],
  );

  // Context menu state (mirrors file-tree pattern). Shape in `status-nav/types`.
  const [contextMenu, setContextMenuState] = useState<NavContextMenuState | null>(null);
  const externalMenuClose = useRef<(() => void) | null>(null);
  const setContextMenu = useCallback((menu: NavContextMenuState | null) => {
    const previous = externalMenuClose.current;
    externalMenuClose.current = null;
    setContextMenuState(menu);
    previous?.();
  }, []);

  // NB-7 / AC-5: Hoisted from StoryRow so each row's status-popover open
  // state survives unmount/remount as the virtualizer evicts rows past the
  // overscan window. Keyed by `story.filePath` (the same stable identifier
  // virtualizer uses for `getItemKey`). The Map identity changes on each
  // toggle — that's fine: only the affected row's `isPopoverOpen` prop
  // value actually flips, so the other StoryRow memo wraps still skip
  // re-renders.
  const [statusPopoverOpenMap, setStatusPopoverOpen, setStatusPopoverOpenMap] =
    useMapToggle<string>();

  const handleContextMenu = useCallback(
    (
      e: React.MouseEvent,
      filePath: string,
      epicDirPath?: string,
      isEpic?: boolean,
      epicFilePath?: string,
    ) => {
      e.preventDefault();
      setContextMenu({
        x: e.clientX,
        y: e.clientY,
        path: filePath,
        epicDirPath,
        epicFilePath,
        isEpic,
      });
    },
    [setContextMenu],
  );

  // SLW-NC-7: right-click on a row INSIDE the Pinned section → an unpin/remove menu.
  const handlePinnedContextMenu = useCallback(
    (e: React.MouseEvent, pin: PinnedNavItem) => {
      e.preventDefault();
      setContextMenu({ x: e.clientX, y: e.clientY, path: pin.path, pin });
    },
    [setContextMenu],
  );

  // NB-7 / code-review patch: GC `statusPopoverOpenMap` entries whose story
  // file no longer exists in the tree (file deleted / renamed / org-switch).
  // Without this the Map grows unbounded across long sessions and a future
  // story file resurrected at the same path would mount with stale open=true.
  // Set-state-in-effect is intentional: this map tracks user-driven popover
  // open/close state plus tree-derived GC; it's not purely derived from tree
  // (open state survives independent of tree mutations), so it can't be a
  // useMemo. Functional updater with reference-equality bail-out keeps
  // re-renders to zero when nothing changes.
  useEffect(() => {
    setStatusPopoverOpenMap((prev) => {
      // Skip the tree walk entirely when nothing is open (the common case).
      if (prev.size === 0) return prev;
      const liveFilePaths = new Set<string>();
      function collectStories(node: DerivedPipelineTree) {
        for (const s of node.stories) liveFilePaths.add(s.filePath);
        for (const sub of node.subEpics) collectStories(sub);
      }
      collectStories(tree);
      return pruneMapByKeys(prev, liveFilePaths);
    });
  }, [tree, setStatusPopoverOpenMap]);

  // SW-LNS-1: left-nav search bar state. Local-only (team-scoped lifetime —
  // unmount on team swap clears it); the raw input is `rawSearchQuery` and the
  // debounced value `useDeferredValue` yields drives the filter walk, so a
  // large-tree walk never runs on every keystroke.
  const [rawSearchQuery, setRawSearchQuery] = useState("");
  const searchQuery = useDeferredValue(rawSearchQuery);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const focusNonce = useUiStore((s) => s.leftNavSearchFocusNonce);
  useEffect(() => {
    if (focusNonce === 0) return;
    searchInputRef.current?.focus();
    searchInputRef.current?.select();
  }, [focusNonce]);
  // The team's lens (which stories show, how they are arranged), kept per team for the session.
  const storedView = useViewerStore((s) => s.navLens?.[teamKey]);
  const view = useMemo(() => ({ ...DEFAULT_NAV_LENS, ...storedView }), [storedView]);
  const setView = useCallback(
    (patch: Partial<NavLensState>) => {
      useViewerStore.getState().setNavLens(teamKey, patch);
    },
    [teamKey],
  );
  const toggleStageGroup = useCallback(
    () => setView({ doneOpen: !view.doneOpen }),
    [setView, view.doneOpen],
  );

  // The Changes view: the team's changed files (in the agent's context when one is picked),
  // listed in their folders. Its folders collapse on their own, separately from Files.
  const changesView = view.view === "changes";
  const historyView = view.view === "history";
  const {
    changes: allChanges,
    loading: changesLoading,
    orgRoot: contextOrgRoot,
    status: contextStatus,
    cloneRoot,
    cloneStatus,
    sourceNote,
    prefix: sourcePrefix,
  } = useContextGit(teamKey);
  const clonePrefix = useTeamContentPrefix(repoPath);
  const teamPrefix = view.source.kind === "private" ? (sourcePrefix ?? null) : clonePrefix;
  const sourceOptions = useContextSourceOptions(teamKey, cloneRoot ?? contextOrgRoot, clonePrefix);
  const setHistoryBranches = useCallback(
    (historyBranches: HistoryBranchFilter) => setView({ historyBranches }),
    [setView],
  );
  const { inScope: changes, outside: changesOutside } = useMemo(
    () => scopeChanges(allChanges, agentContext, contextStatus?.worktree ? cloneRoot : null),
    [allChanges, agentContext, contextStatus?.worktree, cloneRoot],
  );
  const [changesCollapsed, setChangesCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const toggleChangesFolder = useCallback((absolutePath: string) => {
    setChangesCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(absolutePath)) next.delete(absolutePath);
      else next.add(absolutePath);
      return next;
    });
  }, []);
  // A changed file opens on its Changes, in an ordinary tab: preview on click, pinned on
  // double-click.
  const openChange = useCallback(
    (filePath: string, pin: boolean) =>
      useViewerStore.getState().openChangeTab(teamKey, repoPath, filePath, pin),
    [teamKey, repoPath],
  );
  const openChangePreview = useCallback((p: string) => openChange(p, false), [openChange]);
  const openChangePinned = useCallback((p: string) => openChange(p, true), [openChange]);

  // A changed doc no story or epic claims can be attached to one, from its row.
  const docAttach = useAttachDocToNode();
  const syncNodes = useMemo(() => (changesView ? deriveSyncNodes(tree) : []), [changesView, tree]);
  const attachTargets = useMemo(() => buildAttachTargets(syncNodes), [syncNodes]);
  const ownedPaths = useMemo(() => nodeOwnedPaths(syncNodes), [syncNodes]);
  const storyPaths = useMemo(
    () => new Set(collectAllStories(tree).map((st) => st.filePath)),
    [tree],
  );
  const openAttach = docAttach.open;
  const changeAction = useCallback(
    (filePath: string) => {
      const kind = getViewerFileType(filePath);
      const loose =
        (kind === "markdown" || kind === "html") &&
        !storyPaths.has(filePath) &&
        !ownedPaths.has(filePath) &&
        changes.some((c) => c.absPath === filePath && c.kind !== "deleted");
      if (!loose || attachTargets.length === 0) return null;
      return (
        <button
          type="button"
          aria-label="Attach to a story or epic"
          title="Attach to a story or epic"
          data-testid="nav-change-attach"
          className="grid h-5 w-5 place-items-center rounded text-muted-foreground hover:bg-[color-mix(in_srgb,var(--foreground)_9%,transparent)] hover:text-foreground focus-visible:outline focus-visible:outline-1 focus-visible:outline-ring"
          onClick={(e) => {
            e.stopPropagation();
            openAttach(filePath);
          }}
        >
          <Link2 size={13} aria-hidden />
        </button>
      );
    },
    [storyPaths, ownedPaths, changes, attachTargets.length, openAttach],
  );

  // RF-12: the item-list data prep (viewer-root listing + generic children +
  // counts/graph/flatten + the SLW-NC-7 pin subsystem) lives in its own hook now.
  const {
    flatItems,
    menuRows,
    epicTotal,
    epicDone,
    rootAnchor,
    toRel,
    pinResolvable,
    teamPinSet,
    personalPinSet,
    toggleTeamPin,
    togglePersonalPin,
    revealPinned,
    unpinPinnedRow,
    setPinnedCollapsed,
    isSearching,
    lensCounts,
  } = useNavItems({
    tree,
    graphProp,
    expandedPaths,
    genericExpandedPaths,
    refreshKey,
    viewerRootPath,
    isDrilledIn,
    teamKey,
    surface: navSurface,
    rootPath,
    onFileSelect,
    onDrillPathChange,
    searchQuery,
    view,
    agentContext,
    changes,
    changesCloneRoot: view.source?.kind === "worktree" ? cloneRoot : null,
    changesCollapsed,
  });

  useEffect(() => () => externalMenuClose.current?.(), []);

  // NB-7 / AC-2: VirtualList replaces the per-component virtualizer
  // scaffolding. The imperative `scrollToIndex` handle preserves the N.6
  // reveal effect below; per-item stable keys (via getKey) keep React
  // row state attached to the item rather than the position.
  const virtualListRef = useRef<VirtualListHandle>(null);

  // N.6: scroll the selected file into view after a programmatic reveal.
  // revealTargetRef stores the path we still need to scroll to — set when a
  // reveal signal fires (selectedPath / revealCount change) and consumed
  // once the item lands in flatItems. This handles the race where a
  // generic-child file is revealed before useGenericChildrenByPath has
  // resolved its listDirectory. (PTS-2 follow-up: orphan-files retired.)
  const revealCount = useViewerStore((s) => s.revealCounter?.[teamKey] ?? 0);
  const revealTargetRef = useRef<string | null>(null);
  // SLW-FX-21: the in-flight drag path, shared by all NavRowDnd rows so a
  // `dragover` handler can read it synchronously (HTML5 blocks dataTransfer reads
  // mid-drag). One drag at a time → one ref suffices.
  const draggedPathRef = useRef<string | null>(null);
  useEffect(() => {
    revealTargetRef.current = selectedPath;
  }, [selectedPath, revealCount]);
  useEffect(() => {
    const target = revealTargetRef.current;
    if (!target) return;
    const idx = flatItems.findIndex((item) => itemFilePath(item) === target);
    if (idx < 0) return;
    revealTargetRef.current = null;
    const raf = requestAnimationFrame(() => {
      virtualListRef.current?.scrollToIndex(idx, { align: "auto", behavior: "smooth" });
    });
    return () => cancelAnimationFrame(raf);
  }, [flatItems, selectedPath, revealCount]);
  // SW-EW-36: breadcrumb model for the drilled scope. Each ancestor's absolute
  // path is reconstructed from `rootPath` + the leading segments of the drilled
  // scope path (`tree.absoluteDirPath`) — no new prop, no IPC (D1). Crumbs reuse
  // `onDrillPathChange` (D4), so clicks are the same instant in-memory re-scope
  // the folder-drill buttons already use. Returns [] when not drilled (or when
  // the scope isn't a descendant of root — defensive), falling back to the
  // single-label header below.
  //
  // SW-PP-2: the crumb math + collapse now live in the shared, pure
  // `breadcrumb-model` helper so the pipeline-panel header toolbar can reuse the
  // identical model. Rendering stays here (this sidebar's chrome differs).
  const breadcrumbs = useMemo<PipelineCrumb[]>(
    () =>
      buildPipelineBreadcrumbs({
        isDrilledIn: !!isDrilledIn,
        rootPath,
        scopeAbsPath: tree.absoluteDirPath,
        rootLabel: rootDisplayName ?? repoName ?? "",
      }),
    [isDrilledIn, rootPath, tree.absoluteDirPath, rootDisplayName, repoName],
  );

  // AC-3 / D3: "up one level" is the second-to-last crumb; AC-5 / D2: collapse the
  // *middle* once deeper than three crumbs (root + parent + current stay
  // one-click), hidden ancestors surfaced via the `…` tooltip.
  const { displayCrumbs, hiddenAncestors, parentAbsPath } = collapsePipelineBreadcrumbs(
    breadcrumbs,
    rootPath,
  );

  // Defensive fallback label for the non-breadcrumb branch: when drilled but the
  // breadcrumb model couldn't build (no `rootPath`, or a scope that isn't a
  // descendant of root — shouldn't happen in production), show the drilled
  // folder's basename derived locally from the scoped tree. The breadcrumb owns
  // this label in the normal path (SW-EW-36), so no `drillFolderName` prop.
  const drillLeafLabel = tree.absoluteDirPath.split("/").filter(Boolean).pop() ?? "";

  // The breadcrumb row only exists while drilled in: at the root it would just restate the
  // org name the title bar already shows. Drilled, it carries "up one level" + the crumbs.
  const toolbar =
    repoName && isDrilledIn ? (
      <div
        className="flex items-center gap-1 px-2 pb-1 flex-shrink-0 select-none"
        data-testid="left-nav-breadcrumb-row"
      >
        <AppButton
          variant="ghost"
          size="sm"
          className="h-6 w-6 p-0 flex-shrink-0 text-muted-foreground hover:text-foreground"
          onClick={() => onDrillPathChange?.(parentAbsPath)}
          aria-label="Up one level"
          title="Up one level"
        >
          <ArrowLeft size={13} aria-hidden />
        </AppButton>
        {breadcrumbs.length > 0 ? (
          <nav
            aria-label="Breadcrumb"
            className="flex items-center min-w-0 gap-0.5 overflow-hidden whitespace-nowrap text-xs"
          >
            {displayCrumbs.map((crumb, i) => {
              const sep =
                i > 0 ? (
                  <ChevronRight
                    size={12}
                    className="flex-shrink-0 text-muted-foreground"
                    aria-hidden
                  />
                ) : null;
              if (crumb === "ellipsis") {
                return (
                  <span key="ellipsis" className="flex items-center flex-shrink-0">
                    {sep}
                    <span
                      className="px-0.5 text-muted-foreground cursor-default"
                      title={hiddenAncestors.map((c) => c.label).join(" › ")}
                      aria-label={`Hidden: ${hiddenAncestors.map((c) => c.label).join(", ")}`}
                    >
                      <MoreHorizontal size={12} aria-hidden />
                    </span>
                  </span>
                );
              }
              if (crumb.isCurrent) {
                // AC-5: the drilled-into folder is the highest-value label — keep
                // it un-truncated and non-shrinking so the ancestors (root/parent)
                // yield space first. Only clips as an absolute last resort.
                return (
                  <span key={crumb.absPath} className="flex items-center flex-shrink-0">
                    {sep}
                    <span
                      className="font-semibold whitespace-nowrap cursor-default"
                      onClick={(e: React.MouseEvent) => {
                        if (e.metaKey) {
                          sendTextToTerminal(tree.absoluteDirPath);
                        }
                      }}
                    >
                      {crumb.label}
                    </span>
                  </span>
                );
              }
              return (
                <span key={crumb.absPath} className="flex items-center min-w-0">
                  {sep}
                  <button
                    type="button"
                    className="truncate min-w-0 max-w-[120px] text-muted-foreground hover:text-foreground hover:underline"
                    onClick={() => onDrillPathChange?.(crumb.absPath)}
                  >
                    {crumb.label}
                  </button>
                </span>
              );
            })}
          </nav>
        ) : (
          // Drilled but the crumb model couldn't build (no `rootPath`, or a scope outside root):
          // show the drilled folder's own name.
          <nav
            aria-label="Breadcrumb"
            className="flex items-center min-w-0 gap-0.5 overflow-hidden whitespace-nowrap text-xs"
          >
            <span
              className="truncate min-w-0 font-semibold cursor-default"
              title={drillLeafLabel}
              onClick={(e: React.MouseEvent) => {
                if (e.metaKey) sendTextToTerminal(tree.absoluteDirPath);
              }}
            >
              {drillLeafLabel}
            </span>
          </nav>
        )}
      </div>
    ) : null;

  // NB-7 / AC-1 / Task 1.4: One RowCallbacks allocation per render instead
  // of one per visible row. RowCallbacks identity stays stable across
  // renders that don't change any of its 10 inputs — but note that
  // `statusPopoverOpenMap` is one of those inputs, so the memo DOES
  // recompute on every popover toggle. That's intentional: when the Map
  // identity flips, `renderRow` re-runs and each StoryRow receives a
  // recomputed `isPopoverOpen` primitive (Map.get lookup). Rows whose
  // primitive didn't change still skip via their own memo wrap; only the
  // affected row re-renders.
  //
  // Declared above the empty-state early return so the hook runs
  // unconditionally regardless of flatItems content.
  //
  // SW-VE-5: the active team name is the reference for the inherited-file gate
  // (a file whose winning team is a PARENT reads as read-only). Store-read here
  // (not threaded as a prop) so every scope-badge row shares one source.
  const activeTeamName = useOrgStore((s) =>
    owningTeamKey ? owningTeamKey.slice(owningTeamKey.lastIndexOf(":") + 1) : s.activeTeamName,
  );

  // RF-12: the file-action subsystem (dialog state + mutating handlers + the
  // derived menu-scope/materialized gating) lives in its own hook now.
  // RF-12b: keep the whole return as `fileActions` and pass it as a single
  // `actions` bundle to <NavDialogs> and <NavContextMenu>; the orchestrator body
  // itself only needs the three drop/ingest members below.
  const [resolvedFile, setResolvedFile] = useState<{ path: string; inheritedFrom?: string } | null>(
    null,
  );
  const actionTree = useMemo(
    () =>
      resolvedFile
        ? {
            ...tree,
            docs: [
              ...(tree.docs ?? []),
              {
                filePath: resolvedFile.path,
                fileName: resolvedFile.path.split("/").pop()!,
                sourceTeam: resolvedFile.inheritedFrom,
              },
            ],
          }
        : tree,
    [tree, resolvedFile],
  );
  const fileActions = useNavFileActions({
    repoPath,
    teamKey,
    tree: actionTree,
    activeTeamName,
    onFileSelect,
    contextMenu,
    setContextMenu,
    rootAnchor,
    toRel,
  });
  const { setRenameTarget, setDeleteTarget } = fileActions;
  useEffect(
    () =>
      registerNativeMenu(teamKey, "nav", (request) => {
        let cancelled = false;
        const show = (target: Omit<NavContextMenuState, "x" | "y">) => {
          if (cancelled) return;
          if (request.action) {
            if (request.action === "rename") setRenameTarget(target.path);
            else if (request.action === "delete") setDeleteTarget(target.path);
            else fileActions.newFileFromRow(target.path);
            return;
          }
          setContextMenu({
            ...target,
            x: request.anchorRect.x,
            y: request.anchorRect.y,
            owner: request.owner,
          });
          externalMenuClose.current = request.onClose;
        };
        if (request.action === "new-file") {
          fileActions.newFileFromRow(rootAnchor);
          return () => {
            cancelled = true;
          };
        }
        const target = dashboardNavTarget(request.row, rootAnchor, menuRows());
        if (target) show(target);
        else if (
          typeof request.row.path === "string" &&
          ["file", "story", "epic"].includes(String(request.row.kind))
        ) {
          const path = request.row.path.startsWith("/")
            ? request.row.path
            : `${rootAnchor}/${request.row.path}`;
          void fileSurfaceCall("list", { dir: dirname(path) })
            .then((value) => {
              if (cancelled) return;
              const file = (
                value as { rows: Array<{ path: string; inheritedFrom?: string }> }
              ).rows.find((row) => row.path === path);
              if (!file) {
                request.onClose();
                return;
              }
              setResolvedFile(file);
              show({ path });
            })
            .catch(() => request.onClose());
        } else return null;
        return () => {
          cancelled = true;
          if (externalMenuClose.current === request.onClose) setContextMenu(null);
        };
      }),
    [teamKey, menuRows, rootAnchor, setContextMenu, setRenameTarget, setDeleteTarget],
  );

  // The Files dashboard replaces the native body, so its filter button and status marks open
  // these from here: this owner outlives the body.
  const [paneMenu, setPaneMenu] = useState<
    | { kind: "filter"; request: NativeMenuRequest }
    | { kind: "status"; request: NativeMenuRequest; path: string }
    | null
  >(null);
  useEffect(() => {
    const open = (kind: "filter" | "status") => (request: NativeMenuRequest) => {
      const path = typeof request.row.path === "string" ? request.row.path : "";
      if (kind === "status" && (!onStatusChange || !path)) return null;
      setPaneMenu(kind === "filter" ? { kind, request } : { kind, request, path });
      return () => setPaneMenu((current) => (current?.request === request ? null : current));
    };
    const offFilter = registerNativeMenu(teamKey, "nav-filter", open("filter"));
    const offStatus = registerNativeMenu(teamKey, "nav-status", open("status"));
    return () => {
      offFilter();
      offStatus();
    };
  }, [teamKey, onStatusChange]);

  const refreshSources = sourceOptions.refresh;
  const filterOpen = paneMenu?.kind === "filter";
  useEffect(() => {
    if (filterOpen) refreshSources();
  }, [filterOpen, refreshSources]);

  const { handleMoveDrop, ingestBusyRef, handleOsDrop } = fileActions;
  const storyAuthoring = useStoryAuthoringActions({ tree, repoPath, onCreated: onFileSelect });

  // SW-VE-7: subscribe the nav to OS file drops (Tauri webview event). The nav's
  // scroll element supplies the bounds + row rects for hit-testing; a drop while
  // a collision dialog is open is suppressed via `ingestBusyRef`. `getScrollElement`
  // is memoized so the listener re-subscribes only when `handleOsDrop` changes.
  const getNavScrollElement = useCallback(
    () => virtualListRef.current?.getScrollElement() ?? null,
    [],
  );
  const dropHighlight = useNavFileDrop({
    getScrollElement: getNavScrollElement,
    disabledRef: ingestBusyRef,
    onDrop: handleOsDrop,
  });
  // RF-perf-12: preserve the nav tree's scroll position across a team
  // unmount→remount (RF-perf-10). `ready` gates restore until the virtualizer
  // has rows to measure against, so we don't clamp against a zero-height list.
  useScrollSnapshot({
    viewKey: navViewKey(teamKey),
    getEl: getNavScrollElement,
    ready: flatItems.length > 0,
  });
  const highlightDir = dropHighlight && "dir" in dropHighlight ? dropHighlight.dir : null;
  const highlightRoot = !!dropHighlight && "root" in dropHighlight;

  const rowCallbacks: RowInteractionModel = useMemo(
    () => ({
      teamKey,
      selectedPath,
      onOpenStoryWorkspace,
      onFileSelect,
      onFileDoubleClick,
      onDrillPathChange,
      onStatusChange,
      onContextMenu: handleContextMenu,
      toggleExpand,
      toggleGenericExpand,
      statusPopoverOpenMap,
      setStatusPopoverOpen,
      provenance,
      activeTeam: activeTeamName,
      // SLW-NC-7 pinned-section callbacks.
      onTogglePinnedSection: () => setPinnedCollapsed((c) => !c),
      onPinnedSelect: revealPinned,
      onPinnedUnpin: unpinPinnedRow,
      onPinnedContextMenu: handlePinnedContextMenu,
      // SW-CP-27: badge a generic config row when its content-relative path is
      // merge-marked. `toRel` bridges the row's absolute path to the CP-24 key.
      isMergeMarked: (abs: string) => {
        const rel = toRel(abs);
        return rel != null && rel !== "" && mergeMarks.isMarked(rel);
      },
      isStarterPage,
      moveActionSlot,
      onToggleStageGroup: toggleStageGroup,
      ...(changesView
        ? {
            onFileSelect: openChangePreview,
            onFileDoubleClick: openChangePinned,
            toggleExpand: toggleChangesFolder,
            toggleGenericExpand: toggleChangesFolder,
            showChangeLines: true,
            changeAction,
          }
        : {}),
    }),
    [
      teamKey,
      selectedPath,
      onOpenStoryWorkspace,
      onFileSelect,
      onFileDoubleClick,
      onDrillPathChange,
      onStatusChange,
      handleContextMenu,
      toggleExpand,
      toggleGenericExpand,
      statusPopoverOpenMap,
      setStatusPopoverOpen,
      provenance,
      activeTeamName,
      setPinnedCollapsed,
      revealPinned,
      unpinPinnedRow,
      handlePinnedContextMenu,
      toRel,
      mergeMarks,
      isStarterPage,
      moveActionSlot,
      toggleStageGroup,
      changesView,
      openChangePreview,
      openChangePinned,
      toggleChangesFolder,
      changeAction,
    ],
  );

  // SW-SB-15: the right-clicked row's sprint context, resolved ONCE per menu open
  // (memoized on `contextMenu`). A closed menu does no tree work at all; an open
  // one walks the tree a single time — a story row → its current `sprint:`; an
  // epic-dir row → its stories (via the shared `findScopedSubtree`), partitioned
  // once into unassigned vs already-sprinted (reused for both the counts and the
  // bulk confirm — no double filter, no per-render re-walk). Declared above the
  // empty-state early return so the hook runs unconditionally.
  const sprintMenu = useMemo<NavSprintMenu>(() => {
    const base = {
      targets: sprintTargets,
      onSetStory: (filePath: string, current: string | null, sprintId: string) =>
        void applySprintChange(filePath, current, sprintId, refreshAfterSprintWrite),
      onRemoveStory: (filePath: string, current: string) =>
        void applySprintChange(filePath, current, null, refreshAfterSprintWrite),
    };
    if (!contextMenu || contextMenu.pin) {
      return { ...base, story: null, epic: null, onBulkEpic: () => {} };
    }
    const storyRef = findStoryByPath(tree, contextMenu.path);
    const epicSubtree =
      contextMenu.isEpic && contextMenu.epicDirPath
        ? findScopedSubtree(tree, contextMenu.epicDirPath)
        : null;
    const epicStories = epicSubtree ? collectAllStories(epicSubtree) : null;
    const unassigned = epicStories?.filter((s) => !s.sprint) ?? null;
    // Re-point targets only stories in a DIFFERENT sprint; ones already in the
    // active target need no action, so they're excluded from both buckets.
    const sprinted = epicStories?.filter((s) => !!s.sprint && s.sprint !== activeSprintId) ?? null;
    return {
      ...base,
      story: storyRef ? { filePath: storyRef.filePath, current: storyRef.sprint ?? null } : null,
      epic: epicStories ? { unassigned: unassigned!.length, total: epicStories.length } : null,
      onBulkEpic: () => {
        if (!unassigned || !sprinted || !activeSprintId) return;
        setSprintBulkConfirm({ target: activeSprintId, unassigned, sprinted });
      },
    };
  }, [contextMenu, tree, sprintTargets, activeSprintId, refreshAfterSprintWrite]);

  // SW-LNS-1: the left-nav search bar — rendered under the toolbar in every
  // return branch so an empty tree (or an empty filter result) still exposes it.
  // Escape clears, then blurs. Keydown stops propagation so the registry's
  // bubble-phase handler (⌘⇧F self) and the team-view's inline keybindings
  // can't fire while the user is typing in here.
  // The filter row closes the header block (lens row, breadcrumbs when drilled, filter) with
  // its single divider. The scope's story progress trails the field, outside it, so it never
  // reads as a match count.
  const anyChip =
    view.source?.kind === "worktree" ||
    changesView ||
    historyView ||
    view.lens !== "everything" ||
    view.arrange === "stage";
  const searchBar = (
    <div
      className="@container flex items-center gap-2 px-2 pt-0.5 pb-1.5 border-b flex-shrink-0"
      style={{ borderColor: "var(--cs-sidebar-border, var(--border))" }}
    >
      <NavFilterField
        repoPath={repoPath}
        query={rawSearchQuery}
        onQueryChange={setRawSearchQuery}
        inputRef={searchInputRef}
        view={view}
        onViewChange={setView}
        counts={lensCounts}
        changesCount={changes.length}
        sources={sourceOptions.sources}
        onSourcesOpen={sourceOptions.refresh}
      />
      {!anyChip && (
        <span className="contents @max-[13rem]:hidden">
          <ProgressChipRow total={epicTotal} done={epicDone} />
        </span>
      )}
    </div>
  );

  const [privateTarget, setPrivateTarget] = useState<
    import("@/tauri/commands").ScratchOverlayFile | null
  >(null);
  const currentPrivate = contextMenu ? privateFiles?.get(contextMenu.path) : undefined;
  if (currentPrivate && currentPrivate !== privateTarget) setPrivateTarget(currentPrivate);
  const privateItemActions = useScratchItemActions(
    {
      orgSlug: privateTarget?.orgSlug ?? teamKey.split(":")[0],
      logical: privateTarget?.logical ?? "",
      where: "org",
      inertKind: privateTarget?.inertKind ?? null,
      defaultTeam: privateTarget?.team ?? null,
    },
    () => {
      if (rootAnchor) useFileRefreshStore.getState().bumpFileRefresh(rootAnchor);
    },
  );

  // What emptied the list, said so the user knows which half to clear.
  const lensLabel = NAV_LENSES.find((l) => l.lens === view.lens)!.label;
  const query = rawSearchQuery.trim();
  const emptyMessage = changesView ? (
    query ? (
      `No changed file matches "${query}".`
    ) : changesLoading ? (
      "Reading changes…"
    ) : (
      <>
        <span>
          {view.source.kind === "private"
            ? "Nothing changed since the last commit."
            : "GitHub has everything here."}
        </span>
        <button
          type="button"
          className="text-foreground hover:underline"
          onClick={() => setView({ view: "files" })}
        >
          Show all files
        </button>
      </>
    )
  ) : view.lens !== "everything" ? (
    <>
      <span>
        {query
          ? `No story ${lensPhrase(view.lens)} matches "${query}".`
          : `No story ${lensPhrase(view.lens)} here.`}
      </span>
      <button
        type="button"
        className="text-foreground hover:underline"
        onClick={() => setView({ lens: "everything" })}
      >
        Clear &quot;{lensLabel}&quot;
        <span className="text-muted-foreground"> to {query ? "search" : "show"} everything.</span>
      </button>
    </>
  ) : isSearching ? (
    query ? (
      `Nothing matches "${query}".`
    ) : (
      "No stories here."
    )
  ) : view.source.kind === "private" ? (
    `Nothing private in ${teamKey.split(":")[0]} yet.`
  ) : (
    "Nothing here yet"
  );

  let nativeOverride: React.ReactNode;
  if (historyView) {
    nativeOverride = (
      <>
        {toolbar}
        {searchBar}
        <ContextHistoryView
          teamKey={teamKey}
          navRepoPath={repoPath}
          orgRoot={view.source.kind === "worktree" ? (cloneRoot ?? null) : contextOrgRoot}
          localOnly={view.source.kind === "private"}
          teamPrefix={teamPrefix}
          status={view.source.kind === "worktree" ? (cloneStatus ?? null) : contextStatus}
          query={searchQuery}
          branches={view.historyBranches}
          onBranchesChange={setHistoryBranches}
          agentScope={agentContext}
        />
      </>
    );
  }

  if (
    !historyView &&
    flatItems.length === 0 &&
    !(changesView && (contextStatus?.worktree || sourceNote)) &&
    !contextMenu
  ) {
    nativeOverride = (
      <>
        {toolbar}
        {searchBar}
        {changesView && view.source.kind === "private" && (
          <div className="flex items-center justify-between gap-1 px-2 py-1 text-xs">
            <h3>
              {teamKey.split(":")[0]} · {changes.length}
            </h3>
            <PrivateCommitButton teamKey={teamKey} orgRoot={rootAnchor} />
          </div>
        )}
        <div
          className="flex flex-1 flex-col items-center justify-start gap-0.5 px-3 pt-[72px] text-center text-xs leading-normal text-muted-foreground"
          data-testid="left-nav-empty"
        >
          {emptyMessage}
          {!isSearching && !changesView && view.lens === "everything" && rootAnchor && (
            <AppButton
              type="button"
              variant="outline"
              size="xs"
              onClick={() => fileActions.setNewFileDir(rootAnchor)}
            >
              New file
            </AppButton>
          )}
        </div>
      </>
    );
  }

  // In an agent's context the Changes view says whose context it lists, and how much it leaves
  // out (the team's other changes are a scope switch away). Nothing is claimed about what a
  // running copy of the agent has seen.
  // The team's context repo, by its org's name (the key is `org:team`).
  const contextRepoName = teamKey.split(":")[0];
  const scopedItems: FlatNavItem[] =
    changesView && view.source.kind === "private"
      ? [
          {
            type: "section-note",
            depth: 0,
            key: "private-heading",
            tone: "heading",
            text: contextRepoName,
            count: changes.length,
            trailingAction: <PrivateCommitButton teamKey={teamKey} orgRoot={rootAnchor} />,
          },
          ...flatItems,
        ]
      : changesView && agentContext
        ? [
            {
              type: "section-note",
              depth: 0,
              key: "changes-heading",
              tone: "heading",
              text: `Team context · ${contextRepoName}`,
              count: changes.length,
            },
            ...flatItems,
            ...(changesOutside > 0
              ? [
                  {
                    type: "section-note" as const,
                    depth: 0,
                    key: "changes-outside",
                    tone: "note" as const,
                    text: `${changesOutside} more ${changesOutside === 1 ? "change is" : "changes are"} outside what ${agentContext.agentName} gets. Switch to Team to see ${changesOutside === 1 ? "it" : "them"}.`,
                  },
                ]
              : []),
          ]
        : flatItems;

  const worktreeHeading = contextStatus?.worktree;
  const displayItems: FlatNavItem[] =
    changesView && (worktreeHeading || sourceNote)
      ? [
          {
            type: "section-note",
            depth: 0,
            key: "context-source-heading",
            tone: "heading",
            trailingAction: contextStatus?.agents?.length ? (
              <ContextWorktreeCrew agents={contextStatus.agents} />
            ) : undefined,
            text: worktreeHeading
              ? `${worktreeHeading.name} · vs main · ${changes.length} files · ${contextStatus.uncommitted ?? 0} uncommitted`
              : sourceNote!,
          },
          ...scopedItems,
        ]
      : scopedItems;

  // SLW-NC-7: pin/unpin menu state for the right-clicked row. A row is pinnable when
  // its path resolves to a live pin target (dirs + non-story files — story rows are
  // excluded because `collectPinTargets` skips them).
  const menuRel = contextMenu && !contextMenu.pin ? toRel(contextMenu.path) : null;
  const menuPinKey = menuRel != null ? normalizePinPath(menuRel) : null;
  const menuPinnable = menuPinKey != null && pinResolvable.has(menuPinKey);
  const menuIsTeamPinned = menuPinKey != null && teamPinSet.has(menuPinKey);
  const menuIsPersonalPinned = menuPinKey != null && personalPinSet.has(menuPinKey);
  // SW-CP-27: the merge-mark toggle bundle for the right-clicked row — present
  // only for a markable structured config file (`canMergeMark` mirrors CP-24's
  // format gate). `menuRel` is the CP-24 content-relative key; a null/empty rel
  // or a non-structured file yields no item.
  const mergeMarkMenu =
    menuRel && canMergeMark(menuRel)
      ? {
          marked: mergeMarks.isMarked(menuRel),
          onToggle: () => void mergeMarks.toggle(menuRel),
        }
      : undefined;

  // SW-XS-15: "Add to / Remove from ‹Agent›'s context" for the right-clicked row — team
  // content only (a repo-tier row is browse-only), and only with an agent picked.
  const menuContextRel =
    agentContext && contextMenu && !contextMenu.pin
      ? agentContext.editRelOf(contextMenu.path)
      : null;
  const agentContextMenu =
    agentContext && menuContextRel ? { context: agentContext, rel: menuContextRel } : undefined;

  return (
    <>
      {renderNative(
        nativeOverride ?? (
          <>
            {toolbar}
            {searchBar}
            <VirtualList<FlatNavItem>
              ref={virtualListRef}
              items={displayItems}
              estimateSize={ROW_HEIGHT}
              getKey={(item) => itemStableKey(item)}
              overscan={10}
              // SW-VE-7: ring the whole nav when an OS drag is over the root zone
              // (empty space / non-directory row) — the "add to the content root"
              // affordance, distinct from the per-directory-row ring below.
              className={`@container overflow-y-auto flex-1 p-1 ${highlightRoot ? "cs-sidebar-drop-ring rounded-md" : ""}`}
              renderItem={(item) => {
                // SW-VE-7: directory rows are drop targets — mark them for the OS-drop
                // hit-test (`data-nav-drop-dir`) and ring the one under an OS drag.
                const dropDir = navDropDirPath(item);
                const osDropTarget = dropDir != null && dropDir === highlightDir;
                return (
                  <div
                    style={{ paddingLeft: `${item.depth * INDENT_PX}px` }}
                    data-nav-drop-dir={dropDir ?? undefined}
                  >
                    <div className={osDropTarget ? "rounded-md cs-sidebar-drop-ring" : undefined}>
                      <NavRowDnd
                        item={item}
                        rootDir={rootAnchor}
                        draggedPathRef={draggedPathRef}
                        onRelateDrop={onRelateDrop}
                        moveTargetDir={dropDir}
                        onMoveDrop={handleMoveDrop}
                        activeTeam={activeTeamName}
                      >
                        {withAgentContext(item, renderRow(item, rowCallbacks), agentContext)}
                      </NavRowDnd>
                    </div>
                  </div>
                );
              }}
            />
          </>
        ),
      )}
      {privateItemActions.sheets}
      <NavContextMenu
        extraActions={
          currentPrivate
            ? privateItemActions.rows.map((row) => (
                <PositionedMenuItem
                  key={row.verb}
                  disabled={!!row.disabledReason}
                  onClick={() => {
                    privateItemActions.start(row.verb);
                    setContextMenu(null);
                  }}
                >
                  {row.label}
                </PositionedMenuItem>
              ))
            : undefined
        }
        menu={contextMenu}
        onClose={() => setContextMenu(null)}
        actions={fileActions}
        pins={{
          pinnable: view.source.kind === "clone" && menuPinnable,
          pinKey: menuPinKey,
          isTeamPinned: menuIsTeamPinned,
          isPersonalPinned: menuIsPersonalPinned,
          teamKey,
          toggleTeamPin,
          togglePersonalPin,
          unpinPinnedRow,
        }}
        parent={{
          onOpenTrail: (filePath) => {
            const storyId =
              findStoryByPath(scopeTree, filePath)?.storyId ?? storyIdFromPath(filePath);
            void openStoryTrail({ storyId, teamKey, navigate: onFileSelect }).catch((error) =>
              toast.error(String(error.message || error)),
            );
          },
          onOpenStoryWorkspace,
          renderOpenCli,
          isStoryFilePath,
          onOpenEpicTerminal,
          onExportPdf,
        }}
        sprint={sprintMenu}
        mergeMark={mergeMarkMenu}
        agentContext={agentContextMenu}
        storyAuthoring={{ onNewStory: storyAuthoring.openNewStory }}
      />
      <NavDialogs actions={fileActions} />
      {paneMenu?.kind === "filter" && (
        <PaneNavFilterMenu
          anchor={paneMenu.request.anchorRect}
          onClose={() => {
            setPaneMenu(null);
            paneMenu.request.onClose();
          }}
          inputRef={searchInputRef}
          view={view}
          onViewChange={setView}
          counts={lensCounts}
          changesCount={changes.length}
          repoPath={repoPath}
          sources={sourceOptions.sources}
        />
      )}
      {paneMenu?.kind === "status" && (
        <PaneNavStatusPicker
          anchor={paneMenu.request.anchorRect}
          onClose={() => {
            setPaneMenu(null);
            paneMenu.request.onClose();
          }}
          currentStatus={paneStoryStatus(scopeTree, paneMenu.path)}
          onSelect={(status) => onStatusChange?.(paneMenu.path, status)}
        />
      )}
      {docAttach.picker && (
        <AttachToNodePicker
          targets={attachTargets}
          onSelect={(target) => docAttach.selectNode(target.mdPath, target.label)}
          onClose={docAttach.closePicker}
        />
      )}
      {storyAuthoring.dialogs}
      {/* SW-SB-15 · AC-2/AC-3: the epic bulk-add confirm. Unassigned-only is the
          safe default; the re-point action (shown only when some stories already
          belong elsewhere) is the stronger, sprint-listing option. */}
      <AlertDialog
        open={!!sprintBulkConfirm}
        onOpenChange={(open) => {
          if (!open) setSprintBulkConfirm(null);
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Add epic stories to {sprintBulkConfirm?.target}</AlertDialogTitle>
            <AlertDialogDescription>
              {sprintBulkConfirm &&
                (sprintBulkConfirm.unassigned.length > 0
                  ? `Add ${sprintBulkConfirm.unassigned.length} unsprinted ${
                      sprintBulkConfirm.unassigned.length === 1 ? "story" : "stories"
                    } to ${sprintBulkConfirm.target}.`
                  : `Every story in this epic already belongs to a sprint.`)}
              {sprintBulkConfirm && sprintBulkConfirm.sprinted.length > 0 && (
                <>
                  {" "}
                  {sprintBulkConfirm.sprinted.length}{" "}
                  {sprintBulkConfirm.sprinted.length === 1 ? "story is" : "stories are"} already in{" "}
                  {[...new Set(sprintBulkConfirm.sprinted.map((s) => s.sprint))].join(", ")} — left
                  untouched unless you re-point.
                </>
              )}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            {sprintBulkConfirm && sprintBulkConfirm.unassigned.length > 0 && (
              <AlertDialogAction
                onClick={() => {
                  const { target, unassigned } = sprintBulkConfirm;
                  const entries = unassigned.map((s) => ({
                    filePath: s.filePath,
                    prevSprint: null,
                  }));
                  setSprintBulkConfirm(null);
                  void bulkTagStories(entries, target, refreshAfterSprintWrite);
                }}
              >
                Add {sprintBulkConfirm.unassigned.length} unsprinted
              </AlertDialogAction>
            )}
            {sprintBulkConfirm && sprintBulkConfirm.sprinted.length > 0 && (
              <AlertDialogAction
                className="bg-[var(--destructive)] text-white hover:bg-[var(--destructive)]/90"
                onClick={() => {
                  const { target, unassigned, sprinted } = sprintBulkConfirm;
                  const entries = [...unassigned, ...sprinted].map((s) => ({
                    filePath: s.filePath,
                    prevSprint: s.sprint ?? null,
                  }));
                  setSprintBulkConfirm(null);
                  void bulkTagStories(entries, target, refreshAfterSprintWrite);
                }}
              >
                Re-point all{" "}
                {sprintBulkConfirm.unassigned.length + sprintBulkConfirm.sprinted.length}
              </AlertDialogAction>
            )}
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

/** How a lens reads inside "No story … matches". */
function lensPhrase(lens: NavLensState["lens"]): string {
  switch (lens) {
    case "needs":
      return "that needs you";
    case "agents":
      return "with an agent on it";
    case "ready":
      return "that is ready to start";
    case "active":
      return "in progress";
    case "review":
      return "in review";
    case "blocked":
      return "that is blocked";
    case "not_started":
      return "not started";
    case "done":
      return "that is done";
    case "everything":
      return "";
  }
}
